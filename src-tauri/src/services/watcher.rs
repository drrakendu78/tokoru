//! Passive process watcher.
//!
//! Runs a tokio task that refreshes the system process list every 5 seconds
//! and matches running processes against the `exe_path` column of every game
//! in our library. Started/stopped transitions are persisted in
//! `playtime_sessions` so the localconfig sync (`steam_writers`) can later
//! report accurate hours to Steam.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use sysinfo::{Pid, System};
use tokio::time;

use crate::services::db::Database;
use crate::services::steam_writers;

const TICK_INTERVAL_SECS: u64 = 5;
const SYNC_TICK_INTERVAL_SECS: u64 = 60;

/// Launch both background loops: the per-process watcher and the localconfig
/// sync loop. Never blocks — both run on separate spawned tasks.
///
/// Uses `tauri::async_runtime` (which wraps tokio) so we don't depend on a
/// `tokio` runtime being set as the thread-local current handle. Tauri's
/// `setup` closure runs on the main thread *before* the runtime is associated
/// with it; `tauri::async_runtime::spawn` knows to dispatch onto its own
/// runtime regardless of caller context.
pub fn start_background_tasks(db: Database) {
    let watcher_db = db.clone();
    tauri::async_runtime::spawn(async move {
        watcher_loop(watcher_db).await;
    });
    tauri::async_runtime::spawn(async move {
        sync_loop(db).await;
    });
}

/// Best-effort lower-cased filename extracted from a stored exe path. Returns
/// an empty string if the path is empty.
fn exe_basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default()
}

async fn watcher_loop(db: Database) {
    log::info!("steamshelf: process watcher started ({}s tick)", TICK_INTERVAL_SECS);
    let mut sys = System::new();
    // game_id -> open session row id
    let mut open_sessions: HashMap<String, i64> = HashMap::new();

    loop {
        time::sleep(Duration::from_secs(TICK_INTERVAL_SECS)).await;

        sys.refresh_processes();
        let processes: Vec<(Pid, String, String)> = sys
            .processes()
            .iter()
            .map(|(pid, p)| {
                let exe_path = p
                    .exe()
                    .map(|p| p.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                let name = p.name().to_ascii_lowercase();
                (*pid, name, exe_path)
            })
            .collect();

        let games = match db.get_all_games() {
            Ok(g) => g,
            Err(e) => {
                log::error!("watcher: get_all_games failed: {}", e);
                continue;
            }
        };

        let now = chrono::Utc::now().timestamp();
        let mut still_running: HashMap<String, bool> = HashMap::new();

        for game in &games {
            let Some(id) = &game.id else { continue };
            let raw = game.exe_path.trim();
            if raw.is_empty() {
                continue;
            }
            let target_full = raw.to_ascii_lowercase();
            let target_base = exe_basename(raw);

            // RSI Launcher special case: the row's exe_path points at
            // `RSI Launcher.exe`, but only `StarCitizen.exe` should count
            // as "playing". Otherwise the user would accumulate hours
            // every time they just opened the launcher to read news.
            // Reference: TradSC `gamepath.rs::get_launcher_activity_status`
            // tracks `StarCitizen.exe` for the same reason.
            let is_rsi_entry =
                target_full.contains("rsi launcher") || target_base.contains("rsi launcher");
            let is_running = if is_rsi_entry {
                // Match ONLY on StarCitizen.exe — the launcher process
                // is explicitly ignored.
                processes.iter().any(|(_, name, _)| name == "starcitizen.exe")
            } else {
                processes.iter().any(|(_, name, exe)| {
                    if !target_full.is_empty() && exe == &target_full {
                        return true;
                    }
                    if !target_base.is_empty()
                        && (name == &target_base || exe.ends_with(&target_base))
                    {
                        if is_launcher_proxy(name) {
                            return false;
                        }
                        return true;
                    }
                    false
                })
            };

            if is_running {
                still_running.insert(id.clone(), true);
                if !open_sessions.contains_key(id) {
                    match db.start_playtime_session(id, now) {
                        Ok(row) => {
                            log::info!("watcher: started session for {} ({})", game.title, id);
                            open_sessions.insert(id.clone(), row);
                        }
                        Err(e) => {
                            log::error!("watcher: failed to start session for {}: {}", id, e);
                        }
                    }
                }
            }
        }

        // Close sessions for games that stopped this tick.
        let to_close: Vec<String> = open_sessions
            .keys()
            .filter(|id| !still_running.contains_key(*id))
            .cloned()
            .collect();
        for id in to_close {
            if let Some(row) = open_sessions.remove(&id) {
                // Use latest session row to compute duration.
                if let Ok(sessions) = db.get_playtime_sessions(&id) {
                    if let Some(session) = sessions.iter().find(|s| s.id == Some(row)) {
                        let duration = now - session.started_at;
                        let _ = db.end_playtime_session(row, now, duration.max(0));
                        log::info!("watcher: closed session for {} ({}s)", id, duration);
                    }
                }
            }
        }
    }
}

/// Heuristic: process names known to be just launcher shells. Helps avoid
/// counting "Epic is open" as "Hades is running" when the user only matched
/// by filename.
fn is_launcher_proxy(name: &str) -> bool {
    matches!(
        name,
        "epicgameslauncher.exe"
            | "galaxyclient.exe"
            | "galaxyclient"
            | "upc.exe"
            | "ubisoftconnect.exe"
            | "eadesktop.exe"
            | "ea desktop.exe"
            | "ea-desktop.exe"
            | "originwebhelperservice.exe"
            | "battle.net.exe"
            | "battlenet.exe"
            | "rockstargameslauncher.exe"
            | "amazongames.exe"
            | "xboxapp.exe"
            | "gamingservices.exe"
    )
}

async fn sync_loop(db: Database) {
    log::info!(
        "steamshelf: localconfig sync loop started ({}s tick)",
        SYNC_TICK_INTERVAL_SECS
    );
    let mut was_running = steam_writers::is_steam_running();
    loop {
        time::sleep(Duration::from_secs(SYNC_TICK_INTERVAL_SECS)).await;

        let is_running = steam_writers::is_steam_running();
        let just_closed = was_running && !is_running;
        was_running = is_running;

        if is_running {
            continue;
        }
        if just_closed {
            log::info!("sync_loop: Steam just closed — running playtime sync");
        }

        match steam_writers::sync_playtime_to_steam(&db) {
            Ok(result) => {
                if result.updated_count > 0 {
                    log::info!(
                        "sync_loop: wrote playtime for {} appid(s)",
                        result.updated_count
                    );
                    let now = chrono::Utc::now().timestamp().to_string();
                    let _ = db.set_sync_state("last_synced_to_steam_at", &now);
                }
            }
            Err(e) => {
                log::warn!("sync_loop: sync skipped: {}", e);
            }
        }
    }
}
