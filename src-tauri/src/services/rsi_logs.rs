//! Star Citizen playtime calculator — port of TradSC `app_stats.rs`.
//!
//! The RSI Launcher writes a fresh `Game.log` to `<install>/logbackups/`
//! every time the game closes. Each log file's body is timestamped on
//! every line; the first and last timestamps bracket exactly one
//! play-session. Summing `(last - first)` across every backup file gives
//! the total time the user spent IN-GAME (not just with the launcher
//! open). Reference: `cerveau/startrad/wiki/Context/Fonctionnalités/
//! Statistiques de jeu.md`.
//!
//! Log line shape: `<2024-01-15T14:30:00.123456Z> ...`. We capture the
//! first 19 chars after `<` as a `NaiveDateTime`.
//!
//! Sanity filters (same as TradSC):
//! - Sessions <1 min are noise (crashed launches, alt-tab race).
//! - Sessions >24h almost always mean the user left the game on
//!   overnight — counted as 24h would inflate stats; we drop them
//!   instead so the total stays honest.

use chrono::NaiveDateTime;
use regex::Regex;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Channels the RSI Launcher installs side-by-side. Each one has its
/// own `logbackups` folder we need to walk.
const CHANNELS: &[&str] = &["LIVE", "PTU", "EPTU", "TECH-PREVIEW", "HOTFIX"];

static TS_RE: OnceLock<Regex> = OnceLock::new();

fn ts_regex() -> &'static Regex {
    TS_RE.get_or_init(|| Regex::new(r"^<(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})").unwrap())
}

/// Find every `<root>/StarCitizen/<channel>` directory that exists,
/// starting from the RSI Launcher's parent (e.g. the user's library
/// row points at `…/RSI Launcher/RSI Launcher.exe` → root is
/// `…/Roberts Space Industries/`). Falls back to the common Windows
/// install locations when the launcher row gives no hint.
pub fn find_starcitizen_installs(launcher_exe_path: Option<&str>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();

    // 1) Derive from the launcher exe path. `<X>/RSI Launcher/RSI Launcher.exe`
    //    → roots `<X>/StarCitizen/<channel>/`.
    if let Some(exe) = launcher_exe_path {
        if let Some(p) = Path::new(exe).parent().and_then(|p| p.parent()) {
            roots.push(p.to_path_buf());
        }
    }

    // 2) Standard Windows locations — covers users who don't have a
    //    launcher row in Tokoru yet but might've installed SC
    //    through the default wizard.
    for base in [
        "C:\\Program Files\\Roberts Space Industries",
        "D:\\Program Files\\Roberts Space Industries",
        "C:\\Games\\Roberts Space Industries",
        "D:\\Games\\Roberts Space Industries",
        "E:\\Games\\Roberts Space Industries",
    ] {
        let p = PathBuf::from(base);
        if p.exists() && !roots.iter().any(|r| r == &p) {
            roots.push(p);
        }
    }

    let mut installs: Vec<PathBuf> = Vec::new();
    for root in roots {
        for chan in CHANNELS {
            let p = root.join("StarCitizen").join(chan);
            if p.exists() && p.join("logbackups").exists() {
                installs.push(p);
            }
        }
    }
    installs
}

/// Total minutes recorded across every `.log` in a single channel's
/// `logbackups/`. Returns `(minutes, session_count)`. Sessions <1 min
/// or >24h are filtered out — same heuristic as TradSC.
pub fn calculate_playtime_minutes(install_path: &Path) -> (i64, u32) {
    let logbackups = install_path.join("logbackups");
    let Ok(entries) = fs::read_dir(&logbackups) else {
        return (0, 0);
    };
    let re = ts_regex();
    let mut total_minutes: i64 = 0;
    let mut session_count: u32 = 0;

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() || path.extension().map_or(true, |e| e != "log") {
            continue;
        }
        let Some((start, end)) = extract_session_timestamps(&path, re) else {
            continue;
        };
        let minutes = end.signed_duration_since(start).num_minutes();
        if minutes > 0 && minutes < 24 * 60 {
            total_minutes += minutes;
            session_count += 1;
        }
    }
    (total_minutes, session_count)
}

/// First and last timestamps in one log file — they bracket exactly
/// one session because the launcher rotates the log on every game
/// close. Skips lines that don't start with `<` (free-form messages).
fn extract_session_timestamps(
    log_path: &Path,
    re: &Regex,
) -> Option<(NaiveDateTime, NaiveDateTime)> {
    let file = fs::File::open(log_path).ok()?;
    let reader = BufReader::new(file);
    let mut first: Option<NaiveDateTime> = None;
    let mut last: Option<NaiveDateTime> = None;
    for line in reader.lines().filter_map(|l| l.ok()) {
        if !line.starts_with('<') {
            continue;
        }
        let Some(caps) = re.captures(&line) else { continue };
        let Some(ts) = caps.get(1) else { continue };
        if let Ok(dt) = NaiveDateTime::parse_from_str(ts.as_str(), "%Y-%m-%dT%H:%M:%S") {
            if first.is_none() {
                first = Some(dt);
            }
            last = Some(dt);
        }
    }
    Some((first?, last?))
}

/// Sum the per-channel totals into one big "Star Citizen playtime" in
/// seconds — what we'll write to `games.imported_playtime_seconds`.
pub fn total_playtime_seconds(launcher_exe_path: Option<&str>) -> i64 {
    find_starcitizen_installs(launcher_exe_path)
        .iter()
        .map(|p| calculate_playtime_minutes(p).0)
        .sum::<i64>()
        .saturating_mul(60)
}

/// Most-recent log modification time across every channel — unix
/// seconds, or 0 when no logs exist. Used to feed
/// `last_played_imported` so the "Recently Played" filter shows SC
/// even when the user played outside Tokoru's watcher window.
pub fn last_played_timestamp(launcher_exe_path: Option<&str>) -> i64 {
    let mut latest: i64 = 0;
    for install in find_starcitizen_installs(launcher_exe_path) {
        let logbackups = install.join("logbackups");
        let Ok(entries) = fs::read_dir(&logbackups) else { continue };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Ok(meta) = path.metadata() {
                if let Ok(modified) = meta.modified() {
                    if let Ok(epoch) = modified.duration_since(std::time::UNIX_EPOCH) {
                        let secs = epoch.as_secs() as i64;
                        if secs > latest {
                            latest = secs;
                        }
                    }
                }
            }
        }
    }
    latest
}
