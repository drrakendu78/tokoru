use tauri::State;

use crate::services::db::Database;
use crate::services::platforms::local_detect;

#[tauri::command]
pub fn quit_app() {
    std::process::exit(0);
}

/// Return the detected Steam install path, if any.
/// Used by the Steam-shortcut writer to locate `userdata/<id>/config/shortcuts.vdf`.
#[tauri::command]
pub fn get_steam_path() -> Option<String> {
    local_detect::get_steam_path().map(|p| p.to_string_lossy().to_string())
}

/// Launch a protocol URI (steam://, com.epicgames.launcher://, goggalaxy://,
/// etc.) via the OS handler. Bypasses Tauri's `shell:allow-open` scope which
/// only whitelists http/mailto/tel by default — we control the scheme list
/// here at the Rust level instead.
///
/// On Windows we shell out to `cmd /C start "" "<uri>"` because
/// `std::process::Command::new(uri).spawn()` won't invoke the URL handler;
/// only `cmd start` resolves the protocol via the registry.
#[tauri::command]
pub fn launch_uri(uri: String) -> Result<(), String> {
    // Lightweight allow-list — only schemes we actively use anywhere in the
    // app. Rejecting unknown schemes here means an XSS / event-injection
    // can't fire e.g. `file://` or `vbscript:` via this command.
    const ALLOWED_PREFIXES: &[&str] = &[
        "steam://",
        "com.epicgames.launcher://",
        "goggalaxy://",
        "uplay://",
        "origin://",
        "ea://",
        "itch://",
        "http://",
        "https://",
    ];
    let trimmed = uri.trim();
    if !ALLOWED_PREFIXES
        .iter()
        .any(|p| trimmed.to_ascii_lowercase().starts_with(p))
    {
        return Err(format!("Unsupported URI scheme: {}", uri));
    }

    #[cfg(windows)]
    {
        use std::process::Command;
        // `start` is a builtin of cmd.exe, so we need to invoke cmd.
        // The empty `""` after `start` is the window title (start treats the
        // first quoted arg as a title), without it the URI would be parsed
        // as the title instead of the target.
        Command::new("cmd")
            .args(["/C", "start", "", trimmed])
            .spawn()
            .map_err(|e| format!("Failed to launch URI: {}", e))?;
        return Ok(());
    }

    #[cfg(not(windows))]
    {
        return Err("URI launching only implemented on Windows for now".to_string());
    }
}

/// Open the game's install directory in the OS file explorer. Used by
/// the GameDetail "Open install folder" more-menu item. Returns an
/// error string the frontend can surface as a toast when the row has
/// no install path or the directory doesn't exist on disk (uninstalled
/// since last scan).
#[tauri::command]
pub async fn open_install_folder(
    id: String,
    db: State<'_, Database>,
) -> Result<(), String> {
    let game = db
        .get_game_by_id(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Game not found".to_string())?;
    let path = game
        .install_path
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| "This game has no install path on record.".to_string())?;
    if !std::path::Path::new(&path).exists() {
        return Err(format!("Install path no longer exists: {}", path));
    }

    #[cfg(windows)]
    {
        use std::process::Command;
        Command::new("explorer.exe")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
        return Ok(());
    }

    #[cfg(not(windows))]
    {
        let _ = path;
        return Err("Open install folder only implemented on Windows.".to_string());
    }
}
