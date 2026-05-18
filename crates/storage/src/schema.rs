//! Schema versioning. We keep this dead-simple: a `schema_version` row and
//! sequentially-applied SQL strings.
//!
//! Each migration runs in its own transaction so a crash / power loss
//! between the DDL apply and the version-record insert can't leave the
//! schema half-applied. (Without that, a re-run would hit the migration
//! body again and the `CREATE TABLE` / `ALTER TABLE` would fail with
//! "already exists" — none of the statements here use `IF NOT EXISTS`.)
//! `unwrap_or` is deliberately avoided on the version query: real DB
//! errors should surface, not be silently rewritten as "version 0,
//! please re-apply migration 1".

use rusqlite::{params, Connection};

/// Highest schema version known to this build. Surfaced via the
/// Settings tab so users can sanity-check the on-disk DB after a
/// migration.
pub const HEAD: i32 = 4;

pub fn run_migrations(c: &mut Connection) -> rusqlite::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY)")?;

    let cur: i32 = c.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |row| row.get(0),
    )?;

    // Downgrade guard. If the on-disk DB was written by a NEWER build
    // (cur > HEAD), this older binary doesn't know about whatever
    // tables / columns the newer migrations added. Continuing would
    // silently work for a while (older queries don't touch the new
    // schema) but break the moment we try to write to a new column or
    // the user opens a session recorded under the new schema.
    // Surface a loud error here instead of going on.
    if cur > HEAD {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!(
                "database schema version {cur} is newer than this build supports ({HEAD}); \
                 the DB was likely created by a newer mperf release. \
                 Upgrade the app, or move/delete the data.db to start fresh."
            )),
        ));
    }

    for (v, sql) in MIGRATIONS.iter() {
        if *v > cur {
            tracing::info!(version = v, "applying migration");
            let tx = c.transaction()?;
            tx.execute_batch(sql)?;
            tx.execute("INSERT INTO schema_version (version) VALUES (?)", params![v])?;
            tx.commit()?;
        }
    }
    Ok(())
}

const MIGRATIONS: &[(i32, &str)] = &[
    (
        1,
        "
        CREATE TABLE sessions (
            id                INTEGER PRIMARY KEY AUTOINCREMENT,
            wall_start_ms     INTEGER NOT NULL,
            wall_end_ms       INTEGER,
            device_id         TEXT NOT NULL,
            device_platform   TEXT NOT NULL,
            device_model      TEXT,
            app_bundle_id     TEXT,
            meta_json         TEXT
        );
        CREATE INDEX idx_sessions_start ON sessions(wall_start_ms DESC);

        CREATE TABLE samples_wide (
            session_id              INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            ts_us                   INTEGER NOT NULL,
            cpu_total_pct           REAL,
            cpu_app_pct             REAL,
            cpu_temp_c              REAL,
            mem_system_used_bytes   INTEGER,
            mem_app_pss_bytes       INTEGER,
            fps                     REAL,
            frame_time_ms           REAL,
            gpu_tiler_pct           REAL,
            gpu_renderer_pct        REAL,
            gpu_device_pct          REAL,
            battery_level_pct       REAL,
            battery_temp_c          REAL,
            battery_voltage_mv      INTEGER,
            battery_current_ma      INTEGER,
            PRIMARY KEY (session_id, ts_us)
        );

        CREATE TABLE samples_long (
            session_id     INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            ts_us          INTEGER NOT NULL,
            kind           TEXT NOT NULL,
            label_key      TEXT,
            label_value    TEXT,
            value          REAL NOT NULL
        );
        CREATE INDEX idx_samples_long_session ON samples_long(session_id, ts_us);
        CREATE INDEX idx_samples_long_kind ON samples_long(session_id, kind, ts_us);
        ",
    ),
    (
        2,
        "
        ALTER TABLE samples_wide ADD COLUMN jank_count INTEGER;
        ALTER TABLE samples_wide ADD COLUMN big_jank_count INTEGER;
        ",
    ),
    (
        3,
        "
        ALTER TABLE samples_wide ADD COLUMN small_jank_count INTEGER;
        ALTER TABLE samples_wide ADD COLUMN stutter REAL;
        ",
    ),
    (
        4,
        "
        CREATE TABLE markers (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id      INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            ts_us           INTEGER NOT NULL,
            label           TEXT,
            created_at_ms   INTEGER NOT NULL
        );
        CREATE INDEX idx_markers_session ON markers(session_id, ts_us);
        ",
    ),
];
