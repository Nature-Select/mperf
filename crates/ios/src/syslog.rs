//! iOS device syslog stream via lockdown's `com.apple.syslog_relay`
//! service. Same source as the `idevicesyslog` command-line tool.
//!
//! Each line on the wire looks roughly like:
//!
//! ```text
//! Aug 30 10:15:32 iPhone procname[1234] <Notice>: actual message text
//! ```
//!
//! (The terminator is `\n\x00`; `SyslogRelayClient::next` strips that.)
//!
//! Note about concurrency with sysmontap: CLAUDE.md flags that some
//! lockdown queries (`lockdown.get_value(..., Some("com.apple.mobile.battery"))`)
//! return empty while our CPU sampler holds the CoreDeviceProxy DTX
//! tunnel. `syslog_relay` is a different beast — it's a normal
//! lockdown service whose socket goes through usbmuxd directly, not
//! through CoreDeviceProxy. Empirically (and per `idevicesyslog`
//! behavior on stock iOS) it works concurrently with DTX. If a future
//! iOS release breaks this, fall back to the DTX `os_trace_relay`
//! service over the same RemoteServerClient sysmontap uses.

use crate::connect;
use anyhow::{Context, Result};
use idevice::{services::syslog_relay::SyslogRelayClient, IdeviceService};
use mperf_schema::{LogLevel, LogLine};

pub struct SyslogStream {
    client: SyslogRelayClient,
}

impl SyslogStream {
    pub async fn start(udid: &str) -> Result<Self> {
        let provider = connect::provider_for(udid)
            .await
            .context("provider_for")?;
        let client = SyslogRelayClient::connect(&*provider)
            .await
            .context("SyslogRelayClient::connect")?;
        Ok(Self { client })
    }

    /// Pull the next parsed log line. Returns `Ok(None)` only when
    /// the service closes the stream (device unpaired / disconnected).
    pub async fn next_line(&mut self) -> Result<Option<LogLine>> {
        let ts_ms = now_ms();
        let raw = match self.client.next().await {
            Ok(s) => s,
            Err(idevice::IdeviceError::UnexpectedResponse(_)) => return Ok(None),
            Err(e) => return Err(anyhow::Error::new(e).context("syslog_relay.next")),
        };
        Ok(Some(parse_syslog_line(&raw, ts_ms)))
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Parse one syslog line. Format is well-established:
///
/// ```text
/// <Mon DD HH:MM:SS> <hostname> <process>[<pid>] <<Level>>: <message>
/// ```
///
/// We don't need the timestamp (we use host wall-clock from `ts_ms`),
/// we don't need the hostname. We do extract process / pid / level
/// from the prefix.
///
/// Failure modes (malformed prefix, kernel messages without `<Level>`,
/// etc.) degrade gracefully: the whole raw string ends up in
/// `message`, level = Unknown, process/pid = None — so the user
/// still sees it in the terminal.
fn parse_syslog_line(raw: &str, ts_ms: i64) -> LogLine {
    let raw = raw.trim_end_matches(|c: char| c == '\n' || c == '\r' || c == '\0');
    // Strip the date prefix: "Mon DD HH:MM:SS " — 16 chars on most
    // syslog implementations but we just look for the first 3 spaces
    // (after weekday, after day, after time) to be format-agnostic.
    let after_ts = strip_n_tokens(raw, 3).unwrap_or(raw);
    // Strip hostname (one token).
    let after_host = strip_n_tokens(after_ts, 1).unwrap_or(after_ts);
    // Now we should be at `process[pid] <Level>: message`.
    let unknown = || LogLine {
        ts_ms,
        level: LogLevel::Unknown,
        tag: String::new(),
        message: raw.to_string(),
        process: None,
        pid: None,
        subcategory: None,
    };
    // Extract process name + pid.
    let Some(bracket_open) = after_host.find('[') else {
        return unknown();
    };
    let process = after_host[..bracket_open].trim().to_string();
    let Some(bracket_close_rel) = after_host[bracket_open + 1..].find(']') else {
        return unknown();
    };
    let bracket_close = bracket_open + 1 + bracket_close_rel;
    let pid: Option<i32> = after_host[bracket_open + 1..bracket_close].parse().ok();
    let after_pid = after_host[bracket_close + 1..].trim_start();
    // After `]` we expect ` <Level>: message`. The level tag is
    // surrounded by angle brackets.
    let (level, after_level) = if let Some(stripped) = after_pid.strip_prefix('<') {
        if let Some(close) = stripped.find('>') {
            let lvl = match stripped[..close].to_ascii_lowercase().as_str() {
                "emergency" | "alert" | "critical" | "fatal" => LogLevel::Fatal,
                "error" | "err" => LogLevel::Error,
                "warning" | "warn" => LogLevel::Warn,
                "notice" | "info" => LogLevel::Info,
                "debug" => LogLevel::Debug,
                _ => LogLevel::Unknown,
            };
            (lvl, stripped[close + 1..].trim_start())
        } else {
            (LogLevel::Unknown, after_pid)
        }
    } else {
        (LogLevel::Unknown, after_pid)
    };
    let message = after_level
        .strip_prefix(':')
        .map(str::trim_start)
        .unwrap_or(after_level)
        .to_string();
    LogLine {
        ts_ms,
        level,
        tag: String::new(),
        message,
        process: if process.is_empty() { None } else { Some(process) },
        pid,
        subcategory: None,
    }
}

/// Skip past `n` whitespace-separated tokens from the start of `s`,
/// return the remainder. Returns None if `s` has fewer than `n`
/// tokens.
fn strip_n_tokens(s: &str, n: usize) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut idx = 0;
    for _ in 0..n {
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() {
            return None;
        }
        while idx < bytes.len() && !bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
    }
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    Some(&s[idx..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_line() {
        let raw = "Aug 30 10:15:32 iPhone Runner[1234] <Notice>: hello world";
        let l = parse_syslog_line(raw, 0);
        assert_eq!(l.process.as_deref(), Some("Runner"));
        assert_eq!(l.pid, Some(1234));
        assert!(matches!(l.level, LogLevel::Info));
        assert_eq!(l.message, "hello world");
    }

    #[test]
    fn parses_error_with_colon_in_message() {
        let raw = "Aug 30 10:15:32 iPhone foo[42] <Error>: a: b: c";
        let l = parse_syslog_line(raw, 0);
        assert_eq!(l.process.as_deref(), Some("foo"));
        assert!(matches!(l.level, LogLevel::Error));
        assert_eq!(l.message, "a: b: c");
    }

    #[test]
    fn degrades_unknown_format_to_raw_message() {
        // Kernel messages and similar can come through without the
        // process[pid] structure. The line still ends up in the
        // terminal verbatim.
        let raw = "Aug 30 10:15:32 iPhone something weird here";
        let l = parse_syslog_line(raw, 0);
        assert!(matches!(l.level, LogLevel::Unknown));
        assert!(l.message.contains("weird here"));
    }
}
