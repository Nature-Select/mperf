# Architecture

Companion to `docs/abstractions.md`. That doc is the **contract** (rules a PR
must not break). This doc is the **map** — where the code lives and how
data moves at runtime. Read abstractions.md for "why"; read this for "where".

## High level

```
+----------------------------------------------------------------+
|  React 19 UI (Arco + uPlot)            apps/desktop/src/       |
|    Live tab  ───── live tail (event)                           |
|    History tab ── historic query (invoke)                      |
+--------------------------- Tauri IPC --------------------------+
|  Tauri 2 shell                         apps/desktop/src-tauri/ |
|    lib.rs ── builder + invoke_handler                          |
|    setup.rs ─ tracing / panic / adb sidecar / Win DLLs         |
|    session.rs  AppState + Session + writer + UI pump           |
|    commands.rs #[tauri::command] handlers                      |
+----------------------------------------------------------------+
|  Rust workspace (crates/)                                      |
|  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────────┐    |
|  │  core    │  │ android  │  │   ios    │  │   storage    │    |
|  │ Scheduler│  │ samplers │  │ samplers │  │ SQLite (WAL) │    |
|  │ (select_ │  │ (adb)    │  │ (idevice │  │ samples_wide │    |
|  │  all +   │  │          │  │  DTX)    │  │ samples_long │    |
|  │  bcast)  │  │          │  │          │  │ markers      │    |
|  └────┬─────┘  └────┬─────┘  └────┬─────┘  └──────┬───────┘    |
|       │             │             │               │            |
|       └─────────────┴─────────────┴───────────────┘            |
|                        │                                       |
|                  ┌─────┴──────┐                                |
|                  │   schema   │ (Sampler trait, Sample,        |
|                  │            │  MetricKind, Clock, errors)    |
|                  └────────────┘                                |
+----------------------------------------------------------------+
       │                          │
   adb binary                idevice (pure Rust)
   (bundled sidecar         no Python, no sudo, no daemon
    or system PATH)
       │                          │
   Android device (USB         iOS device (USB only; Wi-Fi
   or wifi-adb)                paired devices flagged unusable)
```

No Python. No external pymobiledevice / py-ios-device / go-ios. `idevice`
0.1.61 talks to iOS over CoreDeviceProxy → RemoteServerClient (DTX) directly.

## Crate roles

| Crate | Depends on | Role |
|---|---|---|
| `schema` | — (leaf) | `Sampler` trait, `Sample`, `MetricKind`, `LabelKey`, `Clock`, `SamplerError`, `SessionId`, `AppTarget`. Reexported as `mperf_schema`. |
| `storage` | schema | SQLite via `tokio-rusqlite`. Owns the long→wide pivot, migrations, marker CRUD. Only place that touches the DB. |
| `android` | schema | adb-driven samplers: cpu, fps, memory, gpu, temperature, battery; device + app enumeration. Shells out to the bundled `adb` binary. |
| `ios` | schema | `idevice`-driven samplers: cpu (sysmontap_raw), graphics (graphics_raw), apps, devices. iOS battery/temperature are present but `#[allow(dead_code)]` until DTX power channel lands. |
| `core` | schema | Central scheduler. One `select_all` over all sampler streams, one tokio task per session, broadcast channel for live UI. |

Crate graph is a DAG with `schema` at the leaf. Adding a new metric only
forces edits in `schema` (kind variant) + `storage` (wide column +
migration) + a sampler + frontend; the compiler's exhaustiveness check on
`MetricKind` finds the rest.

## Desktop shell (apps/desktop/src-tauri/src/)

Split intentionally — `lib.rs` used to be 720 lines:

- `main.rs` — entry, calls `lib::run`.
- `lib.rs` — `tauri::Builder`, `invoke_handler` list, `setup` hook wiring.
- `setup.rs` — tracing init (exactly once, see CLAUDE.md pitfalls), panic
  hook writing to `$TMPDIR/mperf-panic.log`, bundled adb resolution,
  Windows DLL staging next to `adb.exe`.
- `session.rs` — `AppState`, `Session`, `start_recording`, the writer
  task (drains the scheduler broadcast and calls
  `storage::insert_sample_batch` in batches), the UI pump task (forwards
  live samples to the frontend over a Tauri event), and the
  `spawn_exit_watcher_task` (waits on `SchedulerHandle::exit_notify()`
  and emits `EVENT_SESSION_ENDED` when the scheduler dies naturally —
  see "Session-ended notification paths" below). Owns the
  `Arc<AtomicBool> user_stopping` flag that prevents user Stop from
  surfacing as "ended automatically" and gates both the UI pump's
  broadcast-close branch AND the exit watcher.
- `commands.rs` — thin `#[tauri::command]` handlers. Delegate to crates or
  to `Session`. No business logic.

## Session-ended notification paths

There are three independent places that can detect a session ended; they
coordinate via `Arc<AtomicBool> user_stopping` so the frontend toasts at
most once per session.

```
                      Scheduler task ────────────► exit_notify (Arc<Notify>)
                          │                              │
              ┌───────────┴──────────────┐               │
              │ broadcast(Sample)        │               │
              ▼                          ▼               ▼
       writer_task             spawn_ui_pump_task  spawn_exit_watcher_task
       (records data)          (forwards samples)  (emits EVENT_SESSION_ENDED
              │                       │             when scheduler exits AND
              ▼                       ▼             user_stopping=false)
       auto-finish_session     emits EVENT_SESSION_
       on Closed               ENDED on broadcast
                               close AND user_stopping=false
```

| Trigger | Path | Latency |
|---|---|---|
| User clicks Stop | `Session::stop` sets `user_stopping=true`, then moves out `scheduler` (dropping `live_tx`), drains writer. Both watcher and pump branches see `user_stopping=true` → no event emitted; the frontend handles state from its own `setActiveSessionId(null)` | immediate |
| Sampler returns non-retriable error (USB unplug, perms, etc.) | scheduler task breaks loop → `notify_one()` on exit_notify → `spawn_exit_watcher_task` (a) emits `EVENT_SESSION_ENDED`, (b) takes the matching `Session` out of `AppState.session` and runs `Session::stop` so the writer drains and `finish_session` fires | ~50ms after sampler error surfaces |
| User force-quits app mid-session | next startup's orphan-cleanup pass in `Storage::open` finalizes any session with `wall_end_ms IS NULL` | next launch |

The reason the broadcast-close path in `spawn_ui_pump_task` doesn't
catch natural exits on its own: `SchedulerHandle` holds a `live_tx`
sender clone (it needs one to back `subscribe()`), so the broadcast
channel only closes when the handle itself drops — which only happens
inside `Session::stop`. By then `user_stopping=true` so the pump
suppresses the emit. The pump branch is retained as a no-op safety net.

Why the exit watcher **also** tears down the session backend-side, not
just emit the event: same `live_tx` clone problem in reverse. Without
backend tearing down, the natural-exit flow only emits to the frontend
— `Session` stays in `AppState.session`, holding the handle, holding
the `live_tx` clone, so the writer task keeps awaiting `rx.recv()`
forever and `finish_session` never runs → `wall_end_ms` stays NULL →
the History tab shows the row as "in progress" forever. The earlier
device-list watchdog accidentally papered over this by calling
`stop_session` 1–3s later; with the toast now arriving immediately
through EVENT_SESSION_ENDED, the watchdog stops firing
(`recording=false`) and the cleanup has to come from the watcher
itself. The watcher's `db_id` equality guard handles the races: a
concurrent user Stop wins via `mutex.lock().take()`; a concurrent user
Start replaces the session with one having a different `db_id`, so the
watcher sees a mismatch and no-ops.

## Data flow during a recording

```
sampler.start(ctx) ─► BoxStream<Result<Sample, SamplerError>>
       │
       ▼
select_all(streams) ─► one merged stream in crates/core
       │
       ▼
broadcast::Sender<Sample>  (capacity 1024, bounded → backpressure)
       │
       ├──► writer task (session.rs) ──► storage.insert_sample_batch
       │                                        │
       │                                        ▼
       │                                  SQLite (WAL):
       │                                  ├ unlabeled metric → samples_wide upsert
       │                                  │   (ON CONFLICT(session_id, ts_us)
       │                                  │    DO UPDATE — see §"Pivot" below)
       │                                  └ labeled metric (CpuCorePct, ThreadCpuPct,
       │                                      Net*, CpuFreqMhz) → samples_long INSERT
       │
       └──► UI pump task (session.rs) ──► Tauri Event "live-sample"
                                                │
                                                ▼
                                         React (Live tab):
                                         per-chart ring buffer
                                         + uPlot.setData() each tick
```

History tab uses a completely different path: it calls
`#[tauri::command] load_session_*` which hit `storage::load_wide_samples` /
`load_long_samples` directly. The chart components don't know whether they
are reading from SQLite or from the live broadcast — same `(ts_us, value)`
shape both ways. That's the "UI pulls from storage, not from samplers"
rule in abstractions.md §8.

## Pivot: long → wide

Sample format on the wire is **long** (one value + labels per row). The
storage layer pivots to **wide** only for unlabeled metrics that share an
exact `(session_id, ts_us)` key:

- Unlabeled metric (`CpuTotalPct`, `Fps`, `MemAppPssBytes`, etc.) → one
  column in `samples_wide`. Multiple unlabeled metrics for the same
  `ts_us` collapse into one row via `INSERT ... ON CONFLICT(session_id,
  ts_us) DO UPDATE SET <col> = excluded.<col>`. No grace-window
  buffering — different samplers can arrive in any order; the upsert
  fills in columns as they come. The `(session_id, ts_us)` PRIMARY KEY
  is enough to keep one row per tick.
- Labeled metric (`CpuCorePct` per `core_idx`, `ThreadCpuPct` per `tid`,
  `NetUpBytes` per `iface`, `CpuFreqMhz` per core) → one row per
  label-value in `samples_long`. We do **not** explode labeled metrics
  into `cpu_core_0_pct, cpu_core_1_pct, ...` columns — core count varies
  per device.

`wide_column()` in `crates/storage/src/lib.rs` is the closed-enum match
that decides which table a metric goes to. The Rust compiler enforces
exhaustiveness — when you add a `MetricKind` variant, that match is
where the compiler will tell you to choose.

Note: this is simpler than abstractions.md §7 originally described
(no 200ms grace window). The ON CONFLICT upsert is sufficient because
each sampler stamps `ts_us` from the same `Clock` (§3 of abstractions),
so colliding rows are intentional, not race conditions.

## Schema versions

`crates/storage/src/schema.rs` keeps migrations as a `&[(version, &str)]`,
each in its own `c.transaction()` so the DDL apply and the
`schema_version` insert are atomic. Current head: **v4**.

- v1 — `sessions`, `samples_wide`, `samples_long`, indexes.
- v2 — `samples_wide.jank_count`, `.big_jank_count`.
- v3 — `samples_wide.small_jank_count`, `.stutter`.
- v4 — `markers` table (id, session_id FK CASCADE, ts_us, label, created_at_ms).

The version probe uses `?` propagation (not `unwrap_or(0)`) — see CLAUDE.md
`REGRESSION:schema-migration-atomicity`.

`run_migrations` rejects on-disk versions > `HEAD` with a loud
`SqliteFailure`. Reason: if the user runs a newer build (writes
schema_version=5), then downgrades to an older binary (HEAD=4), the
older binary's migration loop is a no-op (`*v > cur` matches nothing),
but its queries reference columns the older schema doesn't have —
opaque "no such column" errors at runtime. Better to refuse to open
the DB and tell the user to upgrade or wipe.

`finish_session` is idempotent: `UPDATE sessions SET wall_end_ms = ?
WHERE id = ? AND wall_end_ms IS NULL`. Both the writer task's
auto-finalize on broadcast close and `Session::stop`'s manual finalize
reach it; without the guard the second call overwrites the first
timestamp by 1–300ms, reporting "cleanup finished" rather than
"sampling stopped".

## Markers (annotations)

Stored in their own table because they aren't time-series data: each
marker has a stable `id` and is user-editable (label, ts_us via drag).
The frontend `MarkerOverlay` renders one absolute-positioned HTML strip
per marker over every uPlot chart and converts the marker's `ts_us`
offset to chart x using `wallStartSec + ts_us / 1e6`.

Five commands surface the table to the UI (in `commands.rs`):
`add_marker`, `list_session_markers`, `delete_marker`, `update_marker`
(drag), `update_marker_label`. All chart components receive a single
`markers?: MarkerControls` prop bundling the list + four callbacks — see
`lib/markers.ts`.

## Time

One `Clock` per session, created at session start in
`Scheduler::start`, passed via `SamplerCtx` to every sampler. All
`Sample.ts_us` are microseconds since session origin on the host
monotonic clock. Device-side timestamps go into `device_ts_us` for
diagnostics. Never join two sessions on `device_ts_us`. See
abstractions.md §3.

## Platform-specific entry points

**Android** (`crates/android/`)
- `adb.rs` — process spawning, shell escaping, `MPERF_ADB_PATH` env
  resolution, `is_safe_pkg_name` defense-in-depth.
- Samplers `tokio::time::interval` internally and wrap into a stream.
- FPS sampler merges two sources (`dumpsys gfxinfo` for UI thread,
  `dumpsys SurfaceFlinger --latency <layer>` for SurfaceView games) and
  takes the max — see CLAUDE.md pitfalls for why both are needed.

**iOS** (`crates/ios/`)
- `connect.rs` — usbmuxd → lockdown → CoreDeviceProxy → RemoteServerClient.
- `sysmontap_raw.rs` — bypass for `idevice::SysmontapClient` (upstream
  drops second-and-later rows from each push, see CLAUDE.md). Decodes
  positional proc-attr arrays, captures `PerCPUUsage`, `System` row.
- `graphics_raw.rs` — bypass for `idevice::GraphicsClient` (it parses
  Tiler/Renderer/Device fields and drops them). Reuses the same
  RemoteServerClient as sysmontap.
- HRTB workaround for `RsdService` trait — resolve
  `com.apple.instruments.dtservicehub` port manually from
  `handshake.services`.

## Concurrency

- Tokio multi-thread runtime in the Rust process.
- One scheduler task per session. Currently we only run one session at
  a time (UI enforces it); the architecture allows N.
- All channels bounded (broadcast cap 1024). Backpressure is real — a
  slow writer slows samplers rather than OOMs.
- No globals. State threaded through `SamplerCtx`, `AppState`,
  `SchedulerHandle`. The two exceptions are `MPERF_ADB_PATH` (env)
  and an iOS DeviceName lookup cache (`OnceLock<Mutex<HashMap>>` with
  15-min TTL — lockdown name queries are ~150ms).

## What's deliberately not abstracted

See abstractions.md §10. Notably: no plugin loader, no storage backend
trait, no protobuf, no multi-device-per-session. If you find yourself
wanting one of these, push back on the requirement before the
abstraction.
