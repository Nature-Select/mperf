//! Device enumeration and the rich device-info panel.
//!
//! `device_info()` shells out a handful of parallel `adb shell` queries
//! and assembles a PerfDog-shaped Info/Value table. Each row is a
//! `DeviceField { label, value: Option<String> }` — `None` becomes
//! "unavailable" in the UI, so the field set is stable across devices
//! even when a particular kernel path is missing.

use crate::adb;
use anyhow::Result;
use mperf_schema::{Device, DeviceField, DeviceInfo, Platform, Transport};
use std::collections::BTreeMap;

pub async fn list_devices() -> Result<Vec<Device>> {
    let raw = adb::list_raw().await?;
    Ok(parse_devices_long(&raw))
}

/// Everything we want from the device, bundled into ONE adb shell.
///
/// Why one big script instead of `tokio::join!`-ing 8 small ones: adbd
/// on some vendor builds (notably Samsung One UI) serialises concurrent
/// shells more aggressively than stock AOSP. With 8 parallel shells in
/// flight, the user's next adb command (e.g. `list_apps`) ends up
/// queued behind them — looks indistinguishable from a hang. Running
/// everything through a single sh session sidesteps that.
///
/// `timeout 2` on dumpsys is mandatory: Android's SurfaceFlinger
/// swallows SIGPIPE and keeps producing output after a piped consumer
/// closes, so without the cap `dumpsys | head` can run for 30+ seconds
/// on a busy device.
///
/// Sections are demarcated by `===NAME===` lines so the Rust side can
/// scan once and dispatch. Sub-sections inside DEVFREQ are
/// `---name---` markers.
const DEVICE_INFO_SCRIPT: &str = "\
echo ===GETPROP===
getprop
echo ===DEVICENAME===
settings get global device_name 2>/dev/null
echo ===MEMINFO===
cat /proc/meminfo 2>/dev/null
echo ===WMSIZE===
wm size 2>/dev/null
echo ===WMDENSITY===
wm density 2>/dev/null
echo ===CPUINFO===
cat /proc/cpuinfo 2>/dev/null
echo ===CORES===
ls -1d /sys/devices/system/cpu/cpu[0-9]* 2>/dev/null
echo ===MAXFREQ===
for f in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/cpuinfo_max_freq; do [ -r \"$f\" ] && echo \"$f $(cat $f)\"; done 2>/dev/null
echo ===MINFREQ===
for f in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/cpuinfo_min_freq; do [ -r \"$f\" ] && echo \"$f $(cat $f)\"; done 2>/dev/null
echo ===SF===
timeout 2 dumpsys SurfaceFlinger 2>/dev/null | head -60
echo ===KGSL_MIN===
cat /sys/class/kgsl/kgsl-3d0/devfreq/min_freq 2>/dev/null
echo ===KGSL_MAX===
cat /sys/class/kgsl/kgsl-3d0/devfreq/max_freq 2>/dev/null
echo ===DEVFREQ===
for d in /sys/class/devfreq/* ; do
  [ -d \"$d\" ] || continue
  name=$(basename \"$d\")
  case \"$name\" in
    *gpu*|*mali*|*GPU*|*Mali*)
      echo \"---$name---\"
      cat \"$d/min_freq\" 2>/dev/null
      cat \"$d/max_freq\" 2>/dev/null
      ;;
  esac
done
echo ===SUFILES===
ls /system/bin/su /system/xbin/su /sbin/su /vendor/bin/su 2>/dev/null
echo ===SUWHICH===
which su 2>/dev/null
echo ===DEBUG===
getprop ro.debuggable 2>/dev/null
echo ===LMK===
cat /sys/module/lowmemorykiller/parameters/minfree 2>/dev/null
echo ===END===
";

pub async fn device_info(serial: &str) -> Result<DeviceInfo> {
    let start = std::time::Instant::now();
    // Hard 8-second cap: a single Samsung S9310 in the field reproduced a
    // 110-second hang on this exact shell call where the same script
    // returned in 225ms from an ad-hoc adb terminal session — root cause
    // wasn't identifiable from logs. Rather than leave the UI's "loading
    // app list" spinner indefinitely behind a sibling adb operation, we
    // cap the wall time and surface a typed error so the frontend can
    // show a retry hint. 8s is generous: 95-th-percentile observed
    // device_info shell completes in <1s on the same workspace script.
    let raw = match tokio::time::timeout(
        std::time::Duration::from_secs(8),
        adb::shell_raw(serial, DEVICE_INFO_SCRIPT),
    )
    .await
    {
        Ok(r) => r?,
        Err(_) => {
            anyhow::bail!(
                "device_info shell timed out after 8s (script size {} bytes)",
                DEVICE_INFO_SCRIPT.len()
            );
        }
    };
    tracing::info!(
        ms = start.elapsed().as_millis() as u64,
        bytes = raw.len(),
        "device_info: bundled adb shell complete"
    );

    let sections = parse_sections(&raw);
    let section = |name: &str| sections.get(name).map(|s| s.as_str()).unwrap_or("");

    let props = parse_getprop(section("GETPROP"));
    let get = |k: &str| props.get(k).cloned().filter(|s| !s.is_empty());

    let model_code = get("ro.product.model");
    let device_name_raw = section("DEVICENAME").trim();
    let device_name = if device_name_raw.is_empty() || device_name_raw == "null" {
        model_code.clone()
    } else {
        Some(device_name_raw.to_string())
    };

    let cpu_info = parse_cpuinfo_label(section("CPUINFO"));
    let core_count = parse_core_count(section("CORES"));
    let max_freqs = parse_per_core_freqs(section("MAXFREQ"));
    let min_freqs = parse_per_core_freqs(section("MINFREQ"));
    let (cpu_freq_str, cpu_cluster_str) = format_freq_and_cluster(&min_freqs, &max_freqs);

    let (gpu_type, opengl) = parse_gles_line(section("SF"));
    let gpu_freq = format_gpu_freq(&sections);

    let root = detect_root(&sections);
    let lmk = parse_lmk(section("LMK"));

    let mem_total_kb = parse_meminfo_kv(section("MEMINFO"), "MemTotal");
    let swap_total_kb = parse_meminfo_kv(section("MEMINFO"), "SwapTotal");

    let resolution = parse_wm_size(section("WMSIZE"));
    let _density = parse_wm_density(section("WMDENSITY"));

    let rom = format_rom(get("ro.build.id"), get("ro.build.display.id"));

    let fields = vec![
        DeviceField::new("Device Name", device_name),
        DeviceField::new("Device Type", model_code.clone()),
        DeviceField::new("OS", get("ro.build.version.release")),
        DeviceField::new(
            "CPU Type",
            get("ro.board.platform").or_else(|| get("ro.hardware")),
        ),
        DeviceField::new("CPU Info", cpu_info),
        DeviceField::new("CPU Arch", get("ro.product.cpu.abi")),
        DeviceField::new("CPU CoreNum", core_count.map(|c| c.to_string())),
        DeviceField::new("CPU Freq", cpu_freq_str),
        DeviceField::new("CPU Cluster", cpu_cluster_str),
        DeviceField::new("GPU Type", gpu_type),
        DeviceField::new("OpenGL", opengl),
        DeviceField::new("GPU Freq", gpu_freq),
        DeviceField::new("Resolution", resolution),
        DeviceField::new("Ram Size", mem_total_kb.map(format_kb_size)),
        DeviceField::new("LMK Threshold", lmk),
        DeviceField::new("Swap", swap_total_kb.map(format_kb_size)),
        DeviceField::new("Root", root),
        DeviceField::some("SerialNum", serial),
        DeviceField::new("Rom", rom),
    ];

    Ok(DeviceInfo {
        id: serial.to_string(),
        platform: Platform::Android,
        fields,
    })
}

/// Split text on `===NAME===` markers and return section name → body.
/// Sections are inclusive of trailing whitespace but exclude the marker
/// line itself. Bodies are trimmed of leading/trailing blank lines.
fn parse_sections(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut buf = String::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(name) = t.strip_prefix("===").and_then(|s| s.strip_suffix("===")) {
            if let Some(prev) = current.take() {
                out.insert(prev, std::mem::take(&mut buf).trim().to_string());
            }
            current = Some(name.to_string());
        } else if current.is_some() {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    if let Some(prev) = current {
        out.insert(prev, buf.trim().to_string());
    }
    out
}

/// `/proc/cpuinfo` "Hardware" or "model name" line, if present. Modern
/// AOSP often blanks these for security reasons — fall through to None
/// so the UI shows "unavailable", matching PerfDog's behaviour.
fn parse_cpuinfo_label(s: &str) -> Option<String> {
    for line in s.lines() {
        let line = line.trim();
        for prefix in ["Hardware", "model name", "CPU implementer", "CPU part"] {
            if let Some(rest) = line.strip_prefix(prefix) {
                if let Some((_, v)) = rest.split_once(':') {
                    let v = v.trim();
                    if !v.is_empty() {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

fn parse_core_count(s: &str) -> Option<u32> {
    let n = s
        .lines()
        .filter(|l| {
            let t = l.trim();
            t.starts_with("/sys/devices/system/cpu/cpu")
                && t.chars()
                    .skip_while(|c| !c.is_ascii_digit())
                    .next()
                    .map(|c| c.is_ascii_digit())
                    .unwrap_or(false)
        })
        .count();
    (n > 0).then_some(n as u32)
}

/// Parse lines like
/// `/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq 3532000`
/// into (cpu_index, freq_khz). Frequencies are expressed in kHz by
/// the kernel — we keep them in kHz here and convert to MHz at the
/// formatting layer.
fn parse_per_core_freqs(s: &str) -> Vec<(u32, u64)> {
    let mut out = Vec::new();
    for line in s.lines() {
        let line = line.trim();
        let Some((path, val)) = line.split_once(' ') else { continue };
        let Some(idx_str) = path
            .strip_prefix("/sys/devices/system/cpu/cpu")
            .and_then(|s| s.split('/').next())
        else {
            continue;
        };
        let Ok(idx) = idx_str.parse::<u32>() else { continue };
        let Ok(khz) = val.trim().parse::<u64>() else { continue };
        out.push((idx, khz));
    }
    out.sort_by_key(|&(i, _)| i);
    out
}

/// Build PerfDog-style strings:
///   CPU Freq    = "384MHz - 3532MHz / 1017MHz - 4473MHz"     (per cluster)
///   CPU Cluster = "0-5:3532MHz / 6-7:4473MHz"                 (cluster ranges)
/// Cores are grouped by their max frequency in ascending order.
fn format_freq_and_cluster(
    mins: &[(u32, u64)],
    maxs: &[(u32, u64)],
) -> (Option<String>, Option<String>) {
    if maxs.is_empty() {
        return (None, None);
    }
    let min_by_idx: BTreeMap<u32, u64> = mins.iter().copied().collect();
    let mut by_max: BTreeMap<u64, Vec<u32>> = BTreeMap::new();
    for (idx, max) in maxs.iter().copied() {
        by_max.entry(max).or_default().push(idx);
    }
    let mut freq_parts = Vec::new();
    let mut cluster_parts = Vec::new();
    for (cluster_max_khz, mut cores) in by_max {
        cores.sort();
        // Range string: contiguous runs of CPU indices.
        let mut idx_str = String::new();
        let mut i = 0;
        while i < cores.len() {
            let start = cores[i];
            let mut end = start;
            while i + 1 < cores.len() && cores[i + 1] == end + 1 {
                end += 1;
                i += 1;
            }
            if !idx_str.is_empty() {
                idx_str.push(',');
            }
            if start == end {
                idx_str.push_str(&start.to_string());
            } else {
                idx_str.push_str(&format!("{start}-{end}"));
            }
            i += 1;
        }
        let cluster_min_khz = cores
            .iter()
            .filter_map(|c| min_by_idx.get(c).copied())
            .min()
            .unwrap_or(0);
        let max_mhz = cluster_max_khz / 1000;
        let min_mhz = cluster_min_khz / 1000;
        freq_parts.push(format!("{min_mhz}MHz - {max_mhz}MHz"));
        cluster_parts.push(format!("{idx_str}:{max_mhz}MHz"));
    }
    (
        Some(freq_parts.join(" / ")),
        Some(cluster_parts.join(" / ")),
    )
}

/// Parse the "GLES:" header line out of `dumpsys SurfaceFlinger` and
/// split into `(gpu_type, opengl_version)`. Example input line:
///     GLES: Qualcomm, Adreno (TM) 830, OpenGL ES 3.2 V@0800.40.1 (GIT@e4a2ccdb56)
/// Returns the comma-separated pieces as Vendor+Renderer / Version.
fn parse_gles_line(sf: &str) -> (Option<String>, Option<String>) {
    for line in sf.lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix("GLES:") else { continue };
        // Three comma-separated fields: vendor, renderer, version.
        let parts: Vec<&str> = rest.split(',').map(|s| s.trim()).collect();
        let gpu = match parts.as_slice() {
            [vendor, renderer, ..] if !vendor.is_empty() && !renderer.is_empty() => {
                Some(format!("{vendor} {renderer}"))
            }
            [renderer] if !renderer.is_empty() => Some((*renderer).to_string()),
            _ => None,
        };
        let version = parts.get(2).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        return (gpu, version);
    }
    (None, None)
}

fn format_gpu_freq(sections: &BTreeMap<String, String>) -> Option<String> {
    // Qualcomm Adreno path: /sys/class/kgsl/kgsl-3d0/devfreq/{min,max}_freq.
    let kgsl_min = sections.get("KGSL_MIN").and_then(|s| s.lines().next()).and_then(|s| s.trim().parse::<u64>().ok());
    let kgsl_max = sections.get("KGSL_MAX").and_then(|s| s.lines().next()).and_then(|s| s.trim().parse::<u64>().ok());
    if let (Some(mn), Some(mx)) = (kgsl_min, kgsl_max) {
        return Some(format!("{}MHz - {}MHz", hz_to_mhz(mn), hz_to_mhz(mx)));
    }
    // Mali / generic devfreq fallback: first GPU-looking node wins.
    let devfreq = sections.get("DEVFREQ")?;
    let mut min: Option<u64> = None;
    let mut max: Option<u64> = None;
    for line in devfreq.lines() {
        let t = line.trim();
        if t.starts_with("---") && t.ends_with("---") {
            min = None;
            max = None;
            continue;
        }
        if let Ok(v) = t.parse::<u64>() {
            if min.is_none() {
                min = Some(v);
            } else if max.is_none() {
                max = Some(v);
                break;
            }
        }
    }
    match (min, max) {
        (Some(mn), Some(mx)) => Some(format!("{}MHz - {}MHz", hz_to_mhz(mn), hz_to_mhz(mx))),
        _ => None,
    }
}

fn hz_to_mhz(hz: u64) -> u64 {
    // devfreq exposes values in Hz on most SoCs but in kHz on a few
    // older ones. The split point is "anything ≥ 1e6" → Hz.
    if hz >= 1_000_000 {
        hz / 1_000_000
    } else {
        hz / 1_000
    }
}

fn detect_root(sections: &BTreeMap<String, String>) -> Option<String> {
    let has_su = sections
        .get("SUFILES")
        .map(|s| s.lines().any(|l| l.trim().contains("/su")))
        .unwrap_or(false)
        || sections
            .get("SUWHICH")
            .map(|s| s.lines().any(|l| !l.trim().is_empty()))
            .unwrap_or(false);
    let debuggable = sections
        .get("DEBUG")
        .and_then(|s| s.lines().next())
        .map(|s| s.trim() == "1")
        .unwrap_or(false);
    Some(match (has_su, debuggable) {
        (true, _) => "Yes (su binary present)".to_string(),
        (false, true) => "Debuggable (userdebug/eng build)".to_string(),
        _ => "No".to_string(),
    })
}

fn parse_lmk(s: &str) -> Option<String> {
    let line = s.lines().find(|l| !l.trim().is_empty())?;
    Some(line.trim().to_string())
}

fn parse_meminfo_kv(s: &str, key: &str) -> Option<u64> {
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(rest) = rest.strip_prefix(':') {
                return rest.split_whitespace().next().and_then(|s| s.parse().ok());
            }
        }
    }
    None
}

fn parse_wm_size(s: &str) -> Option<String> {
    s.lines()
        .filter_map(|l| l.split_once("size: "))
        .map(|(_, v)| v.trim().to_string())
        .next()
}

fn parse_wm_density(s: &str) -> Option<String> {
    s.lines()
        .filter_map(|l| l.split_once("density: "))
        .map(|(_, v)| v.trim().to_string())
        .next()
}

fn format_rom(build_id: Option<String>, display_id: Option<String>) -> Option<String> {
    match (build_id, display_id) {
        (Some(b), Some(d)) if b != d => Some(format!("{b} / {d}")),
        (Some(b), _) => Some(b),
        (None, Some(d)) => Some(d),
        _ => None,
    }
}

fn format_kb_size(kb: u64) -> String {
    let bytes = kb.saturating_mul(1024);
    let kb_f = bytes as f64 / 1024.0;
    let mb_f = kb_f / 1024.0;
    let gb_f = mb_f / 1024.0;
    if gb_f >= 1.0 {
        format!("{gb_f:.2} GB")
    } else if mb_f >= 1.0 {
        format!("{mb_f:.0} MB")
    } else {
        format!("{kb_f:.0} KB")
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

    #[test]
    fn parses_getprop_basic() {
        let s = "[ro.product.model]: [SM-S938U]\n[ro.product.manufacturer]: [samsung]\n[ro.build.version.release]: [15]\n";
        let p = parse_getprop(s);
        assert_eq!(p.get("ro.product.model").map(|s| s.as_str()), Some("SM-S938U"));
    }

    #[test]
    fn sections_split() {
        let s = "===A===\nhello\n===B===\nworld\nmore\n";
        let m = parse_sections(s);
        assert_eq!(m.get("A").map(|s| s.as_str()), Some("hello"));
        assert_eq!(m.get("B").map(|s| s.as_str()), Some("world\nmore"));
    }

    #[test]
    fn core_count_skips_subdirs() {
        let s = "/sys/devices/system/cpu/cpu0\n/sys/devices/system/cpu/cpu1\n/sys/devices/system/cpu/cpu2\n";
        assert_eq!(parse_core_count(s), Some(3));
    }

    #[test]
    fn per_core_freqs_parse() {
        let s = "/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq 3532000\n/sys/devices/system/cpu/cpu7/cpufreq/cpuinfo_max_freq 4473000\n";
        let v = parse_per_core_freqs(s);
        assert_eq!(v, vec![(0, 3_532_000), (7, 4_473_000)]);
    }

    #[test]
    fn freq_and_cluster_two_groups() {
        let mins = vec![(0, 384_000), (5, 384_000), (6, 1_017_000), (7, 1_017_000)];
        let maxs = vec![(0, 3_532_000), (5, 3_532_000), (6, 4_473_000), (7, 4_473_000)];
        let (freq, cluster) = format_freq_and_cluster(&mins, &maxs);
        assert_eq!(freq.as_deref(), Some("384MHz - 3532MHz / 1017MHz - 4473MHz"));
        assert_eq!(cluster.as_deref(), Some("0,5:3532MHz / 6-7:4473MHz"));
    }

    #[test]
    fn gles_split() {
        let sf = "EGL implementation : 1.5\nGLES: Qualcomm, Adreno (TM) 830, OpenGL ES 3.2 V@0800.40.1 (GIT@e4a2ccdb56)\nmore stuff\n";
        let (gpu, opengl) = parse_gles_line(sf);
        assert_eq!(gpu.as_deref(), Some("Qualcomm Adreno (TM) 830"));
        assert!(opengl.as_deref().unwrap().starts_with("OpenGL ES 3.2"));
    }
}
