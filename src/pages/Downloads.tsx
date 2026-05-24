// Functional shell for the Downloads page.
//
// Lists every active / queued / paused / failed download with per-row
// pause / resume / cancel / retry controls. The AIDesigner agent is
// designing a richer layout in parallel — this shell uses the canonical
// Tokoru DA tokens (acrylic surfaces, coral accent, mono meta lines)
// so the next pass is a style pass, not a rewrite.

import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { TopBar } from "../components/TopBar";
import { useDownloads } from "../lib/useDownloads";
import { useGames } from "../lib/useGames";
import { useToast } from "../components/Toast";
import type { DownloadState } from "../lib/types";
import { SOURCE_DOT, SOURCE_LONG } from "../lib/types";

function formatBytes(n: number): string {
  if (!n) return "—";
  const mb = n / (1024 * 1024);
  if (mb >= 1024) return `${(mb / 1024).toFixed(2)} GB`;
  return `${mb.toFixed(0)} MB`;
}

function formatSpeed(bps: number): string {
  if (bps <= 0) return "—";
  const mbs = bps / (1024 * 1024);
  if (mbs >= 1) return `${mbs.toFixed(1)} MB/s`;
  return `${(bps / 1024).toFixed(0)} KB/s`;
}

function formatEta(secs: number): string {
  if (secs <= 0) return "—";
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

const STATUS_ORDER: Record<DownloadState["status"], number> = {
  downloading: 0,
  paused: 1,
  queued: 2,
  failed: 3,
  completed: 4,
};

export function Downloads() {
  const { t } = useTranslation();
  const { downloads, loading, pause, resume, cancel, retry, dismiss } = useDownloads();
  const { games } = useGames();
  const toast = useToast();

  const titleByGameId = useMemo(() => {
    const map: Record<string, string> = {};
    for (const g of games) {
      if (g.id) map[g.id] = g.title;
    }
    return map;
  }, [games]);

  const rows = useMemo(() => {
    return Object.values(downloads).sort((a, b) => {
      const ord = STATUS_ORDER[a.status] - STATUS_ORDER[b.status];
      if (ord !== 0) return ord;
      return (titleByGameId[a.game_id] ?? a.game_name).localeCompare(
        titleByGameId[b.game_id] ?? b.game_name
      );
    });
  }, [downloads, titleByGameId]);

  const wrap = async (fn: () => Promise<void>, errMsg: string) => {
    try {
      await fn();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : `${errMsg}: ${String(e)}`);
    }
  };

  return (
    <div className="h-screen w-screen flex flex-col relative overflow-hidden">
      <div className="ambient-vignette" />
      <TopBar />

      <main className="flex-1 overflow-y-auto relative z-10">
        <div className="max-w-[1100px] mx-auto px-10 pt-12 pb-32">
          <div className="mb-8 fade-in-up">
            <h1 className="text-[32px] md:text-[40px] font-semibold tracking-tight leading-none text-white">
              {t("downloads_page.title")}
            </h1>
            <p className="text-sm font-medium text-text-sec mt-3">
              {loading
                ? t("common.loading")
                : rows.length === 0
                  ? t("downloads_page.empty")
                  : t("downloads_page.tracked", { count: rows.length })}
            </p>
          </div>

          {rows.length === 0 ? (
            <div className="acrylic rounded-2xl px-8 py-16 flex flex-col items-center text-center">
              <div className="w-14 h-14 rounded-2xl bg-white/[0.04] border border-white/[0.08] flex items-center justify-center text-text-sec mb-5">
                <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                  <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                  <polyline points="7 10 12 15 17 10" />
                  <line x1="12" y1="15" x2="12" y2="3" />
                </svg>
              </div>
              <h2 className="text-lg font-semibold text-white">{t("downloads_page.nothing_title")}</h2>
              <p className="text-[13px] text-text-sec mt-2 max-w-md leading-relaxed">
                {t("downloads_page.nothing_desc")}
              </p>
            </div>
          ) : (
            <ul className="space-y-3">
              {rows.map((d) => {
                const title = titleByGameId[d.game_id] ?? d.game_name ?? d.game_id;
                const dot = SOURCE_DOT[d.source] ?? "#A1A1A6";
                const pct = Math.max(0, Math.min(100, d.progress_pct ?? 0));
                // Steam downloads are read-only mirrors — Steam owns
                // pause/cancel/resume, so we hide our action buttons and
                // show a clarifying subtitle.
                const isSteam = d.source === "steam";
                return (
                  <li
                    key={d.game_id}
                    className="acrylic rounded-xl px-5 py-4 flex items-center gap-5"
                  >
                    {/* Source badge */}
                    <div className="flex items-center gap-2 min-w-[120px]">
                      <span
                        className="w-2 h-2 rounded-full"
                        style={{ background: dot }}
                      />
                      <span className="text-xs font-medium text-text-sec uppercase tracking-wide">
                        {SOURCE_LONG[d.source] ?? d.source}
                      </span>
                    </div>

                    {/* Title + meta */}
                    <div className="flex-1 min-w-0">
                      <div className="text-[14px] font-semibold text-white truncate">
                        {title}
                      </div>
                      <div className="mt-1 flex items-center gap-3 text-[11px] text-text-muted font-mono tracking-tight">
                        <StatusChip status={d.status} />
                        <span>{formatBytes(d.downloaded_bytes)} / {formatBytes(d.total_bytes)}</span>
                        {d.status === "downloading" && (
                          <>
                            <span>· {formatSpeed(d.speed_bps)}</span>
                            <span>· ETA {formatEta(d.eta_secs)}</span>
                          </>
                        )}
                        {d.status === "failed" && d.last_error && (
                          <span className="text-error/80">· {d.last_error}</span>
                        )}
                        {isSteam && (
                          <span className="text-text-muted/70 normal-case">
                            · Managed by Steam
                          </span>
                        )}
                      </div>
                      {/* Progress bar */}
                      <div className="mt-2 w-full h-1 rounded-full bg-white/[0.06] overflow-hidden">
                        <div
                          className={`h-full rounded-full transition-all duration-300 ${
                            d.status === "downloading"
                              ? "bg-accent"
                              : d.status === "paused"
                                ? "bg-white/40"
                                : d.status === "failed"
                                  ? "bg-error/60"
                                  : "bg-white/20"
                          }`}
                          style={{ width: `${pct}%` }}
                        />
                      </div>
                    </div>

                    {/* Actions — hidden for Steam-source rows because Steam
                        owns pause/cancel/resume. We still allow dismissing
                        the row from the list (no disk impact, just clears
                        the UI). Steam rows can be dismissed at any state
                        since Tokoru can't influence them anyway. */}
                    <div className="flex items-center gap-1.5">
                      {isSteam &&
                        d.status !== "completed" &&
                        d.status !== "failed" && (
                          <span className="text-[10px] text-text-muted/60 italic px-2">
                            read-only
                          </span>
                        )}
                      {(isSteam ||
                        d.status === "completed" ||
                        d.status === "failed") && (
                        <ActionBtn
                          onClick={() =>
                            wrap(() => dismiss(d.game_id), "Dismiss failed")
                          }
                          label="Clear from list"
                        >
                          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
                            <line x1="18" y1="6" x2="6" y2="18" />
                            <line x1="6" y1="6" x2="18" y2="18" />
                          </svg>
                        </ActionBtn>
                      )}
                      {!isSteam && d.status === "downloading" && (
                        <ActionBtn
                          onClick={() =>
                            wrap(() => pause(d.game_id), "Pause failed")
                          }
                          label="Pause"
                        >
                          <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor">
                            <rect x="6" y="4" width="4" height="16" />
                            <rect x="14" y="4" width="4" height="16" />
                          </svg>
                        </ActionBtn>
                      )}
                      {!isSteam && (d.status === "paused" || d.status === "queued") && (
                        <ActionBtn
                          onClick={() =>
                            wrap(() => resume(d.game_id), "Resume failed")
                          }
                          label="Resume"
                        >
                          <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor">
                            <polygon points="5 3 19 12 5 21 5 3" />
                          </svg>
                        </ActionBtn>
                      )}
                      {!isSteam && d.status === "failed" && (
                        <ActionBtn
                          onClick={() =>
                            wrap(() => retry(d.game_id), "Retry failed")
                          }
                          label="Retry"
                          tone="error"
                        >
                          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
                            <path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" />
                            <path d="M3 3v5h5" />
                          </svg>
                        </ActionBtn>
                      )}
                      {!isSteam && d.status !== "completed" && (
                        <ActionBtn
                          onClick={() =>
                            wrap(() => cancel(d.game_id), "Cancel failed")
                          }
                          label="Cancel"
                        >
                          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
                            <line x1="18" y1="6" x2="6" y2="18" />
                            <line x1="6" y1="6" x2="18" y2="18" />
                          </svg>
                        </ActionBtn>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </main>
    </div>
  );
}

function StatusChip({ status }: { status: DownloadState["status"] }) {
  const map: Record<DownloadState["status"], { label: string; cls: string }> = {
    queued: {
      label: "QUEUED",
      cls: "text-text-sec border-white/10",
    },
    downloading: {
      label: "DOWNLOADING",
      cls: "text-accent border-accent/30",
    },
    paused: {
      label: "PAUSED",
      cls: "text-white/60 border-white/10",
    },
    failed: {
      label: "FAILED",
      cls: "text-error border-error/30",
    },
    completed: {
      label: "COMPLETED",
      cls: "text-emerald-400 border-emerald-500/20",
    },
  };
  const m = map[status];
  return (
    <span
      className={`inline-flex items-center px-1.5 py-[1px] rounded border ${m.cls}`}
    >
      {m.label}
    </span>
  );
}

function ActionBtn({
  onClick,
  label,
  children,
  tone,
}: {
  onClick: () => void;
  label: string;
  children: React.ReactNode;
  tone?: "default" | "error";
}) {
  const base =
    tone === "error"
      ? "bg-[#FF3B30]/15 border-[#FF3B30]/20 text-[#FF3B30] hover:bg-[#FF3B30]/25"
      : "bg-white/[0.04] border-white/[0.08] text-white/85 hover:bg-white/[0.08]";
  return (
    <button
      onClick={onClick}
      title={label}
      aria-label={label}
      className={`h-8 w-8 rounded-full border flex items-center justify-center transition-colors shadow-sm ${base}`}
    >
      {children}
    </button>
  );
}
