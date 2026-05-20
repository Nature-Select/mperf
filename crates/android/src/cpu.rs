//! Android CPU sampler. Reads `/proc/stat` over `adb shell`, computes
//! per-tick utilization for the system and each core. Also emits
//! per-app CPU% from `/proc/<pid>/stat` summed across the target
//! package's processes — the target is mandatory (PerfDog-style: user
//! must pick the app before recording).
//!
//! Reference: linux `man 5 proc` — /proc/stat columns. We use the canonical
//! formula `(load_delta) / (tick_delta)` where load = sum of all jiffies
//! except `idle` (we count `iowait` as load).

use crate::adb;
use async_stream::stream;
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use mperf_schema::{
    LabelKey, MetricKind, Sample, Sampler, SamplerCtx, SamplerError,
};
use smallvec::smallvec;
use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};

/// Hard-floor on sampler cadence — anything faster than this puts more
/// load on adb than the resulting data is worth. The configurable
/// `interval_ms` is clamped to this minimum on construction.
const MIN_INTERVAL_MS: u64 = 200;

pub struct CpuSampler {
    serial: String,
    target_pkg: String,
    interval_ms: u64,
}

impl CpuSampler {
    pub fn new(
        serial: impl Into<String>,
        target_pkg: impl Into<String>,
        interval_ms: u64,
    ) -> Self {
        Self {
            serial: serial.into(),
            target_pkg: target_pkg.into(),
            interval_ms: interval_ms.max(MIN_INTERVAL_MS),
        }
    }
}

#[async_trait]
impl Sampler for CpuSampler {
    fn name(&self) -> &'static str {
        "android.cpu"
    }

    fn target_hz(&self) -> f32 {
        1000.0 / self.interval_ms as f32
    }

    async fn start(
        &mut self,
        ctx: SamplerCtx,
    ) -> Result<BoxStream<'static, Result<Sample, SamplerError>>, SamplerError> {
        let serial = self.serial.clone();
        let pkg = self.target_pkg.clone();
        let interval_ms = self.interval_ms;
        // Initial reading; if this fails we surface immediately rather than
        // returning a stream that yields an error on first poll.
        let initial = read_proc_stat(&serial).await?;
        tracing::info!(
            sampler = "android.cpu",
            cores = initial.cores.len(),
            pkg = %pkg,
            "initial /proc/stat read"
        );

        let clock = ctx.clock.clone();
        let s = stream! {
            let mut last = initial;
            let mut last_app_jiffies: Option<u64> = None;
            let mut ticker = interval(Duration::from_millis(interval_ms));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            // First tick fires immediately; skip it so we sample one interval later.
            ticker.tick().await;

            loop {
                ticker.tick().await;
                let curr = match read_proc_stat(&serial).await {
                    Ok(v) => v,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                };

                let ts = clock.now_us();

                if let Some(total_pct) = cpu_pct(&last.total, &curr.total) {
                    yield Ok(Sample {
                        ts_us: ts,
                        device_ts_us: None,
                        kind: MetricKind::CpuTotalPct,
                        value: total_pct,
                        labels: smallvec![],
                    });
                }

                let core_count = last.cores.len().min(curr.cores.len());
                for idx in 0..core_count {
                    if let Some(p) = cpu_pct(&last.cores[idx], &curr.cores[idx]) {
                        yield Ok(Sample {
                            ts_us: ts,
                            device_ts_us: None,
                            kind: MetricKind::CpuCorePct,
                            value: p,
                            labels: smallvec![(LabelKey::CoreIdx, idx.to_string())],
                        });
                    }
                }

                // App CPU%. Same fraction-of-total-system semantics as iOS:
                // app_delta / total_delta * 100 (so a 4-core app at 100% on
                // a 4-core device reads ~25, not 400).
                let curr_app_jiffies = sum_app_jiffies(&serial, &pkg).await;
                let total_delta = curr.total.total().saturating_sub(last.total.total());
                let mut emitted_app = false;
                if let (Some(prev), Some(curr_j)) = (last_app_jiffies, curr_app_jiffies) {
                    if total_delta > 0 {
                        let app_delta = curr_j.saturating_sub(prev);
                        let pct = (app_delta as f64 / total_delta as f64 * 100.0)
                            .clamp(0.0, 100.0);
                        yield Ok(Sample {
                            ts_us: ts,
                            device_ts_us: None,
                            kind: MetricKind::CpuAppPct,
                            value: pct,
                            labels: smallvec![],
                        });
                        emitted_app = true;
                    }
                }
                // App not running this tick (killed / crashed / not yet
                // (re)launched) → emit 0 so the App line keeps drawing at
                // 0% instead of leaving its last value frozen at the right
                // edge. App restart picks up the real value one tick later
                // (delta needs prev jiffies of the new PID).
                if !emitted_app && curr_app_jiffies.is_none() {
                    yield Ok(Sample {
                        ts_us: ts,
                        device_ts_us: None,
                        kind: MetricKind::CpuAppPct,
                        value: 0.0,
                        labels: smallvec![],
                    });
                }
                last_app_jiffies = curr_app_jiffies;

                last = curr;
            }
        };

        Ok(Box::pin(s))
    }
}

#[derive(Debug, Clone)]
struct CpuTimes {
    /// user + nice + sys + irq + softirq + iowait + idle
    /// (we treat iowait as load, idle alone as idle)
    user: u64,
    nice: u64,
    sys: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
}

impl CpuTimes {
    fn busy(&self) -> u64 {
        self.user + self.nice + self.sys + self.irq + self.softirq + self.iowait
    }
    fn total(&self) -> u64 {
        self.busy() + self.idle
    }
}

#[derive(Debug)]
struct ProcStat {
    total: CpuTimes,
    cores: Vec<CpuTimes>,
}

async fn read_proc_stat(serial: &str) -> Result<ProcStat, SamplerError> {
    let raw = adb::shell(serial, "cat /proc/stat").await?;
    parse_proc_stat(&raw)
        .ok_or_else(|| SamplerError::Fatal(anyhow::anyhow!("unparseable /proc/stat")))
}

fn parse_proc_stat(out: &str) -> Option<ProcStat> {
    let mut total: Option<CpuTimes> = None;
    let mut cores: Vec<CpuTimes> = Vec::new();

    for line in out.lines() {
        let line = line.trim_start();
        if !line.starts_with("cpu") {
            continue;
        }
        let mut parts = line.split_whitespace();
        let head = parts.next()?;
        let nums: Vec<u64> = parts.filter_map(|x| x.parse().ok()).collect();
        if nums.len() < 7 {
            continue;
        }
        let times = CpuTimes {
            user: nums[0],
            nice: nums[1],
            sys: nums[2],
            idle: nums[3],
            iowait: nums[4],
            irq: nums[5],
            softirq: nums[6],
        };
        if head == "cpu" {
            total = Some(times);
        } else if let Some(idx_str) = head.strip_prefix("cpu") {
            if idx_str.parse::<usize>().is_ok() {
                cores.push(times);
            }
        }
    }

    Some(ProcStat {
        total: total?,
        cores,
    })
}

fn cpu_pct(a: &CpuTimes, b: &CpuTimes) -> Option<f64> {
    let total_delta = b.total().checked_sub(a.total())?;
    let busy_delta = b.busy().checked_sub(a.busy())?;
    if total_delta == 0 {
        return None;
    }
    Some((busy_delta as f64 / total_delta as f64) * 100.0)
}

/// One adb round-trip that resolves all PIDs of `pkg` via `pidof` and
/// dumps each `/proc/<pid>/stat`. Returns the summed `utime + stime`
/// across every matching process, or `None` if the package isn't running
/// or the package name fails the safety check.
async fn sum_app_jiffies(serial: &str, pkg: &str) -> Option<u64> {
    if !adb::is_safe_pkg_name(pkg) {
        tracing::warn!(pkg, "android.cpu: unsafe package name, skipping app CPU");
        return None;
    }
    // `pidof` ships with toybox on Android 6+; for older devices it may
    // return non-zero. Suppress its stderr so adb::shell doesn't treat
    // the missing tool as a fatal error.
    let cmd = format!(
        "for p in $(pidof {pkg} 2>/dev/null); do cat /proc/$p/stat 2>/dev/null; done"
    );
    let raw = adb::shell(serial, &cmd).await.ok()?;
    let mut sum: u64 = 0;
    let mut matched = 0;
    for line in raw.lines() {
        if let Some(j) = parse_proc_pid_stat(line) {
            sum = sum.saturating_add(j);
            matched += 1;
        }
    }
    if matched == 0 {
        None
    } else {
        Some(sum)
    }
}

/// Parse a `/proc/<pid>/stat` line and return `utime + stime` (in clock
/// ticks). The `comm` field (column 2) is parenthesised and may contain
/// spaces, so we locate the closing `)` and tokenise everything after.
/// In that tail, indices 11 / 12 are utime / stime (man 5 proc fields
/// 14 / 15 minus the 3 pre-tail columns).
fn parse_proc_pid_stat(line: &str) -> Option<u64> {
    let close = line.rfind(')')?;
    let tail = &line[close + 1..];
    let fields: Vec<&str> = tail.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime.saturating_add(stime))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
cpu  100 0 50 800 10 5 5 0 0 0
cpu0 50 0 25 400 5 2 2 0 0 0
cpu1 50 0 25 400 5 3 3 0 0 0
intr 12345 ...
ctxt 67890
";

    #[test]
    fn parses_two_cores() {
        let p = parse_proc_stat(SAMPLE).unwrap();
        assert_eq!(p.cores.len(), 2);
        assert_eq!(p.total.user, 100);
        assert_eq!(p.cores[0].user, 50);
    }

    #[test]
    fn cpu_pct_full_load() {
        let a = CpuTimes { user: 0, nice: 0, sys: 0, idle: 100, iowait: 0, irq: 0, softirq: 0 };
        let b = CpuTimes { user: 100, nice: 0, sys: 0, idle: 100, iowait: 0, irq: 0, softirq: 0 };
        let p = cpu_pct(&a, &b).unwrap();
        assert!((p - 100.0).abs() < 0.01);
    }

    #[test]
    fn cpu_pct_idle() {
        let a = CpuTimes { user: 0, nice: 0, sys: 0, idle: 0, iowait: 0, irq: 0, softirq: 0 };
        let b = CpuTimes { user: 0, nice: 0, sys: 0, idle: 100, iowait: 0, irq: 0, softirq: 0 };
        let p = cpu_pct(&a, &b).unwrap();
        assert!(p.abs() < 0.01);
    }

    #[test]
    fn parse_pid_stat_basic() {
        // Synthetic stat line. comm = "(zygote)"
        // Layout: pid (comm) state ppid pgrp session tty_nr tpgid flags
        //         minflt cminflt majflt cmajflt utime stime ...
        let line = "1234 (zygote) S 1 1234 1234 0 -1 4194560 5 0 0 0 120 80 0 0 20 0 1 0 ...";
        assert_eq!(parse_proc_pid_stat(line), Some(200));
    }

    #[test]
    fn parse_pid_stat_comm_with_spaces() {
        // comm can contain spaces and parens; we anchor on the last ')'.
        let line =
            "5678 (Hello (World) Run) R 1 5678 5678 0 -1 0 1 0 0 0 50 70 0 0 20 0 1 0 ...";
        assert_eq!(parse_proc_pid_stat(line), Some(120));
    }

}
