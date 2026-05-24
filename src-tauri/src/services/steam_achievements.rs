//! Player-achievement fetcher via Steam Community XML feed.
//!
//! `ISteamUserStats/GetPlayerAchievements` rejects the JWT we have in
//! the session cookie ("Required parameter 'key' is missing") and would
//! force every user to register their own Steam Web API key. To stay
//! user-friendly we follow Playnite SuccessStory's approach: scrape the
//! per-profile community stats page.
//!
//! URL: `https://steamcommunity.com/profiles/{steamid}/stats/{appid}/?xml=1&l=english`
//!
//! Authentication: pass the original `steamLoginSecure` cookie value
//! (`<steamid>||<jwt>`, URL-encoded as `<steamid>%7C%7C<jwt>`). This is
//! the SAME cookie we already extracted during login — no extra setup
//! for the user. The cookie unlocks private profiles; public profiles
//! work without it too.
//!
//! XML shape (truncated):
//! ```xml
//! <playerstats>
//!   <gameName>Cyberpunk 2077</gameName>
//!   <achievements>
//!     <achievement closed="1">
//!       <apiname>ACH_X</apiname>
//!       <name>Mafia killer</name>
//!       <description>Kill 25 enemies as a Maelstromer</description>
//!       <iconClosed>...png</iconClosed>
//!       <iconOpen>...png</iconOpen>
//!       <unlockTimestamp>1700000000</unlockTimestamp>
//!     </achievement>
//!   </achievements>
//! </playerstats>
//! ```
//!
//! When the game has no achievements the `<achievements>` block is
//! absent — we return an empty Vec without erroring.

use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::time::Duration;

use crate::services::steam_api::SteamApiError;

const COMMUNITY_BASE: &str = "https://steamcommunity.com";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AchievementItem {
    pub apiname: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub achieved: bool,
    /// Unix seconds. 0 when the achievement isn't unlocked.
    pub unlocktime: i64,
    /// Closed icon URL (full-color, shown when unlocked).
    #[serde(default)]
    pub icon: String,
    /// Open icon URL (grayscale, shown when locked).
    #[serde(default)]
    pub icon_gray: String,
}

/// One regex per field, compiled once. Reusing the same `Regex` across
/// calls is cheaper than re-parsing the pattern on every game.
struct Patterns {
    block: Regex,
    closed: Regex,
    apiname: Regex,
    name: Regex,
    desc: Regex,
    icon: Regex,
    icon_gray: Regex,
    unlock: Regex,
}

static PATTERNS: OnceLock<Patterns> = OnceLock::new();

fn patterns() -> &'static Patterns {
    PATTERNS.get_or_init(|| Patterns {
        // Each <achievement closed="X">...</achievement> block, captured
        // with the full inner XML in group 1.
        block: Regex::new(r#"(?s)<achievement closed="(\d)"[^>]*>(.*?)</achievement>"#).unwrap(),
        closed: Regex::new(r#"closed="(\d)""#).unwrap(),
        // Every text-bearing field below is CDATA-wrapped on Steam's
        // community feed (`<apiname><![CDATA[id]]></apiname>`). The
        // `(?s)` flag turns `.` into "any char incl. newline" so the
        // capture survives multi-line CDATA, then we strip the optional
        // `<![CDATA[ … ]]>` markers.
        apiname: Regex::new(r#"(?s)<apiname>(?:<!\[CDATA\[)?(.*?)(?:\]\]>)?</apiname>"#).unwrap(),
        name: Regex::new(r#"(?s)<name>(?:<!\[CDATA\[)?(.*?)(?:\]\]>)?</name>"#).unwrap(),
        desc: Regex::new(r#"(?s)<description>(?:<!\[CDATA\[)?(.*?)(?:\]\]>)?</description>"#).unwrap(),
        icon: Regex::new(r#"(?s)<iconClosed>(?:<!\[CDATA\[)?(.*?)(?:\]\]>)?</iconClosed>"#).unwrap(),
        icon_gray: Regex::new(r#"(?s)<iconOpen>(?:<!\[CDATA\[)?(.*?)(?:\]\]>)?</iconOpen>"#).unwrap(),
        unlock: Regex::new(r#"<unlockTimestamp>(\d+)</unlockTimestamp>"#).unwrap(),
    })
}

/// Fetch the user's achievements for one game via the community XML
/// feed. Auth = the original `steamLoginSecure` cookie pair (steamid +
/// JWT) reconstructed from what we stored at login time.
///
/// Returns an empty Vec when the game has no achievements declared (the
/// XML page simply omits the `<achievements>` block — Steam doesn't
/// surface an error code).
pub async fn fetch_player_achievements(
    steam_id: &str,
    jwt: &str,
    appid: &str,
) -> Result<Vec<AchievementItem>, SteamApiError> {
    if steam_id.is_empty() || jwt.is_empty() {
        return Err(SteamApiError::Unauthorized);
    }

    let url = format!(
        "{}/profiles/{}/stats/{}/?xml=1&l=english",
        COMMUNITY_BASE, steam_id, appid
    );
    // Reconstruct the original cookie value. Steam stores it as
    // `<steamid>||<jwt>` and serves it URL-encoded (`%7C%7C`).
    let cookie_value = format!("{}%7C%7C{}", steam_id, jwt);

    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        // Disable redirects so we notice if Steam bounces us to the
        // login page (which would mean our cookie is dead).
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| SteamApiError::Network(format!("client build: {}", e)))?;

    let resp = client
        .get(&url)
        .header("Cookie", format!("steamLoginSecure={}", cookie_value))
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
        )
        .send()
        .await
        .map_err(|e| SteamApiError::Network(format!("achievements {}: {}", appid, e)))?;

    let status = resp.status();
    log::info!("[achievements] {} community status: {}", appid, status);
    if status.is_redirection() {
        // Community bounces unauthenticated requests to /login on private
        // profiles. Treat as expired session.
        return Err(SteamApiError::Unauthorized);
    }
    if !status.is_success() {
        return Err(SteamApiError::Network(format!(
            "achievements {} status {}",
            appid, status
        )));
    }
    let body = resp
        .text()
        .await
        .map_err(|e| SteamApiError::Network(format!("achievements {} body: {}", appid, e)))?;
    log::info!(
        "[achievements] {} body len={} first300={:?}",
        appid,
        body.len(),
        body.chars().take(300).collect::<String>()
    );
    if let Some(idx) = body.find("<achievement") {
        let sample = &body[idx..body.len().min(idx + 700)];
        log::info!("[achievements] {} <achievement sample: {:?}", appid, sample);
    } else {
        log::warn!("[achievements] {} no <achievement marker in body", appid);
    }

    // Community renders a private-profile page (HTML) when auth fails,
    // not the XML feed. Detect that and surface as Unauthorized so the
    // UI can ask the user to reconnect Steam.
    if !body.contains("<achievements>") && !body.contains("<playerstats") {
        if body.contains("login") || body.contains("private") {
            return Err(SteamApiError::Unauthorized);
        }
        // Game has no achievements at all — Steam omits the
        // <achievements> block on stats pages for those titles.
        return Ok(Vec::new());
    }

    Ok(parse_achievements_xml(&body))
}

fn parse_achievements_xml(body: &str) -> Vec<AchievementItem> {
    let p = patterns();
    let mut out: Vec<AchievementItem> = Vec::new();
    let total_blocks = p.block.captures_iter(body).count();
    log::info!("[achievements] parser found {} <achievement> blocks", total_blocks);
    for cap in p.block.captures_iter(body) {
        let achieved = cap.get(1).map(|m| m.as_str() == "1").unwrap_or(false);
        let inner = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let apiname = p
            .apiname
            .captures(inner)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        // Skip rows without an apiname — they're stub <achievement>
        // entries Steam sometimes emits at the end of the list.
        if apiname.is_empty() {
            continue;
        }
        let name = p
            .name
            .captures(inner)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        let description = p
            .desc
            .captures(inner)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        let icon = p
            .icon
            .captures(inner)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        let icon_gray = p
            .icon_gray
            .captures(inner)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        let unlocktime = p
            .unlock
            .captures(inner)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<i64>().ok())
            .unwrap_or(0);
        // Fall back to checking the `closed` attribute (already captured
        // above) — sometimes the timestamp is missing but closed=1 is
        // authoritative.
        let _ = p.closed.captures(inner);

        out.push(AchievementItem {
            apiname,
            name,
            description,
            achieved,
            unlocktime,
            icon,
            icon_gray,
        });
    }
    out
}
