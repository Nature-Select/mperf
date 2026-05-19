//! Enumerate iOS devices via the macOS `usbmuxd` daemon and resolve their
//! human-readable device names via lockdown.
//!
//! `idevice::usbmuxd::UsbmuxdConnection` speaks the standard usbmuxd
//! protocol; no special privileges needed because usbmuxd brokers pairing.

use anyhow::{Context, Result};
use idevice::{
    services::lockdown::LockdownClient,
    usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection},
    IdeviceService,
};
use mperf_schema::{Device, DeviceField, DeviceInfo, Platform, Transport};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Process-wide cache for `udid → device_name`. iOS lockdown queries cost
/// ~150ms over USB; the device name rarely changes, so we cache for 15 min.
fn name_cache() -> &'static Mutex<HashMap<String, (String, Instant)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

const NAME_CACHE_TTL: Duration = Duration::from_secs(15 * 60);

pub async fn list_devices() -> Result<Vec<Device>> {
    let mut conn = UsbmuxdConnection::default()
        .await
        .context("connect to usbmuxd")?;
    let raw = conn.get_devices().await.context("get_devices")?;

    // PerfDog convention: surface BOTH USB and Wi-Fi entries for the same
    // UDID. Battery testing has to be done over Wi-Fi (USB charges the
    // phone and skews the reading). We don't dedupe here. The Wi-Fi
    // entry's `usable` is false until DTX-over-network lands (idevice
    // 0.1.61 can't bootstrap CoreDeviceProxy without a host-side tunneld
    // daemon), so the UI shows it grayed out with a tooltip. The
    // watchdog still works correctly because USB unplug removes the USB
    // entry (the Wi-Fi entry, if present, isn't `usable`).
    let mut name_by_udid: HashMap<String, Option<String>> = HashMap::new();
    let mut out = Vec::with_capacity(raw.len());
    for d in raw {
        let model = match name_by_udid.get(&d.udid) {
            Some(cached) => cached.clone(),
            None => {
                let n = lookup_device_name(&d.udid, d.device_id).await.ok().flatten();
                name_by_udid.insert(d.udid.clone(), n.clone());
                n
            }
        };
        let transport = match d.connection_type {
            Connection::Usb => Transport::Usb,
            _ => Transport::Wifi,
        };
        // iOS sampling currently needs the CoreDeviceProxy USB tunnel.
        // Wi-Fi entries are listed for visibility (and so the user knows
        // battery testing will be Wi-Fi-only once it lands) but flagged
        // unusable so the UI can disable Start with a tooltip.
        let usable = matches!(transport, Transport::Usb);
        out.push(Device {
            id: d.udid.clone(),
            platform: Platform::Ios,
            transport,
            state: format!("{:?}", d.connection_type).to_lowercase(),
            model,
            usable,
        });
    }
    // Sort so USB entries appear before Wi-Fi entries for the same UDID
    // (stable display order across polls).
    out.sort_by(|a, b| {
        a.id.cmp(&b.id)
            .then_with(|| match (a.transport, b.transport) {
                (Transport::Usb, Transport::Wifi) => std::cmp::Ordering::Less,
                (Transport::Wifi, Transport::Usb) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            })
    });
    Ok(out)
}

/// Ask lockdown for the device's user-set name (e.g. "cengdong's iPhone 17").
/// Cached per-UDID for 15 minutes so the every-3-second list_devices refresh
/// doesn't keep hitting the lockdown round-trip.
///
/// Returns `Ok(None)` when the device is unpaired or lockdown otherwise
/// refuses — we keep listing the device but show its UDID in the UI.
async fn lookup_device_name(udid: &str, device_id: u32) -> Result<Option<String>> {
    {
        let cache = name_cache().lock().unwrap();
        if let Some((name, fetched_at)) = cache.get(udid) {
            if fetched_at.elapsed() < NAME_CACHE_TTL {
                return Ok(Some(name.clone()));
            }
        }
    }
    let name = match lookup_device_name_uncached(udid, device_id).await? {
        Some(n) => n,
        None => return Ok(None),
    };
    name_cache()
        .lock()
        .unwrap()
        .insert(udid.to_string(), (name.clone(), Instant::now()));
    Ok(Some(name))
}

async fn lookup_device_name_uncached(udid: &str, device_id: u32) -> Result<Option<String>> {
    let mut conn = UsbmuxdConnection::default().await?;
    let usbmuxd_dev = conn
        .get_devices()
        .await?
        .into_iter()
        .find(|d| d.udid == udid && d.device_id == device_id)
        .ok_or_else(|| anyhow::anyhow!("device {udid} vanished during name lookup"))?;
    let provider = usbmuxd_dev.to_provider(UsbmuxdAddr::default(), "mperf");

    let mut lockdown = match LockdownClient::connect(&provider).await {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(udid = %udid, error = %e, "lockdown connect failed");
            return Ok(None);
        }
    };
    let value = match lockdown.get_value(Some("DeviceName"), None).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(udid = %udid, error = %e, "DeviceName query failed (unpaired?)");
            return Ok(None);
        }
    };
    Ok(value.as_string().map(|s| s.to_string()))
}

pub async fn device_info(udid: &str) -> Result<DeviceInfo> {
    let value = query_main(udid).await?;
    let dict = value
        .into_dictionary()
        .ok_or_else(|| anyhow::anyhow!("lockdown response not a dictionary"))?;
    let get = |k: &str| {
        dict.get(k)
            .and_then(|v| v.as_string())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    };

    let device_name = get("DeviceName");
    let product_type = get("ProductType");
    let product_version = get("ProductVersion");
    let build_version = get("BuildVersion");
    let cpu_arch = get("CPUArchitecture");

    // Marketing name (e.g. "iPhone 17") if we know the ProductType, else
    // the raw ProductType code — same fallback the device listing uses.
    let marketing = product_type
        .as_deref()
        .and_then(product_type_to_marketing_name)
        .map(String::from);
    let device_type = marketing
        .clone()
        .or_else(|| product_type.as_deref().map(|pt| family_from_product_type(pt).to_string()));

    let os_combined = match (&product_version, &build_version) {
        (Some(v), Some(b)) => Some(format!("{v} ({b})")),
        (Some(v), None) => Some(v.clone()),
        _ => None,
    };

    let spec = product_type.as_deref().and_then(chipset_for);
    let cpu_type = spec.map(|s| s.cpu.to_string());
    let cpu_core_num = spec.map(|s| s.cpu_cores.to_string());
    let cpu_freq = spec.and_then(|s| s.cpu_max_mhz.map(|mhz| format!("[0,{}]", mhz)));
    let gpu_type = spec.map(|s| s.gpu.to_string());

    let fields = vec![
        DeviceField::new("Device Name", device_name),
        DeviceField::new("Device Type", device_type),
        DeviceField::new("Product Type", product_type),
        DeviceField::new("OS", os_combined),
        DeviceField::new("CPU Type", cpu_type),
        DeviceField::new("CPU Arch", cpu_arch),
        DeviceField::new("CPU CoreNum", cpu_core_num),
        DeviceField::new("CPU Freq", cpu_freq),
        DeviceField::new("GPU Type", gpu_type),
    ];

    Ok(DeviceInfo {
        id: udid.to_string(),
        platform: Platform::Ios,
        fields,
    })
}

async fn query_main(udid: &str) -> Result<plist::Value> {
    let mut conn = UsbmuxdConnection::default().await.context("usbmuxd")?;
    let dev = conn
        .get_devices()
        .await?
        .into_iter()
        .find(|d| d.udid == udid)
        .ok_or_else(|| anyhow::anyhow!("device {udid} not found"))?;
    let provider = dev.to_provider(UsbmuxdAddr::default(), "mperf");
    let mut lockdown = LockdownClient::connect(&provider)
        .await
        .context("lockdown connect")?;
    let value = lockdown
        .get_value(None, None)
        .await
        .context("lockdown get_value(main)")?;
    Ok(value)
}

/// "iPhone14,7" → "iPhone14" — the family prefix matches PerfDog's
/// "Device Type" column when no marketing name is known. Falls back to
/// the full code if there's no comma (shouldn't happen for real
/// Apple ProductType strings, but be defensive).
fn family_from_product_type(pt: &str) -> &str {
    pt.split_once(',').map(|(family, _)| family).unwrap_or(pt)
}

/// Chipset spec sheet info indexed by Apple `ProductType`. iOS doesn't
/// expose CPU/GPU spec at runtime (Apple keeps it private), so this is
/// the same lookup-table approach PerfDog uses. Update when new devices
/// ship.
#[derive(Debug, Clone, Copy)]
struct ChipsetSpec {
    cpu: &'static str,
    cpu_cores: u8,
    /// Max P-core boost frequency, in MHz. Used for the PerfDog-style
    /// "[0,3240]" CPU Freq display — the "0" matches PerfDog's convention
    /// for "min unknown" (iOS doesn't expose live CPU frequency).
    ///
    /// `None` for chips where the public spec is still preliminary
    /// (e.g. A19 / A19 Pro pre-keynote) — the field then renders as
    /// "unavailable" instead of pretending a guess is fact.
    cpu_max_mhz: Option<u32>,
    gpu: &'static str,
}

fn chipset_for(pt: &str) -> Option<ChipsetSpec> {
    // Naming convention: short marketing name ("Apple A15"), no "Bionic"
    // suffix, to match PerfDog's display. GPU strings parenthesise the
    // core count exactly like PerfDog.
    macro_rules! spec {
        ($cpu:expr, $cores:expr, $max:expr, $gpu:expr) => {
            Some(ChipsetSpec { cpu: $cpu, cpu_cores: $cores, cpu_max_mhz: Some($max), gpu: $gpu })
        };
    }
    // Variant for chips whose max-boost figure isn't publicly verified
    // yet — keeps CPU name / core count / GPU but leaves the freq blank.
    macro_rules! spec_no_freq {
        ($cpu:expr, $cores:expr, $gpu:expr) => {
            Some(ChipsetSpec { cpu: $cpu, cpu_cores: $cores, cpu_max_mhz: None, gpu: $gpu })
        };
    }
    match pt {
        // A11 — iPhone 8 / 8 Plus / X
        "iPhone10,1" | "iPhone10,2" | "iPhone10,3"
        | "iPhone10,4" | "iPhone10,5" | "iPhone10,6" =>
            spec!("Apple A11", 6, 2390, "Apple GPU (3-Core GPU)"),
        // A12 — XS / XS Max / XR
        "iPhone11,2" | "iPhone11,4" | "iPhone11,6" | "iPhone11,8" =>
            spec!("Apple A12", 6, 2490, "Apple GPU (4-Core GPU)"),
        // A13 — iPhone 11 series + SE 2
        "iPhone12,1" | "iPhone12,3" | "iPhone12,5" | "iPhone12,8" =>
            spec!("Apple A13", 6, 2650, "Apple GPU (4-Core GPU)"),
        // A14 — iPhone 12 series
        "iPhone13,1" | "iPhone13,2" | "iPhone13,3" | "iPhone13,4" =>
            spec!("Apple A14", 6, 2990, "Apple GPU (4-Core GPU)"),
        // A15 (4-core GPU) — iPhone 13 mini/13, SE 3
        "iPhone14,4" | "iPhone14,5" | "iPhone14,6" =>
            spec!("Apple A15", 6, 3230, "Apple GPU (4-Core GPU)"),
        // A15 (5-core GPU) — iPhone 13 Pro/Pro Max, iPhone 14/14 Plus
        "iPhone14,2" | "iPhone14,3" | "iPhone14,7" | "iPhone14,8" =>
            spec!("Apple A15", 6, 3230, "Apple GPU (5-Core GPU)"),
        // A16 — iPhone 14 Pro/Pro Max, iPhone 15/15 Plus
        "iPhone15,2" | "iPhone15,3" | "iPhone15,4" | "iPhone15,5" =>
            spec!("Apple A16", 6, 3460, "Apple GPU (5-Core GPU)"),
        // A17 Pro — iPhone 15 Pro/Pro Max
        "iPhone16,1" | "iPhone16,2" =>
            spec!("Apple A17 Pro", 6, 3780, "Apple GPU (6-Core GPU)"),
        // A18 — iPhone 16/16 Plus
        "iPhone17,3" | "iPhone17,4" =>
            spec!("Apple A18", 6, 4040, "Apple GPU (5-Core GPU)"),
        // A18 (binned 4-core GPU) — iPhone 16e
        "iPhone17,5" =>
            spec!("Apple A18", 6, 4040, "Apple GPU (4-Core GPU)"),
        // A18 Pro — iPhone 16 Pro/Pro Max
        "iPhone17,1" | "iPhone17,2" =>
            spec!("Apple A18 Pro", 6, 4040, "Apple GPU (6-Core GPU)"),
        // A19 / A19 Pro — iPhone 17 series (2025). Max-boost figures
        // aren't publicly verified yet; surface chip name / cores / GPU
        // but show CPU Freq as "unavailable" rather than print a guess.
        "iPhone18,3" => spec_no_freq!("Apple A19", 6, "Apple GPU (5-Core GPU)"),
        "iPhone18,4" => spec_no_freq!("Apple A19 Pro", 6, "Apple GPU (6-Core GPU)"),
        "iPhone18,1" | "iPhone18,2" =>
            spec_no_freq!("Apple A19 Pro", 6, "Apple GPU (6-Core GPU)"),
        _ => None,
    }
}

/// Translate an iOS `ProductType` identifier into the human-readable
/// marketing name. Returns `None` for ids we don't know yet — the caller
/// falls back to the raw family prefix ("iPhone14" etc.).
///
/// This table only needs to grow as new devices appear; the current set
/// covers iPhones from iPhone 8 onwards (Apple's identifiers prior to that
/// are unlikely to show up in modern testing).
pub(crate) fn product_type_to_marketing_name(pt: &str) -> Option<&'static str> {
    Some(match pt {
        // iPhone 8 / 8 Plus / X
        "iPhone10,1" | "iPhone10,4" => "iPhone 8",
        "iPhone10,2" | "iPhone10,5" => "iPhone 8 Plus",
        "iPhone10,3" | "iPhone10,6" => "iPhone X",
        // iPhone XS / XS Max / XR
        "iPhone11,2" => "iPhone XS",
        "iPhone11,4" | "iPhone11,6" => "iPhone XS Max",
        "iPhone11,8" => "iPhone XR",
        // iPhone 11 series
        "iPhone12,1" => "iPhone 11",
        "iPhone12,3" => "iPhone 11 Pro",
        "iPhone12,5" => "iPhone 11 Pro Max",
        "iPhone12,8" => "iPhone SE (2nd gen)",
        // iPhone 12 series
        "iPhone13,1" => "iPhone 12 mini",
        "iPhone13,2" => "iPhone 12",
        "iPhone13,3" => "iPhone 12 Pro",
        "iPhone13,4" => "iPhone 12 Pro Max",
        // iPhone 13 / SE3
        "iPhone14,2" => "iPhone 13 Pro",
        "iPhone14,3" => "iPhone 13 Pro Max",
        "iPhone14,4" => "iPhone 13 mini",
        "iPhone14,5" => "iPhone 13",
        "iPhone14,6" => "iPhone SE (3rd gen)",
        // iPhone 14 series
        "iPhone14,7" => "iPhone 14",
        "iPhone14,8" => "iPhone 14 Plus",
        "iPhone15,2" => "iPhone 14 Pro",
        "iPhone15,3" => "iPhone 14 Pro Max",
        // iPhone 15 series
        "iPhone15,4" => "iPhone 15",
        "iPhone15,5" => "iPhone 15 Plus",
        "iPhone16,1" => "iPhone 15 Pro",
        "iPhone16,2" => "iPhone 15 Pro Max",
        // iPhone 16 series
        "iPhone17,1" => "iPhone 16 Pro",
        "iPhone17,2" => "iPhone 16 Pro Max",
        "iPhone17,3" => "iPhone 16",
        "iPhone17,4" => "iPhone 16 Plus",
        "iPhone17,5" => "iPhone 16e",
        // iPhone 17 series (2025)
        "iPhone18,1" => "iPhone 17 Pro Max",
        "iPhone18,2" => "iPhone 17 Pro",
        "iPhone18,3" => "iPhone 17",
        "iPhone18,4" => "iPhone Air",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chipset_lookup_iphone_14() {
        let s = chipset_for("iPhone14,7").unwrap();
        assert_eq!(s.cpu, "Apple A15");
        assert_eq!(s.cpu_cores, 6);
        assert!(s.gpu.contains("5-Core"));
    }

    #[test]
    fn family_strip_works() {
        assert_eq!(family_from_product_type("iPhone14,7"), "iPhone14");
        assert_eq!(family_from_product_type("iPad11,1"), "iPad11");
        assert_eq!(family_from_product_type("Watch7,1"), "Watch7");
    }
}
