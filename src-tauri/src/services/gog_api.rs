//! GOG integration. Auth uses the GOG Galaxy public OAuth flow (browser →
//! redirect URL with `?code=`), then we exchange the code via the GOG token
//! endpoint and persist tokens in `gogdl`'s auth.json format so the upstream
//! CLI sees a logged-in session (matches OrionCore's layout).
//!
//! For the library fetch we hit `embed.gog.com` and `api.gog.com` directly —
//! same endpoints `gogdl` uses, but inline. This avoids round-tripping every
//! sync through `gogdl import-info` / similar (which would shell out per
//! game).

use std::path::PathBuf;
use std::process::Command;

use reqwest::Client;
use serde::Deserialize;
use tauri::AppHandle;

use crate::models::Game;
use crate::services::cli_bins;

// ── GOG Galaxy OAuth credentials ──
// These are the **public** identifiers shipped by GOG in the official Galaxy
// client and reused by gogdl (https://github.com/Heroic-Games-Launcher/heroic-gogdl).
// Not secrets — required for Tokoru to be identified as a Galaxy-class
// OAuth client. GOG doesn't issue third-party client IDs with library access.
pub const GOG_CLIENT_ID: &str = "46899977096215655";
const GOG_CLIENT_SECRET: &str =
    "9d85c43b1482497dbbce61f6e4aa173a433796eeae2ca8c5f6129f2dc4de46d9";
const GOG_REDIRECT: &str = "https://embed.gog.com/on_login_success?origin=client";
const GOG_AUTH_URL: &str = "https://auth.gog.com";
const GOG_EMBED_URL: &str = "https://embed.gog.com";

#[derive(Debug, Clone)]
pub struct GogAccount {
    /// Kept for parity with OrionCore — needed when we surface per-account
    /// chips in the Library. Not consumed in the command layer yet.
    #[allow(dead_code)]
    pub user_id: String,
    pub username: String,
    pub access_token: String,
}

// ── Auth ──

pub async fn login_with_code(
    app_handle: &AppHandle,
    auth_code: &str,
) -> Result<GogAccount, String> {
    // 1. Make sure gogdl is on disk (we'll persist tokens to its auth.json).
    cli_bins::ensure_gogdl(app_handle).await?;

    // 2. Exchange the code for tokens.
    let token = exchange_code(auth_code).await?;

    // 3. Look up the display name.
    let (resolved_user_id, username) = get_user_info(&token.access_token)
        .await
        .unwrap_or_else(|e| {
            log::warn!("[gog_api] user info lookup failed: {}", e);
            (token.user_id.clone(), "GOG User".to_string())
        });

    let user_id = if resolved_user_id.is_empty() {
        token.user_id.clone()
    } else {
        resolved_user_id
    };

    // 4. Persist tokens in gogdl's auth.json so the CLI sees us as logged in.
    write_gogdl_auth(app_handle, &token.access_token, &token.refresh_token, &user_id)?;

    Ok(GogAccount {
        user_id,
        username,
        access_token: token.access_token,
    })
}

#[allow(dead_code)]
pub async fn is_logged_in(app_handle: &AppHandle) -> bool {
    let Ok(path) = gogdl_auth_path(app_handle) else {
        return false;
    };
    if !path.exists() {
        return false;
    }
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };
    content.contains("access_token") && content.contains("refresh_token")
}

pub async fn logout(app_handle: &AppHandle) -> Result<(), String> {
    let path = gogdl_auth_path(app_handle)?;
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("Failed to delete gogdl auth.json: {}", e))?;
    }
    Ok(())
}

/// Returns a token guaranteed-fresh (refreshed if expired). Reads the stored
/// refresh token from gogdl's auth.json. We also rewrite auth.json with the
/// refreshed pair so subsequent calls don't refresh again unnecessarily.
pub async fn refreshed_access_token(app_handle: &AppHandle) -> Result<String, String> {
    let path = gogdl_auth_path(app_handle)?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("Could not read gogdl auth.json (not logged in?): {}", e))?;

    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("gogdl auth.json malformed: {}", e))?;

    let entry = value
        .get(GOG_CLIENT_ID)
        .ok_or_else(|| "gogdl auth.json missing client_id entry".to_string())?;

    let refresh_token = entry
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "gogdl auth.json missing refresh_token".to_string())?
        .to_string();
    let user_id = entry
        .get("user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let token = refresh_token_call(&refresh_token).await?;
    write_gogdl_auth(app_handle, &token.access_token, &token.refresh_token, &user_id)?;
    Ok(token.access_token)
}

// ── Library fetch ──

/// GOG Galaxy 2.0 stores per-game playtime in a local SQLite database at
/// `%PROGRAMDATA%\GOG.com\Galaxy\storage\galaxy-2.0.db`. The `GameTimes`
/// table records `(gameReleaseKey, minutesInGame, lastPlayedDate)` for
/// every game the user has launched through Galaxy.
///
/// We snapshot the file before opening (Galaxy holds an exclusive lock
/// while running, plus we don't want to fight its writer) and parse the
/// release keys like `gog_1971477531` → `("gog", 1971477531)`. Keys for
/// non-GOG sources Galaxy aggregates (Steam, Epic via its addons, …) are
/// kept so the same data feeds Steam playtime when GOG Galaxy is the
/// user's primary launcher.
///
/// Returns `(source, platform_id, seconds)` triples. Empty when Galaxy
/// isn't installed.
pub fn read_galaxy_playtimes() -> Result<Vec<(String, String, i64)>, String> {
    let program_data = std::env::var("PROGRAMDATA")
        .unwrap_or_else(|_| "C:\\ProgramData".to_string());
    let src = std::path::Path::new(&program_data)
        .join("GOG.com")
        .join("Galaxy")
        .join("storage")
        .join("galaxy-2.0.db");
    if !src.exists() {
        return Ok(Vec::new());
    }

    // Snapshot to a temp file because Galaxy locks the live DB. A vanilla
    // copy works — SQLite WAL files would be needed for a perfectly
    // consistent read, but `GameTimes` is overwritten in chunks and our
    // worst-case is a few seconds of staleness.
    let tmp = std::env::temp_dir().join(format!("steamshelf-galaxy-{}.db", std::process::id()));
    if let Err(e) = std::fs::copy(&src, &tmp) {
        log::warn!("[gog_api] failed to snapshot galaxy DB: {} — skipping playtime", e);
        return Ok(Vec::new());
    }

    let conn = match rusqlite::Connection::open_with_flags(
        &tmp,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[gog_api] failed to open snapshot {}: {}", tmp.display(), e);
            let _ = std::fs::remove_file(&tmp);
            return Ok(Vec::new());
        }
    };

    let mut out = Vec::new();
    let stmt = conn.prepare(
        "SELECT gameReleaseKey, minutesInGame FROM GameTimes WHERE minutesInGame > 0",
    );
    if let Ok(mut stmt) = stmt {
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        });
        if let Ok(rows) = rows {
            for row in rows.flatten() {
                let (key, minutes) = row;
                if let Some((source, platform_id)) = parse_release_key(&key) {
                    out.push((source, platform_id, minutes.saturating_mul(60)));
                }
            }
        }
    } else {
        log::info!("[gog_api] GameTimes table not found in galaxy DB — schema changed?");
    }

    let _ = std::fs::remove_file(&tmp);
    Ok(out)
}

/// Galaxy `gameReleaseKey` looks like `<source>_<platform_id>`:
///   * `gog_1971477531`
///   * `steam_2322010`
///   * `epic_a7b3c...` (uuid)
/// Returns `(source, platform_id)` when the prefix matches one we sync.
fn parse_release_key(key: &str) -> Option<(String, String)> {
    let (source, rest) = key.split_once('_')?;
    let source = source.to_ascii_lowercase();
    if !matches!(source.as_str(), "gog" | "steam" | "epic" | "origin" | "uplay") {
        return None;
    }
    Some((source, rest.to_string()))
}

pub async fn fetch_owned_games(access_token: &str) -> Result<Vec<Game>, String> {
    let client = Client::new();

    let resp = client
        .get(&format!("{}/user/data/games", GOG_EMBED_URL))
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("GOG owned-games request failed: {}", e))?;

    if !resp.status().is_success() {
        if resp.status().as_u16() == 401 {
            return Err("GOG token expired — please reconnect GOG in Settings.".to_string());
        }
        return Err(format!("GOG library error: HTTP {}", resp.status()));
    }

    #[derive(Deserialize)]
    struct Owned {
        owned: Vec<u64>,
    }
    let owned: Owned = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse GOG owned list: {}", e))?;

    log::info!("[gog_api] {} owned game ids returned", owned.owned.len());

    // Parallelize per-product detail fetches — sequential + 80ms sleep was
    // ~1-2 min for a 200-game library. 8 concurrent requests matches what
    // `fetch_playtimes_api` does and brings it down to ~10s on the same
    // library. Failures are logged and skipped (DLC/pack filtered out below).
    use futures_util::stream::{self, StreamExt};
    let game_ids: Vec<u64> = owned.owned.clone();
    let games: Vec<Game> = stream::iter(game_ids)
        .map(|game_id| {
            let client = client.clone();
            let token = access_token.to_string();
            async move {
                let url = format!("https://api.gog.com/products/{}", game_id);
                match client.get(&url).bearer_auth(&token).send().await {
                    Ok(r) if r.status().is_success() => match r.json::<GogProduct>().await {
                        Ok(product) => {
                            let gtype = product.game_type.as_deref().unwrap_or("");
                            if matches!(gtype, "dlc" | "pack" | "extras") {
                                return None;
                            }
                            let launch_cmd = format!("goggalaxy://openGameView/{}", product.id);
                            let mut game = Game::owned(
                                product.title,
                                "gog".to_string(),
                                product.id.to_string(),
                                launch_cmd,
                            );
                            game.artwork_url = product
                                .images
                                .and_then(|img| img.logo2x)
                                .map(|url| normalize_protocol_relative(&url));
                            Some(game)
                        }
                        Err(e) => {
                            log::warn!("[gog_api] failed to parse product {}: {}", game_id, e);
                            None
                        }
                    },
                    Ok(r) => {
                        log::warn!("[gog_api] product {} HTTP {}", game_id, r.status());
                        None
                    }
                    Err(e) => {
                        log::warn!("[gog_api] product {} request failed: {}", game_id, e);
                        None
                    }
                }
            }
        })
        .buffer_unordered(8)
        .filter_map(|opt| async move { opt })
        .collect()
        .await;

    log::info!("[gog_api] resolved {} owned games", games.len());
    Ok(games)
}

// ── Browser URL ──

pub fn get_login_url() -> String {
    format!(
        "{}/auth?client_id={}&redirect_uri={}&response_type=code&layout=galaxy",
        GOG_AUTH_URL, GOG_CLIENT_ID, GOG_REDIRECT
    )
}

// ── Internal ──

#[derive(Debug)]
struct GogTokens {
    access_token: String,
    refresh_token: String,
    user_id: String,
}

async fn exchange_code(auth_code: &str) -> Result<GogTokens, String> {
    #[derive(Deserialize)]
    struct Resp {
        access_token: String,
        refresh_token: Option<String>,
        user_id: Option<String>,
    }

    let client = Client::new();
    let url = format!(
        "{}/token?client_id={}&client_secret={}&grant_type=authorization_code&code={}&redirect_uri={}",
        GOG_AUTH_URL, GOG_CLIENT_ID, GOG_CLIENT_SECRET, auth_code, GOG_REDIRECT
    );

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GOG token exchange failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("GOG token error {}: {}", status, body));
    }

    let token: Resp = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse GOG token: {}", e))?;

    Ok(GogTokens {
        access_token: token.access_token,
        refresh_token: token.refresh_token.unwrap_or_default(),
        user_id: token.user_id.unwrap_or_default(),
    })
}

async fn refresh_token_call(refresh: &str) -> Result<GogTokens, String> {
    #[derive(Deserialize)]
    struct Resp {
        access_token: String,
        refresh_token: Option<String>,
        user_id: Option<String>,
    }

    let client = Client::new();
    let url = format!(
        "{}/token?client_id={}&client_secret={}&grant_type=refresh_token&refresh_token={}",
        GOG_AUTH_URL, GOG_CLIENT_ID, GOG_CLIENT_SECRET, refresh
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GOG token refresh failed: {}", e))?;

    if !resp.status().is_success() {
        return Err("GOG refresh token expired — please reconnect GOG.".to_string());
    }

    let token: Resp = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse GOG token refresh: {}", e))?;

    Ok(GogTokens {
        access_token: token.access_token,
        refresh_token: token.refresh_token.unwrap_or_default(),
        user_id: token.user_id.unwrap_or_default(),
    })
}

/// Resolve the Galaxy user id required by `gameplay.gog.com` endpoints.
///
/// `userData.json` exposes TWO ids:
///   * `userId` — the GOG website account id (string of digits)
///   * `galaxyUserId` — the Galaxy-internal id, audience-bound to the
///      gameplay endpoints
/// They differ for some accounts (long-lived accounts that pre-date Galaxy
/// in particular). `gameplay.gog.com` returns 403 "Wrong user" when we
/// hand it `userId` instead of `galaxyUserId`. Confirmed against Heroic
/// Games Launcher's own implementation.
pub async fn fetch_user_id(access_token: &str) -> Result<String, String> {
    let resp: serde_json::Value = Client::new()
        .get(&format!("{}/userData.json", GOG_EMBED_URL))
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("GOG userData request failed: {}", e))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse GOG userData: {}", e))?;
    let galaxy_id = resp.get("galaxyUserId").and_then(|v| v.as_str()).unwrap_or("");
    if !galaxy_id.is_empty() {
        return Ok(galaxy_id.to_string());
    }
    // Fall back to `userId` for accounts where Galaxy hasn't minted the
    // newer field yet — better than an empty id.
    Ok(resp
        .get("userId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string())
}

/// Pull per-game playtime from GOG's `gameplay.gog.com` API for users who
/// don't have GOG Galaxy installed (so the local `galaxy-2.0.db` route
/// returns nothing). Hits `/games/{game_id}/users/{user_id}/sessions` for
/// each owned game in parallel batches and sums the per-session
/// `time_in_seconds`.
///
/// Returns `(platform_id, seconds)` pairs for games with > 0 playtime so
/// the caller can drop them straight into `imported_playtime_seconds`.
pub async fn fetch_playtimes_api(
    access_token: &str,
    user_id: &str,
    game_ids: &[u64],
) -> Result<Vec<(String, i64)>, String> {
    use futures_util::stream::{self, StreamExt};

    if access_token.is_empty() || user_id.is_empty() || game_ids.is_empty() {
        return Ok(Vec::new());
    }

    // GOG's gameplay endpoint returns a single rolled-up envelope:
    //   GET /games/<appid>/users/<galaxyUserId>/sessions
    //   → { "time_sum": <minutes>, ... }
    // Confirmed by reading Heroic Games Launcher's implementation.
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct SessionsEnvelope {
        time_sum: Option<i64>,
    }

    let client = Client::new();
    let token = access_token.to_string();
    let user_id_owned = user_id.to_string();

    log::info!(
        "[gog_api] playtime fetch start: user_id={} games={}",
        user_id_owned,
        game_ids.len()
    );

    // 8 concurrent requests — keeps the API happy and finishes a 100-game
    // library in a couple of seconds.
    // One-shot diagnostic: log the first non-empty response so we can see
    // what `gameplay.gog.com` actually returns. Removed once the schema is
    // confirmed.
    use std::sync::atomic::{AtomicBool, Ordering};
    static SAMPLED: AtomicBool = AtomicBool::new(false);

    let results: Vec<(String, i64)> = stream::iter(game_ids.iter().copied())
        .map(|game_id| {
            let client = client.clone();
            let token = token.clone();
            let user_id = user_id_owned.clone();
            async move {
                let url = format!(
                    "https://gameplay.gog.com/games/{}/users/{}/sessions",
                    game_id, user_id
                );
                let resp = client
                    .get(&url)
                    .bearer_auth(&token)
                    .header("User-Agent", "Tokoru/0.1")
                    .send()
                    .await;
                let Ok(resp) = resp else {
                    log::warn!("[gog_api] playtime request transport failed for game {}", game_id);
                    return None;
                };
                let status = resp.status();
                if !status.is_success() {
                    if !SAMPLED.swap(true, Ordering::Relaxed) {
                        let body = resp.text().await.unwrap_or_default();
                        log::warn!(
                            "[gog_api] playtime sample HTTP {} for game {} body={}",
                            status,
                            game_id,
                            body.chars().take(300).collect::<String>()
                        );
                    }
                    return None;
                }
                let body = resp.text().await.ok()?;
                if !SAMPLED.load(Ordering::Relaxed) && !body.trim().is_empty() && body != "[]" {
                    SAMPLED.store(true, Ordering::Relaxed);
                    log::info!(
                        "[gog_api] playtime sample OK for game {}: {}",
                        game_id,
                        body.chars().take(500).collect::<String>()
                    );
                }
                let env: SessionsEnvelope = serde_json::from_str(&body).ok()?;
                let minutes = env.time_sum.unwrap_or(0);
                if minutes <= 0 {
                    return None;
                }
                // GOG reports time_sum in MINUTES; we store seconds.
                Some((game_id.to_string(), minutes.saturating_mul(60)))
            }
        })
        .buffer_unordered(8)
        .filter_map(|x| async move { x })
        .collect()
        .await;

    Ok(results)
}

async fn get_user_info(access_token: &str) -> Result<(String, String), String> {
    #[derive(Deserialize)]
    struct UserData {
        username: Option<String>,
        #[serde(rename = "userId")]
        user_id: Option<String>,
    }
    let client = Client::new();
    let resp: UserData = client
        .get(&format!("{}/userData.json", GOG_EMBED_URL))
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("GOG userData request failed: {}", e))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse GOG userData: {}", e))?;

    Ok((
        resp.user_id.unwrap_or_default(),
        resp.username.unwrap_or_else(|| "GOG User".to_string()),
    ))
}

fn gogdl_auth_path(app_handle: &AppHandle) -> Result<PathBuf, String> {
    let exe = cli_bins::gogdl_path(app_handle)?;
    let parent = exe
        .parent()
        .ok_or_else(|| "gogdl exe has no parent dir".to_string())?
        .to_path_buf();
    Ok(parent.join("gogdl-auth.json"))
}

fn write_gogdl_auth(
    app_handle: &AppHandle,
    access_token: &str,
    refresh_token: &str,
    user_id: &str,
) -> Result<(), String> {
    let path = gogdl_auth_path(app_handle)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create gogdl dir: {}", e))?;
    }

    let now = chrono::Utc::now().timestamp();
    let auth_data = serde_json::json!({
        GOG_CLIENT_ID: {
            "access_token": access_token,
            "refresh_token": refresh_token,
            "user_id": user_id,
            "loginTime": now,
            "expires_in": 3600,
            "token_type": "bearer",
        }
    });

    let content = serde_json::to_string_pretty(&auth_data)
        .map_err(|e| format!("Failed to serialise gogdl auth.json: {}", e))?;
    std::fs::write(&path, content)
        .map_err(|e| format!("Failed to write gogdl auth.json: {}", e))?;
    Ok(())
}

/// Quick sanity probe that gogdl is callable. Used to give a nicer error when
/// the binary download succeeded but the file is broken (anti-virus quarantine,
/// truncated download, etc.). Best-effort.
#[allow(dead_code)]
pub async fn gogdl_self_test(app_handle: &AppHandle) -> Result<(), String> {
    let exe = cli_bins::ensure_gogdl(app_handle).await?;
    tokio::task::spawn_blocking(move || {
        let out = Command::new(&exe)
            .arg("--help")
            .output()
            .map_err(|e| format!("Failed to spawn gogdl: {}", e))?;
        if !out.status.success() {
            return Err(format!("gogdl --help exited {}", out.status));
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("gogdl self-test task panicked: {}", e))?
}

#[derive(Deserialize, Debug)]
struct GogProduct {
    id: u64,
    title: String,
    game_type: Option<String>,
    images: Option<GogImages>,
}

#[derive(Deserialize, Debug)]
struct GogImages {
    logo2x: Option<String>,
}

fn normalize_protocol_relative(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("//") {
        format!("https://{}", rest)
    } else {
        url.to_string()
    }
}

// ── Game metadata fetch (description / screenshots / dev / publisher) ──

/// Subset of the GOG product detail response we surface as game metadata.
/// JSON-encoded `Vec<String>` for the array fields so the consumer (DB
/// schema) stays uniform with the Steam Store / SteamSpy path.
#[derive(Debug, Clone, Default)]
pub struct GogGameMetadata {
    pub description: Option<String>,
    pub header_url: Option<String>,
    pub screenshots: Option<String>,
    pub developers: Option<String>,
    pub publishers: Option<String>,
    pub genres: Option<String>,
}

// GOG product detail — flat `/products/{id}?expand=description,screenshots`
// shape (the v1/products endpoint, not the v2/games one which is HAL-nested
// under `_embedded.product` and harder to parse).
#[derive(Deserialize, Debug)]
struct GogProductDetail {
    #[serde(default)]
    description: Option<GogDescription>,
    #[serde(default)]
    screenshots: Option<Vec<GogScreenshot>>,
    #[serde(default)]
    images: Option<GogProductImages>,
}

#[derive(Deserialize, Debug)]
struct GogDescription {
    full: Option<String>,
    lead: Option<String>,
}

#[derive(Deserialize, Debug)]
struct GogScreenshot {
    #[serde(default, rename = "formatter_template_url")]
    formatter_template_url: Option<String>,
}

#[derive(Deserialize, Debug)]
struct GogProductImages {
    background: Option<String>,
    logo2x: Option<String>,
}

/// Fetch description + screenshots + cover for a single GOG game via the
/// public `/products/{id}` endpoint (no auth required). The locale param
/// is ignored — this endpoint always returns English. The v2/games HAL
/// endpoint would localise but its nested shape is more work to parse;
/// the description rarely matters in another language and IGDB usually
/// provides a localised one when the user really wants it.
///
/// Returns `Ok(None)` when the product doesn't exist on GOG anymore OR
/// when the response contained no usable metadata (description /
/// screenshots BOTH empty) — letting the caller fall back to a Steam
/// Store name search instead of pinning a "synced but blank" row.
pub async fn fetch_game_metadata(
    product_id: &str,
    _locale: &str,
) -> Result<Option<GogGameMetadata>, String> {
    let client = Client::new();
    let url = format!("https://api.gog.com/products/{}", product_id);
    let resp = client
        .get(&url)
        .query(&[("expand", "description,screenshots")])
        .header("User-Agent", "Tokoru/0.1")
        .send()
        .await
        .map_err(|e| format!("gog products {}: {}", product_id, e))?;
    let status = resp.status();
    if status.as_u16() == 404 {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(format!("gog products {} status {}", product_id, status));
    }
    let body = resp
        .text()
        .await
        .map_err(|e| format!("gog products {} body: {}", product_id, e))?;
    let detail: GogProductDetail = serde_json::from_str(&body)
        .map_err(|e| format!("gog products {} parse: {} — body sample: {}", product_id, e, &body.chars().take(200).collect::<String>()))?;

    let description = detail
        .description
        .as_ref()
        .and_then(|d| d.full.clone().or_else(|| d.lead.clone()))
        // Strip the BBCode-ish HTML wrapping that GOG ships. The library
        // renderer wants plain text or simple HTML — leave HTML tags
        // intact (GOG sends real <p>/<br> tags, not BBCode) and trim
        // outer whitespace.
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let screenshots_json = detail.screenshots.as_ref().and_then(|list| {
        let urls: Vec<String> = list
            .iter()
            .filter_map(|s| s.formatter_template_url.as_ref())
            // GOG screenshot URLs come as a template with `{formatter}`
            // placeholder. `ggvgt` is the 600x338 thumbnail size that
            // matches the Steam Store screenshot dimensions.
            .map(|t| t.replace("{formatter}", "ggvgt"))
            .collect();
        if urls.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&urls).unwrap_or_else(|_| "[]".into()))
        }
    });

    let header_url = detail.images.as_ref().and_then(|img| {
        img.background
            .as_ref()
            .or(img.logo2x.as_ref())
            .map(|u| normalize_protocol_relative(u))
    });

    // The `/products` endpoint doesn't expose dev / publisher / genres
    // directly — those come from the storefront page. We leave them None
    // and let the IGDB / Steam Store fallback fill them in.
    let meta = GogGameMetadata {
        description,
        header_url,
        screenshots: screenshots_json,
        developers: None,
        publishers: None,
        genres: None,
    };

    // Bail if the response had nothing usable so the caller can try a
    // Steam Store name search instead of writing an empty row.
    if meta.description.is_none() && meta.screenshots.is_none() {
        return Ok(None);
    }
    Ok(Some(meta))
}
