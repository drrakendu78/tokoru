use serde::{Deserialize, Serialize};

/// A scanned/owned game in the Tokoru library.
///
/// `id` is a stable text identifier derived from a hash of the exe path
/// (assigned by the DB layer at insert time). `source` is the platform
/// the game was detected from (epic, gog, ubi, ea, itch, steam, xbox,
/// amazon, custom...).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Game {
    /// Stable identifier — text hash of exe path. Populated when persisted.
    pub id: Option<String>,
    pub title: String,
    /// epic / gog / ubi / ea / itch / steam / xbox / amazon / custom / local
    pub source: String,
    /// Install directory (best-effort).
    pub install_path: Option<String>,
    /// Path to the launcher exe / launch URI.
    pub exe_path: String,
    /// Best cover URL (from SteamGridDB or the source platform).
    pub artwork_url: Option<String>,
    /// Hero / banner URL (SteamGridDB hero).
    pub hero_url: Option<String>,
    /// Logo URL (SteamGridDB logo).
    pub logo_url: Option<String>,
    /// Icon URL (SteamGridDB icon, mirrored into Steam grid as
    /// `<appid>_icon.jpg`). Shows up in Steam's friends list, taskbar
    /// tooltip, "currently playing" header.
    pub icon_url: Option<String>,
    /// Optional source-side platform id (Steam appid, Epic appName, GOG gameID, etc.).
    pub platform_id: Option<String>,
    /// Optional protocol launch command (steam://, epicgames://, ...).
    pub launch_command: Option<String>,
    /// Unix epoch (seconds) of last scan.
    pub last_scanned_at: Option<i64>,
}

impl Game {
    /// Create a game from a scanned exe path (local exe scanner).
    pub fn new(title: String, exe_path: String, source: String) -> Self {
        Self {
            id: None,
            title,
            source,
            install_path: None,
            exe_path,
            artwork_url: None,
            hero_url: None,
            logo_url: None,
            icon_url: None,
            platform_id: None,
            launch_command: None,
            last_scanned_at: None,
        }
    }

    /// Create a game from a platform "owned" record (account-based imports).
    /// We treat the launch URI as the canonical exe_path so the hash is stable.
    #[allow(dead_code)]
    pub fn owned(
        title: String,
        source: String,
        platform_id: String,
        launch_command: String,
    ) -> Self {
        Self {
            id: None,
            title,
            source,
            install_path: None,
            exe_path: launch_command.clone(),
            artwork_url: None,
            hero_url: None,
            logo_url: None,
            icon_url: None,
            platform_id: Some(platform_id),
            launch_command: Some(launch_command),
            last_scanned_at: None,
        }
    }
}

/// Status of a shortcut pushed to Steam.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ShortcutStatus {
    Pending,
    Pushed,
    Removed,
}

impl ShortcutStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ShortcutStatus::Pending => "pending",
            ShortcutStatus::Pushed => "pushed",
            ShortcutStatus::Removed => "removed",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "pushed" => ShortcutStatus::Pushed,
            "removed" => ShortcutStatus::Removed,
            _ => ShortcutStatus::Pending,
        }
    }
}

/// A shortcut entry that has been (or will be) pushed to Steam.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shortcut {
    pub game_id: String,
    /// Computed CRC32+mask Steam appid for the non-Steam shortcut.
    pub steam_appid: u64,
    /// Unix epoch (seconds) of last push to Steam. None until first push.
    pub pushed_at: Option<i64>,
    pub status: ShortcutStatus,
}

/// A single playtime session observed by Tokoru.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaytimeSession {
    pub id: Option<i64>,
    pub game_id: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub duration_seconds: Option<i64>,
}
