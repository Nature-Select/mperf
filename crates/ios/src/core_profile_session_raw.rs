//! Custom DTX channel for `com.apple.instruments.server.services.coreprofilesessiontap`.
//!
//! Bypasses `idevice::RemoteServerClient` because that crate's shared
//! reader task calls `ns_keyed_archive::decode::from_bytes` on every
//! payload and propagates the error. The coreprofile channel pushes
//! raw kperf/kdebug ring-buffer bytes (NOT a plist) and would crash
//! the reader on the first push, taking down all other channels on
//! that RemoteServerClient with it.
//!
//! We open a dedicated dtservicehub connection, do the bare-minimum
//! DTX handshake by hand, and route NSKeyedArchive decoding ourselves
//! per-message instead of unconditionally.
//!
//! What this gives us: a stream of `KdEvent`s, where each event is the
//! 64-byte `kd_buf` Apple emits per kernel-tracepoint hit. App-launch
//! phases (Dyld init, UIKit init, didFinishLaunching, scene attach,
//! Initial Frame Rendering) all show up as events with specific
//! class/subclass codes on this stream. `Initial Frame Rendering END`
//! is `debug_id == 0x31C00506` and is the marker PerfDog's iOS "App
//! Launch" timer ends on.
//!
//! Protocol reference: pymobiledevice3's `core_profile_session_tap.py`
//! and py-ios-device's `app_lifecycle.py` / `kd_buf_parser.py`. Both
//! are AGPL/GPL — we used them as wire-format documentation, NOT as
//! copyable code. The numeric constants (kdf2 filters, debug_id
//! values, kd_buf field layout) are reproduced verbatim because
//! they're not copyrightable: they're Apple kernel-debug API
//! constants.

use crate::connect;
use anyhow::{anyhow, Context, Result};
use idevice::{
    core_device_proxy::CoreDeviceProxy,
    dvt::message::{Aux, AuxValue, Message, MessageHeader, PayloadHeader},
    rsd::RsdHandshake,
    IdeviceService, ReadWrite,
};
use plist::{Dictionary, Value};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SERVICE_CORE_PROFILE: &str = "com.apple.instruments.server.services.coreprofilesessiontap";
const SERVICE_DEVICEINFO: &str = "com.apple.instruments.server.services.deviceinfo";
const SERVICE_DTSERVICEHUB: &str = "com.apple.instruments.dtservicehub";

/// Apple kdebug debug_id for "Initial Frame Rendering END" — class
/// 0x31, subclass 0xCA, code 1, func DBG_FUNC_END(2). This is the
/// event PerfDog's iOS "App Launch" timer ends on; it fires once per
/// app launch right after the first frame is committed to the display.
pub const FIRST_FRAME_END_DEBUG_ID: u32 = 0x31C00506;

/// Apple kdebug debug_id masks (from `bsd/sys/kdebug.h`). Most of
/// these are only used by the (not-yet-implemented) phase-breakdown
/// path — kept here so the constants live next to the bit-layout
/// they describe.
#[allow(dead_code)]
const KDBG_CLASS_MASK: u32 = 0xff000000;
#[allow(dead_code)]
const KDBG_CLASS_OFFSET: u32 = 24;
#[allow(dead_code)]
const KDBG_SUBCLASS_MASK: u32 = 0x00ff0000;
#[allow(dead_code)]
const KDBG_SUBCLASS_OFFSET: u32 = 16;

/// One kernel-trace event. 64-byte `kd_buf` record straight off the
/// wire. `timestamp_mach` is in device mach ticks; convert via
/// `MachTimeInfo`. Fields `args`/`tid`/`cpuid` aren't read yet —
/// phase-breakdown will use them.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct KdEvent {
    pub timestamp_mach: u64,
    pub args: [u64; 4],
    pub tid: u64,
    pub debug_id: u32,
    pub cpuid: u32,
}

#[allow(dead_code)]
impl KdEvent {
    pub fn class_code(&self) -> u8 {
        ((self.debug_id & KDBG_CLASS_MASK) >> KDBG_CLASS_OFFSET) as u8
    }
    pub fn subclass_code(&self) -> u8 {
        ((self.debug_id & KDBG_SUBCLASS_MASK) >> KDBG_SUBCLASS_OFFSET) as u8
    }
}

/// Device timebase. Multiply mach ticks by `numer/denom` to get ns.
#[derive(Debug, Clone, Copy)]
pub struct MachTimeInfo {
    pub mach_absolute_time: u64,
    pub numer: u32,
    pub denom: u32,
}

impl MachTimeInfo {
    pub fn ticks_delta_to_ns(&self, delta: u64) -> u64 {
        (delta as u128 * self.numer as u128 / self.denom as u128) as u64
    }
}

/// DTX-level kdebug streamer with a tolerant message reader.
pub struct CoreProfileSessionRaw {
    stream: Box<dyn ReadWrite>,
    next_msg_id: u32,
    next_channel: i32,
    /// Mounted coreprofile channel code (server sends pushes on this).
    coreprofile_channel: i32,
}

impl CoreProfileSessionRaw {
    /// Open a fresh CoreDeviceProxy/RSD/dtservicehub stack on its own
    /// dedicated transport. Each instance burns one TCP connection.
    pub async fn connect(udid: &str) -> Result<Self> {
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
        let dvt_port = handshake
            .services
            .get(SERVICE_DTSERVICEHUB)
            .ok_or_else(|| anyhow!("RSD service '{SERVICE_DTSERVICEHUB}' not advertised"))?
            .port;
        let dvt_stream = handle
            .connect(dvt_port)
            .await
            .map_err(|e| anyhow!("connect dvt port {dvt_port}: {e}"))?;
        let stream: Box<dyn ReadWrite> = Box::new(dvt_stream);

        let mut s = Self {
            stream,
            next_msg_id: 0,
            next_channel: 1,
            coreprofile_channel: 0,
        };
        s.consume_initial_capabilities().await?;
        Ok(s)
    }

    /// The device opens DTX with an unsolicited
    /// `_notifyOfPublishedCapabilities:` push on channel 0. Read it
    /// and discard — we don't need the capability list.
    async fn consume_initial_capabilities(&mut self) -> Result<()> {
        let _ = self.read_message().await.context("initial capabilities")?;
        Ok(())
    }

    fn next_identifier(&mut self) -> u32 {
        self.next_msg_id = self.next_msg_id.wrapping_add(1);
        self.next_msg_id
    }

    fn next_channel_code(&mut self) -> i32 {
        let c = self.next_channel;
        self.next_channel += 1;
        c
    }

    /// Send a method invocation on the given channel and (if
    /// expect_reply) wait for the correlated reply.
    async fn call_method(
        &mut self,
        channel: i32,
        selector: Option<&str>,
        args: Option<Vec<AuxValue>>,
        expect_reply: bool,
    ) -> Result<LenientMessage> {
        let identifier = self.next_identifier();
        let mheader = MessageHeader::new(0, 1, identifier, 0, channel, expect_reply);
        let pheader = PayloadHeader::method_invocation();
        let aux = args.map(Aux::from_values);
        let data = selector.map(|s| Value::String(s.into()));
        let msg = Message::new(mheader, pheader, aux, data);
        let bytes = msg.serialize();
        self.stream
            .write_all(&bytes)
            .await
            .map_err(|e| anyhow!("dtx write: {e}"))?;

        if !expect_reply {
            // Pseudo-reply: return an empty placeholder.
            return Ok(LenientMessage::placeholder());
        }

        // Read until we get a reply on the matching identifier.
        loop {
            let reply = self.read_message().await?;
            if reply.identifier == identifier && reply.conversation_index != 0 {
                return Ok(reply);
            }
            tracing::debug!(
                identifier,
                got_id = reply.identifier,
                got_conv = reply.conversation_index,
                got_channel = reply.channel,
                "ignoring intermediate DTX message"
            );
        }
    }

    /// Mount a DTX service channel. Returns the local channel code.
    async fn mount_channel(&mut self, identifier: &str) -> Result<i32> {
        let code = self.next_channel_code();
        let args = vec![
            AuxValue::U32(code as u32),
            AuxValue::Array(
                ns_keyed_archive_encode_string(identifier)
                    .context("encode channel identifier as NSKeyedArchive")?,
            ),
        ];
        let _ = self
            .call_method(0, Some("_requestChannelWithCode:identifier:"), Some(args), true)
            .await
            .with_context(|| format!("mount channel {identifier}"))?;
        Ok(code)
    }

    /// Fetch `(mach_absolute_time, numer, denom)` from
    /// `deviceinfo.machTimeInfo`. Mounts a temporary channel, asks,
    /// drops. `mach_absolute_time` is the device's mach clock at the
    /// moment the device processed the RPC — use as t0 for converting
    /// kdebug timestamps via `ticks_delta_to_ns`.
    pub async fn fetch_mach_time_info(&mut self) -> Result<MachTimeInfo> {
        let ch = self.mount_channel(SERVICE_DEVICEINFO).await?;
        let reply = self
            .call_method(ch, Some("machTimeInfo"), None, true)
            .await
            .context("call machTimeInfo")?;
        let data = reply
            .decoded_data
            .ok_or_else(|| anyhow!("machTimeInfo: empty reply"))?;
        let arr = data
            .as_array()
            .ok_or_else(|| anyhow!("machTimeInfo: reply not Array: {data:?}"))?;
        let mach_absolute_time = arr
            .first()
            .and_then(value_as_u64)
            .ok_or_else(|| anyhow!("machTimeInfo[0] not unsigned: {arr:?}"))?;
        let numer = arr
            .get(1)
            .and_then(value_as_u64)
            .ok_or_else(|| anyhow!("machTimeInfo[1] not unsigned: {arr:?}"))? as u32;
        let denom = arr
            .get(2)
            .and_then(value_as_u64)
            .ok_or_else(|| anyhow!("machTimeInfo[2] not unsigned: {arr:?}"))? as u32;
        Ok(MachTimeInfo {
            mach_absolute_time,
            numer,
            denom,
        })
    }

    /// Mount the coreprofile channel and push the default kdebug
    /// allow-all filter config (`kdf2 = [0xFFFFFFFF]`). After this the
    /// device knows what to capture but hasn't started yet.
    ///
    /// Config matches pymobiledevice3's defaults: `rp=100` (recording
    /// priority), `bm=0` (buffer mode), `csd=128` (callstack depth),
    /// `tk=3` (kind), `ta=[[3],[0],[2],[1,1,0]]` (actions). `kdf2` is
    /// the kdebug class/subclass allow-list; `{0xFFFFFFFF}` is "all
    /// classes" which is more events than we need but avoids worrying
    /// about NSSet encoding (plist::Value has no Set variant; an
    /// allow-list of one element is small enough to send as Array).
    pub async fn set_config(&mut self) -> Result<()> {
        let ch = self.mount_channel(SERVICE_CORE_PROFILE).await?;
        self.coreprofile_channel = ch;

        let mut tc_entry = Dictionary::new();
        tc_entry.insert(
            "kdf2".into(),
            Value::Array(vec![Value::Integer((0xFFFFFFFFu64 as i64).into())]),
        );
        tc_entry.insert("csd".into(), Value::Integer(128i64.into()));
        tc_entry.insert("tk".into(), Value::Integer(3i64.into()));
        tc_entry.insert(
            "ta".into(),
            Value::Array(vec![
                Value::Array(vec![Value::Integer(3i64.into())]),
                Value::Array(vec![Value::Integer(0i64.into())]),
                Value::Array(vec![Value::Integer(2i64.into())]),
                Value::Array(vec![
                    Value::Integer(1i64.into()),
                    Value::Integer(1i64.into()),
                    Value::Integer(0i64.into()),
                ]),
            ]),
        );
        tc_entry.insert(
            "uuid".into(),
            Value::String(uuid_string()),
        );

        let mut config = Dictionary::new();
        config.insert("rp".into(), Value::Integer(100i64.into()));
        config.insert("bm".into(), Value::Integer(0i64.into()));
        config.insert("tc".into(), Value::Array(vec![Value::Dictionary(tc_entry)]));

        let args = vec![AuxValue::archived_value(Value::Dictionary(config))];
        let _ = self
            .call_method(ch, Some("setConfig:"), Some(args), true)
            .await
            .context("setConfig")?;
        Ok(())
    }

    pub async fn start(&mut self) -> Result<()> {
        let ch = self.coreprofile_channel;
        if ch == 0 {
            return Err(anyhow!("coreprofile channel not mounted (call set_config first)"));
        }
        let _ = self
            .call_method(ch, Some("start"), None, true)
            .await
            .context("start")?;
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        let ch = self.coreprofile_channel;
        if ch == 0 {
            return Ok(());
        }
        // Best-effort, don't block on reply.
        let _ = self.call_method(ch, Some("stop"), None, false).await;
        Ok(())
    }

    /// Drain & ignore queued kdebug pushes until none arrive within
    /// `quiet_for`. Used to clear the initial backlog of system events
    /// before the launch we care about.
    pub async fn drain_until_quiet(&mut self, quiet_for: Duration) -> Result<()> {
        loop {
            match tokio::time::timeout(quiet_for, self.read_message()).await {
                Ok(Ok(_)) => continue,
                Ok(Err(e)) => return Err(e),
                Err(_) => return Ok(()), // timeout = quiet
            }
        }
    }

    /// Read one DTX message off the wire and return whatever payload
    /// it carries, parsed loosely (decode plist data if it looks like
    /// bplist, otherwise expose `raw_data` only).
    pub async fn read_message(&mut self) -> Result<LenientMessage> {
        read_lenient(&mut self.stream).await
    }

    /// Read the next batch of kdebug events that arrives on the
    /// coreprofile channel. Skips bplist acks/notices (which would
    /// have decoded_data populated) and stackshot kcdata blobs.
    pub async fn next_events(&mut self) -> Result<Vec<KdEvent>> {
        loop {
            let msg = self.read_message().await?;
            if msg.channel != self.coreprofile_channel {
                continue;
            }
            let Some(bytes) = msg.raw_data else { continue };
            if bytes.is_empty() {
                continue;
            }
            // Stackshot kcdata header (KCDATA_BUFFER_BEGIN_STACKSHOT
            // little-endian) — not events, skip.
            if bytes.starts_with(&[0x07, b'X', 0xa2, b'Y']) {
                continue;
            }
            // bplist acks/notices already decoded into msg.decoded_data;
            // raw_data is still the bplist bytes but they're not
            // kdebug records. Skip them too.
            if bytes.starts_with(b"bplist") {
                continue;
            }
            let events = parse_kd_buf_records(&bytes);
            if events.is_empty() {
                continue;
            }
            return Ok(events);
        }
    }
}

/// One DTX message with optionally-decoded plist data. Same as
/// idevice's Message but doesn't fail the read on NSKeyedArchive
/// decode error — for kdebug raw pushes `decoded_data` stays None
/// while `raw_data` holds the bytes.
#[derive(Debug)]
pub struct LenientMessage {
    pub identifier: u32,
    pub conversation_index: u32,
    pub channel: i32,
    pub raw_data: Option<Vec<u8>>,
    pub decoded_data: Option<Value>,
}

impl LenientMessage {
    fn placeholder() -> Self {
        Self {
            identifier: 0,
            conversation_index: 0,
            channel: 0,
            raw_data: None,
            decoded_data: None,
        }
    }
}

/// Reads one DTX message, mirroring `idevice::Message::from_reader`'s
/// header logic but with tolerant payload decoding: bplist payloads
/// are decoded into `decoded_data`; non-bplist payloads (kdebug raw
/// pushes) leave `decoded_data=None` and surface via `raw_data`.
async fn read_lenient<R: tokio::io::AsyncRead + Unpin>(reader: &mut R) -> Result<LenientMessage> {
    let mut packet_data: Vec<u8> = Vec::new();
    let (identifier, conversation_index, channel) = loop {
        let mut buf = [0u8; 32];
        reader
            .read_exact(&mut buf)
            .await
            .map_err(|e| anyhow!("dtx header read: {e}"))?;
        let _magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let _header_len = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let fragment_id = u16::from_le_bytes([buf[8], buf[9]]);
        let fragment_count = u16::from_le_bytes([buf[10], buf[11]]);
        let length = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
        let identifier = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]);
        let conversation_index = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
        let wire_channel = i32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]);
        // Same channel sign convention as idevice: server-pushed
        // messages have conversation_index % 2 == 0 and the local
        // code is the negation of the wire code.
        let channel = if conversation_index.is_multiple_of(2) {
            -wire_channel
        } else {
            wire_channel
        };
        if fragment_count > 1 && fragment_id == 0 {
            // First fragment is header-only.
            continue;
        }
        let mut payload_buf = vec![0u8; length as usize];
        reader
            .read_exact(&mut payload_buf)
            .await
            .map_err(|e| anyhow!("dtx payload read: {e}"))?;
        packet_data.extend(payload_buf);
        if fragment_id == fragment_count - 1 {
            break (identifier, conversation_index, channel);
        }
    };

    if packet_data.len() < 16 {
        return Err(anyhow!(
            "dtx payload header truncated: {} bytes",
            packet_data.len()
        ));
    }
    let pheader = &packet_data[0..16];
    let aux_length = u32::from_le_bytes([pheader[4], pheader[5], pheader[6], pheader[7]]) as usize;
    let total_length = u32::from_le_bytes([pheader[8], pheader[9], pheader[10], pheader[11]]) as usize;
    let need_len = total_length.saturating_sub(aux_length);
    let data_start = 16 + aux_length;
    let data_end = data_start + need_len;
    if data_end > packet_data.len() {
        return Err(anyhow!(
            "dtx data slice {data_start}..{data_end} exceeds payload {len}",
            len = packet_data.len()
        ));
    }
    let data_bytes = &packet_data[data_start..data_end];
    let raw_data = if data_bytes.is_empty() {
        None
    } else {
        Some(data_bytes.to_vec())
    };
    let decoded_data = if data_bytes.starts_with(b"bplist") {
        ns_keyed_archive::decode::from_bytes(data_bytes).ok()
    } else {
        None
    };

    Ok(LenientMessage {
        identifier,
        conversation_index,
        channel,
        raw_data,
        decoded_data,
    })
}

/// Parse a packed array of 64-byte `kd_buf` records. The live DTX
/// stream has no V2/V3 header (those only appear in file-based
/// dumps), so we just slice records straight out.
fn parse_kd_buf_records(bytes: &[u8]) -> Vec<KdEvent> {
    const RECORD_SIZE: usize = 64;
    let mut events = Vec::with_capacity(bytes.len() / RECORD_SIZE);
    let mut offset = 0;
    while offset + RECORD_SIZE <= bytes.len() {
        let r = &bytes[offset..offset + RECORD_SIZE];
        let timestamp = u64::from_le_bytes(r[0..8].try_into().unwrap());
        let args = [
            u64::from_le_bytes(r[8..16].try_into().unwrap()),
            u64::from_le_bytes(r[16..24].try_into().unwrap()),
            u64::from_le_bytes(r[24..32].try_into().unwrap()),
            u64::from_le_bytes(r[32..40].try_into().unwrap()),
        ];
        let tid = u64::from_le_bytes(r[40..48].try_into().unwrap());
        let debug_id = u32::from_le_bytes(r[48..52].try_into().unwrap());
        let cpuid = u32::from_le_bytes(r[52..56].try_into().unwrap());
        events.push(KdEvent {
            timestamp_mach: timestamp,
            args,
            tid,
            debug_id,
            cpuid,
        });
        offset += RECORD_SIZE;
    }
    events
}

fn value_as_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Integer(i) => i.as_unsigned(),
        _ => None,
    }
}

fn ns_keyed_archive_encode_string(s: &str) -> Result<Vec<u8>> {
    ns_keyed_archive::encode::encode_to_bytes(Value::String(s.into()))
        .map_err(|e| anyhow!("NSKeyedArchive encode: {e}"))
}

/// Generate an upper-case UUID-4 string — used by `setConfig:` to tag
/// the trace config. Apple's parser cares about the format (uppercase
/// hex with dashes); the value itself is opaque. Hand-rolled rather
/// than pulling in the `uuid` crate for one string.
fn uuid_string() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // Tiny xorshift-style mix on the nanoseconds for visual variation;
    // doesn't need to be cryptographically random.
    let mut x = nanos.wrapping_mul(0x9E3779B97F4A7C15);
    let mut bytes = [0u8; 16];
    for b in bytes.iter_mut() {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *b = (x >> 56) as u8;
    }
    // Mark as version 4 / variant 1 per RFC 4122 — not strictly
    // required but keeps the string shape Apple expects.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}
