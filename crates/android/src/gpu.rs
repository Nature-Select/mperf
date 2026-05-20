//! Android GPU utilization. Best-effort — every SoC family puts the
//! counter somewhere different, and Android 11+ OEMs increasingly lock
//! /sys for the shell user.
//!
//! Strategy: at sampler start, run a single `ls` to discover which (if
//! any) of the known counter paths is readable, then read **that one
//! path** per tick. We tried a one-shot probe script with `for` loops
//! and `$()` substitutions on the device — Samsung toybox would hang
//! indefinitely on it (never returned to adb), even though simpler
//! single-file reads work fine. Splitting into simple discrete commands
//! avoids the issue entirely.
//!
//! Sources we try, in priority order:
//!   1. **Adreno KGSL** `/sys/class/kgsl/kgsl-3d0/gpubusy` — `<busy> <total>` snapshot since last read.
//!   2. **Mali devfreq load@freq** — `<load>@<freq_hz>`; load is 0..100 or 0..1024.
//!   3. **Mali platform utilization** — single integer percent.

use crate::adb;
use async_stream::stream;
use async_trait::async_trait;
use futures::stream as fstream;
use futures::StreamExt;
use futures_core::stream::BoxStream;
use mperf_schema::{MetricKind, Sample, Sampler, SamplerCtx, SamplerError};
use smallvec::smallvec;
use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};

const MIN_INTERVAL_MS: u64 = 200;

/// Probe each candidate path with an individual `ls -1d` (semicolon-chained,
/// no `for` loops or `$()` — Samsung toybox hangs on those). Each `ls`
/// fails silently when its path doesn't match. Trailing `; true` is
/// **critical**: without it, `ls -1d` exits non-zero whenever ANY path
/// is missing — even with `2>/dev/null` to hide stderr — which our adb
/// wrapper then maps to `TransientIo` and the whole sampler bails.
///
/// Path list covers:
/// - Qualcomm Adreno KGSL (`/sys/class/kgsl/...`)
/// - ARM Mali devfreq, several path variants by kernel age
/// - Exynos legacy platform `*.mali/utilization`
/// - Samsung One UI 5+ kernel pseudo-file (`/sys/kernel/gpu/gpu_busy`)
const DISCOVERY_CMD: &str = "\
ls -1d /sys/class/kgsl/kgsl-3d0/gpubusy 2>/dev/null; \
ls -1d /sys/class/devfreq/*.gpu/load@* 2>/dev/null; \
ls -1d /sys/class/devfreq/gpu/load@* 2>/dev/null; \
ls -1d /sys/class/devfreq/*.mali/load@* 2>/dev/null; \
ls -1d /sys/class/devfreq/*.gpu_perf/load@* 2>/dev/null; \
ls -1d /sys/devices/platform/*.mali/utilization 2>/dev/null; \
ls -1d /sys/class/misc/mali0/device/utilization 2>/dev/null; \
ls -1d /sys/kernel/gpu/gpu_busy 2>/dev/null; \
true";

#[derive(Debug, Clone, Copy)]
enum Source {
    /// `/sys/class/kgsl/kgsl-3d0/gpubusy` — Qualcomm Adreno.
    /// Read returns "<busy_us> <total_us>" snapshot since previous read.
    AdrenoKgsl,
    /// `/sys/class/devfreq/.../load@<freq>` — ARM Mali devfreq.
    /// Read returns "<load>@<freq>" where load is 0..100 (or 0..1024 on
    /// older kernels — we auto-detect).
    MaliDevfreq,
    /// `/sys/.../utilization` — Mali platform sysfs, single integer percent.
    MaliPlatformUtil,
    /// `/sys/kernel/gpu/gpu_busy` — Samsung One UI 5+ aggregated counter,
    /// single integer percent.
    SamsungKernelGpuBusy,
}

pub struct GpuSampler {
    serial: String,
    interval_ms: u64,
}

impl GpuSampler {
    pub fn new(serial: impl Into<String>, interval_ms: u64) -> Self {
        Self {
            serial: serial.into(),
            interval_ms: interval_ms.max(MIN_INTERVAL_MS),
        }
    }
}

#[async_trait]
impl Sampler for GpuSampler {
    fn name(&self) -> &'static str {
        "android.gpu"
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

        // ---- One-time discovery up-front ----
        let discovery = match adb::shell(&serial, DISCOVERY_CMD).await {
            Ok(s) => s,
            Err(e) => {
                if !e.is_retriable() {
                    return Err(e);
                }
                // Transient adb error on discovery → return an empty
                // stream; let other samplers carry on without us.
                tracing::warn!(sampler = "android.gpu", error = %e, "GPU discovery failed; not emitting");
                return Ok(empty_stream());
            }
        };
        let (source, path) = match pick_source(&discovery) {
            Some(p) => p,
            None => {
                tracing::warn!(
                    sampler = "android.gpu",
                    discovery_output = %discovery.trim(),
                    "no readable GPU counter path on this device — vendor likely restricts /sys for shell user; not emitting"
                );
                return Ok(empty_stream());
            }
        };
        tracing::info!(
            sampler = "android.gpu",
            ?source,
            path,
            "GPU counter source resolved"
        );

        let s = stream! {
            let mut ticker = interval(Duration::from_millis(interval_ms));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            ticker.tick().await; // skip immediate

            // Trailing `; true` for the same reason as DISCOVERY_CMD: cat
            // of a sysfs node that the kernel briefly makes 0-byte during
            // GPU power-collapse can exit non-zero on some kernels.
            let cat_cmd = format!("cat {} 2>/dev/null; true", path);
            let mut consecutive_empty = 0u32;
            loop {
                ticker.tick().await;
                let raw = match adb::shell(&serial, &cat_cmd).await {
                    Ok(v) => v,
                    Err(e) => {
                        if !e.is_retriable() { yield Err(e); return; }
                        continue;
                    }
                };
                let trimmed = raw.trim();
                let parsed = match source {
                    Source::AdrenoKgsl => parse_adreno(trimmed),
                    Source::MaliDevfreq => parse_mali_devfreq(trimmed),
                    Source::MaliPlatformUtil | Source::SamsungKernelGpuBusy => {
                        parse_simple_pct(trimmed)
                    }
                };
                match parsed {
                    Some(pct) => {
                        consecutive_empty = 0;
                        yield Ok(Sample {
                            ts_us: clock.now_us(),
                            device_ts_us: None,
                            kind: MetricKind::GpuDevicePct,
                            value: pct,
                            labels: smallvec![],
                        });
                    }
                    None => {
                        consecutive_empty += 1;
                        if consecutive_empty == 5 {
                            tracing::warn!(
                                sampler = "android.gpu",
                                ?source,
                                last_raw = trimmed,
                                "5 consecutive unparseable reads; stopping GPU emission"
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

/// Typed empty stream for the "no GPU counter readable" path.
fn empty_stream() -> BoxStream<'static, Result<Sample, SamplerError>> {
    fstream::empty().boxed()
}

/// Pick the first valid path discovery returned, mapped to its parser.
/// Priority: Adreno KGSL > Mali devfreq > Samsung kernel aggregated >
/// Mali platform util. The order encodes our confidence in each source.
fn pick_source(discovery: &str) -> Option<(Source, String)> {
    let mut kgsl: Option<String> = None;
    let mut mali_devfreq: Option<String> = None;
    let mut samsung_busy: Option<String> = None;
    let mut mali_util: Option<String> = None;
    for line in discovery.lines() {
        let p = line.trim();
        if p.is_empty() {
            continue;
        }
        if p == "/sys/class/kgsl/kgsl-3d0/gpubusy" {
            kgsl = Some(p.to_string());
        } else if p.contains("/devfreq/") && p.contains("/load@") {
            mali_devfreq.get_or_insert_with(|| p.to_string());
        } else if p == "/sys/kernel/gpu/gpu_busy" {
            samsung_busy = Some(p.to_string());
        } else if p.ends_with("/utilization") {
            mali_util.get_or_insert_with(|| p.to_string());
        }
    }
    if let Some(p) = kgsl {
        return Some((Source::AdrenoKgsl, p));
    }
    if let Some(p) = mali_devfreq {
        return Some((Source::MaliDevfreq, p));
    }
    if let Some(p) = samsung_busy {
        return Some((Source::SamsungKernelGpuBusy, p));
    }
    if let Some(p) = mali_util {
        return Some((Source::MaliPlatformUtil, p));
    }
    None
}

/// Adreno: `<busy_us> <total_us>` — read snapshots since previous read.
/// `total == 0` means the GPU was idle the entire window — that's a real
/// 0% reading, NOT a parse failure. (Treating it as None caused the
/// sampler to stop after a few seconds of idle GPU.)
fn parse_adreno(s: &str) -> Option<f64> {
    let mut parts = s.split_whitespace();
    let busy: u64 = parts.next()?.parse().ok()?;
    let total: u64 = parts.next()?.parse().ok()?;
    if total == 0 {
        return Some(0.0);
    }
    Some(((busy as f64 / total as f64) * 100.0).clamp(0.0, 100.0))
}

/// Mali devfreq `load@freq` — load is 0..100 on most kernels, 0..1024
/// on some older ones (we auto-detect by range).
fn parse_mali_devfreq(s: &str) -> Option<f64> {
    let load_str = s.split('@').next()?.trim();
    let load: f64 = load_str.parse().ok()?;
    let pct = if load <= 100.0 { load } else { (load / 1024.0) * 100.0 };
    Some(pct.clamp(0.0, 100.0))
}

fn parse_simple_pct(s: &str) -> Option<f64> {
    let n: f64 = s.trim().parse().ok()?;
    Some(n.clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_picks_kgsl_when_present() {
        let s = "/sys/class/kgsl/kgsl-3d0/gpubusy\n/sys/class/devfreq/13000000.mali/load@800000000\n";
        let (src, path) = pick_source(s).unwrap();
        assert!(matches!(src, Source::AdrenoKgsl));
        assert_eq!(path, "/sys/class/kgsl/kgsl-3d0/gpubusy");
    }

    #[test]
    fn discovery_falls_back_to_mali_devfreq() {
        let s = "/sys/class/devfreq/13000000.mali/load@800000000\n";
        let (src, _) = pick_source(s).unwrap();
        assert!(matches!(src, Source::MaliDevfreq));
    }

    #[test]
    fn discovery_falls_back_to_mali_util() {
        let s = "/sys/devices/platform/13000000.mali/utilization\n";
        let (src, _) = pick_source(s).unwrap();
        assert!(matches!(src, Source::MaliPlatformUtil));
    }

    #[test]
    fn discovery_empty_returns_none() {
        assert!(pick_source("").is_none());
        assert!(pick_source("\n\n").is_none());
    }

    #[test]
    fn discovery_picks_samsung_kernel_when_only_choice() {
        // Most Samsung One UI 5+ devices: nothing under /sys/class/devfreq
        // accessible, only this aggregated counter.
        let s = "/sys/kernel/gpu/gpu_busy\n";
        let (src, _) = pick_source(s).unwrap();
        assert!(matches!(src, Source::SamsungKernelGpuBusy));
    }

    #[test]
    fn parse_adreno_basic() {
        assert!((parse_adreno("5000 10000").unwrap() - 50.0).abs() < 0.01);
        // "0 0" means GPU was idle for the whole sample window — emit 0%,
        // not None (returning None would let the sampler die on idle).
        assert_eq!(parse_adreno("0 0"), Some(0.0));
        // Garbage / single-token still rejected.
        assert!(parse_adreno("").is_none());
        assert!(parse_adreno("foo bar").is_none());
        assert!(parse_adreno("1234").is_none());
    }

    #[test]
    fn parse_mali_devfreq_units() {
        assert!((parse_mali_devfreq("42@800000000").unwrap() - 42.0).abs() < 0.01);
        assert!((parse_mali_devfreq("512@800000000").unwrap() - 50.0).abs() < 0.01);
    }
}
