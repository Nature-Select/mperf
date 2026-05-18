//! Device-log streaming. Independent of the recording session — users
//! can open / close the log terminal at any time, with or without a
//! session running.
//!
//! One stream at a time. Starting a second stream stops the first.
//! Each line is pushed to the frontend via the Tauri event
//! `mperf://log-line` (payload = `LogLine`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use mperf_android::{pidof, LogcatStream};
use mperf_ios::{fetch_active_pids, resolve_bundle_to_pids, OsTraceStream};
use mperf_schema::{LogLine, Platform};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::task::JoinHandle;

pub const EVENT_LOG_LINE: &str = "mperf://log-line";
pub const EVENT_LOG_STATUS: &str = "mperf://log-status";

/// Status pushed to the frontend so the terminal can show
/// "等待 app 启动" / "已 attach PID xxx" instead of silently sitting
/// empty when the target app isn't running. `ts_ms` lets the frontend
/// fence stale events from torn-down streams the same way log lines
/// are fenced.
#[derive(Serialize, Clone)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LogStreamStatus {
    /// Waiting for the target app to launch (Android only — iOS path
    /// resolves PIDs up-front and bails if none found).
    Waiting { ts_ms: i64 },
    /// Attached and forwarding lines. `pid` is informational; for iOS
    /// it's the first PID in the set (extensions get the same set).
    Attached { ts_ms: i64, pid: i32 },
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn emit_status(app: &AppHandle, status: &LogStreamStatus) {
    if let Err(e) = app.emit(EVENT_LOG_STATUS, status) {
        tracing::warn!(error = %e, "emit log-status failed");
    }
}

/// Held by `AppState`. Cleared on stop and on natural end (device
/// disconnected, child process exited, etc.).
pub struct LogStream {
    /// Mutated by `stop_log_stream` to tell the running task to
    /// shut down between lines. Tokio's `kill_on_drop` would handle
    /// child cleanup either way, but the explicit flag stops the
    /// emit loop immediately rather than waiting for the next line.
    cancel: Arc<AtomicBool>,
    /// Keep the task handle so we can `await` it on stop (gives the
    /// child a chance to drain before the next stream starts).
    task: Option<JoinHandle<()>>,
}

impl LogStream {
    pub async fn stop(mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(t) = self.task.take() {
            // The polite path: task notices `cancel` between log lines
            // and returns. Works fine on Android (logcat is a noisy
            // stream of frames, cancel gets checked promptly), but on
            // iOS the task can be blocked deep inside
            // `OsTraceRelayReceiver::next().await` waiting for the
            // next device frame — and if the device just went quiet
            // (foreground app suspended, screen lock, etc.), that
            // await won't return for a long time. Without a timeout
            // the next `start_log_stream` would wait on this same
            // `t.await` and the log toggle would feel "dead" to the
            // user. So: give cancel ~500ms to take effect, otherwise
            // abort the task. Abort drops the receiver (and any
            // logcat child via `kill_on_drop`), which closes the
            // underlying socket and frees the lockdown service slot
            // for the next start.
            let abort = t.abort_handle();
            if tokio::time::timeout(std::time::Duration::from_millis(500), t)
                .await
                .is_err()
            {
                abort.abort();
            }
        }
    }
}

/// Start a log stream for `device_id` on `platform`. If `target_pkg`
/// is Some, Android filters by PID (best-effort: `pidof <pkg>` at
/// stream start, no auto-refresh if the app restarts mid-stream).
/// iOS resolves bundle id → CFBundleExecutable inside `run_ios` and
/// filters by process name match on each syslog line.
///
/// Replaces any currently-running stream.
pub async fn start_log_stream(
    state: &crate::session::AppState,
    app: &AppHandle,
    device_id: String,
    platform: Platform,
    target_pkg: Option<String>,
) -> Result<(), String> {
    // **Hold the lock for the entire start operation** — including
    // the awaited `prior.stop()`. The earlier version released the
    // lock around the stop().await, which let two concurrent start
    // commands (React 19 StrictMode dev mode emits start → cleanup
    // stop → start in quick succession) both lock-take(None), both
    // spawn a fresh task, both lock-store; the second store
    // overwrites the first, leaving one task orphaned (no handle in
    // state). On iOS that means **two concurrent syslog_relay
    // sockets** to the same lockdownd. lockdown serializes one and
    // the other reads silently nothing, which manifested as "iOS
    // terminal shows zero log lines forever". Holding the lock makes
    // concurrent starts queue strictly through it; only one task
    // can exist at a time. The lock-hold also means stop_log_stream
    // can't run during a start's prior.stop().await, but that's
    // fine — it just waits ~ms.
    let mut guard = state.log_stream.lock().await;
    if let Some(prior) = guard.take() {
        prior.stop().await;
    }

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_task = cancel.clone();
    let cancel_for_log = cancel.clone();
    let app_task = app.clone();

    let task = tokio::spawn(async move {
        let result = match platform {
            Platform::Android => {
                run_android(app_task, cancel_task, device_id, target_pkg).await
            }
            Platform::Ios => run_ios(app_task, cancel_task, device_id, target_pkg).await,
        };
        // Distinguish the three exit paths in the log so a "why is
        // the stream gone" investigation doesn't have to guess.
        let cancelled = cancel_for_log.load(Ordering::Acquire);
        if let Err(e) = result {
            tracing::warn!(error = ?e, cancelled, "log stream task exited with error");
        } else if cancelled {
            tracing::info!("log stream task exited via cancel (caller requested stop / replace)");
        } else {
            tracing::info!("log stream task exited via EOF (device disconnect or service closed)");
        }
    });

    *guard = Some(LogStream {
        cancel,
        task: Some(task),
    });
    Ok(())
}

pub async fn stop_log_stream(state: &crate::session::AppState) {
    let prior = {
        let mut guard = state.log_stream.lock().await;
        guard.take()
    };
    if let Some(s) = prior {
        s.stop().await;
    }
}

/// Android log stream with auto-attach + restart-recovery state machine.
///
/// State flow:
///   1. Wait — `pidof <pkg>` returns None: poll every 1s, emit Waiting once.
///   2. Attach — pidof returns Some: emit Attached{pid}, open logcat
///      with `--pid <pid>`, poll pidof every 5s to detect PID drift.
///   3. PID drift / app died — drop logcat stream, go back to step 1.
///
/// Without this, the old code did `pidof` once at start; if it returned
/// None we silently fell back to all-PIDs logcat (flooding the terminal
/// with unrelated system logs) and on PID change we just went silent.
async fn run_android(
    app: AppHandle,
    cancel: Arc<AtomicBool>,
    serial: String,
    target_pkg: Option<String>,
) -> anyhow::Result<()> {
    // No-pkg path retained for completeness (frontend currently always
    // passes a pkg). Streams everything device-wide with no state
    // machine — pre-existing behavior.
    let pkg = match target_pkg {
        Some(p) if !p.is_empty() => p,
        _ => {
            tracing::info!(serial, "starting android logcat (no pkg filter — all PIDs)");
            let mut stream = LogcatStream::start(&serial, None).await?;
            loop {
                if cancel.load(Ordering::Acquire) {
                    return Ok(());
                }
                match stream.next_line().await {
                    Ok(Some(line)) => emit_line(&app, &line),
                    Ok(None) => {
                        tracing::info!("logcat EOF — device likely disconnected");
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    };

    'reattach: loop {
        if cancel.load(Ordering::Acquire) {
            return Ok(());
        }

        // Step 1: wait for the target PID. Emits Waiting on the first
        // miss so the UI surfaces a "等待 <pkg> 启动" badge instead of
        // looking dead.
        let pid = match wait_for_android_pid(&serial, &pkg, &cancel, &app).await? {
            Some(p) => p,
            None => return Ok(()), // cancelled
        };

        // Step 2: attach.
        emit_status(
            &app,
            &LogStreamStatus::Attached {
                ts_ms: now_ms(),
                pid,
            },
        );
        tracing::info!(serial, pkg = %pkg, pid, "android logcat attached");

        let mut stream = LogcatStream::start(&serial, Some(pid)).await?;
        let mut pid_watch = tokio::time::interval(Duration::from_secs(5));
        pid_watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // First interval tick fires immediately; consume it so the
        // initial 5s window matches the user's expectation.
        pid_watch.tick().await;

        loop {
            if cancel.load(Ordering::Acquire) {
                return Ok(());
            }
            tokio::select! {
                // `tokio::io::Lines::next_line` is cancellation-safe —
                // dropping the future on `pid_watch.tick()` win does
                // not lose buffered bytes; they stay in the BufReader.
                line = stream.next_line() => match line {
                    Ok(Some(l)) => emit_line(&app, &l),
                    Ok(None) => {
                        tracing::info!(pid, "logcat EOF — device likely disconnected");
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                },
                _ = pid_watch.tick() => {
                    match pidof(&serial, &pkg).await {
                        Ok(Some(new_pid)) if new_pid == pid => {} // still alive
                        Ok(Some(new_pid)) => {
                            tracing::info!(old_pid = pid, new_pid, pkg = %pkg, "android pid changed — reattaching");
                            // Drop the stream now (kill_on_drop on the
                            // adb child) so the next pidof doesn't race
                            // against a still-open logcat on the old PID.
                            drop(stream);
                            emit_status(&app, &LogStreamStatus::Waiting { ts_ms: now_ms() });
                            continue 'reattach;
                        }
                        Ok(None) => {
                            tracing::info!(pid, pkg = %pkg, "android target vanished — waiting for restart");
                            drop(stream);
                            emit_status(&app, &LogStreamStatus::Waiting { ts_ms: now_ms() });
                            continue 'reattach;
                        }
                        Err(e) => {
                            // pidof transient failure (device offline blip etc.)
                            // — keep streaming on the existing PID and retry
                            // on the next tick.
                            tracing::warn!(error = ?e, pkg = %pkg, "pidof poll failed");
                        }
                    }
                }
            }
        }
    }
}

/// Poll `pidof <pkg>` every 1s until it returns Some(pid) or `cancel`
/// flips. Emits `Waiting` exactly once (on the first None) so the UI
/// shows the "等待 app 启动" badge as soon as we know the app isn't up.
/// Returns Ok(None) if cancelled.
async fn wait_for_android_pid(
    serial: &str,
    pkg: &str,
    cancel: &AtomicBool,
    app: &AppHandle,
) -> anyhow::Result<Option<i32>> {
    let mut announced = false;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Ok(None);
        }
        match pidof(serial, pkg).await {
            Ok(Some(p)) => return Ok(Some(p)),
            Ok(None) => {
                if !announced {
                    emit_status(app, &LogStreamStatus::Waiting { ts_ms: now_ms() });
                    tracing::info!(pkg, "android target not running — waiting");
                    announced = true;
                }
            }
            Err(e) => {
                // Treat as transient — most likely device-offline blip.
                // Keep polling; if the device is really gone the next
                // adb invocation will surface the same error and we'll
                // log it again.
                tracing::warn!(error = ?e, pkg, "pidof failed during wait");
                if !announced {
                    emit_status(app, &LogStreamStatus::Waiting { ts_ms: now_ms() });
                    announced = true;
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// iOS log stream with auto-attach + restart-recovery state machine.
///
/// Mirrors run_android's structure but with different primitives:
///   - PID resolution: DTX `resolve_bundle_to_pids` (handles Flutter
///     "Runner" name collisions correctly via install-path prefix
///     matching).
///   - Cheap liveness check: `fetch_active_pids` over lockdown os_trace
///     PidList (no DTX setup cost — ~50-100ms per call).
///
/// State flow:
///   1. Wait — `resolve_bundle_to_pids` returns empty set: poll every
///      2s (DTX, heavier than Android's `pidof` but unavoidable), emit
///      Waiting once.
///   2. Attach — non-empty PID set: emit Attached{first_pid}, open
///      os_trace stream (always server-side unfiltered — iOS 17+ broke
///      the server-side PID filter), client-side filter by `target_pids`
///      `HashSet` membership.
///   3. Poll cheap PidList every 3s. If any target PID disappeared,
///      re-resolve via DTX (handles app restart — new "Runner" PID's
///      install path still matches the bundle, so it gets picked up).
///   4. New PID set: update filter in place; the os_trace stream itself
///      keeps running. If new set is empty, emit Waiting and continue
///      polling until non-empty.
async fn run_ios(
    app: AppHandle,
    cancel: Arc<AtomicBool>,
    udid: String,
    target_pkg: Option<String>,
) -> anyhow::Result<()> {
    // The frontend hard-gates the log toggle on `target_pkg != None`,
    // so we shouldn't hit the None branch under normal use; it's
    // retained as an explicit reject so an upstream code-path change
    // can't accidentally re-open the device-wide-logs path that
    // overwhelmed the UI before.
    let pkg = match target_pkg.as_deref() {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => {
            anyhow::bail!(
                "iOS log stream requires a target bundle id (device-wide logs are too noisy to be useful)"
            );
        }
    };

    // Step 1: wait until the bundle has at least one running PID.
    let mut target_pids =
        match wait_for_ios_pids(&udid, &pkg, &cancel, &app).await? {
            Some(set) => set,
            None => return Ok(()), // cancelled
        };

    emit_status(
        &app,
        &LogStreamStatus::Attached {
            ts_ms: now_ms(),
            pid: *target_pids.iter().next().expect("non-empty by wait_for_ios_pids contract"),
        },
    );
    tracing::info!(
        udid,
        pkg = %pkg,
        ?target_pids,
        "iOS log stream attached"
    );

    // Step 2: open os_trace stream + spin up PID watcher.
    tracing::info!(udid, "starting iOS os_trace_relay (server-side unfiltered)");
    let mut stream = OsTraceStream::start(&udid).await?;
    let mut pid_watch = tokio::time::interval(Duration::from_secs(3));
    pid_watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    pid_watch.tick().await; // discard immediate first tick

    let mut total = 0u64;
    let mut matched = 0u64;
    let mut last_report = std::time::Instant::now();
    let report_interval = std::time::Duration::from_secs(30);

    loop {
        if cancel.load(Ordering::Acquire) {
            tracing::info!(total, matched, "iOS log loop cancelled");
            return Ok(());
        }
        tokio::select! {
            line = stream.next_line() => match line {
                Ok(Some(line)) => {
                    total += 1;
                    if total == 1 {
                        tracing::info!(
                            first_pid = ?line.pid,
                            first_image = ?line.process,
                            ?target_pids,
                            "first os_trace line received"
                        );
                    }
                    if last_report.elapsed() >= report_interval {
                        tracing::info!(total, matched, ?target_pids, "iOS os_trace throughput");
                        last_report = std::time::Instant::now();
                    }
                    match line.pid {
                        Some(p) if target_pids.contains(&p) => {}
                        _ => continue,
                    }
                    matched += 1;
                    emit_line(&app, &line);
                }
                Ok(None) => {
                    tracing::info!(total, matched, "os_trace_relay EOF — device likely disconnected");
                    return Ok(());
                }
                Err(e) => return Err(e),
            },
            _ = pid_watch.tick() => {
                // Cheap alive-probe: lockdown PidList round-trip.
                let alive = match fetch_active_pids(&udid).await {
                    Ok(a) => a,
                    Err(e) => {
                        // Transient (device-offline blip etc.) — keep
                        // streaming on the existing filter, retry next
                        // tick.
                        tracing::warn!(error = ?e, "iOS fetch_active_pids failed");
                        continue;
                    }
                };
                if target_pids.iter().all(|p| alive.contains(p)) {
                    continue; // all targets still alive
                }
                tracing::info!(
                    pkg = %pkg,
                    old_pids = ?target_pids,
                    "iOS target PID(s) disappeared — re-resolving via DTX"
                );
                // App restart / extension reload — re-resolve. The
                // os_trace stream itself stays open; we just swap the
                // client-side filter set.
                let resolved = resolve_bundle_to_pids(&udid, &pkg)
                    .await
                    .with_context(|| format!("resolve_bundle_to_pids({pkg})"))?;
                let new_set: std::collections::HashSet<i32> =
                    resolved.pids.iter().map(|p| *p as i32).collect();
                if new_set.is_empty() {
                    tracing::info!(
                        pkg = %pkg,
                        "iOS bundle has no running PIDs — entering Waiting"
                    );
                    emit_status(&app, &LogStreamStatus::Waiting { ts_ms: now_ms() });
                    // Wait until the app comes back. The os_trace
                    // stream is still open and delivering lines, but
                    // they all fail the (empty) PID filter — no leak
                    // to the UI. Polling continues via DTX since
                    // PidList alone can't tell us which new PID
                    // belongs to our bundle (Flutter-Runner ambiguity).
                    target_pids = match wait_for_ios_pids(&udid, &pkg, &cancel, &app).await? {
                        Some(set) => set,
                        None => return Ok(()),
                    };
                } else {
                    target_pids = new_set;
                }
                let first_pid = *target_pids.iter().next().expect("non-empty");
                emit_status(
                    &app,
                    &LogStreamStatus::Attached {
                        ts_ms: now_ms(),
                        pid: first_pid,
                    },
                );
                tracing::info!(
                    pkg = %pkg,
                    ?target_pids,
                    "iOS log stream re-attached"
                );
            }
        }
    }
}

/// Poll `resolve_bundle_to_pids` every 2s until it returns a non-empty
/// PID set or `cancel` flips. Emits `Waiting` exactly once on first
/// empty result so the UI surfaces a "等待 <pkg> 启动" badge.
///
/// Returns Ok(None) if cancelled. Returns Err only on hard failures
/// from `resolve_bundle_to_pids` that aren't "bundle is installed but
/// not running" (e.g. device disconnect, bundle id not installed).
async fn wait_for_ios_pids(
    udid: &str,
    pkg: &str,
    cancel: &AtomicBool,
    app: &AppHandle,
) -> anyhow::Result<Option<std::collections::HashSet<i32>>> {
    let mut announced = false;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Ok(None);
        }
        match resolve_bundle_to_pids(udid, pkg).await {
            Ok(res) if !res.pids.is_empty() => {
                let set: std::collections::HashSet<i32> =
                    res.pids.iter().map(|p| *p as i32).collect();
                tracing::info!(
                    pkg,
                    install_path = %res.install_path,
                    ?res.pids,
                    ?res.matched_paths,
                    "iOS bundle resolved to running PIDs"
                );
                return Ok(Some(set));
            }
            Ok(_) => {
                if !announced {
                    emit_status(app, &LogStreamStatus::Waiting { ts_ms: now_ms() });
                    tracing::info!(pkg, "iOS target not running — waiting");
                    announced = true;
                }
            }
            Err(e) => {
                // Could be transient (USB blip) or fatal (bundle not
                // installed). Log and keep polling — if the user
                // installs the app the next poll picks it up; if the
                // device is truly gone the device-list watchdog ends
                // the session via a separate path.
                tracing::warn!(error = ?e, pkg, "iOS resolve_bundle_to_pids failed during wait");
                if !announced {
                    emit_status(app, &LogStreamStatus::Waiting { ts_ms: now_ms() });
                    announced = true;
                }
            }
        }
        // 2s — DTX resolve is ~1s itself, so effective rate is one
        // probe every ~3s. Don't go lower without optimising the DTX
        // setup (which would need RemoteServerClient reuse, not in
        // scope here).
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn emit_line(app: &AppHandle, line: &LogLine) {
    if let Err(e) = app.emit(EVENT_LOG_LINE, line) {
        tracing::warn!(error = %e, "emit log-line failed");
    }
}
