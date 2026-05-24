use std::collections::HashSet;
use std::path::PathBuf;
use tauri::State;

use crate::services::db::Database;
use crate::services::platforms;
use crate::services::scanner::Scanner;

#[derive(serde::Serialize)]
pub struct ScanResult {
    /// Count of rows newly inserted by this scan (NOT touched by an update
    /// only). Equals `new_game_ids.len()` — both kept for backwards-compat
    /// with the existing toast UIs.
    pub new_games: usize,
    pub total_found: usize,
    /// Ids of the newly inserted rows, in insertion order. Frontend uses
    /// this to drive the incremental artwork backfill so we only fetch
    /// artwork for the freshly-added games on subsequent boots — the first
    /// boot still does a full pass (see `artwork_initial_backfill_done`).
    pub new_game_ids: Vec<String>,
}

/// Snapshot the current set of game ids so the caller can later tell which
/// returned ids are newly inserted (= absent from the snapshot).
fn snapshot_ids(db: &Database) -> HashSet<String> {
    db.list_all_game_ids()
        .map(|v| v.into_iter().collect())
        .unwrap_or_default()
}

/// Scan local directories (Scanner::default_scan_dirs) for game executables.
#[tauri::command]
pub async fn scan_local_games(db: State<'_, Database>) -> Result<ScanResult, String> {
    let pre = snapshot_ids(&db);
    let dirs = Scanner::default_scan_dirs();
    let games = Scanner::scan_directories(&dirs);
    let total_found = games.len();
    let mut new_game_ids: Vec<String> = Vec::new();

    for game in &games {
        if !db.game_exists_by_path(&game.exe_path).unwrap_or(true) {
            if let Ok(id) = db.insert_game(game) {
                if !pre.contains(&id) {
                    new_game_ids.push(id);
                }
            }
        }
    }

    Ok(ScanResult {
        new_games: new_game_ids.len(),
        total_found,
        new_game_ids,
    })
}

/// Scan a specific custom directory provided by the user.
#[tauri::command]
pub async fn scan_custom_directory(
    path: String,
    db: State<'_, Database>,
) -> Result<ScanResult, String> {
    let pre = snapshot_ids(&db);
    let dirs = vec![PathBuf::from(path)];
    let games = Scanner::scan_directories(&dirs);
    let total_found = games.len();
    let mut new_game_ids: Vec<String> = Vec::new();

    for game in &games {
        if !db.game_exists_by_path(&game.exe_path).unwrap_or(true) {
            if let Ok(id) = db.insert_game(game) {
                if !pre.contains(&id) {
                    new_game_ids.push(id);
                }
            }
        }
    }

    Ok(ScanResult {
        new_games: new_game_ids.len(),
        total_found,
        new_game_ids,
    })
}

/// Detect installed games from all supported launchers via local registry /
/// manifest reads. No account auth required.
#[tauri::command]
pub async fn detect_platform_games(db: State<'_, Database>) -> Result<ScanResult, String> {
    let pre = snapshot_ids(&db);
    let games = platforms::detect_all_installed_games();
    let total_found = games.len();
    let mut new_game_ids: Vec<String> = Vec::new();

    for game in &games {
        // Steam games route through the appid-aware merge so they converge on
        // a single row with the Steam Web API sync.
        let result = if game.source == "steam" {
            db.upsert_game_from_local(game)
        } else {
            db.upsert_game(game)
        };
        if let Ok(id) = result {
            if !pre.contains(&id) {
                new_game_ids.push(id);
            }
        }
    }

    Ok(ScanResult {
        new_games: new_game_ids.len(),
        total_found,
        new_game_ids,
    })
}

/// Scan ONE launcher source — used by the per-source Rescan button so the
/// toast count actually reflects what that launcher brought in. Falls back
/// to a directory walk filtered by `source` for sources without a dedicated
/// registry detector (ea / itch / heroic / custom).
#[tauri::command]
pub async fn scan_source(
    source: String,
    db: State<'_, Database>,
) -> Result<ScanResult, String> {
    let pre = snapshot_ids(&db);
    let mut games = platforms::detect_for_source(&source);
    if games.is_empty() {
        // No dedicated detector — fall back to the directory walker and
        // keep only the entries the scanner tagged with this source.
        let local = Scanner::scan_directories(&Scanner::default_scan_dirs());
        games = local.into_iter().filter(|g| g.source == source).collect();
    }
    let total_found = games.len();
    let mut new_game_ids: Vec<String> = Vec::new();
    for game in &games {
        // Steam routes through the appid-aware merge; everything else
        // either upserts on (source, platform_id) when we have an id, or
        // falls back to an insert keyed by exe_path for the rare detector
        // that doesn't surface a platform_id (custom dirs, EA Origin
        // legacy keys with no `subkey` id, etc.). All branches reduce to
        // `Result<String, _>` so we can detect new ids uniformly.
        let result: Result<Option<String>, String> = if game.source == "steam" {
            db.upsert_game_from_local(game)
                .map(Some)
                .map_err(|e| e.to_string())
        } else if game.source != "custom" && game.platform_id.as_deref().is_some() {
            db.upsert_game(game).map(Some).map_err(|e| e.to_string())
        } else if !db.game_exists_by_path(&game.exe_path).unwrap_or(true) {
            db.insert_game(game).map(Some).map_err(|e| e.to_string())
        } else {
            Ok(None)
        };
        if let Ok(Some(id)) = result {
            if !pre.contains(&id) {
                new_game_ids.push(id);
            }
        }
    }
    log::info!(
        "[scan] source={} total_found={} new={}",
        source,
        total_found,
        new_game_ids.len()
    );
    Ok(ScanResult {
        new_games: new_game_ids.len(),
        total_found,
        new_game_ids,
    })
}

/// Count games per source for the Onboarding wizard's detection step.
///
/// Uses `db.count_games_by_source` (the FULL library count, which
/// includes API-synced rows from Steam Web API / Epic OAuth / GOG OAuth)
/// — NOT just the registry/manifest-based local install detection.
/// Local-only detection would dramatically undercount: a Steam user with
/// 400 owned games but only 8 currently installed would see "8 Steam
/// games" in the wizard, which is misleading.
///
/// Falls back to a fresh local detect for sources that have no DB rows
/// yet AND are detected on-disk — that's the legitimate "first boot,
/// nothing synced yet, but we can see Epic / GOG are installed" case
/// so the user knows the source will pick games up later.
#[tauri::command]
pub async fn detect_installed_counts_per_source(
    db: State<'_, Database>,
) -> Result<std::collections::HashMap<String, usize>, String> {
    let mut out = std::collections::HashMap::new();
    // Heroic is intentionally excluded — Tokoru natively scans Epic / GOG, so
    // surfacing Heroic as a separate source in the Onboarding grid would be
    // redundant noise. Xbox / Amazon stay in for users who actually use them.
    for source in ["steam", "epic", "gog", "ubi", "ea", "itch", "xbox", "amazon"] {
        // Real library count first (includes API-synced rows).
        let db_count = db.count_games_by_source(source).unwrap_or(0) as usize;
        if db_count > 0 {
            out.insert(source.to_string(), db_count);
            continue;
        }
        // No DB rows yet: surface the local-install count so the user at
        // least sees their installed games before any sync runs. Detector
        // returning empty just means "source not present" → 0.
        let local = platforms::detect_for_source(source).len();
        out.insert(source.to_string(), local);
    }
    Ok(out)
}

/// Full scan: platform-installed detection + local exe scan.
/// Tokoru has no account-based imports in v0 — that's launcher territory.
#[tauri::command]
pub async fn full_scan(db: State<'_, Database>) -> Result<ScanResult, String> {
    let pre = snapshot_ids(&db);
    let mut all_found = 0;
    let mut new_game_ids: Vec<String> = Vec::new();

    // 1. Installed platform games (registry + manifest reads)
    let platform_games = platforms::detect_all_installed_games();
    all_found += platform_games.len();
    for game in &platform_games {
        // Steam games route through the appid-aware merge so they converge on
        // a single row with the Steam Web API sync.
        let result = if game.source == "steam" {
            db.upsert_game_from_local(game)
        } else {
            db.upsert_game(game)
        };
        if let Ok(id) = result {
            if !pre.contains(&id) {
                new_game_ids.push(id);
            }
        }
    }

    // 2. Local exe scan (default dirs)
    let local_games = Scanner::scan_directories(&Scanner::default_scan_dirs());
    all_found += local_games.len();
    for game in &local_games {
        if !db.game_exists_by_path(&game.exe_path).unwrap_or(true) {
            if let Ok(id) = db.insert_game(game) {
                if !pre.contains(&id) {
                    new_game_ids.push(id);
                }
            }
        }
    }

    Ok(ScanResult {
        new_games: new_game_ids.len(),
        total_found: all_found,
        new_game_ids,
    })
}
