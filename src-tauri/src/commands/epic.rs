//! Tauri commands wiring the Epic OAuth flow + library sync into the frontend.
//!
//! `epic_login_start` opens an in-app WebView pointed at Epic's login page.
//! When Epic redirects to its internal `/id/api/redirect` endpoint, we run a
//! tiny script that JSON-parses the response body, extracts the
//! `authorizationCode`, then navigates the WebView to a sentinel URL we own
//! and intercept here. The captured code is emitted on the
//! `epic-login-success` event channel.
//!
//! The frontend listens for that event, then calls `epic_login_finish` with
//! the code — we exchange it for tokens, persist the account name in
//! `sync_state`, fetch the owned library and upsert into the games table.

use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri::webview::WebviewWindowBuilder;

use crate::services::cli_bins;
use crate::services::db::Database;
use crate::services::epic_api;

const KEY_EPIC_NAME: &str = "epic_account_name";
const KEY_EPIC_TOKEN: &str = "epic_access_token";

#[derive(Debug, Clone, Serialize)]
pub struct EpicConnectionInfo {
    pub connected: bool,
    pub account_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EpicConnectedInfo {
    pub account_name: String,
    pub games_added: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct EpicSyncResult {
    pub added: u32,
    pub updated: u32,
    pub total_owned: u32,
}

/// Open Epic's login page in a WebView and start listening for the redirect
/// that carries the OAuth `authorizationCode`. Resolves immediately — the
/// frontend awaits the `epic-login-success` event for the captured code.
///
/// First call may take a few seconds: we make sure `legendary.exe` is on
/// disk before opening the window, so that step happens up-front rather than
/// after the user has typed their password.
#[tauri::command]
pub async fn epic_login_start(app: AppHandle) -> Result<(), String> {
    // Cleanup any zombie login window left over from a previous failed
    // attempt — see steam_login_start for why destroy() (not close()).
    if let Some(existing) = app.get_webview_window("epic-login") {
        let _ = existing.destroy();
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    // Pre-fetch the CLI so we don't block the post-auth path. We deliberately
    // don't wait inside epic_login_finish — saves several seconds of UI lag
    // after the user comes back from Epic.
    if let Err(e) = cli_bins::ensure_legendary(&app).await {
        log::warn!("[epic] pre-fetch of legendary failed: {}", e);
        // Fall through — we'll try again in login_finish and surface the
        // error there if it still fails. Login window is the higher-value
        // UX step here.
    }

    let login_url = epic_api::get_login_url();
    let extracted = Arc::new(AtomicBool::new(false));
    let app_handle = app.clone();
    let extracted_clone = extracted.clone();

    let url = login_url
        .parse::<tauri::Url>()
        .map_err(|e| format!("Invalid Epic login URL: {}", e))?;

    WebviewWindowBuilder::new(&app, "epic-login", tauri::WebviewUrl::External(url))
        .title("Epic Games — Sign in")
        // Match main window args — see steam_login_start comment for why.
        .additional_browser_args("--ignore-gpu-blocklist --enable-gpu-rasterization --enable-zero-copy --enable-features=D3D11VideoDecoder")
        .inner_size(640.0, 720.0)
        .center()
        .resizable(true)
        .on_navigation(move |url: &tauri::Url| {
            let url_str = url.as_str();

            // Step 1 — Epic's /id/api/redirect returns a JSON body in the
            // browser instead of doing a real HTTP redirect. We need to read
            // the page text via JS, pull authorizationCode out, and bounce to
            // a sentinel URL we can intercept below.
            if url_str.contains("/id/api/redirect")
                && url_str.contains("responseType=code")
                && !extracted_clone.load(Ordering::Relaxed)
            {
                let app_h: AppHandle = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                    if let Some(win) = app_h.get_webview_window("epic-login") {
                        // The script must avoid Tauri-injected globals — we
                        // just read document body innerText as plain text.
                        let _ = win.eval(
                            r#"
                            try {
                                const d = JSON.parse(document.body.innerText);
                                if (d.authorizationCode) {
                                    window.location.href =
                                        'https://steamshelf-epic-auth.internal/callback?code='
                                        + d.authorizationCode;
                                }
                            } catch (e) {}
                            "#,
                        );
                    }
                });
                return true;
            }

            // Step 2 — Sentinel callback. Extract the code, emit, close.
            if url_str.contains("steamshelf-epic-auth.internal/callback")
                && !extracted_clone.load(Ordering::Relaxed)
            {
                if let Some(code) = url
                    .query_pairs()
                    .find(|(k, _)| k == "code")
                    .map(|(_, v)| v.to_string())
                {
                    extracted_clone.store(true, Ordering::Relaxed);
                    log::info!("[epic] auth code captured from page body");
                    let _ = app_handle.emit(
                        "epic-login-success",
                        serde_json::json!({ "auth_code": code }),
                    );
                    let app_h: AppHandle = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        if let Some(win) = app_h.get_webview_window("epic-login") {
                            let _ = win.close();
                        }
                    });
                }
                return false; // don't try to actually load the sentinel URL
            }

            // Fallback path — some Epic flows do a regular redirect with `?code=`.
            if url_str.contains("code=") && !extracted_clone.load(Ordering::Relaxed) {
                if let Some(code) = url
                    .query_pairs()
                    .find(|(k, _)| k == "code")
                    .map(|(_, v)| v.to_string())
                {
                    extracted_clone.store(true, Ordering::Relaxed);
                    log::info!("[epic] auth code captured from redirect URL");
                    let _ = app_handle.emit(
                        "epic-login-success",
                        serde_json::json!({ "auth_code": code }),
                    );
                    let app_h: AppHandle = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                        if let Some(win) = app_h.get_webview_window("epic-login") {
                            let _ = win.close();
                        }
                    });
                }
            }

            true
        })
        .build()
        .map_err(|e| format!("Failed to open Epic login window: {}", e))?;

    Ok(())
}

/// Complete the Epic login by exchanging the captured code for an access
/// token, fetching the owned library, and persisting display name + token in
/// sync_state.
#[tauri::command]
pub async fn epic_login_finish(
    auth_code: String,
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<EpicConnectedInfo, String> {
    let account = epic_api::login_with_code(&app, &auth_code).await?;

    // Persist the lightweight "connected" state — the Source tile badge reads
    // KEY_EPIC_TOKEN. Library fetch is intentionally NOT done here: it takes
    // 30s-1.5min and blocks `onSuccess` (badge stays on "Connect" the whole
    // time). Frontend should call `epic_sync_library` right after this returns
    // — same pattern Steam uses (login_finish is a fast ack, sync is separate).
    db.set_sync_state(KEY_EPIC_NAME, &account.display_name)
        .map_err(|e| e.to_string())?;
    db.set_sync_state(KEY_EPIC_TOKEN, &account.access_token)
        .map_err(|e| e.to_string())?;

    Ok(EpicConnectedInfo {
        account_name: account.display_name,
        games_added: 0,
    })
}

/// Log out from Epic — wipes the legendary session and clears sync_state.
#[tauri::command]
pub async fn epic_logout(app: AppHandle, db: State<'_, Database>) -> Result<(), String> {
    epic_api::logout(&app).await.ok();
    db.set_sync_state(KEY_EPIC_NAME, "")
        .map_err(|e| e.to_string())?;
    db.set_sync_state(KEY_EPIC_TOKEN, "")
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Connection state for the Settings UI. "connected" is sourced from
/// sync_state so the answer survives across Tokoru restarts without
/// needing to shell out to legendary every time the page renders.
#[tauri::command]
pub async fn epic_get_connection(
    db: State<'_, Database>,
) -> Result<EpicConnectionInfo, String> {
    let name = db
        .get_sync_state(KEY_EPIC_NAME)
        .map_err(|e| e.to_string())?
        .filter(|s| !s.is_empty());
    Ok(EpicConnectionInfo {
        connected: name.is_some(),
        account_name: name,
    })
}

/// Re-sync the Epic library on demand. Uses the stored access token first; if
/// that's missing or rejected, surfaces a clear "please reconnect" error.
///
/// After the upsert pass, runs `detect_installs` so any games already on disk
/// (installed via the Epic launcher outside Tokoru, or moved manually)
/// pick up their install_path immediately without forcing the user to click a
/// separate button. detect_installs errors don't fail the sync — the owned
/// list is the important part.
#[tauri::command]
pub async fn epic_sync_library(
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<EpicSyncResult, String> {
    let token = db
        .get_sync_state(KEY_EPIC_TOKEN)
        .map_err(|e| e.to_string())?
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "Epic is not connected. Connect Epic in Settings first.".to_string()
        })?;

    // Tile shows "Fetching library…" during the network call (the long part —
    // owned-list pull is 1-5s). total=0 → frontend renders the indeterminate
    // bar + "Fetching library…" until the real total arrives below.
    let _ = app.emit(
        "library-sync-progress",
        serde_json::json!({
            "source": "epic",
            "processed": 0u32,
            "total": 0u32,
            "title": "Fetching library…",
            "done": false,
        }),
    );

    let games = epic_api::fetch_owned_games(&token).await?;
    let total = games.len() as u32;

    // Kick off the upsert phase with a real total so the user sees "0 / 353"
    // before the loop starts. The loop is fast (SQLite inserts) so we emit
    // every game to give React enough setState calls that intermediate frames
    // actually render — every-5 was too aggressive (50ms total → 1 frame).
    let _ = app.emit(
        "library-sync-progress",
        serde_json::json!({
            "source": "epic",
            "processed": 0u32,
            "total": total,
            "title": "",
            "done": false,
        }),
    );

    let mut added = 0u32;
    let mut updated = 0u32;
    for (idx, g) in games.iter().enumerate() {
        match db.game_exists_by_path(&g.exe_path) {
            Ok(true) => {
                let _ = db.upsert_game(g);
                updated += 1;
            }
            Ok(false) => {
                let _ = db.upsert_game(g);
                added += 1;
            }
            Err(e) => log::warn!("[epic sync] exists check failed: {}", e),
        }
        let processed = (idx as u32) + 1;
        // Emit on every game so the title scrolls visibly. The upsert loop
        // is fast (~50ms total) — gating by % would compress the whole sync
        // into a single render frame.
        let _ = app.emit(
            "library-sync-progress",
            serde_json::json!({
                "source": "epic",
                "processed": processed,
                "total": total,
                "title": g.title.clone(),
                "done": false,
            }),
        );
        // Tiny yield so React gets a paint window between events; otherwise
        // tokio runs the whole loop in one tick and only the final state
        // makes it onscreen.
        tokio::time::sleep(std::time::Duration::from_millis(8)).await;
    }
    // Stamp the per-source last_sync so the Source tile UI can show "Last
    // synced X minutes ago" without re-running legendary.
    let _ = db.set_sync_state(
        "last_sync_at_epic",
        &chrono::Utc::now().timestamp().to_string(),
    );

    // Best-effort install-status reconciliation. Bonus over the owned list:
    // mark rows as installed if legendary already had them.
    let app_for_done = app.clone();
    if let Err(e) =
        crate::commands::downloads::detect_installs(app, db).await
    {
        log::warn!("[epic sync] detect_installs failed: {} (continuing)", e);
    }

    // Signal the tile that sync is over — flips SCANNING off.
    let _ = app_for_done.emit(
        "library-sync-progress",
        serde_json::json!({
            "source": "epic",
            "processed": total,
            "total": total,
            "title": "",
            "done": true,
        }),
    );

    Ok(EpicSyncResult {
        added,
        updated,
        total_owned: total,
    })
}
