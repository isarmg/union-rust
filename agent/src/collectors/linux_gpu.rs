use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::model::{Capability, CapabilityErrorKind, GpuSnapshot};

pub(super) struct LinuxGpuResult {
    pub gpus: Vec<GpuSnapshot>,
    pub capabilities: Vec<Capability>,
}

pub(super) fn collect() -> LinuxGpuResult {
    let mut amd = Vec::new();
    let mut intel = Vec::new();
    let root = Path::new("/sys/class/drm");
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let card = entry.file_name().to_string_lossy().into_owned();
            if !is_primary_card(&card) {
                continue;
            }
            let device = entry.path().join("device");
            match read_trimmed(device.join("vendor")).as_deref() {
                Some("0x1002") => amd.push(collect_card(&card, &device, "amd")),
                Some("0x8086") => intel.push(collect_card(&card, &device, "intel")),
                _ => {}
            }
        }
    }

    let mut capabilities = vec![
        capability_for("gpu.amd", "linux-amdgpu-sysfs", &amd),
        capability_for("gpu.intel", "linux-i915/xe-sysfs", &intel),
        Capability::unavailable(
            "gpu.apple",
            "linux-drm",
            CapabilityErrorKind::NotPresent,
            "Apple GPU telemetry is only relevant on macOS",
        ),
    ];
    capabilities.extend(utilization_capability(
        "gpu.amd.utilization",
        "linux-amdgpu-sysfs",
        &amd,
    ));
    capabilities.extend(utilization_capability(
        "gpu.intel.utilization",
        "linux-i915/xe-sysfs",
        &intel,
    ));
    amd.extend(intel);
    LinuxGpuResult {
        gpus: amd,
        capabilities,
    }
}

fn utilization_capability(name: &str, source: &str, values: &[GpuSnapshot]) -> Option<Capability> {
    if values.is_empty() {
        return None;
    }
    Some(
        if values
            .iter()
            .any(|value| value.utilization_percent.is_some())
        {
            Capability::available(name, source)
        } else {
            Capability::unavailable(
                name,
                source,
                CapabilityErrorKind::Unsupported,
                "the loaded kernel driver exposed no read-only utilization counter",
            )
        },
    )
}

fn collect_card(card: &str, device: &Path, vendor: &str) -> GpuSnapshot {
    let device_code = read_trimmed(device.join("device")).unwrap_or_else(|| "unknown".into());
    let temperature_celsius = first_hwmon_f64(device, "temp1_input")
        .map(|value| value / 1_000.0)
        .filter(|value| value.is_finite());
    let core_clock_mhz = read_f64(device.join("gt_cur_freq_mhz"));
    GpuSnapshot {
        id: pci_slot(device).unwrap_or_else(|| card.to_string()),
        vendor: vendor.to_string(),
        name: format!("{} {}", vendor.to_uppercase(), device_code),
        utilization_percent: read_f64(device.join("gpu_busy_percent")),
        memory_total_bytes: read_u64(device.join("mem_info_vram_total")),
        memory_used_bytes: read_u64(device.join("mem_info_vram_used")),
        temperature_celsius,
        power_watts: first_hwmon_f64(device, "power1_average")
            .map(|microwatts| microwatts / 1_000_000.0),
        core_clock_mhz,
        memory_clock_mhz: None,
        pcie_rx_bytes_per_second: None,
        pcie_tx_bytes_per_second: None,
        source: format!("linux-{vendor}-sysfs"),
    }
}

fn capability_for(name: &str, source: &str, values: &[GpuSnapshot]) -> Capability {
    if values.is_empty() {
        Capability::unavailable(
            name,
            source,
            CapabilityErrorKind::NotPresent,
            "no matching DRM device was exposed by the kernel",
        )
    } else {
        Capability::available(name, source)
    }
}

fn is_primary_card(name: &str) -> bool {
    name.strip_prefix("card").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.chars().all(|value| value.is_ascii_digit())
    })
}

fn pci_slot(device: &Path) -> Option<String> {
    let uevent = fs::read_to_string(device.join("uevent")).ok()?;
    uevent
        .lines()
        .find_map(|line| line.strip_prefix("PCI_SLOT_NAME=").map(ToOwned::to_owned))
}

fn first_hwmon_f64(device: &Path, file_name: &str) -> Option<f64> {
    let entries = fs::read_dir(device.join("hwmon")).ok()?;
    for entry in entries.flatten() {
        if let Some(value) = read_f64(entry.path().join(file_name)) {
            return Some(value);
        }
    }
    None
}

fn read_trimmed(path: PathBuf) -> Option<String> {
    let value = fs::read_to_string(path).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn read_u64(path: PathBuf) -> Option<u64> {
    read_trimmed(path)?.parse().ok()
}

fn read_f64(path: PathBuf) -> Option<f64> {
    let value = read_trimmed(path)?.parse::<f64>().ok()?;
    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::is_primary_card;

    #[test]
    fn excludes_render_nodes_and_connectors() {
        assert!(is_primary_card("card0"));
        assert!(!is_primary_card("card0-DP-1"));
        assert!(!is_primary_card("renderD128"));
    }
}
