//! mperf orchestration: list devices, run sessions.
//!
//! The trait definitions live in `mperf-schema`; this crate composes
//! platform crates and owns the scheduler.

pub mod scheduler;

pub use mperf_schema::{
    AppTarget, Clock, Device, DeviceRef, LabelKey, Labels, MetricKind, Platform, Sample, Sampler,
    SamplerCtx, SamplerError, SessionId,
};
pub use scheduler::{Scheduler, SchedulerHandle};

use anyhow::Result;

pub async fn list_devices() -> Result<Vec<Device>> {
    let (android, ios) = tokio::join!(
        mperf_android::list_devices(),
        mperf_ios::list_devices()
    );

    let mut out = Vec::new();
    match android {
        Ok(mut v) => out.append(&mut v),
        Err(e) => tracing::warn!("android list_devices failed: {e:#}"),
    }
    match ios {
        Ok(mut v) => out.append(&mut v),
        Err(e) => tracing::warn!("ios list_devices failed: {e:#}"),
    }
    Ok(out)
}
