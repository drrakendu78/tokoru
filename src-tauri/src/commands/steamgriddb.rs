//! SteamGridDB settings persistence (API key, artwork style, auto-fetch toggle).
//!
//! All values live in the existing `sync_state` key/value table so no
//! schema migration is required. Keys:
//!   - `steamgriddb_api_key`           — raw API key string ("" = use default)
//!   - `steamgriddb_artwork_style`     — one of: official|painted|photoreal|minimal|any
//!   - `steamgriddb_auto_fetch`        — "true" | "false"
//!   - `steamgriddb_prefer_animated`   — "true" | "false" (default false). When
//!                                       true the cover/hero rankers prefer
//!                                       `.webm` over static images.
//!   - `steamgriddb_allow_nsfw`        — "true" | "false" (default false). When
//!                                       false, NSFW-tagged grids are filtered
//!                                       out of the auto-pick (Browse still
//!                                       lists them with an NSFW toggle).
//!   - `steamgriddb_api_key_saved_at`  — unix seconds the key was last persisted
//!
//! The Settings UI hydrates from `get_steamgriddb_settings` on mount and writes
//! back via `set_steamgriddb_settings` on every change (no save button — each
//! interaction commits immediately). Other backend code reads these keys
//! directly via `Database::get_sync_state` (see `games.rs::get_active_api_key`
//! and the artwork style helper).
//!
//! Field names use snake_case (matches the convention elsewhere — see
//! `commands::sources::SourceState`).

use serde::Serialize;
use tauri::State;

use crate::services::db::Database;

/// Snapshot of the SteamGridDB-related preferences in `sync_state`.
#[derive(Debug, Clone, Serialize)]
pub struct SteamGridDbSettings {
    /// The user-supplied API key. Empty string when the user hasn't set one
    /// (in which case the backend falls back to the bundled default key).
    pub api_key: String,
    /// True iff `api_key` is non-empty. Convenience flag so the UI can decide
    /// whether to show the "Last validated" line without having to length-check.
    pub has_custom_key: bool,
    /// Preferred artwork style — passed through as the `styles=` query param
    /// when fetching covers/heroes/logos. See `services::steamgrid` for the
    /// mapping table.
    pub artwork_style: String,
    /// Whether `add_game` (and future auto-fetch call sites) should hit
    /// SteamGridDB right after an insert. Default true.
    pub auto_fetch: bool,
    /// When true, the auto-pick prefers animated `.webm` grids over static
    /// images for the cover and hero slots. Default false (static = lighter
    /// on the Steam client, see Phase 4 grid lazy-loading discussion).
    pub prefer_animated: bool,
    /// When true, NSFW-tagged grids are eligible in the auto-pick. Default
    /// false. The Browse covers UI ignores this setting (user already opted
    /// in by clicking the explicit NSFW toggle).
    pub allow_nsfw: bool,
    /// Unix seconds the API key was last written, or 0 when no custom key has
    /// ever been saved. Used by the UI to render "Last validated · {date}".
    pub api_key_saved_at: i64,
}

const ALLOWED_STYLES: &[&str] = &["official", "painted", "photoreal", "minimal", "any"];

fn read_settings(db: &Database) -> Result<SteamGridDbSettings, String> {
    let api_key = db
        .get_sync_state("steamgriddb_api_key")
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let artwork_style = db
        .get_sync_state("steamgriddb_artwork_style")
        .map_err(|e| e.to_string())?
        .filter(|s| ALLOWED_STYLES.contains(&s.as_str()))
        .unwrap_or_else(|| "any".to_string());
    let auto_fetch = db
        .get_sync_state("steamgriddb_auto_fetch")
        .map_err(|e| e.to_string())?
        .map(|s| s != "false")
        .unwrap_or(true);
    let prefer_animated = db
        .get_sync_state("steamgriddb_prefer_animated")
        .map_err(|e| e.to_string())?
        .map(|s| s == "true")
        .unwrap_or(false);
    let allow_nsfw = db
        .get_sync_state("steamgriddb_allow_nsfw")
        .map_err(|e| e.to_string())?
        .map(|s| s == "true")
        .unwrap_or(false);
    let api_key_saved_at = db
        .get_sync_state("steamgriddb_api_key_saved_at")
        .map_err(|e| e.to_string())?
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);

    Ok(SteamGridDbSettings {
        has_custom_key: !api_key.is_empty(),
        api_key,
        artwork_style,
        auto_fetch,
        prefer_animated,
        allow_nsfw,
        api_key_saved_at,
    })
}

/// Return the current persisted settings. Defaults: empty key (=> fallback),
/// style = "any", auto_fetch = true.
#[tauri::command]
pub fn get_steamgriddb_settings(
    db: State<'_, Database>,
) -> Result<SteamGridDbSettings, String> {
    read_settings(&db)
}

/// Patch any subset of the three settings. Pass `None` to leave a field
/// untouched. Pass `Some("")` for `api_key` to explicitly clear it (the
/// backend then falls back to the bundled default key on the next fetch).
#[tauri::command]
pub fn set_steamgriddb_settings(
    api_key: Option<String>,
    artwork_style: Option<String>,
    auto_fetch: Option<bool>,
    prefer_animated: Option<bool>,
    allow_nsfw: Option<bool>,
    db: State<'_, Database>,
) -> Result<SteamGridDbSettings, String> {
    if let Some(style) = artwork_style.as_deref() {
        if !ALLOWED_STYLES.contains(&style) {
            return Err(format!(
                "Invalid artwork_style '{}' (allowed: {})",
                style,
                ALLOWED_STYLES.join(", ")
            ));
        }
        db.set_sync_state("steamgriddb_artwork_style", style)
            .map_err(|e| e.to_string())?;
    }

    if let Some(key) = api_key.as_deref() {
        let trimmed = key.trim();
        db.set_sync_state("steamgriddb_api_key", trimmed)
            .map_err(|e| e.to_string())?;
        if !trimmed.is_empty() {
            let now = chrono::Utc::now().timestamp();
            db.set_sync_state("steamgriddb_api_key_saved_at", &now.to_string())
                .map_err(|e| e.to_string())?;
        } else {
            // Clearing the key — wipe the timestamp too so the UI's "Last
            // validated" line disappears.
            db.set_sync_state("steamgriddb_api_key_saved_at", "0")
                .map_err(|e| e.to_string())?;
        }
    }

    if let Some(enabled) = auto_fetch {
        db.set_sync_state(
            "steamgriddb_auto_fetch",
            if enabled { "true" } else { "false" },
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(enabled) = prefer_animated {
        db.set_sync_state(
            "steamgriddb_prefer_animated",
            if enabled { "true" } else { "false" },
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(enabled) = allow_nsfw {
        db.set_sync_state(
            "steamgriddb_allow_nsfw",
            if enabled { "true" } else { "false" },
        )
        .map_err(|e| e.to_string())?;
    }

    read_settings(&db)
}
