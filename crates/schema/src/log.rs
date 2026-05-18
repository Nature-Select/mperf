//! Device-log line abstraction. Used by the in-app log terminal (see
//! `LogTerminal` in the desktop frontend).
//!
//! Deliberately decoupled from `Sample` / `MetricKind` — logs aren't
//! time-series numeric data, they're free-text bursts, often at
//! 100×/s during heavy activity. They're not stored in SQLite either:
//! the terminal is a transient viewer (ring buffer in the frontend),
//! not a recording. If a future feature needs to persist logs along
//! with a session that's a separate design.

use serde::{Deserialize, Serialize};

/// Standard log severity. Both Android (`V/D/I/W/E/F`) and iOS
/// (syslog priority strings or os_log level enum) map cleanly into
/// these. `Unknown` is the catch-all for unparseable / future levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Verbose,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
    Unknown,
}

/// One log line for the terminal. `ts_ms` is host wall-clock at the
/// moment the line was received (not parsed from the device-side
/// timestamp string — that varies in format and we only use it for
/// display). The terminal sorts by arrival order, not ts_ms.
///
/// Both platforms now fill `process`, `pid`, and `tag` — though with
/// platform-specific semantics summarized below. The frontend renders
/// one unified column layout regardless of platform.
///
/// |              | Android (logcat `-v threadtime`) | iOS (os_trace_relay)        |
/// |--------------|----------------------------------|-----------------------------|
/// | `pid`        | from logcat column                | from os_trace packet        |
/// | `process`    | None (logcat doesn't surface)     | `image_name` (e.g. "Runner")|
/// | `tag`        | logcat tag (free-form)            | `subsystem` (e.g.           |
/// |              | (e.g. "ActivityManager")          | "com.apple.WebKit.Loading") |
/// | `subcategory`| None                              | os_log `category`           |
/// | `message`    | message body                      | message body                |
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    /// Host wall-clock milliseconds when this line was received.
    pub ts_ms: i64,
    pub level: LogLevel,
    /// **Source label** — Android logcat tag, or iOS os_log
    /// subsystem. Both are categorization strings; treating them as a
    /// single field lets the frontend show one source column.
    pub tag: String,
    /// Free-text message body, trimmed of trailing newlines.
    pub message: String,
    /// Originating process name. iOS = `image_name` (e.g. "Runner",
    /// "kernel"). Android = None — logcat's `-v threadtime` only
    /// gives PID, not process name. The terminal filters Android
    /// by PID server-side (`--pid`), so the missing field doesn't
    /// hurt UX.
    pub process: Option<String>,
    /// Originating PID. Filled on both platforms.
    pub pid: Option<i32>,
    /// iOS os_log `category` (a finer-grained label nested inside
    /// the subsystem — e.g. subsystem="com.apple.WebKit" + category=
    /// "Loading"). Always None on Android.
    pub subcategory: Option<String>,
}
