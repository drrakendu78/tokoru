//! SteamSpy API — `steamspy.com/api.php?request=appdetails&appid=N`.
//!
//! Public, no auth. Returns crowd-sourced data complementary to the Steam
//! Store API:
//! - `tags`: `{ "Tag Name": vote_count }` — Steam community tags by votes
//! - `developer` / `publisher`: cross-validation against Steam Store (often
//!   matches but sometimes differs on edge cases like delisted titles)
//! - `genre`: human comma-separated string (less authoritative than Steam
//!   Store's `genres[]` array)
//!
//! Rate limit: **1 request per second**, hard. Going faster gets a 403 +
//! temporary IP block. We enforce this with a single `Instant` guard.
//!
//! Reference: <https://steamspy.com/api.php>

use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::time::sleep;

const STEAMSPY_API_BASE: &str = "https://steamspy.com/api.php";
/// Minimum gap between two consecutive SteamSpy calls. SteamSpy's docs say
/// 1 req/sec — we add a small cushion to absorb clock jitter on Windows.
const RATE_LIMIT_MS: u64 = 1100;

/// Tracks the last SteamSpy call timestamp across all in-flight syncs so
/// concurrent callers serialize gracefully. `parking_lot` would be lighter
/// but pulls in a dep — std `Mutex` is fine for a once-per-second touch.
static LAST_CALL: Mutex<Option<Instant>> = Mutex::new(None);

#[derive(Debug, Clone)]
pub struct SteamSpyDetails {
    pub developer: Option<String>,
    pub publisher: Option<String>,
    /// JSON-encoded `Vec<(String, u64)>` sorted by vote count descending —
    /// matches the existing `games.tags` column convention.
    pub tags: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiData {
    #[serde(default)]
    developer: String,
    #[serde(default)]
    publisher: String,
    #[serde(default)]
    tags: serde_json::Value,
}

/// Fetch SteamSpy appdetails for a single Steam appid. Returns `Ok(None)`
/// when SteamSpy has no record (very new titles, hidden apps).
///
/// Respects the global 1-req/sec rate limit: if another call ran within the
/// last `RATE_LIMIT_MS`, we sleep the difference before issuing.
pub async fn fetch_appdetails(appid: &str) -> Result<Option<SteamSpyDetails>, String> {
    // Rate-limit gate: compute how long to wait, drop the lock, then sleep.
    // We must not hold the std Mutex across an .await.
    let wait = {
        let mut last = LAST_CALL.lock().unwrap();
        let now = Instant::now();
        let gap = Duration::from_millis(RATE_LIMIT_MS);
        let needed = match *last {
            Some(t) => {
                let elapsed = now.duration_since(t);
                if elapsed < gap {
                    gap - elapsed
                } else {
                    Duration::ZERO
                }
            }
            None => Duration::ZERO,
        };
        // Reserve our slot up front so other concurrent callers compute
        // their wait relative to ours, not to the previous slot.
        *last = Some(now + needed);
        needed
    };
    if !wait.is_zero() {
        sleep(wait).await;
    }

    let url = format!("{}?request=appdetails&appid={}", STEAMSPY_API_BASE, appid);
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client build: {}", e))?;
    let resp = client
        .get(&url)
        .header("User-Agent", "Tokoru/0.1")
        .send()
        .await
        .map_err(|e| format!("steamspy {}: {}", appid, e))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("steamspy {} status {}", appid, status));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| format!("steamspy {} read: {}", appid, e))?;

    // SteamSpy returns `[]` (empty array, not object) when a game has no
    // record. Catch that before serde fails.
    if body.trim() == "[]" {
        return Ok(None);
    }

    let d: ApiData = serde_json::from_str(&body)
        .map_err(|e| format!("steamspy {} parse: {}", appid, e))?;

    let developer = (!d.developer.is_empty()).then_some(d.developer);
    let publisher = (!d.publisher.is_empty()).then_some(d.publisher);

    // `tags` is either `{}` (no tags yet — common for niche titles) or
    // `{ "Tag": votes, ... }`. Sort descending by vote count.
    let tags_json = match &d.tags {
        serde_json::Value::Object(map) if !map.is_empty() => {
            let parsed: HashMap<String, u64> = serde_json::from_value(d.tags.clone())
                .map_err(|e| format!("steamspy {} tags parse: {}", appid, e))?;
            let mut entries: Vec<(String, u64)> = parsed.into_iter().collect();
            entries.sort_by(|a, b| b.1.cmp(&a.1));
            Some(serde_json::to_string(&entries).unwrap_or_else(|_| "[]".into()))
        }
        _ => None,
    };

    if developer.is_none() && publisher.is_none() && tags_json.is_none() {
        return Ok(None);
    }

    Ok(Some(SteamSpyDetails {
        developer,
        publisher,
        tags: tags_json,
    }))
}
