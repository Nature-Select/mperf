//! Per-recording session lifecycle: holds the scheduler + writer task +
//! UI pump task, plus the global `AppState` they live inside. All Tauri
//! command handlers in `commands.rs` go through the helpers here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mperf_android::{
    BatterySampler as AndroidBatterySampler, CpuSampler as AndroidCpuSampler,
    FpsSampler as AndroidFpsSampler, GpuSampler as AndroidGpuSampler,
    MemSampler as AndroidMemSampler, TempSampler as AndroidTempSampler,
};
use mperf_core::{Sample, Sampler, Scheduler, SchedulerHandle, SessionId};
use mperf_ios::{CpuSampler as IosCpuSampler, GraphicsSampler as IosGraphicsSampler};
use mperf_schema::Platform;
use mperf_storage::{SessionMeta, Storage};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// Event names pushed to the frontend.
pub const EVENT_SAMPLE: &str = "mperf://sample";
pub const EVENT_SESSION_ENDED: &str = "mperf://session-ended";

/// Writer batching parameters. ~5 samples/sec total (1 cpu_total + 4-8 cores)
/// at 1Hz today; the 200ms / 256-sample threshold keeps SQLite write traffic
/// at most ~5 transactions/sec.
const WRITE_BATCH_FLUSH_MS: u64 = 200;
const WRITE_BATCH_MAX: usize = 256;

/// Per-app state. Storage is cheap to clone (Arc inside `tokio-rusqlite`).
pub struct AppState {
    pub storage: Storage,
    pub session: Mutex<Option<Session>>,
    /// Independent of `session`: the log terminal can be opened with
    /// or without a recording in progress. Holds at most one stream;
    /// starting a new one stops the previous.
    pub log_stream: Mutex<Option<crate::log_stream::LogStream>>,
}

/// A live recording session: scheduler + DB-writer task + DB session id.
pub struct Session {
    pub db_id: i64,
    /// Wall-clock ms when the session was created. Markers compute their
    /// `ts_us` (offset from session start) as `(now_ms - wall_start_ms) * 1000`.
    pub wall_start_ms: i64,
    scheduler: SchedulerHandle,
    writer_task: Option<JoinHandle<()>>,
    /// Set true right before we tear down the scheduler from a
    /// user-initiated stop (or session replacement). The UI pump task
    /// reads this on broadcast-Closed to decide whether to emit the
    /// "ended automatically" event — without it the frontend toasts
    /// every clean Stop click.
    user_stopping: Arc<AtomicBool>,
}

impl Session {
    /// Stop the scheduler, drain the writer, finalize the DB row.
    pub async fn stop(mut self, storage: &Storage) {
        self.user_stopping.store(true, Ordering::Release);
        self.scheduler.stop().await;
        if let Some(t) = self.writer_task.take() {
            let _ = t.await;
        }
        let end_ms = now_ms();
        if let Err(e) = storage.finish_session(self.db_id, end_ms).await {
            tracing::warn!(error = %e, session_id = self.db_id, "finish_session failed");
        }
    }
}

#[derive(Serialize, Clone)]
struct SessionEndedPayload {
    session_id: i64,
    reason: String,
}

/// Start a new recording session: build samplers, create DB row, spin up
/// the scheduler + writer + UI pump tasks, and atomically replace any
/// prior session in `AppState`. Returns the newly-assigned DB id.
pub async fn start_recording(
    state: &AppState,
    app: &AppHandle,
    device_id: String,
    platform: Platform,
    device_model: Option<String>,
    target_pkg: String,
    selected_metrics: Option<Vec<String>>,
    sampling_intervals: Option<std::collections::HashMap<String, u64>>,
) -> Result<i64, String> {
    tracing::info!(
        device_id = %device_id,
        ?platform,
        device_model = ?device_model,
        target_pkg = %target_pkg,
        "start_session"
    );

    // Auto-launch — PerfDog parity. Best-effort on both platforms:
    // failure logs a warn and falls through to the scheduler so the
    // user can still record by launching the app themselves (e.g. MDM
    // devices blocking `monkey`, or iOS bundles in a state that DTX
    // processcontrol refuses).
    //
    // iOS launch adds ~1-2s to start time because it has to build a
    // fresh CoreDeviceProxy + RSD + dtservicehub channel just for the
    // one launch call. Unavoidable — that's the protocol cost.
    match platform {
        Platform::Android => match mperf_android::launch_app(&device_id, &target_pkg).await {
            Ok(()) => tracing::info!(target_pkg = %target_pkg, "android: launched via monkey"),
            Err(e) => tracing::warn!(error = %e, target_pkg = %target_pkg, "android: launch failed (continuing)"),
        },
        Platform::Ios => match mperf_ios::launch_app(&device_id, &target_pkg).await {
            Ok(pid) => tracing::info!(target_pkg = %target_pkg, pid, "ios: launched via processcontrol"),
            Err(e) => tracing::warn!(error = %e, target_pkg = %target_pkg, "ios: launch failed (continuing)"),
        },
    }

    let intervals_for_samplers = sampling_intervals.clone().unwrap_or_default();
    let samplers = build_samplers(&device_id, platform, target_pkg.clone(), &intervals_for_samplers);
    let platform_str = match platform {
        Platform::Android => "android",
        Platform::Ios => "ios",
    };

    let session_wall_start_ms = now_ms();
    let db_id = state
        .storage
        .create_session(SessionMeta {
            wall_start_ms: session_wall_start_ms,
            device_id: device_id.clone(),
            device_platform: platform_str.into(),
            device_model,
            app_bundle_id: Some(target_pkg.clone()),
            meta_json: None,
            selected_metrics,
            sampling_intervals,
        })
        .await
        .map_err(|e| e.to_string())?;

    let handle = Scheduler::start(SessionId(db_id as u64), None, samplers)
        .await
        .map_err(|e| e.to_string())?;

    // Subscribe BEFORE first sample to avoid missing the early ticks.
    let rx_writer = handle.subscribe();
    let rx_ui = handle.subscribe();
    let exit_notify = handle.exit_notify();

    let writer_task = spawn_writer_task(state.storage.clone(), db_id, rx_writer);

    let user_stopping = Arc::new(AtomicBool::new(false));
    spawn_ui_pump_task(app.clone(), db_id, rx_ui, user_stopping.clone());
    // Watch for the scheduler exiting on its own (sampler returned a
    // non-retriable error, or all streams ran to completion). Without
    // this task the frontend has no way to learn the session died until
    // it polls `list_devices` and notices the device missing — adding a
    // 1–3s delay before the "session ended automatically" toast.
    // We can't rely on the broadcast channel closing here: the handle
    // holds a `live_tx` clone for `subscribe()`, so the channel only
    // closes when the handle itself drops, which only happens inside
    // `Session::stop` (i.e. AFTER `user_stopping` is set, suppressing
    // EVENT_SESSION_ENDED in the UI pump). The exit_notify path is the
    // one signal that fires on natural scheduler exit AND respects the
    // user_stopping suppression.
    spawn_exit_watcher_task(app.clone(), db_id, exit_notify, user_stopping.clone());

    // Replace any prior session. Doing the swap under the lock and
    // stopping the displaced session off-thread avoids blocking start
    // on the previous session's drain.
    let prior = {
        let mut guard = state.session.lock().await;
        let prior = guard.take();
        *guard = Some(Session {
            db_id,
            wall_start_ms: session_wall_start_ms,
            scheduler: handle,
            writer_task: Some(writer_task),
            user_stopping,
        });
        prior
    };
    if let Some(old) = prior {
        let storage = state.storage.clone();
        tokio::spawn(async move { old.stop(&storage).await });
    }

    Ok(db_id)
}

fn build_samplers(
    device_id: &str,
    platform: Platform,
    target_pkg: String,
    intervals: &std::collections::HashMap<String, u64>,
) -> Vec<Box<dyn Sampler>> {
    // Resolve the effective interval for a sampler that serves N
    // chart-cards. When multiple cards share a sampler (e.g. iOS
    // sysmontap drives cpu_usage / cpu_core / memory at once), the
    // sampler must run at the fastest cadence any of those cards
    // requested — over-sampling is harmless, under-sampling loses
    // data. Cards the user didn't override fall through to `default`.
    let pick = |card_ids: &[&str], default: u64| -> u64 {
        card_ids
            .iter()
            .filter_map(|id| intervals.get(*id).copied())
            .min()
            .unwrap_or(default)
    };
    match platform {
        Platform::Android => vec![
            // CPU / FPS / memory are all scoped to the user-picked package
            // (PerfDog-style explicit selection; no foreground auto-detect).
            // Android CpuSampler emits Total + App + per-core from one
            // /proc/stat tick — cpu_usage and cpu_core share its cadence.
            Box::new(AndroidCpuSampler::new(
                device_id,
                target_pkg.clone(),
                pick(&["cpu_usage", "cpu_core"], 1000),
            )),
            Box::new(AndroidFpsSampler::new(
                device_id,
                target_pkg.clone(),
                pick(&["frame"], 1000),
            )),
            Box::new(AndroidMemSampler::new(
                device_id,
                target_pkg.clone(),
                pick(&["memory"], 1000),
            )),
            // CPU temp from thermal_zone (when accessible — many OEMs lock
            // /sys for shell). Battery temp + level from dumpsys (universal).
            Box::new(AndroidTempSampler::new(
                device_id,
                pick(&["temperature"], 2000),
            )),
            // BatterySampler has no dedicated chart-card today (battery
            // temp piggy-backs on the Temperature card); follow its
            // hardcoded default until a real Battery card lands.
            Box::new(AndroidBatterySampler::new(device_id, 2000)),
            // GPU is best-effort: Adreno KGSL or Mali devfreq. Auto-stops
            // emitting after 3 empty polls if /sys access is locked.
            Box::new(AndroidGpuSampler::new(
                device_id,
                pick(&["gpu"], 1000),
            )),
        ],
        Platform::Ios => {
            // iOS sampler ctors take u32 (sysmontap and graphics.opengl
            // both encode interval as a 32-bit DTX integer). `pick`
            // returns u64 because that's the on-wire type. A plain
            // `as u32` would silently truncate if a future caller sent
            // a value > u32::MAX; saturating cast keeps the cap honest
            // even though the discrete catalog options (500-10000ms)
            // can't get anywhere near the boundary today.
            let to_u32 = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
            vec![
                // iOS sysmontap is one channel producing cpu_total /
                // cpu_app / per-core / app-mem / system-mem. All three
                // chart-cards backed by it share its cadence — we pick
                // the min so each card's view rate is honoured.
                Box::new(IosCpuSampler::new(
                    device_id,
                    target_pkg.clone(),
                    to_u32(pick(&["cpu_usage", "cpu_core", "memory"], 1000)),
                )),
                // graphics.opengl channel produces the Tiler/Renderer/
                // Device GPU triplet plus CoreAnimation FPS — frame and
                // gpu cards share its cadence.
                Box::new(IosGraphicsSampler::new(
                    device_id,
                    to_u32(pick(&["gpu", "frame"], 1000)),
                )),
                // No iOS battery sampler: lockdown queries return empty while
                // our CoreDeviceProxy DTX tunnel is held (iOS 17+ behavior).
                // The proper fix is to use Instruments DTX power services on
                // the same RemoteServerClient — see docs/architecture.md and
                // crates/ios/src/battery.rs (kept for future reference).
            ]
        }
    }
}

fn spawn_writer_task(
    storage: Storage,
    db_id: i64,
    mut rx: tokio::sync::broadcast::Receiver<Sample>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut batch: Vec<Sample> = Vec::with_capacity(WRITE_BATCH_MAX);
        let mut flush = tokio::time::interval(Duration::from_millis(WRITE_BATCH_FLUSH_MS));
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // First tick fires immediately; consume it.
        flush.tick().await;
        loop {
            tokio::select! {
                r = rx.recv() => match r {
                    Ok(s) => {
                        batch.push(s);
                        if batch.len() >= WRITE_BATCH_MAX {
                            drain_and_write(&storage, db_id, &mut batch).await;
                        }
                    }
                    Err(RecvError::Closed) => break,
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(dropped = n, "writer lagged");
                    }
                },
                _ = flush.tick() => {
                    if !batch.is_empty() {
                        drain_and_write(&storage, db_id, &mut batch).await;
                    }
                }
            }
        }
        // Final drain after scheduler exit.
        if !batch.is_empty() {
            drain_and_write(&storage, db_id, &mut batch).await;
        }
        // Scheduler died (device disconnect, fatal sampler error, etc.)
        // without a user-initiated stop. Finalize the DB row here so the
        // History panel doesn't keep showing the session as in-progress
        // until the next app restart's orphan-cleanup pass.
        if let Err(e) = storage.finish_session(db_id, now_ms()).await {
            tracing::warn!(
                session_id = db_id,
                error = %e,
                "auto-finish on scheduler exit failed"
            );
        } else {
            tracing::info!(session_id = db_id, "session auto-finished on scheduler exit");
        }
        tracing::info!(session_id = db_id, "writer task exited");
    })
}

/// Fires `EVENT_SESSION_ENDED` the moment the scheduler task ends on its
/// own (not from a user-initiated stop). Pairs with `spawn_ui_pump_task`,
/// which only emits the same event on broadcast close — a path that
/// never triggers for natural scheduler death because the handle holds
/// a `live_tx` clone (see comments in scheduler.rs / start_recording).
fn spawn_exit_watcher_task(
    app: AppHandle,
    db_id: i64,
    exit_notify: Arc<tokio::sync::Notify>,
    user_stopping: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        tracing::info!(session_id = db_id, "exit watcher armed");
        exit_notify.notified().await;
        let user_stopped = user_stopping.load(Ordering::Acquire);
        tracing::info!(
            session_id = db_id,
            user_stopped,
            "exit watcher fired — scheduler exit_notify received"
        );
        // User-initiated stop sets the flag before tearing the scheduler
        // down; we'd race with that on every Stop click. The pump task
        // uses the same guard for the broadcast-close path, so the user
        // sees at most one "ended automatically" toast per session.
        if user_stopped {
            return;
        }

        tracing::info!(
            session_id = db_id,
            "scheduler exited on its own — emitting EVENT_SESSION_ENDED"
        );
        if let Err(e) = app.emit(
            EVENT_SESSION_ENDED,
            SessionEndedPayload {
                session_id: db_id,
                reason: "sampler error or device disconnect".into(),
            },
        ) {
            tracing::warn!(error = %e, "failed to emit EVENT_SESSION_ENDED");
        }

        // Backend cleanup — the missing half of the natural-exit story.
        // EVENT_SESSION_ENDED makes the frontend forget the session, but
        // the backend `Session` object is still sitting in `AppState`
        // holding a `SchedulerHandle` (and therefore a `live_tx` clone),
        // which means the broadcast channel never closes → the writer
        // task hangs on `rx.recv().await` forever → `finish_session`
        // never runs → the DB row keeps `wall_end_ms = NULL` → the
        // History tab shows the row as "in progress" forever.
        //
        // Before the exit_notify path existed, the frontend's
        // device-list watchdog called `stop_session` ~1-3s later and
        // accidentally papered over this. Now that the toast comes
        // from EVENT_SESSION_ENDED directly, the watchdog never fires
        // and the cleanup never happens. Trigger it explicitly here.
        //
        // Race conditions handled by the `db_id` equality check:
        //   - User clicked Start on a new session before we got here:
        //     the new session has a different db_id → we leave it
        //     alone, the prior session was already torn down by
        //     start_recording's off-thread `old.stop()` spawn.
        //   - User clicked Stop concurrently: stop_session may have
        //     already taken the session out of the mutex; we see
        //     None → no-op.
        let state = app.state::<AppState>();
        let session = {
            let mut guard = state.session.lock().await;
            match guard.as_ref() {
                Some(s) if s.db_id == db_id => guard.take(),
                _ => None,
            }
        };
        if let Some(s) = session {
            tracing::info!(
                session_id = db_id,
                "exit watcher tearing down session backend-side"
            );
            // `Session::stop` will:
            //   1. set user_stopping=true (idempotent — already set here
            //      conceptually, but the field is on the Session, not
            //      our local Arc clone)
            //   2. drop the SchedulerHandle → broadcast closes
            //   3. await the writer task — which now sees `Closed` and
            //      calls `finish_session` (idempotent thanks to the
            //      `WHERE wall_end_ms IS NULL` guard)
            //   4. call `finish_session` itself (no-op for the same
            //      reason — already finalized by the writer)
            s.stop(&state.storage).await;
            tracing::info!(session_id = db_id, "exit watcher cleanup complete");
        }
    });
}

fn spawn_ui_pump_task(
    app: AppHandle,
    db_id: i64,
    mut rx: tokio::sync::broadcast::Receiver<Sample>,
    user_stopping: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(sample) => {
                    if let Err(e) = app.emit(EVENT_SAMPLE, &sample) {
                        tracing::warn!(error = %e, "emit sample failed");
                        break;
                    }
                }
                Err(RecvError::Closed) => {
                    // Only surface the "ended automatically" event when
                    // the scheduler died on its own (USB unplug, fatal
                    // sampler error). User-initiated Stop already drives
                    // the UI; the toast would be misleading.
                    if !user_stopping.load(Ordering::Acquire) {
                        let _ = app.emit(
                            EVENT_SESSION_ENDED,
                            SessionEndedPayload {
                                session_id: db_id,
                                reason: "scheduler exited".into(),
                            },
                        );
                    }
                    break;
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(dropped = n, "frontend lagged; dropping samples");
                }
            }
        }
    });
}

async fn drain_and_write(storage: &Storage, session_id: i64, batch: &mut Vec<Sample>) {
    let drained: Vec<Sample> = batch.drain(..).collect();
    if let Err(e) = storage.insert_sample_batch(session_id, drained).await {
        tracing::warn!(error = %e, session_id, "batch insert failed");
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
