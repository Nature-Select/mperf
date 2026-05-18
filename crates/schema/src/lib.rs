//! Shared abstractions used by every other crate.
//!
//! This crate is the **leaf** of the dependency graph: nothing in this
//! workspace depends on anything else. Platform crates and the orchestrator
//! both depend on it. See `docs/abstractions.md` for the contract.

pub mod clock;
pub mod device;
pub mod error;
pub mod log;
pub mod metric;
pub mod sampler;
pub mod session;

pub use clock::Clock;
pub use device::{Device, Platform, Transport};
pub use error::SamplerError;
pub use log::{LogLevel, LogLine};
pub use metric::{LabelKey, Labels, MetricKind, Sample};
pub use sampler::{Sampler, SamplerCtx};
pub use session::{AppTarget, DeviceRef, SessionId};
