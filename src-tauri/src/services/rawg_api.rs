//! RAWG.io API — universal game metadata fallback.
//!
//! Why RAWG :
//! - 500k+ games indexed, way beyond Steam/GOG (covers Itch, retro, EGS-only,
//!   never-released-on-Steam titles like Star Citizen).
//! - Public API with a free tier (20k req/month, more than enough for the
//!   ~858-game libraries we see in the wild).
//! - Returns description, screenshots, developers, publishers, genres,
//!   release date — same fields the Steam Store path provides.
//! - Localised description / genres (we just pass `lang` and RAWG returns
//!   the matching version when available; falls back to EN otherwise).
//!
//! Endpoints we hit:
//! - `GET /games?search={title}&page_size=5` — search by title, top result
//!   is what RAWG considers the best match. We do a normalised-prefix
//!   double-check before accepting it so "Witcher 3" can't match
//!   "Witcher 4".
//! - `GET /games/{id}` — full details for the matched id (description,
//!   developers, publishers, genres, background image).
//! - `GET /games/{id}/screenshots` — separate paginated endpoint for the
//!   gallery.
//!
//! Auth: query param `?key={api_key}`. No OAuth, no header. The key is
//! per-user (free tier limits are per-key) so Tokoru reads it from
//! `sync_state["rawg_api_key"]` — never bundled.

use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const RAWG_BASE: &str = "https://api.rawg.io/api";

/// Subset of RAWG fields surfaced to the metadata writer. JSON-encoded
/// `Vec<String>` for the array fields to stay uniform with the Steam
/// Store / GOG paths.
#[derive(Debug, Clone, Default)]
pub struct RawgGameMetadata {
    pub description: Option<String>,
    pub header_url: Option<String>,
    /// JSON `Vec<String>` of screenshot thumbnail URLs.
    pub screenshots: Option<String>,
    pub developers: Option<String>,
    pub publishers: Option<String>,
    /// JSON `Vec<String>` of genre names.
    pub genres: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    #[serde(default)]
    results: Vec<SearchItem>,
}

#[derive(Debug, Deserialize)]
struct SearchItem {
    id: i64,
    name: String,
}

#[derive(Debug, Deserialize)]
struct GameDetail {
    #[serde(default)]
    description_raw: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    background_image: Option<String>,
    #[serde(default)]
    developers: Option<Vec<NamedEntity>>,
    #[serde(default)]
    publishers: Option<Vec<NamedEntity>>,
    #[serde(default)]
    genres: Option<Vec<NamedEntity>>,
}

#[derive(Debug, Deserialize)]
struct NamedEntity {
    name: String,
}

#[derive(Debug, Deserialize)]
struct ScreenshotsResult {
    #[serde(default)]
    results: Vec<Screenshot>,
}

#[derive(Debug, Deserialize)]
struct Screenshot {
    image: String,
}

/// Look up a game by name, then fetch its full details + screenshots.
/// Returns `Ok(None)` when no result matches (or the prefix-similarity
/// check rejects the candidate). `Err` only on hard HTTP / parse
/// failures — the caller should fall through silently.
///
/// `api_key` is required (RAWG returns 401 without it). If the caller
/// has no key configured, skip this fallback entirely instead of calling
/// in here.
pub async fn fetch_game_metadata(
    title: &str,
    api_key: &str,
) -> Result<Option<RawgGameMetadata>, String> {
    if api_key.is_empty() {
        return Ok(None);
    }
    let query = title.trim();
    if query.is_empty() {
        return Ok(None);
    }
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client build: {}", e))?;

    // Search phase
    let search_url = format!("{}/games", RAWG_BASE);
    let search_resp = client
        .get(&search_url)
        .query(&[
            ("search", query),
            ("page_size", "5"),
            ("key", api_key),
        ])
        .header("User-Agent", "Tokoru/0.1")
        .send()
        .await
        .map_err(|e| format!("rawg search '{}': {}", query, e))?;
    let status = search_resp.status();
    if status.as_u16() == 401 {
        return Err("RAWG: invalid or missing API key".to_string());
    }
    if !status.is_success() {
        return Err(format!("rawg search '{}' status {}", query, status));
    }
    let env: SearchResult = search_resp
        .json()
        .await
        .map_err(|e| format!("rawg search '{}' parse: {}", query, e))?;
    if env.results.is_empty() {
        return Ok(None);
    }
    let needle = normalize_for_match(query);
    let picked = env.results.into_iter().find(|item| {
        let candidate = normalize_for_match(&item.name);
        needle.starts_with(&candidate) || candidate.starts_with(&needle)
    });
    let Some(picked) = picked else {
        return Ok(None);
    };
    log::info!(
        "[rawg_api] resolved '{}' → '{}' (id {})",
        query,
        picked.name,
        picked.id
    );

    // Detail phase
    let detail_url = format!("{}/games/{}", RAWG_BASE, picked.id);
    let detail_resp = client
        .get(&detail_url)
        .query(&[("key", api_key)])
        .header("User-Agent", "Tokoru/0.1")
        .send()
        .await
        .map_err(|e| format!("rawg detail {}: {}", picked.id, e))?;
    if !detail_resp.status().is_success() {
        return Err(format!(
            "rawg detail {} status {}",
            picked.id,
            detail_resp.status()
        ));
    }
    let detail: GameDetail = detail_resp
        .json()
        .await
        .map_err(|e| format!("rawg detail {} parse: {}", picked.id, e))?;

    // Screenshots phase (best-effort, ignore failure)
    let screenshots_json = match client
        .get(format!("{}/games/{}/screenshots", RAWG_BASE, picked.id))
        .query(&[("key", api_key)])
        .header("User-Agent", "Tokoru/0.1")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => match r.json::<ScreenshotsResult>().await {
            Ok(env) => {
                let urls: Vec<String> = env.results.into_iter().map(|s| s.image).collect();
                if urls.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&urls).unwrap_or_else(|_| "[]".into()))
                }
            }
            Err(e) => {
                log::warn!("[rawg_api] screenshots {} parse: {}", picked.id, e);
                None
            }
        },
        Ok(r) => {
            log::warn!("[rawg_api] screenshots {} status {}", picked.id, r.status());
            None
        }
        Err(e) => {
            log::warn!("[rawg_api] screenshots {} request: {}", picked.id, e);
            None
        }
    };

    let description = detail
        .description_raw
        .or(detail.description)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let developers = detail.developers.map(|list| {
        list.into_iter()
            .map(|n| n.name)
            .collect::<Vec<_>>()
            .join(", ")
    });
    let publishers = detail.publishers.map(|list| {
        list.into_iter()
            .map(|n| n.name)
            .collect::<Vec<_>>()
            .join(", ")
    });
    let genres = detail.genres.map(|list| {
        let names: Vec<String> = list.into_iter().map(|n| n.name).collect();
        serde_json::to_string(&names).unwrap_or_else(|_| "[]".into())
    });

    let meta = RawgGameMetadata {
        description,
        header_url: detail.background_image,
        screenshots: screenshots_json,
        developers,
        publishers,
        genres,
    };

    // Bail when nothing usable came back — let the caller try another
    // fallback instead of pinning a blank row.
    if meta.description.is_none()
        && meta.screenshots.is_none()
        && meta.developers.is_none()
        && meta.publishers.is_none()
        && meta.genres.is_none()
    {
        return Ok(None);
    }
    Ok(Some(meta))
}

fn normalize_for_match(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}
