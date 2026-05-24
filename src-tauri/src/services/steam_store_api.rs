//! Steam Store API — `store.steampowered.com/api/appdetails`.
//!
//! Public, no auth, no key. Returns the canonical store metadata for a given
//! appid: name, short_description, header_image, screenshots, genres,
//! categories, developers, publishers, release_date, metacritic.
//!
//! Quirks:
//! - `cc=us&l=en` keeps the response language / currency deterministic so the
//!   description and category labels don't drift with the user's locale.
//! - The endpoint accepts multiple appids in one call (`appids=440,730`) but
//!   we still issue one request per game so we can short-circuit on
//!   already-synced rows and keep the rate gentle. ~5 req/sec is safe.
//! - Wrapped in `{ "<appid>": { "success": bool, "data": { ... } } }` — we
//!   collapse it on parse.
//! - Some titles (delisted, region-locked) return `success: false` with no
//!   `data`. Return `Ok(None)` so the caller can skip and continue.
//! - Steam returns BBCode in `about_the_game` / `detailed_description`. We
//!   prefer `short_description` (plain text, ~300 chars) for our UI.
//!
//! Reference: <https://wiki.teamfortress.com/wiki/User:RJackson/StorefrontAPI>
//! (unofficial — Valve never published a spec).

use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const STORE_API_BASE: &str = "https://store.steampowered.com/api/appdetails";

/// Subset of fields we extract from the Steam Store appdetails response.
/// Everything is optional because Valve's response shape is inconsistent
/// across game types (free-to-play, demos, soundtracks, hardware).
#[derive(Debug, Clone)]
pub struct SteamStoreDetails {
    pub short_description: Option<String>,
    pub header_image: Option<String>,
    /// JSON-encoded `Vec<String>` of screenshot URLs (640x360 thumbs).
    pub screenshots: Option<String>,
    pub developers: Option<String>,
    pub publishers: Option<String>,
    /// JSON-encoded `Vec<String>` of genre names (e.g. ["Action", "RPG"]).
    pub genres: Option<String>,
    /// JSON-encoded `Vec<String>` of category names (e.g. ["Single-player",
    /// "Steam Achievements"]).
    pub categories: Option<String>,
    /// JSON-encoded `Vec<i64>` of DLC appids that the Steam Store
    /// declares as add-ons of this base game. Empty / None when the
    /// game has no DLC (or the store didn't expose them on this
    /// locale).
    pub dlcs: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiEnvelope {
    success: bool,
    data: Option<ApiData>,
}

#[derive(Debug, Deserialize)]
struct ApiData {
    short_description: Option<String>,
    header_image: Option<String>,
    developers: Option<Vec<String>>,
    publishers: Option<Vec<String>>,
    genres: Option<Vec<ApiNamed>>,
    categories: Option<Vec<ApiNamed>>,
    screenshots: Option<Vec<ApiScreenshot>>,
    #[serde(default)]
    dlc: Vec<i64>,
}

#[derive(Debug, Deserialize)]
struct ApiNamed {
    description: String,
}

#[derive(Debug, Deserialize)]
struct ApiScreenshot {
    path_thumbnail: String,
}

/// Fetch store metadata for a single Steam appid. Returns `Ok(None)` when
/// Valve reports `success: false` (delisted / hidden / not a store entry).
///
/// `cc` / `l` are the Steam Store localisation params (see
/// `commands::locale::steam_store_params`). Pass `("us", "en")` for the
/// neutral English baseline, or the pair resolved from the user-picked
/// UI locale so the cached `description` / `tags` come back already
/// localised.
pub async fn fetch_appdetails(
    appid: &str,
    cc: &str,
    l: &str,
) -> Result<Option<SteamStoreDetails>, String> {
    let url = format!("{}?appids={}&cc={}&l={}", STORE_API_BASE, appid, cc, l);
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client build: {}", e))?;
    let resp = client
        .get(&url)
        .header("User-Agent", "Tokoru/0.1")
        .send()
        .await
        .map_err(|e| format!("store appdetails {}: {}", appid, e))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("store appdetails {} status {}", appid, status));
    }

    // The envelope is keyed by appid (as a string). Parse as a JSON Value
    // first so we can pull the single entry without knowing the appid type.
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("store appdetails {} parse: {}", appid, e))?;
    let entry = v.get(appid).cloned().unwrap_or(serde_json::Value::Null);
    if entry.is_null() {
        return Ok(None);
    }
    let env: ApiEnvelope = serde_json::from_value(entry)
        .map_err(|e| format!("store appdetails {} envelope: {}", appid, e))?;
    if !env.success {
        return Ok(None);
    }
    let Some(d) = env.data else {
        return Ok(None);
    };

    let screenshots_json = d.screenshots.as_ref().map(|list| {
        serde_json::to_string(
            &list.iter().map(|s| s.path_thumbnail.clone()).collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".into())
    });
    let genres_json = d.genres.as_ref().map(|list| {
        serde_json::to_string(
            &list.iter().map(|n| n.description.clone()).collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".into())
    });
    let categories_json = d.categories.as_ref().map(|list| {
        serde_json::to_string(
            &list.iter().map(|n| n.description.clone()).collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".into())
    });

    let dlcs_json = if d.dlc.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&d.dlc).unwrap_or_else(|_| "[]".into()))
    };

    Ok(Some(SteamStoreDetails {
        short_description: d.short_description,
        header_image: d.header_image,
        screenshots: screenshots_json,
        developers: d.developers.map(|v| v.join(", ")),
        publishers: d.publishers.map(|v| v.join(", ")),
        genres: genres_json,
        categories: categories_json,
        dlcs: dlcs_json,
    }))
}

const STORE_SEARCH_BASE: &str = "https://store.steampowered.com/api/storesearch/";

#[derive(Debug, Deserialize)]
struct SearchEnvelope {
    #[serde(default)]
    items: Vec<SearchItem>,
}

#[derive(Debug, Deserialize)]
struct SearchItem {
    id: i64,
    name: String,
}

/// Find a Steam appid for an arbitrary game title via the Steam Store
/// search endpoint. Used to enrich non-Steam library entries (Epic, GOG,
/// Ubisoft, …) with the Steam Store metadata when the same game also
/// exists on Steam — most AAA titles do, and the description /
/// screenshots / publisher / categories are then identical.
///
/// Returns `Ok(None)` when no result matches the title closely enough.
/// The name similarity is a simple normalised-prefix match — we don't
/// want to attach `Witcher 3` metadata to `Witcher 4` by accident.
pub async fn search_by_name(title: &str) -> Result<Option<String>, String> {
    let query = title.trim();
    if query.is_empty() {
        return Ok(None);
    }
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client build: {}", e))?;
    let resp = client
        .get(STORE_SEARCH_BASE)
        .query(&[("term", query), ("l", "english"), ("cc", "US")])
        .header("User-Agent", "Tokoru/0.1")
        .send()
        .await
        .map_err(|e| format!("store search '{}': {}", query, e))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("store search '{}' status {}", query, status));
    }
    let env: SearchEnvelope = resp
        .json()
        .await
        .map_err(|e| format!("store search '{}' parse: {}", query, e))?;
    if env.items.is_empty() {
        return Ok(None);
    }

    let needle = normalize_for_match(query);
    for item in &env.items {
        let candidate = normalize_for_match(&item.name);
        // Accept when either side is a prefix of the other — handles
        // edition suffixes ("The Witcher: Enhanced Edition" → "The
        // Witcher") and franchise-prefix matches in either direction
        // without dragging in unrelated games via a fuzzy substring.
        if needle.starts_with(&candidate) || candidate.starts_with(&needle) {
            log::info!(
                "[steam_store_api] resolved '{}' → '{}' (appid {})",
                query,
                item.name,
                item.id
            );
            return Ok(Some(item.id.to_string()));
        }
    }
    log::debug!(
        "[steam_store_api] search '{}' returned {} results but none matched (top: '{}')",
        query,
        env.items.len(),
        env.items[0].name
    );
    Ok(None)
}

/// Lowercase + strip non-alphanumeric for prefix-match comparisons.
fn normalize_for_match(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}
