//! RAWG.io API key persistence — single key in `sync_state["rawg_api_key"]`.
//!
//! RAWG is the universal metadata fallback used by `sync_metadata_one` when
//! Steam Store / GOG API have nothing for a given title (Star Citizen,
//! Itch-only titles, retro libraries). The free tier allows 20k req/month
//! per key, more than enough for a personal library — keys are per-user,
//! never bundled with the binary.
//!
//! Settings UI hydrates from `get_rawg_api_key` and writes back via
//! `set_rawg_api_key`. Empty string clears the key.

use tauri::State;

use crate::services::db::Database;

#[tauri::command]
pub fn get_rawg_api_key(db: State<'_, Database>) -> Result<String, String> {
    Ok(db
        .get_sync_state("rawg_api_key")
        .map_err(|e| e.to_string())?
        .unwrap_or_default())
}

#[tauri::command]
pub fn set_rawg_api_key(key: String, db: State<'_, Database>) -> Result<(), String> {
    let trimmed = key.trim();
    db.set_sync_state("rawg_api_key", trimmed)
        .map_err(|e| e.to_string())
}
