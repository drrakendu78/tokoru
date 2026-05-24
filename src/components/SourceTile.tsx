// Canonical source tile UI.
//
// Two layers:
//   - SourceTile (compact): a small square tile (~140-160px) with the source
//     logo, the source name, and a tiny status dot in the top-right corner.
//     No inline buttons, no game count, no last-sync timestamp — just the
//     pretty grid look. Click toggles the expanded state via the parent.
//   - SourceTileExpanded: the wider "panel" version that shows account info,
//     last sync timestamp, and the action buttons (Connect / Sync /
//     Disconnect / Rescan / Browse). Rendered by the parent grid as an
//     absolute-positioned popover anchored to the clicked compact tile so
//     other tiles don't reflow.
//
// The components are dumb: they own no remote state. All sync/disconnect/
// rescan behaviour is passed in by the host page so the Sources page and
// the Settings → Sources section can share the same UI but route the
// actions to slightly different busy spinners / toasts.

import {
  forwardRef,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import { useTranslation } from "react-i18next";
import { CoralButton } from "./CoralButton";
import { BrandLogo } from "./BrandLogo";
import { SOURCE_LONG, type SourceState } from "../lib/types";

// Background color of the circular brand disc (matches AIDesigner mockup).
const DISC_BG: Record<string, string> = {
  steam: "#171a21",
  epic: "#ffffff",
  gog: "#C173FF",
  ubi: "#0088F0",
  ea: "#ffffff",
  itch: "#FA5C5C",
  custom: "#A1A1A6",
};

// Foreground color of the brand logo silhouette on top of the disc.
const DISC_FG: Record<string, string> = {
  steam: "#ffffff",
  epic: "#000000",
  gog: "#ffffff",
  ubi: "#ffffff",
  ea: "#000000",
  itch: "#ffffff",
  custom: "#ffffff",
};

const SHORT_LABEL: Record<string, string> = {
  steam: "Steam",
  epic: "Epic Games",
  gog: "GOG Galaxy",
  ubi: "Ubisoft",
  ea: "EA App",
  itch: "Itch.io",
  custom: "Custom",
};

function formatRelative(unix: number | null): string {
  if (!unix) return "Never synced";
  const diff = Date.now() / 1000 - unix;
  if (diff < 0) return "Just now";
  if (diff < 60) return "Just now";
  if (diff < 3600) {
    const m = Math.floor(diff / 60);
    return `${m} minute${m === 1 ? "" : "s"} ago`;
  }
  if (diff < 86400) {
    const h = Math.floor(diff / 3600);
    return `${h} hour${h === 1 ? "" : "s"} ago`;
  }
  const d = Math.floor(diff / 86400);
  return `${d} day${d === 1 ? "" : "s"} ago`;
}

// ----- Compact tile ------------------------------------------------------

export interface SourceTileProps {
  state: SourceState;
  /// Whether some long-running action (login / sync / rescan) is in flight
  /// for this tile.
  busy?: boolean;
  /// Whether the expanded panel is currently anchored to this tile.
  expanded?: boolean;
  /// Click handler from the parent grid. Parent owns the expanded state so
  /// only one tile can be open at a time and click-outside is a single
  /// listener.
  onClick?: () => void;
  /// Compact = even smaller tile, used in Settings → Sources grid.
  compact?: boolean;
}

export const SourceTile = forwardRef<HTMLButtonElement, SourceTileProps>(
  function SourceTile(
    { state, busy = false, expanded = false, onClick, compact = false },
    ref
  ) {
    const { t } = useTranslation();
    const label = SHORT_LABEL[state.source] ?? SOURCE_LONG[state.source] ?? state.source;
    const discBg = DISC_BG[state.source] ?? DISC_BG.custom;
    const discFg = DISC_FG[state.source] ?? DISC_FG.custom;

    // Status dot color (verbatim from the mockup):
    //   - active source (connected OR local-ready) → emerald with glow
    //   - busy → coral
    //   - OAuth not connected → muted grey
    const dotClass = busy
      ? "bg-accent shadow-[0_0_6px_rgba(255,70,51,0.5)]"
      : state.connected || !state.supports_login
      ? "bg-emerald-500 shadow-[0_0_6px_rgba(16,185,129,0.5)]"
      : "bg-text-muted";

    const tileClass =
      "relative bg-white/[0.03] border border-white/[0.06] rounded-xl p-3 flex flex-col items-center justify-center gap-2 hover:bg-white/[0.05] transition-colors shadow-inset-soft cursor-pointer";

    // When the popover is open against this tile, highlight the border.
    const expandedExtra = expanded
      ? " ring-2 ring-accent/40 ring-offset-1 ring-offset-shell"
      : "";

    const customDiscExtra =
      state.source === "custom" ? " border border-white/20" : "";

    const _ignoredCompact = compact; // both states render the same disc layout

    return (
      <button
        ref={ref}
        type="button"
        onClick={onClick}
        aria-expanded={expanded}
        aria-label={label}
        className={`${tileClass}${expandedExtra}`}
        style={{
          transition:
            "background-color 180ms cubic-bezier(0.2,0.8,0.2,1), border-color 180ms cubic-bezier(0.2,0.8,0.2,1)",
        }}
      >
        {/* Status dot in the top-right corner */}
        <span
          className={`absolute top-2 right-2 w-2 h-2 rounded-full ${dotClass}`}
          aria-label={state.connected ? t("source_tile.connected") : t("source_tile.not_connected")}
        />

        {/* Circular brand disc */}
        <div
          className={`w-8 h-8 rounded-full flex items-center justify-center${customDiscExtra}`}
          style={{ background: discBg }}
        >
          <BrandLogo source={state.source} size={18} color={discFg} />
        </div>

        {/* Centered source name */}
        <span className="text-[11px] font-medium text-white">
          {label}
        </span>
        {/* hidden marker just so the unused variable warning doesn't kick in if
            compact is ever inspected externally */}
        <span className="hidden" data-compact={_ignoredCompact ? "1" : "0"} />
      </button>
    );
  }
);

// ----- Expanded panel ----------------------------------------------------

export interface SourceTileExpandedHandlers {
  onConnect?: () => void;
  onSync?: () => void;
  onDisconnect?: () => void;
  onRescan?: () => void;
  onBrowse?: () => void;
}

export interface SourceTileExpandedProps extends SourceTileExpandedHandlers {
  state: SourceState;
  busy?: boolean;
  /// Inline style applied to the popover root. Used by the parent grid to
  /// position the popover absolutely above the clicked tile.
  style?: CSSProperties;
  /// Class merged into the popover root. The parent uses this to control
  /// width/positioning side.
  className?: string;
  /// Close trigger fired by the X button.
  onClose: () => void;
}

export const SourceTileExpanded = forwardRef<
  HTMLDivElement,
  SourceTileExpandedProps
>(function SourceTileExpanded(
  {
    state,
    busy = false,
    onConnect,
    onSync,
    onDisconnect,
    onRescan,
    onBrowse,
    onClose,
    style,
    className,
  },
  ref
) {
  const { t } = useTranslation();
  const label = SHORT_LABEL[state.source] ?? SOURCE_LONG[state.source] ?? state.source;
  const discBg = DISC_BG[state.source] ?? DISC_BG.custom;
  const discFg = DISC_FG[state.source] ?? DISC_FG.custom;

  const accountLine = state.account_name
    ? t("source_tile.signed_in_as", { name: state.account_name })
    : state.source === "steam" && state.steam_id
    ? t("source_tile.signed_in_as_steam", { id: state.steam_id })
    : null;

  // Mount-in animation: scale + opacity, 200ms, app standard easing.
  const [mounted, setMounted] = useState(false);
  useEffect(() => {
    // Next-frame flip so the transition runs.
    const id = requestAnimationFrame(() => setMounted(true));
    return () => cancelAnimationFrame(id);
  }, []);

  return (
    <div
      ref={ref}
      style={{
        ...style,
        transition:
          "opacity 200ms cubic-bezier(0.2,0.8,0.2,1), transform 200ms cubic-bezier(0.2,0.8,0.2,1)",
        opacity: mounted ? 1 : 0,
        transform: mounted
          ? "translateY(0) scale(1)"
          : "translateY(-4px) scale(0.97)",
        transformOrigin: "top center",
      }}
      className={`rounded-2xl border border-white/[0.08] bg-shell/95 backdrop-blur-md shadow-premium p-4 flex flex-col gap-3 ${
        className ?? ""
      }`}
      role="dialog"
      aria-label={t("source_tile.controls", { label })}
    >
      {/* Header: icon + name + close */}
      <div className="flex items-start gap-3">
        <div
          className="shrink-0 w-12 h-12 rounded-full flex items-center justify-center"
          style={{ background: discBg }}
        >
          <BrandLogo source={state.source} size={26} color={discFg} />
        </div>
        <div className="flex-1 min-w-0">
          <h3 className="text-[15px] font-bold text-white tracking-tight leading-tight">
            {label}
          </h3>
          <div className="text-[12px] text-text-muted mt-0.5">
            {t(
              state.game_count === 1 ? "source_tile.game_count" : "source_tile.game_count_plural",
              { count: state.game_count }
            )}
            {state.connected && state.supports_login ? (
              <>
                <span className="mx-1.5 opacity-50">·</span>
                {formatRelative(state.last_sync_at)}
              </>
            ) : !state.supports_login ? (
              <>
                <span className="mx-1.5 opacity-50">·</span>
                {t("source_tile.local_scan")}
              </>
            ) : (
              <>
                <span className="mx-1.5 opacity-50">·</span>
                <span className="text-yellow-300/90">{t("source_tile.not_connected")}</span>
              </>
            )}
          </div>
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label={t("source_tile.close")}
          className="shrink-0 w-7 h-7 rounded-lg text-text-muted hover:text-white hover:bg-white/[0.06] flex items-center justify-center transition-colors"
        >
          <svg
            width="14"
            height="14"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2.5"
            strokeLinecap="round"
          >
            <line x1="18" y1="6" x2="6" y2="18" />
            <line x1="6" y1="6" x2="18" y2="18" />
          </svg>
        </button>
      </div>

      {/* Status / account line */}
      {state.supports_login ? (
        state.connected ? (
          <>
            {accountLine && (
              <div className="text-[12px] text-text-sec">{accountLine}</div>
            )}
            <div className="text-[11px] text-text-muted">
              {t("source_tile.last_sync", { date: formatRelative(state.last_sync_at) })}
            </div>
            <div className="flex items-center gap-2 flex-wrap pt-1">
              {onSync && (
                <CoralButton
                  variant="secondary"
                  size="sm"
                  onClick={onSync}
                  disabled={busy}
                >
                  {busy ? t("source_tile.syncing") : t("source_tile.sync_library")}
                </CoralButton>
              )}
              {onDisconnect && (
                <CoralButton
                  variant="ghost"
                  size="sm"
                  onClick={onDisconnect}
                  disabled={busy}
                >
                  {t("source_tile.disconnect")}
                </CoralButton>
              )}
            </div>
          </>
        ) : (
          <>
            <div className="text-[12px] text-text-sec">
              {t("source_tile.not_connected_yet", { label })}
            </div>
            <div className="flex items-center gap-2 flex-wrap pt-1">
              {onConnect && (
                <CoralButton
                  variant="primary"
                  size="sm"
                  onClick={onConnect}
                  disabled={busy}
                >
                  {busy ? t("source_tile.connecting") : t("source_tile.connect")}
                </CoralButton>
              )}
            </div>
          </>
        )
      ) : (
        <>
          <div className="text-[11px] text-text-muted">
            {state.last_sync_at
              ? t("source_tile.last_scan", { date: formatRelative(state.last_sync_at) })
              : t("source_tile.no_scan_yet")}
          </div>
          <div className="flex items-center gap-2 flex-wrap pt-1">
            {onRescan && (
              <CoralButton
                variant="secondary"
                size="sm"
                onClick={onRescan}
                disabled={busy}
              >
                {busy ? t("source_tile.scanning") : t("source_tile.rescan_now")}
              </CoralButton>
            )}
            {state.source === "custom" && onBrowse && (
              <CoralButton
                variant="ghost"
                size="sm"
                onClick={onBrowse}
                disabled={busy}
              >
                Browse for new .exe
              </CoralButton>
            )}
          </div>
        </>
      )}
    </div>
  );
});

// ----- Grid wrapper ------------------------------------------------------
//
// Single source of truth for the click-outside / Esc / single-expanded
// behaviour. Both Sources.tsx and Settings → Sources render this with the
// same shape of handler bundle (the `tiles` prop matches the output of
// `useSourceTiles().handlersFor(src)`).

export interface SourceTileGridEntry {
  source: string;
  state: SourceState;
  busy: boolean;
  onConnect?: () => void;
  onSync?: () => void;
  onDisconnect?: () => void;
  onRescan?: () => void;
  onBrowse?: () => void;
}

export interface SourceTileGridProps {
  tiles: SourceTileGridEntry[];
  /// Tailwind grid template class; e.g. `grid-cols-2 md:grid-cols-4`.
  gridClassName?: string;
  /// Compact = used in Settings → Sources.
  compact?: boolean;
}

export function SourceTileGrid({
  tiles,
  gridClassName = "grid-cols-2 md:grid-cols-3 lg:grid-cols-4",
  compact = false,
}: SourceTileGridProps) {
  const [expandedKey, setExpandedKey] = useState<string | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const tileRefs = useRef<Map<string, HTMLButtonElement>>(new Map());
  const popoverRef = useRef<HTMLDivElement | null>(null);

  // Popover absolute position (relative to the grid container).
  const [popoverStyle, setPopoverStyle] = useState<CSSProperties | null>(null);

  // Recompute popover position whenever the expanded tile changes (and on
  // window resize while one is open). The popover is anchored to the
  // bottom-left of the tile, clamped to the container's right edge so it
  // never overflows when expanding from the rightmost column.
  useEffect(() => {
    if (!expandedKey) {
      setPopoverStyle(null);
      return;
    }
    const compute = () => {
      const container = containerRef.current;
      const anchor = tileRefs.current.get(expandedKey);
      if (!container || !anchor) return;
      const cRect = container.getBoundingClientRect();
      const aRect = anchor.getBoundingClientRect();
      const popWidth = Math.min(320, cRect.width - 8);
      const gap = 8;
      // Prefer aligning the popover's left edge with the tile, but clamp so
      // the right edge stays inside the container.
      let left = aRect.left - cRect.left;
      if (left + popWidth > cRect.width - 4) {
        left = Math.max(4, cRect.width - popWidth - 4);
      }
      const top = aRect.bottom - cRect.top + gap;
      setPopoverStyle({
        position: "absolute",
        top,
        left,
        width: popWidth,
        zIndex: 30,
      });
    };
    compute();
    window.addEventListener("resize", compute);
    return () => window.removeEventListener("resize", compute);
  }, [expandedKey, tiles.length]);

  // Click-outside: collapse if the click landed outside the popover AND
  // outside the anchor tile (clicks on tiles are handled by their own
  // onClick which toggles expandedKey).
  useEffect(() => {
    if (!expandedKey) return;
    const handler = (e: MouseEvent) => {
      const target = e.target as Node | null;
      if (!target) return;
      if (popoverRef.current && popoverRef.current.contains(target)) return;
      // If the click is on any tile, the tile's onClick will handle the
      // toggle — don't double-fire here.
      for (const node of tileRefs.current.values()) {
        if (node.contains(target)) return;
      }
      setExpandedKey(null);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [expandedKey]);

  // Esc collapses.
  useEffect(() => {
    if (!expandedKey) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") setExpandedKey(null);
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [expandedKey]);

  // If the currently-expanded source disappears from the tiles list
  // (sources changed externally), collapse the popover so we don't render a
  // stale anchor.
  useEffect(() => {
    if (!expandedKey) return;
    const entry = tiles.find((t) => t.source === expandedKey);
    if (!entry) setExpandedKey(null);
  }, [tiles, expandedKey]);

  const expanded = expandedKey
    ? tiles.find((t) => t.source === expandedKey) ?? null
    : null;

  return (
    <div ref={containerRef} className="relative">
      <div className={`grid gap-2.5 ${gridClassName}`}>
        {tiles.map((t) => (
          <SourceTile
            key={t.source}
            ref={(node) => {
              if (node) tileRefs.current.set(t.source, node);
              else tileRefs.current.delete(t.source);
            }}
            state={t.state}
            busy={t.busy}
            compact={compact}
            expanded={expandedKey === t.source}
            onClick={() => {
              setExpandedKey((prev) => (prev === t.source ? null : t.source));
            }}
          />
        ))}
      </div>

      {expanded && popoverStyle && (
        <SourceTileExpanded
          ref={popoverRef}
          state={expanded.state}
          busy={expanded.busy}
          onConnect={expanded.onConnect}
          onSync={expanded.onSync}
          onDisconnect={expanded.onDisconnect}
          onRescan={expanded.onRescan}
          onBrowse={expanded.onBrowse}
          onClose={() => setExpandedKey(null)}
          style={popoverStyle}
        />
      )}
    </div>
  );
}
