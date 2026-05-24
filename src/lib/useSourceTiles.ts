// Single source of truth for the Source Tile grid.
//
// The Sources page and the Settings → Sources section both render the same
// `<SourceTile />` for each provider. This hook holds the snapshot
// (`AllSourceStates`) plus the connect-flow plumbing for Steam/Epic/GOG, and
// exposes one stable `tileFor(source)` accessor each page can call.

import { useCallback, useEffect, useMemo, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import i18n from "../i18n";
import {
  disconnectSteam,
  epicLoginFinish,
  epicLoginStart,
  epicLogout,
  epicSyncLibrary,
  getAllSourceStates,
  scanSource,
  gogLoginFinish,
  gogLoginStart,
  gogLogout,
  gogSyncLibrary,
  steamLoginFinish,
  steamLoginStart,
  syncSteamLibrary,
} from "./api";
import type {
  EpicConnectedInfo,
  GogConnectedInfo,
  Source,
  SourceState,
  SteamConnectedInfo,
} from "./types";
import { SOURCE_LONG } from "./types";
import { useAccountConnect } from "./useAccountConnect";
import { useToast } from "../components/Toast";

type Busy = Record<string, boolean>;

export interface SyncProgress {
  processed: number;
  total: number;
  /// Title of the most recent game upserted — surfaces under the badge so the
  /// user sees individual games flowing past, not just X/Y.
  title: string;
}

export interface SourceTileHandlers {
  state: SourceState;
  busy: boolean;
  /// Set while `epic_sync_library` / `gog_sync_library` are emitting
  /// `library-sync-progress`. Null when no sync is in flight.
  progress: SyncProgress | null;
  onConnect: () => void;
  onSync: () => void;
  onDisconnect: () => void;
  onRescan: () => void;
}

export interface UseSourceTilesResult {
  states: SourceState[];
  byKey: Record<string, SourceState>;
  loading: boolean;
  /// Reload the snapshot — call this after the library `useGames().refresh`
  /// runs so per-source counts stay in sync.
  refresh: () => Promise<void>;
  /// Map a source key to a fully-bound tile handler bundle for the
  /// `<SourceTile />` component.
  handlersFor: (source: string) => SourceTileHandlers | null;
}

export function useSourceTiles(
  onGamesChanged: () => void | Promise<void>
): UseSourceTilesResult {
  const toast = useToast();
  const [states, setStates] = useState<SourceState[]>([]);
  const [busy, setBusy] = useState<Busy>({});
  const [loading, setLoading] = useState(true);
  const [progress, setProgress] = useState<Record<string, SyncProgress | null>>({});

  // Listen for live progress emitted by `epic_sync_library` /
  // `gog_sync_library` (and any future sync command). Done payloads clear the
  // entry so the tile drops back to its normal state.
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    void listen<{
      source: string;
      processed: number;
      total: number;
      title: string;
      done: boolean;
    }>("library-sync-progress", (e) => {
      const p = e.payload;
      setProgress((prev) => ({
        ...prev,
        [p.source]: p.done
          ? null
          : { processed: p.processed, total: p.total, title: p.title },
      }));
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const refresh = useCallback(async () => {
    try {
      const snap = await getAllSourceStates();
      setStates(snap.sources);
    } catch (e) {
      console.error("useSourceTiles: get_all_source_states failed", e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const setKeyBusy = useCallback((key: string, value: boolean) => {
    setBusy((b) => ({ ...b, [key]: value }));
  }, []);

  // Steam web-login (WebView cookie extraction) via the shared hook.
  const steam = useAccountConnect<SteamConnectedInfo, number, { steam_id?: number; account_name?: string | null }>({
    label: "Steam",
    eventName: "steam-login-success",
    failureEventName: "steam-login-failure",
    start: steamLoginStart,
    finish: steamLoginFinish,
    extractToken: (p) => p?.steam_id,
    onSuccess: (r) => {
      toast.success(
        r.account_name
          ? `Steam connected as ${r.account_name}`
          : `Connected as Steam #${r.steam_id}`
      );
      void refresh();
    },
    onError: (m) => toast.error(m),
  });

  const epic = useAccountConnect<EpicConnectedInfo>({
    label: "Epic",
    eventName: "epic-login-success",
    start: epicLoginStart,
    finish: epicLoginFinish,
    onSuccess: (r) => {
      toast.success(`Epic connected as ${r.account_name}`);
      // Badge flips to "Connected" immediately. Library fetch was split out
      // of login_finish so it doesn't block the badge — chain it here behind
      // the source-busy flag so the tile shows the live SCANNING state.
      void refresh();
      setKeyBusy("epic", true);
      void epicSyncLibrary()
        .then((res) => {
          toast.success(
            i18n.t("toast.library_sync_result", {
              source: "Epic",
              added: res.added,
              updated: res.updated,
              total: res.total_owned,
            })
          );
          void refresh();
          void onGamesChanged();
        })
        .catch((e) =>
          toast.error(`Epic library sync failed: ${e instanceof Error ? e.message : String(e)}`)
        )
        .finally(() => setKeyBusy("epic", false));
    },
    onError: (m) => toast.error(m),
  });

  const gog = useAccountConnect<GogConnectedInfo>({
    label: "GOG",
    eventName: "gog-login-success",
    start: gogLoginStart,
    finish: gogLoginFinish,
    onSuccess: (r) => {
      toast.success(`GOG connected as ${r.account_name}`);
      void refresh();
      setKeyBusy("gog", true);
      void gogSyncLibrary()
        .then((res) => {
          toast.success(
            i18n.t("toast.library_sync_result", {
              source: "GOG",
              added: res.added,
              updated: res.updated,
              total: res.total_owned,
            })
          );
          void refresh();
          void onGamesChanged();
        })
        .catch((e) =>
          toast.error(`GOG library sync failed: ${e instanceof Error ? e.message : String(e)}`)
        )
        .finally(() => setKeyBusy("gog", false));
    },
    onError: (m) => toast.error(m),
  });

  const handlersFor = useCallback(
    (source: string): SourceTileHandlers | null => {
      const state = states.find((s) => s.source === source);
      if (!state) return null;

      const isBusy =
        !!busy[source] ||
        (source === "steam" && steam.busy) ||
        (source === "epic" && epic.busy) ||
        (source === "gog" && gog.busy);

      const onConnect = () => {
        if (source === "steam") void steam.connect();
        else if (source === "epic") void epic.connect();
        else if (source === "gog") void gog.connect();
      };

      const onSync = async () => {
        setKeyBusy(source, true);
        try {
          if (source === "steam") {
            const res = await syncSteamLibrary();
            if (res.session_expired) {
              toast.error(i18n.t("toast.steam_session_expired"));
            } else {
              toast.success(
                i18n.t("toast.library_sync_result", {
                  source: "Steam",
                  added: res.added,
                  updated: res.updated,
                  total: res.total_owned,
                })
              );
            }
          } else if (source === "epic") {
            const res = await epicSyncLibrary();
            toast.success(
              i18n.t("toast.library_sync_result", {
                source: "Epic",
                added: res.added,
                updated: res.updated,
                total: res.total_owned,
              })
            );
          } else if (source === "gog") {
            const res = await gogSyncLibrary();
            toast.success(
              i18n.t("toast.library_sync_result", {
                source: "GOG",
                added: res.added,
                updated: res.updated,
                total: res.total_owned,
              })
            );
          }
          await refresh();
          await onGamesChanged();
        } catch (e) {
          const msg = e instanceof Error ? e.message : String(e);
          toast.error(msg);
        } finally {
          setKeyBusy(source, false);
        }
      };

      const onDisconnect = async () => {
        setKeyBusy(source, true);
        try {
          if (source === "steam") await disconnectSteam();
          else if (source === "epic") await epicLogout();
          else if (source === "gog") await gogLogout();
          await refresh();
        } catch (e) {
          const msg = e instanceof Error ? e.message : String(e);
          toast.error(msg);
        } finally {
          setKeyBusy(source, false);
        }
      };

      const onRescan = async () => {
        setKeyBusy(source, true);
        try {
          const res = await scanSource(source);
          const label = SOURCE_LONG[source as Source] ?? source;
          toast.success(
            res.total_found === 0
              ? `${label}: no installed games found.`
              : `${label}: ${res.total_found} game${res.total_found > 1 ? "s" : ""} detected (${res.new_games} new).`
          );
          await refresh();
          await onGamesChanged();
        } catch (e) {
          const msg = e instanceof Error ? e.message : String(e);
          toast.error(msg);
        } finally {
          setKeyBusy(source, false);
        }
      };

      return {
        state,
        busy: isBusy,
        progress: progress[source] ?? null,
        onConnect,
        onSync,
        onDisconnect,
        onRescan,
      };
    },
    [states, busy, progress, steam, epic, gog, setKeyBusy, refresh, onGamesChanged, toast]
  );

  const byKey = useMemo(() => {
    const acc: Record<string, SourceState> = {};
    for (const s of states) acc[s.source] = s;
    return acc;
  }, [states]);

  return { states, byKey, loading, refresh, handlersFor };
}
