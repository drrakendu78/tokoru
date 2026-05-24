//! Helper-binary management for the third-party Epic (legendary) and GOG
//! (gogdl) CLIs that Tokoru uses to authenticate against the official
//! storefronts and read the owned-library list.
//!
//! - We never bundle the executables in our installer — they live at
//!   `<app_data_dir>/bin/{legendary,gogdl}.exe` and get fetched on first use.
//! - We pin specific upstream releases (same ones OrionCore validates against)
//!   so behaviour stays stable even when upstream pushes a breaking change.
//! - Once on disk, we just reuse them; subsequent connects are instant.
//!
//! Concurrent first-time fetches for the same CLI are serialised behind a
//! `tokio::Mutex` per provider so two parallel `Connect` clicks don't race
//! and end up writing a truncated `.exe`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};
use tokio::fs;
use tokio::sync::Mutex;

/// Pinned legendary release — same one OrionCore ships.
const LEGENDARY_URL: &str =
    "https://github.com/legendary-gl/legendary/releases/download/0.20.34/legendary.exe";

/// Pinned gogdl release — same one OrionCore ships.
const GOGDL_URL: &str =
    "https://github.com/Heroic-Games-Launcher/heroic-gogdl/releases/download/v1.2.1/gogdl_windows_x86_64.exe";

fn legendary_mutex() -> &'static Mutex<()> {
    static M: OnceLock<Mutex<()>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(()))
}

fn gogdl_mutex() -> &'static Mutex<()> {
    static M: OnceLock<Mutex<()>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(()))
}

fn app_data_dir(app_handle: &AppHandle) -> Result<PathBuf, String> {
    app_handle
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to resolve app_data_dir: {}", e))
}

fn bin_dir(app_handle: &AppHandle) -> Result<PathBuf, String> {
    Ok(app_data_dir(app_handle)?.join("bin"))
}

/// Returns the on-disk path for legendary.exe (may not exist yet).
pub fn legendary_path(app_handle: &AppHandle) -> Result<PathBuf, String> {
    Ok(bin_dir(app_handle)?.join("legendary.exe"))
}

/// Returns the on-disk path for gogdl.exe (may not exist yet).
pub fn gogdl_path(app_handle: &AppHandle) -> Result<PathBuf, String> {
    Ok(bin_dir(app_handle)?.join("gogdl.exe"))
}

/// Ensure `legendary.exe` is downloaded and ready to run. Idempotent — returns
/// the cached path if it already exists.
pub async fn ensure_legendary(app_handle: &AppHandle) -> Result<PathBuf, String> {
    let _guard = legendary_mutex().lock().await;
    let path = legendary_path(app_handle)?;
    download_if_missing(&path, LEGENDARY_URL, "legendary").await?;
    Ok(path)
}

/// Ensure `gogdl.exe` is downloaded and ready to run. Idempotent.
pub async fn ensure_gogdl(app_handle: &AppHandle) -> Result<PathBuf, String> {
    let _guard = gogdl_mutex().lock().await;
    let path = gogdl_path(app_handle)?;
    download_if_missing(&path, GOGDL_URL, "gogdl").await?;
    Ok(path)
}

/// Force-replace `legendary.exe` (delete + re-fetch the pinned release).
/// Used by the Settings → Downloads → "Re-download" button so the user can
/// recover from a corrupted binary without having to dig through app-data.
pub async fn redownload_legendary(app_handle: &AppHandle) -> Result<PathBuf, String> {
    let _guard = legendary_mutex().lock().await;
    let path = legendary_path(app_handle)?;
    if path.exists() {
        let _ = fs::remove_file(&path).await;
    }
    download_if_missing(&path, LEGENDARY_URL, "legendary").await?;
    Ok(path)
}

/// Force-replace `gogdl.exe`. See `redownload_legendary`.
pub async fn redownload_gogdl(app_handle: &AppHandle) -> Result<PathBuf, String> {
    let _guard = gogdl_mutex().lock().await;
    let path = gogdl_path(app_handle)?;
    if path.exists() {
        let _ = fs::remove_file(&path).await;
    }
    download_if_missing(&path, GOGDL_URL, "gogdl").await?;
    Ok(path)
}

/// Pinned version string surfaced in the Settings → Downloads UI alongside
/// the presence check. We don't actually run `--version` on the binaries
/// (legendary's `--version` exits non-zero on some builds and gogdl's flag is
/// a moving target) — instead we report the version embedded in the pinned
/// URL above, which is what we KNOW is on disk if the file is present.
pub fn pinned_legendary_version() -> &'static str {
    "0.20.34"
}

pub fn pinned_gogdl_version() -> &'static str {
    "v1.2.1"
}

async fn download_if_missing(dest: &Path, url: &str, label: &str) -> Result<(), String> {
    if dest.exists() {
        log::debug!("[cli_bins] {} already at {:?}", label, dest);
        return Ok(());
    }

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create {} dir: {}", label, e))?;
    }

    log::info!("[cli_bins] downloading {} from {}", label, url);

    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Failed to download {}: {}", label, e))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Failed to download {}: HTTP {} (URL: {})",
            label,
            resp.status(),
            url
        ));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed to read {} binary: {}", label, e))?;

    // Tiny sanity check — a real .exe is at least a few hundred KB.
    if bytes.len() < 200_000 {
        return Err(format!(
            "Downloaded {} looks corrupted ({} bytes)",
            label,
            bytes.len()
        ));
    }

    fs::write(dest, &bytes)
        .await
        .map_err(|e| format!("Failed to write {}: {}", label, e))?;

    // Best-effort: set executable perms on unix-ish; no-op on Windows.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(mut perms) = std::fs::metadata(dest).map(|m| m.permissions()) {
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(dest, perms);
        }
    }

    log::info!(
        "[cli_bins] {} downloaded successfully ({} bytes)",
        label,
        bytes.len()
    );
    Ok(())
}
