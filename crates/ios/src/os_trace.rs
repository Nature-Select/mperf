//! iOS device unified logging via lockdownd's
//! `com.apple.os_trace_relay` service. Same source `Console.app` on
//! macOS reads when an iOS device is connected — captures **all** log
//! output including os_log / `OSLog` / Swift `Logger` / os_signpost,
//! plus legacy NSLog / `print()` and kernel messages.
//!
//! Replaces the older `syslog_relay` (kept dead in `syslog.rs` for
//! reference): on iOS 12+, virtually every native app and daemon
//! moved from asl/syslog to unified logging, so syslog_relay returned
//! near-nothing in practice.
//!
//! **Filtering**: the protocol's `StartActivity { Pid }` field is
//! advertised as a server-side PID filter, but on iOS 17+ the server
//! accepts the request and then never pushes any frames. We open
//! unfiltered (`Pid = -1`) and PID-match client-side in
//! `log_stream.rs::run_ios`. The set of allowed PIDs is computed by
//! `pid_resolver::resolve_bundle_to_pids` via the DTX channels
//! (deviceinfo + applicationListing) — the lockdown `PidList` path
//! we used earlier only returns short ProcessName strings, which is
//! ambiguous across Flutter apps (all default to `Runner`).

use crate::connect;
use anyhow::{Context, Result};
use idevice::{
    services::os_trace_relay::{
        LogLevel as OsLogLevel, OsTraceLog, OsTraceRelayClient, OsTraceRelayReceiver,
    },
    IdeviceError, IdeviceService,
};
use mperf_schema::{LogLevel, LogLine};
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

/// Lightweight one-shot "which PIDs are currently alive" probe via the
/// lockdownd `com.apple.os_trace_relay` PidList request. Cheaper than
/// re-running DTX `resolve_bundle_to_pids` (no CoreDeviceProxy / RSD /
/// dtservicehub setup) — used by the log terminal state machine to
/// detect app restart / death every few seconds, only re-resolving via
/// DTX when the cheap probe shows a target PID is gone.
///
/// Each call opens a fresh lockdown connection because os_trace_relay
/// is single-shot per connection on iOS 17+ (CLAUDE.md note).
pub async fn fetch_active_pids(udid: &str) -> Result<HashSet<i32>> {
    let provider = connect::provider_for(udid)
        .await
        .context("provider_for")?;
    let mut client = OsTraceRelayClient::connect(&*provider)
        .await
        .context("OsTraceRelayClient::connect")?;
    let pids = client
        .get_pid_list()
        .await
        .context("os_trace get_pid_list")?;
    Ok(pids.into_iter().map(|p| p as i32).collect())
}

pub struct OsTraceStream {
    receiver: OsTraceRelayReceiver,
}

impl OsTraceStream {
    /// Open an os_trace stream against the device. Always opens
    /// **server-side unfiltered** (`StartActivity { Pid = -1 }`); the
    /// caller is expected to PID-match client-side using PIDs that
    /// came from `pid_resolver::resolve_bundle_to_pids`. We open
    /// unfiltered because on iOS 17+ the server-side filter is
    /// broken — `StartActivity { Pid: N }` succeeds without error
    /// but the device never actually pushes frames, while opening
    /// the stream with `-1` and doing the match in our pump loop is
    /// reliable.
    pub async fn start(udid: &str) -> Result<Self> {
        let provider = connect::provider_for(udid)
            .await
            .context("provider_for")?;
        let trace_client = OsTraceRelayClient::connect(&*provider)
            .await
            .context("OsTraceRelayClient::connect")?;
        let receiver = trace_client
            .start_trace(None)
            .await
            .context("start_trace")?;
        Ok(Self { receiver })
    }

    /// Pull the next parsed log line. Returns `Ok(None)` on EOF
    /// (device disconnected, service closed).
    pub async fn next_line(&mut self) -> Result<Option<LogLine>> {
        match self.receiver.next().await {
            Ok(l) => Ok(Some(convert(l))),
            // The receiver signals EOF via UnexpectedResponse with a
            // message about the service ending; treat that as EOF
            // rather than an error.
            Err(IdeviceError::UnexpectedResponse(_)) => Ok(None),
            Err(e) => Err(anyhow::Error::new(e).context("os_trace_relay.next")),
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn convert(l: OsTraceLog) -> LogLine {
    // os_log levels differ from logcat / syslog:
    //   Notice = "default", everyday informational
    //   Info   = explicit Info (less noisy than Notice)
    //   Debug  = Debug
    //   Error  = Error (recoverable)
    //   Fault  = Fault (programmer error / assertion)
    // Map Notice + Info both → Info on our shared scale; Fault → Fatal.
    let level = match l.level {
        OsLogLevel::Notice | OsLogLevel::Info => LogLevel::Info,
        OsLogLevel::Debug => LogLevel::Debug,
        OsLogLevel::Error => LogLevel::Error,
        OsLogLevel::Fault => LogLevel::Fatal,
    };
    let (tag, subcategory) = match l.label {
        Some(sl) => {
            let subcat = if sl.category.is_empty() {
                None
            } else {
                Some(sl.category)
            };
            (sl.subsystem, subcat)
        }
        None => (String::new(), None),
    };
    LogLine {
        ts_ms: now_ms(),
        level,
        tag,
        message: l.message,
        process: if l.image_name.is_empty() {
            None
        } else {
            Some(l.image_name)
        },
        pid: Some(l.pid as i32),
        subcategory,
    }
}
