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
use mperf_schema::{Device, Platform, Transport};
use serde::Serialize;
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
    // Cache hit?
    {
        let cache = name_cache().lock().unwrap();
        if let Some((name, fetched_at)) = cache.get(udid) {
            if fetched_at.elapsed() < NAME_CACHE_TTL {
                return Ok(Some(name.clone()));
            }
        }
    }
    // Cache miss / stale — query and refresh.
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
    // Fresh usbmuxd connection per call — `UsbmuxdConnection` consumes itself
    // when switching into a per-device session, so we can't share.
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

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub id: String,
    pub platform: String,
    pub model: Option<String>,
    pub manufacturer: Option<String>,
    pub os_version: Option<String>,
    pub build: Option<String>,
    pub extra: Vec<(String, String)>,
}

pub async fn device_info(udid: &str) -> Result<DeviceInfo> {
    // Lockdown queries each take ~150ms over USB. We open three separate
    // lockdown clients concurrently and pipeline the queries; iOS muxes
    // these fine and the total wall time drops from ~600ms to ~250ms.
    let (main_res, battery_res, disk_res) = tokio::join!(
        query_domain(udid, None),
        query_domain(udid, Some("com.apple.mobile.battery")),
        query_domain(udid, Some("com.apple.disk_usage")),
    );
    let dict = main_res?
        .into_dictionary()
        .ok_or_else(|| anyhow::anyhow!("lockdown response not a dictionary"))?;
    let get_str = |k: &str| {
        dict.get(k)
            .and_then(|v| v.as_string())
            .map(|s| s.to_string())
    };
    // Decode the internal product identifier (e.g. "iPhone18,3") to the
    // marketing name ("iPhone 17") when we know the mapping; fall back to
    // the raw id otherwise.
    let product_type = get_str("ProductType");
    let model_decoded = product_type.as_deref().map(|pt| {
        product_type_to_marketing_name(pt)
            .map(String::from)
            .unwrap_or_else(|| pt.to_string())
    });

    let mut info = DeviceInfo {
        id: udid.to_string(),
        platform: "ios".into(),
        model: model_decoded,
        manufacturer: Some("Apple".into()),
        os_version: get_str("ProductVersion"),
        build: get_str("BuildVersion"),
        extra: Vec::new(),
    };
    // The user-set device name belongs in the extra section as "Name".
    if let Some(name) = get_str("DeviceName") {
        info.extra.push(("Name".to_string(), name));
    }
    // QA-relevant: battery, storage. Results were fetched in parallel
    // above; here we just consume them.
    if let Ok(battery_value) = battery_res {
        if let Some(d) = battery_value.into_dictionary() {
            if let Some(line) = format_ios_battery(&d) {
                info.extra.push(("Battery".to_string(), line));
            }
        }
    }
    if let Ok(disk_value) = disk_res {
        if let Some(d) = disk_value.into_dictionary() {
            if let Some(line) = format_ios_disk(&d) {
                info.extra.push(("Storage".to_string(), line));
            }
        }
    }
    // iOS keeps locale in the basic domain under `Locale`. Fall back to
    // `Languages` for older firmwares.
    if let Some(loc) = get_str("Locale").or_else(|| get_str("Languages")) {
        if !loc.is_empty() {
            info.extra.push(("Locale".to_string(), loc));
        }
    }

    for (label, key) in [
        ("Name", ""), // handled above already; placeholder to preserve ordering
        ("Product Type", "ProductType"),
        ("Hardware Model", "HardwareModel"),
        ("CPU Architecture", "CPUArchitecture"),
        ("Chip ID", "ChipID"),
        ("WiFi Address", "WiFiAddress"),
        ("Activation State", "ActivationState"),
    ] {
        if key.is_empty() {
            continue;
        }
        if let Some(v) = get_str(key) {
            if !v.is_empty() {
                info.extra.push((label.to_string(), v));
            }
        }
    }
    Ok(info)
}

/// Open a fresh lockdown session and run a single get_value query.
/// Spawning a new client per query is intentional — we want concurrency.
async fn query_domain(udid: &str, domain: Option<&str>) -> Result<plist::Value> {
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
        .get_value(None, domain)
        .await
        .with_context(|| format!("lockdown get_value(domain={domain:?})"))?;
    Ok(value)
}

fn format_ios_battery(d: &plist::Dictionary) -> Option<String> {
    let cap = d
        .get("BatteryCurrentCapacity")
        .and_then(|v| match v {
            plist::Value::Integer(i) => i.as_signed().map(|x| x as i64),
            plist::Value::Real(r) => Some(*r as i64),
            _ => None,
        });
    let charging = d
        .get("BatteryIsCharging")
        .and_then(|v| v.as_boolean());
    let external = d
        .get("ExternalConnected")
        .and_then(|v| v.as_boolean());
    let cap = cap?;
    let mut s = format!("{cap}%");
    if charging.unwrap_or(false) {
        s.push_str(" (charging)");
    } else if external.unwrap_or(false) {
        s.push_str(" (plugged · not charging)");
    } else {
        s.push_str(" (discharging)");
    }
    Some(s)
}

fn format_ios_disk(d: &plist::Dictionary) -> Option<String> {
    // Common keys: TotalDataCapacity, AmountDataAvailable.
    // (TotalSystemCapacity covers OS partition; we focus on user storage.)
    fn as_u64(v: Option<&plist::Value>) -> Option<u64> {
        v.and_then(|v| match v {
            plist::Value::Integer(i) => i.as_unsigned(),
            plist::Value::Real(r) => Some(*r as u64),
            _ => None,
        })
    }
    let total = as_u64(d.get("TotalDataCapacity"))?;
    let avail = as_u64(d.get("AmountDataAvailable")).unwrap_or(0);
    let used = total.saturating_sub(avail);
    Some(format!(
        "{} / {} (free {})",
        format_bytes_ios(used),
        format_bytes_ios(total),
        format_bytes_ios(avail)
    ))
}

fn format_bytes_ios(bytes: u64) -> String {
    let gb = bytes as f64 / 1024.0 / 1024.0 / 1024.0;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        let mb = bytes as f64 / 1024.0 / 1024.0;
        format!("{mb:.0} MB")
    }
}

/// Translate an iOS `ProductType` identifier into the human-readable
/// marketing name. Returns `None` for ids we don't know yet.
///
/// This table only needs to grow as new devices appear; the current set
/// covers iPhones from iPhone 8 onwards (Apple's identifiers prior to that
/// are unlikely to show up in modern testing).
fn product_type_to_marketing_name(pt: &str) -> Option<&'static str> {
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
