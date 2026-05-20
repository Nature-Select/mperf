//! Custom DTX channel for `com.apple.instruments.server.services.coreprofilesessiontap`.
//!
//! Bypasses `idevice::RemoteServerClient` for this one channel
//! because that crate's shared reader task calls
//! `ns_keyed_archive::decode::from_bytes` on every payload and
//! propagates the error. The coreprofile channel pushes raw
//! kperf/kdebug ring-buffer bytes (NOT a plist) and would crash the
//! reader on the first push, taking down all other channels on that
//! RemoteServerClient with it.
//!
//! Design: this struct owns ONE dtservicehub transport dedicated to
//! the coreprofile channel. The only DTX RPC we wait for a reply on
//! is the initial `_requestChannelWithCode:identifier:` mount; once
//! the channel is mounted, `setConfig:` and `start` are sent
//! fire-and-forget (matching pymobiledevice3's convention), and the
//! read path becomes a pure consumer of kdebug pushes.
//!
//! Other DTX needs (machTimeInfo, processcontrol, sysmontap) run on
//! their own idevice `RemoteServerClient` transports so they don't
//! compete with the kdebug push firehose on this one's read path.
//!
//! Protocol reference: pymobiledevice3's `core_profile_session_tap.py`
//! and py-ios-device's `app_lifecycle.py` / `kd_buf_parser.py`. We
//! used them as wire-format documentation, NOT as copyable code. The
//! numeric constants (kdf2 filter, debug_id, kd_buf field layout)
//! are Apple kernel-debug API constants and aren't copyrightable.

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
const SERVICE_DTSERVICEHUB: &str = "com.apple.instruments.dtservicehub";

/// Apple kdebug debug_id for "Initial Frame Rendering END" on iOS 16
/// and earlier (class 0x31, subclass 0xC0, code 321, FUNC_END=2).
/// iOS 26 no longer emits this — first-frame timing now arrives as
/// class 0x2B / subclass 0x87 (UIKit) events with codes we don't
/// have a phase table for. Kept for documentation + future phase-
/// breakdown work; current measurement uses "last 0x2B event" as a
/// proxy.
#[allow(dead_code)]
pub const FIRST_FRAME_END_DEBUG_ID: u32 = 0x31C00506;

/// Apple kdebug debug_id masks (from `bsd/sys/kdebug.h`). Kept here
/// next to the bit-layout they describe; phase-breakdown will use the
/// helper methods below.
#[allow(dead_code)]
const KDBG_CLASS_MASK: u32 = 0xff000000;
#[allow(dead_code)]
const KDBG_CLASS_OFFSET: u32 = 24;
#[allow(dead_code)]
const KDBG_SUBCLASS_MASK: u32 = 0x00ff0000;
#[allow(dead_code)]
const KDBG_SUBCLASS_OFFSET: u32 = 16;

/// One kernel-trace event. 64-byte `kd_buf` record straight off the
/// wire. `timestamp_mach` is in device mach ticks.
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

/// Coreprofile DTX channel — opens its own transport, mounts the
/// channel, starts the kdebug stream, and surfaces events.
pub struct CoreProfileSessionRaw {
    stream: Box<dyn ReadWrite>,
    next_msg_id: u32,
    coreprofile_channel: i32,
}

impl CoreProfileSessionRaw {
    /// Open transport, mount coreprofile channel, send setConfig +
    /// start. On return the device is streaming kdebug events.
    ///
    /// `mount_timeout` bounds how long we wait for the channel-mount
    /// reply. If the device never replies (very rare — usually a
    /// transport-level failure) we surface that as an error rather
    /// than hang. setConfig and start are fire-and-forget per
    /// pymobiledevice3's convention; if they're rejected, we find
    /// out via "no events for 15s" later.
    pub async fn start(udid: &str) -> Result<Self> {
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
            coreprofile_channel: 0,
        };

        // Bounded: device should publish capabilities < 2s; mount
        // reply should arrive < 2s. Hangs here are protocol-level
        // bugs we want to surface, not silently wait on.
        tracing::info!("coreprofile: transport opened, waiting for capabilities");
        tokio::time::timeout(Duration::from_secs(5), s.consume_initial_capabilities())
            .await
            .map_err(|_| anyhow!("timed out waiting for _notifyOfPublishedCapabilities"))?
            .context("consume initial capabilities")?;
        tracing::info!("coreprofile: capabilities consumed, mounting channel");

        let ch = tokio::time::timeout(
            Duration::from_secs(5),
            s.mount_channel_correlated(SERVICE_CORE_PROFILE),
        )
        .await
        .map_err(|_| anyhow!("timed out mounting coreprofile channel"))?
        .context("mount coreprofile")?;
        s.coreprofile_channel = ch;
        tracing::info!(ch, "coreprofile channel mounted");

        s.send_set_config().await.context("send setConfig")?;
        tracing::info!("coreprofile: setConfig sent");
        s.send_start().await.context("send start")?;
        tracing::info!("coreprofile streaming started");

        Ok(s)
    }

    async fn consume_initial_capabilities(&mut self) -> Result<()> {
        // First push on channel 0 is _notifyOfPublishedCapabilities:.
        // We read once and discard. If the device piles on additional
        // state pushes (rare, but happens on rapid reconnects), the
        // next mount_channel_correlated loop will skip them anyway.
        let _msg = self.read_lenient().await?;
        Ok(())
    }

    fn next_identifier(&mut self) -> u32 {
        self.next_msg_id = self.next_msg_id.wrapping_add(1);
        self.next_msg_id
    }

    /// Mount channel and wait for the reply ack. This is the ONLY
    /// place we correlate by identifier — all other RPCs on this
    /// transport are fire-and-forget.
    async fn mount_channel_correlated(&mut self, identifier: &str) -> Result<i32> {
        // Channel codes for locally-opened channels start at 1.
        let code: i32 = 1;
        let msg_id = self.next_identifier();
        let args = vec![
            AuxValue::U32(code as u32),
            AuxValue::Array(
                ns_keyed_archive::encode::encode_to_bytes(Value::String(identifier.into()))
                    .map_err(|e| anyhow!("encode channel identifier: {e}"))?,
            ),
        ];
        self.send_message(0, msg_id, Some("_requestChannelWithCode:identifier:"), Some(args), true)
            .await
            .context("send channel-mount RPC")?;

        // Loop until we get a reply with matching identifier and a
        // non-zero conversation_index (idevice's convention for
        // server replies vs server pushes).
        loop {
            let reply = self.read_lenient().await?;
            if reply.identifier == msg_id && reply.conversation_index != 0 {
                tracing::debug!(msg_id, "channel mount reply received");
                return Ok(code);
            }
            tracing::trace!(
                msg_id,
                got_id = reply.identifier,
                got_conv = reply.conversation_index,
                got_chan = reply.channel,
                "discarding pre-mount message"
            );
        }
    }

    async fn send_set_config(&mut self) -> Result<()> {
        let ch = self.coreprofile_channel;
        let mut tc_entry = Dictionary::new();
        // kdf2 is a typefilter — each entry is `(class << 24) |
        // (subclass << 16)`, matching every kdebug event with that
        // class+subclass prefix. The sentinel `{0xFFFFFFFF}` that
        // pymobiledevice3 uses as a "match everything" hint is
        // interpreted by the device as class 0xFF / subclass 0xFF —
        // which matches no real events; you get one pre-filter
        // ring-buffer dump and then silence.
        //
        // This is py-ios-device's app_lifecycle.py allow-list:
        // covers Dyld (0x1F/{05,07,08}), early init (0x01/...),
        // image loading (0x04/0x08, 0x07/0x00), AppKit (0x2B/0xD8),
        // UIKit (0x2B/0x87), Dyld-modern (0x2B/0xDC), Initial Frame
        // Rendering (0x31/0xC0). The non-obvious ones (0x21/0x13,
        // 0x2D/0xFF) are kept for completeness — they show up in
        // py-ios-device's traces and removing them risks losing
        // ordering context we may want for phase breakdown later.
        let kdf2_filter: &[u32] = &[
            0x2BD80000, 0x01250000, 0x04080000, 0x31C00000, 0x2BDC0000,
            0x21130000, 0x2B870000, 0x1F070000, 0x07000000, 0x01300000,
            0x010C0000, 0x01050000, 0x01090000, 0x2DFF0000, 0x1F080000,
            0x01400000, 0x1F050000,
        ];
        tc_entry.insert(
            "kdf2".into(),
            Value::Array(
                kdf2_filter
                    .iter()
                    .map(|&n| Value::Integer((n as i64).into()))
                    .collect(),
            ),
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
        tc_entry.insert("uuid".into(), Value::String(uuid_string()));

        let mut config = Dictionary::new();
        config.insert("rp".into(), Value::Integer(100i64.into()));
        // bm=1 (py-ios-device's value) = continuous streaming. bm=0
        // (pymobiledevice3's default for stackshot use) only gives us
        // ONE batch of pre-buffered events and then stops, so launch
        // events fired after `start` never arrive.
        config.insert("bm".into(), Value::Integer(1i64.into()));
        config.insert("tc".into(), Value::Array(vec![Value::Dictionary(tc_entry)]));

        // Second positional arg — py-ios-device's app_lifecycle demo
        // sends this in addition to the trace config. pymobiledevice3
        // omits it (their default usage is one-shot stackshot, not
        // continuous launch capture). We send it because without
        // it the device fires one snapshot of the pre-existing
        // kdebug ring and goes silent. The keys are opaque to us:
        // `tsf` looks like trigger-set-filter ([65537]), `si` is a
        // sample interval in ns (5_000_000 = 5ms), `tk:1` is a
        // different "kind" from tc's `tk:3`.
        let mut second = Dictionary::new();
        second.insert(
            "tsf".into(),
            Value::Array(vec![Value::Integer(65537i64.into())]),
        );
        second.insert(
            "ta".into(),
            Value::Array(vec![
                Value::Array(vec![Value::Integer(0i64.into())]),
                Value::Array(vec![Value::Integer(2i64.into())]),
                Value::Array(vec![
                    Value::Integer(1i64.into()),
                    Value::Integer(1i64.into()),
                    Value::Integer(0i64.into()),
                ]),
            ]),
        );
        second.insert("si".into(), Value::Integer(5_000_000i64.into()));
        second.insert("tk".into(), Value::Integer(1i64.into()));
        second.insert("uuid".into(), Value::String(uuid_string()));

        let args = vec![
            AuxValue::archived_value(Value::Dictionary(config)),
            AuxValue::archived_value(Value::Dictionary(second)),
        ];
        let msg_id = self.next_identifier();
        self.send_message(ch, msg_id, Some("setConfig:"), Some(args), false)
            .await
    }

    async fn send_start(&mut self) -> Result<()> {
        let ch = self.coreprofile_channel;
        let msg_id = self.next_identifier();
        self.send_message(ch, msg_id, Some("start"), None, false)
            .await
    }

    pub async fn stop(&mut self) -> Result<()> {
        let ch = self.coreprofile_channel;
        if ch == 0 {
            return Ok(());
        }
        let msg_id = self.next_identifier();
        let _ = self
            .send_message(ch, msg_id, Some("stop"), None, false)
            .await;
        // Drain & discard whatever the device pushes for 800ms so
        // kperf is fully shut down on the device side before we
        // close the socket. Without this, a second cold-start
        // measurement on the same device hits "kperf already owned"
        // (silent) and the kdebug stream never resumes.
        let drain_deadline = std::time::Instant::now() + Duration::from_millis(800);
        loop {
            let remaining = drain_deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, self.read_lenient()).await {
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }
        tracing::info!("coreprofile: stop drained");
        Ok(())
    }

    /// Send one outgoing DTX message. Doesn't wait for any reply.
    async fn send_message(
        &mut self,
        channel: i32,
        identifier: u32,
        selector: Option<&str>,
        args: Option<Vec<AuxValue>>,
        expects_reply: bool,
    ) -> Result<()> {
        let mheader = MessageHeader::new(0, 1, identifier, 0, channel, expects_reply);
        let pheader = PayloadHeader::method_invocation();
        let aux = args.map(Aux::from_values);
        let data = selector.map(|s| Value::String(s.into()));
        let msg = Message::new(mheader, pheader, aux, data);
        let bytes = msg.serialize();
        self.stream
            .write_all(&bytes)
            .await
            .map_err(|e| anyhow!("dtx write: {e}"))?;
        Ok(())
    }

    /// Read the next batch of kdebug events. Skips:
    ///   - replies on channel 0 (mount/etc — already past that phase)
    ///   - bplist payloads (acks, notices)
    ///   - stackshot kcdata blobs
    /// Returns when we get a payload that looks like a kd_buf batch.
    pub async fn next_events(&mut self) -> Result<Vec<KdEvent>> {
        loop {
            let msg = self.read_lenient().await?;
            let Some(bytes) = msg.raw_data else {
                tracing::debug!(
                    chan = msg.channel,
                    id = msg.identifier,
                    conv = msg.conversation_index,
                    "next_events: empty payload, skipping"
                );
                continue;
            };
            if bytes.is_empty() {
                continue;
            }
            if bytes.starts_with(&[0x07, b'X', 0xa2, b'Y']) {
                tracing::info!(bytes_len = bytes.len(), "next_events: stackshot kcdata (skip)");
                continue;
            }
            // iOS 26 wraps EVERY coreprofile push in a
            // DTKTraceTapMessage NSKeyedArchive container — the raw
            // kdebug bytes live as an NSData field inside. (Older
            // iOS apparently sent raw bytes per pymobiledevice3's
            // docstring, but our device does not.) Unwrap and look
            // for an NSData payload to parse as kd_buf records.
            if bytes.starts_with(b"bplist") {
                match extract_inner_data(&bytes) {
                    Some(inner) if inner.len() >= 8 => {
                        // bm=1 streams add a variable-length preamble
                        // before the 64-byte kd_buf records (~46 bytes
                        // observed). Try parsing from the trailing
                        // (inner_len % 64)-bytes-aligned offset; if
                        // that gives plausible records (any of class
                        // 0x1F / 0x2B / 0x31 present, or just a
                        // non-empty parse), return them.
                        let preamble = inner.len() % 64;
                        let events = parse_kd_buf_records(&inner[preamble..]);
                        if !events.is_empty() {
                            tracing::info!(
                                outer = bytes.len(),
                                inner = inner.len(),
                                preamble,
                                n_events = events.len(),
                                first8 = ?&inner[..inner.len().min(8)],
                                "next_events: unwrapped DTKTraceTapMessage → kdebug batch"
                            );
                            return Ok(events);
                        }
                    }
                    Some(inner) => {
                        tracing::info!(
                            outer = bytes.len(),
                            inner_len = inner.len(),
                            first8 = ?&inner[..inner.len().min(8)],
                            "next_events: unwrapped NSData but too small (skip)"
                        );
                    }
                    None => {
                        let strs = scan_bplist_strings(&bytes);
                        tracing::info!(
                            bytes_len = bytes.len(),
                            strings = ?strs,
                            "next_events: bplist with no NSData (status ping, skip)"
                        );
                    }
                }
                continue;
            }
            let events = parse_kd_buf_records(&bytes);
            if events.is_empty() {
                tracing::info!(
                    bytes_len = bytes.len(),
                    first8 = ?&bytes[..bytes.len().min(8)],
                    "next_events: unknown non-kdebug payload (skip)"
                );
                continue;
            }
            return Ok(events);
        }
    }

    /// Read one DTX message off the wire with tolerant payload
    /// decoding (bplist payloads decoded; everything else kept as
    /// raw bytes).
    async fn read_lenient(&mut self) -> Result<LenientMessage> {
        read_lenient(&mut self.stream).await
    }
}

#[derive(Debug)]
struct LenientMessage {
    identifier: u32,
    conversation_index: u32,
    channel: i32,
    raw_data: Option<Vec<u8>>,
}

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
        // Same channel sign convention as idevice — server-pushed
        // messages have conversation_index % 2 == 0 and the local
        // code is the negation of the wire code.
        let channel = if conversation_index.is_multiple_of(2) {
            -wire_channel
        } else {
            wire_channel
        };
        if fragment_count > 1 && fragment_id == 0 {
            continue;
        }
        let mut payload_buf = vec![0u8; length as usize];
        reader
            .read_exact(&mut payload_buf)
            .await
            .map_err(|e| anyhow!("dtx payload read: {e}"))?;
        packet_data.extend(payload_buf);
        // Done when this is the last fragment of the set. `fragment_count`
        // is sometimes 0 in pings from the device — treat that as a
        // single-fragment message and break.
        let last_fragment = fragment_count == 0 || fragment_id + 1 >= fragment_count;
        if last_fragment {
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

    Ok(LenientMessage {
        identifier,
        conversation_index,
        channel,
        raw_data,
    })
}

/// Parse a packed array of 64-byte `kd_buf` records. Live DTX stream
/// has no V2/V3 header (those only appear in file-based dumps), so
/// we just slice records straight out.
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

/// Walk an NSKeyedArchive bplist and return the LARGEST NSData value
/// found anywhere in its `$objects` pool. DTKTraceTapMessage embeds
/// its kdebug-bytes payload as an NSData; finding the biggest Data
/// is a robust heuristic against minor schema changes (key might be
/// `k`, `ktrace`, something else) — kdebug pushes are kilobytes,
/// any other Data in the archive is much smaller or absent.
fn extract_inner_data(bytes: &[u8]) -> Option<Vec<u8>> {
    let v = plist::Value::from_reader(std::io::Cursor::new(bytes)).ok()?;
    let top = v.as_dictionary()?;
    let objects = top.get("$objects")?.as_array()?;
    let mut best: Option<&[u8]> = None;
    for obj in objects {
        if let Value::Data(b) = obj {
            match best {
                None => best = Some(b),
                Some(prev) if b.len() > prev.len() => best = Some(b),
                _ => {}
            }
        }
    }
    best.map(|b| b.to_vec())
}

/// Pull every String found anywhere in the bplist's value tree. Used
/// to surface human-readable diagnostics ("notice"/"error"/"kperf is
/// already owned by another session"/etc.) from coreprofile's
/// DTTapMessage wrapper, which ns_keyed_archive doesn't fully decode.
fn scan_bplist_strings(bytes: &[u8]) -> Vec<String> {
    let Ok(v) = plist::Value::from_reader(std::io::Cursor::new(bytes)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_strings(&v, &mut out);
    out
}

fn collect_strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => {
            // Skip NSKeyedArchive scaffolding so the signal isn't
            // drowned by '$classname', 'NS.objects', etc.
            if !s.starts_with('$') && !s.starts_with("NS.") && !s.is_empty() {
                out.push(s.clone());
            }
        }
        Value::Array(arr) => arr.iter().for_each(|i| collect_strings(i, out)),
        Value::Dictionary(d) => d.values().for_each(|i| collect_strings(i, out)),
        _ => {}
    }
}

/// Generate an upper-case UUID-4 string — used as the `uuid` field
/// of the trace config. Apple's parser cares about the format
/// (uppercase hex with dashes); the value itself is opaque.
fn uuid_string() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut x = nanos.wrapping_mul(0x9E3779B97F4A7C15);
    let mut bytes = [0u8; 16];
    for b in bytes.iter_mut() {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *b = (x >> 56) as u8;
    }
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
