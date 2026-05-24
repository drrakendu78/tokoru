//! Epic Games integration. Auth uses the legendary CLI as a black-box token
//! handler (browser → auth code → `legendary auth --code <code>`), then we
//! query the Epic Launcher Assets + Catalog APIs directly with the token
//! Epic returned so we can build a clean owned-games list — exactly the same
//! shape `Game::owned()` expects.
//!
//! We deliberately use the public Epic Launcher client credentials (same ones
//! the official EGL and legendary use). No Tokoru-specific OAuth app.
//!
//! Why call legendary at all if we have the token? Because legendary persists
//! the refresh token to `~/.config/legendary/user.json` for us, and that's
//! also where the user can verify (or `legendary auth --delete`) their
//! session. We get the operational benefits of the upstream config layout
//! without rolling our own token store.

use std::collections::HashMap;
use std::process::Command;

use reqwest::Client;
use serde::Deserialize;
use tauri::AppHandle;

use crate::models::Game;
use crate::services::cli_bins;

// ── Epic Launcher OAuth credentials ──
// These are the **public** Launcher client identifiers shipped by Epic in the
// Epic Games Launcher itself, and reused by the legendary open-source CLI
// (https://github.com/derrod/legendary). They're not secrets — embedding them
// is required for the OAuth flow to identify Tokoru as a launcher-class
// client. Replacing them with our own client ID would require Epic to grant
// us launcher-tier OAuth scopes, which they don't issue to third parties.
pub const EPIC_CLIENT_ID: &str = "34a02cf8f4414e29b15921876da36f9a";
const EPIC_CLIENT_SECRET: &str = "daafbccc737745039dffe53d94fc76cf";

const EPIC_TOKEN_URL: &str =
    "https://account-public-service-prod03.ol.epicgames.com/account/api/oauth/token";
const EPIC_ACCOUNT_URL: &str =
    "https://account-public-service-prod03.ol.epicgames.com/account/api/public/account";
const EPIC_ASSETS_URL: &str =
    "https://launcher-public-service-prod06.ol.epicgames.com/launcher/api/public/assets/Windows";
const EPIC_CATALOG_URL: &str =
    "https://catalog-public-service-prod06.ol.epicgames.com/catalog/api/shared/namespace";

// ── Public types ──

#[derive(Debug, Clone)]
pub struct EpicAccount {
    /// Kept for parity with OrionCore — needed when we eventually surface the
    /// account in the Library filter chips. Not consumed in the Tokoru
    /// command layer yet.
    #[allow(dead_code)]
    pub account_id: String,
    pub display_name: String,
    pub access_token: String,
}

// ── Auth ──

/// Exchange the authorisation code captured from the browser flow for a real
/// access token, then hand the token+refresh pair to legendary so it persists
/// the session in its own config (used by `auth_status` / `auth_logout`).
pub async fn login_with_code(
    app_handle: &AppHandle,
    auth_code: &str,
) -> Result<EpicAccount, String> {
    // 1. Make sure legendary is available before we burn a (one-shot) auth code.
    let legendary_exe = cli_bins::ensure_legendary(app_handle).await?;

    // 2. Exchange the code ourselves so we have the token in-process for the
    //    library fetch right after. This also gives us a clean error if the
    //    code is invalid before we shell out.
    let token = exchange_code(auth_code).await?;

    // 3. Look up the display name.
    let display_name = match get_account_name(&token.access_token, &token.account_id).await {
        Ok(name) => name,
        Err(e) => {
            log::warn!("[epic_api] account name lookup failed: {}", e);
            "Epic User".to_string()
        }
    };

    // 4. Tell legendary about the freshly-issued code so it persists tokens
    //    where the rest of the CLI subcommands look for them. Run blocking
    //    on the thread pool — `Command::output` is sync.
    let auth_code_owned = auth_code.to_string();
    tokio::task::spawn_blocking(move || {
        let output = Command::new(&legendary_exe)
            .args(["auth", "--code", &auth_code_owned])
            .output()
            .map_err(|e| format!("Failed to spawn legendary: {}", e))?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.is_empty() {
            log::info!("[legendary auth] {}", stderr.trim());
        }

        // legendary writes its "Stored credentials are valid" message to stderr
        // and returns 0 even when it had to read the code from stdin — so we
        // don't hard-fail on non-zero exit if the credentials file got written.
        if !output.status.success() {
            log::warn!(
                "[legendary auth] non-zero exit ({}); proceeding (token already validated)",
                output.status
            );
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("legendary auth task panicked: {}", e))??;

    Ok(EpicAccount {
        account_id: token.account_id,
        display_name,
        access_token: token.access_token,
    })
}

/// True if legendary reports a valid session. Best-effort — failures here only
/// mean "not connected", never propagate to the UI as errors.
#[allow(dead_code)]
pub async fn is_logged_in(app_handle: &AppHandle) -> bool {
    let Ok(exe) = cli_bins::legendary_path(app_handle) else {
        return false;
    };
    if !exe.exists() {
        return false;
    }
    tokio::task::spawn_blocking(move || {
        let Ok(out) = Command::new(&exe).args(["status"]).output() else {
            return false;
        };
        if !out.status.success() {
            return false;
        }
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        !combined.to_lowercase().contains("not logged in")
    })
    .await
    .unwrap_or(false)
}

/// Wipe the legendary session.
pub async fn logout(app_handle: &AppHandle) -> Result<(), String> {
    let exe = cli_bins::legendary_path(app_handle)?;
    if !exe.exists() {
        return Ok(()); // nothing to clear
    }
    tokio::task::spawn_blocking(move || {
        let output = Command::new(&exe)
            .args(["auth", "--delete"])
            .output()
            .map_err(|e| format!("Failed to spawn legendary: {}", e))?;
        if !output.status.success() {
            log::warn!(
                "[legendary auth --delete] non-zero exit ({})",
                output.status
            );
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("legendary logout task panicked: {}", e))?
}

// ── Library fetch ──

/// Fetch all installable owned games using a fresh access token (we always
/// pass the token we got from `login_with_code`; sync uses the cached one
/// from sync_state — see commands/epic.rs).
///
/// Uses the same Launcher Assets + Catalog API combo that legendary itself
/// uses, so we get clean "playable" entries and skip DLC / Unreal Marketplace
/// items.
pub async fn fetch_owned_games(access_token: &str) -> Result<Vec<Game>, String> {
    let started = std::time::Instant::now();
    let client = Client::new();

    let resp = client
        .get(format!("{}?label=Live", EPIC_ASSETS_URL))
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("Epic assets fetch failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        if status.as_u16() == 401 {
            return Err("Epic token expired — please reconnect Epic in Settings.".to_string());
        }
        return Err(format!("Epic assets error: HTTP {}", status));
    }

    let assets: Vec<EpicAssetItem> = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse Epic assets: {}", e))?;

    // Group catalog ids per namespace, skipping the `ue` (Unreal) namespace.
    let mut by_namespace: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for asset in &assets {
        if asset.namespace == "ue" {
            continue;
        }
        by_namespace
            .entry(asset.namespace.clone())
            .or_default()
            .push((asset.app_name.clone(), asset.catalog_item_id.clone()));
    }

    let mut all_games = Vec::new();

    // Build the full list of (namespace, chunk) pairs across ALL namespaces.
    // Epic's library is split into many tiny namespaces (one per publisher
    // bundle, free-game promo, etc.) — looping namespaces sequentially was
    // the real bottleneck: 50+ namespaces × ~1s/req = 50s+ even when each
    // namespace only had a few items. Flatten everything and parallelize.
    use futures_util::stream::{self, StreamExt};
    const BULK_CHUNK_SIZE: usize = 50;
    const BULK_PARALLELISM: usize = 12;

    let mut all_chunks: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for (namespace, items) in &by_namespace {
        for chunk in items.chunks(BULK_CHUNK_SIZE) {
            all_chunks.push((namespace.clone(), chunk.to_vec()));
        }
    }
    log::info!(
        "[epic_api] fetching {} catalog chunks across {} namespaces (parallelism={})",
        all_chunks.len(),
        by_namespace.len(),
        BULK_PARALLELISM
    );
    let fetch_started = std::time::Instant::now();

    let chunk_results: Vec<HashMap<String, CatalogEntry>> = stream::iter(all_chunks)
        .map(|(namespace, chunk)| {
            let client = client.clone();
            let token = access_token.to_string();
            async move {
                let id_params: String = chunk
                    .iter()
                    .map(|(_, cid)| format!("id={}", cid))
                    .collect::<Vec<_>>()
                    .join("&");
                let catalog_url = format!(
                    "{}/{}/bulk/items?{}&includeMainGameDetails=true&country=US&locale=en-US",
                    EPIC_CATALOG_URL, namespace, id_params
                );
                match client.get(&catalog_url).bearer_auth(&token).send().await {
                    Ok(r) if r.status().is_success() => {
                        r.json::<HashMap<String, CatalogEntry>>().await.unwrap_or_default()
                    }
                    Ok(r) => {
                        log::warn!(
                            "[epic_api] catalog batch HTTP {} for namespace {} (size {})",
                            r.status(),
                            namespace,
                            chunk.len()
                        );
                        HashMap::new()
                    }
                    Err(e) => {
                        log::warn!(
                            "[epic_api] catalog batch failed for namespace {}: {}",
                            namespace,
                            e
                        );
                        HashMap::new()
                    }
                }
            }
        })
        .buffer_unordered(BULK_PARALLELISM)
        .collect()
        .await;

    log::info!(
        "[epic_api] catalog fetch done in {}ms",
        fetch_started.elapsed().as_millis()
    );

    // Merge all chunks across all namespaces into one lookup map.
    // catalog_item_id is globally unique so there are no collisions.
    let mut catalog_entries: HashMap<String, CatalogEntry> = HashMap::new();
    for chunk in chunk_results {
        catalog_entries.extend(chunk);
    }

    for (_namespace, items) in &by_namespace {
        for (app_name, catalog_id) in items {
            let entry = catalog_entries.get(catalog_id);

            // Skip DLC entries.
            if let Some(e) = entry {
                if e.main_game_item.is_some() {
                    continue;
                }
            }

            let title = entry
                .and_then(|e| e.title.as_ref())
                .cloned()
                .unwrap_or_else(|| app_name.replace('-', " ").replace('_', " "));

            let launch_cmd = format!(
                "com.epicgames.launcher://apps/{}?action=launch&silent=true",
                app_name
            );

            all_games.push(Game::owned(
                title,
                "epic".to_string(),
                app_name.clone(),
                launch_cmd,
            ));
        }
    }

    log::info!(
        "[epic_api] fetched {} owned games in {}ms total",
        all_games.len(),
        started.elapsed().as_millis()
    );
    Ok(all_games)
}

// ── Internal helpers ──

#[derive(Debug)]
struct EpicToken {
    access_token: String,
    account_id: String,
}

async fn exchange_code(auth_code: &str) -> Result<EpicToken, String> {
    #[derive(Deserialize)]
    struct EpicTokenResponse {
        access_token: String,
        account_id: Option<String>,
    }

    let client = Client::new();
    let resp = client
        .post(EPIC_TOKEN_URL)
        .basic_auth(EPIC_CLIENT_ID, Some(EPIC_CLIENT_SECRET))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", auth_code),
            ("token_type", "eg1"),
        ])
        .send()
        .await
        .map_err(|e| format!("Epic token exchange failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Epic token error {}: {}", status, body));
    }

    let token: EpicTokenResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse Epic token: {}", e))?;

    Ok(EpicToken {
        access_token: token.access_token,
        account_id: token.account_id.unwrap_or_default(),
    })
}

async fn get_account_name(access_token: &str, account_id: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct AccountInfo {
        #[serde(rename = "displayName")]
        display_name: Option<String>,
    }

    if account_id.is_empty() {
        return Ok("Epic User".to_string());
    }

    let client = Client::new();
    let url = format!("{}/{}", EPIC_ACCOUNT_URL, account_id);

    let resp: AccountInfo = client
        .get(&url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("Epic account info request failed: {}", e))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse Epic account info: {}", e))?;

    Ok(resp.display_name.unwrap_or_else(|| "Epic User".to_string()))
}

#[derive(Deserialize, Debug)]
struct EpicAssetItem {
    #[serde(rename = "appName")]
    app_name: String,
    #[serde(rename = "catalogItemId")]
    catalog_item_id: String,
    namespace: String,
}

#[derive(Deserialize, Debug)]
struct CatalogEntry {
    title: Option<String>,
    #[serde(rename = "mainGameItem")]
    main_game_item: Option<serde_json::Value>,
}

/// Browser URL the user goes to in order to log in. Their token / authorization
/// code is then captured by our WebView's `on_navigation` interceptor.
pub fn get_login_url() -> String {
    format!(
        "https://www.epicgames.com/id/login?redirectUrl=https%3A%2F%2Fwww.epicgames.com%2Fid%2Fapi%2Fredirect%3FclientId%3D{}%26responseType%3Dcode",
        EPIC_CLIENT_ID
    )
}
