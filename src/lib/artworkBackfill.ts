// Shared trigger for the artwork backfill — called from both `App.tsx`
// (boot + window-focus) and `Settings.tsx` (after the user toggles
// `prefer_animated` / `allow_nsfw` so the new prefs apply immediately
// without having to refocus the window).
//
// Posts a persistent live-progress toast that updates per game via the
// `artwork-backfill-progress` Tauri event the Rust side emits.

import { listen } from "@tauri-apps/api/event";
import i18n from "../i18n";
import { fetchAllArtwork, getSteamgriddbSettings } from "./api";

/// Global lock that survives Vite HMR (module-level vars get reset on hot
/// reload — observed 3 concurrent backfills in dev because of this). We
/// stash the flag on `window` instead so the lock persists across HMR
/// cycles. Guarantees never two artwork backfills at the same time.
declare global {
  interface Window {
    __tokoruArtworkBackfillInFlight?: boolean;
  }
}
const lockKey = "__tokoruArtworkBackfillInFlight" as const;
const isLocked = (): boolean => Boolean(window[lockKey]);
const setLocked = (v: boolean): void => {
  window[lockKey] = v;
};

/// Minimum shape of the toast helper we need. Matches what `useToast`
/// returns — kept loose so callers can pass either the hook return value
/// or a mock from tests.
export interface BackfillToast {
  info: (message: string, opts?: { persistent?: boolean }) => number;
  success: (message: string) => number;
  update: (id: number, patch: { message?: string; kind?: "info" | "success" | "error" }) => void;
  dismiss: (id: number) => void;
}

interface ProgressPayload {
  current: number;
  total: number;
  title: string;
  updated: number;
  done: boolean;
}

export interface TriggerArtworkBackfillOptions {
  /// When provided (and non-empty), restricts the backfill to just these
  /// game ids. Used by the boot/focus path to only refetch artwork for
  /// games newly inserted by the latest scan, once the initial full
  /// pass has already happened. An EMPTY array short-circuits with no
  /// toast (nothing to fetch, common case on a boot that detected zero
  /// new games). `undefined` runs the full library pass.
  onlyIds?: string[];
  /// When true, the backend skips per-slot eligibility heuristics and
  /// refetches EVERY game with any URL set. Used by the Settings
  /// "Refresh artwork now" button after the user flipped a SteamGridDB
  /// preference and wants it applied library-wide (Steam + Epic + GOG
  /// + Ubi + EA).
  force?: boolean;
}

/// Run the artwork backfill with a live-progress toast. Silent (early
/// return) when `auto_fetch` is OFF in settings. Errors are swallowed
/// so a bad API key doesn't pop an error toast at boot.
export async function triggerArtworkBackfill(
  toast: BackfillToast,
  source: string = "unknown",
  opts: TriggerArtworkBackfillOptions = {}
): Promise<void> {
  const { onlyIds, force = false } = opts;
  // Empty array = caller knows there's nothing new to fetch. Avoid the
  // "Looking for artwork…" → "0/0" toast flash entirely.
  if (onlyIds !== undefined && onlyIds.length === 0) {
    console.info(`[backfill] skip from=${source} (no new game ids)`);
    return;
  }
  const scope = onlyIds === undefined ? "full" : `ids:${onlyIds.length}`;
  console.info(`[backfill] trigger from=${source} scope=${scope} (inFlight=${isLocked()})`);
  if (isLocked()) {
    console.warn(`[backfill] DROPPED — already running (caller=${source})`);
    return;
  }
  setLocked(true);
  try {
    const settings = await getSteamgriddbSettings();
    console.info(`[backfill] settings auto_fetch=${settings.auto_fetch} prefer_animated=${settings.prefer_animated} allow_nsfw=${settings.allow_nsfw}`);
    if (!settings.auto_fetch) {
      console.info("[backfill] skipped — auto_fetch is OFF");
      return;
    }

    const t = i18n.t.bind(i18n);
    const startId = toast.info(t("toast.artwork_looking"), {
      persistent: true,
    });
    const unlisten = await listen<ProgressPayload>(
      "artwork-backfill-progress",
      (event) => {
        const { current, total, title, updated, done } = event.payload;
        console.info(
          `[backfill] progress ${current}/${total} title=${title} updated=${updated} done=${done}`
        );
        if (total === 0) return;
        if (!done) {
          toast.update(startId, {
            kind: "info",
            message: t("toast.artwork_fetching", { current, total, title }),
          });
        }
      }
    );
    console.info(`[backfill] calling fetchAllArtwork(${scope}, force=${force})`);
    const updated = await fetchAllArtwork(onlyIds, force);
    console.info(`[backfill] fetchAllArtwork returned updated=${updated}`);
    unlisten();
    toast.dismiss(startId);
    if (updated > 0) {
      toast.success(
        updated === 1
          ? t("toast.artwork_added_one")
          : t("toast.artwork_added_many", { count: updated })
      );
    }
  } catch {
    // Silent on purpose — see module doc.
  } finally {
    setLocked(false);
  }
}
