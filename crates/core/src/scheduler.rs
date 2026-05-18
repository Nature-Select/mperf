//! Central scheduler. See docs/abstractions.md §6.

use futures::stream::{select_all, StreamExt};
use mperf_schema::{AppTarget, Clock, Sample, Sampler, SamplerCtx, SamplerError, SessionId};
use std::sync::Arc;
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinHandle;

const LIVE_BROADCAST_CAP: usize = 1024;

pub struct SchedulerHandle {
    session_id: SessionId,
    live_tx: broadcast::Sender<Sample>,
    task: Option<JoinHandle<()>>,
    cancel: tokio::sync::watch::Sender<bool>,
    /// Fires (exactly once) when the scheduler task exits — for any
    /// reason: user cancel, a sampler returning a non-retriable error,
    /// or all sampler streams running to completion. Lets the caller
    /// (session.rs) notice the death immediately and emit
    /// `EVENT_SESSION_ENDED` without waiting for the broadcast channel
    /// to close. (Broadcast doesn't close while the handle is alive,
    /// because the handle holds a `live_tx` clone for `subscribe()` —
    /// so before this Notify existed the only path that closed the
    /// channel was `stop()` taking the handle by value, which always
    /// happens *after* the user_stopping flag is set, suppressing
    /// EVENT_SESSION_ENDED entirely.)
    exit_notify: Arc<Notify>,
}

impl SchedulerHandle {
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Sample> {
        self.live_tx.subscribe()
    }

    /// Clone the exit notifier so an external task can `.notified().await`
    /// the scheduler's death without owning the handle.
    pub fn exit_notify(&self) -> Arc<Notify> {
        self.exit_notify.clone()
    }

    pub async fn stop(mut self) {
        let _ = self.cancel.send(true);
        if let Some(t) = self.task.take() {
            let _ = t.await;
        }
    }
}

impl Drop for SchedulerHandle {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
    }
}

pub struct Scheduler;

impl Scheduler {
    pub async fn start(
        session_id: SessionId,
        target: Option<AppTarget>,
        samplers: Vec<Box<dyn Sampler>>,
    ) -> Result<SchedulerHandle, SamplerError> {
        let clock = Clock::session_origin();
        let ctx = SamplerCtx {
            clock,
            session_id,
            target,
        };

        let mut streams = Vec::with_capacity(samplers.len());
        for mut s in samplers {
            let name = s.name();
            match s.start(ctx.clone()).await {
                Ok(stream) => {
                    tracing::info!(sampler = name, "sampler started");
                    streams.push(stream);
                }
                Err(e) => {
                    tracing::error!(sampler = name, error = %e, "sampler failed to start");
                    return Err(e);
                }
            }
        }

        let merged = select_all(streams);
        let (live_tx, _) = broadcast::channel(LIVE_BROADCAST_CAP);
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);

        let live_tx_clone = live_tx.clone();
        let exit_notify = Arc::new(Notify::new());
        let exit_notify_task = exit_notify.clone();
        let task = tokio::spawn(async move {
            tokio::pin!(merged);
            loop {
                tokio::select! {
                    biased;
                    _ = cancel_rx.changed() => {
                        if *cancel_rx.borrow() {
                            tracing::info!(session_id = session_id.0, "scheduler cancelled");
                            break;
                        }
                    }
                    maybe = merged.next() => {
                        match maybe {
                            Some(Ok(sample)) => {
                                let _ = live_tx_clone.send(sample);
                            }
                            Some(Err(e)) => {
                                tracing::warn!(error = %e, "sampler error");
                                if !e.is_retriable() {
                                    break;
                                }
                            }
                            None => {
                                tracing::info!("all samplers exhausted");
                                break;
                            }
                        }
                    }
                }
            }
            tracing::info!(session_id = session_id.0, "scheduler exited");
            // Drop our sender clone before notifying so anyone waking
            // up on the notify and immediately checking subscriber count
            // sees a consistent view. The handle still holds another
            // sender clone so the broadcast itself doesn't close yet;
            // that's intentional — `Session::stop` is what drops the
            // handle and finally closes the broadcast.
            drop(live_tx_clone);
            // `notify_one` (not `notify_waiters`) so that a sampler
            // erroring out before the watcher task has entered
            // `.notified().await` still triggers — `notify_one` stores
            // a permit consumed by the next `.notified().await`.
            // `notify_waiters` would silently drop the wake-up.
            exit_notify_task.notify_one();
        });

        Ok(SchedulerHandle {
            session_id,
            live_tx,
            task: Some(task),
            cancel: cancel_tx,
            exit_notify,
        })
    }
}
