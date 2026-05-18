//! Sample, MetricKind, Labels. See docs/abstractions.md §2.

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sample {
    pub ts_us: i64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub device_ts_us: Option<i64>,
    pub kind: MetricKind,
    pub value: f64,
    #[serde(default, skip_serializing_if = "SmallVec::is_empty")]
    pub labels: Labels,
}

pub type Labels = SmallVec<[(LabelKey, String); 2]>;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelKey {
    Pid,
    Tid,
    CoreIdx,
    Iface,
    PowerSupply,
    Layer,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    CpuTotalPct,
    CpuAppPct,
    CpuCorePct,
    CpuFreqMhz,
    CpuTempC,

    MemSystemUsedBytes,
    MemAppPssBytes,

    Fps,
    FrameTimeMs,
    /// PerfDog Small Jank tier: frame_time > 2×avg(prev 3) AND > 41.66ms
    /// (one 24fps movie-frame). Count of such frames in the last window.
    SmallJankCount,
    /// PerfDog Jank tier: frame_time > 2×avg(prev 3) AND > 83.33ms (two
    /// movie-frames). Count of such frames in the last window.
    JankCount,
    /// PerfDog BigJank tier: frame_time > 2×avg(prev 3) AND > 125ms (three
    /// movie-frames). Count of such frames in the last window.
    BigJankCount,
    /// Cumulative ratio (0-1) of time spent in jank frames this session.
    Stutter,
    GpuTilerPct,
    GpuRendererPct,
    GpuDevicePct,

    NetUpBytes,
    NetDownBytes,

    BatteryLevelPct,
    BatteryTempC,
    BatteryVoltageMv,
    BatteryCurrentMa,

    ThreadCpuPct,
}

impl MetricKind {
    pub fn short_label(self) -> &'static str {
        use MetricKind::*;
        match self {
            CpuTotalPct => "CPU",
            CpuAppPct => "App CPU",
            CpuCorePct => "CPU core",
            CpuFreqMhz => "CPU freq",
            CpuTempC => "CPU temp",
            MemSystemUsedBytes => "Mem",
            MemAppPssBytes => "App mem",
            Fps => "FPS",
            FrameTimeMs => "Frame",
            SmallJankCount => "Small jank",
            JankCount => "Jank",
            BigJankCount => "Big jank",
            Stutter => "Stutter",
            GpuTilerPct => "GPU tiler",
            GpuRendererPct => "GPU rndr",
            GpuDevicePct => "GPU dev",
            NetUpBytes => "Net up",
            NetDownBytes => "Net down",
            BatteryLevelPct => "Battery",
            BatteryTempC => "Bat temp",
            BatteryVoltageMv => "Bat V",
            BatteryCurrentMa => "Bat I",
            ThreadCpuPct => "Thread CPU",
        }
    }
}
