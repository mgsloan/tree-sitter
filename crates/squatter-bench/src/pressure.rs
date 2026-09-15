use anyhow::{Result, bail, ensure};
use clap::ValueEnum;
use serde::Serialize;
use std::{
    fs,
    hint::black_box,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Do not disturb cache residency.
    #[default]
    None,
    /// Rotate real workloads through a batch sized by aggregate source bytes.
    Carousel,
    /// Traverse a bounded randomized working set before every timed operation.
    Wash,
    /// Run a duty-cycled randomized working set on a concurrent thread.
    Tenant,
}

#[repr(align(64))]
struct Line {
    next: usize,
    _padding: [u8; 56],
}

fn random(mut value: u64) -> u64 {
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn ring(bytes: usize) -> Vec<Line> {
    let count = bytes.div_ceil(64).max(2);
    let mut order: Vec<_> = (0..count).collect();
    let mut state = 42u64;
    for index in (1..count).rev() {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        order.swap(index, random(state) as usize % (index + 1));
    }
    let mut lines: Vec<_> = (0..count)
        .map(|_| Line {
            next: 0,
            _padding: [0; 56],
        })
        .collect();
    for index in 0..count {
        lines[order[index]].next = order[(index + 1) % count];
    }
    lines
}

#[inline(never)]
fn touch(lines: &[Line]) -> usize {
    let mut index = 0;
    for _ in 0..lines.len() {
        index = black_box(lines[index].next);
    }
    black_box(index)
}

fn cache_size() -> Option<usize> {
    for entry in fs::read_dir("/sys/devices/system/cpu/cpu0/cache").ok()? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Ok(level) = fs::read_to_string(path.join("level")) else {
            continue;
        };
        if level.trim() != "3" {
            continue;
        }
        let Ok(value) = fs::read_to_string(path.join("size")) else {
            continue;
        };
        let value = value.trim();
        let (digits, scale) = if let Some(digits) = value.strip_suffix('K') {
            (digits, 1024)
        } else if let Some(digits) = value.strip_suffix('M') {
            (digits, 1024 * 1024)
        } else if let Some(digits) = value.strip_suffix('G') {
            (digits, 1024 * 1024 * 1024)
        } else {
            (value, 1)
        };
        return digits.parse::<usize>().ok()?.checked_mul(scale);
    }
    None
}

#[cfg(target_os = "linux")]
fn pin(cpu: usize) -> Result<()> {
    ensure!(
        cpu < libc::CPU_SETSIZE as usize,
        "CPU index exceeds cpu_set_t"
    );
    let mut set = unsafe { std::mem::zeroed::<libc::cpu_set_t>() };
    unsafe {
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu, &mut set);
    }
    if unsafe { libc::sched_setaffinity(0, std::mem::size_of_val(&set), &set) } != 0 {
        bail!("pin CPU {cpu}: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn pin(cpu: usize) -> Result<()> {
    bail!("CPU affinity {cpu} is only supported on Linux")
}

pub struct Pressure {
    mode: Mode,
    bytes: usize,
    duty_percent: u8,
    wash: Option<Vec<Line>>,
    stop: Arc<AtomicBool>,
    touches: Arc<AtomicU64>,
    worker: Option<thread::JoinHandle<()>>,
    benchmark_cpu: Option<usize>,
    tenant_cpu: Option<usize>,
    detected_llc_bytes: Option<usize>,
}

impl Pressure {
    pub fn new(
        mode: Mode,
        requested_bytes: Option<usize>,
        duty_percent: u8,
        benchmark_cpu: Option<usize>,
        tenant_cpu: Option<usize>,
    ) -> Result<Self> {
        ensure!(
            duty_percent <= 100,
            "pressure duty must be at most 100 percent"
        );
        if matches!(mode, Mode::Tenant) {
            ensure!(duty_percent > 0, "tenant pressure duty must be positive");
        }
        if let Some(cpu) = benchmark_cpu {
            pin(cpu)?;
        }
        let detected_llc_bytes = cache_size();
        let bytes = requested_bytes
            .or_else(|| detected_llc_bytes.and_then(|bytes| bytes.checked_mul(2)))
            .unwrap_or(32 * 1024 * 1024);
        if !matches!(mode, Mode::None) {
            ensure!(
                bytes >= 128,
                "pressure working set must be at least 128 bytes"
            );
        }
        let stop = Arc::new(AtomicBool::new(false));
        let touches = Arc::new(AtomicU64::new(0));
        let mut wash = None;
        let mut worker = None;
        match mode {
            Mode::None | Mode::Carousel => {}
            Mode::Wash => wash = Some(ring(bytes)),
            Mode::Tenant => {
                let lines = ring(bytes);
                let worker_stop = Arc::clone(&stop);
                let worker_touches = Arc::clone(&touches);
                let (ready_tx, ready_rx) = mpsc::sync_channel(1);
                worker = Some(thread::spawn(move || {
                    let pinned = tenant_cpu.map_or(Ok(()), pin);
                    if let Err(error) = pinned {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return;
                    }
                    touch(&lines);
                    worker_touches.fetch_add(lines.len() as u64, Ordering::Relaxed);
                    let _ = ready_tx.send(Ok(()));
                    let quantum = Duration::from_millis(10);
                    let active = quantum.mul_f64(f64::from(duty_percent) / 100.0);
                    while !worker_stop.load(Ordering::Relaxed) {
                        let started = Instant::now();
                        loop {
                            touch(&lines);
                            worker_touches.fetch_add(lines.len() as u64, Ordering::Relaxed);
                            if started.elapsed() >= active || worker_stop.load(Ordering::Relaxed) {
                                break;
                            }
                        }
                        if let Some(rest) = quantum.checked_sub(started.elapsed()) {
                            thread::park_timeout(rest);
                        }
                    }
                }));
                ready_rx
                    .recv()
                    .map_err(anyhow::Error::from)?
                    .map_err(anyhow::Error::msg)?;
            }
        }
        Ok(Self {
            mode,
            bytes,
            duty_percent,
            wash,
            stop,
            touches,
            worker,
            benchmark_cpu,
            tenant_cpu,
            detected_llc_bytes,
        })
    }

    pub fn before_measurement(&mut self) {
        if let Some(lines) = &self.wash {
            touch(lines);
            self.touches
                .fetch_add(lines.len() as u64, Ordering::Relaxed);
        }
    }

    pub fn carousel_bytes(&self) -> Option<usize> {
        matches!(self.mode, Mode::Carousel).then_some(self.bytes)
    }

    pub fn report(&self) -> serde_json::Value {
        let working_set_bytes = (!matches!(self.mode, Mode::None)).then_some(self.bytes);
        let duty_percent = matches!(self.mode, Mode::Tenant).then_some(self.duty_percent);
        serde_json::json!({
            "mode": self.mode,
            "working_set_bytes": working_set_bytes,
            "duty_percent": duty_percent,
            "benchmark_cpu": self.benchmark_cpu,
            "tenant_cpu": self.tenant_cpu,
            "detected_llc_bytes": self.detected_llc_bytes,
            "cache_line_bytes": 64,
            "touches": self.touches.load(Ordering::Relaxed),
        })
    }
}

impl Drop for Pressure {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn randomized_ring_visits_every_line() {
        let lines = ring(64 * 31);
        let mut seen = vec![false; lines.len()];
        let mut index = 0;
        for _ in 0..lines.len() {
            assert!(!seen[index]);
            seen[index] = true;
            index = lines[index].next;
        }
        assert_eq!(index, 0);
        assert!(seen.into_iter().all(|value| value));
    }
}
