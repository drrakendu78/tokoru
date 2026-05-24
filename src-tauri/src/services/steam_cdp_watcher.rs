//! Steam CEF DevTools Protocol watcher — real-time download progress.
//!
//! ## Why this exists
//!
//! Two existing watchers (`steam_dl_watcher`, `steam_log_tailer`) sniff at
//! Steam's on-disk side effects (appmanifest_*.acf and content_log.txt).
//! They both have multi-second lag because Steam writes those files in
//! batches.
//!
//! Steam's own UI doesn't read those files — it talks to a JS API on its
//! own CEF webview process: `SteamClient.Downloads.RegisterForDownload*`.
//! By enabling CEF's remote debugging port and connecting to the same
//! webview via the DevTools Protocol, we can subscribe to the same events
//! Steam paints on its UI — true real-time.
//!
//! ## Proof of concept
//!
//! https://github.com/luthor112/steam-taskbar-progress (frontend/index.tsx)
//! shows the exact JS API surface.
//!
//! ## Architecture
//!
//! 1. **Bootstrap**: drop `<SteamPath>\.cef-enable-remote-debugging` marker
//!    so the next Steam launch boots with the debug port (8080). We do NOT
//!    restart Steam — the manifest/log watchers remain as fallbacks for the
//!    case the user hasn't restarted yet.
//! 2. **Connect loop**: every 10s, hit `http://localhost:8080/json`. Pick the
//!    target whose `url` matches `steamloopback.host/index.html` (or whose
//!    title is "Steam"). Open the websocket on that target.
//! 3. **Register callbacks**: send `Runtime.evaluate` with the JS that calls
//!    `SteamClient.Downloads.RegisterForDownloadOverview` + `*ForDownloadItems`
//!    and stashes the latest values on `window.__steamshelf`.
//! 4. **Poll loop**: every 500ms, `Runtime.evaluate` to grab the stashed
//!    values, parse JSON, push into RuntimeState.
//!
//! Status from CDP is **authoritative** when the watcher is connected —
//! the manifest watcher will be overridden. Byte counters merge with
//! `max(...)` so we never go backwards.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::services::db::Database;
use crate::services::downloads::{ProgressEvent, StatusEvent};
use crate::services::platforms::local_detect::get_steam_path;
use crate::services::runtime_state::{DownloadState, DownloadStatus, RuntimeState};

/// Steam's default CEF remote debugging port. Hardcoded by the Steam client
/// when the `.cef-enable-remote-debugging` marker is present in its install
/// directory.
const CEF_DEBUG_PORT: u16 = 8080;

/// How often to retry the `GET /json` target discovery when Steam isn't
/// running (or the debug port hasn't bound yet).
const CONNECT_RETRY_SECS: u64 = 10;

/// Reconnect delay after a websocket failure or eval throw.
const RECONNECT_DELAY_SECS: u64 = 5;

/// How often to poll `window.__steamshelf` once connected.
const POLL_INTERVAL_MS: u64 = 500;

/// Mark the appid-zero "no active download" state as terminal only after
/// this many consecutive polls report it. Steam sometimes drops to
/// `update_appid: 0` for a beat between two queued downloads.
const NO_DOWNLOAD_POLL_THRESHOLD: u32 = 3;

/// One DevTools target returned by `/json`. We only need a few fields.
#[derive(Debug, Deserialize)]
struct CdpTarget {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(rename = "webSocketDebuggerUrl", default)]
    web_socket_debugger_url: Option<String>,
    #[serde(default, rename = "type")]
    target_type: String,
}

/// Parsed shape of what our injected JS stashes on `window.__steamshelf`.
#[derive(Debug, Deserialize, Default)]
struct TokoruBlob {
    #[serde(default)]
    overview: Option<DownloadOverview>,
}

/// Steam's actual download overview shape (observed via CEF DevTools).
///
/// Schema confirmed in the wild:
/// ```json
/// {
///   "update_appid": 2322010,
///   "update_state": "Updating" | "None" | "Paused" | ...,
///   "update_state_flags": <bitfield>,
///   "update_network_bytes_per_second": <live, bytes/s>,
///   "update_peak_network_bytes_per_second": <session peak>,
///   "update_disc_bytes_per_second": <live disk write rate>,
///   "progress": [{"bytes_in_progress": ..., "bytes_total": ..., "estimated_time_remaining_sec": ...}, ...]
/// }
/// ```
///
/// The `progress` array carries one bucket per phase Steam tracks for the
/// current update (download / install / validate / shader pre-cache, etc.).
/// Steam's own UI takes the slowest phase to drive its progress bar — we
/// mirror that with `min(bucket_pct)`.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
struct DownloadOverview {
    update_appid: u64,
    update_state: String,
    update_state_flags: u32,
    update_network_bytes_per_second: u64,
    update_disc_bytes_per_second: u64,
    progress: Vec<ProgressBucket>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
struct ProgressBucket {
    bytes_in_progress: u64,
    bytes_total: u64,
    estimated_time_remaining_sec: i64,
}

/// Cache the target URL that last worked, so we try it first on the next
/// reconnect without having to iterate every CEF target.
#[derive(Default)]
struct CachedTarget {
    inner: Mutex<Option<String>>,
}

/// Entry point — spawn the CDP watcher as a long-lived task.
pub fn start(app: AppHandle, db: Database, runtime: RuntimeState) {
    tauri::async_runtime::spawn(async move {
        watcher_loop(app, db, runtime).await;
    });
}

async fn watcher_loop(app: AppHandle, db: Database, runtime: RuntimeState) {
    let Some(steam_dir) = get_steam_path() else {
        log::info!("[steam_cdp_watcher] Steam not installed — watcher disabled");
        return;
    };

    // Drop the CEF debug marker file if it's not already there. Empty file,
    // Steam looks at presence only. User has to restart Steam for the port
    // to open.
    let marker = steam_dir.join(".cef-enable-remote-debugging");
    if !marker.exists() {
        match std::fs::File::create(&marker) {
            Ok(_) => log::warn!(
                "[steam_cdp_watcher] Steam CEF debug not enabled — created marker at {}. Restart Steam to enable real-time download tracking.",
                marker.display()
            ),
            Err(e) => log::warn!(
                "[steam_cdp_watcher] failed to create CEF debug marker {}: {} — real-time tracking unavailable until enabled manually",
                marker.display(),
                e
            ),
        }
    }

    log::info!(
        "[steam_cdp_watcher] started (port={}, poll={}ms)",
        CEF_DEBUG_PORT,
        POLL_INTERVAL_MS
    );

    let cached_target = Arc::new(CachedTarget::default());
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    loop {
        // Try to find + connect to a target. On failure, sleep and retry.
        let ws = match connect_to_steam(&http, &cached_target).await {
            Ok(ws) => ws,
            Err(e) => {
                log::debug!(
                    "[steam_cdp_watcher] Steam unreachable, retrying in {}s: {}",
                    CONNECT_RETRY_SECS,
                    e
                );
                time::sleep(Duration::from_secs(CONNECT_RETRY_SECS)).await;
                continue;
            }
        };

        // Run the polling session until the websocket dies or an eval
        // throws. Then go back around for a reconnect.
        if let Err(e) = session_loop(ws, &app, &db, &runtime).await {
            log::warn!(
                "[steam_cdp_watcher] session ended: {} — reconnecting in {}s",
                e,
                RECONNECT_DELAY_SECS
            );
            // Invalidate cached target — that one may have gone away.
            if let Ok(mut g) = cached_target.inner.try_lock() {
                *g = None;
            }
            time::sleep(Duration::from_secs(RECONNECT_DELAY_SECS)).await;
        }
    }
}

/// Find a working CDP target and open a websocket to it. Returns an error if
/// no target accepts our SteamClient probe.
async fn connect_to_steam(
    http: &reqwest::Client,
    cached_target: &Arc<CachedTarget>,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>, String> {
    let resp = http
        .get(format!("http://127.0.0.1:{}/json", CEF_DEBUG_PORT))
        .send()
        .await
        .map_err(|e| format!("GET /json failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("GET /json returned status {}", resp.status()));
    }

    let targets: Vec<CdpTarget> = resp
        .json()
        .await
        .map_err(|e| format!("parse /json failed: {}", e))?;

    if targets.is_empty() {
        return Err("no CDP targets".to_string());
    }

    // Build candidate list. Preference order:
    //   1. Cached last-working URL (if still listed)
    //   2. Targets whose url is steamloopback.host/index.html
    //   3. Targets whose title is exactly "Steam"
    //   4. Anything with type=page
    let cached = cached_target.inner.lock().await.clone();

    let mut ordered: Vec<&CdpTarget> = Vec::with_capacity(targets.len());
    if let Some(ref cu) = cached {
        for t in &targets {
            if t.web_socket_debugger_url.as_deref() == Some(cu.as_str()) {
                ordered.push(t);
                break;
            }
        }
    }
    for t in &targets {
        if t.url.contains("steamloopback.host/index.html") && !ordered.iter().any(|o| std::ptr::eq(*o, t)) {
            ordered.push(t);
        }
    }
    for t in &targets {
        if t.title == "Steam" && !ordered.iter().any(|o| std::ptr::eq(*o, t)) {
            ordered.push(t);
        }
    }
    for t in &targets {
        if t.target_type == "page" && !ordered.iter().any(|o| std::ptr::eq(*o, t)) {
            ordered.push(t);
        }
    }

    if ordered.is_empty() {
        return Err("no candidate targets after filtering".to_string());
    }

    // Try each candidate: open websocket, send the registration JS. First one
    // where eval returns "ok" wins.
    let mut last_err: Option<String> = None;
    for cand in ordered {
        let Some(ws_url) = cand.web_socket_debugger_url.clone() else {
            continue;
        };
        match try_register(&ws_url).await {
            Ok(ws) => {
                log::info!(
                    "[steam_cdp_watcher] CDP target connected: title={:?} url={:?}",
                    cand.title,
                    cand.url
                );
                if let Ok(mut g) = cached_target.inner.try_lock() {
                    *g = Some(ws_url);
                }
                return Ok(ws);
            }
            Err(e) => {
                log::debug!(
                    "[steam_cdp_watcher] target {:?} rejected: {}",
                    cand.title,
                    e
                );
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| "all candidate targets failed".to_string()))
}

/// Open a websocket to the given CDP url, send Runtime.enable and the
/// registration eval. Returns the live websocket on success.
async fn try_register(
    ws_url: &str,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>, String> {
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url)
        .await
        .map_err(|e| format!("ws connect: {}", e))?;

    // 1) Runtime.enable.
    send_cdp(
        &mut ws,
        1,
        "Runtime.enable",
        Value::Object(serde_json::Map::new()),
    )
    .await?;
    let _ = recv_response(&mut ws, 1).await?;

    // 2) Inject the listener. The expression registers two callbacks and
    //    keeps the most recent payload on a global. We wrap in a try/catch
    //    so a throw inside SteamClient surfaces as a string result and not
    //    a CDP exceptionDetails frame we'd have to parse.
    const INJECT_JS: &str = "(()=>{ try { if (typeof SteamClient === 'undefined' || !SteamClient.Downloads) return 'no-steamclient'; window.__steamshelf = window.__steamshelf || { overview: null, items: [] }; if (!window.__steamshelf._registered) { SteamClient.Downloads.RegisterForDownloadOverview(e => { window.__steamshelf.overview = e; }); SteamClient.Downloads.RegisterForDownloadItems((isDl, items) => { window.__steamshelf.items = items || []; }); window.__steamshelf._registered = true; } return 'ok'; } catch(e) { return 'err:' + (e && e.message || String(e)); } })()";

    let params = json!({
        "expression": INJECT_JS,
        "awaitPromise": false,
        "returnByValue": true,
    });
    send_cdp(&mut ws, 2, "Runtime.evaluate", params).await?;
    let resp = recv_response(&mut ws, 2).await?;

    let value = resp
        .get("result")
        .and_then(|r| r.get("result"))
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if value != "ok" {
        return Err(format!("registration returned {:?}", value));
    }
    log::info!("[steam_cdp_watcher] eval registration succeeded on target");
    Ok(ws)
}

/// Run the poll loop until the websocket dies. Errors out so the outer loop
/// reconnects.
async fn session_loop(
    mut ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    app: &AppHandle,
    db: &Database,
    runtime: &RuntimeState,
) -> Result<(), String> {
    let mut next_id: u64 = 100;
    let mut no_download_count: u32 = 0;
    let mut last_status_by_game: std::collections::HashMap<String, DownloadStatus> =
        std::collections::HashMap::new();

    loop {
        time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;

        // Drain any unsolicited frames so the connection stays healthy.
        // tungstenite needs us to consume pings / responses to keep the
        // socket from backing up. We use a non-blocking peek by polling
        // with a timeout of 0.
        drain_pending(&mut ws).await;

        // Read the stash.
        next_id += 1;
        let id = next_id;
        let params = json!({
            "expression": "JSON.stringify({ overview: window.__steamshelf?.overview, items: window.__steamshelf?.items })",
            "returnByValue": true,
        });
        send_cdp(&mut ws, id, "Runtime.evaluate", params).await?;
        let resp = recv_response(&mut ws, id).await?;

        // Eval-side exception? Treat as a fatal session error so the outer
        // loop reconnects (SteamClient may have been swapped under us).
        if let Some(details) = resp
            .get("result")
            .and_then(|r| r.get("exceptionDetails"))
        {
            return Err(format!("eval threw: {}", details));
        }

        let payload_str = resp
            .get("result")
            .and_then(|r| r.get("result"))
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if payload_str.is_empty() {
            continue;
        }

        let blob: TokoruBlob = match serde_json::from_str(payload_str) {
            Ok(b) => b,
            Err(e) => {
                log::warn!(
                    "[steam_cdp_watcher] payload parse failed: {} — raw={}",
                    e,
                    truncate(payload_str, 400)
                );
                continue;
            }
        };

        // Diagnostic: print a sample of incoming payloads at INFO so we can
        // see what Steam sends when behavior is surprising. Throttled — one
        // sample every ~10 polls (~5s) and only when an overview is present.
        static mut LOG_COUNTER: u32 = 0;
        let should_log = unsafe {
            LOG_COUNTER = LOG_COUNTER.wrapping_add(1);
            LOG_COUNTER % 10 == 1
        };
        if should_log && blob.overview.is_some() {
            log::info!(
                "[steam_cdp_watcher] payload sample: raw={}",
                truncate(payload_str, 1500)
            );
        }

        // Apply to runtime.
        apply_blob(
            &blob,
            app,
            db,
            runtime,
            &mut last_status_by_game,
            &mut no_download_count,
        );
    }
}

/// Truncate a string for logging without panicking on multi-byte boundaries.
fn truncate(s: &str, n: usize) -> String {
    let mut end = n.min(s.len());
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    s[..end].to_string()
}

/// Push the parsed blob into RuntimeState using Steam's CDP overview as
/// the authoritative source. Steam's UI is built on the same data, so this
/// matches what the user sees in Steam itself — byte for byte, no drift,
/// no ratio guesswork.
fn apply_blob(
    blob: &TokoruBlob,
    app: &AppHandle,
    db: &Database,
    runtime: &RuntimeState,
    last_status_by_game: &mut std::collections::HashMap<String, DownloadStatus>,
    no_download_count: &mut u32,
) {
    let Some(overview) = blob.overview.as_ref() else {
        // No overview in this poll — leave the manifest watcher to handle
        // whatever's in flight.
        return;
    };

    let appid = overview.update_appid;
    if appid == 0 {
        // Steam reports no active download. The manifest watcher catches
        // the completion / pause transition through StateFlags — we don't
        // need to flap status here.
        *no_download_count = no_download_count.saturating_add(1);
        return;
    }
    *no_download_count = 0;

    let appid_str = appid.to_string();
    if runtime.is_steam_dismissed(&appid_str) {
        return;
    }

    let Ok(Some(game_id)) = db.find_steam_id_by_appid(&appid_str) else {
        // Game not in our DB; ignore (manifest watcher will skip too).
        return;
    };

    // Steam's `progress` array carries one bucket per phase tracked for the
    // current update (download / install / verify / shaders / pre-allocate).
    // Different phases pre-populate at different times:
    //   * `bytes_total == 0` → phase not yet announced; skip.
    //   * `bytes_in_progress == bytes_total` → phase already complete
    //     (e.g. file pre-allocation done early on). It would drag the min
    //     down to 1.0 if we included it — but its 100% isn't the user's
    //     current bottleneck, so skip.
    //   * `bytes_in_progress == 0` → phase queued, not started. Skip.
    //   * Otherwise it's in flight — keep.
    let active_buckets: Vec<&ProgressBucket> = overview
        .progress
        .iter()
        .filter(|b| {
            b.bytes_total > 0 && b.bytes_in_progress > 0 && b.bytes_in_progress < b.bytes_total
        })
        .collect();

    if active_buckets.is_empty() {
        // Either we're between phases or Steam hasn't started writing yet.
        // Don't push noise to the runtime — let the manifest watcher
        // handle the in-between state.
        return;
    }

    // Combined percent: min across all in-flight phases — the bottleneck
    // drives the bar. Matches Steam's own UI.
    let combined_pct = active_buckets
        .iter()
        .map(|b| (b.bytes_in_progress as f64 / b.bytes_total as f64).clamp(0.0, 1.0))
        .fold(1.0_f64, f64::min);

    // "X GO / Y GO" line: pick the bucket with the SMALLEST bytes_total —
    // the compressed network bucket (totals are: compressed download <
    // decompressed staging < pre-allocation). Steam shows this number as
    // "Téléchargement des données" in its UI.
    let primary = *active_buckets
        .iter()
        .min_by(|a, b| a.bytes_total.cmp(&b.bytes_total))
        .unwrap();
    let total_bytes = primary.bytes_total;
    let downloaded_bytes = primary.bytes_in_progress.min(total_bytes);

    let speed_bps = overview.update_network_bytes_per_second;

    // ETA: take the LARGEST estimated_time_remaining_sec across active
    // buckets — the slowest phase determines when the whole download
    // finishes. Steam itself encodes "unknown" as -1, which we map to 0.
    let eta_secs: u32 = active_buckets
        .iter()
        .map(|b| b.estimated_time_remaining_sec.max(0) as u64)
        .max()
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;

    // Status: derived from Steam's own `update_state` string and the live
    // network rate. "Paused" string + zero network = clearly paused;
    // "Updating" / "Installing" / other non-empty = downloading.
    let state_lc = overview.update_state.to_ascii_lowercase();
    let cdp_status: DownloadStatus = if combined_pct >= 1.0 {
        DownloadStatus::Completed
    } else if state_lc.contains("paus") || state_lc.contains("suspend") {
        DownloadStatus::Paused
    } else if state_lc.is_empty() || state_lc == "none" {
        // Steam hasn't labelled the state yet — fall back to existing.
        runtime
            .get(&game_id)
            .map(|s| s.status)
            .unwrap_or(DownloadStatus::Downloading)
    } else {
        DownloadStatus::Downloading
    };

    let current = runtime.get(&game_id);
    let prior_status = current.as_ref().map(|c| c.status);
    let install_path_str = current.as_ref().and_then(|c| c.install_path.clone());

    if current.is_some() {
        runtime.update(&game_id, |s| {
            s.source = "steam".to_string();
            s.platform_id = appid_str.clone();
            s.status = cdp_status;
            s.downloaded_bytes = downloaded_bytes;
            s.total_bytes = total_bytes;
            s.progress_pct = (combined_pct * 100.0) as f32;
            s.speed_bps = speed_bps;
            s.eta_secs = eta_secs;
            s.last_error = None;
        });
    } else {
        // Seed a minimal entry — the manifest watcher will flesh out
        // `game_name` and `install_path` on its next tick.
        let mut state = DownloadState::new(
            game_id.clone(),
            "steam".to_string(),
            appid_str.clone(),
            appid_str.clone(),
        );
        state.status = cdp_status;
        state.downloaded_bytes = downloaded_bytes;
        state.total_bytes = total_bytes;
        state.progress_pct = (combined_pct * 100.0) as f32;
        state.speed_bps = speed_bps;
        state.eta_secs = eta_secs;
        state.install_path = install_path_str;
        runtime.upsert(state);
    }
    // Tell the manifest watcher CDP just wrote this game id, so it stays
    // out of the way for the next few ticks.
    runtime.mark_cdp_seen(&game_id);

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

    let last_known = last_status_by_game.get(&game_id).copied().or(prior_status);
    if last_known != Some(cdp_status) {
        let _ = app.emit(
            "download-status",
            StatusEvent {
                game_id: game_id.clone(),
                status: cdp_status,
                last_error: None,
            },
        );
        last_status_by_game.insert(game_id.clone(), cdp_status);

        if cdp_status == DownloadStatus::Completed {
            let runtime_clone = runtime.clone();
            let id_owned = game_id.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(8)).await;
                runtime_clone.remove(&id_owned);
            });
        }
    }
}

// ── CDP protocol helpers ──────────────────────────────────────────────────

/// Send a CDP request with `{id, method, params}`. The id lets us pair it
/// with its response.
async fn send_cdp(
    ws: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
    id: u64,
    method: &str,
    params: Value,
) -> Result<(), String> {
    let msg = json!({
        "id": id,
        "method": method,
        "params": params,
    });
    ws.send(Message::Text(msg.to_string()))
        .await
        .map_err(|e| format!("ws send: {}", e))
}

/// Read frames until we see the one whose `id` matches. Ignore unrelated
/// notifications.
async fn recv_response(
    ws: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
    expected_id: u64,
) -> Result<Value, String> {
    // 3-second window for any one response — CDP usually replies < 50ms.
    let deadline = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => {
                return Err(format!("timeout waiting for response id={}", expected_id));
            }
            msg = ws.next() => {
                let msg = msg.ok_or_else(|| "ws closed".to_string())?
                    .map_err(|e| format!("ws recv: {}", e))?;
                match msg {
                    Message::Text(t) => {
                        let v: Value = serde_json::from_str(&t)
                            .map_err(|e| format!("json: {}", e))?;
                        if v.get("id").and_then(|x| x.as_u64()) == Some(expected_id) {
                            return Ok(v);
                        }
                        // Otherwise it's a notification — ignore and keep
                        // reading.
                    }
                    Message::Ping(p) => {
                        let _ = ws.send(Message::Pong(p)).await;
                    }
                    Message::Close(_) => return Err("ws closed".to_string()),
                    _ => {}
                }
            }
        }
    }
}

/// Drain pending frames without blocking. Used between polls to keep the
/// socket flowing — CDP sends Runtime.consoleAPICalled and other noise we
/// don't care about, but we still need to consume it so the buffer doesn't
/// back up.
async fn drain_pending(ws: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) {
    loop {
        // 0ms timeout = poll once and bail.
        let msg = tokio::time::timeout(Duration::from_millis(0), ws.next()).await;
        match msg {
            Ok(Some(Ok(Message::Ping(p)))) => {
                let _ = ws.send(Message::Pong(p)).await;
            }
            Ok(Some(Ok(_))) => {
                // Some other frame — drop it.
            }
            Ok(Some(Err(_))) | Ok(None) => return, // dead socket
            Err(_) => return,                       // timeout — nothing pending
        }
    }
}
