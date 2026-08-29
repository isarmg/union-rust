use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, bail};
use sarmg_platform_core::PluginManifest;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_SELECTION_BYTES: u64 = 64 * 1024;
const RELEASE_SCHEMA_VERSION: u32 = 2;
const MAX_RELEASE_METADATA_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RELEASE_FILES: u64 = 100_000;
const MAX_RELEASE_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_RELEASE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_RELEASE_PATH_BYTES: usize = 1024;
const MAX_RELEASE_DEPTH: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageSource {
    Bundled,
}

#[derive(Debug, Clone)]
pub struct SelectedPackage {
    pub manifest: PluginManifest,
    pub root: PathBuf,
    pub source: PackageSource,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveSelection {
    pub module: String,
    pub enabled: bool,
    pub generation: u64,
}

/// Read-only release package discovery plus a small writable enable/disable state directory.
/// Package bytes are supplied only by Builder as part of the Union distribution; runtime HTTP
/// operations cannot upload, select, install, upgrade or delete code.
#[derive(Debug, Clone)]
pub struct PackageStore {
    bundled: Arc<PathBuf>,
    state: Arc<PathBuf>,
    /// Production stores are pinned to the exact Builder inventory loaded before serving HTTP.
    /// Development stores deliberately omit it so source-tree fixtures and explicit overrides keep
    /// working without pretending to be immutable releases.
    release: Option<Arc<ReleaseInventory>>,
}

impl PackageStore {
    pub fn new(bundled: PathBuf, state: PathBuf) -> Self {
        Self {
            bundled: Arc::new(bundled),
            state: Arc::new(state),
            release: None,
        }
    }

    pub fn from_environment() -> anyhow::Result<Self> {
        let production = std::env::var("UNIONC_ENV").as_deref() == Ok("production");
        let distribution = distribution_root()?;
        let bundled = select_bundled_root(
            production,
            std::env::var_os("UNIONC_BUNDLED_MODULES_DIR").map(PathBuf::from),
            distribution.clone(),
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../modules"),
        )?;
        let state = match std::env::var_os("UNIONC_PLUGIN_STATE_DIR") {
            Some(value) => absolute(PathBuf::from(value))?,
            None => crate::infra::paths::data_dir().join("plugins"),
        };
        let release = if production {
            Some(Arc::new(ReleaseInventory::load(
                &distribution.context("production release root is unavailable")?,
            )?))
        } else {
            None
        };
        Ok(Self {
            bundled: Arc::new(bundled),
            state: Arc::new(state),
            release,
        })
    }

    pub fn bundled_root(&self) -> &Path {
        self.bundled.as_path()
    }

    pub fn state_root(&self) -> &Path {
        self.state.as_path()
    }

    pub fn prepare(&self) -> anyhow::Result<()> {
        for directory in [self.active_directory(), self.configuration_directory()] {
            create_private_directory(&directory)?;
        }
        Ok(())
    }

    pub fn configuration_directory(&self) -> PathBuf {
        self.state.join("configuration")
    }

    pub fn discover(&self) -> anyhow::Result<Vec<SelectedPackage>> {
        self.prepare()?;
        if let Some(release) = self.release.as_ref() {
            release.verify()?;
            if self.bundled.as_path() != release.root.join("modules") {
                bail!("production bundled-module root is not the pinned Builder release");
            }
        }
        let selections = self.read_selections()?;
        let mut result = Vec::new();
        let entries = match std::fs::read_dir(self.bundled.as_path()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(result),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if !file_type.is_dir() || file_type.is_symlink() {
                continue;
            }
            let root = entry.path();
            let manifest = match self.release.as_deref() {
                Some(release) => release.load_module_manifest(&root)?,
                None => load_manifest(&root)?,
            };
            if entry.file_name().to_str() != Some(&manifest.id) {
                bail!(
                    "bundled module directory does not match manifest id: {}",
                    root.display()
                );
            }
            let enabled = selections
                .get(&manifest.id)
                .map(|selection| selection.enabled)
                .unwrap_or(false);
            result.push(SelectedPackage {
                manifest,
                root,
                source: PackageSource::Bundled,
                enabled,
            });
        }
        result.sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));
        if let Some(release) = self.release.as_ref() {
            release.validate_discovered_modules(&result)?;
        }
        for id in selections.keys() {
            if !result.iter().any(|package| &package.manifest.id == id) {
                tracing::warn!(
                    module = id,
                    "ignoring enable state for module absent from this distribution"
                );
            }
        }
        Ok(result)
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<ActiveSelection> {
        ensure_module_id(id)?;
        if !self
            .discover()?
            .iter()
            .any(|package| package.manifest.id == id)
        {
            bail!("module is not included in this Union distribution: {id}");
        }
        let generation = self
            .selection(id)?
            .map_or(1, |selection| selection.generation.saturating_add(1));
        let selection = ActiveSelection {
            module: id.into(),
            enabled,
            generation,
        };
        self.write_selection(&selection)?;
        Ok(selection)
    }

    pub fn selection(&self, id: &str) -> anyhow::Result<Option<ActiveSelection>> {
        ensure_module_id(id)?;
        let path = self.active_directory().join(format!("{id}.json"));
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let bytes =
            read_bounded_regular_file(&path, "module enable-state file", MAX_SELECTION_BYTES)?;
        let selection: ActiveSelection = serde_json::from_slice(&bytes)?;
        if selection.module != id {
            bail!("module enable-state identity mismatch for {id}");
        }
        Ok(Some(selection))
    }

    pub fn restore_selection(
        &self,
        id: &str,
        previous: Option<&ActiveSelection>,
    ) -> anyhow::Result<()> {
        ensure_module_id(id)?;
        match previous {
            Some(previous) => self.write_selection(previous),
            None => {
                let path = self.active_directory().join(format!("{id}.json"));
                match std::fs::remove_file(path) {
                    Ok(()) => sync_directory(&self.active_directory()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error.into()),
                }
            }
        }
    }

    pub fn resolve_asset(
        &self,
        package: &SelectedPackage,
        relative: &str,
    ) -> anyhow::Result<PathBuf> {
        resolve_bundle_path(&package.root, relative)
    }

    fn read_selections(&self) -> anyhow::Result<BTreeMap<String, ActiveSelection>> {
        let mut result = BTreeMap::new();
        for entry in std::fs::read_dir(self.active_directory())? {
            let entry = entry?;
            if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            if !entry.file_type()?.is_file() {
                bail!("module enable-state entry must be a non-symlink regular file");
            }
            let bytes = read_bounded_regular_file(
                &entry.path(),
                "module enable-state file",
                MAX_SELECTION_BYTES,
            )?;
            let selection: ActiveSelection = serde_json::from_slice(&bytes)?;
            ensure_module_id(&selection.module)?;
            if result.insert(selection.module.clone(), selection).is_some() {
                bail!("duplicate module enable state");
            }
        }
        Ok(result)
    }

    fn write_selection(&self, selection: &ActiveSelection) -> anyhow::Result<()> {
        self.prepare()?;
        ensure_module_id(&selection.module)?;
        write_json_atomically(
            &self
                .active_directory()
                .join(format!("{}.json", selection.module)),
            selection,
        )
    }

    fn active_directory(&self) -> PathBuf {
        self.state.join("active")
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredReleaseManifest {
    schema_version: u32,
    distribution: StoredReleaseDistribution,
    modules: Vec<StoredReleaseModule>,
    activation_order: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredReleaseDistribution {
    name: String,
    version: String,
    revision: String,
    platform: String,
    architecture: String,
    executable: String,
    web_shell: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredReleaseModule {
    id: String,
    version: String,
    revision: String,
    package: String,
    manifest: String,
}

#[derive(Debug)]
struct ReleaseInventory {
    root: PathBuf,
    release_manifest: Vec<u8>,
    checksum_manifest: Vec<u8>,
    checksums: BTreeMap<String, String>,
    modules: BTreeMap<String, StoredReleaseModule>,
}

impl ReleaseInventory {
    fn load(root: &Path) -> anyhow::Result<Self> {
        validate_real_directory("production release", root)?;
        let release_manifest = read_bounded_regular_file(
            &root.join("union-release.json"),
            "Builder release manifest",
            MAX_RELEASE_METADATA_BYTES,
        )?;
        let stored: StoredReleaseManifest =
            serde_json::from_slice(&release_manifest).context("parse Builder release manifest")?;
        if stored.schema_version != RELEASE_SCHEMA_VERSION {
            bail!(
                "unsupported Builder release schema: {}",
                stored.schema_version
            );
        }
        if stored.distribution.version != env!("CARGO_PKG_VERSION") {
            bail!(
                "Builder release/Core version mismatch: release={}, core={}",
                stored.distribution.version,
                env!("CARGO_PKG_VERSION")
            );
        }
        validate_release_platform(
            &stored.distribution.platform,
            &stored.distribution.architecture,
        )?;
        validate_release_name(&stored.distribution.name)?;
        validate_revision(&stored.distribution.revision)?;
        validate_release_relative_path(&stored.distribution.executable)?;
        validate_release_relative_path(&stored.distribution.web_shell)?;
        if !Path::new(&stored.distribution.executable).starts_with("bin") {
            bail!("Builder Core executable must be below bin");
        }
        if !Path::new(&stored.distribution.web_shell).starts_with("share/union/web") {
            bail!("Builder Web Shell must be below share/union/web");
        }
        validate_release_executable(&root.join(&stored.distribution.executable))?;
        validate_real_directory(
            "Builder Web Shell",
            &root.join(&stored.distribution.web_shell),
        )?;

        let mut modules = BTreeMap::new();
        for module in stored.modules {
            ensure_module_id(&module.id)?;
            validate_release_version(&module.version)
                .with_context(|| format!("invalid release module version for {}", module.id))?;
            validate_revision(&module.revision)?;
            let expected_package = format!("modules/{}", module.id);
            let expected_manifest = format!("{expected_package}/manifest.json");
            if module.package != expected_package || module.manifest != expected_manifest {
                bail!(
                    "release module {} has non-canonical package paths",
                    module.id
                );
            }
            if modules.insert(module.id.clone(), module).is_some() {
                bail!("duplicate module in Builder release manifest");
            }
        }
        let activation_count = stored.activation_order.len();
        let activation = stored.activation_order.into_iter().collect::<BTreeSet<_>>();
        let module_ids = modules.keys().cloned().collect::<BTreeSet<_>>();
        if activation_count != module_ids.len()
            || activation.len() != activation_count
            || activation != module_ids
        {
            bail!("Builder release activation_order does not match its module inventory");
        }

        let checksum_manifest = read_bounded_regular_file(
            &root.join("SHA256SUMS"),
            "Builder checksum manifest",
            MAX_RELEASE_METADATA_BYTES,
        )?;
        let checksums = parse_release_checksums(&checksum_manifest)?;
        for required in [
            "union-release.json",
            stored.distribution.executable.as_str(),
        ] {
            if !checksums.contains_key(required) {
                bail!("Builder SHA256SUMS omits required release file: {required}");
            }
        }
        let web_prefix = format!("{}/", stored.distribution.web_shell.trim_end_matches('/'));
        if !checksums.keys().any(|path| path.starts_with(&web_prefix)) {
            bail!("Builder SHA256SUMS omits the declared Web Shell");
        }
        for module in modules.values() {
            if !checksums.contains_key(&module.manifest) {
                bail!(
                    "Builder SHA256SUMS omits manifest for release module {}",
                    module.id
                );
            }
        }
        let inventory = Self {
            root: root.to_path_buf(),
            release_manifest,
            checksum_manifest,
            checksums,
            modules,
        };
        inventory.verify()?;
        Ok(inventory)
    }

    /// Re-read and hash the current release against the bytes pinned at process construction.
    /// A runtime rescan can therefore detect corruption, but it cannot adopt a replacement release
    /// manifest, replacement checksum list, or a newly-created module directory.
    fn verify(&self) -> anyhow::Result<()> {
        if read_bounded_regular_file(
            &self.root.join("union-release.json"),
            "Builder release manifest",
            MAX_RELEASE_METADATA_BYTES,
        )? != self.release_manifest
        {
            bail!("Builder release manifest changed after startup");
        }
        if read_bounded_regular_file(
            &self.root.join("SHA256SUMS"),
            "Builder checksum manifest",
            MAX_RELEASE_METADATA_BYTES,
        )? != self.checksum_manifest
        {
            bail!("Builder checksum manifest changed after startup");
        }

        let files = scan_release_tree(&self.root)?;
        let actual = files
            .keys()
            .filter(|path| path.as_str() != "SHA256SUMS")
            .cloned()
            .collect::<BTreeSet<_>>();
        let expected = self.checksums.keys().cloned().collect::<BTreeSet<_>>();
        if actual != expected {
            bail!("Builder SHA256SUMS inventory does not exactly match release files");
        }
        validate_module_directory_inventory(&self.root.join("modules"), self.modules.keys())?;
        for (relative, expected) in &self.checksums {
            let path = files
                .get(relative)
                .with_context(|| format!("release file disappeared: {relative}"))?;
            if sha256(path)? != *expected {
                bail!("release checksum mismatch for {relative}");
            }
        }
        Ok(())
    }

    fn validate_discovered_modules(&self, packages: &[SelectedPackage]) -> anyhow::Result<()> {
        let discovered = packages
            .iter()
            .map(|package| package.manifest.id.as_str())
            .collect::<BTreeSet<_>>();
        let expected = self
            .modules
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if discovered != expected {
            bail!("runtime module directory set differs from the pinned Builder release");
        }
        for package in packages {
            let expected = self
                .modules
                .get(&package.manifest.id)
                .context("discovered module is absent from pinned Builder release")?;
            if package.manifest.version != expected.version
                || package.manifest.version_metadata.source_revision.as_deref()
                    != Some(expected.revision.as_str())
            {
                bail!(
                    "module {} identity/version/revision differs from Builder release",
                    package.manifest.id
                );
            }
        }
        Ok(())
    }

    fn load_module_manifest(&self, root: &Path) -> anyhow::Result<PluginManifest> {
        validate_package_directory(root)?;
        let id = root
            .file_name()
            .and_then(|value| value.to_str())
            .context("release module directory name is not UTF-8")?;
        let expected = self
            .modules
            .get(id)
            .with_context(|| format!("module directory is absent from Builder release: {id}"))?;
        if root != self.root.join(&expected.package) {
            bail!("module package path differs from Builder release: {id}");
        }
        let path = root.join("manifest.json");
        let bytes = read_bounded_regular_file(&path, "module manifest", MAX_MANIFEST_BYTES)?;
        let expected_digest = self
            .checksums
            .get(&expected.manifest)
            .context("module manifest is absent from Builder checksums")?;
        if sha256_bytes(&bytes) != *expected_digest {
            bail!("release checksum mismatch for {}", expected.manifest);
        }
        parse_manifest_bytes(&bytes, &path)
    }
}

fn validate_module_directory_inventory<'a>(
    root: &Path,
    expected: impl Iterator<Item = &'a String>,
) -> anyhow::Result<()> {
    validate_real_directory("release module directory", root)?;
    let expected = expected.map(String::as_str).collect::<BTreeSet<_>>();
    let mut actual = BTreeSet::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_dir() || file_type.is_symlink() {
            bail!(
                "release modules directory contains a non-package entry: {}",
                entry.path().display()
            );
        }
        let id = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("release module directory name is not UTF-8"))?;
        ensure_module_id(&id)?;
        actual.insert(id);
    }
    if actual.iter().map(String::as_str).collect::<BTreeSet<_>>() != expected {
        bail!("release module directory set differs from Builder release manifest");
    }
    Ok(())
}

fn validate_release_name(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > 100
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        bail!("invalid Builder release name");
    }
    Ok(())
}

fn validate_release_platform(platform: &str, architecture: &str) -> anyhow::Result<()> {
    let expected_architecture = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => bail!("unsupported Union Core architecture: {other}"),
    };
    if std::env::consts::OS != "linux" {
        bail!("unsupported Union Core platform: {}", std::env::consts::OS);
    }
    if platform != "linux" || architecture != expected_architecture {
        bail!(
            "Builder release target mismatch: release={platform}/{architecture}, core=linux/{expected_architecture}"
        );
    }
    Ok(())
}

fn validate_release_version(value: &str) -> anyhow::Result<()> {
    let parts = value.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || !parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        bail!("release version must be MAJOR.MINOR.PATCH: {value}");
    }
    Ok(())
}

fn validate_release_executable(path: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("missing Builder Core executable: {}", path.display()))?;
    if !metadata.file_type().is_file() {
        bail!(
            "Builder Core executable must be a non-symlink regular file: {}",
            path.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            bail!("Builder Core executable has no executable bit");
        }
    }
    Ok(())
}

fn validate_revision(value: &str) -> anyhow::Result<()> {
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("release revision must be a full 40-character Git object id");
    }
    Ok(())
}

fn validate_release_relative_path(value: &str) -> anyhow::Result<()> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("unsafe path in Builder release manifest: {value}");
    }
    Ok(())
}

fn parse_release_checksums(value: &[u8]) -> anyhow::Result<BTreeMap<String, String>> {
    let value = std::str::from_utf8(value).context("Builder SHA256SUMS is not UTF-8")?;
    if value.is_empty() {
        bail!("Builder SHA256SUMS is empty");
    }
    let mut checksums = BTreeMap::new();
    for (index, line) in value.lines().enumerate() {
        let (digest, path) = line
            .split_once("  ")
            .with_context(|| format!("invalid SHA256SUMS line {}", index + 1))?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            bail!("invalid SHA-256 on line {}", index + 1);
        }
        validate_release_relative_path(path)?;
        if path == "SHA256SUMS" {
            bail!("SHA256SUMS must not hash itself");
        }
        if checksums
            .insert(path.to_owned(), digest.to_owned())
            .is_some()
        {
            bail!("duplicate checksum path: {path}");
        }
    }
    Ok(checksums)
}

fn validate_real_directory(label: &str, path: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("missing {label}: {}", path.display()))?;
    if !metadata.file_type().is_dir() {
        bail!("{label} must be a real directory: {}", path.display());
    }
    Ok(())
}

fn read_bounded_regular_file(path: &Path, label: &str, maximum: u64) -> anyhow::Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("missing {label}: {}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.len() > maximum {
        bail!("{label} must be a bounded non-symlink regular file");
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() as u64 != metadata.len() {
        bail!("{label} changed while it was read");
    }
    Ok(bytes)
}

fn scan_release_tree(root: &Path) -> anyhow::Result<BTreeMap<String, PathBuf>> {
    validate_real_directory("production release", root)?;
    let mut files = BTreeMap::new();
    let mut count = 0_u64;
    let mut bytes = 0_u64;
    scan_release_directory(root, root, 0, &mut count, &mut bytes, &mut files)?;
    Ok(files)
}

fn scan_release_directory(
    root: &Path,
    directory: &Path,
    depth: usize,
    count: &mut u64,
    bytes: &mut u64,
    files: &mut BTreeMap<String, PathBuf>,
) -> anyhow::Result<()> {
    if depth > MAX_RELEASE_DEPTH {
        bail!("release tree exceeds maximum depth");
    }
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .context("release entry escaped release root")?;
        if relative.as_os_str().as_encoded_bytes().len() > MAX_RELEASE_PATH_BYTES {
            bail!(
                "release path exceeds maximum length: {}",
                relative.display()
            );
        }
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            bail!(
                "release tree may not contain symlinks: {}",
                relative.display()
            );
        }
        if kind.is_dir() {
            scan_release_directory(root, &path, depth + 1, count, bytes, files)?;
        } else if kind.is_file() {
            let length = entry.metadata()?.len();
            if length > MAX_RELEASE_FILE_BYTES {
                bail!("release file exceeds maximum size: {}", relative.display());
            }
            *count = count
                .checked_add(1)
                .context("release file count overflow")?;
            *bytes = bytes.checked_add(length).context("release size overflow")?;
            if *count > MAX_RELEASE_FILES || *bytes > MAX_RELEASE_BYTES {
                bail!("release tree exceeds bounded inventory limits");
            }
            let relative = relative
                .to_str()
                .context("release path is not valid UTF-8")?
                .replace('\\', "/");
            if files.insert(relative.clone(), path).is_some() {
                bail!("duplicate release path: {relative}");
            }
        } else {
            bail!(
                "release tree contains a special file: {}",
                relative.display()
            );
        }
    }
    Ok(())
}

fn sha256(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(encode_sha256(hasher.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    encode_sha256(hasher.finalize())
}

fn encode_sha256(digest: impl IntoIterator<Item = u8>) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn select_bundled_root(
    production: bool,
    override_root: Option<PathBuf>,
    distribution: Option<PathBuf>,
    development_fallback: PathBuf,
) -> anyhow::Result<PathBuf> {
    if production {
        if override_root.is_some() {
            bail!(
                "UNIONC_BUNDLED_MODULES_DIR is forbidden in production; modules must come from the current Builder release"
            );
        }
        return distribution.map(|root| root.join("modules")).context(
            "production Union executable must run from <release>/bin to locate bundled modules",
        );
    }
    match override_root {
        Some(root) => absolute(root),
        None => Ok(distribution
            .map(|root| root.join("modules"))
            .unwrap_or(development_fallback)),
    }
}

pub fn resolve_bundle_path(root: &Path, relative: &str) -> anyhow::Result<PathBuf> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!(
            "bundle path must be a safe relative path: {}",
            relative.display()
        );
    }
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("failed to resolve package root {}", root.display()))?;
    let candidate = root.join(relative);
    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("missing bundle path {}", candidate.display()))?;
    if !canonical.starts_with(&canonical_root) {
        bail!("bundle path escapes package root: {}", candidate.display());
    }
    let mut cursor = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            unreachable!("components were validated above")
        };
        cursor.push(component);
        if std::fs::symlink_metadata(&cursor)?.file_type().is_symlink() {
            bail!(
                "symbolic links are not allowed in plugin packages: {}",
                cursor.display()
            );
        }
    }
    Ok(canonical)
}

fn load_manifest(root: &Path) -> anyhow::Result<PluginManifest> {
    validate_package_directory(root)?;
    let path = root.join("manifest.json");
    let bytes = read_bounded_regular_file(&path, "plugin manifest", MAX_MANIFEST_BYTES)?;
    parse_manifest_bytes(&bytes, &path)
}

fn parse_manifest_bytes(bytes: &[u8], path: &Path) -> anyhow::Result<PluginManifest> {
    let value = std::str::from_utf8(bytes)
        .with_context(|| format!("plugin manifest is not UTF-8: {}", path.display()))?;
    PluginManifest::parse_json(value).map_err(Into::into)
}

fn validate_package_directory(path: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("missing plugin package directory {}", path.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!(
            "plugin package must be a non-symlink directory: {}",
            path.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o022 != 0 {
            bail!(
                "plugin package directory must not be group/world writable: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn ensure_module_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id.starts_with(|character: char| character.is_ascii_lowercase())
        || !id.ends_with(|character: char| character.is_ascii_alphanumeric())
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("invalid module id: {id}");
    }
    Ok(())
}

fn write_json_atomically(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path.parent().context("module-state path has no parent")?;
    let temporary = parent.join(format!(".active-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    sync_directory(parent)
}

fn create_private_directory(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(path)?;
        if std::fs::symlink_metadata(path)?.permissions().mode() & 0o022 != 0 {
            bail!(
                "plugin state directory is group/world writable: {}",
                path.display()
            );
        }
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)?;
    Ok(())
}

fn sync_directory(path: &Path) -> anyhow::Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn absolute(path: PathBuf) -> anyhow::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn distribution_root() -> anyhow::Result<Option<PathBuf>> {
    let executable = std::env::current_exe()?;
    let Some(bin) = executable.parent() else {
        return Ok(None);
    };
    if bin.file_name().and_then(|name| name.to_str()) != Some("bin") {
        return Ok(None);
    }
    Ok(bin.parent().map(Path::to_path_buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODULE_REVISION: &str = "1111111111111111111111111111111111111111";
    const DISTRIBUTION_REVISION: &str = "2222222222222222222222222222222222222222";

    fn module_fixture() -> &'static str {
        include_str!("../../testdata/module-fixture/manifest.json")
    }

    fn copy_fixture(destination: &Path) {
        std::fs::create_dir_all(destination).unwrap();
        let source = module_fixture();
        let manifest = PluginManifest::parse_json(source).unwrap();
        std::fs::write(destination.join("manifest.json"), source).unwrap();
        let schema = destination.join(&manifest.configuration.schema);
        std::fs::create_dir_all(schema.parent().unwrap()).unwrap();
        std::fs::write(schema, r#"{"type":"object","additionalProperties":true}"#).unwrap();
    }

    fn write_production_release(root: &Path, manifest_revision: &str) {
        let module = root.join("modules/fixture-module");
        std::fs::create_dir_all(&module).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("share/union/web")).unwrap();

        let mut manifest: serde_json::Value = serde_json::from_str(module_fixture()).unwrap();
        manifest["version_metadata"]["source_revision"] = manifest_revision.into();
        std::fs::write(
            module.join("manifest.json"),
            format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
        )
        .unwrap();
        std::fs::write(root.join("bin/unionc"), b"test-core").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.join("bin/unionc"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        std::fs::write(root.join("share/union/web/index.html"), b"test-shell").unwrap();
        let release = serde_json::json!({
            "schema_version": RELEASE_SCHEMA_VERSION,
            "distribution": {
                "name": "unionc-test",
                "version": env!("CARGO_PKG_VERSION"),
                "revision": DISTRIBUTION_REVISION,
                "platform": "linux",
                "architecture": match std::env::consts::ARCH {
                    "x86_64" => "amd64",
                    "aarch64" => "arm64",
                    other => panic!("unsupported test architecture: {other}"),
                },
                "executable": "bin/unionc",
                "web_shell": "share/union/web"
            },
            "modules": [{
                "id": "fixture-module",
                "version": manifest["version"],
                "revision": MODULE_REVISION,
                "package": "modules/fixture-module",
                "manifest": "modules/fixture-module/manifest.json"
            }],
            "activation_order": ["fixture-module"]
        });
        std::fs::write(
            root.join("union-release.json"),
            format!("{}\n", serde_json::to_string_pretty(&release).unwrap()),
        )
        .unwrap();
        write_release_checksums(root);
    }

    fn write_release_checksums(root: &Path) {
        let files = scan_release_tree(root).unwrap();
        let mut checksums = String::new();
        for (relative, path) in files {
            if relative != "SHA256SUMS" {
                checksums.push_str(&format!("{}  {relative}\n", sha256(&path).unwrap()));
            }
        }
        std::fs::write(root.join("SHA256SUMS"), checksums).unwrap();
    }

    fn production_store(release: &Path, state: PathBuf) -> PackageStore {
        PackageStore {
            bundled: Arc::new(release.join("modules")),
            state: Arc::new(state),
            release: Some(Arc::new(ReleaseInventory::load(release).unwrap())),
        }
    }

    #[test]
    fn bundled_membership_is_immutable_but_enable_state_is_writable() {
        let temporary = tempfile::tempdir().unwrap();
        let bundled = temporary.path().join("bundled");
        copy_fixture(&bundled.join("fixture-module"));
        let store = PackageStore::new(bundled, temporary.path().join("state"));
        assert!(!store.discover().unwrap()[0].enabled);
        assert!(store.set_enabled("fixture-module", true).unwrap().enabled);
        assert!(store.discover().unwrap()[0].enabled);
        assert!(!store.set_enabled("fixture-module", false).unwrap().enabled);
        assert!(!store.discover().unwrap()[0].enabled);
        assert!(store.set_enabled("not-bundled", true).is_err());
    }

    #[test]
    fn bundle_path_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let package = temporary.path().join("package");
        std::fs::create_dir(&package).unwrap();
        symlink("/etc/passwd", package.join("escaped")).unwrap();
        assert!(resolve_bundle_path(&package, "escaped").is_err());
        assert!(resolve_bundle_path(&package, "../outside").is_err());
    }

    #[test]
    fn enable_state_rejects_symlink_and_oversized_json() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let store = PackageStore::new(
            temporary.path().join("bundled"),
            temporary.path().join("state"),
        );
        store.prepare().unwrap();
        let target = temporary.path().join("target.json");
        std::fs::write(
            &target,
            br#"{"module":"example","enabled":true,"generation":1}"#,
        )
        .unwrap();
        let state = store.active_directory().join("example.json");
        symlink(&target, &state).unwrap();
        assert!(store.selection("example").is_err());

        std::fs::remove_file(&state).unwrap();
        std::fs::write(&state, vec![b' '; MAX_SELECTION_BYTES as usize + 1]).unwrap();
        assert!(store.selection("example").is_err());
    }

    #[test]
    fn production_package_root_is_fixed_to_the_current_release() {
        let release = PathBuf::from("/opt/union/releases/0.6.0");
        assert_eq!(
            select_bundled_root(
                true,
                None,
                Some(release.clone()),
                PathBuf::from("source/modules"),
            )
            .unwrap(),
            release.join("modules")
        );
        assert!(
            select_bundled_root(
                true,
                Some(PathBuf::from("/tmp/untrusted")),
                Some(release),
                PathBuf::from("source/modules"),
            )
            .is_err()
        );
        assert!(select_bundled_root(true, None, None, PathBuf::from("source/modules")).is_err());
        assert_eq!(
            select_bundled_root(
                false,
                Some(PathBuf::from("/tmp/development-modules")),
                None,
                PathBuf::from("source/modules"),
            )
            .unwrap(),
            PathBuf::from("/tmp/development-modules")
        );
    }

    #[test]
    fn production_discovery_accepts_the_exact_builder_inventory() {
        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        write_production_release(&release, MODULE_REVISION);
        let store = production_store(&release, temporary.path().join("state"));

        let modules = store.discover().unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].manifest.id, "fixture-module");
        assert_eq!(
            modules[0]
                .manifest
                .version_metadata
                .source_revision
                .as_deref(),
            Some(MODULE_REVISION)
        );
    }

    #[test]
    fn production_release_rejects_a_different_platform_or_architecture() {
        for (field, value) in [("platform", "windows"), ("architecture", "mismatched")] {
            let temporary = tempfile::tempdir().unwrap();
            let release = temporary.path().join("release");
            write_production_release(&release, MODULE_REVISION);
            let path = release.join("union-release.json");
            let mut manifest: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            manifest["distribution"][field] = value.into();
            std::fs::write(
                path,
                format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
            )
            .unwrap();
            write_release_checksums(&release);

            let error = ReleaseInventory::load(&release).unwrap_err().to_string();
            assert!(error.contains("release target mismatch"), "{error}");
        }
    }

    #[test]
    fn production_discovery_rejects_files_and_module_directories_outside_inventory() {
        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        write_production_release(&release, MODULE_REVISION);
        let store = production_store(&release, temporary.path().join("state"));
        std::fs::write(release.join("unexpected"), b"not inventoried").unwrap();
        assert!(
            store
                .discover()
                .unwrap_err()
                .to_string()
                .contains("inventory")
        );

        std::fs::remove_file(release.join("unexpected")).unwrap();
        std::fs::create_dir(release.join("modules/injected")).unwrap();
        assert!(
            store
                .discover()
                .unwrap_err()
                .to_string()
                .contains("module directory set")
        );
    }

    #[test]
    fn production_discovery_rejects_changed_package_bytes() {
        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        write_production_release(&release, MODULE_REVISION);
        let store = production_store(&release, temporary.path().join("state"));
        std::fs::write(release.join("modules/fixture-module/manifest.json"), b"{}").unwrap();
        assert!(
            store
                .discover()
                .unwrap_err()
                .to_string()
                .contains("checksum mismatch")
        );
    }

    #[test]
    fn production_discovery_rejects_a_symlink_added_to_the_release() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        write_production_release(&release, MODULE_REVISION);
        let store = production_store(&release, temporary.path().join("state"));
        symlink(
            "/etc/passwd",
            release.join("modules/fixture-module/escaped"),
        )
        .unwrap();
        assert!(
            store
                .discover()
                .unwrap_err()
                .to_string()
                .contains("symlink")
        );
    }

    #[test]
    fn checksum_manifest_rejects_path_escape() {
        let digest = "0".repeat(64);
        assert!(parse_release_checksums(format!("{digest}  ../outside\n").as_bytes()).is_err());
    }

    #[test]
    fn production_rescan_cannot_adopt_a_replaced_release_inventory() {
        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        write_production_release(&release, MODULE_REVISION);
        let store = production_store(&release, temporary.path().join("state"));
        let path = release.join("union-release.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["distribution"]["name"] = "replacement".into();
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&value).unwrap()),
        )
        .unwrap();
        write_release_checksums(&release);
        assert!(
            store
                .discover()
                .unwrap_err()
                .to_string()
                .contains("release manifest changed")
        );
    }

    #[test]
    fn production_discovery_checks_manifest_revision_against_release_metadata() {
        let temporary = tempfile::tempdir().unwrap();
        let release = temporary.path().join("release");
        write_production_release(&release, DISTRIBUTION_REVISION);
        let store = production_store(&release, temporary.path().join("state"));
        assert!(
            store
                .discover()
                .unwrap_err()
                .to_string()
                .contains("identity/version/revision")
        );
    }
}
