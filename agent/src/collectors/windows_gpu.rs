//! Windows WDDM GPU Engine 的只读 PDH consumer。

use std::{
    fmt,
    mem::{self, MaybeUninit},
    time::Instant,
};

use windows::{
    Win32::System::Performance::{
        PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
        PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery,
        PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
    },
    core::{PCWSTR, w},
};

use crate::model::{Capability, CapabilityErrorKind, GpuSnapshot};

use super::pdh_buffer::{plan_pdh_buffer, validate_pdh_result};
use super::pdh_recovery::{RetryingPdhResource, should_rebuild_pdh_query};

const ERROR_SUCCESS: u32 = 0;

pub(super) struct WindowsGpuCollector {
    session: RetryingPdhResource<PdhSession>,
}

impl WindowsGpuCollector {
    pub fn new() -> Self {
        let mut session = RetryingPdhResource::new();
        let _ = session.get_or_try_init(Instant::now(), open_pdh_session);
        Self { session }
    }

    pub fn collect(&mut self) -> (Vec<GpuSnapshot>, Capability) {
        let (query, counter) = match self
            .session
            .get_or_try_init(Instant::now(), open_pdh_session)
        {
            Ok(session) => (session.query, session.counter),
            Err(error) => {
                let error_kind = if self.session.ever_succeeded() {
                    CapabilityErrorKind::Transient
                } else {
                    CapabilityErrorKind::Unsupported
                };
                return (
                    Vec::new(),
                    Capability::unavailable("gpu.windows.wddm", "windows-pdh", error_kind, error),
                );
            }
        };

        // SAFETY: the session owns this query handle until an invalid-handle result causes the
        // session to be dropped below.
        let collect = unsafe { PdhCollectQueryData(query) };
        if collect != ERROR_SUCCESS {
            let message = format!("PdhCollectQueryData returned 0x{collect:08x}");
            if should_rebuild_pdh_query(collect) {
                self.session.invalidate(Instant::now(), message.clone());
            }
            return (
                Vec::new(),
                Capability::unavailable(
                    "gpu.windows.wddm",
                    "windows-pdh",
                    CapabilityErrorKind::Transient,
                    message,
                ),
            );
        }

        match formatted_values(counter) {
            Ok(values) if !values.is_empty() => {
                let utilization = values.into_iter().fold(0.0_f64, f64::max).clamp(0.0, 100.0);
                (
                    vec![GpuSnapshot {
                        id: "windows-wddm".into(),
                        vendor: "unknown".into(),
                        name: "Windows WDDM GPU".into(),
                        utilization_percent: Some(utilization),
                        memory_total_bytes: None,
                        memory_used_bytes: None,
                        temperature_celsius: None,
                        power_watts: None,
                        core_clock_mhz: None,
                        memory_clock_mhz: None,
                        pcie_rx_bytes_per_second: None,
                        pcie_tx_bytes_per_second: None,
                        source: "windows-pdh-gpu-engine".into(),
                    }],
                    Capability::available("gpu.windows.wddm", "windows-pdh"),
                )
            }
            Ok(_) => (
                Vec::new(),
                Capability::unavailable(
                    "gpu.windows.wddm",
                    "windows-pdh",
                    CapabilityErrorKind::NotPresent,
                    "WDDM exposed no active GPU Engine instances",
                ),
            ),
            Err(error) => {
                let message = error.to_string();
                if error.status.is_some_and(should_rebuild_pdh_query) {
                    self.session.invalidate(Instant::now(), message.clone());
                }
                (
                    Vec::new(),
                    Capability::unavailable(
                        "gpu.windows.wddm",
                        "windows-pdh",
                        CapabilityErrorKind::Transient,
                        message,
                    ),
                )
            }
        }
    }
}

struct PdhSession {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
}

impl Drop for PdhSession {
    fn drop(&mut self) {
        if !self.query.is_invalid() {
            // SAFETY: this session owns the query and Drop is its final use.
            unsafe { PdhCloseQuery(self.query) };
        }
    }
}

fn open_pdh_session() -> Result<PdhSession, String> {
    let mut query = PDH_HQUERY::default();
    let mut counter = PDH_HCOUNTER::default();
    // SAFETY: PDH writes only the two initialized out handles; the counter path is static
    // UTF-16, and a successfully opened query is owned by the returned session.
    let result = unsafe {
        let open = PdhOpenQueryW(PCWSTR::null(), 0, &mut query);
        if open != ERROR_SUCCESS {
            open
        } else {
            let add = PdhAddEnglishCounterW(
                query,
                w!(r"\GPU Engine(*)\Utilization Percentage"),
                0,
                &mut counter,
            );
            if add == ERROR_SUCCESS {
                let _ = PdhCollectQueryData(query);
            }
            add
        }
    };
    if result == ERROR_SUCCESS {
        Ok(PdhSession { query, counter })
    } else {
        if !query.is_invalid() {
            // SAFETY: query was returned by PdhOpenQueryW and is no longer used.
            unsafe { PdhCloseQuery(query) };
        }
        Err(format!("PDH initialization returned 0x{result:08x}"))
    }
}

struct PdhReadError {
    message: String,
    status: Option<u32>,
}

impl PdhReadError {
    fn status(operation: &str, status: u32) -> Self {
        Self {
            message: format!("{operation} returned 0x{status:08x}"),
            status: Some(status),
        }
    }

    fn message(message: String) -> Self {
        Self {
            message,
            status: None,
        }
    }
}

impl fmt::Display for PdhReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

fn formatted_values(counter: PDH_HCOUNTER) -> Result<Vec<f64>, PdhReadError> {
    let mut byte_len = 0_u32;
    let mut item_count = 0_u32;
    // SAFETY: the first call intentionally passes no output buffer to obtain its size.
    let size_result = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut byte_len,
            &mut item_count,
            None,
        )
    };
    if size_result != PDH_MORE_DATA || byte_len == 0 {
        return if size_result == ERROR_SUCCESS {
            Ok(Vec::new())
        } else {
            Err(PdhReadError::status("PDH array sizing", size_result))
        };
    }

    let item_size = mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    let layout = plan_pdh_buffer(byte_len, item_count, item_size).map_err(PdhReadError::message)?;
    let mut buffer = Vec::<MaybeUninit<PDH_FMT_COUNTERVALUE_ITEM_W>>::new();
    buffer.try_reserve_exact(layout.slots).map_err(|_| {
        PdhReadError::message(format!(
            "PDH array could not reserve its bounded {} byte buffer",
            layout.capacity_bytes
        ))
    })?;
    buffer.resize_with(layout.slots, MaybeUninit::uninit);
    let pointer = buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    byte_len = u32::try_from(layout.capacity_bytes).map_err(|_| {
        PdhReadError::message("PDH array capacity does not fit the API's u32 range".to_string())
    })?;
    // SAFETY: the typed MaybeUninit allocation provides both the byte capacity passed to PDH and
    // the alignment required by PDH_FMT_COUNTERVALUE_ITEM_W. PDH returns item_count initialized
    // entries followed by the instance-name storage in the same backing allocation.
    let result = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut byte_len,
            &mut item_count,
            Some(pointer),
        )
    };
    if result != ERROR_SUCCESS {
        return Err(PdhReadError::status("PDH array read", result));
    }
    let item_count = validate_pdh_result(layout.capacity_bytes, byte_len, item_count, item_size)
        .map_err(PdhReadError::message)?;

    // SAFETY: a successful PDH call initialized item_count complete entries, and the checked
    // returned byte count proves that those entries fit inside the still-live typed allocation.
    let items = unsafe { std::slice::from_raw_parts(pointer, item_count) };
    let values = items
        .iter()
        .filter(|item| {
            matches!(
                item.FmtValue.CStatus,
                PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA
            )
        })
        .filter_map(|item| {
            // SAFETY: PDH_FMT_DOUBLE requests the doubleValue union member.
            let value = unsafe { item.FmtValue.Anonymous.doubleValue };
            value.is_finite().then_some(value)
        })
        .collect();
    // Keep the backing allocation alive until after all item values have been copied.
    drop(buffer);
    Ok(values)
}
