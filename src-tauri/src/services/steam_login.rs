//! Steam web-login flow via WebView cookie extraction.
//!
//! Replaces the OpenID 2.0 + public XML feed approach (which required Game
//! Details to be Public and still 403'd in many cases) with the same
//! mechanism the official Steam client uses: log the user in at
//! `store.steampowered.com/login`, then read the `steamLoginSecure` cookie
//! the Steam web stack sets on `.steampowered.com`.
//!
//! The cookie value is `<steamid64>%7C%7C<jwt>` (URL-encoded `||`). The JWT
//! after the separator is a valid `access_token` for the official Steam Web
//! API — we pass it via the `access_token` query param (NOT `key=`) to
//! endpoints like `IPlayerService/GetOwnedGames`. No API key, no public
//! profile requirement.

use std::path::PathBuf;

/// The page we open in the WebView. After a successful sign-in Steam
/// redirects to `/?redir=explore` which is a normal HTML page (the
/// `redir=explore` is what tells Steam to keep the user on the store rather
/// than bouncing through `/openid/login` or similar).
pub const LOGIN_URL: &str = "https://store.steampowered.com/login/?redir=explore";

/// URL used for the cookie lookup on the WebView side. The
/// `steamLoginSecure` cookie is scoped to the `.steampowered.com` domain so
/// both `store.steampowered.com` and `help.steampowered.com` see it.
pub const STEAMPOWERED_URL: &str = "https://store.steampowered.com";

/// Window title shown for the WebView. The label is used to look up the
/// window via `app.get_webview_window`.
pub const LOGIN_WINDOW_LABEL: &str = "steam-login";
pub const LOGIN_WINDOW_TITLE: &str = "Tokoru — Sign in to Steam";

/// Best-effort decode of a Steam cookie value. Cookies come back URL-
/// encoded out of the WebView's cookie store (`%7C%7C` for `||`); we
/// percent-decode so callers can splitn on the literal `||` separator.
pub fn decode_steam_cookie(value: &str) -> String {
    percent_encoding::percent_decode_str(value)
        .decode_utf8_lossy()
        .trim()
        .to_string()
}

/// Parse a `steamLoginSecure` cookie value into `(steamid, jwt)`. Returns
/// None if the value doesn't follow the expected `<id>||<jwt>` shape.
pub fn parse_steam_login_secure(value: &str) -> Option<(String, String)> {
    let decoded = decode_steam_cookie(value);
    let mut parts = decoded.splitn(2, "||");
    let steam_id = parts.next()?.trim().to_string();
    let jwt = parts.next()?.trim().to_string();
    if steam_id.is_empty() || jwt.is_empty() {
        return None;
    }
    Some((steam_id, jwt))
}

/// Mask a SteamID64 for logs — keeps the leading 5 digits, redacts the rest.
pub fn mask_steam_id(id: u64) -> String {
    let s = id.to_string();
    if s.len() <= 5 {
        return s;
    }
    format!("{}…{}", &s[..5], "X".repeat(s.len() - 5))
}

// ---------- loginusers.vdf reads (local Steam install) ----------

fn get_steam_install_path() -> Option<PathBuf> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let steam_key = hkcu.open_subkey("Software\\Valve\\Steam").ok()?;
    let path: String = steam_key.get_value("SteamPath").ok()?;
    Some(PathBuf::from(path))
}

/// Read the most-recently-logged-in SteamID64 from `config/loginusers.vdf`.
/// Returned as a string so callers can compare against the cookie value
/// without re-parsing. Falls back through every user block; the one with
/// `"MostRecent" "1"` wins.
///
/// Kept for fallback paths even though the current cookie flow always
/// surfaces a SteamID — if a future Steam UI change ever delivers the JWT
/// without the SteamID prefix, this becomes the recovery path.
#[allow(dead_code)]
pub fn read_local_steamid() -> Option<String> {
    let steam_path = get_steam_install_path()?;
    let vdf_path = steam_path.join("config").join("loginusers.vdf");
    let content = std::fs::read_to_string(&vdf_path).ok()?;

    let mut current_id: Option<String> = None;
    let mut most_recent_id: Option<String> = None;
    let mut first_id: Option<String> = None;

    for line in content.lines() {
        let trimmed = line.trim().trim_matches('"');
        if trimmed.starts_with("7656") && trimmed.len() >= 17 {
            let id = trimmed.split('"').next().unwrap_or(trimmed);
            if id.chars().all(|c| c.is_ascii_digit()) && id.len() >= 17 {
                current_id = Some(id.to_string());
                if first_id.is_none() {
                    first_id = current_id.clone();
                }
            }
        }
        if line.contains("MostRecent") && line.contains("\"1\"") {
            if let Some(ref id) = current_id {
                most_recent_id = Some(id.clone());
            }
        }
    }

    most_recent_id.or(first_id)
}

/// Read the PersonaName for a given SteamID from `config/loginusers.vdf`.
/// Used to surface "Signed in as <name>" in the UI without an extra Steam
/// Web API call. Returns None if the file is missing, the ID isn't in
/// there, or the block has no `PersonaName` line.
pub fn read_local_username(steam_id: &str) -> Option<String> {
    let steam_path = get_steam_install_path()?;
    let vdf_path = steam_path.join("config").join("loginusers.vdf");
    let content = std::fs::read_to_string(&vdf_path).ok()?;

    let mut in_user_block = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.contains(steam_id) {
            in_user_block = true;
            continue;
        }
        if in_user_block && trimmed.contains("PersonaName") {
            // "PersonaName"		"Display Name"
            let parts: Vec<&str> = trimmed.split('"').collect();
            if parts.len() >= 4 {
                return Some(parts[3].to_string());
            }
        }
        if in_user_block && trimmed == "}" {
            break;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_url_encoded_cookie() {
        let raw = "76561198000000000%7C%7CeyJhbGciOiJFUzI1NiIsInR5cCI6IkpXVCJ9.x.y";
        let decoded = decode_steam_cookie(raw);
        assert!(decoded.starts_with("76561198000000000||"));
    }

    #[test]
    fn parses_steam_login_secure_cookie() {
        let raw = "76561198000000000%7C%7CeyJhbGciOiJFUzI1NiIsInR5cCI6IkpXVCJ9.x.y";
        let (id, jwt) = parse_steam_login_secure(raw).expect("should parse");
        assert_eq!(id, "76561198000000000");
        assert!(jwt.starts_with("eyJ"));
    }

    #[test]
    fn rejects_cookie_without_separator() {
        assert!(parse_steam_login_secure("76561198000000000").is_none());
        assert!(parse_steam_login_secure("").is_none());
    }

    #[test]
    fn rejects_cookie_with_empty_parts() {
        assert!(parse_steam_login_secure("||eyJxxx").is_none());
        assert!(parse_steam_login_secure("76561198000000000||").is_none());
    }

    #[test]
    fn masks_steam_id() {
        let masked = mask_steam_id(76561198000000000);
        assert!(masked.starts_with("76561"));
        assert!(masked.contains('…'));
    }
}
