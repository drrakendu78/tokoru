//! Tauri commands wiring the download orchestrator into the frontend.
//!
//! All progress / state-transition data flows through events
//! (`download-progress`, `download-status`) — these commands are stateful
//! dispatchers. The frontend's `useDownloads` hook subscribes to both events
//! and keeps a local mirror of the runtime state.

use std::collections::HashMap;

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::models::ShortcutStatus;
use crate::services::db::Database;
use crate::services::downloads::{self, DownloadSettings};
use crate::services::runtime_state::{DownloadState, DownloadStatus, RuntimeState};
use crate::services::{cli_bins, gogdl, legendary, steam_collections, steam_writers};

#[tauri::command]
pub async fn start_download(
    game_id: String,
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    db: State<'_, Database>,
) -> Result<(), String> {
    downloads::start(app, runtime.inner().clone(), db.inner().clone(), game_id).await
}

#[tauri::command]
pub async fn pause_download(
    game_id: String,
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
) -> Result<(), String> {
    downloads::pause(&app, runtime.inner(), &game_id)
}

#[tauri::command]
pub async fn resume_download(
    game_id: String,
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    db: State<'_, Database>,
) -> Result<(), String> {
    downloads::resume(app, runtime.inner().clone(), db.inner().clone(), game_id).await
}

#[tauri::command]
pub async fn cancel_download(
    game_id: String,
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
) -> Result<(), String> {
    downloads::cancel(&app, runtime.inner(), &game_id)
}

#[tauri::command]
pub async fn retry_download(
    game_id: String,
    app: AppHandle,
    runtime: State<'_, RuntimeState>,
    db: State<'_, Database>,
) -> Result<(), String> {
    downloads::retry(app, runtime.inner().clone(), db.inner().clone(), game_id).await
}

/// Clear a download entry from the in-memory list (the Downloads page row).
/// Does NOT delete files on disk.
///
/// For Epic/GOG downloads: only allowed when the entry is Completed / Failed
/// (active downloads need to go through `cancel_download` first).
///
/// For Steam downloads: allowed at any state — Tokoru can't influence
/// Steam's downloader anyway, and the user just wants the row gone. We also
/// mark the appid as "user-dismissed" so the ACF watcher won't re-add it on
/// the next tick.
#[tauri::command]
pub async fn dismiss_download(
    game_id: String,
    db: State<'_, Database>,
    runtime: State<'_, RuntimeState>,
) -> Result<(), String> {
    use crate::services::runtime_state::DownloadStatus;

    let state = runtime.get(&game_id);
    let source = state.as_ref().map(|s| s.source.clone()).unwrap_or_default();

    if source != "steam" {
        if let Some(s) = &state {
            if matches!(s.status, DownloadStatus::Downloading | DownloadStatus::Paused) {
                return Err(format!(
                    "Can't dismiss an active download (state: {:?}). Cancel it first.",
                    s.status
                ));
            }
        }
    } else {
        // Look up the Steam appid (platform_id on the game row) so we can
        // suppress it from the watcher's next tick. Fall back to the state's
        // platform_id if the DB lookup misses.
        let appid = db
            .get_game_by_id(&game_id)
            .ok()
            .flatten()
            .and_then(|g| g.platform_id)
            .or_else(|| state.as_ref().map(|s| s.platform_id.clone()));
        if let Some(appid) = appid {
            if !appid.is_empty() {
                runtime.mark_steam_dismissed(&appid);
            }
        }
    }

    runtime.remove(&game_id);
    Ok(())
}

#[tauri::command]
pub async fn get_download_state(
    game_id: String,
    runtime: State<'_, RuntimeState>,
) -> Result<Option<DownloadState>, String> {
    Ok(runtime.get(&game_id))
}

#[derive(Debug, Serialize)]
pub struct DownloadListEntry {
    pub game_id: String,
    pub state: DownloadState,
}

#[tauri::command]
pub async fn get_all_downloads(
    runtime: State<'_, RuntimeState>,
) -> Result<Vec<DownloadListEntry>, String> {
    Ok(runtime
        .all()
        .into_iter()
        .map(|s| DownloadListEntry {
            game_id: s.game_id.clone(),
            state: s,
        })
        .collect())
}

#[tauri::command]
pub async fn get_download_settings(
    db: State<'_, Database>,
) -> Result<DownloadSettings, String> {
    Ok(downloads::load_settings(db.inner()))
}

#[tauri::command]
pub async fn set_download_settings(
    default_dir: Option<String>,
    per_source: Option<HashMap<String, String>>,
    max_parallel: Option<u32>,
    auto_push_after_download: Option<bool>,
    bandwidth_kbps: Option<u64>,
    db: State<'_, Database>,
) -> Result<DownloadSettings, String> {
    let mut current = downloads::load_settings(db.inner());
    if let Some(v) = default_dir {
        current.default_dir = v;
    }
    if let Some(v) = per_source {
        current.per_source = v;
    }
    if let Some(v) = max_parallel {
        current.max_parallel = v.max(1);
    }
    if let Some(v) = auto_push_after_download {
        current.auto_push_after_download = v;
    }
    if let Some(v) = bandwidth_kbps {
        current.bandwidth_kbps = v;
    }
    downloads::save_settings(db.inner(), &current)?;
    Ok(current)
}

// ── Uninstall flow ────────────────────────────────────────────────────────

/// Remove a previously-downloaded game from disk and clean up our tracking.
///
/// - Epic: invokes `legendary uninstall <appName> --yes` (this deletes files
///   AND scrubs legendary's `installed.json` so a future re-install starts
///   clean).
/// - GOG: deletes the install_path tree manually (gogdl has no uninstall
///   subcommand). Also removes the per-game manifest under
///   `<APPDATA>/heroic_gogdl/manifests/` so gogdl doesn't try to resume from
///   a deleted dir on the next install.
/// - Other sources: refused — we never wrote the files in the first place,
///   the user should use the source's launcher to uninstall.
///
/// After the cleanup, the game's `install_path` is cleared in the DB (its
/// `exe_path` stays — for owned-library games that holds the launch URI,
/// which is still valid post-uninstall). If a pushed shortcut exists, it's
/// removed from shortcuts.vdf and marked `removed` in the DB. Best-effort
/// rebuild of the Steam Collections at the end so the sidebar mirrors the
/// new state.
#[tauri::command]
pub async fn uninstall_game(
    game_id: String,
    db: State<'_, Database>,
    runtime: State<'_, RuntimeState>,
    app: AppHandle,
) -> Result<(), String> {
    let game = db
        .get_game_by_id(&game_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Game '{}' not found", game_id))?;

    let install_path = game
        .install_path
        .clone()
        .ok_or_else(|| "Game is not installed".to_string())?;

    // 1) Cancel any in-flight download for this game so we don't race against
    //    a running runner trying to write the same files.
    if let Some(state) = runtime.get(&game_id) {
        if matches!(
            state.status,
            DownloadStatus::Downloading | DownloadStatus::Paused | DownloadStatus::Queued
        ) {
            log::info!(
                "[uninstall] cancelling in-flight download for {} before uninstall",
                game_id
            );
            let _ = downloads::cancel(&app, runtime.inner(), &game_id);
        }
    }

    // 2) Source-specific cleanup of on-disk files + runner metadata.
    let source = game.source.clone();
    let platform_id = game.platform_id.clone();
    match source.as_str() {
        "epic" => {
            let pid = platform_id.clone().ok_or_else(|| {
                "Epic game has no platform_id — can't dispatch to legendary".to_string()
            })?;
            let legendary_exe = legendary::ensure_exe(&app).await?;
            let pid_for_thread = pid.clone();
            // Synchronous subcommand — run on a blocking worker so we don't
            // tie up the async executor while legendary scans its install set.
            tauri::async_runtime::spawn_blocking(move || {
                legendary::uninstall_blocking(&legendary_exe, &pid_for_thread)
            })
            .await
            .map_err(|e| format!("Uninstall worker panicked: {}", e))??;
        }
        "gog" => {
            // gogdl has no uninstall subcommand — wipe the dir ourselves.
            uninstall_gog_files(&install_path)?;
            if let Some(pid) = platform_id.as_deref() {
                let _ = remove_gogdl_manifest(pid);
            }
        }
        other => {
            return Err(format!(
                "Tokoru can't uninstall {} games — use the source's launcher.",
                other
            ));
        }
    }

    // 3) Clear install_path in the DB. We deliberately keep exe_path — for
    //    owned-library games that holds the protocol launch URI (e.g.
    //    `com.epicgames.launcher://...`) which still resolves post-uninstall
    //    and is what shortcuts.vdf was pointed at.
    db.clear_install_path(&game_id).map_err(|e| e.to_string())?;

    // 4) If a pushed shortcut exists for this game, scrub it from
    //    shortcuts.vdf and mark the row as removed.
    if let Ok(Some(existing)) = db.get_shortcut(&game_id) {
        if existing.status == ShortcutStatus::Pushed {
            if let Err(e) = steam_writers::remove_game(&game) {
                log::warn!("[uninstall] removing Steam shortcut for {}: {}", game_id, e);
            } else {
                let updated = crate::models::Shortcut {
                    status: ShortcutStatus::Removed,
                    pushed_at: existing.pushed_at,
                    ..existing
                };
                let _ = db.upsert_shortcut(&updated);
            }
        }
    }

    // 5) Best-effort Steam Collections rebuild so the sidebar reflects the
    //    new state. Don't fail the uninstall if Steam is running or leveldb
    //    isn't reachable — the file-level cleanup is already done.
    if let Err(e) = rebuild_steamshelf_collections(db.inner()) {
        log::warn!("[uninstall] Collections rebuild failed: {}", e);
    }

    Ok(())
}

fn uninstall_gog_files(install_path: &str) -> Result<(), String> {
    let path = std::path::Path::new(install_path);
    if !path.exists() {
        // Already gone — treat as success so the user can recover from a
        // half-uninstalled state without panicking.
        log::info!(
            "[uninstall] gog install_path missing — assuming already removed: {}",
            install_path
        );
        return Ok(());
    }
    std::fs::remove_dir_all(path)
        .map_err(|e| format!("Failed to delete {}: {}", install_path, e))?;
    Ok(())
}

fn remove_gogdl_manifest(platform_id: &str) -> Result<(), String> {
    // heroic-gogdl stores per-game resume manifests under
    // `<APPDATA>/heroic_gogdl/manifests/<platform_id>`. Best-effort cleanup
    // so a future install doesn't try to resume from a deleted directory.
    let Ok(appdata) = std::env::var("APPDATA") else {
        return Ok(());
    };
    let manifest =
        std::path::PathBuf::from(appdata).join("heroic_gogdl").join("manifests").join(platform_id);
    if manifest.exists() {
        // Manifests can be either a file or a folder depending on gogdl
        // version — try both safely.
        if manifest.is_file() {
            let _ = std::fs::remove_file(&manifest);
        } else if manifest.is_dir() {
            let _ = std::fs::remove_dir_all(&manifest);
        }
    }
    Ok(())
}

fn rebuild_steamshelf_collections(db: &Database) -> Result<(), String> {
    use std::collections::HashMap as Map;

    let shortcuts = db.get_all_shortcuts().map_err(|e| e.to_string())?;
    let mut by_source: Map<String, Vec<i64>> = Map::new();
    for s in &shortcuts {
        if s.status != ShortcutStatus::Pushed {
            continue;
        }
        let game = match db.get_game_by_id(&s.game_id) {
            Ok(Some(g)) => g,
            _ => continue,
        };
        by_source
            .entry(game.source)
            .or_default()
            .push(s.steam_appid as i64);
    }
    let mut collections: Vec<steam_collections::Collection> = by_source
        .into_iter()
        .map(|(source, appids)| steam_collections::Collection {
            name: steam_writers::category_tag_for_source(&source),
            appids,
        })
        .collect();
    collections.sort_by(|a, b| a.name.cmp(&b.name));

    let user_id = steam_collections::current_steam_user_id()
        .ok_or_else(|| "Could not resolve current Steam user id.".to_string())?;
    steam_collections::write_collections(&user_id, &collections)
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ── CLI helper binary status (for Settings → Downloads → Helper binaries) ──

#[derive(Debug, Serialize)]
pub struct CliBinsStatus {
    pub legendary_present: bool,
    pub gogdl_present: bool,
    pub legendary_version: Option<String>,
    pub gogdl_version: Option<String>,
    pub legendary_path: String,
    pub gogdl_path: String,
}

#[tauri::command]
pub async fn get_cli_bins_status(app: AppHandle) -> Result<CliBinsStatus, String> {
    let legendary = cli_bins::legendary_path(&app)?;
    let gogdl = cli_bins::gogdl_path(&app)?;
    let legendary_present = legendary.exists();
    let gogdl_present = gogdl.exists();
    Ok(CliBinsStatus {
        legendary_present,
        gogdl_present,
        // We pin to a specific upstream release, so the version we know is on
        // disk is exactly the pinned one (when the file is present).
        legendary_version: legendary_present
            .then(|| cli_bins::pinned_legendary_version().to_string()),
        gogdl_version: gogdl_present.then(|| cli_bins::pinned_gogdl_version().to_string()),
        legendary_path: legendary.to_string_lossy().to_string(),
        gogdl_path: gogdl.to_string_lossy().to_string(),
    })
}

/// Delete + re-fetch the legendary binary. Used by Settings → Downloads →
/// Re-download. Returns the new on-disk path.
#[tauri::command]
pub async fn redownload_legendary(app: AppHandle) -> Result<String, String> {
    let path = cli_bins::redownload_legendary(&app).await?;
    Ok(path.to_string_lossy().to_string())
}

/// Delete + re-fetch the gogdl binary. See `redownload_legendary`.
#[tauri::command]
pub async fn redownload_gogdl(app: AppHandle) -> Result<String, String> {
    let path = cli_bins::redownload_gogdl(&app).await?;
    Ok(path.to_string_lossy().to_string())
}

// ── Install-status reconciliation ─────────────────────────────────────────

/// Count of DB rows whose install_path was refreshed by `detect_installs`,
/// split per source so the toast can read "Epic: 3, GOG: 1".
#[derive(Debug, Serialize)]
pub struct DetectInstallsResult {
    pub epic_updated: u32,
    pub gog_updated: u32,
}

/// Walk legendary's `installed.json` + the user's GOG download dirs for
/// `goggame-*.info` markers, then back-fill `install_path` on every matching
/// owned-library row in the DB.
///
/// Why: when a download finishes the orchestrator already writes the path
/// in `handle_completion`, but the user can also install through the
/// underlying launcher (or move files manually), and we'd never observe
/// those. This command is the catch-up — safe to run on every Epic/GOG
/// library sync (the auto-hook below) and from a Settings button.
///
/// Best-effort: a missing legendary binary, a runner that exits non-zero,
/// or unreadable GOG dirs are logged and counted as zero updates for that
/// source — the other source still gets processed.
#[tauri::command]
pub async fn detect_installs(
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<DetectInstallsResult, String> {
    let mut epic_updated: u32 = 0;
    let mut gog_updated: u32 = 0;

    // ── Epic via legendary list-installed ──
    match legendary::ensure_exe(&app).await {
        Ok(legendary_exe) => {
            // The CLI call is synchronous and reads ~installed.json — fast
            // but we still hop to a blocking worker so we don't stall the
            // async executor while legendary parses its disk state.
            let installs = tauri::async_runtime::spawn_blocking(move || {
                legendary::all_installed(&legendary_exe)
            })
            .await
            .map_err(|e| format!("legendary scan worker panicked: {}", e))?;
            match installs {
                Ok(pairs) => {
                    for (app_name, install_path) in pairs {
                        match db.set_install_path_by_platform(
                            "epic",
                            &app_name,
                            &install_path,
                        ) {
                            Ok(true) => epic_updated += 1,
                            Ok(false) => {
                                // No DB row for this app_name yet — the user
                                // probably hasn't synced Epic library since
                                // they installed it. Not an error.
                            }
                            Err(e) => log::warn!(
                                "[detect_installs] DB update for epic/{} failed: {}",
                                app_name,
                                e
                            ),
                        }
                    }
                }
                Err(e) => log::warn!("[detect_installs] legendary scan failed: {}", e),
            }
        }
        Err(e) => log::warn!(
            "[detect_installs] legendary unavailable, skipping Epic scan: {}",
            e
        ),
    }

    // ── GOG via goggame-*.info scan of configured dirs ──
    let settings = downloads::load_settings(db.inner());
    let mut base_dirs: Vec<String> = Vec::new();
    if !settings.default_dir.is_empty() {
        base_dirs.push(settings.default_dir.clone());
    }
    if let Some(per) = settings.per_source.get("gog") {
        if !per.is_empty() && !base_dirs.iter().any(|d| d == per) {
            base_dirs.push(per.clone());
        }
    }
    let pairs = tauri::async_runtime::spawn_blocking(move || gogdl::scan_installs(&base_dirs))
        .await
        .map_err(|e| format!("gogdl scan worker panicked: {}", e))?;
    for (game_id, install_path) in pairs {
        match db.set_install_path_by_platform("gog", &game_id, &install_path) {
            Ok(true) => gog_updated += 1,
            Ok(false) => {}
            Err(e) => log::warn!(
                "[detect_installs] DB update for gog/{} failed: {}",
                game_id,
                e
            ),
        }
    }

    Ok(DetectInstallsResult {
        epic_updated,
        gog_updated,
    })
}
