//! Thin wrapper around the `gogdl` CLI for GOG installs.
//!
//! Auth tokens are already written by the GOG login flow (`commands::gog`)
//! into `<bin_dir>/gogdl-auth.json` via `services::gog_api::write_gogdl_auth`
//! — we just need to point gogdl at that file with `--auth-config-path`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use tauri::AppHandle;

use crate::services::cli_bins;

/// Resolve the gogdl.exe path, downloading it if missing.
pub async fn ensure_exe(app: &AppHandle) -> Result<PathBuf, String> {
    cli_bins::ensure_gogdl(app).await
}

/// Path to the auth.json file that `services::gog_api` writes after a
/// successful OAuth login. We pass this to every gogdl invocation via
/// `--auth-config-path`.
pub fn auth_path(app: &AppHandle) -> Result<PathBuf, String> {
    // gog_api writes to `<bin_dir>/gogdl-auth.json` — same filename gog_api uses.
    let exe = cli_bins::gogdl_path(app)?;
    let parent = exe
        .parent()
        .ok_or_else(|| "gogdl exe has no parent dir".to_string())?;
    Ok(parent.join("gogdl-auth.json"))
}

/// Spawn `gogdl download <game_id> ...` as a child process. gogdl writes
/// progress to stderr in carriage-return chunks (in-place updates), so the
/// orchestrator reads byte-by-byte and splits on both `\n` and `\r`.
///
/// gogdl quirk: `--auth-config-path` MUST come before the subcommand.
pub fn spawn_install(
    gogdl_exe: &PathBuf,
    auth_path: &Path,
    game_id: &str,
    install_path: &str,
    language: Option<&str>,
) -> Result<Child, String> {
    let lang = language.unwrap_or("en");
    let auth_str = auth_path.to_string_lossy().to_string();

    let args: Vec<String> = vec![
        "--auth-config-path".to_string(),
        auth_str,
        "download".to_string(),
        game_id.to_string(),
        "--platform".to_string(),
        "windows".to_string(),
        "--path".to_string(),
        install_path.to_string(),
        "--lang".to_string(),
        lang.to_string(),
    ];

    log::info!(
        "[gogdl] spawn install: {} {}",
        gogdl_exe.display(),
        args.join(" ")
    );

    let mut cmd = Command::new(gogdl_exe);
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        // CREATE_NEW_PROCESS_GROUP enables CTRL_BREAK_EVENT delivery during
        // pause so gogdl can write its `.gogdl-manifest.json` checkpoint
        // before exiting (otherwise taskkill /F nukes it mid-write).
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn()
        .map_err(|e| format!("Failed to spawn gogdl download: {}", e))
}

/// Parsed progress info. Same shape as `legendary::ProgressDelta`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProgressDelta {
    pub progress_pct: Option<f32>,
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub speed_bps: Option<u64>,
    pub eta_secs: Option<u32>,
}

/// gogdl V2 progress format examples:
/// ```text
/// = Progress: 19.13 13658081053/71381223347, Running for: 00:00:49, ETA: 00:03:29
/// + Download    - 150.11 MiB/s (raw) / 161.52 MiB/s (decompressed)
/// = Downloaded: 12416.29 MiB, Written: 13025.36 MiB
/// ```
pub fn parse_progress_line(line: &str) -> Option<ProgressDelta> {
    let mut delta = ProgressDelta::default();
    let mut matched = false;

    // ── Combined "= Progress: <pct> <dl>/<total>, ..., ETA: HH:MM:SS"
    if line.contains("Progress:") && line.contains("ETA:") {
        let parts: Vec<&str> = line.split_whitespace().collect();
        for (i, part) in parts.iter().enumerate() {
            if *part == "Progress:" {
                if let Some(pct_str) = parts.get(i + 1) {
                    if let Ok(pct) = pct_str.parse::<f32>() {
                        delta.progress_pct = Some(pct);
                        matched = true;
                    }
                }
                if let Some(ratio) = parts.get(i + 2) {
                    let cleaned = ratio.trim_end_matches(',');
                    if let Some((dl, total)) = cleaned.split_once('/') {
                        if let (Ok(d), Ok(t)) = (dl.parse::<u64>(), total.parse::<u64>()) {
                            // gogdl reports raw bytes, not MiB — keep as bytes.
                            delta.downloaded_bytes = Some(d);
                            delta.total_bytes = Some(t);
                            matched = true;
                        }
                    }
                }
            }
            if *part == "ETA:" {
                if let Some(eta) = parts.get(i + 1) {
                    if let Some(secs) = parse_hms_to_secs(eta) {
                        delta.eta_secs = Some(secs);
                        matched = true;
                    }
                }
            }
        }
    }

    // ── Speed line: "+ Download - 150.11 MiB/s (raw)"
    if line.contains("Download") && line.contains("MiB/s") {
        let parts: Vec<&str> = line.split_whitespace().collect();
        for (i, part) in parts.iter().enumerate() {
            if *part == "-" {
                if let Some(spd) = parts.get(i + 1) {
                    if let Ok(n) = spd.parse::<f64>() {
                        // gogdl always reports MiB/s in this position.
                        delta.speed_bps = Some((n * 1024.0 * 1024.0) as u64);
                        matched = true;
                    }
                }
            }
        }
    }

    // ── Downloaded counter: "= Downloaded: 12416.29 MiB, ..."
    if line.contains("Downloaded:") && line.contains("MiB") && delta.downloaded_bytes.is_none() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        for (i, part) in parts.iter().enumerate() {
            if *part == "Downloaded:" {
                if let Some(dl_str) = parts.get(i + 1) {
                    if let Ok(dl) = dl_str.parse::<f64>() {
                        delta.downloaded_bytes = Some((dl * 1024.0 * 1024.0) as u64);
                        matched = true;
                    }
                }
            }
        }
    }

    if matched {
        Some(delta)
    } else {
        None
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

/// Heuristic — true when a gogdl line announces successful completion.
pub fn is_completion_line(line: &str) -> bool {
    line.contains("All files have been downloaded")
        || line.contains("Verification finished")
        || line.contains("Finished installing")
}

/// Walk the given base directories looking for `goggame-<id>.info` files —
/// the marker GOG installs drop at their install root. Returns one
/// (game_id, install_path) pair per distinct gameid found.
///
/// gogdl has no central installs.json (unlike legendary), so the only source
/// of truth post-install is on disk. Caller passes the user's configured GOG
/// download dirs (per-source override + default_dir) so we don't crawl the
/// whole filesystem.
///
/// Bounded to depth 3 to keep scans of large drives sub-second. Symlinks
/// are not followed (otherwise a single `C:` shortcut would explode the
/// scan). If the same gameid is found twice, the first match (alphabetical
/// order) wins.
pub fn scan_installs(base_dirs: &[String]) -> Vec<(String, String)> {
    use std::collections::HashMap;

    let mut by_id: HashMap<String, String> = HashMap::new();

    for base in base_dirs {
        if base.is_empty() {
            continue;
        }
        let base_path = std::path::Path::new(base);
        if !base_path.exists() {
            continue;
        }

        // `sort_by_file_name` gives us the alphabetical-wins behaviour for
        // duplicate ids without an extra sort pass on the output.
        for entry in walkdir::WalkDir::new(base_path)
            .max_depth(3)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .flatten()
        {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.starts_with("goggame-") || !name.ends_with(".info") {
                continue;
            }
            // `goggame-<gameid>.info` → strip the prefix + suffix.
            let gameid = &name["goggame-".len()..name.len() - ".info".len()];
            if gameid.is_empty() {
                continue;
            }
            let Some(parent) = p.parent() else {
                continue;
            };
            by_id
                .entry(gameid.to_string())
                .or_insert_with(|| parent.to_string_lossy().to_string());
        }
    }

    by_id.into_iter().collect()
}
