use nvml_wrapper::{
    Nvml,
    enum_wrappers::device::{Clock, PcieUtilCounter, TemperatureSensor},
};
use std::time::{Duration, Instant};

use crate::model::{Capability, CapabilityErrorKind, GpuSnapshot};

const NVML_INIT_RETRY_INITIAL: Duration = Duration::from_secs(30);
const NVML_INIT_RETRY_MAX: Duration = Duration::from_secs(15 * 60);

struct RetryingInit<T> {
    value: Option<T>,
    last_error: Option<String>,
    next_retry_at: Option<Instant>,
    retry_delay: Duration,
}

impl<T> RetryingInit<T> {
    fn new() -> Self {
        Self {
            value: None,
            last_error: None,
            next_retry_at: None,
            retry_delay: NVML_INIT_RETRY_INITIAL,
        }
    }

    fn get_or_try_init<E>(
        &mut self,
        now: Instant,
        init: impl FnOnce() -> Result<T, E>,
    ) -> Result<&T, String>
    where
        E: ToString,
    {
        if self.value.is_none() {
            if self.next_retry_at.is_some_and(|retry_at| now < retry_at) {
                return Err(self
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "initialization is waiting for retry".to_string()));
            }

            match init() {
                Ok(value) => {
                    self.value = Some(value);
                    self.last_error = None;
                    self.next_retry_at = None;
                    self.retry_delay = NVML_INIT_RETRY_INITIAL;
                }
                Err(error) => {
                    let error = error.to_string();
                    self.last_error = Some(error.clone());
                    self.next_retry_at = now.checked_add(self.retry_delay);
                    self.retry_delay = self.retry_delay.saturating_mul(2).min(NVML_INIT_RETRY_MAX);
                    return Err(error);
                }
            }
        }

        self.value
            .as_ref()
            .ok_or_else(|| "initialization completed without a value".to_string())
    }
}

pub(super) struct NvidiaCollector {
    nvml: RetryingInit<Nvml>,
}

impl NvidiaCollector {
    pub fn new() -> Self {
        let mut nvml = RetryingInit::new();
        let _ = nvml.get_or_try_init(Instant::now(), Nvml::init);
        Self { nvml }
    }

    pub fn collect(&mut self) -> (Vec<GpuSnapshot>, Capability) {
        let nvml = match self.nvml.get_or_try_init(Instant::now(), Nvml::init) {
            Ok(nvml) => nvml,
            Err(error) => {
                return (
                    Vec::new(),
                    Capability::unavailable(
                        "gpu.nvidia",
                        "nvml",
                        CapabilityErrorKind::DriverMissing,
                        error,
                    ),
                );
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_initialization_retries_after_backoff_and_reuses_success() {
        let started_at = Instant::now();
        let mut attempts = 0;
        let mut state = RetryingInit::new();

        let error = state
            .get_or_try_init(started_at, || {
                attempts += 1;
                Err::<usize, _>("driver unavailable")
            })
            .expect_err("the first initialization should fail");
        assert_eq!(error, "driver unavailable");
        assert_eq!(attempts, 1);

        let error = state
            .get_or_try_init(
                started_at + NVML_INIT_RETRY_INITIAL - Duration::from_millis(1),
                || {
                    attempts += 1;
                    Ok::<_, &str>(7)
                },
            )
            .expect_err("initialization must not run during the backoff window");
        assert_eq!(error, "driver unavailable");
        assert_eq!(attempts, 1);

        let value = state
            .get_or_try_init(started_at + NVML_INIT_RETRY_INITIAL, || {
                attempts += 1;
                Ok::<_, &str>(7)
            })
            .expect("initialization should retry when the backoff expires");
        assert_eq!(*value, 7);
        assert_eq!(attempts, 2);

        let value = state
            .get_or_try_init(
                started_at + NVML_INIT_RETRY_INITIAL * 2,
                || -> Result<usize, &str> { panic!("a successful initialization must be reused") },
            )
            .expect("the initialized value should remain available");
        assert_eq!(*value, 7);
        assert_eq!(attempts, 2);
    }

    #[test]
    fn initialization_retry_delay_is_bounded() {
        let mut now = Instant::now();
        let mut state = RetryingInit::<()>::new();

        for _ in 0..16 {
            let attempted_delay = state.retry_delay;
            state
                .get_or_try_init(now, || Err::<(), _>("still unavailable"))
                .expect_err("the synthetic initialization should fail");
            assert!(state.retry_delay <= NVML_INIT_RETRY_MAX);
            now += attempted_delay;
        }

        assert_eq!(state.retry_delay, NVML_INIT_RETRY_MAX);
    }
}
