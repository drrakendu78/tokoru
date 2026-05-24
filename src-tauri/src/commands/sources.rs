//! Consolidated read-side surface for the source tiles UI.
//!
//! The Sources page and the Settings → Sources section both render the same
//! `<SourceTile />` grid, which needs three things per source:
//!   - connected? + account label (only for OAuth/OpenID sources)
//!   - last sync timestamp (one key per source in `sync_state`)
//!   - number of games imported (count from `games` table where `source = ?`)
//!
//! Rather than firing 7+ commands at mount, we batch everything into one
//! call: `get_all_source_states`. The frontend keeps a single piece of
//! state and refreshes the whole snapshot after any sync / login / logout.

use serde::Serialize;
use tauri::State;

use crate::services::db::Database;
use crate::services::steam_login;

#[derive(Debug, Clone, Serialize)]
pub struct SourceState {
    /// Canonical source key (matches `Game::source` and the frontend
    /// `Source` union type). Examples: "steam", "epic", "gog", "ubi", "ea",
    /// "itch", "custom".
    pub source: String,
    /// Sources that support a login flow (Steam/Epic/GOG) report whether
    /// they have stored credentials. Local-only sources (Ubi/EA/Itch/
    /// Custom) report `false` and the UI uses `supports_login = false` to
    /// render them with the "Rescan" affordance instead.
    pub connected: bool,
    /// Whether this source has a login flow at all (drives the click target
    /// of the tile: "Connect" vs "Rescan now").
    pub supports_login: bool,
    /// Account label shown next to the green dot when connected. None for
    /// Steam (we display the steam_id in the UI instead) and for local-only
    /// sources.
    pub account_name: Option<String>,
    /// Stored Steam ID — only populated for `source = "steam"`. The frontend
    /// formats this as `#76561198…`.
    pub steam_id: Option<u64>,
    /// Unix timestamp of the last successful sync for this source, or None
    /// if it's never been synced (or it's a local-only source).
    pub last_sync_at: Option<i64>,
    /// Number of games currently in the `games` table tagged with this
    /// source string.
    pub game_count: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AllSourceStates {
    pub sources: Vec<SourceState>,
}

const SOURCES: &[(&str, bool)] = &[
    // (canonical key, supports_login)
    ("steam", true),
    ("epic", true),
    ("gog", true),
    ("ubi", false),
    ("ea", false),
    ("itch", false),
    ("custom", false),
];

/// One round-trip snapshot of every source's tile state. Cheaper than
/// fanning out to `get_steam_connection`/`epic_get_connection`/`gog_get_connection`
/// + N count queries, and easier to keep consistent across views.
#[tauri::command]
pub async fn get_all_source_states(
    db: State<'_, Database>,
) -> Result<AllSourceStates, String> {
    let mut out = Vec::with_capacity(SOURCES.len());

    for &(key, supports_login) in SOURCES {
        let game_count = db
            .count_games_by_source(key)
            .map_err(|e| e.to_string())? as u32;

        let last_sync_at = db
            .get_sync_state(&format!("last_sync_at_{}", key))
            .map_err(|e| e.to_string())?
            .and_then(|s| s.parse::<i64>().ok());

        let (connected, account_name, steam_id) = match key {
            "steam" => {
                let raw = db
                    .get_sync_state("steam_id")
                    .map_err(|e| e.to_string())?
                    .filter(|s| !s.is_empty());
                let token = db
                    .get_sync_state("steam_access_token")
                    .map_err(|e| e.to_string())?
                    .filter(|s| !s.is_empty());
                let parsed_id = raw.as_deref().and_then(|s| s.parse::<u64>().ok());
                // Connected requires *both* a steam_id and a stored
                // access_token. Legacy installs from the OpenID-only era
                // will only have steam_id — we treat those as disconnected
                // so the UI prompts the user to re-login (and grab a token).
                let connected = parsed_id.is_some() && token.is_some();
                let account_name = parsed_id
                    .map(|id| id.to_string())
                    .as_deref()
                    .and_then(steam_login::read_local_username);
                (connected, account_name, parsed_id)
            }
            "epic" => {
                let token = db
                    .get_sync_state("epic_access_token")
                    .map_err(|e| e.to_string())?
                    .filter(|s| !s.is_empty());
                let name = db
                    .get_sync_state("epic_account_name")
                    .map_err(|e| e.to_string())?
                    .filter(|s| !s.is_empty());
                (token.is_some(), name, None)
            }
            "gog" => {
                let token = db
                    .get_sync_state("gog_access_token")
                    .map_err(|e| e.to_string())?
                    .filter(|s| !s.is_empty());
                let name = db
                    .get_sync_state("gog_account_name")
                    .map_err(|e| e.to_string())?
                    .filter(|s| !s.is_empty());
                (token.is_some(), name, None)
            }
            _ => (false, None, None),
        };

        out.push(SourceState {
            source: key.to_string(),
            connected,
            supports_login,
            account_name,
            steam_id,
            last_sync_at,
            game_count,
        });
    }

    Ok(AllSourceStates { sources: out })
}
