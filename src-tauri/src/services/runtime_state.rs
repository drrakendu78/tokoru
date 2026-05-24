//! In-memory store of active downloads + on-disk persistence so resumable
//! downloads survive an app restart.
//!
//! Ported from OrionCore's `services/runtime_state.rs` and adapted for
//! Tokoru:
//! - Tokoru supports **multiple parallel** downloads (one per game id),
//!   so the state is a `HashMap<GameId, DownloadState>` instead of a single
//!   slot. The `max_parallel` setting gates how many can be `Downloading` at
//!   the same time — anything past that becomes `Queued`.
//! - We don't track running games here (the process `watcher` already does
//!   that for playtime sessions).
//! - We don't track Steam achievements cursor here (we don't have that
//!   feature in Tokoru yet).
//!
//! Persistence lives in `<app_data_dir>/pending_downloads.json`. Active and
//! paused downloads are serialised on every status transition so an app crash
//! / restart can resume them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const PENDING_DL_FILE: &str = "pending_downloads.json";

/// Top-level enum for the install lifecycle of a single game.
///
/// `Queued` → user asked to install but `max_parallel` is saturated.
/// `Downloading` → child process running, progress events flowing.
/// `Paused` → child process stopped via graceful taskkill, on-disk state
///   intact (legendary / gogdl resume natively on next install call).
/// `Failed` → child exited non-zero; `last_error` carries the message.
/// `Completed` → install finished; the entry typically gets removed shortly
///   after but stays in the map long enough for the UI to render the toast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadStatus {
    Queued,
    Downloading,
    Paused,
    Failed,
    Completed,
}

/// Snapshot of one download. Cloneable so commands can copy it out of the
/// mutex without holding the lock longer than necessary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadState {
    pub game_id: String,
    /// "epic" or "gog" — used to dispatch to the right runner.
    pub source: String,
    /// Platform-side id (Epic appName, GOG game id) — what we hand to the
    /// runner CLI.
    pub platform_id: String,
    /// Human-facing title for toasts / event payloads.
    pub game_name: String,
    pub status: DownloadStatus,
    pub progress_pct: f32,
    pub speed_bps: u64,
    pub eta_secs: u32,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub last_error: Option<String>,
    /// Where the runner is installing. Persisted so resume uses the same
    /// directory across app restarts.
    pub install_path: Option<String>,
    /// Language code passed to the runner (`--lang` for gogdl, `--language`
    /// for legendary). Persisted for the same reason.
    pub language: Option<String>,
    /// Tracked separately from `status` so the watcher thread can detect
    /// "kill happened, runner exited, but it was a user-initiated pause" vs
    /// an actual failure.
    #[serde(skip)]
    pub pid: Option<u32>,
    /// "legendary.exe" / "gogdl.exe" — used as a fallback for taskkill-by-name
    /// when PID-based kill misses child workers.
    #[serde(skip)]
    pub exe_name: Option<String>,
}

impl DownloadState {
    pub fn new(
        game_id: String,
        source: String,
        platform_id: String,
        game_name: String,
    ) -> Self {
        Self {
            game_id,
            source,
            platform_id,
            game_name,
            status: DownloadStatus::Queued,
            progress_pct: 0.0,
            speed_bps: 0,
            eta_secs: 0,
            downloaded_bytes: 0,
            total_bytes: 0,
            last_error: None,
            install_path: None,
            language: None,
            pid: None,
            exe_name: None,
        }
    }
}

/// Shared in-memory + on-disk state. Holds `Arc<Mutex<...>>` internally so
/// cloning the struct is cheap and `Send + Sync` for tauri::manage.
#[derive(Clone)]
pub struct RuntimeState {
    downloads: Arc<Mutex<HashMap<String, DownloadState>>>,
    app_data_dir: Arc<Mutex<Option<PathBuf>>>,
    /// Steam appids the user dismissed from the Downloads UI — the ACF
    /// watcher checks this set and skips re-adding them. Cleared when the
    /// user installs / resumes the game manually.
    dismissed_steam_appids: Arc<Mutex<std::collections::HashSet<String>>>,
    /// Last time the CDP watcher pushed a progress update for a game id.
    /// The manifest watcher uses this to step out of the way when CDP is
    /// driving — otherwise both write their own values to the same
    /// `DownloadState` and the UI flickers between them every tick.
    cdp_last_seen: Arc<Mutex<HashMap<String, Instant>>>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            downloads: Arc::new(Mutex::new(HashMap::new())),
            app_data_dir: Arc::new(Mutex::new(None)),
            dismissed_steam_appids: Arc::new(Mutex::new(std::collections::HashSet::new())),
            cdp_last_seen: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl RuntimeState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_app_data_dir(&self, dir: PathBuf) {
        if let Ok(mut guard) = self.app_data_dir.lock() {
            *guard = Some(dir);
        }
    }

    fn pending_path(&self) -> Option<PathBuf> {
        self.app_data_dir
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|d| d.join(PENDING_DL_FILE)))
    }

    /// Snapshot for serialisation. Drops `pid` / `exe_name` (#[serde(skip)])
    /// because those are only meaningful while the runner process is alive.
    fn persist(&self) {
        let Some(path) = self.pending_path() else { return; };
        let snapshot: Vec<DownloadState> = match self.downloads.lock() {
            Ok(g) => g
                .values()
                // Only persist resumable / failed states. Completed ones are
                // cleaned up on the next boot anyway.
                .filter(|d| {
                    matches!(
                        d.status,
                        DownloadStatus::Queued
                            | DownloadStatus::Downloading
                            | DownloadStatus::Paused
                            | DownloadStatus::Failed
                    )
                })
                .cloned()
                .collect(),
            Err(_) => return,
        };

        if snapshot.is_empty() {
            let _ = std::fs::remove_file(&path);
            return;
        }

        if let Ok(json) = serde_json::to_string_pretty(&snapshot) {
            let _ = std::fs::write(&path, json);
        }
    }

    /// Load any persisted downloads from disk. Called once at app boot.
    /// Returns the loaded states so the caller can decide whether to
    /// auto-resume them. Statuses that were `Downloading` when the app
    /// crashed get normalised to `Paused` (we don't know if the partial
    /// runner state is consistent; safer to make the user explicitly resume).
    pub fn load_persisted(app_data_dir: &Path) -> Vec<DownloadState> {
        let path = app_data_dir.join(PENDING_DL_FILE);
        if !path.exists() {
            return Vec::new();
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                log::warn!(
                    "[runtime_state] failed to read pending downloads at {:?}: {}",
                    path,
                    e
                );
                return Vec::new();
            }
        };

        let mut states: Vec<DownloadState> = serde_json::from_str(&content)
            .unwrap_or_else(|e| {
                log::warn!("[runtime_state] pending downloads malformed: {}", e);
                Vec::new()
            });

        for s in &mut states {
            if s.status == DownloadStatus::Downloading {
                s.status = DownloadStatus::Paused;
            }
            s.pid = None;
            s.exe_name = None;

            // Sanity cap: a previous bug let `downloaded_bytes` exceed
            // `total_bytes` (Steam chunk dir held compressed + decompressed
            // briefly, inflating the size). Clamp at boot so a stale entry
            // doesn't render as 175% in the UI.
            if s.total_bytes > 0 && s.downloaded_bytes > s.total_bytes {
                s.downloaded_bytes = s.total_bytes;
            }
            if s.total_bytes > 0 {
                s.progress_pct =
                    ((s.downloaded_bytes as f64 / s.total_bytes as f64) * 100.0)
                        .min(100.0) as f32;
            } else if s.progress_pct > 100.0 {
                s.progress_pct = 100.0;
            }
        }
        states
    }

    /// Populate the in-memory map from a persisted snapshot.
    pub fn hydrate(&self, states: Vec<DownloadState>) {
        if let Ok(mut guard) = self.downloads.lock() {
            for s in states {
                guard.insert(s.game_id.clone(), s);
            }
        }
    }

    // ── Per-game accessors ─────────────────────────────────────────────

    pub fn get(&self, game_id: &str) -> Option<DownloadState> {
        self.downloads
            .lock()
            .ok()
            .and_then(|g| g.get(game_id).cloned())
    }

    pub fn all(&self) -> Vec<DownloadState> {
        self.downloads
            .lock()
            .map(|g| g.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Insert or replace the state for a game. Persists on every call so the
    /// on-disk file always reflects truth.
    pub fn upsert(&self, state: DownloadState) {
        if let Ok(mut guard) = self.downloads.lock() {
            guard.insert(state.game_id.clone(), state);
        }
        self.persist();
    }

    /// Apply a mutation closure to an existing entry. No-op if the entry is
    /// missing. Returns whether the mutation ran. Persists on success.
    pub fn update<F>(&self, game_id: &str, f: F) -> bool
    where
        F: FnOnce(&mut DownloadState),
    {
        let ran = match self.downloads.lock() {
            Ok(mut guard) => match guard.get_mut(game_id) {
                Some(state) => {
                    f(state);
                    true
                }
                None => false,
            },
            Err(_) => false,
        };
        if ran {
            self.persist();
        }
        ran
    }

    /// Mark a Steam-source game as "user-dismissed" so the Steam ACF watcher
    /// won't re-add it on the next tick. Cleared when the user manually
    /// resumes / re-downloads / starts a new install for the same game. The
    /// set is in-memory only — restarting Tokoru shows the row again
    /// (which is fine, the user can just dismiss it again).
    pub fn mark_steam_dismissed(&self, appid: &str) {
        if let Ok(mut guard) = self.dismissed_steam_appids.lock() {
            guard.insert(appid.to_string());
        }
    }

    pub fn is_steam_dismissed(&self, appid: &str) -> bool {
        self.dismissed_steam_appids
            .lock()
            .map(|g| g.contains(appid))
            .unwrap_or(false)
    }

    /// Record that the CDP watcher just pushed authoritative data for this
    /// game id. The manifest watcher checks `cdp_is_recent` before writing
    /// its own progress values so the two don't fight each other every tick.
    pub fn mark_cdp_seen(&self, game_id: &str) {
        if let Ok(mut g) = self.cdp_last_seen.lock() {
            g.insert(game_id.to_string(), Instant::now());
        }
    }

    /// True when the CDP watcher pushed data for this game within
    /// `max_age`. Used by `steam_dl_watcher` to skip its own runtime
    /// updates when CDP is in charge — preventing the "value flickers
    /// between two sources" bug.
    pub fn cdp_is_recent(&self, game_id: &str, max_age: Duration) -> bool {
        self.cdp_last_seen
            .lock()
            .ok()
            .and_then(|g| g.get(game_id).copied())
            .map(|t| t.elapsed() < max_age)
            .unwrap_or(false)
    }

    pub fn clear_steam_dismissed(&self, appid: &str) {
        if let Ok(mut guard) = self.dismissed_steam_appids.lock() {
            guard.remove(appid);
        }
    }

    pub fn remove(&self, game_id: &str) {
        if let Ok(mut guard) = self.downloads.lock() {
            guard.remove(game_id);
        }
        self.persist();
    }

    /// Count of currently-active downloads (Downloading state). Used to gate
    /// `max_parallel`.
    pub fn active_count(&self) -> usize {
        self.downloads
            .lock()
            .map(|g| {
                g.values()
                    .filter(|d| d.status == DownloadStatus::Downloading)
                    .count()
            })
            .unwrap_or(0)
    }

    // ── Process management ─────────────────────────────────────────────

    fn is_pid_running(pid: u32) -> bool {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid)])
            .output();
        match output {
            Ok(out) => {
                if !out.status.success() {
                    return false;
                }
                let stdout = String::from_utf8_lossy(&out.stdout).to_lowercase();
                !stdout.contains("no tasks are running") && stdout.contains(&pid.to_string())
            }
            Err(_) => false,
        }
    }

    fn wait_for_pid_exit(pid: u32, timeout_ms: u64) -> bool {
        let step_ms = 250_u64;
        let mut elapsed_ms = 0_u64;
        while elapsed_ms < timeout_ms {
            if !Self::is_pid_running(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(step_ms));
            elapsed_ms += step_ms;
        }
        !Self::is_pid_running(pid)
    }

    /// Forcefully kill a download's runner process. Used for cancel — we
    /// don't care if the runner gets to flush state cleanly.
    pub fn kill_download_process(&self, game_id: &str) {
        let (pid, exe_name) = match self.get(game_id) {
            Some(s) => (s.pid, s.exe_name),
            None => (None, None),
        };

        if let Some(pid) = pid {
            let result = std::process::Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .output();
            if let Ok(out) = &result {
                log::info!(
                    "[runtime_state] taskkill /F /PID {}: status={}",
                    pid,
                    out.status
                );
            }
        }

        // Fallback by image name only if NO other download is running with
        // the same runner — otherwise we'd kill an unrelated download.
        if let Some(ref name) = exe_name {
            let runner_in_use_by_others = self
                .downloads
                .lock()
                .map(|g| {
                    g.values().any(|d| {
                        d.game_id != game_id
                            && d.status == DownloadStatus::Downloading
                            && d.exe_name.as_deref() == Some(name)
                    })
                })
                .unwrap_or(false);

            if !runner_in_use_by_others {
                let result = std::process::Command::new("taskkill")
                    .args(["/F", "/IM", name])
                    .output();
                if let Ok(out) = &result {
                    log::info!(
                        "[runtime_state] taskkill /F /IM {}: status={}",
                        name,
                        out.status
                    );
                }
            }
        }

        self.update(game_id, |s| {
            s.pid = None;
            s.exe_name = None;
        });
    }

    /// Graceful pause — sends `CTRL_BREAK_EVENT` to the runner's process
    /// group (set via `CREATE_NEW_PROCESS_GROUP` at spawn time) so legendary /
    /// gogdl can write their on-disk resume checkpoint before exiting. Waits
    /// up to 10s, then escalates to `taskkill /F /T`. Returns whether the
    /// process actually stopped.
    pub fn pause_download_process(&self, game_id: &str) -> bool {
        let (pid, _exe_name) = match self.get(game_id) {
            Some(s) => (s.pid, s.exe_name),
            None => (None, None),
        };

        let Some(pid) = pid else {
            // Nothing to kill — already stopped or never started.
            self.update(game_id, |s| {
                s.pid = None;
                s.exe_name = None;
            });
            return true;
        };

        // 1) Send CTRL_BREAK_EVENT to the process group so the Python runner
        //    catches it as KeyboardInterrupt, finishes the in-flight chunk
        //    write, persists its resume manifest, then exits cleanly.
        #[cfg(windows)]
        {
            unsafe extern "system" {
                fn GenerateConsoleCtrlEvent(dwCtrlEvent: u32, dwProcessGroupId: u32) -> i32;
            }
            const CTRL_BREAK_EVENT: u32 = 1;
            // SAFETY: PID is valid as long as the process exists; the FFI call
            // is a system function that doesn't dereference Rust state.
            let _ok = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) };
            log::info!(
                "[runtime_state] sent CTRL_BREAK_EVENT to pid {} for {}",
                pid, game_id
            );
        }

        // 2) Give the runner time to flush its checkpoint. Legendary/gogdl can
        //    be mid-chunk-write (multi-MB), 10s is realistic.
        if Self::wait_for_pid_exit(pid, 10_000) {
            self.update(game_id, |s| {
                s.pid = None;
                s.exe_name = None;
            });
            log::info!(
                "[runtime_state] pid {} exited gracefully after CTRL_BREAK ({} ms)",
                pid, "<=10000"
            );
            return true;
        }

        // 3) Fallback: hard kill the process tree. Checkpoint may be partial
        //    but legendary/gogdl can usually recover from on-disk chunks.
        log::warn!(
            "[runtime_state] pid {} for {} ignored CTRL_BREAK — falling back to taskkill /F",
            pid, game_id
        );
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();

        let stopped = Self::wait_for_pid_exit(pid, 3000);
        if stopped {
            self.update(game_id, |s| {
                s.pid = None;
                s.exe_name = None;
            });
        } else {
            log::warn!(
                "[runtime_state] pid {} for {} still alive after pause attempts",
                pid,
                game_id
            );
        }
        stopped
    }
}
