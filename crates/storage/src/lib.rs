//! SQLite persistence. See `docs/abstractions.md` §7 for the layout
//! contract: sessions + wide samples (unlabeled metrics) + long samples
//! (labeled metrics).
//!
//! Async surface via `tokio-rusqlite`: all SQL runs on a dedicated thread,
//! we await the result.

mod model;
mod schema;

pub use model::{Marker, SamplePoint, SessionInfo, SessionMeta};
pub use schema::HEAD as SCHEMA_HEAD;

use anyhow::{Context, Result};
use mperf_schema::{LabelKey, MetricKind, Sample};
use rusqlite::{params, OptionalExtension};
use std::path::PathBuf;
use tokio_rusqlite::Connection;

/// A thin wrapper around the on-disk SQLite database.
#[derive(Clone)]
pub struct Storage {
    conn: Connection,
}

impl Storage {
    /// Open (or create) the database at `path` and apply pending migrations.
    pub async fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create data dir {}", parent.display()))?;
        }
        let conn = Connection::open(&path)
            .await
            .with_context(|| format!("open sqlite at {}", path.display()))?;
        // Apply pragmas + migrations.
        conn.call(|c| {
            c.execute_batch(
                "
                PRAGMA journal_mode = WAL;
                PRAGMA synchronous = NORMAL;
                PRAGMA foreign_keys = ON;
                ",
            )?;
            schema::run_migrations(c)?;
            // Finalize any sessions that were left "in progress" because the
            // app crashed or was force-quit. We set wall_end_ms to the time
            // of the last recorded sample; if there are no samples we use
            // wall_start_ms so the row is at least deletable from the UI.
            let orphans = c.execute(
                "UPDATE sessions
                 SET wall_end_ms = wall_start_ms + COALESCE(
                     (SELECT MAX(ts_us) FROM (
                         SELECT ts_us FROM samples_wide WHERE samples_wide.session_id = sessions.id
                         UNION ALL
                         SELECT ts_us FROM samples_long WHERE samples_long.session_id = sessions.id
                     )) / 1000,
                     0
                 )
                 WHERE wall_end_ms IS NULL",
                [],
            )?;
            if orphans > 0 {
                tracing::info!(orphans, "finalized leftover in-progress sessions");
            }
            Ok(())
        })
        .await
        .context("apply pragmas / migrations")?;
        tracing::info!(path = %path.display(), "storage opened");
        Ok(Self { conn })
    }

    /// Create a new session row. Returns the auto-assigned id.
    pub async fn create_session(&self, meta: SessionMeta) -> Result<i64> {
        // Snapshots (`selected_metrics`, `sampling_intervals`) live as
        // JSON-encoded TEXT columns so the SQL schema doesn't grow new
        // join tables for what's read exactly once at session-detail
        // open. Serialisation cost is negligible — small structures.
        let selected_json = meta
            .selected_metrics
            .as_ref()
            .map(|v| serde_json::to_string(v).expect("Vec<String> serialises"));
        let intervals_json = meta
            .sampling_intervals
            .as_ref()
            .map(|m| serde_json::to_string(m).expect("HashMap<String,u64> serialises"));
        let id = self
            .conn
            .call(move |c| {
                c.execute(
                    "INSERT INTO sessions (
                        wall_start_ms, device_id, device_platform,
                        device_model, app_bundle_id, meta_json,
                        selected_metrics, sampling_intervals
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        meta.wall_start_ms,
                        meta.device_id,
                        meta.device_platform,
                        meta.device_model,
                        meta.app_bundle_id,
                        meta.meta_json,
                        selected_json,
                        intervals_json,
                    ],
                )?;
                Ok(c.last_insert_rowid())
            })
            .await
            .context("insert session")?;
        Ok(id)
    }

    /// Mark a session finished. Idempotent: a row whose `wall_end_ms` is
    /// already non-null keeps its existing timestamp. Both paths into
    /// this function (writer-task auto-finalize on broadcast close and
    /// `Session::stop`'s manual finalize) reach it for the same session
    /// — without the `WHERE wall_end_ms IS NULL` guard the second call
    /// silently overwrites the first timestamp by 1-300ms, which means
    /// `wall_end_ms` reports when the cleanup finished, not when
    /// sampling actually stopped.
    pub async fn finish_session(&self, id: i64, wall_end_ms: i64) -> Result<()> {
        self.conn
            .call(move |c| {
                c.execute(
                    "UPDATE sessions SET wall_end_ms = ? WHERE id = ? AND wall_end_ms IS NULL",
                    params![wall_end_ms, id],
                )?;
                Ok(())
            })
            .await
            .context("finish session")?;
        Ok(())
    }

    /// Bulk-insert a batch of samples in one transaction.
    pub async fn insert_sample_batch(&self, session_id: i64, batch: Vec<Sample>) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        self.conn
            .call(move |c| {
                let tx = c.transaction()?;
                for s in &batch {
                    let kind_str = serde_plain_kind(s.kind);
                    if s.labels.is_empty() {
                        if let Some(col) = wide_column(s.kind) {
                            // The column name comes from a closed enum so
                            // string substitution is injection-safe.
                            let sql = WIDE_UPSERT_TEMPLATE.replace("__COL__", col);
                            let mut stmt = tx.prepare_cached(&sql)?;
                            stmt.execute(params![session_id, s.ts_us, s.value])?;
                        } else {
                            let mut stmt = tx.prepare_cached(LONG_INSERT)?;
                            stmt.execute(params![
                                session_id,
                                s.ts_us,
                                kind_str,
                                Option::<&str>::None,
                                Option::<&str>::None,
                                s.value,
                            ])?;
                        }
                    } else {
                        let mut stmt = tx.prepare_cached(LONG_INSERT)?;
                        for (lk, lv) in &s.labels {
                            stmt.execute(params![
                                session_id,
                                s.ts_us,
                                kind_str,
                                label_key_str(*lk),
                                lv.as_str(),
                                s.value,
                            ])?;
                        }
                    }
                }
                tx.commit()?;
                Ok(())
            })
            .await
            .context("insert batch")?;
        Ok(())
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let rows = self
            .conn
            .call(|c| {
                let mut stmt = c.prepare(
                    "SELECT id, wall_start_ms, wall_end_ms, device_id, device_platform,
                            device_model, app_bundle_id, selected_metrics, sampling_intervals
                     FROM sessions ORDER BY wall_start_ms DESC",
                )?;
                let it = stmt.query_map([], |row| {
                    let selected_json: Option<String> = row.get(7)?;
                    let intervals_json: Option<String> = row.get(8)?;
                    Ok(SessionInfo {
                        id: row.get(0)?,
                        wall_start_ms: row.get(1)?,
                        wall_end_ms: row.get(2)?,
                        device_id: row.get(3)?,
                        device_platform: row.get(4)?,
                        device_model: row.get(5)?,
                        app_bundle_id: row.get(6)?,
                        selected_metrics: parse_selected_metrics(selected_json),
                        sampling_intervals: parse_sampling_intervals(intervals_json),
                    })
                })?;
                let mut out = Vec::new();
                for r in it {
                    out.push(r?);
                }
                Ok(out)
            })
            .await
            .context("list sessions")?;
        Ok(rows)
    }

    pub async fn get_session(&self, id: i64) -> Result<Option<SessionInfo>> {
        let row = self
            .conn
            .call(move |c| {
                c.query_row(
                    "SELECT id, wall_start_ms, wall_end_ms, device_id, device_platform,
                            device_model, app_bundle_id, selected_metrics, sampling_intervals
                     FROM sessions WHERE id = ?",
                    params![id],
                    |row| {
                        let selected_json: Option<String> = row.get(7)?;
                        let intervals_json: Option<String> = row.get(8)?;
                        Ok(SessionInfo {
                            id: row.get(0)?,
                            wall_start_ms: row.get(1)?,
                            wall_end_ms: row.get(2)?,
                            device_id: row.get(3)?,
                            device_platform: row.get(4)?,
                            device_model: row.get(5)?,
                            app_bundle_id: row.get(6)?,
                            selected_metrics: parse_selected_metrics(selected_json),
                            sampling_intervals: parse_sampling_intervals(intervals_json),
                        })
                    },
                )
                .optional()
                .map_err(Into::into)
            })
            .await
            .context("get session")?;
        Ok(row)
    }

    /// Load (ts_us, value) points for one wide metric in a session.
    pub async fn load_wide_samples(
        &self,
        session_id: i64,
        kind: MetricKind,
    ) -> Result<Vec<SamplePoint>> {
        let col = wide_column(kind).ok_or_else(|| {
            anyhow::anyhow!("metric {:?} is not a wide-table column", kind)
        })?;
        let sql = format!(
            "SELECT ts_us, {col} FROM samples_wide
             WHERE session_id = ? AND {col} IS NOT NULL ORDER BY ts_us"
        );
        let rows = self
            .conn
            .call(move |c| {
                let mut stmt = c.prepare(&sql)?;
                let it = stmt.query_map(params![session_id], |row| {
                    Ok(SamplePoint {
                        ts_us: row.get(0)?,
                        value: row.get(1)?,
                    })
                })?;
                let mut out = Vec::new();
                for r in it {
                    out.push(r?);
                }
                Ok(out)
            })
            .await
            .context("load wide samples")?;
        Ok(rows)
    }

    /// Load (ts_us, label_value, value) for one labeled metric.
    pub async fn load_long_samples(
        &self,
        session_id: i64,
        kind: MetricKind,
    ) -> Result<Vec<(i64, String, f64)>> {
        let kind_str = serde_plain_kind(kind).to_string();
        let rows = self
            .conn
            .call(move |c| {
                let mut stmt = c.prepare(
                    "SELECT ts_us, COALESCE(label_value, ''), value
                     FROM samples_long
                     WHERE session_id = ? AND kind = ? ORDER BY ts_us",
                )?;
                let it = stmt.query_map(params![session_id, kind_str], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, f64>(2)?))
                })?;
                let mut out = Vec::new();
                for r in it {
                    out.push(r?);
                }
                Ok(out)
            })
            .await
            .context("load long samples")?;
        Ok(rows)
    }

    pub async fn delete_session(&self, id: i64) -> Result<()> {
        self.conn
            .call(move |c| {
                let tx = c.transaction()?;
                tx.execute("DELETE FROM samples_long WHERE session_id = ?", params![id])?;
                tx.execute("DELETE FROM samples_wide WHERE session_id = ?", params![id])?;
                tx.execute("DELETE FROM markers WHERE session_id = ?", params![id])?;
                tx.execute("DELETE FROM sessions WHERE id = ?", params![id])?;
                tx.commit()?;
                Ok(())
            })
            .await
            .context("delete session")?;
        Ok(())
    }

    /// Insert a user marker. Returns the new id.
    pub async fn insert_marker(
        &self,
        session_id: i64,
        ts_us: i64,
        label: Option<String>,
        created_at_ms: i64,
    ) -> Result<i64> {
        let id = self
            .conn
            .call(move |c| {
                c.execute(
                    "INSERT INTO markers (session_id, ts_us, label, created_at_ms)
                     VALUES (?, ?, ?, ?)",
                    params![session_id, ts_us, label, created_at_ms],
                )?;
                Ok(c.last_insert_rowid())
            })
            .await
            .context("insert marker")?;
        Ok(id)
    }

    pub async fn list_markers(&self, session_id: i64) -> Result<Vec<Marker>> {
        let rows = self
            .conn
            .call(move |c| {
                let mut stmt = c.prepare(
                    "SELECT id, session_id, ts_us, label, created_at_ms
                     FROM markers WHERE session_id = ? ORDER BY ts_us",
                )?;
                let it = stmt.query_map(params![session_id], |row| {
                    Ok(Marker {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        ts_us: row.get(2)?,
                        label: row.get(3)?,
                        created_at_ms: row.get(4)?,
                    })
                })?;
                let mut out = Vec::new();
                for r in it {
                    out.push(r?);
                }
                Ok(out)
            })
            .await
            .context("list markers")?;
        Ok(rows)
    }

    pub async fn delete_marker(&self, id: i64) -> Result<()> {
        self.conn
            .call(move |c| {
                c.execute("DELETE FROM markers WHERE id = ?", params![id])?;
                Ok(())
            })
            .await
            .context("delete marker")?;
        Ok(())
    }

    /// Update only the timestamp (ts_us offset from session start). Used
    /// for the drag-to-edit interaction in the chart UI.
    pub async fn update_marker_ts(&self, id: i64, ts_us: i64) -> Result<()> {
        self.conn
            .call(move |c| {
                c.execute(
                    "UPDATE markers SET ts_us = ? WHERE id = ?",
                    params![ts_us, id],
                )?;
                Ok(())
            })
            .await
            .context("update marker ts_us")?;
        Ok(())
    }

    /// Update only the label. `None`/empty stores SQL NULL (no label).
    pub async fn update_marker_label(&self, id: i64, label: Option<String>) -> Result<()> {
        let label = label.and_then(|s| {
            let t = s.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        });
        self.conn
            .call(move |c| {
                c.execute(
                    "UPDATE markers SET label = ? WHERE id = ?",
                    params![label, id],
                )?;
                Ok(())
            })
            .await
            .context("update marker label")?;
        Ok(())
    }
}

// ---- column / serialization helpers ----

const WIDE_UPSERT_TEMPLATE: &str = "
    INSERT INTO samples_wide (session_id, ts_us, __COL__)
    VALUES (?, ?, ?)
    ON CONFLICT(session_id, ts_us) DO UPDATE SET __COL__ = excluded.__COL__
";

const LONG_INSERT: &str = "
    INSERT INTO samples_long (session_id, ts_us, kind, label_key, label_value, value)
    VALUES (?, ?, ?, ?, ?, ?)
";

/// Decode the JSON-encoded `selected_metrics` column. A row written
/// pre-migration-5 returns `None` (the column is NULL); a row whose
/// JSON is corrupt also returns `None` with a warning rather than
/// surfacing the error — losing the snapshot just degrades to
/// "show all metrics that have data", which is the legacy default.
fn parse_selected_metrics(raw: Option<String>) -> Option<Vec<String>> {
    let s = raw?;
    match serde_json::from_str::<Vec<String>>(&s) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(error = %e, raw = %s, "selected_metrics JSON parse failed; treating as 'show all'");
            None
        }
    }
}

/// Mirror of `parse_selected_metrics` for the per-metric interval
/// snapshot. Same graceful-degradation policy: corrupt JSON drops to
/// `None`, treated downstream as "this session predates frequency
/// configurability".
fn parse_sampling_intervals(raw: Option<String>) -> Option<std::collections::HashMap<String, u64>> {
    let s = raw?;
    match serde_json::from_str::<std::collections::HashMap<String, u64>>(&s) {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::warn!(error = %e, raw = %s, "sampling_intervals JSON parse failed; treating as 'default frequencies'");
            None
        }
    }
}

/// Returns the wide-table column name for an unlabeled metric, or None
/// if the metric has no dedicated column (and should go to samples_long).
fn wide_column(kind: MetricKind) -> Option<&'static str> {
    use MetricKind::*;
    Some(match kind {
        CpuTotalPct => "cpu_total_pct",
        CpuAppPct => "cpu_app_pct",
        CpuTempC => "cpu_temp_c",
        MemSystemUsedBytes => "mem_system_used_bytes",
        MemAppPssBytes => "mem_app_pss_bytes",
        Fps => "fps",
        FrameTimeMs => "frame_time_ms",
        SmallJankCount => "small_jank_count",
        JankCount => "jank_count",
        BigJankCount => "big_jank_count",
        Stutter => "stutter",
        GpuTilerPct => "gpu_tiler_pct",
        GpuRendererPct => "gpu_renderer_pct",
        GpuDevicePct => "gpu_device_pct",
        BatteryLevelPct => "battery_level_pct",
        BatteryTempC => "battery_temp_c",
        BatteryVoltageMv => "battery_voltage_mv",
        BatteryCurrentMa => "battery_current_ma",
        // Labeled / per-entity metrics → long table:
        CpuCorePct | CpuFreqMhz | NetUpBytes | NetDownBytes | ThreadCpuPct => return None,
    })
}

fn serde_plain_kind(k: MetricKind) -> &'static str {
    // Mirror mperf_schema::MetricKind serde rename_all = "snake_case".
    use MetricKind::*;
    match k {
        CpuTotalPct => "cpu_total_pct",
        CpuAppPct => "cpu_app_pct",
        CpuCorePct => "cpu_core_pct",
        CpuFreqMhz => "cpu_freq_mhz",
        CpuTempC => "cpu_temp_c",
        MemSystemUsedBytes => "mem_system_used_bytes",
        MemAppPssBytes => "mem_app_pss_bytes",
        Fps => "fps",
        FrameTimeMs => "frame_time_ms",
        SmallJankCount => "small_jank_count",
        JankCount => "jank_count",
        BigJankCount => "big_jank_count",
        Stutter => "stutter",
        GpuTilerPct => "gpu_tiler_pct",
        GpuRendererPct => "gpu_renderer_pct",
        GpuDevicePct => "gpu_device_pct",
        NetUpBytes => "net_up_bytes",
        NetDownBytes => "net_down_bytes",
        BatteryLevelPct => "battery_level_pct",
        BatteryTempC => "battery_temp_c",
        BatteryVoltageMv => "battery_voltage_mv",
        BatteryCurrentMa => "battery_current_ma",
        ThreadCpuPct => "thread_cpu_pct",
    }
}

fn label_key_str(k: LabelKey) -> &'static str {
    use LabelKey::*;
    match k {
        Pid => "pid",
        Tid => "tid",
        CoreIdx => "core_idx",
        Iface => "iface",
        PowerSupply => "power_supply",
        Layer => "layer",
    }
}
