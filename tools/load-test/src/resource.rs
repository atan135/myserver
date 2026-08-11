use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub enum ResourceValue<T> {
    Available { value: T },
    Unavailable { reason: String },
}

impl<T> ResourceValue<T> {
    fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeneratorResources {
    pub process_cpu_ms: ResourceValue<u64>,
    pub working_set_bytes: ResourceValue<u64>,
    pub thread_count: ResourceValue<u32>,
    pub handle_count: ResourceValue<u32>,
    pub network_sent_bytes: ResourceValue<u64>,
    pub network_received_bytes: ResourceValue<u64>,
    pub socket_errors: ResourceValue<u64>,
    pub tokio_scheduler_lag_ms: ResourceValue<u64>,
    pub worker_queue_depth: ResourceValue<u64>,
    pub metrics_channel_dropped: ResourceValue<u64>,
}

#[derive(Debug, Default)]
pub struct ResourceSampler;

impl ResourceSampler {
    pub fn sample(
        &self,
        scheduler_lag_ms: u64,
        queue_depth: u64,
        metrics_dropped: u64,
    ) -> GeneratorResources {
        let (process_cpu_ms, working_set_bytes, handle_count) = platform_process_sample();
        GeneratorResources {
            process_cpu_ms,
            working_set_bytes,
            thread_count: ResourceValue::unavailable(
                "thread enumeration is not available in the stage-one sampler",
            ),
            handle_count,
            network_sent_bytes: ResourceValue::unavailable(
                "per-process network byte accounting is unavailable",
            ),
            network_received_bytes: ResourceValue::unavailable(
                "per-process network byte accounting is unavailable",
            ),
            socket_errors: ResourceValue::unavailable(
                "socket error accounting is transport-specific and not active during dry-run",
            ),
            tokio_scheduler_lag_ms: ResourceValue::Available {
                value: scheduler_lag_ms,
            },
            worker_queue_depth: ResourceValue::Available { value: queue_depth },
            metrics_channel_dropped: ResourceValue::Available {
                value: metrics_dropped,
            },
        }
    }
}

#[cfg(windows)]
fn platform_process_sample() -> (ResourceValue<u64>, ResourceValue<u64>, ResourceValue<u32>) {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessHandleCount, GetProcessTimes,
    };

    // These are process-local Win32 calls; unavailable values are intentional
    // when a constrained host denies either query.
    unsafe {
        let process = GetCurrentProcess();
        let mut counters: PROCESS_MEMORY_COUNTERS = zeroed();
        counters.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let memory = if GetProcessMemoryInfo(process, &mut counters, counters.cb) != 0 {
            ResourceValue::Available {
                value: counters.WorkingSetSize as u64,
            }
        } else {
            ResourceValue::unavailable(format!(
                "GetProcessMemoryInfo failed: {}",
                std::io::Error::last_os_error()
            ))
        };
        let mut handle_count = 0_u32;
        let handles = if GetProcessHandleCount(process, &mut handle_count) != 0 {
            ResourceValue::Available {
                value: handle_count,
            }
        } else {
            ResourceValue::unavailable(format!(
                "GetProcessHandleCount failed: {}",
                std::io::Error::last_os_error()
            ))
        };
        let (mut creation, mut exit, mut kernel, mut user): (
            FILETIME,
            FILETIME,
            FILETIME,
            FILETIME,
        ) = (zeroed(), zeroed(), zeroed(), zeroed());
        let cpu = if GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) != 0
        {
            let ticks =
                |time: FILETIME| ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64;
            ResourceValue::Available {
                value: ticks(kernel).saturating_add(ticks(user)) / 10_000,
            }
        } else {
            ResourceValue::unavailable(format!(
                "GetProcessTimes failed: {}",
                std::io::Error::last_os_error()
            ))
        };
        (cpu, memory, handles)
    }
}

#[cfg(not(windows))]
fn platform_process_sample() -> (ResourceValue<u64>, ResourceValue<u64>, ResourceValue<u32>) {
    let unavailable =
        || ResourceValue::unavailable("Windows process sampler is unavailable on this host");
    (unavailable(), unavailable(), unavailable())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_resource_values_are_explicit_not_zero() {
        let sample = ResourceSampler.sample(2, 3, 4);
        assert_eq!(
            sample.tokio_scheduler_lag_ms,
            ResourceValue::Available { value: 2 }
        );
        assert!(matches!(
            sample.network_sent_bytes,
            ResourceValue::Unavailable { .. }
        ));
    }
}
