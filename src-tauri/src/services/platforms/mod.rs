pub mod local_detect;

use crate::models::Game;

/// Detect locally installed games from every supported platform (registry,
/// manifests, files).
///
/// Tokoru scope:
/// - Steam: detected **both** locally (via `libraryfolders.vdf` + ACF
///   manifests) AND through the Steam Web API (`services::steam_api`). The
///   local pass gives us a real `install_path` / `exe_path` so the playtime
///   watcher can observe the actual process; the API pass gives us the full
///   owned library (installed + uninstalled) with canonical metadata. The two
///   converge on a single row per appid — merging happens at upsert time in
///   `db::upsert_game_from_local` / `db::upsert_game_from_api`, both keyed by
///   `(source='steam', platform_id=<appid>)`.
/// - Everyone else (Epic, GOG, Ubi, EA, Itch, Xbox, Amazon, Heroic, ...) is
///   detected locally because they expose enough installed-state on disk.
pub fn detect_all_installed_games() -> Vec<Game> {
    let mut all = Vec::new();

    log::info!("Detecting installed Steam games...");
    all.extend(local_detect::detect_steam_installed());

    log::info!("Detecting installed Epic games...");
    all.extend(local_detect::detect_epic_installed());

    log::info!("Detecting installed GOG games...");
    all.extend(local_detect::detect_gog_installed());

    log::info!("Detecting installed Ubisoft games...");
    all.extend(local_detect::detect_ubisoft_installed());

    log::info!("Detecting installed EA games...");
    all.extend(local_detect::detect_ea_installed());

    log::info!("Detecting installed Xbox games...");
    all.extend(local_detect::detect_xbox_installed());

    log::info!("Detecting installed Amazon games...");
    all.extend(local_detect::detect_amazon_installed());

    log::info!("Found {} installed platform games total", all.len());
    all
}

/// Detect installed games for ONE specific source, used by the per-source
/// Rescan button so the toast count actually reflects what that launcher
/// brought in (not the whole-machine total).
///
/// Returns an empty Vec for sources we don't have a dedicated registry/
/// manifest detector for yet (ea, itch, heroic, custom) — those rely on
/// the directory-walking Scanner instead and the caller can fall back to
/// `Scanner::scan_directories` for them.
pub fn detect_for_source(source: &str) -> Vec<Game> {
    match source {
        "steam" => local_detect::detect_steam_installed(),
        "epic" => local_detect::detect_epic_installed(),
        "gog" => local_detect::detect_gog_installed(),
        "ubi" => local_detect::detect_ubisoft_installed(),
        "ea" => local_detect::detect_ea_installed(),
        "xbox" => local_detect::detect_xbox_installed(),
        "amazon" => local_detect::detect_amazon_installed(),
        _ => Vec::new(),
    }
}
