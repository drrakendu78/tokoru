//! Steam library import via the official `IPlayerService/GetOwnedGames`
//! endpoint, authenticated with the JWT extracted from the
//! `steamLoginSecure` cookie.
//!
//! The JWT inside that cookie is a valid `access_token` for the Steam Web
//! API — same mechanism the official Steam client uses. We pass it via the
//! `access_token` query parameter (NOT `key=`, which would require a
//! registered API key). No public-profile requirement.
//!
//! Auth is bootstrapped by `services::steam_login` (in-app WebView at
//! `store.steampowered.com/login`, cookie extraction on navigation).

use reqwest::Client;
use serde::Deserialize;

use crate::models::Game;

const STEAM_API_BASE: &str = "https://api.steampowered.com";

/// A single owned game shaped from the IPlayerService JSON response.
///
/// `playtime_forever` is the API's value in **minutes**. `rtime_last_played`
/// is a unix-seconds timestamp when present, or `Some(0)` when the user has
/// never launched the game (we filter that down to `None` at the caller).
#[derive(Debug, Clone, Deserialize)]
pub struct SteamOwnedGame {
    pub appid: u64,
    pub name: Option<String>,
    #[allow(dead_code)]
    pub playtime_forever: Option<u64>,
    #[allow(dead_code)]
    pub rtime_last_played: Option<i64>,
}

/// Typed errors returned by the API import path. `Unauthorized` is the one
/// the frontend special-cases — it means the JWT in `steamLoginSecure`
/// expired (or the user changed their Steam password) and the user has to
/// re-connect.
#[derive(Debug)]
pub enum SteamApiError {
    /// Token is missing / expired / revoked. Surface as "Your Steam session
    /// expired — please reconnect" and clear the stored token.
    Unauthorized,
    /// Generic network / HTTP / parse failure.
    Network(String),
}

impl std::fmt::Display for SteamApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SteamApiError::Unauthorized => write!(
                f,
                "Your Steam session expired — please reconnect Steam in Settings."
            ),
            SteamApiError::Network(msg) => write!(f, "{}", msg),
        }
    }
}

impl From<SteamApiError> for String {
    fn from(e: SteamApiError) -> Self {
        e.to_string()
    }
}

/// Fetch the full owned-games list for the given SteamID64. `access_token`
/// is the JWT extracted from the `steamLoginSecure` cookie by
/// `services::steam_login`. Returns `Unauthorized` if Steam rejects the
/// token (401) or the response body has no `response.games` field (Steam's
/// convention for "token isn't valid for this user").
pub async fn fetch_owned_games(
    access_token: &str,
    steam_id: &str,
) -> Result<Vec<SteamOwnedGame>, SteamApiError> {
    if access_token.is_empty() {
        return Err(SteamApiError::Unauthorized);
    }

    let url = format!(
        "{}/IPlayerService/GetOwnedGames/v1/?access_token={}&steamid={}\
         &include_appinfo=1&include_played_free_games=1\
         &skip_unvetted_apps=0&include_free_sub=1",
        STEAM_API_BASE, access_token, steam_id
    );

    let client = Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "Tokoru/0.1")
        .send()
        .await
        .map_err(|e| SteamApiError::Network(format!("Steam API request failed: {}", e)))?;

    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        log::warn!("[steam] IPlayerService returned {}, token likely expired", status);
        return Err(SteamApiError::Unauthorized);
    }
    if !status.is_success() {
        return Err(SteamApiError::Network(format!(
            "Steam API returned HTTP {}",
            status
        )));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| SteamApiError::Network(format!("Steam API body read failed: {}", e)))?;

    parse_owned_games_json(&body)
}

#[derive(Deserialize)]
struct GetOwnedGamesEnvelope {
    response: GetOwnedGamesInner,
}

#[derive(Deserialize)]
struct GetOwnedGamesInner {
    #[serde(default)]
    games: Option<Vec<SteamOwnedGame>>,
}

/// Parse the JSON envelope. Treat a missing `games` field as `Unauthorized`
/// — that's Steam's convention when the access_token isn't valid for the
/// requested `steamid` (e.g. token expired, or `steamid` mismatch).
fn parse_owned_games_json(body: &str) -> Result<Vec<SteamOwnedGame>, SteamApiError> {
    let env: GetOwnedGamesEnvelope = serde_json::from_str(body).map_err(|e| {
        SteamApiError::Network(format!("Steam API JSON parse failed: {}", e))
    })?;
    match env.response.games {
        Some(games) => Ok(games),
        None => Err(SteamApiError::Unauthorized),
    }
}

/// Fetch every game shared with the current user through Steam Families.
///
/// Two-step flow against the undocumented `IFamilyGroupsService`:
///   1. `GetFamilyGroupForUser` → resolve the user's `family_groupid`. If
///      the user isn't in a family group, returns an empty list (the API
///      returns `family_groupid: "0"`).
///   2. `GetSharedLibraryApps` → list every app another family member has
///      made available to the user. Marked exactly like owned games so the
///      existing `steam://install/<appid>` button works — Steam itself
///      handles family-share authorization at install time.
///
/// Both endpoints accept the same JWT we use for `GetOwnedGames`.
pub async fn fetch_family_games(
    access_token: &str,
    steam_id: &str,
) -> Result<Vec<SteamOwnedGame>, SteamApiError> {
    if access_token.is_empty() {
        return Err(SteamApiError::Unauthorized);
    }

    let client = Client::new();

    // 1. Resolve family_groupid for this user.
    let family_url = format!(
        "{}/IFamilyGroupsService/GetFamilyGroupForUser/v1/?access_token={}&steamid={}",
        STEAM_API_BASE, access_token, steam_id
    );
    let resp = client
        .get(&family_url)
        .header("User-Agent", "Tokoru/0.1")
        .send()
        .await
        .map_err(|e| SteamApiError::Network(format!("family group request failed: {}", e)))?;
    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(SteamApiError::Unauthorized);
    }
    if !status.is_success() {
        // No family or no access — treat as empty list, not an error. The
        // user simply isn't in a family group.
        log::info!("[steam] family group lookup returned HTTP {} — skipping", status);
        return Ok(Vec::new());
    }
    let body = resp
        .text()
        .await
        .map_err(|e| SteamApiError::Network(format!("family group body read failed: {}", e)))?;

    let family_id = parse_family_group_id(&body);
    let Some(family_groupid) = family_id else {
        // User isn't in a family group.
        return Ok(Vec::new());
    };

    // 2. List apps shared by other family members.
    let shared_url = format!(
        "{}/IFamilyGroupsService/GetSharedLibraryApps/v1/?access_token={}\
         &family_groupid={}&steamid={}&include_own=false&include_excluded=false\
         &include_free=false&include_non_games=false&max_apps=500&language=french",
        STEAM_API_BASE, access_token, family_groupid, steam_id
    );
    let resp = client
        .get(&shared_url)
        .header("User-Agent", "Tokoru/0.1")
        .send()
        .await
        .map_err(|e| SteamApiError::Network(format!("shared library request failed: {}", e)))?;
    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(SteamApiError::Unauthorized);
    }
    if !status.is_success() {
        return Err(SteamApiError::Network(format!(
            "shared library returned HTTP {}",
            status
        )));
    }
    let body = resp
        .text()
        .await
        .map_err(|e| SteamApiError::Network(format!("shared library body read failed: {}", e)))?;

    parse_shared_library_json(&body)
}

#[derive(Deserialize)]
struct GetFamilyGroupEnvelope {
    response: GetFamilyGroupInner,
}

#[derive(Deserialize)]
struct GetFamilyGroupInner {
    #[serde(default)]
    family_groupid: Option<String>,
}

fn parse_family_group_id(body: &str) -> Option<String> {
    let env: GetFamilyGroupEnvelope = serde_json::from_str(body).ok()?;
    let raw = env.response.family_groupid?;
    // Steam returns "0" for users not in any family group.
    if raw.is_empty() || raw == "0" {
        None
    } else {
        Some(raw)
    }
}

#[derive(Deserialize)]
struct GetSharedLibraryEnvelope {
    response: GetSharedLibraryInner,
}

#[derive(Deserialize)]
struct GetSharedLibraryInner {
    #[serde(default)]
    apps: Vec<SharedLibraryApp>,
}

#[derive(Deserialize)]
struct SharedLibraryApp {
    appid: u64,
    #[serde(default)]
    name: Option<String>,
}

fn parse_shared_library_json(body: &str) -> Result<Vec<SteamOwnedGame>, SteamApiError> {
    let env: GetSharedLibraryEnvelope = serde_json::from_str(body).map_err(|e| {
        SteamApiError::Network(format!("shared library JSON parse failed: {}", e))
    })?;
    Ok(env
        .response
        .apps
        .into_iter()
        .map(|a| SteamOwnedGame {
            appid: a.appid,
            name: a.name,
            playtime_forever: None,
            rtime_last_played: None,
        })
        .collect())
}

/// Map a Steam owned game to our internal `Game` model.
/// Uses cloudflare CDN URLs (more reliable than the akamai legacy hostname).
pub fn steam_owned_to_game(g: &SteamOwnedGame) -> Option<Game> {
    let title = g.name.clone()?;
    let appid = g.appid;
    let launch_command = format!("steam://run/{}", appid);
    let artwork_url = format!(
        "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/library_600x900_2x.jpg",
        appid
    );
    let hero_url = format!(
        "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/library_hero.jpg",
        appid
    );
    let logo_url = format!(
        "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/logo.png",
        appid
    );

    let mut game = Game::owned(
        title,
        "steam".to_string(),
        appid.to_string(),
        launch_command,
    );
    game.artwork_url = Some(artwork_url);
    game.hero_url = Some(hero_url);
    game.logo_url = Some(logo_url);
    Some(game)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_json_response() {
        let body = r#"{
          "response": {
            "game_count": 2,
            "games": [
              { "appid": 440, "name": "Team Fortress 2", "playtime_forever": 738, "rtime_last_played": 1700000000 },
              { "appid": 730, "name": "CS2", "playtime_forever": 0, "rtime_last_played": 0 }
            ]
          }
        }"#;
        let games = parse_owned_games_json(body).expect("should parse");
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].appid, 440);
        assert_eq!(games[0].name.as_deref(), Some("Team Fortress 2"));
        assert_eq!(games[0].playtime_forever, Some(738));
        assert_eq!(games[1].appid, 730);
    }

    #[test]
    fn missing_games_field_is_unauthorized() {
        // Steam returns `{ "response": {} }` when the token is invalid for
        // the requested steamid.
        let body = r#"{ "response": {} }"#;
        let err = parse_owned_games_json(body).expect_err("should fail");
        assert!(matches!(err, SteamApiError::Unauthorized));
    }

    #[test]
    fn malformed_json_is_network_error() {
        let err = parse_owned_games_json("not json").expect_err("should fail");
        assert!(matches!(err, SteamApiError::Network(_)));
    }
}
