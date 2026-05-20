//! Android-side collectors. Phase 0a: device list + CPU sampler via `adb`.

mod adb;
mod apps;
mod battery;
mod cpu;
mod devices;
mod fps;
mod gpu;
mod logcat;
mod memory;
mod startup;
mod temperature;

pub use apps::{launch_app, list_apps};
pub use startup::{measure_cold_start, measure_hot_start, StartupTiming};
pub use battery::BatterySampler;
pub use cpu::CpuSampler;
pub use devices::{device_info, list_devices};
pub use fps::FpsSampler;
pub use gpu::GpuSampler;
pub use logcat::{pidof, LogcatStream};
pub use memory::MemSampler;
pub use temperature::TempSampler;
