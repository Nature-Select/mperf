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

use std::collections::BTreeMap;

type SignpostKey = (String, String, String); // subsystem, category, name
type SignpostAgg = BTreeMap<SignpostKey, (u32, u32, u32)>; // (begin, end, event)

/// First few app-related (UpdateSequence/Commit) signpost rows: time
/// bytes + event-type + pid (for filtering NEW vs OLD Runner).
#[derive(Debug, Default)]
struct AppFramesLog {
    rows: Vec<FrameRow>,
}

#[derive(Debug, Clone)]
struct FrameRow {
    table: String,
    event_type: String,
    time_bytes: Vec<u8>,
    name: String,
    pid: u32,
}

fn aggregate(
    state: &VmState,
    signpost_agg: &mut SignpostAgg,
    launch_keywords: &mut Vec<String>,
    frames: &mut AppFramesLog,
) {
    let get = |row: &Row, col: &str| -> String {
        row.fields
            .iter()
            .find(|(c, _)| c == col)
            .map(|(_, v)| match v {
                StackItem::Bytes(b) => StackItem::Bytes(b.clone()).as_cstring(),
                _ => String::new(),
            })
            .unwrap_or_default()
    };
    let get_bytes = |row: &Row, col: &str| -> Vec<u8> {
        row.fields
            .iter()
            .find(|(c, _)| c == col)
            .map(|(_, v)| match v {
                StackItem::Bytes(b) => b.clone(),
                _ => Vec::new(),
            })
            .unwrap_or_default()
    };
    for row in &state.rows {
        match row.table_name.as_str() {
            "os-signpost" => {
                let key = (get(row, "subsystem"), get(row, "category"), get(row, "name"));
                let et = get(row, "event-type");
                let proc_path = get(row, "process-image-path");
                let entry = signpost_agg.entry(key.clone()).or_insert((0, 0, 0));
                match et.as_str() {
                    "Begin" => entry.0 += 1,
                    "End" => entry.1 += 1,
                    _ => entry.2 += 1,
                }
                // Capture up to 10000 UpdateSequence/Commit signposts
                // from any process (filter by pid later).
                if frames.rows.len() < 10000
                    && (key.2 == "UpdateSequence" || key.2 == "Commit")
                {
                    let _ = proc_path;
                    // Process column is a Group of (pid_bytes, name_bytes).
                    // Decode the pid as little-endian u32 (pad to 4 bytes).
                    let pid = row
                        .fields
                        .iter()
                        .find(|(c, _)| c == "process")
                        .and_then(|(_, v)| match v {
                            StackItem::Group(items) => items.first().map(|it| {
                                let b = match it {
                                    StackItem::Bytes(b) => b.clone(),
                                    _ => Vec::new(),
                                };
                                let mut padded = [0u8; 4];
                                let n = b.len().min(4);
                                padded[..n].copy_from_slice(&b[..n]);
                                u32::from_le_bytes(padded)
                            }),
                            _ => None,
                        })
                        .unwrap_or(0);
                    frames.rows.push(FrameRow {
                        table: row.table_name.clone(),
                        event_type: et.clone(),
                        time_bytes: get_bytes(row, "time"),
                        name: key.2.clone(),
                        pid,
                    });
                }
            }
            "os-log" => {
                let fmt = get(row, "format-string");
                if launch_keywords.len() < 32
                    && (fmt.to_lowercase().contains("launch")
                        || fmt.to_lowercase().contains("first frame")
                        || fmt.to_lowercase().contains("scene")
                        || fmt.to_lowercase().contains("didfinish"))
                {
                    let subsys = get(row, "subsystem");
                    launch_keywords.push(format!("{}|{}|{}", subsys, fmt.chars().take(80).collect::<String>(), get(row, "process-image-path").chars().take(50).collect::<String>()));
                }
            }
            _ => {}
        }
    }
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
    let (mti_mach, mti_numer, mti_denom) = fetch_mach_time_info(&mut remote_pc).await?;
    tracing::info!(
        mti_mach,
        mti_numer,
        mti_denom,
        "mti anchored (before launch)"
    );
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
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut payload_count: u64 = 0;
    let mut bplist_count: u64 = 0;
    let mut event_blob_count: u64 = 0;
    let mut total_event_bytes: u64 = 0;
    let mut signpost_agg: SignpostAgg = BTreeMap::new();
    let mut launch_keywords: Vec<String> = Vec::new();
    let mut frames = AppFramesLog::default();
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
                    let state = decode_event_blob(&payload);
                    aggregate(&state, &mut signpost_agg, &mut launch_keywords, &mut frames);
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
        n_signpost_groups = signpost_agg.len(),
        n_launch_logs = launch_keywords.len(),
        "drain done"
    );
    // Print aggregated signpost groups (sorted, all of them).
    for ((subsys, cat, name), (begin, end, event)) in &signpost_agg {
        tracing::info!(
            subsystem = %subsys,
            category = %cat,
            name = %name,
            begin,
            end,
            event,
            "signpost"
        );
    }
    // Print launch-keyword os_log entries.
    for k in &launch_keywords {
        tracing::info!(log = %k, "launch-keyword log");
    }
    // ---- Compute candidate launch durations ----
    // time bytes are 6-byte little-endian mach_continuous_time ticks.
    // Decode each row's ts, filter to ones past mti, find the first
    // UpdateSequence Begin (= "first frame starts updating") and the
    // first Commit End (= "first frame committed to compositor").
    let decode_ts = |b: &[u8]| -> u64 {
        let mut buf = [0u8; 8];
        let n = b.len().min(8);
        buf[..n].copy_from_slice(&b[..n]);
        u64::from_le_bytes(buf)
    };
    let to_ms = |ticks_delta: u64| -> u64 {
        (ticks_delta as u128 * mti_numer as u128 / mti_denom as u128 / 1_000_000) as u64
    };
    let target_pid: u32 = pid as u32;
    let mut first_update_begin: Option<u64> = None;
    let mut first_commit_end: Option<u64> = None;
    let mut first_update_end: Option<u64> = None;
    let mut n_post_mti_target_pid: u32 = 0;
    let mut all_runner_pids: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    for fr in &frames.rows {
        all_runner_pids.insert(fr.pid);
        if fr.pid != target_pid {
            continue;
        }
        let ts = decode_ts(&fr.time_bytes);
        if ts <= mti_mach {
            continue;
        }
        n_post_mti_target_pid += 1;
        if fr.name == "UpdateSequence" && fr.event_type == "Begin" && first_update_begin.is_none() {
            first_update_begin = Some(ts);
        }
        if fr.name == "UpdateSequence" && fr.event_type == "End" && first_update_end.is_none() {
            first_update_end = Some(ts);
        }
        if fr.name == "Commit" && fr.event_type == "End" && first_commit_end.is_none() {
            first_commit_end = Some(ts);
        }
    }
    tracing::info!(
        target_pid,
        ?all_runner_pids,
        n_post_mti_target_pid,
        "pid analysis"
    );
    let delta_ms = |t: Option<u64>| t.map(|ts| to_ms(ts - mti_mach));
    tracing::info!(
        first_update_begin_ms = ?delta_ms(first_update_begin),
        first_update_end_ms = ?delta_ms(first_update_end),
        first_commit_end_ms = ?delta_ms(first_commit_end),
        "candidate launch durations"
    );

    // Show all Runner frame ts relative to mti (signed) so we can see
    // whether they're pre- or post-launch on the device clock.
    tracing::info!(n_frames = frames.rows.len(), "captured Runner frames");
    // Print only post-mti rows (filter out pre-launch noise)
    for (idx, fr) in frames.rows.iter().enumerate() {
        let table = &fr.table;
        let et = &fr.event_type;
        let time_bytes = &fr.time_bytes;
        let name = &fr.name;
        let row_pid = fr.pid;
        let ts_check = decode_ts(time_bytes);
        if ts_check <= mti_mach { continue; }
        if row_pid != target_pid { continue; }
        let ts = decode_ts(time_bytes);
        let signed_delta_ticks: i128 = ts as i128 - mti_mach as i128;
        let signed_delta_ms: i128 =
            signed_delta_ticks * mti_numer as i128 / mti_denom as i128 / 1_000_000;
        tracing::info!(
            idx,
            table = %table,
            et = %et,
            name = %name,
            pid = row_pid,
            ts,
            mti = mti_mach,
            signed_delta_ms = signed_delta_ms as i64,
            "frame row (post-mti)"
        );
    }

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
    cfg.insert("bm".into(), Value::Integer(1i64.into()));
    cfg.insert("combineDataScope".into(), Value::Integer(0i64.into()));
    cfg.insert("machTimebaseDenom".into(), Value::Integer(3i64.into()));
    cfg.insert("machTimebaseNumer".into(), Value::Integer(125i64.into()));
    cfg.insert("onlySignposts".into(), Value::Integer(1i64.into()));
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

async fn fetch_mach_time_info(
    remote: &mut RemoteServerClient<Box<dyn ReadWrite>>,
) -> Result<(u64, u32, u32)> {
    let mut ch = remote
        .make_channel("com.apple.instruments.server.services.deviceinfo".to_string())
        .await
        .context("mount deviceinfo")?;
    ch.call_method(
        Some(Value::String("machTimeInfo".into())),
        None::<Vec<AuxValue>>,
        true,
    )
    .await
    .context("call machTimeInfo")?;
    let msg = ch.read_message().await.context("read machTimeInfo reply")?;
    let data = msg.data.ok_or_else(|| anyhow!("machTimeInfo: empty reply"))?;
    let arr = data
        .as_array()
        .ok_or_else(|| anyhow!("not array: {data:?}"))?;
    let to_u = |v: &Value| match v {
        Value::Integer(i) => i.as_unsigned(),
        _ => None,
    };
    let mach = arr.first().and_then(to_u).ok_or_else(|| anyhow!("[0]"))?;
    let numer = arr.get(1).and_then(to_u).ok_or_else(|| anyhow!("[1]"))? as u32;
    let denom = arr.get(2).and_then(to_u).ok_or_else(|| anyhow!("[2]"))? as u32;
    Ok((mach, numer, denom))
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

/// Stack-VM decoder. Mirrors pymobiledevice3's `_parse` /
/// `_handle_*` logic, but in idiomatic Rust. Yields `Row`s with
/// named columns according to the device's runtime-defined tables.
#[derive(Debug, Clone)]
struct Table {
    name: String,
    columns: Vec<String>,
}

#[derive(Debug, Clone)]
enum StackItem {
    Bytes(Vec<u8>),
    Sentinel,
    Group(Vec<StackItem>),
}

impl StackItem {
    fn as_bytes(&self) -> &[u8] {
        match self {
            StackItem::Bytes(b) => b,
            _ => &[],
        }
    }
    fn as_cstring(&self) -> String {
        let b = self.as_bytes();
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf8_lossy(&b[..end]).to_string()
    }
}

#[derive(Debug, Default)]
struct VmState {
    tables: Vec<Table>,
    stack: Vec<StackItem>,
    rows: Vec<Row>,
}

#[derive(Debug, Clone)]
struct Row {
    table_name: String,
    fields: Vec<(String, StackItem)>,
}

fn decode_event_blob(bytes: &[u8]) -> VmState {
    let mut state = VmState::default();
    let mut i = 0usize;
    while i + 2 <= bytes.len() {
        let w = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        i += 2;
        let high = (w >> 8) as u8;
        let low = (w & 0xff) as u8;
        match high {
            0x01 => handle_define_table(&mut state, low),
            0x02 => handle_end_row(&mut state, low),
            0x05 => {} // CONVERT_MACH_CONTINUOUS — host no-op
            0x64 => {
                // TABLE_RESET — bump generation, clear stack
                state.stack.clear();
            }
            0x65 => {
                // COPY(distance) — pymobiledevice3: `stack[-distance - 1]`
                // i.e. distance is 0-indexed from top. distance==0 copies
                // the top of stack. We skip the special 0xFF (long-struct)
                // path for now.
                let d = low as usize;
                if low != 0xFF && state.stack.len() > d {
                    let item = state.stack[state.stack.len() - d - 1].clone();
                    state.stack.push(item);
                }
            }
            0x68 => state.stack.push(StackItem::Sentinel),
            0x69 => {
                // STRUCT(distance) — group last `distance` items
                let d = low as usize;
                if d <= state.stack.len() {
                    let n = state.stack.len();
                    let group: Vec<StackItem> = state.stack.drain(n - d..).collect();
                    state.stack.push(StackItem::Group(group));
                }
            }
            0x6A => {
                // PLACEHOLDER_COUNT(count) — drop last `count` items
                let n = low as usize;
                if n <= state.stack.len() {
                    let len = state.stack.len();
                    state.stack.truncate(len - n);
                }
            }
            0x6B => {
                // DEBUG — pop + sanity check; we just pop
                state.stack.pop();
            }
            _ => {
                // Push — accumulate 14-bit words until terminator
                i = handle_push(&mut state, w, bytes, i);
            }
        }
    }
    state
}

fn handle_push(state: &mut VmState, mut word: u16, bytes: &[u8], mut i: usize) -> usize {
    // Build bit stream into Vec<u8> MSB-first (matches pymobiledevice3's
    // `imm.to_bytes(..., "big")` semantics for arbitrarily-long pushes
    // — using u128 panics with shift overflow on strings > 16 bytes).
    let mut bit_buf: Vec<u8> = Vec::new();
    let mut bit_pos: usize = 0;
    loop {
        let top = word >> 14;
        let data14: u16 = word & 0x3FFF;
        // Append 14 bits of data14 to bit_buf, MSB-first
        for bit_i in (0..14).rev() {
            let bit = ((data14 >> bit_i) & 1) as u8;
            let byte_idx = bit_pos / 8;
            let bit_in_byte = 7 - (bit_pos % 8);
            if byte_idx >= bit_buf.len() {
                bit_buf.push(0);
            }
            bit_buf[byte_idx] |= bit << bit_in_byte;
            bit_pos += 1;
        }
        if top == 0b11 {
            break;
        }
        if i + 2 > bytes.len() {
            break;
        }
        word = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        i += 2;
    }
    // bit_pos is the actual data-bit count. Final byte already has
    // trailing zero bits — i.e. left-aligned. That matches python's
    // `imm <<= 8 - bit_count % 8` then to_bytes("big") behaviour.
    state.stack.push(StackItem::Bytes(bit_buf));
    i
}

fn handle_define_table(state: &mut VmState, _low: u8) {
    // Pop the last 4 items: (unknown0, unknown2, name, columns)
    // pymobiledevice3 does `Table(*self.stack[-distance:])` with distance=4.
    if state.stack.len() < 4 {
        return;
    }
    let n = state.stack.len();
    let parts: Vec<StackItem> = state.stack.drain(n - 4..).collect();
    // parts[0] = unknown0, parts[1] = unknown2, parts[2] = name, parts[3] = columns
    let name = parts[2].as_cstring();
    let columns: Vec<String> = match &parts[3] {
        StackItem::Group(items) => items.iter().map(|i| i.as_cstring()).collect(),
        StackItem::Bytes(b) => {
            // Single column? Treat as one.
            let s = StackItem::Bytes(b.clone()).as_cstring();
            vec![s]
        }
        _ => Vec::new(),
    };
    state.tables.push(Table { name, columns });
}

fn handle_end_row(state: &mut VmState, table_idx: u8) {
    let table = match state.tables.get(table_idx as usize) {
        Some(t) => t.clone(),
        None => return,
    };
    let ncols = table.columns.len();
    if state.stack.len() < ncols {
        return;
    }
    let n = state.stack.len();
    let popped: Vec<StackItem> = state.stack.drain(n - ncols..).collect();
    let fields: Vec<(String, StackItem)> = table
        .columns
        .iter()
        .zip(popped.into_iter())
        .map(|(c, v)| (c.clone(), v))
        .collect();
    state.rows.push(Row {
        table_name: table.name,
        fields,
    });
}

fn print_decoded(blob_idx: usize, bytes: &[u8]) {
    let state = decode_event_blob(bytes);
    let table_names: Vec<&str> = state.tables.iter().map(|t| t.name.as_str()).collect();
    let columns_per_table: Vec<String> = state
        .tables
        .iter()
        .map(|t| format!("{}={:?}", t.name, t.columns))
        .collect();
    tracing::info!(
        blob_idx,
        n_tables = state.tables.len(),
        n_rows = state.rows.len(),
        ?table_names,
        "decode summary"
    );
    if !columns_per_table.is_empty() {
        tracing::info!(?columns_per_table, "table schemas");
    }
    // Group signposts by (subsystem, category, name) so we can spot
    // the launch-related ones quickly.
    use std::collections::BTreeMap;
    let mut signpost_summary: BTreeMap<(String, String, String), (u32, u32)> = BTreeMap::new(); // (begin, end)
    for row in &state.rows {
        if !row.table_name.starts_with("os-signpost") {
            continue;
        }
        let get = |col: &str| -> String {
            row.fields
                .iter()
                .find(|(c, _)| c == col)
                .map(|(_, v)| match v {
                    StackItem::Bytes(b) => StackItem::Bytes(b.clone()).as_cstring(),
                    _ => String::new(),
                })
                .unwrap_or_default()
        };
        let subsystem = get("subsystem");
        let category = get("category");
        let name = get("name");
        let event_type = get("event-type");
        let key = (subsystem, category, name);
        let entry = signpost_summary.entry(key).or_insert((0, 0));
        if event_type == "Begin" {
            entry.0 += 1;
        } else if event_type == "End" {
            entry.1 += 1;
        } else {
            // Event-type may also be "Event" (no scope)
        }
    }
    for ((subsys, cat, name), (begins, ends)) in &signpost_summary {
        tracing::info!(
            subsystem = %subsys,
            category = %cat,
            name = %name,
            begins,
            ends,
            "signpost group"
        );
    }
}
