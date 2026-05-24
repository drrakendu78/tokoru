import { useState, useMemo, useEffect, useCallback, useRef } from "react";
import { useTranslation } from "react-i18next";
import { useVirtualizer } from "@tanstack/react-virtual";
import { TopBar } from "../components/TopBar";
import { GameCard } from "../components/GameCard";
import { GameDetail } from "./GameDetail";
import { useGames } from "../lib/useGames";
import { useSourceTiles } from "../lib/useSourceTiles";
import { useTheme } from "../lib/useTheme";
import { useDownloads } from "../lib/useDownloads";
import {
  addGame,
  fetchAllArtwork,
  fullScan,
  getAllShortcuts,
  getGameUserTags,
  getLibraryTagIndex,
  listFavoriteGameIds,
  listGamePlayStats,
  listNeverPlayedIds,
  listRecentlyPlayedIds,
  pushFavoritesToSteam,
  pushToSteam,
  restartSteam,
  setGameUserTags,
  toggleGameFavorite,
} from "../lib/api";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import type { LibraryTagIndex, PlayStatEntry } from "../lib/api";

type SortMode = "name" | "playtime" | "last_played";

// i18n keys for sort labels — the display string is resolved at render
// time via `t(SORT_LABEL_KEYS[mode])` so it reacts to language changes
// without re-mounting the component.
const SORT_LABEL_KEYS: Record<SortMode, string> = {
  name: "library.sort.name",
  playtime: "library.sort.playtime",
  last_played: "library.sort.last_played",
};
import {
  SOURCE_LONG,
  SOURCE_DOT,
  KNOWN_SOURCES,
  type Game,
  type Shortcut,
  type Source,
} from "../lib/types";
import { useToast } from "../components/Toast";

export function Library() {
  const { t } = useTranslation();
  const { density, setDensity } = useTheme();
  const { games, loading, error, refresh } = useGames();
  // Source states (connected / launcher detected) — used to hide empty
  // sources from the sidebar that aren't even reachable on this machine.
  const { byKey: sourceStates } = useSourceTiles(refresh);
  const [addingGame, setAddingGame] = useState(false);

  const handleAddGame = useCallback(async () => {
    if (addingGame) return;
    let path: string | null = null;
    try {
      const picked = await openDialog({
        title: "Pick a game executable",
        multiple: false,
        directory: false,
        filters: [
          { name: "Executable", extensions: ["exe"] },
          { name: "All files", extensions: ["*"] },
        ],
      });
      path = typeof picked === "string" ? picked : null;
    } catch (e) {
      toast.error(`Pick failed: ${e instanceof Error ? e.message : String(e)}`);
      return;
    }
    if (!path) return;
    const base = path.split(/[\\/]/).pop() ?? "Game";
    const title = base.replace(/\.exe$/i, "").replace(/[._-]+/g, " ").trim() || "Game";
    setAddingGame(true);
    try {
      await addGame({ title, exePath: path, source: "custom" });
      toast.success(`Added "${title}" to library`);
      refresh();
    } catch (e) {
      toast.error(`Add failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setAddingGame(false);
    }
  }, [addingGame, refresh]);

  const { downloads, start: startDl, pause: pauseDl, resume: resumeDl, retry: retryDl } = useDownloads();
  const [shortcuts, setShortcuts] = useState<Record<string, Shortcut>>({});
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [detailId, setDetailId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [activeNav, setActiveNav] = useState<string>("all");
  const [scanning, setScanning] = useState(false);
  const [scanError, setScanError] = useState<string | null>(null);
  // Tag filter — the dynamic sidebar pills (sourced from the metadata
  // sync's SteamSpy tags) toggle this. `null` = no tag filter, show
  // every game. Selecting an already-selected pill clears it.
  const [selectedTag, setSelectedTag] = useState<string | null>(null);
  const [tagIndex, setTagIndex] = useState<LibraryTagIndex | null>(null);
  // IDs of every game the user has favorited (heart icon in GameDetail).
  // Drives both the "Favorites" sidebar count and the actual filtering
  // when activeNav === "fav".
  const [favoriteIds, setFavoriteIds] = useState<Set<string>>(new Set());
  const [recentlyPlayedIds, setRecentlyPlayedIds] = useState<Set<string>>(new Set());
  const [neverPlayedIds, setNeverPlayedIds] = useState<Set<string>>(new Set());
  // Sort mode + the play-stat lookup it depends on. `playStats` is a
  // Map keyed by game id so the comparator runs in O(1) per pair.
  // The mode persists across launches via localStorage — the user
  // shouldn't have to re-pick their preferred ordering on every boot.
  const [sortMode, setSortModeRaw] = useState<SortMode>(() => {
    try {
      const stored = localStorage.getItem("steamshelf.library_sort");
      if (stored === "name" || stored === "playtime" || stored === "last_played") {
        return stored;
      }
    } catch {}
    return "name";
  });
  const setSortMode = useCallback((next: SortMode) => {
    setSortModeRaw(next);
    try {
      localStorage.setItem("steamshelf.library_sort", next);
    } catch {}
  }, []);
  const [sortMenuOpen, setSortMenuOpen] = useState(false);
  const sortMenuRef = useRef<HTMLDivElement | null>(null);
  const [gridMenuOpen, setGridMenuOpen] = useState(false);
  const gridMenuRef = useRef<HTMLDivElement | null>(null);
  const mainScrollRef = useRef<HTMLElement | null>(null);
  const [playStats, setPlayStats] = useState<Map<string, PlayStatEntry>>(new Map());
  const toast = useToast();

  const refreshFavorites = useCallback(async () => {
    try {
      const ids = await listFavoriteGameIds();
      setFavoriteIds(new Set(ids));
    } catch {}
  }, []);

  // Debounce timer for the Steam Favoris push. Multi-clicks across cards
  // coalesce into a single Steam restart — same 2s window as GameDetail.
  const favoritesPushTimer = useRef<number | null>(null);

  /// Optimistic favorite toggle from a GameCard heart icon. Mirror of
  /// GameDetail's `toggleFavorite` — flips the local Set immediately,
  /// fires the IPC, then schedules a debounced push to Steam's Favoris
  /// collection (auto-restarts Steam, see `pushFavoritesToSteam`).
  const handleToggleFavorite = useCallback(
    (id: string) => {
      const wasFavorite = favoriteIds.has(id);
      setFavoriteIds((prev) => {
        const next = new Set(prev);
        if (wasFavorite) next.delete(id);
        else next.add(id);
        return next;
      });
      void toggleGameFavorite(id)
        .then((actual) => {
          setFavoriteIds((prev) => {
            const next = new Set(prev);
            if (actual) next.add(id);
            else next.delete(id);
            return next;
          });
        })
        .catch((e) => {
          // Revert optimistic flip on IPC failure.
          setFavoriteIds((prev) => {
            const next = new Set(prev);
            if (wasFavorite) next.add(id);
            else next.delete(id);
            return next;
          });
          const msg = e instanceof Error ? e.message : String(e);
          toast.error(`Couldn't toggle favorite: ${msg}`);
        });

      if (favoritesPushTimer.current !== null) {
        window.clearTimeout(favoritesPushTimer.current);
      }
      favoritesPushTimer.current = window.setTimeout(() => {
        favoritesPushTimer.current = null;
        void pushFavoritesToSteam(true)
          .then((res) => {
            if (res.steam_restarted) {
              toast.success(
                t("gamedetail.favorites_pushed_restarted", { count: res.pushed }),
              );
            } else if (res.pushed > 0) {
              toast.success(
                t("gamedetail.favorites_pushed", { count: res.pushed }),
              );
            }
          })
          .catch((e) => {
            const msg = e instanceof Error ? e.message : String(e);
            toast.error(t("gamedetail.favorites_push_failed", { error: msg }));
          });
      }, 2000);
    },
    [favoriteIds, toast, t],
  );

  // Flush pending push on unmount so a click followed by a route change
  // still gets to Steam.
  useEffect(() => {
    return () => {
      if (favoritesPushTimer.current !== null) {
        window.clearTimeout(favoritesPushTimer.current);
        favoritesPushTimer.current = null;
        void pushFavoritesToSteam(true).catch(() => {});
      }
    };
  }, []);

  const refreshPlayedSets = useCallback(async () => {
    try {
      const [recent, never, stats] = await Promise.all([
        // 365-day window: the user wants to see real play history, not
        // just the "last 2 weeks" Steam-style recency. The Library sort
        // by "last_played" gives a finer-grained ordering inside this
        // pool.
        listRecentlyPlayedIds(365),
        listNeverPlayedIds(),
        listGamePlayStats(),
      ]);
      setRecentlyPlayedIds(new Set(recent));
      setNeverPlayedIds(new Set(never));
      setPlayStats(new Map(stats.map((s) => [s.game_id, s])));
    } catch {}
  }, []);

  useEffect(() => {
    void refreshFavorites();
    void refreshPlayedSets();
  }, [refreshFavorites, refreshPlayedSets]);

  // Close the sort dropdown on outside click — same pattern as the
  // GameDetail more menu.
  useEffect(() => {
    if (!sortMenuOpen) return;
    const onClick = (e: MouseEvent) => {
      if (
        sortMenuRef.current &&
        e.target instanceof Node &&
        !sortMenuRef.current.contains(e.target)
      ) {
        setSortMenuOpen(false);
      }
    };
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, [sortMenuOpen]);

  // Same click-outside guard for the grid-size dropdown.
  useEffect(() => {
    if (!gridMenuOpen) return;
    const onClick = (e: MouseEvent) => {
      if (
        gridMenuRef.current &&
        e.target instanceof Node &&
        !gridMenuRef.current.contains(e.target)
      ) {
        setGridMenuOpen(false);
      }
    };
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, [gridMenuOpen]);

  // Refresh the tag index whenever the library changes — covers both the
  // initial load and the "metadata sync just finished, new tags arrived"
  // case. Cheap: a single SQL scan + JSON parse per game (the Rust side
  // already caps to top-3 tags per row).
  useEffect(() => {
    if (games.length === 0) return;
    getLibraryTagIndex()
      .then((idx) => setTagIndex(idx))
      .catch(() => {});
  }, [games.length]);

  const refreshShortcuts = useCallback(async () => {
    try {
      const list = await getAllShortcuts();
      const map: Record<string, Shortcut> = {};
      for (const s of list) map[s.game_id] = s;
      setShortcuts(map);
    } catch (e) {
      console.error("Library: get_all_shortcuts failed", e);
    }
  }, []);

  useEffect(() => {
    void refreshShortcuts();
  }, [refreshShortcuts]);

  // "Already in Steam" = Steam-native games (source === "steam") +
  // non-Steam games whose shortcut has been pushed. Steam-native games are
  // by definition already in Steam, no shortcut row required.
  const alreadyInSteamCount = useMemo(() => {
    let n = 0;
    for (const g of games) {
      if (g.source === "steam") {
        n += 1;
        continue;
      }
      if (g.id && shortcuts[g.id]?.status === "pushed") {
        n += 1;
      }
    }
    return n;
  }, [games, shortcuts]);
  const notInSteamCount = Math.max(games.length - alreadyInSteamCount, 0);

  // Source counts — derived from real DB rows.
  const sourceCounts = useMemo(() => {
    const acc: Record<string, number> = {};
    for (const g of games) {
      const key = (g.source as string) || "custom";
      acc[key] = (acc[key] ?? 0) + 1;
    }
    return acc;
  }, [games]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    // Tag filter goes first because it's a cheap Set lookup; the title
    // search runs over the (potentially smaller) tag-filtered set.
    let pool: Game[] = games;
    if (activeNav === "fav") {
      pool = pool.filter((g) => g.id != null && favoriteIds.has(g.id));
    } else if (activeNav === "recent") {
      pool = pool.filter((g) => g.id != null && recentlyPlayedIds.has(g.id));
    } else if (activeNav === "never") {
      pool = pool.filter((g) => g.id != null && neverPlayedIds.has(g.id));
    } else if (activeNav.startsWith("source:")) {
      const src = activeNav.slice("source:".length);
      pool = pool.filter((g) => g.source === src);
    } else if (activeNav === "in_steam") {
      // Mirror the `alreadyInSteamCount` rule above: Steam-native games are
      // always "in Steam", non-Steam games count when their shortcut row is
      // in the `pushed` state. Anything else (queued/failed/missing) → no.
      pool = pool.filter(
        (g) =>
          g.source === "steam" ||
          (g.id != null && shortcuts[g.id]?.status === "pushed")
      );
    } else if (activeNav === "not_in_steam") {
      pool = pool.filter(
        (g) =>
          g.source !== "steam" &&
          (g.id == null || shortcuts[g.id]?.status !== "pushed")
      );
    }
    if (selectedTag && tagIndex) {
      const allowed = new Set(tagIndex.games_by_tag[selectedTag] ?? []);
      pool = pool.filter((g) => g.id != null && allowed.has(g.id));
    }
    if (q) {
      pool = pool.filter((g) => g.title.toLowerCase().includes(q));
    }
    // Sort last — runs over the already-filtered set, which is usually
    // small enough that the comparator cost is negligible. Use a stable
    // tie-breaker (title ascending) so games with identical playtime /
    // last-played don't shuffle randomly on every render.
    const sorted = [...pool];
    if (sortMode === "name") {
      sorted.sort((a, b) =>
        a.title.localeCompare(b.title, undefined, { sensitivity: "base" })
      );
    } else if (sortMode === "playtime") {
      sorted.sort((a, b) => {
        const sa = a.id ? playStats.get(a.id)?.total_seconds ?? 0 : 0;
        const sb = b.id ? playStats.get(b.id)?.total_seconds ?? 0 : 0;
        if (sa !== sb) return sb - sa;
        return a.title.localeCompare(b.title, undefined, { sensitivity: "base" });
      });
    } else if (sortMode === "last_played") {
      sorted.sort((a, b) => {
        const la = a.id ? playStats.get(a.id)?.last_played ?? 0 : 0;
        const lb = b.id ? playStats.get(b.id)?.last_played ?? 0 : 0;
        if (la !== lb) return lb - la;
        return a.title.localeCompare(b.title, undefined, { sensitivity: "base" });
      });
    }
    return sorted;
  }, [games, query, selectedTag, tagIndex, activeNav, favoriteIds, recentlyPlayedIds, neverPlayedIds, sortMode, playStats]);

  const detailGame = useMemo(
    () => games.find((g) => g.id === detailId) ?? null,
    [games, detailId]
  );

  const toggle = (id: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const clearSelection = () => setSelected(new Set());

  // ============ Bulk actions for the floating selection bar ============
  const [bulkBusy, setBulkBusy] = useState<null | "push" | "artwork" | "tags">(null);
  const [bulkTagsOpen, setBulkTagsOpen] = useState(false);
  const [bulkTagsDraft, setBulkTagsDraft] = useState("");

  const handleBulkPush = async () => {
    if (selected.size === 0 || bulkBusy) return;
    setBulkBusy("push");
    const ids = Array.from(selected);
    let pushed = 0;
    let failed = 0;
    for (const id of ids) {
      try {
        await pushToSteam(id);
        pushed += 1;
      } catch (e) {
        failed += 1;
        console.error("Bulk push failed for", id, e);
      }
    }
    if (failed === 0) {
      toast.success(t("library.batch.push_done", { count: pushed }));
    } else {
      toast.error(t("library.batch.push_partial", { pushed, failed }));
    }
    await refreshShortcuts();
    // Steam reads shortcuts.vdf at startup — restart so the newly pushed
    // games show up immediately in the Steam library instead of waiting
    // for the next manual close/relaunch.
    if (pushed > 0) {
      try {
        await restartSteam();
        toast.success(t("library.batch.steam_restarted"));
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        toast.error(t("library.batch.steam_restart_failed", { error: msg }));
      }
    }
    setBulkBusy(null);
    // Push is one-shot per shortcut — clear so the user moves on. Artwork
    // and tags keep selection so the user can chain (re-fetch / add more).
    clearSelection();
  };

  const handleBulkFetchArtwork = async () => {
    if (selected.size === 0 || bulkBusy) return;
    setBulkBusy("artwork");
    try {
      const ids = Array.from(selected);
      const updated = await fetchAllArtwork(ids, false);
      toast.success(t("library.batch.artwork_done", { count: updated }));
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast.error(t("library.batch.artwork_failed", { error: msg }));
    } finally {
      setBulkBusy(null);
      await refresh();
      // Selection persists.
    }
  };

  const handleBulkAddTags = () => {
    if (selected.size === 0 || bulkBusy) return;
    setBulkTagsDraft("");
    setBulkTagsOpen(true);
  };

  const commitBulkTags = async () => {
    const tagsToAdd = bulkTagsDraft
      .split(",")
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
    if (tagsToAdd.length === 0) {
      setBulkTagsOpen(false);
      return;
    }
    setBulkBusy("tags");
    const ids = Array.from(selected);
    let ok = 0;
    let failed = 0;
    for (const id of ids) {
      try {
        const existing = await getGameUserTags(id);
        const seen = new Set(existing.map((s) => s.toLowerCase()));
        const merged = [...existing];
        for (const tag of tagsToAdd) {
          if (!seen.has(tag.toLowerCase())) {
            merged.push(tag);
            seen.add(tag.toLowerCase());
          }
        }
        await setGameUserTags(id, merged);
        ok += 1;
      } catch (e) {
        failed += 1;
        console.error("Bulk tag failed for", id, e);
      }
    }
    setBulkBusy(null);
    setBulkTagsOpen(false);
    if (failed === 0) {
      toast.success(t("library.batch.tags_done", { count: ok }));
    } else {
      toast.error(t("library.batch.tags_partial", { ok, failed }));
    }
    // Selection persists.
  };

  const runFullScan = async () => {
    setScanning(true);
    setScanError(null);
    try {
      const result = await fullScan();
      await refresh();
      await refreshShortcuts();
      toast.success(
        `Scan complete — ${result.new_games} new / ${result.total_found} total`
      );
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      console.error("Library: full_scan failed", e);
      setScanError(msg);
      toast.error(msg);
    } finally {
      setScanning(false);
    }
  };

  const LEFT_NAV = [
    { key: "all", label: t("library.sidebar.all_games"), count: games.length },
    { key: "recent", label: t("library.sidebar.recently_played"), count: recentlyPlayedIds.size },
    { key: "fav", label: t("library.sidebar.favorites"), count: favoriteIds.size },
    { key: "never", label: t("library.sidebar.never_played"), count: neverPlayedIds.size },
  ];

  const rightSlot = (
    <>
      {scanning && (
        <div className="inline-flex items-center gap-2 px-3 py-1 rounded-full bg-accent/15 border border-accent/20 text-accent text-[11px] font-bold uppercase tracking-widest">
          <span className="w-1.5 h-1.5 rounded-full bg-accent pulse-slow" />
          {t("library.scanning")}
        </div>
      )}
      <div className="relative w-64 group">
        <div className="absolute inset-y-0 left-0 flex items-center pl-3 pointer-events-none text-text-sec group-focus-within:text-white transition-colors">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
            <circle cx="11" cy="11" r="8" />
            <path d="m21 21-4.3-4.3" />
          </svg>
        </div>
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          type="text"
          placeholder={t("library.search_placeholder", { count: games.length })}
          className="w-full bg-white/[0.03] border border-transparent focus:border-white/10 hover:bg-white/[0.05] focus:bg-white/[0.08] text-sm font-medium text-white placeholder-text-muted rounded-full pl-9 pr-4 py-1.5 outline-none transition-all duration-200"
        />
      </div>
      <div className="w-px h-5 bg-white/10 mx-1" />
      <div className="flex items-center gap-1">
        <div className="relative" ref={gridMenuRef}>
          <button
            onClick={() => setGridMenuOpen((o) => !o)}
            className={`p-2 hover:text-white hover:bg-white/[0.05] rounded-md transition-colors ${
              gridMenuOpen || density !== "comfortable"
                ? "text-white bg-white/[0.08]"
                : "text-text-sec"
            }`}
            title={`Grid size: ${density}`}
            aria-expanded={gridMenuOpen}
            aria-haspopup="menu"
          >
            {density === "compact" ? (
              <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <rect x="3" y="3" width="5" height="5" rx="0.5" />
                <rect x="10" y="3" width="5" height="5" rx="0.5" />
                <rect x="17" y="3" width="4" height="5" rx="0.5" />
                <rect x="3" y="10" width="5" height="5" rx="0.5" />
                <rect x="10" y="10" width="5" height="5" rx="0.5" />
                <rect x="17" y="10" width="4" height="5" rx="0.5" />
                <rect x="3" y="17" width="5" height="4" rx="0.5" />
                <rect x="10" y="17" width="5" height="4" rx="0.5" />
                <rect x="17" y="17" width="4" height="4" rx="0.5" />
              </svg>
            ) : density === "spacious" ? (
              <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <rect x="3" y="3" width="8" height="18" rx="1.5" />
                <rect x="13" y="3" width="8" height="18" rx="1.5" />
              </svg>
            ) : (
              <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <rect x="3" y="3" width="7" height="7" rx="1" />
                <rect x="14" y="3" width="7" height="7" rx="1" />
                <rect x="14" y="14" width="7" height="7" rx="1" />
                <rect x="3" y="14" width="7" height="7" rx="1" />
              </svg>
            )}
          </button>
          {gridMenuOpen && (
            <div
              role="menu"
              className="absolute right-0 top-full mt-2 min-w-[170px] py-1 rounded-lg bg-surface border border-white/[0.08] shadow-premium z-50"
            >
              {(
                [
                  { value: "compact", label: "Compact", desc: "+1 column" },
                  { value: "comfortable", label: "Comfortable", desc: "Default" },
                  { value: "spacious", label: "Spacious", desc: "Larger cards" },
                ] as const
              ).map((opt) => {
                const active = density === opt.value;
                return (
                  <button
                    key={opt.value}
                    role="menuitemradio"
                    aria-checked={active}
                    onClick={() => {
                      setDensity(opt.value);
                      setGridMenuOpen(false);
                    }}
                    className={`w-full flex items-center justify-between gap-3 px-3 py-1.5 text-left text-[13px] transition-colors ${
                      active
                        ? "text-accent bg-white/[0.04]"
                        : "text-text-main hover:text-white hover:bg-white/[0.04]"
                    }`}
                  >
                    <span className="flex items-center gap-2">
                      {active ? (
                        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round">
                          <polyline points="20 6 9 17 4 12" />
                        </svg>
                      ) : (
                        <span className="inline-block w-[13px]" />
                      )}
                      {opt.label}
                    </span>
                    <span className="text-[11px] text-text-muted">{opt.desc}</span>
                  </button>
                );
              })}
            </div>
          )}
        </div>
        <div className="relative" ref={sortMenuRef}>
          <button
            onClick={() => setSortMenuOpen((o) => !o)}
            className={`p-2 hover:text-white hover:bg-white/[0.05] rounded-md transition-colors ${
              sortMenuOpen ? "text-white bg-white/[0.08]" : "text-text-sec"
            }`}
            title={`Sort: ${t(SORT_LABEL_KEYS[sortMode])}`}
            aria-expanded={sortMenuOpen}
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <path d="M11 16H3" />
              <path d="M16 16v-9" />
              <path d="m20 11-4-4-4 4" />
              <path d="M3 8h11" />
              <path d="M3 12h11" />
            </svg>
          </button>
          {sortMenuOpen ? (
            <div className="absolute right-0 top-full mt-1.5 w-44 bg-surface border border-white/[0.08] rounded-lg shadow-premium z-20 py-1 acrylic">
              {(Object.keys(SORT_LABEL_KEYS) as SortMode[]).map((m) => (
                <button
                  key={m}
                  onClick={() => {
                    setSortMode(m);
                    setSortMenuOpen(false);
                  }}
                  className={`w-full text-left px-3 py-2 text-[13px] font-medium transition-colors flex items-center justify-between ${
                    sortMode === m
                      ? "text-accent bg-accent/10"
                      : "text-text-main hover:bg-white/[0.05] hover:text-white"
                  }`}
                >
                  <span>{t(SORT_LABEL_KEYS[m])}</span>
                  {sortMode === m ? (
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round">
                      <polyline points="20 6 9 17 4 12" />
                    </svg>
                  ) : null}
                </button>
              ))}
            </div>
          ) : null}
        </div>
      </div>
      <button
        onClick={handleAddGame}
        disabled={addingGame}
        className="px-3 py-1.5 flex items-center gap-2 text-xs font-medium text-text-sec hover:text-white bg-white/[0.02] border border-white/[0.05] hover:bg-white/[0.06] rounded-md transition-colors ml-1 disabled:opacity-50 disabled:cursor-wait"
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
          <path d="M5 12h14" />
          <path d="M12 5v14" />
        </svg>
        {addingGame ? t("common.loading") : t("library.add_game")}
      </button>
    </>
  );

  return (
    <div className="h-screen w-screen flex flex-col relative overflow-hidden">
      <div className="ambient-vignette" />
      <TopBar rightSlot={rightSlot} />

      <div className="flex-1 flex overflow-hidden relative z-10">
        {/* Left rail */}
        <aside className="w-[260px] flex-shrink-0 border-r border-white/[0.04] bg-surface/30 backdrop-blur-2xl flex flex-col pt-8 pb-10 px-6 overflow-y-auto">
          <NavSection title={t("library.sidebar.library")}>
            {LEFT_NAV.map((n) => {
              const sel = activeNav === n.key;
              return (
                <button
                  key={n.key}
                  onClick={() => setActiveNav(n.key)}
                  className={`w-full flex items-center justify-between py-1.5 px-3 rounded-lg text-sm font-medium transition-colors text-left ${
                    sel
                      ? "bg-white/[0.04] text-white cursor-default"
                      : "text-text-sec hover:text-white hover:bg-white/[0.04]"
                  }`}
                >
                  {n.label}
                  <span className={`text-xs ${sel ? "text-text-sec" : "text-text-muted"}`}>
                    {n.count}
                  </span>
                </button>
              );
            })}
          </NavSection>

          <NavSection title={t("library.sidebar.sources")}>
            {KNOWN_SOURCES.filter((s) => {
              // Hide sources that have zero games AND aren't actively
              // reachable on this machine — keeps the sidebar tight.
              // A source stays visible when:
              //   * it has at least 1 game in the library, OR
              //   * its tile state is `connected` (OAuth account OR
              //     locally-detected launcher with a path).
              const count = sourceCounts[s] ?? 0;
              if (count > 0) return true;
              return sourceStates[s]?.connected ?? false;
            }).map((s: Source) => {
              const navKey = `source:${s}`;
              const sel = activeNav === navKey;
              return (
              <button
                key={s}
                onClick={() => setActiveNav(navKey)}
                className={`w-full flex items-center justify-between py-1.5 px-3 rounded-lg text-sm transition-colors group text-left ${
                  sel
                    ? "bg-white/[0.04] text-white cursor-default"
                    : "text-text-sec hover:text-white hover:bg-white/[0.04]"
                }`}
              >
                <span className="flex items-center gap-2.5">
                  <span
                    className="w-1.5 h-1.5 rounded-full opacity-70 group-hover:opacity-100 transition-opacity"
                    style={{ background: SOURCE_DOT[s] }}
                  />
                  {SOURCE_LONG[s]}
                </span>
                <span className={`text-xs ${sel ? "text-text-sec" : "text-text-muted"}`}>
                  {sourceCounts[s] ?? 0}
                </span>
              </button>
              );
            })}
          </NavSection>

          <NavSection title={t("library.sidebar.status")}>
            <div className="space-y-0.5">
              <button
                onClick={() => setActiveNav("in_steam")}
                className={`w-full flex items-center justify-between py-1.5 px-3 rounded-lg text-sm transition-colors text-left ${
                  activeNav === "in_steam"
                    ? "bg-white/[0.04] text-white cursor-default"
                    : "text-text-sec hover:text-white hover:bg-white/[0.04]"
                }`}
              >
                <span className="flex items-center gap-2">
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" className="text-emerald-500">
                    <polyline points="20 6 9 17 4 12" />
                  </svg>
                  {t("library.sidebar.already_in_steam")}
                </span>
                <span className="text-xs text-text-muted">{alreadyInSteamCount}</span>
              </button>
              <button
                onClick={() => setActiveNav("not_in_steam")}
                className={`w-full flex items-center justify-between py-1.5 px-3 rounded-lg text-sm transition-colors text-left ${
                  activeNav === "not_in_steam"
                    ? "bg-white/[0.04] text-white cursor-default"
                    : "text-text-sec hover:text-white hover:bg-white/[0.04]"
                }`}
              >
                <span className="flex items-center gap-2">
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" className="text-accent">
                    <line x1="12" y1="5" x2="12" y2="19" />
                    <line x1="5" y1="12" x2="19" y2="12" />
                  </svg>
                  {t("library.sidebar.not_in_steam")}
                </span>
                <span className="text-xs text-text-muted">{notInSteamCount}</span>
              </button>
              <button className="w-full flex items-center justify-between py-1.5 px-3 rounded-lg text-sm text-text-sec opacity-50 cursor-not-allowed text-left">
                <span className="flex items-center gap-2">
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" className="text-yellow-500">
                    <circle cx="12" cy="12" r="10" />
                    <line x1="12" y1="8" x2="12" y2="12" />
                    <line x1="12" y1="16" x2="12.01" y2="16" />
                  </svg>
                  {t("library.sidebar.sync_errors")}
                </span>
                <span className="text-xs">0</span>
              </button>
            </div>
          </NavSection>

          <div>
            <h3 className="text-[11px] uppercase tracking-widest text-text-muted font-bold mb-3 pl-2 flex items-center justify-between">
              <span>{t("library.sidebar.tags")}</span>
              {selectedTag ? (
                <button
                  onClick={() => setSelectedTag(null)}
                  className="text-[10px] font-medium text-accent hover:text-accent-hover normal-case tracking-normal"
                >
                  Clear
                </button>
              ) : null}
            </h3>
            <div className="flex flex-wrap gap-2 pl-1">
              {tagIndex && tagIndex.top_tags.length > 0 ? (
                tagIndex.top_tags.map((t) => {
                  const active = selectedTag === t.name;
                  return (
                    <button
                      key={t.name}
                      onClick={() =>
                        setSelectedTag((cur) => (cur === t.name ? null : t.name))
                      }
                      className={`px-2.5 py-1 rounded-md text-[11px] font-medium border transition-colors flex items-center gap-1.5 ${
                        active
                          ? "bg-accent/15 border-accent/40 text-accent"
                          : "bg-white/[0.03] border-white/[0.04] text-text-sec hover:text-white hover:bg-white/[0.06]"
                      }`}
                      title={`${t.game_count} games`}
                    >
                      <span>{t.name}</span>
                      <span
                        className={`text-[10px] font-bold ${
                          active ? "opacity-90" : "opacity-50"
                        }`}
                      >
                        {t.game_count}
                      </span>
                    </button>
                  );
                })
              ) : (
                <p className="text-[11px] text-text-muted px-1">
                  Run Settings → Sync metadata to populate.
                </p>
              )}
            </div>
          </div>
        </aside>

        {/* Main grid */}
        <main ref={mainScrollRef} className="flex-1 overflow-y-auto relative">
          <div className="max-w-[1600px] mx-auto px-10 pt-10 pb-32">
            <div className="mb-10 card-enter" style={{ animationDelay: "0.1s" }}>
              <h1 className="text-[32px] md:text-[40px] font-semibold tracking-tight leading-none text-white">
                {t("library.title")}
              </h1>
              <p className="text-sm font-medium text-text-sec mt-3 flex items-center gap-2">
                {filtered.length === games.length
                  ? t("library.count_games", { count: games.length })
                  : t("library.count_filtered", { shown: filtered.length, total: games.length })}
                {(error || scanError) && (
                  <>
                    <span className="w-1 h-1 rounded-full bg-white/20" />
                    <span className="text-yellow-400">{error ?? scanError}</span>
                  </>
                )}
              </p>
            </div>

            {loading && games.length === 0 ? (
              <div className="flex flex-col items-center justify-center py-32 text-text-sec text-sm">
                {t("library.loading")}
              </div>
            ) : games.length === 0 ? (
              <EmptyState onScan={runFullScan} scanning={scanning} />
            ) : (
              <VirtualizedLibraryGrid
                scrollParentRef={mainScrollRef}
                games={filtered}
                density={density}
                downloads={downloads}
                shortcuts={shortcuts}
                selected={selected}
                onToggle={toggle}
                onOpen={(id) => setDetailId(id)}
                onShortcutChange={refreshShortcuts}
                onInstall={(g) => {
                  if (!g.id) return;
                  if (g.source === "steam") {
                    if (!g.platform_id) {
                      toast.error("Steam appid manquant pour ce jeu.");
                      return;
                    }
                    void import("../lib/api")
                      .then(({ launchUri }) => launchUri(`steam://install/${g.platform_id}`))
                      .then(() =>
                        toast.success(`Ouverture de Steam pour installer ${g.title}…`)
                      )
                      .catch((e) =>
                        toast.error(
                          e instanceof Error ? e.message : `Install failed: ${String(e)}`
                        )
                      );
                    return;
                  }
                  void startDl(g.id).catch((e) =>
                    toast.error(e instanceof Error ? e.message : `Install failed: ${String(e)}`)
                  );
                }}
                onPauseDownload={(id) =>
                  void pauseDl(id).catch((e) =>
                    toast.error(e instanceof Error ? e.message : `Pause failed: ${String(e)}`)
                  )
                }
                onResumeDownload={(id) =>
                  void resumeDl(id).catch((e) =>
                    toast.error(e instanceof Error ? e.message : `Resume failed: ${String(e)}`)
                  )
                }
                onRetryDownload={(id) =>
                  void retryDl(id).catch((e) =>
                    toast.error(e instanceof Error ? e.message : `Retry failed: ${String(e)}`)
                  )
                }
                favoriteIds={favoriteIds}
                onToggleFavorite={handleToggleFavorite}
              />
            )}

            {/* Legacy non-virtualized grid kept for reference under the dead flag below */}
            {false && (
              <div className="library-grid grid grid-cols-3 lg:grid-cols-4 xl:grid-cols-5 2xl:grid-cols-6 gap-x-6 gap-y-10">
                {filtered.map((g, i) => {
                  const dl = g.id ? downloads[g.id] ?? null : null;
                  return (
                    <GameCard
                      key={g.id ?? g.exe_path}
                      game={g}
                      shortcut={g.id ? shortcuts[g.id] ?? null : null}
                      download={dl}
                      selected={!!g.id && selected.has(g.id)}
                      onToggle={() => g.id && toggle(g.id)}
                      onOpen={() => g.id && setDetailId(g.id)}
                      onShortcutChange={refreshShortcuts}
                      onInstall={() => {
                        if (!g.id) return;
                        // Steam games delegate to the Steam client via the
                        // steam://install/<appid> protocol URI instead of our
                        // own legendary/gogdl runners.
                        if (g.source === "steam") {
                          if (!g.platform_id) {
                            toast.error("Steam appid manquant pour ce jeu.");
                            return;
                          }
                          void import("../lib/api")
                            .then(({ launchUri }) => launchUri(`steam://install/${g.platform_id}`))
                            .then(() =>
                              toast.success(`Ouverture de Steam pour installer ${g.title}…`)
                            )
                            .catch((e) =>
                              toast.error(
                                e instanceof Error ? e.message : `Install failed: ${String(e)}`
                              )
                            );
                          return;
                        }
                        void startDl(g.id).catch((e) =>
                          toast.error(
                            e instanceof Error ? e.message : `Install failed: ${String(e)}`
                          )
                        );
                      }}
                      onPauseDownload={() => {
                        if (!g.id) return;
                        void pauseDl(g.id).catch((e) =>
                          toast.error(
                            e instanceof Error ? e.message : `Pause failed: ${String(e)}`
                          )
                        );
                      }}
                      onResumeDownload={() => {
                        if (!g.id) return;
                        void resumeDl(g.id).catch((e) =>
                          toast.error(
                            e instanceof Error ? e.message : `Resume failed: ${String(e)}`
                          )
                        );
                      }}
                      onRetryDownload={() => {
                        if (!g.id) return;
                        void retryDl(g.id).catch((e) =>
                          toast.error(
                            e instanceof Error ? e.message : `Retry failed: ${String(e)}`
                          )
                        );
                      }}
                      animationDelay={`${0.15 + i * 0.04}s`}
                    />
                  );
                })}
              </div>
            )}
          </div>

          {/* Floating batch action bar — compact, single-line, no wrap. */}
          {selected.size > 0 && (
            <div className="fixed bottom-8 left-1/2 -translate-x-1/2 z-30">
              <div className="acrylic rounded-full p-1.5 pl-4 flex items-center gap-2 border border-white/10 shadow-premium">
                <span className="text-[12px] font-semibold text-white/90 whitespace-nowrap mr-1">
                  {t("library.batch.selected", { count: selected.size })}
                </span>
                <button
                  onClick={() => void handleBulkPush()}
                  disabled={bulkBusy !== null}
                  className="bg-accent hover:bg-accent-hover text-white h-8 px-3.5 rounded-full text-[12px] font-semibold shadow-glow transition-all duration-200 active:scale-95 disabled:opacity-60 disabled:cursor-not-allowed whitespace-nowrap"
                >
                  {bulkBusy === "push"
                    ? t("library.batch.pushing")
                    : t("library.batch.push_to_steam", { count: selected.size })}
                </button>
                <div className="w-px h-5 bg-white/10" />
                <button
                  onClick={() => void handleBulkFetchArtwork()}
                  disabled={bulkBusy !== null}
                  className="bg-white/5 hover:bg-white/10 text-white/80 hover:text-white h-8 px-3 rounded-full text-[12px] font-medium transition-colors active:scale-95 disabled:opacity-60 disabled:cursor-not-allowed whitespace-nowrap"
                >
                  {bulkBusy === "artwork"
                    ? t("library.batch.fetching")
                    : t("library.batch.fetch_artwork")}
                </button>
                <button
                  onClick={handleBulkAddTags}
                  disabled={bulkBusy !== null}
                  className="bg-white/5 hover:bg-white/10 text-white/80 hover:text-white h-8 px-3 rounded-full text-[12px] font-medium transition-colors active:scale-95 disabled:opacity-60 disabled:cursor-not-allowed whitespace-nowrap"
                >
                  {t("library.batch.add_tags")}
                </button>
                <button
                  onClick={clearSelection}
                  className="w-8 h-8 text-text-muted hover:text-white hover:bg-white/10 rounded-full transition-colors flex items-center justify-center"
                  title={t("library.batch.clear")}
                >
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                    <line x1="18" y1="6" x2="6" y2="18" />
                    <line x1="6" y1="6" x2="18" y2="18" />
                  </svg>
                </button>
              </div>
            </div>
          )}

          {/* Bulk-tags modal — opens from the floating action bar
              "Add tags" button. Comma-separated input, merges with each
              selected game's existing user_tags (case-insensitive dedupe). */}
          {bulkTagsOpen && (
            <div
              className="fixed inset-0 z-[100] flex items-center justify-center bg-black/60 backdrop-blur-sm pointer-events-auto"
              onClick={(e) => {
                if (e.target === e.currentTarget) setBulkTagsOpen(false);
              }}
            >
              <div className="acrylic border border-white/[0.08] rounded-2xl shadow-premium w-[460px] max-w-[90vw] p-6 flex flex-col gap-4">
                <div className="flex flex-col gap-1">
                  <h2 className="text-[18px] font-semibold text-white tracking-tight">
                    {t("library.batch.tags_modal_title", { count: selected.size })}
                  </h2>
                  <p className="text-[12px] text-text-sec">
                    {t("library.batch.tags_modal_subtitle")}
                  </p>
                </div>
                <input
                  type="text"
                  autoFocus
                  value={bulkTagsDraft}
                  onChange={(e) => setBulkTagsDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void commitBulkTags();
                    else if (e.key === "Escape") setBulkTagsOpen(false);
                  }}
                  placeholder={t("library.batch.tags_modal_placeholder") ?? ""}
                  className="w-full bg-white/[0.04] border border-white/[0.08] focus:border-accent/60 rounded-lg px-3 py-2 text-[13px] text-white outline-none transition-colors"
                />
                <div className="flex items-center justify-end gap-2 mt-2">
                  <button
                    type="button"
                    onClick={() => setBulkTagsOpen(false)}
                    disabled={bulkBusy === "tags"}
                    className="px-3 py-1.5 rounded-md text-[12px] font-medium text-text-sec hover:text-white bg-white/[0.02] hover:bg-white/[0.05] border border-white/[0.06] transition-colors disabled:opacity-50"
                  >
                    {t("common.cancel")}
                  </button>
                  <button
                    type="button"
                    onClick={() => void commitBulkTags()}
                    disabled={bulkBusy === "tags" || bulkTagsDraft.trim().length === 0}
                    className="px-4 py-1.5 rounded-md text-[12px] font-semibold text-white bg-accent hover:bg-accent-hover shadow-glow transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    {bulkBusy === "tags" ? t("common.saving") : t("common.save")}
                  </button>
                </div>
              </div>
            </div>
          )}
        </main>
      </div>

      {detailGame && (
        <GameDetail
          game={detailGame}
          shortcut={detailGame.id ? shortcuts[detailGame.id] ?? null : null}
          onShortcutChange={refreshShortcuts}
          onGameUpdated={() => {
            void refresh();
            void refreshFavorites();
            void refreshPlayedSets();
          }}
          onClose={() => setDetailId(null)}
        />
      )}
    </div>
  );
}

function NavSection({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div className="mb-8">
      <h3 className="text-[11px] uppercase tracking-widest text-text-muted font-bold mb-3 pl-2">
        {title}
      </h3>
      <div className="space-y-0.5">{children}</div>
    </div>
  );
}

function EmptyState({
  onScan,
  scanning,
}: {
  onScan: () => void;
  scanning: boolean;
}) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col items-center justify-center py-32 text-center max-w-[480px] mx-auto">
      <div className="w-16 h-16 rounded-2xl bg-white/[0.04] border border-white/[0.08] flex items-center justify-center text-accent mb-6">
        <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
          <rect x="2" y="6" width="20" height="12" rx="2" />
          <path d="M6 12h.01M10 12h.01M14 12h.01M18 12h.01" />
        </svg>
      </div>
      <h2 className="text-2xl font-semibold tracking-tight text-white">
        {t("library.empty_title")}
      </h2>
      <p className="text-[13px] text-text-sec mt-3 leading-relaxed">
        {t("library.empty_desc")}
      </p>
      <button
        onClick={onScan}
        disabled={scanning}
        className="mt-8 px-6 py-3 rounded-full bg-accent hover:bg-accent-hover disabled:opacity-60 disabled:cursor-not-allowed text-white text-sm font-semibold shadow-glow transition-all active:scale-95"
      >
        {scanning ? t("library.scanning") : t("library.run_first_scan")}
      </button>
    </div>
  );
}

/// Virtualized version of the Library grid — only renders the rows visible
/// in the scroll viewport (+/- a small overscan). Drops WebView2 RAM from
/// ~1 GB (857 GameCards × decoded covers all mounted) to ~150 MB by
/// keeping ~30 cards in the DOM at any time. Image bitmaps for unmounted
/// cards are released by Chromium because the <img> elements no longer
/// exist.
///
/// The grid stays responsive: column count is recomputed from the
/// container width + density. Cell height includes the cover (2/3
/// aspect) plus title row, plus row-gap.
type DensityKey = "compact" | "comfortable" | "spacious";

interface VirtualizedLibraryGridProps {
  scrollParentRef: React.RefObject<HTMLElement | null>;
  games: Game[];
  density: DensityKey;
  downloads: Record<string, import("../lib/types").DownloadState | null | undefined>;
  shortcuts: Record<string, Shortcut>;
  selected: Set<string>;
  onToggle: (id: string) => void;
  onOpen: (id: string) => void;
  onShortcutChange: () => void | Promise<void>;
  onInstall: (game: Game) => void;
  onPauseDownload: (id: string) => void;
  onResumeDownload: (id: string) => void;
  onRetryDownload: (id: string) => void;
  favoriteIds: Set<string>;
  onToggleFavorite: (id: string) => void;
}

function VirtualizedLibraryGrid({
  scrollParentRef,
  games,
  density,
  downloads,
  shortcuts,
  selected,
  onToggle,
  onOpen,
  onShortcutChange,
  onInstall,
  onPauseDownload,
  onResumeDownload,
  onRetryDownload,
  favoriteIds,
  onToggleFavorite,
}: VirtualizedLibraryGridProps) {
  // Live width of the container — drives column count. Updated by a
  // ResizeObserver so resizing the window or toggling the density
  // recomputes the grid without a full remount.
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [containerWidth, setContainerWidth] = useState(0);
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    setContainerWidth(el.clientWidth);
    const ro = new ResizeObserver((entries) => {
      const w = entries[0]?.contentRect.width ?? 0;
      setContainerWidth(w);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Density-aware tile sizing. Numbers mirror the breakpoints that the
  // legacy CSS grid used (see `html[data-density=...] .library-grid`
  // rules in index.css) so the visual feel is preserved.
  const { minTileWidth, columnGap, rowGap, aspect } = useMemo(() => {
    switch (density) {
      case "compact":
        return { minTileWidth: 160, columnGap: 12, rowGap: 16, aspect: 2 / 3 };
      case "spacious":
        return { minTileWidth: 280, columnGap: 32, rowGap: 48, aspect: 2 / 3 };
      default:
        return { minTileWidth: 220, columnGap: 24, rowGap: 40, aspect: 2 / 3 };
    }
  }, [density]);

  const columnCount = Math.max(
    1,
    Math.floor((containerWidth + columnGap) / (minTileWidth + columnGap))
  );
  const tileWidth = columnCount
    ? Math.floor((containerWidth - columnGap * (columnCount - 1)) / columnCount)
    : minTileWidth;
  // GameCard footer (title + meta line) is roughly 90px on top of the
  // cover. Add a tiny breathing margin so virtualization doesn't clip
  // hover scale transforms.
  const rowHeight = Math.ceil(tileWidth / aspect) + 90 + rowGap;
  const rowCount = Math.ceil(games.length / Math.max(1, columnCount));

  const rowVirtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => scrollParentRef.current,
    estimateSize: () => rowHeight,
    overscan: 3,
  });

  return (
    <div ref={containerRef}>
      <div
        style={{
          height: rowVirtualizer.getTotalSize(),
          position: "relative",
          width: "100%",
        }}
      >
        {rowVirtualizer.getVirtualItems().map((vRow) => {
          const startIndex = vRow.index * columnCount;
          const rowGames = games.slice(startIndex, startIndex + columnCount);
          return (
            <div
              key={vRow.key}
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                width: "100%",
                transform: `translateY(${vRow.start}px)`,
                display: "grid",
                gridTemplateColumns: `repeat(${columnCount}, minmax(0, 1fr))`,
                columnGap,
                paddingBottom: rowGap,
              }}
            >
              {rowGames.map((g) => {
                const dl = g.id ? downloads[g.id] ?? null : null;
                return (
                  <GameCard
                    key={g.id ?? g.exe_path}
                    game={g}
                    shortcut={g.id ? shortcuts[g.id] ?? null : null}
                    download={dl}
                    selected={!!g.id && selected.has(g.id)}
                    onToggle={() => g.id && onToggle(g.id)}
                    onOpen={() => g.id && onOpen(g.id)}
                    onShortcutChange={onShortcutChange}
                    onInstall={() => onInstall(g)}
                    onPauseDownload={() => g.id && onPauseDownload(g.id)}
                    onResumeDownload={() => g.id && onResumeDownload(g.id)}
                    onRetryDownload={() => g.id && onRetryDownload(g.id)}
                    isFavorite={!!g.id && favoriteIds.has(g.id)}
                    onToggleFavorite={() => g.id && onToggleFavorite(g.id)}
                  />
                );
              })}
            </div>
          );
        })}
      </div>
    </div>
  );
}

// Re-export Game type for legacy imports (GameCard/GameDetail still import from here)
export type { Game };
