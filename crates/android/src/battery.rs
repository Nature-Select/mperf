//! Android battery sampler. Reads `dumpsys battery` every 2s and emits:
//! `BatteryTempC`, `BatteryLevelPct`, `BatteryVoltageMv`. Universal —
//! works on virtually every Android since API 14, no root required.
//!
//! Compared to thermal_zone, dumpsys battery is much more reliable:
//! OEMs (Samsung, Xiaomi, Huawei) lock /sys/class/thermal but never
//! lock dumpsys for the shell user, so this is the temperature signal
//! we can count on across the fleet.

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

pub struct BatterySampler {
    serial: String,
    interval_ms: u64,
}

impl BatterySampler {
    pub fn new(serial: impl Into<String>, interval_ms: u64) -> Self {
        Self {
            serial: serial.into(),
            interval_ms: interval_ms.max(MIN_INTERVAL_MS),
        }
    }
}

#[async_trait]
impl Sampler for BatterySampler {
    fn name(&self) -> &'static str {
        "android.battery"
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
            loop {
                ticker.tick().await;
                let raw = match adb::shell(&serial, "dumpsys battery").await {
                    Ok(v) => v,
                    Err(e) => {
                        if !e.is_retriable() {
                            yield Err(e);
                            return;
                        }
                        continue;
                    }
                };
                let parsed = parse_dumpsys_battery(&raw);
                let ts = clock.now_us();
                if let Some(c) = parsed.temperature_c {
                    yield Ok(Sample {
                        ts_us: ts, device_ts_us: None,
                        kind: MetricKind::BatteryTempC,
                        value: c,
                        labels: smallvec![],
                    });
                }
                if let Some(p) = parsed.level_pct {
                    yield Ok(Sample {
                        ts_us: ts, device_ts_us: None,
                        kind: MetricKind::BatteryLevelPct,
                        value: p,
                        labels: smallvec![],
                    });
                }
                if let Some(v) = parsed.voltage_mv {
                    yield Ok(Sample {
                        ts_us: ts, device_ts_us: None,
                        kind: MetricKind::BatteryVoltageMv,
                        value: v as f64,
                        labels: smallvec![],
                    });
                }
            }
        };
        Ok(Box::pin(s))
    }
}

#[derive(Debug, Default, PartialEq)]
struct BatteryReading {
    /// °C — `temperature` field is in tenths of °C.
    temperature_c: Option<f64>,
    /// 0..100 — derived from `level` divided by `scale` (almost always 100).
    level_pct: Option<f64>,
    /// mV — `voltage` field is already in millivolts.
    voltage_mv: Option<i64>,
}

/// Parse `dumpsys battery` output. Sample lines:
///   `  level: 87`
///   `  scale: 100`
///   `  voltage: 4123`
///   `  temperature: 312`     ← tenths of °C → 31.2 °C
fn parse_dumpsys_battery(out: &str) -> BatteryReading {
    let mut level: Option<i64> = None;
    let mut scale: Option<i64> = None;
    let mut r = BatteryReading::default();
    for line in out.lines() {
        let line = line.trim();
        if let Some(v) = strip_kv(line, "temperature") {
            if let Ok(n) = v.parse::<i64>() {
                r.temperature_c = Some(n as f64 / 10.0);
            }
        } else if let Some(v) = strip_kv(line, "level") {
            level = v.parse().ok();
        } else if let Some(v) = strip_kv(line, "scale") {
            scale = v.parse().ok();
        } else if let Some(v) = strip_kv(line, "voltage") {
            r.voltage_mv = v.parse().ok();
        }
    }
    if let (Some(l), Some(s)) = (level, scale) {
        if s > 0 {
            r.level_pct = Some((l as f64 / s as f64) * 100.0);
        }
    } else if let Some(l) = level {
        // Some devices omit `scale` — assume the modern default of 100.
        r.level_pct = Some(l as f64);
    }
    r
}

/// Trim "key: value" lines tolerant of leading whitespace and exact
/// match on the key portion.
fn strip_kv<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(key)?.trim_start();
    rest.strip_prefix(':').map(|v| v.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_modern_dumpsys() {
        let s = "\
Current Battery Service state:
  AC powered: true
  USB powered: false
  level: 87
  scale: 100
  voltage: 4123
  temperature: 312
  technology: Li-ion
";
        let r = parse_dumpsys_battery(s);
        assert_eq!(r.temperature_c, Some(31.2));
        assert_eq!(r.level_pct, Some(87.0));
        assert_eq!(r.voltage_mv, Some(4123));
    }

    #[test]
    fn parse_handles_missing_scale() {
        let s = "  level: 50\n  temperature: 250\n";
        let r = parse_dumpsys_battery(s);
        assert_eq!(r.level_pct, Some(50.0));
        assert_eq!(r.temperature_c, Some(25.0));
    }

    #[test]
    fn parse_handles_non_centred_scale() {
        // Some old devices report scale=255.
        let s = "  level: 200\n  scale: 255\n";
        let r = parse_dumpsys_battery(s);
        let p = r.level_pct.unwrap();
        assert!((p - 78.43).abs() < 0.01);
    }

    #[test]
    fn parse_empty() {
        let r = parse_dumpsys_battery("");
        assert_eq!(r, BatteryReading::default());
    }

    #[test]
    fn parse_ignores_unrelated_keys() {
        // A "temperature:" substring inside another key shouldn't match.
        let s = "  power_temperature_guess: 999\n  temperature: 280\n";
        let r = parse_dumpsys_battery(s);
        assert_eq!(r.temperature_c, Some(28.0));
    }
}
