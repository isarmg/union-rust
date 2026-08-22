//! Sunshine 主机配置的持久化往返。
//!
//! 守护目标：保存运行配置**不得**扰动既有主机的身份、顺序与添加时间。
//!
//! 这里守的是两个只有在重启之后才显形的缺陷：
//!
//! 1. **展示顺序不是添加顺序。** `load_sunshine_hosts` 曾按 `created_at, host_id`
//!    取序，看上去等价于"按添加先后"，但同一批次时间戳可能相同，最终退化为按随机
//!    UUID 排。用户按 A、B、C 添加，控制台却会以毫无意义的顺序展示。顺序必须由
//!    SQLite schema 的显式 `position` 列承载。
//! 2. **添加时间会被抹掉。** 保存曾用"清空整表再逐条 INSERT"实现，于是每一次保存
//!    ——哪怕只是改了某台主机的名字——都会把全部行的 `created_at` 重置为当前时间。
//!
//! 单元测试覆盖不到这两条：缺陷只存在于"写进库再读回来"这一整个往返里。

use unionc::{
    config::{Settings, SunshineHostConfig},
    infra::database,
};

mod common;

async fn fresh_settings(url: String) -> (Settings, database::DbPool) {
    unionc::infra::secrets::init(unionc::config::RuntimeMode::Development)
        .expect("initialize test keyring");
    let mut settings = Settings::default();
    settings.database.url = url;
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");
    (settings, pool)
}

fn host(name: &str) -> SunshineHostConfig {
    SunshineHostConfig {
        name: name.to_string(),
        host: "192.0.2.10".to_string(),
        username: "admin".to_string(),
        password: format!("{name}-secret"),
        ..SunshineHostConfig::default()
    }
}

async fn reload(pool: &database::DbPool) -> Vec<SunshineHostConfig> {
    database::load_sunshine_hosts(pool)
        .await
        .expect("load hosts")
}

async fn insert_all(pool: &database::DbPool, hosts: &[SunshineHostConfig]) {
    for host in hosts {
        database::insert_sunshine_host(pool, host, "test fixture")
            .await
            .expect("insert host");
    }
}

/// 编辑一台主机后，列表顺序与其余主机的身份必须原封不动。
#[tokio::test]
async fn editing_one_host_preserves_the_order_of_all_hosts() {
    let url = common::test_database_url("editing_one_host_preserves_the_order_of_all_hosts");
    let (_settings, pool) = fresh_settings(url.to_string()).await;

    let names = ["alpha", "bravo", "charlie", "delta"];
    let initial: Vec<_> = names.iter().map(|name| host(name)).collect();
    insert_all(&pool, &initial).await;

    let mut stored = reload(&pool).await;
    let original_ids: Vec<String> = stored.iter().map(|host| host.id.clone()).collect();
    assert_eq!(
        stored.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
        names,
        "首次保存后应按插入顺序返回"
    );

    let before_target_secret: Option<String> = sqlx_core::query::query(
        "SELECT secret FROM external_hosts WHERE kind='sunshine' AND host_id=?",
    )
    .bind(&stored[1].id)
    .fetch_one(&pool)
    .await
    .map(|row| sqlx_core::row::Row::get(&row, "secret"))
    .expect("read target ciphertext");
    let untouched_before: (Option<String>, i64) = sqlx_core::query::query(
        "SELECT secret,updated_at FROM external_hosts WHERE kind='sunshine' AND host_id=?",
    )
    .bind(&stored[2].id)
    .fetch_one(&pool)
    .await
    .map(|row| {
        (
            sqlx_core::row::Row::get(&row, "secret"),
            sqlx_core::row::Row::get(&row, "updated_at"),
        )
    })
    .expect("read untouched row");

    // 只改中间那一台的名称；未提供密码时密文也必须保持原字节。
    stored[1].name = "bravo-renamed".to_string();
    database::update_sunshine_host(&pool, &stored[1], false, "rename")
        .await
        .expect("update host");

    let reloaded = reload(&pool).await;
    assert_eq!(
        reloaded.iter().map(|h| h.id.clone()).collect::<Vec<_>>(),
        original_ids,
        "改一台主机的名称不得改变整个列表的顺序"
    );
    assert_eq!(
        reloaded.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
        ["alpha", "bravo-renamed", "charlie", "delta"],
        "只有被编辑的那台应当变化"
    );
    // 密码走的是单独的加密列，往返后必须仍然解得出原值。
    assert_eq!(reloaded[0].password, "alpha-secret");
    assert_eq!(reloaded[1].password, "bravo-secret");
    let after_target_secret: Option<String> = sqlx_core::query::query(
        "SELECT secret FROM external_hosts WHERE kind='sunshine' AND host_id=?",
    )
    .bind(&stored[1].id)
    .fetch_one(&pool)
    .await
    .map(|row| sqlx_core::row::Row::get(&row, "secret"))
    .expect("read target ciphertext after update");
    assert_eq!(before_target_secret, after_target_secret);
    let untouched_after: (Option<String>, i64) = sqlx_core::query::query(
        "SELECT secret,updated_at FROM external_hosts WHERE kind='sunshine' AND host_id=?",
    )
    .bind(&stored[2].id)
    .fetch_one(&pool)
    .await
    .map(|row| {
        (
            sqlx_core::row::Row::get(&row, "secret"),
            sqlx_core::row::Row::get(&row, "updated_at"),
        )
    })
    .expect("read untouched row after update");
    assert_eq!(
        untouched_before, untouched_after,
        "更新一台主机不得重写其他主机行"
    );
}

/// `created_at` 记录的是主机第一次被添加的时间，保存运行配置不得重写它。
#[tokio::test]
async fn saving_settings_does_not_rewrite_registration_timestamps() {
    let url = common::test_database_url("saving_settings_does_not_rewrite_registration_timestamps");
    let (_settings, pool) = fresh_settings(url.to_string()).await;

    let original = host("original");
    database::insert_sunshine_host(&pool, &original, "create")
        .await
        .expect("initial save");

    let created_at: i64 = sqlx_core::query::query(
        "SELECT created_at FROM external_hosts WHERE kind='sunshine' LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .map(|row| sqlx_core::row::Row::get(&row, "created_at"))
    .expect("read created_at");

    // 时间戳精度足以区分两次写入。
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let mut stored = reload(&pool).await;
    stored[0].name = "renamed".to_string();
    database::update_sunshine_host(&pool, &stored[0], false, "rename")
        .await
        .expect("second save");

    let after: i64 = sqlx_core::query::query(
        "SELECT created_at FROM external_hosts WHERE kind='sunshine' LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .map(|row| sqlx_core::row::Row::get(&row, "created_at"))
    .expect("read created_at");

    assert_eq!(
        created_at, after,
        "created_at 是主机的添加时间，保存配置不得把它重置为本次保存时间"
    );
}

/// 移除的主机必须真的从库里消失——定向删除不能只删对了顺序、漏了内容。
#[tokio::test]
async fn removed_hosts_are_deleted_and_the_last_removal_empties_the_table() {
    let url = common::test_database_url(
        "removed_hosts_are_deleted_and_the_last_removal_empties_the_table",
    );
    let (_settings, pool) = fresh_settings(url.to_string()).await;

    let initial = vec![host("keep"), host("drop")];
    insert_all(&pool, &initial).await;

    let stored = reload(&pool).await;
    database::delete_sunshine_host(&pool, &stored[1].id)
        .await
        .expect("delete host");

    let remaining = reload(&pool).await;
    assert_eq!(remaining.len(), 1, "被移除的主机必须从库中删除");
    assert_eq!(remaining[0].name, "keep");

    database::delete_sunshine_host(&pool, &remaining[0].id)
        .await
        .expect("delete final host");
    let count: i64 = sqlx_core::query::query(
        "SELECT COUNT(*) AS total FROM external_hosts WHERE kind='sunshine'",
    )
    .fetch_one(&pool)
    .await
    .map(|row| sqlx_core::row::Row::get(&row, "total"))
    .expect("count hosts");
    assert_eq!(count, 0, "删光所有主机后表内不应残留行");
}
