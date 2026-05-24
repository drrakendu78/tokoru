// Frontend mirrors of the Rust models in `src-tauri/src/models/`.
// Field names must match the serde JSON output exactly (snake_case in Rust ->
// snake_case here).

/// Canonical sources Tokoru understands. Steam is API-imported; everything
/// else is detected locally.
export type Source =
  | "steam"
  | "epic"
  | "gog"
  | "ubi"
  | "ea"
  | "itch"
  | "xbox"
  | "amazon"
  | "custom"
  | "local";

export interface Game {
  /// Stable id; only None right before insert. After a DB roundtrip it's always set.
  id: string | null;
  title: string;
  source: Source | string; // accept unknown strings defensively
  install_path: string | null;
  exe_path: string;
  artwork_url: string | null;
  hero_url: string | null;
  logo_url: string | null;
  /// Steam grid `<appid>_icon.jpg` URL (SteamGridDB or user-picked).
  /// Shown in Steam's friends list and taskbar tooltip.
  icon_url: string | null;
  platform_id: string | null;
  launch_command: string | null;
  last_scanned_at: number | null;
}

export type ShortcutStatus = "pending" | "pushed" | "removed";

export interface Shortcut {
  game_id: string;
  steam_appid: number;
  pushed_at: number | null;
  status: ShortcutStatus;
}

export interface PlaytimeSession {
  id: number | null;
  game_id: string;
  started_at: number;
  ended_at: number | null;
  duration_seconds: number | null;
}

// ============ Command result types ============

export interface ScanResult {
  new_games: number;
  total_found: number;
  /// Ids of rows newly inserted by this scan (not already in the DB).
  /// Drives the incremental artwork backfill: once the initial full
  /// pass has happened, subsequent boots only fetch artwork for these.
  new_game_ids: string[];
}

export interface SyncSteamResult {
  added: number;
  updated: number;
  total_owned: number;
  /// True when Steam rejected the stored `steamLoginSecure` JWT (token
  /// expired, password changed, account signed out elsewhere). The frontend
  /// surfaces "Steam session expired — reconnect" and the backend has
  /// already cleared the stored token, so the tile flips to the Connect
  /// state immediately after this returns.
  session_expired: boolean;
}

export interface SteamConnectionInfo {
  /// 64-bit SteamID, or null when not connected. We use `number` even though
  /// SteamID64 is technically beyond `Number.MAX_SAFE_INTEGER` for very old
  /// accounts (>2^53). In practice every modern SteamID fits — and serde_json
  /// emits a JS number so this matches the wire format.
  steam_id: number | null;
  connected: boolean;
  /// Persona name read from `loginusers.vdf` for the connected SteamID,
  /// when available. Used for "Signed in as <name>" in the Source tile UI.
  account_name: string | null;
}

export interface SteamConnectedInfo {
  steam_id: number;
  account_name: string | null;
}

// ============ Epic / GOG account connection ============

export interface EpicConnectionInfo {
  connected: boolean;
  account_name: string | null;
}

export interface EpicConnectedInfo {
  account_name: string;
  /// Number of brand-new games inserted at first connect. Updates are not
  /// counted here — Settings already shows them via `epic_sync_library`.
  games_added: number;
}

export interface EpicSyncResult {
  added: number;
  updated: number;
  total_owned: number;
}

export interface GogConnectionInfo {
  connected: boolean;
  account_name: string | null;
}

export interface GogConnectedInfo {
  account_name: string;
  games_added: number;
}

export interface GogSyncResult {
  added: number;
  updated: number;
  total_owned: number;
}

export interface PushResult {
  appid: number;
  /// True when the real Steam Collection (sidebar entry) was refreshed in
  /// Steam's leveldb. False when the leveldb write was skipped (Steam was
  /// running, DB not found, etc.).
  collection_updated: boolean;
  /// User-facing message when `collection_updated === false`. Otherwise null.
  collection_error: string | null;
}

export interface SyncPlaytimeResult {
  updated_count: number;
  steam_running: boolean;
}

export interface PlaytimeSummary {
  total_seconds: number;
  sessions: number;
  last_played: number | null;
  last_2weeks_seconds: number;
}

export interface HeatmapBucket {
  /// YYYY-MM-DD (UTC)
  date: string;
  total_seconds: number;
}

export interface TopPlayedRow {
  game_id: string;
  title: string;
  total_seconds: number;
}

export interface GlobalStats {
  total_hours: number;
  hours_this_month: number;
  longest_session_seconds: number;
  longest_streak_days: number;
}

export interface SessionsDayBucket {
  /// YYYY-MM-DD UTC
  date: string;
  sessions_count: number;
  avg_seconds: number;
  total_seconds: number;
}

export interface UntouchedGame {
  game_id: string;
  title: string;
  source: string;
  artwork_url: string | null;
  last_played: number | null;
  total_seconds: number;
}

// ============ Source display metadata ============
//
// The seven canonical "user-visible" sources. We keep `local` / `xbox` /
// `amazon` out of the label maps for now to keep the UI tight — they fall back
// to the source string itself.

export const KNOWN_SOURCES: Source[] = [
  "steam",
  "epic",
  "gog",
  "ubi",
  "ea",
  "itch",
  "custom",
];

export const SOURCE_LABEL: Record<string, string> = {
  steam: "Steam",
  epic: "Epic",
  gog: "GOG",
  ubi: "Uplay",
  ea: "EA App",
  itch: "Itch.io",
  xbox: "Xbox",
  amazon: "Amazon",
  custom: "Custom",
  local: "Local",
};

export const SOURCE_LONG: Record<string, string> = {
  steam: "Steam",
  epic: "Epic Games",
  gog: "GOG Galaxy",
  ubi: "Ubisoft Connect",
  ea: "EA App",
  itch: "Itch.io",
  xbox: "Xbox",
  amazon: "Amazon Games",
  custom: "Custom .exe",
  local: "Local",
};

// Per-source accent dots (matches the HANDOFF design system color tokens).
export const SOURCE_DOT: Record<string, string> = {
  steam: "#66C0F4",
  epic: "#FFFFFF",
  gog: "#C173FF",
  ubi: "#0088F0",
  ea: "#FFFFFF",
  itch: "#FA5C5C",
  xbox: "#107C10",
  amazon: "#FF9900",
  custom: "#A1A1A6",
  local: "#A1A1A6",
};

// ============ Consolidated source tile state ============
//
// Mirrors `commands::sources::SourceState` on the Rust side. One snapshot of
// every source's tile so the SourceTile UI doesn't have to fan out to
// `get_steam_connection` / `epic_get_connection` / `gog_get_connection` plus
// N separate count queries.

export interface SourceState {
  source: string;
  connected: boolean;
  /// Whether this source has a login flow at all. Local-only sources
  /// (ubi/ea/itch/custom) drop here as `false` and the UI renders the
  /// "Rescan now" affordance instead of "Connect".
  supports_login: boolean;
  account_name: string | null;
  /// Only populated for source = "steam".
  steam_id: number | null;
  /// Unix seconds, or null if never synced.
  last_sync_at: number | null;
  game_count: number;
}

export interface AllSourceStates {
  sources: SourceState[];
}

// ============ SteamGridDB browse ============

export interface CoverOption {
  url: string;
  thumb: string;
  is_animated: boolean;
  nsfw: boolean;
  width: number;
  height: number;
}

// ============ SteamGridDB user settings ============
//
// Mirrors `commands::steamgriddb::SteamGridDbSettings` on the Rust side. All
// three fields persist in `sync_state`. The artwork style flows through to
// `?styles=` on outgoing SGDB requests; the auto-fetch toggle gates future
// auto-fetch call sites in `add_game`; the API key (when non-empty) overrides
// the bundled default key for all artwork fetches.

export type SteamGridDbArtworkStyle =
  | "official"
  | "painted"
  | "photoreal"
  | "minimal"
  | "any";

export interface SteamGridDbSettings {
  /// Empty string when no custom key has been saved (the backend falls back
  /// to the bundled default key in that case).
  api_key: string;
  /// Convenience flag — equivalent to `api_key !== ""`, exposed so the UI can
  /// decide whether to render the "Last validated" subtitle without having to
  /// re-check.
  has_custom_key: boolean;
  artwork_style: SteamGridDbArtworkStyle;
  auto_fetch: boolean;
  /// When true, the auto-pick prefers animated `.webm` grids over static
  /// images for cover + hero. Cosmetic — Steam library lag scales with
  /// animated grid count (see lazy-loading discussion).
  prefer_animated: boolean;
  /// When true, NSFW-tagged grids are eligible in the auto-pick.
  /// Default false. The Browse covers UI ignores this (user already
  /// opted in by clicking the explicit NSFW toggle there).
  allow_nsfw: boolean;
  /// Unix seconds the API key was last persisted. 0 when no custom key was
  /// ever saved.
  api_key_saved_at: number;
}

// ============ Downloads ============
//
// Mirrors `services::runtime_state::DownloadState` and the two Tauri event
// channels emitted by `services::downloads`:
//   - `download-progress` → `DownloadProgressEvent` (~1Hz while downloading)
//   - `download-status` → `DownloadStatusEvent` (on every status change)
//
// The frontend hook (`useDownloads`) folds these into a local mirror so the
// `GameCard` can render install/pause/fail states without polling.

export type DownloadStatus =
  | "queued"
  | "downloading"
  | "paused"
  | "failed"
  | "completed";

export interface DownloadState {
  game_id: string;
  source: string;
  platform_id: string;
  game_name: string;
  status: DownloadStatus;
  progress_pct: number;
  speed_bps: number;
  eta_secs: number;
  downloaded_bytes: number;
  total_bytes: number;
  last_error: string | null;
  install_path: string | null;
  language: string | null;
}

export interface DownloadListEntry {
  game_id: string;
  state: DownloadState;
}

export interface DownloadProgressEvent {
  game_id: string;
  progress_pct: number;
  speed_bps: number;
  eta_secs: number;
  downloaded_bytes: number;
  total_bytes: number;
}

export interface DownloadStatusEvent {
  game_id: string;
  status: DownloadStatus;
  last_error: string | null;
}

export interface DownloadSettings {
  default_dir: string;
  per_source: Record<string, string>;
  max_parallel: number;
  auto_push_after_download: boolean;
  /// 0 = unlimited. Currently a UI-only setting — the runners (legendary /
  /// gogdl) don't expose a native bandwidth cap.
  bandwidth_kbps: number;
}
