use rusqlite::{params, Connection, OptionalExtension, Result as SqlResult};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::models::{Game, PlaytimeSession, Shortcut, ShortcutStatus};

/// Optional-by-field update payload for the multi-source metadata sync. We
/// pass it as a struct rather than a 14-arg function call because the merge
/// touches every metadata column and ordering positional args got fragile.
///
/// Every field is `Option<&str>` / `Option<i64>` / `Option<f64>`: `None`
/// means "don't touch this column" (COALESCE preserves the existing value),
/// `Some(...)` overwrites. The writer always bumps `metadata_synced_at` to
/// the current timestamp regardless of which fields were populated.
#[derive(Default, Debug, Clone, Copy)]
pub struct GameMetadataUpdate<'a> {
    pub description: Option<&'a str>,
    pub header_url: Option<&'a str>,
    pub screenshots: Option<&'a str>,
    pub developer: Option<&'a str>,
    pub publisher: Option<&'a str>,
    pub genres: Option<&'a str>,
    pub categories: Option<&'a str>,
    pub tags: Option<&'a str>,
    pub franchise: Option<&'a str>,
    pub igdb_id: Option<i64>,
    pub themes: Option<&'a str>,
    pub similar_games: Option<&'a str>,
    pub hltb_main_hours: Option<f64>,
    pub dlcs: Option<&'a str>,
}

/// Tokoru SQLite store.
///
/// Schema is intentionally minimal — Tokoru only scans, pushes to Steam,
/// and observes playtime. No download state, no achievements, no launcher
/// overrides. See `init_tables` for the full schema.
///
/// Cloning a `Database` shares the underlying connection via `Arc` — both the
/// Tauri-managed state and the background watcher hold one handle each.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

/// Compute a stable text id for a game from its exe path.
/// We use a simple non-cryptographic hash so the id is short, URL-safe and
/// repeatable across scans.
pub fn game_id_from_exe(exe_path: &str) -> String {
    let normalized = exe_path.trim().to_ascii_lowercase();
    let hash = crc32(normalized.as_bytes());
    format!("g_{:08x}", hash)
}

/// Plain CRC32 (IEEE polynomial 0xEDB88320) — same one used to derive
/// non-Steam shortcut appids, so we keep one shared impl.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for i in 0..256u32 {
        let mut c = i;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
        }
        table[i as usize] = c;
    }

    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in bytes {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

impl Database {
    pub fn new(app_data_dir: PathBuf) -> SqlResult<Self> {
        std::fs::create_dir_all(&app_data_dir).ok();
        let db_path = app_data_dir.join("tokoru.db");
        // Migration: pre-rename builds wrote `steamshelf.db`. If the legacy
        // file exists and the new one doesn't, rename in place so the user
        // keeps their library / settings / metadata across the rebrand.
        let legacy = app_data_dir.join("steamshelf.db");
        if legacy.exists() && !db_path.exists() {
            if let Err(e) = std::fs::rename(&legacy, &db_path) {
                log::warn!(
                    "Failed to migrate legacy steamshelf.db → tokoru.db: {} (continuing with fresh DB)",
                    e
                );
            } else {
                log::info!("Migrated legacy steamshelf.db → tokoru.db");
            }
        }
        let conn = Connection::open(db_path)?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.init_tables()?;
        Ok(db)
    }

    fn init_tables(&self) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS games (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                source TEXT NOT NULL,
                install_path TEXT,
                exe_path TEXT NOT NULL UNIQUE,
                artwork_url TEXT,
                hero_url TEXT,
                logo_url TEXT,
                platform_id TEXT,
                launch_command TEXT,
                last_scanned_at INTEGER
            );

            CREATE INDEX IF NOT EXISTS idx_games_title ON games(title);
            CREATE INDEX IF NOT EXISTS idx_games_source ON games(source);

            CREATE TABLE IF NOT EXISTS shortcuts (
                game_id TEXT PRIMARY KEY,
                steam_appid INTEGER NOT NULL,
                pushed_at INTEGER,
                status TEXT NOT NULL DEFAULT 'pending',
                FOREIGN KEY(game_id) REFERENCES games(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_shortcuts_status ON shortcuts(status);

            CREATE TABLE IF NOT EXISTS playtime_sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                game_id TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                ended_at INTEGER,
                duration_seconds INTEGER,
                FOREIGN KEY(game_id) REFERENCES games(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_playtime_game ON playtime_sessions(game_id);
            CREATE INDEX IF NOT EXISTS idx_playtime_started ON playtime_sessions(started_at);

            CREATE TABLE IF NOT EXISTS sync_state (
                key TEXT PRIMARY KEY,
                value TEXT
            );
            ",
        )?;

        // Migration: add `imported_playtime_seconds` to games. Holds the
        // cumulative playtime reported by the platform API itself (Steam:
        // `playtime_forever`, GOG: gameplay endpoint), summed with local
        // session tracking by `total_playtime_seconds`. ALTER TABLE returns
        // an error when the column already exists — we swallow it.
        let _ = conn.execute(
            "ALTER TABLE games ADD COLUMN imported_playtime_seconds INTEGER NOT NULL DEFAULT 0",
            [],
        );

        // Migration: `tags` JSON column for Steam community tags. We fetch
        // these from SteamSpy on demand (rate-limited) and use them for
        // the "By tags" Collections grouping mode. JSON-encoded
        // `Vec<(String, u64)>` sorted by vote count descending — top tag
        // first.
        let _ = conn.execute(
            "ALTER TABLE games ADD COLUMN tags TEXT",
            [],
        );

        // Migration: multi-source metadata enrichment.
        //   Phase 1 = Steam Store + SteamSpy
        //     description, header_url, screenshots, developer, publisher,
        //     genres, categories
        //   Phase 2 = IGDB (Twitch OAuth) + HowLongToBeat (scrape)
        //     franchise (canonical from IGDB), igdb_id, themes (JSON),
        //     similar_games (JSON), hltb_main_hours
        //   Phase 3 = Wikidata (SPARQL fallback for series)
        //     no extra column — slots into `franchise` when IGDB is empty
        // All nullable, populated on demand by sync_metadata_now. ALTER
        // TABLE returns an error when the column already exists — we
        // swallow it.
        for stmt in [
            "ALTER TABLE games ADD COLUMN description TEXT",
            "ALTER TABLE games ADD COLUMN header_url TEXT",
            "ALTER TABLE games ADD COLUMN screenshots TEXT",
            "ALTER TABLE games ADD COLUMN developer TEXT",
            "ALTER TABLE games ADD COLUMN publisher TEXT",
            "ALTER TABLE games ADD COLUMN genres TEXT",
            "ALTER TABLE games ADD COLUMN categories TEXT",
            "ALTER TABLE games ADD COLUMN franchise TEXT",
            "ALTER TABLE games ADD COLUMN igdb_id INTEGER",
            "ALTER TABLE games ADD COLUMN themes TEXT",
            "ALTER TABLE games ADD COLUMN similar_games TEXT",
            "ALTER TABLE games ADD COLUMN hltb_main_hours REAL",
            "ALTER TABLE games ADD COLUMN metadata_synced_at INTEGER",
            // User-curated flags. `favorite` is the heart icon in
            // GameDetail / the "Favorites" sidebar filter — purely
            // local, never synced to any external source.
            "ALTER TABLE games ADD COLUMN favorite INTEGER NOT NULL DEFAULT 0",
            // DLC appids JSON, fetched from Steam Store appdetails.
            // Stored as `Vec<i64>` of appids — frontend resolves names
            // lazily via another store call when the user opens the
            // panel (most games have 0-2 DLCs so eager-fetching all
            // names would bloat the sync).
            "ALTER TABLE games ADD COLUMN dlcs TEXT",
            // Achievements JSON (Vec<AchievementItem>) — fetched from
            // ISteamUserStats/GetPlayerAchievements using the JWT
            // access_token. Empty array when the game has no
            // achievements at all (Steam returns "no stats" → we
            // record an empty list and bump the sync timestamp so we
            // don't re-hit the API for nothing).
            "ALTER TABLE games ADD COLUMN achievements TEXT",
            "ALTER TABLE games ADD COLUMN achievements_synced_at INTEGER",
            // Platform-side last-played timestamp (unix seconds). Steam
            // gives us `rtime_last_played` on the owned-games endpoint;
            // GOG gives us a similar field on the gameplay endpoint.
            // Without this column the Library "Recently Played" filter
            // misses games the user played on the platform directly
            // (Tokoru only sees locally-watched sessions otherwise).
            "ALTER TABLE games ADD COLUMN last_played_imported INTEGER",
            // Sticky-artwork flags. Set to 1 by `set_game_cover` /
            // `set_game_hero` / `set_game_logo` so subsequent
            // `sync_steam_library` upserts don't clobber the user's
            // manual artwork choice with the Steam Store fallback.
            "ALTER TABLE games ADD COLUMN artwork_user_set INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE games ADD COLUMN hero_user_set INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE games ADD COLUMN logo_user_set INTEGER NOT NULL DEFAULT 0",
            // User-supplied override for the displayed title. The original
            // `title` column stays as Steam/launcher reported it (matters
            // for the scanner's name-based matching); the UI prefers
            // `custom_title` when set. Null clears the override.
            "ALTER TABLE games ADD COLUMN custom_title TEXT",
            // Sticky-playtime flag — set by the manual "Set playtime"
            // modal in GameDetail. Once flipped, the various automated
            // playtime importers (Steam Web API, RSI logbackups
            // parser) refuse to overwrite the value so the user's
            // hand-picked total doesn't get clobbered on the next boot.
            "ALTER TABLE games ADD COLUMN imported_playtime_user_set INTEGER NOT NULL DEFAULT 0",
            // Game icon URL (Steam grid `<appid>_icon.jpg`) — fourth
            // artwork slot alongside cover/hero/logo. Steam shows this
            // tiny icon in the friends list, taskbar tooltip, and the
            // "currently playing" header. SteamGridDB hosts icons under
            // `/icons/game/{id}`. Sticky like the other slots.
            "ALTER TABLE games ADD COLUMN icon_url TEXT",
            "ALTER TABLE games ADD COLUMN icon_user_set INTEGER NOT NULL DEFAULT 0",
            // User-curated tags — JSON `Vec<String>`. Distinct from the
            // SteamSpy `tags` column (community-sourced, vote-weighted):
            // these are picked by hand in GameDetail, never overwritten
            // by any sync, and folded into the Library tag index alongside
            // the auto tags so the sidebar filter sees both. Empty/null
            // = no user tags.
            "ALTER TABLE games ADD COLUMN user_tags TEXT",
        ] {
            let _ = conn.execute(stmt, []);
        }

        Ok(())
    }

    // ============ Games ============

    /// Insert a new game (no-op if exe_path already exists).
    pub fn insert_game(&self, game: &Game) -> SqlResult<String> {
        let id = game
            .id
            .clone()
            .unwrap_or_else(|| game_id_from_exe(&game.exe_path));
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO games (
                id, title, source, install_path, exe_path,
                artwork_url, hero_url, logo_url,
                platform_id, launch_command, last_scanned_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                id,
                game.title,
                game.source,
                game.install_path,
                game.exe_path,
                game.artwork_url,
                game.hero_url,
                game.logo_url,
                game.platform_id,
                game.launch_command,
                game.last_scanned_at.unwrap_or(now),
            ],
        )?;
        Ok(id)
    }

    /// Look up an existing Steam game by its `platform_id` (Steam appid).
    /// Returns the row's stable id when the (source='steam', platform_id) pair
    /// already exists. Used by the Steam-aware merge upserts so the API path
    /// and the local-install detect path converge on a single row.
    pub(crate) fn find_steam_id_by_appid(&self, appid: &str) -> SqlResult<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id FROM games WHERE source = 'steam' AND platform_id = ?1",
            params![appid],
            |row| row.get::<_, String>(0),
        )
        .optional()
    }

    /// Every Steam-source row that currently has an `install_path` set.
    /// Used by the Steam manifest watcher to detect rows whose install dir
    /// no longer exists (uninstall, drive moved, etc.) and clear the path.
    pub(crate) fn list_steam_installed(&self) -> SqlResult<Vec<(String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, platform_id, install_path FROM games
             WHERE source = 'steam' AND install_path IS NOT NULL AND install_path != ''",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.collect()
    }

    /// Upsert a Steam game coming from the **Steam Web API** path.
    ///
    /// Merge policy when an existing row matches `(source='steam', platform_id)`:
    /// - Overwrites: `title`, `artwork_url`, `hero_url`, `logo_url`,
    ///   `launch_command`, `last_scanned_at` (API is canonical for metadata).
    /// - Preserves: `install_path`, `exe_path` (the local detect path knows the
    ///   real exe on disk, which is what the playtime watcher needs).
    ///
    /// Falls back to the generic `upsert_game` if the game has no `platform_id`
    /// or isn't tagged as `source = "steam"`.
    pub fn upsert_game_from_api(&self, game: &Game) -> SqlResult<String> {
        if game.source != "steam" {
            return self.upsert_game(game);
        }
        let Some(appid) = game.platform_id.as_deref() else {
            return self.upsert_game(game);
        };

        let now = chrono::Utc::now().timestamp();

        if let Some(existing_id) = self.find_steam_id_by_appid(appid)? {
            let conn = self.conn.lock().unwrap();
            // Sticky artwork: `*_user_set = 1` means the user picked
            // a cover/hero/logo in GameDetail; we never overwrite
            // those, even if Steam now returns its store fallback.
            conn.execute(
                "UPDATE games SET
                    title = ?1,
                    artwork_url = CASE WHEN artwork_user_set = 1 THEN artwork_url ELSE COALESCE(?2, artwork_url) END,
                    hero_url = CASE WHEN hero_user_set = 1 THEN hero_url ELSE COALESCE(?3, hero_url) END,
                    logo_url = CASE WHEN logo_user_set = 1 THEN logo_url ELSE COALESCE(?4, logo_url) END,
                    launch_command = COALESCE(?5, launch_command),
                    last_scanned_at = ?6
                 WHERE id = ?7",
                params![
                    game.title,
                    game.artwork_url,
                    game.hero_url,
                    game.logo_url,
                    game.launch_command,
                    now,
                    existing_id,
                ],
            )?;
            return Ok(existing_id);
        }

        // No existing Steam row for this appid — insert fresh.
        let id = game
            .id
            .clone()
            .unwrap_or_else(|| game_id_from_exe(&game.exe_path));
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO games (
                id, title, source, install_path, exe_path,
                artwork_url, hero_url, logo_url,
                platform_id, launch_command, last_scanned_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                id,
                game.title,
                game.source,
                game.install_path,
                game.exe_path,
                game.artwork_url,
                game.hero_url,
                game.logo_url,
                game.platform_id,
                game.launch_command,
                game.last_scanned_at.unwrap_or(now),
            ],
        )?;
        Ok(id)
    }

    /// Upsert a Steam game coming from the **local install detect** path.
    ///
    /// Merge policy when an existing row matches `(source='steam', platform_id)`:
    /// - Overwrites: `install_path`, `exe_path`, `launch_command`,
    ///   `last_scanned_at` (local knows where the binary actually is).
    /// - Preserves: `title`, `artwork_url`, `hero_url`, `logo_url`
    ///   (the API has the canonical Steam-store name + library art).
    ///
    /// Falls back to the generic `upsert_game` if the game has no `platform_id`
    /// or isn't tagged as `source = "steam"`.
    pub fn upsert_game_from_local(&self, game: &Game) -> SqlResult<String> {
        if game.source != "steam" {
            return self.upsert_game(game);
        }
        let Some(appid) = game.platform_id.as_deref() else {
            return self.upsert_game(game);
        };

        let now = chrono::Utc::now().timestamp();

        if let Some(existing_id) = self.find_steam_id_by_appid(appid)? {
            let conn = self.conn.lock().unwrap();
            conn.execute(
                "UPDATE games SET
                    install_path = COALESCE(?1, install_path),
                    exe_path = ?2,
                    launch_command = COALESCE(?3, launch_command),
                    last_scanned_at = ?4
                 WHERE id = ?5",
                params![
                    game.install_path,
                    game.exe_path,
                    game.launch_command,
                    now,
                    existing_id,
                ],
            )?;
            return Ok(existing_id);
        }

        // No existing Steam row for this appid — insert fresh.
        let id = game
            .id
            .clone()
            .unwrap_or_else(|| game_id_from_exe(&game.exe_path));
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO games (
                id, title, source, install_path, exe_path,
                artwork_url, hero_url, logo_url,
                platform_id, launch_command, last_scanned_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                id,
                game.title,
                game.source,
                game.install_path,
                game.exe_path,
                game.artwork_url,
                game.hero_url,
                game.logo_url,
                game.platform_id,
                game.launch_command,
                game.last_scanned_at.unwrap_or(now),
            ],
        )?;
        Ok(id)
    }

    /// Upsert by id (or compute from exe_path): updates title/install/launch
    /// info and refreshes `last_scanned_at`, but never overwrites existing
    /// artwork with NULL (artwork is sticky).
    pub fn upsert_game(&self, game: &Game) -> SqlResult<String> {
        let id = game
            .id
            .clone()
            .unwrap_or_else(|| game_id_from_exe(&game.exe_path));
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock().unwrap();

        let existing: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM games WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;

        if existing.is_some() {
            // Sticky artwork — same guard as the Steam-API upsert.
            conn.execute(
                "UPDATE games SET
                    title = ?1,
                    source = ?2,
                    install_path = COALESCE(?3, install_path),
                    exe_path = ?4,
                    artwork_url = CASE WHEN artwork_user_set = 1 THEN artwork_url ELSE COALESCE(?5, artwork_url) END,
                    hero_url = CASE WHEN hero_user_set = 1 THEN hero_url ELSE COALESCE(?6, hero_url) END,
                    logo_url = CASE WHEN logo_user_set = 1 THEN logo_url ELSE COALESCE(?7, logo_url) END,
                    platform_id = COALESCE(?8, platform_id),
                    launch_command = COALESCE(?9, launch_command),
                    last_scanned_at = ?10
                 WHERE id = ?11",
                params![
                    game.title,
                    game.source,
                    game.install_path,
                    game.exe_path,
                    game.artwork_url,
                    game.hero_url,
                    game.logo_url,
                    game.platform_id,
                    game.launch_command,
                    now,
                    id,
                ],
            )?;
        } else {
            conn.execute(
                "INSERT INTO games (
                    id, title, source, install_path, exe_path,
                    artwork_url, hero_url, logo_url,
                    platform_id, launch_command, last_scanned_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    id,
                    game.title,
                    game.source,
                    game.install_path,
                    game.exe_path,
                    game.artwork_url,
                    game.hero_url,
                    game.logo_url,
                    game.platform_id,
                    game.launch_command,
                    now,
                ],
            )?;
        }
        Ok(id)
    }

    pub fn get_all_games(&self) -> SqlResult<Vec<Game>> {
        let conn = self.conn.lock().unwrap();
        // `COALESCE(custom_title, title)` lets the user rename a game
        // in GameDetail without us having to touch every read site —
        // the rest of the app keeps reading `game.title` and gets the
        // override transparently.
        let mut stmt = conn.prepare(
            "SELECT
                id, COALESCE(custom_title, title), source, install_path, exe_path,
                artwork_url, hero_url, logo_url,
                platform_id, launch_command, last_scanned_at,
                icon_url
             FROM games ORDER BY COALESCE(custom_title, title) ASC",
        )?;
        let games = stmt
            .query_map([], row_to_game)?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(games)
    }

    /// Snapshot of every game id currently in the DB. Used by the scan
    /// commands so they can compute "which games are *newly* inserted by
    /// this scan" (= ids returned by an upsert that weren't in the snapshot).
    /// Cheaper than a full `get_all_games` since we only need the ids.
    pub fn list_all_game_ids(&self) -> SqlResult<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id FROM games")?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(ids)
    }

    pub fn get_game_by_id(&self, id: &str) -> SqlResult<Option<Game>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT
                id, COALESCE(custom_title, title), source, install_path, exe_path,
                artwork_url, hero_url, logo_url,
                platform_id, launch_command, last_scanned_at,
                icon_url
             FROM games WHERE id = ?1",
            params![id],
            row_to_game,
        )
        .optional()
    }

    /// Set a user-chosen display title for a game. Pass `None` to clear
    /// the override (UI reverts to the source-reported title). The
    /// original `title` column is never touched — keeps the scanner's
    /// name-based matching working when an install gets re-detected.
    pub fn set_custom_title(&self, id: &str, title: Option<&str>) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        let value: Option<&str> = title.map(|s| s.trim()).filter(|s| !s.is_empty());
        conn.execute(
            "UPDATE games SET custom_title = ?1 WHERE id = ?2",
            params![value, id],
        )?;
        Ok(())
    }

    /// Overwrite the JSON-encoded user_tags column. The command-side wrapper
    /// is responsible for trimming / deduping / serializing; this just writes
    /// the raw string. Pass `None` to clear the column.
    pub fn set_user_tags(&self, id: &str, json: Option<&str>) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE games SET user_tags = ?1 WHERE id = ?2",
            params![json, id],
        )?;
        Ok(())
    }

    /// Read the raw JSON of the user_tags column. `None` when the row
    /// doesn't exist or the column is NULL. Parsing into `Vec<String>` is
    /// the caller's job.
    pub fn get_user_tags(&self, id: &str) -> SqlResult<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT user_tags FROM games WHERE id = ?1",
            params![id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(|opt| opt.flatten())
    }

    /// Return every `(game_id, user_tags_json)` pair where user_tags is set.
    /// Used by `get_library_tag_index` to fold user tags into the sidebar
    /// filter alongside the SteamSpy ones.
    pub fn list_all_user_tags(&self) -> SqlResult<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, user_tags FROM games
             WHERE user_tags IS NOT NULL AND user_tags != ''",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect()
    }

    /// Total games tagged with `source = ?`. Used by the SourceTile UI to
    /// show "N games" under each tile without pulling the full game list.
    pub fn count_games_by_source(&self, source: &str) -> SqlResult<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM games WHERE source = ?1",
            params![source],
            |row| row.get(0),
        )
    }

    pub fn game_exists_by_path(&self, exe_path: &str) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM games WHERE exe_path = ?1",
            params![exe_path],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// True if a row with `source='steam'` and the given Steam appid already
    /// exists. Used by the API sync to differentiate "newly added" vs "updated"
    /// when the local-install detect path may have inserted the row first
    /// (with a different exe_path).
    pub fn steam_game_exists_by_appid(&self, appid: &str) -> SqlResult<bool> {
        Ok(self.find_steam_id_by_appid(appid)?.is_some())
    }

    /// Clear the install_path column for the given game. Used by the uninstall
    /// flow — `upsert_game` uses COALESCE for sticky fields and would refuse
    /// to set install_path back to NULL, so we need a dedicated update.
    pub fn clear_install_path(&self, id: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE games SET install_path = NULL WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// Set the install_path for a game looked up by (source, platform_id).
    ///
    /// Used by the install-detection sync (legendary list-installed / gogdl
    /// goggame-*.info scan) to mark owned-library rows as installed without
    /// having to re-derive the synthetic id. Also a safer write path for the
    /// post-completion handler than `upsert_game` because it touches
    /// `install_path` + `exe_path` only — no UNIQUE(exe_path) collision risk
    /// from setting some other column.
    ///
    /// When `exe_path` looks like a launch URI (an owned-library placeholder
    /// such as `com.epicgames.launcher://...`), this helper makes a
    /// best-effort attempt to derive a real .exe under the install root
    /// (max depth 2) so the playtime watcher has something to bind to. If
    /// nothing is found, `exe_path` is left unchanged — better stale than
    /// a UNIQUE collision.
    ///
    /// Returns `true` if a row was updated, `false` when no row matched.
    pub fn set_install_path_by_platform(
        &self,
        source: &str,
        platform_id: &str,
        install_path: &str,
    ) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        // Look up the row + its current exe_path so we can decide whether
        // to refresh exe_path or leave it sticky.
        let existing: Option<(String, String)> = conn
            .query_row(
                "SELECT id, exe_path FROM games
                 WHERE source = ?1 AND platform_id = ?2",
                params![source, platform_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((id, current_exe)) = existing else {
            return Ok(false);
        };

        // Refresh exe_path only when the current value looks like an
        // owned-library placeholder (protocol URI) — i.e. the file doesn't
        // actually exist on disk yet. Otherwise the local scanner already
        // bound it to a real .exe and we shouldn't second-guess.
        let exe_looks_placeholder = current_exe.contains("://")
            || !std::path::Path::new(&current_exe).exists();
        let derived_exe = if exe_looks_placeholder {
            derive_main_exe(install_path)
        } else {
            None
        };

        // Update exe_path only if we found a candidate AND nobody else is
        // already pointing at it (UNIQUE constraint). On collision we keep
        // the existing exe_path.
        let next_exe = match derived_exe {
            Some(candidate) => {
                let in_use: Option<String> = conn
                    .query_row(
                        "SELECT id FROM games WHERE exe_path = ?1 AND id <> ?2",
                        params![candidate, id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                if in_use.is_some() {
                    None
                } else {
                    Some(candidate)
                }
            }
            None => None,
        };

        let now = chrono::Utc::now().timestamp();
        let updated = if let Some(exe) = next_exe.as_deref() {
            conn.execute(
                "UPDATE games SET
                    install_path = ?1,
                    exe_path = ?2,
                    last_scanned_at = ?3
                 WHERE id = ?4",
                params![install_path, exe, now, id],
            )?
        } else {
            conn.execute(
                "UPDATE games SET
                    install_path = ?1,
                    last_scanned_at = ?2
                 WHERE id = ?3",
                params![install_path, now, id],
            )?
        };
        Ok(updated > 0)
    }

    /// User picks an artwork in GameDetail → we also flip the
    /// `artwork_user_set` flag so subsequent sync passes (Steam Web
    /// API, GOG, Epic, anything that goes through `upsert_game_*`)
    /// stop clobbering it with the source-provided default.
    pub fn update_artwork(&self, id: &str, artwork_url: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE games SET artwork_url = ?1, artwork_user_set = 1 WHERE id = ?2",
            params![artwork_url, id],
        )?;
        Ok(())
    }

    pub fn update_hero(&self, id: &str, hero_url: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE games SET hero_url = ?1, hero_user_set = 1 WHERE id = ?2",
            params![hero_url, id],
        )?;
        Ok(())
    }

    pub fn update_logo(&self, id: &str, logo_url: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE games SET logo_url = ?1, logo_user_set = 1 WHERE id = ?2",
            params![logo_url, id],
        )?;
        Ok(())
    }

    pub fn update_icon(&self, id: &str, icon_url: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE games SET icon_url = ?1, icon_user_set = 1 WHERE id = ?2",
            params![icon_url, id],
        )?;
        Ok(())
    }

    pub fn delete_game(&self, id: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        // ON DELETE CASCADE handles shortcuts/playtime
        conn.execute("DELETE FROM games WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ============ Shortcuts ============

    pub fn upsert_shortcut(&self, shortcut: &Shortcut) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO shortcuts (game_id, steam_appid, pushed_at, status)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(game_id) DO UPDATE SET
                steam_appid = excluded.steam_appid,
                pushed_at = excluded.pushed_at,
                status = excluded.status",
            params![
                shortcut.game_id,
                shortcut.steam_appid as i64,
                shortcut.pushed_at,
                shortcut.status.as_str(),
            ],
        )?;
        Ok(())
    }

    pub fn get_shortcut(&self, game_id: &str) -> SqlResult<Option<Shortcut>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT game_id, steam_appid, pushed_at, status
             FROM shortcuts WHERE game_id = ?1",
            params![game_id],
            row_to_shortcut,
        )
        .optional()
    }

    pub fn get_all_shortcuts(&self) -> SqlResult<Vec<Shortcut>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT game_id, steam_appid, pushed_at, status FROM shortcuts ORDER BY game_id ASC",
        )?;
        let rows = stmt
            .query_map([], row_to_shortcut)?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(rows)
    }

    #[allow(dead_code)]
    pub fn delete_shortcut(&self, game_id: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM shortcuts WHERE game_id = ?1", params![game_id])?;
        Ok(())
    }

    // ============ Playtime sessions ============

    /// Start a new playtime session. Returns the autoincrement row id.
    pub fn start_playtime_session(&self, game_id: &str, started_at: i64) -> SqlResult<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO playtime_sessions (game_id, started_at) VALUES (?1, ?2)",
            params![game_id, started_at],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Close out an existing playtime session.
    pub fn end_playtime_session(
        &self,
        session_id: i64,
        ended_at: i64,
        duration_seconds: i64,
    ) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE playtime_sessions
             SET ended_at = ?1, duration_seconds = ?2
             WHERE id = ?3",
            params![ended_at, duration_seconds, session_id],
        )?;
        Ok(())
    }

    pub fn get_playtime_sessions(&self, game_id: &str) -> SqlResult<Vec<PlaytimeSession>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, game_id, started_at, ended_at, duration_seconds
             FROM playtime_sessions WHERE game_id = ?1
             ORDER BY started_at DESC",
        )?;
        let rows = stmt
            .query_map(params![game_id], row_to_session)?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(rows)
    }

    /// All tracked sessions that started after `cutoff_ts` (UTC seconds).
    /// Drives the Stats page's "Sessions over time" line chart — we need
    /// EVERY session in the window across all games, not per-game.
    pub fn get_all_sessions_since(&self, cutoff_ts: i64) -> SqlResult<Vec<PlaytimeSession>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, game_id, started_at, ended_at, duration_seconds
             FROM playtime_sessions
             WHERE started_at >= ?1
             ORDER BY started_at ASC",
        )?;
        let rows = stmt
            .query_map(params![cutoff_ts], row_to_session)?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(rows)
    }

    /// Most recent session end (or start, if the session never closed) for
    /// a game. Returns None when no tracked sessions exist — caller can
    /// then fall back to imported playtime metadata if they want.
    pub fn last_played_at(&self, game_id: &str) -> SqlResult<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        let ts: Option<i64> = conn
            .query_row(
                "SELECT MAX(COALESCE(ended_at, started_at))
                 FROM playtime_sessions WHERE game_id = ?1",
                params![game_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(ts)
    }

    /// Total accumulated playtime in seconds for a game — sums local
    /// session-watcher data with the platform-imported total (Steam API
    /// playtime_forever, GOG gameplay endpoint). Either component may be 0.
    pub fn total_playtime_seconds(&self, game_id: &str) -> SqlResult<i64> {
        let conn = self.conn.lock().unwrap();
        let sessions: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(duration_seconds), 0)
                 FROM playtime_sessions WHERE game_id = ?1",
                params![game_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let imported: i64 = conn
            .query_row(
                "SELECT COALESCE(imported_playtime_seconds, 0) FROM games WHERE id = ?1",
                params![game_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(sessions + imported)
    }

    /// Set the platform-imported playtime total for a game looked up
    /// by its stable id. Respects the `imported_playtime_user_set`
    /// flag — once the user has typed a manual total in GameDetail's
    /// "Set playtime" modal, we don't let the automated importers
    /// (RSI logbackups parser, Steam Web API sync) overwrite it.
    pub fn set_imported_playtime_by_id(&self, id: &str, seconds: i64) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE games SET imported_playtime_seconds = ?1
             WHERE id = ?2 AND COALESCE(imported_playtime_user_set, 0) = 0",
            params![seconds, id],
        )?;
        Ok(())
    }

    /// Force-set the playtime override AND flip the sticky flag — the
    /// "Set playtime" modal in GameDetail uses this so subsequent
    /// auto-imports keep their hands off.
    pub fn set_manual_playtime_by_id(&self, id: &str, seconds: i64) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE games SET imported_playtime_seconds = ?1,
                              imported_playtime_user_set = 1
             WHERE id = ?2",
            params![seconds, id],
        )?;
        Ok(())
    }

    /// Set the platform-imported playtime total for a game (looked up by
    /// `(source, platform_id)`). Used by Steam / GOG sync to drop their
    /// `playtime_forever` equivalent into the games row without colliding
    /// with the synthetic id derivation. Returns true when a row updated.
    /// Skips rows where the user has manually overridden the playtime.
    pub fn set_imported_playtime_by_platform(
        &self,
        source: &str,
        platform_id: &str,
        seconds: i64,
    ) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE games SET imported_playtime_seconds = ?1
             WHERE source = ?2 AND platform_id = ?3
               AND COALESCE(imported_playtime_user_set, 0) = 0",
            params![seconds, source, platform_id],
        )?;
        Ok(n > 0)
    }

    /// Sum of every game's `imported_playtime_seconds`. Used by the global
    /// stats endpoint to fold platform-reported playtime (Steam / GOG)
    /// into the Tokoru-tracked session total.
    pub fn total_imported_playtime_seconds(&self) -> SqlResult<i64> {
        let conn = self.conn.lock().unwrap();
        let total: Option<i64> = conn
            .query_row(
                "SELECT COALESCE(SUM(imported_playtime_seconds), 0) FROM games",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(total.unwrap_or(0))
    }

    // ============ Achievements ============

    /// Persist the JSON-encoded achievements list for a game + bump the
    /// sync timestamp. Empty JSON `"[]"` is a legitimate stored value —
    /// means we asked Steam and the game genuinely has no achievements
    /// (so we don't re-poll).
    pub fn set_game_achievements(&self, id: &str, json: &str) -> SqlResult<()> {
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE games SET achievements = ?1, achievements_synced_at = ?2
             WHERE id = ?3",
            params![json, now, id],
        )?;
        Ok(())
    }

    /// Read the stored achievements JSON + synced_at timestamp for a
    /// game. None on either field when never synced. Used by the
    /// GameDetail panel + the freshness check before re-fetching.
    pub fn get_game_achievements_raw(
        &self,
        id: &str,
    ) -> SqlResult<(Option<String>, Option<i64>)> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT achievements, achievements_synced_at FROM games WHERE id = ?1",
            params![id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                ))
            },
        )
        .optional()
        .map(|opt| opt.unwrap_or((None, None)))
    }

    // ============ User flags (favorite, ...) ============

    /// Toggle the `favorite` flag on a game row. Returns the new state so
    /// the UI can update without a follow-up read. No-op + Ok(false) when
    /// the id doesn't exist.
    pub fn toggle_favorite(&self, id: &str) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        let current: Option<i64> = conn
            .query_row(
                "SELECT favorite FROM games WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(cur) = current else {
            return Ok(false);
        };
        let next = if cur == 0 { 1 } else { 0 };
        conn.execute(
            "UPDATE games SET favorite = ?1 WHERE id = ?2",
            params![next, id],
        )?;
        Ok(next == 1)
    }

    /// Force-set the favorite flag for a game. Distinct from
    /// `toggle_favorite` because the Steam-favorites importer needs to
    /// set absolutely (`favorite=1`), not flip — running the import
    /// twice should leave the same rows favorited, not un-favorite them.
    pub fn set_favorite(&self, id: &str, favorite: bool) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE games SET favorite = ?1 WHERE id = ?2",
            params![if favorite { 1 } else { 0 }, id],
        )?;
        Ok(())
    }

    /// Read the favorite flag for one game. None when the id doesn't
    /// exist; Some(false) when the row exists but isn't marked.
    pub fn get_favorite(&self, id: &str) -> SqlResult<Option<bool>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT favorite FROM games WHERE id = ?1",
            params![id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map(|opt| opt.map(|n| n != 0))
    }

    /// Return the ids of every favorited game. Used by the Library
    /// sidebar's "Favorites" filter — fast (no per-row IPC) and stays
    /// stable across renders since it's a flat Vec<String>.
    pub fn list_favorite_ids(&self) -> SqlResult<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id FROM games WHERE favorite = 1")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect()
    }

    /// IDs of every game played within the last `days` window — union
    /// of locally-tracked sessions (`playtime_sessions.started_at`) and
    /// the platform-imported `last_played_imported` (set by the Steam /
    /// GOG sync from their `rtime_last_played` equivalent). Without
    /// the union, games the user opened directly on Steam wouldn't
    /// show in the Library "Recently Played" filter.
    pub fn list_recently_played_ids(&self, days: i64) -> SqlResult<Vec<String>> {
        let cutoff = chrono::Utc::now().timestamp() - days * 86_400;
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT id FROM (
                 SELECT g.id FROM games g
                 WHERE COALESCE(g.last_played_imported, 0) >= ?1
                 UNION
                 SELECT game_id AS id FROM playtime_sessions
                 WHERE started_at >= ?1
             )",
        )?;
        let rows = stmt.query_map(params![cutoff], |row| row.get::<_, String>(0))?;
        rows.collect()
    }

    /// Set `last_played_imported` for a game looked up by its id —
    /// the by-platform variant doesn't work for local rows that have
    /// no `platform_id`.
    pub fn set_imported_last_played_by_id(&self, id: &str, ts: i64) -> SqlResult<()> {
        if ts <= 0 {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE games SET last_played_imported = ?1 WHERE id = ?2",
            params![ts, id],
        )?;
        Ok(())
    }

    /// Set the platform-imported last-played timestamp for a game
    /// (looked up by `(source, platform_id)`). Mirrors the pattern of
    /// `set_imported_playtime_by_platform`. Returns true when a row
    /// was updated. Treats `ts <= 0` as "never played" — leaves the
    /// existing value alone instead of overwriting with 0.
    pub fn set_imported_last_played_by_platform(
        &self,
        source: &str,
        platform_id: &str,
        ts: i64,
    ) -> SqlResult<bool> {
        if ts <= 0 {
            return Ok(false);
        }
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE games SET last_played_imported = ?1
             WHERE source = ?2 AND platform_id = ?3",
            params![ts, source, platform_id],
        )?;
        Ok(n > 0)
    }

    /// Per-game play stats — total seconds (local sessions +
    /// platform-imported) and most-recent play timestamp (MAX of local
    /// session `started_at` and the platform-imported `last_played`,
    /// or None when neither is set). Used by the Library sort
    /// (by playtime / last played). One SQL pass.
    pub fn list_game_play_stats(&self) -> SqlResult<Vec<(String, i64, Option<i64>)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT
                g.id,
                COALESCE(g.imported_playtime_seconds, 0)
                  + COALESCE((
                      SELECT SUM(duration_seconds) FROM playtime_sessions
                      WHERE game_id = g.id AND duration_seconds IS NOT NULL
                    ), 0) AS total_seconds,
                CASE
                    WHEN COALESCE(g.last_played_imported, 0) > 0
                         OR EXISTS (SELECT 1 FROM playtime_sessions WHERE game_id = g.id)
                    THEN MAX(
                        COALESCE(g.last_played_imported, 0),
                        COALESCE((SELECT MAX(started_at) FROM playtime_sessions WHERE game_id = g.id), 0)
                    )
                    ELSE NULL
                END AS last_played
             FROM games g",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })?;
        rows.collect()
    }

    /// IDs of every game with zero recorded playtime — neither a local
    /// session, an imported total from Steam/GOG, nor a recorded
    /// platform-side last-played. Powers the Library "Never Played"
    /// sidebar filter.
    pub fn list_never_played_ids(&self) -> SqlResult<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT g.id FROM games g
             WHERE COALESCE(g.imported_playtime_seconds, 0) = 0
               AND COALESCE(g.last_played_imported, 0) = 0
               AND NOT EXISTS (
                 SELECT 1 FROM playtime_sessions s
                 WHERE s.game_id = g.id AND COALESCE(s.duration_seconds, 0) > 0
               )",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect()
    }

    // ============ Metadata enrichment (Steam Store + SteamSpy) ============

    /// List every Steam-source row with its title + current
    /// `metadata_synced_at` so the metadata sync can decide which appids
    /// need a refresh. Returns `(game_id, appid, title, synced_at_or_zero)`
    /// tuples ordered by least-recent first so the sync naturally picks up
    /// stale rows before fresh ones. Title is included because HLTB
    /// searches by name (no appid mapping), and IGDB falls back to
    /// name-search when the Steam external ID lookup misses.
    pub fn list_steam_games_for_metadata(
        &self,
    ) -> SqlResult<Vec<(String, String, String, i64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, platform_id, title, COALESCE(metadata_synced_at, 0)
             FROM games
             WHERE source = 'steam' AND platform_id IS NOT NULL AND platform_id != ''
             ORDER BY COALESCE(metadata_synced_at, 0) ASC, title ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        rows.collect()
    }

    /// Fetch the multi-source metadata for a single game. All fields
    /// are nullable — populated progressively by `sync_metadata_now`,
    /// any of them can be NULL when a source didn't return data.
    /// Returns `(description, header_url, screenshots_json, developer,
    /// publisher, genres_json, categories_json, tags_json, franchise,
    /// igdb_id, themes_json, similar_games_json, hltb_main_hours,
    /// dlcs_json, metadata_synced_at)`.
    #[allow(clippy::type_complexity)]
    pub fn get_game_metadata(
        &self,
        id: &str,
    ) -> SqlResult<
        Option<(
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<f64>,
            Option<String>,
            Option<i64>,
        )>,
    > {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT description, header_url, screenshots, developer, publisher,
                    genres, categories, tags, franchise, igdb_id, themes,
                    similar_games, hltb_main_hours, dlcs, metadata_synced_at
             FROM games WHERE id = ?1",
            params![id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<f64>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<i64>>(14)?,
                ))
            },
        )
        .optional()
    }

    /// Return the franchise-resolution signals for every game in the DB.
    /// Used by the Franchise collections mode to feed a multi-pass grouping
    /// algorithm without having to widen the `Game` model with metadata
    /// columns that no other caller needs.
    ///
    /// `(game_id, source, platform_id, title, franchise, developer, tags_json)`
    /// — `franchise`, `developer` and `tags_json` are NULL when the row has
    /// never been enriched by the metadata sync.
    #[allow(clippy::type_complexity)]
    pub fn list_franchise_signals(
        &self,
    ) -> SqlResult<
        Vec<(
            String,
            String,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        )>,
    > {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, source, platform_id, title, franchise, developer, tags
             FROM games",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        rows.collect()
    }

    /// Merge enriched metadata into the games row. All fields are optional:
    /// passing `None` for a column keeps the existing value (COALESCE), so
    /// the same writer can absorb partial results from Steam Store and
    /// SteamSpy without ever clobbering one source with another's NULL.
    ///
    /// `metadata_synced_at` is set unconditionally to the current unix-
    /// seconds timestamp — the row was touched, regardless of which source
    /// returned what.
    #[allow(clippy::too_many_arguments)]
    /// Reset every game's `metadata_synced_at` to NULL so the next metadata
    /// sync treats them as stale and re-fetches their descriptions / tags
    /// in the currently configured Steam Store locale. Used by
    /// `set_app_locale` when the user picks a different UI language so the
    /// cached EN descriptions stop leaking through.
    pub fn invalidate_all_game_metadata(&self) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute("UPDATE games SET metadata_synced_at = NULL", [])
    }

    pub fn upsert_game_metadata(
        &self,
        id: &str,
        meta: GameMetadataUpdate<'_>,
    ) -> SqlResult<()> {
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE games SET
                description = COALESCE(?1, description),
                header_url = COALESCE(?2, header_url),
                screenshots = COALESCE(?3, screenshots),
                developer = COALESCE(?4, developer),
                publisher = COALESCE(?5, publisher),
                genres = COALESCE(?6, genres),
                categories = COALESCE(?7, categories),
                tags = COALESCE(?8, tags),
                franchise = COALESCE(?9, franchise),
                igdb_id = COALESCE(?10, igdb_id),
                themes = COALESCE(?11, themes),
                similar_games = COALESCE(?12, similar_games),
                hltb_main_hours = COALESCE(?13, hltb_main_hours),
                dlcs = COALESCE(?14, dlcs),
                metadata_synced_at = ?15
             WHERE id = ?16",
            params![
                meta.description,
                meta.header_url,
                meta.screenshots,
                meta.developer,
                meta.publisher,
                meta.genres,
                meta.categories,
                meta.tags,
                meta.franchise,
                meta.igdb_id,
                meta.themes,
                meta.similar_games,
                meta.hltb_main_hours,
                meta.dlcs,
                now,
                id,
            ],
        )?;
        Ok(())
    }

    // ============ Sync state (key/value) ============

    pub fn set_sync_state(&self, key: &str, value: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sync_state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_sync_state(&self, key: &str) -> SqlResult<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT value FROM sync_state WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
    }
}

// ============ Module-level helpers ============

/// Best-effort: pick the largest non-uninstaller .exe under `install_path`,
/// scanning up to 2 directory levels deep. Used by
/// `set_install_path_by_platform` to upgrade an owned-library placeholder
/// (launch URI) into a real on-disk binary so the playtime watcher can bind
/// to it. Returns None if the directory doesn't exist or contains no
/// plausible .exe yet — caller keeps the existing exe_path unchanged.
fn derive_main_exe(install_path: &str) -> Option<String> {
    let path = std::path::Path::new(install_path);
    if !path.exists() {
        return None;
    }
    let mut best: Option<(String, u64)> = None;
    for entry in walkdir::WalkDir::new(path).max_depth(2).into_iter().flatten() {
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

// ============ Row mappers ============

fn row_to_game(row: &rusqlite::Row) -> SqlResult<Game> {
    // `icon_url` is the 12th column appended by the get_all_games / get_game_by_id
    // SELECT lists. Optional so legacy SELECTs that haven't been updated to
    // fetch it gracefully return None instead of panicking on a missing column.
    let icon_url: Option<String> = row.get(11).ok().flatten();
    Ok(Game {
        id: row.get(0)?,
        title: row.get(1)?,
        source: row.get(2)?,
        install_path: row.get(3)?,
        exe_path: row.get(4)?,
        artwork_url: row.get(5)?,
        hero_url: row.get(6)?,
        logo_url: row.get(7)?,
        icon_url,
        platform_id: row.get(8)?,
        launch_command: row.get(9)?,
        last_scanned_at: row.get(10)?,
    })
}

fn row_to_shortcut(row: &rusqlite::Row) -> SqlResult<Shortcut> {
    let appid: i64 = row.get(1)?;
    let status_str: String = row.get(3)?;
    Ok(Shortcut {
        game_id: row.get(0)?,
        steam_appid: appid as u64,
        pushed_at: row.get(2)?,
        status: ShortcutStatus::from_str(&status_str),
    })
}

fn row_to_session(row: &rusqlite::Row) -> SqlResult<PlaytimeSession> {
    Ok(PlaytimeSession {
        id: row.get(0)?,
        game_id: row.get(1)?,
        started_at: row.get(2)?,
        ended_at: row.get(3)?,
        duration_seconds: row.get(4)?,
    })
}
