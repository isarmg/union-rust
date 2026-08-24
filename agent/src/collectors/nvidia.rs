use nvml_wrapper::{
    Nvml,
    enum_wrappers::device::{Clock, PcieUtilCounter, TemperatureSensor},
    error::NvmlError,
};
use std::time::{Duration, Instant};

use crate::model::{Capability, CapabilityErrorKind, GpuSnapshot};

const NVML_INIT_RETRY_INITIAL: Duration = Duration::from_secs(30);
const NVML_INIT_RETRY_MAX: Duration = Duration::from_secs(15 * 60);

struct RetryingInit<T, E> {
    value: Option<T>,
    last_error: Option<E>,
    next_retry_at: Option<Instant>,
    retry_delay: Duration,
}

impl<T, E> RetryingInit<T, E> {
    fn new() -> Self {
        Self {
            value: None,
            last_error: None,
            next_retry_at: None,
            retry_delay: NVML_INIT_RETRY_INITIAL,
        }
    }

    fn initialize_if_due(&mut self, now: Instant, init: impl FnOnce() -> Result<T, E>) {
        if self.value.is_none() {
            if self.next_retry_at.is_some_and(|retry_at| now < retry_at) {
                return;
            }

            match init() {
                Ok(value) => {
                    self.value = Some(value);
                    self.last_error = None;
                    self.next_retry_at = None;
                    self.retry_delay = NVML_INIT_RETRY_INITIAL;
                }
                Err(error) => {
                    self.last_error = Some(error);
                    self.next_retry_at = now.checked_add(self.retry_delay);
                    self.retry_delay = self.retry_delay.saturating_mul(2).min(NVML_INIT_RETRY_MAX);
                }
            }
        }
    }

    fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }

    fn last_error(&self) -> Option<&E> {
        self.last_error.as_ref()
    }
}

pub(super) struct NvidiaCollector {
    nvml: RetryingInit<Nvml, NvmlError>,
}

impl NvidiaCollector {
    pub fn new() -> Self {
        let mut nvml = RetryingInit::new();
        nvml.initialize_if_due(Instant::now(), Nvml::init);
        Self { nvml }
    }

    pub fn collect(&mut self) -> (Vec<GpuSnapshot>, Capability) {
        self.nvml.initialize_if_due(Instant::now(), Nvml::init);
        let Some(nvml) = self.nvml.value() else {
            let capability = self.nvml.last_error().map_or_else(
                || {
                    Capability::unavailable(
                        "gpu.nvidia",
                        "nvml",
                        CapabilityErrorKind::Transient,
                        "NVML initialization has not completed",
                    )
                },
                nvml_error_capability,
            );
            return (Vec::new(), capability);
        };
        let count = match nvml.device_count() {
            Ok(count) => count,
            Err(error) => {
                return (Vec::new(), nvml_error_capability(&error));
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
        let mut first_device_error = None;
        for index in 0..count {
            let device = match nvml.device_by_index(index) {
                Ok(device) => device,
                Err(error) => {
                    first_device_error.get_or_insert(error);
                    continue;
                }
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
            if let Some(error) = first_device_error {
                return (gpus, nvml_error_capability(&error));
            }
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

fn nvml_error_capability(error: &NvmlError) -> Capability {
    Capability::unavailable(
        "gpu.nvidia",
        "nvml",
        classify_nvml_error(error),
        error.to_string(),
    )
}

#[allow(deprecated)]
fn classify_nvml_error(error: &NvmlError) -> CapabilityErrorKind {
    match error {
        NvmlError::NoPermission | NvmlError::OperatingSystem => {
            CapabilityErrorKind::PermissionDenied
        }
        NvmlError::LibloadingError(_)
        | NvmlError::DriverNotLoaded
        | NvmlError::LibraryNotFound
        | NvmlError::LibRmVersionMismatch => CapabilityErrorKind::DriverMissing,
        NvmlError::FailedToLoadSymbol(_)
        | NvmlError::NotSupported
        | NvmlError::FunctionNotFound
        | NvmlError::VgpuEccNotSupported => CapabilityErrorKind::Unsupported,
        NvmlError::NotFound | NvmlError::NoData => CapabilityErrorKind::NotPresent,
        NvmlError::Utf8Error(_)
        | NvmlError::NulError(_)
        | NvmlError::StringTooLong { .. }
        | NvmlError::IncorrectBits(_)
        | NvmlError::UnexpectedVariant(_)
        | NvmlError::PciInfoToCFailed
        | NvmlError::InvalidArg
        | NvmlError::InsufficientSize(_)
        | NvmlError::CorruptedInfoROM => CapabilityErrorKind::InvalidData,
        NvmlError::SetReleaseFailed
        | NvmlError::GetPciInfoFailed
        | NvmlError::Uninitialized
        | NvmlError::AlreadyInitialized
        | NvmlError::InsufficientPower
        | NvmlError::Timeout
        | NvmlError::IrqIssue
        | NvmlError::GpuLost
        | NvmlError::ResetRequired
        | NvmlError::InUse
        | NvmlError::InsufficientMemory
        | NvmlError::Unknown => CapabilityErrorKind::Transient,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_initialization_retries_after_backoff_and_reuses_success() {
        let started_at = Instant::now();
        let mut attempts = 0;
        let mut state = RetryingInit::<usize, &str>::new();

        state.initialize_if_due(started_at, || {
            attempts += 1;
            Err("driver unavailable")
        });
        assert!(state.value().is_none());
        assert_eq!(state.last_error(), Some(&"driver unavailable"));
        assert_eq!(attempts, 1);

        state.initialize_if_due(
            started_at + NVML_INIT_RETRY_INITIAL - Duration::from_millis(1),
            || {
                attempts += 1;
                Ok(7)
            },
        );
        assert!(state.value().is_none());
        assert_eq!(state.last_error(), Some(&"driver unavailable"));
        assert_eq!(attempts, 1);

        state.initialize_if_due(started_at + NVML_INIT_RETRY_INITIAL, || {
            attempts += 1;
            Ok(7)
        });
        assert_eq!(state.value(), Some(&7));
        assert_eq!(state.last_error(), None);
        assert_eq!(attempts, 2);

        state.initialize_if_due(
            started_at + NVML_INIT_RETRY_INITIAL * 2,
            || -> Result<usize, &str> {
                attempts += 1;
                panic!("a successful initialization must be reused")
            },
        );
        assert_eq!(state.value(), Some(&7));
        assert_eq!(attempts, 2);
    }

    #[test]
    fn initialization_retry_delay_is_bounded() {
        let mut now = Instant::now();
        let mut state = RetryingInit::<(), &str>::new();

        for _ in 0..16 {
            let attempted_delay = state.retry_delay;
            state.initialize_if_due(now, || Err("still unavailable"));
            assert!(state.value().is_none());
            assert!(state.retry_delay <= NVML_INIT_RETRY_MAX);
            now += attempted_delay;
        }

        assert_eq!(state.retry_delay, NVML_INIT_RETRY_MAX);
    }

    #[test]
    fn nvml_errors_map_to_actionable_capability_kinds() {
        let cases = [
            (
                NvmlError::NoPermission,
                CapabilityErrorKind::PermissionDenied,
            ),
            (
                NvmlError::OperatingSystem,
                CapabilityErrorKind::PermissionDenied,
            ),
            (
                NvmlError::DriverNotLoaded,
                CapabilityErrorKind::DriverMissing,
            ),
            (
                NvmlError::LibraryNotFound,
                CapabilityErrorKind::DriverMissing,
            ),
            (
                NvmlError::LibRmVersionMismatch,
                CapabilityErrorKind::DriverMissing,
            ),
            (NvmlError::NotSupported, CapabilityErrorKind::Unsupported),
            (
                NvmlError::FunctionNotFound,
                CapabilityErrorKind::Unsupported,
            ),
            (NvmlError::NotFound, CapabilityErrorKind::NotPresent),
            (NvmlError::NoData, CapabilityErrorKind::NotPresent),
            (NvmlError::InvalidArg, CapabilityErrorKind::InvalidData),
            (
                NvmlError::CorruptedInfoROM,
                CapabilityErrorKind::InvalidData,
            ),
            (NvmlError::Timeout, CapabilityErrorKind::Transient),
            (NvmlError::GpuLost, CapabilityErrorKind::Transient),
            (NvmlError::Unknown, CapabilityErrorKind::Transient),
        ];

        for (error, expected) in cases {
            assert_eq!(classify_nvml_error(&error), expected, "{error}");
        }

        let capability = nvml_error_capability(&NvmlError::NoPermission);
        assert_eq!(
            capability.error_kind,
            Some(CapabilityErrorKind::PermissionDenied)
        );
        assert_eq!(
            capability.message.as_deref(),
            Some("the current user does not have permission to perform this operation")
        );
    }
}
