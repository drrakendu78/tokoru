import { useEffect, useRef, useState } from "react";
import { triggerArtworkBackfill } from "./lib/artworkBackfill";
// (syncMetadataNow imported below alongside getMetadataStatus)
import { RouterProvider, useRouter, type Route } from "./router";
import { Library } from "./pages/Library";
import { Downloads } from "./pages/Downloads";
import { Sources } from "./pages/Sources";
import { Stats } from "./pages/Stats";
import { Settings } from "./pages/Settings";
import { Onboarding } from "./pages/Onboarding";
import { ToastProvider, useToast } from "./components/Toast";
import {
  fullScan,
  getMetadataStatus,
  importStarcitizenPlaytime,
  isArtworkInitialBackfillDone,
  isOnboardingDone,
  mirrorExistingArtworkToSteam,
  syncMetadataNow,
  syncSteamLibrary,
} from "./lib/api";

/// Side-effect-only component that runs once when the app mounts to
/// surface "things Tokoru can do on its own DB" — currently just the
/// metadata enrichment for franchise/tags/dev/publisher. We deliberately
/// don't include anything that touches Steam (rebuild collections, sync
/// playtime) — the user has to choose to fire those.
///
/// Stays silent on a fresh install (no Steam library connected → nothing
/// to sync). Doesn't spam: one toast per app launch, dismissable, with
/// an inline "Sync now" action that fires `sync_metadata_now` directly
/// so the user doesn't have to navigate anywhere.
// Module-level latch — survives React StrictMode's double-mount in dev
// (which would otherwise queue two background syncs) but is reset on a
// fresh webview reload (Ctrl+R) since the module re-executes. That's the
// behavior we want: launch Tokoru or hard-reload → sync runs again
// if data is stale; pure HMR / navigation re-mount → no re-trigger.
let startupSyncTriggered = false;

function StartupChecks() {
  const toast = useToast();
  // Stable ref so the effect body can call into the latest toast methods
  // without re-running (the provider's context value reference rotates
  // every time a toast is pushed).
  const toastRef = useRef(toast);
  toastRef.current = toast;

  useEffect(() => {
    if (startupSyncTriggered) return;
    startupSyncTriggered = true;
    // Background Steam library re-sync — keeps `last_played_imported`
    // fresh so the Library "Recently Played" filter doesn't drift.
    // Silent: any failure (token expired, network) is swallowed; the
    // user will see stale data but never an error toast. The Steam
    // sync also chains the favorites import (Steam-side collection).
    syncSteamLibrary().catch(() => {});
    // Catch-up mirror for libraries whose artwork was fetched by an older
    // Tokoru build that didn't push grid overrides to Steam. Silent + one-
    // shot (gated by `steam_grid_mirror_done` in sync_state): after this
    // runs once, every subsequent artwork pick is mirrored synchronously
    // by `fetch_artwork` / `fetch_all_artwork`, so this no-ops forever.
    mirrorExistingArtworkToSteam().catch(() => {});
    // Star Citizen playtime — re-parse `<install>/logbackups/*.log`
    // every boot to surface session totals Tokoru's local watcher
    // missed (game launched directly from RSI Launcher outside the
    // current Tokoru session). Silent: no-op when SC isn't
    // installed, when the row isn't present, or when there are no
    // logs yet.
    importStarcitizenPlaytime().catch(() => {});
    // Auto re-scan ALL local launchers (Epic / GOG / Ubi / EA / Xbox /
    // Amazon registry + manifest reads) so newly-installed games show
    // up without forcing the user to hit a Rescan button anywhere. The
    // call is cheap — just a few registry reads + manifest globs — and
    // silent: any new game flows into the library via the usual upsert
    // path. Failure is swallowed so a transient detector hiccup never
    // surfaces an error toast at launch.
    //
    // Artwork backfill gating: on first ever boot (`isArtworkInitialBackfillDone`
    // returns false), do a full library pass. On every boot after that, only
    // fetch artwork for the games newly inserted by this very scan — so a
    // 800-game library doesn't trigger an 800-game backfill every time the
    // user opens the app. The full-pass success flips the gate inside
    // `fetch_all_artwork`.
    Promise.all([fullScan(), isArtworkInitialBackfillDone()])
      .then(([scan, initialDone]) => {
        if (!initialDone) {
          void triggerArtworkBackfill(toastRef.current, "boot:first-run");
        } else {
          void triggerArtworkBackfill(toastRef.current, "boot:incremental", {
            onlyIds: scan.new_game_ids,
          });
        }
      })
      .catch(() => {});

    // Re-scan when the user comes back to the window — typical flow is
    // they alt-tab to EA App / Ubisoft Connect, install a game, then
    // refocus Tokoru and expect to see it immediately. Debounced to once
    // per 60s so rapid focus toggles don't hammer the registry.
    //
    // Focus path is ALWAYS incremental: by the time the user comes back,
    // the initial pass has happened on a previous boot (or is happening
    // now and the `inFlight` lock will drop us anyway), so we only care
    // about freshly-installed games. Empty `new_game_ids` short-circuits
    // silently — no toast unless we actually have something to fetch.
    let lastFocusScan = Date.now();
    const onFocus = () => {
      const now = Date.now();
      if (now - lastFocusScan < 60_000) return;
      lastFocusScan = now;
      fullScan()
        .then((scan) =>
          triggerArtworkBackfill(toastRef.current, "focus", {
            onlyIds: scan.new_game_ids,
          })
        )
        .catch(() => {});
    };
    window.addEventListener("focus", onFocus);
    // No `cancelled` flag here: React StrictMode in dev double-mounts the
    // component, which would normally have the first mount's cleanup set
    // `cancelled = true` before the promise resolves — silently killing
    // the toast. Since the work is intentionally one-shot per app launch
    // (and the module-level latch handles re-entry), we let the promise
    // chain run to completion regardless of unmount.
    let pollTimer: ReturnType<typeof setInterval> | null = null;
    let progressToastId: number | null = null;

    const clearPoll = () => {
      if (pollTimer !== null) {
        clearInterval(pollTimer);
        pollTimer = null;
      }
    };

    getMetadataStatus()
      .then((status) => {
        if (status.total_steam_games === 0) return;
        if (status.pending_count === 0) return;

        const total = status.total_steam_games;
        const initialPending = status.pending_count;
        const initialSynced = total - initialPending;

        // Persistent progress toast — its message gets live-edited by
        // the poll loop below every 3 seconds while the sync runs.
        progressToastId = toastRef.current.info(
          `Syncing metadata… ${initialSynced}/${total} done (${initialPending} remaining).`,
          { persistent: true }
        );

        pollTimer = setInterval(() => {
          getMetadataStatus()
            .then((s) => {
              if (progressToastId === null) return;
              const synced = s.total_steam_games - s.pending_count;
              toastRef.current.update(progressToastId, {
                message: `Syncing metadata… ${synced}/${s.total_steam_games} done (${s.pending_count} remaining).`,
              });
            })
            .catch(() => {});
        }, 3000);

        syncMetadataNow()
          .then((r) => {
            clearPoll();
            if (progressToastId !== null) {
              toastRef.current.update(progressToastId, {
                kind: "success",
                message: `Metadata synced: ${r.synced}/${r.total}${
                  r.failed > 0 ? ` (${r.failed} failed)` : ""
                }. Franchise grouping is now ready.`,
              });
            }
          })
          .catch((e) => {
            clearPoll();
            const msg = e instanceof Error ? e.message : String(e);
            if (progressToastId !== null) {
              toastRef.current.update(progressToastId, {
                kind: "error",
                message: `Metadata sync failed: ${msg}`,
              });
            }
          });
      })
      .catch(() => {});
    return () => {
      window.removeEventListener("focus", onFocus);
      clearPoll();
    };
  }, []);
  return null;
}

function CurrentPage() {
  const { route } = useRouter();
  switch (route) {
    case "onboarding":
      return <Onboarding />;
    case "downloads":
      return <Downloads />;
    case "sources":
      return <Sources />;
    case "stats":
      return <Stats />;
    case "settings":
      return <Settings />;
    case "library":
    default:
      return <Library />;
  }
}

export default function App() {
  // Route picked at boot: `onboarding` on a fresh install, `library`
  // afterwards. `null` while we wait on the backend probe — the brief
  // delay (~1 frame) avoids flashing the Library before swapping to the
  // wizard. ToastProvider mounts unconditionally so any error toast
  // raised during the probe still has a place to land.
  const [initialRoute, setInitialRoute] = useState<Route | null>(null);
  useEffect(() => {
    let cancelled = false;
    isOnboardingDone()
      .then((done) => {
        if (!cancelled) setInitialRoute(done ? "library" : "onboarding");
      })
      .catch(() => {
        // Backend probe failed — default to Library so the app stays
        // usable. The user can still trigger the wizard manually from
        // Settings later if/when we add a "Replay onboarding" button.
        if (!cancelled) setInitialRoute("library");
      });
    return () => {
      cancelled = true;
    };
  }, []);
  return (
    <ToastProvider>
      {initialRoute && (
        <>
          <StartupChecks />
          <RouterProvider initial={initialRoute}>
            <CurrentPage />
          </RouterProvider>
        </>
      )}
    </ToastProvider>
  );
}
