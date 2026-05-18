//! iOS connection helpers.
//!
//! We expose only the cheap usbmuxd-level lookup here. The heavyweight
//! CoreDeviceProxy + software tunnel + RSD handshake setup is inlined into
//! each sampler's async stream so that the borrowed `RemoteServerClient`
//! and the owned `AdapterHandle` live in the same scope.

use anyhow::{Context, Result};
use idevice::{
    provider::IdeviceProvider,
    usbmuxd::{UsbmuxdAddr, UsbmuxdConnection},
};

pub async fn provider_for(udid: &str) -> Result<Box<dyn IdeviceProvider>> {
    let mut conn = UsbmuxdConnection::default()
        .await
        .context("connect to usbmuxd")?;
    let device = conn
        .get_device(udid)
        .await
        .with_context(|| format!("get_device({udid})"))?;
    let provider = device.to_provider(UsbmuxdAddr::default(), "mperf");
    Ok(Box::new(provider))
}
