//! Android memory sampler.
//!
//! Two metrics per tick:
//! * `MemAppPssBytes` — total PSS of the target package summed across all
//!   processes (`dumpsys meminfo <pkg>`).
//! * `MemSystemUsedBytes` — `MemTotal - MemAvailable` from `/proc/meminfo`,
//!   roughly the device-wide used RAM.
//!
//! Target package is mandatory — the user must pick an app before recording
//! (PerfDog-style explicit selection; no foreground auto-detect).

use crate::adb;
use async_stream::stream;
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use mperf_schema::{MetricKind, Sample, Sampler, SamplerCtx, SamplerError};
use smallvec::smallvec;
use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};

const MIN_INTERVAL_MS: u64 = 200;

pub struct MemSampler {
    serial: String,
    target_pkg: String,
    interval_ms: u64,
}

impl MemSampler {
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
impl Sampler for MemSampler {
    fn name(&self) -> &'static str {
        "android.mem"
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
        if !adb::is_safe_pkg_name(&pkg) {
            return Err(SamplerError::Fatal(anyhow::anyhow!(
                "refusing unsafe package name: {pkg}"
            )));
        }
        let clock = ctx.clock.clone();
        let s = stream! {
            let mut ticker = interval(Duration::from_millis(interval_ms));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            ticker.tick().await; // skip immediate
            loop {
                ticker.tick().await;
                let ts = clock.now_us();

                // System memory: cheap and never fails as long as adb works.
                if let Ok(raw) = adb::shell(&serial, "cat /proc/meminfo").await {
                    if let Some(used_kb) = parse_system_used_kb(&raw) {
                        yield Ok(Sample {
                            ts_us: ts, device_ts_us: None,
                            kind: MetricKind::MemSystemUsedBytes,
                            value: (used_kb * 1024) as f64,
                            labels: smallvec![],
                        });
                    }
                }

                // App PSS for the (validated) target package.
                match adb::shell(&serial, &format!("dumpsys meminfo {pkg}")).await {
                    Ok(raw) => {
                        // App not running: emit explicit 0 so the chart
                        // visibly drops instead of freezing at the last
                        // value before the app died.
                        let value = if raw.contains("No process found") {
                            0.0
                        } else {
                            let pss_kb = parse_total_pss_kb(&raw);
                            (pss_kb * 1024) as f64
                        };
                        tracing::debug!(
                            sampler = "android.mem",
                            pkg = %pkg,
                            bytes = value,
                            "mem tick"
                        );
                        yield Ok(Sample {
                            ts_us: ts, device_ts_us: None,
                            kind: MetricKind::MemAppPssBytes,
                            value,
                            labels: smallvec![],
                        });
                    }
                    Err(e) => {
                        let retriable = e.is_retriable();
                        yield Err(e);
                        if !retriable { return; }
                    }
                }
            }
        };
        Ok(Box::pin(s))
    }
}

/// Sum all per-process "TOTAL" PSS lines from `dumpsys meminfo <pkg>`.
/// Each `** MEMINFO in pid N **` section ends with:
///     `           TOTAL  XXXX  yyyy  zzzz  ...`
/// The first number after "TOTAL" is the process's PSS Total (in KB).
fn parse_total_pss_kb(text: &str) -> u64 {
    let mut sum_kb: u64 = 0;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("TOTAL") {
            // Skip "TOTAL PSS by ..." which is the category summary.
            if rest.starts_with(" PSS") || rest.starts_with(" RSS") {
                continue;
            }
            if let Some(n) = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<u64>().ok())
            {
                sum_kb += n;
            }
        }
    }
    sum_kb
}

/// Parse `/proc/meminfo` and return used = MemTotal - MemAvailable (in KB).
/// MemAvailable is preferred over MemFree because it accounts for
/// reclaimable cache.
fn parse_system_used_kb(text: &str) -> Option<u64> {
    let mut total: Option<u64> = None;
    let mut available: Option<u64> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = rest.split_whitespace().next().and_then(|s| s.parse().ok());
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available = rest.split_whitespace().next().and_then(|s| s.parse().ok());
        }
    }
    match (total, available) {
        (Some(t), Some(a)) if t >= a => Some(t - a),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pss_single_process() {
        let s = "\
** MEMINFO in pid 1234 [com.foo] **
  Native Heap   12345 ...
  Dalvik Heap    5678 ...
           TOTAL 56789 11111 ...

 Objects
   Views: 12
";
        assert_eq!(parse_total_pss_kb(s), 56789);
    }

    #[test]
    fn parse_pss_multi_process() {
        let s = "\
** MEMINFO in pid 1 [com.foo] **
           TOTAL 10000 ...
** MEMINFO in pid 2 [com.foo:srv] **
           TOTAL  5000 ...
Total PSS by category:
   Dalvik Heap: 1234
";
        // 10000 + 5000 = 15000. Category line is skipped because we
        // require the "TOTAL" prefix to be a whole word with a number
        // immediately after.
        assert_eq!(parse_total_pss_kb(s), 15000);
    }

    #[test]
    fn parse_pss_skips_category_summary() {
        // Make sure "TOTAL PSS by category" never adds a bogus number.
        let s = "TOTAL PSS by category:\n";
        assert_eq!(parse_total_pss_kb(s), 0);
    }

    #[test]
    fn parse_system_meminfo() {
        let s = "MemTotal: 16777216 kB\nMemFree:  1000000 kB\nMemAvailable: 5000000 kB\n";
        assert_eq!(parse_system_used_kb(s), Some(16777216 - 5000000));
    }

    #[test]
    fn parse_system_meminfo_missing() {
        assert_eq!(parse_system_used_kb("MemTotal: 1234 kB\n"), None);
    }
}
