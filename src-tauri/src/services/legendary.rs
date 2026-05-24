//! Thin wrapper around the `legendary` CLI for Epic Games installs.
//!
//! The binary itself is managed by `cli_bins::ensure_legendary` (download-on-
//! first-use, pinned release). Auth tokens are already written by the Epic
//! login flow (`commands::epic`) via `legendary auth --code`, so we don't
//! re-auth here.
//!
//! This module ONLY exposes the install/uninstall/info entry points used by
//! the download orchestrator. Library listing + auth status live in
//! `services::epic_api` and are kept separate so the existing OAuth flow
//! doesn't get muddled with download concerns.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use tauri::AppHandle;

use crate::services::cli_bins;

/// Resolve the legendary.exe path, downloading it if missing. Returns an
/// error if the download fails — caller surfaces it to the UI.
pub async fn ensure_exe(app: &AppHandle) -> Result<PathBuf, String> {
    cli_bins::ensure_legendary(app).await
}

/// Spawn `legendary install <app_name>` as a child process with stdout +
/// stderr piped, so the orchestrator can parse progress lines.
///
/// `--yes` skips the "are you sure?" prompts. `--base-path` overrides the
/// install location (legendary defaults to `~/Games/<app_name>`).
/// `--language` selects the install language code (legendary's default
/// falls back to English).
pub fn spawn_install(
    legendary_exe: &PathBuf,
    app_name: &str,
    base_path: Option<&str>,
    language: Option<&str>,
) -> Result<Child, String> {
    let mut args: Vec<String> = vec![
        "install".to_string(),
        app_name.to_string(),
        "--yes".to_string(),
    ];
    if let Some(path) = base_path {
        args.push("--base-path".to_string());
        args.push(path.to_string());
    }
    if let Some(lang) = language {
        args.push("--language".to_string());
        args.push(lang.to_string());
    }

    log::info!(
        "[legendary] spawn install: {} {}",
        legendary_exe.display(),
        args.join(" ")
    );

    let mut cmd = Command::new(legendary_exe);
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        // CREATE_NEW_PROCESS_GROUP so we can send CTRL_BREAK_EVENT later
        // (legendary registers signal handlers and writes its resume manifest
        // on shutdown — without this, taskkill is a no-op for console apps
        // and we end up SIGKILLing before checkpoint).
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn()
        .map_err(|e| format!("Failed to spawn legendary install: {}", e))
}

/// Best-effort lookup of the installed path for a game already on disk
/// (used by the post-completion `install_path` resolution flow — legendary
/// picks its own subfolder under `--base-path` based on the game's *real*
/// title, which may differ from the slugified name we passed). Synchronous
/// — runs the CLI; caller should wrap in `spawn_blocking`.
pub fn installed_path(
    legendary_exe: &PathBuf,
    app_name: &str,
) -> Result<Option<String>, String> {
    for (name, path) in all_installed(legendary_exe)? {
        if name == app_name {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// Returns every installed Epic game as (app_name, install_path) pairs by
/// reading `installed.json` once via `legendary list-installed --json`.
/// Preferred over calling `installed_path` in a loop when re-syncing the
/// full install set — one process spawn instead of N. Synchronous; caller
/// should wrap in `spawn_blocking`.
pub fn all_installed(legendary_exe: &PathBuf) -> Result<Vec<(String, String)>, String> {
    let out = Command::new(legendary_exe)
        .args(["list-installed", "--json"])
        .output()
        .map_err(|e| format!("Failed to run legendary list-installed: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "legendary list-installed exited {}",
            out.status
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_default();
    let items = parsed.as_array().cloned().unwrap_or_default();
    let mut result = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = item.get("app_name").and_then(|v| v.as_str()) else {
            continue;
        };
        // Legendary's installed.json puts `install_path` either inside an
        // `install` sub-object (newer versions) or at the top level (older).
        let path = item
            .get("install")
            .and_then(|i| i.get("install_path"))
            .and_then(|v| v.as_str())
            .or_else(|| item.get("install_path").and_then(|v| v.as_str()));
        if let Some(p) = path {
            result.push((name.to_string(), p.to_string()));
        }
    }
    Ok(result)
}

/// Parsed progress info from a legendary stderr line. `false` means the
/// line wasn't a progress line and the caller should skip it.
///
/// Legendary's progress lines look like:
/// ```text
/// [DLManager] INFO: = Progress: 12.34% (123.4/1000.0 MiB), Running for 00:01:23, ETA: 00:10:00
/// [DLManager] INFO: + Download speed: 50.0 MiB/s (raw) / 60.1 MiB/s (decompressed)
/// ```
#[derive(Debug, Default, Clone, Copy)]
pub struct ProgressDelta {
    pub progress_pct: Option<f32>,
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub speed_bps: Option<u64>,
    pub eta_secs: Option<u32>,
}

/// Returns Some(delta) if the line was a recognised progress line. The caller
/// should fold the deltas into the persistent `DownloadState` (we only emit
/// what changed in this line — speed lines don't carry percent, etc.).
pub fn parse_progress_line(line: &str) -> Option<ProgressDelta> {
    let mut delta = ProgressDelta::default();
    let mut matched = false;

    // ── Progress line: "Progress: 12.34% (123.4/1000.0 MiB), ..., ETA: HH:MM:SS"
    if line.contains("Progress:") {
        if let Some(idx) = line.find("Progress:") {
            let after = &line[idx + "Progress:".len()..].trim();
            if let Some(pct_end) = after.find('%') {
                if let Ok(pct) = after[..pct_end].trim().parse::<f32>() {
                    delta.progress_pct = Some(pct);
                    matched = true;
                }
            }
            if let (Some(paren_start), Some(paren_end)) = (after.find('('), after.find(')')) {
                if paren_end > paren_start {
                    let inside = &after[paren_start + 1..paren_end];
                    // "123.4/1000.0 MiB"
                    let parts: Vec<&str> = inside.split('/').collect();
                    if parts.len() == 2 {
                        let dl = parts[0].trim().parse::<f64>().ok();
                        // total side carries the unit: "1000.0 MiB"
                        let mut total_iter = parts[1].trim().split_whitespace();
                        let total_num = total_iter.next().and_then(|s| s.parse::<f64>().ok());
                        let unit = total_iter.next().unwrap_or("MiB");
                        let mul = unit_multiplier(unit);
                        if let (Some(d), Some(t)) = (dl, total_num) {
                            delta.downloaded_bytes = Some((d * mul) as u64);
                            delta.total_bytes = Some((t * mul) as u64);
                            matched = true;
                        }
                    }
                }
            }
            if let Some(eta_idx) = after.find("ETA:") {
                let eta_str = after[eta_idx + 4..]
                    .split(',')
                    .next()
                    .unwrap_or("")
                    .trim();
                if let Some(secs) = parse_hms_to_secs(eta_str) {
                    delta.eta_secs = Some(secs);
                    matched = true;
                }
            }
        }
    }

    // ── Speed line: "Download speed: 50.0 MiB/s (raw) ..."
    if let Some(idx) = line.find("Download speed:") {
        let after = line[idx + "Download speed:".len()..].trim();
        let mut iter = after.split_whitespace();
        let num = iter.next().and_then(|s| s.parse::<f64>().ok());
        let unit = iter.next().unwrap_or("MiB/s");
        if let Some(n) = num {
            let mul = unit_multiplier(unit.trim_end_matches("/s"));
            delta.speed_bps = Some((n * mul) as u64);
            matched = true;
        }
    }

    if matched {
        Some(delta)
    } else {
        None
    }
}

fn unit_multiplier(unit: &str) -> f64 {
    match unit {
        "B" => 1.0,
        "KB" => 1_000.0,
        "MB" => 1_000_000.0,
        "GB" => 1_000_000_000.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        _ => 1024.0 * 1024.0, // sensible default — legendary prefers MiB
    }
}

fn parse_hms_to_secs(s: &str) -> Option<u32> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let h = parts[0].parse::<u32>().ok()?;
    let m = parts[1].parse::<u32>().ok()?;
    let sec = parts[2].parse::<u32>().ok()?;
    Some(h * 3600 + m * 60 + sec)
}

/// Run `legendary uninstall <app_name> --yes`. Removes both the on-disk files
/// AND legendary's own metadata for the install — so a subsequent re-install
/// starts cleanly (no "resume from a non-existent dir"). Synchronous; caller
/// should wrap in `spawn_blocking`.
pub fn uninstall_blocking(legendary_exe: &PathBuf, app_name: &str) -> Result<(), String> {
    let mut cmd = Command::new(legendary_exe);
    cmd.args(["uninstall", app_name, "--yes"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("Failed to run legendary uninstall: {}", e))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "legendary uninstall exited {} — {}",
            out.status,
            stderr.trim()
        ));
    }
    Ok(())
}

/// Heuristic — true when a legendary line announces install completion. Used
/// to flip the orchestrator state even before the process exits.
pub fn is_completion_line(line: &str) -> bool {
    line.contains("Finished installation")
        || line.contains("Game has been successfully installed")
        || line.contains("Verification finished successfully")
}
