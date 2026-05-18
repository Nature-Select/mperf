//! `adb logcat` streaming. Spawns a child process per stream session;
//! the consumer pulls lines off via `.next_line().await`. Dropping the
//! `LogcatStream` kills the child (Tokio's `kill_on_drop(true)`).
//!
//! We use the `-v threadtime` format because it's stable, unambiguous,
//! and adb-version-agnostic:
//!
//! ```text
//! MM-DD HH:MM:SS.mmm  PID  TID L tag: message
//! 05-16 13:45:23.123  1234  5678 I ActivityManager: starting...
//! ```
//!
//! Lines that don't parse (header banners, multi-line dumps continuing
//! a prior message, etc.) are emitted with `level=Unknown`, `tag=""`,
//! and the raw text as the message — so the user still sees them in
//! the terminal but they don't poison filtering.

use anyhow::{Context, Result};
use mperf_schema::{LogLevel, LogLine};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};
use tracing::warn;

fn adb_binary() -> String {
    std::env::var("MPERF_ADB_PATH").unwrap_or_else(|_| "adb".to_string())
}

pub struct LogcatStream {
    /// Held for the lifetime of the stream so `kill_on_drop` does its
    /// job; we don't poll it ourselves.
    _child: Child,
    lines: Lines<BufReader<ChildStdout>>,
}

impl LogcatStream {
    /// Start an `adb logcat -v threadtime -T 1` stream for `serial`.
    /// `pid_filter` adds `--pid <pid>` if Some, restricting the stream
    /// to that PID. Resolve PID with `pidof <pkg>` ahead of time and
    /// pass it in; if the app restarts mid-stream the PID changes and
    /// the stream will go silent — caller should restart the stream
    /// in that case (the desktop UI surfaces this as "Reload").
    pub async fn start(serial: &str, pid_filter: Option<i32>) -> Result<Self> {
        let mut cmd = Command::new(adb_binary());
        cmd.args([
            "-s",
            serial,
            "logcat",
            // -T 1 → start from the most recent existing line; without
            // this logcat dumps the entire ring buffer (~256k lines)
            // on connect, which would flood the terminal with stale
            // history.
            "-T",
            "1",
            "-v",
            "threadtime",
        ]);
        if let Some(pid) = pid_filter {
            cmd.args(["--pid", &pid.to_string()]);
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());
        // Without `kill_on_drop`, dropping LogcatStream just releases our
        // pipe handle — the adb child keeps running until the OS reaps
        // it, potentially leaking child processes across recordings.
        cmd.kill_on_drop(true);
        let mut child = cmd.spawn().context("spawn adb logcat")?;
        let stdout = child
            .stdout
            .take()
            .context("logcat child has no stdout")?;
        let lines = BufReader::new(stdout).lines();
        Ok(Self {
            _child: child,
            lines,
        })
    }

    /// Pull the next parsed log line. Returns `Ok(None)` only when
    /// the child closes its stdout (logcat exited — usually because
    /// the device disconnected). Unparseable lines are returned as
    /// `LogLine { level: Unknown, ... raw text in message }` rather
    /// than skipped.
    pub async fn next_line(&mut self) -> Result<Option<LogLine>> {
        let ts_ms = now_ms();
        match self
            .lines
            .next_line()
            .await
            .context("reading logcat stdout")?
        {
            Some(raw) => {
                let parsed = parse_threadtime(&raw, ts_ms).unwrap_or_else(|| LogLine {
                    ts_ms,
                    level: LogLevel::Unknown,
                    tag: String::new(),
                    message: raw.clone(),
                    process: None,
                    pid: None,
                    subcategory: None,
                });
                Ok(Some(parsed))
            }
            None => Ok(None),
        }
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Parse one `-v threadtime` line.
///
/// Layout (whitespace runs are 1 or more spaces, columns are not
/// fixed-width across all Android versions — be liberal):
///
/// ```text
/// <date> <time> <pid> <tid> <L> <tag>: <message>
/// 05-16 13:45:23.123  1234  5678 I ActivityManager: starting ...
/// ```
///
/// Where `<L>` is one of `V D I W E F` and `<tag>` may contain spaces
/// (but not a colon followed by a space — that's the message
/// separator).
fn parse_threadtime(line: &str, ts_ms: i64) -> Option<LogLine> {
    // Header banners look like `--------- beginning of main`; skip
    // those (they'd just be Unknown noise, not log content).
    if line.starts_with("---") {
        return None;
    }

    // Five whitespace-separated tokens: date, time, pid, tid, level.
    // Then the remainder is `tag: message`. `splitn` on `char::is_whitespace`
    // doesn't work cleanly here because runs of whitespace yield empty
    // splits that consume the split budget — easiest to do a manual
    // scan that skips runs of whitespace.
    let bytes = line.as_bytes();
    let mut idx = 0;
    let mut tokens: [&str; 5] = [""; 5];
    for slot in tokens.iter_mut() {
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        let start = idx;
        while idx < bytes.len() && !bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if start == idx {
            return None;
        }
        *slot = &line[start..idx];
    }
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    let rest = &line[idx..];

    let pid: i32 = tokens[2].parse().ok()?;
    let _tid: u32 = tokens[3].parse().ok()?;
    let level = match tokens[4] {
        "V" => LogLevel::Verbose,
        "D" => LogLevel::Debug,
        "I" => LogLevel::Info,
        "W" => LogLevel::Warn,
        "E" => LogLevel::Error,
        "F" => LogLevel::Fatal,
        _ => LogLevel::Unknown,
    };
    let (tag, message) = match rest.split_once(": ") {
        Some((t, m)) => (t.trim().to_string(), m.to_string()),
        // No "tag: message" structure — treat the whole tail as the
        // message and leave tag empty.
        None => (String::new(), rest.to_string()),
    };
    Some(LogLine {
        ts_ms,
        level,
        tag,
        message,
        process: None,
        pid: Some(pid),
        subcategory: None,
    })
}

/// Best-effort `pidof <pkg>` to resolve a target package to a PID for
/// logcat's `--pid` filter. Returns None if the app isn't running or
/// the device's toybox lacks `pidof` (rare on Android 6+).
pub async fn pidof(serial: &str, pkg: &str) -> Result<Option<i32>> {
    if !crate::adb::is_safe_pkg_name(pkg) {
        warn!(pkg, "logcat: unsafe package name, refusing pidof");
        return Ok(None);
    }
    let out = Command::new(adb_binary())
        .args(["-s", serial, "shell", &format!("pidof {pkg}")])
        .output()
        .await
        .context("spawn adb shell pidof")?;
    if !out.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    // `pidof` may return space-separated PIDs (multi-process apps).
    // Take the first; logcat --pid only accepts one. If the user
    // wants secondary processes too, they can disable PID filter
    // in the terminal toolbar.
    let pid = stdout.split_whitespace().next().and_then(|s| s.parse().ok());
    Ok(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_threadtime_line() {
        let raw =
            "05-16 13:45:23.123  1234  5678 I ActivityManager: Start proc 9999 for activity";
        let line = parse_threadtime(raw, 1_700_000_000_000).unwrap();
        assert_eq!(line.pid, Some(1234));
        assert!(matches!(line.level, LogLevel::Info));
        assert_eq!(line.tag, "ActivityManager");
        assert_eq!(line.message, "Start proc 9999 for activity");
    }

    #[test]
    fn parses_error_line_with_colon_in_message() {
        let raw = "05-16 13:45:23.123  1234  5678 E TheTag: foo: bar: baz";
        let line = parse_threadtime(raw, 0).unwrap();
        assert_eq!(line.tag, "TheTag");
        assert_eq!(line.message, "foo: bar: baz");
        assert!(matches!(line.level, LogLevel::Error));
    }

    #[test]
    fn skips_header_banner() {
        let raw = "--------- beginning of main";
        assert!(parse_threadtime(raw, 0).is_none());
    }

    #[test]
    fn parses_line_with_no_tag_colon() {
        let raw = "05-16 13:45:23.123  1234  5678 W some-orphan message";
        let line = parse_threadtime(raw, 0).unwrap();
        // No "tag: message" — message gets the whole tail.
        assert_eq!(line.tag, "");
        assert_eq!(line.message, "some-orphan message");
    }
}
