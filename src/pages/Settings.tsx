import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { SUPPORTED_LOCALES } from "../i18n";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { TopBar } from "../components/TopBar";
import { Toggle } from "../components/Toggle";
import { PillGroup } from "../components/PillGroup";
import { CoralButton } from "../components/CoralButton";
import { SourceTileGrid } from "../components/SourceTile";
import {
  detectInstalls,
  getCliBinsStatus,
  getDownloadSettings,
  getLastSyncAt,
  getRestartAfterPush,
  getSteamgriddbSettings,
  redownloadGogdl,
  redownloadLegendary,
  setDownloadSettings,
  setRestartAfterPush,
  setSteamgriddbSettings,
  getCollectionsMode,
  setCollectionsMode,
  restartSteam,
  syncMetadataNow,
  getMetadataStatus,
  resetOnboardingDone,
} from "../lib/api";
import { useRouter } from "../router";
import type { CliBinsStatus, CollectionsMode, MetadataStatus } from "../lib/api";
import type { DownloadSettings, SteamGridDbArtworkStyle } from "../lib/types";
import { SOURCE_DOT, SOURCE_LONG } from "../lib/types";
import { useGames } from "../lib/useGames";
import { useSourceTiles } from "../lib/useSourceTiles";
import { useToast } from "../components/Toast";
import { useTheme, type Theme, ACCENT_PRESETS } from "../lib/useTheme";
import { triggerArtworkBackfill } from "../lib/artworkBackfill";
import { HexColorPicker, HexColorInput } from "react-colorful";

// Labels for the artwork-style toast (keep keys in sync with the PillGroup
// options below and the Rust-side `ALLOWED_STYLES` list).
const STYLE_LABELS: Record<SteamGridDbArtworkStyle, string> = {
  official: "Official",
  painted: "Painted",
  photoreal: "Photoreal",
  minimal: "Minimal",
  any: "Any",
};

// Sidebar layout — account-management is now consolidated under the
// "Sources" section (which embeds the SourceTile grid). "Steam integration"
// is kept separate because it covers the Steam install path / profile
// picker, not the account login.
// `tKey` resolves at render time so the sidebar reacts to language switches
// without any remount.
const SECTIONS: { id: string; tKey: string }[] = [
  { id: "general", tKey: "settings.sections.general" },
  { id: "steam-integration", tKey: "settings.sections.steam" },
  { id: "background", tKey: "settings.sections.background" },
  { id: "downloads", tKey: "settings.sections.downloads" },
  { id: "playtime", tKey: "settings.sections.playtime" },
  { id: "steamgriddb", tKey: "settings.sections.steamgriddb" },
  { id: "sources", tKey: "settings.sections.sources" },
  { id: "appearance", tKey: "settings.sections.appearance" },
  { id: "startup", tKey: "settings.sections.startup" },
  { id: "about", tKey: "settings.sections.about" },
];

// Per-source rows for the override table — one entry per source we have a
// downloader for (epic / gog via legendary / gogdl) or that a user might
// reasonably want to scope to a different drive even though Tokoru can't
// download them yet (ea / ubi / itch / custom). Keep in lock-step with the
// `PER_SOURCE_KEYS` array on the Rust side.
const DOWNLOAD_SOURCE_ROWS: { key: string; label: string }[] = [
  { key: "epic", label: SOURCE_LONG.epic },
  { key: "gog", label: SOURCE_LONG.gog },
  { key: "ea", label: SOURCE_LONG.ea },
  { key: "ubi", label: SOURCE_LONG.ubi },
  { key: "itch", label: SOURCE_LONG.itch },
  { key: "custom", label: SOURCE_LONG.custom },
];

const SOURCE_TILE_ORDER = [
  "steam",
  "epic",
  "gog",
  "ubi",
  "ea",
  "itch",
  "custom",
];

function formatTimestamp(unix: number | null): string {
  if (!unix) return "Never";
  const date = new Date(unix * 1000);
  return date.toLocaleString();
}

export function Settings() {
  const toast = useToast();
  const { t, i18n } = useTranslation();
  const { setRoute } = useRouter();
  const replayOnboarding = useCallback(async () => {
    try {
      await resetOnboardingDone();
      setRoute("onboarding");
    } catch (e) {
      toast.error(
        `${t("settings.general.replay_onboarding_failed")}: ${e instanceof Error ? e.message : String(e)}`,
      );
    }
  }, [setRoute, toast, t]);
  // The Language picker reads `i18n.language` (already folded to the 2-letter
  // code via `nonExplicitSupportedLngs` in i18n init). Changing it flips the
  // UI immediately AND persists to backend `sync_state` so the Steam Store /
  // GOG metadata fetchers can pick the right `Accept-Language` / `l=` param.
  const currentLocale = (i18n.resolvedLanguage ?? i18n.language ?? "en").split("-")[0];
  const changeLocale = useCallback(
    async (code: string) => {
      try {
        await i18n.changeLanguage(code);
        // Mirror to backend so Rust-side outgoing API calls (Steam Store,
        // GOG, …) use the same language for metadata localisation. The
        // command bumps `locale_changed_at` in sync_state — each
        // GameDetail open compares its `metadata_synced_at` against this
        // stamp and re-fetches its own metadata on the fly when stale.
        // Avoids hammering the Steam Store API with a 400-game batch.
        const { invoke } = await import("@tauri-apps/api/core");
        await invoke("set_app_locale", { locale: code });
      } catch (e) {
        toast.error(`Language change failed: ${e instanceof Error ? e.message : String(e)}`);
      }
    },
    [i18n, toast]
  );
  const {
    theme,
    setTheme,
    accent,
    setAccent,
    setCustomAccent,
    reduceMotion,
    setReduceMotion,
    density,
    setDensity,
  } = useTheme();
  const [customPickerOpen, setCustomPickerOpen] = useState(false);
  const [pickerPos, setPickerPos] = useState<{ top: number; left: number } | null>(null);
  const customBtnRef = useRef<HTMLButtonElement>(null);

  const openCustomPicker = useCallback(() => {
    if (customPickerOpen) {
      setCustomPickerOpen(false);
      return;
    }
    if (customBtnRef.current) {
      const rect = customBtnRef.current.getBoundingClientRect();
      const pickerWidth = 232; // 200px picker + 16px padding * 2
      // Clamp to viewport so the popover never spills off the right edge.
      const left = Math.min(rect.left, window.innerWidth - pickerWidth - 16);
      setPickerPos({ top: rect.bottom + 8, left: Math.max(16, left) });
    }
    setCustomPickerOpen(true);
  }, [customPickerOpen]);
  const [active, setActive] = useState("general");
  const [lastSync, setLastSync] = useState<number | null>(null);
  const [playtimeSyncBusy, setPlaytimeSyncBusy] = useState(false);
  const [collectionsSyncBusy, setCollectionsSyncBusy] = useState(false);
  const [collectionsMode, setCollectionsModeState] = useState<CollectionsMode>("platform");
  const [collectionsModeBusy, setCollectionsModeBusy] = useState(false);
  // Metadata enrichment — Steam Store + SteamSpy + IGDB + HLTB + Wikidata.
  // IGDB credentials live in a local config file (see `services::igdb_api`);
  // when missing, IGDB is silently skipped and the other 4 sources still run.
  const [metadataSyncBusy, setMetadataSyncBusy] = useState(false);
  // Snapshot of metadata coverage. Refreshed on mount + after every sync
  // pass. Used to gate the "Rebuild Steam Collections" button in Franchise
  // mode — rebuilding before the sync has filled franchise/tags/developer
  // columns gives a degraded grouping (only the title-prefix passes run).
  const [metadataStatus, setMetadataStatus] = useState<MetadataStatus | null>(null);

  // Library state — drives the source tile game counts.
  const { refresh: refreshGames } = useGames();
  const { handlersFor: tileHandlersFor } = useSourceTiles(refreshGames);

  const settingsTileEntries = useMemo(() => {
    return SOURCE_TILE_ORDER.map((src) => {
      const h = tileHandlersFor(src);
      if (!h) return null;
      return {
        source: src,
        state: h.state,
        busy: h.busy,
        onConnect: h.state.supports_login ? h.onConnect : undefined,
        onSync:
          h.state.supports_login && h.state.connected ? h.onSync : undefined,
        onDisconnect:
          h.state.supports_login && h.state.connected
            ? h.onDisconnect
            : undefined,
        onRescan: !h.state.supports_login ? h.onRescan : undefined,
      };
    }).filter((e): e is NonNullable<typeof e> => e !== null);
  }, [tileHandlersFor]);

  // toggles
  const [restoreView, setRestoreView] = useState(true);
  const [alwaysRestart, setAlwaysRestartRaw] = useState(getRestartAfterPush());
  const setAlwaysRestart = (v: boolean) => {
    setAlwaysRestartRaw(v);
    setRestartAfterPush(v);
  };
  const [backupVdf, setBackupVdf] = useState(true);
  const [runBg, setRunBg] = useState(true);
  const [tray, setTray] = useState(true);
  const [trackBg, setTrackBg] = useState(true);
  const [syncToSteam, setSyncToSteam] = useState(true);
  const [startWindows, setStartWindows] = useState(true);
  const [startMin, setStartMin] = useState(true);
  const [autoUpdate, setAutoUpdate] = useState(true);

  const [polling, setPolling] = useState(15);
  const [detection, setDetection] = useState<"window" | "tree" | "hybrid">("hybrid");

  // ---- SteamGridDB section state (hydrated from backend on mount) ----
  // `apiKey` is the user-supplied key — empty string means "fall back to the
  // bundled default" on the backend. `apiKeyDraft` is the live input value
  // (committed on blur). `apiKeySavedAt` drives the "Last validated · {date}"
  // subtitle. `apiKeyShown` toggles the password-mask via the eye icon — we
  // also reveal automatically as soon as the user types.
  const [apiKey, setApiKey] = useState("");
  const [apiKeyDraft, setApiKeyDraft] = useState("");
  const [apiKeyShown, setApiKeyShown] = useState(false);
  const [apiKeySavedAt, setApiKeySavedAt] = useState(0);
  const [hasCustomKey, setHasCustomKey] = useState(false);
  const [artStyle, setArtStyle] = useState<SteamGridDbArtworkStyle>("any");
  const [autoFetch, setAutoFetch] = useState(true);
  const [preferAnimated, setPreferAnimated] = useState(false);
  const [allowNsfw, setAllowNsfw] = useState(false);

  // ---- Downloads section state ----
  // `dlSettings` mirrors `services::downloads::DownloadSettings` on the Rust
  // side; persisted via `set_download_settings` on every change. We also
  // keep `dlMaxParallelDraft` as a separate string for the controlled
  // number input — the input fires onChange on every keystroke including
  // "" while the user is mid-type, so we don't want to immediately persist
  // the empty string. The committed value lives in `dlSettings.max_parallel`.
  const [dlSettings, setDlSettings] = useState<DownloadSettings | null>(null);
  const [dlMaxParallelDraft, setDlMaxParallelDraft] = useState("2");
  const [dlBandwidthDraft, setDlBandwidthDraft] = useState("0");
  const [cliStatus, setCliStatus] = useState<CliBinsStatus | null>(null);
  const [cliBusy, setCliBusy] = useState<"legendary" | "gogdl" | null>(null);
  const [detectBusy, setDetectBusy] = useState(false);

  useEffect(() => {
    void getLastSyncAt()
      .then(setLastSync)
      .catch((e) => console.error("Settings: get_last_sync_at failed", e));
    void getSteamgriddbSettings()
      .then((s) => {
        setApiKey(s.api_key);
        setApiKeyDraft(s.api_key);
        setApiKeySavedAt(s.api_key_saved_at);
        setHasCustomKey(s.has_custom_key);
        setArtStyle(s.artwork_style);
        setAutoFetch(s.auto_fetch);
        setPreferAnimated(s.prefer_animated);
        setAllowNsfw(s.allow_nsfw);
      })
      .catch((e) =>
        console.error("Settings: get_steamgriddb_settings failed", e)
      );
    void getDownloadSettings()
      .then((s) => {
        setDlSettings(s);
        setDlMaxParallelDraft(String(s.max_parallel));
        setDlBandwidthDraft(String(s.bandwidth_kbps));
      })
      .catch((e) =>
        console.error("Settings: get_download_settings failed", e)
      );
    void getCliBinsStatus()
      .then(setCliStatus)
      .catch((e) =>
        console.error("Settings: get_cli_bins_status failed", e)
      );
  }, []);

  // ---- Downloads save handlers ----
  // Each setter optimistically updates local state, calls the backend, and
  // rolls back on error. Toast messages are intentionally short — Settings
  // already has a lot of small persistence chatter.

  const saveDlDefaultDir = useCallback(
    async (value: string) => {
      if (!dlSettings) return;
      const previous = dlSettings;
      setDlSettings({ ...dlSettings, default_dir: value });
      try {
        const updated = await setDownloadSettings({ defaultDir: value });
        setDlSettings(updated);
      } catch (e) {
        setDlSettings(previous);
        toast.error(`Failed to save install directory: ${
          e instanceof Error ? e.message : String(e)
        }`);
      }
    },
    [dlSettings, toast]
  );

  const saveDlPerSource = useCallback(
    async (next: Record<string, string>) => {
      if (!dlSettings) return;
      const previous = dlSettings;
      // Clean empty strings client-side so the table doesn't persist blanks
      // that bypass the "Inherit default" fallback on read.
      const cleaned = Object.fromEntries(
        Object.entries(next).filter(([, v]) => v && v.trim().length > 0)
      );
      setDlSettings({ ...dlSettings, per_source: cleaned });
      try {
        const updated = await setDownloadSettings({ perSource: cleaned });
        setDlSettings(updated);
      } catch (e) {
        setDlSettings(previous);
        toast.error(`Failed to save per-source override: ${
          e instanceof Error ? e.message : String(e)
        }`);
      }
    },
    [dlSettings, toast]
  );

  const saveDlMaxParallel = useCallback(
    async (value: number) => {
      if (!dlSettings) return;
      const clamped = Math.max(1, Math.min(5, Math.round(value || 1)));
      const previous = dlSettings;
      setDlSettings({ ...dlSettings, max_parallel: clamped });
      setDlMaxParallelDraft(String(clamped));
      try {
        const updated = await setDownloadSettings({ maxParallel: clamped });
        setDlSettings(updated);
      } catch (e) {
        setDlSettings(previous);
        setDlMaxParallelDraft(String(previous.max_parallel));
        toast.error(`Failed to save max parallel: ${
          e instanceof Error ? e.message : String(e)
        }`);
      }
    },
    [dlSettings, toast]
  );

  const saveDlAutoPush = useCallback(
    async (value: boolean) => {
      if (!dlSettings) return;
      const previous = dlSettings;
      setDlSettings({ ...dlSettings, auto_push_after_download: value });
      try {
        const updated = await setDownloadSettings({
          autoPushAfterDownload: value,
        });
        setDlSettings(updated);
      } catch (e) {
        setDlSettings(previous);
        toast.error(`Failed to toggle auto-push: ${
          e instanceof Error ? e.message : String(e)
        }`);
      }
    },
    [dlSettings, toast]
  );

  const saveDlBandwidth = useCallback(
    async (value: number) => {
      if (!dlSettings) return;
      const clamped = Math.max(0, Math.round(value || 0));
      const previous = dlSettings;
      setDlSettings({ ...dlSettings, bandwidth_kbps: clamped });
      setDlBandwidthDraft(String(clamped));
      try {
        const updated = await setDownloadSettings({ bandwidthKbps: clamped });
        setDlSettings(updated);
      } catch (e) {
        setDlSettings(previous);
        setDlBandwidthDraft(String(previous.bandwidth_kbps));
        toast.error(`Failed to save bandwidth limit: ${
          e instanceof Error ? e.message : String(e)
        }`);
      }
    },
    [dlSettings, toast]
  );

  const browseDefaultDir = useCallback(async () => {
    try {
      const picked = await openDialog({
        directory: true,
        multiple: false,
        defaultPath: dlSettings?.default_dir,
      });
      if (typeof picked === "string" && picked) {
        await saveDlDefaultDir(picked);
      }
    } catch (e) {
      toast.error(
        `Couldn't open folder picker: ${
          e instanceof Error ? e.message : String(e)
        }`
      );
    }
  }, [dlSettings, saveDlDefaultDir, toast]);

  const browsePerSource = useCallback(
    async (sourceKey: string) => {
      if (!dlSettings) return;
      try {
        const picked = await openDialog({
          directory: true,
          multiple: false,
          defaultPath:
            dlSettings.per_source[sourceKey] || dlSettings.default_dir,
        });
        if (typeof picked === "string" && picked) {
          await saveDlPerSource({
            ...dlSettings.per_source,
            [sourceKey]: picked,
          });
        }
      } catch (e) {
        toast.error(
          `Couldn't open folder picker: ${
            e instanceof Error ? e.message : String(e)
          }`
        );
      }
    },
    [dlSettings, saveDlPerSource, toast]
  );

  const clearPerSource = useCallback(
    async (sourceKey: string) => {
      if (!dlSettings) return;
      const next = { ...dlSettings.per_source };
      delete next[sourceKey];
      await saveDlPerSource(next);
    },
    [dlSettings, saveDlPerSource]
  );

  const triggerRedownloadLegendary = useCallback(async () => {
    setCliBusy("legendary");
    try {
      await redownloadLegendary();
      const next = await getCliBinsStatus();
      setCliStatus(next);
      toast.success("legendary.exe re-downloaded");
    } catch (e) {
      toast.error(
        `Failed to re-download legendary: ${
          e instanceof Error ? e.message : String(e)
        }`
      );
    } finally {
      setCliBusy(null);
    }
  }, [toast]);

  const triggerRedownloadGogdl = useCallback(async () => {
    setCliBusy("gogdl");
    try {
      await redownloadGogdl();
      const next = await getCliBinsStatus();
      setCliStatus(next);
      toast.success("gogdl.exe re-downloaded");
    } catch (e) {
      toast.error(
        `Failed to re-download gogdl: ${
          e instanceof Error ? e.message : String(e)
        }`
      );
    } finally {
      setCliBusy(null);
    }
  }, [toast]);

  // Reconcile install status — back-fills install_path from legendary's
  // installed.json + on-disk GOG markers. Same call wired into Epic/GOG
  // sync, exposed here as a manual escape hatch for users who installed
  // through the launcher and want Tokoru to notice without re-running
  // the full library sync.
  const triggerDetectInstalls = useCallback(async () => {
    setDetectBusy(true);
    try {
      const res = await detectInstalls();
      const total = res.epic_updated + res.gog_updated;
      if (total === 0) {
        toast.success("No new installs detected.");
      } else {
        toast.success(
          `Detected ${total} installed game${total === 1 ? "" : "s"} — Epic: ${res.epic_updated}, GOG: ${res.gog_updated}`
        );
      }
    } catch (e) {
      toast.error(
        `Detect installs failed: ${
          e instanceof Error ? e.message : String(e)
        }`
      );
    } finally {
      setDetectBusy(false);
    }
  }, [toast]);

  // ---- SteamGridDB save handlers ----
  // Each setter commits to the backend immediately (no save button) and
  // shows a toast on success/failure. We don't roll back local state on
  // failure — the input stays where the user left it so they can retry.

  const saveApiKey = useCallback(
    async (value: string) => {
      // No-op if the value didn't change vs the last persisted state.
      if (value === apiKey) return;
      try {
        const updated = await setSteamgriddbSettings({ apiKey: value });
        setApiKey(updated.api_key);
        setApiKeyDraft(updated.api_key);
        setApiKeySavedAt(updated.api_key_saved_at);
        setHasCustomKey(updated.has_custom_key);
        toast.success(
          updated.has_custom_key
            ? "SteamGridDB API key saved"
            : "Using default SteamGridDB key"
        );
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        toast.error(`Failed to save API key: ${msg}`);
      }
    },
    [apiKey, toast]
  );

  const saveArtStyle = useCallback(
    async (value: SteamGridDbArtworkStyle) => {
      const previous = artStyle;
      setArtStyle(value);
      try {
        await setSteamgriddbSettings({ artworkStyle: value });
        toast.success(`Artwork style: ${STYLE_LABELS[value]}`);
      } catch (e) {
        setArtStyle(previous);
        const msg = e instanceof Error ? e.message : String(e);
        toast.error(`Failed to set style: ${msg}`);
      }
    },
    [artStyle, toast]
  );

  const saveAutoFetch = useCallback(
    async (value: boolean) => {
      const previous = autoFetch;
      setAutoFetch(value);
      try {
        await setSteamgriddbSettings({ autoFetch: value });
        toast.success(value ? "Auto-fetch enabled" : "Auto-fetch disabled");
      } catch (e) {
        setAutoFetch(previous);
        const msg = e instanceof Error ? e.message : String(e);
        toast.error(`Failed to toggle auto-fetch: ${msg}`);
      }
    },
    [autoFetch, toast]
  );

  const savePreferAnimated = useCallback(
    async (value: boolean) => {
      const previous = preferAnimated;
      setPreferAnimated(value);
      try {
        await setSteamgriddbSettings({ preferAnimated: value });
        // Re-run the backfill regardless of direction:
        //   - ON  → upgrade existing static covers to animated picks
        //   - OFF → downgrade currently-animated picks back to static
        //           (the new static for Steam games comes from the local
        //           Steam appcache via fs::copy, so zero download)
        // Fire-and-forget — `triggerArtworkBackfill` creates its own toast.
        void triggerArtworkBackfill(toast, `toggle:prefer_animated=${value}`);
      } catch (e) {
        setPreferAnimated(previous);
        toast.error(`Failed: ${e instanceof Error ? e.message : String(e)}`);
      }
    },
    [preferAnimated, toast]
  );

  const saveAllowNsfw = useCallback(
    async (value: boolean) => {
      const previous = allowNsfw;
      setAllowNsfw(value);
      try {
        await setSteamgriddbSettings({ allowNsfw: value });
      } catch (e) {
        setAllowNsfw(previous);
        toast.error(`Failed: ${e instanceof Error ? e.message : String(e)}`);
      }
    },
    [allowNsfw, toast]
  );

  const openSteamGridDbApi = useCallback(async () => {
    try {
      await openExternal(
        "https://www.steamgriddb.com/profile/preferences/api"
      );
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast.error(`Couldn't open browser: ${msg}`);
    }
  }, [toast]);

  const triggerCollectionsSync = async () => {
    setCollectionsSyncBusy(true);
    try {
      // `restartSteam` already chains the full sequence we need:
      //   1. graceful `steam -shutdown`
      //   2. kill orphan steamwebhelper.exe
      //   3. rebuild Collections (writes the JSON file while Steam is
      //      closed, otherwise Steam overwrites our changes on shutdown)
      //   4. relaunch Steam
      // Calling it directly avoids the "Steam is running — close it first"
      // toast users would otherwise hit on a hot rebuild.
      await restartSteam();
      toast.success("Steam restarted — Collections rebuilt from your library.");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast.error(msg);
    } finally {
      setCollectionsSyncBusy(false);
    }
  };

  // Load saved mode on mount.
  useEffect(() => {
    getCollectionsMode()
      .then((m) => setCollectionsModeState(m))
      .catch(() => {});
  }, []);

  const handleCollectionsModeChange = async (next: CollectionsMode | "tags") => {
    if (next === "tags") {
      toast.error("Tags mode is coming soon — pick another for now.");
      return;
    }
    if (next === collectionsMode) return;
    setCollectionsModeBusy(true);
    setCollectionsModeState(next);
    try {
      await setCollectionsMode(next);
      const label =
        next === "platform"
          ? "By platform"
          : next === "franchise"
            ? "By franchise"
            : "None";
      // restartSteam does the heavy lifting (shutdown → rebuild
      // Collections while closed → relaunch). Awaiting keeps the busy
      // spinner visible until the new Steam is actually back up.
      await restartSteam();
      toast.success(`Collections grouping set to ${label}. Steam restarted.`);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast.error(msg);
      // Roll back the visual select so it matches what's actually persisted.
      try {
        const reloaded = await getCollectionsMode();
        setCollectionsModeState(reloaded);
      } catch {}
    } finally {
      setCollectionsModeBusy(false);
    }
  };

  // Load metadata coverage on mount + poll while a sync is running so
  // the Rebuild button knows whether the franchise algorithm has the
  // data it needs. The poll is gentle (every 3s, only when there's
  // still pending work) and stops as soon as everything's synced.
  useEffect(() => {
    let stopped = false;
    let timer: ReturnType<typeof setInterval> | null = null;
    const tick = () => {
      getMetadataStatus()
        .then((s) => {
          if (stopped) return;
          setMetadataStatus(s);
          // No pending work → no reason to keep polling. The Rebuild
          // button is already unlocked, we save CPU and IPC traffic.
          if (s.pending_count === 0 && timer !== null) {
            clearInterval(timer);
            timer = null;
          }
        })
        .catch(() => {});
    };
    tick();
    timer = setInterval(tick, 3000);
    return () => {
      stopped = true;
      if (timer !== null) clearInterval(timer);
    };
  }, []);

  const triggerMetadataSync = async (force: boolean) => {
    setMetadataSyncBusy(true);
    // Snapshot the starting point so the progress toast can show a real
    // X/Y from the first frame instead of "?/?" while it waits for the
    // first poll tick. On a `force` refresh the denominator is the full
    // library since every row will be re-touched.
    let total = 0;
    let initialSynced = 0;
    try {
      const s = await getMetadataStatus();
      total = s.total_steam_games;
      initialSynced = force ? 0 : s.total_steam_games - s.pending_count;
    } catch {}

    if (total === 0) {
      setMetadataSyncBusy(false);
      toast.info("No Steam games in library — nothing to sync.");
      return;
    }

    const progressToastId = toast.info(
      `Syncing metadata… ${initialSynced}/${total} done (${total - initialSynced} remaining).`,
      { persistent: true }
    );

    // Poll the status every 3s while the sync runs. We use the live
    // `pending_count` to derive a synced count that matches the toast's
    // display. The Rust side does the real work; this poll is just a
    // cheap "are we done yet" SQL count.
    const pollTimer = setInterval(() => {
      getMetadataStatus()
        .then((s) => {
          const synced = s.total_steam_games - s.pending_count;
          toast.update(progressToastId, {
            message: `Syncing metadata… ${synced}/${s.total_steam_games} done (${s.pending_count} remaining).`,
          });
        })
        .catch(() => {});
    }, 3000);

    try {
      const report = await syncMetadataNow(force);
      clearInterval(pollTimer);
      toast.update(progressToastId, {
        kind: "success",
        message: `Metadata synced: ${report.synced}/${report.total}${
          report.failed > 0 ? ` (${report.failed} failed)` : ""
        }. Franchise grouping is now ready.`,
      });
      if (report.failed > 0 && report.errors.length > 0) {
        console.warn("[metadata] errors:", report.errors);
      }
      try {
        const next = await getMetadataStatus();
        setMetadataStatus(next);
      } catch {}
    } catch (e) {
      clearInterval(pollTimer);
      const msg = e instanceof Error ? e.message : String(e);
      toast.update(progressToastId, {
        kind: "error",
        message: `Metadata sync failed: ${msg}`,
      });
    } finally {
      setMetadataSyncBusy(false);
    }
  };

  const triggerPlaytimeSync = async () => {
    setPlaytimeSyncBusy(true);
    try {
      // `restartSteam` does the full close → sync → relaunch dance so
      // the user doesn't have to quit Steam manually. The Rust side
      // also rebuilds Collections in the same closed window, but the
      // playtime push is what we're after here.
      await restartSteam();
      toast.success("Steam restarted — playtime synced.");
      const ts = await getLastSyncAt().catch(() => null);
      setLastSync(ts);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast.error(msg);
    } finally {
      setPlaytimeSyncBusy(false);
    }
  };

  const scrollTo = (id: string) => {
    setActive(id);
    const el = document.getElementById(id);
    el?.scrollIntoView({ behavior: "smooth", block: "start" });
  };

  return (
    <div className="h-screen w-screen flex flex-col relative overflow-hidden">
      <div className="ambient-vignette" />
      <TopBar acrylic />
      <div className="flex-1 flex overflow-hidden relative z-10">
        {/* Left rail */}
        <aside className="w-[240px] flex-shrink-0 bg-transparent flex flex-col pt-12 pb-10 px-6 overflow-y-auto hidden md:flex border-r border-white/[0.02]">
          <nav className="flex flex-col space-y-1 sticky top-0">
            {SECTIONS.map((s) => {
              const selected = active === s.id;
              return (
                <button
                  key={s.id}
                  onClick={() => scrollTo(s.id)}
                  className={`group relative flex items-center justify-between py-2 px-3 rounded-lg text-[13px] font-medium transition-colors text-left ${
                    selected
                      ? "bg-white/[0.04] text-white pl-3 cursor-default"
                      : "text-text-sec hover:text-white hover:bg-white/[0.02] pl-4"
                  }`}
                >
                  {selected && (
                    <span className="absolute left-0 top-1/2 -translate-y-1/2 w-[3px] h-4 bg-accent rounded-r-md" />
                  )}
                  {t(s.tKey)}
                </button>
              );
            })}
          </nav>
        </aside>

        {/* Content */}
        <main className="flex-1 overflow-y-auto scroll-smooth">
          <div className="max-w-[880px] w-full mx-auto px-10 py-12 pb-32">
            <div className="mb-12">
              <h1 className="text-3xl font-semibold tracking-tight text-white">
                {t("settings.title")}
              </h1>
            </div>

            <div className="space-y-16">
              {/* General */}
              <Section id="general" title={t("settings.sections.general")}>
                <Card>
                  <Row
                    title={t("settings.language.title")}
                    desc={t("settings.language.desc")}
                  >
                    <select
                      value={currentLocale}
                      onChange={(e) => void changeLocale(e.target.value)}
                      className="w-48 bg-white/[0.03] border border-white/[0.06] hover:bg-white/[0.06] rounded-lg px-3 py-2 text-[13px] font-medium text-white focus:border-white/20 transition-colors cursor-pointer"
                    >
                      {SUPPORTED_LOCALES.map((l) => (
                        <option key={l.code} value={l.code}>
                          {l.native}
                        </option>
                      ))}
                    </select>
                  </Row>
                  <Row
                    title={t("settings.general.default_action")}
                    desc={t("settings.general.default_action_desc")}
                  >
                    <select className="w-64 bg-white/[0.03] border border-white/[0.06] hover:bg-white/[0.06] rounded-lg px-3 py-2 text-[13px] font-medium text-white focus:border-white/20 transition-colors cursor-pointer">
                      <option>{t("settings.general.action_launch")}</option>
                      <option>{t("settings.general.action_details")}</option>
                      <option>{t("settings.general.action_push")}</option>
                    </select>
                  </Row>
                  <Row
                    title={t("settings.general.restore_view")}
                    desc={t("settings.general.restore_view_desc")}
                  >
                    <Toggle checked={restoreView} onChange={setRestoreView} />
                  </Row>
                  <Row
                    title={t("settings.general.replay_onboarding")}
                    desc={t("settings.general.replay_onboarding_desc")}
                    last
                  >
                    <button
                      type="button"
                      onClick={() => void replayOnboarding()}
                      className="px-4 py-2 rounded-lg bg-white/[0.06] hover:bg-white/[0.10] border border-white/[0.08] text-[13px] font-medium text-white transition-colors"
                    >
                      {t("settings.general.replay_onboarding_btn")}
                    </button>
                  </Row>
                </Card>
              </Section>

              {/* Steam integration */}
              <Section id="steam-integration" title={t("settings.sections.steam")}>
                <Card>
                  <Row
                    title={
                      <span className="flex items-center gap-2">
                        {t("settings.steam.install_path")}
                        <span className="px-2 py-0.5 rounded bg-emerald-500/10 text-emerald-400 text-[10px] font-bold uppercase tracking-wider border border-emerald-500/20 flex items-center gap-1">
                          <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3">
                            <polyline points="20 6 9 17 4 12" />
                          </svg>{" "}
                          {t("settings.steam.verified")}
                        </span>
                      </span>
                    }
                  >
                    <div className="flex items-center gap-2">
                      <input
                        readOnly
                        value="C:\\Program Files (x86)\\Steam"
                        className="bg-white/[0.02] border border-white/[0.04] text-text-sec font-mono text-[12px] px-3 py-1.5 rounded-md w-64 outline-none"
                      />
                      <button className="px-3 py-1.5 bg-white/[0.03] hover:bg-white/[0.08] border border-white/[0.06] rounded-md text-[13px] font-medium text-white transition-colors">
                        {t("settings.steam.browse")}
                      </button>
                    </div>
                  </Row>
                  <Row
                    title={t("settings.steam.user_profile")}
                    desc={t("settings.steam.user_profile_desc")}
                  >
                    <div className="flex items-center gap-4">
                      <div className="flex items-center gap-2 px-3 py-1.5 bg-white/[0.03] border border-white/[0.06] rounded-lg">
                        <div className="w-4 h-4 rounded-full bg-accent shadow-[0_0_8px_rgba(255,70,51,0.5)]" />
                        <span className="text-[13px] font-medium text-white">
                          drrakendu
                        </span>
                        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" className="text-text-muted ml-2">
                          <polyline points="6 9 12 15 18 9" />
                        </svg>
                      </div>
                      <a
                        href="#"
                        onClick={(e) => e.preventDefault()}
                        className="text-[13px] text-accent hover:text-accent-hover font-medium transition-colors"
                      >
                        {t("settings.steam.switch_profile")}
                      </a>
                    </div>
                  </Row>
                  <Row
                    title={t("settings.steam.restart_after_push")}
                    desc={t("settings.steam.restart_after_push_desc")}
                  >
                    <Toggle checked={alwaysRestart} onChange={setAlwaysRestart} />
                  </Row>
                  <Row
                    title={
                      <>
                        {t("settings.steam.backup_vdf_before")}{" "}
                        <span className="font-mono text-[11px] bg-white/[0.05] px-1 rounded text-text-sec">
                          shortcuts.vdf
                        </span>{" "}
                        {t("settings.steam.backup_vdf_after")}
                      </>
                    }
                    desc={t("settings.steam.backup_vdf_desc")}
                  >
                    <Toggle checked={backupVdf} onChange={setBackupVdf} />
                  </Row>
                  <Row
                    title={t("settings.steam.collections_grouping")}
                    desc={t("settings.steam.collections_grouping_desc")}
                  >
                    <select
                      value={collectionsMode}
                      onChange={(e) => handleCollectionsModeChange(e.target.value as CollectionsMode | "tags")}
                      disabled={collectionsModeBusy}
                      className="bg-white/[0.04] border border-white/[0.05] rounded-lg px-3 py-2 text-[13px] text-white outline-none focus:border-accent w-56"
                    >
                      <option value="platform">{t("settings.steam.by_platform")}</option>
                      <option value="franchise">{t("settings.steam.by_franchise")}</option>
                      <option value="tags" disabled>{t("settings.steam.by_tags")}</option>
                      <option value="none">{t("settings.steam.none")}</option>
                    </select>
                  </Row>
                  {(() => {
                    // In Franchise mode the 5-pass algorithm needs the
                    // metadata sync to have populated franchise/tags/
                    // developer columns. Until it has, the Rebuild would
                    // only run the title-prefix passes (degraded result),
                    // so we gate the button. Platform / None modes don't
                    // depend on metadata — rebuild stays enabled there.
                    const needsMetadata =
                      collectionsMode === "franchise" &&
                      metadataStatus !== null &&
                      metadataStatus.pending_count > 0;
                    const desc = needsMetadata
                      ? t("settings.steam.rebuild_needs_metadata", {
                          pending: metadataStatus!.pending_count,
                          total: metadataStatus!.total_steam_games,
                        })
                      : t("settings.steam.rebuild_desc");
                    return (
                      <Row title={t("settings.steam.rebuild_collections")} desc={desc}>
                        <CoralButton
                          variant="primary"
                          size="sm"
                          onClick={triggerCollectionsSync}
                          disabled={collectionsSyncBusy || needsMetadata}
                        >
                          {collectionsSyncBusy ? t("settings.steam.rebuilding") : t("settings.steam.rebuild")}
                        </CoralButton>
                      </Row>
                    );
                  })()}
                  <Row
                    title={t("settings.steam.fetch_metadata")}
                    desc={t("settings.steam.fetch_metadata_desc")}
                    last
                  >
                    <div className="flex items-center gap-2">
                      <CoralButton
                        variant="primary"
                        size="sm"
                        onClick={() => void triggerMetadataSync(false)}
                        disabled={metadataSyncBusy}
                      >
                        {metadataSyncBusy ? t("settings.steam.syncing") : t("settings.steam.sync_now")}
                      </CoralButton>
                      <button
                        type="button"
                        onClick={() => void triggerMetadataSync(true)}
                        disabled={metadataSyncBusy}
                        className="px-3 py-1.5 rounded-md text-[12px] font-medium text-text-sec hover:text-white bg-white/[0.02] hover:bg-white/[0.05] border border-white/[0.06] transition-colors disabled:opacity-50"
                      >
                        {t("settings.steam.force_refresh")}
                      </button>
                    </div>
                  </Row>
                </Card>
              </Section>

              {/* Background service */}
              <Section id="background" title={t("settings.sections.background")}>
                <Card>
                  <Row
                    title={t("settings.background.run_bg")}
                    desc={t("settings.background.run_bg_desc")}
                  >
                    <Toggle checked={runBg} onChange={setRunBg} />
                  </Row>
                  <Row
                    title={t("settings.background.polling")}
                    desc={t("settings.background.polling_desc")}
                  >
                    <div className="flex items-center gap-4 w-72">
                      <input
                        type="range"
                        min={5}
                        max={60}
                        value={polling}
                        onChange={(e) => setPolling(Number(e.target.value))}
                        className="flex-1"
                      />
                      <span className="w-12 text-center text-[12px] font-mono bg-white/[0.04] px-2 py-1 rounded text-white border border-white/[0.05]">
                        {polling}s
                      </span>
                    </div>
                  </Row>
                  <Row title={t("settings.background.detection_method")}>
                    <PillGroup
                      options={[
                        { value: "window", label: t("settings.background.detect_window") },
                        { value: "tree", label: t("settings.background.detect_tree") },
                        { value: "hybrid", label: t("settings.background.detect_hybrid") },
                      ]}
                      value={detection}
                      onChange={setDetection}
                    />
                  </Row>
                  <Row title={t("settings.background.show_tray")} last>
                    <Toggle checked={tray} onChange={setTray} />
                  </Row>
                </Card>
              </Section>

              {/* Downloads — install dirs, parallelism, helper binaries.
                  Mirrors the AIDesigner mockup at
                  SteamShelf-ui-mockup/.aidesigner/runs/...settings-downloads.
                  Loads on mount via `getDownloadSettings`; all controls
                  persist on commit (blur / change) through
                  `setDownloadSettings`. */}
              <Section id="downloads" title={t("settings.sections.downloads")}>
                <Card>
                  <Row
                    title={t("settings.downloads.default_dir")}
                    desc={t("settings.downloads.default_dir_desc")}
                  >
                    <div className="flex items-center gap-2 w-[420px]">
                      <input
                        type="text"
                        readOnly
                        value={dlSettings?.default_dir ?? ""}
                        placeholder={t("settings.downloads.loading")}
                        className="flex-1 min-w-0 bg-black/30 border border-white/[0.06] focus:border-white/20 rounded-md px-3 py-1.5 text-[12px] font-mono text-text-sec outline-none transition-colors"
                      />
                      <button
                        type="button"
                        onClick={browseDefaultDir}
                        disabled={!dlSettings}
                        className="px-3 py-1.5 bg-white/[0.03] hover:bg-white/[0.08] border border-white/[0.06] hover:border-white/[0.15] rounded-md text-[12px] font-medium text-white transition-colors disabled:opacity-40"
                      >
                        {t("settings.downloads.browse")}
                      </button>
                    </div>
                  </Row>

                  <Row
                    title={t("settings.downloads.per_source")}
                    desc={t("settings.downloads.per_source_desc")}
                    column
                  >
                    <div className="bg-black/20 border border-white/[0.05] rounded-lg divide-y divide-white/[0.04] overflow-hidden w-full">
                      {DOWNLOAD_SOURCE_ROWS.map((row) => {
                        const value =
                          dlSettings?.per_source[row.key] ?? "";
                        const dot = SOURCE_DOT[row.key] ?? "#A1A1A6";
                        return (
                          <div
                            key={row.key}
                            className="flex items-center px-4 py-2 hover:bg-white/[0.02] transition-colors group"
                          >
                            <div className="w-[140px] flex items-center gap-2.5 shrink-0">
                              <span
                                className="w-1.5 h-1.5 rounded-full"
                                style={{ background: dot }}
                              />
                              <span className="text-[13px] font-medium text-white/80">
                                {row.label}
                              </span>
                            </div>
                            <input
                              type="text"
                              value={value}
                              placeholder={t("settings.downloads.inherit_default")}
                              readOnly
                              className="flex-1 bg-transparent border-none outline-none text-[12px] font-mono text-white/90 min-w-0 placeholder-text-muted/60 cursor-default"
                            />
                            <button
                              type="button"
                              onClick={() => void browsePerSource(row.key)}
                              disabled={!dlSettings}
                              className="ml-2 px-2 py-1 text-[11px] font-medium text-text-sec hover:text-white bg-white/[0.02] hover:bg-white/[0.06] border border-white/[0.06] rounded transition-colors disabled:opacity-40"
                            >
                              {t("settings.downloads.browse_short")}
                            </button>
                            {value && (
                              <button
                                type="button"
                                onClick={() => void clearPerSource(row.key)}
                                title={t("settings.downloads.reset_to_default")}
                                className="ml-1 text-text-muted hover:text-accent p-1 rounded transition-colors"
                              >
                                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                                  <line x1="5" y1="12" x2="19" y2="12" />
                                </svg>
                              </button>
                            )}
                          </div>
                        );
                      })}
                    </div>
                  </Row>

                  <Row
                    title={t("settings.downloads.max_parallel")}
                    desc={t("settings.downloads.max_parallel_desc")}
                  >
                    <input
                      type="number"
                      min={1}
                      max={5}
                      value={dlMaxParallelDraft}
                      onChange={(e) => setDlMaxParallelDraft(e.target.value)}
                      onBlur={() => {
                        const parsed = Number(dlMaxParallelDraft);
                        if (Number.isFinite(parsed)) {
                          void saveDlMaxParallel(parsed);
                        } else {
                          setDlMaxParallelDraft(
                            String(dlSettings?.max_parallel ?? 2)
                          );
                        }
                      }}
                      className="w-20 h-8 bg-black/30 border border-white/[0.06] focus:border-accent/50 outline-none rounded-md px-3 text-[13px] font-medium text-white text-center"
                    />
                  </Row>

                  <Row
                    title={t("settings.downloads.auto_push")}
                    desc={t("settings.downloads.auto_push_desc")}
                  >
                    <Toggle
                      checked={dlSettings?.auto_push_after_download ?? true}
                      onChange={(v) => void saveDlAutoPush(v)}
                    />
                  </Row>

                  <Row
                    title={t("settings.downloads.bandwidth")}
                    desc={
                      <>
                        {t("settings.downloads.bandwidth_desc_first")}{" "}
                        <span className="text-text-muted/70 italic">
                          {t("settings.downloads.bandwidth_desc_second")}
                        </span>
                      </>
                    }
                  >
                    <div className="flex items-center gap-3">
                      <div className="relative flex items-center h-8">
                        <input
                          type="number"
                          min={0}
                          value={dlBandwidthDraft}
                          onChange={(e) =>
                            setDlBandwidthDraft(e.target.value)
                          }
                          onBlur={() => {
                            const parsed = Number(dlBandwidthDraft);
                            if (Number.isFinite(parsed) && parsed >= 0) {
                              void saveDlBandwidth(parsed);
                            } else {
                              setDlBandwidthDraft(
                                String(dlSettings?.bandwidth_kbps ?? 0)
                              );
                            }
                          }}
                          className="w-24 h-full bg-black/30 border border-white/[0.06] focus:border-accent/50 outline-none rounded-md pl-3 pr-10 text-[13px] font-medium text-white"
                        />
                        <span className="absolute right-3 text-[10px] font-semibold text-text-muted pointer-events-none">
                          KB/s
                        </span>
                      </div>
                      {(dlSettings?.bandwidth_kbps ?? 0) === 0 && (
                        <div className="px-2 py-0.5 rounded text-[10px] font-bold tracking-wide uppercase border border-accent/20 bg-accent/10 text-accent">
                          {t("settings.downloads.unlimited")}
                        </div>
                      )}
                    </div>
                  </Row>

                  <Row
                    title={t("settings.downloads.helper_bins")}
                    desc={t("settings.downloads.helper_bins_desc")}
                    column
                  >
                    <div className="w-full flex flex-col gap-2 mt-2">
                      <BinaryRow
                        name="legendary.exe"
                        present={cliStatus?.legendary_present ?? false}
                        version={cliStatus?.legendary_version ?? null}
                        busy={cliBusy === "legendary"}
                        onRedownload={triggerRedownloadLegendary}
                      />
                      <BinaryRow
                        name="gogdl.exe"
                        present={cliStatus?.gogdl_present ?? false}
                        version={cliStatus?.gogdl_version ?? null}
                        busy={cliBusy === "gogdl"}
                        onRedownload={triggerRedownloadGogdl}
                      />
                    </div>
                  </Row>

                  <Row
                    title={t("settings.downloads.detect_installs")}
                    desc={t("settings.downloads.detect_installs_desc")}
                    last
                  >
                    <button
                      type="button"
                      onClick={triggerDetectInstalls}
                      disabled={detectBusy}
                      className="px-3 py-1.5 text-[12px] font-semibold text-accent hover:text-accent-hover hover:bg-accent/10 border border-accent/30 rounded-md transition-colors disabled:opacity-40"
                    >
                      {detectBusy ? t("settings.downloads.detecting") : t("settings.downloads.detect_installs")}
                    </button>
                  </Row>
                </Card>
              </Section>

              {/* Playtime */}
              <Section id="playtime" title={t("settings.sections.playtime")}>
                <Card>
                  <Row title={t("settings.background.track_bg")}>
                    <Toggle checked={trackBg} onChange={setTrackBg} />
                  </Row>
                  <Row
                    title={t("settings.background.sync_to_steam")}
                    desc={t("settings.background.sync_to_steam_desc", { date: formatTimestamp(lastSync) })}
                  >
                    <Toggle checked={syncToSteam} onChange={setSyncToSteam} />
                  </Row>
                  <Row
                    title={t("settings.background.sync_now")}
                    desc={t("settings.background.sync_now_desc")}
                  >
                    <CoralButton
                      variant="primary"
                      size="sm"
                      onClick={triggerPlaytimeSync}
                      disabled={playtimeSyncBusy}
                    >
                      {playtimeSyncBusy ? t("settings.background.syncing") : t("settings.background.run_sync")}
                    </CoralButton>
                  </Row>
                  <Row title={t("settings.background.min_session")}>
                    <select className="w-40 bg-white/[0.03] border border-white/[0.06] hover:bg-white/[0.06] rounded-lg px-3 py-2 text-[13px] font-medium text-white focus:border-white/20 transition-colors cursor-pointer">
                      <option>0 {t("stats_page.minutes").startsWith("min") ? "secondes" : "seconds"}</option>
                      <option>30 {t("stats_page.minutes").startsWith("min") ? "secondes" : "seconds"}</option>
                      <option>1 {t("stats_page.minutes").startsWith("min") ? "minute" : "minute"}</option>
                      <option>5 {t("stats_page.minutes").startsWith("min") ? "minutes" : "minutes"}</option>
                    </select>
                  </Row>
                  <Row
                    title={t("settings.background.ignore_launchers")}
                    desc={t("settings.background.ignore_launchers_desc")}
                    column
                    last
                  >
                    <div className="w-full min-h-[44px] bg-white/[0.02] border border-white/[0.05] rounded-xl p-2 flex flex-wrap gap-2 items-center mt-3">
                      {["EpicGamesLauncher.exe", "GalaxyClient.exe", "upc.exe", "EADesktop.exe"].map((p) => (
                        <span
                          key={p}
                          className="flex items-center gap-1.5 px-2.5 py-1 bg-white/[0.04] border border-white/[0.06] rounded-md text-[12px] font-mono text-text-main shadow-sm"
                        >
                          {p}
                          <button className="text-text-muted hover:text-accent transition-colors">
                            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                              <line x1="18" y1="6" x2="6" y2="18" />
                              <line x1="6" y1="6" x2="18" y2="18" />
                            </svg>
                          </button>
                        </span>
                      ))}
                      <button className="px-2.5 py-1 border border-dashed border-white/[0.1] hover:border-white/[0.2] hover:bg-white/[0.02] rounded-md text-[12px] text-text-muted hover:text-text-main transition-colors flex items-center gap-1 ml-1">
                        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                          <line x1="12" y1="5" x2="12" y2="19" />
                          <line x1="5" y1="12" x2="19" y2="12" />
                        </svg>
                        Add
                      </button>
                    </div>
                  </Row>
                </Card>
              </Section>

              {/* SteamGridDB */}
              <Section id="steamgriddb" title={t("settings.sections.steamgriddb")}>
                <Card>
                  <Row
                    title={t("settings.steamgriddb.api_key")}
                    desc={
                      hasCustomKey && apiKeySavedAt > 0
                        ? t("settings.steamgriddb.api_key_last_validated", {
                            date: new Date(apiKeySavedAt * 1000).toLocaleString(),
                          })
                        : t("settings.steamgriddb.api_key_blank_hint")
                    }
                  >
                    <div className="flex items-center gap-3">
                      <div className="relative w-64">
                        <input
                          type={apiKeyShown ? "text" : "password"}
                          value={apiKeyDraft}
                          placeholder={t("settings.steamgriddb.api_key_placeholder")}
                          onChange={(e) => {
                            setApiKeyDraft(e.target.value);
                            // Reveal automatically as the user types so they
                            // can verify the paste; the eye toggle still
                            // works for re-masking.
                            if (!apiKeyShown) setApiKeyShown(true);
                          }}
                          onBlur={() => {
                            void saveApiKey(apiKeyDraft);
                            setApiKeyShown(false);
                          }}
                          className="w-full bg-white/[0.02] border border-white/[0.06] hover:bg-white/[0.04] focus:border-white/20 rounded-lg pl-3 pr-10 py-1.5 text-[13px] text-white font-mono transition-colors"
                        />
                        <button
                          type="button"
                          onClick={() => setApiKeyShown((v) => !v)}
                          aria-label={apiKeyShown ? t("settings.steamgriddb.hide_key") : t("settings.steamgriddb.show_key")}
                          className="absolute inset-y-0 right-2 flex items-center text-text-muted hover:text-white transition-colors"
                        >
                          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                            <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
                            <circle cx="12" cy="12" r="3" />
                          </svg>
                        </button>
                      </div>
                      <button
                        type="button"
                        onClick={openSteamGridDbApi}
                        className="text-[12px] text-accent hover:text-accent-hover font-medium flex items-center gap-1 transition-colors"
                      >
                        {t("settings.steamgriddb.get_key")}
                        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                          <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
                          <polyline points="15 3 21 3 21 9" />
                          <line x1="10" y1="14" x2="21" y2="3" />
                        </svg>
                      </button>
                    </div>
                  </Row>
                  <Row
                    title={t("settings.steamgriddb.preferred_style")}
                    desc={t("settings.steamgriddb.preferred_style_desc")}
                  >
                    <PillGroup
                      options={[
                        { value: "official", label: t("settings.steamgriddb.style.official") },
                        { value: "painted", label: t("settings.steamgriddb.style.painted") },
                        { value: "photoreal", label: t("settings.steamgriddb.style.photoreal") },
                        { value: "minimal", label: t("settings.steamgriddb.style.minimal") },
                        { value: "any", label: t("settings.steamgriddb.style.any") },
                      ]}
                      value={artStyle}
                      onChange={(v) => void saveArtStyle(v)}
                    />
                  </Row>
                  <Row
                    title={t("settings.steamgriddb.auto_fetch")}
                    desc={t("settings.steamgriddb.auto_fetch_desc")}
                  >
                    <Toggle checked={autoFetch} onChange={(v) => void saveAutoFetch(v)} />
                  </Row>
                  <Row
                    title={t("settings.steamgriddb.prefer_animated")}
                    desc={t("settings.steamgriddb.prefer_animated_desc")}
                  >
                    <Toggle
                      checked={preferAnimated}
                      onChange={(v) => void savePreferAnimated(v)}
                    />
                  </Row>
                  <Row
                    title={t("settings.steamgriddb.allow_nsfw")}
                    desc={t("settings.steamgriddb.allow_nsfw_desc")}
                  >
                    <Toggle checked={allowNsfw} onChange={(v) => void saveAllowNsfw(v)} />
                  </Row>
                  <Row
                    title={t("settings.steamgriddb.refetch_now")}
                    desc={t("settings.steamgriddb.refetch_now_desc")}
                    last
                  >
                    <CoralButton
                      variant="primary"
                      size="sm"
                      onClick={() =>
                        void triggerArtworkBackfill(toast, "settings:refetch_button", {
                          force: true,
                        })
                      }
                    >
                      {t("settings.steamgriddb.refetch_button")}
                    </CoralButton>
                  </Row>
                </Card>
              </Section>

              {/* Sources — AIDesigner mockup look + click-to-expand popover */}
              <Section id="sources" title={t("settings.sections.sources")}>
                <div className="bg-surface/40 border border-white/[0.04] rounded-2xl p-5 shadow-sm">
                  <SourceTileGrid
                    tiles={settingsTileEntries}
                    gridClassName="grid-cols-4"
                    compact
                  />
                  <a
                    href="#"
                    onClick={(e) => e.preventDefault()}
                    className="text-[13px] text-text-muted hover:text-white font-medium flex items-center gap-1 transition-colors pl-1 mt-5"
                  >
                    Manage sources
                    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                      <line x1="5" y1="12" x2="19" y2="12"></line>
                      <polyline points="12 5 19 12 12 19"></polyline>
                    </svg>
                  </a>
                </div>
              </Section>

              {/* Appearance */}
              <Section id="appearance" title={t("settings.sections.appearance")}>
                <Card>
                  <div className="p-6 border-b border-white/[0.03]">
                    <div className="text-[14px] font-medium text-white mb-4">{t("settings.appearance.theme")}</div>
                    <div className="grid grid-cols-3 gap-4">
                      <ThemePreview kind="dark" selected={theme === "dark"} onSelect={setTheme} />
                      <ThemePreview kind="light" selected={theme === "light"} onSelect={setTheme} />
                      <ThemePreview kind="system" selected={theme === "system"} onSelect={setTheme} />
                    </div>
                  </div>
                  <Row title={t("settings.appearance.accent")} desc={t("settings.appearance.accent_desc")}>
                    <div className="flex items-center gap-3 flex-wrap relative">
                      {ACCENT_PRESETS.map((preset) => {
                        const isActive =
                          accent.kind === "preset" && accent.key === preset.key;
                        return (
                          <button
                            key={preset.key}
                            type="button"
                            title={preset.label}
                            aria-label={preset.label}
                            aria-pressed={isActive}
                            onClick={() => {
                              setAccent(preset.key);
                              setCustomPickerOpen(false);
                            }}
                            className={`w-6 h-6 rounded-full transition-transform shadow-sm ${
                              isActive
                                ? "ring-2 ring-offset-2 ring-offset-surface shadow-premium cursor-default"
                                : "hover:scale-110 cursor-pointer"
                            }`}
                            style={{
                              background: preset.hex,
                              ...(isActive
                                ? { boxShadow: `0 0 0 2px ${preset.hex}` }
                                : {}),
                            }}
                          />
                        );
                      })}
                      {/* Custom hex picker — rainbow swatch when inactive,
                          shows the currently-picked color when active.
                          The popover is rendered through a portal so it
                          escapes the Card / Row overflow:hidden parents. */}
                      <button
                        ref={customBtnRef}
                        type="button"
                        title={t("settings.appearance.custom")}
                        aria-label={t("settings.appearance.custom")}
                        aria-pressed={accent.kind === "custom"}
                        onClick={openCustomPicker}
                        className={`w-6 h-6 rounded-full transition-transform shadow-sm ${
                          accent.kind === "custom"
                            ? "ring-2 ring-offset-2 ring-offset-surface shadow-premium"
                            : "hover:scale-110 cursor-pointer"
                        }`}
                        style={
                          accent.kind === "custom"
                            ? {
                                background: accent.hex,
                                boxShadow: `0 0 0 2px ${accent.hex}`,
                              }
                            : {
                                background:
                                  "conic-gradient(from 180deg, #ff4633, #ff8c00, #eab308, #10b981, #00d1ff, #3b82f6, #a855f7, #e024c4, #ff4633)",
                              }
                        }
                      />
                    </div>
                  </Row>
                  <Row title={t("settings.appearance.reduce_motion")} desc={t("settings.appearance.reduce_motion_desc")}>
                    <Toggle checked={reduceMotion} onChange={setReduceMotion} />
                  </Row>
                  <Row title={t("settings.appearance.density")} desc={t("settings.appearance.density_desc")} last>
                    <PillGroup
                      options={[
                        { value: "compact", label: t("library.density.compact") },
                        { value: "comfortable", label: t("library.density.comfortable") },
                        { value: "spacious", label: t("library.density.spacious") },
                      ]}
                      value={density}
                      onChange={setDensity}
                    />
                  </Row>
                </Card>
              </Section>

              {/* Startup */}
              <Section id="startup" title={t("settings.sections.startup")}>
                <Card>
                  <Row title={t("settings.startup.open_at_login")} desc={t("settings.startup.open_at_login_desc")}>
                    <Toggle checked={startWindows} onChange={setStartWindows} />
                  </Row>
                  <Row title={t("settings.startup.start_minimized")} desc={t("settings.startup.start_minimized_desc")}>
                    <Toggle checked={startMin} onChange={setStartMin} />
                  </Row>
                  <Row
                    title={t("settings.startup.auto_update")}
                    desc={
                      <span className="flex items-center gap-1.5">
                        {t("settings.startup.auto_update_status", { version: "1.0.0" })}
                        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" className="text-emerald-500">
                          <polyline points="20 6 9 17 4 12" />
                        </svg>
                      </span>
                    }
                    last
                  >
                    <Toggle checked={autoUpdate} onChange={setAutoUpdate} />
                  </Row>
                </Card>
              </Section>

              {/* About */}
              <section id="about" className="pt-8 pb-4 flex flex-col items-center justify-center text-center">
                <img
                  src="/logo-dark.png"
                  alt="Tokoru"
                  className="w-14 h-14 rounded-2xl mb-4 shadow-[0_4px_16px_rgba(0,0,0,0.5)]"
                />
                <h3 className="text-xl font-bold tracking-tight text-white m-0">
                  Tokoru
                </h3>
                <p className="text-[13px] text-text-sec mt-1">
                  {t("settings.about.tagline")}
                </p>
                <div className="flex items-center gap-4 mt-6">
                  <span className="text-[12px] font-mono text-text-muted bg-white/[0.03] px-2 py-0.5 rounded">
                    v1.0.0
                  </span>
                  <div className="w-1 h-1 rounded-full bg-white/20" />
                  <span className="text-[12px] text-text-muted">{t("settings.about.build", { build: "2405A" })}</span>
                </div>
                <div className="flex items-center gap-6 mt-6">
                  {[
                    { tKey: "settings.about.changelog" },
                    { tKey: "settings.about.source" },
                    { tKey: "settings.about.report_issue" },
                    { tKey: "settings.about.privacy" },
                  ].map((l) => (
                    <a
                      key={l.tKey}
                      href="#"
                      onClick={(e) => e.preventDefault()}
                      className="text-[13px] font-medium text-text-sec hover:text-white transition-colors"
                    >
                      {t(l.tKey)}
                    </a>
                  ))}
                </div>
                <div className="mt-10 text-[11px] text-text-muted/60">
                  {t("settings.about.credits")}
                  <br />
                  {t("settings.about.disclaimer")}
                </div>
              </section>
            </div>
          </div>
        </main>
      </div>
      {customPickerOpen &&
        pickerPos &&
        createPortal(
          <>
            <div
              className="fixed inset-0 z-[100]"
              onClick={() => setCustomPickerOpen(false)}
            />
            <div
              className="fixed z-[101] p-4 rounded-xl bg-surface border border-white/[0.08] shadow-premium tokoru-color-picker"
              style={{ top: pickerPos.top, left: pickerPos.left }}
              onClick={(e) => e.stopPropagation()}
            >
              <HexColorPicker
                color={accent.hex}
                onChange={setCustomAccent}
              />
              <div className="mt-3 flex items-center gap-2">
                <span className="text-text-sec text-[12px]">#</span>
                <HexColorInput
                  color={accent.hex}
                  onChange={setCustomAccent}
                  prefixed={false}
                  className="bg-shell border border-white/[0.08] rounded px-2 py-1 text-[13px] text-text-main font-mono w-20 uppercase focus:outline-none focus:border-accent"
                />
              </div>
            </div>
          </>,
          document.body
        )}
    </div>
  );
}

// One row in the "Helper binaries" group inside Settings → Downloads. Shows
// the on-disk presence chip (green when present, amber when missing), the
// pinned version we know is on disk, and a Re-download CTA. Wires
// `get_cli_bins_status` + `redownload_{legendary,gogdl}` on the Rust side.
function BinaryRow({
  name,
  present,
  version,
  busy,
  onRedownload,
}: {
  name: string;
  present: boolean;
  version: string | null;
  busy: boolean;
  onRedownload: () => void;
}) {
  return (
    <div className="flex items-center justify-between px-3 py-2 bg-black/20 border border-white/[0.05] rounded-lg">
      <div className="flex items-center gap-3">
        <div className="w-6 h-6 rounded flex items-center justify-center bg-white/[0.08] text-white">
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <rect x="3" y="4" width="18" height="16" rx="2" />
            <path d="M8 2v4M16 2v4M3 10h18" />
          </svg>
        </div>
        <span className="text-[12px] font-mono text-white/90">{name}</span>
        {present ? (
          <span className="px-1.5 py-0.5 rounded bg-emerald-400/10 border border-emerald-400/20 text-[10.5px] font-medium text-emerald-400 flex items-center gap-1">
            <span className="w-1.5 h-1.5 rounded-full bg-emerald-400 shadow-[0_0_8px_rgba(52,211,153,0.8)]" />
            {version ? `${version} · installed` : "installed"}
          </span>
        ) : (
          <span className="px-1.5 py-0.5 rounded bg-amber-400/10 border border-amber-400/20 text-[10.5px] font-medium text-amber-300 flex items-center gap-1">
            <span className="w-1.5 h-1.5 rounded-full bg-amber-400" />
            not installed
          </span>
        )}
      </div>
      <button
        type="button"
        onClick={onRedownload}
        disabled={busy}
        className="px-2.5 py-1 text-[11px] font-semibold text-accent hover:text-accent-hover hover:bg-accent/10 border border-accent/30 rounded transition-colors disabled:opacity-40"
      >
        {busy ? "Downloading..." : present ? "Re-download" : "Download"}
      </button>
    </div>
  );
}

function Section({
  id,
  title,
  children,
}: {
  id: string;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section id={id}>
      <h2 className="text-[13px] uppercase tracking-widest text-text-muted font-bold mb-4 pl-1">
        {title}
      </h2>
      {children}
    </section>
  );
}

function Card({ children }: { children: React.ReactNode }) {
  return (
    <div className="bg-surface/40 border border-white/[0.04] rounded-2xl overflow-hidden shadow-sm">
      {children}
    </div>
  );
}

function Row({
  title,
  desc,
  children,
  last = false,
  column = false,
}: {
  title: React.ReactNode;
  desc?: React.ReactNode;
  children: React.ReactNode;
  last?: boolean;
  column?: boolean;
}) {
  return (
    <div
      className={`p-5 ${!last ? "border-b border-white/[0.03]" : ""} ${
        column ? "flex flex-col gap-2" : "flex items-center justify-between gap-6"
      }`}
    >
      <div className={column ? "" : "pr-2"}>
        <div className="text-[14px] font-medium text-white">{title}</div>
        {desc && <div className="text-[13px] text-text-muted mt-0.5">{desc}</div>}
      </div>
      {!column && <div className="shrink-0">{children}</div>}
      {column && <div className="w-full">{children}</div>}
    </div>
  );
}

function ThemePreview({
  kind,
  selected,
  onSelect,
}: {
  kind: "dark" | "light" | "system";
  selected?: boolean;
  onSelect?: (k: Theme) => void;
}) {
  const labels: Record<typeof kind, string> = {
    dark: "Dark",
    light: "Light",
    system: "System",
  };
  return (
    <div
      className="cursor-pointer group"
      onClick={() => onSelect?.(kind)}
      role="button"
      tabIndex={0}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onSelect?.(kind);
        }
      }}
    >
      <div
        className={`w-full aspect-[4/3] rounded-lg border overflow-hidden relative flex flex-col ${
          selected
            ? "ring-2 ring-accent ring-offset-2 ring-offset-surface shadow-premium border-white/[0.1]"
            : "border-white/[0.08] group-hover:border-white/20 shadow-sm transition-colors"
        }`}
        style={
          kind === "dark"
            ? { background: "#111113" }
            : kind === "light"
            ? { background: "#F8F9FA" }
            : { background: "#111113" }
        }
      >
        {kind === "system" ? (
          <div className="absolute inset-0 flex">
            <div className="w-1/2 h-full" style={{ background: "#111113" }} />
            <div className="w-1/2 h-full" style={{ background: "#F8F9FA" }} />
          </div>
        ) : (
          <>
            {/* Inline styles only — Tailwind `bg-white/*` and `bg-black/*` are
                themed away by the global light overrides, but a preview tile
                must always look like the theme it represents regardless of
                the active app theme. */}
            <div
              className="h-4 w-full shrink-0 border-b"
              style={
                kind === "dark"
                  ? {
                      background: "rgba(255,255,255,0.02)",
                      borderBottomColor: "rgba(255,255,255,0.05)",
                    }
                  : {
                      background: "rgba(0,0,0,0.02)",
                      borderBottomColor: "rgba(0,0,0,0.05)",
                    }
              }
            />
            <div className="flex-1 flex p-1.5 gap-1.5">
              <div
                className="w-1/4 h-full rounded-sm"
                style={{
                  background:
                    kind === "dark"
                      ? "rgba(255,255,255,0.02)"
                      : "rgba(0,0,0,0.05)",
                }}
              />
              <div className="flex-1 grid grid-cols-2 gap-1.5">
                <div
                  className="rounded-sm"
                  style={{
                    background:
                      kind === "dark"
                        ? "rgba(255,255,255,0.05)"
                        : "rgba(0,0,0,0.1)",
                  }}
                />
                <div
                  className="rounded-sm"
                  style={{
                    background:
                      kind === "dark"
                        ? "rgba(255,255,255,0.05)"
                        : "rgba(0,0,0,0.1)",
                  }}
                />
              </div>
            </div>
          </>
        )}
      </div>
      <div
        className={`text-center text-[12px] font-medium mt-2 transition-colors ${
          selected ? "text-white" : "text-text-sec group-hover:text-white"
        }`}
      >
        {labels[kind]}
      </div>
    </div>
  );
}
