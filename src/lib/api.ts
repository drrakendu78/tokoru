// Typed wrappers around `@tauri-apps/api/core` `invoke`.
//
// Mirrors the Rust commands registered in `src-tauri/src/lib.rs`. Keep argument
// names in camelCase — Tauri converts them to snake_case on the Rust side.

import { invoke } from "@tauri-apps/api/core";
import type {
  AllSourceStates,
  CoverOption,
  DownloadListEntry,
  DownloadSettings,
  DownloadState,
  EpicConnectedInfo,
  EpicConnectionInfo,
  EpicSyncResult,
  Game,
  GlobalStats,
  GogConnectedInfo,
  GogConnectionInfo,
  GogSyncResult,
  HeatmapBucket,
  PlaytimeSummary,
  PushResult,
  ScanResult,
  SessionsDayBucket,
  Shortcut,
  SteamConnectedInfo,
  SteamConnectionInfo,
  SteamGridDbArtworkStyle,
  SteamGridDbSettings,
  SyncPlaytimeResult,
  SyncSteamResult,
  TopPlayedRow,
  UntouchedGame,
} from "./types";

// ============ Games ============

export const getAllGames = (): Promise<Game[]> => invoke("get_all_games");

export const getGame = (id: string): Promise<Game | null> =>
  invoke("get_game", { id });

export const addGame = (args: {
  title: string;
  exePath: string;
  source?: string;
  launchCommand?: string;
}): Promise<string> =>
  invoke("add_game", {
    title: args.title,
    exePath: args.exePath,
    source: args.source ?? null,
    launchCommand: args.launchCommand ?? null,
  });

export const deleteGame = (id: string): Promise<void> =>
  invoke("delete_game", { id });

/// Set or clear the user-chosen display title (the "Rename" item in
/// the GameDetail more-menu). Pass `null` to revert to the
/// source-reported title.
export const setGameCustomTitle = (id: string, title: string | null): Promise<void> =>
  invoke("set_game_custom_title", { id, title });

/// User-curated tags (independent from the SteamSpy community tags).
/// Trimmed + deduped case-insensitively by the backend. Returns the
/// canonicalised list so the UI can re-render with the exact stored
/// order. Empty array clears all user tags.
export const setGameUserTags = (id: string, tags: string[]): Promise<string[]> =>
  invoke("set_game_user_tags", { id, tags });

export const getGameUserTags = (id: string): Promise<string[]> =>
  invoke("get_game_user_tags", { id });

/// Override the imported playtime for a game (in hours). Useful when
/// the source's log-based estimate undershoots (Star Citizen rotates
/// its logbackups after a few weeks so older sessions get lost). The
/// value seeds the total; locally-tracked sessions still accumulate
/// on top.
export const setGameManualPlaytimeHours = (id: string, hours: number): Promise<void> =>
  invoke("set_game_manual_playtime_hours", { id, hours });

/// Parse Star Citizen's `logbackups/*.log` files across every
/// installed channel (LIVE / PTU / EPTU / TECH-PREVIEW) and import
/// the total into the RSI Launcher row's playtime. Returns the
/// seconds written. Same approach as TradSC's `app_stats::get_playtime`.
export const importStarcitizenPlaytime = (): Promise<number> =>
  invoke("import_starcitizen_playtime");

/// Toggle the heart-icon favorite flag for a single game. Returns the
/// new state so the UI can flip optimistically without a follow-up read.
export const toggleGameFavorite = (id: string): Promise<boolean> =>
  invoke("toggle_game_favorite", { id });

export const getGameFavorite = (id: string): Promise<boolean> =>
  invoke("get_game_favorite", { id });

/// IDs of every favorited game — used by the Library "Favorites"
/// sidebar filter for a single O(1) Set lookup per render.
export const listFavoriteGameIds = (): Promise<string[]> =>
  invoke("list_favorite_game_ids");

export const listRecentlyPlayedIds = (days = 14): Promise<string[]> =>
  invoke("list_recently_played_ids", { days });

export const listNeverPlayedIds = (): Promise<string[]> =>
  invoke("list_never_played_ids");

export interface PlayStatEntry {
  game_id: string;
  total_seconds: number;
  last_played: number | null;
}

/// Per-game total playtime + last-played for the Library sort dropdown.
export const listGamePlayStats = (): Promise<PlayStatEntry[]> =>
  invoke("list_game_play_stats");

export interface SteamFavoritesImport {
  matched_collection: string;
  imported: number;
  unmatched: number;
}

/// Read the Steam-side "Favorites" collection (created in Steam itself,
/// not by Tokoru) and mark the matching games as favorites locally.
/// One-way: Tokoru → not pushed back to Steam.
export const importSteamFavorites = (): Promise<SteamFavoritesImport> =>
  invoke("import_steam_favorites");

export interface PushFavoritesResult {
  pushed: number;
  steam_restarted: boolean;
}

/// Push every Tokoru-favorited game into the Steam Favoris collection
/// (creates one if missing). Auto-restarts Steam when it's running —
/// `restartIfRunning=false` makes the call fail fast with a
/// `SteamRunning` error so the caller can offer a "save for later"
/// path instead.
export const pushFavoritesToSteam = (
  restartIfRunning = true,
): Promise<PushFavoritesResult> =>
  invoke("push_favorites_to_steam", { restartIfRunning });

// ============ Artwork ============

export const fetchArtwork = (id: string): Promise<string | null> =>
  invoke("fetch_artwork", { id });

/// `onlyIds` — when provided, only refetch artwork for these specific game
/// ids (the incremental "new games only" path called from boot/focus once
/// the initial full pass has happened). `undefined` walks the whole library
/// and flips the `artwork_initial_backfill_done` flag on success.
///
/// `force` — when true, bypasses the per-slot eligibility heuristics so
/// EVERY game with any artwork URL gets a fresh pick. Used by the Settings
/// "Refresh artwork now" button when the user just flipped a SteamGridDB
/// preference and wants it applied across the whole library regardless of
/// source (Steam / Epic / GOG / …).
export const fetchAllArtwork = (
  onlyIds?: string[],
  force = false
): Promise<number> =>
  invoke("fetch_all_artwork", { onlyIds: onlyIds ?? null, force });

export const isArtworkInitialBackfillDone = (): Promise<boolean> =>
  invoke("is_artwork_initial_backfill_done");

/// True once the user has gone through the welcome wizard at least once.
/// Drives the App boot route: when false → render `<Onboarding />` first
/// instead of jumping straight to the Library. Persisted in `sync_state`.
export const isOnboardingDone = (): Promise<boolean> =>
  invoke("is_onboarding_done");

export const markOnboardingDone = (): Promise<void> =>
  invoke("mark_onboarding_done");

/// Flip the onboarding flag back to false so the wizard re-runs. Triggered
/// by the "Replay onboarding" button in Settings — credentials and library
/// rows stay intact, only the boot-route flag is cleared.
export const resetOnboardingDone = (): Promise<void> =>
  invoke("reset_onboarding_done");

/// Real installed-game counts per source — populates the Onboarding
/// detection step with the actual numbers (Steam: 403, Epic: 23, …)
/// instead of placeholder constants. Cheap: just registry + manifest
/// walks, no DB writes.
export const detectInstalledCountsPerSource = (): Promise<Record<string, number>> =>
  invoke("detect_installed_counts_per_source");

/// One-shot catch-up that mirrors every existing Tokoru artwork URL into
/// Steam's `userdata/<id>/config/grid/` folder. Gated by a flag so it only
/// runs once per install (new artwork picked AFTER this runs is mirrored
/// synchronously by `fetch_artwork` / `fetch_all_artwork`). Returns the
/// number of grid files actually written (0 when the gate is closed).
export const mirrorExistingArtworkToSteam = (): Promise<number> =>
  invoke("mirror_existing_artwork_to_steam");

export const browseCovers = (gameName: string): Promise<CoverOption[]> =>
  invoke("browse_covers", { gameName });

export const browseHeroes = (gameName: string): Promise<CoverOption[]> =>
  invoke("browse_heroes", { gameName });

export const browseLogos = (gameName: string): Promise<CoverOption[]> =>
  invoke("browse_logos", { gameName });

export const browseIcons = (gameName: string): Promise<CoverOption[]> =>
  invoke("browse_icons", { gameName });

export const setGameCover = (id: string, coverUrl: string): Promise<void> =>
  invoke("set_game_cover", { id, coverUrl });

export const setGameHero = (id: string, heroUrl: string): Promise<void> =>
  invoke("set_game_hero", { id, heroUrl });

export const setGameLogo = (id: string, logoUrl: string): Promise<void> =>
  invoke("set_game_logo", { id, logoUrl });

export interface SetIconResult {
  steam_restarted: boolean;
}

/// Set a specific icon URL for a game. Auto-restarts Steam when running
/// because both the user-grid override AND `shortcuts.vdf` get re-read
/// only at Steam boot — the icon wouldn't apply visually otherwise.
export const setGameIcon = (id: string, iconUrl: string): Promise<SetIconResult> =>
  invoke("set_game_icon", { id, iconUrl });

// ============ SteamGridDB user settings ============
//
// API key / preferred artwork style / auto-fetch toggle. All three persist
// in `sync_state`; the backend reads them on every artwork fetch so changes
// take effect immediately (no app restart). Note: changing the artwork style
// does NOT refetch existing artwork — only the next `fetchArtwork` /
// `browse*` call observes the new preference.

export const getSteamgriddbSettings = (): Promise<SteamGridDbSettings> =>
  invoke("get_steamgriddb_settings");

export const setSteamgriddbSettings = (args: {
  apiKey?: string;
  artworkStyle?: SteamGridDbArtworkStyle;
  autoFetch?: boolean;
  preferAnimated?: boolean;
  allowNsfw?: boolean;
}): Promise<SteamGridDbSettings> =>
  invoke("set_steamgriddb_settings", {
    apiKey: args.apiKey ?? null,
    artworkStyle: args.artworkStyle ?? null,
    autoFetch: args.autoFetch ?? null,
    preferAnimated: args.preferAnimated ?? null,
    allowNsfw: args.allowNsfw ?? null,
  });

// ============ Scanning ============

export const scanLocalGames = (): Promise<ScanResult> =>
  invoke("scan_local_games");

export const scanCustomDirectory = (path: string): Promise<ScanResult> =>
  invoke("scan_custom_directory", { path });

export const detectPlatformGames = (): Promise<ScanResult> =>
  invoke("detect_platform_games");

export const fullScan = (): Promise<ScanResult> => invoke("full_scan");

/// Rescan ONE launcher source via its registry/manifest detector. Used by
/// the per-source Rescan button so the toast count actually reflects that
/// specific launcher (rather than the whole-machine total a `full_scan`
/// would report).
export const scanSource = (source: string): Promise<ScanResult> =>
  invoke("scan_source", { source });

// ============ Consolidated source tile state ============
//
// Batched read for the SourceTile grid (Sources page + Settings → Sources).
// Returns connected/account/last-sync/game-count for every supported source
// in one round-trip.

export const getAllSourceStates = (): Promise<AllSourceStates> =>
  invoke("get_all_source_states");

// ============ Steam web-login + IPlayerService library import ============
//
// Two-step flow matching Epic / GOG: `steamLoginStart` opens an in-app WebView
// at `store.steampowered.com/login/?redir=explore`. After the user signs in,
// the backend reads the `steamLoginSecure` cookie (`<steamid>%7C%7C<jwt>`),
// persists both halves in `sync_state`, and emits `steam-login-success` with
// `{ steam_id: number, account_name: string | null }`. The frontend then
// calls `steamLoginFinish` which acks the event and returns the display info.
// Cancellation / timeout / parse failure is signalled via the
// `steam-login-failure` event with `{ message: string }`.

export const steamLoginStart = (): Promise<void> => invoke("steam_login_start");

export const steamLoginFinish = (steamId: number): Promise<SteamConnectedInfo> =>
  invoke("steam_login_finish", { steamId });

export const disconnectSteam = (): Promise<void> => invoke("disconnect_steam");

export const getSteamConnection = (): Promise<SteamConnectionInfo> =>
  invoke("get_steam_connection");

export const syncSteamLibrary = (): Promise<SyncSteamResult> =>
  invoke("sync_steam_library");

// ============ Epic Games — browser OAuth via legendary, no API key ============
//
// Two-step flow: `epicLoginStart` opens a WebView; the user signs in there,
// the WebView captures the auth code internally and emits an
// `epic-login-success` event with `{ auth_code: string }`. The frontend
// listens for that event and calls `epicLoginFinish` with the code.

export const epicLoginStart = (): Promise<void> => invoke("epic_login_start");

export const epicLoginFinish = (authCode: string): Promise<EpicConnectedInfo> =>
  invoke("epic_login_finish", { authCode });

export const epicLogout = (): Promise<void> => invoke("epic_logout");

export const epicGetConnection = (): Promise<EpicConnectionInfo> =>
  invoke("epic_get_connection");

export const epicSyncLibrary = (): Promise<EpicSyncResult> =>
  invoke("epic_sync_library");

// ============ GOG — browser OAuth via gogdl, no API key ============

export const gogLoginStart = (): Promise<void> => invoke("gog_login_start");

export const gogLoginFinish = (authCode: string): Promise<GogConnectedInfo> =>
  invoke("gog_login_finish", { authCode });

export const gogLogout = (): Promise<void> => invoke("gog_logout");

export const gogGetConnection = (): Promise<GogConnectionInfo> =>
  invoke("gog_get_connection");

export const gogSyncLibrary = (): Promise<GogSyncResult> =>
  invoke("gog_sync_library");

// ============ Shortcuts (push/remove from Steam) ============

export const pushToSteam = (gameId: string): Promise<PushResult> =>
  invoke("push_to_steam", { gameId });

export const removeFromSteam = (gameId: string): Promise<void> =>
  invoke("remove_from_steam", { gameId });

export const getShortcut = (gameId: string): Promise<Shortcut | null> =>
  invoke("get_shortcut", { gameId });

export const getAllShortcuts = (): Promise<Shortcut[]> =>
  invoke("get_all_shortcuts");

export const syncPlaytimeNow = (): Promise<SyncPlaytimeResult> =>
  invoke("sync_playtime_now");

/// Rebuild every Tokoru-managed Steam Collection (sidebar entry) from
/// the current shortcuts DB state. Steam must be closed — Valve holds an
/// exclusive lock on the leveldb otherwise and will overwrite our changes
/// on shutdown. Returns the count of distinct source groups written.
export const syncCollectionsNow = (): Promise<number> =>
  invoke("sync_collections_now");

/// Collections grouping mode. Persisted on the Rust side under
/// `sync_state["collections_mode"]`. Defaults to "platform".
///   * `platform` — one collection per non-Steam source (Epic / GOG / Ubi).
///   * `franchise` — group by title-prefix (Dragon Ball, Yakuza, Witcher).
///                   Applies to BOTH Steam-native games and non-Steam shortcuts.
///   * `none`      — no automatic collections (existing Tokoru ones get
///                   stripped on next push so the sidebar resets).
export type CollectionsMode = "platform" | "franchise" | "none";

export const getCollectionsMode = (): Promise<CollectionsMode> =>
  invoke("get_collections_mode") as Promise<CollectionsMode>;

/// Persist a new mode. Does NOT rebuild the Steam Collections file — the
/// caller is expected to follow up with `restartSteam`, which shuts
/// Steam down, rebuilds Collections in the closed window, and relaunches.
export const setCollectionsMode = (mode: CollectionsMode): Promise<void> =>
  invoke("set_collections_mode", { mode });

export const getLastSyncAt = (): Promise<number | null> =>
  invoke("get_last_sync_at");

// ============ Metadata enrichment (Steam Store + SteamSpy + IGDB) ============

export interface MetadataSyncReport {
  total: number;
  synced: number;
  skipped_fresh: number;
  failed: number;
  errors: string[];
}

/// Multi-source metadata refresh. Iterates every Steam-source game and fills
/// in description / header / screenshots / dev / publisher / genres /
/// categories / tags / franchise / hltb_main_hours / similar_games / themes
/// from the configured sources (Steam Store + SteamSpy always; IGDB when
/// Twitch credentials are set; HLTB always; Wikidata as fallback).
///
/// `force = true` re-syncs every row regardless of `metadata_synced_at`.
export const syncMetadataNow = (force = false): Promise<MetadataSyncReport> =>
  invoke("sync_metadata_now", { force });

/// On-demand re-sync of metadata for a single game (more-menu "Refresh
/// metadata" item). Force semantics — bypasses the 14-day stale window.
/// Steam-source rows only; rejects on non-Steam or missing appid.
export const syncMetadataOne = (id: string): Promise<void> =>
  invoke("sync_metadata_one", { id });

/// Open the game's install directory in the OS file explorer.
export const openInstallFolder = (id: string): Promise<void> =>
  invoke("open_install_folder", { id });

export interface MetadataStatus {
  pending_count: number;
  total_steam_games: number;
  fully_synced: boolean;
}

/// Snapshot of how many Steam games still need their metadata enriched.
/// The Settings page uses this to gate the "Rebuild Collections" button:
/// rebuilding in Franchise mode before the sync has run gives a degraded
/// grouping (only the title-prefix passes), so we'd rather block it.
export const getMetadataStatus = (): Promise<MetadataStatus> =>
  invoke("get_metadata_status");

export interface TagEntry {
  name: string;
  votes: number;
}

export interface GameMetadataView {
  description: string | null;
  header_url: string | null;
  screenshots: string[];
  developer: string | null;
  publisher: string | null;
  genres: string[];
  categories: string[];
  tags: TagEntry[];
  franchise: string | null;
  igdb_id: number | null;
  themes: string[];
  similar_games: string[];
  hltb_main_hours: number | null;
  dlcs: number[];
  metadata_synced_at: number | null;
  /// True when the cached metadata was synced before the user picked the
  /// current UI language. GameDetail triggers a per-game refresh on open
  /// when this is true so the description / tags re-fetch in the new
  /// locale (instead of batch-syncing the whole library).
  needs_locale_refresh: boolean;
}

/// Rich enriched metadata for a single game (description, screenshots,
/// tags, dev/publisher, franchise, similar games, time-to-beat). Returns
/// null when the row doesn't exist; an "empty" view (all fields null /
/// empty arrays) when the row exists but `sync_metadata_now` hasn't
/// touched it yet.
export const getGameMetadata = (id: string): Promise<GameMetadataView | null> =>
  invoke("get_game_metadata", { id });

// ============ Achievements ============

export interface AchievementItem {
  apiname: string;
  name: string;
  description: string;
  achieved: boolean;
  /// Unix seconds. 0 when the achievement isn't unlocked.
  unlocktime: number;
  /// Full-color icon URL (shown for unlocked achievements).
  icon: string;
  /// Grayscale icon URL (shown when locked).
  icon_gray: string;
}

export interface AchievementsView {
  total: number;
  unlocked: number;
  items: AchievementItem[];
  synced_at: number | null;
}

/// Read cached achievements without touching Steam. Used by GameDetail
/// to render the panel instantly from the local DB before optionally
/// firing a refresh.
export const getGameAchievements = (id: string): Promise<AchievementsView> =>
  invoke("get_game_achievements", { id });

/// Refresh achievements from Steam's IPlayerStats endpoint and return
/// the resolved view. Respects a 24h stale window unless `force` is set.
export const syncGameAchievements = (
  id: string,
  force = false,
): Promise<AchievementsView> =>
  invoke("sync_game_achievements", { id, force });

export interface RecentUnlock {
  game_id: string;
  game_title: string;
  name: string;
  icon: string | null;
  unlocktime: number;
}

export interface GameCompletion {
  game_id: string;
  game_title: string;
  artwork_url: string | null;
  unlocked: number;
  total: number;
  pct: number;
}

export interface GlobalAchievementsStats {
  total_unlocked: number;
  total_available: number;
  games_with_achievements: number;
  recent_unlocks: RecentUnlock[];
  top_completion: GameCompletion[];
}

/// Cross-library achievements aggregation — Stats page block. Pure DB
/// read (no Steam round-trip), counts only games that already have a
/// cached achievement list.
export const getGlobalAchievementsStats = (): Promise<GlobalAchievementsStats> =>
  invoke("get_global_achievements_stats");

export interface TagCount {
  name: string;
  game_count: number;
}

export interface LibraryTagIndex {
  top_tags: TagCount[];
  /// Reverse map: tag name → game ids carrying that tag in their top-3.
  /// Used to filter the grid in O(1) when a sidebar pill is selected.
  games_by_tag: Record<string, string[]>;
}

/// Library-wide tag index — the actual community tags drawn from the
/// metadata sync, NOT the hardcoded RPG/FPS/Indie placeholders. Top 30
/// most-present tags + reverse index for fast filtering.
export const getLibraryTagIndex = (): Promise<LibraryTagIndex> =>
  invoke("get_library_tag_index");

export interface IgdbCredentials {
  client_id: string;
  client_secret: string;
}

export const getIgdbCredentials = (): Promise<IgdbCredentials> =>
  invoke("get_igdb_credentials");

export const setIgdbCredentials = (
  client_id: string,
  client_secret: string,
): Promise<void> => invoke("set_igdb_credentials", { clientId: client_id, clientSecret: client_secret });

/// Gracefully restart Steam (used after a push so the new shortcut appears
/// in the library immediately). Steam writes its in-memory state to disk on
/// `-shutdown`, so this is non-destructive.
export const restartSteam = (): Promise<void> => invoke("restart_steam");

/// Launch a protocol URI (steam://install, com.epicgames.launcher://, etc.)
/// via the OS handler. Bypasses Tauri's restrictive shell:allow-open scope
/// — Rust-side allowlist enforces what schemes are accepted.
export const launchUri = (uri: string): Promise<void> => invoke("launch_uri", { uri });

// ============ Settings preference (localStorage) ============

const KEY_RESTART_AFTER_PUSH = "steamshelf.restart_after_push";

export function getRestartAfterPush(): boolean {
  try {
    const v = localStorage.getItem(KEY_RESTART_AFTER_PUSH);
    if (v === null) return true; // default on
    return v === "true";
  } catch {
    return true;
  }
}

export function setRestartAfterPush(value: boolean): void {
  try {
    localStorage.setItem(KEY_RESTART_AFTER_PUSH, value ? "true" : "false");
  } catch {
    // ignore
  }
}

// ============ Stats ============

export const getPlaytimeSummary = (gameId: string): Promise<PlaytimeSummary> =>
  invoke("get_playtime_summary", { gameId });

export const getPlaytimeHeatmap = (days: number): Promise<HeatmapBucket[]> =>
  invoke("get_playtime_heatmap", { days });

export const getTopPlayed = (limit: number): Promise<TopPlayedRow[]> =>
  invoke("get_top_played", { limit });

export const getGlobalStats = (): Promise<GlobalStats> =>
  invoke("get_global_stats");

export const getSessionsOverTime = (days: number): Promise<SessionsDayBucket[]> =>
  invoke("get_sessions_over_time", { days });

export const getUntouchedGames = (limit: number): Promise<UntouchedGame[]> =>
  invoke("get_untouched_games", { limit });

// ============ Downloads (Epic via legendary, GOG via gogdl) ============
//
// The backend orchestrates the runner CLIs (legendary.exe / gogdl.exe) and
// emits progress / status changes via Tauri events:
//   - `download-progress` → DownloadProgressEvent (~1Hz while running)
//   - `download-status`   → DownloadStatusEvent (on every state change)
// `useDownloads()` is the canonical subscriber — these wrappers exist so any
// page can poll once at mount before plugging into the live event stream.

export const startDownload = (gameId: string): Promise<void> =>
  invoke("start_download", { gameId });

export const pauseDownload = (gameId: string): Promise<void> =>
  invoke("pause_download", { gameId });

export const resumeDownload = (gameId: string): Promise<void> =>
  invoke("resume_download", { gameId });

export const cancelDownload = (gameId: string): Promise<void> =>
  invoke("cancel_download", { gameId });

export const retryDownload = (gameId: string): Promise<void> =>
  invoke("retry_download", { gameId });

/// Clear a finished or failed download row from the Downloads page. Doesn't
/// delete files on disk and refuses to dismiss an actively-downloading entry.
export const dismissDownload = (gameId: string): Promise<void> =>
  invoke("dismiss_download", { gameId });

export const getDownloadState = (
  gameId: string
): Promise<DownloadState | null> =>
  invoke("get_download_state", { gameId });

export const getAllDownloads = (): Promise<DownloadListEntry[]> =>
  invoke("get_all_downloads");

export const getDownloadSettings = (): Promise<DownloadSettings> =>
  invoke("get_download_settings");

export const setDownloadSettings = (args: {
  defaultDir?: string;
  perSource?: Record<string, string>;
  maxParallel?: number;
  autoPushAfterDownload?: boolean;
  bandwidthKbps?: number;
}): Promise<DownloadSettings> =>
  invoke("set_download_settings", {
    defaultDir: args.defaultDir ?? null,
    perSource: args.perSource ?? null,
    maxParallel: args.maxParallel ?? null,
    autoPushAfterDownload: args.autoPushAfterDownload ?? null,
    bandwidthKbps: args.bandwidthKbps ?? null,
  });

/// Remove a game's downloaded files + clean up Tokoru tracking. Source
/// must be `epic` or `gog` — other sources don't have a Tokoru-managed
/// install. Cancels any in-flight download for the same game first.
export const uninstallGame = (gameId: string): Promise<void> =>
  invoke("uninstall_game", { gameId });

/// Presence + pinned-version check for the helper CLI binaries
/// (legendary / gogdl). Used by Settings → Downloads → Helper binaries.
export interface CliBinsStatus {
  legendary_present: boolean;
  gogdl_present: boolean;
  legendary_version: string | null;
  gogdl_version: string | null;
  legendary_path: string;
  gogdl_path: string;
}

export const getCliBinsStatus = (): Promise<CliBinsStatus> =>
  invoke("get_cli_bins_status");

export const redownloadLegendary = (): Promise<string> =>
  invoke("redownload_legendary");

export const redownloadGogdl = (): Promise<string> => invoke("redownload_gogdl");

/// Reconcile install status from the launchers themselves: reads
/// legendary's installed.json and scans configured GOG dirs for
/// `goggame-*.info` markers, then back-fills `install_path` on matching
/// DB rows. Runs automatically after Epic/GOG library sync; this wrapper
/// exists for the Settings → Downloads → "Detect installed games" button.
export interface DetectInstallsResult {
  epic_updated: number;
  gog_updated: number;
}

export const detectInstalls = (): Promise<DetectInstallsResult> =>
  invoke("detect_installs");

// ============ System ============

export const quitApp = (): Promise<void> => invoke("quit_app");

export const getSteamPath = (): Promise<string | null> =>
  invoke("get_steam_path");
