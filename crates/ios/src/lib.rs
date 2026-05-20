//! iOS-side collectors using the pure-Rust `idevice` crate.
//!
//! Phase 0c spike: device enumeration + CPU sampler via `sysmontap`.
//! No Python, no privileged tunnel daemon — `idevice` ships a user-space
//! TCP stack that talks DTX over a software tunnel.

mod apps;
// Battery sampler is intentionally not registered in the desktop
// `start_session` handler: lockdown queries return an empty dict while
// our cpu sampler holds CoreDeviceProxy on iOS 17+, so it can never
// produce data. Kept here so the eventual DTX-power-channel rewrite
// has a home and the schema/key-discovery work isn't lost.
#[allow(dead_code)]
mod battery;
mod connect;
mod core_profile_session_raw;
mod cpu;
mod devices;
mod graphics;
mod graphics_raw;
mod launch;
mod startup;
mod os_trace;
mod pid_resolver;
// `syslog_relay` proved too sparse on modern iOS — virtually every
// native app and daemon uses os_log / unified logging now, so the old
// asl/syslog channel returned almost nothing. Kept as dead code in
// case it's useful for kernel-only diagnostics later, and so the
// commit history is recoverable. The active log path is os_trace.rs.
#[allow(dead_code)]
mod syslog;
mod sysmontap_raw;

pub use apps::list_apps;
#[allow(unused_imports)]
pub use battery::BatterySampler;
pub use cpu::{resolve_bundle_to_exec, CpuSampler};
pub use devices::{device_info, list_devices};
pub use graphics::GraphicsSampler;
pub use launch::launch_app;
pub use startup::{measure_cold_start, measure_hot_start, StartupTiming};
pub use os_trace::{fetch_active_pids, OsTraceStream};
pub use pid_resolver::{resolve_bundle_to_pids, BundleResolution};
#[allow(unused_imports)]
pub use syslog::SyslogStream;

/// Re-exports for `cargo run --example` harnesses that exercise low-
/// level DTX paths against a real device. Not part of the public API
/// for desktop / consumers — `#[doc(hidden)]` keeps it out of docs.
#[doc(hidden)]
pub mod testing {
    pub use crate::connect::provider_for;
}
