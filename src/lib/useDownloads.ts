// Live mirror of the backend download runtime.
//
// On mount: pulls the current list via `getAllDownloads`, then subscribes to
// `download-progress` + `download-status` events so the UI stays current
// without polling.
//
// Mirrors the existing `useGames` pattern — one hook, exposes
// `{ downloads, start, pause, resume, cancel, retry }` for the GameCard and
// Downloads page to consume.

import { useCallback, useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  cancelDownload as apiCancel,
  dismissDownload as apiDismiss,
  getAllDownloads,
  pauseDownload as apiPause,
  resumeDownload as apiResume,
  retryDownload as apiRetry,
  startDownload as apiStart,
} from "./api";
import type {
  DownloadProgressEvent,
  DownloadState,
  DownloadStatusEvent,
} from "./types";

export interface UseDownloadsResult {
  /// Snapshot of every active / failed / paused download, keyed by game id.
  downloads: Record<string, DownloadState>;
  loading: boolean;
  start: (gameId: string) => Promise<void>;
  pause: (gameId: string) => Promise<void>;
  resume: (gameId: string) => Promise<void>;
  cancel: (gameId: string) => Promise<void>;
  retry: (gameId: string) => Promise<void>;
  /// Clear a finished / failed entry from the list. Does NOT delete files
  /// on disk and refuses while the download is still active.
  dismiss: (gameId: string) => Promise<void>;
  refresh: () => Promise<void>;
}

export function useDownloads(): UseDownloadsResult {
  const [downloads, setDownloads] = useState<Record<string, DownloadState>>({});
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const entries = await getAllDownloads();
      const map: Record<string, DownloadState> = {};
      for (const e of entries) {
        map[e.game_id] = e.state;
      }
      setDownloads(map);
    } catch (e) {
      console.error("useDownloads: refresh failed", e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    let unlistenProgress: UnlistenFn | null = null;
    let unlistenStatus: UnlistenFn | null = null;
    let cancelled = false;

    void listen<DownloadProgressEvent>("download-progress", (event) => {
      const p = event.payload;
      if (!p) return;
      setDownloads((prev) => {
        const existing = prev[p.game_id];
        if (!existing) return prev;
        return {
          ...prev,
          [p.game_id]: {
            ...existing,
            progress_pct: p.progress_pct,
            speed_bps: p.speed_bps,
            eta_secs: p.eta_secs,
            downloaded_bytes: p.downloaded_bytes,
            total_bytes: p.total_bytes,
          },
        };
      });
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenProgress = fn;
    });

    void listen<DownloadStatusEvent>("download-status", (event) => {
      const p = event.payload;
      if (!p) return;
      setDownloads((prev) => {
        // If the backend cancelled (Failed + "Cancelled by user"), drop it
        // from the map. Otherwise upsert.
        const cancellation =
          p.status === "failed" && p.last_error === "Cancelled by user";
        if (cancellation) {
          if (!(p.game_id in prev)) return prev;
          const next = { ...prev };
          delete next[p.game_id];
          return next;
        }
        // Backend might emit a status event for a game we haven't loaded
        // yet (e.g. boot-time hydrate). Refetch as a fallback so the entry
        // appears.
        if (!prev[p.game_id]) {
          void refresh();
          return prev;
        }
        return {
          ...prev,
          [p.game_id]: {
            ...prev[p.game_id],
            status: p.status,
            last_error: p.last_error,
          },
        };
      });
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenStatus = fn;
    });

    return () => {
      cancelled = true;
      unlistenProgress?.();
      unlistenStatus?.();
    };
  }, [refresh]);

  const start = useCallback(async (gameId: string) => {
    await apiStart(gameId);
    // Optimistically seed the entry so the GameCard flips immediately —
    // the backend will overwrite this with a real state event very soon.
    setDownloads((prev) => {
      if (prev[gameId]) return prev;
      return {
        ...prev,
        [gameId]: {
          game_id: gameId,
          source: "",
          platform_id: "",
          game_name: "",
          status: "downloading",
          progress_pct: 0,
          speed_bps: 0,
          eta_secs: 0,
          downloaded_bytes: 0,
          total_bytes: 0,
          last_error: null,
          install_path: null,
          language: null,
        },
      };
    });
  }, []);

  const pause = useCallback((gameId: string) => apiPause(gameId), []);
  const resume = useCallback((gameId: string) => apiResume(gameId), []);
  const cancel = useCallback((gameId: string) => apiCancel(gameId), []);
  const retry = useCallback((gameId: string) => apiRetry(gameId), []);
  const dismiss = useCallback(async (gameId: string) => {
    await apiDismiss(gameId);
    // Optimistically drop the row so the UI doesn't wait for the next event.
    setDownloads((prev) => {
      if (!(gameId in prev)) return prev;
      const next = { ...prev };
      delete next[gameId];
      return next;
    });
  }, []);

  return { downloads, loading, start, pause, resume, cancel, retry, dismiss, refresh };
}
