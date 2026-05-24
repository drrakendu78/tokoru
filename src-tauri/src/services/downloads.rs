//! Download orchestrator — drives legendary / gogdl child processes for one
//! game at a time and merges their progress into the shared `RuntimeState`.
//!
//! Responsibilities:
//! - Look up the game in the DB, pick the runner (legendary for `source=epic`,
//!   gogdl for `source=gog`), spawn the child process.
//! - Parse runner stdout/stderr line-by-line, fold updates into the
//!   `DownloadState`, and emit `download-progress` events at most once a
//!   second so the frontend isn't flooded.
//! - On any status change (`Queued → Downloading`, `Downloading → Paused`,
//!   etc.) emit a `download-status` event.
//! - Persist state to `pending_downloads.json` on every status transition so
//!   restarts can resume / re-fail correctly.
//! - On Completed: optionally call `push_to_steam` if the user enabled
//!   `auto_push_after_download` (see `download_settings`).
//!
//! Bandwidth throttling is stubbed — neither legendary nor gogdl supports
//! `--limit` natively. See the comment on `DownloadSettings::bandwidth_kbps`.

use std::io::{BufRead, BufReader, Read};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::services::db::Database;
use crate::services::runtime_state::{DownloadState, DownloadStatus, RuntimeState};
use crate::services::{gog_api, gogdl, legendary};

/// User-tunable download settings — all persisted in `sync_state`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadSettings {
    /// Default install directory (when no per-source override is set).
    pub default_dir: String,
    /// Per-source overrides keyed by source string ("epic", "gog", ...).
    pub per_source: std::collections::HashMap<String, String>,
    /// How many downloads can run simultaneously. Extras go to Queued.
    pub max_parallel: u32,
    /// When true, completed downloads automatically push to Steam.
    pub auto_push_after_download: bool,
    /// Bandwidth cap in KB/s, 0 = unlimited. **Not yet enforced** — legendary
    /// and gogdl don't expose a native bandwidth limit, and adding our own
    /// would require proxying the HTTP traffic. Stored for the UI to render
    /// and for future enforcement.
    pub bandwidth_kbps: u64,
}

impl Default for DownloadSettings {
    fn default() -> Self {
        let default_dir = std::env::var("USERPROFILE")
            .map(|home| format!("{}\\Games", home))
            .unwrap_or_else(|_| "C:\\Games".to_string());
        Self {
            default_dir,
            per_source: std::collections::HashMap::new(),
            max_parallel: 2,
            auto_push_after_download: true,
            bandwidth_kbps: 0,
        }
    }
}

const KEY_DEFAULT_DIR: &str = "download_default_dir";
const KEY_PER_SOURCE_PREFIX: &str = "download_per_source_";
const KEY_MAX_PARALLEL: &str = "download_max_parallel";
const KEY_AUTO_PUSH: &str = "download_auto_push";
const KEY_BANDWIDTH: &str = "download_bandwidth_kbps";
const PER_SOURCE_KEYS: &[&str] = &[
    "epic", "gog", "ubi", "ea", "itch", "xbox", "amazon", "custom",
];

/// Load settings from sync_state, filling with defaults for missing keys.
pub fn load_settings(db: &Database) -> DownloadSettings {
    let mut s = DownloadSettings::default();

    if let Ok(Some(v)) = db.get_sync_state(KEY_DEFAULT_DIR) {
        if !v.is_empty() {
            s.default_dir = v;
        }
    }
    if let Ok(Some(v)) = db.get_sync_state(KEY_MAX_PARALLEL) {
        if let Ok(n) = v.parse::<u32>() {
            s.max_parallel = n.max(1);
        }
    }
    if let Ok(Some(v)) = db.get_sync_state(KEY_AUTO_PUSH) {
        s.auto_push_after_download = v == "true";
    }
    if let Ok(Some(v)) = db.get_sync_state(KEY_BANDWIDTH) {
        if let Ok(n) = v.parse::<u64>() {
            s.bandwidth_kbps = n;
        }
    }
    for src in PER_SOURCE_KEYS {
        let key = format!("{}{}", KEY_PER_SOURCE_PREFIX, src);
        if let Ok(Some(v)) = db.get_sync_state(&key) {
            if !v.is_empty() {
                s.per_source.insert((*src).to_string(), v);
            }
        }
    }

    s
}

/// Save settings to sync_state. Always writes every field so flipping
/// auto_push from true to false is durable.
pub fn save_settings(db: &Database, s: &DownloadSettings) -> Result<(), String> {
    db.set_sync_state(KEY_DEFAULT_DIR, &s.default_dir)
        .map_err(|e| e.to_string())?;
    db.set_sync_state(KEY_MAX_PARALLEL, &s.max_parallel.to_string())
        .map_err(|e| e.to_string())?;
    db.set_sync_state(
        KEY_AUTO_PUSH,
        if s.auto_push_after_download { "true" } else { "false" },
    )
    .map_err(|e| e.to_string())?;
    db.set_sync_state(KEY_BANDWIDTH, &s.bandwidth_kbps.to_string())
        .map_err(|e| e.to_string())?;
    for src in PER_SOURCE_KEYS {
        let key = format!("{}{}", KEY_PER_SOURCE_PREFIX, src);
        let value = s.per_source.get(*src).cloned().unwrap_or_default();
        db.set_sync_state(&key, &value).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Resolve the install directory for a given source — per-source override
/// wins, then default_dir.
pub fn install_dir_for_source(settings: &DownloadSettings, source: &str) -> String {
    settings
        .per_source
        .get(source)
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| settings.default_dir.clone())
}

// ── Event payloads ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ProgressEvent {
    pub game_id: String,
    pub progress_pct: f32,
    pub speed_bps: u64,
    pub eta_secs: u32,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusEvent {
    pub game_id: String,
    pub status: DownloadStatus,
    pub last_error: Option<String>,
}

fn emit_progress(app: &AppHandle, state: &DownloadState) {
    let _ = app.emit(
        "download-progress",
        ProgressEvent {
            game_id: state.game_id.clone(),
            progress_pct: state.progress_pct,
            speed_bps: state.speed_bps,
            eta_secs: state.eta_secs,
            downloaded_bytes: state.downloaded_bytes,
            total_bytes: state.total_bytes,
        },
    );
}

fn emit_status(app: &AppHandle, state: &DownloadState) {
    let _ = app.emit(
        "download-status",
        StatusEvent {
            game_id: state.game_id.clone(),
            status: state.status,
            last_error: state.last_error.clone(),
        },
    );
}

// ── Public entry points ────────────────────────────────────────────────

/// Kick off a download for the given game. Looks up the game in the DB,
/// picks the runner, spawns it on a blocking thread, and returns immediately.
/// Progress flows through Tauri events.
pub async fn start(
    app: AppHandle,
    runtime: RuntimeState,
    db: Database,
    game_id: String,
) -> Result<(), String> {
    let game = db
        .get_game_by_id(&game_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Game '{}' not found", game_id))?;

    let source = game.source.clone();
    if source != "epic" && source != "gog" {
        return Err(format!(
            "Download not supported for source '{}' — only epic / gog have a CLI",
            source
        ));
    }

    let platform_id = game
        .platform_id
        .clone()
        .ok_or_else(|| format!("Game '{}' has no platform_id (can't dispatch to runner)", game_id))?;

    let game_name = game.title.clone();
    let settings = load_settings(&db);
    let install_base = install_dir_for_source(&settings, &source);

    // Check parallel slot. If we're at the cap, mark Queued and bail — the
    // user can either pause an active one or wait. We don't auto-dequeue
    // because that would require a scheduler we don't have yet; v1 keeps it
    // explicit.
    let active = runtime.active_count();
    if active >= settings.max_parallel as usize {
        let mut state = DownloadState::new(
            game_id.clone(),
            source.clone(),
            platform_id.clone(),
            game_name.clone(),
        );
        state.status = DownloadStatus::Queued;
        state.install_path = Some(install_base.clone());
        runtime.upsert(state.clone());
        emit_status(&app, &state);
        return Ok(());
    }

    // Construct the running state — preserving any previously-recorded
    // progress so a resume doesn't visually jump back to 0%. The runner will
    // overwrite these fields as soon as it emits its first progress line
    // (legendary/gogdl resume from on-disk checkpoint, so the bytes already
    // on disk are still valid). For a fresh start (no existing entry) we
    // build a default DownloadState with zeros.
    let previous = runtime.get(&game_id);
    let mut state = match previous {
        Some(prev) => {
            let mut next = prev.clone();
            next.status = DownloadStatus::Downloading;
            next.last_error = None;
            next.speed_bps = 0; // unknown until runner reports back
            next.eta_secs = 0;
            next.pid = None; // will be set after spawn
            next
        }
        None => {
            let mut fresh = DownloadState::new(
                game_id.clone(),
                source.clone(),
                platform_id.clone(),
                game_name.clone(),
            );
            fresh.status = DownloadStatus::Downloading;
            fresh.install_path = Some(install_base.clone());
            fresh
        }
    };
    if state.install_path.is_none() {
        state.install_path = Some(install_base.clone());
    }
    runtime.upsert(state.clone());
    emit_status(&app, &state);

    // Resolve runner exe up-front so failures show before we go into the
    // blocking thread.
    let (runner_label, runner_exe, gogdl_auth) = match source.as_str() {
        "epic" => {
            let exe = legendary::ensure_exe(&app).await?;
            ("legendary.exe", exe, None)
        }
        "gog" => {
            // Make sure GOG token is fresh — gogdl reads from auth.json but
            // tokens expire in ~1h.
            if let Err(e) = gog_api::refreshed_access_token(&app).await {
                log::warn!("[downloads] gog token refresh failed: {} (continuing)", e);
            }
            let exe = gogdl::ensure_exe(&app).await?;
            let auth = gogdl::auth_path(&app)?;
            ("gogdl.exe", exe, Some(auth))
        }
        _ => unreachable!(),
    };

    // ── Runner-specific install path semantics ─────────────────────────────
    // Legendary appends `<base>/<GameTitle>` itself when given `--base-path`,
    // so we hand it the base directory unchanged. gogdl on the other hand
    // takes a literal `--path`, so we precompute `<base>/<sanitized_name>`
    // and pass that.
    //
    // After a legendary install finishes we don't yet know the exact path
    // legendary picked (its title can differ from our `game_name`). The
    // completion handler resolves it via `legendary list-installed --json`.
    // For gogdl the path we pass IS the install path, so we use it directly.
    let (runner_install_arg, gogdl_explicit_path) = match source.as_str() {
        "epic" => (install_base.clone(), None::<String>),
        "gog" => {
            let p = compute_install_path(&install_base, &game_name);
            (p.clone(), Some(p))
        }
        _ => unreachable!(),
    };

    // Make sure the base/target dir exists before the runner tries to write.
    let _ = std::fs::create_dir_all(&runner_install_arg);

    // Persist our best-guess install_path for resume. For legendary this is
    // refined after the install finishes (see `handle_completion`). For gogdl
    // it's already the final path.
    let persisted_install_path = gogdl_explicit_path
        .clone()
        .unwrap_or_else(|| install_base.clone());
    runtime.update(&game_id, |s| {
        s.install_path = Some(persisted_install_path.clone());
    });

    let runtime_clone = runtime.clone();
    let app_clone = app.clone();
    let db_clone = db.clone();
    let game_id_clone = game_id.clone();
    let source_clone = source.clone();
    let platform_id_clone = platform_id.clone();
    let install_path_for_thread = runner_install_arg.clone();
    let legendary_exe_for_completion = if source == "epic" {
        Some(runner_exe.clone())
    } else {
        None
    };

    // The whole runner lifecycle (spawn → parse → wait) goes on a blocking
    // worker. tokio::spawn would block the executor while reading stderr.
    tauri::async_runtime::spawn_blocking(move || {
        let spawn_result = match source_clone.as_str() {
            "epic" => legendary::spawn_install(
                &runner_exe,
                &platform_id_clone,
                Some(&install_path_for_thread),
                None,
            ),
            "gog" => {
                let auth = gogdl_auth.as_ref().expect("gogdl auth path resolved above");
                gogdl::spawn_install(
                    &runner_exe,
                    auth,
                    &platform_id_clone,
                    &install_path_for_thread,
                    None,
                )
            }
            _ => unreachable!(),
        };

        let mut child = match spawn_result {
            Ok(c) => c,
            Err(e) => {
                let err = e.clone();
                runtime_clone.update(&game_id_clone, |s| {
                    s.status = DownloadStatus::Failed;
                    s.last_error = Some(err.clone());
                });
                if let Some(latest) = runtime_clone.get(&game_id_clone) {
                    emit_status(&app_clone, &latest);
                }
                return;
            }
        };

        // Record PID + exe name for taskkill.
        let pid = child.id();
        runtime_clone.update(&game_id_clone, |s| {
            s.pid = Some(pid);
            s.exe_name = Some(runner_label.to_string());
        });

        // Drain stdout in a side thread (gogdl uses stderr for progress;
        // legendary uses stderr too, but we log stdout in case of warnings).
        if let Some(stdout) = child.stdout.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().flatten() {
                    log::debug!("[runner stdout] {}", line);
                }
            });
        }

        // Main progress parse loop on stderr. We read byte-by-byte and split
        // on both `\n` and `\r` because gogdl uses CR for in-place updates.
        let mut last_emit = Instant::now();
        let emit_min_interval = Duration::from_millis(1000);

        if let Some(stderr) = child.stderr.take() {
            let mut reader = BufReader::new(stderr);
            let mut buf: Vec<u8> = Vec::with_capacity(512);
            let mut byte = [0u8; 1];

            loop {
                match reader.read(&mut byte) {
                    Ok(0) => break,
                    Ok(_) => {
                        if byte[0] == b'\n' || byte[0] == b'\r' {
                            if buf.is_empty() {
                                continue;
                            }
                            let line = String::from_utf8_lossy(&buf).to_string();
                            buf.clear();
                            log::debug!("[runner stderr] {}", line);

                            let delta = match source_clone.as_str() {
                                "epic" => legendary::parse_progress_line(&line).map(|d| {
                                    ProgressDeltaCommon {
                                        progress_pct: d.progress_pct,
                                        downloaded_bytes: d.downloaded_bytes,
                                        total_bytes: d.total_bytes,
                                        speed_bps: d.speed_bps,
                                        eta_secs: d.eta_secs,
                                    }
                                }),
                                "gog" => gogdl::parse_progress_line(&line).map(|d| {
                                    ProgressDeltaCommon {
                                        progress_pct: d.progress_pct,
                                        downloaded_bytes: d.downloaded_bytes,
                                        total_bytes: d.total_bytes,
                                        speed_bps: d.speed_bps,
                                        eta_secs: d.eta_secs,
                                    }
                                }),
                                _ => None,
                            };

                            if let Some(d) = delta {
                                runtime_clone.update(&game_id_clone, |s| {
                                    // Bytes counters use MAX semantics so a
                                    // session-relative runner report (gogdl
                                    // after resume only counts THIS session's
                                    // bytes) doesn't drag the absolute total
                                    // back down. As soon as the runner's
                                    // session counter overtakes the preserved
                                    // value, normal monotonic growth kicks in.
                                    if let Some(dl) = d.downloaded_bytes {
                                        if dl > s.downloaded_bytes {
                                            s.downloaded_bytes = dl;
                                        }
                                    }
                                    if let Some(t) = d.total_bytes {
                                        if t > s.total_bytes {
                                            s.total_bytes = t;
                                        }
                                    }
                                    // Re-derive percent from the merged byte
                                    // counters so the UI bar matches the
                                    // displayed `X GB / Y GB` line — ignore
                                    // the runner-reported pct entirely when we
                                    // have both byte fields.
                                    if s.total_bytes > 0 {
                                        s.progress_pct =
                                            (s.downloaded_bytes as f32 / s.total_bytes as f32)
                                                * 100.0;
                                    } else if let Some(p) = d.progress_pct {
                                        s.progress_pct = p;
                                    }
                                    if let Some(sp) = d.speed_bps {
                                        s.speed_bps = sp;
                                    }
                                    if let Some(e) = d.eta_secs {
                                        s.eta_secs = e;
                                    }
                                });
                                // Throttle emits to one per second.
                                if last_emit.elapsed() >= emit_min_interval {
                                    if let Some(latest) = runtime_clone.get(&game_id_clone) {
                                        emit_progress(&app_clone, &latest);
                                    }
                                    last_emit = Instant::now();
                                }
                            }

                            // Completion lines are best-effort signals — the
                            // authoritative source is the child exit status
                            // below.
                            let completion = match source_clone.as_str() {
                                "epic" => legendary::is_completion_line(&line),
                                "gog" => gogdl::is_completion_line(&line),
                                _ => false,
                            };
                            if completion {
                                runtime_clone.update(&game_id_clone, |s| {
                                    s.progress_pct = 100.0;
                                });
                            }
                        } else {
                            buf.push(byte[0]);
                        }
                    }
                    Err(_) => break,
                }
            }
        }

        // Push one final progress event before the wait so the bar settles.
        if let Some(latest) = runtime_clone.get(&game_id_clone) {
            emit_progress(&app_clone, &latest);
        }

        let exit_status = child.wait();
        runtime_clone.update(&game_id_clone, |s| {
            s.pid = None;
            s.exe_name = None;
        });

        let was_paused_before_wait = runtime_clone
            .get(&game_id_clone)
            .map(|s| s.status == DownloadStatus::Paused)
            .unwrap_or(false);

        match exit_status {
            Ok(status) if status.success() => {
                runtime_clone.update(&game_id_clone, |s| {
                    s.status = DownloadStatus::Completed;
                    s.progress_pct = 100.0;
                    s.last_error = None;
                });
                if let Some(latest) = runtime_clone.get(&game_id_clone) {
                    emit_progress(&app_clone, &latest);
                    emit_status(&app_clone, &latest);
                }
                // Resolve the real install_path. Legendary picks its own
                // `<base>/<title>` subdir which may differ from our slugified
                // game name; gogdl already used the exact path we handed it.
                let final_install_path = match source_clone.as_str() {
                    "epic" => legendary_exe_for_completion
                        .as_ref()
                        .and_then(|exe| {
                            match legendary::installed_path(exe, &platform_id_clone) {
                                Ok(Some(p)) => Some(p),
                                Ok(None) => None,
                                Err(e) => {
                                    log::warn!(
                                        "[downloads] legendary list-installed failed: {} — falling back to base path",
                                        e
                                    );
                                    None
                                }
                            }
                        })
                        .unwrap_or_else(|| install_path_for_thread.clone()),
                    _ => install_path_for_thread.clone(),
                };
                runtime_clone.update(&game_id_clone, |s| {
                    s.install_path = Some(final_install_path.clone());
                });
                handle_completion(
                    &app_clone,
                    &db_clone,
                    &runtime_clone,
                    &game_id_clone,
                    &final_install_path,
                );
            }
            Ok(status) => {
                // Non-zero exit + paused flag => user cancelled/paused. Don't
                // mark as failed.
                if was_paused_before_wait {
                    if let Some(latest) = runtime_clone.get(&game_id_clone) {
                        emit_status(&app_clone, &latest);
                    }
                } else {
                    let err = format!("Runner exited {}", status);
                    runtime_clone.update(&game_id_clone, |s| {
                        s.status = DownloadStatus::Failed;
                        s.last_error = Some(err.clone());
                    });
                    if let Some(latest) = runtime_clone.get(&game_id_clone) {
                        emit_status(&app_clone, &latest);
                    }
                }
            }
            Err(e) => {
                let err = format!("Failed to wait for runner: {}", e);
                runtime_clone.update(&game_id_clone, |s| {
                    s.status = DownloadStatus::Failed;
                    s.last_error = Some(err.clone());
                });
                if let Some(latest) = runtime_clone.get(&game_id_clone) {
                    emit_status(&app_clone, &latest);
                }
            }
        }
    });

    Ok(())
}

/// Pause an active download. Returns Err if the game isn't currently
/// downloading.
pub fn pause(
    app: &AppHandle,
    runtime: &RuntimeState,
    game_id: &str,
) -> Result<(), String> {
    let state = runtime
        .get(game_id)
        .ok_or_else(|| format!("No download in flight for '{}'", game_id))?;
    if state.status != DownloadStatus::Downloading {
        return Err(format!(
            "Game '{}' is not downloading (state: {:?})",
            game_id, state.status
        ));
    }

    // Flip the state FIRST so the post-wait branch in the runner thread can
    // distinguish a user-initiated pause from a runner crash.
    runtime.update(game_id, |s| {
        s.status = DownloadStatus::Paused;
        s.speed_bps = 0;
    });

    if let Some(latest) = runtime.get(game_id) {
        emit_status(app, &latest);
    }

    let _ = runtime.pause_download_process(game_id);
    Ok(())
}

/// Resume a paused download by re-running the install command. Legendary +
/// gogdl resume natively from their on-disk state.
pub async fn resume(
    app: AppHandle,
    runtime: RuntimeState,
    db: Database,
    game_id: String,
) -> Result<(), String> {
    let state = runtime
        .get(&game_id)
        .ok_or_else(|| format!("No paused download for '{}'", game_id))?;
    if !matches!(state.status, DownloadStatus::Paused | DownloadStatus::Queued) {
        return Err(format!(
            "Game '{}' is not paused (state: {:?})",
            game_id, state.status
        ));
    }
    // Just re-enter the start flow — the runner deals with continuing.
    start(app, runtime, db, game_id).await
}

/// Cancel a download — kill the runner if running, delete the partial
/// install directory, and remove the state entry.
///
/// We DO delete the partial files because that's what a "Cancel" button
/// implies (vs Pause which preserves them). Steam-source downloads aren't
/// tracked by this orchestrator (`steam://install/<appid>` is delegated to
/// the Steam client which owns the file lifecycle), so cancel for Steam is
/// a no-op on disk.
pub fn cancel(
    app: &AppHandle,
    runtime: &RuntimeState,
    game_id: &str,
) -> Result<(), String> {
    let state = runtime
        .get(game_id)
        .ok_or_else(|| format!("No download tracked for '{}'", game_id))?;

    if matches!(state.status, DownloadStatus::Downloading | DownloadStatus::Paused) {
        runtime.kill_download_process(game_id);
    }

    // Wipe the partial install dir if we know it (and it looks safe to
    // delete — must be under our managed install directory, not a system
    // path the user typed). Steam isn't routed here but guard anyway.
    if state.source != "steam" {
        if let Some(install_path) = state.install_path.clone() {
            // Sanity: refuse to delete root-like paths.
            let normalized = install_path.replace('/', "\\");
            let segments = normalized
                .split('\\')
                .filter(|s| !s.is_empty())
                .count();
            if segments >= 3 {
                match std::fs::remove_dir_all(&install_path) {
                    Ok(()) => log::info!(
                        "[downloads] cancel: removed partial install at {}",
                        install_path
                    ),
                    Err(e) => log::warn!(
                        "[downloads] cancel: could not remove {}: {}",
                        install_path,
                        e
                    ),
                }
            } else {
                log::warn!(
                    "[downloads] cancel: refusing to delete shallow path {} (segments={})",
                    install_path,
                    segments
                );
            }
        }

        // GOG-specific: also remove the gogdl manifest so a re-install
        // doesn't try to resume from a deleted dir.
        if state.source == "gog" {
            if let Ok(appdata) = std::env::var("APPDATA") {
                let manifest = std::path::PathBuf::from(appdata)
                    .join("heroic_gogdl")
                    .join("manifests")
                    .join(&state.platform_id);
                let _ = std::fs::remove_file(&manifest);
            }
        }
    }

    // Emit one final status event so the frontend can react.
    runtime.remove(game_id);
    let _ = app.emit(
        "download-status",
        StatusEvent {
            game_id: game_id.to_string(),
            status: DownloadStatus::Failed,
            last_error: Some("Cancelled by user".to_string()),
        },
    );
    Ok(())
}

/// Retry a failed download — same as start but only valid if state is
/// Failed.
pub async fn retry(
    app: AppHandle,
    runtime: RuntimeState,
    db: Database,
    game_id: String,
) -> Result<(), String> {
    if let Some(state) = runtime.get(&game_id) {
        if state.status != DownloadStatus::Failed {
            return Err(format!(
                "Game '{}' is not failed (state: {:?})",
                game_id, state.status
            ));
        }
    }
    start(app, runtime, db, game_id).await
}

// ── Internal helpers ─────────────────────────────────────────────────

#[derive(Default, Clone, Copy)]
struct ProgressDeltaCommon {
    progress_pct: Option<f32>,
    downloaded_bytes: Option<u64>,
    total_bytes: Option<u64>,
    speed_bps: Option<u64>,
    eta_secs: Option<u32>,
}

/// Compute the per-game install path: `<base>/<sanitised game name>`. Mirrors
/// OrionCore's behaviour so resume across the two apps doesn't blow up.
fn compute_install_path(base: &str, game_name: &str) -> String {
    let sanitized: String = game_name
        .replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_")
        .trim()
        .to_string();

    // If `base` already ends with the sanitized name (user pre-set it),
    // don't double-nest.
    let base_path = std::path::PathBuf::from(base);
    let last_segment = base_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    if last_segment.eq_ignore_ascii_case(&sanitized) {
        base.to_string()
    } else {
        format!("{}\\{}", base.trim_end_matches('\\'), sanitized)
    }
}

/// Called when a download transitions to Completed. Updates the game's
/// install_path in the DB and (if auto-push is on) calls the Steam push flow.
///
/// We persist via `set_install_path_by_platform` (keyed on source +
/// platform_id) instead of `upsert_game`. Two reasons:
///   - `upsert_game` writes `exe_path = ?4` unconditionally, which can hit a
///     UNIQUE constraint if the picked exe is already pointed at by another
///     row — and the previous `let _ = db.upsert_game(...)` swallowed that
///     error, leaving install_path unwritten. That's the ABZU bug.
///   - The platform-keyed accessor scopes the write to two columns
///     (install_path + best-effort exe_path) and explicitly avoids the
///     collision, so the path is recorded even when the exe isn't.
fn handle_completion(
    app: &AppHandle,
    db: &Database,
    runtime: &RuntimeState,
    game_id: &str,
    install_path: &str,
) {
    if let Ok(Some(game)) = db.get_game_by_id(game_id) {
        if let Some(platform_id) = game.platform_id.as_deref() {
            match db.set_install_path_by_platform(
                &game.source,
                platform_id,
                install_path,
            ) {
                Ok(true) => {
                    log::info!(
                        "[downloads] handle_completion: install_path set for {}/{} → {}",
                        game.source,
                        platform_id,
                        install_path
                    );
                }
                Ok(false) => {
                    // No row matched (source, platform_id) — fall back to the
                    // id-keyed upsert path which doesn't need platform_id.
                    log::warn!(
                        "[downloads] handle_completion: no row for {}/{} — falling back to id upsert",
                        game.source,
                        platform_id
                    );
                    let mut g = game.clone();
                    g.install_path = Some(install_path.to_string());
                    if let Some(exe) = find_main_exe(install_path) {
                        g.exe_path = exe;
                    }
                    if let Err(e) = db.upsert_game(&g) {
                        log::warn!(
                            "[downloads] handle_completion fallback upsert failed: {}",
                            e
                        );
                    }
                }
                Err(e) => log::warn!(
                    "[downloads] handle_completion: set_install_path_by_platform failed for {}/{}: {}",
                    game.source,
                    platform_id,
                    e
                ),
            }
        } else {
            // No platform_id on this game (shouldn't happen for owned-library
            // installs — keeping the fallback for safety).
            let mut g = game.clone();
            g.install_path = Some(install_path.to_string());
            if let Some(exe) = find_main_exe(install_path) {
                g.exe_path = exe;
            }
            if let Err(e) = db.upsert_game(&g) {
                log::warn!("[downloads] handle_completion upsert failed: {}", e);
            }
        }
    }

    let settings = load_settings(db);
    if settings.auto_push_after_download {
        // Dispatch in a separate task — push_to_steam touches files + leveldb,
        // not the runner. We can't call the Tauri command directly from here
        // (we're not in an async fn), so just invoke the underlying writer
        // synchronously.
        let db_clone = db.clone();
        let app_clone = app.clone();
        let game_id_owned = game_id.to_string();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = auto_push(&db_clone, &game_id_owned).await {
                log::warn!("[downloads] auto-push failed for {}: {}", game_id_owned, e);
                // Best-effort: emit a status carrying the auto-push error so
                // the frontend can toast.
                let _ = app_clone.emit(
                    "download-status",
                    StatusEvent {
                        game_id: game_id_owned.clone(),
                        status: DownloadStatus::Completed,
                        last_error: Some(format!("Auto-push to Steam failed: {}", e)),
                    },
                );
            } else {
                log::info!("[downloads] auto-pushed '{}' to Steam", game_id_owned);
            }
        });
    }

    // Schedule a cleanup of the runtime entry after a short delay so the UI
    // can render the "Completed" toast before it disappears.
    let runtime_clone = runtime.clone();
    let game_id_owned = game_id.to_string();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(8)).await;
        runtime_clone.remove(&game_id_owned);
    });
}

async fn auto_push(db: &Database, game_id: &str) -> Result<(), String> {
    let game = db
        .get_game_by_id(game_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Game '{}' not found for auto-push", game_id))?;
    // Reuse the same writer the manual push command does. Calling the
    // command directly here would require an AppHandle State<Database> which
    // we already have. Carry forward an existing appid when present so
    // renames don't orphan the previous Steam entry.
    let existing_appid = db
        .get_shortcut(game_id)
        .ok()
        .flatten()
        .map(|s| s.steam_appid as u32);
    let appid = crate::services::steam_writers::push_game(&game, existing_appid)?;
    let shortcut = crate::models::Shortcut {
        game_id: game_id.to_string(),
        steam_appid: appid as u64,
        pushed_at: Some(chrono::Utc::now().timestamp()),
        status: crate::models::ShortcutStatus::Pushed,
    };
    db.upsert_shortcut(&shortcut).map_err(|e| e.to_string())?;
    Ok(())
}

/// Walks `install_path` up to 3 levels deep, returning the largest .exe that
/// doesn't look like an uninstaller / redist / launcher artifact. Mirrors
/// OrionCore's `find_main_exe`.
fn find_main_exe(install_path: &str) -> Option<String> {
    let path = std::path::Path::new(install_path);
    if !path.exists() {
        return None;
    }

    let mut best: Option<(String, u64)> = None;
    for entry in walkdir::WalkDir::new(path).max_depth(3).into_iter().flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("exe") {
            continue;
        }
        let name_lower = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if name_lower.contains("unins")
            || name_lower.contains("crash")
            || name_lower.contains("redist")
            || name_lower.contains("setup")
            || name_lower.contains("vcredist")
            || name_lower.contains("dxsetup")
        {
            continue;
        }
        if let Ok(meta) = p.metadata() {
            let size = meta.len();
            if best.as_ref().map_or(true, |(_, s)| size > *s) {
                best = Some((p.to_string_lossy().to_string(), size));
            }
        }
    }
    best.map(|(p, _)| p)
}

/// Hydrate the runtime from persisted state at app boot. Any entry that was
/// `Downloading` when we crashed/exited becomes `Paused` (see
/// `RuntimeState::load_persisted`). We don't auto-resume — the user gets to
/// see the paused chip and decide.
pub fn hydrate_from_disk(app: &AppHandle, runtime: &RuntimeState) {
    let Ok(dir) = app.path().app_data_dir() else {
        return;
    };
    let states = RuntimeState::load_persisted(&dir);
    if !states.is_empty() {
        log::info!(
            "[downloads] restored {} pending downloads from disk",
            states.len()
        );
        runtime.hydrate(states);
        // Best-effort one-shot emit so the frontend, once it boots, sees the
        // restored downloads through normal channels.
        for state in runtime.all() {
            emit_status(app, &state);
        }
    }
    // Lock the runtime to the app data dir so persists land in the right place.
    runtime.set_app_data_dir(dir);
}

