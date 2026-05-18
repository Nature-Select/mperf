//! List installed apps on an iOS device via the `installation_proxy` service.

use crate::connect;
use anyhow::{Context, Result};
use idevice::{services::installation_proxy::InstallationProxyClient, IdeviceService};
use serde::Serialize;

/// One installed app, surfaced to the UI.
#[derive(Debug, Clone, Serialize)]
pub struct AppInfo {
    /// Bundle identifier (e.g. "com.tyrell.eve").
    pub id: String,
    /// User-visible label. Falls back to `id` when no name is provided.
    pub label: String,
}

pub async fn list_apps(udid: &str) -> Result<Vec<AppInfo>> {
    let provider = connect::provider_for(udid)
        .await
        .context("provider_for")?;
    let mut client = InstallationProxyClient::connect(&*provider)
        .await
        .context("InstallationProxyClient::connect")?;

    // "User" = third-party apps only. Excludes system apps which would
    // bloat the picker similar to the Android `pm list packages -3` filter.
    let map = client
        .get_apps(Some("User"), None)
        .await
        .context("get_apps")?;

    let mut out: Vec<AppInfo> = map
        .into_iter()
        .map(|(bundle, value)| {
            let label = value
                .as_dictionary()
                .and_then(|d| {
                    d.get("CFBundleDisplayName")
                        .or_else(|| d.get("CFBundleName"))
                        .and_then(|v| v.as_string())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| bundle.clone());
            AppInfo { id: bundle, label }
        })
        .collect();
    out.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    Ok(out)
}
