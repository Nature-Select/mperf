//! Persistent coreprofile drainer + broadcast fan-out.
//!
//! Why this exists: iOS 26 holds the global `kperf` lock for several
//! seconds (sometimes longer) after a coreprofile TCP connection
//! closes. Opening a fresh session per cold-start measurement hits
//! `_lockKPerf: could not lock kperf` randomly. PerfDog's solution
//! (and ours, now) is to open ONE coreprofile session per device
//! when mperf attaches to it, keep the TCP stream draining
//! continuously in a background task, and broadcast kdebug events to
//! whichever measurement is interested at the moment.
//!
//! The supervisor owns a `CoreProfileSessionRaw` inside a spawned
//! task. The task loops on `next_payload()`:
//!  - `KdEvents(...)` → broadcast to subscribers
//!  - `StatusNotice` → log warn; if it's `_lockKPerf` during initial
//!     handshake the supervisor task exits and the next `acquire`
//!     call rebuilds. After healthy events have flowed, notices
//!     during steady state are advisory and ignored.
//!  - `Stackshot` / `EmptyAck` / `Unknown` → drop silently.
//!  - Stream error → exit task; next acquire rebuilds.
//!
//! Subscribers use `broadcast::Receiver<KdEvent>`. `broadcast` is
//! preferred over `mpsc` because:
//!  - multiple consumers might want events in the future (live phase
//!    chart, jank detector, etc.)
//!  - "lagged" receivers (slow consumer) drop OLD events first, not
//!    new ones — exactly what we want for a real-time perf tool.
//! Channel capacity is sized generously (16k events) — at the
//! observed ~30k events / launch and 300ms drain window, this
//! should not lag under normal conditions.

use crate::core_profile_session_raw::{CoreProfileSessionRaw, KdEvent, RawPayload};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;

/// Capacity of the broadcast channel. Sized to absorb ~3 launches
/// worth of events without lagging the slowest subscriber. Each KdEvent
/// is ~64 bytes so capacity 16384 ≈ 1MB resident.
const EVENT_CHANNEL_CAPACITY: usize = 16 * 1024;

/// Held in the per-UDID pool. The supervisor owns the background
/// drain task and the broadcast Sender. `subscribe()` hands out fresh
/// Receivers that get every event from now on.
pub struct KdebugSupervisor {
    /// `None` means the bg task exited (stream died, or initial
    /// handshake failed). The pool detects this and rebuilds.
    state: Arc<Mutex<SupervisorState>>,
    /// Broadcast channel; survives as long as the supervisor does.
    /// Subscribers via `subscribe()`.
    tx: broadcast::Sender<KdEvent>,
}

struct SupervisorState {
    task: Option<JoinHandle<()>>,
    /// Last error reported by the drain task, if any. Useful for
    /// surfacing to the user when the supervisor dies.
    last_error: Option<String>,
}

impl KdebugSupervisor {
    /// Start the supervisor: open a fresh CoreProfileSessionRaw,
    /// spawn the drain task. Returns once the task is spawned —
    /// healthy operation depends on the device pushing events soon
    /// after, which is the supervisor's job to monitor.
    pub async fn start(udid: &str) -> Result<Arc<Self>> {
        let session = CoreProfileSessionRaw::start(udid)
            .await
            .map_err(|e| anyhow!("CoreProfileSessionRaw::start: {e}"))?;
        let (tx, _initial_rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let state = Arc::new(Mutex::new(SupervisorState {
            task: None,
            last_error: None,
        }));
        let supervisor = Arc::new(Self {
            state: state.clone(),
            tx: tx.clone(),
        });
        let task = tokio::spawn(run_drain_task(session, tx, state.clone()));
        {
            let mut guard = state.lock().await;
            guard.task = Some(task);
        }
        Ok(supervisor)
    }

    /// Subscribe to the kdebug event stream. Every event the drain
    /// task receives from now on goes to this Receiver until the
    /// Receiver is dropped or lags past `EVENT_CHANNEL_CAPACITY`.
    pub fn subscribe(&self) -> broadcast::Receiver<KdEvent> {
        self.tx.subscribe()
    }

    /// True if the background drain task is still running. Pool uses
    /// this at acquire-time to decide whether to rebuild.
    pub async fn is_alive(&self) -> bool {
        let guard = self.state.lock().await;
        guard.task.as_ref().map(|h| !h.is_finished()).unwrap_or(false)
    }

    /// Last error message from the drain task, if it died.
    pub async fn last_error(&self) -> Option<String> {
        let guard = self.state.lock().await;
        guard.last_error.clone()
    }
}

/// The drain task body — reads payloads from `session` forever,
/// broadcasts kdebug events. Exits on stream error (logged into
/// `state.last_error`).
async fn run_drain_task(
    mut session: CoreProfileSessionRaw,
    tx: broadcast::Sender<KdEvent>,
    state: Arc<Mutex<SupervisorState>>,
) {
    let mut healthy_events_seen = false;
    let mut payloads_seen: u64 = 0;
    tracing::info!("kdebug supervisor: drain task starting");
    loop {
        let payload = match session.next_payload().await {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("drain stream error: {e}");
                tracing::warn!(payloads_seen, error = %e, "kdebug supervisor: drain task exiting");
                let mut guard = state.lock().await;
                guard.last_error = Some(msg);
                return;
            }
        };
        payloads_seen += 1;
        match payload {
            RawPayload::KdEvents(events) => {
                if events.is_empty() {
                    continue;
                }
                if !healthy_events_seen {
                    tracing::info!(
                        first_batch = events.len(),
                        payloads_seen,
                        "kdebug supervisor: first healthy events arrived"
                    );
                    healthy_events_seen = true;
                }
                // Broadcast every event. `send` errors if there are
                // zero subscribers — that's normal (idle between
                // measurements), so we ignore.
                for ev in events {
                    let _ = tx.send(ev);
                }
            }
            RawPayload::StatusNotice { strings } => {
                // Surface the human-readable notice in the log. If
                // we never saw events and this is the kperf-lock
                // notice, exit and let the pool rebuild later.
                let detail = strings
                    .iter()
                    .find(|s| s.contains(' ') && s.len() > 20)
                    .cloned()
                    .unwrap_or_else(|| format!("notice: {strings:?}"));
                if !healthy_events_seen {
                    tracing::warn!(
                        %detail,
                        "kdebug supervisor: status notice before any events — session unhealthy, exiting"
                    );
                    let mut guard = state.lock().await;
                    guard.last_error = Some(detail);
                    return;
                }
                tracing::info!(%detail, "kdebug supervisor: mid-session status notice (ignoring)");
            }
            RawPayload::Stackshot { bytes_len } => {
                tracing::debug!(bytes_len, "kdebug supervisor: stackshot (drop)");
            }
            RawPayload::EmptyAck { bytes_len } => {
                tracing::trace!(bytes_len, "kdebug supervisor: empty ack (drop)");
            }
            RawPayload::Unknown { first8, bytes_len } => {
                tracing::warn!(
                    bytes_len,
                    ?first8,
                    "kdebug supervisor: unknown payload (drop)"
                );
            }
        }
    }
}

// ---------------------------------------------------------------
// Per-UDID supervisor pool
// ---------------------------------------------------------------

type PoolMap = HashMap<String, Arc<KdebugSupervisor>>;
static POOL: OnceLock<Mutex<PoolMap>> = OnceLock::new();

fn pool() -> &'static Mutex<PoolMap> {
    POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Get-or-create a live supervisor for `udid`. If the cached
/// supervisor's drain task has died (stream EOF, kperf-lock notice
/// during handshake, etc.), it's removed and a fresh one is built
/// inline.
///
/// Bounded retry on rebuild: if the first build fails with a
/// kperf-lock notice (or any other startup failure), wait briefly
/// and try again — total budget 2 attempts. Beyond that, surface
/// the last error to the caller.
pub async fn acquire(udid: &str) -> Result<Arc<KdebugSupervisor>> {
    // Fast path: live supervisor in the pool.
    {
        let map = pool().lock().await;
        if let Some(s) = map.get(udid) {
            if s.is_alive().await {
                return Ok(s.clone());
            }
        }
    }
    // Build (or rebuild). Drop the dead entry first so a parallel
    // acquire doesn't keep getting it.
    {
        let mut map = pool().lock().await;
        map.remove(udid);
    }
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 1..=2u32 {
        match KdebugSupervisor::start(udid).await {
            Ok(s) => {
                // Brief settle window: let the drain task either see
                // a healthy event (sets healthy_events_seen) or fail
                // with kperf-lock notice. If it fails fast, we'll see
                // is_alive() == false on the recheck.
                tokio::time::sleep(Duration::from_millis(400)).await;
                if !s.is_alive().await {
                    let err = s.last_error().await.unwrap_or_else(|| {
                        "supervisor task exited without an error message".into()
                    });
                    tracing::warn!(attempt, %err, "kdebug supervisor: died during settle");
                    last_err = Some(anyhow!("{err}"));
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_millis(800)).await;
                    }
                    continue;
                }
                let mut map = pool().lock().await;
                map.insert(udid.to_string(), s.clone());
                return Ok(s);
            }
            Err(e) => {
                tracing::warn!(attempt, error = %e, "kdebug supervisor: start failed");
                last_err = Some(e);
                if attempt < 2 {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("kdebug supervisor: build failed after retries")))
}
