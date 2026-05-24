use crate::models::Game;
use std::path::PathBuf;

#[cfg(target_os = "windows")]
use winreg::enums::*;
#[cfg(target_os = "windows")]
use winreg::RegKey;

// ============ Steam (installed) ============
//
// NOTE: this complements `services::steam_api`. The API knows the full owned
// library (installed + uninstalled) with canonical names and library art; the
// local pass below knows the actual `install_path` / `exe_path` on disk so the
// playtime watcher can observe the real process. Both produce games tagged
// with `source = "steam"` and `platform_id = Some(appid_as_string)`, and they
// merge at upsert time via `db::upsert_game_from_local` /
// `db::upsert_game_from_api` keyed on `(source='steam', platform_id)`.

pub fn detect_steam_installed() -> Vec<Game> {
    let mut games = Vec::new();

    #[cfg(target_os = "windows")]
    {
        if let Some(steam_dir) = get_steam_path() {
            let libraries = find_steam_libraries(&steam_dir);
            for lib_path in libraries {
                let steamapps = lib_path.join("steamapps");
                if !steamapps.exists() {
                    continue;
                }
                if let Ok(entries) = std::fs::read_dir(&steamapps) {
                    for entry in entries.flatten() {
                        let fname = entry.file_name().to_string_lossy().to_string();
                        if fname.starts_with("appmanifest_") && fname.ends_with(".acf") {
                            if let Some(game) = parse_acf_manifest(&entry.path(), &steamapps) {
                                games.push(game);
                            }
                        }
                    }
                }
            }
        }
    }

    games
}

#[cfg(target_os = "windows")]
pub fn get_steam_path() -> Option<PathBuf> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    if let Ok(key) = hklm.open_subkey("SOFTWARE\\Wow6432Node\\Valve\\Steam") {
        if let Ok(path) = key.get_value::<String, _>("InstallPath") {
            return Some(PathBuf::from(path));
        }
    }
    if let Ok(key) = hklm.open_subkey("SOFTWARE\\Valve\\Steam") {
        if let Ok(path) = key.get_value::<String, _>("InstallPath") {
            return Some(PathBuf::from(path));
        }
    }
    let default = PathBuf::from("C:\\Program Files (x86)\\Steam");
    if default.exists() {
        return Some(default);
    }
    None
}

#[cfg(not(target_os = "windows"))]
pub fn get_steam_path() -> Option<PathBuf> {
    None
}

#[cfg(target_os = "windows")]
pub(crate) fn find_steam_libraries(steam_dir: &PathBuf) -> Vec<PathBuf> {
    let mut libraries = vec![steam_dir.clone()];
    let vdf_path = steam_dir.join("steamapps").join("libraryfolders.vdf");

    if let Ok(content) = std::fs::read_to_string(&vdf_path) {
        let re = regex::Regex::new(r#""path"\s+"([^"]+)""#).unwrap();
        for cap in re.captures_iter(&content) {
            let path_str = cap[1].replace("\\\\", "\\");
            let path = PathBuf::from(&path_str);
            if path.exists() && !libraries.contains(&path) {
                libraries.push(path);
            }
        }
    }

    libraries
}

fn parse_acf_manifest(acf_path: &PathBuf, steamapps: &PathBuf) -> Option<Game> {
    let content = std::fs::read_to_string(acf_path).ok()?;

    let name = extract_acf_value(&content, "name")?;
    let appid = extract_acf_value(&content, "appid")?;
    let installdir = extract_acf_value(&content, "installdir")?;

    // Skip tools, SDKs
    if let Some(app_type) = extract_acf_value(&content, "apptype") {
        if app_type.to_lowercase() != "game" {
            return None;
        }
    }

    let game_dir = steamapps.join("common").join(&installdir);
    let exe_path = if game_dir.exists() {
        find_main_exe(&game_dir).unwrap_or_else(|| game_dir.to_string_lossy().to_string())
    } else {
        game_dir.to_string_lossy().to_string()
    };

    let launch_cmd = format!("steam://rungameid/{}", appid);
    let cover_url = format!(
        "https://steamcdn-a.akamaihd.net/steam/apps/{}/library_600x900_2x.jpg",
        appid
    );

    let mut game = Game::new(name, exe_path, "steam".to_string());
    game.install_path = Some(game_dir.to_string_lossy().to_string());
    game.launch_command = Some(launch_cmd);
    game.platform_id = Some(appid);
    game.artwork_url = Some(cover_url);
    Some(game)
}

fn extract_acf_value(content: &str, key: &str) -> Option<String> {
    let re = regex::Regex::new(&format!(r#""{key}"\s+"([^"]+)""#)).ok()?;
    re.captures(content).map(|c| c[1].to_string())
}

fn find_main_exe(game_dir: &PathBuf) -> Option<String> {
    let mut best: Option<(PathBuf, u64)> = None;

    for entry in walkdir::WalkDir::new(game_dir)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("exe") {
            continue;
        }
        let fname = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let fname_lower = fname.to_lowercase();

        if fname_lower.contains("uninstall")
            || fname_lower.contains("setup")
            || fname_lower.contains("crash")
            || fname_lower.contains("redist")
            || fname_lower.contains("update")
        {
            continue;
        }

        if let Ok(meta) = std::fs::metadata(path) {
            let size = meta.len();
            match &best {
                Some((_, best_size)) if size > *best_size => {
                    best = Some((path.to_path_buf(), size));
                }
                None => best = Some((path.to_path_buf(), size)),
                _ => {}
            }
        }
    }

    best.map(|(p, _)| p.to_string_lossy().to_string())
}

// ============ Epic (installed) ============

pub fn detect_epic_installed() -> Vec<Game> {
    let mut games = Vec::new();

    #[cfg(target_os = "windows")]
    {
        let manifests_dir =
            PathBuf::from("C:\\ProgramData\\Epic\\EpicGamesLauncher\\Data\\Manifests");
        if manifests_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&manifests_dir) {
                for entry in entries.flatten() {
                    if entry.path().extension().and_then(|e| e.to_str()) == Some("item") {
                        if let Some(game) = parse_epic_manifest(&entry.path()) {
                            games.push(game);
                        }
                    }
                }
            }
        }
    }

    games
}

fn parse_epic_manifest(path: &PathBuf) -> Option<Game> {
    let content = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    let display_name = json.get("DisplayName")?.as_str()?.to_string();
    let install_location = json.get("InstallLocation")?.as_str()?.to_string();
    let launch_exe = json.get("LaunchExecutable")?.as_str()?.to_string();
    let app_name = json.get("AppName")?.as_str()?.to_string();

    // Filter UE tools
    if app_name.starts_with("UE_") {
        return None;
    }

    let exe_path = PathBuf::from(&install_location)
        .join(&launch_exe)
        .to_string_lossy()
        .to_string();

    let launch_cmd = format!(
        "com.epicgames.launcher://apps/{}?action=launch&silent=true",
        app_name
    );

    let mut game = Game::new(display_name, exe_path, "epic".to_string());
    game.install_path = Some(install_location);
    game.launch_command = Some(launch_cmd);
    game.platform_id = Some(app_name);
    Some(game)
}

// ============ GOG (installed) ============

pub fn detect_gog_installed() -> Vec<Game> {
    let mut games = Vec::new();

    #[cfg(target_os = "windows")]
    {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        if let Ok(gog_key) = hklm.open_subkey("SOFTWARE\\WOW6432Node\\GOG.com\\Games") {
            for subkey_name in gog_key.enum_keys().filter_map(|k| k.ok()) {
                if let Ok(game_key) = gog_key.open_subkey(&subkey_name) {
                    let name: String = game_key.get_value("gameName").unwrap_or_default();
                    let exe: String = game_key.get_value("exe").unwrap_or_default();
                    let game_id: String = game_key.get_value("gameID").unwrap_or_default();
                    let install_path: String =
                        game_key.get_value("path").unwrap_or_default();

                    if !name.is_empty() && !exe.is_empty() {
                        let launch_cmd = if !game_id.is_empty() {
                            Some(format!("goggalaxy://openGameView/{}", game_id))
                        } else {
                            None
                        };
                        let mut game = Game::new(name, exe, "gog".to_string());
                        game.launch_command = launch_cmd;
                        game.platform_id = if !game_id.is_empty() {
                            Some(game_id)
                        } else {
                            None
                        };
                        game.install_path = if !install_path.is_empty() {
                            Some(install_path)
                        } else {
                            None
                        };
                        games.push(game);
                    }
                }
            }
        }
    }

    games
}

// ============ Title normalization ============

/// Clean a raw folder name into a presentable game title.
///
/// Many Ubi / EA install folders ship in all-caps with underscores
/// (`WATCH_DOGS2`, `DRAGON_AGE_INQUISITION`) which then fails any
/// metadata lookup downstream (SteamGridDB, Steam Store, IGDB). This
/// helper:
///   * replaces underscores with spaces
///   * title-cases entries that are entirely uppercase (preserves
///     CamelCase / mixed-case names as-is so we don't ruin "PUBG" or
///     "DOOM Eternal")
///   * splits trailing digits from a word ("WATCHDOGS2" → "Watchdogs 2")
fn clean_title_from_folder(name: &str) -> String {
    let with_spaces = name.replace('_', " ").trim().to_string();
    if with_spaces.is_empty() {
        return name.to_string();
    }
    // Insert a space between a letter and a trailing digit run
    // (`WATCH DOGS2` → `WATCH DOGS 2`).
    let mut digit_split = String::with_capacity(with_spaces.len() + 4);
    let chars: Vec<char> = with_spaces.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 {
            let prev = chars[i - 1];
            if prev.is_ascii_alphabetic() && c.is_ascii_digit() {
                digit_split.push(' ');
            }
        }
        digit_split.push(c);
    }
    // Only title-case when the whole string is uppercase (preserve
    // intentional mixed-case names).
    let is_all_caps = digit_split.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_uppercase());
    if !is_all_caps {
        return digit_split;
    }
    digit_split
        .split_whitespace()
        .map(|word| {
            let mut cs = word.chars();
            match cs.next() {
                Some(first) => {
                    let rest: String = cs.collect::<String>().to_lowercase();
                    format!("{}{}", first.to_uppercase(), rest)
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod title_tests {
    use super::clean_title_from_folder;

    #[test]
    fn underscores_become_spaces_and_titlecased() {
        assert_eq!(clean_title_from_folder("WATCH_DOGS2"), "Watch Dogs 2");
        assert_eq!(
            clean_title_from_folder("DRAGON_AGE_INQUISITION"),
            "Dragon Age Inquisition"
        );
    }

    #[test]
    fn mixed_case_preserved() {
        assert_eq!(clean_title_from_folder("DOOM Eternal"), "DOOM Eternal");
        assert_eq!(clean_title_from_folder("CyberpunkLauncher"), "CyberpunkLauncher");
    }

    #[test]
    fn split_digit_run() {
        assert_eq!(clean_title_from_folder("WATCHDOGS2"), "Watchdogs 2");
    }
}

// ============ Ubisoft (installed) ============

pub fn detect_ubisoft_installed() -> Vec<Game> {
    let mut games = Vec::new();

    #[cfg(target_os = "windows")]
    {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        if let Ok(ubi_key) = hklm.open_subkey("SOFTWARE\\WOW6432Node\\Ubisoft\\Launcher\\Installs")
        {
            for subkey_name in ubi_key.enum_keys().filter_map(|k| k.ok()) {
                if let Ok(game_key) = ubi_key.open_subkey(&subkey_name) {
                    let install_dir: String = game_key.get_value("InstallDir").unwrap_or_default();
                    if !install_dir.is_empty() {
                        let game_dir = PathBuf::from(&install_dir);
                        if game_dir.exists() {
                            if let Some(exe) = find_main_exe(&game_dir) {
                                let raw_name = game_dir
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("Unknown");
                                let name = clean_title_from_folder(raw_name);
                                let launch_cmd = format!("uplay://launch/{}/0", subkey_name);
                                let mut game = Game::new(name, exe, "ubi".to_string());
                                game.install_path = Some(install_dir);
                                game.launch_command = Some(launch_cmd);
                                game.platform_id = Some(subkey_name.clone());
                                games.push(game);
                            }
                        }
                    }
                }
            }
        }
    }

    games
}

// ============ EA / Origin (installed) ============

/// Detect games installed by the EA App / Origin client.
///
/// Two places to look:
///   1. Default install roots — `C:\Program Files\EA Games\<game>`,
///      `Program Files (x86)\EA Games\<game>`,
///      `Program Files (x86)\Origin Games\<game>`. The EA App + Origin both
///      drop one folder per game here.
///   2. Registry `HKLM\SOFTWARE\WOW6432Node\Origin Games\<id>\InstallLocation`
///      for users who changed the default install root. Modern EA App
///      entries often have an empty `InstallLocation`, so this is a
///      fallback rather than the primary source.
pub fn detect_ea_installed() -> Vec<Game> {
    let mut games = Vec::new();
    let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();

    #[cfg(target_os = "windows")]
    {
        // 1. Default EA / Origin install roots.
        let root_candidates = [
            "C:\\Program Files\\EA Games",
            "C:\\Program Files (x86)\\EA Games",
            "C:\\Program Files (x86)\\Origin Games",
            "D:\\EA Games",
            "D:\\Origin Games",
        ];
        for root in root_candidates {
            let root_path = std::path::Path::new(root);
            if !root_path.exists() {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(root_path) else { continue };
            for entry in entries.flatten() {
                let game_dir = entry.path();
                if !game_dir.is_dir() {
                    continue;
                }
                let key = game_dir.to_string_lossy().to_lowercase();
                if !seen_dirs.insert(key) {
                    continue;
                }
                if let Some(exe) = find_main_exe(&game_dir) {
                    let raw_name = game_dir
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("Unknown");
                    let name = clean_title_from_folder(raw_name);
                    let mut game = Game::new(name, exe, "ea".to_string());
                    game.install_path = Some(game_dir.to_string_lossy().to_string());
                    games.push(game);
                }
            }
        }

        // 2. Registry fallback for users with custom install paths. Walk
        //    both Origin Games and EA Games hives — many users still have
        //    the legacy `Origin Games` keys from when they migrated.
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        for hive in [
            "SOFTWARE\\WOW6432Node\\Origin Games",
            "SOFTWARE\\WOW6432Node\\EA Games",
        ] {
            let Ok(root_key) = hklm.open_subkey(hive) else { continue };
            for subkey_name in root_key.enum_keys().filter_map(|k| k.ok()) {
                let Ok(game_key) = root_key.open_subkey(&subkey_name) else { continue };
                let install_dir: String = game_key.get_value("InstallLocation").unwrap_or_default();
                if install_dir.is_empty() {
                    continue;
                }
                let key = install_dir.to_lowercase();
                if !seen_dirs.insert(key) {
                    continue;
                }
                let game_dir = PathBuf::from(&install_dir);
                if !game_dir.exists() {
                    continue;
                }
                if let Some(exe) = find_main_exe(&game_dir) {
                    let display_name: String = game_key
                        .get_value("DisplayName")
                        .unwrap_or_else(|_| subkey_name.clone());
                    let mut game = Game::new(display_name, exe, "ea".to_string());
                    game.install_path = Some(install_dir);
                    game.platform_id = Some(subkey_name);
                    games.push(game);
                }
            }
        }
    }

    games
}

// ============ Xbox (installed) ============

pub fn detect_xbox_installed() -> Vec<Game> {
    let mut games = Vec::new();

    #[cfg(target_os = "windows")]
    {
        let possible_dirs = ["C:\\XboxGames", "D:\\XboxGames", "E:\\XboxGames"];
        for dir_str in &possible_dirs {
            let dir = PathBuf::from(dir_str);
            if dir.exists() {
                if let Ok(entries) = std::fs::read_dir(&dir) {
                    for entry in entries.flatten() {
                        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            let name = entry.file_name().to_string_lossy().to_string();
                            let content_dir = entry.path().join("Content");
                            let game_dir = if content_dir.exists() {
                                content_dir
                            } else {
                                entry.path()
                            };
                            if let Some(exe) = find_main_exe(&game_dir.to_path_buf()) {
                                let mut game = Game::new(name, exe, "xbox".to_string());
                                game.install_path = Some(game_dir.to_string_lossy().to_string());
                                games.push(game);
                            }
                        }
                    }
                }
            }
        }
    }

    games
}

// ============ Amazon (installed) ============

pub fn detect_amazon_installed() -> Vec<Game> {
    let mut games = Vec::new();

    #[cfg(target_os = "windows")]
    {
        // Amazon Games stores installed game info in a SQLite DB
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let db_path = PathBuf::from(&local_app_data)
                .join("Amazon Games")
                .join("Data")
                .join("Games")
                .join("Sql")
                .join("GameInstallInfo.sqlite");

            if db_path.exists() {
                if let Ok(conn) = rusqlite::Connection::open_with_flags(
                    &db_path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                ) {
                    let mut stmt = match conn.prepare(
                        "SELECT Id, ProductTitle, InstallDirectory FROM DbSet WHERE Installed = 1",
                    ) {
                        Ok(s) => s,
                        Err(_) => return games,
                    };

                    let rows = stmt
                        .query_map([], |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        })
                        .ok();

                    if let Some(rows) = rows {
                        for row in rows.flatten() {
                            let (product_id, title, install_dir) = row;
                            let game_dir = PathBuf::from(&install_dir);
                            if game_dir.exists() {
                                let exe =
                                    find_main_exe(&game_dir).unwrap_or_else(|| install_dir.clone());
                                let launch_cmd = format!("amazon-games://play/{}", product_id);
                                let mut game = Game::new(title, exe, "amazon".to_string());
                                game.install_path = Some(install_dir);
                                game.launch_command = Some(launch_cmd);
                                game.platform_id = Some(product_id);
                                games.push(game);
                            }
                        }
                    }
                }
            }
        }
    }

    games
}
