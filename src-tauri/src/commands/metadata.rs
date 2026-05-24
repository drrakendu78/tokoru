//! Multi-source metadata enrichment commands.
//!
//! Sources (each independently fallible — a 5xx from one never breaks the
//! others):
//!
//! | Source        | Auth          | Fields                                          |
//! |---------------|---------------|-------------------------------------------------|
//! | Steam Store   | none          | description, header, screenshots, genres,       |
//! |               |               | categories, developer, publisher                |
//! | SteamSpy      | none          | tags (votes), developer, publisher              |
//! | IGDB          | Twitch OAuth  | franchise, themes, similar_games, igdb_id       |
//! | HowLongToBeat | none (scrape) | hltb_main_hours                                 |
//! | Wikidata      | none (SPARQL) | franchise (fallback when IGDB empty)            |
//!
//! Merge policy when sources overlap on a field:
//! - **description / header / screenshots / genres / categories** → Steam
//!   Store wins (canonical, official store copy)
//! - **developer / publisher** → Steam Store first, SteamSpy fills the gap
//! - **tags** → SteamSpy only (Steam Store has no community tags)
//! - **franchise** → IGDB (authoritative `franchise` field) > Wikidata
//!   (P179 series) — never derived from titles here (the title-prefix
//!   heuristic lives in `commands/shortcuts.rs` and runs separately as a
//!   safety net for un-enriched rows)
//! - **themes / similar_games / igdb_id** → IGDB only
//! - **hltb_main_hours** → HLTB only
//!
//! Cadence: callable on demand from Settings. We re-sync rows whose
//! `metadata_synced_at` is older than `STALE_AFTER_SECONDS` so users don't
//! pay the API cost every launch. Force-refresh exposed via `force=true`.
//!
//! IGDB / HLTB / Wikidata are best-effort — when their creds are missing
//! (IGDB) or a request fails, we still write whatever Steam Store +
//! SteamSpy returned.

use futures_util::stream::{self, StreamExt};
use serde::Serialize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tauri::State;

use crate::services::db::{Database, GameMetadataUpdate};
use crate::services::igdb_api::IgdbToken;
use crate::services::{gog_api, hltb_api, igdb_api, rawg_api, steam_store_api, steamspy_api, wikidata_api};

/// Refresh window. 14 days is a reasonable balance — enough that tag votes
/// shift visibly between syncs, short enough that a rebought game's metadata
/// doesn't go stale forever.
const STALE_AFTER_SECONDS: i64 = 14 * 86_400;

#[derive(Debug, Clone, Serialize)]
pub struct MetadataSyncReport {
    pub total: usize,
    pub synced: usize,
    pub skipped_fresh: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

/// Maximum number of error strings retained in the final report — a
/// network outage shouldn't balloon the payload.
const MAX_ERRORS: usize = 20;

/// Inter-game concurrency. IGDB rate-limits at 4 req/sec per token and we
/// make ~5 IGDB calls per game (external_games → games → franchise +
/// themes + similar_games), so 2 games in flight = ~10 req/sec peaks
/// briefly, which IGDB tolerates. SteamSpy's 1 req/sec gate is enforced
/// inside the service itself via a global Mutex, so multiple in-flight
/// games naturally queue up there without us coordinating here.
const INTER_GAME_CONCURRENCY: usize = 2;

#[tauri::command]
pub async fn sync_metadata_now(
    force: Option<bool>,
    db: State<'_, Database>,
) -> Result<MetadataSyncReport, String> {
    let force = force.unwrap_or(false);
    let all_rows = db
        .list_steam_games_for_metadata()
        .map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().timestamp();
    let total = all_rows.len();

    // Partition: rows that need a touch this run vs. rows already fresh
    // enough to skip.
    let (to_process, skipped): (Vec<_>, Vec<_>) = all_rows.into_iter().partition(
        |(_, _, _, synced_at)| {
            force || *synced_at == 0 || (now - synced_at) > STALE_AFTER_SECONDS
        },
    );
    let skipped_count = skipped.len();

    // One-shot IGDB token acquisition for the whole pass. Missing creds
    // → None → IGDB is silently skipped per game (other sources still run).
    let igdb_token = match igdb_api::acquire_token(&db).await {
        Ok(t) => t,
        Err(e) => {
            log::warn!("[metadata] IGDB auth failed (skipping IGDB this run): {}", e);
            None
        }
    };

    // Shared accumulators — Arc<Atomic*> for counters, Arc<Mutex<Vec>> for
    // the (capped) error list. The closure passed to `for_each_concurrent`
    // clones these per-spawn so each task can independently bump them
    // without serializing on a single lock.
    let synced = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let errors: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));

    // Database implements Clone via Arc<Mutex<Connection>>, so we can hand
    // each spawn its own handle without re-locking the Tauri State. The
    // SQLite mutex inside still serializes the actual writes.
    let db_handle: Database = (*db).clone();
    let igdb_token_arc = Arc::new(igdb_token);

    stream::iter(to_process)
        .for_each_concurrent(
            INTER_GAME_CONCURRENCY,
            |(game_id, appid, title, _synced_at)| {
                let synced = synced.clone();
                let failed = failed.clone();
                let errors = errors.clone();
                let db_handle = db_handle.clone();
                let igdb_token = igdb_token_arc.clone();
                async move {
                    match process_one_game(
                        &game_id,
                        &appid,
                        &title,
                        igdb_token.as_ref().as_ref(),
                        &db_handle,
                    )
                    .await
                    {
                        Ok(partial_errs) => {
                            // DB write succeeded — count as synced even if
                            // some sources errored. Partial errors are still
                            // pushed into the report so the user can debug
                            // them later, but they don't downgrade the row.
                            synced.fetch_add(1, Ordering::Relaxed);
                            if !partial_errs.is_empty() {
                                let mut bucket = errors.lock().unwrap();
                                for e in partial_errs {
                                    if bucket.len() < MAX_ERRORS {
                                        bucket.push(e);
                                    }
                                }
                            }
                        }
                        Err(errs) => {
                            // Hard failure — every source dry AND the
                            // timestamp-bump DB write itself errored, or the
                            // merged DB write errored. The row didn't move
                            // forward.
                            failed.fetch_add(1, Ordering::Relaxed);
                            let mut bucket = errors.lock().unwrap();
                            for e in errs {
                                if bucket.len() < MAX_ERRORS {
                                    bucket.push(e);
                                }
                            }
                        }
                    }
                }
            },
        )
        .await;

    let final_errors = std::mem::take(&mut *errors.lock().unwrap());
    let report = MetadataSyncReport {
        total,
        synced: synced.load(Ordering::Relaxed),
        skipped_fresh: skipped_count,
        failed: failed.load(Ordering::Relaxed),
        errors: final_errors,
    };

    log::info!(
        "[metadata] sync done: total={} synced={} skipped={} failed={}",
        report.total,
        report.synced,
        report.skipped_fresh,
        report.failed
    );
    // Surface a sample of the first few errors so we can diagnose mass
    // failures (rate-limit, API down, parse regression) without having
    // to round-trip to the frontend.
    for (i, err) in report.errors.iter().take(5).enumerate() {
        log::warn!("[metadata] sample error #{}: {}", i + 1, err);
    }
    Ok(report)
}

/// Re-sync metadata for one game on demand (more menu "Refresh
/// metadata" item). Bypasses the 14-day stale window — the user clicked
/// because they want it now. Steam-source rows only; non-Steam games
/// don't have an appid to query the external APIs against.
#[tauri::command]
pub async fn sync_metadata_one(
    id: String,
    db: State<'_, Database>,
) -> Result<(), String> {
    let game = db
        .get_game_by_id(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Game not found".to_string())?;

    // For GOG-source games, prefer the GOG API directly (more accurate
    // metadata for GOG-exclusive releases, native French / German / etc.
    // descriptions, GOG-managed screenshots). Falls back to the Steam
    // Store name-search path if the GOG fetch returns nothing.
    if game.source == "gog" {
        let pid = game.platform_id.clone().unwrap_or_default();
        if !pid.is_empty() {
            let locale_code = crate::commands::locale::read_app_locale(&db);
            let gog_locale = match locale_code.as_str() {
                "fr" => "fr-FR",
                "de" => "de-DE",
                "es" => "es-ES",
                "it" => "it-IT",
                "pt" => "pt-BR",
                "ru" => "ru-RU",
                "zh" => "zh-Hans",
                "ja" => "ja-JP",
                "ko" => "ko-KR",
                _ => "en-US",
            };
            match gog_api::fetch_game_metadata(&pid, gog_locale).await {
                Ok(Some(meta)) => {
                    let update = GameMetadataUpdate {
                        description: meta.description.as_deref(),
                        header_url: meta.header_url.as_deref(),
                        screenshots: meta.screenshots.as_deref(),
                        developer: meta.developers.as_deref(),
                        publisher: meta.publishers.as_deref(),
                        genres: meta.genres.as_deref(),
                        ..Default::default()
                    };
                    db.upsert_game_metadata(&id, update)
                        .map_err(|e| e.to_string())?;
                    log::info!(
                        "[metadata] sync_one GOG '{}' ({}) — wrote native GOG metadata",
                        game.title,
                        pid
                    );
                    return Ok(());
                }
                Ok(None) => {
                    log::warn!(
                        "[metadata] GOG product {} not found — falling back to Steam Store search",
                        pid
                    );
                }
                Err(e) => {
                    log::warn!(
                        "[metadata] GOG metadata fetch for {} failed: {} — falling back to Steam Store search",
                        pid,
                        e
                    );
                }
            }
        }
    }

    // Steam-source games use their own appid. For non-Steam libraries
    // without a direct API (Epic, Ubisoft, EA, custom) or as a fallback
    // when the source-native fetch above failed, try to resolve a matching
    // Steam appid by name search — most AAA titles exist on both stores
    // and the Steam Store metadata is canonical. The game's `source` and
    // `platform_id` stay untouched — we only use the resolved appid for
    // the metadata fetch.
    let appid = if game.source == "steam" {
        let pid = game.platform_id.clone().unwrap_or_default();
        if pid.is_empty() {
            return Err("This Steam row has no appid.".to_string());
        }
        Some(pid)
    } else {
        match steam_store_api::search_by_name(&game.title).await {
            Ok(Some(found)) => {
                log::info!(
                    "[metadata] sync_one non-Steam '{}' ({}) → resolved Steam appid {}",
                    game.title,
                    game.source,
                    found
                );
                Some(found)
            }
            Ok(None) => None,
            Err(e) => {
                log::warn!("[metadata] Steam Store search for '{}' failed: {}", game.title, e);
                None
            }
        }
    };

    if let Some(appid) = appid {
        let igdb_token = igdb_api::acquire_token(&db).await.ok().flatten();
        return process_one_game(&id, &appid, &game.title, igdb_token.as_ref(), &db)
            .await
            .map(|partial_errs| {
                if !partial_errs.is_empty() {
                    log::warn!(
                        "[metadata] sync_one {} completed with partial errors: {:?}",
                        appid,
                        partial_errs
                    );
                }
            })
            .map_err(|errs| errs.join("; "));
    }

    // No Steam appid available — try RAWG as a final fallback. RAWG covers
    // 500k+ games including obscure / never-Steam-released titles (Star
    // Citizen, retro, Itch-only). API key required (free 20k req/month);
    // skip silently when the user hasn't configured one.
    let rawg_key = db
        .get_sync_state("rawg_api_key")
        .ok()
        .flatten()
        .unwrap_or_default();
    if !rawg_key.is_empty() {
        match rawg_api::fetch_game_metadata(&game.title, &rawg_key).await {
            Ok(Some(meta)) => {
                let update = GameMetadataUpdate {
                    description: meta.description.as_deref(),
                    header_url: meta.header_url.as_deref(),
                    screenshots: meta.screenshots.as_deref(),
                    developer: meta.developers.as_deref(),
                    publisher: meta.publishers.as_deref(),
                    genres: meta.genres.as_deref(),
                    ..Default::default()
                };
                db.upsert_game_metadata(&id, update)
                    .map_err(|e| e.to_string())?;
                log::info!(
                    "[metadata] sync_one '{}' ({}) — wrote RAWG fallback metadata",
                    game.title,
                    game.source
                );
                return Ok(());
            }
            Ok(None) => {
                log::warn!("[metadata] RAWG returned no match for '{}'", game.title);
            }
            Err(e) => {
                log::warn!("[metadata] RAWG fetch for '{}' failed: {}", game.title, e);
            }
        }
    }

    Err(format!(
        "Aucune metadata trouvée pour '{}' (Steam Store + RAWG vides).",
        game.title
    ))
}

/// Process a single game: fan out to all five sources in parallel
/// (`tokio::join!`), merge with the same policy as the sequential
/// version, write to the DB.
///
/// Returns:
/// - `Ok(partial_errs)` — DB write succeeded. The row moved forward.
///   `partial_errs` carries per-source failures (e.g. SteamSpy returned
///   503 but Steam Store + IGDB succeeded) so the caller can surface
///   them in the final report without flagging the row as failed.
/// - `Err(errs)` — DB write itself failed. The row didn't progress.
async fn process_one_game(
    game_id: &str,
    appid: &str,
    title: &str,
    igdb_token: Option<&IgdbToken>,
    db: &Database,
) -> Result<Vec<String>, Vec<String>> {
    // Localise Steam Store responses based on the user-picked UI locale —
    // `description` / categories / genre tags come back in the right
    // language so the GameDetail panel doesn't mix EN metadata under a
    // French UI.
    let locale = crate::commands::locale::read_app_locale(db);
    let (cc, l) = crate::commands::locale::steam_store_params(&locale);
    let store_fut = steam_store_api::fetch_appdetails(appid, cc, l);
    let spy_fut = steamspy_api::fetch_appdetails(appid);
    let hltb_fut = hltb_api::fetch_playtimes(title);
    let igdb_fut = async {
        match igdb_token {
            Some(t) => igdb_api::fetch_by_steam_appid(t, appid, title).await,
            None => Ok(None),
        }
    };

    let (store_res, spy_res, igdb_res, hltb_res) =
        tokio::join!(store_fut, spy_fut, igdb_fut, hltb_fut);

    let mut errs: Vec<String> = Vec::new();
    let store = match store_res {
        Ok(v) => v,
        Err(e) => {
            errs.push(format!("{}: store: {}", appid, e));
            None
        }
    };
    let spy = match spy_res {
        Ok(v) => v,
        Err(e) => {
            errs.push(format!("{}: steamspy: {}", appid, e));
            None
        }
    };
    let igdb = match igdb_res {
        Ok(v) => v,
        Err(e) => {
            errs.push(format!("{}: igdb: {}", appid, e));
            None
        }
    };
    let hltb = match hltb_res {
        Ok(v) => v,
        Err(e) => {
            errs.push(format!("{}: hltb: {}", appid, e));
            None
        }
    };

    // Wikidata only when IGDB didn't return a franchise (sequential here
    // — kept off the hot path since it's rarely needed).
    let wikidata = if igdb.as_ref().and_then(|i| i.franchise.as_deref()).is_none() {
        match wikidata_api::fetch_franchise(title).await {
            Ok(v) => v,
            Err(e) => {
                errs.push(format!("{}: wikidata: {}", appid, e));
                None
            }
        }
    } else {
        None
    };

    if store.is_none()
        && spy.is_none()
        && igdb.is_none()
        && hltb.is_none()
        && wikidata.is_none()
    {
        // Bump the timestamp so we don't re-hit the dry sources until
        // the stale window elapses. Partial source errors still go in
        // the report as warnings but the row counts as synced.
        return match db.upsert_game_metadata(game_id, GameMetadataUpdate::default()) {
            Ok(()) => Ok(errs),
            Err(e) => {
                errs.push(format!("{}: db: {}", appid, e));
                Err(errs)
            }
        };
    }

    let franchise = igdb
        .as_ref()
        .and_then(|i| i.franchise.as_deref())
        .or_else(|| wikidata.as_ref().and_then(|w| w.franchise.as_deref()));

    let update = GameMetadataUpdate {
        description: store.as_ref().and_then(|s| s.short_description.as_deref()),
        header_url: store.as_ref().and_then(|s| s.header_image.as_deref()),
        screenshots: store.as_ref().and_then(|s| s.screenshots.as_deref()),
        developer: store
            .as_ref()
            .and_then(|s| s.developers.as_deref())
            .or_else(|| spy.as_ref().and_then(|s| s.developer.as_deref())),
        publisher: store
            .as_ref()
            .and_then(|s| s.publishers.as_deref())
            .or_else(|| spy.as_ref().and_then(|s| s.publisher.as_deref())),
        genres: store.as_ref().and_then(|s| s.genres.as_deref()),
        categories: store.as_ref().and_then(|s| s.categories.as_deref()),
        tags: spy.as_ref().and_then(|s| s.tags.as_deref()),
        franchise,
        igdb_id: igdb.as_ref().and_then(|i| i.igdb_id),
        themes: igdb.as_ref().and_then(|i| i.themes.as_deref()),
        similar_games: igdb.as_ref().and_then(|i| i.similar_games.as_deref()),
        hltb_main_hours: hltb.as_ref().and_then(|h| h.main_hours),
        dlcs: store.as_ref().and_then(|s| s.dlcs.as_deref()),
    };

    match db.upsert_game_metadata(game_id, update) {
        Ok(()) => Ok(errs),
        Err(e) => {
            errs.push(format!("{}: db: {}", appid, e));
            Err(errs)
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MetadataStatus {
    /// Steam-source games whose `metadata_synced_at` is NULL or older than
    /// the stale window. These are the rows the next sync would touch.
    pub pending_count: usize,
    /// Total Steam-source games — denominator for any "X / Y synced"
    /// progress indicator.
    pub total_steam_games: usize,
    /// `pending_count == 0` && `total_steam_games > 0`. The Franchise
    /// Collections mode uses this gate to know whether the algorithm's 5
    /// passes have the data they need. False when there's nothing to sync
    /// (empty Steam library) so the UI doesn't claim "fully synced" on a
    /// fresh install.
    pub fully_synced: bool,
}

/// Snapshot of metadata coverage. Lightweight (a single COUNT-style scan
/// of the games table). The Settings page reads this on mount + after each
/// sync_metadata_now to gate the "Rebuild" button when the user picked the
/// Franchise collections mode.
#[tauri::command]
pub async fn get_metadata_status(db: State<'_, Database>) -> Result<MetadataStatus, String> {
    let rows = db
        .list_steam_games_for_metadata()
        .map_err(|e| e.to_string())?;
    let total = rows.len();
    let now = chrono::Utc::now().timestamp();
    let pending = rows
        .iter()
        .filter(|(_, _, _, synced_at)| {
            *synced_at == 0 || (now - synced_at) > STALE_AFTER_SECONDS
        })
        .count();
    log::info!(
        "[metadata] status: total_steam_games={} pending_count={}",
        total,
        pending
    );
    Ok(MetadataStatus {
        pending_count: pending,
        total_steam_games: total,
        fully_synced: pending == 0 && total > 0,
    })
}

/// Rich metadata returned to GameDetail. The JSON-encoded columns in DB
/// (`screenshots`, `genres`, `categories`, `tags`, `themes`,
/// `similar_games`) are parsed here so the frontend gets typed arrays
/// instead of having to JSON.parse strings.
#[derive(Debug, Clone, Serialize)]
pub struct GameMetadataView {
    pub description: Option<String>,
    pub header_url: Option<String>,
    /// Screenshot thumbnail URLs (640x360).
    pub screenshots: Vec<String>,
    pub developer: Option<String>,
    pub publisher: Option<String>,
    pub genres: Vec<String>,
    pub categories: Vec<String>,
    /// Community tags as `(name, votes)` pairs, sorted by votes descending.
    pub tags: Vec<TagEntry>,
    pub franchise: Option<String>,
    pub igdb_id: Option<i64>,
    pub themes: Vec<String>,
    pub similar_games: Vec<String>,
    pub hltb_main_hours: Option<f64>,
    /// DLC appids declared by the Steam Store for this game. Resolving
    /// them to names is a separate per-id Steam Store call; the
    /// frontend does that lazily when the DLC panel is opened.
    pub dlcs: Vec<i64>,
    pub metadata_synced_at: Option<i64>,
    /// True when this row was last synced BEFORE the user picked the
    /// current UI language. Drives GameDetail's lazy on-open refresh —
    /// when this flag is true, the frontend triggers `refresh_metadata`
    /// for this single game so its description / tags re-fetch in the
    /// new locale. Avoids batching 400+ Steam Store calls all at once.
    pub needs_locale_refresh: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TagEntry {
    pub name: String,
    pub votes: u64,
}

/// Fetch the enriched metadata for a single game. Returns `Ok(None)` when
/// the game row doesn't exist (deleted concurrently, bad id). Returns a
/// view with empty arrays + None fields when the game exists but
/// `sync_metadata_now` hasn't enriched it yet — the frontend uses this
/// to render an empty state instead of erroring out.
#[tauri::command]
pub async fn get_game_metadata(
    id: String,
    db: State<'_, Database>,
) -> Result<Option<GameMetadataView>, String> {
    let row = db
        .get_game_metadata(&id)
        .map_err(|e| e.to_string())?;
    let Some((
        description,
        header_url,
        screenshots,
        developer,
        publisher,
        genres,
        categories,
        tags,
        franchise,
        igdb_id,
        themes,
        similar_games,
        hltb_main_hours,
        dlcs,
        metadata_synced_at,
    )) = row
    else {
        return Ok(None);
    };

    // JSON columns may be null OR malformed (legacy rows). Tolerate by
    // parsing into empty vec on any error — never let a single bad row
    // crash the GameDetail page.
    let parse_str_vec = |s: Option<&str>| -> Vec<String> {
        s.and_then(|v| serde_json::from_str::<Vec<String>>(v).ok())
            .unwrap_or_default()
    };
    let tags_parsed: Vec<TagEntry> = tags
        .as_deref()
        .and_then(|v| serde_json::from_str::<Vec<(String, u64)>>(v).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|(name, votes)| TagEntry { name, votes })
        .collect();

    // Stale-vs-current-locale check: the row needs a refresh in the
    // current language when EITHER:
    //   * its `metadata_synced_at` predates `locale_changed_at` (row was
    //     synced under the old language), OR
    //   * `metadata_synced_at` is NULL while a locale_changed_at stamp
    //     exists (row was explicitly invalidated by a prior locale flip,
    //     or never synced at all — either way we want fresh strings in
    //     the current language).
    //   * the app_locale is non-EN and no metadata has ever been synced
    //     under that locale yet (boot-time migration for users who were
    //     already on FR/etc. before this lazy-refresh code landed — we
    //     can't tell what locale the row was synced under so we assume
    //     EN as a baseline).
    // The frontend reads `needs_locale_refresh` and calls
    // `refresh_metadata` for THIS game only, keeping the refresh load
    // proportional to what the user actually browses.
    let mut locale_changed_at = crate::commands::locale::read_locale_changed_at(&db);
    if locale_changed_at == 0 {
        let app_locale = crate::commands::locale::read_app_locale(&db);
        if app_locale != "en" {
            // Migration path: stamp now so subsequent reads + the
            // current one both treat existing rows as stale. Without
            // this, users who picked their language *before* this code
            // shipped would never see their metadata localise.
            let now = chrono::Utc::now().timestamp();
            if db
                .set_sync_state(
                    crate::commands::locale::KEY_LOCALE_CHANGED_AT,
                    &now.to_string(),
                )
                .is_ok()
            {
                locale_changed_at = now;
                log::info!(
                    "[locale] auto-stamped locale_changed_at={} for non-EN app_locale='{}' (no prior stamp)",
                    now,
                    app_locale
                );
            }
        }
    }
    let needs_locale_refresh = match metadata_synced_at {
        // Invalidated (set to NULL by a prior locale flip) OR never
        // synced — either way the row has no localised data we can
        // trust. Refresh on open.
        None => true,
        // Synced under an older locale stamp → outdated language.
        Some(synced) => locale_changed_at > 0 && synced < locale_changed_at,
    };

    Ok(Some(GameMetadataView {
        description,
        header_url,
        screenshots: parse_str_vec(screenshots.as_deref()),
        developer,
        publisher,
        genres: parse_str_vec(genres.as_deref()),
        categories: parse_str_vec(categories.as_deref()),
        tags: tags_parsed,
        franchise,
        igdb_id,
        themes: parse_str_vec(themes.as_deref()),
        similar_games: parse_str_vec(similar_games.as_deref()),
        hltb_main_hours,
        dlcs: dlcs
            .as_deref()
            .and_then(|v| serde_json::from_str::<Vec<i64>>(v).ok())
            .unwrap_or_default(),
        metadata_synced_at,
        needs_locale_refresh,
    }))
}

#[derive(Debug, Clone, Serialize)]
pub struct LibraryTagIndex {
    /// Top tags across the whole library, sorted by how many games carry
    /// them descending. Capped to keep the sidebar pill cluster legible.
    pub top_tags: Vec<TagCount>,
    /// Reverse index: tag name → list of game ids that have it in their
    /// top-3 tags. Used by the Library filter UI for an O(1) lookup
    /// when a sidebar pill is clicked — no need to re-scan the entire
    /// games table.
    pub games_by_tag: std::collections::HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TagCount {
    pub name: String,
    pub game_count: usize,
}

/// Build the library-wide tag index from every game's stored top tags.
/// Only the top **3** tags per game count toward the index — beyond that
/// the signal is too weak to be useful for grouping (a 20-vote tag on a
/// game that has 5000 votes on its top tag is essentially noise).
#[tauri::command]
pub async fn get_library_tag_index(
    db: State<'_, Database>,
) -> Result<LibraryTagIndex, String> {
    use std::collections::HashMap;

    let conn_rows = db
        .list_franchise_signals()
        .map_err(|e| e.to_string())?;

    let mut games_by_tag: HashMap<String, Vec<String>> = HashMap::new();
    let mut tag_game_counts: HashMap<String, usize> = HashMap::new();

    for (game_id, _source, _platform_id, _title, _franchise, _developer, tags_json) in conn_rows {
        let Some(raw) = tags_json else { continue };
        let parsed: Vec<(String, u64)> = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Top-3 tags only — the rest add noise to the filter.
        for (name, _votes) in parsed.into_iter().take(3) {
            games_by_tag
                .entry(name.clone())
                .or_default()
                .push(game_id.clone());
            *tag_game_counts.entry(name).or_insert(0) += 1;
        }
    }

    // Fold user-curated tags in — every one of them counts (the user
    // picked them deliberately, no top-N truncation). A game already
    // contributing to a tag via SteamSpy won't be added twice for the
    // same tag; we dedupe via the games_by_tag membership.
    let user_rows = db.list_all_user_tags().map_err(|e| e.to_string())?;
    for (game_id, raw) in user_rows {
        let parsed: Vec<String> = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for name in parsed {
            let entry = games_by_tag.entry(name.clone()).or_default();
            if !entry.contains(&game_id) {
                entry.push(game_id.clone());
                *tag_game_counts.entry(name).or_insert(0) += 1;
            }
        }
    }

    // Top tags sorted by game count descending, name asc for ties so the
    // order is deterministic across runs.
    let mut top_tags: Vec<TagCount> = tag_game_counts
        .into_iter()
        .map(|(name, count)| TagCount {
            name,
            game_count: count,
        })
        .collect();
    top_tags.sort_by(|a, b| {
        b.game_count
            .cmp(&a.game_count)
            .then_with(|| a.name.cmp(&b.name))
    });
    // Cap at 30 — Library sidebar can comfortably show that many pills
    // wrapped over a few rows; beyond that the long tail is noise.
    top_tags.truncate(30);

    Ok(LibraryTagIndex {
        top_tags,
        games_by_tag,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct IgdbCredentials {
    pub client_id: String,
    pub client_secret: String,
}

/// Returns the persisted IGDB credentials (Twitch OAuth client_id +
/// client_secret). Both empty strings when nothing is configured — the UI
/// can use that to render placeholders.
#[tauri::command]
pub async fn get_igdb_credentials(db: State<'_, Database>) -> Result<IgdbCredentials, String> {
    let client_id = db
        .get_sync_state("igdb_client_id")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let client_secret = db
        .get_sync_state("igdb_client_secret")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    Ok(IgdbCredentials {
        client_id,
        client_secret,
    })
}

/// Persist the IGDB credentials in sync_state. Wipes the cached OAuth token
/// so the next sync_metadata_now picks up the new creds.
#[tauri::command]
pub async fn set_igdb_credentials(
    client_id: String,
    client_secret: String,
    db: State<'_, Database>,
) -> Result<(), String> {
    db.set_sync_state("igdb_client_id", &client_id)
        .map_err(|e| e.to_string())?;
    db.set_sync_state("igdb_client_secret", &client_secret)
        .map_err(|e| e.to_string())?;
    igdb_api::clear_cached_token();
    Ok(())
}
