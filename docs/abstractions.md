# Core abstractions

> Status: **canonical**. Any PR that violates these rules should be rejected
> or the rule should be amended here first.
>
> This document is the contract that lets us build Android first without
> repainting the codebase when iOS lands. Every choice below is justified by
> "if we don't do this, iOS or multi-device will force a rewrite later".

## Goals

- One stable data shape that fits Android (pull-based ADB) and iOS (push-based DTX).
- One time base that survives multi-device sessions and offline replay.
- Errors that the UI can recover from (vs panic).
- A small surface — we **deliberately don't** abstract everything; see §10.

## 1 · Sampler trait (stream-first, not poll-first)

**Rule**: the sampler boundary is a stream, not a "give me one sample" call.

```rust
use futures_core::stream::BoxStream;

#[async_trait::async_trait]
pub trait Sampler: Send {
    /// Stable identifier, e.g. "android.cpu", "ios.fps".
    fn name(&self) -> &'static str;

    /// Hint for the scheduler / UI. Returned stream MAY violate it
    /// (e.g. iOS FPS arrives per-frame; we just record it).
    fn target_hz(&self) -> f32 { 1.0 }

    /// Begin sampling. The returned stream lives for the session.
    /// Implementors MUST:
    ///   - Stamp `ts_us` from the shared monotonic clock (see §3).
    ///   - Surface device disconnects as `SamplerError::DeviceDisconnected`.
    ///   - Honor cancellation: drop the stream → stop the sampler.
    async fn start(&mut self, ctx: SamplerCtx)
        -> Result<BoxStream<'static, Result<Sample, SamplerError>>, SamplerError>;
}

pub struct SamplerCtx {
    pub clock: Clock,                // shared monotonic clock (§3)
    pub session_id: u64,
    pub target: Option<AppTarget>,   // None = system-wide samplers
}
```

**Why**: Android samplers internally `tokio::time::interval`, transform into a
stream, and emit. iOS samplers register a DTX channel callback that pushes onto
an unbounded channel and emit the receiver as a stream. The scheduler treats
both identically with `select_all`. **Never** add a `fn sample_once()` shortcut —
it will leak Android assumptions into iOS code paths.

## 2 · Sample is long-format, not wide-format

**Rule**: a `Sample` carries **one number** plus enough labels to disambiguate.
Pivoting to a wide row happens only at the storage boundary.

```rust
pub struct Sample {
    pub ts_us: i64,                  // §3
    pub device_ts_us: Option<i64>,   // §3
    pub kind: MetricKind,
    pub value: f64,
    pub labels: Labels,              // small inline-friendly map
}

pub type Labels = smallvec::SmallVec<[(LabelKey, String); 2]>;

#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub enum LabelKey {
    Pid,
    Tid,
    CoreIdx,
    Iface,
    PowerSupply,
    Layer,        // Android SurfaceFlinger layer name (FPS sampler)
    // Add new keys here; do NOT use free-form strings.
}
```

**Why**:

- FPS samples at frame rate, CPU at 1 Hz, GPU counters in bursts. A single
  wide row can't be written until _all_ metrics for that tick arrive, which
  forces buffering and gates the slowest sampler.
- Multi-core CPU, per-thread CPU, per-interface network are intrinsically
  labeled. Long format makes that natural; wide tables would need
  `cpu_core_0_pct ... cpu_core_15_pct` columns.
- Storage layer (§7) pivots to wide form at write time. Pivot in one place,
  exactly once.

## 3 · Time has two axes; the primary is host monotonic

**Rule**: every `Sample.ts_us` is microseconds on a **shared host monotonic
clock**, not `SystemTime`, not device time. Device time goes into
`device_ts_us` for diagnostics only.

```rust
#[derive(Clone)]
pub struct Clock { /* opaque: wraps Instant of session origin */ }

impl Clock {
    pub fn session_origin() -> Self { /* call once when session starts */ }
    pub fn now_us(&self) -> i64    { /* (Instant::now - origin).as_micros() as i64 */ }
}
```

The `Clock` is created at session start and passed via `SamplerCtx` to every
sampler. Multi-device sessions share one clock — that's how we get cross-device
alignment for free.

**Why not `SystemTime`**: laptop sleep / NTP jumps will corrupt the timeline.
**Why not device time**: device clocks drift, can jump across reboots, and
DTX exposes Mach time while ADB exposes boottime — incompatible.
**Why we still keep device time**: useful when correlating with crash logs,
sysmontap timestamps, etc.

## 4 · Identity: DeviceRef + AppTarget, no platform-special-cased IDs

**Rule**: the rest of the codebase sees one device-identity type and one
app-target type. Platform peculiarities (UDID format, ADB serial, iOS 17+
tunnel address) live inside the platform crates.

```rust
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct DeviceRef {
    pub platform: Platform,   // Android | Ios
    pub id: String,           // ADB serial OR iOS UDID — opaque outside platform crates
}

#[derive(Clone)]
pub struct AppTarget {
    pub device: DeviceRef,
    pub bundle_id: String,    // "com.foo.bar" on both Android and iOS
    pub pid: Option<i32>,     // discovered at runtime
}
```

**Convention**: we call the package id `bundle_id` on both platforms. Android's
"package name" and iOS's "bundle identifier" are isomorphic in our model.
Naming them differently anywhere outside the Android crate is forbidden.

## 5 · Errors are typed; `anyhow::Error` only at the very top

**Rule**: samplers and platform crates return `SamplerError`. UI gets to
branch on the variant. `anyhow` is acceptable only inside a sampler's own
private helpers, never at the trait boundary.

```rust
#[derive(thiserror::Error, Debug)]
pub enum SamplerError {
    #[error("device disconnected: {0}")]
    DeviceDisconnected(String),

    #[error("permission denied: {0}")]
    PermissionDenied(String),         // adb root needed, DDI not mounted

    #[error("app not running: {0}")]
    AppNotRunning(String),

    #[error("transient io: {0}")]
    TransientIo(String),              // retriable; scheduler may auto-retry

    #[error("fatal: {0}")]
    Fatal(#[from] anyhow::Error),     // bug / unsupported OS / etc.
}
```

UI policy (lives in `crates/core`):

- `DeviceDisconnected` → mark device offline, stop session, surface toast.
- `PermissionDenied` → modal explaining what to enable, do not auto-retry.
- `AppNotRunning` → keep session running, gray out app-targeted metrics.
- `TransientIo` → exponential backoff up to N retries, then escalate.
- `Fatal` → log, stop session, "report an issue" link.

## 6 · Scheduler is dumb and central

**Rule**: there is exactly **one** scheduler in `crates/core`. Samplers do not
spawn their own tasks for emit timing. The scheduler:

1. Holds a `Vec<Box<dyn Sampler>>` for the session.
2. Calls `start()` on each, gets `N` streams.
3. `stream::select_all` merges them into one stream of `Sample`.
4. Pushes onto a bounded mpsc to the **writer task** (§7).
5. Tees a copy to the **live-view broadcaster** for the UI.

Implication: samplers do not know about storage, IPC, or UI. They only know
how to emit `Sample`s.

## 7 · Storage boundary: long → wide happens here, nowhere else

**Rule**: SQLite is the only persistence. The writer task pivots unlabeled
samples into a wide row, batches, and inserts. **No other code may write
to the DB.**

```
sessions(id, wall_start_ms, wall_end_ms, device_id, device_platform,
         device_model, app_bundle_id, meta_json)

samples_wide(session_id FK, ts_us,
             cpu_total_pct, cpu_app_pct, cpu_temp_c,
             mem_system_used_bytes, mem_app_pss_bytes,
             fps, frame_time_ms,
             small_jank_count, jank_count, big_jank_count, stutter,
             gpu_tiler_pct, gpu_renderer_pct, gpu_device_pct,
             battery_level_pct, battery_temp_c,
             battery_voltage_mv, battery_current_ma,
             PRIMARY KEY (session_id, ts_us))

samples_long(session_id FK, ts_us, kind, label_key, label_value, value)
  -- one row per labeled emission; index on (session_id, ts_us)
  -- and (session_id, kind, ts_us)

markers(id PK, session_id FK CASCADE, ts_us, label, created_at_ms)
```

Pivot rule (simpler than originally specified — no grace window):

- Unlabeled metric → upsert into `samples_wide`:
  `INSERT ... ON CONFLICT(session_id, ts_us) DO UPDATE SET <col> = excluded.<col>`.
  Different samplers emit at the same `ts_us` only when their `Clock`s
  agree, which is guaranteed because all samplers share one `Clock` per
  session (§3). Out-of-order arrivals just upsert another column into
  the same row.
- Labeled metric (`CpuCorePct`, `ThreadCpuPct`, `CpuFreqMhz`,
  `NetUpBytes`, `NetDownBytes`) → one INSERT per label-value into
  `samples_long`. We never explode to per-label columns — core count,
  thread count, interface count vary per device.

The closed-enum match `wide_column(MetricKind) -> Option<&str>` in
`crates/storage/src/lib.rs` decides which table a metric lands in. The
compiler enforces exhaustiveness when a new `MetricKind` variant is
added.

**All this logic is in `crates/storage` only.**

Migrations live in `crates/storage/src/schema.rs` as a
`&[(version, &str)]` array. Each runs in its own `c.transaction()` so the
DDL apply and the `schema_version` row are atomic — power loss between
the two would otherwise re-apply the migration on next launch and the
non-idempotent `CREATE TABLE` would fail. Current head: **v4**.

Downgrade is rejected loudly. If the on-disk `schema_version` exceeds
`HEAD`, `run_migrations` returns a `SqliteFailure` explaining that the
DB was written by a newer build and asks the user to upgrade or wipe.
Silent no-op (the previous behavior) would defer the failure to the
first query against a column the older schema doesn't have, producing
opaque runtime errors.

`finish_session` is idempotent. Both the writer task (broadcast-close
auto-finalize) and `Session::stop` (manual finalize) call it; the
`WHERE wall_end_ms IS NULL` clause keeps the first-arriving timestamp.

## 8 · UI contract: pull from storage, not from samplers

**Rule**: the UI subscribes to two things:

- A **historic query** API (`get_samples(session, range, kinds)`) for charts
  and reports.
- A **live tail** broadcast (the scheduler's tee) for in-progress sessions.

The UI must never hold a stream from a sampler directly. This keeps replay
("open old session") and live ("recording now") on the same code path — the
chart component does not care whether the data source is SQLite or live.

## 9 · Concurrency model

- **Tokio multi-thread runtime** in the Rust process.
- **Bounded** channels everywhere (capacity 1024). Backpressure is real — if
  the writer is slow, samplers slow down rather than OOM.
- One scheduler task per session. Multiple sessions = multiple schedulers.
- No globals. Everything threaded through `SamplerCtx` / session handle.

## 10 · What we deliberately DO NOT abstract (yet)

Anti-feature list. Adding any of these requires a doc amendment first.

| Thing                          | Why not                                                            |
| ------------------------------ | ------------------------------------------------------------------ |
| Plugin loading at runtime      | Samplers are compiled-in; recompile is fine for Phase 0–1.         |
| Multiple storage backends      | SQLite only. Don't add a Storage trait.                            |
| Multiple IPC serializers       | Tauri IPC + JSON. No protobuf, no msgpack.                         |
| Multiple report renderers      | One HTML template. PDF = headless Chromium of the same template.   |
| Multi-device per session       | Phase 0–1 is single device. Multi-device is an explicit milestone. |
| Custom sampling rates per user | Hardcoded per-sampler defaults. Tunable later.                     |
| Live remote control / cloud    | Local app only. Web platform is a separate product.                |

If you find yourself wanting one of these in Phase 0–1, you're almost
certainly solving the wrong problem.

## 11 · Open questions (resolve during iOS spike, Week 3)

- Does iOS DTX expose a usable monotonic clock for `device_ts_us`, or only
  Mach absolute time? If the latter, what's our conversion?
- Does py-ios-device tunnel survive USB cable hiccups, or do we need to
  expose a "reconnecting" sub-state of `DeviceDisconnected`?
- iOS GPU counters arrive in bursts of 30+ values at once. Does our 200ms
  bucket grace window survive that, or do we need a per-kind grace?
- Per-thread CPU on Android is `/proc/<pid>/task/<tid>/stat`; iOS thread info
  comes from the activity tracing tap. Are the dimensions comparable enough
  to share a `MetricKind::ThreadCpuPct`, or do we need separate kinds?

These are explicitly **not** answered upfront. The Week 3 iOS spike exists to
answer them with real data. If a question turns out to break an abstraction
above, we amend this doc; we do not paper over it in code.

## 12 · Change process

Amending this doc:

1. Open a PR that edits this file _only_.
2. Get it reviewed; merge.
3. \_Then_this doc:

4. Open a PR that edits this file _only_.
5. Get it reviewed; merge.
6. _Then_ open the code PR that depends on the new rule.

This forces the "why" to be written down before the code lands. Skipping the
doc PR is a smell — usually means you don't yet know if the change is right.
