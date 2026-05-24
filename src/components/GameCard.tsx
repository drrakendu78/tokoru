import { useState } from "react";
import {
  pushToSteam,
  removeFromSteam,
  restartSteam,
  getRestartAfterPush,
} from "../lib/api";
import type { DownloadState, Game, Shortcut } from "../lib/types";
import { SOURCE_DOT } from "../lib/types";
import { SourceBadge } from "./SourceBadge";
import { useTranslation } from "react-i18next";
import { StatusPill } from "./StatusPill";
import type { SteamStatus } from "./StatusPill";
import { useToast } from "./Toast";

interface GameCardProps {
  game: Game;
  shortcut?: Shortcut | null;
  download?: DownloadState | null;
  selected: boolean;
  onToggle: () => void;
  onOpen: () => void;
  onShortcutChange?: () => void;
  onInstall?: () => void;
  onPauseDownload?: () => void;
  onResumeDownload?: () => void;
  onRetryDownload?: () => void;
  /// Reflects the same `favorite` flag as the GameDetail heart icon —
  /// kept in sync via the parent's `favoriteIds` Set so flips on either
  /// surface stay coherent without an extra IPC roundtrip per card.
  isFavorite?: boolean;
  /// Optimistic toggle handler. Parent updates `favoriteIds` immediately
  /// then fires the IPC + debounced push to Steam Favoris (same pattern
  /// as GameDetail's `toggleFavorite`).
  onToggleFavorite?: () => void;
  animationDelay?: string;
}

function statusFromShortcut(
  shortcut: Shortcut | null | undefined,
  source: string
): SteamStatus {
  // Steam-native games are by definition already in Steam — no shortcut row
  // ever needs to be created for them.
  if (source === "steam") return "in-steam";
  if (!shortcut) return "not-in-steam";
  switch (shortcut.status) {
    case "pushed":
      return "in-steam";
    case "pending":
      return "syncing";
    case "removed":
    default:
      return "not-in-steam";
  }
}

function formatSpeed(bps: number): string {
  if (bps <= 0) return "—";
  const mibs = bps / (1024 * 1024);
  if (mibs >= 1) return `${mibs.toFixed(1)} MB/s`;
  const kbs = bps / 1024;
  return `${kbs.toFixed(0)} KB/s`;
}

function formatEta(secs: number): string {
  if (secs <= 0) return "—";
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

export function GameCard({
  game,
  shortcut,
  download,
  selected,
  onToggle,
  onOpen,
  onShortcutChange,
  onInstall,
  onPauseDownload,
  onResumeDownload,
  onRetryDownload,
  isFavorite = false,
  onToggleFavorite,
  animationDelay,
}: GameCardProps) {
  const { t } = useTranslation();
  const source = (game.source as string) || "custom";
  const status = statusFromShortcut(shortcut, source);
  const isSteamNative = source === "steam";
  const dot = SOURCE_DOT[source] ?? "#A1A1A6";
  const toast = useToast();
  const [busy, setBusy] = useState(false);

  // ── Install / download state machine ──────────────────────────────
  // The 5 spec states (see `.aidesigner/runs/2026-05-21T18-25-43-697Z-now-design-the-5-game-card-install-s/design.html`):
  //   1. Installed     — game has install_path, no active download.
  //   2. Owned         — Epic/GOG game with no install_path AND no active download.
  //   3. Downloading   — DownloadState.status === "downloading".
  //   4. Paused        — DownloadState.status === "paused".
  //   5. Failed        — DownloadState.status === "failed".
  //
  // For sources without a runner (ubi/ea/itch/custom/local) we never
  // get to "Owned" — they all surface as "Installed" because the local
  // scanner only adds them when the binary already exists on disk. Steam
  // games are always installed (sync via API uses the local detect path too).
  // Steam is "installable" too — via `steam://install/<appid>` which Steam
  // intercepts and handles itself. The parent Library page wires onInstall
  // to dispatch on game.source (Epic/GOG → legendary/gogdl, Steam → shell).
  const installable = source === "epic" || source === "gog" || source === "steam";
  const dlStatus = download?.status;
  const isDownloading = dlStatus === "downloading";
  const isPaused = dlStatus === "paused";
  const isFailed = dlStatus === "failed";
  const inFlight = isDownloading || isPaused || isFailed;
  const isOwnedNotInstalled =
    installable && !game.install_path && !inFlight;

  const handlePush = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (busy || !game.id) return;
    setBusy(true);
    try {
      const res = await pushToSteam(game.id);
      toast.success(`${game.title} added to Steam (appid ${res.appid})`);
      const willRestart = getRestartAfterPush();
      // Skip the "Collection not updated" error when restart is queued —
      // restart_steam re-runs the rebuild after shutting Steam down,
      // which is the only point the leveldb write sticks.
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
    } catch (err) {
      toast.error(
        err instanceof Error ? err.message : `Push failed: ${String(err)}`
      );
    } finally {
      setBusy(false);
    }
  };

  const handleRemove = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (busy || !game.id) return;
    setBusy(true);
    try {
      await removeFromSteam(game.id);
      toast.success(`${game.title} removed from Steam`);
      onShortcutChange?.();
      // Steam ignores shortcuts.vdf edits while running — restart so the
      // sidebar reflects the removal immediately. Shares the same toggle
      // as the push flow.
      if (getRestartAfterPush()) {
        restartSteam()
          .then(() =>
            toast.success(
              "Steam restarted — shortcut removed + Collections updated"
            )
          )
          .catch((restartErr) =>
            toast.error(
              `Remove OK, but restart failed: ${
                restartErr instanceof Error ? restartErr.message : String(restartErr)
              }`
            )
          );
      }
    } catch (err) {
      toast.error(
        err instanceof Error ? err.message : `Remove failed: ${String(err)}`
      );
    } finally {
      setBusy(false);
    }
  };

  const handleInstall = (e: React.MouseEvent) => {
    e.stopPropagation();
    onInstall?.();
  };
  const handlePause = (e: React.MouseEvent) => {
    e.stopPropagation();
    onPauseDownload?.();
  };
  const handleResume = (e: React.MouseEvent) => {
    e.stopPropagation();
    onResumeDownload?.();
  };
  const handleRetry = (e: React.MouseEvent) => {
    e.stopPropagation();
    onRetryDownload?.();
  };

  // Visual treatment — opacity is the active install-state signal
  // (failed / downloading dim the art). For owned-but-not-installed we
  // used to desaturate + dim the art too, but the "Install" badge + the
  // "Owned · not installed" caption already telegraph that state — the
  // veil just made the cover ugly. Keep full color.
  const artClass = isFailed
    ? "opacity-50"
    : isDownloading || isPaused
      ? "opacity-60"
      : "opacity-100";

  const ringPct = Math.max(0, Math.min(100, download?.progress_pct ?? 0));

  return (
    <div
      className="group relative flex flex-col gap-3 cursor-pointer select-none card-enter"
      style={animationDelay ? { animationDelay } : undefined}
    >
      <div
        onClick={onOpen}
        className={`relative w-full aspect-[2/3] rounded-[14px] overflow-hidden shadow-ambient
                    transition-all duration-[400ms]
                    group-hover:scale-[1.02] group-hover:-translate-y-1 group-hover:shadow-premium group-active:scale-[0.98]
                    ${
                      selected
                        ? "ring-2 ring-accent ring-offset-2 ring-offset-shell"
                        : ""
                    }`}
        style={{ transitionTimingFunction: "cubic-bezier(0.2,0.8,0.2,1)" }}
      >
        {/* Multi-select checkbox — top-right when not in the "in-steam"
            badge zone. Only visible on hover OR when at least one card is
            already selected (so it stays discoverable once selection mode
            is active). Click toggles the selection without opening the
            detail. */}
        <button
          onClick={(e) => {
            e.stopPropagation();
            onToggle();
          }}
          className={`absolute top-2 right-2 z-40 w-5 h-5 rounded-md flex items-center justify-center transition-all duration-200 ${
            selected
              ? "bg-accent border border-accent text-white opacity-100"
              : "bg-black/60 backdrop-blur-md border border-white/30 text-transparent opacity-0 group-hover:opacity-100 hover:bg-black/80 hover:border-white/60"
          }`}
          aria-label={selected ? t("library.deselect") : t("library.select")}
          title={selected ? t("library.deselect") : t("library.select")}
        >
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round">
            <polyline points="20 6 9 17 4 12" />
          </svg>
        </button>
        {/* Source badge top-left. For Downloading, swap to a white pill
            with a spinning icon + "Téléchargement" label (matches the
            mockup state 5). Paused/Failed reuse the same shape with a
            different leading icon. */}
        <div
          className="absolute top-3 left-3 z-30"
          onClick={(e) => {
            e.stopPropagation();
            onToggle();
          }}
        >
          {isDownloading ? (
            <span className="dark-pill bg-zinc-200 text-zinc-900 text-[9px] font-bold px-1.5 py-0.5 rounded-[4px] uppercase tracking-wider backdrop-blur-md flex items-center gap-1.5 whitespace-nowrap shadow-sm">
              <svg className="animate-spin" width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round">
                <line x1="12" y1="2" x2="12" y2="6" />
                <line x1="12" y1="18" x2="12" y2="22" />
                <line x1="4.93" y1="4.93" x2="7.76" y2="7.76" />
                <line x1="16.24" y1="16.24" x2="19.07" y2="19.07" />
                <line x1="2" y1="12" x2="6" y2="12" />
                <line x1="18" y1="12" x2="22" y2="12" />
                <line x1="4.93" y1="19.07" x2="7.76" y2="16.24" />
                <line x1="16.24" y1="7.76" x2="19.07" y2="4.93" />
              </svg>
              {t("gamecard.downloading")}
            </span>
          ) : isPaused ? (
            <span className="dark-pill bg-zinc-700 text-white text-[9px] font-bold px-1.5 py-0.5 rounded-[4px] uppercase tracking-wider backdrop-blur-md flex items-center gap-1.5 whitespace-nowrap shadow-sm">
              <svg width="9" height="9" viewBox="0 0 24 24" fill="currentColor">
                <rect x="6" y="4" width="4" height="16" />
                <rect x="14" y="4" width="4" height="16" />
              </svg>
              {t("gamecard.paused")}
            </span>
          ) : (
            <SourceBadge source={source} selected={selected} />
          )}
          {isFailed && (
            // Error dot — matches mockup card 5
            <div className="absolute -top-1 -right-1 w-2.5 h-2.5 bg-[#FF3B30] rounded-full border-2 border-[#151518]" />
          )}
        </div>

        {/* Top-right pill — only the StatusPill "✓ Sur Steam" badge for
            shortcuts pushed to Steam (per mockup card 2). All other
            actions (pause / resume / retry / install) moved to the bottom
            action row. Native Steam games + non-pushed shortcuts render
            nothing here. */}
        <div className="absolute top-3 right-3 z-30">
          {!isSteamNative && !inFlight && !isOwnedNotInstalled && status === "in-steam" ? (
            <StatusPill status={status} />
          ) : null}
        </div>

        {/* Art base — when we have a real artwork URL, render JUST the
            image. The colored placeholder gradient below was painting a
            diagonal black veil ON TOP of vivid cover art (Cyberpunk
            etc.), so it's now a fallback only when artwork_url is
            missing. */}
        {game.artwork_url ? (
          (() => {
            const url = game.artwork_url;
            const ext = url.toLowerCase().split("?")[0];
            const isVideo = ext.endsWith(".webm") || ext.endsWith(".mp4");
            // Real video containers go through the hardware decoder
            // (D3D11VideoDecoder for VP9/H.264, enabled via the
            // `additionalBrowserArgs` flag in tauri.conf.json). Decoded
            // frames live on the GPU layer — zero CPU per cover even at
            // 60 fps. Animated .webp / .gif stay in <img> because
            // Chromium has no HW path for animated image formats.
            return isVideo ? (
              <video
                src={url}
                autoPlay
                loop
                muted
                playsInline
                preload="metadata"
                className={`absolute inset-0 z-0 w-full h-full object-cover transition-all duration-300 ${artClass}`}
                style={{ willChange: "transform" }}
              />
            ) : (
              <img
                src={url}
                alt={game.title}
                loading="lazy"
                decoding="async"
                className={`absolute inset-0 z-0 w-full h-full object-cover transition-all duration-300 ${artClass}`}
                onError={(e) => {
                  (e.currentTarget as HTMLImageElement).style.display = "none";
                }}
              />
            );
          })()
        ) : (
          <div
            className={`absolute inset-0 z-0 transition-all duration-300 ${artClass}`}
            style={{
              background: `linear-gradient(135deg, ${dot}33 0%, #0a0a0c 100%)`,
            }}
          />
        )}
        <div className="art-overlay" />

        {/* Red wash for failed state */}
        {isFailed && (
          <div className="absolute inset-0 bg-[#FF3B30]/10 mix-blend-screen z-10 pointer-events-none" />
        )}

        {/* Bottom progress sliver — coral when downloading, grey when paused */}
        {(isDownloading || isPaused) && (
          <div
            className={`absolute bottom-0 left-0 h-[3px] rounded-r-full z-20 ${
              isDownloading
                ? "bg-accent shadow-[0_-1px_10px_2px_rgba(255,70,51,0.5)]"
                : "bg-white/40"
            }`}
            style={{ width: `${ringPct}%` }}
          />
        )}

        {/* Hover scrim + action row — covers ALL states (Ajouter / Retirer /
            Sur Steam / Installer / 47% downloading / Pause 47% / Failed).
            Ported 1:1 from the aidesigner mockup run
            `2026-05-24T12-32-16-757Z-redessine-les-boutons-d-action-en-ba`.
            heart + CTA same height (h-9), full-pill rounded, heart fixed
            w-9 / CTA flex-1. */}
        <div className="absolute inset-0 z-10 bg-[linear-gradient(to_top,rgba(0,0,0,0.95)_0%,rgba(0,0,0,0.5)_40%,transparent_100%)] opacity-0 group-hover:opacity-100 transition-opacity duration-300 pointer-events-none" />
        <div className="absolute bottom-0 left-0 right-0 z-20 p-3.5 flex items-center justify-between gap-2.5 opacity-0 group-hover:opacity-100 transition-all duration-300 translate-y-2 group-hover:translate-y-0">
          <button
            onClick={(e) => {
              e.stopPropagation();
              onToggleFavorite?.();
            }}
            className={`dark-pill w-9 h-9 rounded-full shrink-0 flex items-center justify-center border transition-all ${
              isFavorite
                ? "bg-accent/10 border-accent/30 text-accent shadow-glow hover:bg-accent/20 hover:shadow-[0_0_16px_rgba(255,70,51,0.4)]"
                : "bg-black/40 backdrop-blur-md border-white/15 text-white/70 hover:bg-black/60 hover:text-white"
            }`}
            aria-label={isFavorite ? t("gamecard.unfavorite") : t("gamecard.favorite")}
            title={isFavorite ? t("gamecard.unfavorite") : t("gamecard.favorite")}
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill={isFavorite ? "currentColor" : "none"} stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
            </svg>
          </button>

          {/* CTA — state-driven, mockup states 1-5 mapped 1:1. */}
          {isDownloading ? (
            // State 5 — inline progress button. Click pauses.
            <button
              onClick={isSteamNative ? (e) => e.stopPropagation() : handlePause}
              disabled={isSteamNative}
              className="dark-pill h-9 flex-1 rounded-full bg-black/40 backdrop-blur-md border border-white/15 overflow-hidden relative transition-all disabled:cursor-default disabled:opacity-80"
              aria-label={isSteamNative ? t("gamecard.managed_by_steam") : t("gamecard.pause_download")}
              title={isSteamNative ? t("gamecard.managed_by_steam") : t("gamecard.pause_download")}
            >
              <div className="absolute top-0 left-0 bottom-0 bg-accent/20 w-full" />
              <div
                className="absolute top-0 left-0 bottom-0 bg-accent shadow-[0_0_8px_rgba(255,70,51,0.5)] transition-[width] duration-500"
                style={{ width: `${ringPct}%` }}
              />
              <div className="absolute inset-0 z-10 flex items-center justify-center px-3 gap-2 text-white whitespace-nowrap">
                <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor">
                  <rect x="6" y="4" width="4" height="16" />
                  <rect x="14" y="4" width="4" height="16" />
                </svg>
                <span className="text-[13px] font-semibold drop-shadow-[0_1px_1px_rgba(0,0,0,0.5)]">
                  {Math.round(download?.progress_pct ?? 0)}%
                </span>
              </div>
            </button>
          ) : isPaused ? (
            // Paused — same shape, grey fill, click resumes.
            <button
              onClick={isSteamNative ? (e) => e.stopPropagation() : handleResume}
              disabled={isSteamNative}
              className="dark-pill h-9 flex-1 rounded-full bg-black/40 backdrop-blur-md border border-white/15 overflow-hidden relative transition-all disabled:cursor-default disabled:opacity-80"
              aria-label={isSteamNative ? t("gamecard.managed_by_steam") : t("gamecard.resume_download")}
              title={isSteamNative ? t("gamecard.managed_by_steam") : t("gamecard.resume_download")}
            >
              <div className="absolute top-0 left-0 bottom-0 bg-white/[0.05] w-full" />
              <div
                className="absolute top-0 left-0 bottom-0 bg-white/30 transition-[width] duration-500"
                style={{ width: `${ringPct}%` }}
              />
              <div className="absolute inset-0 z-10 flex items-center justify-center px-3 gap-2 text-white whitespace-nowrap">
                <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor">
                  <polygon points="5 3 19 12 5 21 5 3" />
                </svg>
                <span className="text-[13px] font-semibold drop-shadow-[0_1px_1px_rgba(0,0,0,0.5)]">
                  {Math.round(download?.progress_pct ?? 0)}%
                </span>
              </div>
            </button>
          ) : isFailed ? (
            // Failed — red CTA with retry icon.
            <button
              onClick={handleRetry}
              className="h-9 flex-1 rounded-full bg-[#FF3B30]/15 border border-[#FF3B30]/30 text-[#FF3B30] hover:bg-[#FF3B30]/25 flex items-center justify-center gap-1.5 px-3 transition-all whitespace-nowrap"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                <path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" />
                <path d="M3 3v5h5" />
              </svg>
              <span className="text-[13px] font-semibold">
                {t("gamecard.retry_download")}
              </span>
            </button>
          ) : isOwnedNotInstalled ? (
            // State 4 — Installer (coral plein with download icon).
            <button
              onClick={handleInstall}
              className="h-9 flex-1 rounded-full bg-accent hover:bg-accent-hover text-white flex items-center justify-center gap-1.5 px-3 shadow-glow transition-all whitespace-nowrap"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                <polyline points="7 10 12 15 17 10" />
                <line x1="12" y1="15" x2="12" y2="3" />
              </svg>
              <span className="text-[13px] font-semibold">
                {t("gamecard.install")}
              </span>
            </button>
          ) : isSteamNative ? (
            // State 3 — Sur Steam (disabled pill).
            <div className="dark-pill h-9 flex-1 rounded-full bg-black/40 backdrop-blur-md border border-white/15 text-white/60 flex items-center justify-center gap-1.5 px-3 cursor-default">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                <polyline points="20 6 9 17 4 12" />
              </svg>
              <span className="text-[13px] font-medium whitespace-nowrap">
                {t("gamecard.on_steam")}
              </span>
            </div>
          ) : status === "in-steam" ? (
            // State 2 — Retirer (transparent, coral hover).
            <button
              onClick={handleRemove}
              disabled={busy}
              className="dark-pill h-9 flex-1 rounded-full bg-black/40 backdrop-blur-md border border-white/15 text-white/90 hover:bg-accent hover:border-transparent hover:text-white hover:shadow-glow flex items-center justify-center gap-1.5 px-3 transition-all disabled:opacity-50 group/btn"
            >
              <svg className="opacity-70 group-hover/btn:opacity-100" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                <line x1="5" y1="12" x2="19" y2="12" />
              </svg>
              <span className="text-[13px] font-medium whitespace-nowrap">
                {busy ? t("gamecard.removing") : t("gamecard.remove_from_steam")}
              </span>
            </button>
          ) : (
            // State 1 — Ajouter (coral plein, default).
            <button
              onClick={handlePush}
              disabled={busy}
              className="h-9 flex-1 rounded-full bg-accent hover:bg-accent-hover text-white flex items-center justify-center gap-1.5 px-3 shadow-glow transition-all disabled:opacity-60"
            >
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                <line x1="12" y1="5" x2="12" y2="19" />
                <line x1="5" y1="12" x2="19" y2="12" />
              </svg>
              <span className="text-[13px] font-semibold whitespace-nowrap">
                {busy ? t("gamecard.removing") : t("gamecard.add_to_steam")}
              </span>
            </button>
          )}
        </div>
      </div>

      {/* Meta text — uses semantic text tokens so the title flips dark
          in light theme. (`text-white/95` was invisible on light shell
          because the override targets `.text-white` literal, not opacity
          variants). */}
      <div className="px-1 shrink-0">
        <h3
          className={`text-[13px] font-semibold truncate w-full ${
            isOwnedNotInstalled ? "text-text-sec" : "text-text-main"
          }`}
        >
          {game.title}
        </h3>
        {isDownloading ? (
          <p className="text-[10px] text-accent mt-1 truncate w-full font-mono tracking-tight uppercase">
            {t("gamecard.downloading")} · {Math.round(download?.progress_pct ?? 0)}% ·{" "}
            {formatSpeed(download?.speed_bps ?? 0)} ·{" "}
            {formatEta(download?.eta_secs ?? 0)}
          </p>
        ) : isPaused ? (
          <p className="text-[10px] text-text-muted mt-1 truncate w-full font-mono tracking-tight uppercase">
            {t("gamecard.paused")} · {Math.round(download?.progress_pct ?? 0)}%
          </p>
        ) : isFailed ? (
          <p className="text-[10px] text-error/80 mt-1 truncate w-full font-mono tracking-tight uppercase">
            {t("gamecard.failed")} · {download?.last_error ?? t("gamecard.unknown_error")}
          </p>
        ) : isOwnedNotInstalled ? (
          <p className="text-[11px] mt-0.5 truncate w-full text-text-muted">
            {t("gamecard.owned_not_installed")}
          </p>
        ) : (
          <p className="text-[11px] mt-0.5 truncate w-full text-text-muted">
            {game.install_path ?? game.exe_path ?? "—"}
          </p>
        )}
      </div>
    </div>
  );
}
