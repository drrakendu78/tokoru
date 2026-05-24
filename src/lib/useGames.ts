// Light React Query-ish hook for the games list — no external dep, just
// `useState` + `useEffect`. Re-runs on `refresh()`, and auto-refreshes when
// the backend emits `games-changed` (Steam download finishes, install_path
// cleared after an uninstall, etc.).

import { useCallback, useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getAllGames } from "./api";
import type { Game } from "./types";

export interface UseGamesResult {
  games: Game[];
  loading: boolean;
  error: string | null;
  /** Force-refresh from the backend. */
  refresh: () => Promise<void>;
}

export function useGames(): UseGamesResult {
  const [games, setGames] = useState<Game[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const next = await getAllGames();
      setGames(next);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      console.error("useGames: failed to load games", e);
      setError(msg);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Backend pings `games-changed` after install_path additions (download
  // complete) and clearings (game uninstalled / files vanished). The UI
  // re-fetches so it never lies about install state without a manual F5.
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    void listen("games-changed", () => {
      void refresh();
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [refresh]);

  return { games, loading, error, refresh };
}
