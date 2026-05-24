//! Steam download progress watcher.
//!
//! Steam owns the downloader for `source = "steam"` games — we can't pause /
//! cancel / resume through us. But we can **mirror** its progress into the
//! same `RuntimeState` + `download-progress` / `download-status` events the
//! Epic/GOG runners use, so the rest of the app (GameCard, Downloads page,
//! GameDetail) doesn't need a separate code path for Steam-source rows.
//!
//! How it works:
//! 1. Every `TICK_INTERVAL_SECS`, scan `<SteamPath>\steamapps\` + every
//!    library folder listed in `libraryfolders.vdf` for `appmanifest_<id>.acf`
//!    files.
//! 2. Parse `StateFlags`, `BytesDownloaded`, `BytesToDownload`, `name`,
//!    `installdir` from each manifest (text VDF / KeyValues1 — a quick
//!    string scan, no full parser required).
//! 3. For each manifest currently downloading, look up the matching game in
//!    our DB by `(source='steam', platform_id=appid)`; if found, upsert a
//!    `DownloadState` keyed on our DB id and emit the same events Epic/GOG
//!    emit. Speed and ETA are derived from byte deltas between ticks (we
//!    cache the previous reading).
//! 4. When a manifest transitions from "downloading" to "fully installed"
//!    (StateFlags & 4, no pending bytes), or disappears between ticks,
//!    flip the entry to `Completed` (and update `install_path`).
//! 5. If Steam isn't installed or `libraryfolders.vdf` is missing, log once
//!    and exit — never panic, never retry endlessly.
//!
//! Frontend controls (Pause/Cancel/Resume/Retry) are hidden for
//! `source === "steam"` cards because Steam owns the file lifecycle and the
//! state belongs to its UI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;
use tokio::time;

use crate::services::db::Database;
use crate::services::downloads::{ProgressEvent, StatusEvent};
use crate::services::platforms::local_detect::{find_steam_libraries, get_steam_path};
use crate::services::runtime_state::{DownloadState, DownloadStatus, RuntimeState};

const TICK_INTERVAL_MS: u64 = 1000;

// Steam StateFlags bitmask values we care about. See module doc and the
// public references on the ACF format; we only use bits we trust.
const STATE_INSTALLED: u32 = 4;
const STATE_UPDATE_REQUIRED: u32 = 8;
const STATE_UPDATING: u32 = 32;
const STATE_DOWNLOADING_FIRST_INSTALL: u32 = 1024;

/// Snapshot of the relevant fields from one `appmanifest_<id>.acf`.
#[derive(Debug, Clone)]
struct AcfSnapshot {
    appid: String,
    name: String,
    installdir: String,
    state_flags: u32,
    /// Cooked "display" bytes — combined dl_ratio + stage_ratio against the
    /// network total. Used as the historical `snap.bytes_downloaded` so
    /// downstream logic (completion detection, etc.) keeps working.
    bytes_downloaded: u64,
    bytes_to_download: u64,
    /// Raw ACF `BytesDownloaded` value (compressed network bytes received,
    /// as Steam itself accounts them). Stable between flushes — used as the
    /// baseline anchor for the kernel-counter extrapolation. We MUST track
    /// this separately from `bytes_downloaded` because the latter wobbles
    /// every tick (live_staged grows continuously) which would invalidate
    /// the counter baseline.
    raw_dl: u64,
    /// Raw ACF `BytesToStage` value (total decompressed install size).
    /// Used with `bytes_to_download` to estimate the compressed:decompressed
    /// ratio so we can scale the kernel counter delta (which captures TOTAL
    /// disk writes, compressed + decompressed) down to "compressed bytes
    /// received" — what users expect to see in a download progress bar.
    raw_to_stage: u64,
    /// Raw ACF `BytesStaged` (decompressed bytes written to the install dir
    /// so far, as Steam itself accounts them). Combined with `raw_dl` and
    /// the kernel counter we can extrapolate BOTH phases in real time and
    /// take the min — matching Steam's own UI which shows the slower phase.
    raw_staged: u64,
    /// Absolute path to the `<library>\steamapps` directory the manifest lives in.
    steamapps_dir: PathBuf,
}

/// (Kept for reference but no longer called — chunk-dir size over-counts
/// when Steam holds compressed + decompressed data briefly. Manifest-only
/// is more reliable. See `parse_acf` for context.)
#[allow(dead_code)]
fn live_bytes_downloaded(steamapps_dir: &Path, appid: &str, manifest_value: u64) -> u64 {
    let dl_dir = steamapps_dir.join("downloading").join(appid);
    let live = match std::fs::read_dir(&dl_dir) {
        Ok(_) => recursive_dir_size(&dl_dir),
        Err(_) => 0,
    };
    manifest_value.max(live)
}

/// Read a file with up to 5 retries (60ms between) — Steam holds the
/// manifest under a write lock during active download flushes, which makes
/// a single `read_to_string` fail spuriously. We just want SOMETHING
/// readable; if it still fails after the retries, return None and the
/// caller treats the manifest as gone.
fn read_with_retry(path: &Path) -> Option<String> {
    for attempt in 0..5 {
        match std::fs::read_to_string(path) {
            Ok(s) if !s.is_empty() => return Some(s),
            Ok(_) => {} // empty mid-write, retry
            Err(_) => {}
        }
        if attempt < 4 {
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
    }
    None
}

/// Recursively sum the size of every file under `root`. Symlinks aren't
/// followed (best-effort, ignore errors per-entry).
fn recursive_dir_size(root: &Path) -> u64 {
    let mut total: u64 = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                if let Ok(meta) = entry.metadata() {
                    total = total.saturating_add(meta.len());
                }
            }
        }
    }
    total
}

impl AcfSnapshot {
    /// Compute the on-disk install path Steam would use once the install completes.
    fn install_path(&self) -> PathBuf {
        self.steamapps_dir.join("common").join(&self.installdir)
    }

    /// True while Steam still has bytes to fetch (downloading, updating, or
    /// validating with pending downloads).
    fn is_downloading(&self) -> bool {
        let updating = self.state_flags & (STATE_UPDATING | STATE_DOWNLOADING_FIRST_INSTALL) != 0;
        let pending_update = self.state_flags & STATE_UPDATE_REQUIRED != 0;
        let pending_bytes =
            self.bytes_to_download > 0 && self.bytes_downloaded < self.bytes_to_download;
        updating || (pending_update && pending_bytes) || pending_bytes
    }

    /// True once Steam declares the install complete and no bytes are pending.
    fn is_installed(&self) -> bool {
        let installed_bit = self.state_flags & STATE_INSTALLED != 0;
        let bytes_done =
            self.bytes_to_download == 0 || self.bytes_downloaded >= self.bytes_to_download;
        installed_bit && bytes_done && !self.is_downloading()
    }
}

/// Per-appid cached previous reading used to derive speed_bps.
#[derive(Default)]
struct PrevTick {
    /// game_id -> (acf_bytes_downloaded, instant_of_that_observation).
    /// Updated ONLY when the ACF byte counter actually changes between ticks
    /// (Steam batches flushes every ~5-10s during active downloads, so most
    /// ticks read the same value as the last). Tracking the change instant
    /// lets us extrapolate bytes between flushes using the last known speed,
    /// instead of UI snapping every 10s. game_id is our DB id, not appid.
    inner: HashMap<String, (u64, Instant)>,
    /// game_id -> last known speed in bytes/sec, derived from the delta
    /// between two ACF flushes. Persists between ticks so extrapolation has
    /// a usable speed even on ticks where the ACF hasn't moved.
    last_known_speed: HashMap<String, u64>,
    /// Appids we knew about last tick. Used to detect "manifest disappeared"
    /// transitions (cancelled or removed in Steam) so we can finalise our
    /// RuntimeState entry.
    last_seen_appids: HashMap<String, String>, // appid -> game_id (our DB id)
    /// How many consecutive ticks an appid has been missing. We only treat
    /// the manifest as truly gone after N misses so a single locked-during-
    /// write tick doesn't kill the entry.
    miss_counts: HashMap<String, u32>,
    /// game_id -> steam.exe `WriteTransferCount` at the moment we last
    /// observed the ACF baseline for this download. Subtracting it from the
    /// current counter gives the bytes Steam has written to disk since the
    /// last flush — i.e. real-time progress that doesn't wait for Steam to
    /// flush the ACF. Required because Steam's "first install" flow holds the
    /// ACF at 0 for the entire download until completion.
    counter_at_acf_baseline: HashMap<String, u64>,
    /// Last observed (steam.exe write counter, instant). Used to derive
    /// instantaneous speed_bps from kernel I/O counter deltas. Global
    /// (single steam.exe) — when multiple games download concurrently the
    /// counter delta is shared across all of them and bytes are slightly
    /// over-attributed, but the typical case is one download at a time.
    last_steam_io: Option<(u64, Instant)>,
}

/// Read steam.exe's cumulative write byte counter from the OS kernel.
/// Returns `None` if steam.exe isn't running. This is the only live signal
/// we have during the "first install" pre-flush window when the ACF holds
/// `BytesDownloaded=0` even though Steam is actively writing chunks.
fn read_steam_write_bytes() -> Option<u64> {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_processes();
    sys.processes().values().find_map(|p| {
        let n = p.name().to_ascii_lowercase();
        if n == "steam.exe" || n == "steam" {
            Some(p.disk_usage().total_written_bytes)
        } else {
            None
        }
    })
}

/// Entry point: spawn the watcher loop. Returns immediately. Logs once and
/// exits if Steam isn't installed.
pub fn start(app: AppHandle, db: Database, runtime: RuntimeState) {
    tauri::async_runtime::spawn(async move {
        watcher_loop(app, db, runtime).await;
    });
}

async fn watcher_loop(app: AppHandle, db: Database, runtime: RuntimeState) {
    let Some(steam_dir) = get_steam_path() else {
        log::info!("[steam_dl_watcher] Steam not installed — watcher disabled");
        return;
    };
    log::info!(
        "[steam_dl_watcher] started ({}s tick, steam_dir={})",
        TICK_INTERVAL_MS,
        steam_dir.display()
    );

    let prev = Arc::new(Mutex::new(PrevTick::default()));

    loop {
        time::sleep(Duration::from_millis(TICK_INTERVAL_MS)).await;

        let libraries = find_steam_libraries(&steam_dir);
        let snapshots = scan_libraries(&libraries);

        if let Err(e) = tick(&app, &db, &runtime, &prev, snapshots).await {
            log::warn!("[steam_dl_watcher] tick failed: {}", e);
        }
    }
}

/// Walk every library's `steamapps` folder and parse every appmanifest file
/// found. Errors on individual files are logged at debug and skipped.
fn scan_libraries(libraries: &[PathBuf]) -> Vec<AcfSnapshot> {
    let mut out = Vec::new();
    for lib in libraries {
        let steamapps = lib.join("steamapps");
        let entries = match std::fs::read_dir(&steamapps) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if !(fname.starts_with("appmanifest_") && fname.ends_with(".acf")) {
                continue;
            }
            if let Some(snap) = parse_acf(&entry.path(), &steamapps) {
                out.push(snap);
            }
        }
    }
    out
}

/// Minimal ACF / KeyValues1 reader — pulls the five string-ish fields we
/// need. Robust against ordering and indentation; ignores everything else.
///
/// Steam locks the manifest briefly while it flushes during active downloads,
/// which causes `read_to_string` to fail transiently. Retry a few times with
/// a short sleep so we don't drop the row mid-write and trigger a spurious
/// "appmanifest disappeared" path.
fn parse_acf(path: &Path, steamapps: &Path) -> Option<AcfSnapshot> {
    let content = read_with_retry(path)?;
    let appid = extract(&content, "appid")?;
    let name = extract(&content, "name").unwrap_or_default();
    let installdir = extract(&content, "installdir").unwrap_or_default();
    let state_flags = extract(&content, "StateFlags")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let raw_dl = extract(&content, "BytesDownloaded")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let raw_to_dl = extract(&content, "BytesToDownload")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let raw_staged = extract(&content, "BytesStaged")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let raw_to_stage = extract(&content, "BytesToStage")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    // Steam has two distinct phases when installing a game:
    //   1. Download data — compressed chunks from the CDN
    //      (BytesDownloaded / BytesToDownload)
    //   2. Install files — decompress + write to the install dir
    //      (BytesStaged / BytesToStage)
    // Both can be in flight or sequential depending on the game's manifest
    // depots. We mirror Steam's own UI: show network bytes (compressed) as
    // the byte counters, and use MIN(dl_ratio, stage_ratio) as the honest
    // progress percent so the bar doesn't claim 100% while the install
    // phase is still decompressing.
    let dl_ratio = if raw_to_dl > 0 {
        (raw_dl.min(raw_to_dl) as f64) / (raw_to_dl as f64)
    } else {
        1.0
    };

    // Live install-dir size: as Steam decompresses chunks into
    // `common\<installdir>` the folder grows monotonically. Polling its size
    // each tick gives us a real-time stage progress that doesn't depend on
    // Steam flushing the manifest. Cap at `raw_to_stage` (decompressed total)
    // so we never report > 100%.
    let install_dir = steamapps.join("common").join(&installdir);
    let live_staged = recursive_dir_size(&install_dir);
    let effective_staged = raw_staged.max(live_staged);
    let stage_ratio_opt: Option<f64> = if raw_to_stage > 0 {
        Some((effective_staged.min(raw_to_stage) as f64) / (raw_to_stage as f64))
    } else if effective_staged > 0 && raw_to_dl > 0 {
        // No BytesToStage published yet — approximate with the network total
        // so the bar moves while Steam is just unpacking the first chunks.
        Some((effective_staged as f64).min(raw_to_dl as f64) / (raw_to_dl as f64))
    } else {
        None
    };

    // Pick whichever phase carries the fresher signal:
    //   * Both phases in flight → max(dl_ratio, stage_ratio). Steam flushes
    //     BytesDownloaded into the ACF in 5–10s batches, but the install-dir
    //     size grows continuously, so stage_ratio is usually the live one and
    //     dl_ratio lags. Taking max prefers the live signal without lying
    //     during the network-only phase.
    //   * Network done (dl_ratio >= 1.0) → fall back to stage_ratio so the
    //     bar reflects ongoing decompression instead of jumping to 100%.
    //   * No staging info yet → just use dl_ratio.
    let combined_ratio = match stage_ratio_opt {
        Some(stage_ratio) => {
            if dl_ratio >= 1.0 {
                stage_ratio
            } else {
                dl_ratio.max(stage_ratio)
            }
        }
        None => dl_ratio,
    }
    .clamp(0.0, 1.0);

    // Byte counters match Steam's UI ("5.3 GO / 100 GO"): compressed network
    // transfer values. The honest percent is encoded by `bytes_downloaded =
    // combined_ratio * bytes_to_download` so the bar reflects the slowest
    // phase even when the network phase visually finishes early.
    let bytes_to_download = raw_to_dl;
    let bytes_downloaded = if bytes_to_download > 0 {
        ((combined_ratio * bytes_to_download as f64) as u64).min(bytes_to_download)
    } else {
        raw_dl
    };

    Some(AcfSnapshot {
        appid,
        name,
        installdir,
        state_flags,
        bytes_downloaded,
        bytes_to_download,
        raw_dl,
        raw_to_stage,
        raw_staged,
        steamapps_dir: steamapps.to_path_buf(),
    })
}

/// Pull a `"key" "value"` pair out of a VDF KeyValues1 block. Steam writes
/// these one per line with arbitrary whitespace between key and value.
fn extract(content: &str, key: &str) -> Option<String> {
    // Walk line by line, look for `"<key>"` followed by `"<value>"`. We avoid
    // a regex crate dependency on this hot path by manual parsing.
    let target = format!("\"{}\"", key);
    for line in content.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with(&target) {
            continue;
        }
        // Skip past the key token.
        let rest = &trimmed[target.len()..];
        // Find the next quoted string in `rest`.
        let mut chars = rest.char_indices();
        let mut start: Option<usize> = None;
        for (i, c) in chars.by_ref() {
            if c == '"' {
                start = Some(i + 1);
                break;
            }
        }
        let start = start?;
        let after_start = &rest[start..];
        // Find the closing quote (no escapes in Steam's ACF strings).
        let end_offset = after_start.find('"')?;
        return Some(after_start[..end_offset].to_string());
    }
    None
}

/// Process one polling tick: diff against the previous tick, update
/// `RuntimeState`, and emit events for Steam-source games only.
async fn tick(
    app: &AppHandle,
    db: &Database,
    runtime: &RuntimeState,
    prev: &Arc<Mutex<PrevTick>>,
    snapshots: Vec<AcfSnapshot>,
) -> Result<(), String> {
    let mut prev_guard = prev.lock().await;
    let now = Instant::now();
    let mut current_appids: HashMap<String, String> = HashMap::new(); // appid -> game_id

    // Sample steam.exe's cumulative write counter once per tick. We use this
    // as the canonical real-time signal for speed/bytes because Steam
    // batches ACF flushes (sometimes for MINUTES on a first install) and
    // the kernel counter updates the instant Steam writes to disk.
    let counter_now = read_steam_write_bytes();
    let global_speed_bps = match (counter_now, prev_guard.last_steam_io) {
        (Some(c), Some((prev_c, prev_t))) => {
            let dt = now.saturating_duration_since(prev_t).as_secs_f64();
            if dt > 0.0 && c > prev_c {
                ((c - prev_c) as f64 / dt).round() as u64
            } else {
                0
            }
        }
        _ => 0,
    };
    if let Some(c) = counter_now {
        prev_guard.last_steam_io = Some((c, now));
    }

    for snap in &snapshots {
        // Only act on entries whose manifest is in some "doing things" state.
        // Fully-installed-and-up-to-date manifests don't need touching.
        let downloading = snap.is_downloading();
        let installed = snap.is_installed();
        if !downloading && !installed {
            continue;
        }

        // User-dismissed appids: skip while the game is still downloading
        // (we honor the user's "clear from list" intent). Clear the dismiss
        // flag once the install completes — re-installs after this should
        // light the row back up.
        if runtime.is_steam_dismissed(&snap.appid) {
            if installed {
                runtime.clear_steam_dismissed(&snap.appid);
            } else {
                continue;
            }
        }

        // Locate the matching game in our DB.
        let game_id = match db.find_steam_id_by_appid(&snap.appid) {
            Ok(Some(id)) => id,
            _ => continue, // game not in our library; ignore
        };

        // When the CDP watcher recently pushed authoritative data for this
        // game, step out of the way for the progress update. We still want
        // to maintain `last_seen_appids` (for stale-manifest detection) and
        // run the `installed` branch (for install_path DB writes), so we
        // just skip the per-tick progress write.
        let cdp_owns = runtime.cdp_is_recent(&game_id, Duration::from_secs(3));

        if downloading && cdp_owns {
            // CDP is driving — record the appid as live and move on.
            current_appids.insert(snap.appid.clone(), game_id.clone());
            continue;
        }

        if downloading {
            // Steam flushes BytesDownloaded into the ACF every ~5-10s while
            // downloading. If we just compared "now vs last tick" we'd see
            // delta == 0 most ticks and speed would oscillate to 0 between
            // flushes — and worse, the UI would snap forward only when Steam
            // flushed (the "ça saffiche que quand je met pause" symptom).
            //
            // Instead: only update our (bytes, ts) baseline when the ACF
            // value ACTUALLY changes, recompute speed from that bigger
            // delta, and keep the last known speed around to extrapolate
            // between flushes.
            // IMPORTANT: track changes against `raw_dl` (the raw ACF
            // BytesDownloaded value), NOT `snap.bytes_downloaded`. The latter
            // is the COOKED display value that wobbles every tick because
            // `live_staged` (install-dir size) keeps shifting `stage_ratio`.
            // Using the cooked value here would trip the "ACF changed" branch
            // every single tick and reset `counter_at_acf_baseline` on every
            // tick, killing the kernel-counter extrapolation entirely.
            let prev_entry = prev_guard.inner.get(&game_id).copied();
            let acf_changed = match prev_entry {
                Some((prev_bytes, _)) => snap.raw_dl != prev_bytes,
                None => true,
            };
            if acf_changed {
                if let Some((prev_bytes, prev_ts)) = prev_entry {
                    let dt = now.saturating_duration_since(prev_ts).as_secs_f64();
                    if dt > 0.0 && snap.raw_dl > prev_bytes {
                        let delta = (snap.raw_dl - prev_bytes) as f64;
                        let speed = (delta / dt).round() as u64;
                        prev_guard.last_known_speed.insert(game_id.clone(), speed);
                    }
                }
                prev_guard
                    .inner
                    .insert(game_id.clone(), (snap.raw_dl, now));
                // Pin the kernel counter to this ACF baseline. From here on
                // we'll display `acf_bytes + (counter_now - counter_at_baseline)`,
                // which moves smoothly between ACF flushes.
                if let Some(c) = counter_now {
                    prev_guard
                        .counter_at_acf_baseline
                        .insert(game_id.clone(), c);
                }
            }

            // Compute the displayed byte counter. Two paths:
            //
            //   1. Kernel-counter path (preferred): `acf_baseline +
            //      (current_counter - counter_at_baseline)`. Updates each
            //      tick with the actual bytes Steam wrote to disk since the
            //      last ACF flush — works even when Steam never flushes
            //      (first-install pre-flush window).
            //   2. Time-extrapolation fallback (Steam not running for some
            //      reason, or sysinfo failed): `acf_baseline +
            //      last_known_speed * elapsed`. Worst case during a first
            //      install: stays at 0 until Steam flushes the ACF.
            let (baseline_bytes, baseline_ts) = prev_guard
                .inner
                .get(&game_id)
                .copied()
                .unwrap_or((snap.bytes_downloaded, now));
            let counter_at_base = prev_guard.counter_at_acf_baseline.get(&game_id).copied();

            // Split the kernel counter delta into the two phases Steam
            // writes simultaneously: compressed chunks (network bytes) +
            // decompressed staged data. The split ratio is the decompressed
            // size relative to the compressed size from the ACF.
            //   writes ≈ compressed_received + decompressed_staged
            //   decompressed ≈ compressed × (raw_to_stage / raw_to_dl)
            // So per 1 byte of counter growth:
            //   dl_share    = 1 / (1 + decompress_ratio)
            //   stage_share = decompress_ratio / (1 + decompress_ratio)
            let decompress_ratio = if snap.bytes_to_download > 0 && snap.raw_to_stage > 0 {
                snap.raw_to_stage as f64 / snap.bytes_to_download as f64
            } else {
                1.0
            };
            let dl_share = 1.0 / (1.0 + decompress_ratio);
            let stage_share = decompress_ratio / (1.0 + decompress_ratio);

            // Counter delta since the ACF baseline.
            let raw_counter_delta = match (counter_now, counter_at_base) {
                (Some(c), Some(c_base)) => c.saturating_sub(c_base),
                _ => 0,
            };

            // Extrapolate BOTH phases. baseline_bytes is snap.raw_dl (set
            // when ACF last changed). For staged we use snap.raw_staged.
            let extrap_dl = baseline_bytes
                .saturating_add(((raw_counter_delta as f64) * dl_share) as u64)
                .min(snap.bytes_to_download);
            let extrap_staged = snap
                .raw_staged
                .saturating_add(((raw_counter_delta as f64) * stage_share) as u64)
                .min(snap.raw_to_stage.max(snap.raw_staged));

            // Match Steam's UI: percent = min(network%, stage%). Whichever
            // phase is currently the bottleneck drives the bar.
            let dl_pct = if snap.bytes_to_download > 0 {
                extrap_dl as f64 / snap.bytes_to_download as f64
            } else {
                1.0
            };
            let stage_pct = if snap.raw_to_stage > 0 {
                extrap_staged as f64 / snap.raw_to_stage as f64
            } else {
                1.0
            };
            let combined_pct = dl_pct.min(stage_pct).clamp(0.0, 1.0);

            // Bytes line shows compressed network bytes, matching Steam's
            // "X GO / Y GO" display in its own UI.
            let displayed_bytes =
                ((combined_pct * snap.bytes_to_download as f64) as u64).min(snap.bytes_to_download);

            // Speed: from the kernel counter, scaled by dl_share so the MB/s
            // reflects network throughput, not raw disk writes.
            let speed_bps = if global_speed_bps > 0 {
                ((global_speed_bps as f64) * dl_share) as u64
            } else {
                prev_guard
                    .last_known_speed
                    .get(&game_id)
                    .copied()
                    .unwrap_or(0)
            };
            // Silence the unused-warning for `baseline_ts` left over from the
            // previous time-extrapolation fallback path.
            let _ = baseline_ts;

            let eta_secs = if speed_bps > 0 {
                let remaining = snap.bytes_to_download.saturating_sub(displayed_bytes);
                let secs = (remaining as f64 / speed_bps as f64).round() as u64;
                secs.min(u32::MAX as u64) as u32
            } else {
                0
            };

            // progress_pct directly tracks combined_pct (min of network +
            // staging) so the bar matches Steam's UI even when bytes-line
            // rounding makes the two diverge.
            let progress_pct = (combined_pct * 100.0) as f32;

            // Build / refresh the RuntimeState entry. Track status transitions
            // out-of-band so we know when to emit a `download-status` event.
            let prior_status = runtime.get(&game_id).map(|s| s.status);
            let install_path_str = snap.install_path().to_string_lossy().to_string();

            if prior_status.is_some() {
                runtime.update(&game_id, |s| {
                    s.source = "steam".to_string();
                    s.platform_id = snap.appid.clone();
                    if !snap.name.is_empty() {
                        s.game_name = snap.name.clone();
                    }
                    s.status = DownloadStatus::Downloading;
                    // No .max() against previous value: when the ACF flushes
                    // a new raw_dl, the kernel-counter extrapolation gets a
                    // fresh baseline. The new displayed_bytes may briefly
                    // dip below the previous (over-extrapolated) value —
                    // that's a correction toward truth, not a regression.
                    s.downloaded_bytes = displayed_bytes;
                    s.total_bytes = snap.bytes_to_download;
                    s.progress_pct = progress_pct;
                    s.speed_bps = speed_bps;
                    s.eta_secs = eta_secs;
                    s.install_path = Some(install_path_str.clone());
                    s.last_error = None;
                });
            } else {
                let mut state = DownloadState::new(
                    game_id.clone(),
                    "steam".to_string(),
                    snap.appid.clone(),
                    if snap.name.is_empty() {
                        snap.installdir.clone()
                    } else {
                        snap.name.clone()
                    },
                );
                state.status = DownloadStatus::Downloading;
                state.downloaded_bytes = displayed_bytes;
                state.total_bytes = snap.bytes_to_download;
                state.progress_pct = progress_pct;
                state.speed_bps = speed_bps;
                state.eta_secs = eta_secs;
                state.install_path = Some(install_path_str.clone());
                runtime.upsert(state);
            }

            // Emit a `download-status` only when the status transitions
            // (None → Downloading, or any other → Downloading) so the UI
            // doesn't get spammed.
            if prior_status != Some(DownloadStatus::Downloading) {
                let _ = app.emit(
                    "download-status",
                    StatusEvent {
                        game_id: game_id.clone(),
                        status: DownloadStatus::Downloading,
                        last_error: None,
                    },
                );
            }

            // Always emit progress on each tick (we throttle implicitly via
            // the 2s poll interval already).
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
        } else if installed {
            // Manifest says fully installed. If we had this in runtime (mid
            // download last tick) → Completed. Update install_path either
            // way (cheap, idempotent).
            let install_path = snap.install_path().to_string_lossy().to_string();
            // Persist install_path on the game row regardless of whether the
            // runtime had an entry (Steam might have installed it without us
            // ever observing the in-progress state, e.g. very fast download).
            if let Ok(true) = db.set_install_path_by_platform("steam", &snap.appid, &install_path) {
                // Library UI hook: useGames re-fetches when it sees this so
                // the just-installed game flips to "Installed" without F5.
                let _ = app.emit("games-changed", ());
            }

            if runtime.get(&game_id).is_some() {
                runtime.update(&game_id, |s| {
                    s.status = DownloadStatus::Completed;
                    s.progress_pct = 100.0;
                    s.speed_bps = 0;
                    s.eta_secs = 0;
                    s.downloaded_bytes = s.total_bytes.max(s.downloaded_bytes);
                    s.install_path = Some(install_path.clone());
                    s.last_error = None;
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
                    let _ = app.emit(
                        "download-status",
                        StatusEvent {
                            game_id: game_id.clone(),
                            status: DownloadStatus::Completed,
                            last_error: None,
                        },
                    );
                }
                // Schedule a cleanup so the toast clears like Epic/GOG.
                let runtime_clone = runtime.clone();
                let id_owned = game_id.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(8)).await;
                    runtime_clone.remove(&id_owned);
                });
            }
            prev_guard.inner.remove(&game_id);
            prev_guard.last_known_speed.remove(&game_id);
            prev_guard.counter_at_acf_baseline.remove(&game_id);
        }

        if downloading || installed {
            current_appids.insert(snap.appid.clone(), game_id);
        }
    }

    // Detect disappearance: any appid we were tracking last tick that isn't
    // present this tick. Steam either finished (some races) or the user
    // cancelled in their UI. Best-effort: if we have a runtime entry that's
    // not already Completed, mark it Completed if bytes match, else cancelled.
    // Bump or clear the miss counter for each previously-seen appid. Steam
    // briefly locks the manifest during writes so a single missed tick is
    // expected — we only flush after MISS_THRESHOLD consecutive misses.
    const MISS_THRESHOLD: u32 = 5;
    for (appid, _gid) in prev_guard.last_seen_appids.clone() {
        if current_appids.contains_key(&appid) {
            prev_guard.miss_counts.remove(&appid);
        } else {
            let c = prev_guard.miss_counts.entry(appid.clone()).or_insert(0);
            *c += 1;
        }
    }
    // Clear miss counters for appids that haven't been seen in a while at all.
    let known: std::collections::HashSet<String> = prev_guard
        .last_seen_appids
        .keys()
        .cloned()
        .chain(current_appids.keys().cloned())
        .collect();
    prev_guard.miss_counts.retain(|k, _| known.contains(k));

    let stale: Vec<(String, String)> = prev_guard
        .last_seen_appids
        .iter()
        .filter(|(appid, _)| {
            !current_appids.contains_key(*appid)
                && prev_guard
                    .miss_counts
                    .get(*appid)
                    .copied()
                    .unwrap_or(0)
                    >= MISS_THRESHOLD
        })
        .map(|(a, g)| (a.clone(), g.clone()))
        .collect();
    for (appid, game_id) in stale {
        // Skip if we never had a runtime entry, or if it's already terminal.
        let Some(state) = runtime.get(&game_id) else {
            continue;
        };
        if matches!(
            state.status,
            DownloadStatus::Completed | DownloadStatus::Failed
        ) {
            continue;
        }
        let was_done = state.total_bytes > 0 && state.downloaded_bytes >= state.total_bytes;
        if was_done {
            runtime.update(&game_id, |s| {
                s.status = DownloadStatus::Completed;
                s.progress_pct = 100.0;
                s.speed_bps = 0;
                s.eta_secs = 0;
            });
            let _ = app.emit(
                "download-status",
                StatusEvent {
                    game_id: game_id.clone(),
                    status: DownloadStatus::Completed,
                    last_error: None,
                },
            );
        } else {
            runtime.update(&game_id, |s| {
                s.status = DownloadStatus::Failed;
                s.speed_bps = 0;
                s.eta_secs = 0;
                s.last_error = Some("Cancelled in Steam".to_string());
            });
            let _ = app.emit(
                "download-status",
                StatusEvent {
                    game_id: game_id.clone(),
                    status: DownloadStatus::Failed,
                    last_error: Some("Cancelled in Steam".to_string()),
                },
            );
        }
        prev_guard.inner.remove(&game_id);
        prev_guard.last_known_speed.remove(&game_id);
        prev_guard.counter_at_acf_baseline.remove(&game_id);
        prev_guard.miss_counts.remove(&appid);
        prev_guard.last_seen_appids.remove(&appid);
        log::info!(
            "[steam_dl_watcher] appmanifest for appid={} disappeared after {} ticks — flushed runtime state",
            appid,
            MISS_THRESHOLD
        );
    }

    // Merge: keep previously-seen appids that aren't yet over the miss
    // threshold, layer in the current snapshot. This way an entry that
    // disappeared briefly (file locked) stays in `last_seen_appids` and
    // gets re-seen on the next tick once Steam releases the lock.
    for (appid, gid) in current_appids {
        prev_guard.last_seen_appids.insert(appid, gid);
    }
    drop(prev_guard);

    // Validate Steam install paths against disk: a row may have install_path
    // set from a previous session, but the user may have uninstalled the game
    // in Steam (or Steam may have wiped the files for some other reason) so
    // the path no longer exists. Without this check the UI shows games as
    // installed when they aren't.
    if let Ok(rows) = db.list_steam_installed() {
        let mut any_cleared = false;
        for (id, _appid, install_path) in rows {
            if !std::path::Path::new(&install_path).exists() {
                if let Err(e) = db.clear_install_path(&id) {
                    log::warn!(
                        "[steam_dl_watcher] failed to clear stale install_path for id={}: {}",
                        id,
                        e
                    );
                } else {
                    log::info!(
                        "[steam_dl_watcher] cleared install_path for id={} (path {:?} no longer exists)",
                        id,
                        install_path
                    );
                    any_cleared = true;
                }
            }
        }
        if any_cleared {
            let _ = app.emit("games-changed", ());
        }
    }

    Ok(())
}
