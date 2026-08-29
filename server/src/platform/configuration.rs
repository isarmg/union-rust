use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::RwLock;

const MAX_CONFIGURATION_BYTES: u64 = 1024 * 1024;
const RESOURCE_ANNOTATION: &str = "x-union-resource";
const POSTGRESQL_DATABASE_RESOURCE: &str = "postgresql_database";
const STORAGE_TREE_RESOURCE: &str = "storage_tree";

#[derive(Debug, Clone, PartialEq, Eq)]
struct PostgresqlResource {
    endpoint_host: String,
    endpoint_port: u16,
    database: String,
    username: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResourceIdentity {
    Postgresql(PostgresqlResource),
    StorageTree(PathBuf),
}

#[derive(Debug, Clone)]
struct ResourceClaim {
    module: String,
    identity: ResourceIdentity,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModuleConfiguration {
    pub module: String,
    pub schema_version: u32,
    pub schema: Value,
    pub configured: bool,
    /// A persisted value that no longer matches the bundled schema is retained on disk but is
    /// never exposed or injected into a worker. Administrators can replace it through the normal
    /// configuration API after an upgrade instead of being locked out of that API.
    pub validation_error: Option<String>,
    pub value: Option<Value>,
}

#[derive(Clone)]
struct ConfigurationEntry {
    schema_version: u32,
    schema: Value,
    secret_fields: Vec<String>,
    value: Option<Value>,
    validation_error: Option<String>,
}

#[derive(Clone)]
pub struct ConfigurationRegistry {
    directory: Arc<PathBuf>,
    /// Actual Core-owned roots, resolved from this process' deployment configuration. Keeping
    /// these separate from module claims avoids inventing a pseudo-module while applying the same
    /// component-aware lexical overlap rule in every read, write and injection path.
    reserved_storage_trees: Arc<Vec<PathBuf>>,
    entries: Arc<RwLock<BTreeMap<String, ConfigurationEntry>>>,
}

impl ConfigurationRegistry {
    /// Construct a registry with every Core-owned storage root that modules must not overlap.
    ///
    /// Reserved roots are normalized here because deployment paths may come from environment
    /// configuration and need not already be in the strict textual form required of module JSON.
    pub fn new(
        directory: PathBuf,
        reserved_storage_trees: impl IntoIterator<Item = PathBuf>,
    ) -> anyhow::Result<Self> {
        let mut normalized_reserved_storage_trees = Vec::new();
        for path in reserved_storage_trees {
            let path = crate::infra::paths::normalize_absolute(path)?;
            if path == Path::new("/") {
                anyhow::bail!("reserved Core storage tree must not be the filesystem root");
            }
            if !normalized_reserved_storage_trees.contains(&path) {
                normalized_reserved_storage_trees.push(path);
            }
        }
        Ok(Self {
            directory: Arc::new(directory),
            reserved_storage_trees: Arc::new(normalized_reserved_storage_trees),
            entries: Arc::new(RwLock::new(BTreeMap::new())),
        })
    }

    pub async fn register(
        &self,
        module: &str,
        schema_version: u32,
        schema: Value,
        secret_fields: Vec<String>,
    ) -> anyhow::Result<()> {
        validate_schema(&schema)?;
        for pointer in &secret_fields {
            if !valid_json_pointer(pointer) {
                anyhow::bail!("invalid secret JSON pointer for {module}: {pointer}");
            }
        }
        let path = self.configuration_path(module)?;
        let (value, validation_error) = match read_private_configuration(&path)? {
            None => (None, None),
            Some(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(value) => match validate_value(&schema, &value, "$") {
                    Ok(()) => (Some(value), None),
                    Err(error) => (None, Some(safe_validation_error(schema_version, &error))),
                },
                Err(error) => (
                    None,
                    Some(safe_validation_error(
                        schema_version,
                        &anyhow::Error::new(error),
                    )),
                ),
            },
        };
        let mut entries = self.entries.write().await;
        let (value, validation_error) = if let Some(candidate) = value {
            match validate_resource_conflicts(
                &entries,
                Some((module, &schema, &candidate)),
                &self.reserved_storage_trees,
            ) {
                Ok(()) => (Some(candidate), validation_error),
                Err(error) => (None, Some(safe_resource_validation_error(&error))),
            }
        } else {
            (None, validation_error)
        };
        entries.insert(
            module.into(),
            ConfigurationEntry {
                schema_version,
                schema,
                secret_fields,
                value,
                validation_error,
            },
        );
        Ok(())
    }

    pub async fn unregister(&self, module: &str) {
        self.entries.write().await.remove(module);
    }

    pub async fn get(&self, module: &str) -> Option<ModuleConfiguration> {
        let entries = self.entries.read().await;
        let entry = entries.get(module)?;
        let mut value = entry.value.clone();
        if let Some(value) = value.as_mut() {
            for pointer in &entry.secret_fields {
                if let Some(secret) = value.pointer_mut(pointer) {
                    *secret = Value::String("***".into());
                }
            }
        }
        Some(ModuleConfiguration {
            module: module.into(),
            schema_version: entry.schema_version,
            schema: entry.schema.clone(),
            configured: value.is_some()
                && entry.validation_error.is_none()
                && validate_resource_conflicts(&entries, None, &self.reserved_storage_trees)
                    .is_ok(),
            validation_error: entry.validation_error.clone(),
            value,
        })
    }

    pub(crate) async fn raw_value(&self, module: &str) -> Option<Value> {
        let module = module.to_owned();
        self.raw_value_owned(module).await
    }

    pub(crate) async fn raw_value_owned(&self, module: String) -> Option<Value> {
        let entries = self.entries.read().await;
        if validate_resource_conflicts(&entries, None, &self.reserved_storage_trees).is_err() {
            return None;
        }
        entries.get(&module).and_then(|entry| entry.value.clone())
    }

    pub async fn is_configured(&self, module: &str) -> bool {
        let entries = self.entries.read().await;
        entries.get(module).is_some_and(|entry| {
            entry.value.is_some()
                && entry.validation_error.is_none()
                && validate_resource_conflicts(&entries, None, &self.reserved_storage_trees).is_ok()
        })
    }

    /// Re-check every currently configured module's declared resource identities.
    ///
    /// This is a conservative typo/misconfiguration gate, not an OS isolation boundary: DNS
    /// aliases, filesystem symlinks and workers running under the same UID remain trusted
    /// deployment concerns.
    pub async fn validate_resource_isolation(&self) -> anyhow::Result<()> {
        let entries = self.entries.read().await;
        validate_resource_conflicts(&entries, None, &self.reserved_storage_trees)
    }

    pub async fn set(&self, module: &str, value: Value) -> anyhow::Result<ModuleConfiguration> {
        let mut entries = self.entries.write().await;
        {
            let entry = entries.get(module).ok_or_else(|| {
                anyhow::anyhow!("module configuration is not registered: {module}")
            })?;
            validate_value(&entry.schema, &value, "$")?;
            for pointer in &entry.secret_fields {
                if value.pointer(pointer).and_then(Value::as_str) == Some("***") {
                    anyhow::bail!(
                        "redacted placeholder cannot be persisted as secret configuration: {pointer}"
                    );
                }
            }
        }
        let schema = &entries
            .get(module)
            .expect("configuration remained registered while validating")
            .schema;
        validate_resource_conflicts(
            &entries,
            Some((module, schema, &value)),
            &self.reserved_storage_trees,
        )?;
        std::fs::create_dir_all(self.directory.as_ref())?;
        write_json_atomically(&self.configuration_path(module)?, &value)?;
        let entry = entries
            .get_mut(module)
            .expect("configuration remained registered while updating");
        entry.value = Some(value);
        entry.validation_error = None;
        drop(entries);
        self.get(module).await.ok_or_else(|| {
            anyhow::anyhow!("module configuration disappeared after update: {module}")
        })
    }

    fn configuration_path(&self, module: &str) -> anyhow::Result<PathBuf> {
        if !valid_module_id(module) {
            anyhow::bail!("invalid module id: {module}");
        }
        Ok(self.directory.join(format!("{module}.json")))
    }
}

fn read_private_configuration(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    // `File::metadata` is fstat on the already-open descriptor, so validation cannot be swapped to
    // a symlink or device between path inspection and reading.
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_CONFIGURATION_BYTES {
        anyhow::bail!("module configuration must be a regular file no larger than 1 MiB");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            anyhow::bail!("module configuration must not grant group or world permissions");
        }
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(MAX_CONFIGURATION_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIGURATION_BYTES {
        anyhow::bail!("module configuration grew beyond 1 MiB while it was read");
    }
    Ok(Some(bytes))
}

fn validate_schema(schema: &Value) -> anyhow::Result<()> {
    let object = schema
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("configuration schema must be a JSON object"))?;
    if object.get("type").and_then(Value::as_str) != Some("object") {
        anyhow::bail!("configuration schema root type must be object");
    }
    if let Some(properties) = object.get("properties")
        && !properties.is_object()
    {
        anyhow::bail!("configuration schema properties must be an object");
    }
    if let Some(required) = object.get("required") {
        let required = required
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("configuration schema required must be an array"))?;
        if required.iter().any(|name| !name.is_string()) {
            anyhow::bail!("configuration schema required values must be strings");
        }
    }
    validate_schema_node(schema, "$")
}

fn validate_schema_node(schema: &Value, path: &str) -> anyhow::Result<()> {
    let object = schema
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("configuration schema at {path} must be an object"))?;
    if let Some(resource) = object.get(RESOURCE_ANNOTATION) {
        let resource = resource.as_str().ok_or_else(|| {
            anyhow::anyhow!("configuration schema {RESOURCE_ANNOTATION} at {path} must be a string")
        })?;
        if !matches!(
            resource,
            POSTGRESQL_DATABASE_RESOURCE | STORAGE_TREE_RESOURCE
        ) {
            anyhow::bail!(
                "configuration schema {RESOURCE_ANNOTATION} at {path} has an unsupported value"
            );
        }
        if object.get("type").and_then(Value::as_str) != Some("string") {
            anyhow::bail!(
                "configuration schema resource declaration at {path} must have type string"
            );
        }
    }
    for keyword in ["minLength", "maxLength", "minItems", "maxItems"] {
        if object
            .get(keyword)
            .is_some_and(|value| value.as_u64().is_none())
        {
            anyhow::bail!(
                "configuration schema {keyword} at {path} must be a non-negative integer"
            );
        }
    }
    for keyword in ["minimum", "maximum"] {
        if object.get(keyword).is_some_and(|value| !value.is_number()) {
            anyhow::bail!("configuration schema {keyword} at {path} must be a number");
        }
    }
    if let (Some(minimum), Some(maximum)) = (
        object.get("minimum").and_then(Value::as_f64),
        object.get("maximum").and_then(Value::as_f64),
    ) && minimum > maximum
    {
        anyhow::bail!("configuration schema minimum exceeds maximum at {path}");
    }
    if let (Some(minimum), Some(maximum)) = (
        object.get("minItems").and_then(Value::as_u64),
        object.get("maxItems").and_then(Value::as_u64),
    ) && minimum > maximum
    {
        anyhow::bail!("configuration schema minItems exceeds maxItems at {path}");
    }
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        for (name, child) in properties {
            validate_schema_node(child, &format!("{path}/{name}"))?;
        }
    }
    if let Some(items) = object.get("items") {
        validate_schema_node(items, &format!("{path}/items"))?;
    }
    Ok(())
}

fn validate_value(schema: &Value, value: &Value, path: &str) -> anyhow::Result<()> {
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        anyhow::bail!("configuration value at {path} is not in enum");
    }
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        let matches = match expected {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            other => anyhow::bail!("unsupported configuration schema type: {other}"),
        };
        if !matches {
            anyhow::bail!("configuration value at {path} must be {expected}");
        }
    }
    if let Some(object) = value.as_object() {
        let properties = schema.get("properties").and_then(Value::as_object);
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for required in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(required) {
                    anyhow::bail!("configuration value at {path} is missing {required}");
                }
            }
        }
        if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false)
            && object
                .keys()
                .any(|key| properties.is_none_or(|properties| !properties.contains_key(key)))
        {
            anyhow::bail!("configuration value at {path} contains an unknown property");
        }
        if let Some(properties) = properties {
            for (key, child) in object {
                if let Some(child_schema) = properties.get(key) {
                    validate_value(child_schema, child, &format!("{path}/{key}"))?;
                }
            }
        }
    }
    if let Some(array) = value.as_array() {
        if let Some(minimum) = schema.get("minItems").and_then(Value::as_u64)
            && (array.len() as u64) < minimum
        {
            anyhow::bail!("configuration array at {path} has too few items");
        }
        if let Some(maximum) = schema.get("maxItems").and_then(Value::as_u64)
            && (array.len() as u64) > maximum
        {
            anyhow::bail!("configuration array at {path} has too many items");
        }
        if let Some(item_schema) = schema.get("items") {
            for (index, child) in array.iter().enumerate() {
                validate_value(item_schema, child, &format!("{path}/{index}"))?;
            }
        }
    }
    if let Some(number) = value.as_f64() {
        if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64)
            && number < minimum
        {
            anyhow::bail!("configuration number at {path} is below minimum");
        }
        if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64)
            && number > maximum
        {
            anyhow::bail!("configuration number at {path} exceeds maximum");
        }
    }
    if let Some(text) = value.as_str() {
        if let Some(minimum) = schema.get("minLength").and_then(Value::as_u64)
            && text.chars().count() < minimum as usize
        {
            anyhow::bail!("configuration string at {path} is too short");
        }
        if let Some(maximum) = schema.get("maxLength").and_then(Value::as_u64)
            && text.chars().count() > maximum as usize
        {
            anyhow::bail!("configuration string at {path} is too long");
        }
        if let Some(resource) = schema.get(RESOURCE_ANNOTATION).and_then(Value::as_str) {
            parse_resource_identity(resource, text, path)?;
        }
    }
    Ok(())
}

fn validate_resource_conflicts(
    entries: &BTreeMap<String, ConfigurationEntry>,
    replacement: Option<(&str, &Value, &Value)>,
    reserved_storage_trees: &[PathBuf],
) -> anyhow::Result<()> {
    let mut claims = Vec::new();
    for (module, entry) in entries {
        if replacement.is_some_and(|(replacement_module, _, _)| replacement_module == module) {
            continue;
        }
        if let Some(value) = entry.value.as_ref() {
            collect_resource_claims(module, &entry.schema, value, "$", &mut claims)?;
        }
    }
    if let Some((module, schema, value)) = replacement {
        collect_resource_claims(module, schema, value, "$", &mut claims)?;
    }

    for claim in &claims {
        let ResourceIdentity::StorageTree(path) = &claim.identity else {
            continue;
        };
        if reserved_storage_trees
            .iter()
            .any(|reserved| storage_trees_overlap(path, reserved))
        {
            anyhow::bail!(
                "configuration resource conflict: module {} declares a storage tree that overlaps reserved Core storage",
                claim.module
            );
        }
    }

    for (index, left) in claims.iter().enumerate() {
        for right in &claims[index + 1..] {
            match (&left.identity, &right.identity) {
                (
                    ResourceIdentity::Postgresql(left_database),
                    ResourceIdentity::Postgresql(right_database),
                ) if left.module != right.module => {
                    let same_endpoint = left_database.endpoint_host == right_database.endpoint_host
                        && left_database.endpoint_port == right_database.endpoint_port;
                    if same_endpoint && left_database.database == right_database.database {
                        anyhow::bail!(
                            "configuration resource conflict: modules {} and {} must use different PostgreSQL databases",
                            left.module,
                            right.module
                        );
                    }
                    if same_endpoint && left_database.username == right_database.username {
                        anyhow::bail!(
                            "configuration resource conflict: modules {} and {} must use different PostgreSQL roles",
                            left.module,
                            right.module
                        );
                    }
                }
                (
                    ResourceIdentity::StorageTree(left_path),
                    ResourceIdentity::StorageTree(right_path),
                ) if storage_trees_overlap(left_path, right_path) => {
                    if left.module == right.module {
                        anyhow::bail!(
                            "configuration resource conflict: storage tree declarations for module {} overlap",
                            left.module
                        );
                    }
                    anyhow::bail!(
                        "configuration resource conflict: modules {} and {} declare overlapping storage trees",
                        left.module,
                        right.module
                    );
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn storage_trees_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn collect_resource_claims(
    module: &str,
    schema: &Value,
    value: &Value,
    path: &str,
    claims: &mut Vec<ResourceClaim>,
) -> anyhow::Result<()> {
    if let Some(resource) = schema.get(RESOURCE_ANNOTATION).and_then(Value::as_str) {
        let text = value.as_str().ok_or_else(|| {
            anyhow::anyhow!("configuration resource declaration at {path} must be a string")
        })?;
        claims.push(ResourceClaim {
            module: module.to_string(),
            identity: parse_resource_identity(resource, text, path)?,
        });
    }
    if let (Some(object), Some(properties)) = (
        value.as_object(),
        schema.get("properties").and_then(Value::as_object),
    ) {
        for (name, child) in object {
            if let Some(child_schema) = properties.get(name) {
                collect_resource_claims(
                    module,
                    child_schema,
                    child,
                    &format!("{path}/{name}"),
                    claims,
                )?;
            }
        }
    }
    if let (Some(array), Some(item_schema)) = (value.as_array(), schema.get("items")) {
        for (index, child) in array.iter().enumerate() {
            collect_resource_claims(
                module,
                item_schema,
                child,
                &format!("{path}/{index}"),
                claims,
            )?;
        }
    }
    Ok(())
}

fn parse_resource_identity(
    kind: &str,
    value: &str,
    path: &str,
) -> anyhow::Result<ResourceIdentity> {
    match kind {
        POSTGRESQL_DATABASE_RESOURCE => Ok(ResourceIdentity::Postgresql(
            parse_postgresql_resource(value, path)?,
        )),
        STORAGE_TREE_RESOURCE => Ok(ResourceIdentity::StorageTree(parse_storage_tree(
            value, path,
        )?)),
        _ => anyhow::bail!("unsupported configuration resource declaration at {path}"),
    }
}

fn parse_postgresql_resource(value: &str, path: &str) -> anyhow::Result<PostgresqlResource> {
    let parsed = url::Url::parse(value).map_err(|_| {
        anyhow::anyhow!(
            "configuration PostgreSQL resource at {path} must be a valid postgresql:// URL"
        )
    })?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql")
        || parsed.cannot_be_a_base()
        || parsed.fragment().is_some()
    {
        anyhow::bail!(
            "configuration PostgreSQL resource at {path} must be a valid postgresql:// URL"
        );
    }
    if parsed.query_pairs().any(|(name, _)| {
        matches!(
            name.as_ref(),
            "host" | "hostaddr" | "port" | "user" | "dbname" | "database"
        )
    }) {
        anyhow::bail!(
            "configuration PostgreSQL resource at {path} must declare endpoint, role, and database in the URL authority/path"
        );
    }
    let endpoint_host = match parsed.host() {
        Some(url::Host::Domain(host)) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            if host.is_empty() || host.contains(',') {
                anyhow::bail!(
                    "configuration PostgreSQL resource at {path} must use one explicit endpoint"
                );
            }
            host
        }
        Some(url::Host::Ipv4(host)) => host.to_string(),
        Some(url::Host::Ipv6(host)) => host.to_string(),
        None => {
            anyhow::bail!("configuration PostgreSQL resource at {path} must include an endpoint")
        }
    };
    let endpoint_port = parsed.port().unwrap_or(5432);
    let username = decode_url_component(parsed.username(), path, "role")?;
    if username.is_empty() {
        anyhow::bail!("configuration PostgreSQL resource at {path} must include an explicit role");
    }
    let raw_database = parsed.path().strip_prefix('/').unwrap_or_default();
    if raw_database.is_empty() || raw_database.contains('/') {
        anyhow::bail!(
            "configuration PostgreSQL resource at {path} must include exactly one database name"
        );
    }
    let database = decode_url_component(raw_database, path, "database")?;
    if database.is_empty() {
        anyhow::bail!(
            "configuration PostgreSQL resource at {path} must include exactly one database name"
        );
    }
    Ok(PostgresqlResource {
        endpoint_host,
        endpoint_port,
        database,
        username,
    })
}

fn decode_url_component(value: &str, path: &str, label: &str) -> anyhow::Result<String> {
    let input = value.as_bytes();
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'%' {
            if index + 2 >= input.len() {
                anyhow::bail!(
                    "configuration PostgreSQL {label} at {path} has invalid percent encoding"
                );
            }
            let high = hex_value(input[index + 1]);
            let low = hex_value(input[index + 2]);
            let (Some(high), Some(low)) = (high, low) else {
                anyhow::bail!(
                    "configuration PostgreSQL {label} at {path} has invalid percent encoding"
                );
            };
            output.push((high << 4) | low);
            index += 3;
        } else {
            output.push(input[index]);
            index += 1;
        }
    }
    let decoded = String::from_utf8(output).map_err(|_| {
        anyhow::anyhow!("configuration PostgreSQL {label} at {path} must be valid UTF-8")
    })?;
    if decoded.chars().any(char::is_control) {
        anyhow::bail!("configuration PostgreSQL {label} at {path} contains control characters");
    }
    Ok(decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_storage_tree(value: &str, path: &str) -> anyhow::Result<PathBuf> {
    use std::path::Component;

    if value.chars().any(char::is_control) {
        anyhow::bail!("configuration storage tree at {path} contains control characters");
    }
    let candidate = Path::new(value);
    if !candidate.is_absolute() {
        anyhow::bail!("configuration storage tree at {path} must be an absolute path");
    }
    let mut normalized = PathBuf::new();
    let mut has_tree_component = false;
    for component in candidate.components() {
        match component {
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Normal(part) => {
                normalized.push(part);
                has_tree_component = true;
            }
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                anyhow::bail!("configuration storage tree at {path} must be lexically normalized")
            }
        }
    }
    if !has_tree_component {
        anyhow::bail!("configuration storage tree at {path} must not be the filesystem root");
    }
    if normalized.to_str() != Some(value) {
        anyhow::bail!("configuration storage tree at {path} must be lexically normalized");
    }
    Ok(normalized)
}

fn write_json_atomically(path: &Path, value: &Value) -> anyhow::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("configuration path has no parent"))?;
    let temporary = parent.join(format!(".configuration-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn valid_module_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_json_pointer(value: &str) -> bool {
    value.starts_with('/') && !value.contains("//") && !value.chars().any(char::is_control)
}

fn safe_validation_error(schema_version: u32, error: &anyhow::Error) -> String {
    format!(
        "stored configuration is incompatible with schema v{schema_version}: {}",
        bounded_safe_error(error)
    )
}

fn safe_resource_validation_error(error: &anyhow::Error) -> String {
    format!(
        "stored configuration failed Union resource isolation checks: {}",
        bounded_safe_error(error)
    )
}

fn bounded_safe_error(error: &anyhow::Error) -> String {
    const MAX_ERROR_CHARS: usize = 512;
    let mut message = format!("{error:#}")
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(MAX_ERROR_CHARS + 1)
        .collect::<String>();
    if message.chars().count() > MAX_ERROR_CHARS {
        message = message.chars().take(MAX_ERROR_CHARS).collect();
        message.push('…');
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_registry(directory: &Path) -> ConfigurationRegistry {
        ConfigurationRegistry::new(directory.to_path_buf(), []).unwrap()
    }

    #[tokio::test]
    async fn configuration_is_validated_persisted_and_redacted() {
        let directory = tempfile::tempdir().unwrap();
        let registry = test_registry(directory.path());
        registry
            .register(
                "example",
                1,
                serde_json::json!({
                    "type":"object",
                    "additionalProperties":false,
                    "required":["endpoint","token","count","targets"],
                    "properties": {
                        "endpoint":{"type":"string","minLength":1},
                        "token":{"type":"string"},
                        "count":{"type":"integer","minimum":1,"maximum":5},
                        "targets":{
                            "type":"array",
                            "minItems":1,
                            "maxItems":2,
                            "items":{"type":"string"}
                        }
                    }
                }),
                vec!["/token".into()],
            )
            .await
            .unwrap();
        let unconfigured = registry.get("example").await.unwrap();
        assert!(!unconfigured.configured);
        assert_eq!(unconfigured.value, None);
        assert!(!directory.path().join("example.json").exists());

        std::fs::write(
            directory.path().join("example.json"),
            br#"{"endpoint":"https://example.invalid","token":"secret","count":2,"targets":["one"]}"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                directory.path().join("example.json"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        registry
            .register(
                "example",
                1,
                serde_json::json!({
                    "type":"object",
                    "additionalProperties":false,
                    "required":["endpoint","token","count","targets"],
                    "properties": {
                        "endpoint":{"type":"string","minLength":1},
                        "token":{"type":"string"},
                        "count":{"type":"integer","minimum":1,"maximum":5},
                        "targets":{
                            "type":"array",
                            "minItems":1,
                            "maxItems":2,
                            "items":{"type":"string"}
                        }
                    }
                }),
                vec!["/token".into()],
            )
            .await
            .unwrap();
        let configured = registry.get("example").await.unwrap();
        assert!(configured.configured);
        assert_eq!(configured.value.unwrap()["token"], "***");
        assert!(
            registry
                .set("example", serde_json::json!({"endpoint":""}))
                .await
                .is_err()
        );
        for invalid in [
            serde_json::json!({
                "endpoint":"https://example.invalid",
                "token":"secret",
                "count":0,
                "targets":["one"]
            }),
            serde_json::json!({
                "endpoint":"https://example.invalid",
                "token":"secret",
                "count":6,
                "targets":["one"]
            }),
            serde_json::json!({
                "endpoint":"https://example.invalid",
                "token":"secret",
                "count":2,
                "targets":[]
            }),
            serde_json::json!({
                "endpoint":"https://example.invalid",
                "token":"secret",
                "count":2,
                "targets":["one","two","three"]
            }),
            serde_json::json!({
                "endpoint":"https://example.invalid",
                "token":"***",
                "count":2,
                "targets":["one"]
            }),
        ] {
            assert!(registry.set("example", invalid).await.is_err());
        }
    }

    #[tokio::test]
    async fn invalid_persisted_configuration_is_withheld_until_fully_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("example.json");
        let original = br#"{"unknown":"do-not-expose"}"#;
        std::fs::write(&path, original).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let registry = test_registry(directory.path());
        registry
            .register(
                "example",
                2,
                serde_json::json!({
                    "type":"object",
                    "additionalProperties":false,
                    "required":["endpoint"],
                    "properties":{"endpoint":{"type":"string","minLength":1}}
                }),
                vec![],
            )
            .await
            .unwrap();

        let invalid = registry.get("example").await.unwrap();
        assert!(!invalid.configured);
        assert_eq!(invalid.value, None);
        let error = invalid.validation_error.unwrap();
        assert!(error.contains("schema v2"));
        assert!(!error.contains("do-not-expose"));
        assert_eq!(std::fs::read(&path).unwrap(), original);

        registry
            .set(
                "example",
                serde_json::json!({"endpoint":"https://example.invalid"}),
            )
            .await
            .unwrap();
        let recovered = registry.get("example").await.unwrap();
        assert!(recovered.configured);
        assert_eq!(recovered.validation_error, None);
        assert_eq!(
            recovered.value.unwrap()["endpoint"],
            "https://example.invalid"
        );
    }

    fn postgresql_resource_schema() -> Value {
        serde_json::json!({
            "type":"object",
            "additionalProperties":false,
            "required":["database_url"],
            "properties": {
                "database_url": {
                    "type":"string",
                    "minLength":1,
                    "x-union-resource":"postgresql_database"
                }
            }
        })
    }

    fn storage_resource_schema() -> Value {
        serde_json::json!({
            "type":"object",
            "additionalProperties":false,
            "required":["data_dir"],
            "properties": {
                "data_dir": {
                    "type":"string",
                    "minLength":1,
                    "x-union-resource":"storage_tree"
                }
            }
        })
    }

    #[test]
    fn resource_annotation_is_closed_and_requires_a_string_property() {
        for schema in [
            serde_json::json!({
                "type":"object",
                "properties": {
                    "database_url": {
                        "type":"string",
                        "x-union-resource":"shared_database"
                    }
                }
            }),
            serde_json::json!({
                "type":"object",
                "properties": {
                    "database_url": {
                        "type":"object",
                        "x-union-resource":"postgresql_database"
                    }
                }
            }),
        ] {
            assert!(validate_schema(&schema).is_err());
        }
        validate_schema(&postgresql_resource_schema()).unwrap();
        validate_schema(&storage_resource_schema()).unwrap();
    }

    #[tokio::test]
    async fn postgresql_database_and_role_must_both_be_unique_per_endpoint() {
        let directory = tempfile::tempdir().unwrap();
        let registry = test_registry(directory.path());
        for module in ["alpha", "beta"] {
            registry
                .register(
                    module,
                    1,
                    postgresql_resource_schema(),
                    vec!["/database_url".into()],
                )
                .await
                .unwrap();
        }
        registry
            .set(
                "alpha",
                serde_json::json!({
                    "database_url":"postgres://role-alpha:alpha-secret@DB.Example.:5432/photos"
                }),
            )
            .await
            .unwrap();

        let database_conflict = registry
            .set(
                "beta",
                serde_json::json!({
                    "database_url":"postgresql://role-beta:beta-secret@db.example/photos"
                }),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(database_conflict.contains("different PostgreSQL databases"));
        assert!(!database_conflict.contains("alpha-secret"));
        assert!(!database_conflict.contains("beta-secret"));
        assert!(!database_conflict.contains("role-alpha"));
        assert!(!database_conflict.contains("photos"));

        let role_conflict = registry
            .set(
                "beta",
                serde_json::json!({
                    "database_url":"postgresql://role-alpha:beta-secret@db.example/other"
                }),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(role_conflict.contains("different PostgreSQL roles"));
        assert!(!role_conflict.contains("role-alpha"));
        assert!(!role_conflict.contains("beta-secret"));
        assert!(!role_conflict.contains("other"));

        registry
            .set(
                "beta",
                serde_json::json!({
                    "database_url":"postgresql://role-beta:beta-secret@db.example/other"
                }),
            )
            .await
            .unwrap();
        assert!(registry.is_configured("alpha").await);
        assert!(registry.is_configured("beta").await);
        registry.validate_resource_isolation().await.unwrap();
    }

    #[tokio::test]
    async fn storage_trees_must_be_normalized_absolute_and_non_overlapping() {
        let directory = tempfile::tempdir().unwrap();
        let registry = test_registry(directory.path());
        for module in ["alpha", "beta"] {
            registry
                .register(module, 1, storage_resource_schema(), vec![])
                .await
                .unwrap();
        }
        registry
            .set("alpha", serde_json::json!({"data_dir":"/srv/union/photo"}))
            .await
            .unwrap();

        for invalid in [
            "relative/data",
            "/",
            "/srv/union/../photo",
            "/srv/union/photo/",
        ] {
            assert!(
                registry
                    .set("beta", serde_json::json!({"data_dir":invalid}))
                    .await
                    .is_err()
            );
        }
        let overlap = registry
            .set(
                "beta",
                serde_json::json!({"data_dir":"/srv/union/photo/thumbs"}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(overlap.contains("overlapping storage trees"));
        assert!(!overlap.contains("/srv/union"));

        registry
            .set("beta", serde_json::json!({"data_dir":"/srv/union/dufs"}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn module_storage_must_not_overlap_reserved_core_storage() {
        let directory = tempfile::tempdir().unwrap();
        let core_data_root = directory.path().join("core/data");
        let plugin_state_root = directory.path().join("external-plugin-state");
        let registry = ConfigurationRegistry::new(
            directory.path().join("configuration"),
            [
                core_data_root.join("temporary/.."),
                plugin_state_root.clone(),
            ],
        )
        .unwrap();
        registry
            .register("example", 1, storage_resource_schema(), vec![])
            .await
            .unwrap();

        let core_ancestor = core_data_root.parent().unwrap().to_path_buf();
        for (relation, candidate) in [
            ("equal", core_data_root.clone()),
            ("ancestor", core_ancestor),
            ("descendant", core_data_root.join("module-data")),
            (
                "external plugin state",
                plugin_state_root.join("module-data"),
            ),
        ] {
            let error = registry
                .set("example", serde_json::json!({"data_dir":candidate}))
                .await
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("overlaps reserved Core storage"),
                "{relation}: {error}"
            );
            assert!(
                !error.contains(directory.path().to_str().unwrap()),
                "reserved paths must not be disclosed: {error}"
            );
        }

        registry
            .set(
                "example",
                serde_json::json!({"data_dir":directory.path().join("module-data")}),
            )
            .await
            .unwrap();
        assert!(registry.is_configured("example").await);
        registry.validate_resource_isolation().await.unwrap();
    }

    #[tokio::test]
    async fn persisted_resource_conflict_is_retained_but_not_configured_or_injected() {
        let directory = tempfile::tempdir().unwrap();
        let registry = test_registry(directory.path());
        registry
            .register(
                "alpha",
                1,
                postgresql_resource_schema(),
                vec!["/database_url".into()],
            )
            .await
            .unwrap();
        registry
            .set(
                "alpha",
                serde_json::json!({
                    "database_url":"postgresql://role-alpha:alpha-secret@db.example/shared_data"
                }),
            )
            .await
            .unwrap();

        let beta_path = directory.path().join("beta.json");
        std::fs::write(
            &beta_path,
            br#"{"database_url":"postgresql://role-beta:beta-secret@db.example/shared_data"}"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&beta_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        registry
            .register(
                "beta",
                1,
                postgresql_resource_schema(),
                vec!["/database_url".into()],
            )
            .await
            .unwrap();

        let beta = registry.get("beta").await.unwrap();
        assert!(!beta.configured);
        assert_eq!(beta.value, None);
        let error = beta.validation_error.unwrap();
        assert!(error.contains("resource isolation"));
        assert!(!error.contains("beta-secret"));
        assert!(!error.contains("role-beta"));
        assert!(!error.contains("shared_data"));
        assert_eq!(
            std::fs::read(&beta_path).unwrap(),
            br#"{"database_url":"postgresql://role-beta:beta-secret@db.example/shared_data"}"#
        );
        assert!(registry.raw_value("beta").await.is_none());
        assert!(!registry.is_configured("beta").await);
    }

    #[tokio::test]
    async fn declarations_within_one_module_may_not_overlap() {
        let directory = tempfile::tempdir().unwrap();
        let registry = test_registry(directory.path());
        registry
            .register(
                "example",
                1,
                serde_json::json!({
                    "type":"object",
                    "additionalProperties":false,
                    "required":["content","state"],
                    "properties": {
                        "content":{"type":"string","x-union-resource":"storage_tree"},
                        "state":{"type":"string","x-union-resource":"storage_tree"}
                    }
                }),
                vec![],
            )
            .await
            .unwrap();
        let error = registry
            .set(
                "example",
                serde_json::json!({
                    "content":"/srv/example",
                    "state":"/srv/example/state"
                }),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("storage tree declarations for module example overlap"));
        assert!(!error.contains("/srv/example"));
    }

    #[cfg(unix)]
    #[test]
    fn existing_configuration_rejects_symlink_oversize_and_non_private_mode() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("private.json");
        std::fs::write(&private, b"{}").unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            read_private_configuration(&private).unwrap().unwrap(),
            b"{}"
        );

        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(read_private_configuration(&private).is_err());

        let oversized = directory.path().join("oversized.json");
        std::fs::write(&oversized, vec![b' '; MAX_CONFIGURATION_BYTES as usize + 1]).unwrap();
        std::fs::set_permissions(&oversized, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_private_configuration(&oversized).is_err());

        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = directory.path().join("linked.json");
        symlink(&private, &link).unwrap();
        assert!(read_private_configuration(&link).is_err());
    }
}
