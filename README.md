# mperf

Open-source mobile performance testing toolchain for **Android** and
**iOS**. Built with Tauri 2 + Rust + React; ships as a single desktop
binary, no Python / no sudo / no host daemon.

## Install

Download the binary for your OS from the
[latest release](https://github.com/Nature-Select/mperf/releases/latest).

### macOS

Releases ship unsigned (no Apple Developer ID yet).
On first launch Gatekeeper will say **"mperf is damaged and can't be
opened. You should move it to the Trash."** The app isn't damaged —
that's macOS's wording for "downloaded from the internet and not signed
by Apple". To unblock, run once in Terminal after installing:

```bash
sudo xattr -rd com.apple.quarantine /Applications/mperf.app
```

That removes the `com.apple.quarantine` extended attribute the browser
attached on download. After that, the app opens normally.

GUI alternative: double-click → click Cancel on the dialog → open
**System Settings → Privacy & Security**, scroll to the bottom, click
**"Open Anyway"** next to the mperf line. Re-double-click and confirm.

### Windows

The `.exe` / `.msi` aren't EV-Code-Signed, so SmartScreen will show a
blue **"Windows protected your PC"** dialog on first run. Click
**"More info"** → **"Run anyway"**. Subsequent launches are silent.

### Linux

Use the package that matches your distro:
- Debian/Ubuntu: `.deb` (`sudo apt install ./mperf_*.deb`)
- Fedora/RHEL: `.rpm` (`sudo dnf install ./mperf-*.rpm`)
- Anywhere: `.AppImage` (`chmod +x mperf_*.AppImage && ./mperf_*.AppImage`)

For iOS device support on Linux you also need `usbmuxd`:
```bash
sudo apt-get install -y usbmuxd
```

## Build from source

```bash
git clone <repo>
cd mperf

pnpm setup           # verify toolchain, install deps, fetch bundled adb
pnpm tauri dev       # run the app
```

Setup will refuse and print install hints if Rust, Node ≥ 20, or pnpm are
missing.

## Stack

- **Desktop shell**: Tauri 2 (single native binary, no Electron)
- **Backend**: Rust (cargo workspace under `crates/`)
- **Frontend**: React 19 + Vite + Arco Design + uPlot
- **Storage**: SQLite (sessions, samples)
- **Android transport**: bundled `adb` binary via Tauri sidecar
- **iOS transport**: `idevice` crate (pure-Rust DTX over user-space tunnel,
  no Python, no `sudo` daemon)

## Layout

```
apps/desktop/         Tauri app (frontend + src-tauri)
crates/core           Sampler scheduler, session manager
crates/android        adb-based collectors (CPU, FPS, memory, apps, ...)
crates/ios            idevice-based collectors (CPU, FPS, memory, apps, ...)
crates/storage        SQLite persistence
crates/schema         Shared sampler trait + data model
script/               Dev / build scripts (zx)
.github/workflows/    CI for cross-platform release builds
```

## Connecting a device

**Android**
- Enable Developer Options + USB Debugging
- On first plug, tap "Allow" on the device authorization dialog
- The bundled adb starts a daemon automatically

**iOS**
- Plug device over USB; on first connect, tap "Trust this computer"
- For iOS 16+, enable Developer Mode (Settings → Privacy & Security → Developer Mode)
- No sudo, no separate tunnel daemon — the app starts its own user-space tunnel

Linux specifically needs `usbmuxd` for iOS:
```bash
sudo apt-get install -y usbmuxd
```

Windows specifically needs Apple Mobile Device Support (installed by iTunes).

## Develop

```bash
pnpm tauri dev       # frontend HMR + Rust hot-rebuild on change
pnpm test            # cargo test --workspace
pnpm format          # prettier on TS/JS/CSS
pnpm cargo:check     # cargo check on full workspace
```

After modifying Rust, restart `pnpm tauri dev` to pick up changes (only the
frontend HMR is automatic).

## Build a release bundle

For the current platform:

```bash
pnpm release         # pnpm tauri build with sidecar verification
```

Artifacts land in `apps/desktop/src-tauri/target/release/bundle/`:
- macOS: `.dmg` + `.app.tar.gz`
- Linux: `.deb` + `.AppImage`
- Windows: `.msi` + `.exe`

## Cross-platform release (CI)

`.github/workflows/release.yml` builds for all 4 targets on push of a `v*`
tag, then creates a GitHub Release with downloadable artifacts. CI also
guards that the git tag matches the workspace Cargo version — mismatch
fails the run before the 20-minute cross-build.

To cut a release:

```bash
pnpm bump 0.1.0                       # syncs Cargo.toml + tauri.conf.json + package.json
git commit -am "release: v0.1.0"
git tag v0.1.0 && git push --tags
```

Targets:
- macOS arm64 (`aarch64-apple-darwin`) — Apple Silicon
- Linux x64 (`x86_64-unknown-linux-gnu`)
- Windows x64 (`x86_64-pc-windows-msvc`)

Intel Macs are not in the prebuilt matrix (GitHub Actions `macos-13`
runners now queue for 30min+ as Apple finishes the Silicon transition).
Build from source via `pnpm release` on an Intel Mac if needed.

You can also trigger the workflow manually from the Actions tab.

## Common dev tasks

| Task | Command |
|---|---|
| Verify env + install | `pnpm setup` |
| Run app | `pnpm tauri dev` |
| Run all tests | `pnpm test` |
| Format | `pnpm format` |
| Build current platform | `pnpm release` |
| Fetch all platform adb (release prep) | `pnpm fetch:adb:all` |
| Bump version (Cargo + Tauri + pkg.json) | `pnpm bump <version>` |

## Architecture

See `docs/abstractions.md` for the Sampler trait + Scheduler contract that
every collector follows. Read it before adding a new metric.

## License

Apache-2.0. See `LICENSE` and `NOTICE` for the full text and attribution.

## Trademark notice

**PerfDog** is a registered trademark of Tencent Holdings Limited. This
project is an independent reimplementation built on public Apple
(Instruments / DTX) and Google (adb / dumpsys) interfaces. It is not
affiliated with, endorsed by, or sponsored by Tencent. Where this codebase
mentions PerfDog, it does so descriptively to explain shared performance-
metric conventions (e.g. the three-tier jank classification), not to claim
association.
</content>
</invoke>