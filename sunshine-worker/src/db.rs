use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::{PgPool, Postgres, Transaction, migrate::Migrator, postgres::PgPoolOptions};

use crate::{
    crypto::SecretBox,
    error::{AppError, AppResult},
    model::{Host, HostPatchRequest, HostSaveRequest, normalize_host, validate_host_request},
};

pub const SCHEMA: &str = "sunshine";
static MIGRATOR: Migrator = sqlx::migrate!("./migrations");
const WRITE_LOCK: i64 = 0x5355_4e53_4849_4e45;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub(crate) struct StoredHost {
    pub host_id: String,
    pub name: String,
    pub address: String,
    pub web_port: i32,
    pub username: String,
    pub secret: Option<String>,
    pub verify_tls: bool,
    pub position: i64,
    pub created_at_micros: i64,
    pub updated_at_micros: i64,
}

pub async fn connect(database_url: &str) -> anyhow::Result<PgPool> {
    Ok(PgPoolOptions::new()
        .max_connections(12)
        .acquire_timeout(Duration::from_secs(10))
        .connect(database_url)
        .await?)
}

/// Run only module-owned migrations. The deployment must provision the
/// `sunshine` schema and make the worker role its owner before startup.
pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.schemata WHERE schema_name = $1)",
    )
    .bind(SCHEMA)
    .fetch_one(pool)
    .await?;
    anyhow::ensure!(
        exists,
        "PostgreSQL schema 'sunshine' is not provisioned; the runtime role never creates schemas"
    );
    let mut connection = pool.acquire().await?;
    sqlx::query("SET search_path TO sunshine, pg_catalog")
        .execute(&mut *connection)
        .await?;
    MIGRATOR.run(&mut *connection).await?;
    Ok(())
}

pub async fn ready(pool: &PgPool) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT to_regclass('sunshine.hosts') IS NOT NULL AND to_regclass('sunshine.import_batches') IS NOT NULL",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false)
}

pub async fn list_hosts(pool: &PgPool, secrets: &SecretBox) -> AppResult<Vec<Host>> {
    let rows = sqlx::query_as::<_, StoredHost>(
        r#"SELECT host_id,name,address,web_port,username,secret,verify_tls,position,
                  created_at_micros,updated_at_micros
           FROM sunshine.hosts
           ORDER BY position,created_at_micros,host_id"#,
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| decode_host(row, secrets))
        .collect()
}

pub async fn get_host(pool: &PgPool, secrets: &SecretBox, id: &str) -> AppResult<Host> {
    let row = get_stored_host(pool, id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Sunshine host '{id}' does not exist")))?;
    decode_host(row, secrets)
}

pub async fn insert_host(
    pool: &PgPool,
    secrets: &SecretBox,
    request: HostSaveRequest,
    production: bool,
    actor: &str,
) -> AppResult<Host> {
    validate_host_request(&request, production)?;
    let now = now_micros()?;
    let mut transaction = pool.begin().await?;
    lock_writes(&mut transaction).await?;
    let position: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(position), -1) + 1 FROM sunshine.hosts")
            .fetch_one(&mut *transaction)
            .await?;
    let host = Host {
        id: uuid::Uuid::new_v4().to_string(),
        name: request.name.trim().to_string(),
        host: normalize_host(&request.host),
        web_port: request.web_port,
        username: request.username.trim().to_string(),
        password: request.password.unwrap_or_default(),
        verify_tls: request.verify_tls,
        position,
        created_at_micros: now,
        updated_at_micros: now,
    };
    let stored = encode_host(&host, secrets)?;
    insert_stored(&mut transaction, &stored).await?;
    insert_audit(
        &mut transaction,
        "sunshine.host.create",
        &host.id,
        actor,
        Some(&format!(
            "name={} host={} port={} verify_tls={}",
            host.name, host.host, host.web_port, host.verify_tls
        )),
    )
    .await?;
    transaction.commit().await?;
    Ok(host)
}

pub async fn update_host(
    pool: &PgPool,
    secrets: &SecretBox,
    id: &str,
    patch: HostPatchRequest,
    production: bool,
    actor: &str,
) -> AppResult<Host> {
    if patch.is_empty() {
        return Err(AppError::BadRequest(
            "at least one host field must be provided".to_string(),
        ));
    }
    let update_password = patch.password.is_some();
    let mut transaction = pool.begin().await?;
    lock_writes(&mut transaction).await?;
    let row = get_stored_host_for_update(&mut transaction, id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Sunshine host '{id}' does not exist")))?;
    let mut host = decode_host(row.clone(), secrets)?;
    if let Some(value) = patch.name {
        host.name = value.trim().to_string();
    }
    if let Some(value) = patch.host {
        host.host = normalize_host(&value);
    }
    if let Some(value) = patch.web_port {
        host.web_port = value;
    }
    if let Some(value) = patch.username {
        host.username = value.trim().to_string();
    }
    if let Some(value) = patch.password {
        host.password = value;
    }
    if let Some(value) = patch.verify_tls {
        host.verify_tls = value;
    }
    validate_host_request(
        &HostSaveRequest {
            name: host.name.clone(),
            host: host.host.clone(),
            web_port: host.web_port,
            username: host.username.clone(),
            password: update_password.then(|| host.password.clone()),
            verify_tls: host.verify_tls,
        },
        production,
    )?;
    host.updated_at_micros = now_micros()?;
    let mut stored = encode_host(&host, secrets)?;
    if !update_password {
        stored.secret = row.secret;
    }
    update_stored(&mut transaction, &stored).await?;
    insert_audit(
        &mut transaction,
        "sunshine.host.update",
        &host.id,
        actor,
        Some(&format!(
            "name={} host={} port={} verify_tls={}",
            host.name, host.host, host.web_port, host.verify_tls
        )),
    )
    .await?;
    transaction.commit().await?;
    Ok(host)
}

pub async fn delete_host(pool: &PgPool, id: &str, actor: &str) -> AppResult<()> {
    let mut transaction = pool.begin().await?;
    lock_writes(&mut transaction).await?;
    let result = sqlx::query("DELETE FROM sunshine.hosts WHERE host_id=$1")
        .bind(id)
        .execute(&mut *transaction)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!(
            "Sunshine host '{id}' does not exist"
        )));
    }
    insert_audit(
        &mut transaction,
        "sunshine.host.delete",
        id,
        actor,
        Some("host removed"),
    )
    .await?;
    transaction.commit().await?;
    Ok(())
}

pub async fn audit_best_effort(
    pool: &PgPool,
    action: &str,
    target: &str,
    actor: &str,
    detail: Option<&str>,
) {
    let result = async {
        let mut transaction = pool.begin().await?;
        insert_audit(&mut transaction, action, target, actor, detail).await?;
        transaction.commit().await
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, action, target, "upstream mutation succeeded but audit insert failed");
    }
}

pub(crate) async fn lock_writes(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(WRITE_LOCK)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

pub(crate) async fn get_stored_host(
    pool: &PgPool,
    id: &str,
) -> Result<Option<StoredHost>, sqlx::Error> {
    sqlx::query_as::<_, StoredHost>(
        r#"SELECT host_id,name,address,web_port,username,secret,verify_tls,position,
                  created_at_micros,updated_at_micros
           FROM sunshine.hosts WHERE host_id=$1"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub(crate) async fn get_stored_host_for_update(
    transaction: &mut Transaction<'_, Postgres>,
    id: &str,
) -> Result<Option<StoredHost>, sqlx::Error> {
    sqlx::query_as::<_, StoredHost>(
        r#"SELECT host_id,name,address,web_port,username,secret,verify_tls,position,
                  created_at_micros,updated_at_micros
           FROM sunshine.hosts WHERE host_id=$1 FOR UPDATE"#,
    )
    .bind(id)
    .fetch_optional(&mut **transaction)
    .await
}

pub(crate) async fn insert_stored(
    transaction: &mut Transaction<'_, Postgres>,
    row: &StoredHost,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO sunshine.hosts(
               host_id,name,address,web_port,username,secret,verify_tls,position,
               created_at_micros,updated_at_micros)
           VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)"#,
    )
    .bind(&row.host_id)
    .bind(&row.name)
    .bind(&row.address)
    .bind(row.web_port)
    .bind(&row.username)
    .bind(&row.secret)
    .bind(row.verify_tls)
    .bind(row.position)
    .bind(row.created_at_micros)
    .bind(row.updated_at_micros)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

pub(crate) async fn upsert_stored(
    transaction: &mut Transaction<'_, Postgres>,
    row: &StoredHost,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO sunshine.hosts(
               host_id,name,address,web_port,username,secret,verify_tls,position,
               created_at_micros,updated_at_micros)
           VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
           ON CONFLICT(host_id) DO UPDATE SET
             name=EXCLUDED.name,address=EXCLUDED.address,web_port=EXCLUDED.web_port,
             username=EXCLUDED.username,secret=EXCLUDED.secret,verify_tls=EXCLUDED.verify_tls,
             position=EXCLUDED.position,created_at_micros=EXCLUDED.created_at_micros,
             updated_at_micros=EXCLUDED.updated_at_micros"#,
    )
    .bind(&row.host_id)
    .bind(&row.name)
    .bind(&row.address)
    .bind(row.web_port)
    .bind(&row.username)
    .bind(&row.secret)
    .bind(row.verify_tls)
    .bind(row.position)
    .bind(row.created_at_micros)
    .bind(row.updated_at_micros)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

pub(crate) async fn update_stored(
    transaction: &mut Transaction<'_, Postgres>,
    row: &StoredHost,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE sunshine.hosts SET name=$2,address=$3,web_port=$4,username=$5,
             secret=$6,verify_tls=$7,position=$8,created_at_micros=$9,updated_at_micros=$10
           WHERE host_id=$1"#,
    )
    .bind(&row.host_id)
    .bind(&row.name)
    .bind(&row.address)
    .bind(row.web_port)
    .bind(&row.username)
    .bind(&row.secret)
    .bind(row.verify_tls)
    .bind(row.position)
    .bind(row.created_at_micros)
    .bind(row.updated_at_micros)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

pub(crate) async fn insert_audit(
    transaction: &mut Transaction<'_, Postgres>,
    action: &str,
    target: &str,
    actor: &str,
    detail: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO sunshine.audit_logs(action,target,detail,actor,created_at_micros) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(action)
    .bind(target)
    .bind(detail)
    .bind(actor)
    .bind(now_micros().unwrap_or(0))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

pub(crate) fn encode_host(host: &Host, secrets: &SecretBox) -> AppResult<StoredHost> {
    Ok(StoredHost {
        host_id: host.id.clone(),
        name: host.name.clone(),
        address: host.host.clone(),
        web_port: i32::from(host.web_port),
        username: host.username.clone(),
        secret: (!host.password.is_empty())
            .then(|| secrets.encrypt(&host.password))
            .transpose()?,
        verify_tls: host.verify_tls,
        position: host.position,
        created_at_micros: host.created_at_micros,
        updated_at_micros: host.updated_at_micros,
    })
}

pub(crate) fn decode_host(row: StoredHost, secrets: &SecretBox) -> AppResult<Host> {
    Ok(Host {
        id: row.host_id,
        name: row.name,
        host: row.address,
        web_port: u16::try_from(row.web_port)
            .map_err(|_| AppError::Internal(anyhow::anyhow!("invalid stored web_port")))?,
        username: row.username,
        password: row
            .secret
            .map(|value| secrets.decrypt(&value))
            .transpose()?
            .unwrap_or_default(),
        verify_tls: row.verify_tls,
        position: row.position,
        created_at_micros: row.created_at_micros,
        updated_at_micros: row.updated_at_micros,
    })
}

pub(crate) fn now_micros() -> anyhow::Result<i64> {
    let micros = SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros();
    i64::try_from(micros).map_err(Into::into)
}
