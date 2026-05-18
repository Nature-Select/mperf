//! Bypass for `idevice::services::dvt::graphics::GraphicsClient`.
//!
//! idevice 0.1.61's `GraphicsClient::sample()` returns a typed
//! `GraphicsSample` that exposes only `fps` + `memory`. The wire format
//! actually contains the full Instruments graphics push — including the
//! GPU **Tiler / Renderer / Device Utilization %** triplet that PerfDog
//! reports — but those fields are silently dropped before reaching
//! callers.
//!
//! This module talks to the same DTX channel directly and surfaces every
//! key in the push as a `GraphicsRawSample`. Same pattern as
//! `sysmontap_raw.rs`.
//!
//! Channel: `com.apple.instruments.server.services.graphics.opengl`
//! Method: `setSamplingRate:` then `startSampling`. Each subsequent push
//! is a Dictionary with keys like (varies by iOS version):
//!   - `CoreAnimationFramesPerSecond`     ← FPS (always present)
//!   - `Device Utilization %`              ← GPU device
//!   - `Renderer Utilization %`            ← GPU renderer (vertex/etc)
//!   - `Tiler Utilization %`               ← GPU tiler
//!   - `Alloc system memory` / `In use system memory`
//!   - …other `* %` and bytes counters

use idevice::IdeviceError;
use idevice::ReadWrite;
use idevice::dvt::message::AuxValue;
use idevice::dvt::remote_server::{Channel, RemoteServerClient};
use plist::{Dictionary, Value};

const SERVICE: &str = "com.apple.instruments.server.services.graphics.opengl";

#[derive(Debug, Default)]
pub struct GraphicsRawSample {
    pub fps: Option<f64>,
    pub gpu_device_pct: Option<f64>,
    pub gpu_renderer_pct: Option<f64>,
    pub gpu_tiler_pct: Option<f64>,
    /// All keys we saw in this push — diagnostic only. The decoder above
    /// looks at canonical names; if a future iOS renames them, the
    /// caller can dump this list to discover the new key.
    pub all_keys: Vec<String>,
}

pub struct GraphicsRaw<'a, R: ReadWrite> {
    channel: Channel<'a, R>,
}

impl<'a, R: ReadWrite> GraphicsRaw<'a, R> {
    pub async fn new(client: &'a mut RemoteServerClient<R>) -> Result<Self, IdeviceError> {
        let channel = client.make_channel(SERVICE.to_string()).await?;
        Ok(Self { channel })
    }

    /// Start sampling at the given interval (seconds). Single-call
    /// handshake — `startSamplingAtTimeInterval:` with a Double aux,
    /// expecting a reply. Matches the wire protocol idevice's typed
    /// `GraphicsClient::start_sampling` uses; the previous two-step
    /// `setSamplingRate:` + `startSampling` was a guess and iOS 17/26
    /// silently ignored it (sampler hung waiting for an ack).
    pub async fn start_sampling(&mut self, interval_sec: f64) -> Result<(), IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String("startSamplingAtTimeInterval:".into())),
                Some(vec![AuxValue::Double(interval_sec)]),
                true,
            )
            .await?;
        // Drain the reply so it doesn't get returned as the first sample.
        let _ = self.channel.read_message().await?;
        Ok(())
    }

    /// Wait for the next graphics **data** push (skipping ACKs and
    /// capability notices). The canonical signal that a frame is
    /// real data is the presence of `XRVideoCardRunTimeStamp` (also
    /// what idevice's typed parser uses).
    pub async fn next_sample(&mut self) -> Result<GraphicsRawSample, IdeviceError> {
        loop {
            let msg = self.channel.read_message().await?;
            let Some(decoded) = msg.data else { continue };
            let dict = match decoded {
                Value::Dictionary(d) => d,
                Value::Array(arr) => {
                    let mut found: Option<Dictionary> = None;
                    for v in arr {
                        if let Value::Dictionary(d) = v {
                            found = Some(d);
                            break;
                        }
                    }
                    match found {
                        Some(d) => d,
                        None => continue,
                    }
                }
                _ => continue,
            };
            if !dict.contains_key("XRVideoCardRunTimeStamp")
                && !dict.contains_key("CoreAnimationFramesPerSecond")
            {
                // Not a graphics data frame — keep waiting.
                continue;
            }
            return Ok(decode(&dict));
        }
    }
}

/// Hunt for canonical key names. iOS has shipped slight variations
/// across versions (with/without trailing space, "Util%" vs "Utilization
/// %"), so we accept any prefix match.
fn decode(d: &Dictionary) -> GraphicsRawSample {
    let mut s = GraphicsRawSample::default();
    s.all_keys = d.keys().map(|k| k.to_string()).collect();
    for (k, v) in d.iter() {
        let n = match value_as_f64(v) {
            Some(n) => n,
            None => continue,
        };
        let lk = k.to_ascii_lowercase();
        // FPS — exactly one well-known key.
        if k == "CoreAnimationFramesPerSecond" {
            s.fps = Some(n);
            continue;
        }
        // The three GPU utilization fields — match keywords; PerfDog
        // documentation uses these labels, the instruments wire format
        // uses the same keywords with " Utilization %" suffix.
        if lk.contains("device") && lk.contains("util") {
            s.gpu_device_pct = Some(n.clamp(0.0, 100.0));
        } else if lk.contains("renderer") && lk.contains("util") {
            s.gpu_renderer_pct = Some(n.clamp(0.0, 100.0));
        } else if lk.contains("tiler") && lk.contains("util") {
            s.gpu_tiler_pct = Some(n.clamp(0.0, 100.0));
        }
    }
    s
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => i.as_signed().map(|x| x as f64),
        Value::Real(r) => Some(*r),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_canonical_keys() {
        let mut d = Dictionary::new();
        d.insert("CoreAnimationFramesPerSecond".into(), Value::Real(58.7));
        d.insert("Device Utilization %".into(), Value::Real(42.0));
        d.insert("Renderer Utilization %".into(), Value::Real(31.5));
        d.insert("Tiler Utilization %".into(), Value::Real(18.0));
        let s = decode(&d);
        assert_eq!(s.fps, Some(58.7));
        assert_eq!(s.gpu_device_pct, Some(42.0));
        assert_eq!(s.gpu_renderer_pct, Some(31.5));
        assert_eq!(s.gpu_tiler_pct, Some(18.0));
    }

    #[test]
    fn matches_keyword_variants() {
        // Exercise the case-insensitive keyword match.
        let mut d = Dictionary::new();
        d.insert("device utilization%".into(), Value::Integer(5.into()));
        d.insert("Tiler Util %".into(), Value::Integer(3.into()));
        let s = decode(&d);
        assert_eq!(s.gpu_device_pct, Some(5.0));
        assert_eq!(s.gpu_tiler_pct, Some(3.0));
    }

    #[test]
    fn ignores_unrelated_keys() {
        let mut d = Dictionary::new();
        d.insert("Alloc system memory".into(), Value::Integer(123_456.into()));
        d.insert("CoreAnimationFramesPerSecond".into(), Value::Real(60.0));
        let s = decode(&d);
        assert_eq!(s.fps, Some(60.0));
        assert!(s.gpu_device_pct.is_none());
    }

    #[test]
    fn clamps_out_of_range() {
        let mut d = Dictionary::new();
        d.insert("Device Utilization %".into(), Value::Real(150.0));
        d.insert("Tiler Utilization %".into(), Value::Real(-5.0));
        let s = decode(&d);
        assert_eq!(s.gpu_device_pct, Some(100.0));
        assert_eq!(s.gpu_tiler_pct, Some(0.0));
    }
}
