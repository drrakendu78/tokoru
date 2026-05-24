//! Steam shortcut + localconfig writers.
//!
//! - `shortcuts.vdf` (binary VDF, KeyValues1): list of non-Steam shortcuts that
//!   appear in the Steam library on next Steam launch.
//! - `localconfig.vdf` (text VDF): per-user app config including reported
//!   playtime for non-Steam shortcuts. Steam reads this at launch and writes
//!   it on shutdown — never touch while Steam is running.
//!
//! Both files live under `<SteamPath>/userdata/<steamUserId>/config/`. The
//! most-recently-used userdata subdir is picked by mtime; a multi-account
//! picker can be wired later from Settings if needed.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::models::Game;
use crate::services::db::{crc32, Database};
use crate::services::platforms::local_detect;

// ============ Steam user discovery ============

/// Public accessor for the Steam install root (used by the restart command).
pub fn find_steam_root() -> Option<PathBuf> {
    local_detect::get_steam_path()
}

/// Return `<SteamPath>/userdata/<mostRecentUserId>` or an error.
pub fn most_recent_userdata_dir() -> Result<PathBuf, String> {
    let steam = local_detect::get_steam_path().ok_or_else(|| {
        "Steam install path not found — install Steam or set the path in Settings".to_string()
    })?;
    let userdata = steam.join("userdata");
    if !userdata.exists() {
        return Err(format!(
            "Steam userdata directory not found at {}",
            userdata.display()
        ));
    }

    let mut candidates: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    for entry in fs::read_dir(&userdata).map_err(|e| e.to_string())?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // userdata subdirs are 32-bit Steam account IDs (numeric).
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name == "0" || name.is_empty() || !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        candidates.push((path, mtime));
    }

    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    candidates
        .into_iter()
        .next()
        .map(|(p, _)| p)
        .ok_or_else(|| "No Steam user found in userdata/ — sign in to Steam at least once".to_string())
}

/// `<SteamPath>/userdata/<id>/config/shortcuts.vdf`
pub fn shortcuts_vdf_path() -> Result<PathBuf, String> {
    Ok(most_recent_userdata_dir()?.join("config").join("shortcuts.vdf"))
}

/// `<SteamPath>/userdata/<id>/config/localconfig.vdf`
pub fn localconfig_vdf_path() -> Result<PathBuf, String> {
    Ok(most_recent_userdata_dir()?
        .join("config")
        .join("localconfig.vdf"))
}

/// `<SteamPath>/userdata/<id>/config/grid` — the folder Steam's library
/// reads for cover / hero / logo overrides. Created on demand if missing.
pub fn grid_dir() -> Result<PathBuf, String> {
    let dir = most_recent_userdata_dir()?.join("config").join("grid");
    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|e| {
            format!("Failed to create Steam grid directory {}: {}", dir.display(), e)
        })?;
    }
    Ok(dir)
}

/// Which slot to write to in Steam's grid folder. File naming follows
/// Steam's library conventions:
///   * `Cover` → `<appid>p.png/jpg`  (600x900 vertical capsule shown in grid view)
///   * `Hero`  → `<appid>_hero.png/jpg`  (1920x620 banner)
///   * `Logo`  → `<appid>_logo.png/jpg`  (transparent PNG over the hero)
///   * `Icon`  → `<appid>_icon.png/jpg`  (32px / 64px icon shown in the
///     friends list, taskbar tooltip, and the "currently playing" header)
#[derive(Debug, Clone, Copy)]
pub enum GridSlot {
    Cover,
    Hero,
    Logo,
    Icon,
}

impl GridSlot {
    fn suffix(&self) -> &'static str {
        match self {
            GridSlot::Cover => "p",
            GridSlot::Hero => "_hero",
            GridSlot::Logo => "_logo",
            GridSlot::Icon => "_icon",
        }
    }
}

/// Download `image_url` and drop it into Steam's grid folder for `appid`.
/// Overwrites any existing override for that slot. Extension is inferred
/// from the URL (defaults to `png`).
///
/// Used to mirror an artwork choice from Tokoru into Steam's own
/// library so the user sees the same cover in both UIs without having to
/// edit anything in Steam.
///
/// Optimization (SARM-style): when the image_url is a Steam-CDN URL
/// (`*.steamstatic.com` / `*.akamaihd.net`), Steam has already downloaded
/// that exact image into `appcache/librarycache/<appid>/...` for its own
/// library renderer. We `fs::copy` from there — zero bandwidth instead of
/// re-fetching the same bytes from the CDN.
///
/// IMPORTANT: this shortcut is conditional on the URL pointing at a
/// Steam-CDN host. SteamGridDB-sourced URLs (`cdn2.steamgriddb.com`) skip
/// the shortcut and always download — the appcache file is Steam's
/// static original, which would silently replace the user's SGDB animated
/// / NSFW pick. The host check guarantees we only `fs::copy` when the
/// bytes are guaranteed identical to what the URL would have returned.
pub async fn write_grid_image(appid: u32, slot: GridSlot, image_url: &str) -> Result<PathBuf, String> {
    let dir = grid_dir()?;
    // For ICON specifically Steam ONLY reads `.jpg` from the grid folder —
    // a `.png` named `<appid>_icon.png` is silently ignored. SARM hardcodes
    // `.jpg` for this slot for the same reason (cf. their `handle_changes.rs`
    // `get_grid_filename` "Icon" branch). Same SARM-style write-bytes-as-jpg
    // trick we already use for `.webp` sources.
    let ext = match slot {
        GridSlot::Icon => "jpg",
        _ => guess_image_ext(image_url),
    };

    // Steam's library reads whichever extension is present — if multiple
    // exist for the same slot, behaviour is undefined. Remove every
    // sibling so the new file definitely wins. Also wipes any stale .webp
    // from a previous Tokoru version (Steam ignored those silently,
    // which looked like "the mirror didn't fire" — see the SARM trick
    // in `guess_image_ext`).
    for sibling_ext in ["png", "jpg", "jpeg", "webp", "webm", "mp4"] {
        if sibling_ext == ext {
            continue;
        }
        let sibling = dir.join(format!("{}{}.{}", appid, slot.suffix(), sibling_ext));
        let _ = fs::remove_file(&sibling);
    }

    let dest = dir.join(format!("{}{}.{}", appid, slot.suffix(), ext));

    // Local-copy fast path: when the URL is a Steam-CDN host AND Steam
    // already has the matching file in its appcache, skip the download.
    if is_steam_cdn_url(image_url) {
        if let Some(local) = steam_appcache_path_for(appid, slot) {
            match fs::copy(&local, &dest) {
                Ok(bytes) => {
                    log::info!(
                        "[steam_writers] copied {} from Steam appcache ({} bytes, no download)",
                        dest.display(),
                        bytes
                    );
                    if matches!(slot, GridSlot::Icon) {
                        mirror_icon_to_appcache(appid, &dest);
                        update_shortcut_icon_field(appid, &dest);
                    }
                    return Ok(dest);
                }
                Err(e) => {
                    // Fall through to network download — appcache file might
                    // have been pruned mid-flight or perms issue.
                    log::warn!(
                        "[steam_writers] appcache copy failed ({}), falling back to download",
                        e
                    );
                }
            }
        }
    }

    let client = reqwest::Client::new();
    let bytes = client
        .get(image_url)
        .header("User-Agent", "Tokoru/0.1")
        .send()
        .await
        .map_err(|e| format!("download {} failed: {}", image_url, e))?
        .bytes()
        .await
        .map_err(|e| format!("read {} body failed: {}", image_url, e))?;
    fs::write(&dest, &bytes)
        .map_err(|e| format!("write {} failed: {}", dest.display(), e))?;
    log::info!("[steam_writers] wrote {} ({} bytes)", dest.display(), bytes.len());

    // Steam ignores the user-override grid file for the small Icon slot
    // for native Steam games (it reads from `appcache/librarycache/<appid>/
    // icon.jpg` for the friends list / "now playing" / sidebar). Mirror
    // the icon bytes there too so it actually shows. Best-effort — for
    // non-Steam shortcuts the per-appid librarycache folder doesn't
    // exist and the copy is silently skipped.
    //
    // For non-Steam shortcuts, the small icon comes from the `icon` field
    // in `shortcuts.vdf` (NOT from grid or librarycache). Wire that too —
    // SARM does the same in `handle_changes.rs::save_changes`. No-op for
    // native Steam appids (no matching shortcut entry).
    if matches!(slot, GridSlot::Icon) {
        mirror_icon_to_appcache(appid, &dest);
        update_shortcut_icon_field(appid, &dest);
    }

    Ok(dest)
}

/// For a non-Steam shortcut, update its `icon` field in `shortcuts.vdf`
/// to point to the freshly-written grid icon file. This is the
/// SARM-equivalent of `handle_changes.rs::save_changes`'s shortcut icon
/// pass — Steam reads the icon for shortcuts from THIS field (not from
/// the grid folder), so without this step the sidebar / friends list /
/// "now playing" all keep the placeholder.
///
/// No-op when no matching shortcut entry exists (e.g. the appid is a
/// native Steam appid, or the shortcut hasn't been pushed yet). Logs
/// success / failure but never errors — caller is best-effort.
fn update_shortcut_icon_field(appid: u32, icon_path: &std::path::Path) {
    let Ok(path) = shortcuts_vdf_path() else {
        log::debug!("[steam_writers] no shortcuts.vdf — skipping icon field update");
        return;
    };
    let mut entries = match read_shortcuts_vdf(&path) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("[steam_writers] read shortcuts.vdf for icon update failed: {}", e);
            return;
        }
    };
    let mut updated = false;
    for entry in entries.iter_mut() {
        if entry.appid == appid {
            entry.icon = icon_path.to_string_lossy().to_string();
            updated = true;
            break;
        }
    }
    if !updated {
        return;
    }
    if is_steam_running() {
        log::warn!(
            "shortcuts.vdf: updating icon field while steam.exe is running — Steam may overwrite this on shutdown"
        );
    }
    match write_shortcuts_vdf(&path, &entries) {
        Ok(()) => log::info!(
            "[steam_writers] updated shortcuts.vdf icon field for appid {} → {}",
            appid,
            icon_path.display()
        ),
        Err(e) => log::warn!(
            "[steam_writers] write shortcuts.vdf for icon update failed: {}",
            e
        ),
    }
}

/// Copy the freshly-written icon file into Steam's librarycache so the
/// small-icon surfaces (sidebar / friends list / "in game X") actually
/// pick up the new artwork. Native Steam games only — for shortcuts the
/// per-appid folder doesn't exist and we silently bail.
///
/// Writes to BOTH the modern (`<librarycache>/<appid>/icon.jpg`) and the
/// legacy (`<librarycache>/<appid>_icon.jpg`) locations because Steam has
/// been seen reading either depending on UI surface + client build.
/// Steam may refresh from its CDN on next launch and revert — that's
/// upstream behavior we can't prevent without injecting into the client.
fn mirror_icon_to_appcache(appid: u32, src: &std::path::Path) {
    let Some(steam_root) = local_detect::get_steam_path() else {
        return;
    };
    let cache_root = steam_root.join("appcache").join("librarycache");
    if !cache_root.exists() {
        return;
    }

    // Modern per-appid subfolder. Only write if the folder already
    // exists — creating one for a shortcut (which Steam doesn't know
    // about as a real appid) would just leave orphan files.
    let modern_dir = cache_root.join(appid.to_string());
    if modern_dir.is_dir() {
        // Remove sibling extensions first so our chosen one wins.
        for ext in ["jpg", "png"] {
            let _ = fs::remove_file(modern_dir.join(format!("icon.{}", ext)));
        }
        let modern_target = modern_dir.join("icon.jpg");
        match fs::copy(src, &modern_target) {
            Ok(n) => log::info!(
                "[steam_writers] mirrored icon to {} ({} bytes)",
                modern_target.display(),
                n
            ),
            Err(e) => log::warn!(
                "[steam_writers] icon mirror to {} failed: {}",
                modern_target.display(),
                e
            ),
        }
    }

    // Legacy flat layout — older Steam clients still hit this path.
    let legacy_target = cache_root.join(format!("{}_icon.jpg", appid));
    if cache_root.is_dir() {
        for ext in ["jpg", "png"] {
            let _ = fs::remove_file(cache_root.join(format!("{}_icon.{}", appid, ext)));
        }
        match fs::copy(src, &legacy_target) {
            Ok(n) => log::info!(
                "[steam_writers] mirrored icon to {} ({} bytes)",
                legacy_target.display(),
                n
            ),
            Err(e) => log::warn!(
                "[steam_writers] legacy icon mirror to {} failed: {}",
                legacy_target.display(),
                e
            ),
        }
    }
}

/// True when the URL points at a host Steam itself uses to serve store
/// artwork — meaning Steam has likely already cached that exact file in
/// `appcache/librarycache/`. The check is intentionally conservative:
/// any other host (SGDB, Epic CDN, GOG images, …) falls through to a
/// regular download because the appcache wouldn't have the right bytes.
fn is_steam_cdn_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("steamstatic.com")
        || lower.contains("akamaihd.net")
        || lower.contains("steamcdn-a.")
        || lower.contains("cdn.akamai.steamstatic")
}

/// Resolve the local Steam-appcache file for a given `(appid, slot)` —
/// the file Steam downloads itself to render the in-app library. Returns
/// `None` when Steam isn't installed, the per-appid folder hasn't been
/// populated yet (game owned but library not visited), or the specific
/// slot file is missing.
///
/// Modern Steam (post-2023) lays these out as
/// `appcache/librarycache/<appid>/library_600x900.jpg` (per-appid
/// subfolder). We also try the legacy flat layout
/// `appcache/librarycache/<appid>_library_600x900.jpg` as a fallback
/// for older installs that haven't been refreshed yet.
fn steam_appcache_path_for(appid: u32, slot: GridSlot) -> Option<PathBuf> {
    let steam_root = local_detect::get_steam_path()?;
    let cache_root = steam_root.join("appcache").join("librarycache");
    // Modern names Steam uses inside the per-appid subfolder. We try a
    // few extensions per slot because Steam serves jpg for covers/heroes
    // but png for logos, and occasionally the opposite.
    let (modern_names, legacy_names): (&[&str], &[&str]) = match slot {
        GridSlot::Cover => (
            &["library_600x900.jpg", "library_600x900.png"],
            &["library_600x900.jpg", "library_600x900.png"],
        ),
        GridSlot::Hero => (
            &["library_hero.jpg", "library_hero.png"],
            &["library_hero.jpg", "library_hero.png"],
        ),
        GridSlot::Logo => (
            &["logo.png", "library_logo.png"],
            &["logo.png", "library_logo.png"],
        ),
        // Steam ships per-game icons in librarycache as `<appid>_icon.jpg`
        // (or `.png`). The "modern" per-appid subfolder layout uses
        // `icon.jpg`. Same pattern as logo — try modern subfolder first,
        // legacy flat name second.
        GridSlot::Icon => (
            &["icon.jpg", "icon.png"],
            &["icon.jpg", "icon.png"],
        ),
    };
    // Try the modern per-appid subfolder first.
    let modern_dir = cache_root.join(appid.to_string());
    for name in modern_names {
        let candidate = modern_dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // Fallback: legacy flat layout `<appid>_<name>` directly under librarycache.
    for name in legacy_names {
        let candidate = cache_root.join(format!("{}_{}", appid, name));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn guess_image_ext(url: &str) -> &'static str {
    let lower = url.to_ascii_lowercase();
    let stripped = lower.split('?').next().unwrap_or("");
    if stripped.ends_with(".webm") {
        // Animated heroes from SteamGridDB. Steam renders <appid>_hero.webm
        // as a looping background on the library page.
        "webm"
    } else if stripped.ends_with(".mp4") {
        "mp4"
    } else if stripped.ends_with(".jpg") || stripped.ends_with(".jpeg") {
        "jpg"
    } else if stripped.ends_with(".webp") {
        // Steam's library renderer doesn't decode `.webp` files in the
        // grid folder — they're silently ignored. SARM (Steam Art
        // Manager) works around this by writing the webp BYTES with a
        // `.jpg` extension; Steam's libstb-style decoder sniffs the
        // magic bytes and renders the image anyway. Confirmed to work
        // for cover / hero / logo slots.
        "jpg"
    } else {
        // PNG is the safest default — Steam handles it for every slot.
        "png"
    }
}

// ============ Steam-shortcut appid math ============

/// CRC32(exe + name) | 0x80000000  → unsigned 32-bit appid stored in shortcuts.vdf.
pub fn compute_shortcut_appid(exe: &str, name: &str) -> u32 {
    let mut input = String::with_capacity(exe.len() + name.len());
    input.push_str(exe);
    input.push_str(name);
    crc32(input.as_bytes()) | 0x8000_0000
}

// ============ Binary VDF (KeyValues1) ============
//
// Types:
//   0x00 = object (nested) — children follow, then 0x08 terminator
//   0x01 = string (NUL-terminated)
//   0x02 = int32 (4 bytes little-endian)
//   0x08 = end-of-object marker
//
// Layout: <type byte><key NUL-terminated><value>
// For objects: <0x00><key NUL>...children...<0x08>
//
// Root file is one object named "shortcuts" containing indexed children "0",
// "1", "2", ... Each child is a shortcut entry.

#[derive(Debug, Clone)]
pub struct ShortcutEntry {
    pub appid: u32,
    pub app_name: String,
    pub exe: String,
    pub start_dir: String,
    pub icon: String,
    pub shortcut_path: String,
    pub launch_options: String,
    pub is_hidden: i32,
    pub allow_desktop_config: i32,
    pub allow_overlay: i32,
    pub open_vr: i32,
    pub devkit: i32,
    pub devkit_game_id: String,
    pub devkit_override_app_id: i32,
    pub last_play_time: i32,
    pub flatpak_app_id: String,
    /// We only round-trip an empty `tags` object for now.
    pub tags: Vec<String>,
}

impl ShortcutEntry {
    /// Default fields, given a computed appid + identifying strings.
    pub fn new(appid: u32, app_name: String, exe: String, start_dir: String) -> Self {
        Self {
            appid,
            app_name,
            exe,
            start_dir,
            icon: String::new(),
            shortcut_path: String::new(),
            launch_options: String::new(),
            is_hidden: 0,
            allow_desktop_config: 1,
            allow_overlay: 1,
            open_vr: 0,
            devkit: 0,
            devkit_game_id: String::new(),
            devkit_override_app_id: 0,
            last_play_time: 0,
            flatpak_app_id: String::new(),
            tags: Vec::new(),
        }
    }
}

// ---------- writer ----------

fn write_u8(buf: &mut Vec<u8>, b: u8) {
    buf.push(b);
}

fn write_cstr(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(s.as_bytes());
    buf.push(0);
}

fn write_string_field(buf: &mut Vec<u8>, key: &str, value: &str) {
    write_u8(buf, 0x01);
    write_cstr(buf, key);
    write_cstr(buf, value);
}

fn write_int_field(buf: &mut Vec<u8>, key: &str, value: i32) {
    write_u8(buf, 0x02);
    write_cstr(buf, key);
    buf.extend_from_slice(&value.to_le_bytes());
}

fn write_object_header(buf: &mut Vec<u8>, key: &str) {
    write_u8(buf, 0x00);
    write_cstr(buf, key);
}

fn write_object_end(buf: &mut Vec<u8>) {
    write_u8(buf, 0x08);
}

fn write_entry(buf: &mut Vec<u8>, index: usize, e: &ShortcutEntry) {
    write_object_header(buf, &index.to_string());

    // Steam stores appid as int32; reinterpret the u32 bit pattern.
    let appid_as_i32: i32 = e.appid as i32;
    write_int_field(buf, "appid", appid_as_i32);
    write_string_field(buf, "AppName", &e.app_name);
    write_string_field(buf, "Exe", &e.exe);
    write_string_field(buf, "StartDir", &e.start_dir);
    write_string_field(buf, "icon", &e.icon);
    write_string_field(buf, "ShortcutPath", &e.shortcut_path);
    write_string_field(buf, "LaunchOptions", &e.launch_options);
    write_int_field(buf, "IsHidden", e.is_hidden);
    write_int_field(buf, "AllowDesktopConfig", e.allow_desktop_config);
    write_int_field(buf, "AllowOverlay", e.allow_overlay);
    write_int_field(buf, "OpenVR", e.open_vr);
    write_int_field(buf, "Devkit", e.devkit);
    write_string_field(buf, "DevkitGameID", &e.devkit_game_id);
    write_int_field(buf, "DevkitOverrideAppID", e.devkit_override_app_id);
    write_int_field(buf, "LastPlayTime", e.last_play_time);
    write_string_field(buf, "FlatpakAppID", &e.flatpak_app_id);

    // tags object — empty container or indexed string children
    write_object_header(buf, "tags");
    for (i, tag) in e.tags.iter().enumerate() {
        write_string_field(buf, &i.to_string(), tag);
    }
    write_object_end(buf);

    // end of this shortcut entry
    write_object_end(buf);
}

pub fn write_shortcuts_vdf(path: &Path, entries: &[ShortcutEntry]) -> Result<(), String> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);

    write_object_header(&mut buf, "shortcuts");
    for (i, e) in entries.iter().enumerate() {
        write_entry(&mut buf, i, e);
    }
    write_object_end(&mut buf); // end "shortcuts"
    write_u8(&mut buf, 0x08); // outer end-of-file marker Steam expects

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut f = fs::File::create(path).map_err(|e| e.to_string())?;
    f.write_all(&buf).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------- reader ----------

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn read_u8(&mut self) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn peek_u8(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn read_cstr(&mut self) -> Option<String> {
        let start = self.pos;
        let end = self.data[start..].iter().position(|&b| b == 0)?;
        let s = String::from_utf8_lossy(&self.data[start..start + end]).to_string();
        self.pos = start + end + 1; // skip NUL
        Some(s)
    }

    fn read_i32(&mut self) -> Option<i32> {
        let slice = self.data.get(self.pos..self.pos + 4)?;
        let mut arr = [0u8; 4];
        arr.copy_from_slice(slice);
        self.pos += 4;
        Some(i32::from_le_bytes(arr))
    }
}

/// Parse one shortcut entry's key/value body (already past the entry's
/// `0x00 <index>` header). Returns the parsed entry and consumes through the
/// closing `0x08`.
fn parse_entry_body(c: &mut Cursor) -> Option<ShortcutEntry> {
    let mut entry = ShortcutEntry::new(0, String::new(), String::new(), String::new());

    while let Some(t) = c.read_u8() {
        if t == 0x08 {
            return Some(entry);
        }
        let key = c.read_cstr()?;
        match t {
            0x00 => {
                // Nested object — only `tags` is meaningful, drain its body.
                if key.eq_ignore_ascii_case("tags") {
                    loop {
                        let nt = c.read_u8()?;
                        if nt == 0x08 {
                            break;
                        }
                        let nk = c.read_cstr()?;
                        match nt {
                            0x01 => {
                                let v = c.read_cstr()?;
                                entry.tags.push(v);
                                let _ = nk;
                            }
                            0x02 => {
                                let _ = c.read_i32()?;
                            }
                            0x00 => {
                                // skip nested object (unlikely under tags)
                                let mut depth = 1;
                                while depth > 0 {
                                    let bt = c.read_u8()?;
                                    if bt == 0x08 {
                                        depth -= 1;
                                        continue;
                                    }
                                    let _ = c.read_cstr()?;
                                    match bt {
                                        0x00 => depth += 1,
                                        0x01 => {
                                            let _ = c.read_cstr()?;
                                        }
                                        0x02 => {
                                            let _ = c.read_i32()?;
                                        }
                                        _ => return None,
                                    }
                                }
                            }
                            _ => return None,
                        }
                    }
                } else {
                    // Unknown nested object — drain until matching 0x08.
                    let mut depth = 1;
                    while depth > 0 {
                        let bt = c.read_u8()?;
                        if bt == 0x08 {
                            depth -= 1;
                            continue;
                        }
                        let _ = c.read_cstr()?;
                        match bt {
                            0x00 => depth += 1,
                            0x01 => {
                                let _ = c.read_cstr()?;
                            }
                            0x02 => {
                                let _ = c.read_i32()?;
                            }
                            _ => return None,
                        }
                    }
                }
            }
            0x01 => {
                let value = c.read_cstr()?;
                match key.as_str() {
                    "AppName" | "appname" => entry.app_name = value,
                    "Exe" | "exe" => entry.exe = value,
                    "StartDir" => entry.start_dir = value,
                    "icon" => entry.icon = value,
                    "ShortcutPath" => entry.shortcut_path = value,
                    "LaunchOptions" => entry.launch_options = value,
                    "DevkitGameID" => entry.devkit_game_id = value,
                    "FlatpakAppID" => entry.flatpak_app_id = value,
                    _ => {}
                }
            }
            0x02 => {
                let value = c.read_i32()?;
                match key.as_str() {
                    "appid" => entry.appid = value as u32,
                    "IsHidden" => entry.is_hidden = value,
                    "AllowDesktopConfig" => entry.allow_desktop_config = value,
                    "AllowOverlay" => entry.allow_overlay = value,
                    "OpenVR" => entry.open_vr = value,
                    "Devkit" => entry.devkit = value,
                    "DevkitOverrideAppID" => entry.devkit_override_app_id = value,
                    "LastPlayTime" => entry.last_play_time = value,
                    _ => {}
                }
            }
            _ => return None,
        }
    }
    Some(entry)
}

pub fn read_shortcuts_vdf(path: &Path) -> Result<Vec<ShortcutEntry>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    f.read_to_end(&mut data).map_err(|e| e.to_string())?;
    if data.is_empty() {
        return Ok(Vec::new());
    }

    let mut c = Cursor { data: &data, pos: 0 };

    // Outer container: 0x00 "shortcuts" then a stream of entries.
    let t = c.read_u8().ok_or_else(|| "empty file".to_string())?;
    if t != 0x00 {
        return Err(format!("unexpected first byte 0x{:02x}", t));
    }
    let root_key = c.read_cstr().ok_or_else(|| "missing root key".to_string())?;
    if !root_key.eq_ignore_ascii_case("shortcuts") {
        return Err(format!("unexpected root key {}", root_key));
    }

    let mut entries = Vec::new();
    while let Some(b) = c.peek_u8() {
        if b == 0x08 {
            // closing the "shortcuts" root
            let _ = c.read_u8();
            break;
        }
        let t = c.read_u8().unwrap();
        if t != 0x00 {
            return Err(format!("unexpected entry type byte 0x{:02x}", t));
        }
        // index key — ignored, entries are positional
        let _ = c.read_cstr().ok_or_else(|| "missing entry index".to_string())?;
        if let Some(entry) = parse_entry_body(&mut c) {
            entries.push(entry);
        } else {
            return Err("malformed shortcut entry".to_string());
        }
    }

    Ok(entries)
}

// ============ Public push / remove ============

/// True when ANY Steam process (steam.exe OR its steamwebhelper.exe
/// children) is alive. The webhelpers are what own the Chromium leveldb
/// at `htmlcache/Default/Local Storage/leveldb` — killing only the parent
/// `steam.exe` leaves them as orphans holding the DB lock, which is
/// invisible to a naive "steam.exe" check.
pub fn is_steam_running() -> bool {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_processes();
    sys.processes().values().any(|p| {
        let n = p.name().to_ascii_lowercase();
        n == "steam.exe"
            || n == "steam"
            || n == "steamwebhelper.exe"
            || n == "steamwebhelper"
    })
}

/// Best-effort kill of every lingering `steamwebhelper.exe` after a
/// `steam -shutdown`. Steam's graceful shutdown sometimes leaves these
/// orphans alive, which keeps the leveldb locked and silently breaks any
/// Collections write that follows.
pub fn kill_orphan_steamwebhelpers() {
    use sysinfo::{Pid, System};
    let mut sys = System::new();
    sys.refresh_processes();
    let pids: Vec<Pid> = sys
        .processes()
        .iter()
        .filter_map(|(pid, p)| {
            let n = p.name().to_ascii_lowercase();
            if n == "steamwebhelper.exe" || n == "steamwebhelper" {
                Some(*pid)
            } else {
                None
            }
        })
        .collect();
    if pids.is_empty() {
        return;
    }
    log::info!("[steam_writers] killing {} orphan steamwebhelper.exe", pids.len());
    for pid in pids {
        if let Some(p) = sys.process(pid) {
            p.kill();
        }
    }
}

/// Push a Tokoru game into shortcuts.vdf. Returns the resolved appid.
pub fn push_game(game: &Game, existing_appid: Option<u32>) -> Result<u32, String> {
    let path = shortcuts_vdf_path()?;
    let mut entries = read_shortcuts_vdf(&path).unwrap_or_default();

    let (exe, start_dir) = exe_and_start_dir(game);
    let name = game.title.clone();
    // Stable-id policy: if we've already pushed this game, reuse the
    // appid the caller carries forward from the DB. Lets the user
    // rename the row (custom_title) without breaking the link to the
    // shortcuts.vdf entry Steam already knows about.
    let appid = existing_appid.unwrap_or_else(|| compute_shortcut_appid(&exe, &name));
    let category_tag = category_tag_for_source(&game.source);

    // Match by computed appid first, then by (Exe, AppName) — handles imports
    // that don't yet carry our appid bit pattern.
    let mut found = false;
    for e in entries.iter_mut() {
        let is_match = e.appid == appid
            || (e.exe.eq_ignore_ascii_case(&exe) && e.app_name.eq_ignore_ascii_case(&name));
        if is_match {
            e.appid = appid;
            e.app_name = name.clone();
            e.exe = exe.clone();
            e.start_dir = start_dir.clone();
            apply_category_tag(&mut e.tags, &category_tag);
            found = true;
            break;
        }
    }

    if !found {
        let mut entry = ShortcutEntry::new(appid, name, exe, start_dir);
        apply_category_tag(&mut entry.tags, &category_tag);
        entries.push(entry);
    }

    if is_steam_running() {
        log::warn!(
            "shortcuts.vdf: writing while steam.exe is running — Steam may overwrite this on shutdown"
        );
    }

    write_shortcuts_vdf(&path, &entries)?;
    Ok(appid)
}

/// Remove a Tokoru game from shortcuts.vdf. Matches by computed appid.
pub fn remove_game(game: &Game) -> Result<(), String> {
    let path = shortcuts_vdf_path()?;
    let mut entries = read_shortcuts_vdf(&path).unwrap_or_default();
    let (exe, _start_dir) = exe_and_start_dir(game);
    let appid = compute_shortcut_appid(&exe, &game.title);

    let before = entries.len();
    entries.retain(|e| {
        e.appid != appid
            && !(e.exe.eq_ignore_ascii_case(&exe) && e.app_name.eq_ignore_ascii_case(&game.title))
    });
    if entries.len() == before {
        return Ok(()); // not present, no-op
    }

    if is_steam_running() {
        log::warn!(
            "shortcuts.vdf: removing while steam.exe is running — Steam may overwrite this on shutdown"
        );
    }

    write_shortcuts_vdf(&path, &entries)?;
    Ok(())
}

/// Per-source human-readable label used as a Steam Collection tag. Steam
/// surfaces shortcut `tags` as "Collections" in the library UI, so every
/// pushed shortcut gets one collection tag matching its source platform
/// (Epic Games, GOG Galaxy, Ubisoft Connect, ...). That's the
/// "category par plateforme" behavior — Boilr does the same thing.
pub fn category_tag_for_source(source: &str) -> String {
    match source {
        "epic" => "Epic Games".to_string(),
        "gog" => "GOG Galaxy".to_string(),
        "ubi" => "Ubisoft Connect".to_string(),
        "ea" => "EA App".to_string(),
        "itch" => "Itch.io".to_string(),
        "xbox" => "Xbox".to_string(),
        "amazon" => "Amazon Games".to_string(),
        "custom" => "Custom".to_string(),
        "local" => "Local".to_string(),
        "steam" => "Steam".to_string(), // shouldn't happen — Steam games aren't pushed
        other => other.to_string(),
    }
}

/// Every label `category_tag_for_source` can produce. Used by
/// `apply_category_tag` to recognize tags WE put on a shortcut so we can
/// replace them without nuking user-added tags. Kept in sync by hand —
/// if you add a source here, add it there too.
const SOURCE_TAGS: &[&str] = &[
    "Epic Games",
    "GOG Galaxy",
    "Ubisoft Connect",
    "EA App",
    "Itch.io",
    // Legacy — Heroic is no longer a supported source, but kept here so a
    // re-push strips the old tag from shortcuts written by earlier versions.
    "Heroic",
    "Xbox",
    "Amazon Games",
    "Custom",
    "Local",
    "Steam",
    // Legacy: prior versions of Tokoru wrote prefixed tags like
    // "Tokoru · GOG Galaxy" — strip those on the next push too so
    // users upgrading don't end up with duplicate tags on their
    // shortcuts. The legacy prefix is unique enough that we can detect
    // it with a starts_with check in the caller.
];

/// Insert / refresh the Tokoru source tag on a shortcut without nuking
/// any other Collection tags the user added manually (e.g. "Favorites").
fn apply_category_tag(tags: &mut Vec<String>, new_tag: &str) {
    // Idempotent across re-pushes: drop any tag that's a Tokoru-managed
    // platform label (including the legacy "Tokoru · …" prefix) before
    // re-adding the current one.
    tags.retain(|t| !SOURCE_TAGS.contains(&t.as_str()) && !t.starts_with("Tokoru · "));
    tags.push(new_tag.to_string());
}

/// Steam quotes the exe path and the StartDir; recreate Valve's convention so
/// the file looks like the ones Steam itself writes.
fn exe_and_start_dir(game: &Game) -> (String, String) {
    let raw_exe = game.exe_path.trim().to_string();
    let dir = game
        .install_path
        .clone()
        .or_else(|| {
            Path::new(&raw_exe)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
        })
        .unwrap_or_default();
    let quoted_exe = format!("\"{}\"", raw_exe);
    let quoted_dir = format!("\"{}\"", dir);
    (quoted_exe, quoted_dir)
}

// ============ localconfig.vdf (text VDF) ============

#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncResult {
    pub updated_count: u32,
    pub steam_running: bool,
}

/// Per-app values we want to write under
/// `UserLocalConfigStore -> Software -> Valve -> Steam -> apps -> <appid>`.
struct AppSync {
    appid: u32,
    playtime_minutes: u64,
    last_played: i64,
}

/// Sync our computed per-shortcut playtime back into Steam's per-user
/// `localconfig.vdf`. Bails when Steam is running.
pub fn sync_playtime_to_steam(db: &Database) -> Result<SyncResult, String> {
    if is_steam_running() {
        return Err(
            "Steam is currently running — close Steam before syncing playtime to localconfig.vdf"
                .to_string(),
        );
    }

    let shortcuts = db.get_all_shortcuts().map_err(|e| e.to_string())?;
    if shortcuts.is_empty() {
        return Ok(SyncResult {
            updated_count: 0,
            steam_running: false,
        });
    }

    // Only sync `pushed` shortcuts.
    let mut targets: Vec<AppSync> = Vec::new();
    for s in &shortcuts {
        if s.status != crate::models::ShortcutStatus::Pushed {
            continue;
        }
        let total = db
            .total_playtime_seconds(&s.game_id)
            .map_err(|e| e.to_string())?;
        if total <= 0 {
            continue;
        }
        let sessions = db
            .get_playtime_sessions(&s.game_id)
            .map_err(|e| e.to_string())?;
        let last_end = sessions
            .iter()
            .filter_map(|x| x.ended_at)
            .max()
            .unwrap_or(0);
        targets.push(AppSync {
            appid: s.steam_appid as u32,
            playtime_minutes: (total as u64) / 60,
            last_played: last_end,
        });
    }

    if targets.is_empty() {
        return Ok(SyncResult {
            updated_count: 0,
            steam_running: false,
        });
    }

    let path = localconfig_vdf_path()?;
    let original = if path.exists() {
        fs::read_to_string(&path).map_err(|e| e.to_string())?
    } else {
        return Err(format!(
            "localconfig.vdf not found at {} — launch Steam at least once first",
            path.display()
        ));
    };

    let updated_text = apply_playtime_updates(&original, &targets)?;
    let updated_count = targets.len() as u32;

    // Atomic-ish replace: backup, then write.
    let backup = path.with_extension("vdf.steamshelf.bak");
    let _ = fs::copy(&path, &backup);
    let mut f = fs::File::create(&path).map_err(|e| e.to_string())?;
    f.write_all(updated_text.as_bytes()).map_err(|e| e.to_string())?;

    Ok(SyncResult {
        updated_count,
        steam_running: false,
    })
}

/// Inject or update per-appid `Playtime` + `LastPlayed` entries inside the
/// `UserLocalConfigStore.Software.Valve.Steam.apps` block of a text VDF.
///
/// This is a structural string-edit that preserves Steam's formatting for
/// every other entry. We deliberately don't reformat the whole file.
fn apply_playtime_updates(
    original: &str,
    targets: &[AppSync],
) -> Result<String, String> {
    // Locate the `"apps"` block. Track only the one under Valve.Steam.
    let apps_start = find_apps_block(original)?;
    let block_open = find_next_open_brace(original, apps_start)
        .ok_or_else(|| "could not find opening brace of apps block".to_string())?;
    let block_close = find_matching_close(original, block_open)
        .ok_or_else(|| "could not find closing brace of apps block".to_string())?;

    let inner = &original[block_open + 1..block_close];
    // Steam Beta (2026+) writes non-Steam shortcut playtime under the
    // appid serialized as a SIGNED 32-bit integer, while older Steam
    // and our previous writes used the unsigned form. The two
    // representations describe the same appid bits — but VDF parsers
    // treat the strings as distinct keys, so we have to match both
    // when looking up an existing entry and prefer the signed form
    // when writing so Steam's native tracker picks up our value.
    let mut by_appid: HashMap<u32, (usize, usize)> = HashMap::new();
    // Collect existing `"<appid>" { ... }` ranges within the apps block.
    let mut p = 0usize;
    let bytes = inner.as_bytes();
    while p < bytes.len() {
        // skip whitespace
        while p < bytes.len() && (bytes[p] as char).is_whitespace() {
            p += 1;
        }
        if p >= bytes.len() {
            break;
        }
        if bytes[p] != b'"' {
            // Skip unknown line
            while p < bytes.len() && bytes[p] != b'\n' {
                p += 1;
            }
            continue;
        }
        // read quoted appid
        let key_start = p + 1;
        let mut q = key_start;
        while q < bytes.len() && bytes[q] != b'"' {
            q += 1;
        }
        if q >= bytes.len() {
            break;
        }
        let key = &inner[key_start..q];
        p = q + 1;
        // skip whitespace + optional opening brace OR scalar value
        while p < bytes.len() && (bytes[p] as char).is_whitespace() {
            p += 1;
        }
        if p < bytes.len() && bytes[p] == b'{' {
            let open = p;
            let close = find_matching_close(inner, open)
                .ok_or_else(|| format!("unbalanced braces near appid {}", key))?;
            // Accept both signed and unsigned 32-bit forms — they
            // re-cast to the same u32 bit pattern.
            let parsed = key
                .parse::<u32>()
                .ok()
                .or_else(|| key.parse::<i32>().ok().map(|v| v as u32));
            if let Some(id) = parsed {
                by_appid.insert(id, (open, close));
            }
            p = close + 1;
        } else if p < bytes.len() && bytes[p] == b'"' {
            // scalar value — uncommon under apps but tolerated
            let v_start = p + 1;
            let mut r = v_start;
            while r < bytes.len() && bytes[r] != b'"' {
                r += 1;
            }
            p = r + 1;
        } else {
            p += 1;
        }
    }

    // We'll rebuild the apps block by splicing per-target replacements / appends.
    let mut new_inner = inner.to_string();
    // Apply replacements in reverse byte order so earlier ranges stay valid.
    let mut ordered: Vec<(u32, (usize, usize), bool)> = Vec::new();
    for t in targets {
        if let Some(range) = by_appid.get(&t.appid) {
            ordered.push((t.appid, *range, true));
        }
    }
    ordered.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));

    for (appid, (open, close), _exists) in &ordered {
        let target = targets.iter().find(|t| t.appid == *appid).unwrap();
        let body = &new_inner[*open + 1..*close];
        let updated_body = update_or_insert_kv(
            body,
            &[
                ("Playtime", target.playtime_minutes.to_string()),
                ("LastPlayed", target.last_played.to_string()),
            ],
        );
        let mut rebuilt = String::with_capacity(new_inner.len());
        rebuilt.push_str(&new_inner[..*open + 1]);
        rebuilt.push_str(&updated_body);
        rebuilt.push_str(&new_inner[*close..]);
        new_inner = rebuilt;
    }

    // Append any new appids that weren't already present. Steam Beta
    // expects the SIGNED i32 form for non-Steam shortcut entries, so we
    // serialize the key that way (e.g. 2776720408 → "-1518246888").
    // Older Steam clients accept the signed form too — the underlying
    // u32 bit pattern is identical.
    let mut appended = String::new();
    for t in targets {
        if by_appid.contains_key(&t.appid) {
            continue;
        }
        let signed_key = (t.appid as i32).to_string();
        appended.push_str(&format!(
            "\n\t\t\t\t\t\"{}\"\n\t\t\t\t\t{{\n\t\t\t\t\t\t\"Playtime2wks\"\t\t\"{}\"\n\t\t\t\t\t\t\"Playtime\"\t\t\"{}\"\n\t\t\t\t\t\t\"LastPlayed\"\t\t\"{}\"\n\t\t\t\t\t}}",
            signed_key, t.playtime_minutes, t.playtime_minutes, t.last_played
        ));
    }
    if !appended.is_empty() {
        // Place before the trailing whitespace/newlines of the block.
        let trimmed = new_inner.trim_end_matches(|c: char| c == ' ' || c == '\t');
        let trail_start = trimmed.len();
        let trail = &new_inner[trail_start..];
        let mut rebuilt = String::with_capacity(new_inner.len() + appended.len());
        rebuilt.push_str(&new_inner[..trail_start]);
        rebuilt.push_str(&appended);
        rebuilt.push('\n');
        rebuilt.push_str(trail);
        new_inner = rebuilt;
    }

    // Splice the new inner back into the original.
    let mut out = String::with_capacity(original.len() + appended.len());
    out.push_str(&original[..block_open + 1]);
    out.push_str(&new_inner);
    out.push_str(&original[block_close..]);
    Ok(out)
}

/// Update existing scalar keys in-place inside a text-VDF object body,
/// or insert missing ones at the end of the body. Keys are matched
/// case-insensitively.
fn update_or_insert_kv(body: &str, kvs: &[(&str, String)]) -> String {
    let mut out = body.to_string();
    for (key, value) in kvs {
        let lowered = out.to_ascii_lowercase();
        let needle = format!("\"{}\"", key.to_ascii_lowercase());
        if let Some(pos) = lowered.find(&needle) {
            // Skip past the key, find the next quoted value
            let after_key = pos + needle.len();
            let bytes = out.as_bytes();
            let mut q = after_key;
            while q < bytes.len() && bytes[q] != b'"' {
                q += 1;
            }
            if q >= bytes.len() {
                continue;
            }
            let v_start = q + 1;
            let mut r = v_start;
            while r < bytes.len() && bytes[r] != b'"' {
                r += 1;
            }
            if r >= bytes.len() {
                continue;
            }
            let mut rebuilt = String::with_capacity(out.len());
            rebuilt.push_str(&out[..v_start]);
            rebuilt.push_str(value);
            rebuilt.push_str(&out[r..]);
            out = rebuilt;
        } else {
            // Append at end of body, indented.
            let trimmed = out.trim_end_matches(|c: char| c == ' ' || c == '\t' || c == '\n');
            let trailing = &out[trimmed.len()..];
            let mut rebuilt = String::with_capacity(out.len() + 80);
            rebuilt.push_str(&out[..trimmed.len()]);
            rebuilt.push('\n');
            rebuilt.push_str("\t\t\t\t\t\t\"");
            rebuilt.push_str(key);
            rebuilt.push_str("\"\t\t\"");
            rebuilt.push_str(value);
            rebuilt.push('"');
            rebuilt.push_str(trailing);
            out = rebuilt;
        }
    }
    out
}

/// Locate the `"apps"` block under `UserLocalConfigStore -> Software -> Valve -> Steam`.
fn find_apps_block(text: &str) -> Result<usize, String> {
    // Walk the chain of nested objects. Each find_keyed_block returns the
    // index of the opening `"<key>"` so the next pass can scope its search
    // inside that block.
    let user = find_keyed(text, 0, "UserLocalConfigStore")
        .ok_or_else(|| "no UserLocalConfigStore block".to_string())?;
    let user_open = find_next_open_brace(text, user)
        .ok_or_else(|| "UserLocalConfigStore missing opening brace".to_string())?;
    let user_close = find_matching_close(text, user_open)
        .ok_or_else(|| "UserLocalConfigStore missing closing brace".to_string())?;

    let software = find_keyed(&text[..user_close], user_open, "Software")
        .ok_or_else(|| "no Software block under UserLocalConfigStore".to_string())?;
    let software_open = find_next_open_brace(text, software)
        .ok_or_else(|| "Software missing opening brace".to_string())?;
    let software_close = find_matching_close(text, software_open)
        .ok_or_else(|| "Software missing closing brace".to_string())?;

    let valve = find_keyed(&text[..software_close], software_open, "Valve")
        .ok_or_else(|| "no Valve block under Software".to_string())?;
    let valve_open = find_next_open_brace(text, valve)
        .ok_or_else(|| "Valve missing opening brace".to_string())?;
    let valve_close = find_matching_close(text, valve_open)
        .ok_or_else(|| "Valve missing closing brace".to_string())?;

    let steam = find_keyed(&text[..valve_close], valve_open, "Steam")
        .ok_or_else(|| "no Steam block under Valve".to_string())?;
    let steam_open = find_next_open_brace(text, steam)
        .ok_or_else(|| "Steam missing opening brace".to_string())?;
    let steam_close = find_matching_close(text, steam_open)
        .ok_or_else(|| "Steam missing closing brace".to_string())?;

    let apps = find_keyed(&text[..steam_close], steam_open, "apps")
        .ok_or_else(|| "no apps block under Steam".to_string())?;
    Ok(apps)
}

/// Find a `"<key>"` token at-or-after `from`, case-insensitive.
fn find_keyed(text: &str, from: usize, key: &str) -> Option<usize> {
    let needle = format!("\"{}\"", key.to_ascii_lowercase());
    let lowered = text[from..].to_ascii_lowercase();
    lowered.find(&needle).map(|i| from + i)
}

fn find_next_open_brace(text: &str, from: usize) -> Option<usize> {
    text[from..].find('{').map(|i| from + i)
}

fn find_matching_close(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0;
    let mut in_string = false;
    let mut i = open;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
            in_string = !in_string;
        } else if !in_string {
            if c == b'{' {
                depth += 1;
            } else if c == b'}' {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

// ============ Tests ============

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appid_has_high_bit_set() {
        let id = compute_shortcut_appid("\"C:\\game.exe\"", "Game");
        assert!(id & 0x8000_0000 != 0);
    }

    #[test]
    fn shortcuts_vdf_roundtrip() {
        let dir = std::env::temp_dir().join("steamshelf-test-shortcuts");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("shortcuts.vdf");
        let entries = vec![ShortcutEntry::new(
            0x9001_0203,
            "Hades II".to_string(),
            "\"C:\\Games\\HadesII.exe\"".to_string(),
            "\"C:\\Games\"".to_string(),
        )];
        write_shortcuts_vdf(&path, &entries).unwrap();
        let parsed = read_shortcuts_vdf(&path).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].appid, 0x9001_0203);
        assert_eq!(parsed[0].app_name, "Hades II");
        assert_eq!(parsed[0].exe, "\"C:\\Games\\HadesII.exe\"");
        assert_eq!(parsed[0].allow_overlay, 1);
        assert_eq!(parsed[0].is_hidden, 0);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn apply_playtime_updates_inserts_new_appid() {
        let original = r#""UserLocalConfigStore"
{
	"Software"
	{
		"Valve"
		{
			"Steam"
			{
				"apps"
				{
					"440"
					{
						"LastPlayed"		"1700000000"
						"Playtime"		"42"
					}
				}
			}
		}
	}
}
"#;
        let targets = vec![
            AppSync {
                appid: 440,
                playtime_minutes: 99,
                last_played: 1_700_001_000,
            },
            AppSync {
                appid: 0x9001_0203,
                playtime_minutes: 12,
                last_played: 1_700_002_000,
            },
        ];
        let out = apply_playtime_updates(original, &targets).unwrap();
        assert!(out.contains("\"99\""), "Playtime for 440 should be 99 — got: {}", out);
        assert!(out.contains(&0x9001_0203u32.to_string()));
        assert!(out.contains("\"12\""));
    }
}
