use crate::services::matcher;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const STEAMGRID_API_URL: &str = "https://www.steamgriddb.com/api/v2";
const STEAMGRID_PAGE_SIZE: usize = 50;
const STEAMGRID_MAX_PAGES: usize = 30;

/// User-facing artwork-style preference mapped to SteamGridDB's `styles=`
/// query param values. Returns `None` when no filter should be applied (the
/// `"any"` preference and the catch-all path for unknown inputs both fall
/// here, which causes SteamGridDB to return all available styles).
///
/// These mappings are best-effort: SteamGridDB's enum is fixed
/// (`official|alternate|white|material|monochrome|blueprint|no_logo|padded`
/// etc.) so we pick the closest match for each user-facing label. If a
/// mapping yields no results, the call site naturally falls back to "no
/// filter" by passing `None` from the caller's helper.
///
/// Covers (`/grids`) and heroes (`/heroes`) share the same allowed styles.
/// Logos (`/logos`) use a different set — see `logo_style_param` below.
fn cover_style_param(style: Option<&str>) -> Option<&'static str> {
    match style? {
        "official" => Some("official"),
        // "Painted" is not a native SGDB style. `alternate` is the typical
        // fan-art / hand-painted bucket, and `blurred` rounds it out with the
        // soft, painterly cover variants users associate with the label.
        "painted" => Some("alternate,blurred"),
        // "Photoreal" maps to "no_logo" — clean original artwork without
        // overlayed title text.
        "photoreal" => Some("no_logo"),
        // "Minimal" — flat / clean styles.
        "minimal" => Some("white,material"),
        _ => None,
    }
}

fn logo_style_param(style: Option<&str>) -> Option<&'static str> {
    match style? {
        "official" => Some("official"),
        "painted" => Some("monochrome,padded"),
        // Photoreal doesn't really make sense for logos — fall back to no
        // filter and let SGDB return what it has.
        "photoreal" => None,
        "minimal" => Some("white,material"),
        _ => None,
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    success: bool,
    data: Vec<SearchResult>,
}

#[derive(Deserialize, Clone)]
struct SearchResult {
    id: i64,
    name: String,
    /// SGDB tags each game with the platforms where it's listed
    /// (`["steam"]`, `["egs", "gog"]`, …). Empty `types` typically marks
    /// legacy console-only entries (PS2/PSP releases) that share a name
    /// with a modern PC port — and those legacy entries don't have any
    /// PC-era animated grids. We use this to break search ties.
    #[serde(default)]
    types: Vec<String>,
}

#[derive(Deserialize)]
struct GridResponse {
    success: bool,
    data: Vec<GridResult>,
}

#[derive(Deserialize, Clone)]
struct GridResult {
    url: String,
    thumb: Option<String>,
    mime: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    nsfw: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverOption {
    pub url: String,
    pub thumb: String,
    pub is_animated: bool,
    pub nsfw: bool,
    pub width: u32,
    pub height: u32,
}

pub struct SteamGridClient {
    api_key: String,
    client: reqwest::Client,
    /// Auto-pick prefs (see `with_prefs`). Drive ordering / filtering in
    /// `get_cover` and `get_hero`. Default `(false, false)` — backward-
    /// compatible with existing call sites that just call `new(api_key)`.
    prefer_animated: bool,
    allow_nsfw: bool,
}


impl SteamGridClient {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::new(),
            prefer_animated: false,
            allow_nsfw: false,
        }
    }

    /// Builder-style wrapper that swaps in the user's auto-pick prefs.
    /// Currently honored by `get_cover` / `get_hero`. Logo picks ignore
    /// these (logos are typically static + safe by nature).
    pub fn with_prefs(mut self, prefer_animated: bool, allow_nsfw: bool) -> Self {
        self.prefer_animated = prefer_animated;
        self.allow_nsfw = allow_nsfw;
        self
    }

    /// Apply the user's NSFW + animated prefs to a freshly-fetched grid
    /// list before we pick `[0]`. Returns the items in the order the
    /// auto-pick should consider them. Never empties the slice when there
    /// were results — falls back to the original order when filtering
    /// strips everything (better SOMETHING than nothing).
    fn rank_results(&self, raw: Vec<GridResult>) -> Vec<GridResult> {
        if raw.is_empty() {
            return raw;
        }
        // NSFW filter first — only enforced when the user opted out.
        let safe: Vec<GridResult> = if self.allow_nsfw {
            raw.clone()
        } else {
            raw.iter()
                .filter(|g| !g.nsfw.unwrap_or(false))
                .cloned()
                .collect()
        };
        let pool = if safe.is_empty() { raw } else { safe };

        // Sort by animation priority so the format the user wants ends up at
        // index 0. `animated_priority` is `.webm`=0, `.gif`=1, `.apng`=2,
        // static=3 — so ascending = animated first (when prefer_animated=ON),
        // descending = static first (when OFF). Stable sort keeps
        // SteamGridDB's popularity order within each tier.
        let mut sorted = pool;
        if self.prefer_animated {
            sorted.sort_by_key(|g| Self::animated_priority(&g.url, g.mime.as_deref()));
        } else {
            // OFF = downgrade path: deprioritize any animated candidate so
            // the static one ends up at [0]. Without this, SGDB's default
            // popularity order can land an animated grid at the top and
            // the auto-pick keeps shipping animations under "prefer
            // static".
            sorted.sort_by_key(|g| {
                u8::MAX - Self::animated_priority(&g.url, g.mime.as_deref())
            });
        }
        sorted
    }

    fn is_animated_url(url: &str, mime: Option<&str>) -> bool {
        let lower_url = url.to_ascii_lowercase();
        if lower_url.ends_with(".webm")
            || lower_url.ends_with(".gif")
            || lower_url.ends_with(".apng")
        {
            return true;
        }
        matches!(
            mime,
            Some(m) if m.contains("webm") || m.contains("gif") || m.contains("apng")
        )
    }

    /// Numeric priority for sorting animated candidates inside the
    /// already-filtered list. Lower = picked first. Real video formats
    /// (.webm) > GIF > APNG > static. SteamGridDB's `types=animated`
    /// filter returns a mix of all 3 animated kinds.
    fn animated_priority(url: &str, mime: Option<&str>) -> u8 {
        let lower_url = url.to_ascii_lowercase();
        if lower_url.ends_with(".webm") || matches!(mime, Some(m) if m.contains("webm")) {
            return 0;
        }
        if lower_url.ends_with(".gif") || matches!(mime, Some(m) if m.contains("gif")) {
            return 1;
        }
        if lower_url.ends_with(".apng") || matches!(mime, Some(m) if m.contains("apng")) {
            return 2;
        }
        3
    }

    /// Search for a game and return the best matching game ID
    /// Resolve the SteamGridDB game id from a known platform identifier.
    /// Way more accurate than `search_game` for titles that have multiple
    /// SGDB entries (PC release vs PS classic vs remake) — without this,
    /// "God of War" resolves to the PS2 entry (no animated grids) instead
    /// of the 2018 PC port (4 animated grids). Supported `source` values
    /// match SteamGridDB's `/games/{source}/{id}` route: `steam`, `epic`,
    /// `gog`, `origin`, `uplay`, `flashpoint`.
    pub async fn resolve_by_platform(
        &self,
        source: &str,
        platform_id: &str,
    ) -> Option<i64> {
        // SteamGridDB uses `egs` for Epic Games Store, not `epic`.
        // Other sources keep their canonical name. Anything we don't
        // recognize → bail out and let the caller fall back to search.
        let sgdb_source = match source {
            "steam" => "steam",
            "epic" => "egs",
            "gog" => "gog",
            "ea" => "origin",
            "ubi" => "uplay",
            "itch" => "flashpoint",
            _ => return None,
        };
        let url = format!(
            "{}/games/{}/{}",
            STEAMGRID_API_URL, sgdb_source, platform_id
        );
        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .ok()?;
        #[derive(serde::Deserialize)]
        struct LookupResponse {
            success: bool,
            data: Option<SearchResult>,
        }
        let body: LookupResponse = response.json().await.ok()?;
        if body.success {
            body.data.map(|d| d.id)
        } else {
            None
        }
    }

    pub async fn search_game(&self, name: &str) -> Option<i64> {
        let query = matcher::build_search_query(name);
        let url = format!("{}/search/autocomplete/{}", STEAMGRID_API_URL, query);

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .ok()?;

        let search: SearchResponse = response.json().await.ok()?;

        if !search.success || search.data.is_empty() {
            return None;
        }

        // Score each result on TWO axes:
        //  1. Has PC platform listings? (`types` contains steam/egs/gog/
        //     origin/uplay/flashpoint) — these have artwork uploaded by
        //     the modern PC community, INCLUDING animated grids.
        //  2. Name similarity to the query.
        //
        // We prefer "PC-listed + slightly lower similarity" over
        // "no-types + perfect match". This fixes the "God of War" case
        // where the search returns the PS2 entry first (perfect string
        // match but 0 animated) and the Steam PC port second (4 animated
        // grids available).
        let normalized_query = matcher::normalize_name(name);
        let pc_sources = ["steam", "egs", "gog", "origin", "uplay", "flashpoint"];
        let has_pc_listing = |r: &SearchResult| {
            r.types.iter().any(|t| pc_sources.contains(&t.as_str()))
        };

        let mut best_match: Option<(i64, f64, bool)> = None;
        for result in &search.data {
            let normalized_result = matcher::normalize_name(&result.name);
            let sim = matcher::similarity(&normalized_query, &normalized_result);
            let pc = has_pc_listing(result);
            let candidate = (result.id, sim, pc);
            best_match = match best_match {
                None => Some(candidate),
                Some((_, best_sim, best_pc)) => {
                    // PC listing wins over no-listing (unless the
                    // no-listing entry is dramatically more similar).
                    let take = match (pc, best_pc) {
                        (true, false) if sim + 0.15 >= best_sim => true,
                        (false, true) if sim > best_sim + 0.15 => true,
                        _ => sim > best_sim,
                    };
                    if take { Some(candidate) } else { best_match }
                }
            };
        }

        if let Some((id, sim, pc)) = &best_match {
            log::info!(
                "[steamgrid] search '{}' → sgdb_id={} sim={:.2} pc_listed={}",
                name, id, sim, pc
            );
        }
        best_match.map(|(id, _, _)| id)
    }

    /// Get a cover/grid image for a game. The optional `style` is the user's
    /// preferred-artwork-style string from sync_state (`"official"|"painted"|
    /// "photoreal"|"minimal"|"any"`); when it maps to a real SteamGridDB
    /// filter, we pass it as `?styles=<value>`. When the style yields no
    /// matches, we automatically retry without the filter so the user still
    /// gets *something* rather than an empty cover.
    pub async fn get_cover(&self, game_id: i64, style: Option<&str>) -> Option<String> {
        let style_param = cover_style_param(style);
        let url = format!("{}/grids/game/{}", STEAMGRID_API_URL, game_id);
        let nsfw_param = if self.allow_nsfw { "any" } else { "false" };

        // ── Pass 1 (animated-only) ──
        // When the user opted into animated, hit the API for animated grids
        // first. The default `dimensions=600x900` filter often hides
        // animated variants (most animated grids are uploaded at other
        // resolutions), so we DROP dimensions on this pass too.
        if self.prefer_animated {
            // Pass 1a: respecting the user's style preference.
            let mut req = self
                .client
                .get(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .query(&[("types", "animated"), ("nsfw", nsfw_param)]);
            if let Some(s) = style_param {
                req = req.query(&[("styles", s)]);
            }
            let pass1a = req.send().await.ok();
            if let Some(response) = pass1a {
                if let Ok(grid) = response.json::<GridResponse>().await {
                    if grid.success && !grid.data.is_empty() {
                        let count = grid.data.len();
                        let ranked = self.rank_results(grid.data);
                        if let Some(first) = ranked.first() {
                            log::info!(
                                "[steamgrid] animated PICKED game_id={} ({} candidates) mime={:?} url={}",
                                game_id, count, first.mime, first.url
                            );
                            return Some(first.url.clone());
                        }
                    } else {
                        log::info!(
                            "[steamgrid] animated EMPTY game_id={} (pass 1a with styles={:?})",
                            game_id, style_param
                        );
                    }
                }
            }

            // Pass 1b: retry animated WITHOUT the style filter — many
            // animated grids aren't tagged with an "official" style, so
            // the previous query may have stripped them all.
            if style_param.is_some() {
                let req = self
                    .client
                    .get(&url)
                    .header("Authorization", format!("Bearer {}", self.api_key))
                    .query(&[("types", "animated"), ("nsfw", nsfw_param)]);
                if let Ok(response) = req.send().await {
                    if let Ok(grid) = response.json::<GridResponse>().await {
                        if grid.success && !grid.data.is_empty() {
                            let count = grid.data.len();
                            let ranked = self.rank_results(grid.data);
                            if let Some(first) = ranked.first() {
                                log::info!(
                                    "[steamgrid] animated PICKED game_id={} ({} candidates, no-style retry) mime={:?} url={}",
                                    game_id, count, first.mime, first.url
                                );
                                return Some(first.url.clone());
                            }
                        }
                    }
                }
            }
            log::info!(
                "[steamgrid] animated NONE-AVAILABLE game_id={} → falling back to static",
                game_id
            );
        }

        // ── Pass 2 (600x900 static + animated mix) ──
        let mut req = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .query(&[
                ("dimensions", "600x900"),
                ("types", "static,animated"),
                ("nsfw", nsfw_param),
            ]);
        if let Some(s) = style_param {
            req = req.query(&[("styles", s)]);
        }

        let response = req.send().await.ok()?;
        let grid: GridResponse = response.json().await.ok()?;

        if grid.success && !grid.data.is_empty() {
            let ranked = self.rank_results(grid.data);
            if let Some(first) = ranked.first() {
                return Some(first.url.clone());
            }
        }

        // ── Pass 3 (last-resort unfiltered) ──
        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .query(&[("types", "static,animated"), ("nsfw", nsfw_param)])
            .send()
            .await
            .ok()?;
        let grid: GridResponse = response.json().await.ok()?;
        if grid.success && !grid.data.is_empty() {
            let ranked = self.rank_results(grid.data);
            if let Some(first) = ranked.first() {
                return Some(first.url.clone());
            }
        }

        None
    }

    /// Get a hero/banner image for a game.
    pub async fn get_hero(&self, game_id: i64, style: Option<&str>) -> Option<String> {
        let style_param = cover_style_param(style);
        let url = format!("{}/heroes/game/{}", STEAMGRID_API_URL, game_id);

        // Same reasoning as get_cover — opt into animated explicitly,
        // honor NSFW pref.
        let nsfw_param = if self.allow_nsfw { "any" } else { "false" };

        let mut req = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .query(&[("types", "static,animated"), ("nsfw", nsfw_param)]);
        if let Some(s) = style_param {
            req = req.query(&[("styles", s)]);
        }

        let response = req.send().await.ok()?;

        let grid: GridResponse = response.json().await.ok()?;
        if grid.success && !grid.data.is_empty() {
            let ranked = self.rank_results(grid.data);
            if let Some(first) = ranked.first() {
                return Some(first.url.clone());
            }
        }

        // Style-filter fallback — if the user's preference returned nothing,
        // try once more with no style filter (but keep types + nsfw).
        if style_param.is_some() {
            let response = self
                .client
                .get(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .query(&[("types", "static,animated"), ("nsfw", nsfw_param)])
                .send()
                .await
                .ok()?;
            let grid: GridResponse = response.json().await.ok()?;
            if grid.success && !grid.data.is_empty() {
                let ranked = self.rank_results(grid.data);
                if let Some(first) = ranked.first() {
                    return Some(first.url.clone());
                }
            }
        }
        None
    }

    /// Get a logo image for a game.
    pub async fn get_logo(&self, game_id: i64, style: Option<&str>) -> Option<String> {
        let style_param = logo_style_param(style);
        let url = format!("{}/logos/game/{}", STEAMGRID_API_URL, game_id);

        let mut req = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key));
        if let Some(s) = style_param {
            req = req.query(&[("styles", s)]);
        }

        let response = req.send().await.ok()?;

        let grid: GridResponse = response.json().await.ok()?;
        let mut data = if grid.success && !grid.data.is_empty() {
            grid.data
        } else if style_param.is_some() {
            // Fallback — no logos matched the user's style. Retry unfiltered.
            let response = self
                .client
                .get(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .send()
                .await
                .ok()?;
            let grid: GridResponse = response.json().await.ok()?;
            if grid.success && !grid.data.is_empty() {
                grid.data
            } else {
                return None;
            }
        } else {
            return None;
        };

        let mut options = self.map_cover_options(std::mem::take(&mut data), 1200, 400);
        Self::sort_logo_options(&mut options);

        if let Some(best) = options
            .iter()
            .find(|o| Self::is_single_line_logo(o.width, o.height) && !o.nsfw && !o.is_animated)
        {
            return Some(best.url.clone());
        }

        if let Some(best) = options
            .iter()
            .find(|o| Self::is_single_line_logo(o.width, o.height))
        {
            return Some(best.url.clone());
        }

        if let Some(best) = options.iter().find(|o| !o.nsfw) {
            return Some(best.url.clone());
        }

        if let Some(best) = options.first() {
            return Some(best.url.clone());
        }

        None
    }

    /// Fetch a SteamGridDB icon for the resolved game id. Returns the URL
    /// of the best candidate. Endpoint: `/icons/game/{id}`. Icons skip
    /// the style filter (SGDB's icon catalogue is small and unfiltered
    /// queries surface more usable matches).
    pub async fn get_icon(&self, game_id: i64) -> Option<String> {
        let url = format!("{}/icons/game/{}", STEAMGRID_API_URL, game_id);
        let nsfw_param = if self.allow_nsfw { "any" } else { "false" };
        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .query(&[("nsfw", nsfw_param)])
            .send()
            .await
            .ok()?;
        let grid: GridResponse = response.json().await.ok()?;
        if !grid.success || grid.data.is_empty() {
            return None;
        }
        // Take the first non-NSFW result that the SGDB popularity sort
        // surfaced. `rank_results` keeps the NSFW guard centralised.
        let ranked = self.rank_results(grid.data);
        ranked.first().map(|r| r.url.clone())
    }

    /// Browse all available icons for a game (`/icons/game/{id}`). Drives the
    /// "Icon" tab in GameDetail browse covers panel.
    pub async fn browse_icons(
        &self,
        game_name: &str,
        _style: Option<&str>,
    ) -> Result<Vec<CoverOption>, String> {
        let game_id = match self.search_game(game_name).await {
            Some(id) => id,
            None => return Ok(Vec::new()),
        };
        let url = format!("{}/icons/game/{}", STEAMGRID_API_URL, game_id);
        let nsfw_param = if self.allow_nsfw { "any" } else { "false" };
        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .query(&[("nsfw", nsfw_param)])
            .send()
            .await
            .map_err(|e| format!("icons fetch: {}", e))?;
        let grid: GridResponse = response
            .json()
            .await
            .map_err(|e| format!("icons parse: {}", e))?;
        if !grid.success {
            return Ok(Vec::new());
        }
        Ok(self.map_cover_options(grid.data, 256, 256))
    }

    /// Search and get cover in one call
    #[allow(dead_code)]
    pub async fn fetch_cover_for_game(&self, name: &str, style: Option<&str>) -> Option<String> {
        let game_id = self.search_game(name).await?;
        self.get_cover(game_id, style).await
    }

    /// Search once and get cover + hero + logo + icon together. `style` is the
    /// user's preferred-artwork-style string from sync_state; pass `None`
    /// (or "any") to skip the style filter. The icon slot ignores `style`
    /// (SGDB's icon catalogue is small enough that styled filtering wipes
    /// most results).
    pub async fn fetch_artwork_for_game(
        &self,
        name: &str,
        style: Option<&str>,
    ) -> (Option<String>, Option<String>, Option<String>, Option<String>) {
        self.fetch_artwork_for_game_resolved(name, None, None, style).await
    }

    /// Like `fetch_artwork_for_game` but prefers platform-keyed resolution
    /// when `(source, platform_id)` is supplied. Falls back to name search
    /// when no platform mapping is available, the source isn't supported by
    /// SGDB's platform routes, or the lookup misses.
    ///
    /// Returns `(cover, hero, logo, icon)`. Icon is the 32x32-ish bitmap
    /// that ends up in the friends list / taskbar / "now playing" header.
    pub async fn fetch_artwork_for_game_resolved(
        &self,
        name: &str,
        source: Option<&str>,
        platform_id: Option<&str>,
        style: Option<&str>,
    ) -> (Option<String>, Option<String>, Option<String>, Option<String>) {
        let game_id = if let (Some(src), Some(pid)) = (source, platform_id) {
            if !pid.is_empty() {
                if let Some(id) = self.resolve_by_platform(src, pid).await {
                    log::info!(
                        "[steamgrid] resolve_by_platform {}={} → sgdb_id={}",
                        src, pid, id
                    );
                    Some(id)
                } else {
                    self.search_game(name).await
                }
            } else {
                self.search_game(name).await
            }
        } else {
            self.search_game(name).await
        };

        let Some(game_id) = game_id else {
            return (None, None, None, None);
        };

        let cover = self.get_cover(game_id, style).await;
        let hero = self.get_hero(game_id, style).await;
        let logo = self.get_logo(game_id, style).await;
        let icon = self.get_icon(game_id).await;
        (cover, hero, logo, icon)
    }

    /// Search and get logo in one call
    #[allow(dead_code)]
    pub async fn fetch_logo_for_game(&self, name: &str, style: Option<&str>) -> Option<String> {
        let game_id = self.search_game(name).await?;
        self.get_logo(game_id, style).await
    }

    async fn fetch_all_images(
        &self,
        url: &str,
        style_param: Option<&str>,
    ) -> Result<Vec<GridResult>, String> {
        let mut all_results = Vec::new();

        for page in 0..STEAMGRID_MAX_PAGES {
            let page_str = page.to_string();
            let mut req = self
                .client
                .get(url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .query(&[
                    ("types", "static,animated"),
                    ("nsfw", "any"),
                    ("page", page_str.as_str()),
                ]);
            if let Some(s) = style_param {
                req = req.query(&[("styles", s)]);
            }
            let response = req
                .send()
                .await
                .map_err(|e| format!("SteamGridDB request failed: {}", e))?;

            let grid: GridResponse = response
                .json()
                .await
                .map_err(|e| format!("Failed to parse SteamGridDB response: {}", e))?;

            if !grid.success || grid.data.is_empty() {
                break;
            }

            let page_count = grid.data.len();
            all_results.extend(grid.data);

            if page_count < STEAMGRID_PAGE_SIZE {
                break;
            }
        }

        Ok(all_results)
    }

    fn map_cover_options(
        &self,
        items: Vec<GridResult>,
        default_width: u32,
        default_height: u32,
    ) -> Vec<CoverOption> {
        let mut seen_urls = HashSet::new();

        items
            .into_iter()
            .filter_map(|r| {
                let url = r.url;
                if !seen_urls.insert(url.clone()) {
                    return None;
                }

                let is_animated = Self::is_animated_asset(r.mime.as_deref(), &url);

                Some(CoverOption {
                    thumb: r.thumb.unwrap_or_else(|| url.clone()),
                    url,
                    is_animated,
                    nsfw: r.nsfw.unwrap_or(false),
                    width: r.width.unwrap_or(default_width),
                    height: r.height.unwrap_or(default_height),
                })
            })
            .collect()
    }

    fn is_animated_asset(mime: Option<&str>, url: &str) -> bool {
        let url_lower = url.to_ascii_lowercase();
        mime == Some("image/webp")
            || mime == Some("image/gif")
            || url_lower.ends_with(".webm")
            || url_lower.ends_with(".gif")
            || url_lower.ends_with(".apng")
    }

    fn is_single_line_logo(width: u32, height: u32) -> bool {
        let safe_height = height.max(1);
        let ratio = width as f32 / safe_height as f32;
        ratio >= 2.35 && safe_height <= 460
    }

    fn score_logo_candidate(item: &CoverOption) -> i32 {
        let safe_height = item.height.max(1);
        let ratio = item.width as f32 / safe_height as f32;
        let mut score = 0i32;

        if ratio >= 3.3 {
            score += 170;
        } else if ratio >= 2.8 {
            score += 130;
        } else if ratio >= 2.35 {
            score += 90;
        } else if ratio >= 2.0 {
            score += 35;
        } else {
            score -= 90;
        }

        if item.width >= 1200 {
            score += 25;
        } else if item.width >= 900 {
            score += 15;
        }

        if safe_height <= 320 {
            score += 30;
        } else if safe_height <= 420 {
            score += 15;
        } else if safe_height >= 700 {
            score -= 30;
        }

        if item.is_animated {
            score -= 20;
        }
        if item.nsfw {
            score -= 500;
        }

        score
    }

    fn sort_logo_options(options: &mut [CoverOption]) {
        options.sort_by(|a, b| {
            Self::score_logo_candidate(b)
                .cmp(&Self::score_logo_candidate(a))
                .then_with(|| b.width.cmp(&a.width))
                .then_with(|| a.height.cmp(&b.height))
        });
    }

    /// Browse all available grid covers (static + animated) for a game.
    /// The style filter is applied via `?styles=` when set; if it returns
    /// nothing we transparently retry without the filter.
    pub async fn browse_all_covers(
        &self,
        name: &str,
        style: Option<&str>,
    ) -> Result<Vec<CoverOption>, String> {
        let game_id = self
            .search_game(name)
            .await
            .ok_or_else(|| format!("Game '{}' not found on SteamGridDB", name))?;

        let url = format!("{}/grids/game/{}", STEAMGRID_API_URL, game_id);
        let style_param = cover_style_param(style);
        let mut all_images = self.fetch_all_images(&url, style_param).await?;
        if all_images.is_empty() && style_param.is_some() {
            all_images = self.fetch_all_images(&url, None).await?;
        }

        Ok(self.map_cover_options(all_images, 600, 900))
    }

    /// Browse hero banners for a game.
    pub async fn browse_heroes(
        &self,
        name: &str,
        style: Option<&str>,
    ) -> Result<Vec<CoverOption>, String> {
        let game_id = self
            .search_game(name)
            .await
            .ok_or_else(|| format!("Game '{}' not found on SteamGridDB", name))?;

        let url = format!("{}/heroes/game/{}", STEAMGRID_API_URL, game_id);
        let style_param = cover_style_param(style);
        let mut all_images = self.fetch_all_images(&url, style_param).await?;
        if all_images.is_empty() && style_param.is_some() {
            all_images = self.fetch_all_images(&url, None).await?;
        }

        Ok(self.map_cover_options(all_images, 1920, 620))
    }

    /// Browse logo variants for a game, sorted to prioritize one-line logos.
    pub async fn browse_logos(
        &self,
        name: &str,
        style: Option<&str>,
    ) -> Result<Vec<CoverOption>, String> {
        let game_id = self
            .search_game(name)
            .await
            .ok_or_else(|| format!("Game '{}' not found on SteamGridDB", name))?;

        let url = format!("{}/logos/game/{}", STEAMGRID_API_URL, game_id);
        let style_param = logo_style_param(style);
        let mut all_images = self.fetch_all_images(&url, style_param).await?;
        if all_images.is_empty() && style_param.is_some() {
            all_images = self.fetch_all_images(&url, None).await?;
        }
        let mut logos = self.map_cover_options(all_images, 1200, 400);
        Self::sort_logo_options(&mut logos);
        Ok(logos)
    }

}
