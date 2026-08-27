async fn open_read_only(path: &Path) -> anyhow::Result<SqliteConnection> {
    ensure_regular_file(path, "SQLite database")?;
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .create_if_missing(false)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(30));
    Ok(SqliteConnection::connect_with(&options).await?)
}

pub(super) async fn validate_database_file(path: &Path) -> anyhow::Result<i64> {
    let (expected_metadata, expected_schema) = reference_database_metadata().await?;
    validate_database_file_against(path, &expected_metadata, &expected_schema).await
}

async fn validate_database_file_against(
    path: &Path,
    expected_metadata: &SchemaMetadata,
    expected_schema: &[SchemaObject],
) -> anyhow::Result<i64> {
    let mut connection = open_read_only(path).await?;
    let rows = query("PRAGMA integrity_check")
        .fetch_all(&mut connection)
        .await?;
    for row in rows {
        let result: String = row.try_get(0)?;
        if result != "ok" {
            bail!(
                "SQLite integrity_check failed for {}: {result}",
                path.display()
            );
        }
    }
    let foreign_key_errors = query("PRAGMA foreign_key_check")
        .fetch_all(&mut connection)
        .await?;
    if !foreign_key_errors.is_empty() {
        bail!(
            "SQLite foreign_key_check found {} violation(s) in {}",
            foreign_key_errors.len(),
            path.display()
        );
    }
    let actual_metadata = load_schema_metadata(&mut connection)
        .await
        .context("database does not contain valid UnionC schema metadata")?;
    let actual_schema = load_schema_objects(&mut connection).await?;
    if actual_metadata != *expected_metadata {
        bail!(
            "unsupported UnionC SQLite schema metadata in {}: expected {:?}, found {:?}",
            path.display(),
            expected_metadata,
            actual_metadata
        );
    }
    if actual_schema != expected_schema {
        bail!(
            "UnionC SQLite schema mismatch in {}: {}",
            path.display(),
            describe_schema_mismatch(expected_schema, &actual_schema)
        );
    }

    Ok(actual_metadata.version)
}

#[derive(Debug, PartialEq, Eq)]
struct SchemaMetadata {
    version: i64,
    application_version: String,
    checksum: String,
}

#[derive(Debug, PartialEq, Eq)]
struct SchemaObject {
    object_type: String,
    name: String,
    table_name: String,
    sql: Option<String>,
}

async fn load_schema_metadata(connection: &mut SqliteConnection) -> anyhow::Result<SchemaMetadata> {
    let rows = query(
        "SELECT schema_version AS version,application_version,checksum \
         FROM schema_metadata ORDER BY schema_version",
    )
    .fetch_all(connection)
    .await?;
    if rows.len() != 1 {
        bail!(
            "schema_metadata must contain exactly one row; found {}",
            rows.len()
        );
    }
    let row = &rows[0];
    Ok(SchemaMetadata {
        version: row.try_get("version")?,
        application_version: row.try_get("application_version")?,
        checksum: row.try_get("checksum")?,
    })
}

async fn load_schema_objects(
    connection: &mut SqliteConnection,
) -> anyhow::Result<Vec<SchemaObject>> {
    query(
        r#"
        SELECT type AS object_type,name,tbl_name AS table_name,sql
        FROM sqlite_schema
        WHERE type IN ('table','index','view','trigger')
          AND name NOT LIKE 'sqlite_%'
        ORDER BY type,name,tbl_name
        "#,
    )
    .fetch_all(connection)
    .await?
    .into_iter()
    .map(|row| {
        Ok(SchemaObject {
            object_type: row.try_get("object_type")?,
            name: row.try_get("name")?,
            table_name: row.try_get("table_name")?,
            sql: row.try_get("sql")?,
        })
    })
    .collect()
}

/// Build the expected metadata and schema with the exact bundled SQLite engine
/// used to validate the live file. Comparing `sqlite_schema` catches missing,
/// additional or altered tables, columns, CHECK constraints, foreign keys,
/// STRICT declarations and explicit indexes; checking only schema version
/// would accept a damaged database whose metadata happened to survive.
async fn reference_database_metadata() -> anyhow::Result<(SchemaMetadata, Vec<SchemaObject>)> {
    let options = SqliteConnectOptions::new()
        .in_memory(true)
        .foreign_keys(true);
    let mut connection = SqliteConnection::connect_with(&options).await?;
    super::initialize_schema_inner(&mut connection).await?;
    let metadata = load_schema_metadata(&mut connection).await?;
    let schema = load_schema_objects(&mut connection).await?;
    connection.close().await?;
    Ok((metadata, schema))
}

fn describe_schema_mismatch(expected: &[SchemaObject], actual: &[SchemaObject]) -> String {
    let difference = expected
        .iter()
        .zip(actual)
        .find(|(expected, actual)| expected != actual);
    if let Some((expected, actual)) = difference {
        if expected.object_type == actual.object_type
            && expected.name == actual.name
            && expected.table_name == actual.table_name
        {
            return format!(
                "definition differs for {} {} on {}",
                expected.object_type, expected.name, expected.table_name
            );
        }
        return format!(
            "expected {} {} on {}, found {} {} on {}",
            expected.object_type,
            expected.name,
            expected.table_name,
            actual.object_type,
            actual.name,
            actual.table_name
        );
    }
    format!(
        "expected {} table/index objects, found {}",
        expected.len(),
        actual.len()
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacySunshineHostConfig {
    name: String,
    web_port: u16,
    username: String,
    verify_tls: bool,
}

pub(super) async fn validate_encrypted_values(path: &Path) -> anyhow::Result<()> {
    let mut connection = open_read_only(path).await?;
    let rows = query("SELECT config,secret FROM external_hosts")
        .fetch_all(&mut connection)
        .await?;
    for row in rows {
        let config: String = row.try_get("config")?;
        let parsed: LegacySunshineHostConfig = serde_json::from_str(&config)
            .context("backup contains invalid legacy Sunshine host configuration")?;
        let _ = (
            parsed.name,
            parsed.web_port,
            parsed.username,
            parsed.verify_tls,
        );
        if let Some(encrypted) = row.try_get::<Option<String>, _>("secret")? {
            crate::infra::secrets::decrypt(&encrypted)
                .context("backup contains a Sunshine secret that cannot be decrypted")?;
        }
    }
    Ok(())
}
