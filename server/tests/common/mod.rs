//! 集成测试共享辅助。

/// 使用只存在于当前测试进程内的固定密钥初始化密钥环。
///
/// 不走开发环境的磁盘密钥回退：它依赖启动流程先认领数据目录，也会让测试结果受到
/// 仓库里是否残留 `unionc/data` 的影响。
#[allow(dead_code)]
pub fn init_test_keyring() {
    unionc::infra::secrets::init_with_test_key("integration-test", [0x42; 32])
        .expect("initialize in-memory test keyring");
}

/// An isolated SQLite URL whose containing directory is removed when the test
/// finishes. Keeping the guard alive also covers WAL/SHM files and any sibling
/// database derived from this URL by a test.
pub struct TestDatabaseUrl {
    _directory: tempfile::TempDir,
    url: String,
}

impl std::ops::Deref for TestDatabaseUrl {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.url
    }
}

impl std::fmt::Display for TestDatabaseUrl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.url)
    }
}

/// 为每个持久层用例创建唯一且会自动清理的本地 SQLite 数据库 URL。
///
/// SQLite 是 Server 的内嵌式持久层，因此测试不依赖外部数据库，也不会以“未配置”
/// 为由跳过。RAII guard 消除了过去每次测试都向系统临时目录泄漏 `.db/-wal/-shm` 文件
/// 的问题。
pub fn test_database_url(test_name: &str) -> TestDatabaseUrl {
    let safe_name: String = test_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let directory = tempfile::Builder::new()
        .prefix(&format!("unionc-test-{safe_name}-"))
        .tempdir()
        .expect("create test database directory");
    let path = directory.path().join("unionc.db");
    TestDatabaseUrl {
        _directory: directory,
        url: format!("sqlite://{}?mode=rwc", path.display()),
    }
}

/// Insert an already-approved current-protocol Agent for persistence tests.
/// HTTP pairing behavior is covered separately in `agent_pairing.rs`; these
/// tests only need a host and its active report credential as fixture data.
#[allow(dead_code)]
pub async fn insert_active_monitoring_host(
    pool: &unionc::infra::database::DbPool,
    host: &unionc::monitoring::HostIdentity,
    token_hash: &str,
) -> anyhow::Result<()> {
    let now = unionc::infra::database::now_epoch_micros();
    let mut transaction = unionc::infra::database::begin_write(pool)
        .await
        .expect("begin fixture transaction");
    sqlx_core::query::query(
        r#"
        INSERT INTO monitored_hosts(
            host_id,name,os,os_version,kernel_version,arch,agent_version,
            registered_at,last_seen_at
        ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8)
        "#,
    )
    .bind(&host.id)
    .bind(&host.name)
    .bind(&host.os)
    .bind(host.os_version.as_deref())
    .bind(host.kernel_version.as_deref())
    .bind(&host.arch)
    .bind(&host.agent_version)
    .bind(now)
    .execute(transaction.connection())
    .await
    .expect("insert monitoring host fixture");
    sqlx_core::query::query(
        r#"
        INSERT INTO agent_credentials(credential_id,host_id,token_hash,issued_at)
        VALUES(?1,?2,?3,?4)
        "#,
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&host.id)
    .bind(token_hash)
    .bind(now)
    .execute(transaction.connection())
    .await
    .expect("insert Agent credential fixture");
    transaction.commit().await.expect("commit fixture");
    Ok(())
}
