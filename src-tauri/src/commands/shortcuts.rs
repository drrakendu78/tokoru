//! Tauri commands for non-Steam shortcuts + localconfig playtime sync.

use std::collections::HashMap;

use tauri::State;

use crate::models::{Shortcut, ShortcutStatus};
use crate::services::db::Database;
use crate::services::steam_collections::{
    self, Collection, CollectionsError, STEAMSHELF_TAG,
};
use crate::services::steam_writers;

#[derive(Debug, serde::Serialize)]
pub struct PushResult {
    pub appid: u32,
    /// True when the real Steam Collection (sidebar entry) was refreshed in
    /// Steam's leveldb. False when we either skipped (Steam running) or
    /// couldn't reach the DB.
    pub collection_updated: bool,
    /// User-facing message when the collection wasn't refreshed. None when
    /// `collection_updated == true`.
    pub collection_error: Option<String>,
}

/// Append (or upsert) a non-Steam shortcut for the given game into
/// `shortcuts.vdf`, then mirror the row in our `shortcuts` table.
///
/// After the shortcut write succeeds we ALSO refresh the per-source Steam
/// Collection in the leveldb so the game shows up in a real sidebar group
/// (not just a tag) on Steam's next launch. If Steam is running, the leveldb
/// step is skipped — the frontend toasts about it but the push itself stays
/// successful (the shortcuts.vdf is still written).
#[tauri::command]
pub async fn push_to_steam(
    game_id: String,
    db: State<'_, Database>,
) -> Result<PushResult, String> {
    let game = db
        .get_game_by_id(&game_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Game not found".to_string())?;

    // Steam-native games are already in the user's Steam library — pushing a
    // shortcut for them would create a duplicate entry that fights the real
    // game in Big Picture / Steam Input. Refuse server-side as a safety net.
    if game.source == "steam" {
        return Err("This game is already in Steam — no shortcut needed.".to_string());
    }

    // Reuse the existing shortcut's appid when we've pushed this game
    // before — otherwise renaming the row (custom_title in GameDetail)
    // would change the computed appid and orphan the Steam-side entry
    // (the user would see "no playtime" because we'd be writing to a
    // different appid than what Steam holds).
    let existing_appid = db
        .get_shortcut(&game_id)
        .map_err(|e| e.to_string())?
        .map(|s| s.steam_appid as u32);
    let appid = steam_writers::push_game(&game, existing_appid)?;

    let now = chrono::Utc::now().timestamp();
    let shortcut = Shortcut {
        game_id: game_id.clone(),
        steam_appid: appid as u64,
        pushed_at: Some(now),
        status: ShortcutStatus::Pushed,
    };
    db.upsert_shortcut(&shortcut).map_err(|e| e.to_string())?;

    // Rebuild every Tokoru collection from the current DB state. The
    // single leveldb write replaces ALL our entries atomically — so even if
    // the user pushes one game at a time, the sidebar always reflects what
    // Tokoru thinks is pushed right now.
    let (collection_updated, collection_error) = match rebuild_all_collections(&db) {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e.to_string())),
    };

    Ok(PushResult {
        appid,
        collection_updated,
        collection_error,
    })
}

/// Remove the shortcut from `shortcuts.vdf` and mark the row as `removed`
/// (we keep the row so the UI can still display past status). Also rebuilds
/// the per-source Steam Collection so the sidebar entry reflects the
/// removal.
#[tauri::command]
pub async fn remove_from_steam(
    game_id: String,
    db: State<'_, Database>,
) -> Result<(), String> {
    let game = db
        .get_game_by_id(&game_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Game not found".to_string())?;

    steam_writers::remove_game(&game)?;

    if let Some(existing) = db.get_shortcut(&game_id).map_err(|e| e.to_string())? {
        let updated = Shortcut {
            status: ShortcutStatus::Removed,
            pushed_at: existing.pushed_at,
            ..existing
        };
        db.upsert_shortcut(&updated).map_err(|e| e.to_string())?;
    }

    // Best-effort — don't surface the rebuild error to the user here. If
    // Steam is running, the removal is recorded but the sidebar won't update
    // until the next push or a manual "Rebuild Steam Collections" click.
    if let Err(e) = rebuild_all_collections(&db) {
        log::warn!("Skipped Collections rebuild on remove: {}", e);
    }

    Ok(())
}

#[tauri::command]
pub async fn get_shortcut(
    game_id: String,
    db: State<'_, Database>,
) -> Result<Option<Shortcut>, String> {
    db.get_shortcut(&game_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_all_shortcuts(db: State<'_, Database>) -> Result<Vec<Shortcut>, String> {
    db.get_all_shortcuts().map_err(|e| e.to_string())
}

/// Manual trigger for the localconfig.vdf sync (the background loop runs it
/// automatically every 60s, but Settings exposes a button for clarity).
#[tauri::command]
pub async fn sync_playtime_now(
    db: State<'_, Database>,
) -> Result<steam_writers::SyncResult, String> {
    let result = steam_writers::sync_playtime_to_steam(&db)?;
    if result.updated_count > 0 {
        let now = chrono::Utc::now().timestamp().to_string();
        let _ = db.set_sync_state("last_synced_to_steam_at", &now);
    }
    Ok(result)
}

/// Surfaced for the Settings UI status block.
#[tauri::command]
pub async fn get_last_sync_at(db: State<'_, Database>) -> Result<Option<i64>, String> {
    let v = db
        .get_sync_state("last_synced_to_steam_at")
        .map_err(|e| e.to_string())?;
    Ok(v.and_then(|s| s.parse::<i64>().ok()))
}

/// Manual trigger for the leveldb Steam Collections rebuild. Wired to the
/// Settings → Steam integration "Rebuild Steam Collections" button.
///
/// Returns the number of distinct source groups written. Errors out with a
/// human-readable string when Steam is running or the leveldb path can't be
/// resolved.
#[tauri::command]
pub async fn sync_collections_now(db: State<'_, Database>) -> Result<u32, String> {
    let collections = build_collections_from_db(&db)?;
    let count = collections.len() as u32;
    let user_id = steam_collections::current_steam_user_id()
        .ok_or_else(|| "Could not resolve current Steam user id.".to_string())?;
    steam_collections::write_collections(&user_id, &collections).map_err(|e| e.to_string())?;
    Ok(count)
}

/// Read the user's preferred Collections grouping mode. Returns one of
/// `"platform"`, `"franchise"`, `"none"`. Used by Settings to render the
/// current selection.
#[tauri::command]
pub async fn get_collections_mode(db: State<'_, Database>) -> Result<String, String> {
    Ok(match load_collections_mode(&db) {
        CollectionsMode::Platform => "platform".to_string(),
        CollectionsMode::Franchise => "franchise".to_string(),
        CollectionsMode::None => "none".to_string(),
    })
}

/// Persist the user's Collections grouping mode. Does NOT rebuild the
/// Steam Collections file — the caller is expected to follow up with
/// `restart_steam`, which atomically shuts Steam down, rebuilds, and
/// relaunches. Splitting these two steps lets the UI display "Saved" +
/// "Restarting Steam…" as a coherent flow instead of a confusing
/// "Steam is running" toast.
#[tauri::command]
pub async fn set_collections_mode(
    mode: String,
    db: State<'_, Database>,
) -> Result<(), String> {
    if !matches!(mode.as_str(), "platform" | "franchise" | "none") {
        return Err(format!("unknown collections mode: {}", mode));
    }
    db.set_sync_state(COLLECTIONS_MODE_KEY, &mode)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Gracefully restart Steam so newly-pushed shortcuts.vdf entries appear in
/// the library immediately (Steam reads the file at startup, not while
/// running). The flow: send Steam the graceful `-shutdown` command, wait up
/// to 15s for the process to exit, then relaunch via the `steam` executable.
///
/// Returns once Steam has been relaunched (or with an error if either step
/// failed). The caller (frontend) should toast accordingly.
///
/// **Important**: this command also rebuilds the Steam Collections leveldb
/// AFTER Steam has shut down but BEFORE relaunching it. That's the only
/// safe window — the leveldb is locked by Steam while it runs, and Steam
/// overwrites our writes on startup with what it had in memory. By rebuilding
/// in the gap, the relaunched Steam reads our refreshed collections fresh
/// from disk.
#[tauri::command]
pub async fn restart_steam(db: State<'_, Database>) -> Result<(), String> {
    use std::process::Command;
    use std::time::Duration;

    // 1. Locate Steam — same heuristic as `shortcuts_vdf_path` uses
    //    (registry → standard install paths). For the relaunch we need the
    //    actual `steam.exe` path.
    let steam_root = steam_writers::find_steam_root()
        .ok_or_else(|| "Could not find your Steam install path.".to_string())?;
    let steam_exe = steam_root.join("steam.exe");
    if !steam_exe.exists() {
        return Err(format!(
            "steam.exe not found at {}",
            steam_exe.display()
        ));
    }

    // 2. If Steam isn't running, just launch it.
    let was_running = steam_writers::is_steam_running();
    if was_running {
        // 3. Ask Steam to shut down gracefully (this writes shortcuts.vdf /
        //    localconfig.vdf cleanly before exiting — never SIGKILL).
        let status = Command::new(&steam_exe)
            .arg("-shutdown")
            .spawn()
            .and_then(|mut c| c.wait())
            .map_err(|e| format!("Could not launch `steam -shutdown`: {}", e))?;
        if !status.success() {
            log::warn!(
                "steam -shutdown returned non-zero ({:?}) — will still wait for process to exit",
                status.code()
            );
        }

        // 4. Wait for steam.exe to actually disappear (max 15s).
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while steam_writers::is_steam_running() {
            if std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }

        // 4b. `steam -shutdown` exits cleanly but sometimes leaves the
        //     steamwebhelper.exe children alive. Those Chromium processes
        //     are what own the htmlcache leveldb — if they're still up,
        //     our Collections write will see a locked DB or, worse, a
        //     half-flushed WAL (which is what we hit in testing — the
        //     `cloud-storage-namespace` keys lived in 000009.log and
        //     never made it into a .ldb SSTable).
        steam_writers::kill_orphan_steamwebhelpers();

        // Give Windows a tick to actually reap the killed processes and
        // release their file handles before we try to open the leveldb.
        tokio::time::sleep(Duration::from_millis(800)).await;
    }

    // 5. Steam is now closed → rebuild Collections in the leveldb +
    //    push playtime totals into localconfig.vdf. Same closed-window
    //    treatment — Steam would overwrite both on shutdown otherwise.
    if let Err(e) = rebuild_all_collections(&db) {
        log::warn!("Collections rebuild during restart failed: {}", e);
    }
    match steam_writers::sync_playtime_to_steam(&db) {
        Ok(r) => log::info!(
            "[restart_steam] playtime sync wrote {} appid(s)",
            r.updated_count
        ),
        Err(e) => log::warn!("playtime sync during restart failed: {}", e),
    }

    // 6. Relaunch Steam.
    Command::new(&steam_exe)
        .spawn()
        .map_err(|e| format!("Could not relaunch Steam: {}", e))?;
    Ok(())
}

// ============ Helpers ============

/// User-facing choice for how Tokoru groups games into Steam
/// Collections. Persisted via `sync_state["collections_mode"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollectionsMode {
    /// One collection per source (Epic Games, GOG Galaxy, Ubi …). Default.
    Platform,
    /// Group games sharing the same franchise (title-prefix matching).
    /// Solo titles (no siblings) are skipped — a collection of one is noise.
    Franchise,
    /// No automatic collections. Tokoru still pushes shortcuts but the
    /// sidebar stays as-is.
    None,
}

impl CollectionsMode {
    fn from_str(s: &str) -> Self {
        match s {
            "franchise" => CollectionsMode::Franchise,
            "none" => CollectionsMode::None,
            _ => CollectionsMode::Platform,
        }
    }
}

const COLLECTIONS_MODE_KEY: &str = "collections_mode";

fn load_collections_mode(db: &Database) -> CollectionsMode {
    db.get_sync_state(COLLECTIONS_MODE_KEY)
        .ok()
        .flatten()
        .map(|s| CollectionsMode::from_str(&s))
        .unwrap_or(CollectionsMode::Platform)
}

/// One enriched row from the DB used to populate Collections. Carries the
/// metadata signals the franchise-mode multi-pass grouping needs (canonical
/// franchise from IGDB / Wikidata, developer, top tag) alongside the
/// always-present (appid, source, title).
struct GameEntry {
    appid: i64,
    source: String,
    title: String,
    /// Authoritative franchise from IGDB (preferred) or Wikidata, when the
    /// metadata sync has filled it in. None for rows never enriched.
    franchise: Option<String>,
    /// Developer string from Steam Store (canonical) or SteamSpy (fallback).
    /// We use this as a secondary grouping signal after explicit franchise
    /// and title-prefix have run.
    developer: Option<String>,
    /// Top community tag from SteamSpy (the highest-voted entry in the
    /// JSON `Vec<(name, votes)>`). Drives the "tag → franchise-like name"
    /// mapping (Hack and Slash → Diablo-like, Souls-like → Souls-like).
    top_tag: Option<String>,
}

/// Tag → display-name mapping used to project the top SteamSpy tag into a
/// franchise-looking collection. Only includes tags that read as a genre /
/// sub-genre identity, NOT overly broad tags like "Open World" or "Action"
/// which would create giant catch-all bins. Order matters: earlier entries
/// win when multiple tags above the threshold would map (we only ever map
/// the highest-voted top tag, so order is mostly cosmetic).
const TAG_TO_DISPLAY_NAME: &[(&str, &str)] = &[
    ("Hack and Slash", "Diablo-like"),
    ("ARPG", "Diablo-like"),
    ("Action RPG", "Diablo-like"),
    ("Souls-like", "Souls-like"),
    ("Soulslike", "Souls-like"),
    ("Soulsborne", "Souls-like"),
    ("Roguelike", "Roguelike"),
    ("Roguelite", "Roguelike"),
    ("Survival Horror", "Survival Horror"),
    ("Horror", "Horror"),
    ("Battle Royale", "Battle Royale"),
    ("Auto Battler", "Auto Battler"),
    ("MOBA", "MOBA"),
    ("Metroidvania", "Metroidvania"),
    ("Tower Defense", "Tower Defense"),
    ("City Builder", "City Builder"),
    ("Colony Sim", "Colony Sim"),
    ("4X", "4X Strategy"),
    ("Grand Strategy", "Grand Strategy"),
    ("Real Time Tactics", "Real-Time Tactics"),
    ("CRPG", "CRPG"),
    ("JRPG", "JRPG"),
    ("Visual Novel", "Visual Novel"),
    ("Dating Sim", "Dating Sim"),
    ("Walking Simulator", "Walking Simulator"),
    ("Bullet Hell", "Bullet Hell"),
    ("Shoot 'Em Up", "Shoot 'Em Up"),
    ("Boomer Shooter", "Boomer Shooter"),
    ("Extraction Shooter", "Extraction Shooter"),
    ("Tactical RPG", "Tactical RPG"),
    ("Turn-Based Tactics", "Turn-Based Tactics"),
    ("Survival", "Survival"),
    ("Crafting", "Crafting & Survival"),
    ("Farming Sim", "Farming Sim"),
    ("Life Sim", "Life Sim"),
    ("Idle", "Idle"),
    ("Clicker", "Idle"),
    ("Deckbuilder", "Deckbuilder"),
    ("Card Battler", "Deckbuilder"),
    ("Rhythm", "Rhythm"),
];

/// Extract the highest-voted tag from a SteamSpy `tags` JSON string. The
/// column stores `Vec<(name, votes)>` sorted descending by votes, so the
/// first entry is already the winner — we just need to parse it out.
fn top_tag_from_json(json: Option<&str>) -> Option<String> {
    let raw = json?;
    let parsed: Vec<(String, u64)> = serde_json::from_str(raw).ok()?;
    parsed.into_iter().next().map(|(name, _)| name)
}

/// Look up a tag in the franchise-like display-name mapping. Returns the
/// projected name (e.g. "Diablo-like") when the tag is in the table,
/// `None` when it's too broad / not mapped.
fn tag_display_name(tag: &str) -> Option<&'static str> {
    let lowered = tag.to_lowercase();
    TAG_TO_DISPLAY_NAME
        .iter()
        .find(|(needle, _)| needle.to_lowercase() == lowered)
        .map(|(_, display)| *display)
}

/// Enumerate every game eligible to land in a Tokoru-managed
/// collection, enriched with the franchise-resolution signals from
/// `list_franchise_signals`:
///   * Steam-native games (source=steam) → `platform_id` IS the Steam appid.
///   * Non-Steam shortcuts the user pushed → the CRC32 we recorded in
///     `shortcuts.steam_appid` when writing `shortcuts.vdf`.
fn enumerate_game_entries(db: &Database) -> Result<Vec<GameEntry>, String> {
    let rows = db.list_franchise_signals().map_err(|e| e.to_string())?;
    let shortcuts: HashMap<String, u64> = db
        .get_all_shortcuts()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|s| s.status == ShortcutStatus::Pushed)
        .map(|s| (s.game_id, s.steam_appid))
        .collect();

    let mut out: Vec<GameEntry> = Vec::new();
    for (id, source, platform_id, title, franchise, developer, tags_json) in rows {
        let appid_opt = if source == "steam" {
            platform_id.as_deref().and_then(|p| p.parse::<i64>().ok())
        } else {
            shortcuts.get(&id).map(|n| *n as i64)
        };
        let Some(appid) = appid_opt else { continue };
        out.push(GameEntry {
            appid,
            source,
            title,
            franchise: franchise.filter(|s| !s.trim().is_empty()),
            developer: developer.filter(|s| !s.trim().is_empty()),
            top_tag: top_tag_from_json(tags_json.as_deref()),
        });
    }
    Ok(out)
}

/// Tokenise a title into significant alphanumeric words, dropping
/// leading articles. Used by both `franchise_key` (2-word primary key)
/// and `franchise_fallback_key` (1-word fallback).
fn franchise_tokens(title: &str) -> Vec<String> {
    let words: Vec<&str> = title
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .collect();
    let skip = matches!(
        words.first().map(|w| w.to_ascii_lowercase()).as_deref(),
        Some("the") | Some("a") | Some("an")
    ) as usize;
    words.iter().skip(skip).map(|w| w.to_ascii_lowercase()).collect()
}

/// Primary franchise key: first 2 significant words.
/// "Dragon Ball Z: Kakarot" → "dragon ball" matches "Dragon Ball FighterZ".
fn franchise_key(title: &str) -> Option<String> {
    let tokens = franchise_tokens(title);
    if tokens.is_empty() {
        return None;
    }
    let key = tokens.iter().take(2).cloned().collect::<Vec<_>>().join(" ");
    if key.is_empty() {
        None
    } else {
        Some(key)
    }
}

/// Fallback franchise key: just the first significant word. Used when a
/// game is a singleton under its 2-word key — falling back to 1 word
/// often glues "Yakuza 0" and "Yakuza Kiwami 2" together.
fn franchise_fallback_key(title: &str) -> Option<String> {
    let tokens = franchise_tokens(title);
    tokens.into_iter().next()
}

/// Return the first 1 or 2 words of `title` (skipping a leading article),
/// in their original capitalisation, for use as a human-readable
/// collection name. `len` should be 1 or 2.
fn franchise_display_name(title: &str, len: usize) -> String {
    let words: Vec<&str> = title
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .collect();
    let skip = matches!(
        words.first().map(|w| w.to_ascii_lowercase()).as_deref(),
        Some("the") | Some("a") | Some("an")
    ) as usize;
    words.iter().skip(skip).take(len).copied().collect::<Vec<_>>().join(" ")
}

/// Detect demo / playtest / beta titles for the auto-grouped "Demos"
/// collection. Loose match: any of these words anywhere in the title.
fn is_demo_title(title: &str) -> bool {
    let lower = title.to_lowercase();
    [" demo", " demo ", "(demo)", " playtest", "beta test"]
        .iter()
        .any(|needle| lower.contains(needle))
        || lower.ends_with(" demo")
        || lower.starts_with("demo ")
}

/// Build the full list of source-grouped Collections from the current DB
/// state. The grouping strategy depends on the user's `collections_mode`
/// setting (Platform / Franchise / None). Used by `push_to_steam`,
/// `remove_from_steam`, the manual `sync_collections_now` command, and
/// `set_collections_mode` (which re-runs to apply the new grouping).
fn build_collections_from_db(db: &Database) -> Result<Vec<Collection>, String> {
    let mode = load_collections_mode(db);
    if mode == CollectionsMode::None {
        // Writing an empty Vec to `write_collections` strips every existing
        // Tokoru-managed collection and leaves user-made ones intact.
        return Ok(Vec::new());
    }

    let entries = enumerate_game_entries(db)?;

    // Helper: one collection per non-Steam source (Epic Games, GOG Galaxy,
    // Ubisoft Connect, …). Used as the WHOLE strategy in Platform mode
    // and as a fallback layer in Franchise mode so non-Steam shortcuts
    // always land in their store collection regardless of whether
    // they're also part of a franchise.
    let platform_collections = |entries: &[GameEntry]| -> Vec<Collection> {
        let mut by_source: HashMap<String, Vec<i64>> = HashMap::new();
        for e in entries {
            if e.source == "steam" {
                // Steam-native games stay out of Tokoru-managed
                // platform groups — Steam already labels them itself.
                continue;
            }
            by_source.entry(e.source.clone()).or_default().push(e.appid);
        }
        by_source
            .into_iter()
            .map(|(source, appids)| Collection {
                name: steam_writers::category_tag_for_source(&source),
                appids,
            })
            .collect()
    };

    let mut collections: Vec<Collection> = match mode {
        CollectionsMode::Platform => platform_collections(&entries),
        CollectionsMode::Franchise => {
            // Multi-pass grouping, most authoritative signal first. A game
            // is "claimed" by the first pass that produces a 2+ group it
            // belongs to; remaining unclaimed games fall through to the
            // next pass. Solo titles after every pass are dropped (Steam's
            // built-in "Uncategorized" bin keeps them visible).
            //
            // Demos are pulled out into a dedicated "Demos" group BEFORE
            // every other pass so they never pollute franchise collections.
            //
            // Pass 1: explicit franchise from IGDB / Wikidata
            //   "Yakuza 0" + "Like a Dragon: Ishin" → "Like a Dragon"
            //   (IGDB knows these belong together — title-prefix never
            //   would).
            // Pass 2: 2-word title prefix
            //   "Dragon Ball Z: Kakarot" + "Dragon Ball FighterZ" → "Dragon Ball"
            // Pass 3: 1-word title prefix fallback
            //   "Yakuza 0" + "Yakuza Kiwami" → "Yakuza" when 2-word missed
            // Pass 4: top SteamSpy tag mapped to a franchise-like name
            //   Singletons whose top tag is "Hack and Slash" → "Diablo-like"
            // Pass 5: developer name when 2+ singletons share one studio
            //   FromSoftware orphans → "FromSoftware" bin (rare; last
            //   resort before giving up).
            let (demos, regulars): (Vec<&GameEntry>, Vec<&GameEntry>) =
                entries.iter().partition(|e| is_demo_title(&e.title));

            // `claimed` tracks every appid already absorbed by a finalized
            // group. Each later pass only considers entries NOT in here.
            let mut claimed: std::collections::HashSet<i64> = std::collections::HashSet::new();
            let mut out: Vec<Collection> = Vec::new();

            let push_groups = |out: &mut Vec<Collection>,
                               claimed: &mut std::collections::HashSet<i64>,
                               groups: HashMap<String, (String, Vec<i64>)>| {
                for (_key, (display, appids)) in groups {
                    if appids.len() >= 2 {
                        for &id in &appids {
                            claimed.insert(id);
                        }
                        out.push(Collection {
                            name: display,
                            appids,
                        });
                    }
                }
            };

            // Pass 1 — explicit franchise from IGDB / Wikidata. Always
            // applied, regardless of count: 1 game with a known franchise
            // still beats title-prefix grouping (it's authoritative). For
            // the singleton-display-name we still want 2+ to surface as
            // a collection — Steam's "Uncategorized" handles the rest.
            let mut igdb_groups: HashMap<String, (String, Vec<i64>)> = HashMap::new();
            for e in &regulars {
                let Some(fr) = e.franchise.as_deref() else { continue };
                let key = fr.to_lowercase();
                let entry = igdb_groups
                    .entry(key)
                    .or_insert_with(|| (fr.to_string(), Vec::new()));
                entry.1.push(e.appid);
            }
            push_groups(&mut out, &mut claimed, igdb_groups);

            // Pass 2 — 2-word title prefix on unclaimed entries.
            let mut primary_groups: HashMap<String, (String, Vec<i64>)> = HashMap::new();
            for e in regulars.iter().filter(|e| !claimed.contains(&e.appid)) {
                let Some(key) = franchise_key(&e.title) else { continue };
                let display = franchise_display_name(&e.title, 2);
                let entry = primary_groups
                    .entry(key)
                    .or_insert_with(|| (display.clone(), Vec::new()));
                entry.1.push(e.appid);
            }
            push_groups(&mut out, &mut claimed, primary_groups);

            // Pass 3 — 1-word title prefix on remaining unclaimed.
            let mut fallback_groups: HashMap<String, (String, Vec<i64>)> = HashMap::new();
            for e in regulars.iter().filter(|e| !claimed.contains(&e.appid)) {
                let Some(key) = franchise_fallback_key(&e.title) else { continue };
                let display = franchise_display_name(&e.title, 1);
                let entry = fallback_groups
                    .entry(key)
                    .or_insert_with(|| (display.clone(), Vec::new()));
                entry.1.push(e.appid);
            }
            push_groups(&mut out, &mut claimed, fallback_groups);

            // Pass 4 — tag → franchise-like name mapping.
            let mut tag_groups: HashMap<String, (String, Vec<i64>)> = HashMap::new();
            for e in regulars.iter().filter(|e| !claimed.contains(&e.appid)) {
                let Some(tag) = e.top_tag.as_deref() else { continue };
                let Some(display) = tag_display_name(tag) else { continue };
                let key = display.to_lowercase();
                let entry = tag_groups
                    .entry(key)
                    .or_insert_with(|| (display.to_string(), Vec::new()));
                entry.1.push(e.appid);
            }
            push_groups(&mut out, &mut claimed, tag_groups);

            // Pass 5 — developer fallback. Last resort for orphans whose
            // studio is recognizable enough to be useful (FromSoftware,
            // Supergiant, etc.). Skips overly generic studio names by
            // requiring 2+ titles to share the exact same string.
            let mut dev_groups: HashMap<String, (String, Vec<i64>)> = HashMap::new();
            for e in regulars.iter().filter(|e| !claimed.contains(&e.appid)) {
                let Some(dev) = e.developer.as_deref() else { continue };
                let key = dev.to_lowercase();
                let entry = dev_groups
                    .entry(key)
                    .or_insert_with(|| (dev.to_string(), Vec::new()));
                entry.1.push(e.appid);
            }
            push_groups(&mut out, &mut claimed, dev_groups);

            // Dedicated Demos group — any title containing "demo" /
            // "playtest" / "beta test" lands here, regardless of whether
            // it would have grouped by franchise.
            if !demos.is_empty() {
                out.push(Collection {
                    name: "Demos".to_string(),
                    appids: demos.iter().map(|e| e.appid).collect(),
                });
            }

            // Plus the per-source bucket for non-Steam shortcuts so they
            // always show under Epic Games / GOG Galaxy / Ubisoft / etc.
            // even when they don't have a franchise sibling.
            out.extend(platform_collections(&entries));
            out
        }
        CollectionsMode::None => unreachable!(),
    };

    // Deterministic order helps tests and produces stable diffs in the file.
    collections.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(collections)
}

/// Rebuild every Tokoru-tagged Collection in Steam's leveldb from the
/// current DB state. Returns a typed error so the caller can decide how to
/// surface it (push_to_steam includes the message in PushResult; remove
/// only logs it).
fn rebuild_all_collections(db: &Database) -> Result<(), CollectionsError> {
    let collections = build_collections_from_db(db).map_err(CollectionsError::Other)?;
    let user_id = steam_collections::current_steam_user_id().ok_or_else(|| {
        CollectionsError::Other(
            "Could not resolve current Steam user id (userdata dir missing?).".to_string(),
        )
    })?;
    steam_collections::write_collections(&user_id, &collections)
}

// Force the `STEAMSHELF_TAG` import to count as used in release builds —
// it's referenced via the const elsewhere but Rust's dead-code lint can
// trip if the consumer is `cfg`-gated. Keep this assert cheap.
#[allow(dead_code)]
const _: () = {
    assert!(!STEAMSHELF_TAG.is_empty());
};
