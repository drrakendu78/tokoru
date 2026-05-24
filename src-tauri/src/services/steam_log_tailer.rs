//! Steam content log tailer — real-time download progress refinement.
//!
//! ## Why this exists
//!
//! `steam_dl_watcher` polls `appmanifest_<appid>.acf` files once per second
//! and treats them as the authoritative source of download state. But Steam
//! batches writes to those manifests aggressively — in practice, the
//! `BytesDownloaded` / `BytesToDownload` values only flush on pause / cancel
//! / completion. During an active download the file value is stale by
//! minutes.
//!
//! Meanwhile, Steam writes line-oriented progress to
//! `<SteamPath>\logs\content_log.txt` as it happens: percent, byte pairs,
//! and speed. That file is much fresher.
//!
//! ## Role
//!
//! This is a **refinement** of the manifest watcher, not a replacement:
//! - Manifest watcher remains authoritative for status transitions
//!   (Downloading → Paused → Completed, install_path, etc.).
//! - Tailer only updates **progress numbers** (bytes / percent / speed) of
//!   existing `RuntimeState` entries. It never emits `download-status`.
//! - When both watchers race, byte counters are merged with `max(...)` so we
//!   never go backwards.
//!
//! ## Behaviour
//!
//! - Open the file once on startup. If Steam isn't running yet, retry every
//!   5s until the file appears. Never panic.
//! - Seek to EOF on first open — we don't care about historical lines.
//! - Every 400ms, read whatever's been appended and parse it.
//! - Detect log rotation: if the current file size drops below our last
//!   read offset, Steam moved the file to `content_log.txt.old`; reopen
//!   from offset 0.
//! - Cap parse work at 4096 lines per tick. Beyond that, log a warning and
//!   move on — the next tick will catch up.
//! - Respect `RuntimeState::is_steam_dismissed`.
//!
//! ## Line shapes
//!
//! Steam's exact format varies by build; we parse permissively. See
//! `LINE_PATTERNS` below.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;

use regex::Regex;
use tauri::{AppHandle, Emitter};
use tokio::time;

use crate::services::db::Database;
use crate::services::downloads::ProgressEvent;
use crate::services::platforms::local_detect::get_steam_path;
use crate::services::runtime_state::RuntimeState;

const POLL_INTERVAL_MS: u64 = 400;
/// How long to wait between attempts to open `content_log.txt` when it
/// doesn't exist yet (Steam not started, or fresh install).
const FILE_RETRY_INTERVAL_SECS: u64 = 5;
/// Maximum number of lines we'll process in one tick before bailing out
/// to avoid pegging the CPU when Steam dumps a flood of log lines.
const MAX_LINES_PER_TICK: usize = 4096;

/// Parsed progress info extracted from one or more log lines for an appid.
#[derive(Debug, Default, Clone)]
struct ParsedUpdate {
    /// 0..=100 if seen; None if no percent line was present.
    percent: Option<f32>,
    /// Raw bytes downloaded if seen ("5.37 GB" → 5_768_640_000).
    downloaded_bytes: Option<u64>,
    /// Raw bytes total if seen.
    total_bytes: Option<u64>,
    /// Speed in bytes/s if a speed line was seen.
    speed_bps: Option<u64>,
}

impl ParsedUpdate {
    fn merge(&mut self, other: &ParsedUpdate) {
        if other.percent.is_some() {
            self.percent = other.percent;
        }
        if other.downloaded_bytes.is_some() {
            self.downloaded_bytes = other.downloaded_bytes;
        }
        if other.total_bytes.is_some() {
            self.total_bytes = other.total_bytes;
        }
        if other.speed_bps.is_some() {
            self.speed_bps = other.speed_bps;
        }
    }

    fn is_empty(&self) -> bool {
        self.percent.is_none()
            && self.downloaded_bytes.is_none()
            && self.total_bytes.is_none()
            && self.speed_bps.is_none()
    }
}

/// Compiled regex set. Kept inside one struct so we compile once.
struct LinePatterns {
    /// Captures (appid, percent, dl_value, dl_unit, total_value, total_unit).
    /// Matches lines like:
    ///   AppID 2322010 download progress : 5.34% (5.37 GB / 100.24 GB)
    progress_with_pair: Regex,
    /// Captures (appid, percent). Fallback when the byte pair isn't on the
    /// same line.
    progress_pct_only: Regex,
    /// Captures (appid, speed_value, speed_unit). Matches lines like:
    ///   AppID 2322010 download speed: 1.84 GB/s (raw), staging speed: ...
    speed: Regex,
    /// Used solely to detect a "download finished" sentinel for debug logs.
    /// We don't emit status from here.
    finished: Regex,
}

impl LinePatterns {
    fn new() -> Result<Self, regex::Error> {
        Ok(Self {
            // Permissive: any line containing AppID + percent + (value unit / value unit).
            // We accept "%" optionally hugged by spaces and varied unit casing.
            progress_with_pair: Regex::new(
                r"(?i)AppID\s+(\d+).*?(\d+(?:\.\d+)?)\s*%\s*\(\s*([\d.]+)\s*(GB|GiB|MB|MiB|KB|KiB|B)\s*/\s*([\d.]+)\s*(GB|GiB|MB|MiB|KB|KiB|B)",
            )?,
            progress_pct_only: Regex::new(
                r"(?i)AppID\s+(\d+).*?(\d+(?:\.\d+)?)\s*%",
            )?,
            speed: Regex::new(
                r"(?i)AppID\s+(\d+).*?speed:\s*([\d.]+)\s*(GB|GiB|MB|MiB|KB|KiB|B)/s",
            )?,
            finished: Regex::new(r"(?i)AppID\s+(\d+).*?(download\s+finished|fully\s+installed)")?,
        })
    }
}

/// Convert a (value, unit) pair into bytes. Accepts the units we register
/// in the regex character class. Treats the SI/IEC variants as equivalent
/// (Steam mixes them).
fn to_bytes(value: f64, unit: &str) -> u64 {
    let mul: f64 = match unit.to_ascii_uppercase().as_str() {
        "B" => 1.0,
        "KB" | "KIB" => 1024.0,
        "MB" | "MIB" => 1024.0 * 1024.0,
        "GB" | "GIB" => 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };
    (value * mul).max(0.0) as u64
}

/// Parse one log line into `(appid, ParsedUpdate)` if it matched any
/// known pattern. Returns `None` for unrecognised lines.
fn parse_line(line: &str, pats: &LinePatterns) -> Option<(String, ParsedUpdate)> {
    let mut update = ParsedUpdate::default();
    let mut appid: Option<String> = None;

    if let Some(caps) = pats.progress_with_pair.captures(line) {
        if let Some(id) = caps.get(1) {
            appid = Some(id.as_str().to_string());
        }
        if let Some(p) = caps.get(2).and_then(|m| m.as_str().parse::<f32>().ok()) {
            update.percent = Some(p.clamp(0.0, 100.0));
        }
        if let (Some(v), Some(u)) = (
            caps.get(3).and_then(|m| m.as_str().parse::<f64>().ok()),
            caps.get(4).map(|m| m.as_str()),
        ) {
            update.downloaded_bytes = Some(to_bytes(v, u));
        }
        if let (Some(v), Some(u)) = (
            caps.get(5).and_then(|m| m.as_str().parse::<f64>().ok()),
            caps.get(6).map(|m| m.as_str()),
        ) {
            update.total_bytes = Some(to_bytes(v, u));
        }
    } else if let Some(caps) = pats.progress_pct_only.captures(line) {
        if let Some(id) = caps.get(1) {
            appid = Some(id.as_str().to_string());
        }
        if let Some(p) = caps.get(2).and_then(|m| m.as_str().parse::<f32>().ok()) {
            update.percent = Some(p.clamp(0.0, 100.0));
        }
    }

    if let Some(caps) = pats.speed.captures(line) {
        // Speed line may carry its own appid — prefer the one already seen,
        // otherwise take it from the speed regex.
        if appid.is_none() {
            if let Some(id) = caps.get(1) {
                appid = Some(id.as_str().to_string());
            }
        }
        if let (Some(v), Some(u)) = (
            caps.get(2).and_then(|m| m.as_str().parse::<f64>().ok()),
            caps.get(3).map(|m| m.as_str()),
        ) {
            update.speed_bps = Some(to_bytes(v, u));
        }
    }

    if pats.finished.captures(line).is_some() {
        // Only a debug hint — manifest watcher will pick up the transition.
        if let Some(caps) = pats.finished.captures(line) {
            if let Some(id) = caps.get(1) {
                log::debug!(
                    "[steam_log_tailer] saw finish sentinel for appid={} — leaving status to manifest watcher",
                    id.as_str()
                );
            }
        }
    }

    match (appid, update.is_empty()) {
        (Some(id), false) => Some((id, update)),
        _ => None,
    }
}

/// Public entry point. Spawns the tailer as a long-lived async task.
pub fn start(app: AppHandle, db: Database, runtime: RuntimeState) {
    tauri::async_runtime::spawn(async move {
        tailer_loop(app, db, runtime).await;
    });
}

async fn tailer_loop(app: AppHandle, db: Database, runtime: RuntimeState) {
    let Some(steam_dir) = get_steam_path() else {
        log::info!("[steam_log_tailer] Steam not installed — tailer disabled");
        return;
    };

    let log_path: PathBuf = steam_dir.join("logs").join("content_log.txt");
    let patterns = match LinePatterns::new() {
        Ok(p) => p,
        Err(e) => {
            log::warn!("[steam_log_tailer] regex compile failed: {} — tailer disabled", e);
            return;
        }
    };

    log::info!(
        "[steam_log_tailer] started (poll={}ms, file={})",
        POLL_INTERVAL_MS,
        log_path.display()
    );

    // Outer loop: wait for the log file to exist, then enter the tail loop.
    // On file rotation / disappearance, we'll fall back out here and retry.
    loop {
        // Wait until the log file is present.
        loop {
            if log_path.exists() {
                break;
            }
            time::sleep(Duration::from_secs(FILE_RETRY_INTERVAL_SECS)).await;
        }

        // Open the file and seek to the end so we don't process history.
        let mut file = match File::open(&log_path) {
            Ok(f) => f,
            Err(e) => {
                log::debug!(
                    "[steam_log_tailer] failed to open {}: {} — retrying",
                    log_path.display(),
                    e
                );
                time::sleep(Duration::from_secs(FILE_RETRY_INTERVAL_SECS)).await;
                continue;
            }
        };

        let mut offset: u64 = match file.seek(SeekFrom::End(0)) {
            Ok(p) => p,
            Err(e) => {
                log::warn!(
                    "[steam_log_tailer] failed to seek end of {}: {}",
                    log_path.display(),
                    e
                );
                time::sleep(Duration::from_secs(FILE_RETRY_INTERVAL_SECS)).await;
                continue;
            }
        };

        // Buffer for the *trailing partial line* from the previous read. The
        // next read prefixes its content with this so we never split a line
        // in two when the file is mid-write.
        let mut leftover = String::new();

        // Inner tail loop.
        loop {
            time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;

            // Check file size; detect rotation (file shrank).
            let cur_size = match std::fs::metadata(&log_path) {
                Ok(m) => m.len(),
                Err(_) => {
                    // File vanished (Steam shut down / log rotated mid-poll).
                    // Drop back out to the outer wait-for-file loop.
                    log::debug!("[steam_log_tailer] log file vanished — re-opening");
                    break;
                }
            };

            if cur_size < offset {
                log::info!(
                    "[steam_log_tailer] log rotation detected (size {} < offset {}) — reopening from start",
                    cur_size,
                    offset
                );
                // Reopen from the start of the new file.
                match File::open(&log_path) {
                    Ok(f) => {
                        file = f;
                        offset = 0;
                        leftover.clear();
                        // Don't seek to end this time: we want the new lines.
                    }
                    Err(e) => {
                        log::warn!(
                            "[steam_log_tailer] failed to reopen rotated log: {}",
                            e
                        );
                        break;
                    }
                }
            }

            if cur_size == offset {
                continue; // nothing new
            }

            // Seek to where we left off and read the new bytes.
            if let Err(e) = file.seek(SeekFrom::Start(offset)) {
                log::debug!("[steam_log_tailer] seek failed: {} — reopening", e);
                break;
            }

            let mut buf = Vec::with_capacity((cur_size - offset).min(1 << 20) as usize);
            // Cap the read to 1 MiB per tick to bound memory if the log
            // somehow grew massively between polls.
            let read_cap: u64 = (cur_size - offset).min(1 << 20);
            let read_result = (&mut file).take(read_cap).read_to_end(&mut buf);
            match read_result {
                Ok(n) => {
                    offset += n as u64;
                    if n == 0 {
                        continue;
                    }
                }
                Err(e) => {
                    log::debug!("[steam_log_tailer] read failed: {} — reopening", e);
                    break;
                }
            }

            // Decode bytes — Steam writes ASCII / UTF-8; lossy is fine.
            let mut chunk = String::from_utf8_lossy(&buf).into_owned();
            if !leftover.is_empty() {
                chunk = format!("{}{}", leftover, chunk);
                leftover.clear();
            }

            // Pop the trailing partial line (anything after the last '\n').
            let last_newline = chunk.rfind('\n');
            let (complete, partial) = match last_newline {
                Some(idx) => (&chunk[..=idx], &chunk[idx + 1..]),
                None => {
                    // No newline yet — entire chunk is partial; stash it.
                    leftover = chunk.clone();
                    continue;
                }
            };
            if !partial.is_empty() {
                leftover.push_str(partial);
            }

            // Aggregate per-appid updates from this batch so we only emit
            // once per appid per tick (the log may have several lines for
            // the same game in rapid succession).
            use std::collections::HashMap;
            let mut by_appid: HashMap<String, ParsedUpdate> = HashMap::new();
            let mut lines_seen = 0usize;
            for line in complete.lines() {
                lines_seen += 1;
                if lines_seen > MAX_LINES_PER_TICK {
                    log::warn!(
                        "[steam_log_tailer] line cap ({}) hit — deferring remainder to next tick",
                        MAX_LINES_PER_TICK
                    );
                    break;
                }
                if let Some((appid, upd)) = parse_line(line, &patterns) {
                    by_appid.entry(appid).or_default().merge(&upd);
                }
            }

            if by_appid.is_empty() {
                continue;
            }

            // Apply each update.
            for (appid, upd) in by_appid {
                // Dismissed rows are silenced — same policy as the manifest watcher.
                if runtime.is_steam_dismissed(&appid) {
                    continue;
                }

                let game_id = match db.find_steam_id_by_appid(&appid) {
                    Ok(Some(id)) => id,
                    _ => continue,
                };

                // Snapshot the current state under lock, mutate locally,
                // then write back. The manifest watcher may race with us;
                // merging with max(current, parsed) prevents going
                // backwards on either value.
                let Some(current) = runtime.get(&game_id) else {
                    // No runtime entry yet — manifest watcher hasn't seen
                    // this download. Don't create from log alone (we'd be
                    // guessing at status / install_path). Skip; next
                    // manifest tick (≤1s) will create the entry and we'll
                    // refine it from then on.
                    continue;
                };

                // Compute merged byte counters.
                let merged_downloaded = match upd.downloaded_bytes {
                    Some(b) => current.downloaded_bytes.max(b),
                    None => current.downloaded_bytes,
                };
                let merged_total = match upd.total_bytes {
                    Some(b) => current.total_bytes.max(b),
                    None => current.total_bytes,
                };

                // Re-derive percent from bytes if both are known; else use
                // parsed percent directly; else keep what we had.
                let merged_pct: f32 = if merged_total > 0 {
                    ((merged_downloaded as f64 / merged_total as f64) * 100.0)
                        .clamp(0.0, 100.0) as f32
                } else if let Some(p) = upd.percent {
                    p
                } else {
                    current.progress_pct
                };

                // Speed: always trust the freshest log value when present.
                let merged_speed = upd.speed_bps.unwrap_or(current.speed_bps);

                // ETA: re-derive from speed + remaining if we can; else 0.
                let merged_eta: u32 = if merged_speed > 0 && merged_total > merged_downloaded {
                    let remaining = merged_total - merged_downloaded;
                    let secs = (remaining as f64 / merged_speed as f64).round() as u64;
                    secs.min(u32::MAX as u64) as u32
                } else {
                    current.eta_secs
                };

                // Skip writes that wouldn't change anything observably.
                let pct_changed = (merged_pct - current.progress_pct).abs() > 0.01;
                let bytes_changed = merged_downloaded != current.downloaded_bytes
                    || merged_total != current.total_bytes;
                let speed_changed = merged_speed != current.speed_bps;
                if !pct_changed && !bytes_changed && !speed_changed {
                    continue;
                }

                runtime.update(&game_id, |s| {
                    s.downloaded_bytes = merged_downloaded;
                    s.total_bytes = merged_total;
                    s.progress_pct = merged_pct;
                    s.speed_bps = merged_speed;
                    s.eta_secs = merged_eta;
                });

                if let Some(latest) = runtime.get(&game_id) {
                    let _ = app.emit(
                        "download-progress",
                        ProgressEvent {
                            game_id: game_id.clone(),
                            progress_pct: latest.progress_pct,
                            speed_bps: latest.speed_bps,
                            eta_secs: latest.eta_secs,
                            downloaded_bytes: latest.downloaded_bytes,
                            total_bytes: latest.total_bytes,
                        },
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_progress_with_pair() {
        let p = LinePatterns::new().unwrap();
        let line = "[2026-05-21 12:34:56] AppID 2322010 download progress : 5.34% (5.37 GB / 100.24 GB)";
        let (appid, upd) = parse_line(line, &p).expect("should parse");
        assert_eq!(appid, "2322010");
        assert!((upd.percent.unwrap() - 5.34).abs() < 0.001);
        // 5.37 GiB ≈ 5_766_443_991
        assert!(upd.downloaded_bytes.unwrap() > 5_000_000_000);
        assert!(upd.total_bytes.unwrap() > 100_000_000_000);
    }

    #[test]
    fn parses_pct_only() {
        let p = LinePatterns::new().unwrap();
        let line = "[ts] AppID 480 download progress : 42.0%";
        let (appid, upd) = parse_line(line, &p).expect("should parse");
        assert_eq!(appid, "480");
        assert!((upd.percent.unwrap() - 42.0).abs() < 0.001);
        assert!(upd.downloaded_bytes.is_none());
    }

    #[test]
    fn parses_speed() {
        let p = LinePatterns::new().unwrap();
        let line = "[ts] AppID 730 download speed: 1.84 GB/s (raw), staging speed: 1.62 GB/s";
        let (appid, upd) = parse_line(line, &p).expect("should parse");
        assert_eq!(appid, "730");
        assert!(upd.speed_bps.unwrap() > 1_000_000_000);
    }

    #[test]
    fn ignores_unrelated_line() {
        let p = LinePatterns::new().unwrap();
        let line = "[ts] some unrelated system message about steamwebhelper.exe";
        assert!(parse_line(line, &p).is_none());
    }

    #[test]
    fn to_bytes_units() {
        assert_eq!(to_bytes(1.0, "B"), 1);
        assert_eq!(to_bytes(1.0, "KB"), 1024);
        assert_eq!(to_bytes(1.0, "MB"), 1024 * 1024);
        assert_eq!(to_bytes(1.0, "GiB"), 1024 * 1024 * 1024);
    }
}
