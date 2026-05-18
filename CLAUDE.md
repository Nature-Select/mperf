# mperf — Claude Code briefing

Mobile performance testing toolchain for **Android (USB / adb)** and **iOS (USB / pure-Rust DTX via `idevice`)**. Apache-2.0. Desktop only (macOS / Windows / Linux). Frontend is React 19 + Arco + uPlot inside a Tauri 2 shell. Functionally overlaps with Tencent PerfDog and Xcode Instruments; reimplemented from scratch on public Apple / Google APIs. PerfDog is a Tencent trademark — see README's "Trademark notice" for the disclaimer. When this briefing mentions PerfDog, it's descriptive (shared conventions like the three-tier jank formula), not affiliative.

The user is **not** a perf-tooling / Rust / Tauri / DTX specialist. They verify by *functionality*, not architecture review. Surface risks and trade-offs in plain Chinese — don't bury them in code comments.

Useful docs already in repo:
- `docs/abstractions.md` — canonical contract (Sampler trait, long-format Sample, central scheduler, storage long→wide pivot)
- `docs/architecture.md` — diagrams + crate roles
- `docs/test-cases.md` — manual functional test checklist + REGRESSION tags. Run after every refactor / new sampler / new chart.
- `README.md` — user-facing quickstart

## Layout

```
crates/
  schema/          # MetricKind enum, Sample, Sampler trait — leaf, no deps on others
  storage/         # SQLite via tokio-rusqlite, samples_wide + samples_long, migrations
  android/         # adb-based samplers (cpu, fps, memory, devices, gpu, temp, battery, logcat)
  ios/             # idevice 0.1.61 — sysmontap (cpu+mem+per-core), graphics_raw (gpu), devices, apps, os_trace_relay
  core/            # central scheduler (one broadcast channel, single tokio task per sampler)
apps/desktop/
  src-tauri/src/
    main.rs        # entry (calls lib::run)
    lib.rs         # tauri::Builder + setup hook + invoke_handler list
    setup.rs       # tracing init, panic hook, adb sidecar resolution, Windows DLL staging
    session.rs     # AppState + Session + start_recording + writer task + UI pump + exit watcher
    commands.rs    # #[tauri::command] handlers (thin, delegate to crates / session)
    log_stream.rs  # device log terminal backend
  src/             # React (Live + History tabs, Arco UI, uPlot charts)
    components/{Live,Static}*Chart.tsx, LiveView.tsx, HistoryView.tsx, MarkerOverlay.tsx, LogTerminal.tsx, ...
    lib/{format,ipc,markers,chartResize,useResizableSidebar}.ts
script/            # fetch-binaries (adb sidecar), setup, release
.github/workflows/release.yml   # tag-triggered 4-triple cross-build
```

## Platform coverage matrix

| Metric | Android | iOS | Notes |
|---|---|---|---|
| CPU total %  | ✅ dumpsys cpuinfo | ✅ sysmontap CPU_TotalLoad / cpu_count | normalized 0–100 |
| CPU per-core | ✅ /proc/stat diff | ✅ sysmontap PerCPUUsage | iOS each entry's `CPU_TotalLoad` already 0–100 per core |
| App CPU %    | ✅ `/proc/<pid>/stat` summed | ✅ sysmontap proc `cpuUsage` | both normalized as "fraction of total system CPU" |
| Memory app   | ✅ dumpsys meminfo (PSS) | ✅ sysmontap `physFootprint` | iOS physFootprint = Xcode "Memory" column |
| Memory system used | ✅ /proc/meminfo | ✅ sysmontap System row | iOS: `vmUsed` > `physMemSize − vmFreeCount × vmPageSize` > sum of active+wired+inactive+compressor |
| FPS          | ✅ gfxinfo + SurfaceView merge (**per-app**) | ✅ CoreAnimationFramesPerSecond (**screen-level**) | Android picks max(gfxinfo, sv-layer) |
| Frame time   | ✅ gfxinfo p50 | ❌ | iOS DVT doesn't expose frame-time series |
| Jank + Stutter | ✅ PerfDog formula on SurfaceView | ❌ | needs per-frame timestamps; iOS not implemented |
| GPU tiler/renderer/device | ⚠ Adreno KGSL / Mali devfreq (single Device value, best-effort) | ✅ DTX `services.graphics.opengl` raw bypass — full triplet | Android auto-stops after 5 unparseable polls; Mali Job Manager (Pixel/OverDraw/BusBW) needs vendor SDK — out of scope |
| CPU Temp (CTemp)  | ✅ `/sys/class/thermal/*` max | ❌ | iOS IORegistry not exposed |
| Battery (Temp/Level/V) | ✅ `dumpsys battery` | ❌ blocked on iOS 17+ | lockdown returns empty while CoreDeviceProxy held |
| Net up/down  | ⚠ planned | ⚠ planned | |
| Screenshot   | ⚠ planned | ⚠ planned | |
| Markers      | ✅ schema v4 + UI | ✅ same | Cmd+Shift+M during recording → vertical dashed lines on all charts |
| Device log   | ✅ logcat -T 1 (PID-filtered) | ✅ os_trace_relay (client-side PID filter) | iOS: **NOT** syslog_relay — modern apps use os_log/unified logging |
| Device info  | ✅ getprop parallel | ✅ lockdown parallel (DeviceName cached 15min) | |
| App list     | ✅ pm list | ✅ installation_proxy | filter system / launchable |
| Auto foreground app | ❌ removed | ❌ never | Explicit app selection is **mandatory** on both platforms |

Per-metric scope: `CpuTotalPct / CpuCorePct / MemSystemUsedBytes / GPU* / temp / battery` are device-level on both platforms. `CpuAppPct / MemAppPssBytes` are per-app on both. `Fps / jank / stutter` is **per-app on Android, screen-level on iOS** (DTX has no PID breakdown for CoreAnimation). Don't conflate.

Chart subtitle shape: `<series + ...> · <unit> [· <scope>]` — scope tail only when it contradicts intuition (e.g. iOS FPS: `Frame rate · fps · 屏幕级`).

## Pitfalls — read before you debug

**iOS / idevice 0.1.61**

- `SysmontapClient::next_sample` returns only the first row of each push, dropping the Processes row. Bypass via `crates/ios/src/sysmontap_raw.rs` (`make_channel` directly, merges all rows; also captures `System` + `PerCPUUsage`). Don't replace until upstream fixes it.
- Per-PID values in `Processes` are **positional Arrays** keyed by `proc_attrs` order, not Dicts. Startup logs `name_idx=... mem_idx=... cpu_idx=...`.
- `cpuUsage` per-process is "fraction of one core" (1.0 = one core 100%). For Android-parity `CpuAppPct`, multiply by 100 then divide by `cpu_count`.
- `PerCPUUsage` is an Array parallel to logical CPUs; each entry's `CPU_TotalLoad` is already 0–100. No division.
- System memory: iOS hands sysAttrs back as **top-level keys**, not wrapped in `"System"`. `SysmontapRaw` collects unrecognized top-level keys into `sample.system`. Startup logs full advertised sysAttrs — check there first if `MemSystemUsedBytes` never emits.
- **No battery / temperature on iOS 17+**: lockdown returns empty Dict (no error) while CPU sampler holds `CoreDeviceProxy`. Proper fix is DTX power channel (`com.apple.instruments.server.services.power` / `services.iopowerstate`) on the existing `RemoteServerClient`. `battery.rs` kept `#[allow(dead_code)]`. LiveTemperatureChart gated to `platform === 'android'`.
- Kernel `p_comm` caps at 15 chars. `find_proc_memory` falls back exact → case-insensitive → prefix. Flutter apps have exec `"Runner"`.
- iOS DeviceName lookup ~150ms/query — cached in `OnceLock<Mutex<HashMap>>`, 15-min TTL. Keep it.
- `RsdService` trait fails async-stream HRTB inference. Workaround: resolve `com.apple.instruments.dtservicehub` port from `handshake.services` manually and call `RemoteServerClient::new(boxed_stream)`. See `crates/ios/src/cpu.rs`.
- No reliable foreground-app API. App selection is **mandatory**. Don't add "highest CPU non-system" heuristic — user already rejected it, misattributes to daemons / music.
- **USB + Wi-Fi entries both surface (PerfDog convention)**. Same UDID returned twice with `transport: Usb | Wifi`. Wi-Fi entry `usable=false` until DTX-over-network lands. Rationale: battery testing needs Wi-Fi (USB charges the phone and skews readings).
- Memory **field semantics**: `physFootprint` matches Xcode / PerfDog. Other keys (`residentMemory`, `memResidentSize`, `privateMemory`) have different semantics — fall back only if physFootprint isn't in proc_attrs.
- **sysmontap partial-frame debounce** (`missing_strikes` in `cpu.rs`): sysmontap occasionally pushes a procs frame missing target exec while app is still alive (especially during screen-lock / suspend). Below threshold (3 consecutive misses) → skip the tick (uPlot continues line). At/above → **emit 0 every tick** (not just on threshold-crossing). Counter resets when target reappears. Don't reduce to "emit once" — uPlot pins the line at one 0-point and the chart looks blank. Android does the same continuous-zero behavior in its mem/cpu samplers.
- iOS log capture uses `com.apple.os_trace_relay`, **not** `com.apple.syslog_relay`. Modern apps migrated to `os_log` at iOS 10+; syslog_relay returns near-nothing. `syslog.rs` kept `#[allow(dead_code)]`. Stream is opened **server-side unfiltered** (`start_trace(None)`) — `StartActivity{Pid: N}` is the documented per-process filter but iOS 17+ accepts it without error and never pushes any frames. PID match happens **client-side in `log_stream.rs::run_ios`**. Bundle id → `CFBundleExecutable` via `resolve_bundle_to_exec`, then look up exec in the device's `PidList` to get the PID; the exec name itself is **not** used to filter log lines (`image_name` is the emitting binary path — e.g. UIKit logs inside an app arrive with `image_name="/System/Library/.../UIKitCore"` and would be incorrectly rejected). App restart changes PID → user must toggle log terminal off/on.
- `com.apple.os_trace_relay` is **single-shot per connection**: after a `PidList` request the device half-closes the socket. Use two separate lockdown connections — one for the PidList probe (dropped immediately), one for the StartActivity stream. See `os_trace.rs::OsTraceStream::start`.
- idevice 0.1.61's `OsTraceRelayClient::get_pid_list()` drops the `ProcessName` strings from the response. `os_trace.rs::pid_list_with_names` reimplements the request by hand (binary plist + 4-byte length prefix, 0x01 marker, parses `Payload.<pid>.ProcessName`). Replace with upstream helper when idevice 0.2 fixes it.

**Android**

- gfxinfo measures UI thread only. Games rendering to SurfaceView/GLSurfaceView are invisible to it. Sample both and pick max FPS per tick.
- `SurfaceFlinger --latency` returns the layer's last 128 frame timestamps. When the layer is idle the buffer freezes — naive `frames / (max - min)` reports stale FPS forever. Sampler tracks `prev_max_ts` per layer and only counts newer frames.
- PerfDog jank formula: `current_frame_ms > 2 × avg(prev 3)` AND `> {1,2,3} × (1000/24)`. Small=1×, Jank=2×, Big=3×. 24fps movie frame is the threshold base — industry standard, don't "fix" to vsync-relative without explicit approval.
- Stutter = cumulative `jank_duration_ms / total_duration_ms` per layer. Resets on foreground app change.
- `adb_binary()` reads `PERFDOG_ADB_PATH`. Tauri setup sets it from bundled sidecar; falls back to system adb.
- Device `usable = (state == "device")`. `offline / unauthorized / no permissions / recovery / bootloader / sideload` all flip to false — the watchdog relies on this. Don't hardcode `usable: true`.
- All adb pkg / layer interpolation goes through `is_safe_pkg_name` (in `adb.rs`); layer names additionally filter `"` `$` `` ` `` `\`. Skip with warn rather than escape.
- `logcat -T 1 -v threadtime` with `--pid` resolved via `pidof` at start. If target app restarts mid-stream the PID stales and the stream goes silent — user toggles log terminal off/on. Not auto-recovered (alternative is non-stop pidof polling).

**Tauri / desktop shell**

- **Never install the tracing subscriber twice.** Panics on `set_global_default`, fires inside macOS `tao` `did_finish_launching` (`extern "C" cannot_unwind`) → process aborts before window appears. `init_tracing` lives in `setup()` only.
- Panic hook installed before `tauri::Builder::default()` writes to `$TMPDIR/perfdog-oss-panic.log`. Only thing that survives a startup-time abort. Don't remove.
- `target/release/bundle/` lives at the **workspace root**, not under `src-tauri/`. `script/release.mjs` already knows this.
- Bundled adb resolution scans `current_exe`'s parent for `adb` or `adb-*`, excluding `*.dll`. Tauri dev → just `adb`; packaged → `adb-<triple>`. Match both.
- Windows-only: `stage_windows_dlls()` copies `AdbWinApi.dll` + `AdbWinUsbApi.dll` next to `adb.exe` at startup (adb's DLL lookup is dir-local).
- ESM scripts in `script/` use `import.meta.dirname`, **not** `__dirname` (throws in ESM).
- Tauri icons need RGBA (color type 6), not RGB.
- **Scheduler does NOT close the broadcast channel when it exits naturally** — `SchedulerHandle` holds a `live_tx` clone for `subscribe()`. Broadcast close only fires on user-initiated stop. Natural exits (sampler death / non-retriable error) reach `spawn_exit_watcher_task` via `SchedulerHandle::exit_notify()` (`Arc<Notify>`). The watcher (a) emits `EVENT_SESSION_ENDED` if `user_stopping=false`, and (b) tears down the backend `Session` (lock `AppState.session`, take only if `db_id` matches to avoid races with a concurrent Start, run `Session::stop`). Without (b), the writer task hangs forever and `wall_end_ms` stays NULL → History shows "in progress" forever. Use `notify_one()` (not `notify_waiters`) so a permit is preserved if the watcher hasn't entered `.notified().await` yet.
- `storage::finish_session` uses `WHERE wall_end_ms IS NULL` so the writer-task auto-finalize and the explicit `Session::stop` don't double-write (would report cleanup time instead of sampling-stop time).
- `schema::run_migrations` rejects on-disk schema versions > `HEAD` with a loud `SqliteFailure` (don't silently no-op on a future schema — older binary then errors opaquely on unknown columns). Each migration's DDL + version-record runs in a transaction.

**Frontend / UX**

- Arco `<Checkbox>` `onClick` swallows `onChange`. Wrap in `<div onClick={stopPropagation}>` for row-click vs check-click separation.
- Arco `Modal.confirm` sometimes doesn't render. Use state-controlled `<Modal visible={open}>` instead.
- Arco `.arco-tabs-header` is `display: inline-block` by default. `flex: 1` on `.arco-tabs-header-title` is a no-op alone — override the header to `display: flex; width: 100%`.
- uPlot `setData()` is fine for live updates, but the chart must be destroyed + recreated on series-count changes (per-core count). Live/History tabs use `display:none` toggling — destroying on tab switch breaks live recording state.
- Chart resize uses `lib/chartResize.ts::attachChartResize` (ResizeObserver, fires on any layout change including sidebar drag). window.resize is fallback only.
- Min-width 0 chain: every block from `Layout.Content` → wrapper divs → `chartCard` → `chartHost` needs `min-width: 0` for flex layout to shrink below uPlot's pixel-explicit width. Grid items default to it; flex items don't.
- For UI/UX changes: start `pnpm dev` and use the feature in a browser before reporting done. Type-check passing is **not** correctness.
- **Disconnect detection has three independent paths**, coordinated via `user_stopping` so the user sees at most one toast: (1) device-list watchdog (~1.5s for `!current`, ~3s for `!current.usable`), (2) sample-heartbeat watchdog (12s — backstop for silent sampler hang), (3) backend `EVENT_SESSION_ENDED` via exit_notify (~50ms after sampler reports DeviceDisconnected). Don't collapse them — each catches a different failure mode.
- Watchdog condition uses `!current.usable` alone (not `selected.usable && !current.usable`) and the sibling sync effect depends on `selected?.id` (not the whole `selected` object) so it doesn't re-fire within the same poll. The compound condition broke on iOS Wi-Fi fallback.
- NoticeBanner toasts use `position: fixed` rendered via `createPortal` to `document.body` (the Live/History tab toggle uses `display: none` and would otherwise swallow them). Warning toasts use `auto: true` (3.5s self-dismiss); error banners stick until Dismiss.
- Log terminal is **independent of the recording session**: can open without recording, or record with terminal closed. Logs are **not** persisted to SQLite (per-second log volume on a busy app dwarfs sample volume by 100×).

**General**

- Tauri dev hot-reloads the frontend but NOT Rust. Any change under `crates/` or `src-tauri/` requires `Ctrl+C` and re-running `pnpm dev`. Tell the user explicitly when you've changed Rust code.
- Sample format is long (one value + labels per row). Storage pivots long→wide for unlabeled metrics only. Per-core / per-thread / per-iface stays in `samples_long`. Don't pivot in the sampler or in the frontend.
- Host monotonic clock is the primary timestamp. `device_ts_us` is best-effort secondary. Never join across sessions on device time.
- `MetricKind` is a closed enum. Adding a metric: schema variant + storage column (new migration) + frontend MetricKind union + chart wiring. The compiler finds the missing arms.

## User collaboration preferences

- Verifies by functionality, not architecture review. Show results, not designs.
- Flag risks and trade-offs in plain Chinese. No condescension, no jargon-dumping.
- Doesn't have perf-domain knowledge. When a metric is non-obvious (e.g. PerfDog's 24fps base for jank), explain *why* it's the convention.
- Heuristics that can misattribute (e.g. "highest CPU = foreground app") are explicitly off-limits. Require explicit user selection, return errors loudly.
- For algorithms documented by upstream sources that may be outdated, use judgment but flag the divergence. PerfDog docs are not gospel.
- Prefers one bundled PR for related refactors over many tiny commits.
- Likes terse responses; no trailing summaries of what was just done.

## Build / run

```
# first time
pnpm install
pnpm fetch:adb           # downloads platform-tools for current host
pnpm dev                 # cargo build + tauri dev

# rebuild after Rust changes — Tauri dev does NOT auto-rebuild Rust
Ctrl+C, then pnpm dev again

# release artifacts (current host only)
pnpm release             # writes to target/release/bundle/

# cross-platform release: push a `v*` tag → .github/workflows/release.yml builds 4 triples
```

Log file (per-user, daily rotation):
- macOS: `~/Library/Logs/io.perfdog.oss/perfdog.log.YYYY-MM-DD`
- Linux: `~/.local/state/io.perfdog.oss/`
- Windows: `%LOCALAPPDATA%\io.perfdog.oss\logs\`

Database (macOS): `~/Library/Application Support/io.perfdog.oss/data.db`

Panic crash log (last-resort, written before tracing is ready): `$TMPDIR/perfdog-oss-panic.log`

## Next features (rough order)

1. **iOS jank** — needs per-frame timestamps from CoreAnimationFramesPerSecond or a different DTX channel. Investigate before promising.
2. **Screenshot** — `adb exec-out screencap -p` (Android); idevice screenshot service (iOS).
3. **Network bytes** — Android `/proc/net/dev` filtered; iOS sysmontap has `bytesIn/bytesOut` (system-wide, already in sys_attrs we request).
4. **Marker management v2** — list panel in History with bulk delete / rename / click-to-jump, hover-tooltip on Live charts.
5. **iOS battery + temperature via DTX** — `com.apple.instruments.server.services.power` / `services.iopowerstate`. They multiplex through the existing `RemoteServerClient`, unlike lockdown. Likely needs a raw DTX handler similar to `sysmontap_raw.rs`. `crates/ios/src/battery.rs` lockdown approach preserved as fallback for iOS < 17.
6. **Per-thread CPU on iOS** — different proc_attr key; positional-array technique.
7. **GPU on iOS — extended attributes**: `graphics_raw.rs` currently surfaces Tiler/Renderer/Device % only. DTX channel also pushes memory counters, IOGL bundle info — `GraphicsRawSample.all_keys` is logged on the first push.

## Things not to do

- Don't reach for pymobiledevice3 / py-ios-device / go-ios. We chose pure-Rust `idevice` for zero-Python, zero-sudo, single binary distribution. The hard work (sysmontap_raw, HRTB workaround, positional decode) is done.
- Don't add backwards-compat shims for removed metrics — schema migrations are versioned, just write a new one.
- Don't pivot long→wide in the frontend or in the sampler. Storage is the only place.
- Don't `git commit` unless the user explicitly asks. Don't `git push` unless asked. Don't force-push to `master`/`main`.
- Don't add comments narrating WHAT the code does. Only WHY — non-obvious constraints, workarounds, references to specific bugs.
