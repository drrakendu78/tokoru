// Sources page — verbatim port of the AIDesigner mockup at
// `.aidesigner/runs/2026-05-20T22-33-58-265Z-now-design-the-sources-page-for-stea/design.html`.
//
// Visual fidelity is the priority. Toggle switches and "edit install path"
// pencils are visual-only (no backend yet) — real handlers (connect / sync /
// disconnect / rescan) are wired through useSourceTiles where applicable.

import { useCallback, useMemo, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { TopBar } from "../components/TopBar";
import { BrandLogo } from "../components/BrandLogo";
import { useGames } from "../lib/useGames";
import { useSourceTiles } from "../lib/useSourceTiles";
import { fullScan } from "../lib/api";
import { useToast } from "../components/Toast";
import type { SourceTileHandlers } from "../lib/useSourceTiles";

// ---- Helpers ----------------------------------------------------------------

function formatRelative(unix: number | null, t: (k: string, opts?: Record<string, unknown>) => string): string {
  if (!unix) return t("sources_page.relative.never");
  const diff = Date.now() / 1000 - unix;
  if (diff < 60) return t("sources_page.relative.just_now");
  if (diff < 3600) {
    const m = Math.floor(diff / 60);
    return t("sources_page.relative.minutes", { count: m });
  }
  if (diff < 86400) {
    const h = Math.floor(diff / 3600);
    return t("sources_page.relative.hours", { count: h });
  }
  if (diff < 86400 * 7) {
    const d = Math.floor(diff / 86400);
    return d === 1
      ? t("sources_page.relative.yesterday")
      : t("sources_page.relative.days", { count: d });
  }
  return t("sources_page.relative.a_week_ago");
}

// ---- Brand logos via /public/logos (SimpleIcons) ----------------------------

const BRAND_COLOR: Record<string, string> = {
  steam: "#66c0f4",
  epic: "#ffffff",
  gog: "#C173FF",
  ubi: "#0088F0",
  ea: "#ffffff",
  itch: "#FA5C5C",
  custom: "#ffffff",
};

// ---- Pencil button (visual-only "edit install path") ------------------------

function PencilButton() {
  return (
    <button
      className="p-2 text-text-muted hover:text-white rounded-lg hover:bg-white/10 transition-colors"
      title="Edit install path"
      type="button"
    >
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
        <path d="M17 3a2.828 2.828 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5L17 3z"></path>
      </svg>
    </button>
  );
}

// ---- Visual-only Toggle -----------------------------------------------------

function Toggle({ on, disabled = false }: { on: boolean; disabled?: boolean }) {
  if (disabled) {
    return (
      <button
        className="w-11 h-6 rounded-full bg-white/5 relative border border-white/5 cursor-not-allowed"
        aria-pressed="false"
        disabled
      >
        <span className="absolute top-[2px] left-[2px] w-4 h-4 rounded-full bg-white/20 shadow-sm transition-all duration-300" />
      </button>
    );
  }
  return (
    <button
      className={
        on
          ? "w-11 h-6 rounded-full bg-accent relative transition-colors border border-white/10 shadow-glow"
          : "w-11 h-6 rounded-full bg-white/10 relative transition-colors border border-white/10 hover:bg-white/20"
      }
      aria-pressed={on}
    >
      <span
        className={
          on
            ? "absolute top-[2px] left-[22px] w-4 h-4 rounded-full bg-white shadow-sm transition-all duration-300"
            : "absolute top-[2px] left-[2px] w-4 h-4 rounded-full bg-text-sec shadow-sm transition-all duration-300"
        }
      />
    </button>
  );
}

// ---- Source row (used for Steam/Epic/GOG/Ubi/EA/Itch) -----------------------

interface SourceRowProps {
  source: string;
  label: string;
  pathLine: string;
  handlers: SourceTileHandlers;
  scanning?: boolean;
}

function SourceRow({ source, label, pathLine, handlers, scanning = false }: SourceRowProps) {
  const { t } = useTranslation();
  const { state, busy, progress, onConnect, onSync, onDisconnect, onRescan } = handlers;
  const glowColor = BRAND_COLOR[source] ?? "#ffffff";
  const rowScanning = scanning || busy;
  const isOAuth = state.supports_login;
  const isConnected = isOAuth ? state.connected : true;

  // Per-row right-side controls. Pencil + Rescan + separator + Toggle.
  const rightCluster = (
    <div className="flex items-center gap-3 shrink-0 opacity-80 group-hover:opacity-100 transition-opacity">
      <PencilButton />
      <button
        onClick={isOAuth ? (isConnected ? onSync : onConnect) : onRescan}
        disabled={rowScanning}
        className="px-3 py-1.5 text-xs font-semibold text-text-sec hover:text-white border border-white/10 hover:bg-white/5 rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
      >
        {isOAuth && !isConnected ? t("sources_page.connect") : t("sources_page.rescan")}
      </button>
      {isOAuth && isConnected && (
        <button
          onClick={onDisconnect}
          className="px-3 py-1.5 text-xs font-semibold text-text-muted hover:text-white border border-white/10 hover:bg-white/5 rounded-lg transition-colors"
        >
          {t("sources_page.disconnect")}
        </button>
      )}
      <div className="w-px h-6 bg-white/10 mx-2"></div>
      <Toggle on={isConnected} />
    </div>
  );

  const ringClasses = rowScanning
    ? "border-accent/40 ring-1 ring-accent/20 shadow-[inset_0_1px_rgba(255,255,255,0.05)]"
    : "border-white/[0.04]";

  // Subtitle: connected/sync info or scan info.
  let subtitle: ReactNode = null;
  if (isOAuth && isConnected) {
    const who = state.account_name ?? (source === "steam" && state.steam_id ? `Steam #${state.steam_id}` : null);
    subtitle = (
      <div className="text-xs font-medium text-text-sec">
        {who ? t("sources_page.signed_in_as", { name: who }) : t("sources_page.connected")} ·{" "}
        {t("sources_page.games_imported", { count: state.game_count })}
        {state.last_sync_at && <> · {t("sources_page.last_sync", { when: formatRelative(state.last_sync_at, t) })}</>}
      </div>
    );
  } else if (!isOAuth) {
    subtitle = (
      <div className="text-xs font-medium text-text-sec">
        {t("sources_page.last_scanned", { when: formatRelative(state.last_sync_at, t) })} ·{" "}
        {t("sources_page.games_found", { count: state.game_count })}
      </div>
    );
  }

  return (
    <div
      className={`relative group flex items-center justify-between p-5 pr-6 rounded-2xl bg-surface/40 hover:bg-surface/60 ${ringClasses} transition-all gap-5 overflow-hidden`}
    >
      {/* Left: Icon */}
      <div className="w-14 h-14 rounded-[14px] bg-[#1a1a1c] border border-white/10 flex items-center justify-center relative shrink-0">
        <div
          className="absolute inset-0 opacity-20 blur-[12px] rounded-full group-hover:opacity-30 transition-opacity"
          style={{ background: glowColor }}
        ></div>
        <BrandLogo source={source} size={28} color={glowColor} className="relative z-10" />
      </div>

      {/* Center: Content */}
      <div className="flex-1 min-w-0 flex flex-col justify-center gap-1.5">
        <div className="flex items-center gap-3">
          <h3 className="text-lg font-bold text-white tracking-tight leading-none">
            {label}
          </h3>
          {rowScanning && (
            <span className="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-md bg-accent/15 border border-accent/20 text-accent text-[10px] font-bold uppercase tracking-widest">
              <span className="w-1.5 h-1.5 rounded-full bg-accent animate-pulse"></span>
              {progress && progress.total > 0
                ? t("sources_page.syncing", { processed: progress.processed, total: progress.total })
                : progress
                ? t("sources_page.fetching")
                : t("sources_page.scanning")}
            </span>
          )}
        </div>
        <div className="text-[13px] text-text-muted font-mono truncate">{pathLine}</div>
        {progress && progress.title ? (
          <div className="text-[12px] text-text-muted truncate italic">
            → {progress.title}
          </div>
        ) : (
          subtitle
        )}
      </div>

      {/* Right: Actions */}
      {rightCluster}

      {/* Bottom progress sliver when scanning — real % bar if we have
          per-game progress, indeterminate animation otherwise. */}
      {rowScanning && (
        <div className="absolute bottom-0 left-0 right-0 h-[2px] bg-white/5">
          {progress && progress.total > 0 ? (
            <div
              className="h-full bg-accent rounded-r-full transition-[width] duration-150 ease-out"
              style={{
                width: `${Math.min(100, Math.round((progress.processed / progress.total) * 100))}%`,
              }}
            />
          ) : (
            <div className="h-full bg-accent progress-indeterminate rounded-r-full" />
          )}
        </div>
      )}
    </div>
  );
}

// ---- Custom .exe expanded card ----------------------------------------------

interface CustomCardProps {
  handlers: SourceTileHandlers | null;
  customGames: Array<{ id: string | null; title: string; exe_path: string }>;
}

function CustomCard({ handlers, customGames }: CustomCardProps) {
  const { t } = useTranslation();
  const count = customGames.length;
  return (
    <div className="relative flex flex-col p-6 rounded-[24px] bg-gradient-to-b from-surface/50 to-surface/30 border border-white/[0.06] mt-8 gap-6 shadow-ambient">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <h3 className="text-xl font-bold text-white tracking-tight">{t("sources_page.custom.title")}</h3>
          <span className="px-2 py-0.5 rounded-full bg-white/5 text-text-sec text-xs font-semibold uppercase tracking-widest">
            {t("sources_page.custom.added_count", { count })}
          </span>
        </div>
        <button
          onClick={handlers?.onRescan}
          className="px-3 py-1.5 text-xs font-semibold text-text-sec hover:text-white border border-white/10 hover:bg-white/5 rounded-lg transition-colors"
        >
          {t("sources_page.rescan")}
        </button>
      </div>

      {/* Scroller */}
      {count > 0 && (
        <div className="flex gap-4 overflow-x-auto pb-4 pt-1 snap-x">
          {customGames.slice(0, 12).map((g, i) => (
            <div
              key={g.id ?? `${g.exe_path}-${i}`}
              className="w-28 flex flex-col gap-2.5 shrink-0 snap-start group cursor-pointer"
            >
              <div className="aspect-[2/3] w-full rounded-xl overflow-hidden relative shadow-md ring-1 ring-white/10 group-hover:ring-white/20 transition-all group-hover:-translate-y-1">
                <div
                  className="absolute inset-0 opacity-80 group-hover:opacity-100 transition-opacity"
                  style={{ background: "linear-gradient(135deg, #FF4633 0%, #1A0B2E 100%)" }}
                />
                <div className="absolute inset-0 bg-gradient-to-t from-black/80 to-transparent"></div>
                <button className="absolute top-2 right-2 p-1.5 rounded-md bg-black/60 text-white/70 hover:text-white backdrop-blur-md opacity-0 group-hover:opacity-100 transition-opacity">
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                    <path d="M17 3a2.828 2.828 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5L17 3z"></path>
                  </svg>
                </button>
              </div>
              <div>
                <h4 className="text-xs font-bold text-white truncate">{g.title}</h4>
                <p className="text-[10px] text-text-muted font-mono truncate mt-0.5">
                  {g.exe_path}
                </p>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Inline add form (verbatim from mockup) */}
      <div className="bg-black/30 rounded-2xl p-6 border border-white/[0.04] mt-2 shadow-[inset_0_2px_10px_rgba(0,0,0,0.2)]">
        <div className="flex items-center justify-between mb-5">
          <h4 className="text-sm font-semibold text-white uppercase tracking-widest">
            {t("sources_page.custom.add_new_executable")}
          </h4>
          <button className="text-text-muted hover:text-white transition-colors">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <line x1="18" y1="6" x2="6" y2="18"></line>
              <line x1="6" y1="6" x2="18" y2="18"></line>
            </svg>
          </button>
        </div>

        <div className="grid grid-cols-1 md:grid-cols-3 gap-8">
          {/* Inputs Col */}
          <div className="col-span-1 md:col-span-2 space-y-5">
            <div className="relative group">
              <label className="block text-[11px] font-bold text-text-sec uppercase tracking-widest mb-2">
                {t("sources_page.custom.game_name")}
              </label>
              <input
                type="text"
                placeholder={t("sources_page.custom.game_name_placeholder")}
                className="w-full bg-white/[0.03] border border-white/[0.05] focus:border-white/10 rounded-xl px-4 py-3 text-sm text-white placeholder-text-muted outline-none transition-all hover:bg-white/[0.05] shadow-[inset_0_1px_2px_rgba(0,0,0,0.2)]"
              />
            </div>
            <div className="relative group">
              <label className="block text-[11px] font-bold text-text-sec uppercase tracking-widest mb-2">
                {t("sources_page.custom.exe_path")}
              </label>
              <div className="flex gap-2">
                <input
                  type="text"
                  placeholder={t("sources_page.custom.exe_path_placeholder")}
                  className="flex-1 bg-white/[0.03] border border-white/[0.05] focus:border-white/10 rounded-xl px-4 py-3 text-[13px] font-mono text-white placeholder-text-muted outline-none transition-all hover:bg-white/[0.05] shadow-[inset_0_1px_2px_rgba(0,0,0,0.2)]"
                />
                <button className="px-5 py-3 rounded-xl bg-white/5 hover:bg-white/10 text-white text-[13px] font-semibold border border-white/5 transition-colors shrink-0">
                  {t("sources_page.custom.browse")}
                </button>
              </div>
            </div>
            <div className="grid grid-cols-2 gap-4">
              <div className="relative group">
                <label className="block text-[11px] font-bold text-text-sec uppercase tracking-widest mb-2">
                  {t("sources_page.custom.working_dir")}{" "}
                  <span className="text-text-muted font-normal normal-case tracking-normal">
                    {t("sources_page.custom.optional")}
                  </span>
                </label>
                <input
                  type="text"
                  className="w-full bg-white/[0.03] border border-white/[0.05] focus:border-white/10 rounded-xl px-4 py-3 text-[13px] font-mono text-white placeholder-text-muted outline-none transition-all hover:bg-white/[0.05]"
                />
              </div>
              <div className="relative group">
                <label className="block text-[11px] font-bold text-text-sec uppercase tracking-widest mb-2">
                  {t("sources_page.custom.launch_args")}{" "}
                  <span className="text-text-muted font-normal normal-case tracking-normal">
                    {t("sources_page.custom.optional")}
                  </span>
                </label>
                <input
                  type="text"
                  placeholder={t("sources_page.custom.launch_args_placeholder")}
                  className="w-full bg-white/[0.03] border border-white/[0.05] focus:border-white/10 rounded-xl px-4 py-3 text-[13px] font-mono text-white placeholder-text-muted outline-none transition-all hover:bg-white/[0.05]"
                />
              </div>
            </div>
          </div>

          {/* Dropzone Col */}
          <div className="col-span-1 flex flex-col items-center">
            <label className="w-full text-[11px] font-bold text-text-sec uppercase tracking-widest mb-2">
              {t("sources_page.custom.cover_art")}
            </label>
            <div className="w-full h-full min-h-[160px] border-2 border-dashed border-white/10 hover:border-accent/40 rounded-xl bg-white/[0.02] hover:bg-accent/5 flex flex-col items-center justify-center gap-3 text-center p-6 cursor-pointer transition-colors group relative">
              <div className="p-4 rounded-full bg-white/[0.04] group-hover:bg-accent/10 transition-colors">
                <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" className="text-text-sec group-hover:text-accent transition-colors">
                  <rect x="3" y="3" width="18" height="18" rx="2" ry="2"></rect>
                  <circle cx="8.5" cy="8.5" r="1.5"></circle>
                  <polyline points="21 15 16 10 5 21"></polyline>
                </svg>
              </div>
              <div>
                <p className="text-[13px] font-semibold text-text-main group-hover:text-white transition-colors">
                  {t("sources_page.custom.drop_image")}
                </p>
                <p className="text-[11px] text-text-muted mt-1">
                  {t("sources_page.custom.aspect_ratio_hint")}
                </p>
              </div>
              <div className="absolute bottom-3 text-[10px] text-accent font-medium opacity-0 group-hover:opacity-100 transition-opacity flex items-center gap-1">
                <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                  <path d="M21 12a9 9 0 0 0-9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"></path>
                  <path d="M3 3v5h5"></path>
                  <path d="M3 12a9 9 0 0 0 9 9 9.75 9.75 0 0 0 6.74-2.74L21 16"></path>
                  <path d="M16 21v-5h5"></path>
                </svg>
                {t("sources_page.custom.fetch_from_sgdb")}
              </div>
            </div>
          </div>
        </div>

        {/* Form Actions */}
        <div className="flex items-center gap-3 mt-8 pt-6 border-t border-white/[0.06]">
          <button className="px-5 py-2.5 flex items-center gap-2 text-[14px] font-bold text-white bg-accent hover:bg-accent-hover rounded-xl shadow-glow transition-all active:scale-95">
            {t("sources_page.custom.add_to_library")}
          </button>
          <button className="px-5 py-2.5 text-[14px] font-semibold text-text-sec hover:text-white bg-transparent hover:bg-white/5 rounded-xl transition-all">
            {t("sources_page.custom.cancel")}
          </button>
        </div>
      </div>
    </div>
  );
}

// ---- Page -------------------------------------------------------------------

const ROWS_ORDER = ["steam", "epic", "gog", "ubi", "ea", "itch"] as const;

const ROW_PATHS: Record<string, string> = {
  steam: "C:\\Program Files (x86)\\Steam",
  epic: "C:\\Program Files\\Epic Games",
  gog: "C:\\Program Files (x86)\\GOG Galaxy",
  ubi: "C:\\Program Files (x86)\\Ubisoft\\Ubisoft Game Launcher",
  ea: "C:\\Program Files\\Electronic Arts\\EA Desktop",
  itch: "%LOCALAPPDATA%\\itch\\app-*",
};

const ROW_LABELS: Record<string, string> = {
  steam: "Steam",
  epic: "Epic Games",
  gog: "GOG Galaxy",
  ubi: "Ubisoft Connect",
  ea: "EA App",
  itch: "Itch.io",
};

export function Sources() {
  const { t } = useTranslation();
  const toast = useToast();
  const { games, refresh: refreshGames } = useGames();
  const { states, handlersFor, refresh: refreshTiles } = useSourceTiles(refreshGames);
  const [rescanning, setRescanning] = useState(false);

  const totalCount = games.length;
  const activeCount = states.filter((s) => !s.supports_login || s.connected).length;

  const customGames = useMemo(
    () =>
      games
        .filter((g) => g.source === "custom")
        .map((g) => ({ id: g.id, title: g.title, exe_path: g.exe_path })),
    [games]
  );

  const runFullScan = useCallback(async () => {
    setRescanning(true);
    try {
      const res = await fullScan();
      toast.success(t("sources_page.scan_complete", { count: res.total_found }));
      await refreshGames();
      await refreshTiles();
      // Force a full artwork re-fetch against the current prefs (toggle
      // `prefer_animated` ON/OFF, NSFW, style…). Bypasses `new_game_ids`
      // because the explicit user click means "apply my current prefs to
      // the whole library now" — useful after flipping a SteamGridDB
      // toggle whose first backfill ran with stale ranker code.
      const { triggerArtworkBackfill } = await import("../lib/artworkBackfill");
      void triggerArtworkBackfill(toast, "sources:scan_all");
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setRescanning(false);
    }
  }, [refreshGames, refreshTiles, toast, t]);

  return (
    <div className="h-screen w-screen overflow-hidden flex flex-col relative">
      <div className="ambient-vignette" />
      <TopBar />

      <main className="flex-1 overflow-y-auto relative p-6 lg:p-12">
        <div className="max-w-[1100px] w-full mx-auto pb-20">
          {/* Page Header */}
          <div
            className="flex flex-col md:flex-row md:items-end justify-between gap-6 mb-8 card-enter"
            style={{ animationDelay: "0.05s" }}
          >
            <div>
              <h1 className="text-4xl md:text-[44px] font-bold tracking-tight leading-none text-white bg-gradient-to-br from-white to-white/60 bg-clip-text text-transparent pb-1">
                {t("sources_page.title")}
              </h1>
              <p className="text-sm font-medium text-text-sec mt-3 max-w-xl">
                {t(
                  activeCount === 1 ? "sources_page.subtitle" : "sources_page.subtitle_plural",
                  { total: totalCount, active: activeCount }
                )}
              </p>
            </div>

            <div className="flex items-center gap-3 shrink-0">
              <button className="px-3 py-2 flex items-center gap-2 text-[13px] font-semibold text-text-main hover:text-white bg-white/[0.04] border border-white/[0.06] hover:bg-white/[0.08] rounded-xl transition-colors">
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M5 12h14"></path>
                  <path d="M12 5v14"></path>
                </svg>
                {t("sources_page.add_custom")}
              </button>
              <button
                onClick={runFullScan}
                disabled={rescanning}
                className="px-4 py-2 flex items-center gap-2 text-[13px] font-bold text-white bg-accent hover:bg-accent-hover rounded-xl shadow-glow transition-all active:scale-95 disabled:opacity-60 disabled:cursor-not-allowed"
              >
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M21 12a9 9 0 0 0-9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"></path>
                  <path d="M3 3v5h5"></path>
                  <path d="M3 12a9 9 0 0 0 9 9 9.75 9.75 0 0 0 6.74-2.74L21 16"></path>
                  <path d="M16 21v-5h5"></path>
                </svg>
                {rescanning ? t("sources_page.scanning") : t("sources_page.scan_all")}
              </button>
            </div>
          </div>

          {/* Active Scan Banner */}
          {rescanning && (
            <div
              className="acrylic rounded-2xl border border-white/10 mb-8 overflow-hidden relative shadow-ambient flex justify-between items-center p-4 pl-5 card-enter"
              style={{ animationDelay: "0.1s" }}
            >
              <div className="flex items-center gap-3">
                <div className="w-5 h-5 rounded-full flex items-center justify-center bg-accent/20 text-accent shrink-0">
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" className="spinner">
                    <line x1="12" y1="2" x2="12" y2="6"></line>
                    <line x1="12" y1="18" x2="12" y2="22"></line>
                    <line x1="4.93" y1="4.93" x2="7.76" y2="7.76"></line>
                    <line x1="16.24" y1="16.24" x2="19.07" y2="19.07"></line>
                    <line x1="2" y1="12" x2="6" y2="12"></line>
                    <line x1="18" y1="12" x2="22" y2="12"></line>
                    <line x1="4.93" y1="19.07" x2="7.76" y2="16.24"></line>
                    <line x1="16.24" y1="7.76" x2="19.07" y2="4.93"></line>
                  </svg>
                </div>
                <span className="text-sm font-medium text-white/90">
                  Scanning all sources
                  <span className="text-text-sec ml-2 border-l border-white/20 pl-2">
                    parsing local launchers + connected libraries
                  </span>
                </span>
              </div>
              <div className="absolute bottom-0 left-0 right-0 h-[2px] bg-white/5">
                <div className="h-full bg-accent progress-indeterminate rounded-r-full"></div>
              </div>
            </div>
          )}

          {/* Sources Stack */}
          <div className="flex flex-col gap-3 card-enter" style={{ animationDelay: "0.15s" }}>
            {ROWS_ORDER.map((src) => {
              const h = handlersFor(src);
              if (!h) return null;
              return (
                <SourceRow
                  key={src}
                  source={src}
                  label={ROW_LABELS[src]}
                  pathLine={ROW_PATHS[src]}
                  handlers={h}
                  scanning={rescanning}
                />
              );
            })}

            <CustomCard handlers={handlersFor("custom")} customGames={customGames} />
          </div>

          {/* Footer */}
          <div
            className="text-center mt-16 pb-12 opacity-80 card-enter"
            style={{ animationDelay: "0.2s" }}
          >
            <p className="text-[13px] font-medium text-text-muted">
              {t("sources_page.footer.unsupported_question")}
              <a
                href="#"
                onClick={(e) => e.preventDefault()}
                className="text-accent hover:text-accent-hover transition-colors ml-1"
              >
                {t("sources_page.footer.request_github")}
              </a>
            </p>
          </div>
        </div>
      </main>
    </div>
  );
}
