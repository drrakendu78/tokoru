//! Tauri commands for the Steam web-login integration.
//!
//! Auth mechanism: we open `store.steampowered.com/login/?redir=explore` in
//! an in-app WebView, let the user sign in normally, then read the
//! `steamLoginSecure` cookie that Steam sets on `.steampowered.com`. The
//! cookie value is `<steamid64>%7C%7C<jwt>` — the JWT is a valid
//! `access_token` for the Steam Web API (same mechanism the official client
//! uses). We persist both `steam_id` and `steam_access_token` in
//! `sync_state` and use the token to call `IPlayerService/GetOwnedGames`.
//!
//! Commands:
//! - `steam_login_start`     — opens the WebView, returns immediately;
//!                              persistence + event emission happen inside
//!                              the `on_navigation` callback once the cookie
//!                              shows up.
//! - `steam_login_finish`    — frontend acks the `steam-login-success`
//!                              event. Reads the already-persisted values
//!                              from `sync_state` and returns the display
//!                              info. Keeps the API surface symmetric with
//!                              `epic_login_finish` / `gog_login_finish`.
//! - `disconnect_steam`      — wipes both `steam_id` and
//!                              `steam_access_token` from sync_state.
//! - `get_steam_connection`  — `connected = true` iff both a steam_id and
//!                              an access_token are stored. Legacy
//!                              OpenID-only installs read as
//!                              disconnected.
//! - `sync_steam_library`    — calls `IPlayerService/GetOwnedGames` with
//!                              the stored token + steamid. On
//!                              `Unauthorized`: clears the token, returns
//!                              `session_expired: true` so the UI can
//!                              prompt re-login.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use serde::Serialize;
use tauri::webview::WebviewWindowBuilder;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::services::db::Database;
use crate::services::steam_collections;
use crate::services::steam_api::{self, SteamApiError};
use crate::services::steam_login;

pub const KEY_STEAM_ID: &str = "steam_id";
pub const KEY_STEAM_TOKEN: &str = "steam_access_token";
/// Optional user-supplied Steam Web API key (https://steamcommunity.com/dev/apikey).
/// Unlocks endpoints like `ISteamUserStats/GetPlayerAchievements` that
/// reject the JWT cookie token. When absent, achievement features
/// silently no-op — the rest of the Steam integration (owned games,
/// playtime, collections) works with the JWT alone.
pub const KEY_STEAM_WEB_KEY: &str = "steam_web_api_key";
const LOGIN_TIMEOUT_SECS: u64 = 300; // 5 min
const COOKIE_POLL_INTERVAL_MS: u64 = 500;

#[derive(Debug, Clone, Serialize)]
pub struct SyncSteamResult {
    pub added: u32,
    pub updated: u32,
    pub total_owned: u32,
    /// True when Steam rejected the stored `access_token` (cookie expired /
    /// password changed). The UI should clear the connection and prompt the
    /// user to reconnect Steam.
    pub session_expired: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SteamConnectionInfo {
    pub steam_id: Option<u64>,
    pub connected: bool,
    pub account_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SteamConnectedInfo {
    pub steam_id: u64,
    pub account_name: Option<String>,
}

/// Start the Steam web-login flow. Opens a WebView at Steam's sign-in page;
/// resolves immediately. After the user signs in, the `on_navigation`
/// callback polls the WebView's cookie jar for `steamLoginSecure`. As soon
/// as we have a JWT and the WebView is past the `/login/` page, we:
///
///   1. Parse the cookie value (`<steamid64>%7C%7C<jwt>` → `(id, jwt)`)
///   2. Persist `steam_id` + `steam_access_token` in `sync_state`
///   3. Emit `steam-login-success` with `{ steam_id, account_name }`
///   4. Close the WebView
///
/// Watchdog: 5-minute timeout or user-closes-window → `steam-login-failure`
/// with `{ message: string }`.
#[tauri::command]
pub async fn steam_login_start(app: AppHandle, _db: State<'_, Database>) -> Result<(), String> {
    // Cleanup any zombie login window left over from a previous failed
    // attempt. Without this, WebviewWindowBuilder::new() trips on the
    // label still being in use ("a webview with label ... already exists").
    // destroy() is synchronous and immediately frees the label, unlike
    // close() which queues an OS close-request that may not unregister
    // the label before our next build() call.
    if let Some(existing) = app.get_webview_window(steam_login::LOGIN_WINDOW_LABEL) {
        let _ = existing.destroy();
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    let login_url = steam_login::LOGIN_URL
        .parse::<tauri::Url>()
        .map_err(|e| format!("Invalid Steam login URL: {}", e))?;

    let resolved = Arc::new(AtomicBool::new(false));

    let resolved_nav = resolved.clone();
    let app_nav = app.clone();

    WebviewWindowBuilder::new(
        &app,
        steam_login::LOGIN_WINDOW_LABEL,
        tauri::WebviewUrl::External(login_url),
    )
    .title(steam_login::LOGIN_WINDOW_TITLE)
    .inner_size(1100.0, 800.0)
    .center()
    .resizable(true)
    // Must match the main window's additionalBrowserArgs in tauri.conf.json.
    // WebView2 shares a single user-data-folder across all WebViews of one
    // app; if popups and main use different `--enable-features=...` strings,
    // the second WebView creation fails with ERROR_INVALID_STATE (0x8007139F).
    .additional_browser_args("--ignore-gpu-blocklist --enable-gpu-rasterization --enable-zero-copy --enable-features=D3D11VideoDecoder")
    .on_navigation(move |url: &tauri::Url| {
        let url_str = url.as_str().to_string();
        log::debug!("[steam] WebView navigating to {}", url_str);

        // Only consider extraction once we're past the login form. Steam
        // bounces through several intermediate pages (captcha, 2FA confirm,
        // etc.) all under `/login/...` — the cookie isn't set until the
        // user lands somewhere else.
        let on_login_page = url.path().starts_with("/login");
        if on_login_page {
            return true;
        }
        if resolved_nav.load(Ordering::SeqCst) {
            return true;
        }

        let app_h = app_nav.clone();
        let resolved_task = resolved_nav.clone();
        tauri::async_runtime::spawn(async move {
            try_extract_cookie(&app_h, &resolved_task).await;
        });

        true
    })
    .build()
    .map_err(|e| format!("Failed to open Steam login window: {}", e))?;

    // Watchdog: timeout / window-closed. Also runs a periodic cookie poll
    // as a backstop — `on_navigation` only fires on actual navigation
    // events, but Steam can occasionally set the cookie via XHR on the
    // same `/login/` page (the user clicks "Sign in" and the URL doesn't
    // change, the cookie just appears). Polling catches that case.
    let app_watch = app.clone();
    let resolved_watch = resolved.clone();
    tauri::async_runtime::spawn(async move {
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(LOGIN_TIMEOUT_SECS);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(COOKIE_POLL_INTERVAL_MS)).await;
            if resolved_watch.load(Ordering::SeqCst) {
                return;
            }
            // Backstop poll. Cheap — same cookies_for_url call the nav
            // handler does.
            try_extract_cookie(&app_watch, &resolved_watch).await;
            if resolved_watch.load(Ordering::SeqCst) {
                return;
            }
            let window_gone = app_watch
                .get_webview_window(steam_login::LOGIN_WINDOW_LABEL)
                .is_none();
            let timed_out = std::time::Instant::now() >= deadline;
            if window_gone || timed_out {
                if resolved_watch.swap(true, Ordering::SeqCst) {
                    return;
                }
                let msg = if timed_out {
                    format!(
                        "Steam sign-in timed out after {} minutes. Please try again.",
                        LOGIN_TIMEOUT_SECS / 60
                    )
                } else {
                    "Steam sign-in cancelled (window closed).".to_string()
                };
                let _ = app_watch.emit(
                    "steam-login-failure",
                    serde_json::json!({ "message": msg }),
                );
                return;
            }
        }
    });

    Ok(())
}

/// Read `steamLoginSecure` from the WebView, parse it, persist, emit
/// success. Idempotent — guarded by the `resolved` atomic so concurrent
/// nav-callback + polling-task firings don't double-emit.
async fn try_extract_cookie(app: &AppHandle, resolved: &Arc<AtomicBool>) {
    let Some(win) = app.get_webview_window(steam_login::LOGIN_WINDOW_LABEL) else {
        return;
    };
    let url: tauri::Url = match steam_login::STEAMPOWERED_URL.parse() {
        Ok(u) => u,
        Err(_) => return,
    };
    let cookies = match win.cookies_for_url(url) {
        Ok(c) => c,
        Err(e) => {
            log::debug!("[steam] cookies_for_url failed: {}", e);
            return;
        }
    };
    let Some(login_cookie) = cookies.iter().find(|c| c.name() == "steamLoginSecure") else {
        return;
    };
    let Some((steam_id_str, jwt)) =
        steam_login::parse_steam_login_secure(login_cookie.value())
    else {
        log::debug!("[steam] steamLoginSecure cookie present but unparseable");
        return;
    };

    // Claim the win — multiple firings race here, only the first wins.
    if resolved.swap(true, Ordering::SeqCst) {
        return;
    }

    let steam_id_u64 = match steam_id_str.parse::<u64>() {
        Ok(v) => v,
        Err(_) => {
            log::warn!(
                "[steam] cookie steamid is not a valid u64: {:?}",
                steam_id_str
            );
            let _ = app.emit(
                "steam-login-failure",
                serde_json::json!({ "message": "Steam returned a malformed session ID." }),
            );
            return;
        }
    };

    log::info!(
        "[steam] cookie extracted: id={}, jwt_len={}",
        steam_login::mask_steam_id(steam_id_u64),
        jwt.len()
    );

    // Persist before emitting so `steam_login_finish` can read sync_state.
    let db_state = app.state::<Database>();
    if let Err(e) = db_state.set_sync_state(KEY_STEAM_ID, &steam_id_str) {
        log::warn!("[steam] failed to persist steam_id: {}", e);
    }
    if let Err(e) = db_state.set_sync_state(KEY_STEAM_TOKEN, &jwt) {
        log::warn!("[steam] failed to persist steam_access_token: {}", e);
    }

    let account_name = steam_login::read_local_username(&steam_id_str);

    let _ = app.emit(
        "steam-login-success",
        serde_json::json!({
            "steam_id": steam_id_u64,
            "account_name": account_name,
        }),
    );

    // Close the WebView a beat after emitting so the frontend listener has
    // a chance to register before the window disappears.
    let app_close = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if let Some(w) = app_close.get_webview_window(steam_login::LOGIN_WINDOW_LABEL) {
            let _ = w.close();
        }
    });
}

/// Ack the `steam-login-success` event. Persistence already happened inside
/// `steam_login_start`'s cookie callback — this command exists so the
/// frontend's `useAccountConnect` hook keeps the same `start → finish`
/// shape as Epic/GOG.
#[tauri::command]
pub async fn steam_login_finish(
    steam_id: u64,
    db: State<'_, Database>,
) -> Result<SteamConnectedInfo, String> {
    // Defensive: make sure the cookie callback wrote the values. If the
    // event somehow arrived before persistence (shouldn't happen — we
    // persist *before* emitting), stamp the steam_id at least.
    let stored_id = db
        .get_sync_state(KEY_STEAM_ID)
        .map_err(|e| e.to_string())?
        .filter(|s| !s.is_empty());
    if stored_id.is_none() {
        db.set_sync_state(KEY_STEAM_ID, &steam_id.to_string())
            .map_err(|e| e.to_string())?;
    }

    let account_name = steam_login::read_local_username(&steam_id.to_string());
    Ok(SteamConnectedInfo {
        steam_id,
        account_name,
    })
}

/// Disconnect Steam — clears both the stored SteamID and the access token.
/// Imported games stay (the user can prune them manually from the Library).
#[tauri::command]
pub async fn disconnect_steam(db: State<'_, Database>) -> Result<(), String> {
    db.set_sync_state(KEY_STEAM_ID, "").map_err(|e| e.to_string())?;
    db.set_sync_state(KEY_STEAM_TOKEN, "")
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Get the current Steam connection state for the Settings UI. `connected`
/// requires *both* a SteamID and an access_token — legacy installs that
/// only have a steam_id (from the old OpenID flow) read as disconnected.
#[tauri::command]
pub async fn get_steam_connection(
    db: State<'_, Database>,
) -> Result<SteamConnectionInfo, String> {
    let raw_id = db.get_sync_state(KEY_STEAM_ID).map_err(|e| e.to_string())?;
    let token = db
        .get_sync_state(KEY_STEAM_TOKEN)
        .map_err(|e| e.to_string())?
        .filter(|s| !s.is_empty());
    let steam_id = raw_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<u64>().ok());
    let connected = steam_id.is_some() && token.is_some();
    let account_name = steam_id
        .map(|id| id.to_string())
        .as_deref()
        .and_then(steam_login::read_local_username);
    Ok(SteamConnectionInfo {
        connected,
        steam_id,
        account_name,
    })
}

/// Sync the owned Steam library into the games table via
/// `IPlayerService/GetOwnedGames`. On `Unauthorized`: clears the stored
/// token and returns `session_expired: true` (counters zeroed) so the UI
/// can prompt re-login.
#[tauri::command]
pub async fn sync_steam_library(db: State<'_, Database>) -> Result<SyncSteamResult, String> {
    let steam_id_str = db
        .get_sync_state(KEY_STEAM_ID)
        .map_err(|e| e.to_string())?
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "Steam is not connected. Click Connect Steam in Settings first.".to_string()
        })?;
    let access_token = db
        .get_sync_state(KEY_STEAM_TOKEN)
        .map_err(|e| e.to_string())?
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "Steam session missing — please reconnect Steam in Settings.".to_string()
        })?;

    let owned = match steam_api::fetch_owned_games(&access_token, &steam_id_str).await {
        Ok(list) => list,
        Err(SteamApiError::Unauthorized) => {
            // Wipe the dead token so `get_steam_connection` flips to
            // disconnected immediately and the tile re-renders with the
            // Connect button.
            let _ = db.set_sync_state(KEY_STEAM_TOKEN, "");
            return Ok(SyncSteamResult {
                added: 0,
                updated: 0,
                total_owned: 0,
                session_expired: true,
            });
        }
        Err(e) => return Err(e.to_string()),
    };

    // Steam Family games — apps another family member shares with this user.
    // Treated like owned games in the DB so the existing Install button +
    // steam://install URI work without changes (Steam handles family-share
    // authorization at install time).
    let family = match steam_api::fetch_family_games(&access_token, &steam_id_str).await {
        Ok(list) => list,
        Err(SteamApiError::Unauthorized) => {
            // Same handling as owned: clear token, flag session_expired.
            let _ = db.set_sync_state(KEY_STEAM_TOKEN, "");
            return Ok(SyncSteamResult {
                added: 0,
                updated: 0,
                total_owned: 0,
                session_expired: true,
            });
        }
        Err(e) => {
            // Don't fail the whole sync if family lookup is flaky — log and
            // continue with owned games only.
            log::warn!("[steam] family library sync failed: {} — continuing with owned only", e);
            Vec::new()
        }
    };

    // Merge with owned. If an appid is both owned and shared, owned wins.
    use std::collections::HashSet;
    let owned_appids: HashSet<u64> = owned.iter().map(|g| g.appid).collect();
    let mut combined: Vec<&steam_api::SteamOwnedGame> = owned.iter().collect();
    for fg in &family {
        if !owned_appids.contains(&fg.appid) {
            combined.push(fg);
        }
    }
    let total_owned = combined.len() as u32;

    let mut added = 0u32;
    let mut updated = 0u32;

    for og in combined {
        let Some(game) = steam_api::steam_owned_to_game(og) else {
            continue;
        };
        let exists = game
            .platform_id
            .as_deref()
            .and_then(|appid| db.steam_game_exists_by_appid(appid).ok())
            .unwrap_or(false);

        if db.upsert_game_from_api(&game).is_ok() {
            if exists {
                updated += 1;
            } else {
                added += 1;
            }
            // Mirror Steam's `playtime_forever` (minutes) into the games
            // row so the Stats page surfaces playtime accumulated in Steam
            // itself, not just the Tokoru-tracked sessions. Family-
            // shared games get whatever playtime the API reports for THIS
            // user against them (usually 0 if you've never played).
            if let (Some(pid), Some(minutes)) =
                (game.platform_id.as_deref(), og.playtime_forever)
            {
                let _ = db.set_imported_playtime_by_platform(
                    "steam",
                    pid,
                    (minutes as i64).saturating_mul(60),
                );
            }
            // Mirror Steam's `rtime_last_played` into the games row so
            // the Library "Recently Played" filter picks up titles the
            // user opened directly on Steam (no local watcher session
            // needed). Steam reports 0 for never-played, which the DB
            // helper treats as "no-op".
            if let (Some(pid), Some(last)) =
                (game.platform_id.as_deref(), og.rtime_last_played)
            {
                let _ = db.set_imported_last_played_by_platform("steam", pid, last);
            }
        }
    }

    let _ = db.set_sync_state(
        "last_sync_at_steam",
        &chrono::Utc::now().timestamp().to_string(),
    );

    // Best-effort import of the Steam-side "Favorites" collection into
    // Tokoru's local favorite flag. Runs after the library upserts
    // so newly-added rows can be matched in the same pass. Silent fail:
    // missing collection / Steam closed / parse error are NOT propagated
    // — the library sync result is what the user actually clicked for.
    if let Some(user_id) = steam_collections::current_steam_user_id() {
        match steam_collections::read_user_collections(&user_id) {
            Ok(all) => {
                const ALIASES: &[&str] = &[
                    "favorite", "favorites", "favoris", "favori", "fav",
                ];
                let picked = all.iter().find(|c| {
                    let lower = c.name.to_lowercase();
                    ALIASES.iter().any(|a| lower.contains(a))
                });
                if let Some(c) = picked {
                    let mut imported = 0usize;
                    for appid in &c.appids {
                        if let Ok(Some(game_id)) =
                            db.find_steam_id_by_appid(&appid.to_string())
                        {
                            let _ = db.set_favorite(&game_id, true);
                            imported += 1;
                        }
                    }
                    log::info!(
                        "[steam_sync] imported {}/{} favorites from Steam collection '{}'",
                        imported,
                        c.appids.len(),
                        c.name
                    );
                }
            }
            Err(e) => {
                log::debug!(
                    "[steam_sync] favorites read skipped: {}",
                    e
                );
            }
        }
    }

    Ok(SyncSteamResult {
        added,
        updated,
        total_owned,
        session_expired: false,
    })
}
