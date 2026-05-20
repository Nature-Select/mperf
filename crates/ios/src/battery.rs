//! iOS battery sampler. Polls lockdown's `com.apple.mobile.battery`
//! domain every ~3 seconds and emits `BatteryTempC` and
//! `BatteryLevelPct`. Same data source we already use for the static
//! "Battery" line in DeviceInfoPanel — just refreshed on a timer for
//! the live chart.
//!
//! Why polling instead of a sysmontap attribute: iOS sysmontap does not
//! expose battery temperature in `sysAttrs` (we logged the full list at
//! startup, no battery key). Lockdown is the only third-party-accessible
//! source. ~150ms per query over USB; 3s period keeps overhead trivial.

use crate::connect;
use async_stream::stream;
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use idevice::{services::lockdown::LockdownClient, IdeviceService};
use mperf_schema::{MetricKind, Sample, Sampler, SamplerCtx, SamplerError};
use smallvec::smallvec;
use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};

const MIN_INTERVAL_MS: u64 = 500;
const DOMAIN: &str = "com.apple.mobile.battery";

pub struct BatterySampler {
    udid: String,
    interval_ms: u64,
}

impl BatterySampler {
    pub fn new(udid: impl Into<String>, interval_ms: u64) -> Self {
        Self {
            udid: udid.into(),
            interval_ms: interval_ms.max(MIN_INTERVAL_MS),
        }
    }
}

#[async_trait]
impl Sampler for BatterySampler {
    fn name(&self) -> &'static str {
        "ios.battery"
    }

    fn target_hz(&self) -> f32 {
        1000.0 / self.interval_ms as f32
    }

    async fn start(
        &mut self,
        ctx: SamplerCtx,
    ) -> Result<BoxStream<'static, Result<Sample, SamplerError>>, SamplerError> {
        // Cheap up-front reachability check; full polls happen inside.
        let _provider = connect::provider_for(&self.udid).await.map_err(map_setup)?;

        let udid = self.udid.clone();
        let interval_ms = self.interval_ms;
        let clock = ctx.clock.clone();
        let s = stream! {
            let mut ticker = interval(Duration::from_millis(interval_ms));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            ticker.tick().await; // skip immediate
            let mut warned_no_temp = false;
            let mut first_tick = true;
            let mut fail_count = 0u32;
            loop {
                ticker.tick().await;
                let dict = match query_battery_raw(&udid).await {
                    Ok(d) => {
                        fail_count = 0;
                        d
                    }
                    Err(e) => {
                        fail_count += 1;
                        if fail_count <= 3 {
                            tracing::warn!(
                                sampler = "ios.battery",
                                fail_count,
                                error = %e,
                                "lockdown query failed"
                            );
                        }
                        continue;
                    }
                };
                // First successful tick: dump all keys + a sample of
                // numeric values so we can identify the right temperature
                // field across iOS versions.
                if first_tick {
                    first_tick = false;
                    let keys: Vec<&str> = dict.keys().map(|k| k.as_str()).collect();
                    tracing::info!(
                        sampler = "ios.battery",
                        keys = ?keys,
                        "first lockdown battery response — schema dump"
                    );
                    for (k, v) in dict.iter() {
                        if let Some(n) = value_as_f64(v) {
                            tracing::info!(
                                sampler = "ios.battery",
                                key = %k, value = n,
                                "battery numeric field"
                            );
                        }
                    }
                }
                let parsed = decode_battery(&dict);
                let ts = clock.now_us();
                if let Some(c) = parsed.temperature_c {
                    yield Ok(Sample {
                        ts_us: ts, device_ts_us: None,
                        kind: MetricKind::BatteryTempC,
                        value: c,
                        labels: smallvec![],
                    });
                } else if !warned_no_temp {
                    warned_no_temp = true;
                    let keys: Vec<&str> = dict.keys().map(|k| k.as_str()).collect();
                    tracing::warn!(
                        sampler = "ios.battery",
                        available_keys = ?keys,
                        "no temperature key matched in lockdown response; BatteryTempC will not be emitted"
                    );
                }
                if let Some(p) = parsed.level_pct {
                    yield Ok(Sample {
                        ts_us: ts, device_ts_us: None,
                        kind: MetricKind::BatteryLevelPct,
                        value: p,
                        labels: smallvec![],
                    });
                }
            }
        };
        Ok(Box::pin(s))
    }
}

#[derive(Debug, Default)]
struct BatteryReading {
    temperature_c: Option<f64>,
    level_pct: Option<f64>,
}

/// Query lockdown for battery info. iOS 17+ frequently returns an empty
/// dictionary when fetching the whole `com.apple.mobile.battery` domain
/// in one shot — especially while another tool holds a CoreDeviceProxy /
/// developer tunnel (which our cpu sampler does). The fix: ask for each
/// key individually. We assemble the responses into a Dictionary the
/// caller can decode just like a normal domain dump.
///
/// We also try keys with NO domain ("global" namespace) as a fallback,
/// since some keys (BatteryCurrentCapacity) live there too on certain
/// iOS versions.
async fn query_battery_raw(udid: &str) -> anyhow::Result<plist::Dictionary> {
    use anyhow::Context;
    let provider = connect::provider_for(udid).await?;
    let mut lockdown = LockdownClient::connect(&*provider)
        .await
        .context("lockdown connect")?;
    let mut out = plist::Dictionary::new();
    let candidates: &[&str] = &[
        "BatteryCurrentCapacity",
        "BatteryCurrentTemperature",
        "BatteryIsCharging",
        "ExternalChargeCapable",
        "ExternalConnected",
        "Temperature",
        "BatteryTemperature",
        "CurrentTemperature",
        "RawBatteryTemperature",
        "CurrentCapacity",
        "BatteryLevel",
    ];
    for key in candidates {
        // First try in the battery domain; fall back to no-domain.
        let v = match lockdown.get_value(Some(*key), Some(DOMAIN)).await {
            Ok(v) => Some(v),
            Err(_) => lockdown.get_value(Some(*key), None).await.ok(),
        };
        if let Some(v) = v {
            // Empty dict / null responses → skip (key not present here).
            if !matches!(&v, plist::Value::Dictionary(d) if d.is_empty()) {
                out.insert((*key).to_string(), v);
            }
        }
    }
    if out.is_empty() {
        anyhow::bail!("no battery keys returned");
    }
    Ok(out)
}

/// Lockdown temperature is stored in *centi-celsius* (3245 = 32.45°C) on
/// every variant we know about. Different iOS versions / device classes
/// use different key names — we try the most common ones in order. If
/// nothing parses to a plausible value the caller logs the full key list
/// for adjustment.
const TEMP_KEYS: &[&str] = &[
    "BatteryCurrentTemperature",
    "Temperature",
    "BatteryTemperature",
    "CurrentTemperature",
    "RawBatteryTemperature",
];

const LEVEL_KEYS: &[&str] = &[
    "BatteryCurrentCapacity",
    "CurrentCapacity",
    "BatteryLevel",
];

fn decode_battery(d: &plist::Dictionary) -> BatteryReading {
    let mut r = BatteryReading::default();
    for k in TEMP_KEYS {
        if let Some(raw) = d.get(*k).and_then(value_as_f64) {
            let c = raw / 100.0;
            if (-10.0..=70.0).contains(&c) {
                r.temperature_c = Some(c);
                break;
            }
            // Try tenths if centi-celsius gave us an absurd value.
            let c10 = raw / 10.0;
            if (-10.0..=70.0).contains(&c10) {
                r.temperature_c = Some(c10);
                break;
            }
        }
    }
    for k in LEVEL_KEYS {
        if let Some(p) = d.get(*k).and_then(value_as_f64) {
            if (0.0..=100.0).contains(&p) {
                r.level_pct = Some(p);
                break;
            }
        }
    }
    r
}

fn value_as_f64(v: &plist::Value) -> Option<f64> {
    match v {
        plist::Value::Integer(i) => i.as_signed().map(|x| x as f64),
        plist::Value::Real(r) => Some(*r),
        _ => None,
    }
}

fn map_setup(e: anyhow::Error) -> SamplerError {
    let msg = format!("{e:#}");
    let lower = msg.to_ascii_lowercase();
    if lower.contains("not found") || lower.contains("disconnect") {
        SamplerError::DeviceDisconnected(msg)
    } else if lower.contains("trust") || lower.contains("paired") {
        SamplerError::PermissionDenied(msg)
    } else {
        SamplerError::Fatal(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plist::{Dictionary, Value};

    fn dict(entries: &[(&str, Value)]) -> Dictionary {
        let mut d = Dictionary::new();
        for (k, v) in entries {
            d.insert((*k).to_string(), v.clone());
        }
        d
    }

    #[test]
    fn decodes_centi_celsius() {
        let d = dict(&[
            ("BatteryCurrentTemperature", Value::Integer(3245.into())),
            ("BatteryCurrentCapacity", Value::Integer(82.into())),
        ]);
        let r = decode_battery(&d);
        assert_eq!(r.level_pct, Some(82.0));
        assert!((r.temperature_c.unwrap() - 32.45).abs() < 0.001);
    }

    #[test]
    fn rejects_implausible_temp() {
        // 999999 / 100 = 9999°C, outside -10..70 range.
        let d = dict(&[(
            "BatteryCurrentTemperature",
            Value::Integer(999_999.into()),
        )]);
        let r = decode_battery(&d);
        assert!(r.temperature_c.is_none());
    }

    #[test]
    fn rejects_invalid_capacity() {
        let d = dict(&[("BatteryCurrentCapacity", Value::Integer(150.into()))]);
        let r = decode_battery(&d);
        assert!(r.level_pct.is_none());
    }
}
