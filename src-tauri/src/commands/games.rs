use tauri::State;

use crate::models::Game;
use crate::services::db::Database;
use crate::services::rsi_logs;
use crate::services::steam_collections;
use crate::services::steam_writers::{self, GridSlot};
use crate::services::steamgrid::{CoverOption, SteamGridClient};

/// Pick the SteamGridDB API key to use for outgoing requests.
///
/// Returns the user-configured key from `sync_state["steamgriddb_api_key"]`.
/// **No bundled default** — distributing the app with someone else's key would
/// risk getting that key abused/banned. The user must supply their own via
/// Settings → SteamGridDB → API Key (free at https://www.steamgriddb.com).
///
/// Callers should error gracefully and surface a clear "set your API key"
/// message to the UI when this returns `None`.
fn get_active_api_key(db: &Database) -> Option<String> {
    db.get_sync_state("steamgriddb_api_key")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

/// Convenience that converts the Option into the canonical user-facing error
/// string for commands that can't proceed without a key.
fn require_api_key(db: &Database) -> Result<String, String> {
    get_active_api_key(db).ok_or_else(|| {
        "SteamGridDB API key not set — open Settings → SteamGridDB and add your key (free at steamgriddb.com).".to_string()
    })
}

/// Read the user's preferred artwork style from sync_state. Returns `None`
/// when the user hasn't set a preference or chose "any" — the `SteamGridClient`
/// then omits the `styles=` query param and lets SGDB return everything.
///
/// NOTE: changing this preference does NOT refetch artwork for existing
/// games — only future `fetch_artwork` / `browse_*` calls observe the new
/// style. Re-running "Fetch all" or per-game "Refresh artwork" picks it up.
fn get_active_artwork_style(db: &Database) -> Option<String> {
    db.get_sync_state("steamgriddb_artwork_style")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty() && s != "any")
}

/// Read the user's "prefer animated" + "allow NSFW" auto-pick prefs from
/// sync_state. Both default to `false` when no key is present. Wired into
/// `SteamGridClient::with_prefs` at every call site that builds a client.
fn get_steamgrid_pick_prefs(db: &Database) -> (bool, bool) {
    let prefer_animated = db
        .get_sync_state("steamgriddb_prefer_animated")
        .ok()
        .flatten()
        .map(|s| s == "true")
        .unwrap_or(false);
    let allow_nsfw = db
        .get_sync_state("steamgriddb_allow_nsfw")
        .ok()
        .flatten()
        .map(|s| s == "true")
        .unwrap_or(false);
    (prefer_animated, allow_nsfw)
}

/// True when the user wants `add_game` (and future auto-fetch call sites)
/// to hit SteamGridDB automatically after an insert. Default true.
fn get_auto_fetch_enabled(db: &Database) -> bool {
    db.get_sync_state("steamgriddb_auto_fetch")
        .ok()
        .flatten()
        .map(|s| s != "false")
        .unwrap_or(true)
}

/// Get all games from the database.
#[tauri::command]
pub async fn get_all_games(db: State<'_, Database>) -> Result<Vec<Game>, String> {
    db.get_all_games().map_err(|e| e.to_string())
}

/// Get a single game by id (text hash).
#[tauri::command]
pub async fn get_game(id: String, db: State<'_, Database>) -> Result<Option<Game>, String> {
    db.get_game_by_id(&id).map_err(|e| e.to_string())
}

/// Add a game manually (e.g. from a custom path picker).
///
/// `add_game` currently does NOT trigger an artwork fetch — that's done
/// separately by the frontend after this command returns (or via
/// `fetch_all_artwork`). The `steamgriddb_auto_fetch` toggle in Settings is
/// already persisted; if/when we wire auto-fetch here, gate it on
/// `get_auto_fetch_enabled(&db)`. See the TODO marker below.
#[tauri::command]
pub async fn add_game(
    title: String,
    exe_path: String,
    source: Option<String>,
    launch_command: Option<String>,
    db: State<'_, Database>,
) -> Result<String, String> {
    let mut game = Game::new(
        title,
        exe_path,
        source.unwrap_or_else(|| "custom".to_string()),
    );
    game.launch_command = launch_command;
    let id = db.insert_game(&game).map_err(|e| e.to_string())?;

    // TODO(auto-fetch): when add_game starts kicking off an artwork fetch
    // after insert, guard it like this:
    //
    //   if get_auto_fetch_enabled(&db) {
    //       let key = get_active_api_key(&db);
    //       let style = get_active_artwork_style(&db);
    //       let client = SteamGridClient::new(key);
    //       let (cover, hero, logo) =
    //           client.fetch_artwork_for_game(&game.title, style.as_deref()).await;
    //       ...write back via db.update_artwork/hero/logo...
    //   }
    //
    // Today the toggle exists for forward compatibility; the auto-fetch
    // pathway is the explicit `fetch_artwork` command the UI calls.
    let _ = get_auto_fetch_enabled; // silence dead-code on this path

    Ok(id)
}

/// Delete a game (cascades to shortcuts and playtime sessions).
#[tauri::command]
pub async fn delete_game(id: String, db: State<'_, Database>) -> Result<(), String> {
    db.delete_game(&id).map_err(|e| e.to_string())
}

/// Manually set the `imported_playtime_seconds` of a game — used by
/// GameDetail's "Set playtime" menu item when the log-parsing import
/// undershoots (Star Citizen rotates its `logbackups/` after N files,
/// so logs older than ~2 weeks just don't exist anymore). The value
/// is treated as authoritative: the local-session counter is added
/// on top via `total_playtime_seconds`.
#[tauri::command]
pub async fn set_game_manual_playtime_hours(
    id: String,
    hours: f64,
    db: State<'_, Database>,
) -> Result<(), String> {
    let seconds = (hours.max(0.0) * 3600.0).round() as i64;
    db.set_manual_playtime_by_id(&id, seconds)
        .map_err(|e| e.to_string())
}

/// Scan the Star Citizen log backups across every installed channel
/// (LIVE / PTU / EPTU / TECH-PREVIEW) and write the total into
/// `games.imported_playtime_seconds` for every library row whose
/// `exe_path` points at the RSI Launcher. Idempotent: rerun whenever
/// the user closes the game to get the freshest count.
///
/// Returns the total minutes computed so the UI can surface "Star
/// Citizen playtime: 47h 12m" without a follow-up read.
///
/// Reference: TradSC `app_stats.rs::get_playtime` does the same
/// timestamp-bracket-per-log calculation.
#[tauri::command]
pub async fn import_starcitizen_playtime(
    db: State<'_, Database>,
) -> Result<i64, String> {
    let games = db.get_all_games().map_err(|e| e.to_string())?;
    let rsi_row = games
        .iter()
        .find(|g| {
            let exe = g.exe_path.to_lowercase();
            exe.contains("rsi launcher")
        });
    let Some(row) = rsi_row else {
        return Ok(0);
    };
    let Some(id) = row.id.clone() else {
        return Ok(0);
    };
    let seconds = rsi_logs::total_playtime_seconds(Some(&row.exe_path));
    if seconds <= 0 {
        return Ok(0);
    }
    // Mirror into `imported_playtime_seconds` — the Stats page's
    // `total_playtime_seconds` helper folds this in alongside any
    // locally-tracked sessions, so the user sees the right total.
    db.set_imported_playtime_by_id(&id, seconds)
        .map_err(|e| e.to_string())?;
    // Also bump `last_played_imported` to the most recent log mtime so
    // the Library "Recently Played" filter picks SC up properly.
    let last_played = rsi_logs::last_played_timestamp(Some(&row.exe_path));
    if last_played > 0 {
        // Reuse the by-platform helper's behavior by writing directly.
        let conn_db = (*db).clone();
        let _ = conn_db.set_imported_last_played_by_id(&id, last_played);
    }
    log::info!("[rsi_logs] imported Star Citizen playtime: {}s", seconds);
    Ok(seconds)
}

/// Set or clear the user-chosen display title for a game (the
/// "Rename" affordance in GameDetail's more-menu). Passing an empty
/// string or `null` reverts to the source-reported title.
#[tauri::command]
pub async fn set_game_custom_title(
    id: String,
    title: Option<String>,
    db: State<'_, Database>,
) -> Result<(), String> {
    db.set_custom_title(&id, title.as_deref())
        .map_err(|e| e.to_string())
}

/// Overwrite the user-curated tags for a game. The list is trimmed,
/// empty entries stripped, duplicates merged (case-insensitive — first
/// spelling wins) and stored as a JSON `Vec<String>`. Passing an empty
/// vec clears the column. Returns the canonicalised list so the UI
/// can re-render the chips with the exact stored order.
#[tauri::command]
pub async fn set_game_user_tags(
    id: String,
    tags: Vec<String>,
    db: State<'_, Database>,
) -> Result<Vec<String>, String> {
    let mut seen = std::collections::HashSet::<String>::new();
    let cleaned: Vec<String> = tags
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .filter(|s| seen.insert(s.to_lowercase()))
        .collect();
    if cleaned.is_empty() {
        db.set_user_tags(&id, None).map_err(|e| e.to_string())?;
        return Ok(Vec::new());
    }
    let json = serde_json::to_string(&cleaned).map_err(|e| e.to_string())?;
    db.set_user_tags(&id, Some(&json))
        .map_err(|e| e.to_string())?;
    Ok(cleaned)
}

/// Read the user-curated tags for a single game. Empty vec when the
/// game has none (also returned when the row doesn't exist — the UI
/// uses an empty list as a safe default).
#[tauri::command]
pub async fn get_game_user_tags(
    id: String,
    db: State<'_, Database>,
) -> Result<Vec<String>, String> {
    let raw = db.get_user_tags(&id).map_err(|e| e.to_string())?;
    Ok(raw
        .as_deref()
        .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
        .unwrap_or_default())
}

/// Toggle the favorite flag for a single game. Returns the new state
/// (true = favorited, false = not). Used by the heart icon in the
/// GameDetail header — the frontend optimistically updates its local
/// state from the returned bool without a follow-up read.
#[tauri::command]
pub async fn toggle_game_favorite(
    id: String,
    db: State<'_, Database>,
) -> Result<bool, String> {
    db.toggle_favorite(&id).map_err(|e| e.to_string())
}

/// Read the favorite flag for one game. False both when the game is
/// explicitly not favorited and when the id doesn't exist (the UI
/// uses `false` as a safe default).
#[tauri::command]
pub async fn get_game_favorite(
    id: String,
    db: State<'_, Database>,
) -> Result<bool, String> {
    Ok(db
        .get_favorite(&id)
        .map_err(|e| e.to_string())?
        .unwrap_or(false))
}

/// IDs of every favorited game. Used by the Library sidebar "Favorites"
/// pill — much cheaper than re-walking the entire games table on each
/// filter change.
#[tauri::command]
pub async fn list_favorite_game_ids(
    db: State<'_, Database>,
) -> Result<Vec<String>, String> {
    db.list_favorite_ids().map_err(|e| e.to_string())
}

/// IDs of games whose last play session lands within the given window
/// (defaults to 14 days, matching Steam's own "Recently Played" cutoff).
/// Drives the Library sidebar's "Recently Played" filter + count.
#[tauri::command]
pub async fn list_recently_played_ids(
    days: Option<i64>,
    db: State<'_, Database>,
) -> Result<Vec<String>, String> {
    let window = days.unwrap_or(14).max(1);
    db.list_recently_played_ids(window)
        .map_err(|e| e.to_string())
}

/// IDs of games with zero playtime anywhere (no local sessions AND no
/// imported total from Steam/GOG). Drives "Never Played" filter.
#[tauri::command]
pub async fn list_never_played_ids(
    db: State<'_, Database>,
) -> Result<Vec<String>, String> {
    db.list_never_played_ids().map_err(|e| e.to_string())
}

#[derive(Debug, serde::Serialize)]
pub struct PlayStatEntry {
    pub game_id: String,
    pub total_seconds: i64,
    /// Unix-seconds of the most-recent session, or null when there's
    /// never been one. Used to sort the Library by "last played".
    pub last_played: Option<i64>,
}

/// Per-game play stats for the Library sort dropdown. Returns a flat
/// vec the frontend can index into a Map<id, stats> for O(1) lookup
/// during sort. One SQL pass, recomputed whenever sort criteria changes.
#[tauri::command]
pub async fn list_game_play_stats(
    db: State<'_, Database>,
) -> Result<Vec<PlayStatEntry>, String> {
    let rows = db.list_game_play_stats().map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|(game_id, total_seconds, last_played)| PlayStatEntry {
            game_id,
            total_seconds,
            last_played,
        })
        .collect())
}

#[derive(Debug, serde::Serialize)]
pub struct SteamFavoritesImport {
    /// Name of the Steam-side collection we matched. Surfaced so the
    /// user sees which one was picked (helpful when they have multiple
    /// "fav-ish" collections).
    pub matched_collection: String,
    /// Count of games whose `favorite` flag was flipped to 1 — i.e.
    /// Steam-side appids that match a Tokoru Steam-source row.
    pub imported: usize,
    /// Count of Steam-side appids that had no matching Tokoru row
    /// (the user owns the game on Steam but it isn't in our DB yet —
    /// usually means the Steam library sync hasn't run for that title).
    pub unmatched: usize,
}

/// Read the user's Steam-side "Favorites" collection from cloudstorage
/// and flip the local `favorite` flag on every Tokoru row that has
/// a matching Steam appid. One-way import: changes to favorites in
/// Tokoru afterwards do NOT push back to Steam (yet — that needs
/// a Steam-closed window the way Collections rebuild does).
///
/// Matches the collection by name, case-insensitive, against the usual
/// English / French aliases. Returns an error when no collection
/// matches so the user can rename theirs or pick a different label.
#[tauri::command]
pub async fn import_steam_favorites(
    db: State<'_, Database>,
) -> Result<SteamFavoritesImport, String> {
    let user_id = steam_collections::current_steam_user_id()
        .ok_or_else(|| "Couldn't resolve the current Steam user id.".to_string())?;
    let all = steam_collections::read_user_collections(&user_id)
        .map_err(|e| e.to_string())?;

    // Tolerant name match — Steam's UI lets the user call this collection
    // anything they want; we cover the obvious EN/FR variants. Substring
    // match so decorations like "★ Favoris" still hit.
    const ALIASES: &[&str] = &["favorite", "favorites", "favoris", "favori", "fav"];
    let picked = all.iter().find(|c| {
        let lower = c.name.to_lowercase();
        ALIASES.iter().any(|a| lower.contains(a))
    });
    let Some(picked) = picked else {
        return Err(format!(
            "No Steam collection matching 'Favorites' / 'Favoris'. Steam-side collections found: {}",
            if all.is_empty() {
                "(none)".to_string()
            } else {
                all.iter()
                    .map(|c| format!("'{}'", c.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ));
    };

    let mut imported = 0usize;
    let mut unmatched = 0usize;
    for appid in &picked.appids {
        let appid_str = appid.to_string();
        match db.find_steam_id_by_appid(&appid_str).map_err(|e| e.to_string())? {
            Some(game_id) => {
                db.set_favorite(&game_id, true).map_err(|e| e.to_string())?;
                imported += 1;
            }
            None => {
                unmatched += 1;
            }
        }
    }

    log::info!(
        "[favorites] imported {}/{} from Steam collection '{}' ({} unmatched)",
        imported,
        picked.appids.len(),
        picked.name,
        unmatched
    );

    Ok(SteamFavoritesImport {
        matched_collection: picked.name.clone(),
        imported,
        unmatched,
    })
}

#[derive(Debug, serde::Serialize)]
pub struct PushFavoritesResult {
    /// Number of appids we pushed into the Steam Favoris collection.
    pub pushed: usize,
    /// True when Steam was running and we had to restart it. The frontend
    /// uses this to differentiate the silent path ("just wrote") from the
    /// noisy one ("Steam was restarted").
    pub steam_restarted: bool,
}

/// Push every Tokoru-favorited game into Steam's Favoris collection (or
/// create one if none exists). Auto-restarts Steam when it's running —
/// the cloudstorage JSON gets overwritten on Steam shutdown otherwise.
///
/// Each favorited Tokoru row resolves to a Steam appid:
///   - native Steam game: `platform_id` (the actual appid)
///   - non-Steam shortcut pushed via `shortcuts.vdf`: the computed
///     `steam_appid` from the `shortcuts` table
///
/// Rows that can't resolve (e.g. an Epic favorite that was never pushed
/// to Steam) are silently skipped — pushing them would dangle in Steam.
///
/// `restart_if_running` defaults to `true`. When `false`, the command
/// returns `SteamRunning` error so the caller can offer a "save for
/// later" affordance.
#[tauri::command]
pub async fn push_favorites_to_steam(
    restart_if_running: Option<bool>,
    app_handle: tauri::AppHandle,
    db: State<'_, Database>,
) -> Result<PushFavoritesResult, String> {
    let user_id = steam_collections::current_steam_user_id()
        .ok_or_else(|| "Couldn't resolve the current Steam user id.".to_string())?;

    // Resolve every favorited game id → Steam appid. Same heuristic as
    // the grid-mirror path (native Steam appid first, then the
    // shortcut's computed appid).
    let favorited_ids = db
        .list_favorite_ids()
        .map_err(|e| e.to_string())?;
    let mut appids: Vec<i64> = Vec::with_capacity(favorited_ids.len());
    let mut skipped = 0usize;
    for game_id in &favorited_ids {
        match resolve_steam_appid(&db, game_id) {
            Some(appid) => appids.push(appid as i64),
            None => skipped += 1,
        }
    }
    // Dedupe in case a game appears via both paths (shouldn't, but be
    // safe — Steam refuses duplicate ids inside `added`).
    appids.sort_unstable();
    appids.dedup();

    let was_running = steam_writers::is_steam_running();
    let restart = restart_if_running.unwrap_or(true);
    if was_running && !restart {
        return Err(steam_collections::CollectionsError::SteamRunning.to_string());
    }

    if was_running {
        // Close → write → relaunch. We reuse the same pattern as
        // `restart_steam`: shutdown gracefully, wait for the process to
        // exit, kill orphan steamwebhelper children, then write while
        // the window is open.
        let steam_root = steam_writers::find_steam_root()
            .ok_or_else(|| "Could not find your Steam install path.".to_string())?;
        let steam_exe = steam_root.join("steam.exe");
        if !steam_exe.exists() {
            return Err(format!("steam.exe not found at {}", steam_exe.display()));
        }
        // Notify Steam — graceful shutdown writes shortcuts.vdf /
        // localconfig.vdf cleanly before exiting.
        let _ = std::process::Command::new(&steam_exe)
            .arg("-shutdown")
            .spawn()
            .and_then(|mut c| c.wait());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while steam_writers::is_steam_running() {
            if std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        }
        steam_writers::kill_orphan_steamwebhelpers();
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;

        let count = steam_collections::push_favorites(&user_id, appids.clone())
            .map_err(|e| e.to_string())?;

        // Relaunch
        std::process::Command::new(&steam_exe)
            .spawn()
            .map_err(|e| format!("Could not relaunch Steam: {}", e))?;

        log::info!(
            "[favorites] pushed {} appids to Steam Favoris ({} skipped, Steam restarted)",
            count,
            skipped
        );
        let _ = app_handle; // reserved for future progress events
        Ok(PushFavoritesResult {
            pushed: count,
            steam_restarted: true,
        })
    } else {
        let count = steam_collections::push_favorites(&user_id, appids)
            .map_err(|e| e.to_string())?;
        log::info!(
            "[favorites] pushed {} appids to Steam Favoris ({} skipped, Steam was closed)",
            count,
            skipped
        );
        let _ = app_handle;
        Ok(PushFavoritesResult {
            pushed: count,
            steam_restarted: false,
        })
    }
}

/// Fetch cover/hero/logo from SteamGridDB for one game and persist the URLs.
/// Returns the resolved cover URL when available.
#[tauri::command]
pub async fn fetch_artwork(id: String, db: State<'_, Database>) -> Result<Option<String>, String> {
    let game = db
        .get_game_by_id(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Game not found".to_string())?;

    let api_key = require_api_key(&db)?;
    let style = get_active_artwork_style(&db);
    let (prefer_animated, allow_nsfw) = get_steamgrid_pick_prefs(&db);
    let client = SteamGridClient::new(api_key).with_prefs(prefer_animated, allow_nsfw);
    let needs_cover = game.artwork_url.is_none();
    let needs_hero = game.hero_url.is_none();
    let needs_logo = game.logo_url.is_none();
    let needs_icon = game.icon_url.is_none();

    let mut resolved_cover = game.artwork_url.clone();

    if !needs_cover && !needs_hero && !needs_logo && !needs_icon {
        return Ok(resolved_cover);
    }

    let (cover_url, hero_url, logo_url, icon_url) = client
        .fetch_artwork_for_game(&game.title, style.as_deref())
        .await;

    if needs_cover {
        if let Some(ref url) = cover_url {
            db.update_artwork(&id, url).map_err(|e| e.to_string())?;
            resolved_cover = Some(url.clone());
            mirror_to_steam_grid(&db, &id, GridSlot::Cover, url).await;
        }
    }
    if needs_hero {
        if let Some(ref url) = hero_url {
            db.update_hero(&id, url).map_err(|e| e.to_string())?;
            mirror_to_steam_grid(&db, &id, GridSlot::Hero, url).await;
        }
    }
    if needs_logo {
        if let Some(ref url) = logo_url {
            db.update_logo(&id, url).map_err(|e| e.to_string())?;
            mirror_to_steam_grid(&db, &id, GridSlot::Logo, url).await;
        }
    }
    if needs_icon {
        if let Some(ref url) = icon_url {
            db.update_icon(&id, url).map_err(|e| e.to_string())?;
            mirror_to_steam_grid(&db, &id, GridSlot::Icon, url).await;
        }
    }

    Ok(resolved_cover)
}

/// Live progress payload for the artwork backfill — emitted on the
/// `artwork-backfill-progress` event so the frontend can render a real
/// progress toast instead of a silent "running in background" message.
#[derive(serde::Serialize, Clone)]
pub struct ArtworkBackfillProgress {
    /// 1-based position of the game we just processed.
    pub current: usize,
    /// Total games scheduled (only the ones that needed at least one of
    /// cover/hero/logo — already-decorated games are NOT counted).
    pub total: usize,
    /// Title of the game we just processed (for the toast subtitle).
    pub title: String,
    /// Cumulative count of games that received at least one artwork.
    pub updated: usize,
    /// `true` on the final tick so the frontend knows to swap the toast
    /// from "Fetching..." to "Done".
    pub done: bool,
}

/// Sync_state key flipped to `"true"` after the very first full artwork
/// backfill completes. Used to gate boot/focus backfill runs to "only
/// the games newly inserted by the latest scan" once the lib has had
/// its initial pass — otherwise re-walking 800+ games every reboot
/// drowns the user in toasts for nothing.
const KEY_ARTWORK_INITIAL_DONE: &str = "artwork_initial_backfill_done";

/// True when the initial library-wide artwork backfill has already
/// completed once on this install. The frontend reads this on boot to
/// decide whether to ask for a full pass (false → first run) or to
/// pass only the freshly-inserted ids (true → incremental).
#[tauri::command]
pub async fn is_artwork_initial_backfill_done(
    db: State<'_, Database>,
) -> Result<bool, String> {
    Ok(db
        .get_sync_state(KEY_ARTWORK_INITIAL_DONE)
        .map_err(|e| e.to_string())?
        .map(|s| s == "true")
        .unwrap_or(false))
}

/// Fetch artwork for every game that's missing any of cover/hero/logo.
///
/// Emits `artwork-backfill-progress` events with the running counter so
/// the frontend can show a live "Fetching artwork: X/Y (Title)" toast.
/// The final event has `done=true` (current=total).
///
/// `only_ids` — when `Some`, restricts the backfill to just those game
/// ids (incremental "new games only" path called from boot/focus once
/// the initial full pass has already happened). When `None`, walks the
/// entire library; on success a `None` run flips
/// `artwork_initial_backfill_done` to `"true"` so subsequent boots take
/// the incremental path.
///
/// `force` — when `Some(true)`, the per-slot heuristics
/// (blank / low-quality / animation-mismatch) are skipped and EVERY
/// candidate game gets its slots re-fetched. Used by the Settings →
/// SteamGridDB "Refresh artwork now" button so the user can apply a
/// freshly-flipped preference (style / NSFW / static) to their existing
/// covers regardless of source — Steam, Epic, GOG, etc.
#[tauri::command]
pub async fn fetch_all_artwork(
    app_handle: tauri::AppHandle,
    only_ids: Option<Vec<String>>,
    force: Option<bool>,
    db: State<'_, Database>,
) -> Result<usize, String> {
    use tauri::Emitter;

    let all_games = db.get_all_games().map_err(|e| e.to_string())?;
    // Pre-filter to the requested subset BEFORE the slot-check so the
    // "X games pending out of Y" log reflects the actual scope of this
    // run (boot incremental = "out of N new games", not "out of 857").
    let games: Vec<crate::models::Game> = match &only_ids {
        Some(ids) => {
            let want: std::collections::HashSet<&str> =
                ids.iter().map(String::as_str).collect();
            all_games
                .into_iter()
                .filter(|g| g.id.as_deref().map_or(false, |id| want.contains(id)))
                .collect()
        }
        None => all_games,
    };
    let api_key = require_api_key(&db)?;
    let style = get_active_artwork_style(&db);
    let (prefer_animated, allow_nsfw) = get_steamgrid_pick_prefs(&db);
    let client = SteamGridClient::new(api_key).with_prefs(prefer_animated, allow_nsfw);
    let mut updated = 0;
    let is_full_pass = only_ids.is_none();

    // Treat both NULL and empty-string columns as "no artwork yet" so we
    // don't accidentally skip games whose row has `artwork_url = ""` (a
    // legacy state from earlier sync passes).
    let is_blank = |opt: &Option<String>| opt.as_deref().map_or(true, |s| s.is_empty());

    // GOG ships its native covers from `images.gog.com` at 200x300-ish —
    // pixelated on a modern 1440p library grid. Treat those as "needs
    // replacement" so the backfill upgrades them to a SteamGridDB pick
    // automatically, without any toggle to flip. We also tag Epic's
    // legacy `cdn1.epicgames.com/offer/` URLs the same way — those are
    // often 480px portraits, also too small. `cdn.akamai.steamstatic`
    // (Steam) and `cdn2.steamgriddb` (already SteamGridDB-sourced) stay
    // untouched.
    let is_low_quality_source = |opt: &Option<String>| {
        opt.as_deref().map_or(false, |url| {
            // GOG ships covers from `images-N.gog-statics.com` (NB: hyphen
            // in `gog-statics`) at `_glx_logo_2x` — a landscape logo
            // recentered inside a 600x900 portrait, which reads pixelated
            // on a modern library grid. Force the upgrade to SteamGridDB.
            url.contains("gog-statics.com")
                || url.contains("images.gog.com")
                || url.contains("img.youtube.com/vi")
                || url.starts_with("https://cdn1.epicgames.com/offer/")
        })
    };
    // Symmetric prefer_animated handling:
    //
    // - When ON: treat any static cover as eligible for an UPGRADE to an
    //   animated pick (ranker only swaps when SGDB actually has an animated
    //   variant). Survey of the user's 403 Steam apps: 195 have animated
    //   grids on SGDB, only 4 were caught by the previous SGDB-only filter,
    //   hence catching every static format below.
    //
    // - When OFF: treat any animated cover as eligible for a DOWNGRADE to a
    //   static pick. Used after the user toggles the pref off — without this
    //   the existing animated picks would stay forever. The new fetch picks
    //   a static cover, which for native Steam games comes from the local
    //   appcache (`fs::copy`, zero download).
    let is_artwork_eligible_for_refresh = move |opt: &Option<String>| {
        let Some(url) = opt.as_deref() else {
            return false;
        };
        let lower = url.to_ascii_lowercase();
        // Strip query string for extension sniffing.
        let stripped = lower.split('?').next().unwrap_or("");
        let is_definitely_animated = stripped.ends_with(".webm")
            || stripped.ends_with(".gif")
            || stripped.ends_with(".apng");
        let is_definitely_static = stripped.ends_with(".jpg")
            || stripped.ends_with(".jpeg")
            || stripped.ends_with(".png")
            || stripped.ends_with(".bmp");
        // `.webp` from SGDB is the painful ambiguous case: same extension
        // for static AND animated covers. The Steam Cloudflare CDN and
        // GOG images.gog.com ship `.webp` static only — treat those as
        // static. On `cdn2.steamgriddb.com` `.webp` is more often than
        // not the animated variant, so we treat it as animated when the
        // user wants to DOWNGRADE (OFF case) and as static-ish when the
        // user wants to UPGRADE (ON case, the ranker will swap if SGDB
        // has a `.webm`).
        let is_webp = stripped.ends_with(".webp");
        let is_sgdb = lower.contains("steamgriddb");
        if prefer_animated {
            // ON → eligible if currently static or a non-SGDB `.webp`. SGDB
            // `.webp` already animated → leave alone.
            is_definitely_static || (is_webp && !is_sgdb)
        } else {
            // OFF → eligible if currently animated OR an SGDB `.webp`
            // (likely animated). Steam-CDN / GOG `.webp` are static → skip.
            is_definitely_animated || (is_webp && is_sgdb)
        }
    };
    // Alias kept for the existing call sites below.
    let is_static_pending_animation = is_artwork_eligible_for_refresh;
    let _ = allow_nsfw; // currently consumed via SteamGridClient::with_prefs

    let force_all = force.unwrap_or(false);
    let needs_slot = |opt: &Option<String>| {
        // `force` from the Settings → "Refresh artwork" button skips the
        // per-slot heuristics so non-Steam sources (Epic/GOG/Ubi/EA) get
        // a real re-fetch even when their existing URLs look fine
        // (.png/.jpg from launcher CDNs that the default filter ignores
        // because they're neither animated nor low-res).
        if force_all && !is_blank(opt) {
            return true;
        }
        is_blank(opt)
            || is_low_quality_source(opt)
            || is_static_pending_animation(opt)
    };

    // Pre-filter: only count games that actually need work. Whatever
    // already has a Steam-CDN / user-picked artwork is left untouched —
    // the backfill ONLY fills missing slots OR replaces the low-res
    // launcher-native artworks (and SGDB static when prefer_animated).
    // Exception: the `force` mode considers every game with any URL set
    // eligible so the user can re-apply their full set of SteamGridDB
    // preferences across the whole library.
    let pending: Vec<&crate::models::Game> = games
        .iter()
        .filter(|g| {
            let any_slot_eligible = needs_slot(&g.artwork_url)
                || needs_slot(&g.hero_url)
                || needs_slot(&g.logo_url)
                || needs_slot(&g.icon_url);
            // In force mode, also include games with NO URLs set so they
            // get a fresh pick.
            let force_include = force_all
                && g.artwork_url.is_none()
                && g.hero_url.is_none()
                && g.logo_url.is_none()
                && g.icon_url.is_none();
            (any_slot_eligible || force_include) && g.id.is_some()
        })
        .collect();
    let total = pending.len();
    log::info!(
        "[artwork_backfill] mode={} force={} prefer_animated={} allow_nsfw={} → {} games pending out of {}",
        if is_full_pass { "full" } else { "incremental" },
        force_all,
        prefer_animated,
        allow_nsfw,
        total,
        games.len()
    );

    if total == 0 {
        // Still emit a single done event so any listening toast can
        // dismiss / show "nothing to fetch".
        let _ = app_handle.emit(
            "artwork-backfill-progress",
            ArtworkBackfillProgress {
                current: 0,
                total: 0,
                title: String::new(),
                updated: 0,
                done: true,
            },
        );
        // "Nothing to do" on a full pass still counts as the initial
        // backfill having happened — flip the gate so we stay on the
        // incremental path from now on.
        if is_full_pass {
            let _ = db.set_sync_state(KEY_ARTWORK_INITIAL_DONE, "true");
        }
        return Ok(0);
    }

    for (idx, game) in pending.iter().enumerate() {
        let id = game.id.as_ref().unwrap();
        // Re-check per-slot so a game that already has e.g. a great cover
        // but is missing hero/logo only gets the missing slots filled.
        // Slots considered "needs replacement" include both empty and
        // low-quality launcher-native URLs.
        let needs_cover = needs_slot(&game.artwork_url);
        let needs_hero = needs_slot(&game.hero_url);
        let needs_logo = needs_slot(&game.logo_url);
        let needs_icon = needs_slot(&game.icon_url);

        // Prefer the platform-keyed lookup when we know the game's source +
        // platform_id (steam appid, gog product id, …). This jumps straight
        // to the right SGDB entry — avoids the "God of War PS2 vs God of
        // War 2018 PC" disambiguation drama where `search_game` would
        // sometimes pick the wrong title because they share a name.
        let (cover_url, hero_url, logo_url, icon_url) = client
            .fetch_artwork_for_game_resolved(
                &game.title,
                Some(game.source.as_str()),
                game.platform_id.as_deref(),
                style.as_deref(),
            )
            .await;

        let mut changed = false;
        if needs_cover {
            if let Some(url) = cover_url {
                if db.update_artwork(id, &url).is_ok() {
                    changed = true;
                    mirror_to_steam_grid(&db, id, GridSlot::Cover, &url).await;
                }
            }
        }
        if needs_hero {
            if let Some(url) = hero_url {
                if db.update_hero(id, &url).is_ok() {
                    changed = true;
                    mirror_to_steam_grid(&db, id, GridSlot::Hero, &url).await;
                }
            }
        }
        if needs_logo {
            if let Some(url) = logo_url {
                if db.update_logo(id, &url).is_ok() {
                    changed = true;
                    mirror_to_steam_grid(&db, id, GridSlot::Logo, &url).await;
                }
            }
        }
        if needs_icon {
            if let Some(url) = icon_url {
                if db.update_icon(id, &url).is_ok() {
                    changed = true;
                    mirror_to_steam_grid(&db, id, GridSlot::Icon, &url).await;
                }
            }
        }

        if changed {
            updated += 1;
        }

        let _ = app_handle.emit(
            "artwork-backfill-progress",
            ArtworkBackfillProgress {
                current: idx + 1,
                total,
                title: game.title.clone(),
                updated,
                done: idx + 1 == total,
            },
        );

        // Small delay to stay polite with SteamGridDB.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    // Mark "initial full backfill happened" so the next boots take the
    // incremental new-games-only path. Only flip the flag for a true
    // full pass (`only_ids = None`) — incremental runs leave the gate
    // alone since they don't cover the whole library.
    if is_full_pass {
        let _ = db.set_sync_state(KEY_ARTWORK_INITIAL_DONE, "true");
    }

    Ok(updated)
}

/// Browse all available covers from SteamGridDB for a game name.
#[tauri::command]
pub async fn browse_covers(
    game_name: String,
    db: State<'_, Database>,
) -> Result<Vec<CoverOption>, String> {
    let api_key = require_api_key(&db)?;
    let style = get_active_artwork_style(&db);
    let (prefer_animated, allow_nsfw) = get_steamgrid_pick_prefs(&db);
    let client = SteamGridClient::new(api_key).with_prefs(prefer_animated, allow_nsfw);
    client.browse_all_covers(&game_name, style.as_deref()).await
}

/// Browse hero banners from SteamGridDB for a game name.
#[tauri::command]
pub async fn browse_heroes(
    game_name: String,
    db: State<'_, Database>,
) -> Result<Vec<CoverOption>, String> {
    let api_key = require_api_key(&db)?;
    let style = get_active_artwork_style(&db);
    let (prefer_animated, allow_nsfw) = get_steamgrid_pick_prefs(&db);
    let client = SteamGridClient::new(api_key).with_prefs(prefer_animated, allow_nsfw);
    client.browse_heroes(&game_name, style.as_deref()).await
}

/// Browse logo variants from SteamGridDB for a game name.
#[tauri::command]
pub async fn browse_logos(
    game_name: String,
    db: State<'_, Database>,
) -> Result<Vec<CoverOption>, String> {
    let api_key = require_api_key(&db)?;
    let style = get_active_artwork_style(&db);
    let (prefer_animated, allow_nsfw) = get_steamgrid_pick_prefs(&db);
    let client = SteamGridClient::new(api_key).with_prefs(prefer_animated, allow_nsfw);
    client.browse_logos(&game_name, style.as_deref()).await
}

/// Browse icon variants from SteamGridDB for a game name. Drives the
/// "Icon" tab in GameDetail browse covers.
#[tauri::command]
pub async fn browse_icons(
    game_name: String,
    db: State<'_, Database>,
) -> Result<Vec<CoverOption>, String> {
    let api_key = require_api_key(&db)?;
    let style = get_active_artwork_style(&db);
    let (prefer_animated, allow_nsfw) = get_steamgrid_pick_prefs(&db);
    let client = SteamGridClient::new(api_key).with_prefs(prefer_animated, allow_nsfw);
    client.browse_icons(&game_name, style.as_deref()).await
}

/// Sync_state key flipped to `"true"` after we've mirrored every existing
/// Tokoru artwork into Steam's `userdata/<id>/config/grid/` folder at least
/// once. Catch-up gate for libraries whose artwork was fetched by an older
/// Tokoru build that wrote to the DB but NOT to Steam's grid override.
const KEY_STEAM_GRID_MIRROR_DONE: &str = "steam_grid_mirror_done";

/// Walk every Tokoru game that has any of cover/hero/logo set and mirror
/// those URLs into Steam's grid folder. Idempotent: re-running on an
/// already-mirrored library is fine (Steam reads on demand, and
/// `write_grid_image` overwrites the existing file).
///
/// Gated by the `steam_grid_mirror_done` flag — runs the first time the
/// frontend asks for it on this install, then short-circuits forever.
/// New artwork written *after* that gate flips is mirrored synchronously
/// by `fetch_artwork` / `fetch_all_artwork` (added 2026-05) so there's
/// no drift.
///
/// Returns the count of grid files actually written (cover+hero+logo
/// across all games). 0 when the gate is already closed.
///
/// Emits `steam-grid-mirror-progress` events so the frontend can show a
/// live "Mirroring artwork to Steam X/Y — Title" toast (same shape as
/// the artwork backfill progress so the UI can reuse the toast helper).
#[tauri::command]
pub async fn mirror_existing_artwork_to_steam(
    app_handle: tauri::AppHandle,
    db: State<'_, Database>,
) -> Result<usize, String> {
    use tauri::Emitter;

    let done = db
        .get_sync_state(KEY_STEAM_GRID_MIRROR_DONE)
        .map_err(|e| e.to_string())?
        .map(|s| s == "true")
        .unwrap_or(false);
    if done {
        // Emit a single done event so the frontend listener can short-
        // circuit cleanly — keeps the toast-helper code symmetrical with
        // the artwork backfill path.
        let _ = app_handle.emit(
            "steam-grid-mirror-progress",
            SteamGridMirrorProgress {
                current: 0,
                total: 0,
                title: String::new(),
                written: 0,
                done: true,
            },
        );
        return Ok(0);
    }

    let games = db.get_all_games().map_err(|e| e.to_string())?;
    // Pre-filter: only games that (a) have an id, (b) can resolve to a
    // Steam appid, and (c) have at least one artwork URL worth mirroring.
    // Counting upfront makes the progress denominator honest — without
    // this the user would see "1/857" jumping straight to "212/857" as
    // we skipped 211 non-Steam rows.
    let candidates: Vec<&crate::models::Game> = games
        .iter()
        .filter(|g| g.id.is_some())
        .filter(|g| resolve_steam_appid(&db, g.id.as_deref().unwrap()).is_some())
        .filter(|g| {
            g.artwork_url.as_deref().map_or(false, |s| !s.is_empty())
                || g.hero_url.as_deref().map_or(false, |s| !s.is_empty())
                || g.logo_url.as_deref().map_or(false, |s| !s.is_empty())
        })
        .collect();
    let total = candidates.len();
    log::info!(
        "[steam_grid_mirror] catch-up starting: {} games to mirror out of {}",
        total,
        games.len()
    );

    if total == 0 {
        let _ = db.set_sync_state(KEY_STEAM_GRID_MIRROR_DONE, "true");
        let _ = app_handle.emit(
            "steam-grid-mirror-progress",
            SteamGridMirrorProgress {
                current: 0,
                total: 0,
                title: String::new(),
                written: 0,
                done: true,
            },
        );
        return Ok(0);
    }

    let mut written = 0usize;
    for (idx, game) in candidates.iter().enumerate() {
        let id = game.id.as_deref().unwrap();
        if let Some(url) = game.artwork_url.as_deref().filter(|s| !s.is_empty()) {
            mirror_to_steam_grid(&db, id, GridSlot::Cover, url).await;
            written += 1;
        }
        if let Some(url) = game.hero_url.as_deref().filter(|s| !s.is_empty()) {
            mirror_to_steam_grid(&db, id, GridSlot::Hero, url).await;
            written += 1;
        }
        if let Some(url) = game.logo_url.as_deref().filter(|s| !s.is_empty()) {
            mirror_to_steam_grid(&db, id, GridSlot::Logo, url).await;
            written += 1;
        }
        let _ = app_handle.emit(
            "steam-grid-mirror-progress",
            SteamGridMirrorProgress {
                current: idx + 1,
                total,
                title: game.title.clone(),
                written,
                done: idx + 1 == total,
            },
        );

        // Throttle so Steam (which watches the grid folder via its CEF
        // library renderer) has time to ingest each new file without
        // queuing a 1200-image redecode burst. Empirically 50ms keeps
        // Steam's RAM/CPU smooth without dragging the catch-up loop
        // (50ms × ~1200 ≈ +1 minute total).
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let _ = db.set_sync_state(KEY_STEAM_GRID_MIRROR_DONE, "true");
    log::info!(
        "[steam_grid_mirror] catch-up wrote {} grid files across {} games",
        written,
        total
    );
    Ok(written)
}

/// Live progress payload for the Steam grid mirror catch-up. Same shape
/// as `ArtworkBackfillProgress` so the frontend toast helpers stay
/// symmetric.
#[derive(serde::Serialize, Clone)]
pub struct SteamGridMirrorProgress {
    pub current: usize,
    pub total: usize,
    pub title: String,
    /// Cumulative grid files written so far (1-3 per game).
    pub written: usize,
    pub done: bool,
}

/// Resolve the Steam appid to use for grid-file overrides for a given
/// Tokoru game id:
///   * Native Steam game (`source = "steam"`) → its `platform_id` parsed as
///     u32 (the Steam appid).
///   * Non-Steam shortcut → the `steam_appid` we computed when the user
///     pushed the shortcut into `shortcuts.vdf`. None when not pushed yet.
fn resolve_steam_appid(db: &Database, game_id: &str) -> Option<u32> {
    let game = db.get_game_by_id(game_id).ok().flatten()?;
    if game.source == "steam" {
        return game.platform_id.as_deref().and_then(|p| p.parse::<u32>().ok());
    }
    let shortcut = db.get_shortcut(game_id).ok().flatten()?;
    // Shortcut appids are stored as u64 in our model to fit SQLite's signed
    // i64 range, but Steam's grid filenames want the 32-bit value.
    Some(shortcut.steam_appid as u32)
}

/// Best-effort mirror an artwork choice into Steam's own grid folder so the
/// Steam library shows the same picture. Failures are logged and ignored so
/// they don't break the Tokoru-side save.
async fn mirror_to_steam_grid(db: &Database, game_id: &str, slot: GridSlot, image_url: &str) {
    let Some(appid) = resolve_steam_appid(db, game_id) else {
        log::debug!(
            "[set_game_*] no Steam appid for {} (not in Steam library and no shortcut pushed) — skipping grid mirror",
            game_id
        );
        return;
    };
    if let Err(e) = steam_writers::write_grid_image(appid, slot, image_url).await {
        log::warn!(
            "[set_game_*] grid mirror for game {} (appid {}) failed: {}",
            game_id,
            appid,
            e
        );
    }
}

/// Set a specific cover URL for a game.
#[tauri::command]
pub async fn set_game_cover(
    id: String,
    cover_url: String,
    db: State<'_, Database>,
) -> Result<(), String> {
    db.update_artwork(&id, &cover_url).map_err(|e| e.to_string())?;
    mirror_to_steam_grid(&db, &id, GridSlot::Cover, &cover_url).await;
    Ok(())
}

/// Set a specific hero/banner URL for a game.
#[tauri::command]
pub async fn set_game_hero(
    id: String,
    hero_url: String,
    db: State<'_, Database>,
) -> Result<(), String> {
    db.update_hero(&id, &hero_url).map_err(|e| e.to_string())?;
    mirror_to_steam_grid(&db, &id, GridSlot::Hero, &hero_url).await;
    Ok(())
}

/// Set a specific logo URL for a game.
#[tauri::command]
pub async fn set_game_logo(
    id: String,
    logo_url: String,
    db: State<'_, Database>,
) -> Result<(), String> {
    db.update_logo(&id, &logo_url).map_err(|e| e.to_string())?;
    mirror_to_steam_grid(&db, &id, GridSlot::Logo, &logo_url).await;
    Ok(())
}

#[derive(Debug, serde::Serialize)]
pub struct SetIconResult {
    /// True when Steam was running and we had to restart it. Frontend
    /// uses this to surface a "Steam restarted" toast instead of the
    /// silent path.
    pub steam_restarted: bool,
}

/// Set a specific icon URL for a game.
///
/// Auto-restarts Steam when it's running — the cloudstorage / shortcuts
/// VDF files get overwritten on Steam shutdown otherwise, so the icon
/// change wouldn't stick. For native Steam games, we also mirror into
/// `appcache/librarycache/`; for shortcuts we update the `icon` field
/// in `shortcuts.vdf`. Both are SARM-equivalent paths.
#[tauri::command]
pub async fn set_game_icon(
    id: String,
    icon_url: String,
    db: State<'_, Database>,
) -> Result<SetIconResult, String> {
    db.update_icon(&id, &icon_url).map_err(|e| e.to_string())?;

    let was_running = steam_writers::is_steam_running();
    if !was_running {
        // Fast path — Steam closed, just write.
        mirror_to_steam_grid(&db, &id, GridSlot::Icon, &icon_url).await;
        return Ok(SetIconResult {
            steam_restarted: false,
        });
    }

    // Steam is running — close gracefully, write, relaunch. Same dance
    // as `push_favorites_to_steam`. Failing to find steam.exe is fatal
    // for the restart but we still attempt the write (the user can
    // restart Steam manually).
    let steam_root = steam_writers::find_steam_root()
        .ok_or_else(|| "Could not find your Steam install path.".to_string())?;
    let steam_exe = steam_root.join("steam.exe");
    if !steam_exe.exists() {
        return Err(format!("steam.exe not found at {}", steam_exe.display()));
    }

    let _ = std::process::Command::new(&steam_exe)
        .arg("-shutdown")
        .spawn()
        .and_then(|mut c| c.wait());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while steam_writers::is_steam_running() {
        if std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }
    steam_writers::kill_orphan_steamwebhelpers();
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    mirror_to_steam_grid(&db, &id, GridSlot::Icon, &icon_url).await;

    std::process::Command::new(&steam_exe)
        .spawn()
        .map_err(|e| format!("Could not relaunch Steam: {}", e))?;

    log::info!("[set_game_icon] icon updated for {} (Steam restarted)", id);
    Ok(SetIconResult {
        steam_restarted: true,
    })
}
