//! UI language persistence — the user-facing locale picked in Settings is
//! stored in `sync_state["app_locale"]`. Backend modules that hit external
//! APIs (Steam Store, GOG) read it to localise their `Accept-Language` /
//! `l=` parameters so the metadata flowing back into Tokoru matches the
//! language the user actually sees in the UI.

use tauri::State;

use crate::services::db::Database;

/// Key in `sync_state` that holds the active UI locale (2-letter ISO 639-1
/// code, e.g. `en`, `fr`, `ja`). Empty / missing = auto-detect at boot.
pub const KEY_APP_LOCALE: &str = "app_locale";

/// Unix-seconds timestamp of the most recent locale change. GameDetail
/// compares each game's `metadata_synced_at` to this stamp to decide
/// whether the cached description / tags need a lazy on-open refresh in
/// the current language.
pub const KEY_LOCALE_CHANGED_AT: &str = "locale_changed_at";

/// Persist the user-picked locale so the Rust side can read it on its
/// next outgoing API call. Called by the Settings → Language picker
/// whenever the user changes language.
///
/// When the new locale differs from the previously stored one, a
/// `locale_changed_at` timestamp is written to `sync_state`. GameDetail
/// then performs a lazy on-open refresh for the specific game the user
/// is looking at, rather than batch-refetching the whole library (which
/// would saturate the Steam Store API rate limit for 400+ games).
#[tauri::command]
pub async fn set_app_locale(
    locale: String,
    db: State<'_, Database>,
) -> Result<(), String> {
    // Trim + lowercase to keep the stored value canonical (i18next can
    // hand us things like `EN-us` if the OS locale leaks through).
    let normalized = locale.trim().to_ascii_lowercase();
    let previous = read_app_locale(&db);
    db.set_sync_state(KEY_APP_LOCALE, &normalized)
        .map_err(|e| e.to_string())?;
    // ALWAYS bump `locale_changed_at`, even when the picked language
    // matches the stored one. Rationale: the user might be retrying a
    // language they already had (e.g. to retry the lazy-refresh path
    // after my last bugfix), and a re-tap should reliably mark every
    // existing metadata row as "synced under an older locale stamp".
    // It also covers the upgrade case where users were already on FR
    // before the lazy-refresh code landed — they re-pick FR, the stamp
    // bumps, and GameDetail finally refreshes them.
    let now = chrono::Utc::now().timestamp().to_string();
    if let Err(e) = db.set_sync_state(KEY_LOCALE_CHANGED_AT, &now) {
        log::warn!("[locale] failed to stamp locale_changed_at: {}", e);
    }
    log::info!(
        "[locale] app_locale picked '{}' (previous '{}') — locale_changed_at stamped at {}",
        normalized,
        previous,
        now
    );
    Ok(())
}

/// Read the `locale_changed_at` stamp (unix seconds). Returns 0 when the
/// user hasn't changed language yet on this install. Synchronous helper
/// for the metadata read path that needs to flag stale rows.
pub fn read_locale_changed_at(db: &Database) -> i64 {
    db.get_sync_state(KEY_LOCALE_CHANGED_AT)
        .ok()
        .flatten()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
}

// ────────────────────────────────────────────────────────────────────────
// Onboarding completion flag
// ────────────────────────────────────────────────────────────────────────
//
// The onboarding wizard (`pages/Onboarding.tsx`) runs once on a fresh
// install — Steam/Epic/GOG OAuth pairing, playtime consent toggles, then
// hands off to the Library. After it finishes, this flag is flipped so
// the app boots straight to the Library on every subsequent launch.

const KEY_ONBOARDING_DONE: &str = "onboarding_done";

#[tauri::command]
pub async fn is_onboarding_done(db: State<'_, Database>) -> Result<bool, String> {
    Ok(db
        .get_sync_state(KEY_ONBOARDING_DONE)
        .map_err(|e| e.to_string())?
        .map(|s| s == "true")
        .unwrap_or(false))
}

#[tauri::command]
pub async fn mark_onboarding_done(db: State<'_, Database>) -> Result<(), String> {
    db.set_sync_state(KEY_ONBOARDING_DONE, "true")
        .map_err(|e| e.to_string())?;
    log::info!("[onboarding] marked done");
    Ok(())
}

/// Reset the onboarding flag so the next boot — or the immediate route
/// switch from Settings → Replay onboarding — re-runs the wizard. Does
/// NOT touch stored credentials, library rows, or playtime data.
#[tauri::command]
pub async fn reset_onboarding_done(db: State<'_, Database>) -> Result<(), String> {
    db.set_sync_state(KEY_ONBOARDING_DONE, "false")
        .map_err(|e| e.to_string())?;
    log::info!("[onboarding] reset — wizard will replay");
    Ok(())
}

/// Read the active UI locale. Returns the stored value when present,
/// otherwise `"en"` as the canonical default. Exposed as a Tauri command
/// for symmetry — frontend gets its own value from i18next, but other
/// callers (debug overlays, future plugins) may want it.
#[tauri::command]
pub async fn get_app_locale(db: State<'_, Database>) -> Result<String, String> {
    Ok(read_app_locale(&db))
}

/// Synchronous helper for Rust-side callers (Steam Store, GOG). Returns
/// the stored locale or `"en"` when nothing has been picked yet.
pub fn read_app_locale(db: &Database) -> String {
    db.get_sync_state(KEY_APP_LOCALE)
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "en".to_string())
}

/// Steam Store API uses two query params for localisation: `cc` (country
/// code, drives currency + regional availability) and `l` (full English
/// name of the language, e.g. `french`, `schinese`). Return the right
/// `(cc, l)` pair for our 2-letter locale code. Unknown locales fall back
/// to US/English.
pub fn steam_store_params(locale: &str) -> (&'static str, &'static str) {
    match locale {
        "fr" => ("fr", "french"),
        "es" => ("es", "spanish"),
        "de" => ("de", "german"),
        "it" => ("it", "italian"),
        "pt" => ("br", "brazilian"),
        "ru" => ("ru", "russian"),
        "zh" => ("cn", "schinese"),
        "ja" => ("jp", "japanese"),
        "ko" => ("kr", "koreana"),
        _ => ("us", "en"),
    }
}

/// GOG's `gameplay.gog.com` API takes an `X-Gog-Lc` header in BCP-47 form
/// (`fr-FR`, `de-DE`). Return the right tag for our 2-letter locale.
/// Defaults to `en-US`.
pub fn gog_locale_tag(locale: &str) -> &'static str {
    match locale {
        "fr" => "fr-FR",
        "es" => "es-ES",
        "de" => "de-DE",
        "it" => "it-IT",
        "pt" => "pt-BR",
        "ru" => "ru-RU",
        "zh" => "zh-CN",
        "ja" => "ja-JP",
        "ko" => "ko-KR",
        _ => "en-US",
    }
}
