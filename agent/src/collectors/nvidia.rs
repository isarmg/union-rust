use nvml_wrapper::{
    Nvml,
    enum_wrappers::device::{Clock, PcieUtilCounter, TemperatureSensor},
};

use crate::model::{Capability, CapabilityErrorKind, GpuSnapshot};

pub(super) struct NvidiaCollector {
    nvml: Option<Nvml>,
    init_error: Option<String>,
}

impl NvidiaCollector {
    pub fn new() -> Self {
        match Nvml::init() {
            Ok(nvml) => Self {
                nvml: Some(nvml),
                init_error: None,
            },
            Err(error) => Self {
                nvml: None,
                init_error: Some(error.to_string()),
            },
        }
    }

    pub fn collect(&mut self) -> (Vec<GpuSnapshot>, Capability) {
        let Some(nvml) = self.nvml.as_ref() else {
            return (
                Vec::new(),
                Capability::unavailable(
                    "gpu.nvidia",
                    "nvml",
                    CapabilityErrorKind::DriverMissing,
                    self.init_error
                        .clone()
                        .unwrap_or_else(|| "NVML is unavailable".into()),
                ),
            );
        };
        let count = match nvml.device_count() {
            Ok(count) => count,
            Err(error) => {
                return (
                    Vec::new(),
                    Capability::unavailable(
                        "gpu.nvidia",
                        "nvml",
                        CapabilityErrorKind::Transient,
                        error.to_string(),
                    ),
                );
            }
        };
        if count == 0 {
            return (
                Vec::new(),
                Capability::unavailable(
                    "gpu.nvidia",
                    "nvml",
                    CapabilityErrorKind::NotPresent,
                    "NVML reported no NVIDIA devices",
                ),
            );
        }

        let mut gpus = Vec::new();
        for index in 0..count {
            let Ok(device) = nvml.device_by_index(index) else {
                continue;
            };
            let utilization = device.utilization_rates().ok();
            let memory = device.memory_info().ok();
            gpus.push(GpuSnapshot {
                id: device.uuid().unwrap_or_else(|_| format!("nvidia-{index}")),
                vendor: "nvidia".to_string(),
                name: device
                    .name()
                    .unwrap_or_else(|_| format!("NVIDIA GPU {index}")),
                utilization_percent: utilization.as_ref().map(|value| value.gpu as f64),
                memory_total_bytes: memory.as_ref().map(|value| value.total),
                memory_used_bytes: memory.as_ref().map(|value| value.used),
                temperature_celsius: device
                    .temperature(TemperatureSensor::Gpu)
                    .ok()
                    .map(|value| value as f64),
                power_watts: device
                    .power_usage()
                    .ok()
                    .map(|milliwatts| milliwatts as f64 / 1_000.0),
                core_clock_mhz: device.clock_info(Clock::Graphics).ok().map(f64::from),
                memory_clock_mhz: device.clock_info(Clock::Memory).ok().map(f64::from),
                pcie_rx_bytes_per_second: device
                    .pcie_throughput(PcieUtilCounter::Receive)
                    .ok()
                    .map(|kilobytes| f64::from(kilobytes) * 1024.0),
                pcie_tx_bytes_per_second: device
                    .pcie_throughput(PcieUtilCounter::Send)
                    .ok()
                    .map(|kilobytes| f64::from(kilobytes) * 1024.0),
                source: "nvml".to_string(),
            });
        }
        if gpus.is_empty() {
            return (
                gpus,
                Capability::unavailable(
                    "gpu.nvidia",
                    "nvml",
                    CapabilityErrorKind::Transient,
                    "NVML enumerated devices but none could be queried",
                ),
            );
        }
        (gpus, Capability::available("gpu.nvidia", "nvml"))
    }
}
