mod commands;
mod log_stream;
mod session;
mod setup;

use std::sync::Arc;

use mperf_storage::Storage;
use tauri::Manager;
use tokio::sync::Mutex;

use crate::session::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Last-resort panic capture: dump any panic (including ones that fire
    // before the tracing subscriber is set up — e.g. during macOS app
    // delegate init) to a fixed temp-dir file the user can attach to a
    // bug report. We chain to the default hook so stderr still prints.
    setup::install_panic_log_hook();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .setup(move |app| {
            // Resolve the log directory (per-user data dir) and install the
            // tracing subscriber with file rotation. Keep the worker guard
            // for the app lifetime so the async flusher stays alive.
            let log_dir = app.path().app_log_dir().ok();
            if let Some(d) = &log_dir {
                let _ = std::fs::create_dir_all(d);
            }
            let guard = setup::init_tracing(log_dir.as_deref());
            // Leak the guard intentionally — its drop would close the file
            // writer; we want it open until process exit.
            std::mem::forget(guard);

            tracing::info!(version = env!("CARGO_PKG_VERSION"), "mperf starting");

            // Point the Android crate at the bundled adb. If the sidecar
            // isn't present (e.g. someone forgot to run `pnpm fetch:adb`),
            // we leave the env var unset and the crate falls back to
            // whichever `adb` is on PATH.
            let bundled_adb = setup::resolve_bundled_adb();
            if let Some(path) = &bundled_adb {
                tracing::info!(path = %path.display(), "using bundled adb sidecar");
                std::env::set_var("MPERF_ADB_PATH", path);
            } else {
                tracing::warn!(
                    "no bundled adb found next to executable — falling back to system `adb` on PATH"
                );
            }

            // On Windows, adb.exe needs AdbWinApi.dll + AdbWinUsbApi.dll in
            // its own directory. Tauri stages adb.exe via externalBin
            // (target/.../debug/adb-<triple>.exe) but resources live in
            // `<install>/resources/`. Copy the DLLs next to adb.exe at
            // first launch so adb's DLL lookup finds them.
            if let Some(adb_path) = bundled_adb.as_deref() {
                setup::stage_windows_dlls(app.handle(), adb_path);
            }

            // Open the database in app_data_dir, blocking the setup briefly.
            // For a perf tool this is a small price for a clean lifecycle.
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("app_data_dir: {e}"))?;
            let db_path = data_dir.join("data.db");
            let storage_cell = Arc::new(Mutex::new(None::<Storage>));
            let storage_clone = storage_cell.clone();
            tauri::async_runtime::block_on(async move {
                let s = Storage::open(db_path).await.expect("open storage");
                *storage_clone.lock().await = Some(s);
            });
            let storage = {
                let mut g = tauri::async_runtime::block_on(async { storage_cell.lock().await });
                g.take().expect("storage initialized")
            };
            app.manage(AppState {
                storage,
                session: Mutex::new(None),
                log_stream: Mutex::new(None),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_devices,
            commands::list_apps,
            commands::get_device_info,
            commands::start_session,
            commands::measure_startup,
            commands::detect_startup_mode,
            commands::stop_session,
            commands::list_sessions,
            commands::get_session_samples,
            commands::get_session_core_samples,
            commands::delete_session,
            commands::add_marker,
            commands::list_session_markers,
            commands::delete_marker,
            commands::update_marker,
            commands::update_marker_label,
            commands::get_diagnostics,
            commands::reveal_path,
            commands::start_log_stream,
            commands::stop_log_stream,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
