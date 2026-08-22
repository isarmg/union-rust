use std::{fs, path::Path};

use crate::model::TemperatureSnapshot;

pub(super) fn collect() -> Vec<TemperatureSnapshot> {
    let root = Path::new("/sys/class/hwmon");
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut temperatures = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let stable_device = fs::canonicalize(path.join("device"))
            .or_else(|_| fs::canonicalize(&path))
            .unwrap_or_else(|_| path.clone());
        let driver = read_trimmed(path.join("name"))
            .unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
        let Ok(files) = fs::read_dir(&path) else {
            continue;
        };
        for file in files.flatten() {
            let file_name = file.file_name().to_string_lossy().into_owned();
            let Some(index) = file_name
                .strip_prefix("temp")
                .and_then(|value| value.strip_suffix("_input"))
            else {
                continue;
            };
            let Some(celsius) = read_millidegrees(file.path()) else {
                continue;
            };
            let label = read_trimmed(path.join(format!("temp{index}_label")))
                .unwrap_or_else(|| format!("temp{index}"));
            temperatures.push(TemperatureSnapshot {
                id: format!("{}:{index}", stable_device.display()),
                label: format!("{driver} {label}"),
                celsius: Some(celsius),
                max_celsius: read_millidegrees(path.join(format!("temp{index}_max"))),
                critical_celsius: read_millidegrees(path.join(format!("temp{index}_crit"))),
                source: "linux-hwmon".to_string(),
            });
        }
    }
    temperatures.sort_by(|left, right| left.id.cmp(&right.id));
    temperatures
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    let value = fs::read_to_string(path).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn read_millidegrees(path: impl AsRef<Path>) -> Option<f64> {
    let raw = read_trimmed(path)?.parse::<f64>().ok()?;
    let value = raw / 1_000.0;
    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests {
    #[test]
    fn missing_hwmon_is_a_capability_gap_not_zero() {
        assert!(super::read_millidegrees("/definitely/missing").is_none());
    }
}
