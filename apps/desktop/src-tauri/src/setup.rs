//! Startup-side concerns split out of `lib.rs`: tracing/log setup, the
//! last-resort panic hook, sidecar `adb` resolution, and Windows DLL
//! staging. Keeping them here means `lib.rs` is just the wiring of run().

use std::path::{Path, PathBuf};

use tracing_appender::non_blocking::WorkerGuard;

/// Install a panic hook that appends every panic to
/// `$TMPDIR/mperf-panic.log` in addition to running the default hook
/// (which still prints to stderr). The temp dir works on every platform
/// without needing the Tauri app handle, so panics from very early in
/// startup — before `init_tracing` runs — still get captured.
pub fn install_panic_log_hook() {
    let path = std::env::temp_dir().join("mperf-panic.log");
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            use std::io::Write;
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = writeln!(
                f,
                "\n=== mperf panic @ unix {ts} (pid {}) ===\n{info}",
                std::process::id()
            );
        }
        eprintln!("[mperf] panic log: {}", path.display());
        default(info);
    }));
}

/// Install `tracing` subscribers: stderr (dev terminal) + rolling file log
/// under the user's data dir. The returned `WorkerGuard` must outlive the
/// process (intentionally leaked by the caller) so the async flusher
/// keeps writing.
pub fn init_tracing(log_dir: Option<&Path>) -> Option<WorkerGuard> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let stderr_layer = tracing_subscriber::fmt::layer().with_ansi(true);

    if let Some(dir) = log_dir {
        let file_appender = tracing_appender::rolling::daily(dir, "mperf.log");
        let (file_writer, guard) = tracing_appender::non_blocking(file_appender);
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(file_writer)
            .with_ansi(false);
        tracing_subscriber::registry()
            .with(env_filter)
            .with(stderr_layer)
            .with(file_layer)
            .init();
        tracing::info!(dir = %dir.display(), "log file rotation enabled");
        Some(guard)
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(stderr_layer)
            .init();
        None
    }
}

/// Resolve the bundled `adb` sidecar binary path. Tauri's `externalBin`
/// mechanism stages it next to the main executable as `adb-<TARGET-TRIPLE>`
/// (release) or just `adb` (dev, where Tauri strips the triple suffix).
/// Returns None if no sidecar is found — the android crate then falls
/// back to the system `adb` on PATH.
pub fn resolve_bundled_adb() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    tracing::info!(dir = %dir.display(), "scanning for bundled adb");
    for entry in std::fs::read_dir(dir).ok()? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".dll") {
            continue;
        }
        let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
        let is_adb = stem == "adb" || stem.starts_with("adb-");
        if is_adb {
            let is_file = entry.path().is_file();
            tracing::info!(name = %name, is_file, "candidate adb file");
            if is_file {
                return Some(entry.path());
            }
        }
    }
    None
}

/// Windows-only: copy `AdbWinApi.dll` and `AdbWinUsbApi.dll` from the
/// Tauri resource dir to the directory of `adb.exe`, so Windows' DLL
/// search finds them. No-op on macOS / Linux.
#[cfg(target_os = "windows")]
pub fn stage_windows_dlls(app: &tauri::AppHandle, adb_path: &Path) {
    use tauri::Manager;
    let Some(adb_dir) = adb_path.parent() else { return };
    let Ok(resource_dir) = app.path().resource_dir() else { return };
    for dll in ["AdbWinApi.dll", "AdbWinUsbApi.dll"] {
        let dst = adb_dir.join(dll);
        if dst.exists() {
            continue;
        }
        // tauri.windows.conf.json lists these as `binaries/*.dll`, and Tauri 2
        // preserves that subpath under resource_dir. Fall back to a flat layout
        // in case a future config change drops the prefix.
        let candidates = [
            resource_dir.join("binaries").join(dll),
            resource_dir.join(dll),
        ];
        let Some(src) = candidates.iter().find(|p| p.exists()) else {
            tracing::error!(
                dll,
                searched = ?candidates,
                "adb DLL missing from bundled resources — adb.exe will fail to load"
            );
            continue;
        };
        match std::fs::copy(src, &dst) {
            Ok(_) => tracing::info!(dll, dst = %dst.display(), "staged adb DLL"),
            Err(e) => tracing::error!(
                dll,
                src = %src.display(),
                dst = %dst.display(),
                error = %e,
                "copy adb DLL failed — adb.exe will fail to load"
            ),
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn stage_windows_dlls(_app: &tauri::AppHandle, _adb_path: &Path) {}
