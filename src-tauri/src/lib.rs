mod commands;
mod models;
mod services;

use services::db::Database;
use services::downloads as dl_service;
use services::runtime_state::RuntimeState;
use services::steam_cdp_watcher;
use services::steam_dl_watcher;
use services::steam_log_tailer;
use services::watcher;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            let app_data_dir = app
                .path()
                .app_data_dir()
                .expect("Failed to get app data directory");

            // Migration: pre-rename builds wrote to
            // `%APPDATA%/com.startrad.steamshelf`. If that legacy dir
            // exists and the new one doesn't, rename it so the user keeps
            // their DB, settings, downloaded CLIs, credentials, etc.
            if let Some(parent) = app_data_dir.parent() {
                let legacy = parent.join("com.startrad.steamshelf");
                if legacy.exists() && !app_data_dir.exists() {
                    if let Err(e) = std::fs::rename(&legacy, &app_data_dir) {
                        log::warn!(
                            "Failed to migrate legacy SteamShelf data dir {} -> {}: {} (continuing with fresh dir)",
                            legacy.display(),
                            app_data_dir.display(),
                            e
                        );
                    } else {
                        log::info!(
                            "Migrated legacy SteamShelf data dir -> Tokoru ({})",
                            app_data_dir.display()
                        );
                    }
                }
            }

            let db = Database::new(app_data_dir.clone())
                .expect("Failed to initialize Tokoru database");

            // Download runtime — in-memory map of active downloads, mirrored to
            // pending_downloads.json so app restarts can recover.
            let runtime = RuntimeState::new();
            dl_service::hydrate_from_disk(app.handle(), &runtime);

            // Background tasks share the same connection handle via Database::clone.
            // Both the process watcher and the localconfig sync loop live for the
            // lifetime of the app.
            let watcher_db = db.clone();
            let steam_dl_db = db.clone();
            let steam_dl_runtime = runtime.clone();
            let steam_dl_app = app.handle().clone();
            let steam_log_db = db.clone();
            let steam_log_runtime = runtime.clone();
            let steam_log_app = app.handle().clone();
            let steam_cdp_db = db.clone();
            let steam_cdp_runtime = runtime.clone();
            let steam_cdp_app = app.handle().clone();
            app.manage(db);
            app.manage(runtime);
            watcher::start_background_tasks(watcher_db);
            // Steam download progress watcher — mirrors appmanifest_*.acf
            // state into RuntimeState so the UI sees Steam downloads the
            // same way it sees Epic/GOG downloads.
            steam_dl_watcher::start(steam_dl_app, steam_dl_db, steam_dl_runtime);
            // Real-time refinement: tails Steam's content_log.txt for
            // per-second byte / percent / speed updates that don't wait on
            // the manifest watcher's once-per-minute flush.
            steam_log_tailer::start(steam_log_app, steam_log_db, steam_log_runtime);
            // True real-time progress: connects to Steam's CEF DevTools
            // Protocol and subscribes to the same SteamClient.Downloads JS
            // API that paints Steam's own UI. Authoritative for status
            // when connected; falls back to the manifest + log watchers
            // when Steam isn't running with debug enabled.
            steam_cdp_watcher::start(steam_cdp_app, steam_cdp_db, steam_cdp_runtime);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Games CRUD + artwork
            commands::games::get_all_games,
            commands::games::get_game,
            commands::games::add_game,
            commands::games::delete_game,
            commands::games::set_game_custom_title,
            commands::games::set_game_user_tags,
            commands::games::get_game_user_tags,
            commands::games::set_game_manual_playtime_hours,
            commands::games::import_starcitizen_playtime,
            commands::games::toggle_game_favorite,
            commands::games::get_game_favorite,
            commands::games::list_favorite_game_ids,
            commands::games::list_recently_played_ids,
            commands::games::list_never_played_ids,
            commands::games::list_game_play_stats,
            commands::games::import_steam_favorites,
            commands::games::push_favorites_to_steam,
            commands::games::fetch_artwork,
            commands::games::fetch_all_artwork,
            commands::games::is_artwork_initial_backfill_done,
            commands::games::mirror_existing_artwork_to_steam,
            commands::games::browse_covers,
            commands::games::browse_heroes,
            commands::games::browse_logos,
            commands::games::browse_icons,
            commands::games::set_game_cover,
            commands::games::set_game_hero,
            commands::games::set_game_logo,
            commands::games::set_game_icon,
            // SteamGridDB user-facing settings (API key / style / auto-fetch)
            commands::steamgriddb::get_steamgriddb_settings,
            commands::steamgriddb::set_steamgriddb_settings,
            // UI locale persistence (used by Steam Store + GOG fetchers for `l=` / `Accept-Language`)
            commands::locale::set_app_locale,
            commands::locale::get_app_locale,
            commands::locale::is_onboarding_done,
            commands::locale::mark_onboarding_done,
            commands::locale::reset_onboarding_done,
            // Scanning
            commands::scan::detect_installed_counts_per_source,
            commands::scan::scan_local_games,
            commands::scan::scan_custom_directory,
            commands::scan::detect_platform_games,
            commands::scan::full_scan,
            commands::scan::scan_source,
            // Consolidated source tile state (batched read for SourceTile UI)
            commands::sources::get_all_source_states,
            // Steam OpenID + public XML library import (no API key)
            commands::steam::steam_login_start,
            commands::steam::steam_login_finish,
            commands::steam::disconnect_steam,
            commands::steam::get_steam_connection,
            commands::steam::sync_steam_library,
            // Epic Games — browser OAuth via legendary, no API key
            commands::epic::epic_login_start,
            commands::epic::epic_login_finish,
            commands::epic::epic_logout,
            commands::epic::epic_get_connection,
            commands::epic::epic_sync_library,
            // GOG — browser OAuth via gogdl, no API key
            commands::gog::gog_login_start,
            commands::gog::gog_login_finish,
            commands::gog::gog_logout,
            commands::gog::gog_get_connection,
            commands::gog::gog_sync_library,
            // Steam shortcuts + localconfig sync
            commands::shortcuts::push_to_steam,
            commands::shortcuts::remove_from_steam,
            commands::shortcuts::get_shortcut,
            commands::shortcuts::get_all_shortcuts,
            commands::shortcuts::sync_playtime_now,
            commands::shortcuts::sync_collections_now,
            commands::shortcuts::get_collections_mode,
            commands::shortcuts::set_collections_mode,
            commands::shortcuts::get_last_sync_at,
            commands::shortcuts::restart_steam,
            // Metadata enrichment (Steam Store + SteamSpy + IGDB + HLTB + Wikidata)
            commands::metadata::sync_metadata_now,
            commands::metadata::sync_metadata_one,
            commands::metadata::get_metadata_status,
            commands::metadata::get_game_metadata,
            commands::metadata::get_library_tag_index,
            commands::achievements::get_game_achievements,
            commands::achievements::sync_game_achievements,
            commands::achievements::get_global_achievements_stats,
            commands::metadata::get_igdb_credentials,
            commands::metadata::set_igdb_credentials,
            // Stats
            commands::stats::get_playtime_summary,
            commands::stats::get_playtime_heatmap,
            commands::stats::get_top_played,
            commands::stats::get_global_stats,
            commands::stats::get_sessions_over_time,
            commands::stats::get_untouched_games,
            // Downloads — Epic via legendary, GOG via gogdl
            commands::downloads::start_download,
            commands::downloads::pause_download,
            commands::downloads::resume_download,
            commands::downloads::cancel_download,
            commands::downloads::retry_download,
            commands::downloads::dismiss_download,
            commands::downloads::get_download_state,
            commands::downloads::get_all_downloads,
            commands::downloads::get_download_settings,
            commands::downloads::set_download_settings,
            commands::downloads::uninstall_game,
            commands::downloads::get_cli_bins_status,
            commands::downloads::redownload_legendary,
            commands::downloads::redownload_gogdl,
            commands::downloads::detect_installs,
            // System
            commands::system::quit_app,
            commands::system::get_steam_path,
            commands::system::launch_uri,
            commands::system::open_install_folder,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
