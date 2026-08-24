use std::{
    collections::HashSet,
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

use crate::model::{Capability, CapabilityErrorKind, TemperatureSnapshot};

const HWMON_ROOT: &str = "/sys/class/hwmon";
const CAPABILITY_NAME: &str = "system.temperature";
const CAPABILITY_SOURCE: &str = "sysinfo/hwmon";

pub(super) struct LinuxHwmonResult {
    pub temperatures: Vec<TemperatureSnapshot>,
    pub capability: Capability,
}

#[derive(Debug)]
struct HwmonError {
    kind: CapabilityErrorKind,
    message: String,
}

impl HwmonError {
    fn io(action: &str, path: &Path, error: io::Error) -> Self {
        Self {
            kind: classify_io_error(&error),
            message: format!("{action} {}: {error}", path.display()),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: CapabilityErrorKind::InvalidData,
            message: message.into(),
        }
    }
}

pub(super) fn collect() -> LinuxHwmonResult {
    collect_from(Path::new(HWMON_ROOT))
}

fn collect_from(root: &Path) -> LinuxHwmonResult {
    let root_canonical = match fs::canonicalize(root) {
        Ok(path) => path,
        Err(error) if error.kind() == ErrorKind::NotFound => return empty_result(),
        Err(error) => {
            return failed_result(HwmonError::io("failed to resolve hwmon root", root, error));
        }
    };
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return empty_result(),
        Err(error) => {
            return failed_result(HwmonError::io(
                "failed to enumerate hwmon root",
                root,
                error,
            ));
        }
    };

    let mut paths = Vec::new();
    let mut errors = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                if is_hwmon_name(&name) {
                    paths.push(entry.path());
                }
            }
            Err(error) => errors.push(HwmonError::io(
                "failed to read an entry from hwmon root",
                root,
                error,
            )),
        }
    }
    paths.sort();

    let mut temperatures = Vec::new();
    for path in paths {
        collect_device(root, &root_canonical, &path, &mut temperatures, &mut errors);
    }

    temperatures.sort_by(|left, right| left.id.cmp(&right.id));
    let mut seen = HashSet::new();
    temperatures.retain(|temperature| seen.insert(temperature.id.clone()));

    let capability = match errors.into_iter().next() {
        Some(error) => Capability::unavailable(
            CAPABILITY_NAME,
            CAPABILITY_SOURCE,
            error.kind,
            error.message,
        ),
        None if temperatures.is_empty() => unsupported_capability(),
        None => Capability::available(CAPABILITY_NAME, CAPABILITY_SOURCE),
    };
    LinuxHwmonResult {
        temperatures,
        capability,
    }
}

fn collect_device(
    root: &Path,
    root_canonical: &Path,
    path: &Path,
    temperatures: &mut Vec<TemperatureSnapshot>,
    errors: &mut Vec<HwmonError>,
) {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            errors.push(HwmonError::io("failed to inspect hwmon entry", path, error));
            return;
        }
    };
    if !metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        errors.push(HwmonError::invalid(format!(
            "hwmon entry {} is neither a directory nor a symlink",
            path.display()
        )));
        return;
    }

    let canonical_path = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            errors.push(HwmonError::invalid(format!(
                "hwmon entry {} is dangling",
                path.display()
            )));
            return;
        }
        Err(error) => {
            errors.push(HwmonError::io("failed to resolve hwmon entry", path, error));
            return;
        }
    };
    if !allowed_target(root, root_canonical, &canonical_path) {
        errors.push(HwmonError::invalid(format!(
            "hwmon entry {} resolves outside the allowed device tree",
            path.display()
        )));
        return;
    }
    match fs::metadata(&canonical_path) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            errors.push(HwmonError::invalid(format!(
                "hwmon entry {} does not resolve to a directory",
                path.display()
            )));
            return;
        }
        Err(error) => {
            errors.push(HwmonError::io(
                "failed to inspect resolved hwmon entry",
                &canonical_path,
                error,
            ));
            return;
        }
    }

    let stable_device = match optional_device_path(&path.join("device")) {
        Ok(Some(device)) if allowed_target(root, root_canonical, &device) => device,
        Ok(Some(_)) => {
            errors.push(HwmonError::invalid(format!(
                "hwmon device link {} resolves outside the allowed device tree",
                path.join("device").display()
            )));
            return;
        }
        Ok(None) => canonical_path,
        Err(error) => {
            errors.push(error);
            return;
        }
    };
    let fallback_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "hwmon".to_string());
    let driver = match read_optional_trimmed(&path.join("name")) {
        Ok(Some(driver)) => driver,
        Ok(None) => fallback_name,
        Err(error) => {
            errors.push(error);
            fallback_name
        }
    };

    let files = match fs::read_dir(path) {
        Ok(files) => files,
        Err(error) => {
            errors.push(HwmonError::io(
                "failed to enumerate hwmon device",
                path,
                error,
            ));
            return;
        }
    };
    let mut inputs = Vec::new();
    for file in files {
        match file {
            Ok(file) => {
                let file_name = file.file_name();
                let name = file_name.to_string_lossy();
                if let Some(index) = temperature_input_index(&name) {
                    inputs.push((index.to_string(), file.path()));
                }
            }
            Err(error) => errors.push(HwmonError::io(
                "failed to read an entry from hwmon device",
                path,
                error,
            )),
        }
    }
    inputs.sort_by(|left, right| left.0.cmp(&right.0));

    for (index, input_path) in inputs {
        let celsius = match read_required_millidegrees(&input_path) {
            Ok(celsius) => celsius,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        let label = match read_optional_trimmed(&path.join(format!("temp{index}_label"))) {
            Ok(Some(label)) => label,
            Ok(None) => format!("temp{index}"),
            Err(error) => {
                errors.push(error);
                format!("temp{index}")
            }
        };
        let max_celsius =
            read_optional_millidegrees(&path.join(format!("temp{index}_max")), errors);
        let critical_celsius =
            read_optional_millidegrees(&path.join(format!("temp{index}_crit")), errors);
        temperatures.push(TemperatureSnapshot {
            id: format!("{}:{index}", stable_device.display()),
            label: format!("{driver} {label}"),
            celsius: Some(celsius),
            max_celsius,
            critical_celsius,
            source: "linux-hwmon".to_string(),
        });
    }
}

fn allowed_target(root: &Path, root_canonical: &Path, target: &Path) -> bool {
    if root == Path::new(HWMON_ROOT) {
        target.starts_with("/sys/devices")
    } else {
        target.starts_with(root_canonical)
    }
}

fn optional_device_path(path: &Path) -> Result<Option<PathBuf>, HwmonError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(HwmonError::io(
                "failed to inspect optional hwmon device path",
                path,
                error,
            ));
        }
    };
    if !metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        return Err(HwmonError::invalid(format!(
            "hwmon device path {} is neither a directory nor a symlink",
            path.display()
        )));
    }
    let canonical = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(HwmonError::invalid(format!(
                "hwmon device path {} is dangling",
                path.display()
            )));
        }
        Err(error) => {
            return Err(HwmonError::io(
                "failed to resolve optional hwmon device path",
                path,
                error,
            ));
        }
    };
    match fs::metadata(&canonical) {
        Ok(metadata) if metadata.is_dir() => Ok(Some(canonical)),
        Ok(_) => Err(HwmonError::invalid(format!(
            "hwmon device path {} does not resolve to a directory",
            path.display()
        ))),
        Err(error) => Err(HwmonError::io(
            "failed to inspect resolved hwmon device path",
            &canonical,
            error,
        )),
    }
}

fn read_optional_trimmed(path: &Path) -> Result<Option<String>, HwmonError> {
    let value = match read_optional_regular_file(path)? {
        Some(value) => value,
        None => return Ok(None),
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(HwmonError::invalid(format!(
            "hwmon attribute {} is empty",
            path.display()
        )));
    }
    Ok(Some(value.to_string()))
}

fn read_required_millidegrees(path: &Path) -> Result<f64, HwmonError> {
    let raw = read_regular_file(path)?;
    parse_millidegrees(path, raw.trim())
}

fn read_optional_millidegrees(path: &Path, errors: &mut Vec<HwmonError>) -> Option<f64> {
    match read_optional_regular_file(path) {
        Ok(Some(raw)) => match parse_millidegrees(path, raw.trim()) {
            Ok(value) => Some(value),
            Err(error) => {
                errors.push(error);
                None
            }
        },
        Ok(None) => None,
        Err(error) => {
            errors.push(error);
            None
        }
    }
}

fn read_optional_regular_file(path: &Path) -> Result<Option<String>, HwmonError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => read_regular_file(path).map(Some),
        Ok(_) => Err(HwmonError::invalid(format!(
            "hwmon attribute {} is not a regular file",
            path.display()
        ))),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(HwmonError::io(
            "failed to inspect hwmon attribute",
            path,
            error,
        )),
    }
}

fn read_regular_file(path: &Path) -> Result<String, HwmonError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| HwmonError::io("failed to inspect hwmon attribute", path, error))?;
    if !metadata.file_type().is_file() {
        return Err(HwmonError::invalid(format!(
            "hwmon attribute {} is not a regular file",
            path.display()
        )));
    }
    fs::read_to_string(path)
        .map_err(|error| HwmonError::io("failed to read hwmon attribute", path, error))
}

fn parse_millidegrees(path: &Path, raw: &str) -> Result<f64, HwmonError> {
    let raw = raw.parse::<f64>().map_err(|error| {
        HwmonError::invalid(format!(
            "hwmon attribute {} is not numeric: {error}",
            path.display()
        ))
    })?;
    let value = raw / 1_000.0;
    if !value.is_finite() {
        return Err(HwmonError::invalid(format!(
            "hwmon attribute {} is not finite",
            path.display()
        )));
    }
    Ok(value)
}

fn is_hwmon_name(name: &str) -> bool {
    name.strip_prefix("hwmon")
        .is_some_and(|index| !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()))
}

fn temperature_input_index(name: &str) -> Option<&str> {
    let index = name.strip_prefix("temp")?.strip_suffix("_input")?;
    (!index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())).then_some(index)
}

fn classify_io_error(error: &io::Error) -> CapabilityErrorKind {
    match error.kind() {
        ErrorKind::PermissionDenied => CapabilityErrorKind::PermissionDenied,
        ErrorKind::InvalidData => CapabilityErrorKind::InvalidData,
        _ => CapabilityErrorKind::Transient,
    }
}

fn empty_result() -> LinuxHwmonResult {
    LinuxHwmonResult {
        temperatures: Vec::new(),
        capability: unsupported_capability(),
    }
}

fn failed_result(error: HwmonError) -> LinuxHwmonResult {
    LinuxHwmonResult {
        temperatures: Vec::new(),
        capability: Capability::unavailable(
            CAPABILITY_NAME,
            CAPABILITY_SOURCE,
            error.kind,
            error.message,
        ),
    }
}

fn unsupported_capability() -> Capability {
    Capability::unavailable(
        CAPABILITY_NAME,
        CAPABILITY_SOURCE,
        CapabilityErrorKind::Unsupported,
        "the operating system or hardware exposed no readable numeric sensor",
    )
}

#[cfg(test)]
mod tests {
    use std::{
        fs, io,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use crate::model::CapabilityErrorKind;

    use super::{classify_io_error, collect_from};

    static NEXT_TREE: AtomicU64 = AtomicU64::new(0);

    struct TestTree(PathBuf);

    impl TestTree {
        fn new() -> Self {
            let sequence = NEXT_TREE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "unionc-linux-hwmon-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create isolated hwmon test root");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn sensor(&self, hwmon: u32, index: u32, input: &str) -> PathBuf {
            let path = self.0.join(format!("hwmon{hwmon}"));
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("name"), format!("driver{hwmon}")).unwrap();
            fs::write(path.join(format!("temp{index}_input")), input).unwrap();
            path
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn absent_root_and_absent_optional_limits_are_capability_gaps_not_errors() {
        let tree = TestTree::new();
        let missing = tree.path().join("missing");
        let result = collect_from(&missing);
        assert_eq!(
            result.capability.error_kind,
            Some(CapabilityErrorKind::Unsupported)
        );

        let sensor = tree.sensor(0, 1, "42000\n");
        fs::write(sensor.join("temp1_label"), "package\n").unwrap();
        let result = collect_from(tree.path());
        assert!(result.capability.available);
        assert_eq!(result.temperatures.len(), 1);
        assert_eq!(result.temperatures[0].max_celsius, None);
        assert_eq!(result.temperatures[0].critical_celsius, None);
    }

    #[test]
    fn malformed_optional_attribute_preserves_snapshot_and_reports_invalid_data() {
        let tree = TestTree::new();
        let sensor = tree.sensor(0, 1, "42000\n");
        fs::write(sensor.join("temp1_max"), "not-a-number\n").unwrap();

        let result = collect_from(tree.path());

        assert_eq!(result.temperatures.len(), 1);
        assert_eq!(result.temperatures[0].celsius, Some(42.0));
        assert_eq!(result.temperatures[0].max_celsius, None);
        assert_eq!(
            result.capability.error_kind,
            Some(CapabilityErrorKind::InvalidData)
        );
    }

    #[test]
    fn partial_sensor_failure_preserves_valid_snapshots() {
        let tree = TestTree::new();
        tree.sensor(0, 1, "51000\n");
        tree.sensor(1, 1, "malformed\n");

        let result = collect_from(tree.path());

        assert_eq!(result.temperatures.len(), 1);
        assert_eq!(result.temperatures[0].celsius, Some(51.0));
        assert!(!result.capability.available);
        assert_eq!(
            result.capability.error_kind,
            Some(CapabilityErrorKind::InvalidData)
        );
    }

    #[test]
    fn io_errors_have_stable_capability_classification() {
        assert_eq!(
            classify_io_error(&io::Error::from(io::ErrorKind::PermissionDenied)),
            CapabilityErrorKind::PermissionDenied
        );
        assert_eq!(
            classify_io_error(&io::Error::from(io::ErrorKind::InvalidData)),
            CapabilityErrorKind::InvalidData
        );
        assert_eq!(
            classify_io_error(&io::Error::from(io::ErrorKind::Other)),
            CapabilityErrorKind::Transient
        );
    }

    #[cfg(unix)]
    #[test]
    fn in_tree_hwmon_symlinks_are_allowed_but_escape_links_are_rejected() {
        use std::os::unix::fs::symlink;

        let tree = TestTree::new();
        let targets = tree.path().join("targets");
        let device = targets.join("device0");
        fs::create_dir_all(&device).unwrap();
        fs::write(device.join("name"), "driver\n").unwrap();
        fs::write(device.join("temp1_input"), "40000\n").unwrap();
        symlink(&device, tree.path().join("hwmon0")).unwrap();
        symlink(&device, tree.path().join("hwmon1")).unwrap();

        let outside = std::env::temp_dir();
        symlink(outside, tree.path().join("hwmon2")).unwrap();

        let result = collect_from(tree.path());

        assert_eq!(
            result.temperatures.len(),
            1,
            "duplicate stable IDs are removed"
        );
        assert_eq!(
            result.capability.error_kind,
            Some(CapabilityErrorKind::InvalidData)
        );
    }
}
