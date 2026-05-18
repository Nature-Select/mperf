//! The Sampler trait and execution context. See docs/abstractions.md §1.

use crate::clock::Clock;
use crate::error::SamplerError;
use crate::metric::Sample;
use crate::session::{AppTarget, SessionId};
use async_trait::async_trait;
use futures_core::stream::BoxStream;

#[derive(Clone)]
pub struct SamplerCtx {
    pub clock: Clock,
    pub session_id: SessionId,
    pub target: Option<AppTarget>,
}

#[async_trait]
pub trait Sampler: Send {
    fn name(&self) -> &'static str;

    fn target_hz(&self) -> f32 {
        1.0
    }

    async fn start(
        &mut self,
        ctx: SamplerCtx,
    ) -> Result<BoxStream<'static, Result<Sample, SamplerError>>, SamplerError>;
}
