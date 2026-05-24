//! Real Steam Collections via the per-user `cloudstorage` JSON files.
//!
//! Background — Steam used to store library "Collections" (the sidebar
//! groups) inside an embedded Chromium LevelDB at
//! `htmlcache/Default/Local Storage/leveldb`. As of a 2025/2026 Steam
//! client update, Collections migrated OUT of LevelDB into a per-user
//! folder of plain JSON files:
//!
//!   `<SteamPath>/userdata/<accountid>/config/cloudstorage/`
//!     ├── cloud-storage-namespaces.json          (list of `[id, version]`)
//!     ├── cloud-storage-namespace-1.json         (the actual collections)
//!     ├── cloud-storage-namespace-1.modified.json
//!     └── cloud-storage-namespace-<N>.json       (other namespaces)
//!
//! The schema inside each `cloud-storage-namespace-<N>.json` is the SAME
//! `Vec<(String, SteamCollection)>` BoilR's old LevelDB writer used —
//! Steam just dropped the Chromium wrapper. That makes our job easier:
//! plain file I/O, no leveldb dependency, no .log replay issues.
//!
//! Layout of one collection inside the array:
//!   ["user-collections.Tokoru-<base64name>", {
//!     "key": "user-collections.Tokoru-<base64name>",
//!     "timestamp": <unix>,
//!     "value": "{\"id\":\"Tokoru-...\",\"name\":\"...\",\"added\":[id...],\"removed\":[]}",
//!     "conflictResolutionMethod": "custom",
//!     "strMethodId": "union-collections"
//!   }]
//!
//! Hat tip to lovemonkeyz on BoilR/PhilipK/BoilR#380 (Mar 2026) for
//! pin-pointing the migration.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::services::steam_writers;

/// Prefix we own — any collection whose key starts with this is treated as
/// Tokoru-managed and replaced on every write. Anything else (the
/// user's hand-crafted collections, Steam's built-in `hidden`, etc.) is
/// preserved untouched.
pub const STEAMSHELF_TAG: &str = "Tokoru";

/// Legacy prefix from the pre-rename era (when the app was still called
/// "SteamShelf"). Collections written under this tag by older builds are
/// still recognized as ours so they get dropped on the next write —
/// otherwise the user ends up with 80+ orphan SteamShelf-* collections
/// AND a fresh set of Tokoru-* duplicates next to them.
const LEGACY_TAG: &str = "SteamShelf";

/// One source-grouped collection to push into Steam.
///
/// `name` is the human-readable label the sidebar will display (e.g.
/// "GOG Galaxy"). Recognition of Tokoru-managed collections happens
/// via the JSON KEY (which always embeds `STEAMSHELF_TAG` regardless of
/// what the display name is — see `name_to_key`), so the visible name is
/// free to be just the platform label without any prefix.
///
/// `appids` are the 32-bit Steam shortcut appids (as computed by
/// `compute_shortcut_appid`). Steam needs them as signed-ish JSON numbers —
/// we store them as `i64` so the high-bit-set u32s serialize positive.
#[derive(Debug, Clone)]
pub struct Collection {
    pub name: String,
    pub appids: Vec<i64>,
}

// ============ Public errors ============

#[derive(Debug)]
pub enum CollectionsError {
    /// Steam (or steamwebhelper) is running — refuse to write.
    SteamRunning,
    /// Neither candidate cloudstorage path was found.
    DbNotFound,
    /// File I/O / JSON parse failure.
    Other(String),
}

impl std::fmt::Display for CollectionsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CollectionsError::SteamRunning => write!(
                f,
                "Steam is running — close Steam first to update Collections."
            ),
            CollectionsError::DbNotFound => write!(
                f,
                "Steam Collections folder not found — sign in to Steam at least once and create / open a Collection, then retry."
            ),
            CollectionsError::Other(s) => write!(f, "{}", s),
        }
    }
}

impl From<CollectionsError> for String {
    fn from(e: CollectionsError) -> Self {
        e.to_string()
    }
}

// ============ Wire format ============

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
enum SteamCollection {
    Actual(ActualSteamCollection),
    Deleted(DeletedCollection),
}

impl SteamCollection {
    fn is_steamshelf(&self) -> bool {
        match self {
            SteamCollection::Actual(a) => a.is_steamshelf(),
            SteamCollection::Deleted(_) => false,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct ActualSteamCollection {
    key: String,
    timestamp: u64,
    /// JSON-encoded `ValueCollection` (string-of-JSON, like Steam does).
    value: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "conflictResolutionMethod"
    )]
    conflict_resolution_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strMethodId")]
    str_method_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

impl ActualSteamCollection {
    fn new(name: &str, ids: &[i64]) -> Self {
        let key = format!("user-collections.{}", name_to_key(name));
        let value = serialize_value_collection(name, ids);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        ActualSteamCollection {
            key,
            timestamp,
            value,
            conflict_resolution_method: Some("custom".to_string()),
            str_method_id: Some("union-collections".to_string()),
            version: None,
        }
    }

    fn is_steamshelf(&self) -> bool {
        self.key
            .contains(&format!("user-collections.{}", STEAMSHELF_TAG))
            || self
                .key
                .contains(&format!("user-collections.{}", LEGACY_TAG))
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct DeletedCollection {
    key: String,
    timestamp: u64,
    is_deleted: bool,
    version: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct ValueCollection {
    id: String,
    name: String,
    added: Vec<i64>,
    removed: Vec<i64>,
}

fn serialize_value_collection(name: &str, ids: &[i64]) -> String {
    let value = ValueCollection {
        id: name_to_key(name),
        name: name.to_string(),
        added: ids.to_vec(),
        removed: vec![],
    };
    serde_json::to_string(&value).unwrap_or_else(|_| String::from("{}"))
}

/// Steam collection ids are `"<tag>-<base64-no-padding>"`. BoilR strips a
/// trailing `==` (base64 padding) — we do the same so our keys match
/// exactly what BoilR / the Steam UI would produce for the same name.
fn name_to_key(name: &str) -> String {
    use base64::{engine::general_purpose, Engine as _};
    let b64 = general_purpose::STANDARD_NO_PAD.encode(name.as_bytes());
    let trimmed = if let Some(stripped) = b64.strip_suffix("==") {
        stripped.to_string()
    } else {
        b64
    };
    format!("{}-{}", STEAMSHELF_TAG, trimmed)
}

// ============ File location ============

/// `<SteamPath>/userdata/<accountid>/config/cloudstorage`. Returns `None`
/// when the userdata dir for this user doesn't have the new cloudstorage
/// folder yet (Steam creates it lazily once Collections are used).
fn cloudstorage_dir(steam_user_id: &str) -> Option<PathBuf> {
    let steam = steam_writers::find_steam_root()?;
    let dir = steam
        .join("userdata")
        .join(steam_user_id)
        .join("config")
        .join("cloudstorage");
    if dir.exists() {
        Some(dir)
    } else {
        None
    }
}

// ============ Read / write ============

type CategoryContents = Vec<(String, SteamCollection)>;

/// Read every `cloud-storage-namespace-<N>.json` listed in
/// `cloud-storage-namespaces.json`. Returns one `(filename, contents)`
/// per readable namespace — missing or unparsable files are logged and
/// skipped.
fn load_categories(cloudstorage: &std::path::Path) -> Result<Vec<(PathBuf, CategoryContents)>, CollectionsError> {
    let index_path = cloudstorage.join("cloud-storage-namespaces.json");
    let index_str = fs::read_to_string(&index_path).map_err(|e| {
        CollectionsError::Other(format!(
            "read {} failed: {}",
            index_path.display(),
            e
        ))
    })?;
    // Index is `[[<id>, "<version>"], ...]`. We only care about <id>.
    let index: Vec<(i64, String)> = serde_json::from_str(&index_str).map_err(|e| {
        CollectionsError::Other(format!(
            "parse {} failed: {}",
            index_path.display(),
            e
        ))
    })?;

    let mut out: Vec<(PathBuf, CategoryContents)> = Vec::new();
    for (id, _version) in index {
        let path = cloudstorage.join(format!("cloud-storage-namespace-{}.json", id));
        if !path.exists() {
            log::debug!(
                "[steam_collections] namespace file {} missing — skipping",
                path.display()
            );
            continue;
        }
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                log::warn!(
                    "[steam_collections] read {} failed: {} — skipping",
                    path.display(),
                    e
                );
                continue;
            }
        };
        let contents: CategoryContents = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                log::warn!(
                    "[steam_collections] parse {} failed: {} — skipping",
                    path.display(),
                    e
                );
                continue;
            }
        };
        out.push((path, contents));
    }
    Ok(out)
}

/// Pick the namespace file that holds the user's library Collections.
/// Heuristic:
///   1. Prefer any namespace whose contents already include at least one
///      `user-collections.` entry — Steam has been writing there.
///   2. Fall back to the namespace with the most entries (the empty
///      placeholders have 0).
///   3. Last resort: the first namespace in the index.
fn pick_collections_namespace(
    mut categories: Vec<(PathBuf, CategoryContents)>,
) -> Option<(PathBuf, CategoryContents)> {
    if categories.is_empty() {
        return None;
    }

    // Only count NON-Tokoru entries as evidence of the real namespace.
    // Otherwise our own previous writes to a wrong namespace would make us
    // re-pick that wrong namespace forever.
    let has_real_collections = |contents: &CategoryContents| -> bool {
        contents.iter().any(|(key, c)| {
            key.starts_with("user-collections.") && !c.is_steamshelf()
        })
    };

    if let Some(idx) = categories
        .iter()
        .position(|(_, contents)| has_real_collections(contents))
    {
        return Some(categories.swap_remove(idx));
    }

    // No namespace had `user-collections.` yet — pick the largest one
    // (Steam puts the collections in namespace-1 once any collection
    // gets created; empty namespaces are tiny).
    let idx = categories
        .iter()
        .enumerate()
        .max_by_key(|(_, (_, c))| c.len())
        .map(|(i, _)| i)
        .unwrap_or(0);
    Some(categories.swap_remove(idx))
}

/// Pull the human-readable `name` out of the JSON-encoded `value` field
/// of an `ActualSteamCollection`. The collection's `value` is a
/// stringified JSON object like `{"id":"...","name":"My Group",...}` —
/// we only need the name for overlap detection.
fn decode_value_name(value: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(value).ok()?;
    v.get("name").and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// Normalise a collection name for prefix-overlap matching: drop every
/// non-alphanumeric character, lowercase. "Batman" / "BATMAN" / "Batman!"
/// all collapse to "batman" and prefix-compare cleanly with "batmanarkham".
fn normalize_for_overlap(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn save_category(path: &std::path::Path, contents: &CategoryContents) -> Result<(), CollectionsError> {
    let json = serde_json::to_string(contents).map_err(|e| {
        CollectionsError::Other(format!("serialize {} failed: {}", path.display(), e))
    })?;
    fs::write(path, json).map_err(|e| {
        CollectionsError::Other(format!("write {} failed: {}", path.display(), e))
    })?;
    log::info!("[steam_collections] wrote {}", path.display());
    Ok(())
}

// ============ Reader: import user-side collections ============

/// One collection as read from Steam's cloudstorage — exposed so callers
/// (the Favorites import) can match by `name` and then walk `appids`.
/// Tokoru-managed collections are filtered out at the read site so
/// we never re-import our own auto-generated groupings.
#[derive(Debug, Clone)]
pub struct UserCollection {
    pub name: String,
    pub appids: Vec<i64>,
}

/// Read every NON-Tokoru collection from the user's cloudstorage.
/// We pick the same namespace `write_collections` would write to (so the
/// reader/writer stay in sync) and decode each `value` blob into its
/// `(name, added)` pair.
///
/// Hidden / deleted Steam collections are silently skipped — same as the
/// write path.
pub fn read_user_collections(
    steam_user_id: &str,
) -> Result<Vec<UserCollection>, CollectionsError> {
    let dir = cloudstorage_dir(steam_user_id).ok_or(CollectionsError::DbNotFound)?;
    let categories = load_categories(&dir)?;
    let (_, contents) = pick_collections_namespace(categories)
        .ok_or_else(|| {
            CollectionsError::Other(
                "Could not identify which cloud-storage-namespace holds Collections.".to_string(),
            )
        })?;

    let mut out: Vec<UserCollection> = Vec::new();
    for (key, value) in contents {
        if !key.starts_with("user-collections.") {
            continue;
        }
        let SteamCollection::Actual(actual) = value else {
            continue;
        };
        if actual.is_steamshelf() {
            continue;
        }
        let parsed: ValueCollection = match serde_json::from_str(&actual.value) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if parsed.added.is_empty() {
            continue;
        }
        out.push(UserCollection {
            name: parsed.name,
            appids: parsed.added,
        });
    }
    Ok(out)
}

// ============ Public entry point ============

/// Replace every Tokoru-tagged collection in Steam's cloudstorage
/// JSON files with the supplied set. Refuses to write while Steam (or
/// any steamwebhelper.exe) is running — they'd overwrite the file at
/// shutdown.
pub fn write_collections(
    steam_user_id: &str,
    collections: &[Collection],
) -> Result<(), CollectionsError> {
    if steam_writers::is_steam_running() {
        return Err(CollectionsError::SteamRunning);
    }

    let new_entries: Vec<(String, SteamCollection)> = collections
        .iter()
        .map(|c| {
            let actual = ActualSteamCollection::new(&c.name, &c.appids);
            (actual.key.clone(), SteamCollection::Actual(actual))
        })
        .collect();

    let dir = cloudstorage_dir(steam_user_id).ok_or(CollectionsError::DbNotFound)?;
    log::info!(
        "[steam_collections] write_collections: user={} dir={} entries={}",
        steam_user_id,
        dir.display(),
        new_entries.len()
    );

    let categories = load_categories(&dir)?;
    if categories.is_empty() {
        return Err(CollectionsError::Other(
            "No cloud-storage-namespace JSON files for this user — open a Collection in Steam at least once.".to_string(),
        ));
    }

    // Steam splits cloudstorage into multiple namespaces (1, 3, …). Only
    // ONE of them holds the library Collections — the rest are empty
    // placeholders for unrelated features (game-released notifications,
    // cloud-saves metadata, etc.). Identify the right one by looking for
    // any existing `user-collections.` key; fall back to the largest file
    // otherwise so we don't pick an empty placeholder.
    let (path, mut contents) = pick_collections_namespace(categories)
        .ok_or_else(|| {
            CollectionsError::Other(
                "Could not identify which cloud-storage-namespace holds Collections.".to_string(),
            )
        })?;
    let before = contents.len();
    contents.retain(|(_k, c)| !c.is_steamshelf());

    // Build the set of normalised names we're about to add. Anything we
    // want to SUPERSEDE on the user's side overlaps with one of these.
    let our_norms: Vec<String> = new_entries
        .iter()
        .filter_map(|(_k, c)| match c {
            SteamCollection::Actual(a) => decode_value_name(&a.value),
            _ => None,
        })
        .map(|n| normalize_for_overlap(&n))
        .filter(|n| !n.is_empty())
        .collect();

    // Drop user-made collections that overlap with any of our auto
    // franchises (prefix-match in either direction on the normalised
    // alphanumeric name). The user's other collections — "Favoris",
    // "BAC A SABLE", "BATTLE ROYAL", etc. — stay untouched because they
    // don't share a prefix with a game-franchise key.
    let after_strip = contents.len();
    let mut dropped_user = 0usize;
    contents.retain(|(_k, c)| match c {
        SteamCollection::Actual(a) => {
            let user_norm = decode_value_name(&a.value)
                .map(|n| normalize_for_overlap(&n))
                .unwrap_or_default();
            if user_norm.is_empty() {
                return true;
            }
            let overlaps = our_norms
                .iter()
                .any(|ours| ours.starts_with(&user_norm) || user_norm.starts_with(ours));
            if overlaps {
                dropped_user += 1;
            }
            !overlaps
        }
        SteamCollection::Deleted(_) => true,
    });
    if dropped_user > 0 {
        log::info!(
            "[steam_collections] removed {} user collections that overlap with auto franchises",
            dropped_user
        );
    }

    let new_count = new_entries.len();
    contents.extend(new_entries);
    log::info!(
        "[steam_collections] {}: {} entries → strip {} steamshelf, drop {} overlapping user → add {} new → write {} total",
        path.display(),
        before,
        before - after_strip,
        dropped_user,
        new_count,
        contents.len()
    );
    save_category(&path, &contents)?;
    Ok(())
}

// ============ Push favorites ============

/// Same alias list as the import path so the two stay symmetric — case-
/// insensitive substring match. "★ Favoris" → matches. "My faves" →
/// matches via "fav".
const FAVORITES_ALIASES: &[&str] = &["favorite", "favorites", "favoris", "favori", "fav"];

/// Default name when no existing user collection matches. Localised to
/// match the user's preferred Steam UI language; we don't have access to
/// Steam's locale here so we go with "Favoris" (the French default) since
/// the typical user pairs Tokoru with a French Steam UI. The label is
/// editable in Steam afterwards if needed.
const FAVORITES_DEFAULT_NAME: &str = "Favoris";

/// Push the user-curated favorites to Steam by writing the appid list into
/// the user's Favoris collection (or creating one if none matches the
/// aliases above). Other user collections + Tokoru-tagged collections are
/// preserved verbatim — we only touch the one row whose key matches the
/// favorites collection (or insert a new row).
///
/// Refuses to write while Steam is running — same constraint as
/// `write_collections`. The caller (the `push_favorites_to_steam`
/// command) is responsible for the close → write → relaunch dance when
/// Steam needs to be restarted.
///
/// `appids` is the canonical list — empty vec clears the collection
/// (Steam shows it as 0 entries; not deleted).
pub fn push_favorites(
    steam_user_id: &str,
    appids: Vec<i64>,
) -> Result<usize, CollectionsError> {
    if steam_writers::is_steam_running() {
        return Err(CollectionsError::SteamRunning);
    }

    let dir = cloudstorage_dir(steam_user_id).ok_or(CollectionsError::DbNotFound)?;
    let categories = load_categories(&dir)?;
    if categories.is_empty() {
        return Err(CollectionsError::Other(
            "No cloud-storage-namespace JSON files for this user — open a Collection in Steam at least once.".to_string(),
        ));
    }

    let (path, mut contents) = pick_collections_namespace(categories)
        .ok_or_else(|| {
            CollectionsError::Other(
                "Could not identify which cloud-storage-namespace holds Collections.".to_string(),
            )
        })?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Find existing favorites collection by alias — case-insensitive
    // substring match on the decoded `name` field. Tokoru-tagged
    // collections are excluded so we never hijack one of our own
    // franchise groupings if it happens to embed "fav" (unlikely but
    // cheap safety).
    let existing_idx = contents.iter().position(|(_k, c)| match c {
        SteamCollection::Actual(a) if !a.is_steamshelf() => {
            let lower = decode_value_name(&a.value)
                .map(|n| n.to_lowercase())
                .unwrap_or_default();
            FAVORITES_ALIASES.iter().any(|alias| lower.contains(alias))
        }
        _ => false,
    });

    let count = appids.len();

    match existing_idx {
        Some(idx) => {
            // Update the existing collection: keep its key, timestamp
            // bumped, conflict resolution etc — only replace the inner
            // `value` JSON with the new appid list. Preserves the user-
            // chosen name verbatim.
            if let (_key, SteamCollection::Actual(ref mut actual)) = &mut contents[idx] {
                let existing_name = decode_value_name(&actual.value)
                    .unwrap_or_else(|| FAVORITES_DEFAULT_NAME.to_string());
                let existing_id = parse_value_id(&actual.value);
                actual.value = serialize_user_value_collection(
                    existing_id.as_deref(),
                    &existing_name,
                    &appids,
                );
                actual.timestamp = now;
                log::info!(
                    "[steam_collections] updated existing favorites collection '{}' → {} appids",
                    existing_name,
                    count
                );
            }
        }
        None => {
            // Create a new "Favoris" collection NOT tagged Tokoru so
            // `write_collections` won't strip it. We use a slug-based
            // id (no base64 prefix) to match the format the Steam UI
            // generates when the user creates a collection by hand —
            // that way Steam treats it as a regular user-owned row.
            let new_id = format!("uc-{}", now);
            let new_key = format!("user-collections.{}", new_id);
            let value = serialize_user_value_collection(
                Some(&new_id),
                FAVORITES_DEFAULT_NAME,
                &appids,
            );
            contents.push((
                new_key.clone(),
                SteamCollection::Actual(ActualSteamCollection {
                    key: new_key,
                    timestamp: now,
                    value,
                    conflict_resolution_method: Some("custom".to_string()),
                    str_method_id: Some("union-collections".to_string()),
                    version: None,
                }),
            ));
            log::info!(
                "[steam_collections] no existing favorites collection — created '{}' with {} appids",
                FAVORITES_DEFAULT_NAME,
                count
            );
        }
    }

    save_category(&path, &contents)?;
    Ok(count)
}

/// Serialise a user-owned (non-Tokoru) collection value. Same shape as
/// `serialize_value_collection` but lets the caller keep an existing `id`
/// (preserving stable references inside Steam) instead of deriving one
/// from the name with the Tokoru prefix.
fn serialize_user_value_collection(id: Option<&str>, name: &str, ids: &[i64]) -> String {
    let resolved_id = id
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("uc-{}", name));
    let value = ValueCollection {
        id: resolved_id,
        name: name.to_string(),
        added: ids.to_vec(),
        removed: vec![],
    };
    serde_json::to_string(&value).unwrap_or_else(|_| String::from("{}"))
}

/// Pull `id` out of a JSON-encoded ValueCollection. `None` when the field
/// is missing or the value isn't valid JSON.
fn parse_value_id(value: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(value).ok()?;
    v.get("id").and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// Best-effort: derive the Steam3 numeric account id from the most-recent
/// userdata subdir. Returns `None` if the Steam install / userdata layout
/// is missing — caller surfaces it as a non-fatal error.
pub fn current_steam_user_id() -> Option<String> {
    let dir = steam_writers::most_recent_userdata_dir().ok()?;
    dir.file_name().and_then(|s| s.to_str()).map(|s| s.to_string())
}

// ============ Tests ============

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_to_key_matches_boilr_format() {
        let k = name_to_key("Itch");
        assert!(k.starts_with("Tokoru-"));
        assert!(!k.ends_with("=="));
    }

    #[test]
    fn collection_key_includes_steamshelf_tag() {
        let c = ActualSteamCollection::new("Tokoru · GOG Galaxy", &[1]);
        assert!(c.is_steamshelf());
        assert!(c.key.starts_with("user-collections.Tokoru-"));
    }

    #[test]
    fn value_payload_round_trips() {
        let raw = serialize_value_collection("Tokoru · Epic Games", &[1234567890]);
        let parsed: ValueCollection = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.name, "Tokoru · Epic Games");
        assert_eq!(parsed.added, vec![1234567890]);
        assert!(parsed.removed.is_empty());
    }

    #[test]
    fn category_round_trip() {
        let entry = ActualSteamCollection::new("Tokoru · GOG Galaxy", &[7]);
        let v: CategoryContents = vec![(entry.key.clone(), SteamCollection::Actual(entry))];
        let s = serde_json::to_string(&v).unwrap();
        let back: CategoryContents = serde_json::from_str(&s).unwrap();
        assert_eq!(back.len(), 1);
        match &back[0].1 {
            SteamCollection::Actual(a) => assert!(a.is_steamshelf()),
            _ => panic!("expected Actual variant"),
        }
    }
}
