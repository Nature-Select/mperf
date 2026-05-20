//! DTX `processcontrol` launch — the iOS equivalent of `monkey -p`.
//!
//! Opens its own CoreDeviceProxy + RSD + dtservicehub channel (mirrors
//! `pid_resolver.rs:60-91`), calls
//! `ProcessControlClient::launchSuspendedProcessWithDevicePath...`, and
//! drops the whole stack on return. Each invocation is independent of
//! the CPU sampler's long-lived DTX channel — multiple
//! `RemoteServerClient`s can coexist on the same device; the lockdown-
//! holding-CoreDeviceProxy issue (battery / temperature returning
//! empty Dict on iOS 17+) only applies to lockdown calls run during a
//! DTX session, not to a second DTX session opened in parallel.

use crate::connect;
use anyhow::{anyhow, Context, Result};
use idevice::{
    core_device_proxy::CoreDeviceProxy,
    dvt::{process_control::ProcessControlClient, remote_server::RemoteServerClient},
    rsd::RsdHandshake,
    IdeviceService, ReadWrite,
};

/// Launch `bundle_id` via Instruments processcontrol. Returns the new
/// PID on success. Convenience wrapper that preserves running state
/// (kill_existing=false) — used by start_recording so the user's
/// foregrounded app doesn't get killed if they already have it open.
pub async fn launch_app(udid: &str, bundle_id: &str) -> Result<u64> {
    launch_app_with_options(udid, bundle_id, false).await
}

/// Launch `bundle_id`, choosing whether to terminate any existing
/// instance first. `kill_existing=true` gives a clean cold-start
/// measurement; `false` brings a backgrounded app forward without
/// re-initialising it (hot-start path).
///
/// `start_suspended=false`: the app comes up immediately so the
/// samplers' first ticks find a PID instead of emitting zeros.
///
/// Setup overhead is ~1–2 s (one fresh CoreDeviceProxy + RSD + DTX
/// channel build). Acceptable for a "click record" gesture; not
/// something to call in a hot loop.
pub async fn launch_app_with_options(
    udid: &str,
    bundle_id: &str,
    kill_existing: bool,
) -> Result<u64> {
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

    // Same HRTB workaround as cpu.rs / pid_resolver.rs — resolve
    // dtservicehub port directly instead of going through RsdService.
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

    let mut pc = ProcessControlClient::new(&mut remote)
        .await
        .context("ProcessControlClient::new")?;
    let pid = pc
        .launch_app(
            bundle_id.to_string(),
            None,  // env_vars
            None,  // arguments
            false, // start_suspended
            kill_existing,
        )
        .await
        .with_context(|| format!("launch_app({bundle_id})"))?;
    Ok(pid)
}
