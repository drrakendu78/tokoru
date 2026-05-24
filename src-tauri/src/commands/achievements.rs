//! Achievements commands — fetch from Steam's IPlayerStats endpoint and
//! cache in the games table. Stale window of 24h keeps repeat reads on
//! the GameDetail panel cheap without ever showing the user data from
//! days ago.

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::commands::steam::{KEY_STEAM_ID, KEY_STEAM_TOKEN};
use crate::services::db::Database;
use crate::services::{gog_achievements, gog_api, steam_achievements};
use crate::services::steam_achievements::AchievementItem;

/// How long we trust a cached achievement list before refetching. 24h is
/// fine — achievements unlock at human speed, the user opening the
/// GameDetail panel doesn't need second-level freshness.
const ACHIEVEMENTS_STALE_SECONDS: i64 = 24 * 3600;

#[derive(Debug, Clone, Serialize)]
pub struct GlobalAchievementsStats {
    pub total_unlocked: u32,
    pub total_available: u32,
    /// Number of games with at least one cached achievement row (i.e. that
    /// contribute to the totals above). Used for the "across X games"
    /// subtitle on the Stats page.
    pub games_with_achievements: u32,
    /// Up to N most recently unlocked achievements across the whole
    /// library, newest first. Each entry knows which game it came from
    /// so the frontend can render `<game> · <achievement>`.
    pub recent_unlocks: Vec<RecentUnlock>,
    /// Top N games by completion %, only counting games with at least
    /// one unlock so the list isn't dominated by zero-progress titles.
    pub top_completion: Vec<GameCompletion>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecentUnlock {
    pub game_id: String,
    pub game_title: String,
    pub name: String,
    pub icon: Option<String>,
    pub unlocktime: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GameCompletion {
    pub game_id: String,
    pub game_title: String,
    pub artwork_url: Option<String>,
    pub unlocked: u32,
    pub total: u32,
    pub pct: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AchievementsView {
    /// Total number of achievements the game declares. `0` when the
    /// game has no stats — in that case the UI should render nothing.
    pub total: usize,
    /// How many of those the player has unlocked.
    pub unlocked: usize,
    /// The full list, ordered as Steam returned them. Frontend slices
    /// the first N for the preview grid.
    pub items: Vec<AchievementItem>,
    /// When this cache was last refreshed (unix seconds). None when
    /// the game has never been synced.
    pub synced_at: Option<i64>,
}

/// Read the cached achievements for a game without hitting Steam.
/// Returns an empty view (`total=0`) when never synced — the frontend
/// uses that as the "loading" signal and follows up with
/// `sync_game_achievements`.
#[tauri::command]
pub async fn get_game_achievements(
    id: String,
    db: State<'_, Database>,
) -> Result<AchievementsView, String> {
    let (raw, synced_at) = db
        .get_game_achievements_raw(&id)
        .map_err(|e| e.to_string())?;
    let items: Vec<AchievementItem> = raw
        .as_deref()
        .and_then(|v| serde_json::from_str(v).ok())
        .unwrap_or_default();
    let unlocked = items.iter().filter(|a| a.achieved).count();
    Ok(AchievementsView {
        total: items.len(),
        unlocked,
        items,
        synced_at,
    })
}

/// Refresh achievements for one Steam game from the API and store the
/// result. Skips the API call when the cached row is younger than the
/// stale window unless `force = true` is passed. Returns the resolved
/// view either way so the caller always gets fresh-looking data on a
/// single round-trip.
#[tauri::command]
pub async fn sync_game_achievements(
    id: String,
    force: Option<bool>,
    app_handle: AppHandle,
    db: State<'_, Database>,
) -> Result<AchievementsView, String> {
    let force = force.unwrap_or(false);
    log::info!("[achievements] sync_game_achievements called id={} force={}", id, force);
    let game = db
        .get_game_by_id(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Game not found".to_string())?;
    log::info!(
        "[achievements] game source={} platform_id={:?}",
        game.source,
        game.platform_id
    );

    // Achievements are sourced per-platform. Add a new branch + a service
    // module under `services/` when extending to Epic, Itch, etc.
    let supported = matches!(game.source.as_str(), "steam" | "gog");
    if !supported {
        return get_game_achievements(id, db).await;
    }
    let Some(platform_id) = game.platform_id.clone().filter(|p| !p.is_empty()) else {
        return get_game_achievements(id, db).await;
    };

    // Honor the stale window unless force is set. Shared across sources so
    // we don't punish GOG just because the user reopened a card.
    if !force {
        let (_, synced_at) = db
            .get_game_achievements_raw(&id)
            .map_err(|e| e.to_string())?;
        let now = chrono::Utc::now().timestamp();
        if let Some(t) = synced_at {
            if now - t < ACHIEVEMENTS_STALE_SECONDS {
                return get_game_achievements(id, db).await;
            }
        }
    }

    let items: Vec<AchievementItem> = match game.source.as_str() {
        "steam" => {
            let steam_id = db
                .get_sync_state(KEY_STEAM_ID)
                .map_err(|e| e.to_string())?
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "Steam not connected.".to_string())?;
            let jwt = db
                .get_sync_state(KEY_STEAM_TOKEN)
                .map_err(|e| e.to_string())?
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "Steam session missing — reconnect Steam in Settings.".to_string())?;
            steam_achievements::fetch_player_achievements(&steam_id, &jwt, &platform_id)
                .await
                .map_err(|e| e.to_string())?
        }
        "gog" => {
            // Heroic-style fetch: pass the GOG product id straight to the
            // gameplay endpoint, no Galaxy DB lookup. We do however need
            // a live access_token + the GOG `galaxyUserId` (NOT the
            // wallet/userId — gameplay.gog.com 403s on that).
            let access_token = gog_api::refreshed_access_token(&app_handle)
                .await
                .map_err(|e| format!("GOG not connected: {}", e))?;
            let user_id = gog_api::fetch_user_id(&access_token)
                .await
                .map_err(|e| format!("GOG userData lookup: {}", e))?;
            // Pick the GOG locale tag (`en-US`, `fr-FR`, …) from the user's
            // UI locale so the achievement names + descriptions come back
            // localised, matching what the GameDetail panel renders.
            let app_locale = crate::commands::locale::read_app_locale(&db);
            let locale = crate::commands::locale::gog_locale_tag(&app_locale);
            gog_achievements::fetch_player_achievements(
                &access_token,
                &user_id,
                &platform_id,
                locale,
            )
            .await
            .map_err(|e| e.to_string())?
        }
        _ => Vec::new(),
    };

    let json = serde_json::to_string(&items)
        .map_err(|e| format!("serialize achievements: {}", e))?;
    db.set_game_achievements(&id, &json)
        .map_err(|e| e.to_string())?;

    get_game_achievements(id, db).await
}

/// Aggregate every cached achievement row into a single global view —
/// powers the "Achievements" block on the Stats page. Pure DB read; no
/// network. Games without a cached row contribute nothing (no implicit
/// 0/N to avoid showing thousands of zero-unlocked games dragging the
/// global % to zero).
#[tauri::command]
pub async fn get_global_achievements_stats(
    db: State<'_, Database>,
) -> Result<GlobalAchievementsStats, String> {
    let games = db.get_all_games().map_err(|e| e.to_string())?;

    let mut total_unlocked: u32 = 0;
    let mut total_available: u32 = 0;
    let mut games_with_achievements: u32 = 0;
    let mut all_unlocks: Vec<RecentUnlock> = Vec::new();
    let mut per_game_completion: Vec<GameCompletion> = Vec::new();

    for g in &games {
        let Some(id) = g.id.as_ref() else { continue };
        let (raw, _synced_at) = db
            .get_game_achievements_raw(id)
            .map_err(|e| e.to_string())?;
        let Some(json) = raw else { continue };
        let items: Vec<AchievementItem> = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if items.is_empty() {
            continue;
        }
        games_with_achievements += 1;
        let total = items.len() as u32;
        let unlocked = items.iter().filter(|a| a.achieved).count() as u32;
        total_available += total;
        total_unlocked += unlocked;

        for it in &items {
            if it.achieved && it.unlocktime > 0 {
                all_unlocks.push(RecentUnlock {
                    game_id: id.clone(),
                    game_title: g.title.clone(),
                    name: if it.name.is_empty() {
                        it.apiname.clone()
                    } else {
                        it.name.clone()
                    },
                    icon: if it.icon.is_empty() { None } else { Some(it.icon.clone()) },
                    unlocktime: it.unlocktime,
                });
            }
        }

        if unlocked > 0 {
            let pct = (unlocked * 100) / total.max(1);
            per_game_completion.push(GameCompletion {
                game_id: id.clone(),
                game_title: g.title.clone(),
                artwork_url: g.artwork_url.clone(),
                unlocked,
                total,
                pct,
            });
        }
    }

    // Newest unlocks first — return everything so the Stats page can
    // scroll through the entire history. The block is horizontal-scroll
    // so even thousands of icons stay performant (only the visible
    // tiles render meaningful work, the rest are just <img loading=lazy>).
    all_unlocks.sort_by(|a, b| b.unlocktime.cmp(&a.unlocktime));

    // Highest completion first — return all games with achievements so
    // the Stats grid covers the full library, not just a top-N teaser.
    per_game_completion.sort_by(|a, b| b.pct.cmp(&a.pct));

    Ok(GlobalAchievementsStats {
        total_unlocked,
        total_available,
        games_with_achievements,
        recent_unlocks: all_unlocks,
        top_completion: per_game_completion,
    })
}
