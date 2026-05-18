//! DTX-based bundle id → PIDs resolver.
//!
//! Background: `os_trace_relay::PidList` (the simpler lockdown call
//! we used initially) only exposes `pid → ProcessName` short names
//! = `CFBundleExecutable`. Flutter projects default to `Runner` for
//! every Xcode target (main + widget + share extension), and native
//! apps often share an executable name across extensions too, so
//! `ProcessName` alone is ambiguous on real devices (a device with
//! two Flutter apps installed has multiple PIDs all named `Runner`).
//!
//! The DTX `deviceinfo.runningProcesses` selector exposes the full
//! executable path of each running process (`realAppName`), and
//! `applicationListing.installedApplicationsMatching:` exposes the
//! install Path of each installed bundle. Joining the two by path
//! prefix gives the precise bundle → PIDs mapping that Xcode
//! Instruments uses internally, and we believe is the same path
//! PerfDog takes (USB-only Log support + "multi-extension subprocess
//! testing" UI strongly suggests it).
//!
//! This is a one-shot probe — open DTX, ask both channels, close.
//! It does not coexist with the CPU sampler's DTX session (each
//! opens its own RemoteServerClient over its own software tunnel).

use crate::connect;
use anyhow::{anyhow, Context, Result};
use idevice::{
    core_device_proxy::CoreDeviceProxy,
    dvt::{
        application_listing::ApplicationListingClient,
        device_info::DeviceInfoClient,
        remote_server::RemoteServerClient,
    },
    rsd::RsdHandshake,
    IdeviceService, ReadWrite,
};
use plist::Value;

/// Result of `resolve_bundle_to_pids`.
#[derive(Debug)]
pub struct BundleResolution {
    /// PIDs of every running process whose executable lives under the
    /// target bundle's install Path. Includes the main app plus any
    /// extension (`<App>.app/PlugIns/<Ext>.appex/<Ext>`) that's
    /// currently running. Empty if the bundle is installed but not
    /// running — caller decides whether that's an error or a "wait
    /// for the user to launch the app" state.
    pub pids: Vec<u32>,
    /// Bundle install path, e.g.
    /// `/var/containers/Bundle/Application/<UUID>/Runner.app`. Used
    /// for diagnostics + logging.
    pub install_path: String,
    /// Full executable path of each matched process, in PID order.
    /// Used in startup logs so the user can see e.g. "Runner +
    /// RunnerWidget + RunnerShareExtension" without trusting opaque
    /// PIDs.
    pub matched_paths: Vec<String>,
}

pub async fn resolve_bundle_to_pids(udid: &str, bundle_id: &str) -> Result<BundleResolution> {
    let provider = connect::provider_for(udid).await.context("provider_for")?;
    let proxy = CoreDeviceProxy::connect(&*provider)
        .await
        .context("CoreDeviceProxy::connect")?;
    let rsd_port = proxy.tunnel_info().server_rsd_port;
    let adapter = proxy
        .create_software_tunnel()
        .context("create_software_tunnel")?;
    let mut handle = adapter.to_async_handle();
    let rsd_stream = handle
        .connect(rsd_port)
        .await
        .map_err(|e| anyhow!("adapter connect rsd: {e}"))?;
    let handshake = RsdHandshake::new(rsd_stream)
        .await
        .context("RsdHandshake::new")?;

    // Same HRTB workaround as cpu.rs — resolve dtservicehub port from
    // the handshake's advertised services manually instead of going
    // through the `RsdService` trait.
    const DTSERVICEHUB: &str = "com.apple.instruments.dtservicehub";
    let dvt_port = handshake
        .services
        .get(DTSERVICEHUB)
        .ok_or_else(|| anyhow!("RSD service '{DTSERVICEHUB}' not advertised by device"))?
        .port;
    let dvt_stream = handle
        .connect(dvt_port)
        .await
        .map_err(|e| anyhow!("connect dvt port {dvt_port}: {e}"))?;
    let boxed: Box<dyn ReadWrite> = Box::new(dvt_stream);
    let mut remote = RemoteServerClient::new(boxed);

    // ---- Step 1: find target bundle's install Path ----
    let install_path = {
        let mut listing = ApplicationListingClient::new(&mut remote)
            .await
            .context("ApplicationListingClient::new")?;
        let apps = listing
            .installed_applications()
            .await
            .context("installed_applications")?;
        // One-shot diagnostic dump: first entry's key set. Apple's
        // field names (CFBundleIdentifier / Path) have been stable for
        // many iOS versions, but logging the actual shape on the first
        // call lets us debug quickly if a future iOS renames anything,
        // and confirms the API actually returned bundle metadata
        // rather than some unexpected shape.
        if let Some(first) = apps.first() {
            let keys: Vec<&String> = first.keys().collect();
            tracing::info!(
                count = apps.len(),
                first_app_keys = ?keys,
                "installed_applications: response shape (one-shot diagnostic)",
            );
        }
        let target = apps.iter().find(|a| {
            a.get("CFBundleIdentifier")
                .and_then(Value::as_string)
                .map(|s| s == bundle_id)
                .unwrap_or(false)
        });
        let target = target.ok_or_else(|| {
            anyhow!(
                "bundle id '{bundle_id}' not found among {} installed apps",
                apps.len()
            )
        })?;
        target
            .get("Path")
            .and_then(Value::as_string)
            .or_else(|| target.get("BundlePath").and_then(Value::as_string))
            .ok_or_else(|| anyhow!("installed app for '{bundle_id}' has no Path field"))?
            .to_string()
    };

    // ---- Step 2: running processes filtered by install Path prefix ----
    let processes = {
        let mut info = DeviceInfoClient::new(&mut remote)
            .await
            .context("DeviceInfoClient::new")?;
        info.running_processes()
            .await
            .context("running_processes")?
    };
    let total_procs = processes.len();
    let mut pids = Vec::new();
    let mut matched_paths = Vec::new();
    for proc in &processes {
        // Extensions sit at `<App>.app/PlugIns/<Ext>.appex/<Ext>`, so
        // a prefix match on the install Path catches them along with
        // the main executable at `<App>.app/<Exec>`.
        if paths_match(&install_path, &proc.real_app_name) {
            pids.push(proc.pid);
            matched_paths.push(proc.real_app_name.clone());
        }
    }

    if pids.is_empty() {
        // One-shot diagnostic of the first few process paths so the
        // next time the install-path schema changes we have something
        // concrete to compare against, instead of just "nothing matched".
        let sample: Vec<&str> = processes
            .iter()
            .map(|p| p.real_app_name.as_str())
            .filter(|p| p.contains("/Application/") || p.contains("/Bundle/"))
            .take(5)
            .collect();
        tracing::warn!(
            bundle_id,
            install_path = %install_path,
            total_running = total_procs,
            ?sample,
            "bundle is installed but no running PID matched — app not launched yet?",
        );
    } else {
        tracing::info!(
            bundle_id,
            install_path = %install_path,
            total_running = total_procs,
            matched_count = pids.len(),
            ?pids,
            ?matched_paths,
            "bundle → PID resolution complete",
        );
    }

    Ok(BundleResolution {
        pids,
        install_path,
        matched_paths,
    })
}

/// Compare an installed bundle's directory against a running process's
/// executable path, tolerating iOS's `/private/var` ↔ `/var` symlink.
///
/// `installed_applications` returns `Path` under `/private/var/...`,
/// but `running_processes.realAppName` resolves through the kernel
/// which hands back `/var/...`. A naive `starts_with` misses every
/// match. Normalize both sides by stripping the optional `/private`
/// prefix before comparing.
fn paths_match(install_dir: &str, proc_path: &str) -> bool {
    let n_install = install_dir.strip_prefix("/private").unwrap_or(install_dir);
    let n_proc = proc_path.strip_prefix("/private").unwrap_or(proc_path);
    n_proc.starts_with(n_install)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_match_handles_private_var_symlink() {
        let install = "/private/var/containers/Bundle/Application/UUID/Runner.app";
        let proc_main = "/var/containers/Bundle/Application/UUID/Runner.app/Runner";
        let proc_ext =
            "/var/containers/Bundle/Application/UUID/Runner.app/PlugIns/Widget.appex/Widget";
        let proc_other = "/var/containers/Bundle/Application/OTHER/Other.app/Other";
        assert!(paths_match(install, proc_main));
        assert!(paths_match(install, proc_ext));
        assert!(!paths_match(install, proc_other));
    }

    #[test]
    fn paths_match_no_private_either_side() {
        let install = "/var/containers/Bundle/Application/UUID/Runner.app";
        let proc_main = "/var/containers/Bundle/Application/UUID/Runner.app/Runner";
        assert!(paths_match(install, proc_main));
    }

    #[test]
    fn paths_match_private_on_proc_side() {
        let install = "/var/containers/Bundle/Application/UUID/Runner.app";
        let proc_main = "/private/var/containers/Bundle/Application/UUID/Runner.app/Runner";
        assert!(paths_match(install, proc_main));
    }
}
