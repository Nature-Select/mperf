//! Android temperature sampler. Reads `/sys/class/thermal/thermal_zone*/`
//! every 2 seconds and emits the **maximum** CPU/SoC zone temperature as
//! `CpuTempC`.
//!
//! Why `max` and not `avg`: thermal throttling kicks in when *any* zone
//! crosses its trip point. Averaging hides hot cores behind cool ones and
//! makes "device is throttling now" undetectable from the chart.
//!
//! Zone selection: zone `type` files name the sensor. Across SoCs we see:
//! Snapdragon `cpu0-0..cpu7-0`, MediaTek `cpu_thermal0`, Samsung Exynos
//! `BIG/LITTLE`, Tensor `tsensX`. We match any zone whose lowercase type
//! contains `cpu`, `soc`, `tsens`, `big`, `little`, or `kryo`. If no zone
//! matches (some vendors restrict /sys access for non-root), we fall back
//! to the max of all readable zones.

use crate::adb;
use async_stream::stream;
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use mperf_schema::{MetricKind, Sample, Sampler, SamplerCtx, SamplerError};
use smallvec::smallvec;
use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};

const MIN_INTERVAL_MS: u64 = 500;
pub const DEFAULT_INTERVAL_MS: u64 = 2000;

/// One adb shell roundtrip dumps every zone's `type` and `temp`. Cheaper
/// than per-zone calls and avoids the ~30ms shell-spawn overhead each.
const DUMP_CMD: &str = "for z in /sys/class/thermal/thermal_zone*; do \
    echo \"$(cat $z/type 2>/dev/null):$(cat $z/temp 2>/dev/null)\"; \
    done";

const CPU_KEYWORDS: &[&str] =
    &["cpu", "soc", "tsens", "big", "little", "kryo"];

pub struct TempSampler {
    serial: String,
    interval_ms: u64,
}

impl TempSampler {
    pub fn new(serial: impl Into<String>, interval_ms: u64) -> Self {
        Self {
            serial: serial.into(),
            interval_ms: interval_ms.max(MIN_INTERVAL_MS),
        }
    }
}

#[async_trait]
impl Sampler for TempSampler {
    fn name(&self) -> &'static str {
        "android.temp"
    }

    fn target_hz(&self) -> f32 {
        1000.0 / self.interval_ms as f32
    }

    async fn start(
        &mut self,
        ctx: SamplerCtx,
    ) -> Result<BoxStream<'static, Result<Sample, SamplerError>>, SamplerError> {
        let serial = self.serial.clone();
        let interval_ms = self.interval_ms;
        let clock = ctx.clock.clone();
        let s = stream! {
            let mut ticker = interval(Duration::from_millis(interval_ms));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            ticker.tick().await; // skip immediate
            // OEMs that restrict /sys/class/thermal for shell users will
            // return empty cats forever; warn once and stop emitting
            // (instead of spamming the chart with zeros).
            let mut empty_strikes = 0u32;
            loop {
                ticker.tick().await;
                let raw = match adb::shell(&serial, DUMP_CMD).await {
                    Ok(v) => v,
                    Err(e) => {
                        // Disconnect / fatal: propagate. Transient errors
                        // we just skip this tick.
                        if !e.is_retriable() {
                            yield Err(e);
                            return;
                        }
                        continue;
                    }
                };
                match max_cpu_temp_c(&raw) {
                    Some(c) => {
                        empty_strikes = 0;
                        yield Ok(Sample {
                            ts_us: clock.now_us(),
                            device_ts_us: None,
                            kind: MetricKind::CpuTempC,
                            value: c,
                            labels: smallvec![],
                        });
                    }
                    None => {
                        empty_strikes += 1;
                        if empty_strikes == 3 {
                            tracing::warn!(
                                sampler = "android.temp",
                                "no readable thermal_zone after 3 polls — \
                                vendor likely restricts /sys for shell user; \
                                stopping temperature emission"
                            );
                            return;
                        }
                    }
                }
            }
        };
        Ok(Box::pin(s))
    }
}

/// Parse the dump output (one `type:millideg` per line) and return the
/// max temperature in °C across CPU/SoC-related zones. Falls back to the
/// max of all zones if no CPU zone matches.
fn max_cpu_temp_c(out: &str) -> Option<f64> {
    let mut cpu_max: Option<i64> = None;
    let mut any_max: Option<i64> = None;
    for line in out.lines() {
        let line = line.trim();
        let (type_str, temp_str) = match line.split_once(':') {
            Some(p) => p,
            None => continue,
        };
        let temp_md: i64 = match temp_str.trim().parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Sanity: 1°C..150°C in millidegrees. Lower bound rejects offline
        // sensors that report 0 (or near-zero) and uninitialized garbage
        // like single-digit raw values; upper rejects sentinel huge ints
        // some zones use to mean "no reading".
        if !(1_000..=150_000).contains(&temp_md) {
            continue;
        }
        any_max = Some(any_max.map_or(temp_md, |a| a.max(temp_md)));
        let lower = type_str.to_ascii_lowercase();
        if CPU_KEYWORDS.iter().any(|k| lower.contains(k)) {
            cpu_max = Some(cpu_max.map_or(temp_md, |a| a.max(temp_md)));
        }
    }
    cpu_max.or(any_max).map(|md| md as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_max_cpu_zone() {
        let s = "\
cpu0-0:42000
cpu0-1:55000
gpu_thermal:60000
battery:31000
soc-thermal:50000
";
        // CPU/SoC max = max(42, 55, 50) = 55°C; gpu/battery skipped.
        let v = max_cpu_temp_c(s).unwrap();
        assert!((v - 55.0).abs() < 0.001);
    }

    #[test]
    fn falls_back_when_no_cpu_zone() {
        let s = "battery:31000\nskin-therm:33000\n";
        let v = max_cpu_temp_c(s).unwrap();
        assert!((v - 33.0).abs() < 0.001);
    }

    #[test]
    fn rejects_sentinel_values() {
        let s = "cpu0-0:42000\ncpu1-0:9999999\n";
        let v = max_cpu_temp_c(s).unwrap();
        assert!((v - 42.0).abs() < 0.001);
    }

    #[test]
    fn empty_returns_none() {
        assert!(max_cpu_temp_c("").is_none());
        assert!(max_cpu_temp_c("type:\n:42\n").is_none());
    }

    #[test]
    fn matches_exynos_naming() {
        let s = "BIG:48000\nLITTLE:42000\nGPU:55000\n";
        let v = max_cpu_temp_c(s).unwrap();
        assert!((v - 48.0).abs() < 0.001);
    }
}
