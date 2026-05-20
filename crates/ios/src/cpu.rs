//! iOS CPU + per-app memory sampler.
//!
//! sysmontap returns both `system_cpu_usage` (we use for total CPU%) and
//! a `processes` dict keyed by PID. If a `target_pkg` is set, we resolve
//! its bundle id to the binary executable name via
//! `installation_proxy.get_apps` once at session start, then look up that
//! process in every sysmontap sample to emit `MemAppPssBytes`.
//!
//! All RSD / tunnel / DTX setup is inlined into the async-stream block so the
//! borrowed `RemoteServerClient` and the owned `AdapterHandle` live together
//! for the session.

use crate::connect;
use crate::sysmontap_raw::{SysmontapConfig, SysmontapRaw};
use async_stream::stream;
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use idevice::{
    core_device_proxy::CoreDeviceProxy,
    dvt::{device_info::DeviceInfoClient, remote_server::RemoteServerClient},
    rsd::RsdHandshake,
    services::installation_proxy::InstallationProxyClient,
    IdeviceError, IdeviceService, ReadWrite,
};
use mperf_schema::{LabelKey, MetricKind, Sample, Sampler, SamplerCtx, SamplerError};
use smallvec::smallvec;

const MIN_INTERVAL_MS: u32 = 200;
pub const DEFAULT_INTERVAL_MS: u32 = 1000;

pub struct CpuSampler {
    udid: String,
    /// Bundle id to track per-app CPU% and memory for. Mandatory —
    /// PerfDog-style explicit selection; no foreground heuristic.
    target_pkg: String,
    /// sysmontap channel cadence (ms). Lower bound clamped to
    /// `MIN_INTERVAL_MS` — DTX jitter dominates below ~200ms anyway.
    interval_ms: u32,
}

impl CpuSampler {
    pub fn new(
        udid: impl Into<String>,
        target_pkg: impl Into<String>,
        interval_ms: u32,
    ) -> Self {
        Self {
            udid: udid.into(),
            target_pkg: target_pkg.into(),
            interval_ms: interval_ms.max(MIN_INTERVAL_MS),
        }
    }
}

#[async_trait]
impl Sampler for CpuSampler {
    fn name(&self) -> &'static str {
        "ios.cpu"
    }

    fn target_hz(&self) -> f32 {
        1000.0 / self.interval_ms as f32
    }

    async fn start(
        &mut self,
        ctx: SamplerCtx,
    ) -> Result<BoxStream<'static, Result<Sample, SamplerError>>, SamplerError> {
        // Early reachability check; the heavy setup happens inside the stream
        // so that borrow lifetimes stay in one scope.
        let _provider = connect::provider_for(&self.udid).await.map_err(map_setup)?;

        let udid = self.udid.clone();
        let target_pkg = self.target_pkg.clone();
        let interval_ms = self.interval_ms;
        // Resolve bundle_id → CFBundleExecutable up-front. One install_proxy
        // round-trip; cached for the session. If the app got uninstalled
        // between selection and Start we still proceed with system-only
        // metrics so CPU/per-core/system-mem keep recording.
        let target_exec = match resolve_bundle_to_exec(&udid, &target_pkg).await {
            Ok(name) => {
                tracing::info!(sampler = "ios.cpu", pkg = %target_pkg, exec = %name, "resolved process name");
                Some(name)
            }
            Err(e) => {
                tracing::warn!(sampler = "ios.cpu", pkg = %target_pkg, error = %e, "could not resolve bundle id → exec");
                None
            }
        };
        let clock = ctx.clock.clone();

        // One-time diagnostic dump (first sample with processes) so we can
        // see the actual key/name schema sysmontap returns on this iOS
        // version — used when memory matching fails.
        let mut diagnostic_dumped = false;
        let mut system_diag_dumped = false;
        let s = stream! {
            // ---- usbmuxd → lockdown → CoreDeviceProxy ----
            let provider = match connect::provider_for(&udid).await {
                Ok(p) => p,
                Err(e) => { yield Err(map_setup(e)); return; }
            };
            let proxy = match CoreDeviceProxy::connect(&*provider).await {
                Ok(p) => p,
                Err(e) => { yield Err(map_ide("CoreDeviceProxy::connect", e)); return; }
            };
            let rsd_port = proxy.tunnel_info().server_rsd_port;

            // ---- software tunnel + RSD handshake ----
            let adapter = match proxy.create_software_tunnel() {
                Ok(a) => a,
                Err(e) => { yield Err(map_ide("create_software_tunnel", e)); return; }
            };
            let mut handle = adapter.to_async_handle();
            let rsd_stream = match handle.connect(rsd_port).await {
                Ok(s) => s,
                Err(e) => {
                    yield Err(SamplerError::Fatal(anyhow::anyhow!("adapter connect rsd: {e}")));
                    return;
                }
            };
            let handshake = match RsdHandshake::new(rsd_stream).await {
                Ok(h) => h,
                Err(e) => { yield Err(map_ide("RsdHandshake::new", e)); return; }
            };

            // ---- DTX RemoteServerClient over the tunneled RSD ----
            // We bypass the `RsdService` trait entirely (it triggers an HRTB
            // inference error inside async-stream generators) and resolve
            // the dtservicehub service port + connect manually.
            const DTSERVICEHUB: &str = "com.apple.instruments.dtservicehub";
            let dvt_port = match handshake.services.get(DTSERVICEHUB) {
                Some(svc) => svc.port,
                None => {
                    yield Err(SamplerError::Fatal(anyhow::anyhow!(
                        "RSD service '{DTSERVICEHUB}' not advertised by device"
                    )));
                    return;
                }
            };
            let dvt_stream = match handle.connect(dvt_port).await {
                Ok(s) => s,
                Err(e) => {
                    yield Err(SamplerError::Fatal(anyhow::anyhow!(
                        "connect to dvt port {dvt_port}: {e}"
                    )));
                    return;
                }
            };
            let boxed: Box<dyn ReadWrite> = Box::new(dvt_stream);
            let mut remote: RemoteServerClient<Box<dyn ReadWrite>> =
                RemoteServerClient::new(boxed);

            // ---- Fetch attribute lists + hardware info, then drop info to release the borrow ----
            let (proc_attrs, sys_attrs, cpu_count) = {
                let mut info = match DeviceInfoClient::new(&mut remote).await {
                    Ok(c) => c,
                    Err(e) => { yield Err(map_ide("DeviceInfoClient::new", e)); return; }
                };
                let proc_attrs = match info.sysmon_process_attributes().await {
                    Ok(a) => a,
                    Err(e) => { yield Err(map_ide("sysmon_process_attributes", e)); return; }
                };
                let sys_attrs = match info.sysmon_system_attributes().await {
                    Ok(a) => a,
                    Err(e) => { yield Err(map_ide("sysmon_system_attributes", e)); return; }
                };
                // CPUCount isn't surfaced in sample.system_cpu_usage on iOS
                // 26 — query it once via hardwareInformation. Log all keys
                // the first time so we know the actual schema.
                let hw = match info.hardware_information().await {
                    Ok(d) => d,
                    Err(e) => { yield Err(map_ide("hardware_information", e)); return; }
                };
                let hw_keys: Vec<&str> = hw.keys().map(|k| k.as_str()).collect();
                tracing::info!(sampler = "ios.cpu", hardware_keys = ?hw_keys, "hardwareInformation keys");
                let cpu_count = extract_cpu_count(&hw).unwrap_or(0);
                tracing::info!(sampler = "ios.cpu", cpu_count, "resolved CPU count");
                (proc_attrs, sys_attrs, cpu_count)
            };
            // sysmontap returns each process row as a POSITIONAL Array
            // matching `proc_attrs` order, not a Dictionary keyed by name.
            // Resolve the indices of the fields we care about ahead of time.
            //
            // For memory we walk MEM_KEYS in *preference* order (most-
            // accurate first) so we pick `physFootprint` if available —
            // that's Apple's "Memory" reading shown in Xcode and what
            // PerfDog reports. Falls back to RSS-style measures only when
            // the device doesn't expose physFootprint.
            let name_idx = NAME_KEYS
                .iter()
                .find_map(|k| proc_attrs.iter().position(|a| a == k));
            let (mem_idx, mem_key) = MEM_KEYS
                .iter()
                .find_map(|k| {
                    proc_attrs
                        .iter()
                        .position(|a| a == k)
                        .map(|idx| (Some(idx), Some(*k)))
                })
                .unwrap_or((None, None));
            // Per-app CPU%: sysmontap exposes a per-process `cpuUsage`
            // attribute. Semantics: fraction of one core, range typically
            // [0.0, cpu_count] (e.g. 1.5 = 150% = one and a half cores).
            // We normalize to total system capacity (divide by cpu_count and
            // ×100) so it's comparable to CpuTotalPct and to Android's
            // `cpu_app_pct` semantics ("fraction of total CPU").
            let cpu_idx = CPU_KEYS
                .iter()
                .find_map(|k| proc_attrs.iter().position(|a| a == k));
            tracing::info!(
                sampler = "ios.cpu",
                ?name_idx,
                ?mem_idx,
                mem_key = ?mem_key,
                ?cpu_idx,
                "resolved process attribute indices (memory shown is whichever \
                 key is logged; physFootprint matches Xcode's Memory reading)"
            );
            if target_exec.is_some() && (name_idx.is_none() || mem_idx.is_none()) {
                tracing::warn!(
                    sampler = "ios.cpu",
                    all_proc_attrs = ?proc_attrs,
                    "could not find name and/or memory attribute index; per-app memory will not work"
                );
            }
            // Dump the full sysAttrs list once. iOS varies by version
            // which keys it offers; if MemSystemUsedBytes never gets
            // emitted, this is the first place to look.
            tracing::info!(
                sampler = "ios.cpu",
                sys_attrs = ?sys_attrs,
                "device-advertised sysAttrs (these are what we request via setConfig)"
            );
            if cpu_count == 0 {
                yield Err(SamplerError::Fatal(anyhow::anyhow!(
                    "could not determine CPU count from hardwareInformation; cannot normalize CPU_TotalLoad"
                )));
                return;
            }
            tracing::info!(
                sampler = "ios.cpu",
                proc_attrs = proc_attrs.len(),
                sys_attrs = sys_attrs.len(),
                cpu_count,
                "sysmontap attributes fetched"
            );

            // ---- Sysmontap configure + start ----
            // We use a local SysmontapRaw that bypasses idevice's
            // SysmontapClient. The upstream version drops Processes rows
            // when iOS pushes CPU + Processes as separate rows in the same
            // message; SysmontapRaw merges them.
            tracing::info!(sampler = "ios.cpu", "creating SysmontapRaw");
            let mut sysmontap = match SysmontapRaw::new(&mut remote).await {
                Ok(c) => c,
                Err(e) => { yield Err(map_ide("SysmontapRaw::new", e)); return; }
            };
            tracing::info!(sampler = "ios.cpu", "set_config");
            if let Err(e) = sysmontap
                .set_config(&SysmontapConfig {
                    interval_ms,
                    process_attributes: proc_attrs,
                    system_attributes: sys_attrs,
                })
                .await
            {
                yield Err(map_ide("set_config", e));
                return;
            }
            tracing::info!(sampler = "ios.cpu", "starting sysmontap");
            if let Err(e) = sysmontap.start().await {
                yield Err(map_ide("sysmontap.start", e));
                return;
            }
            // Capture the raw plist of the first real data sample so we can
            // dump it once and inspect what iOS actually sends in this
            // version. Mem schema has changed enough across versions that
            // a real one-shot dump is more useful than a fixed parser.
            sysmontap.capture_raw_once();
            tracing::info!(sampler = "ios.cpu", "waiting for first sample");

            // ---- Sample loop ----
            // iOS only emits the System block on some pushes (not every
            // tick), so we cache the last-computed system memory and
            // re-emit each second to keep the chart populated, matching
            // Android's once-per-second cadence.
            let mut last_sys_used_bytes: Option<f64> = None;
            let mut received = 0u64;
            // Strike counter for the "procs row was present but our target
            // exec wasn't in it" case. sysmontap occasionally pushes a
            // procs frame that's missing entries (idevice 0.1.61 quirk
            // we partly worked around with SysmontapRaw, but the residual
            // shows up especially while the screen is locked or the app
            // is suspended in the background). A single missing frame
            // shouldn't trip "app killed → emit zeros" because then the
            // chart drops one tick and recovers — that flicker is what
            // the user reported as "偏偏一个点零". Require N consecutive
            // missing frames before we treat the app as truly gone.
            const MISSING_STRIKES_TO_ZERO: u32 = 3;
            let mut missing_strikes: u32 = 0;
            // Consecutive transient sysmontap errors. One USB blip used
            // to kill the iOS CPU sampler permanently (yield + return);
            // now we tolerate up to N in a row before giving up so a
            // momentary cable wiggle doesn't flatline the chart for
            // the rest of the session. Reset to 0 on every successful
            // next_sample.
            const MAX_TRANSIENT_ERRORS: u32 = 5;
            let mut transient_errors: u32 = 0;
            loop {
                match sysmontap.next_sample().await {
                    Ok(sample) => {
                        transient_errors = 0;
                        received += 1;
                        if received <= 3 || received % 30 == 0 {
                            let has_cpu = sample.system_cpu_usage.is_some();
                            let has_procs = sample.processes.is_some();
                            let has_sys = sample.system.is_some();
                            let per_cpu_n = sample.per_cpu_usage.as_ref().map(|v| v.len());
                            let sys_attrs_echo =
                                sample.system_attributes.as_ref().map(|v| v.len());
                            tracing::info!(
                                sampler = "ios.cpu",
                                received,
                                has_cpu,
                                has_procs,
                                has_sys,
                                ?per_cpu_n,
                                ?sys_attrs_echo,
                                "sample received"
                            );
                            if !sample.unknown_keys.is_empty() {
                                tracing::info!(
                                    sampler = "ios.cpu",
                                    unknown_keys = ?sample.unknown_keys,
                                    "unrecognized top-level sysmontap keys (filtered for proto markers)"
                                );
                            }
                        }
                        // One-shot raw dump of the first push so we can
                        // see exactly which structural keys, which Dict
                        // shapes, and which nested attrs iOS sends.
                        if let Some(rows) = sample.raw_rows.as_ref() {
                            for (i, row) in rows.iter().enumerate() {
                                let preview = format_row_preview(row);
                                tracing::info!(
                                    sampler = "ios.cpu",
                                    row_idx = i,
                                    row = %preview,
                                    "raw first-sample row"
                                );
                            }
                        }
                        let ts = clock.now_us();
                        if let Some(cpu_map) = sample.system_cpu_usage.as_ref() {
                            if let Some(load) =
                                cpu_map.get("CPU_TotalLoad").and_then(value_as_f64)
                            {
                                yield Ok(Sample {
                                    ts_us: ts,
                                    device_ts_us: None,
                                    kind: MetricKind::CpuTotalPct,
                                    value: (load / cpu_count as f64).clamp(0.0, 100.0),
                                    labels: smallvec![],
                                });
                            }
                        }
                        // Per-core CPU%. iOS hands this out as a parallel
                        // `PerCPUUsage` array, one entry per logical CPU.
                        // Each entry's `CPU_TotalLoad` is already 0-100 for
                        // that core; emit one row per index with a
                        // `core_idx` label, matching Android's shape.
                        if let Some(per_cpu) = sample.per_cpu_usage.as_ref() {
                            for (idx, entry) in per_cpu.iter().enumerate() {
                                if let Some(dict) = entry.as_dictionary() {
                                    if let Some(v) = dict
                                        .get("CPU_TotalLoad")
                                        .and_then(value_as_f64)
                                    {
                                        yield Ok(Sample {
                                            ts_us: ts,
                                            device_ts_us: None,
                                            kind: MetricKind::CpuCorePct,
                                            value: v.clamp(0.0, 100.0),
                                            labels: smallvec![(
                                                LabelKey::CoreIdx,
                                                idx.to_string(),
                                            )],
                                        });
                                    }
                                }
                            }
                        }
                        // System-wide memory used. Dump the merged System
                        // dict keys once for diagnostics.
                        if !system_diag_dumped && sample.system.is_some() {
                            system_diag_dumped = true;
                            let sys_keys: Vec<&str> = sample
                                .system
                                .as_ref()
                                .map(|d| d.keys().map(|k| k.as_str()).collect())
                                .unwrap_or_default();
                            tracing::info!(
                                sampler = "ios.cpu",
                                sys_keys = ?sys_keys,
                                "first non-empty System dict keys"
                            );
                        }
                        if let Some(sys) = sample.system.as_ref() {
                            match system_used_bytes(sys) {
                                Some(used) => {
                                    if last_sys_used_bytes.is_none() {
                                        let phys = sys
                                            .get("physMemSize")
                                            .and_then(value_as_f64);
                                        let free = sys
                                            .get("vmFreeCount")
                                            .and_then(value_as_f64);
                                        let active = sys
                                            .get("vmActiveCount")
                                            .and_then(value_as_f64);
                                        let wired = sys
                                            .get("vmWireCount")
                                            .and_then(value_as_f64);
                                        let inactive = sys
                                            .get("vmInactiveCount")
                                            .and_then(value_as_f64);
                                        let compressor = sys
                                            .get("vmCompressorPageCount")
                                            .and_then(value_as_f64);
                                        tracing::info!(
                                            sampler = "ios.cpu",
                                            used_bytes = used,
                                            used_gb = used / 1024.0 / 1024.0 / 1024.0,
                                            phys_bytes = ?phys,
                                            vm_free_pages = ?free,
                                            vm_active_pages = ?active,
                                            vm_wire_pages = ?wired,
                                            vm_inactive_pages = ?inactive,
                                            vm_compressor_pages = ?compressor,
                                            "first system memory reading"
                                        );
                                    }
                                    last_sys_used_bytes = Some(used);
                                }
                                None => {
                                    if received <= 3 {
                                        let keys: Vec<&str> = sys
                                            .keys()
                                            .map(|k| k.as_str())
                                            .collect();
                                        tracing::warn!(
                                            sampler = "ios.cpu",
                                            sys_keys = ?keys,
                                            "system_used_bytes returned None — neither vmUsed nor (page counts + vmPageSize) keys present; MemSystemUsedBytes will not be emitted"
                                        );
                                    }
                                }
                            }
                        }
                        // Emit cached system memory every tick. iOS only
                        // pushes the System block sporadically; this keeps
                        // the chart at the same cadence as CpuTotalPct.
                        if let Some(used) = last_sys_used_bytes {
                            yield Ok(Sample {
                                ts_us: ts,
                                device_ts_us: None,
                                kind: MetricKind::MemSystemUsedBytes,
                                value: used,
                                labels: smallvec![],
                            });
                        }
                        // Per-app memory: only when an explicit target is
                        // set. iOS has no reliable foreground-app API
                        // accessible to third-party tools (the "highest
                        // CPU" heuristic would misattribute memory to
                        // background daemons or music players), so we
                        // require the user to pick an app.
                        // First sample with non-empty processes: dump the
                        // per-PID value type and a slice of names so we can
                        // diagnose any future schema change.
                        if let Some(procs) = sample.processes.as_ref() {
                            if !diagnostic_dumped && !procs.is_empty() {
                                diagnostic_dumped = true;
                                let (_pid, first) = procs.iter().next().unwrap();
                                let kind = describe_value_kind(first);
                                let entry_len = first.as_array().map(|a| a.len());
                                tracing::info!(
                                    sampler = "ios.cpu",
                                    proc_count = procs.len(),
                                    first_value_kind = %kind,
                                    first_array_len = ?entry_len,
                                    "first sysmontap process entry schema"
                                );
                                let mut names: Vec<String> = procs
                                    .values()
                                    .filter_map(|v| read_proc_name(v, name_idx))
                                    .collect();
                                names.sort();
                                tracing::info!(
                                    sampler = "ios.cpu",
                                    target_exec = ?target_exec,
                                    candidate_count = names.len(),
                                    candidates = ?names,
                                    "first-sample process candidates"
                                );
                            }
                        }
                        if let (Some(target), Some(procs)) =
                            (target_exec.as_deref(), sample.processes.as_ref())
                        {
                            match find_proc_metrics(
                                procs, target, name_idx, mem_idx, cpu_idx,
                            ) {
                                Some((bytes, cpu_frac)) => {
                                    missing_strikes = 0;
                                    if let Some(bytes) = bytes {
                                        yield Ok(Sample {
                                            ts_us: ts,
                                            device_ts_us: None,
                                            kind: MetricKind::MemAppPssBytes,
                                            value: bytes,
                                            labels: smallvec![],
                                        });
                                    }
                                    if let Some(frac) = cpu_frac {
                                        // `cpuUsage` from sysmontap on modern
                                        // iOS is *percent* per core (0..100 ×
                                        // core, so a 6-core saturated process
                                        // reads ~600), NOT a 0..cpu_count
                                        // fraction. Divide by cpu_count to
                                        // normalize to "% of total system",
                                        // matching Android's CpuAppPct.
                                        let pct = (frac / cpu_count as f64).clamp(0.0, 100.0);
                                        yield Ok(Sample {
                                            ts_us: ts,
                                            device_ts_us: None,
                                            kind: MetricKind::CpuAppPct,
                                            value: pct,
                                            labels: smallvec![],
                                        });
                                    }
                                }
                                None => {
                                    // Target exec missing from this procs
                                    // frame. Could be (a) sysmontap pushed
                                    // a partial frame (transient — common
                                    // while screen-locked or app suspended)
                                    // or (b) the app really got killed.
                                    // Strike-count to N consecutive misses
                                    // before we commit to zeros so a single
                                    // dropped frame doesn't flicker a 0
                                    // into the chart between two healthy
                                    // ticks. Below threshold: skip this
                                    // tick entirely (no sample → uPlot just
                                    // doesn't get a new point; the line
                                    // visually continues from the previous
                                    // value). Above threshold: mirror
                                    // Android's "No process found" → 0
                                    // behavior on both CPU and memory.
                                    missing_strikes = missing_strikes.saturating_add(1);
                                    // Below threshold: skip this tick — the
                                    // single missing frame is most likely a
                                    // partial sysmontap push, not a real
                                    // kill. uPlot just doesn't get a new
                                    // point and the line visually continues
                                    // from the previous value.
                                    //
                                    // At/above threshold: emit 0 on **every**
                                    // tick going forward (not just the
                                    // threshold-crossing one). The previous
                                    // "emit once" attempt produced a single
                                    // 0 data point at the kill moment and
                                    // then no new points — uPlot's x-axis
                                    // kept advancing but the line stayed
                                    // pinned to that one 0, looking like
                                    // the chart had gone blank rather than
                                    // showing "app is dead, App% = 0%".
                                    // Matches Android's behavior: its mem
                                    // sampler emits 0 every tick after
                                    // `dumpsys meminfo` returns "No process
                                    // found", and its cpu sampler emits 0
                                    // every tick where `pidof` returns
                                    // empty. The chart needs the continuous
                                    // stream of 0s to keep drawing the
                                    // flatline. Counter resets to 0 the
                                    // moment target reappears (the
                                    // `Some(_)` arm above).
                                    if missing_strikes >= MISSING_STRIKES_TO_ZERO {
                                        yield Ok(Sample {
                                            ts_us: ts,
                                            device_ts_us: None,
                                            kind: MetricKind::CpuAppPct,
                                            value: 0.0,
                                            labels: smallvec![],
                                        });
                                        yield Ok(Sample {
                                            ts_us: ts,
                                            device_ts_us: None,
                                            kind: MetricKind::MemAppPssBytes,
                                            value: 0.0,
                                            labels: smallvec![],
                                        });
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        transient_errors += 1;
                        let give_up = transient_errors >= MAX_TRANSIENT_ERRORS;
                        if give_up {
                            // Surface the giving-up event as Fatal so
                            // the scheduler stops dispatching to this
                            // stream instead of treating it as just
                            // another retriable nick.
                            yield Err(SamplerError::Fatal(anyhow::anyhow!(
                                "sysmontap next_sample failed {MAX_TRANSIENT_ERRORS} times in a row: {e}"
                            )));
                            return;
                        }
                        tracing::warn!(
                            sampler = "ios.cpu",
                            transient_errors,
                            error = %e,
                            "transient sysmontap error — continuing"
                        );
                        yield Err(SamplerError::TransientIo(format!(
                            "sysmontap next_sample: {e}"
                        )));
                        continue;
                    }
                }
            }
        };

        Ok(Box::pin(s))
    }
}

fn map_setup(e: anyhow::Error) -> SamplerError {
    let msg = format!("{e:#}");
    let lower = msg.to_ascii_lowercase();
    if lower.contains("not found") || lower.contains("disconnect") {
        SamplerError::DeviceDisconnected(msg)
    } else if lower.contains("trust") || lower.contains("paired") {
        SamplerError::PermissionDenied(msg)
    } else {
        SamplerError::Fatal(e)
    }
}

fn map_ide(what: &'static str, e: IdeviceError) -> SamplerError {
    SamplerError::Fatal(anyhow::anyhow!("{what}: {e}"))
}

fn value_as_f64(v: &plist::Value) -> Option<f64> {
    match v {
        plist::Value::Integer(i) => i.as_signed().map(|x| x as f64),
        plist::Value::Real(r) => Some(*r),
        plist::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Ask installation_proxy for the bundle's `CFBundleExecutable`, which is
/// the on-device process name we'll see inside sysmontap.processes
/// **and** inside syslog's `process[pid]` prefix. Re-exported from
/// `lib.rs` so the log stream (`apps/desktop/src-tauri/src/log_stream.rs`)
/// can reuse it for syslog filtering — iOS syslog is device-wide, the
/// only way to scope it to one app is to match on the exec name we
/// resolve here.
pub async fn resolve_bundle_to_exec(udid: &str, bundle_id: &str) -> anyhow::Result<String> {
    use anyhow::Context;
    let provider = crate::connect::provider_for(udid).await?;
    let mut client = InstallationProxyClient::connect(&*provider)
        .await
        .context("installation_proxy connect")?;
    let map = client
        .get_apps(Some("Any"), Some(vec![bundle_id.to_string()]))
        .await
        .context("get_apps")?;
    let value = map
        .get(bundle_id)
        .ok_or_else(|| anyhow::anyhow!("app {bundle_id} not installed"))?;
    let exec = value
        .as_dictionary()
        .and_then(|d| d.get("CFBundleExecutable"))
        .and_then(|v| v.as_string())
        .ok_or_else(|| anyhow::anyhow!("no CFBundleExecutable for {bundle_id}"))?;
    Ok(exec.to_string())
}

const NAME_KEYS: &[&str] = &["name", "processName"];
const MEM_KEYS: &[&str] = &[
    "physFootprint",
    "residentMemory",
    "memResidentSize",
    "privateMemory",
];
/// Per-process CPU usage attribute. iOS sysmontap exposes this as
/// `cpuUsage` (a fraction where 1.0 = one core fully busy).
const CPU_KEYS: &[&str] = &["cpuUsage"];

/// Top-level System keys that already report bytes.
const SYS_USED_BYTES_KEYS: &[&str] = &["vmUsed"];

/// Page size key. iOS sysAttrs does *not* always advertise `vmPageSize`
/// (iPhone 17 / iOS 26 does not); we fall back to a hardcoded 16384 (16KB)
/// which is the page size on every 64-bit iPhone since A7.
const SYS_PAGE_SIZE_KEY: &str = "vmPageSize";
const IOS_DEFAULT_PAGE_SIZE: f64 = 16384.0;

/// Total physical memory in bytes.
const SYS_PHYS_MEM_KEYS: &[&str] = &["physMemSize"];

/// Page-count keys for *free* memory. We compute used = physMemSize − free.
const SYS_FREE_PAGE_KEYS: &[&str] = &["vmFreeCount", "vmFree"];

/// Page-count keys we sum to estimate used memory. Wired + active + inactive
/// + compressor pages roughly matches the "Used" figure Apple's Activity
/// Monitor reports. We use whichever subset the device exposes.
const SYS_USED_PAGE_KEYS: &[&str] = &[
    "vmWireCount",
    "vmActiveCount",
    "vmInactiveCount",
    "vmCompressorPageCount",
];

/// Read the process name from a per-PID sysmontap value. Handles both the
/// positional-Array layout (each value is an Array matching `proc_attrs`
/// order — what iOS actually sends) and the named-Dictionary fallback.
fn read_proc_name(value: &plist::Value, name_idx: Option<usize>) -> Option<String> {
    if let Some(arr) = value.as_array() {
        let idx = name_idx?;
        arr.get(idx).and_then(|v| v.as_string()).map(|s| s.to_string())
    } else if let Some(dict) = value.as_dictionary() {
        NAME_KEYS
            .iter()
            .find_map(|k| dict.get(*k).and_then(|v| v.as_string()))
            .map(|s| s.to_string())
    } else {
        None
    }
}

fn read_proc_memory(value: &plist::Value, mem_idx: Option<usize>) -> Option<f64> {
    if let Some(arr) = value.as_array() {
        let idx = mem_idx?;
        arr.get(idx).and_then(value_as_f64)
    } else if let Some(dict) = value.as_dictionary() {
        MEM_KEYS
            .iter()
            .find_map(|k| dict.get(*k).and_then(value_as_f64))
    } else {
        None
    }
}

fn read_proc_cpu(value: &plist::Value, cpu_idx: Option<usize>) -> Option<f64> {
    if let Some(arr) = value.as_array() {
        let idx = cpu_idx?;
        arr.get(idx).and_then(value_as_f64)
    } else if let Some(dict) = value.as_dictionary() {
        CPU_KEYS
            .iter()
            .find_map(|k| dict.get(*k).and_then(value_as_f64))
    } else {
        None
    }
}

fn describe_value_kind(v: &plist::Value) -> &'static str {
    match v {
        plist::Value::Array(_) => "Array",
        plist::Value::Dictionary(_) => "Dictionary",
        plist::Value::String(_) => "String",
        plist::Value::Integer(_) => "Integer",
        plist::Value::Real(_) => "Real",
        plist::Value::Boolean(_) => "Boolean",
        plist::Value::Data(_) => "Data",
        plist::Value::Date(_) => "Date",
        _ => "Unknown",
    }
}

/// Look up a specific process by CFBundleExecutable in the sysmontap
/// processes dict and return its (memory bytes, cpu fraction) pair. Either
/// component may be `None` if the corresponding attribute index wasn't
/// resolved.
///
/// Match strategy, in order:
/// 1. Exact, case-sensitive equality on the process name.
/// 2. Case-insensitive equality.
/// 3. **Truncation tolerant**: the kernel `p_comm` field is capped at 15
///    chars, so e.g. `"Eve Echoes Mobile"` (17) is reported as
///    `"Eve Echoes Mobi"` (15). We match when the process name is a
///    non-trivial (≥8 char) prefix of the target exec.
fn find_proc_metrics(
    procs: &plist::Dictionary,
    target_exec: &str,
    name_idx: Option<usize>,
    mem_idx: Option<usize>,
    cpu_idx: Option<usize>,
) -> Option<(Option<f64>, Option<f64>)> {
    let extract = |value: &plist::Value| {
        (
            read_proc_memory(value, mem_idx),
            read_proc_cpu(value, cpu_idx),
        )
    };
    // Pass 1: exact.
    for (_pid, value) in procs.iter() {
        if read_proc_name(value, name_idx).as_deref() == Some(target_exec) {
            return Some(extract(value));
        }
    }
    // Pass 2 + 3: case-insensitive equality or truncation prefix.
    let target_lower = target_exec.to_ascii_lowercase();
    for (_pid, value) in procs.iter() {
        let Some(name) = read_proc_name(value, name_idx) else { continue };
        if name.eq_ignore_ascii_case(target_exec) {
            tracing::debug!(target = %target_exec, matched = %name, "ios proc matched by case-insensitive equality");
            return Some(extract(value));
        }
        if name.len() >= 8
            && name.len() < target_exec.len()
            && target_lower.starts_with(&name.to_ascii_lowercase())
        {
            tracing::debug!(target = %target_exec, matched = %name, "ios proc matched by truncation");
            return Some(extract(value));
        }
    }
    None
}

/// Compute system-wide used memory in bytes from a `System` row Dictionary.
///
/// Path priority:
///   1. `vmUsed` (already in bytes — rarely present)
///   2. `physMemSize − vmFreeCount × pageSize` — Activity-Monitor's
///      "Used" reading. Most reliable across iOS versions.
///   3. `(active + wired + inactive + compressor) × pageSize` — fallback
///      sum when path 2's pieces aren't both available.
///
/// **NOT** using `vmUsedCount`: on iOS that key reports total physical
/// pages (≈ physMemSize / pageSize), not currently-used pages — it would
/// pin the chart at the device's full RAM size.
///
/// `pageSize` prefers `vmPageSize` from the device, falls back to a
/// hardcoded 16384 (every 64-bit iPhone uses 16KB pages).
fn system_used_bytes(sys: &plist::Dictionary) -> Option<f64> {
    for k in SYS_USED_BYTES_KEYS {
        if let Some(v) = sys.get(*k).and_then(value_as_f64) {
            if v > 0.0 {
                return Some(v);
            }
        }
    }
    let page_size = sys
        .get(SYS_PAGE_SIZE_KEY)
        .and_then(value_as_f64)
        .filter(|v| *v > 0.0)
        .unwrap_or(IOS_DEFAULT_PAGE_SIZE);
    let phys_total = SYS_PHYS_MEM_KEYS
        .iter()
        .find_map(|k| sys.get(*k).and_then(value_as_f64));
    let free_pages = SYS_FREE_PAGE_KEYS
        .iter()
        .find_map(|k| sys.get(*k).and_then(value_as_f64));
    if let (Some(total), Some(free)) = (phys_total, free_pages) {
        if total > 0.0 && free >= 0.0 {
            let used = total - free * page_size;
            // Sanity check: if used is suspiciously close to total
            // (within 1 page), the "free" key probably has a different
            // semantic on this iOS version — fall through to path 3.
            if used > 0.0 && (total - used) > page_size {
                return Some(used);
            }
        }
    }
    let mut pages: f64 = 0.0;
    let mut matched = false;
    for k in SYS_USED_PAGE_KEYS {
        if let Some(v) = sys.get(*k).and_then(value_as_f64) {
            if v >= 0.0 {
                pages += v;
                matched = true;
            }
        }
    }
    if matched {
        Some(pages * page_size)
    } else {
        None
    }
}

/// Compact diagnostic representation of a plist row. Big dicts get their
/// keys listed; big arrays get their length + first entry shape. Used
/// only for the one-shot first-sample dump.
fn format_row_preview(v: &plist::Value) -> String {
    match v {
        plist::Value::Dictionary(d) => {
            let mut entries: Vec<String> = Vec::with_capacity(d.len());
            for (k, vv) in d.iter() {
                let kind = describe_value_kind(vv);
                let extra = match vv {
                    plist::Value::Array(a) => format!("(len={})", a.len()),
                    plist::Value::Dictionary(dd) => format!("(keys={})", dd.len()),
                    plist::Value::Integer(i) => format!(
                        "={}",
                        i.as_signed()
                            .map(|x| x.to_string())
                            .unwrap_or_else(|| "?".into())
                    ),
                    plist::Value::Real(r) => format!("={r:.3}"),
                    plist::Value::String(s) => {
                        let t: String = s.chars().take(40).collect();
                        format!("={t:?}")
                    }
                    plist::Value::Boolean(b) => format!("={b}"),
                    _ => String::new(),
                };
                entries.push(format!("{k}:{kind}{extra}"));
            }
            format!("{{ {} }}", entries.join(", "))
        }
        plist::Value::Array(a) => {
            let head = a
                .first()
                .map(|v| describe_value_kind(v))
                .unwrap_or("(empty)");
            format!("[len={}, first={}]", a.len(), head)
        }
        _ => describe_value_kind(v).to_string(),
    }
}

/// Hunt for CPU count in the hardwareInformation dictionary.
/// iOS uses several names across versions; we try the most common ones.
fn extract_cpu_count(d: &plist::Dictionary) -> Option<u32> {
    const KEY_CANDIDATES: &[&str] = &[
        "numberOfCpus",
        "Active CPUs",
        "ActiveCPUs",
        "numActiveCPUs",
        "physicalCpuCount",
        "logicalCpuCount",
        "_cpuType",
    ];
    for k in KEY_CANDIDATES {
        if let Some(v) = d.get(*k) {
            if let Some(n) = value_as_f64(v) {
                if n > 0.0 && n < 256.0 {
                    return Some(n as u32);
                }
            }
        }
    }
    None
}
