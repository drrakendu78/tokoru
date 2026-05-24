//! HowLongToBeat — playtime estimates by community submissions.
//!
//! HLTB has no public API. We hit the same internal endpoint their web app
//! uses (`/api/search` — JSON in, JSON out). The endpoint requires a token
//! that's baked into the bundled JS chunk on howlongtobeat.com; we extract
//! it on first call and cache it for the session. The token rotates ~every
//! few months — when we get a 401, we re-extract.
//!
//! This is fragile by nature (we're scraping their JS). Treat failures as
//! optional metadata, never as a hard error.
//!
//! Returned field of interest: `comp_main` — average playtime to "beat the
//! main story", in seconds.

use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const HLTB_ORIGIN: &str = "https://howlongtobeat.com";
const HLTB_SCRIPT_RE: &str = r#"/_next/static/chunks/pages/_app-[a-f0-9]+\.js"#;
const HLTB_TOKEN_RE: &str = r#""([a-f0-9]{32,})""#;

static SEARCH_TOKEN: OnceLock<Mutex<Option<String>>> = OnceLock::new();
/// HLTB switched search-endpoint paths several times in 2024-2025
/// (`/api/search`, `/api/find`, `/api/seek`). They all share the same
/// request schema. We cache whichever one their bundled JS currently
/// points at so we don't have to update on every rotation.
static SEARCH_ENDPOINT: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn token_cache() -> &'static Mutex<Option<String>> {
    SEARCH_TOKEN.get_or_init(|| Mutex::new(None))
}
fn endpoint_cache() -> &'static Mutex<Option<String>> {
    SEARCH_ENDPOINT.get_or_init(|| Mutex::new(None))
}

#[derive(Debug, Clone, Default)]
pub struct HltbDetails {
    /// "Main Story" average, in **hours** (HLTB reports seconds; we
    /// downconvert to hours with 1 decimal so the column doesn't store
    /// over-precise numbers).
    pub main_hours: Option<f64>,
}

#[derive(Debug, Serialize)]
struct SearchOptionsGames {
    #[serde(rename = "userId")]
    user_id: i32,
    platform: String,
    #[serde(rename = "sortCategory")]
    sort_category: String,
    #[serde(rename = "rangeCategory")]
    range_category: String,
    #[serde(rename = "rangeTime")]
    range_time: RangeTime,
    gameplay: Gameplay,
    #[serde(rename = "rangeYear")]
    range_year: RangeYear,
    #[serde(rename = "modifier")]
    modifier: String,
}

#[derive(Debug, Serialize, Default)]
struct RangeTime {
    min: Option<i32>,
    max: Option<i32>,
}

#[derive(Debug, Serialize, Default)]
struct Gameplay {
    perspective: String,
    flow: String,
    genre: String,
}

#[derive(Debug, Serialize, Default)]
struct RangeYear {
    min: String,
    max: String,
}

#[derive(Debug, Serialize)]
struct SearchOptionsUsers {
    #[serde(rename = "sortCategory")]
    sort_category: String,
}

#[derive(Debug, Serialize)]
struct SearchOptionsLists {
    #[serde(rename = "currentUser")]
    current_user: i32,
    #[serde(rename = "lists")]
    lists: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SearchOptions {
    games: SearchOptionsGames,
    users: SearchOptionsUsers,
    lists: SearchOptionsLists,
    filter: String,
    sort: i32,
    randomizer: i32,
}

#[derive(Debug, Serialize)]
struct SearchRequest {
    #[serde(rename = "searchType")]
    search_type: String,
    #[serde(rename = "searchTerms")]
    search_terms: Vec<String>,
    #[serde(rename = "searchPage")]
    search_page: i32,
    size: i32,
    #[serde(rename = "searchOptions")]
    search_options: SearchOptions,
    #[serde(rename = "useCache")]
    use_cache: bool,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    data: Vec<SearchHit>,
}

#[derive(Debug, Deserialize)]
struct SearchHit {
    #[serde(default)]
    game_name: String,
    #[serde(default)]
    comp_main: i64,
}

/// Fetch HLTB playtime estimates for a game by name. Returns `Ok(None)`
/// when HLTB has no record (very obscure titles) or when our scraped token
/// failed to refresh.
pub async fn fetch_playtimes(title: &str) -> Result<Option<HltbDetails>, String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let (endpoint, token) = ensure_endpoint_and_token().await?;
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client build: {}", e))?;

    let body = SearchRequest {
        search_type: "games".into(),
        search_terms: trimmed.split_whitespace().map(String::from).collect(),
        search_page: 1,
        size: 20,
        search_options: SearchOptions {
            games: SearchOptionsGames {
                user_id: 0,
                platform: String::new(),
                sort_category: "popular".into(),
                range_category: "main".into(),
                range_time: RangeTime::default(),
                gameplay: Gameplay::default(),
                range_year: RangeYear::default(),
                modifier: String::new(),
            },
            users: SearchOptionsUsers {
                sort_category: "postcount".into(),
            },
            lists: SearchOptionsLists {
                current_user: 0,
                lists: vec![],
            },
            filter: String::new(),
            sort: 0,
            randomizer: 0,
        },
        use_cache: true,
    };

    let url = format!("{}/{}", HLTB_ORIGIN, endpoint.trim_start_matches('/'));
    let resp = client
        .post(&url)
        .header("Origin", HLTB_ORIGIN)
        .header("Referer", format!("{}/", HLTB_ORIGIN))
        .header("User-Agent", browser_user_agent())
        .header("Authorization", format!("Bearer {}", token))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("hltb search: {}", e))?;
    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        // Token rotated — drop the cache and let the next call re-scrape.
        if let Ok(mut t) = token_cache().lock() {
            *t = None;
        }
        return Err(format!("hltb auth rejected ({}), token cleared", status));
    }
    if !status.is_success() {
        return Err(format!("hltb status {}", status));
    }
    let body: SearchResponse = resp
        .json()
        .await
        .map_err(|e| format!("hltb parse: {}", e))?;

    // Pick the first hit whose name matches the query loosely. HLTB's
    // popular-first sort usually has the right entry at index 0, but we
    // also accept any hit whose normalized name shares the trimmed query
    // as a prefix.
    let needle = normalize(trimmed);
    let hit = body.data.iter().find(|h| {
        let n = normalize(&h.game_name);
        n == needle || n.starts_with(&needle) || needle.starts_with(&n)
    });
    let Some(hit) = hit else {
        return Ok(None);
    };
    if hit.comp_main <= 0 {
        return Ok(None);
    }

    Ok(Some(HltbDetails {
        main_hours: Some((hit.comp_main as f64 / 3600.0 * 10.0).round() / 10.0),
    }))
}

/// Scrape (or reuse) the JWT bearer token + search endpoint path from the
/// HLTB Next.js bundle. The token + endpoint live in the bundled
/// `_app-<hash>.js` chunk as plain strings.
async fn ensure_endpoint_and_token() -> Result<(String, String), String> {
    let cached = {
        let e = endpoint_cache().lock().ok().and_then(|g| g.clone());
        let t = token_cache().lock().ok().and_then(|g| g.clone());
        e.zip(t)
    };
    if let Some(pair) = cached {
        return Ok(pair);
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client build: {}", e))?;
    let home = client
        .get(HLTB_ORIGIN)
        .header("User-Agent", browser_user_agent())
        .send()
        .await
        .map_err(|e| format!("hltb home: {}", e))?
        .text()
        .await
        .map_err(|e| format!("hltb home read: {}", e))?;
    let script_re = Regex::new(HLTB_SCRIPT_RE).unwrap();
    let script_path = script_re
        .find(&home)
        .ok_or("hltb: chunk path not found in homepage")?
        .as_str();
    let script_url = format!("{}{}", HLTB_ORIGIN, script_path);
    let script = client
        .get(&script_url)
        .header("User-Agent", browser_user_agent())
        .send()
        .await
        .map_err(|e| format!("hltb chunk: {}", e))?
        .text()
        .await
        .map_err(|e| format!("hltb chunk read: {}", e))?;

    let token_re = Regex::new(HLTB_TOKEN_RE).unwrap();
    let token = token_re
        .captures_iter(&script)
        .map(|c| c[1].to_string())
        .max_by_key(|s| s.len())
        .ok_or("hltb: token not found in chunk")?;

    // The endpoint path appears in the same chunk as a string like
    // `"api/search"` / `"api/seek/<digits>"`. Match the first occurrence.
    let endpoint_re = Regex::new(r#""(api/(?:search|find|seek)[a-zA-Z0-9_/]*)""#).unwrap();
    let endpoint = endpoint_re
        .captures(&script)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| "api/search".to_string());

    if let Ok(mut g) = token_cache().lock() {
        *g = Some(token.clone());
    }
    if let Ok(mut g) = endpoint_cache().lock() {
        *g = Some(endpoint.clone());
    }
    Ok((endpoint, token))
}

fn browser_user_agent() -> &'static str {
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}
