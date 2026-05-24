use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::models::Game;

/// Blacklisted exe name substrings — if any of these appear in the exe filename, skip it
const EXE_BLACKLIST: &[&str] = &[
    "setup", "install", "uninstall", "unins", "crash", "reporter", "update",
    "updater", "redistributable", "redist", "vcredist", "dxsetup", "dotnet",
    "ue4prereq", "ue4-prereq", "steamworks", "easyanticheat", "battleye",
    "d3d", "directx", "7z", "winrar", "python", "node", "cmd", "powershell",
    "editor", "benchmark", "dedicated", "creationkit", "modtool", "config",
    "settings", "preferences", "unrealceceditor",
    // Dev / system tools
    "plugin", "plugins", "cli", "daemon", "service", "helper", "agent",
    "driver", "diagnostic", "repair", "recovery", "backup",
    // Hardware / peripherals
    "armoury", "armourycrate",
    // Media / browsers
    "vlc", "codec", "antivirus", "malware", "firewall",
    "overlay",
];

/// Directory path segments that indicate non-game software — skip any exe under these
const DIR_BLACKLIST: &[&str] = &[
    // OS / system
    "\\windows\\", "\\system32\\", "\\syswow64\\", "\\microsoft\\",
    "\\common files\\", "\\windowsapps\\", "\\.net\\", "\\dotnet\\",
    // Drivers / hardware
    "\\nvidia\\", "\\amd\\", "\\intel\\", "\\realtek\\",
    "\\asus\\", "\\armoury crate\\", "\\armourycrate\\",
    "\\corsair\\", "\\razer\\", "\\logitech\\", "\\steelseries\\",
    "\\msi\\", "\\gigabyte\\", "\\nzxt\\", "\\moza\\",
    // Redistributables
    "\\redistributable", "\\__installer", "\\_commonredist",
    // Platform client internals (not the games themselves)
    "\\steam\\bin\\", "\\steam\\package\\", "\\steam\\logs\\",
    "\\steam\\steamapps\\workshop\\",
    // Dev tools / non-game apps
    "\\google\\", "\\mozilla\\", "\\opera\\", "\\java\\", "\\oracle\\",
    "\\vmware\\", "\\docker\\", "\\git\\", "\\github\\",
    "\\node_modules\\", "\\visual studio\\", "\\vscode\\",
    "\\elgato\\", "\\streamdeck\\", "\\snap camera\\",
    "\\upscayl\\", "\\fences\\", "\\stardock\\",
    "\\synergy\\", "\\lghub\\", "\\logitech\\",
    "\\windows defender\\",
];

/// Full exe names (without .exe) to always skip
const EXE_EXACT_BLACKLIST: &[&str] = &[
    "steam", "steamservice", "steamerrorreporter",
    "epicgameslauncher", "epicwebhelper",
    "galaxyclient", "galaxyclienthelper",
    "vortex", "docker", "obs64", "obs32",
    "discord", "spotify", "chrome", "firefox", "brave", "edge", "vivaldi",
];

/// Parent folder names that are clearly not game names
const FOLDER_NAME_BLACKLIST: &[&str] = &[
    "bin", "binaries", "win64", "win32", "x64", "x86", "game", "build",
    "resources", "tests", "test", "tools", "support", "data", "assets",
    "plugins", "cli plugins", "frontend", "backend", "system tray",
    "evo", "bin64",
];

/// Minimum exe file size to consider (10 MB — filters out small tools more aggressively)
const MIN_EXE_SIZE: u64 = 10 * 1024 * 1024;

/// Maximum depth to scan in directories
const MAX_SCAN_DEPTH: usize = 5;

pub struct Scanner;

impl Scanner {
    /// Get default scan directories for Windows — only game-specific locations.
    /// Steam/Epic/GOG/Xbox libraries are imported via platform readers, NOT here.
    pub fn default_scan_dirs() -> Vec<PathBuf> {
        let mut dirs = Vec::new();

        let drives = ["C", "D", "E", "F", "G"];

        // Standalone game directories on each drive
        let game_dirs = [
            "Games",
            "Roberts Space Industries",
            "Riot Games",
            "Rockstar Games",
            "Battlestate Games",
            "Blizzard",
            "Battle.net",
        ];
        for drive in &drives {
            for suffix in &game_dirs {
                let path = PathBuf::from(format!("{}:\\{}", drive, suffix));
                if path.exists() {
                    dirs.push(path);
                }
            }
        }

        // Game-related folders inside Program Files / Program Files (x86)
        let pf_game_folders = [
            "Roberts Space Industries",
            "Riot Games",
            "Rockstar Games",
            "Battle.net",
            "Battlestate Games",
            "Ubisoft\\Ubisoft Game Launcher\\games",
        ];
        for pf_var in &["ProgramFiles", "ProgramFiles(x86)"] {
            if let Ok(pf) = std::env::var(pf_var) {
                for folder in &pf_game_folders {
                    let path = PathBuf::from(&pf).join(folder);
                    if path.exists() {
                        dirs.push(path);
                    }
                }
            }
        }

        dirs
    }

    /// Scan directories for game executables.
    /// Deduplicates by keeping only the largest exe per game directory.
    pub fn scan_directories(dirs: &[PathBuf]) -> Vec<Game> {
        let blacklist_re = build_blacklist_regex();
        let mut seen_paths: HashSet<String> = HashSet::new();

        // Group: game_directory -> (best_exe_path, best_exe_size)
        let mut best_per_dir: HashMap<String, (PathBuf, u64)> = HashMap::new();

        for dir in dirs {
            if !dir.exists() {
                continue;
            }

            for entry in WalkDir::new(dir)
                .max_depth(MAX_SCAN_DEPTH)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();

                if !is_valid_exe(path, &blacklist_re) {
                    continue;
                }

                let file_size = match std::fs::metadata(path) {
                    Ok(m) => m.len(),
                    Err(_) => continue,
                };
                if file_size < MIN_EXE_SIZE {
                    continue;
                }

                let path_str = path.to_string_lossy().to_string();
                if seen_paths.contains(&path_str) {
                    continue;
                }
                seen_paths.insert(path_str);

                // Use the game directory (top-level folder under the scan root) as key
                let game_dir_key = get_game_directory_key(dir, path);

                let entry = best_per_dir
                    .entry(game_dir_key)
                    .or_insert_with(|| (path.to_path_buf(), file_size));

                if file_size > entry.1 {
                    *entry = (path.to_path_buf(), file_size);
                }
            }
        }

        best_per_dir
            .into_values()
            .map(|(path, _)| {
                let name = extract_game_name(&path);
                let path_str = path.to_string_lossy().to_string();
                let install_path = path
                    .parent()
                    .map(|p| p.to_string_lossy().to_string());
                let mut game = Game::new(name, path_str, "local".to_string());
                game.install_path = install_path;
                game
            })
            .collect()
    }
}

/// Get a key representing the "game directory" — the first subfolder under the scan root.
/// This groups all exes from the same game together so we only keep the best one.
fn get_game_directory_key(scan_root: &Path, exe_path: &Path) -> String {
    if let Ok(relative) = exe_path.strip_prefix(scan_root) {
        // Take the first path component as the game directory
        if let Some(first) = relative.components().next() {
            return scan_root.join(first).to_string_lossy().to_lowercase();
        }
    }
    // Fallback: use the exe's parent directory
    exe_path
        .parent()
        .unwrap_or(exe_path)
        .to_string_lossy()
        .to_lowercase()
}

fn build_blacklist_regex() -> Regex {
    let pattern = EXE_BLACKLIST
        .iter()
        .map(|s| regex::escape(s))
        .collect::<Vec<_>>()
        .join("|");
    Regex::new(&format!("(?i)({})", pattern)).unwrap()
}

/// Check if a path is a valid game executable
fn is_valid_exe(path: &Path, blacklist: &Regex) -> bool {
    let ext = path.extension().and_then(|e| e.to_str());
    if ext != Some("exe") {
        return false;
    }

    let file_name = match path.file_stem().and_then(|s| s.to_str()) {
        Some(name) => name.to_lowercase(),
        None => return false,
    };

    // Check exact name blacklist
    if EXE_EXACT_BLACKLIST.contains(&file_name.as_str()) {
        return false;
    }

    // Check substring blacklist
    if blacklist.is_match(&file_name) {
        return false;
    }

    // Check directory path blacklist
    let path_str = path.to_string_lossy().to_lowercase();
    for skip in DIR_BLACKLIST {
        if path_str.contains(skip) {
            return false;
        }
    }

    true
}

/// Extract a human-readable game name from the file path
pub fn extract_game_name(path: &Path) -> String {
    let parent_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");

    let exe_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown");

    // Use parent dir name if it looks meaningful, otherwise exe name
    let parent_lower = parent_name.to_lowercase();
    let raw_name = if !parent_name.is_empty()
        && parent_name.len() > 2
        && !FOLDER_NAME_BLACKLIST.contains(&parent_lower.as_str())
    {
        parent_name
    } else {
        exe_name
    };

    clean_game_name(raw_name)
}

/// Normalize a game name for display
fn clean_game_name(name: &str) -> String {
    let mut cleaned = name.to_string();

    cleaned = cleaned
        .replace('_', " ")
        .replace('-', " ")
        .replace('.', " ");

    let re = Regex::new(r"\s+").unwrap();
    cleaned = re.replace_all(&cleaned, " ").trim().to_string();

    // Title case
    cleaned
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    let upper: String = first.to_uppercase().collect();
                    format!("{}{}", upper, chars.as_str())
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
