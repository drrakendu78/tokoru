//! IGDB — Twitch OAuth-authenticated game database.
//!
//! IGDB is now owned by Twitch and authenticates via Twitch's OAuth
//! `client_credentials` flow. Credentials (client_id + client_secret) come
//! from a free Twitch developer app (<https://dev.twitch.tv/console/apps>)
//! and are persisted in `sync_state` so each user provides their own.
//!
//! Why IGDB matters for Tokoru:
//! - **franchise** — IGDB has a canonical `franchises` / `collections`
//!   relation. Resident Evil 4 → franchise "Resident Evil". Yakuza 0 →
//!   franchise "Yakuza" + collection "Like a Dragon". This beats every
//!   title-prefix heuristic.
//! - **themes** — high-level mood tags (Horror, Comedy, Stealth) that
//!   complement the gameplay-oriented Steam tags.
//! - **similar_games** — IGDB's "people who played this also enjoyed"
//!   relation. Useful for "Diablo-like" / "Souls-like" suggestion clusters
//!   without us having to maintain a mapping.
//!
//! Lookup strategy:
//! 1. `/external_games` with `category = 1` (Steam) + the appid → IGDB
//!    `game.id`. This is the authoritative path; <0.1% miss rate.
//! 2. Fallback: name search via `/games` `search` (Steam Store-localized
//!    title). Less precise — first hit is usually right but watch out for
//!    re-releases sharing names (e.g. "Tomb Raider" 1996 vs 2013).
//! 3. Once we have the IGDB `game.id`, fetch the full row with `fields`
//!    we care about (franchises, themes, similar_games, summary).
//!
//! Rate limit: 4 req/sec per token. We don't enforce it strictly here —
//! sync_metadata_now is sequential, and the slowest step is SteamSpy
//! (1 req/sec) so we never bunch up.
//!
//! Reference: <https://api-docs.igdb.com>

use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::services::db::Database;

/// Filename of the optional local credentials file. When present in the
/// Tokoru `app_data_dir` (alongside `steamshelf.db`), it's used as a
/// fallback when `sync_state` doesn't have the IGDB creds. Kept under
/// `app_data_dir` — outside the project tree — so it's never committed
/// and never distributed with builds.
///
/// File shape: `{ "client_id": "...", "client_secret": "..." }`.
const LOCAL_CREDS_FILENAME: &str = "igdb-creds.local.json";

const TWITCH_OAUTH_URL: &str = "https://id.twitch.tv/oauth2/token";
const IGDB_API_BASE: &str = "https://api.igdb.com/v4";

#[derive(Debug, Clone)]
pub struct IgdbToken {
    access_token: String,
    client_id: String,
    expires_at: Instant,
}

/// Cached OAuth token, shared across calls within the same Tokoru
/// session. Twitch tokens last ~60 days but we keep a 5-min safety margin
/// before refresh. Cleared whenever the user updates their creds.
static TOKEN_CACHE: OnceLock<Mutex<Option<IgdbToken>>> = OnceLock::new();

fn token_cache() -> &'static Mutex<Option<IgdbToken>> {
    TOKEN_CACHE.get_or_init(|| Mutex::new(None))
}

/// Drop any cached token. Call this after `set_igdb_credentials` so the
/// next acquire pulls a fresh one with the new client_id / secret.
pub fn clear_cached_token() {
    if let Ok(mut guard) = token_cache().lock() {
        *guard = None;
    }
}

/// Acquire (or reuse) an IGDB OAuth token. Returns `Ok(None)` when the user
/// hasn't entered credentials yet — sync_metadata_now treats that as "skip
/// IGDB this run", not an error.
pub async fn acquire_token(db: &Database) -> Result<Option<IgdbToken>, String> {
    let mut client_id = db
        .get_sync_state("igdb_client_id")
        .map_err(|e| format!("igdb creds read: {}", e))?
        .unwrap_or_default();
    let mut client_secret = db
        .get_sync_state("igdb_client_secret")
        .map_err(|e| format!("igdb creds read: {}", e))?
        .unwrap_or_default();
    if client_id.is_empty() || client_secret.is_empty() {
        if let Some((id, secret)) = read_local_config() {
            client_id = id;
            client_secret = secret;
        }
    }
    if client_id.is_empty() || client_secret.is_empty() {
        return Ok(None);
    }

    // Reuse the cached token if it's still valid for at least 5 minutes
    // AND was issued for the same client_id (creds may have changed).
    if let Some(t) = token_cache().lock().ok().and_then(|g| g.clone()) {
        if t.client_id == client_id
            && t.expires_at > Instant::now() + Duration::from_secs(300)
        {
            return Ok(Some(t));
        }
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client build: {}", e))?;
    let url = format!(
        "{}?client_id={}&client_secret={}&grant_type=client_credentials",
        TWITCH_OAUTH_URL, client_id, client_secret
    );
    let resp = client
        .post(&url)
        .send()
        .await
        .map_err(|e| format!("twitch oauth request: {}", e))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("twitch oauth status {}: {}", status, body));
    }

    #[derive(Deserialize)]
    struct OAuthResponse {
        access_token: String,
        expires_in: u64,
    }
    let r: OAuthResponse = resp.json().await.map_err(|e| format!("oauth parse: {}", e))?;
    let token = IgdbToken {
        access_token: r.access_token,
        client_id: client_id.clone(),
        expires_at: Instant::now() + Duration::from_secs(r.expires_in),
    };
    if let Ok(mut guard) = token_cache().lock() {
        *guard = Some(token.clone());
    }
    Ok(Some(token))
}

#[derive(Debug, Clone, Default)]
pub struct IgdbDetails {
    pub igdb_id: Option<i64>,
    pub franchise: Option<String>,
    /// JSON-encoded `Vec<String>` of theme names ("Horror", "Action", etc).
    pub themes: Option<String>,
    /// JSON-encoded `Vec<String>` of similar game titles, capped at 6
    /// entries to keep the column compact.
    pub similar_games: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExternalGameRow {
    game: i64,
}

#[derive(Debug, Deserialize)]
struct IgdbGameSearchRow {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct IgdbGameDetailRow {
    #[serde(default)]
    franchises: Vec<i64>,
    #[serde(default)]
    collections: Vec<i64>,
    #[serde(default)]
    themes: Vec<i64>,
    #[serde(default)]
    similar_games: Vec<i64>,
}

#[derive(Debug, Deserialize)]
struct NamedRow {
    id: i64,
    name: String,
}

/// Resolve IGDB metadata for a Steam game. Tries the external_games appid
/// lookup first, falls back to name search.
pub async fn fetch_by_steam_appid(
    token: &IgdbToken,
    appid: &str,
    title: &str,
) -> Result<Option<IgdbDetails>, String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client build: {}", e))?;

    // Step 1: external_games (category 1 = Steam)
    let body = format!("fields game; where category = 1 & uid = \"{}\";", appid);
    let rows: Vec<ExternalGameRow> = post_igdb(&client, token, "external_games", &body).await?;
    let igdb_game_id = if let Some(row) = rows.first() {
        Some(row.game)
    } else {
        // Step 2: fallback to name search. Strip trademark / edition noise
        // because IGDB's search is fuzzy but rewards close matches.
        let search_title = title
            .replace(['™', '®', '©'], "")
            .trim()
            .to_string();
        let safe = search_title.replace('"', "\\\"");
        let body = format!("fields id; search \"{}\"; limit 1;", safe);
        let rows: Vec<IgdbGameSearchRow> = post_igdb(&client, token, "games", &body).await?;
        rows.first().map(|r| r.id)
    };

    let Some(game_id) = igdb_game_id else {
        return Ok(None);
    };

    // Step 3: fetch the rich detail row
    let body = format!(
        "fields franchises, collections, themes, similar_games; where id = {};",
        game_id
    );
    let detail_rows: Vec<IgdbGameDetailRow> = post_igdb(&client, token, "games", &body).await?;
    let Some(detail) = detail_rows.into_iter().next() else {
        return Ok(None);
    };

    // Resolve franchise: prefer the first entry in `franchises`, then
    // `collections` (IGDB sometimes uses the latter for series like
    // "Like a Dragon" / "Dark Souls").
    let franchise = if let Some(&fid) = detail.franchises.first() {
        resolve_named(&client, token, "franchises", fid).await?
    } else if let Some(&cid) = detail.collections.first() {
        resolve_named(&client, token, "collections", cid).await?
    } else {
        None
    };

    let themes = if !detail.themes.is_empty() {
        Some(resolve_named_batch(&client, token, "themes", &detail.themes).await?)
    } else {
        None
    };
    let themes_json = themes
        .map(|v| serde_json::to_string(&v).unwrap_or_else(|_| "[]".into()));

    let similar = if !detail.similar_games.is_empty() {
        // Cap at 6 entries — IGDB returns ~10 and we don't need more.
        let ids: Vec<i64> = detail.similar_games.iter().take(6).copied().collect();
        Some(resolve_named_batch(&client, token, "games", &ids).await?)
    } else {
        None
    };
    let similar_json = similar
        .map(|v| serde_json::to_string(&v).unwrap_or_else(|_| "[]".into()));

    Ok(Some(IgdbDetails {
        igdb_id: Some(game_id),
        franchise,
        themes: themes_json,
        similar_games: similar_json,
    }))
}

/// Single-id lookup against any IGDB `name`-bearing endpoint.
async fn resolve_named(
    client: &Client,
    token: &IgdbToken,
    endpoint: &str,
    id: i64,
) -> Result<Option<String>, String> {
    let body = format!("fields name; where id = {};", id);
    let rows: Vec<NamedRow> = post_igdb(client, token, endpoint, &body).await?;
    Ok(rows.into_iter().next().map(|r| r.name))
}

/// Batch-resolve a list of ids → names. Preserves the order of `ids`,
/// dropping any id IGDB doesn't return (rare — usually obsolete relations).
async fn resolve_named_batch(
    client: &Client,
    token: &IgdbToken,
    endpoint: &str,
    ids: &[i64],
) -> Result<Vec<String>, String> {
    let id_list = ids
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let body = format!(
        "fields id, name; where id = ({}); limit {};",
        id_list,
        ids.len()
    );
    let rows: Vec<NamedRow> = post_igdb(client, token, endpoint, &body).await?;
    let map: HashMap<i64, String> = rows.into_iter().map(|r| (r.id, r.name)).collect();
    Ok(ids.iter().filter_map(|i| map.get(i).cloned()).collect())
}

/// POST a raw IGDB query body. IGDB uses Apicalypse (its own DSL) in the
/// request body rather than JSON, so we send `text/plain`.
async fn post_igdb<T: for<'de> Deserialize<'de>>(
    client: &Client,
    token: &IgdbToken,
    endpoint: &str,
    body: &str,
) -> Result<Vec<T>, String> {
    let url = format!("{}/{}", IGDB_API_BASE, endpoint);
    let resp = client
        .post(&url)
        .header("Client-ID", &token.client_id)
        .header("Authorization", format!("Bearer {}", token.access_token))
        .header("Content-Type", "text/plain")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| format!("igdb {} request: {}", endpoint, e))?;
    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!("igdb {} status {}: {}", endpoint, status, detail));
    }
    resp.json::<Vec<T>>()
        .await
        .map_err(|e| format!("igdb {} parse: {}", endpoint, e))
}

/// Resolve the Tokoru `app_data_dir` from the same env vars Tauri
/// uses. We avoid pulling the `tauri::AppHandle` into this module so the
/// service stays standalone-testable (and to keep the dependency surface
/// small). On Windows that resolves to `%APPDATA%\com.startrad.tokoru`,
/// with a legacy fallback to `%APPDATA%\com.startrad.steamshelf` so users
/// who upgraded across the rename keep their IGDB creds without resyncing.
fn app_data_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let new = PathBuf::from(&appdata).join("com.startrad.tokoru");
    if new.exists() {
        return Some(new);
    }
    let legacy = PathBuf::from(&appdata).join("com.startrad.steamshelf");
    if legacy.exists() {
        return Some(legacy);
    }
    Some(new)
}

/// Look for the optional local config file next to the SQLite DB. When
/// present, returns the parsed `(client_id, client_secret)` pair. Absent
/// file or any I/O / parse error → returns None, and the caller falls back
/// to its "no credentials configured" branch.
fn read_local_config() -> Option<(String, String)> {
    let path = app_data_dir()?.join(LOCAL_CREDS_FILENAME);
    let body = std::fs::read_to_string(&path).ok()?;
    #[derive(Deserialize)]
    struct LocalConfig {
        client_id: String,
        client_secret: String,
    }
    let cfg: LocalConfig = serde_json::from_str(&body).ok()?;
    if cfg.client_id.is_empty() || cfg.client_secret.is_empty() {
        return None;
    }
    Some((cfg.client_id, cfg.client_secret))
}
