//! Bypass for `idevice::services::dvt::sysmontap::SysmontapClient`.
//!
//! idevice 0.1.61's `SysmontapClient::next_sample` returns only the **first**
//! row in a sysmontap push that contains `Processes` / `System` /
//! `SystemCPUUsage`. iOS pushes those as separate rows in a single Array, so
//! the per-process dict is silently dropped, every sample we see has only
//! `SystemCPUUsage`, and per-app memory never gets emitted.
//!
//! This module talks to the sysmontap DTX channel directly and merges all
//! matching rows from one push into a single `SysmontapSample`.

use idevice::IdeviceError;
use idevice::ReadWrite;
use idevice::dvt::message::AuxValue;
use idevice::dvt::remote_server::{Channel, RemoteServerClient};
use plist::{Dictionary, Value};

const SERVICE: &str = "com.apple.instruments.server.services.sysmontap";

pub struct SysmontapConfig {
    pub interval_ms: u32,
    pub process_attributes: Vec<String>,
    pub system_attributes: Vec<String>,
}

#[derive(Debug, Default)]
pub struct SysmontapSample {
    pub processes: Option<Dictionary>,
    pub system_cpu_usage: Option<Dictionary>,
    /// `System` row decoded — keys are the attribute names from
    /// `SystemAttributes` (positional Array) or whatever iOS actually
    /// sent as a Dict. Either way the caller sees a flat Dict keyed by
    /// attr name with numeric values.
    pub system: Option<Dictionary>,
    /// `PerCPUUsage` array — per-core breakdown. Each entry is a Dictionary
    /// with keys like `CPU_TotalLoad`, `CPU_UserLoad`, `CPU_SystemLoad`.
    /// iOS hands this out in the same row as `Processes`.
    pub per_cpu_usage: Option<Vec<Value>>,
    /// The `SystemAttributes` array iOS echoes back to confirm which
    /// sysAttrs it'll send and in what order. Used to positional-decode
    /// a `System` Array (mirroring the way Processes entries work).
    pub system_attributes: Option<Vec<String>>,
    /// All top-level keys we saw across the rows of this push that we
    /// didn't classify as structural — purely diagnostic, NOT folded
    /// into `system` (NSKeyedArchiver markers like `$class` would
    /// otherwise look like fake data).
    pub unknown_keys: Vec<String>,
    /// Raw rows of this push, kept around for first-sample diagnostics
    /// only when the caller asks (`set_capture_raw(true)`). Each entry
    /// is the original Plist Value of one row in the Array push.
    pub raw_rows: Option<Vec<Value>>,
}

pub struct SysmontapRaw<'a, R: ReadWrite> {
    channel: Channel<'a, R>,
    /// When set, the next sample returned will include the raw decoded
    /// rows so the caller can dump them for first-sample diagnostics.
    /// Auto-reset after one sample.
    capture_raw_next: bool,
}

impl<'a, R: ReadWrite> SysmontapRaw<'a, R> {
    pub async fn new(client: &'a mut RemoteServerClient<R>) -> Result<Self, IdeviceError> {
        let channel = client.make_channel(SERVICE.to_string()).await?;
        Ok(Self {
            channel,
            capture_raw_next: false,
        })
    }

    /// Ask the next call to `next_sample()` to also return the original
    /// decoded plist rows in `SysmontapSample::raw_rows`. Auto-resets
    /// after one sample.
    pub fn capture_raw_once(&mut self) {
        self.capture_raw_next = true;
    }

    pub async fn set_config(&mut self, config: &SysmontapConfig) -> Result<(), IdeviceError> {
        // Layout copied from idevice's SysmontapClient::set_config — same keys
        // Apple's Instruments client uses. The bug is in *reading*, not
        // configuring, so the wire format is unchanged.
        let mut cfg = Dictionary::new();
        cfg.insert("ur".into(), Value::Integer((config.interval_ms as i64).into()));
        cfg.insert("bm".into(), Value::Integer(0i64.into()));
        cfg.insert(
            "procAttrs".into(),
            Value::Array(
                config
                    .process_attributes
                    .iter()
                    .map(|s| Value::String(s.clone()))
                    .collect(),
            ),
        );
        cfg.insert(
            "sysAttrs".into(),
            Value::Array(
                config
                    .system_attributes
                    .iter()
                    .map(|s| Value::String(s.clone()))
                    .collect(),
            ),
        );
        cfg.insert("cpuUsage".into(), Value::Boolean(true));
        cfg.insert("physFootprint".into(), Value::Boolean(true));
        cfg.insert(
            "sampleInterval".into(),
            Value::Integer(((config.interval_ms as i64) * 1_000_000).into()),
        );

        self.channel
            .call_method(
                Some(Value::String("setConfig:".into())),
                Some(vec![AuxValue::archived_value(Value::Dictionary(cfg))]),
                false,
            )
            .await
    }

    pub async fn start(&mut self) -> Result<(), IdeviceError> {
        self.channel
            .call_method(Some(Value::String("start".into())), None, false)
            .await?;
        // Consume the start ack.
        let _ = self.channel.read_message().await?;
        Ok(())
    }

    /// Wait for the next sysmontap **data** push and merge its rows into a
    /// single sample.
    ///
    /// We require at least one of `Processes` / `SystemCPUUsage` /
    /// `PerCPUUsage` / `System` to consider a push a real data sample —
    /// this filters out protocol metadata pushes (`DTTapMessagePlist`,
    /// `$class`-bearing NSKeyedArchiver wrappers, etc.) that arrive on
    /// the same channel.
    pub async fn next_sample(&mut self) -> Result<SysmontapSample, IdeviceError> {
        loop {
            let msg = self.channel.read_message().await?;
            let Some(decoded) = msg.data else { continue };

            let rows: Vec<Value> = match decoded {
                Value::Array(arr) => arr,
                Value::Dictionary(d) => vec![Value::Dictionary(d)],
                _ => continue,
            };

            let mut sample = SysmontapSample::default();
            let mut has_structural = false;
            // System sysAttr values come either:
            //  - as a Dict under "System" — name → value, OR
            //  - as a positional Array under "System" — decode via
            //    SystemAttributes, OR
            //  - as flat top-level numeric keys alongside SystemCPUUsage.
            // Build the merged Dict across all three shapes.
            let mut system_merged = Dictionary::new();
            let mut deferred_system_array: Option<Vec<Value>> = None;
            let raw_rows_snapshot = if self.capture_raw_next {
                Some(rows.clone())
            } else {
                None
            };
            for row in rows {
                if let Some(dict) = row.into_dictionary() {
                    for (key, value) in dict.iter() {
                        match key.as_str() {
                            "Processes" => {
                                if let Some(v) = value.as_dictionary() {
                                    sample.processes = Some(v.clone());
                                    has_structural = true;
                                }
                            }
                            "SystemCPUUsage" => {
                                if let Some(v) = value.as_dictionary() {
                                    sample.system_cpu_usage = Some(v.clone());
                                    has_structural = true;
                                }
                            }
                            "System" => {
                                if let Some(v) = value.as_dictionary() {
                                    for (k, vv) in v.iter() {
                                        system_merged.insert(k.clone(), vv.clone());
                                    }
                                    has_structural = true;
                                } else if let Some(arr) = value.as_array() {
                                    // Positional — wait for SystemAttributes
                                    // to come in (could be same row or a
                                    // previous row), then decode.
                                    deferred_system_array = Some(arr.clone());
                                    has_structural = true;
                                }
                            }
                            "PerCPUUsage" => {
                                if let Some(v) = value.as_array() {
                                    sample.per_cpu_usage = Some(v.clone());
                                    has_structural = true;
                                }
                            }
                            "SystemAttributes" => {
                                if let Some(arr) = value.as_array() {
                                    let names: Vec<String> = arr
                                        .iter()
                                        .filter_map(|v| v.as_string().map(|s| s.to_string()))
                                        .collect();
                                    if !names.is_empty() {
                                        sample.system_attributes = Some(names);
                                    }
                                }
                            }
                            // Structural / metadata keys we don't need.
                            "ProcessesAttributes" | "CPUCount" | "EnabledCPUs"
                            | "Type" | "StartMachAbsTime" | "EndMachAbsTime"
                            | "CPUUsage" => {}
                            // Anything else: only fold into system if it's
                            // a numeric value AND the name doesn't look
                            // like NSKeyedArchiver protocol gunk.
                            other => {
                                let is_proto_marker = other.starts_with('$')
                                    || other.starts_with("NS")
                                    || other == "DTTapMessagePlist";
                                let is_numeric = matches!(
                                    value,
                                    Value::Integer(_) | Value::Real(_)
                                );
                                if !is_proto_marker && is_numeric {
                                    system_merged.insert(other.to_string(), value.clone());
                                }
                                sample.unknown_keys.push(other.to_string());
                            }
                        }
                    }
                }
            }
            // If we collected a positional System Array AND we now know
            // the attribute names, zip them into the merged Dict.
            if let (Some(arr), Some(names)) =
                (deferred_system_array, sample.system_attributes.as_ref())
            {
                for (i, val) in arr.iter().enumerate() {
                    if let Some(name) = names.get(i) {
                        system_merged.insert(name.clone(), val.clone());
                    }
                }
            }
            if !system_merged.is_empty() {
                sample.system = Some(system_merged);
            }
            if !has_structural {
                // Protocol message / ack — drop and keep reading.
                continue;
            }
            if raw_rows_snapshot.is_some() {
                self.capture_raw_next = false;
                sample.raw_rows = raw_rows_snapshot;
            }
            return Ok(sample);
        }
    }
}
