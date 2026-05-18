use crate::adb;
use anyhow::Result;
use mperf_schema::{Device, Platform, Transport};
use serde::Serialize;

pub async fn list_devices() -> Result<Vec<Device>> {
    let raw = adb::list_raw().await?;
    Ok(parse_devices_long(&raw))
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub id: String,
    pub platform: String,
    pub model: Option<String>,
    pub manufacturer: Option<String>,
    pub os_version: Option<String>,
    pub build: Option<String>,
    /// Additional key-value pairs surfaced to the UI as a compact table.
    pub extra: Vec<(String, String)>,
}

/// Fetch a curated set of Android properties via `adb shell getprop` plus
/// QA-relevant runtime info (RAM, storage, battery, screen, locale).
/// Runs all the secondary queries in parallel so the panel still loads in
/// a single round-trip's worth of latency.
pub async fn device_info(serial: &str) -> Result<DeviceInfo> {
    let (props_raw, name_raw, meminfo_raw, df_raw, battery_raw, wm_size_raw, wm_density_raw) = tokio::join!(
        adb::shell_raw(serial, "getprop"),
        adb::shell_raw(serial, "settings get global device_name"),
        adb::shell_raw(serial, "cat /proc/meminfo"),
        adb::shell_raw(serial, "df -k /data"),
        adb::shell_raw(serial, "dumpsys battery"),
        adb::shell_raw(serial, "wm size"),
        adb::shell_raw(serial, "wm density"),
    );
    let raw = props_raw?;
    let props = parse_getprop(&raw);
    let get = |k: &str| props.get(k).map(|s| s.to_string());

    // Prefer the marketing name when the OEM provides it (Samsung newer
    // ROMs set ro.config.marketing_name; Xiaomi / Oppo use other variants);
    // fall back to the raw `ro.product.model` code.
    let model = get("ro.config.marketing_name")
        .or_else(|| get("ro.product.vendor.marketing_name"))
        .or_else(|| get("ro.product.marketing_name"))
        .or_else(|| get("ro.product.model"));

    let mut info = DeviceInfo {
        id: serial.to_string(),
        platform: "android".into(),
        model,
        manufacturer: get("ro.product.manufacturer"),
        os_version: get("ro.build.version.release"),
        // `display.id` is usually the human "PPR1.xxx" / "TQ3A.xxx" build
        // tag. The fingerprint is too long for the primary row.
        build: get("ro.build.display.id"),
        extra: Vec::new(),
    };

    // User-set device name.
    if let Ok(name_raw) = name_raw {
        let name = name_raw.trim();
        if !name.is_empty() && name != "null" {
            info.extra.push(("Name".to_string(), name.to_string()));
        }
    }

    // RAM (MemTotal in KB).
    if let Ok(s) = &meminfo_raw {
        if let Some(kb) = parse_mem_total_kb(s) {
            info.extra
                .push(("RAM".to_string(), format_bytes(kb * 1024)));
        }
    }

    // Storage (df -k /data — KB-aligned columns).
    if let Ok(s) = &df_raw {
        if let Some((total_kb, used_kb)) = parse_df_data(s) {
            let total = format_bytes(total_kb * 1024);
            let used = format_bytes(used_kb * 1024);
            let free = format_bytes((total_kb.saturating_sub(used_kb)) * 1024);
            info.extra
                .push(("Storage".to_string(), format!("{used} / {total} (free {free})")));
        }
    }

    // Battery.
    if let Ok(s) = &battery_raw {
        if let Some(b) = parse_battery(s) {
            info.extra.push(("Battery".to_string(), b));
        }
    }

    // Screen.
    let screen = format_screen(wm_size_raw.as_deref().ok(), wm_density_raw.as_deref().ok());
    if let Some(line) = screen {
        info.extra.push(("Screen".to_string(), line));
    }

    // Locale (from getprop).
    if let Some(loc) = get("persist.sys.locale").or_else(|| get("ro.product.locale")) {
        if !loc.is_empty() {
            info.extra.push(("Locale".to_string(), loc));
        }
    }

    for (label, key) in [
        ("Raw model", "ro.product.model"),
        ("SDK", "ro.build.version.sdk"),
        ("Device", "ro.product.device"),
        ("CPU ABI", "ro.product.cpu.abi"),
        ("Hardware", "ro.hardware"),
    ] {
        if let Some(v) = get(key) {
            if !v.is_empty() {
                if label == "Raw model" && Some(&v) == info.model.as_ref() {
                    continue;
                }
                info.extra.push((label.to_string(), v));
            }
        }
    }
    Ok(info)
}

fn parse_mem_total_kb(s: &str) -> Option<u64> {
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            return rest.split_whitespace().next().and_then(|s| s.parse().ok());
        }
    }
    None
}

/// Parse `df -k /data` output. Returns (total_kb, used_kb).
/// Output:
/// ```text
/// Filesystem     1K-blocks      Used Available Use% Mounted on
/// /dev/...       123456789 12345678  98765432  12% /data
/// ```
fn parse_df_data(s: &str) -> Option<(u64, u64)> {
    let last = s.lines().rev().find(|l| !l.trim().is_empty())?;
    let parts: Vec<&str> = last.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    let total = parts[1].parse::<u64>().ok()?;
    let used = parts[2].parse::<u64>().ok()?;
    Some((total, used))
}

fn parse_battery(s: &str) -> Option<String> {
    let mut level: Option<u32> = None;
    let mut status_str: Option<&'static str> = None;
    let mut ac: Option<bool> = None;
    let mut usb: Option<bool> = None;
    let mut temp: Option<f64> = None;
    for line in s.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("level:") {
            level = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("status:") {
            status_str = match v.trim() {
                "1" => Some("unknown"),
                "2" => Some("charging"),
                "3" => Some("discharging"),
                "4" => Some("not charging"),
                "5" => Some("full"),
                _ => None,
            };
        } else if let Some(v) = line.strip_prefix("AC powered:") {
            ac = matches!(v.trim(), "true").then_some(true);
        } else if let Some(v) = line.strip_prefix("USB powered:") {
            usb = matches!(v.trim(), "true").then_some(true);
        } else if let Some(v) = line.strip_prefix("temperature:") {
            // dumpsys returns tenths of °C.
            temp = v.trim().parse::<f64>().ok().map(|x| x / 10.0);
        }
    }
    let l = level?;
    let mut s = format!("{l}%");
    if let Some(st) = status_str {
        s.push_str(&format!(" ({st})"));
    }
    if ac.unwrap_or(false) || usb.unwrap_or(false) {
        s.push_str(" · plugged");
    }
    if let Some(t) = temp {
        s.push_str(&format!(" · {t:.1}°C"));
    }
    Some(s)
}

fn format_screen(size_raw: Option<&str>, density_raw: Option<&str>) -> Option<String> {
    let size = size_raw.and_then(|s| {
        s.lines()
            .filter_map(|l| l.split_once("size: "))
            .map(|(_, v)| v.trim().to_string())
            .next()
    });
    let density = density_raw.and_then(|s| {
        s.lines()
            .filter_map(|l| l.split_once("density: "))
            .map(|(_, v)| v.trim().to_string())
            .next()
    });
    match (size, density) {
        (Some(s), Some(d)) => Some(format!("{s} @ {d} dpi")),
        (Some(s), None) => Some(s),
        _ => None,
    }
}

fn format_bytes(bytes: u64) -> String {
    let kb = bytes as f64 / 1024.0;
    let mb = kb / 1024.0;
    let gb = mb / 1024.0;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else if mb >= 1.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{kb:.0} KB")
    }
}

fn parse_getprop(text: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    // Lines look like: `[ro.product.model]: [SM-S938U]`
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('[') else { continue };
        let Some(close) = rest.find(']') else { continue };
        let key = &rest[..close];
        let after = &rest[close + 1..];
        let Some(val_part) = after.trim_start().strip_prefix(": [") else { continue };
        let val_part = val_part.trim_end();
        let val = val_part.strip_suffix(']').unwrap_or(val_part);
        out.insert(key.to_string(), val.to_string());
    }
    out
}

#[cfg(test)]
mod info_tests {
    use super::*;

    #[test]
    fn parses_getprop_basic() {
        let s = "[ro.product.model]: [SM-S938U]\n[ro.product.manufacturer]: [samsung]\n[ro.build.version.release]: [15]\n";
        let p = parse_getprop(s);
        assert_eq!(p.get("ro.product.model").map(|s| s.as_str()), Some("SM-S938U"));
        assert_eq!(p.get("ro.product.manufacturer").map(|s| s.as_str()), Some("samsung"));
        assert_eq!(p.get("ro.build.version.release").map(|s| s.as_str()), Some("15"));
    }
}

fn parse_devices_long(out: &str) -> Vec<Device> {
    let mut devices = Vec::new();
    for line in out.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(id) = parts.next() else { continue };
        let Some(state) = parts.next() else { continue };

        let mut model = None;
        for kv in parts {
            if let Some(v) = kv.strip_prefix("model:") {
                model = Some(v.to_string());
            }
        }

        // `adb connect <ip>:<port>` produces serials like "192.168.1.5:5555".
        // Plain USB serials never contain a colon. Both transports support
        // full sampling on Android (unlike iOS, where Wi-Fi is sample-blind).
        let transport = if id.contains(':') {
            Transport::Wifi
        } else {
            Transport::Usb
        };
        // `usable` must reflect the live transport state — adb keeps an
        // unplugged device in its list as `state="offline"` for some
        // seconds (sometimes longer) after the cable is yanked. If we
        // hardcode true, the frontend watchdog sees the device still
        // present + usable and never fires the "device disconnected"
        // path, so Stop stays stuck and the History row stays
        // "in progress" indefinitely. Only `device` is actively
        // command-ready; `offline`/`unauthorized`/`no permissions`/
        // `recovery`/`bootloader`/`sideload` all mean adb can't run
        // shell commands on it.
        let usable = state == "device";
        devices.push(Device {
            id: id.to_string(),
            platform: Platform::Android,
            transport,
            state: state.to_string(),
            model,
            usable,
        });
    }
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_long_output() {
        let sample = "List of devices attached\n\
            R5CN30ABCDE             device usb:1-1 product:foo model:Pixel_7 device:bar transport_id:1\n\
            emulator-5554           offline\n\
            192.168.1.5:5555        device product:foo model:Pixel_7 device:bar transport_id:2\n";
        let devs = parse_devices_long(sample);
        assert_eq!(devs.len(), 3);
        assert_eq!(devs[0].id, "R5CN30ABCDE");
        assert_eq!(devs[0].model.as_deref(), Some("Pixel_7"));
        assert_eq!(devs[0].transport, Transport::Usb);
        assert!(devs[0].usable);
        assert_eq!(devs[1].state, "offline");
        assert!(!devs[1].usable);
        assert_eq!(devs[2].transport, Transport::Wifi);
        assert!(devs[2].usable);
    }
}
