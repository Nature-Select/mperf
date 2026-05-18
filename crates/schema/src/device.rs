//! Device descriptor surfaced to the UI.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Android,
    Ios,
}

/// How the host reaches the device. Same UDID/serial may surface twice
/// (once per transport) — that's intentional: PerfDog convention is that
/// battery testing requires Wi-Fi (USB charges the phone and skews the
/// reading).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Usb,
    Wifi,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub platform: Platform,
    #[serde(default = "default_transport")]
    pub transport: Transport,
    pub state: String,
    pub model: Option<String>,
    /// Whether this device can actually be sampled. iOS over Wi-Fi-only
    /// is visible to usbmuxd but our DTX/sysmontap path needs the USB
    /// CoreDeviceProxy tunnel — so wifi-only iOS devices come back with
    /// `usable=false` so the UI can show them grayed out / Start
    /// disabled. Android is always `true` (USB or Wi-Fi adb both work).
    #[serde(default = "default_true")]
    pub usable: bool,
}

fn default_true() -> bool {
    true
}

fn default_transport() -> Transport {
    Transport::Usb
}
