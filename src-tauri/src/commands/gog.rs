//! Tauri commands wiring the GOG OAuth flow + library sync into the frontend.
//! Same shape as `commands::epic` but for the GOG Galaxy public client.

use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri::webview::WebviewWindowBuilder;

use crate::services::cli_bins;
use crate::services::db::Database;
use crate::services::gog_api;

const KEY_GOG_NAME: &str = "gog_account_name";
const KEY_GOG_TOKEN: &str = "gog_access_token";

#[derive(Debug, Clone, Serialize)]
pub struct GogConnectionInfo {
    pub connected: bool,
    pub account_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GogConnectedInfo {
    pub account_name: String,
    pub games_added: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct GogSyncResult {
    pub added: u32,
    pub updated: u32,
    pub total_owned: u32,
}

#[tauri::command]
pub async fn gog_login_start(app: AppHandle) -> Result<(), String> {
    // Cleanup zombie login window from a previous failed attempt — see
    // steam_login_start for why destroy() (not close()).
    if let Some(existing) = app.get_webview_window("gog-login") {
        let _ = existing.destroy();
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    // Pre-fetch gogdl in parallel so the post-auth path is fast.
    if let Err(e) = cli_bins::ensure_gogdl(&app).await {
        log::warn!("[gog] pre-fetch of gogdl failed: {}", e);
    }

    let login_url = gog_api::get_login_url();
    let extracted = Arc::new(AtomicBool::new(false));
    let app_handle = app.clone();
    let extracted_clone = extracted.clone();

    let url = login_url
        .parse::<tauri::Url>()
        .map_err(|e| format!("Invalid GOG login URL: {}", e))?;

    WebviewWindowBuilder::new(&app, "gog-login", tauri::WebviewUrl::External(url))
        .title("GOG — Sign in")
        // Match main window args — see steam_login_start comment for why.
        .additional_browser_args("--ignore-gpu-blocklist --enable-gpu-rasterization --enable-zero-copy --enable-features=D3D11VideoDecoder")
        .inner_size(640.0, 720.0)
        .center()
        .resizable(true)
        .on_navigation(move |url: &tauri::Url| {
            let url_str = url.as_str();

            // GOG redirects to https://embed.gog.com/on_login_success?...&code=<auth_code>
            if url_str.contains("on_login_success")
                && url_str.contains("code=")
                && !extracted_clone.load(Ordering::Relaxed)
            {
                if let Some(code) = url
                    .query_pairs()
                    .find(|(k, _)| k == "code")
                    .map(|(_, v)| v.to_string())
                {
                    extracted_clone.store(true, Ordering::Relaxed);
                    log::info!("[gog] auth code captured from redirect");
                    let _ = app_handle.emit(
                        "gog-login-success",
                        serde_json::json!({ "auth_code": code }),
                    );
                    let app_h: AppHandle = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                        if let Some(win) = app_h.get_webview_window("gog-login") {
                            let _ = win.close();
                        }
                    });
                }
            }

            true
        })
        .build()
        .map_err(|e| format!("Failed to open GOG login window: {}", e))?;

    Ok(())
}

#[tauri::command]
pub async fn gog_login_finish(
    auth_code: String,
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<GogConnectedInfo, String> {
    let account = gog_api::login_with_code(&app, &auth_code).await?;

    // Persist token only — library fetch is split into `gog_sync_library` so
    // the Source tile badge flips to "Connected" immediately. See the same
    // comment in `epic_login_finish` for the rationale.
    db.set_sync_state(KEY_GOG_NAME, &account.username)
        .map_err(|e| e.to_string())?;
    db.set_sync_state(KEY_GOG_TOKEN, &account.access_token)
        .map_err(|e| e.to_string())?;

    Ok(GogConnectedInfo {
        account_name: account.username,
        games_added: 0,
    })
}

#[tauri::command]
pub async fn gog_logout(app: AppHandle, db: State<'_, Database>) -> Result<(), String> {
    gog_api::logout(&app).await.ok();
    db.set_sync_state(KEY_GOG_NAME, "")
        .map_err(|e| e.to_string())?;
    db.set_sync_state(KEY_GOG_TOKEN, "")
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn gog_get_connection(
    db: State<'_, Database>,
) -> Result<GogConnectionInfo, String> {
    let name = db
        .get_sync_state(KEY_GOG_NAME)
        .map_err(|e| e.to_string())?
        .filter(|s| !s.is_empty());
    Ok(GogConnectionInfo {
        connected: name.is_some(),
        account_name: name,
    })
}

#[tauri::command]
pub async fn gog_sync_library(
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<GogSyncResult, String> {
    let stored = db
        .get_sync_state(KEY_GOG_TOKEN)
        .map_err(|e| e.to_string())?
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "GOG is not connected. Connect GOG in Settings first.".to_string()
        })?;

    // Tile shows "Fetching library…" during the slow GOG network call
    // (owned-list resolution can be 1-2 min for big libraries). total=0
    // triggers the indeterminate progress bar in the UI until we know the
    // real count below.
    let _ = app.emit(
        "library-sync-progress",
        serde_json::json!({
            "source": "gog",
            "processed": 0u32,
            "total": 0u32,
            "title": "Fetching library…",
            "done": false,
        }),
    );

    // Try the stored token first. If it 401s we refresh via gogdl auth.json
    // and retry once before bubbling up. This keeps the happy path one
    // request long.
    let mut token = stored.clone();
    let games = match gog_api::fetch_owned_games(&token).await {
        Ok(g) => g,
        Err(e) if e.contains("expired") => {
            log::info!("[gog sync] token expired, refreshing");
            token = gog_api::refreshed_access_token(&app).await?;
            db.set_sync_state(KEY_GOG_TOKEN, &token)
                .map_err(|e| e.to_string())?;
            gog_api::fetch_owned_games(&token).await?
        }
        Err(e) => return Err(e),
    };

    let total = games.len() as u32;

    // Real total after fetch → tile flips from "Fetching library…" (emitted
    // upstream of the fetch, see below) to "0 / 96".
    let _ = app.emit(
        "library-sync-progress",
        serde_json::json!({
            "source": "gog",
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
            Err(e) => log::warn!("[gog sync] exists check failed: {}", e),
        }
        let processed = (idx as u32) + 1;
        // See epic_sync_library: emit every game + tiny yield so React gets
        // a paint window between updates and the user actually sees titles
        // scrolling past.
        let _ = app.emit(
            "library-sync-progress",
            serde_json::json!({
                "source": "gog",
                "processed": processed,
                "total": total,
                "title": g.title.clone(),
                "done": false,
            }),
        );
        tokio::time::sleep(std::time::Duration::from_millis(8)).await;
    }
    // Import playtime. Two paths in priority order:
    //   1. GOG Galaxy local DB (`galaxy-2.0.db`) when the user runs Galaxy.
    //   2. `gameplay.gog.com` API per game (fallback for users who never
    //      installed Galaxy — Tokoru is meant to *replace* the 400
    //      launchers, so most users will fall here).
    let mut imported = 0u32;
    let galaxy_entries = gog_api::read_galaxy_playtimes().unwrap_or_default();
    let mut from_galaxy = std::collections::HashSet::<String>::new();
    for (source, platform_id, seconds) in galaxy_entries {
        if source != "gog" {
            continue;
        }
        if let Ok(true) = db.set_imported_playtime_by_platform(&source, &platform_id, seconds) {
            imported += 1;
            from_galaxy.insert(platform_id);
        }
    }

    // API fallback for games Galaxy didn't cover (or for users with no
    // Galaxy at all). Best-effort: any per-call failure is silently
    // skipped inside `fetch_playtimes_api`.
    let api_game_ids: Vec<u64> = games
        .iter()
        .filter_map(|g| g.platform_id.as_deref().and_then(|s| s.parse::<u64>().ok()))
        .filter(|id| !from_galaxy.contains(&id.to_string()))
        .collect();
    if !api_game_ids.is_empty() {
        match gog_api::fetch_user_id(&token).await {
            Ok(user_id) if !user_id.is_empty() => {
                match gog_api::fetch_playtimes_api(&token, &user_id, &api_game_ids).await {
                    Ok(entries) => {
                        for (platform_id, seconds) in entries {
                            if let Ok(true) = db.set_imported_playtime_by_platform(
                                "gog",
                                &platform_id,
                                seconds,
                            ) {
                                imported += 1;
                            }
                        }
                    }
                    Err(e) => log::warn!("[gog sync] API playtime fetch failed: {}", e),
                }
            }
            Ok(_) => log::warn!("[gog sync] empty user_id — skipping API playtime"),
            Err(e) => log::warn!("[gog sync] user_id resolution failed: {}", e),
        }
    }
    log::info!("[gog sync] imported playtime for {} games (galaxy+api)", imported);

    // Stamp the per-source last_sync so the Source tile UI can show "Last
    // synced X minutes ago" without re-running gogdl.
    let _ = db.set_sync_state(
        "last_sync_at_gog",
        &chrono::Utc::now().timestamp().to_string(),
    );

    // Best-effort install-status reconciliation. Picks up games the user
    // installed directly through GOG Galaxy (or moved manually) so the
    // owned-list refresh + on-disk status converge in one round-trip.
    let app_for_done = app.clone();
    if let Err(e) =
        crate::commands::downloads::detect_installs(app, db).await
    {
        log::warn!("[gog sync] detect_installs failed: {} (continuing)", e);
    }

    // Done — tile flips SCANNING off and clears the X/Y counter.
    let _ = app_for_done.emit(
        "library-sync-progress",
        serde_json::json!({
            "source": "gog",
            "processed": total,
            "total": total,
            "title": "",
            "done": true,
        }),
    );

    Ok(GogSyncResult {
        added,
        updated,
        total_owned: total,
    })
}
