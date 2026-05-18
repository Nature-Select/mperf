//! iOS Graphics sampler — emits Core Animation FPS.
//!
//! Uses the `com.apple.instruments.server.services.graphics.opengl` DTX
//! channel via idevice's `GraphicsClient`. The device pushes one sample
//! every `interval` seconds; we forward `CoreAnimationFramesPerSecond`
//! as `Fps`.
//!
//! Lifetime / setup pattern mirrors `cpu.rs`:
//! * The whole tunnel chain (CoreDeviceProxy → software tunnel → RSD →
//!   DTX RemoteServerClient → GraphicsClient) is created **inside** the
//!   async-stream so borrows don't cross function boundaries (we hit a
//!   HRTB inference bug earlier when we tried).
//! * The RSD `dtservicehub` service port is resolved manually rather
//!   than via the `RsdService` trait, because the trait method triggers
//!   the same HRTB issue.
//!
//! GPU utilization (`Tiler %`, `Renderer %`, `Device %`) IS in the raw
//! plist that the device pushes. `idevice 0.1.61`'s `GraphicsSample`
//! drops them; we read the channel directly via
//! [`crate::graphics_raw::GraphicsRaw`] and emit the full triplet
//! alongside FPS — the same set PerfDog shows.

use crate::connect;
use crate::graphics_raw::GraphicsRaw;
use async_stream::stream;
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use idevice::{
    core_device_proxy::CoreDeviceProxy,
    dvt::remote_server::RemoteServerClient,
    rsd::RsdHandshake,
    IdeviceError, IdeviceService, ReadWrite,
};
use mperf_schema::{MetricKind, Sample, Sampler, SamplerCtx, SamplerError};
use smallvec::smallvec;

const SAMPLE_INTERVAL_SEC: f64 = 1.0;

pub struct GraphicsSampler {
    udid: String,
}

impl GraphicsSampler {
    pub fn new(udid: impl Into<String>) -> Self {
        Self { udid: udid.into() }
    }
}

#[async_trait]
impl Sampler for GraphicsSampler {
    fn name(&self) -> &'static str {
        "ios.graphics"
    }

    fn target_hz(&self) -> f32 {
        1.0
    }

    async fn start(
        &mut self,
        ctx: SamplerCtx,
    ) -> Result<BoxStream<'static, Result<Sample, SamplerError>>, SamplerError> {
        // Cheap reachability check; full setup happens inside the stream.
        let _provider = connect::provider_for(&self.udid).await.map_err(map_setup)?;

        let udid = self.udid.clone();
        let clock = ctx.clock.clone();

        let s = stream! {
            let provider = match connect::provider_for(&udid).await {
                Ok(p) => p,
                Err(e) => { yield Err(map_setup(e)); return; }
            };
            let proxy = match CoreDeviceProxy::connect(&*provider).await {
                Ok(p) => p,
                Err(e) => { yield Err(map_ide("CoreDeviceProxy::connect", e)); return; }
            };
            let rsd_port = proxy.tunnel_info().server_rsd_port;
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

            // Same manual port resolution as cpu.rs.
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

            let mut gfx = match GraphicsRaw::new(&mut remote).await {
                Ok(c) => c,
                Err(e) => { yield Err(map_ide("GraphicsRaw::new", e)); return; }
            };
            if let Err(e) = gfx.start_sampling(SAMPLE_INTERVAL_SEC).await {
                yield Err(map_ide("start_sampling", e));
                return;
            }
            tracing::info!(
                sampler = "ios.graphics",
                interval_sec = SAMPLE_INTERVAL_SEC,
                "graphics sampling started"
            );

            let mut diag_dumped = false;
            // Tolerate up to N consecutive transient DTX read errors
            // before giving up — a single USB blip used to kill the
            // sampler permanently. Reset on every successful read.
            const MAX_TRANSIENT_ERRORS: u32 = 5;
            let mut transient_errors: u32 = 0;
            loop {
                match gfx.next_sample().await {
                    Ok(sample) => {
                        transient_errors = 0;
                        if !diag_dumped {
                            diag_dumped = true;
                            tracing::info!(
                                sampler = "ios.graphics",
                                keys = ?sample.all_keys,
                                "first graphics push — schema dump"
                            );
                        }
                        let ts = clock.now_us();
                        if let Some(fps) = sample.fps {
                            yield Ok(Sample {
                                ts_us: ts, device_ts_us: None,
                                kind: MetricKind::Fps,
                                value: fps,
                                labels: smallvec![],
                            });
                        }
                        if let Some(v) = sample.gpu_device_pct {
                            yield Ok(Sample {
                                ts_us: ts, device_ts_us: None,
                                kind: MetricKind::GpuDevicePct,
                                value: v,
                                labels: smallvec![],
                            });
                        }
                        if let Some(v) = sample.gpu_renderer_pct {
                            yield Ok(Sample {
                                ts_us: ts, device_ts_us: None,
                                kind: MetricKind::GpuRendererPct,
                                value: v,
                                labels: smallvec![],
                            });
                        }
                        if let Some(v) = sample.gpu_tiler_pct {
                            yield Ok(Sample {
                                ts_us: ts, device_ts_us: None,
                                kind: MetricKind::GpuTilerPct,
                                value: v,
                                labels: smallvec![],
                            });
                        }
                    }
                    Err(e) => {
                        transient_errors += 1;
                        if transient_errors >= MAX_TRANSIENT_ERRORS {
                            yield Err(SamplerError::Fatal(anyhow::anyhow!(
                                "graphics next_sample failed {MAX_TRANSIENT_ERRORS} times in a row: {e}"
                            )));
                            return;
                        }
                        tracing::warn!(
                            sampler = "ios.graphics",
                            transient_errors,
                            error = %e,
                            "transient graphics error — continuing"
                        );
                        yield Err(SamplerError::TransientIo(format!(
                            "graphics next sample: {e}"
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
