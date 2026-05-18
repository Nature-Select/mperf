//! Android FPS / jank sampler.
//!
//! Hybrid strategy each tick (~1Hz):
//!
//! 1. **gfxinfo** for the target package — accurate for UI-heavy apps
//!    (browsers, social, productivity). Covers UI thread Choreographer
//!    frames + percentile frame times. Used for the headline FPS number.
//!
//! 2. **SurfaceView layer latency** — per-frame timestamps for games /
//!    video apps. Used both as an alternate FPS source AND as the input
//!    to PerfDog's three-tier jank classification.
//!
//! Headline FPS is the max of gfxinfo-derived and SurfaceView-derived
//! values. Jank stats (SmallJank/Jank/BigJank/Stutter) come only from the
//! SurfaceView path, which provides per-frame durations.
//!
//! ## PerfDog Jank formula
//!
//! For each frame with duration `d` (ms), classify as jank when:
//!
//! ```text
//! d > 2 * avg(previous 3 frame durations)
//! AND
//!   d > 41.66ms (1×24fps film-frame)   → SmallJank
//!   d > 83.33ms (2×)                   → Jank
//!   d > 125.00ms (3×)                  → BigJank
//! ```
//!
//! Film-frame thresholds come from PerfDog's documentation. They predate
//! 60/120Hz target rates but remain the industry-comparable convention.
//!
//! ## Stutter
//!
//! Cumulative `total_jank_duration_ms / total_session_duration_ms` for the
//! tracked layer. Reset on layer switch (app change in auto-detect mode).

use crate::adb;
use async_stream::stream;
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use mperf_schema::{MetricKind, Sample, Sampler, SamplerCtx, SamplerError};
use smallvec::smallvec;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use tokio::time::{interval, MissedTickBehavior};

const PERIOD_MS: u64 = 1000;
const MOVIE_FRAME_MS: f64 = 1000.0 / 24.0; // ~41.66ms

/// Per-layer state tracked across sampling ticks: last frame seen + the
/// PerfDog rolling 3-frame window + cumulative jank/total durations.
#[derive(Debug, Default)]
struct LayerState {
    /// Newest frame timestamp (seconds) we've already classified.
    max_ts: f64,
    /// Last 3 frame durations (ms). PerfDog jank check compares the next
    /// frame against `2 × avg(prev_3)`.
    prev_durations: VecDeque<f64>,
    /// Cumulative time (ms) spent inside jank frames (any tier).
    jank_duration_ms: f64,
    /// Cumulative time (ms) of the layer's recorded frames overall.
    total_duration_ms: f64,
}

#[derive(Debug, Default, Clone, Copy)]
struct JankTick {
    small_jank: u32,
    jank: u32,
    big_jank: u32,
}

impl LayerState {
    /// Classify a single new frame whose duration (ms) is `dt`. Returns the
    /// tier increment to add to this tick's PerfDog counters.
    fn classify_frame(&mut self, dt_ms: f64) -> JankTick {
        self.total_duration_ms += dt_ms;
        let tier = if self.prev_durations.len() == 3 {
            let avg_prev: f64 = self.prev_durations.iter().sum::<f64>() / 3.0;
            if dt_ms > 2.0 * avg_prev {
                if dt_ms > 3.0 * MOVIE_FRAME_MS {
                    Tier::Big
                } else if dt_ms > 2.0 * MOVIE_FRAME_MS {
                    Tier::Normal
                } else if dt_ms > MOVIE_FRAME_MS {
                    Tier::Small
                } else {
                    Tier::None
                }
            } else {
                Tier::None
            }
        } else {
            Tier::None
        };

        self.prev_durations.push_back(dt_ms);
        if self.prev_durations.len() > 3 {
            self.prev_durations.pop_front();
        }

        match tier {
            Tier::None => JankTick::default(),
            Tier::Small => {
                self.jank_duration_ms += dt_ms;
                JankTick { small_jank: 1, ..Default::default() }
            }
            Tier::Normal => {
                self.jank_duration_ms += dt_ms;
                JankTick { jank: 1, ..Default::default() }
            }
            Tier::Big => {
                self.jank_duration_ms += dt_ms;
                JankTick { big_jank: 1, ..Default::default() }
            }
        }
    }

    fn stutter_ratio(&self) -> f64 {
        if self.total_duration_ms > 0.0 {
            (self.jank_duration_ms / self.total_duration_ms).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Tier {
    None,
    Small,
    Normal,
    Big,
}

/// Result of one SurfaceView sampling pass: fps + this-tick jank counters
/// + the layer's *cumulative* stutter ratio.
#[derive(Debug, Default)]
struct SvTickResult {
    fps: f64,
    tick: JankTick,
    stutter: f64,
    /// Layer we picked (for logging only).
    #[allow(dead_code)]
    layer: Option<String>,
}

pub struct FpsSampler {
    serial: String,
    target_pkg: String,
}

impl FpsSampler {
    pub fn new(serial: impl Into<String>, target_pkg: impl Into<String>) -> Self {
        Self {
            serial: serial.into(),
            target_pkg: target_pkg.into(),
        }
    }
}

#[async_trait]
impl Sampler for FpsSampler {
    fn name(&self) -> &'static str {
        "android.fps"
    }

    fn target_hz(&self) -> f32 {
        1.0
    }

    async fn start(
        &mut self,
        ctx: SamplerCtx,
    ) -> Result<BoxStream<'static, Result<Sample, SamplerError>>, SamplerError> {
        let serial = self.serial.clone();
        let pkg = self.target_pkg.clone();
        if !adb::is_safe_pkg_name(&pkg) {
            return Err(SamplerError::Fatal(anyhow::anyhow!(
                "refusing unsafe package name: {pkg}"
            )));
        }
        let clock = ctx.clock.clone();
        let s = stream! {
            let mut ticker = interval(Duration::from_millis(PERIOD_MS));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            ticker.tick().await; // skip immediate
            let mut last: Option<(GfxStats, Instant)> = None;
            // Per-SurfaceView-layer state: max ts + PerfDog rolling window +
            // cumulative jank duration. Used to (a) filter --latency output
            // down to new frames (an idle game's frozen 128-frame buffer
            // would otherwise look like steady rendering), and (b) maintain
            // the 3-frame sliding window the PerfDog jank formula needs.
            let mut sv_state: HashMap<String, LayerState> = HashMap::new();
            let mut sv_last_tick: Option<Instant> = None;
            loop {
                ticker.tick().await;
                let now = Instant::now();
                let sv_dt = sv_last_tick
                    .map(|t| now.saturating_duration_since(t).as_secs_f64())
                    .unwrap_or(0.0);
                sv_last_tick = Some(now);

                // Run gfxinfo and SurfaceView FPS measurement in sequence.
                // gfxinfo is fast (~30ms); SurfaceView path may run 1-3
                // extra adb calls.
                let gfx = match query_gfxinfo(&serial, &pkg).await {
                    Ok(v) => v,
                    Err(e) => {
                        let retriable = e.is_retriable();
                        yield Err(e);
                        if !retriable { return; }
                        continue;
                    }
                };
                let sv = surfaceview_tick(&serial, &pkg, &mut sv_state, sv_dt)
                    .await
                    .ok()
                    .flatten();

                let Some(stats) = gfx else {
                    // App process not found. Emit 0 and keep going.
                    let ts = clock.now_us();
                    yield Ok(Sample {
                        ts_us: ts, device_ts_us: None,
                        kind: MetricKind::Fps, value: 0.0, labels: smallvec![],
                    });
                    continue;
                };

                let Some((prev, prev_t)) = last.as_ref() else {
                    tracing::info!(sampler = "android.fps", pkg = %pkg, total = stats.total_frames, "fps seeded");
                    last = Some((stats, now));
                    continue;
                };
                let dt = now.saturating_duration_since(*prev_t).as_secs_f64();
                if dt <= 0.0 { continue; }
                if stats.total_frames < prev.total_frames {
                    tracing::info!(sampler = "android.fps", pkg = %pkg, "gfxinfo counters reset; reseeding");
                    last = Some((stats, now));
                    continue;
                }

                let d_frames = stats.total_frames - prev.total_frames;
                let gfx_fps = d_frames as f64 / dt;

                // Headline FPS: prefer SurfaceView when it's higher (game /
                // video pipeline). gfxinfo wins for UI-heavy apps.
                let chosen_fps = match sv.as_ref().map(|s| s.fps) {
                    Some(sv_fps) if sv_fps > gfx_fps => sv_fps,
                    _ => gfx_fps,
                };

                let ts = clock.now_us();
                let mut emits: Vec<(MetricKind, f64)> = vec![
                    (MetricKind::Fps, chosen_fps),
                    (MetricKind::FrameTimeMs, stats.p50_ms),
                ];
                // Jank metrics only emit when a SurfaceView frame stream
                // was sampled this tick — PerfDog's classifier needs
                // per-frame durations, which gfxinfo doesn't expose.
                if let Some(sv) = sv.as_ref() {
                    emits.push((MetricKind::SmallJankCount, sv.tick.small_jank as f64));
                    emits.push((MetricKind::JankCount, sv.tick.jank as f64));
                    emits.push((MetricKind::BigJankCount, sv.tick.big_jank as f64));
                    emits.push((MetricKind::Stutter, sv.stutter));
                }
                for (kind, value) in emits {
                    yield Ok(Sample {
                        ts_us: ts, device_ts_us: None,
                        kind, value, labels: smallvec![],
                    });
                }
                tracing::info!(
                    sampler = "android.fps",
                    pkg = %pkg,
                    fps = format!("{:.1}", chosen_fps),
                    gfx_fps = format!("{:.1}", gfx_fps),
                    sv_fps = sv.as_ref().map(|s| format!("{:.1}", s.fps)).unwrap_or_else(|| "-".into()),
                    small = sv.as_ref().map(|s| s.tick.small_jank).unwrap_or(0),
                    jank = sv.as_ref().map(|s| s.tick.jank).unwrap_or(0),
                    big = sv.as_ref().map(|s| s.tick.big_jank).unwrap_or(0),
                    stutter = sv.as_ref().map(|s| format!("{:.3}", s.stutter)).unwrap_or_else(|| "-".into()),
                    p50_ms = stats.p50_ms,
                    p99_ms = stats.p99_ms,
                    "fps tick"
                );
                last = Some((stats, now));
            }
        };
        Ok(Box::pin(s))
    }
}

#[derive(Debug, Clone)]
struct GfxStats {
    total_frames: u64,
    p50_ms: f64,
    /// Currently unused; kept for an upcoming percentile chart.
    #[allow(dead_code)]
    p90_ms: f64,
    p99_ms: f64,
}


/// Sample SurfaceView frame timestamps for the target package and compute
/// (a) the layer's FPS this tick and (b) PerfDog jank classifications for
/// every newly-arrived frame.
///
/// Returns the result for the most-active layer this tick. State for that
/// layer (max-timestamp + 3-frame rolling window + cumulative jank
/// duration) lives in `state` so subsequent ticks can keep classifying
/// without losing the sliding window.
async fn surfaceview_tick(
    serial: &str,
    pkg: &str,
    state: &mut HashMap<String, LayerState>,
    dt: f64,
) -> Result<Option<SvTickResult>, SamplerError> {
    let raw_list = adb::shell(serial, "dumpsys SurfaceFlinger --list").await?;
    let layers: Vec<String> = raw_list
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .filter(|l| l.contains(pkg))
        .filter(|l| {
            let lower = l.to_ascii_lowercase();
            (lower.contains("surfaceview")
                || lower.contains("blast")
                || lower.contains("buffer queue"))
                && !lower.contains("backgroundforsurfaceview")
                && !lower.contains("bounds for -")
        })
        .map(|l| clean_layer_name(l))
        .filter(|s| !s.is_empty())
        .collect();

    if layers.is_empty() {
        return Ok(None);
    }

    let mut best: Option<SvTickResult> = None;
    for layer in &layers {
        // Layer name is wrapped in double quotes on the device shell; if it
        // contains any of these, the quoting breaks and we'd interpolate a
        // shell metachar (`$`, backtick, backslash, escaped quote).
        // SurfaceFlinger names in the wild are component-derived
        // (`pkg/Activity#N`, `SurfaceView ...`) and don't contain these,
        // but skip defensively rather than escape.
        if layer.chars().any(|c| matches!(c, '"' | '$' | '`' | '\\')) {
            tracing::warn!(layer, "android.fps: skipping layer with shell-unsafe chars");
            continue;
        }
        let cmd = format!("dumpsys SurfaceFlinger --latency \"{layer}\"");
        let raw = adb::shell(serial, &cmd).await?;
        let timestamps = parse_latency(&raw);
        if timestamps.is_empty() {
            continue;
        }

        // Sort and isolate frames newer than the last-seen high water mark
        // for this layer.
        let mut sorted = timestamps.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let curr_max = *sorted.last().unwrap();

        let entry = state.entry(layer.clone()).or_default();
        let prev_max = entry.max_ts;
        if prev_max == 0.0 {
            // First sighting: just seed the high water mark; no
            // classification yet because we don't know the previous frame.
            entry.max_ts = curr_max;
            continue;
        }

        // PerfDog jank classification needs the previous frame's timestamp
        // to compute duration. We use prev_max as the boundary marker;
        // the first new frame's duration is (first_new - prev_max).
        let mut prev_ts = prev_max;
        let mut tick_counts = JankTick::default();
        let mut new_count = 0u32;
        for &t in &sorted {
            if t <= prev_max {
                continue;
            }
            let dur_ms = (t - prev_ts) * 1000.0;
            // Sanity: ignore wraparound or absurd values (>5s gap likely
            // means buffer reset, not a real frame interval).
            if dur_ms > 0.0 && dur_ms < 5000.0 {
                let inc = entry.classify_frame(dur_ms);
                tick_counts.small_jank += inc.small_jank;
                tick_counts.jank += inc.jank;
                tick_counts.big_jank += inc.big_jank;
            }
            prev_ts = t;
            new_count += 1;
        }
        entry.max_ts = curr_max;

        if new_count == 0 {
            if best.is_none() {
                best = Some(SvTickResult {
                    fps: 0.0,
                    tick: JankTick::default(),
                    stutter: entry.stutter_ratio(),
                    layer: Some(layer.clone()),
                });
            }
            continue;
        }
        if dt <= 0.0 {
            continue;
        }
        let fps = new_count as f64 / dt;
        let stutter = entry.stutter_ratio();
        let candidate = SvTickResult {
            fps,
            tick: tick_counts,
            stutter,
            layer: Some(layer.clone()),
        };
        tracing::debug!(
            layer = %layer,
            new_frames = new_count,
            fps,
            small = candidate.tick.small_jank,
            jank = candidate.tick.jank,
            big = candidate.tick.big_jank,
            stutter,
            "surfaceview candidate"
        );
        if best.as_ref().map(|b| candidate.fps > b.fps).unwrap_or(true) {
            best = Some(candidate);
        }
    }
    Ok(best)
}

fn clean_layer_name(s: &str) -> String {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("RequestedLayerState{") {
        let mut layer = rest.to_string();
        for key in [
            "parentId=",
            "relativeParentId=",
            "mirrorId=",
            " z=",
            " layerStack=",
        ] {
            if let Some(idx) = layer.find(key) {
                layer.truncate(idx);
            }
        }
        if layer.ends_with('}') {
            layer.pop();
        }
        return layer.trim().to_string();
    }
    s.to_string()
}

fn parse_latency(text: &str) -> Vec<f64> {
    let mut out = Vec::new();
    for line in text.lines() {
        let nums: Vec<u64> = line
            .split_whitespace()
            .filter_map(|s| s.parse::<u64>().ok())
            .collect();
        if nums.len() < 3 {
            continue;
        }
        let mid = nums[1];
        if mid == 0 || mid == u64::MAX || mid > 1_000_000_000_000_000_000 {
            continue;
        }
        out.push(mid as f64 / 1e9);
    }
    if !out.is_empty() {
        out.pop();
    }
    out
}

/// Returns Ok(None) when the process isn't running ("No process found for: pkg").
async fn query_gfxinfo(serial: &str, pkg: &str) -> Result<Option<GfxStats>, SamplerError> {
    if !adb::is_safe_pkg_name(pkg) {
        tracing::warn!(pkg, "android.fps: unsafe package name, skipping gfxinfo");
        return Ok(None);
    }
    let cmd = format!("dumpsys gfxinfo {pkg}");
    let raw = adb::shell(serial, &cmd).await?;
    if raw.contains("No process found for:") {
        return Ok(None);
    }
    parse_gfxinfo(&raw)
        .map(Some)
        .ok_or_else(|| SamplerError::Fatal(anyhow::anyhow!("unparseable gfxinfo output")))
}

fn parse_gfxinfo(text: &str) -> Option<GfxStats> {
    // Multi-process apps emit one "** Graphics info for pid X **" block each.
    // We sum totals across them and take the latest percentile values
    // (gfxinfo percentiles are per-process; for multi-process there's no
    // perfect aggregation, so we use the first non-zero block).
    let mut total_frames: u64 = 0;
    let mut p50 = None;
    let mut p90 = None;
    let mut p99 = None;

    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Total frames rendered:") {
            if let Ok(n) = rest.trim().parse::<u64>() {
                total_frames += n;
            }
        } else if let Some(rest) = line.strip_prefix("50th percentile:") {
            p50 = parse_ms(rest).or(p50);
        } else if let Some(rest) = line.strip_prefix("90th percentile:") {
            p90 = parse_ms(rest).or(p90);
        } else if let Some(rest) = line.strip_prefix("99th percentile:") {
            p99 = parse_ms(rest).or(p99);
        }
    }

    if total_frames == 0 && p50.is_none() {
        return None;
    }
    Some(GfxStats {
        total_frames,
        p50_ms: p50.unwrap_or(0.0),
        p90_ms: p90.unwrap_or(0.0),
        p99_ms: p99.unwrap_or(0.0),
    })
}

/// Parse "5ms" or " 5ms" → Some(5.0).
fn parse_ms(s: &str) -> Option<f64> {
    let s = s.trim();
    let s = s.strip_suffix("ms").unwrap_or(s);
    s.trim().parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_GFXINFO: &str = "\
Applications Graphics Acceleration Info:
Uptime: 34303840 Realtime: 417063733

** Graphics info for pid 18338 [com.sec.android.app.sbrowser] **

Stats since: 34300799054778ns
Total frames rendered: 231
Janky frames: 9 (3.90%)
Janky frames (legacy): 9 (3.90%)
50th percentile: 5ms
90th percentile: 8ms
95th percentile: 9ms
99th percentile: 42ms
HISTOGRAM: 5ms=194 6ms=1 7ms=3 8ms=19 9ms=5 10ms=1 38ms=1 42ms=1
";

    #[test]
    fn parse_basic() {
        let s = parse_gfxinfo(SAMPLE_GFXINFO).unwrap();
        assert_eq!(s.total_frames, 231);
        assert_eq!(s.p50_ms, 5.0);
        assert_eq!(s.p90_ms, 8.0);
        assert_eq!(s.p99_ms, 42.0);
    }

    #[test]
    fn jank_tier_classification() {
        let mut s = LayerState::default();
        // Seed with three normal 16.67ms frames (60fps).
        for _ in 0..3 {
            let t = s.classify_frame(16.67);
            assert_eq!(t.small_jank, 0);
            assert_eq!(t.jank, 0);
            assert_eq!(t.big_jank, 0);
        }
        // 60ms frame: > 2×avg(16.67)=33.34 AND > 41.66 (1×movie). → SmallJank.
        let t = s.classify_frame(60.0);
        assert_eq!((t.small_jank, t.jank, t.big_jank), (1, 0, 0));

        // Now the window has [16.67, 16.67, 60.0], avg ≈ 31.11, 2× ≈ 62.22.
        // 100ms > 62.22 AND > 83.33 (2×movie) → Jank.
        let t = s.classify_frame(100.0);
        assert_eq!((t.small_jank, t.jank, t.big_jank), (0, 1, 0));

        // Window: [16.67, 60.0, 100.0], avg ≈ 58.89, 2× ≈ 117.78.
        // 130ms > 117.78 AND > 125 (3×movie) → BigJank.
        let t = s.classify_frame(130.0);
        assert_eq!((t.small_jank, t.jank, t.big_jank), (0, 0, 1));
        assert!(s.stutter_ratio() > 0.0 && s.stutter_ratio() <= 1.0);
    }

    #[test]
    fn no_process_handled_by_caller() {
        // We don't parse "No process found"; the caller checks for that
        // string before parsing.
        assert!(parse_gfxinfo("No process found for: com.foo").is_none());
    }
}
