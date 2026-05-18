//! Typed sampler errors. See docs/abstractions.md §5.

#[derive(thiserror::Error, Debug)]
pub enum SamplerError {
    #[error("device disconnected: {0}")]
    DeviceDisconnected(String),

    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("app not running: {0}")]
    AppNotRunning(String),

    #[error("transient io: {0}")]
    TransientIo(String),

    #[error(transparent)]
    Fatal(#[from] anyhow::Error),
}

impl SamplerError {
    pub fn is_retriable(&self) -> bool {
        matches!(self, SamplerError::TransientIo(_))
    }
}
