//! Spike: open `com.apple.instruments.server.services.activitytracetap`,
//! send pymobiledevice3's setConfig + start, launch the target app via
//! processcontrol, dump the first ~5s of payloads as hex.
//!
//! Goal: confirm (a) this channel returns events at all, (b) we can
//! open it back-to-back without the kperf-release ~30s wait that
//! coreprofile suffers from. If both true, this is the path.
//!
//! Run: cargo run --example activitytrace_spike -p mperf-ios -- \
//!         [udid] [bundle_id] [iterations]

use anyhow::{anyhow, Context, Result};
use idevice::{
    core_device_proxy::CoreDeviceProxy,
    dvt::{
        message::{Aux, AuxValue, Message, MessageHeader, PayloadHeader},
        process_control::ProcessControlClient,
        remote_server::RemoteServerClient,
    },
    rsd::RsdHandshake,
    IdeviceService, ReadWrite,
};
use plist::{Dictionary, Value};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing_subscriber::EnvFilter;

const SERVICE_ACTIVITY_TRACE: &str =
    "com.apple.instruments.server.services.activitytracetap";
const SERVICE_DTSERVICEHUB: &str = "com.apple.instruments.dtservicehub";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,mperf_ios=info")),
        )
        .with_target(true)
        .init();

    let args: Vec<String> = std::env::args().collect();
    let udid = match args.get(1).cloned() {
        Some(s) if !s.is_empty() => s,
        _ => discover_udid().await?,
    };
    let bundle_id = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "com.tyrell.eve".to_string());
    let iterations: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);

    tracing::info!(udid, bundle_id, iterations, "activitytrace spike begin");
    for i in 1..=iterations {
        tracing::info!(iter = i, "---- iteration starting ----");
        let t0 = Instant::now();
        match run_one(&udid, &bundle_id).await {
            Ok(payload_count) => tracing::info!(
                iter = i,
                payload_count,
                elapsed_ms = t0.elapsed().as_millis() as u64,
                "iteration ok"
            ),
            Err(e) => tracing::error!(iter = i, error = %e, "iteration FAIL"),
        }
        if i < iterations {
            tracing::info!("inter-iter sleep 3s (deliberately short to test back-to-back)");
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
    Ok(())
}

/// One spike iteration: open activitytracetap, setConfig + start, then
/// open a separate processcontrol transport and launch the app, then
/// read payloads off activitytracetap for ~5s and dump them.
async fn run_one(udid: &str, bundle_id: &str) -> Result<u64> {
    // -- Open activitytracetap on dedicated transport --
    let mut stream = open_dtservicehub_stream(udid).await?;
    tracing::info!("activitytrace: transport open");
    consume_capabilities(&mut stream).await?;
    let ch = mount_channel(&mut stream, SERVICE_ACTIVITY_TRACE).await?;
    tracing::info!(ch, "activitytrace: channel mounted");
    send_set_config(&mut stream, ch).await?;
    tracing::info!("activitytrace: setConfig sent");
    send_start(&mut stream, ch).await?;
    tracing::info!("activitytrace: start sent");

    // -- Launch app on separate transport so we don't interleave with
    // -- the event stream we're trying to drain.
    let mut remote_pc = build_dtx_remote(udid).await?;
    let mut pc = ProcessControlClient::new(&mut remote_pc).await?;
    let pid = pc
        .launch_app(
            bundle_id.to_string(),
            None,
            None,
            false, // start_suspended
            true,  // kill_existing
        )
        .await
        .with_context(|| format!("launch_app({bundle_id})"))?;
    tracing::info!(pid, "activitytrace: launch_app returned, draining 5s of payloads");

    // -- Drain payloads for 5s and dump them --
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut payload_count: u64 = 0;
    let mut bplist_count: u64 = 0;
    let mut event_blob_count: u64 = 0;
    let mut total_event_bytes: u64 = 0;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, read_payload(&mut stream)).await {
            Ok(Ok(payload)) => {
                payload_count += 1;
                let is_bplist = payload.starts_with(b"bplist");
                if is_bplist {
                    bplist_count += 1;
                    let strings = scan_bplist_strings(&payload);
                    tracing::info!(
                        bytes_len = payload.len(),
                        strings = ?strings,
                        "payload: bplist (heartbeat / status)"
                    );
                } else {
                    event_blob_count += 1;
                    total_event_bytes += payload.len() as u64;
                    let preview = payload
                        .iter()
                        .take(64)
                        .map(|b| format!("{:02x}", b))
                        .collect::<Vec<_>>()
                        .join(" ");
                    tracing::info!(
                        bytes_len = payload.len(),
                        first64_hex = %preview,
                        "payload: event blob"
                    );
                    // Try to decode a few opcodes for visibility
                    decode_first_opcodes(&payload);
                }
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "read_payload error");
                break;
            }
            Err(_) => break, // timeout
        }
    }
    tracing::info!(
        payload_count,
        bplist_count,
        event_blob_count,
        total_event_bytes,
        "drain done"
    );

    // -- Send stop, close --
    let _ = send_stop(&mut stream, ch).await;
    Ok(payload_count)
}

// -----------------------------------------------------------------
// Minimal DTX framing (copy-pasted style from core_profile_session_raw)
// -----------------------------------------------------------------

async fn open_dtservicehub_stream(udid: &str) -> Result<Box<dyn ReadWrite>> {
    let provider = mperf_ios::testing::provider_for(udid).await?;
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
        .map_err(|e| anyhow!("connect rsd: {e}"))?;
    let handshake = RsdHandshake::new(rsd_stream)
        .await
        .context("RsdHandshake::new")?;
    let dvt_port = handshake
        .services
        .get(SERVICE_DTSERVICEHUB)
        .ok_or_else(|| anyhow!("RSD service '{SERVICE_DTSERVICEHUB}' not advertised"))?
        .port;
    let dvt = handle
        .connect(dvt_port)
        .await
        .map_err(|e| anyhow!("connect dvt {dvt_port}: {e}"))?;
    Ok(Box::new(dvt) as Box<dyn ReadWrite>)
}

async fn consume_capabilities(stream: &mut Box<dyn ReadWrite>) -> Result<()> {
    let _ = read_dtx_frame(stream).await?;
    Ok(())
}

async fn mount_channel(stream: &mut Box<dyn ReadWrite>, identifier: &str) -> Result<i32> {
    let code: i32 = 1;
    let args = vec![
        AuxValue::U32(code as u32),
        AuxValue::Array(
            ns_keyed_archive::encode::encode_to_bytes(Value::String(identifier.into()))
                .map_err(|e| anyhow!("encode identifier: {e}"))?,
        ),
    ];
    send_dtx_method(stream, 0, 1, Some("_requestChannelWithCode:identifier:"), Some(args), true).await?;
    // Wait for the mount reply
    loop {
        let f = read_dtx_frame(stream).await?;
        if f.identifier == 1 && f.conversation_index != 0 {
            return Ok(code);
        }
    }
}

async fn send_set_config(stream: &mut Box<dyn ReadWrite>, channel: i32) -> Result<()> {
    // Verbatim from pymobiledevice3 activity_trace_tap.py
    let mut cfg = Dictionary::new();
    cfg.insert("bm".into(), Value::Integer(0i64.into()));
    cfg.insert("combineDataScope".into(), Value::Integer(0i64.into()));
    cfg.insert("machTimebaseDenom".into(), Value::Integer(3i64.into()));
    cfg.insert("machTimebaseNumer".into(), Value::Integer(125i64.into()));
    cfg.insert("onlySignposts".into(), Value::Integer(0i64.into()));
    cfg.insert("pidToInjectCombineDYLIB".into(), Value::String("-1".into()));
    cfg.insert(
        "predicate".into(),
        Value::String(
            "(messageType == info OR messageType == debug OR messageType == default OR messageType == error OR messageType == fault)".into(),
        ),
    );
    cfg.insert("signpostsAndLogs".into(), Value::Integer(1i64.into()));
    cfg.insert("trackPidToExecNameMapping".into(), Value::Boolean(true));
    cfg.insert("enableHTTPArchiveLogging".into(), Value::Boolean(false));
    cfg.insert("targetPID".into(), Value::Integer((-3i64).into()));
    cfg.insert("trackExpiredPIDs".into(), Value::Integer(1i64.into()));
    cfg.insert("ur".into(), Value::Integer(500i64.into()));
    let args = vec![AuxValue::archived_value(Value::Dictionary(cfg))];
    send_dtx_method(stream, channel, 2, Some("setConfig:"), Some(args), false).await
}

async fn send_start(stream: &mut Box<dyn ReadWrite>, channel: i32) -> Result<()> {
    send_dtx_method(stream, channel, 3, Some("start"), None, false).await
}

async fn send_stop(stream: &mut Box<dyn ReadWrite>, channel: i32) -> Result<()> {
    send_dtx_method(stream, channel, 4, Some("stop"), None, false).await
}

async fn send_dtx_method(
    stream: &mut Box<dyn ReadWrite>,
    channel: i32,
    identifier: u32,
    selector: Option<&str>,
    args: Option<Vec<AuxValue>>,
    expects_reply: bool,
) -> Result<()> {
    let mh = MessageHeader::new(0, 1, identifier, 0, channel, expects_reply);
    let ph = PayloadHeader::method_invocation();
    let aux = args.map(Aux::from_values);
    let data = selector.map(|s| Value::String(s.into()));
    let msg = Message::new(mh, ph, aux, data);
    let bytes = msg.serialize();
    stream.write_all(&bytes).await.map_err(|e| anyhow!("dtx write: {e}"))?;
    Ok(())
}

/// Read one DTX frame; return the data section bytes + correlation info.
async fn read_payload(stream: &mut Box<dyn ReadWrite>) -> Result<Vec<u8>> {
    let f = read_dtx_frame(stream).await?;
    Ok(f.data)
}

struct DtxFrame {
    identifier: u32,
    conversation_index: u32,
    data: Vec<u8>,
}

async fn read_dtx_frame(stream: &mut Box<dyn ReadWrite>) -> Result<DtxFrame> {
    let mut packet_data: Vec<u8> = Vec::new();
    let (identifier, conversation_index) = loop {
        let mut buf = [0u8; 32];
        stream
            .read_exact(&mut buf)
            .await
            .map_err(|e| anyhow!("dtx header read: {e}"))?;
        let fragment_id = u16::from_le_bytes([buf[8], buf[9]]);
        let fragment_count = u16::from_le_bytes([buf[10], buf[11]]);
        let length = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
        let identifier = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]);
        let conversation_index = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
        if fragment_count > 1 && fragment_id == 0 {
            continue;
        }
        let mut payload_buf = vec![0u8; length as usize];
        stream
            .read_exact(&mut payload_buf)
            .await
            .map_err(|e| anyhow!("dtx payload read: {e}"))?;
        packet_data.extend(payload_buf);
        let last = fragment_count == 0 || fragment_id + 1 >= fragment_count;
        if last {
            break (identifier, conversation_index);
        }
    };
    if packet_data.len() < 16 {
        return Err(anyhow!("payload header truncated: {} bytes", packet_data.len()));
    }
    let pheader = &packet_data[0..16];
    let aux_length = u32::from_le_bytes([pheader[4], pheader[5], pheader[6], pheader[7]]) as usize;
    let total_length = u32::from_le_bytes([pheader[8], pheader[9], pheader[10], pheader[11]]) as usize;
    let need_len = total_length.saturating_sub(aux_length);
    let data_start = 16 + aux_length;
    let data_end = data_start + need_len;
    if data_end > packet_data.len() {
        return Err(anyhow!("payload slice out of bounds"));
    }
    let data = packet_data[data_start..data_end].to_vec();
    Ok(DtxFrame {
        identifier,
        conversation_index,
        data,
    })
}

async fn build_dtx_remote(udid: &str) -> Result<RemoteServerClient<Box<dyn ReadWrite>>> {
    let stream = open_dtservicehub_stream(udid).await?;
    Ok(RemoteServerClient::new(stream))
}

// -----------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------

async fn discover_udid() -> Result<String> {
    let devices = mperf_ios::list_devices().await.context("list_devices")?;
    let usable: Vec<_> = devices.into_iter().filter(|d| d.usable).collect();
    if usable.is_empty() {
        anyhow::bail!("no usable iOS devices");
    }
    if usable.len() > 1 {
        let ids: Vec<String> = usable.iter().map(|d| d.id.clone()).collect();
        anyhow::bail!("multiple iOS devices, pass UDID: {ids:?}");
    }
    Ok(usable[0].id.clone())
}

fn scan_bplist_strings(bytes: &[u8]) -> Vec<String> {
    let Ok(v) = plist::Value::from_reader(std::io::Cursor::new(bytes)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => {
                if !s.starts_with('$') && !s.starts_with("NS.") && !s.is_empty() {
                    out.push(s.clone());
                }
            }
            Value::Array(a) => a.iter().for_each(|i| walk(i, out)),
            Value::Dictionary(d) => d.values().for_each(|i| walk(i, out)),
            _ => {}
        }
    }
    walk(&v, &mut out);
    out
}

/// Best-effort: decode the first few opcodes of an event blob. Stack-VM
/// per pymobiledevice3 docs; for the spike we just print recognized
/// opcodes / push terminator words. The point is to see "something
/// structurally plausible" — full decoder lives in the real client.
fn decode_first_opcodes(bytes: &[u8]) {
    let mut events: Vec<String> = Vec::new();
    let mut i = 0;
    let mut steps = 0;
    while i + 2 <= bytes.len() && steps < 24 {
        let w = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let high = w >> 8;
        let low = w & 0xff;
        let kind = match (w >> 14, high) {
            (_, 0x01) => format!("CMD_DEFINE_TABLE(low=0x{low:02x})"),
            (_, 0x02) => format!("CMD_END_ROW(low=0x{low:02x})"),
            (_, 0x05) => format!("CMD_CONVERT_MACH_CONTINUOUS"),
            (_, 0x64) => format!("CMD_TABLE_RESET(low=0x{low:02x})"),
            (_, 0x65) => format!("CMD_COPY(low=0x{low:02x})"),
            (_, 0x68) => format!("CMD_SENTINEL"),
            (_, 0x69) => format!("CMD_STRUCT(low=0x{low:02x})"),
            (_, 0x6A) => format!("CMD_PLACEHOLDER_COUNT(low=0x{low:02x})"),
            (_, 0x6B) => format!("CMD_DEBUG"),
            (0b10, _) => format!("PUSH_continue(low=0x{low:02x})"),
            (0b11, _) => format!("PUSH_terminate(low=0x{low:02x})"),
            _ => format!("UNKNOWN(0x{w:04x})"),
        };
        events.push(kind);
        i += 2;
        steps += 1;
    }
    tracing::info!(?events, "opcode preview");
}
