//! GOG achievements via `gameplay.gog.com`.
//!
//! Endpoint:
//!   `GET /clients/{productId}/users/{galaxyUserId}/achievements`
//!   Authorization: Bearer <access_token>
//!   X-Gog-Lc: <locale>     (e.g. "en-US", "fr-FR")
//!
//! Even though the URL says `clients`, you pass the GOG **product_id**
//! (a.k.a. `platform_id` on our side, what `gog_api::fetch_owned_games`
//! returns) directly — gameplay.gog.com resolves it to the right title
//! server-side. Confirmed against Heroic Games Launcher's GOG store
//! manager (`src/backend/storeManagers/gog/games.ts::getAchievements`)
//! and the Comet GOG SDK reimplementation. **No Galaxy DB lookup
//! required**, no client_id resolution dance — the OAuth Bearer token +
//! product_id is enough.
//!
//! Response shape (paginated, small games fit one page):
//! ```json
//! {
//!   "items": [
//!     {
//!       "achievement_id": "...",
//!       "achievement_key": "ACH_X",
//!       "visible": true,
//!       "name": "Mafia killer",
//!       "description": "Kill 25 enemies as a Maelstromer",
//!       "image_url_unlocked": "https://images.gog-statics.com/...png",
//!       "image_url_locked":   "https://images.gog-statics.com/...png",
//!       "date_unlocked": "2024-01-12T18:42:00.000+00:00" | null,
//!       "rarity": 12.3,
//!       "rarity_level_description": "uncommon"
//!     }
//!   ],
//!   "total_count": 42
//! }
//! ```

use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

use crate::services::steam_achievements::AchievementItem;

const GAMEPLAY_BASE: &str = "https://gameplay.gog.com";

#[derive(Debug)]
pub enum GogAchievementsError {
    /// `access_token`, `user_id` or `client_id` was empty / unresolved.
    Missing(&'static str),
    /// HTTP / network failure talking to gameplay.gog.com.
    Network(String),
    /// GOG returned a 4xx/5xx — body is included for diagnostics.
    Api { status: u16, body: String },
}

impl std::fmt::Display for GogAchievementsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GogAchievementsError::Missing(what) => write!(f, "{} missing", what),
            GogAchievementsError::Network(s) => write!(f, "GOG network: {}", s),
            GogAchievementsError::Api { status, body } => {
                write!(f, "GOG api status {}: {}", status, body)
            }
        }
    }
}

impl From<GogAchievementsError> for String {
    fn from(e: GogAchievementsError) -> Self {
        e.to_string()
    }
}

#[derive(Debug, Deserialize)]
struct GogAchievementsResponse {
    items: Vec<GogAchievementItem>,
    #[serde(default)]
    total_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct GogAchievementItem {
    achievement_key: Option<String>,
    #[serde(default)]
    achievement_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    image_url_unlocked: Option<String>,
    #[serde(default)]
    image_url_locked: Option<String>,
    /// ISO-8601 timestamp the user unlocked the achievement; `None` when locked.
    #[serde(default)]
    date_unlocked: Option<String>,
}

/// Fetch one product's achievement list as the authenticated user.
///
/// Returns the rolled-up `Vec<AchievementItem>` already mapped onto the same
/// shape `steam_achievements` produces, so the frontend renders them via the
/// same component path (icon + icon_gray + apiname + achieved + unlocktime).
///
/// `product_id` is the GOG store id (`platform_id` on our side, e.g.
/// "1971477531" for The Witcher 3). The gameplay endpoint resolves it
/// internally; no Galaxy DB / clientId lookup needed.
pub async fn fetch_player_achievements(
    access_token: &str,
    galaxy_user_id: &str,
    product_id: &str,
    locale: &str,
) -> Result<Vec<AchievementItem>, GogAchievementsError> {
    if access_token.is_empty() {
        return Err(GogAchievementsError::Missing("access_token"));
    }
    if galaxy_user_id.is_empty() {
        return Err(GogAchievementsError::Missing("galaxy_user_id"));
    }
    if product_id.is_empty() {
        return Err(GogAchievementsError::Missing("product_id"));
    }

    let url = format!(
        "{}/clients/{}/users/{}/achievements",
        GAMEPLAY_BASE, product_id, galaxy_user_id
    );

    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| GogAchievementsError::Network(format!("client build: {}", e)))?;

    let resp = client
        .get(&url)
        .bearer_auth(access_token)
        // X-Gog-Lc localises the name + description per the user's UI lang.
        // Same header Heroic / Galaxy native client send.
        .header("X-Gog-Lc", locale)
        .send()
        .await
        .map_err(|e| GogAchievementsError::Network(format!("{}: {}", url, e)))?;

    let status = resp.status();
    log::info!(
        "[gog_achievements] product_id={} user={} status={}",
        product_id,
        galaxy_user_id,
        status
    );

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        // 404 means the product genuinely has no achievements declared —
        // surface that as Ok(empty) rather than an error so the UI just
        // renders "no achievements" cleanly.
        if status.as_u16() == 404 {
            return Ok(Vec::new());
        }
        return Err(GogAchievementsError::Api {
            status: status.as_u16(),
            body,
        });
    }

    // Read body as text first so we can log a sample BEFORE parsing.
    // Helps debug "0 unlocked" cases — sometimes the wrong user_id
    // returns the template list with all date_unlocked = null.
    let body = resp
        .text()
        .await
        .map_err(|e| GogAchievementsError::Network(format!("read body: {}", e)))?;
    log::info!(
        "[gog_achievements] body len={} first600={:?}",
        body.len(),
        body.chars().take(600).collect::<String>()
    );
    let parsed: GogAchievementsResponse = serde_json::from_str(&body)
        .map_err(|e| GogAchievementsError::Network(format!("parse achievements: {}", e)))?;

    let raw_unlocked = parsed.items.iter().filter(|i| i.date_unlocked.is_some()).count();
    // Sample the first date_unlocked we see — formats observed in the wild:
    // "2024-01-12T18:42:00.000+00:00" (RFC3339 with ms + tz) and bare "Z".
    let first_date_sample = parsed
        .items
        .iter()
        .find_map(|i| i.date_unlocked.clone());
    log::info!(
        "[gog_achievements] received {} items (total_count={:?}, {} unlocked per date_unlocked, first_date_sample={:?})",
        parsed.items.len(),
        parsed.total_count,
        raw_unlocked,
        first_date_sample
    );

    let items: Vec<AchievementItem> = parsed
        .items
        .into_iter()
        .map(|raw| {
            let unlocktime = raw
                .date_unlocked
                .as_deref()
                .and_then(parse_iso8601_seconds)
                .unwrap_or(0);
            let achieved = unlocktime > 0;
            AchievementItem {
                apiname: raw
                    .achievement_key
                    .or(raw.achievement_id)
                    .unwrap_or_default(),
                name: raw.name.unwrap_or_default(),
                description: raw.description.unwrap_or_default(),
                achieved,
                unlocktime,
                icon: raw.image_url_unlocked.unwrap_or_default(),
                icon_gray: raw.image_url_locked.unwrap_or_default(),
            }
        })
        .collect();
    let mapped_unlocked = items.iter().filter(|i| i.achieved).count();
    log::info!(
        "[gog_achievements] after map: {} achieved (raw was {} unlocked) — if mismatched the ISO parser failed",
        mapped_unlocked,
        raw_unlocked
    );
    Ok(items)
}

/// Parse the `date_unlocked` timestamp into unix seconds.
///
/// GOG returns RFC822-style timezone offsets without a colon
/// (`"2023-10-07T02:56:48+0000"`), which `parse_from_rfc3339` rejects
/// because it wants `+00:00`. We try `%z` first (accepts both `+0000` and
/// `+00:00`), then a variant with fractional seconds, then RFC3339 as a
/// last-resort fallback in case GOG ever switches formats. Returns `None`
/// when nothing matches (= treat as locked).
fn parse_iso8601_seconds(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%z")
        .or_else(|_| chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%z"))
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(s))
        .ok()
        .map(|dt| dt.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gog_no_colon_tz_format() {
        // Format observed in the wild: "2023-10-07T02:56:48+0000".
        let ts = parse_iso8601_seconds("2023-10-07T02:56:48+0000");
        assert!(ts.is_some(), "GOG no-colon tz format should parse");
        assert!(ts.unwrap() > 1_600_000_000);
    }

    #[test]
    fn rfc3339_fallback_still_works() {
        let ts = parse_iso8601_seconds("2024-01-12T18:42:00.000+00:00");
        assert!(ts.is_some());
        assert!(ts.unwrap() > 1_700_000_000);
    }

    #[test]
    fn locked_timestamp_is_none() {
        assert!(parse_iso8601_seconds("").is_none());
        assert!(parse_iso8601_seconds("not-a-date").is_none());
    }
}
