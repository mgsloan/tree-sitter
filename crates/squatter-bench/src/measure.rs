use serde::Serialize;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Metrics {
    pub wall_ms: f64,
    pub cpu_ms: f64,
    pub instructions: Option<f64>,
    pub cache_references: Option<f64>,
    pub cache_misses: Option<f64>,
}
impl Metrics {
    pub const NAMES: [&'static str; 5] = [
        "wall_ms",
        "cpu_ms",
        "instructions",
        "cache_references",
        "cache_misses",
    ];
    pub fn values(self) -> [Option<f64>; 5] {
        [
            Some(self.wall_ms),
            Some(self.cpu_ms),
            self.instructions,
            self.cache_references,
            self.cache_misses,
        ]
    }
    pub fn median(samples: &[Self]) -> Self {
        let values: Vec<_> = (0..5)
            .map(|index| {
                let mut values: Vec<_> = samples
                    .iter()
                    .filter_map(|sample| sample.values()[index])
                    .collect();
                values.sort_by(f64::total_cmp);
                percentile(&values, 50.0)
            })
            .collect();
        Self {
            wall_ms: values[0].unwrap_or_default(),
            cpu_ms: values[1].unwrap_or_default(),
            instructions: values[2],
            cache_references: values[3],
            cache_misses: values[4],
        }
    }
}
pub fn percentile(sorted: &[f64], percent: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let position = percent / 100.0 * (sorted.len() - 1) as f64;
    let low = position.floor() as usize;
    let high = position.ceil() as usize;
    Some(sorted[low] + (sorted[high] - sorted[low]) * position.fract())
}

fn cpu_ms() -> f64 {
    #[cfg(unix)]
    {
        let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
        if unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, time.as_mut_ptr()) } == 0 {
            let time = unsafe { time.assume_init() };
            return time.tv_sec as f64 * 1000.0 + time.tv_nsec as f64 / 1e6;
        }
    }
    0.0
}

/// Linux hardware events count only this thread's userspace execution. Other
/// platforms and restricted kernels leave counter fields null in every output.
pub struct Meter {
    #[cfg(target_os = "linux")]
    counters: Vec<std::os::fd::OwnedFd>,
    pub counter_status: String,
}
impl Meter {
    pub fn new() -> Self {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::FromRawFd;
            // PERF_ATTR_SIZE_VER0. Hardware counting needs only this stable
            // prefix of linux/perf_event.h's perf_event_attr.
            #[repr(C)]
            struct Attr {
                kind: u32,
                size: u32,
                config: u64,
                sample_period: u64,
                sample_type: u64,
                read_format: u64,
                flags: u64,
                wakeup_events: u32,
                bp_type: u32,
                config1: u64,
            }
            let mut counters = Vec::new();
            for config in [1, 2, 3] {
                // instructions, cache references, cache misses
                let attr = Attr {
                    kind: 0,
                    size: 64,
                    config,
                    sample_period: 0,
                    sample_type: 0,
                    read_format: 3,
                    flags: 1 | (1 << 5) | (1 << 6),
                    wakeup_events: 0,
                    bp_type: 0,
                    config1: 0,
                };
                let fd = unsafe { libc::syscall(libc::SYS_perf_event_open, &attr, 0, -1, -1, 8) };
                if fd < 0 {
                    return Self {
                        counters: Vec::new(),
                        counter_status: std::io::Error::last_os_error().to_string(),
                    };
                }
                counters.push(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as i32) });
            }
            Self {
                counters,
                counter_status: "available; userspace thread counts, multiplexing scaled".into(),
            }
        }
        #[cfg(not(target_os = "linux"))]
        Self {
            counter_status: "hardware counters unsupported on this platform".into(),
        }
    }
    pub fn measure<T>(&mut self, operation: impl FnOnce() -> T) -> (T, Metrics) {
        #[cfg(target_os = "linux")]
        let mut previous_times = [[0u64; 3]; 3];
        #[cfg(target_os = "linux")]
        for (index, counter) in self.counters.iter().enumerate() {
            use std::os::fd::AsRawFd;
            unsafe {
                // RESET clears the count but not enabled/running time. Scale
                // using this measurement's time deltas, not the cursor lifetime.
                libc::read(
                    counter.as_raw_fd(),
                    previous_times[index].as_mut_ptr().cast(),
                    24,
                );
                libc::ioctl(counter.as_raw_fd(), 0x2403, 0); // PERF_EVENT_IOC_RESET
                libc::ioctl(counter.as_raw_fd(), 0x2400, 0); // PERF_EVENT_IOC_ENABLE
            }
        }
        let cpu_start = cpu_ms();
        let start = Instant::now();
        let output = operation();
        let wall_ms = start.elapsed().as_secs_f64() * 1000.0;
        let cpu_ms = cpu_ms() - cpu_start;
        let mut hardware = [None; 3];
        #[cfg(target_os = "linux")]
        for (index, counter) in self.counters.iter().enumerate() {
            use std::os::fd::AsRawFd;
            let mut values = [0u64; 3];
            let size = std::mem::size_of_val(&values);
            unsafe {
                libc::ioctl(counter.as_raw_fd(), 0x2401, 0); // PERF_EVENT_IOC_DISABLE
                if libc::read(counter.as_raw_fd(), values.as_mut_ptr().cast(), size)
                    == size as isize
                    && values[2] > previous_times[index][2]
                {
                    let enabled = values[1] - previous_times[index][1];
                    let running = values[2] - previous_times[index][2];
                    hardware[index] = Some(values[0] as f64 * enabled as f64 / running as f64);
                }
            }
        }
        (
            output,
            Metrics {
                wall_ms,
                cpu_ms,
                instructions: hardware[0],
                cache_references: hardware[1],
                cache_misses: hardware[2],
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quantiles_interpolate_and_keep_missing_counters_missing() {
        assert_eq!(percentile(&[1.0, 3.0], 50.0), Some(2.0));
        assert_eq!(percentile(&[1.0, 3.0], 0.0), Some(1.0));
        assert_eq!(percentile(&[], 50.0), None);
        let metrics = Metrics::median(&[
            Metrics {
                wall_ms: 1.0,
                ..Default::default()
            },
            Metrics {
                wall_ms: 3.0,
                ..Default::default()
            },
        ]);
        assert_eq!(metrics.wall_ms, 2.0);
        assert_eq!(metrics.instructions, None);
    }
}
