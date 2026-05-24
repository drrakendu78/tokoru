import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  fetchArtwork,
  deleteGame as apiDeleteGame,
  getGameAchievements,
  getGameFavorite,
  getGameMetadata,
  getGameUserTags,
  launchUri,
  openInstallFolder,
  pushFavoritesToSteam,
  setGameCustomTitle,
  setGameManualPlaytimeHours,
  setGameUserTags,
  syncGameAchievements,
  syncMetadataOne,
  toggleGameFavorite,
} from "../lib/api";
import type { AchievementsView, GameMetadataView } from "../lib/api";
import type { CoverOption, Game, PlaytimeSummary, Shortcut } from "../lib/types";
import { SOURCE_LONG, SOURCE_DOT } from "../lib/types";
import {
  browseCovers,
  browseIcons,
  browseHeroes,
  browseLogos,
  getPlaytimeSummary,
  getRestartAfterPush,
  pushToSteam,
  removeFromSteam,
  restartSteam,
  setGameCover,
  setGameHero,
  setGameLogo,
  setGameIcon,
  uninstallGame,
} from "../lib/api";
import { CoralButton } from "../components/CoralButton";
import { useToast } from "../components/Toast";
import { useDownloads } from "../lib/useDownloads";

interface GameDetailProps {
  game: Game;
  shortcut?: Shortcut | null;
  onClose: () => void;
  onShortcutChange?: () => void;
  onGameUpdated?: () => void;
}

const ARTWORK_TABS = ["Library Capsule", "Hero", "Logo", "Icon"] as const;
type ArtTab = (typeof ARTWORK_TABS)[number];

interface FilterPillProps {
  label: string;
  count: number;
  active: boolean;
  onClick: () => void;
  accent: "white" | "fuchsia" | "red";
}

function FilterPill({ label, count, active, onClick, accent }: FilterPillProps) {
  const activeRing =
    accent === "fuchsia"
      ? "border-fuchsia-400/60 bg-fuchsia-500/15 text-fuchsia-200"
      : accent === "red"
      ? "border-red-400/60 bg-red-500/15 text-red-200"
      : "border-white/30 bg-white/10 text-white";
  const idle =
    "border-white/[0.08] bg-white/[0.02] text-text-muted hover:text-white hover:bg-white/[0.05]";
  return (
    <button
      type="button"
      onClick={onClick}
      className={`px-2.5 py-1 rounded-full border text-[11px] font-semibold uppercase tracking-wider transition-colors flex items-center gap-1.5 ${
        active ? activeRing : idle
      }`}
    >
      {label}
      <span
        className={`text-[10px] font-bold ${
          active ? "opacity-90" : "opacity-50"
        }`}
      >
        {count}
      </span>
    </button>
  );
}

function formatHoursMinutes(seconds: number): { value: string; sub: string } {
  if (seconds <= 0) return { value: "0h", sub: "0m" };
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  return { value: `${hours}h`, sub: `${minutes}m` };
}

function relativeFromNow(unix: number | null): string {
  if (!unix) return "—";
  const now = Math.floor(Date.now() / 1000);
  const delta = Math.max(now - unix, 0);
  if (delta < 60) return "just now";
  if (delta < 3600) return `${Math.floor(delta / 60)}m ago`;
  if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`;
  if (delta < 86400 * 30) return `${Math.floor(delta / 86400)}d ago`;
  return `${Math.floor(delta / (86400 * 30))}mo ago`;
}

export function GameDetail({
  game,
  shortcut,
  onClose,
  onShortcutChange,
  onGameUpdated,
}: GameDetailProps) {
  const { t } = useTranslation();
  const [tab, setTab] = useState<ArtTab>("Library Capsule");
  const [summary, setSummary] = useState<PlaytimeSummary | null>(null);
  const [busy, setBusy] = useState(false);
  // Multi-source enriched metadata (description, tags, dev/publisher,
  // franchise, HLTB). Loaded lazily on mount via `get_game_metadata` so
  // we don't bloat the games table read path used by Library scrolling.
  const [metadata, setMetadata] = useState<GameMetadataView | null>(null);
  // Heart icon state — optimistic toggle on click, persisted via the
  // toggle_game_favorite command. Loaded on mount; defaults to false
  // for rows that don't exist yet (e.g. a freshly added shortcut).
  const [favorite, setFavorite] = useState(false);
  // Achievements — cached read first (instant), then background sync
  // honoring the 24h stale window so re-opens of the panel are silent.
  const [achievements, setAchievements] = useState<AchievementsView | null>(null);
  const [achievementsLoading, setAchievementsLoading] = useState(false);
  // User-curated tags (independent from the SteamSpy community tags
  // exposed via `metadata.tags`). Loaded on mount, written back via
  // `set_game_user_tags` from the edit modal.
  const [userTags, setUserTags] = useState<string[]>([]);
  const [userTagsModalOpen, setUserTagsModalOpen] = useState(false);
  // 3-tab layout per aidesigner mockup
  // `2026-05-24T13-52-25-603Z-redessine-la-page-game-detail-de-tok`.
  // - overview: description + meta + achievements + playtime + tags
  // - artworks: SteamGridDB picker (Capsule / Hero / Logo / Icon)
  // - tech: install paths / exe / command / steam appid (read-only)
  const [activeSection, setActiveSection] = useState<"overview" | "artworks" | "tech">("overview");

  useEffect(() => {
    let cancelled = false;
    if (!game.id) return;
    const gameIdAtLoad = game.id;
    getGameMetadata(gameIdAtLoad)
      .then((m) => {
        if (cancelled) return;
        setMetadata(m);
        // Lazy locale refresh — when the cached metadata was synced under
        // a different UI language, kick a one-shot resync for THIS game
        // (Steam-source only since the locale-aware fetcher is Steam Store
        // for now). Cheap: 1 jeu = 1 Steam Store call, no batch.
        if (m?.needs_locale_refresh) {
          // Steam-source games use their appid directly; non-Steam games
          // (Epic/GOG/Ubi/EA) trigger a Steam Store name search on the
          // backend and reuse the matching appid for the metadata fetch.
          void syncMetadataOne(gameIdAtLoad)
            .then(() => getGameMetadata(gameIdAtLoad))
            .then((refreshed) => {
              if (!cancelled && refreshed) setMetadata(refreshed);
            })
            .catch(() => {});
        }
      })
      .catch(() => {});
    getGameFavorite(game.id)
      .then((f) => {
        if (!cancelled) setFavorite(f);
      })
      .catch(() => {});
    getGameUserTags(game.id)
      .then((tags) => {
        if (!cancelled) setUserTags(tags);
      })
      .catch(() => {});
    // Steam + GOG: fetch achievements. Other sources don't have a service yet.
    if (game.source === "steam" || game.source === "gog") {
      setAchievementsLoading(true);
      const gid = game.id;
      getGameAchievements(gid)
        .then((a) => {
          if (!cancelled) setAchievements(a);
          console.info("[achievements] cached:", a.total, "items,", a.unlocked, "unlocked");
        })
        .catch((e) => {
          console.warn("[achievements] cache read failed:", e);
        })
        .then(() => syncGameAchievements(gid, true))
        .then((a) => {
          if (!cancelled && a) setAchievements(a);
          console.info("[achievements] synced:", a?.total, "items,", a?.unlocked, "unlocked");
        })
        .catch((e) => {
          console.error("[achievements] sync failed:", e);
        })
        .finally(() => {
          if (!cancelled) setAchievementsLoading(false);
        });
    } else {
      setAchievements(null);
    }
    return () => {
      cancelled = true;
    };
  }, [game.id, game.source]);

  // More-menu dropdown state. Click-outside closes it; an Escape press
  // would too but we leave that off for now since it's a single, low-
  // risk surface.
  const [moreMenuOpen, setMoreMenuOpen] = useState(false);
  const moreMenuRef = useRef<HTMLDivElement | null>(null);
  // Title-rename inline edit. `editingTitle = true` swaps the h1 for a
  // controlled input; Enter saves, Escape cancels. The original
  // `title` column in DB stays untouched — only `custom_title` flips.
  const [editingTitle, setEditingTitle] = useState(false);
  const [titleDraft, setTitleDraft] = useState("");
  // Manual-playtime modal — replaces the browser `prompt()`, which broke
  // the dark/acrylic design. State + draft live next to the rename
  // pair so both inline edits share the same opt-in mode.
  const [playtimeModalOpen, setPlaytimeModalOpen] = useState(false);
  const [playtimeDraft, setPlaytimeDraft] = useState("");

  useEffect(() => {
    if (!moreMenuOpen) return;
    const onClick = (e: MouseEvent) => {
      if (
        moreMenuRef.current &&
        e.target instanceof Node &&
        !moreMenuRef.current.contains(e.target)
      ) {
        setMoreMenuOpen(false);
      }
    };
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, [moreMenuOpen]);

  const handleRefreshMetadata = async () => {
    if (!game.id) return;
    setMoreMenuOpen(false);
    try {
      toast.info(`Refreshing metadata for ${game.title}…`);
      await syncMetadataOne(game.id);
      toast.success(`Metadata refreshed for ${game.title}.`);
      const m = await getGameMetadata(game.id).catch(() => null);
      setMetadata(m);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast.error(`Refresh failed: ${msg}`);
    }
  };

  const handleRefreshArtwork = async () => {
    if (!game.id) return;
    setMoreMenuOpen(false);
    try {
      toast.info("Re-fetching artwork from SteamGridDB…");
      await fetchArtwork(game.id);
      toast.success("Artwork refreshed.");
      onGameUpdated?.();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast.error(`Artwork refresh failed: ${msg}`);
    }
  };

  const handleOpenInstallFolder = async () => {
    if (!game.id) return;
    setMoreMenuOpen(false);
    try {
      await openInstallFolder(game.id);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast.error(msg);
    }
  };

  const handleSetPlaytime = () => {
    setMoreMenuOpen(false);
    const currentSec = summary?.total_seconds ?? 0;
    const currentHours = currentSec > 0 ? (currentSec / 3600).toFixed(1) : "";
    setPlaytimeDraft(currentHours);
    setPlaytimeModalOpen(true);
  };

  const commitPlaytime = async () => {
    console.info("[playtime] commitPlaytime fired, draft=", playtimeDraft);
    if (!game.id) {
      setPlaytimeModalOpen(false);
      return;
    }
    const raw = playtimeDraft.replace(",", ".").trim();
    const hours = parseFloat(raw);
    if (!isFinite(hours) || hours < 0) {
      toast.error("Invalid number of hours.");
      return;
    }
    try {
      await setGameManualPlaytimeHours(game.id, hours);
      toast.success(`Playtime set to ${hours}h. Run Sync to push to Steam.`);
      const next = await getPlaytimeSummary(game.id).catch(() => null);
      if (next) setSummary(next);
      onGameUpdated?.();
      setPlaytimeModalOpen(false);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast.error(`Couldn't set playtime: ${msg}`);
    }
  };

  const handleStartRename = () => {
    setMoreMenuOpen(false);
    setTitleDraft(game.title);
    setEditingTitle(true);
  };

  const commitRename = async () => {
    if (!game.id) {
      setEditingTitle(false);
      return;
    }
    const next = titleDraft.trim();
    // Setting the same name OR an empty value: clear the override —
    // revert to the source-reported title.
    const payload = next === "" || next === game.title ? null : next;
    try {
      await setGameCustomTitle(game.id, payload);
      onGameUpdated?.();
      toast.success(payload === null ? "Reverted to original title." : `Renamed to "${next}".`);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast.error(`Rename failed: ${msg}`);
    } finally {
      setEditingTitle(false);
    }
  };

  const handleRemoveFromLibrary = async () => {
    if (!game.id) return;
    setMoreMenuOpen(false);
    const ok = await ask(
      `Remove "${game.title}" from your Tokoru library?\n\nThis only removes it from Tokoru — the game files and the original launcher entry are not touched.`,
      { title: "Remove from library", kind: "warning" }
    );
    if (!ok) return;
    try {
      await apiDeleteGame(game.id);
      toast.success(`${game.title} removed.`);
      onGameUpdated?.();
      onClose();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast.error(`Remove failed: ${msg}`);
    }
  };

  // Debounce ref for the post-toggle Steam Favoris sync. Rapid multi-
  // clicks coalesce into one push so a user favoriting 5 games in a row
  // only triggers ONE Steam restart at the end (not 5). The 2s window
  // is short enough to feel immediate without flooding restarts.
  //
  // This auto-restart-on-toggle behavior was explicitly chosen by the
  // user (option "1 mais il le restart") — they want clicking the heart
  // to immediately reflect in Steam's Favoris collection, even when
  // Steam is running. The backend command refuses to write while Steam
  // runs (would lose the change on shutdown) so the restart dance is
  // the only path that delivers what was asked.
  const favoritesPushTimer = useRef<number | null>(null);

  const toggleFavorite = async () => {
    if (!game.id) return;
    // Optimistic flip — the heart should feel instant. We reconcile
    // with the persisted state when the IPC returns.
    setFavorite((f) => !f);
    try {
      const next = await toggleGameFavorite(game.id);
      setFavorite(next);
      onGameUpdated?.();
      // Schedule a debounced push to Steam's Favoris collection.
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
    } catch (e) {
      // Revert on error so the UI doesn't lie about the DB state.
      setFavorite((f) => !f);
      const msg = e instanceof Error ? e.message : String(e);
      toast.error(`Couldn't toggle favorite: ${msg}`);
    }
  };

  // Flush the pending debounced push when GameDetail unmounts so a click
  // followed by a quick close still gets persisted to Steam.
  useEffect(() => {
    return () => {
      if (favoritesPushTimer.current !== null) {
        window.clearTimeout(favoritesPushTimer.current);
        favoritesPushTimer.current = null;
        void pushFavoritesToSteam(true).catch(() => {});
      }
    };
  }, []);

  // Live download state for the current game, if any.
  const { downloads, start: startDl, pause: pauseDl, resume: resumeDl, cancel: cancelDl } = useDownloads();
  const download = game.id ? downloads[game.id] : undefined;
  const isDownloading = download?.status === "downloading";
  const isPaused = download?.status === "paused";
  const isQueued = download?.status === "queued";
  const hasActiveDownload = isDownloading || isPaused || isQueued;
  const toast = useToast();
  const source = (game.source as string) || "custom";
  const dot = SOURCE_DOT[source] ?? "#A1A1A6";
  const longName = SOURCE_LONG[source] ?? source;

  // ---- Artwork browse state ----
  const [options, setOptions] = useState<CoverOption[]>([]);
  const [loadingOptions, setLoadingOptions] = useState(false);
  const [optionsError, setOptionsError] = useState<string | null>(null);
  const [applying, setApplying] = useState<string | null>(null); // url being applied
  // Independent visibility toggles. Default: static + animated on, NSFW off.
  // The user can mix freely (e.g. NSFW + Animated only, Static + NSFW, etc.).
  const [showStatic, setShowStatic] = useState(true);
  const [showAnimated, setShowAnimated] = useState(true);
  const [showNsfw, setShowNsfw] = useState(false);

  // Bucket counts for the pill labels.
  const counts = options.reduce(
    (acc, o) => {
      if (o.nsfw) acc.nsfw += 1;
      else if (o.is_animated) acc.animated += 1;
      else acc.static += 1;
      return acc;
    },
    { static: 0, animated: 0, nsfw: 0 }
  );

  // Each pill is its own independent bucket — matching how the counts are
  // displayed. NSFW items are their OWN bucket regardless of being static
  // or animated. So:
  //   - Static = safe-only static images
  //   - Animated = safe-only animated images
  //   - NSFW = every NSFW image (static + animated combined)
  // Activate any combination to display only those buckets.
  const visibleOptions = options.filter((o) => {
    if (o.nsfw) return showNsfw;
    return o.is_animated ? showAnimated : showStatic;
  });
  const currentUrlByTab: Record<ArtTab, string | null | undefined> = {
    "Library Capsule": game.artwork_url,
    Hero: game.hero_url,
    Logo: game.logo_url,
    Icon: game.icon_url,
  };

  const loadOptions = useCallback(
    async (which: ArtTab) => {
      setLoadingOptions(true);
      setOptionsError(null);
      setOptions([]);
      try {
        let res: CoverOption[] = [];
        if (which === "Library Capsule") res = await browseCovers(game.title);
        else if (which === "Hero") res = await browseHeroes(game.title);
        else if (which === "Logo") res = await browseLogos(game.title);
        else if (which === "Icon") res = await browseIcons(game.title);
        setOptions(res);
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        setOptionsError(msg);
      } finally {
        setLoadingOptions(false);
      }
    },
    [game.title]
  );

  // Auto-load when tab or game changes.
  useEffect(() => {
    void loadOptions(tab);
  }, [tab, loadOptions]);

  // Guard: the Icon tab is hidden for native Steam games (Steam refreshes
  // icons from its CDN, no override possible). If we land on it after
  // switching to a Steam-source game, snap back to the Capsule tab.
  useEffect(() => {
    if (tab === "Icon" && game.source === "steam") {
      setTab("Library Capsule");
    }
  }, [tab, game.source]);

  const applyArtwork = async (opt: CoverOption) => {
    if (!game.id || applying) return;
    setApplying(opt.url);
    try {
      let steamRestarted = false;
      if (tab === "Library Capsule") await setGameCover(game.id, opt.url);
      else if (tab === "Hero") await setGameHero(game.id, opt.url);
      else if (tab === "Logo") await setGameLogo(game.id, opt.url);
      else if (tab === "Icon") {
        const res = await setGameIcon(game.id, opt.url);
        steamRestarted = res.steam_restarted;
      }
      if (steamRestarted) {
        toast.success(`${tab} updated for ${game.title} (Steam restarted)`);
      } else {
        toast.success(`${tab} updated for ${game.title}`);
      }
      onGameUpdated?.();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast.error(msg);
    } finally {
      setApplying(null);
    }
  };

  useEffect(() => {
    if (!game.id) return;
    const id = game.id;
    void getPlaytimeSummary(id)
      .then(setSummary)
      .catch((e) => {
        console.error("GameDetail: get_playtime_summary failed", e);
      });
  }, [game.id]);

  const isSteamNative = game.source === "steam";
  const inSteam = isSteamNative || shortcut?.status === "pushed";

  const handlePush = async () => {
    if (!game.id || busy) return;
    setBusy(true);
    try {
      const res = await pushToSteam(game.id);
      toast.success(`${game.title} added to Steam (appid ${res.appid})`);
      const willRestart = getRestartAfterPush();
      // Soft failure: shortcut is written but the sidebar Collection wasn't
      // refreshed (Steam still open, leveldb not yet initialized, etc.).
      // Suppress the toast when a restart is queued — `restart_steam` will
      // retry the rebuild with Steam closed, which is the only window where
      // the leveldb write actually sticks. Reporting both would just look
      // contradictory.
      if (!res.collection_updated && res.collection_error && !willRestart) {
        toast.error(`Steam Collection not updated: ${res.collection_error}`);
      }
      onShortcutChange?.();
      if (willRestart) {
        restartSteam()
          .then(() =>
            toast.success("Steam restarted — shortcut + Collections updated")
          )
          .catch((err) =>
            toast.error(
              `Push OK, but restart failed: ${
                err instanceof Error ? err.message : String(err)
              }`
            )
          );
      }
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleRemove = async () => {
    if (!game.id || busy) return;
    setBusy(true);
    try {
      await removeFromSteam(game.id);
      toast.success(`${game.title} removed from Steam`);
      onShortcutChange?.();
      // Same auto-restart behaviour as push — Steam reads shortcuts.vdf at
      // startup and ignores it while running, so the sidebar still shows
      // the removed entry until Steam relaunches. The preference is shared
      // between push and remove (one toggle, both flows).
      if (getRestartAfterPush()) {
        restartSteam()
          .then(() =>
            toast.success(
              "Steam restarted — shortcut removed + Collections updated"
            )
          )
          .catch((err) =>
            toast.error(
              `Remove OK, but restart failed: ${
                err instanceof Error ? err.message : String(err)
              }`,
            ),
          );
      }
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  // ── Uninstall (Epic / GOG only — see commands::downloads::uninstall_game)
  // The user gets a confirm dialog mentioning the disk-space cost when we can
  // estimate it (i.e. while the download tracker still remembers total_bytes
  // for this game). For games installed in past sessions we just say "the
  // game files" without a number — the runtime state is in-memory only.
  const handleUninstall = async () => {
    if (!game.id || busy) return;
    const sizeGb = download?.total_bytes
      ? (download.total_bytes / 1024 ** 3).toFixed(1) + " GB"
      : null;
    const detail = sizeGb
      ? t("gamedetail.uninstall_confirm_size", { size: sizeGb })
      : t("gamedetail.uninstall_confirm");
    const ok = await ask(
      t("gamedetail.uninstall_confirm_title", { title: game.title }) + " " + detail,
      {
        title: t("gamedetail.uninstall"),
        kind: "warning",
      },
    );
    if (!ok) return;

    setBusy(true);
    try {
      await uninstallGame(game.id);
      toast.success(`${game.title} uninstalled`);
      onGameUpdated?.();
      onShortcutChange?.();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  // Eligibility — only Epic/GOG games that we currently track as installed.
  // Steam-native and local/custom sources are out of Tokoru's scope (we
  // never wrote those files).
  const canUninstall =
    !!game.install_path &&
    (game.source === "epic" || game.source === "gog") &&
    !hasActiveDownload;

  // Install button — owned but not installed games. Epic/GOG go through our
  // own legendary/gogdl runners; Steam delegates to the Steam client itself
  // via the `steam://install/<appid>` protocol URI (Steam intercepts and
  // shows its own install dialog).
  const canInstall =
    !game.install_path &&
    (game.source === "epic" || game.source === "gog" || game.source === "steam") &&
    !hasActiveDownload;
  const installViaSteam = game.source === "steam";

  const handleInstall = async () => {
    if (!game.id || busy) return;
    setBusy(true);
    try {
      if (installViaSteam) {
        // Delegate to Steam: opening this URI brings Steam to the front and
        // pops the install confirmation dialog for the given appid. Tokoru
        // doesn't track this download — Steam owns the progress UI.
        const appid = game.platform_id;
        if (!appid) {
          throw new Error("Steam appid manquant pour ce jeu.");
        }
        await launchUri(`steam://install/${appid}`);
        toast.success(`Ouverture de Steam pour installer ${game.title}…`);
      } else {
        await startDl(game.id);
        toast.success(`Téléchargement de ${game.title} démarré`);
      }
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const total = formatHoursMinutes(summary?.total_seconds ?? 0);
  const last2w = formatHoursMinutes(summary?.last_2weeks_seconds ?? 0);

  return (
    <div className="absolute inset-0 z-40 flex justify-end pointer-events-none">
      {/* Scrim */}
      <div
        onClick={onClose}
        className="absolute inset-0 bg-black/40 backdrop-blur-[1px] pointer-events-auto"
        style={{
          animation: "fadeUp 0.3s ease-out forwards",
        }}
      />

      {/* Drawer */}
      <div
        className="relative w-[680px] max-w-[90vw] h-full bg-surface flex flex-col shadow-premium pointer-events-auto drawer-enter overflow-hidden border-l border-white/[0.06]"
        style={{ boxShadow: "0 0 60px -10px rgba(0,0,0,0.7)" }}
      >
        {/* Coral accent stripe */}
        <div className="absolute left-0 top-0 bottom-0 w-[3px] bg-gradient-to-b from-accent via-accent/40 to-transparent" />

        <div className="flex-1 overflow-y-auto">
          {/* Top floating controls */}
          <div className="absolute top-4 left-4 right-4 flex justify-between z-30">
            <button
              onClick={onClose}
              className="w-9 h-9 flex items-center justify-center rounded-full bg-black/40 hover:bg-black/70 backdrop-blur-md text-white border border-white/10 transition-colors shadow-sm"
              aria-label={t("gamedetail.back")}
            >
              <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="m15 18-6-6 6-6" />
              </svg>
            </button>
            <button
              onClick={onClose}
              className="w-9 h-9 flex items-center justify-center rounded-full bg-black/40 hover:bg-black/70 backdrop-blur-md text-white border border-white/10 transition-colors shadow-sm"
              aria-label={t("gamedetail.close")}
            >
              <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </div>

          {/* Hero artwork */}
          <div className="relative w-full h-[220px]">
            {game.hero_url ? (
              <img
                src={game.hero_url}
                alt=""
                className="absolute inset-0 w-full h-full object-cover"
                onError={(e) => {
                  (e.currentTarget as HTMLImageElement).style.display = "none";
                }}
              />
            ) : null}
            <div
              className="absolute inset-0"
              style={{
                background: `linear-gradient(135deg, ${dot}22 0%, #0a0a0c 100%)`,
              }}
            />
            <div className="absolute inset-0 bg-gradient-to-t from-surface via-surface/30 to-transparent" />
            <div className="absolute inset-0 bg-gradient-to-r from-surface/80 via-transparent to-transparent" />
            <div className="absolute bottom-6 left-8 right-8 z-20">
              {editingTitle ? (
                <input
                  type="text"
                  value={titleDraft}
                  autoFocus
                  onChange={(e) => setTitleDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void commitRename();
                    else if (e.key === "Escape") setEditingTitle(false);
                  }}
                  onBlur={() => void commitRename()}
                  className="text-[44px] font-bold tracking-tighter leading-none text-white bg-black/40 border border-white/20 focus:border-accent rounded-md px-2 py-0 outline-none w-full max-w-[80%]"
                />
              ) : (
                <h1
                  className="text-[44px] font-bold tracking-tighter leading-none text-white drop-shadow-lg"
                  title="Rename via the ⋯ menu"
                >
                  {game.title}
                </h1>
              )}
              <p className="text-sm font-medium text-white/70 mt-2 tracking-wide drop-shadow-md">
                {longName}
                {game.platform_id && (
                  <>
                    <span className="mx-1.5 opacity-40">·</span>
                    <span className="font-mono text-[12px]">
                      {game.platform_id}
                    </span>
                  </>
                )}
              </p>
            </div>
          </div>

          {/* Body */}
          <div className="px-8 pb-10">
            {/* Source & status row */}
            <div className="flex items-center justify-between pb-8 border-b border-white/[0.04]">
              <div className="flex items-center gap-3">
                <div className="px-3 py-1.5 rounded-md bg-white/[0.04] border border-white/[0.06] flex items-center gap-2 text-xs font-semibold text-white tracking-wide">
                  <span
                    className="w-2 h-2 rounded-full"
                    style={{
                      background: dot,
                      boxShadow: `0 0 8px ${dot}80`,
                    }}
                  />
                  {longName}
                </div>
                {inSteam ? (
                  <div className="h-7 px-3 rounded-full bg-emerald-500/10 border border-emerald-500/30 flex items-center gap-1.5 text-xs font-medium text-emerald-400">
                    {t("gamedetail.in_steam_library")}
                  </div>
                ) : (
                  <div className="h-7 px-3 rounded-full bg-accent/10 border border-accent/20 flex items-center gap-1.5 text-xs font-medium text-accent">
                    {t("gamedetail.not_in_steam_yet")}
                  </div>
                )}
              </div>
              <div className="flex items-center gap-1 text-text-sec">
                <button
                  onClick={() => void toggleFavorite()}
                  className={`p-2 hover:bg-white/5 rounded-md transition-colors ${
                    favorite ? "text-accent" : "text-text-sec hover:text-white"
                  }`}
                  aria-label={favorite ? "Unfavorite" : "Favorite"}
                  title={favorite ? "Remove from favorites" : "Add to favorites"}
                >
                  <svg
                    width="18"
                    height="18"
                    viewBox="0 0 24 24"
                    fill={favorite ? "currentColor" : "none"}
                    stroke="currentColor"
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                  >
                    <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
                  </svg>
                </button>
                <button
                  onClick={() => setUserTagsModalOpen(true)}
                  className={`p-2 hover:bg-white/5 rounded-md transition-colors ${
                    userTags.length > 0 ? "text-accent" : "text-text-sec hover:text-white"
                  }`}
                  aria-label={t("gamedetail.edit_tags")}
                  title={
                    userTags.length > 0
                      ? t("gamedetail.user_tags_count", { count: userTags.length })
                      : t("gamedetail.add_tags")
                  }
                >
                  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M20.59 13.41l-7.17 7.17a2 2 0 0 1-2.83 0L2 12V2h10l8.59 8.59a2 2 0 0 1 0 2.82z" />
                    <line x1="7" y1="7" x2="7.01" y2="7" />
                  </svg>
                </button>
                <div className="relative" ref={moreMenuRef}>
                  <button
                    onClick={() => setMoreMenuOpen((o) => !o)}
                    className={`p-2 hover:bg-white/5 hover:text-white rounded-md transition-colors ${
                      moreMenuOpen ? "bg-white/10 text-white" : ""
                    }`}
                    aria-label={t("gamedetail.more_actions")}
                    aria-expanded={moreMenuOpen}
                  >
                    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                      <circle cx="12" cy="12" r="1" />
                      <circle cx="19" cy="12" r="1" />
                      <circle cx="5" cy="12" r="1" />
                    </svg>
                  </button>
                  {moreMenuOpen ? (
                    <div className="absolute right-0 top-full mt-1.5 w-56 bg-surface border border-white/[0.08] rounded-lg shadow-premium z-20 py-1 acrylic">
                      <MenuItem onClick={handleStartRename}>
                        {t("gamedetail.rename_dots")}
                      </MenuItem>
                      <MenuItem onClick={() => void handleSetPlaytime()}>
                        {t("gamedetail.set_playtime_dots")}
                      </MenuItem>
                      <MenuItem onClick={() => void handleRefreshMetadata()}>
                        {t("gamedetail.refresh_metadata")}
                      </MenuItem>
                      <MenuItem onClick={() => void handleRefreshArtwork()}>
                        {t("gamedetail.refresh_artwork")}
                      </MenuItem>
                      <MenuItem onClick={() => void handleOpenInstallFolder()} disabled={!game.install_path}>
                        {t("gamedetail.open_install_folder")}
                      </MenuItem>
                      <div className="my-1 border-t border-white/[0.05]" />
                      <MenuItem onClick={() => void handleRemoveFromLibrary()} danger>
                        {t("gamedetail.remove_from_library")}
                      </MenuItem>
                    </div>
                  ) : null}
                </div>
              </div>
            </div>

            {/* 3-tab navigation (per aidesigner mockup) — splits the long
                scroll into focused sections. Vue d'ensemble is the default
                landing. Artworks isolates the picker (which can have
                200+ thumbnails). Propriétés Techniques keeps the rarely-
                consulted install paths one click away. */}
            <div className="mt-6 flex items-center gap-6 border-b border-white/[0.06]">
              {([
                ["overview", t("gamedetail.tab_overview")],
                ["artworks", t("gamedetail.tab_artworks")],
                ["tech", t("gamedetail.tab_tech")],
              ] as const).map(([key, label]) => (
                <button
                  key={key}
                  type="button"
                  onClick={() => setActiveSection(key)}
                  className={`pb-3 -mb-px border-b-2 text-[13px] transition-colors ${
                    activeSection === key
                      ? "border-accent text-white font-semibold"
                      : "border-transparent text-text-muted font-medium hover:text-text-main"
                  }`}
                >
                  {label}
                </button>
              ))}
            </div>

            {activeSection === "overview" && (
            <>
            {/* User-curated tags — coral chips, visually distinct from the
                SteamSpy community tags below. Rendered standalone so the
                section appears even when the game has no enriched metadata
                yet (e.g. brand-new custom shortcut). Click the tag-icon in
                the header to edit. */}
            {userTags.length > 0 ? (
              <div className="mt-8 flex flex-wrap items-center gap-1.5">
                <span className="text-text-muted uppercase tracking-wider text-[10px] font-semibold mr-1">
                  {t("gamedetail.user_tags")}
                </span>
                {userTags.map((tag) => (
                  <button
                    key={tag}
                    type="button"
                    onClick={() => setUserTagsModalOpen(true)}
                    className="px-2.5 py-1 rounded-full bg-accent/15 border border-accent/30 text-[11px] font-medium text-accent hover:bg-accent/25 transition-colors"
                  >
                    {tag}
                  </button>
                ))}
              </div>
            ) : null}

            {/* Enriched metadata (description, tags, dev/publisher,
                franchise). Rendered only when the multi-source sync has
                actually filled the row — silent on empty rows so the
                page doesn't get an awkward blank section. */}
            {metadata && (metadata.description || metadata.tags.length > 0 || metadata.developer || metadata.franchise || metadata.hltb_main_hours) ? (
              <div className="mt-8 space-y-5">
                {metadata.description ? (
                  <p className="text-[14px] leading-relaxed text-text-sec">
                    {metadata.description}
                  </p>
                ) : null}

                {(metadata.developer || metadata.publisher || metadata.franchise || metadata.hltb_main_hours) ? (
                  <div className="flex flex-wrap gap-x-6 gap-y-2 text-[12px]">
                    {metadata.developer ? (
                      <div>
                        <span className="text-text-muted uppercase tracking-wider text-[10px] font-semibold">{t("gamedetail.developer")}</span>
                        <span className="ml-2 text-white font-medium">{metadata.developer}</span>
                      </div>
                    ) : null}
                    {metadata.publisher ? (
                      <div>
                        <span className="text-text-muted uppercase tracking-wider text-[10px] font-semibold">{t("gamedetail.publisher")}</span>
                        <span className="ml-2 text-white font-medium">{metadata.publisher}</span>
                      </div>
                    ) : null}
                    {metadata.franchise ? (
                      <div>
                        <span className="text-text-muted uppercase tracking-wider text-[10px] font-semibold">{t("gamedetail.franchise")}</span>
                        <span className="ml-2 text-white font-medium">{metadata.franchise}</span>
                      </div>
                    ) : null}
                    {metadata.hltb_main_hours ? (
                      <div>
                        <span className="text-text-muted uppercase tracking-wider text-[10px] font-semibold">{t("gamedetail.time_to_beat")}</span>
                        <span className="ml-2 text-white font-medium">{metadata.hltb_main_hours}h</span>
                      </div>
                    ) : null}
                    {metadata.dlcs.length > 0 ? (
                      <div>
                        <span className="text-text-muted uppercase tracking-wider text-[10px] font-semibold">{t("gamedetail.dlc")}</span>
                        <span className="ml-2 text-white font-medium">{t("gamedetail.dlc_available", { count: metadata.dlcs.length })}</span>
                      </div>
                    ) : null}
                  </div>
                ) : null}

                {metadata.tags.length > 0 ? (
                  <div className="flex flex-wrap gap-1.5">
                    {metadata.tags.slice(0, 12).map((t) => (
                      <span
                        key={t.name}
                        className="px-2.5 py-1 rounded-full bg-white/[0.04] border border-white/[0.06] text-[11px] font-medium text-text-main"
                        title={`${t.votes} votes`}
                      >
                        {t.name}
                      </span>
                    ))}
                  </div>
                ) : null}

                {metadata.screenshots.length > 0 ? (
                  <div className="flex gap-2 overflow-x-auto pb-1 -mx-1 px-1">
                    {metadata.screenshots.slice(0, 6).map((url) => (
                      <img
                        key={url}
                        src={url}
                        alt=""
                        className="h-32 rounded-lg border border-white/[0.06] flex-shrink-0"
                        loading="lazy"
                      />
                    ))}
                  </div>
                ) : null}
              </div>
            ) : null}

            {/* Playtime stats */}
            <div className="mt-8 bg-white/[0.02] border border-white/[0.05] rounded-2xl overflow-hidden text-center">
              <div className="grid grid-cols-4 divide-x divide-white/[0.04]">
                <Stat label={t("gamedetail.total_playtime")} value={total.value} sub={total.sub} />
                <Stat label={t("gamedetail.last_2_weeks")} value={last2w.value} sub={last2w.sub} />
                <Stat
                  label={t("gamedetail.last_played_label")}
                  value={relativeFromNow(summary?.last_played ?? null)}
                  sub=""
                />
                <Stat
                  label={t("gamedetail.sessions")}
                  value={String(summary?.sessions ?? 0)}
                  sub=""
                />
              </div>
            </div>

            {/* Achievements — Steam + GOG, silent when the game has no
                stats. Mirrors the aidesigner mockup at
                SteamShelf-ui-mockup/.aidesigner/runs/...-achievements-block.
                Loading state = a single quiet skeleton line. Loaded
                state = header + progress bar + 6×2 icon grid + show-all
                + last-unlocked. */}
            {game.source === "steam" || game.source === "gog" ? (
              <AchievementsBlock
                achievements={achievements}
                loading={achievementsLoading}
              />
            ) : null}
            </>
            )}

            {activeSection === "artworks" && (
            <>
            {/* Artwork tabs (UI shell only — picker not wired yet) */}
            <section className="mt-6">
              <div className="flex items-end justify-between mb-4">
                <h2 className="text-xl font-semibold tracking-tight text-text-main">
                  {t("gamedetail.artwork_config")}
                </h2>
                <a
                  href="#"
                  className="text-[10px] text-text-muted hover:text-text-sec transition-colors flex items-center gap-1"
                  onClick={(e) => e.preventDefault()}
                >
                  <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                    <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
                    <polyline points="15 3 21 3 21 9" />
                    <line x1="10" y1="14" x2="21" y2="3" />
                  </svg>
                  SteamGridDB
                </a>
              </div>

              <div className="flex items-center gap-6 border-b border-white/[0.06] mb-5">
                {ARTWORK_TABS
                  // Steam doesn't allow overriding icons for its own native
                  // games (the small icon comes from Steam's CDN-refreshed
                  // appcache, not from the user-grid override). SARM hits
                  // the same limit (`handle_changes.rs::filter_paths` keeps
                  // the old path for `Icon && !is_shortcut`). For shortcuts
                  // the icon override works via the `icon` field in
                  // shortcuts.vdf — that tab stays visible.
                  .filter((tabId) => !(tabId === "Icon" && game.source === "steam"))
                  .map((tabId) => {
                    // Keep the internal `ArtTab` discriminant in English (used
                    // as a dictionary key elsewhere) but render a localized
                    // label.
                    const tabLabel =
                      tabId === "Library Capsule"
                        ? t("gamedetail.tab_capsule")
                        : tabId === "Hero"
                          ? t("gamedetail.tab_hero")
                          : tabId === "Logo"
                            ? t("gamedetail.tab_logo")
                            : t("gamedetail.tab_icon");
                    return (
                      <button
                        key={tabId}
                        onClick={() => setTab(tabId)}
                        className={`pb-3 border-b-2 text-[13px] transition-colors ${
                          tab === tabId
                            ? "border-accent text-white font-semibold"
                            : "border-transparent text-text-muted font-medium hover:text-text-main"
                        }`}
                      >
                        {tabLabel}
                      </button>
                    );
                  })}
              </div>

              <div className="flex items-center justify-between mb-4 gap-4 flex-wrap">
                <p className="text-[12px] text-text-sec shrink">
                  {t("gamedetail.click_thumbnail", {
                    kind: t(
                      tab === "Library Capsule"
                        ? "gamedetail.apply_artwork_kind.capsule"
                        : tab === "Hero"
                          ? "gamedetail.apply_artwork_kind.hero"
                          : tab === "Logo"
                            ? "gamedetail.apply_artwork_kind.logo"
                            : "gamedetail.apply_artwork_kind.icon"
                    ),
                  })}
                </p>
                <div className="flex items-center gap-2 shrink-0">
                  <FilterPill
                    label={t("gamedetail.static")}
                    count={counts.static}
                    active={showStatic}
                    onClick={() => setShowStatic((v) => !v)}
                    accent="white"
                  />
                  <FilterPill
                    label={t("gamedetail.animated")}
                    count={counts.animated}
                    active={showAnimated}
                    onClick={() => setShowAnimated((v) => !v)}
                    accent="fuchsia"
                  />
                  <FilterPill
                    label={t("gamedetail.nsfw")}
                    count={counts.nsfw}
                    active={showNsfw}
                    onClick={() => setShowNsfw((v) => !v)}
                    accent="red"
                  />
                  <div className="w-px h-5 bg-white/10 mx-1" />
                  <button
                    onClick={() => void loadOptions(tab)}
                    disabled={loadingOptions}
                    className="text-[11px] font-medium text-text-muted hover:text-white transition-colors disabled:opacity-50"
                  >
                    {loadingOptions ? t("common.loading") : t("gamedetail.refresh")}
                  </button>
                </div>
              </div>

              {optionsError && (
                <div className="mb-4 p-3 rounded-lg border border-accent/30 bg-accent/5 text-[12px] text-accent">
                  {optionsError}
                </div>
              )}

              {loadingOptions && !optionsError && (
                <div className="text-[12px] text-text-muted">Loading from SteamGridDB…</div>
              )}

              {!loadingOptions && !optionsError && visibleOptions.length === 0 && (
                <div className="text-[12px] text-text-muted">
                  {options.length > 0
                    ? "No results match the current filters — toggle a pill to see more."
                    : `No ${tab.toLowerCase()}s found for this title on SteamGridDB.`}
                </div>
              )}

              <div
                className={
                  tab === "Hero"
                    ? "grid grid-cols-2 gap-3"
                    : tab === "Logo"
                    ? "grid grid-cols-3 gap-3"
                    : tab === "Icon"
                    ? "grid grid-cols-6 gap-3"
                    : "grid grid-cols-4 gap-3"
                }
              >
                {visibleOptions.map((opt) => {
                  const isCurrent = currentUrlByTab[tab] === opt.url;
                  const isApplying = applying === opt.url;
                  const aspect =
                    tab === "Hero"
                      ? "aspect-[96/31]"
                      : tab === "Logo"
                      ? "aspect-[16/9]"
                      : tab === "Icon"
                      ? "aspect-square"
                      : "aspect-[2/3]";
                  // Force GPU decode for animated artworks when SGDB serves
                  // them as a real video container (.webm/.mp4). Chromium
                  // gives those to the VP9/H.264 hardware decoder, which
                  // means ~0% CPU even with 10 tiles autoplaying. Animated
                  // .webp / .gif fall back to <img> because the browser
                  // decodes those in software (no HW path exists in
                  // Chromium for animated image formats).
                  const fullLower = opt.url.toLowerCase().split("?")[0];
                  const thumbLower = opt.thumb.toLowerCase().split("?")[0];
                  // Prefer the full url when it's a hardware-friendly video
                  // container — SGDB sometimes serves the thumb as animated
                  // webp but the full as webm.
                  const videoSrc = fullLower.endsWith(".webm") || fullLower.endsWith(".mp4")
                    ? opt.url
                    : thumbLower.endsWith(".webm") || thumbLower.endsWith(".mp4")
                    ? opt.thumb
                    : null;
                  return (
                    <button
                      key={opt.thumb}
                      onClick={() => void applyArtwork(opt)}
                      disabled={!!applying}
                      className={`relative w-full ${aspect} rounded-lg overflow-hidden shadow-ambient bg-black/30 transition-all
                        ${
                          isCurrent
                            ? "border-2 border-accent shadow-premium"
                            : "border border-white/[0.08] hover:border-white/30 hover:scale-[1.02]"
                        }
                        ${applying && !isApplying ? "opacity-40" : ""}`}
                      style={{ transitionTimingFunction: "cubic-bezier(0.2,0.8,0.2,1)" }}
                    >
                      {videoSrc ? (
                        <video
                          src={videoSrc}
                          autoPlay
                          loop
                          muted
                          playsInline
                          preload="metadata"
                          className={
                            tab === "Logo" || tab === "Icon"
                              ? "absolute inset-0 w-full h-full object-contain p-2"
                              : "absolute inset-0 w-full h-full object-cover"
                          }
                          // GPU compositing hint — keeps the decoded frames
                          // on their own layer so paint stays cheap when
                          // scrolling the grid.
                          style={{ willChange: "transform" }}
                        />
                      ) : (
                        <img
                          src={opt.thumb}
                          alt=""
                          loading="lazy"
                          decoding="async"
                          onError={(e) => {
                            // Some SGDB thumbs 404 or fail to decode (esp. webm).
                            // Fallback to the full URL.
                            const el = e.currentTarget as HTMLImageElement;
                            if (el.dataset.fallback !== "1") {
                              el.dataset.fallback = "1";
                              el.src = opt.url;
                            } else {
                              el.style.display = "none";
                              (el.parentElement as HTMLElement | null)
                                ?.classList.add("art-broken");
                            }
                          }}
                          className={
                            tab === "Logo" || tab === "Icon"
                              ? "absolute inset-0 w-full h-full object-contain p-2"
                              : "absolute inset-0 w-full h-full object-cover"
                          }
                        />
                      )}
                      {/* Badges — always visible so the user can scan the grid */}
                      <div className="absolute top-2 left-2 z-20 flex items-center gap-1">
                        {isCurrent && (
                          <span className="px-1.5 py-0.5 rounded bg-accent text-white text-[9px] font-bold tracking-wider uppercase">
                            {t("gamedetail.current")}
                          </span>
                        )}
                        {opt.is_animated ? (
                          <span className="px-1.5 py-0.5 rounded bg-fuchsia-500/90 text-white text-[9px] font-bold tracking-wider uppercase">
                            {t("gamedetail.animated")}
                          </span>
                        ) : (
                          <span className="px-1.5 py-0.5 rounded bg-white/10 backdrop-blur text-white/80 text-[9px] font-semibold tracking-wider uppercase">
                            {t("gamedetail.static")}
                          </span>
                        )}
                        {opt.nsfw && (
                          <span className="px-1.5 py-0.5 rounded bg-red-500/90 text-white text-[9px] font-bold tracking-wider uppercase">
                            NSFW
                          </span>
                        )}
                      </div>
                      {isApplying && (
                        <div className="absolute inset-0 bg-black/60 flex items-center justify-center">
                          <div className="text-[10px] font-bold text-white uppercase tracking-wider">
                            {t("gamedetail.applying")}
                          </div>
                        </div>
                      )}
                    </button>
                  );
                })}
              </div>
            </section>
            </>
            )}

            {activeSection === "tech" && (
            <div className="mt-6 space-y-5">
              <FormRow label={t("gamedetail.install_path")}>
                <ReadOnlyField value={game.install_path ?? ""} />
              </FormRow>
              <FormRow label={t("gamedetail.launches")}>
                <ReadOnlyField value={game.exe_path} />
              </FormRow>
              <FormRow label={t("gamedetail.command")}>
                <ReadOnlyField value={game.launch_command ?? ""} />
              </FormRow>
              {shortcut?.steam_appid ? (
                <FormRow label={t("gamedetail.steam_appid")}>
                  <ReadOnlyField value={String(shortcut.steam_appid)} />
                </FormRow>
              ) : null}
            </div>
            )}
          </div>
        </div>

        {/* Pinned bottom action bar */}
        <div className="shrink-0 w-full bg-surface/80 backdrop-blur-xl border-t border-white/[0.06] p-4 flex items-center justify-between shadow-[0_-10px_20px_-5px_rgba(0,0,0,0.3)] z-30">
          <button className="px-3 py-2 flex items-center gap-2 text-[13px] font-medium text-text-sec hover:text-white transition-colors rounded hover:bg-white/5">
            <svg
              width="14"
              height="14"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2.5"
              strokeLinecap="round"
              strokeLinejoin="round"
              style={{ color: dot }}
            >
              <circle cx="12" cy="12" r="10" />
              <path d="M12 8v8" />
              <path d="M8 12h8" />
            </svg>
            {t("gamedetail.open_in", { source: longName })}
          </button>
          <div className="flex items-center gap-3 flex-1 justify-end">
            {hasActiveDownload && game.id && download ? (
              // Inline progress card replaces the generic CTA — coral when
              // downloading, neutral grey when paused. Mirrors Pass 9 mockup.
              <DownloadInlineCard
                download={download}
                source={source}
                onPause={() => void pauseDl(game.id!)}
                onResume={() => void resumeDl(game.id!)}
                onCancel={() => void cancelDl(game.id!)}
              />
            ) : isSteamNative && canInstall ? (
              // Steam-owned but not yet installed → delegate to Steam itself
              // via the steam://install/<appid> protocol URI. Steam shows its
              // own install confirmation dialog.
              <CoralButton
                variant="primary"
                size="md"
                onClick={handleInstall}
                disabled={busy}
              >
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                  <polyline points="7 10 12 15 17 10" />
                  <line x1="12" y1="15" x2="12" y2="3" />
                </svg>
                {busy ? t("gamedetail.opening_steam") : t("gamedetail.install_via_steam")}
              </CoralButton>
            ) : isSteamNative ? (
              // Native Steam game already installed — no shortcut needed.
              <span className="px-4 py-2 rounded-xl bg-white/[0.04] border border-white/[0.06] text-text-sec text-[13px] font-medium">
                {t("gamedetail.on_steam")}
              </span>
            ) : inSteam ? (
              <>
                {canUninstall && (
                  <button
                    type="button"
                    onClick={handleUninstall}
                    disabled={busy}
                    title={t("gamedetail.delete_game_files")}
                    className="h-9 px-3 inline-flex items-center gap-1.5 rounded-md bg-white/[0.03] hover:bg-white/[0.06] border border-white/[0.08] text-text-sec hover:text-white text-[12px] font-medium transition-colors disabled:opacity-40"
                  >
                    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                      <polyline points="3 6 5 6 21 6" />
                      <path d="M19 6l-2 14a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2L5 6" />
                      <path d="M10 11v6M14 11v6" />
                      <path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2" />
                    </svg>
                    {t("gamedetail.uninstall")}
                  </button>
                )}
                <CoralButton
                  variant="danger-outline"
                  size="md"
                  onClick={handleRemove}
                  disabled={busy}
                >
                  {busy ? t("gamedetail.removing") : t("gamedetail.remove_from_steam")}
                </CoralButton>
              </>
            ) : (
              <>
                {canUninstall && (
                  <button
                    type="button"
                    onClick={handleUninstall}
                    disabled={busy}
                    title={t("gamedetail.delete_game_files")}
                    className="h-9 px-3 inline-flex items-center gap-1.5 rounded-md bg-white/[0.03] hover:bg-white/[0.06] border border-white/[0.08] text-text-sec hover:text-white text-[12px] font-medium transition-colors disabled:opacity-40"
                  >
                    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                      <polyline points="3 6 5 6 21 6" />
                      <path d="M19 6l-2 14a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2L5 6" />
                      <path d="M10 11v6M14 11v6" />
                      <path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2" />
                    </svg>
                    {t("gamedetail.uninstall")}
                  </button>
                )}
                {canInstall ? (
                  <CoralButton
                    variant="primary"
                    size="md"
                    onClick={handleInstall}
                    disabled={busy}
                  >
                    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                      <polyline points="7 10 12 15 17 10" />
                      <line x1="12" y1="15" x2="12" y2="3" />
                    </svg>
                    {busy ? t("gamedetail.starting") : t("gamedetail.install")}
                  </CoralButton>
                ) : (
                  <CoralButton
                    variant="primary"
                    size="md"
                    onClick={handlePush}
                    disabled={busy}
                  >
                    {busy ? t("gamedetail.adding") : t("gamedetail.push_to_steam")}
                  </CoralButton>
                )}
              </>
            )}
          </div>
        </div>
      </div>
      {userTagsModalOpen ? (
        <UserTagsModal
          gameId={game.id!}
          title={game.title}
          initial={userTags}
          onClose={() => setUserTagsModalOpen(false)}
          onSave={(saved) => {
            setUserTags(saved);
            setUserTagsModalOpen(false);
          }}
        />
      ) : null}
      {playtimeModalOpen ? (
        <div
          className="fixed inset-0 z-[100] flex items-center justify-center bg-black/60 backdrop-blur-sm pointer-events-auto"
          onClick={(e) => {
            if (e.target === e.currentTarget) setPlaytimeModalOpen(false);
          }}
        >
          <div className="acrylic border border-white/[0.08] rounded-2xl shadow-premium w-[420px] max-w-[90vw] p-6 flex flex-col gap-4">
            <div className="flex flex-col gap-1">
              <h2 className="text-[18px] font-semibold text-white tracking-tight">{t("gamedetail.set_playtime_title")}</h2>
              <p className="text-[12px] text-text-sec">
                Total hours for <span className="text-white font-medium">{game.title}</span>. Useful when the log-based import undershoots (Star Citizen rotates its <span className="font-mono text-[11px] bg-white/[0.04] px-1 rounded">logbackups/</span> so older sessions get lost).
              </p>
            </div>
            <div className="flex items-center gap-2">
              <input
                type="number"
                inputMode="decimal"
                step="0.1"
                min="0"
                value={playtimeDraft}
                autoFocus
                onChange={(e) => setPlaytimeDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void commitPlaytime();
                  else if (e.key === "Escape") setPlaytimeModalOpen(false);
                }}
                className="flex-1 bg-white/[0.04] border border-white/[0.08] focus:border-accent/60 rounded-lg px-3 py-2 text-[14px] text-white font-mono outline-none transition-colors"
                placeholder="150"
              />
              <span className="text-[13px] text-text-sec font-medium">hours</span>
            </div>
            <div className="flex items-center justify-end gap-2 mt-2">
              <button
                type="button"
                onClick={() => setPlaytimeModalOpen(false)}
                className="px-3 py-1.5 rounded-md text-[12px] font-medium text-text-sec hover:text-white bg-white/[0.02] hover:bg-white/[0.05] border border-white/[0.06] transition-colors"
              >
                Cancel
              </button>
              <CoralButton variant="primary" size="sm" onClick={() => void commitPlaytime()}>
                Save
              </CoralButton>
            </div>
            <p className="text-[10px] text-text-muted leading-snug">
              After saving, run <span className="font-medium text-text-sec">Settings → Playtime tracking → Sync to Steam</span> with Steam closed to push the new total.
            </p>
          </div>
        </div>
      ) : null}
    </div>
  );
}

// Inline download progress card — replaces the bottom CTA while a game is
// downloading / paused / queued. Visual = port of AIDesigner Pass 9 mockup.
function DownloadInlineCard({
  download,
  source,
  onPause,
  onResume,
  onCancel,
}: {
  download: { status: string; progress_pct: number; downloaded_bytes: number; total_bytes: number; speed_bps: number; eta_secs: number; last_error?: string | null };
  source: string;
  onPause: () => void;
  onResume: () => void;
  onCancel: () => void;
}) {
  // Steam owns its downloader (no IPC for pause/cancel/resume). We render
  // the same visual progress as Epic/GOG but swap the action button row
  // for a small footer hint.
  const isSteam = source === "steam";
  const isDownloading = download.status === "downloading";
  const isPaused = download.status === "paused";
  const isQueued = download.status === "queued";
  const label = isDownloading
    ? "DOWNLOADING"
    : isPaused
    ? "PAUSED"
    : isQueued
    ? "QUEUED"
    : download.status.toUpperCase();
  const pct = Math.max(0, Math.min(100, Math.round(download.progress_pct)));
  const downloadedGb = (download.downloaded_bytes / (1024 ** 3)).toFixed(2);
  const totalGb = (download.total_bytes / (1024 ** 3)).toFixed(2);
  const speedMb = (download.speed_bps / (1024 * 1024)).toFixed(1);
  const eta = formatEta(download.eta_secs);
  const barColor = isDownloading ? "bg-accent" : "bg-white/[0.25]";
  const barShadow = isDownloading
    ? "shadow-[0_0_12px_rgba(255,70,51,0.5)]"
    : "";
  const labelColor = isDownloading ? "text-accent" : "text-text-muted";

  return (
    <div className="w-[440px] bg-black/25 border border-white/[0.06] rounded-[14px] p-4">
      {/* Top status row */}
      <div className="flex justify-between items-end mb-3">
        <span className={`text-[10px] font-bold tracking-[0.15em] uppercase ${labelColor}`}>
          {label}
        </span>
        <span className="text-[20px] font-semibold text-white tracking-tight leading-none">
          {pct}%
        </span>
      </div>

      {/* Progress track */}
      <div className="w-full h-[6px] rounded-full bg-white/[0.06] overflow-hidden relative mb-3">
        <div
          className={`absolute top-0 left-0 bottom-0 rounded-full ${barColor} ${barShadow}`}
          style={{ width: `${pct}%`, transition: "width 0.5s cubic-bezier(0.2,0.8,0.2,1)" }}
        />
      </div>

      {/* Meta mono row */}
      <div className="text-[11px] font-mono text-text-muted flex justify-between tracking-tight mb-4">
        <span>{downloadedGb} GB / {totalGb} GB</span>
        <span>
          {isDownloading ? `${speedMb} MB/s · ${eta} left` : isPaused ? `last speed: ${speedMb} MB/s` : "queued"}
        </span>
      </div>

      {/* Action buttons — Steam owns its downloader, so for source==='steam'
          we replace the Pause / Cancel row with a small footer hint. */}
      {isSteam ? (
        <div className="text-[11px] text-text-muted/80 leading-snug">
          Pause / Cancel must be done in Steam — Tokoru only mirrors progress.
        </div>
      ) : (
        <div className="flex items-center gap-2">
          {isDownloading ? (
            <button
              onClick={onPause}
              className="flex-1 max-w-[150px] h-[34px] bg-accent/5 hover:bg-accent/15 border border-accent/70 text-accent rounded-md font-semibold text-[12px] flex items-center justify-center gap-2 transition-colors active:scale-[0.98]"
            >
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                <rect x="6" y="4" width="4" height="16" />
                <rect x="14" y="4" width="4" height="16" />
              </svg>
              Pause
            </button>
          ) : (
            <button
              onClick={onResume}
              className="flex-1 max-w-[150px] h-[34px] bg-accent hover:bg-accent-hover text-white rounded-md shadow-glow font-semibold text-[12px] flex items-center justify-center gap-2 transition-all active:scale-[0.98]"
            >
              <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor">
                <polygon points="5 3 19 12 5 21 5 3" />
              </svg>
              Resume
            </button>
          )}
          <button
            onClick={onCancel}
            className="flex-1 h-[34px] hover:bg-white/[0.04] text-text-sec hover:text-white rounded-md font-medium text-[12px] flex items-center justify-center gap-1.5 transition-colors active:scale-[0.98]"
          >
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
            Cancel
          </button>
        </div>
      )}
    </div>
  );
}

function formatEta(seconds: number): string {
  if (!seconds || seconds <= 0) return "—";
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.floor(seconds % 60);
  if (h > 0) return `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

function relativeFromUnix(unix: number): string {
  if (!unix) return "—";
  const diff = Math.max(0, Math.floor(Date.now() / 1000) - unix);
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)} min ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  if (diff < 86400 * 14) return `${Math.floor(diff / 86400)} days ago`;
  return new Date(unix * 1000).toLocaleDateString();
}

/// Steam-style achievements panel. Renders nothing when the game has
/// no achievements (Steam reported "no stats") — matches the mockup
/// brief that the empty state is silent. Loading shows a single quiet
/// skeleton line so the panel doesn't jank on first open.
function AchievementsBlock({
  achievements,
  loading,
}: {
  achievements: AchievementsView | null;
  loading: boolean;
}) {
  const { t } = useTranslation();
  const [showAll, setShowAll] = useState(false);
  if (loading && !achievements) {
    return (
      <div className="mt-8 flex items-center gap-3 text-[12px] text-text-sec">
        <span className="w-3 h-3 rounded-full border-2 border-accent border-t-transparent animate-spin" />
        {t("gamedetail.loading_achievements")}
      </div>
    );
  }
  if (!achievements || achievements.total === 0) {
    return null;
  }
  const pct = Math.round((achievements.unlocked / achievements.total) * 100);
  // Preview slice — up to 12 (6 cols × 2 rows). Unlocked first so the
  // grid leads with progress, then locked fills the remaining slots.
  // When `showAll` is on we render the full list instead, in a more
  // detailed list-style layout (icon + name + status).
  const ordered = [
    ...achievements.items.filter((a) => a.achieved),
    ...achievements.items.filter((a) => !a.achieved),
  ];
  const preview = ordered.slice(0, 12);
  // Last unlocked = the achievement with the largest unlocktime.
  const lastUnlocked = achievements.items
    .filter((a) => a.achieved && a.unlocktime > 0)
    .sort((a, b) => b.unlocktime - a.unlocktime)[0];

  return (
    <div className="mt-8 flex flex-col">
      {/* Header row */}
      <div className="flex items-end justify-between w-full">
        <div className="flex flex-col gap-0.5">
          <h3 className="text-[18px] font-semibold tracking-tight text-text-main leading-none">
            {t("gamedetail.achievements")}
          </h3>
          <span className="text-[12px] font-medium text-text-muted">
            {t("gamedetail.achievements_count", { unlocked: achievements.unlocked, total: achievements.total })}
          </span>
        </div>
        <div
          className="px-2.5 py-1 rounded-full bg-white/[0.02] border border-white/[0.06] text-accent text-[12px] font-semibold leading-none translate-y-[2px]"
          style={{ boxShadow: "inset 0 0 12px rgba(255, 70, 51, 0.15)" }}
        >
          {pct}%
        </div>
      </div>

      {/* Progress bar with last-unlock marker — small floating chip with
          the achievement name sliding at the % position, matching the
          mockup. Pinned to the bar position, clamped 4-92% so the chip
          stays readable at the edges. */}
      <div className="w-full mt-6 relative">
        {lastUnlocked ? (
          <div
            className="absolute -top-5 transform -translate-x-1/2 pointer-events-none"
            style={{ left: `${Math.min(92, Math.max(4, pct))}%` }}
          >
            <div
              className="px-1.5 py-0.5 rounded-[5px] bg-accent text-white text-[10px] font-bold uppercase tracking-wide leading-none whitespace-nowrap shadow-[0_2px_8px_rgba(255,70,51,0.4)]"
            >
              {lastUnlocked.name || lastUnlocked.apiname}
            </div>
          </div>
        ) : null}
        <div className="w-full h-[5px] rounded-full bg-white/[0.06] relative overflow-hidden">
          <div
            className="absolute top-0 left-0 h-full bg-accent rounded-full transition-all duration-700"
            style={{
              width: `${pct}%`,
              boxShadow: "0 0 12px rgba(255, 70, 51, 0.45)",
            }}
          />
        </div>
      </div>

      {/* Icon grid (max 12) — Steam's own icons via the community CDN.
          Unlocked uses `icon` (full color); locked uses `icon_gray`
          plus a 40% dim + lock badge to match the mockup. */}
      <div className="mt-6 grid grid-cols-6 gap-[10px]">
        {preview.map((a, i) => {
          const src = a.achieved ? a.icon : a.icon_gray;
          return (
            <div
              key={a.apiname || i}
              className={`relative w-12 h-12 rounded-[10px] ring-1 ring-inset overflow-hidden flex items-center justify-center cursor-default transition-transform duration-200 ${
                a.achieved
                  ? "ring-white/[0.08] hover:scale-[1.04] hover:z-10 group"
                  : "ring-white/[0.04] opacity-40"
              }`}
              style={{
                background: a.achieved
                  ? undefined
                  : "linear-gradient(135deg, #2a2a2e 0%, #1a1a1d 100%)",
                boxShadow: a.achieved
                  ? "0 0 14px rgba(255, 70, 51, 0.18), 0 2px 8px rgba(0,0,0,0.4)"
                  : undefined,
              }}
              title={a.achieved ? a.name : `${a.name} (locked)`}
            >
              {src ? (
                <img
                  src={src}
                  alt=""
                  className="w-full h-full object-cover"
                  loading="lazy"
                  onError={(e) => {
                    (e.currentTarget as HTMLImageElement).style.display = "none";
                  }}
                />
              ) : null}
              {!a.achieved ? (
                <div className="absolute bottom-1 right-1 bg-black/60 backdrop-blur-sm rounded-[4px] p-[2px] border border-white/[0.05] z-10">
                  <svg width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" className="text-text-muted">
                    <rect x="3" y="11" width="18" height="11" rx="2" ry="2" />
                    <path d="M7 11V7a5 5 0 0 1 10 0v4" />
                  </svg>
                </div>
              ) : null}
              {a.achieved ? (
                <div className="absolute inset-0 rounded-[10px] ring-1 ring-inset ring-white/[0.2] opacity-0 group-hover:opacity-100 transition-opacity pointer-events-none" />
              ) : null}
            </div>
          );
        })}
      </div>

      {/* Full list — shown only when the user clicked "Show all". List
          layout (icon + name + state) so the names are actually
          readable; tiles alone don't scale past the 12-slot preview. */}
      {showAll ? (
        <div className="mt-4 max-h-[420px] overflow-y-auto pr-1 flex flex-col gap-1.5 border-t border-white/[0.04] pt-4">
          {ordered.map((a) => {
            const src = a.achieved ? a.icon : a.icon_gray;
            return (
              <div
                key={a.apiname}
                className={`flex items-center gap-3 px-2 py-2 rounded-md transition-colors ${
                  a.achieved
                    ? "bg-white/[0.02] hover:bg-white/[0.04]"
                    : "opacity-50 hover:opacity-75"
                }`}
              >
                <div className="w-10 h-10 rounded-md ring-1 ring-inset ring-white/[0.06] overflow-hidden flex-shrink-0 bg-black/30">
                  {src ? (
                    <img
                      src={src}
                      alt=""
                      className="w-full h-full object-cover"
                      loading="lazy"
                    />
                  ) : null}
                </div>
                <div className="flex-1 min-w-0">
                  <div className="text-[13px] font-semibold text-white truncate">
                    {a.name || a.apiname}
                  </div>
                  {a.description ? (
                    <div className="text-[11px] text-text-muted truncate">
                      {a.description}
                    </div>
                  ) : null}
                </div>
                <div className="flex-shrink-0 text-[10px] font-medium">
                  {a.achieved ? (
                    <span className="text-accent">
                      {a.unlocktime > 0 ? relativeFromUnix(a.unlocktime) : "Unlocked"}
                    </span>
                  ) : (
                    <span className="text-text-muted">Locked</span>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      ) : null}

      {/* Show-all + last-unlocked */}
      <div className="mt-4 flex flex-col items-start gap-1.5">
        {achievements.total > 12 ? (
          <button
            type="button"
            onClick={() => setShowAll((v) => !v)}
            className="flex items-center gap-1 text-[12px] font-medium text-accent hover:text-accent-hover transition-colors group"
          >
            {showAll
              ? "Hide details"
              : `Show all ${achievements.total} achievements`}
            <svg
              width="12"
              height="12"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2.5"
              strokeLinecap="round"
              strokeLinejoin="round"
              className={`transform transition-transform ${
                showAll ? "rotate-90" : "group-hover:translate-x-0.5"
              }`}
            >
              <polyline points="9 18 15 12 9 6" />
            </svg>
          </button>
        ) : null}
        {lastUnlocked ? (
          <div className="flex items-center text-[11px] font-medium">
            <span className="text-text-muted font-normal">Last unlocked:&nbsp;</span>
            <span className="text-text-sec">{lastUnlocked.name}</span>
            <span className="text-text-muted font-normal">
              &nbsp;·&nbsp;{relativeFromUnix(lastUnlocked.unlocktime)}
            </span>
          </div>
        ) : null}
      </div>
    </div>
  );
}

/// Single-row dropdown item used by the more-menu in the header.
/// `danger` flips the hover state to coral so destructive actions read
/// distinct from neutral ones; `disabled` greys it out and blocks the
/// click handler.
function MenuItem({
  children,
  onClick,
  disabled,
  danger,
}: {
  children: React.ReactNode;
  onClick: () => void;
  disabled?: boolean;
  danger?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={() => {
        if (!disabled) onClick();
      }}
      disabled={disabled}
      className={`w-full text-left px-3 py-2 text-[13px] font-medium transition-colors ${
        disabled
          ? "text-text-muted cursor-not-allowed opacity-50"
          : danger
          ? "text-accent hover:bg-accent/10"
          : "text-text-main hover:bg-white/[0.05] hover:text-white"
      }`}
    >
      {children}
    </button>
  );
}

function Stat({ label, value, sub }: { label: string; value: string; sub: string }) {
  return (
    <div className="py-5 px-3 flex flex-col items-center justify-center hover:bg-white/[0.02] transition-colors">
      <span className="text-[10px] uppercase tracking-[0.15em] font-bold text-text-muted mb-1.5">
        {label}
      </span>
      <span className="text-xl font-semibold text-white tracking-tight">
        {value}{" "}
        {sub && <span className="text-text-sec text-sm">{sub}</span>}
      </span>
    </div>
  );
}

function FormRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-12 gap-4 items-center">
      <label className="col-span-3 text-[11px] uppercase tracking-wider font-bold text-text-muted text-right pr-2">
        {label}
      </label>
      <div className="col-span-9">{children}</div>
    </div>
  );
}

function ReadOnlyField({ value }: { value: string }) {
  return (
    <div className="w-full bg-black/30 border border-white/[0.04] rounded-md px-3 py-2 text-[12px] font-mono text-text-sec truncate cursor-text hover:border-white/10 transition-colors">
      {value || <span className="opacity-40">—</span>}
    </div>
  );
}

/// Edit user-curated tags in a modal. Type-to-add, Enter / comma commits
/// the current draft, click a chip's × to remove it, Save persists via
/// `setGameUserTags`. The backend trims / dedupes / serializes; we just
/// build a clean array and let it canonicalise.
function UserTagsModal({
  gameId,
  title,
  initial,
  onClose,
  onSave,
}: {
  gameId: string;
  title: string;
  initial: string[];
  onClose: () => void;
  onSave: (tags: string[]) => void;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const [tags, setTags] = useState<string[]>(initial);
  const [draft, setDraft] = useState("");
  const [saving, setSaving] = useState(false);

  const commitDraft = useCallback(() => {
    const cleaned = draft.trim();
    if (!cleaned) return;
    setTags((prev) => {
      const lower = cleaned.toLowerCase();
      if (prev.some((t) => t.toLowerCase() === lower)) return prev;
      return [...prev, cleaned];
    });
    setDraft("");
  }, [draft]);

  const removeTag = (tag: string) => {
    setTags((prev) => prev.filter((t) => t !== tag));
  };

  const handleSave = async () => {
    // Flush any in-progress draft so the user doesn't lose a tag they
    // typed but forgot to hit Enter on.
    const pendingDraft = draft.trim();
    const candidate = pendingDraft
      ? (() => {
          const lower = pendingDraft.toLowerCase();
          if (tags.some((t) => t.toLowerCase() === lower)) return tags;
          return [...tags, pendingDraft];
        })()
      : tags;
    setSaving(true);
    try {
      const saved = await setGameUserTags(gameId, candidate);
      onSave(saved);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast.error(t("gamedetail.user_tags_save_failed", { error: msg }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-[100] flex items-center justify-center bg-black/60 backdrop-blur-sm pointer-events-auto"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="acrylic border border-white/[0.08] rounded-2xl shadow-premium w-[480px] max-w-[90vw] p-6 flex flex-col gap-4">
        <div className="flex flex-col gap-1">
          <h2 className="text-[18px] font-semibold text-white tracking-tight">
            {t("gamedetail.user_tags_title")}
          </h2>
          <p className="text-[12px] text-text-sec">
            {t("gamedetail.user_tags_subtitle")}{" "}
            <span className="text-white font-medium">{title}</span>
          </p>
        </div>

        <div className="flex flex-wrap gap-1.5 min-h-[36px] p-2 bg-white/[0.02] border border-white/[0.06] rounded-lg">
          {tags.length === 0 ? (
            <span className="text-[11px] text-text-muted italic px-1.5 py-1">
              {t("gamedetail.user_tags_empty_hint")}
            </span>
          ) : (
            tags.map((tag) => (
              <span
                key={tag}
                className="inline-flex items-center gap-1 pl-2.5 pr-1 py-1 rounded-full bg-accent/15 border border-accent/30 text-[11px] font-medium text-accent"
              >
                {tag}
                <button
                  type="button"
                  onClick={() => removeTag(tag)}
                  className="w-4 h-4 rounded-full hover:bg-accent/30 flex items-center justify-center transition-colors"
                  aria-label={t("gamedetail.user_tags_remove", { tag })}
                >
                  <svg width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round">
                    <line x1="18" y1="6" x2="6" y2="18" />
                    <line x1="6" y1="6" x2="18" y2="18" />
                  </svg>
                </button>
              </span>
            ))
          )}
        </div>

        <input
          type="text"
          value={draft}
          autoFocus
          maxLength={32}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === ",") {
              e.preventDefault();
              commitDraft();
            } else if (e.key === "Backspace" && draft === "" && tags.length > 0) {
              setTags((prev) => prev.slice(0, -1));
            } else if (e.key === "Escape") {
              onClose();
            }
          }}
          onBlur={commitDraft}
          placeholder={t("gamedetail.user_tags_placeholder") ?? ""}
          className="w-full bg-white/[0.04] border border-white/[0.08] focus:border-accent/60 rounded-lg px-3 py-2 text-[13px] text-white outline-none transition-colors"
        />

        <div className="flex items-center justify-end gap-2 mt-2">
          <button
            type="button"
            onClick={onClose}
            disabled={saving}
            className="px-3 py-1.5 rounded-md text-[12px] font-medium text-text-sec hover:text-white bg-white/[0.02] hover:bg-white/[0.05] border border-white/[0.06] transition-colors disabled:opacity-50"
          >
            {t("common.cancel")}
          </button>
          <CoralButton
            variant="primary"
            size="sm"
            onClick={() => void handleSave()}
            disabled={saving}
          >
            {saving ? t("common.saving") : t("common.save")}
          </CoralButton>
        </div>
      </div>
    </div>
  );
}
