import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { TopBar } from "../components/TopBar";
import {
  getGlobalAchievementsStats,
  getGlobalStats,
  getPlaytimeHeatmap,
  getSessionsOverTime,
  getTopPlayed,
  getUntouchedGames,
} from "../lib/api";
import type { GlobalAchievementsStats } from "../lib/api";
import type {
  GlobalStats,
  HeatmapBucket,
  SessionsDayBucket,
  TopPlayedRow,
  UntouchedGame,
} from "../lib/types";
import { useToast } from "../components/Toast";

type Range = "7d" | "30d" | "90d" | "all";

function formatHours(seconds: number): { value: string; sub: string } {
  if (!seconds || seconds <= 0) return { value: "0h", sub: "0m" };
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  return { value: `${hours}h`, sub: `${minutes}m` };
}

function formatHoursFromHours(hours: number): { value: string; sub: string } {
  if (!hours || hours <= 0) return { value: "0h", sub: "0m" };
  const whole = Math.floor(hours);
  const minutes = Math.round((hours - whole) * 60);
  return { value: `${whole}h`, sub: `${minutes}m` };
}

export function Stats() {
  const { t } = useTranslation();
  const [range, setRange] = useState<Range>("30d");
  const [globalStats, setGlobalStats] = useState<GlobalStats | null>(null);
  const [heatmap, setHeatmap] = useState<HeatmapBucket[]>([]);
  const [top, setTop] = useState<TopPlayedRow[]>([]);
  const [sessionsOverTime, setSessionsOverTime] = useState<SessionsDayBucket[]>([]);
  const [sessionsMetric, setSessionsMetric] = useState<"sessions" | "avg">("sessions");
  const [untouched, setUntouched] = useState<UntouchedGame[]>([]);
  const [achStats, setAchStats] = useState<GlobalAchievementsStats | null>(null);
  const toast = useToast();

  // Load global stats + top played once.
  useEffect(() => {
    void getGlobalStats()
      .then(setGlobalStats)
      .catch((e) => {
        console.error("Stats: get_global_stats failed", e);
        toast.error(e instanceof Error ? e.message : String(e));
      });
    void getTopPlayed(10)
      .then(setTop)
      .catch((e) => {
        console.error("Stats: get_top_played failed", e);
      });
    void getUntouchedGames(8)
      .then(setUntouched)
      .catch((e) => {
        console.error("Stats: get_untouched_games failed", e);
      });
    void getGlobalAchievementsStats()
      .then(setAchStats)
      .catch((e) => {
        console.error("Stats: get_global_achievements_stats failed", e);
      });
  }, [toast]);

  // Heatmap re-loads when range changes. The Rust side filters sessions
  // by the days window so a "7d" pick only returns the last 7 daily
  // buckets — the calendar grid then renders just those (older squares
  // stay empty / grayed).
  useEffect(() => {
    const days =
      range === "7d" ? 7 : range === "30d" ? 30 : range === "90d" ? 90 : 365;
    void getPlaytimeHeatmap(days)
      .then(setHeatmap)
      .catch((e) => {
        console.error("Stats: get_playtime_heatmap failed", e);
      });
    void getSessionsOverTime(days)
      .then(setSessionsOverTime)
      .catch((e) => {
        console.error("Stats: get_sessions_over_time failed", e);
      });
  }, [range]);

  // Most-active weekday — computed from the heatmap so it stays in sync
  // with whatever range the user picked. Empty heatmap → null label.
  const mostActiveDay = useMemo(() => {
    if (heatmap.length === 0) return null;
    const byDow: number[] = [0, 0, 0, 0, 0, 0, 0]; // Sun..Sat (UTC)
    for (const b of heatmap) {
      const d = new Date(`${b.date}T00:00:00Z`);
      byDow[d.getUTCDay()] += b.total_seconds;
    }
    let best = 0;
    for (let i = 1; i < 7; i++) {
      if (byDow[i] > byDow[best]) best = i;
    }
    if (byDow[best] === 0) return null;
    return best; // 0=Sun..6=Sat, frontend formats via i18n
  }, [heatmap]);

  const heatmapCells = useMemo(
    () => buildHeatmap(heatmap),
    [heatmap]
  );

  const totalHours = formatHoursFromHours(globalStats?.total_hours ?? 0);
  const monthHours = formatHoursFromHours(globalStats?.hours_this_month ?? 0);
  const longest = formatHours(globalStats?.longest_session_seconds ?? 0);

  const topMax = top.length > 0 ? top[0].total_seconds : 0;

  const rightSlot = (
    <>
      <div className="flex p-0.5 bg-white/[0.03] rounded-md border border-white/[0.02]">
        {(["7d", "30d", "90d", "all"] as Range[]).map((r) => {
          const selected = range === r;
          return (
            <button
              key={r}
              onClick={() => setRange(r)}
              className={`px-3 py-1 rounded-[4px] text-[11px] font-medium transition-colors ${
                selected
                  ? "bg-accent text-white shadow-sm cursor-default"
                  : "text-text-sec hover:text-white hover:bg-white/[0.04]"
              }`}
            >
              {r === "all"
                ? t("stats_page.range_all")
                : r === "7d"
                  ? t("stats_page.range_7d")
                  : r === "30d"
                    ? t("stats_page.range_30d")
                    : t("stats_page.range_90d")}
            </button>
          );
        })}
      </div>
      <div className="w-px h-5 bg-white/10 mx-1" />
    </>
  );

  return (
    <div className="h-screen w-screen flex flex-col relative overflow-hidden">
      <div className="ambient-vignette" />
      <TopBar rightSlot={rightSlot} acrylic />
      <main className="flex-1 overflow-y-auto relative w-full">
        <div className="max-w-[1440px] mx-auto px-10 md:px-20 pt-10 pb-32">
          {/* Title */}
          <div
            className="mb-10 flex items-end justify-between fade-in-up"
            style={{ animationDelay: "0.1s" }}
          >
            <div>
              <h1 className="text-[32px] md:text-[40px] font-semibold tracking-tight leading-none text-white">
                {t("stats_page.title")}
              </h1>
              <p className="text-[13px] font-medium text-text-sec mt-3 max-w-xl leading-relaxed">
                {t("stats_page.subtitle")}
              </p>
            </div>
            <button className="hidden md:flex px-4 py-2 items-center gap-2 text-[12px] font-medium text-text-sec hover:text-white bg-white/[0.03] border border-white/[0.06] hover:bg-white/[0.08] rounded-lg transition-colors shadow-sm">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                <polyline points="7 10 12 15 17 10" />
                <line x1="12" y1="15" x2="12" y2="3" />
              </svg>
              {t("stats_page.export_csv")}
            </button>
          </div>

          {/* Quick stats */}
          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-5 mb-8">
            <StatCard
              title={t("stats_page.total_hours_tracked")}
              value={totalHours.value}
              sub={totalHours.sub}
              note={t("stats_page.this_month", { value: monthHours.value, sub: monthHours.sub })}
              noteColor="accent"
              delay="0.15s"
            />
            <StatCard
              title={t("stats_page.hours_this_month")}
              value={monthHours.value}
              sub={monthHours.sub}
              note={t("stats_page.watch_only_note")}
              delay="0.2s"
            />
            <StatCard
              title={t("stats_page.longest_session")}
              value={longest.value}
              sub={longest.sub}
              note={t("stats_page.across_all_games")}
              delay="0.25s"
            />
            <StatCard
              title={t("stats_page.longest_streak")}
              value={String(globalStats?.longest_streak_days ?? 0)}
              sub={t("stats_page.days")}
              note={t("stats_page.consecutive_days")}
              delay="0.3s"
            />
          </div>

          {/* Heatmap */}
          <div
            className="acrylic rounded-[14px] p-8 mb-8 fade-in-up"
            style={{ animationDelay: "0.35s" }}
          >
            <h2 className="text-[15px] font-semibold text-white mb-6">
              {t("stats_page.activity_calendar")} ·{" "}
              <span className="text-text-sec font-normal">{t("stats_page.last_12_months")}</span>
            </h2>
            <div className="w-full overflow-x-auto pb-4 relative">
              <svg
                width="100%"
                height="auto"
                viewBox="0 -15 880 130"
                className="block min-w-[700px] mx-auto"
              >
                {/* Day labels */}
                <text x={0} y={22} fill="#6C6C70" fontSize={10} fontFamily="Inter, sans-serif" fontWeight={500}>
                  {t("stats_page.mon")}
                </text>
                <text x={0} y={52} fill="#6C6C70" fontSize={10} fontFamily="Inter, sans-serif" fontWeight={500}>
                  {t("stats_page.wed")}
                </text>
                <text x={0} y={82} fill="#6C6C70" fontSize={10} fontFamily="Inter, sans-serif" fontWeight={500}>
                  {t("stats_page.fri")}
                </text>
                {/* Cells */}
                {heatmapCells.map((cell, i) => (
                  <rect
                    key={i}
                    x={cell.x}
                    y={cell.y}
                    width={12}
                    height={12}
                    rx={3}
                    ry={3}
                    fill={cell.color}
                  >
                    <title>
                      {cell.date}
                      {cell.seconds > 0
                        ? ` · ${Math.round(cell.seconds / 60)}m`
                        : ""}
                    </title>
                  </rect>
                ))}
              </svg>
            </div>
            <div className="flex items-center justify-between mt-2 pt-4 border-t border-white/[0.04] w-full max-w-[880px] mx-auto">
              <div className="flex items-center gap-2 text-[11px] font-medium text-text-sec">
                {t("stats_page.less")}
                <div className="flex items-center gap-1">
                  {["#1E1E22", "#4A201C", "#732A20", "#D93A2A", "#FF4633"].map((c) => (
                    <span
                      key={c}
                      className="w-[11px] h-[11px] rounded-[3px]"
                      style={{ background: c }}
                    />
                  ))}
                </div>
                {t("stats_page.more")}
              </div>
              <div className="text-[11px] font-medium text-text-muted flex items-center gap-4">
                {mostActiveDay !== null && (
                  <span>
                    {t("stats_page.most_active_day")}:{" "}
                    <span className="text-text-sec">
                      {t(`stats_page.weekday.${["sun", "mon", "tue", "wed", "thu", "fri", "sat"][mostActiveDay]}`)}
                    </span>
                  </span>
                )}
                <span>
                  {t("stats_page.tracked_days")}:{" "}
                  <span className="text-text-sec">
                    {heatmap.filter((b) => b.total_seconds > 0).length}
                  </span>
                </span>
              </div>
            </div>
          </div>

          {/* Top 10 + Sessions over time — 2-column row on lg+, stacked
              on smaller screens. Mirrors the original Stats mockup
              (run `2026-05-20T22-44-17-988Z`) which had these two blocks
              side-by-side under the calendar. */}
          <div className="grid grid-cols-1 lg:grid-cols-2 gap-6 mb-8">
            {/* Top 10 */}
            <div
              className="acrylic rounded-[14px] p-8 fade-in-up"
              style={{ animationDelay: "0.4s" }}
            >
              <h2 className="text-[15px] font-semibold text-white mb-6">
                {t("stats_page.top_games", { count: top.length || 10 })}
              </h2>
              {top.length === 0 ? (
                <p className="text-[12px] text-text-muted">
                  {t("stats_page.no_tracked")}
                </p>
              ) : (
                <div className="space-y-4">
                  {top.map((g) => {
                    const pct =
                      topMax > 0
                        ? Math.max(4, Math.round((g.total_seconds / topMax) * 100))
                        : 0;
                    const time = formatHours(g.total_seconds);
                    return (
                      <div key={g.game_id} className="flex items-center gap-4 group">
                        <div className="w-36 shrink-0">
                          <p className="text-[13px] font-medium text-white/90 truncate group-hover:text-white transition-colors">
                            {g.title}
                          </p>
                        </div>
                        <div className="flex-1 h-2 bg-white/[0.04] rounded-full overflow-hidden">
                          <div
                            className="h-full rounded-full bg-accent"
                            style={{ width: `${pct}%` }}
                          />
                        </div>
                        <div className="w-20 text-right shrink-0">
                          <span className="text-[12px] font-medium text-text-sec">
                            {time.value} {time.sub}
                          </span>
                        </div>
                      </div>
                    );
                  })}
                </div>
              )}
            </div>

            {/* Sessions over time */}
            <div
              className="acrylic rounded-[14px] p-8 fade-in-up"
              style={{ animationDelay: "0.5s" }}
            >
              <div className="flex items-center justify-between mb-6">
                <h2 className="text-[15px] font-semibold text-white">
                  {t("stats_page.sessions_over_time")}
                </h2>
                <div className="flex items-center gap-1 text-[11px] font-medium">
                  <button
                    onClick={() => setSessionsMetric("sessions")}
                    className={
                      sessionsMetric === "sessions"
                        ? "px-2.5 py-1 rounded-md bg-accent/15 text-accent border border-accent/20"
                        : "px-2.5 py-1 rounded-md text-text-muted hover:text-white"
                    }
                  >
                    {t("stats_page.metric_sessions")}
                  </button>
                  <button
                    onClick={() => setSessionsMetric("avg")}
                    className={
                      sessionsMetric === "avg"
                        ? "px-2.5 py-1 rounded-md bg-accent/15 text-accent border border-accent/20"
                        : "px-2.5 py-1 rounded-md text-text-muted hover:text-white"
                    }
                  >
                    {t("stats_page.metric_avg_length")}
                  </button>
                </div>
              </div>
              <SessionsLineChart data={sessionsOverTime} metric={sessionsMetric} />
            </div>
          </div>

          {/* Global achievements block — cross-library aggregation. Only
              renders when we actually have cached achievement data, to
              avoid showing 0/0 on a fresh install before any GameDetail
              has been opened. */}
          {achStats && achStats.total_available > 0 && (
            <div
              className="acrylic rounded-[14px] p-8 mb-8 fade-in-up"
              style={{ animationDelay: "0.55s" }}
            >
              <div className="flex items-end justify-between mb-2">
                <div className="flex flex-col gap-0.5">
                  <h2 className="text-[15px] font-semibold text-white">
                    {t("stats_page.achievements_title")}
                  </h2>
                  <p className="text-[11px] text-text-muted">
                    {t("stats_page.achievements_subtitle", {
                      unlocked: achStats.total_unlocked,
                      total: achStats.total_available,
                      games: achStats.games_with_achievements,
                    })}
                  </p>
                </div>
                <div
                  className="px-2.5 py-1 rounded-full bg-white/[0.02] border border-white/[0.06] text-accent text-[12px] font-semibold leading-none"
                  style={{ boxShadow: "inset 0 0 12px rgba(255, 70, 51, 0.15)" }}
                >
                  {achStats.total_available > 0
                    ? Math.round((achStats.total_unlocked / achStats.total_available) * 100)
                    : 0}
                  %
                </div>
              </div>
              <div className="w-full mt-4 h-[5px] rounded-full bg-white/[0.06] relative overflow-hidden">
                <div
                  className="absolute top-0 left-0 h-full bg-accent rounded-full"
                  style={{
                    width: `${achStats.total_available > 0 ? Math.round((achStats.total_unlocked / achStats.total_available) * 100) : 0}%`,
                    boxShadow: "0 0 12px rgba(255, 70, 51, 0.45)",
                  }}
                />
              </div>

              {/* Recently unlocked — horizontal icon scroller */}
              {achStats.recent_unlocks.length > 0 && (
                <div className="mt-6">
                  <h3 className="text-[11px] font-bold text-text-muted uppercase tracking-widest mb-3">
                    {t("stats_page.recent_unlocks")}
                  </h3>
                  <div className="flex gap-3 overflow-x-auto pb-2">
                    {achStats.recent_unlocks.map((u) => (
                      <div
                        key={`${u.game_id}-${u.unlocktime}`}
                        className="flex flex-col gap-1.5 shrink-0 w-[88px]"
                        title={`${u.game_title} — ${u.name}`}
                      >
                        <div
                          className="w-12 h-12 rounded-[10px] ring-1 ring-white/[0.08] overflow-hidden bg-white/[0.04]"
                          style={{
                            boxShadow: "0 0 14px rgba(255,70,51,0.18), 0 2px 8px rgba(0,0,0,0.4)",
                          }}
                        >
                          {u.icon ? (
                            <img
                              src={u.icon}
                              alt=""
                              className="w-full h-full object-cover"
                              loading="lazy"
                            />
                          ) : null}
                        </div>
                        <p className="text-[10px] font-semibold text-white/90 truncate leading-tight">
                          {u.name}
                        </p>
                        <p className="text-[9px] text-text-muted truncate leading-tight">
                          {u.game_title}
                        </p>
                      </div>
                    ))}
                  </div>
                </div>
              )}

              {/* Top completion grid */}
              {achStats.top_completion.length > 0 && (
                <div className="mt-6">
                  <h3 className="text-[11px] font-bold text-text-muted uppercase tracking-widest mb-3">
                    {t("stats_page.top_completion")}
                  </h3>
                  <div className="grid grid-cols-2 md:grid-cols-3 gap-3">
                    {achStats.top_completion.map((g) => (
                      <div
                        key={g.game_id}
                        className="flex items-center gap-3 p-2.5 rounded-lg bg-white/[0.02] border border-white/[0.04]"
                      >
                        <div
                          className="w-9 h-12 rounded-md overflow-hidden bg-white/[0.04] shrink-0"
                          style={
                            g.artwork_url
                              ? {
                                  backgroundImage: `url(${g.artwork_url})`,
                                  backgroundSize: "cover",
                                  backgroundPosition: "center",
                                }
                              : undefined
                          }
                        />
                        <div className="flex-1 min-w-0">
                          <p className="text-[12px] font-semibold text-white truncate">
                            {g.game_title}
                          </p>
                          <p className="text-[10px] text-text-muted">
                            {g.unlocked} / {g.total}
                          </p>
                        </div>
                        <span className="text-[11px] font-bold text-accent shrink-0">{g.pct}%</span>
                      </div>
                    ))}
                  </div>
                </div>
              )}
            </div>
          )}

          {/* Haven't been touched */}
          {untouched.length > 0 && (
            <div
              className="acrylic rounded-[14px] p-8 mb-8 fade-in-up"
              style={{ animationDelay: "0.6s" }}
            >
              <h2 className="text-[15px] font-semibold text-white mb-6">
                {t("stats_page.untouched_title")}
              </h2>
              <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-6 gap-4">
                {untouched.map((g) => (
                  <div key={g.game_id} className="flex flex-col gap-2 group">
                    <div
                      className="aspect-[2/3] rounded-lg overflow-hidden bg-white/[0.04] border border-white/5 group-hover:border-white/10 transition-colors"
                      style={
                        g.artwork_url
                          ? {
                              backgroundImage: `url(${g.artwork_url})`,
                              backgroundSize: "cover",
                              backgroundPosition: "center",
                            }
                          : undefined
                      }
                    />
                    <div>
                      <p className="text-[12px] font-semibold text-white/90 truncate">
                        {g.title}
                      </p>
                      <p className="text-[10px] text-text-muted">
                        {g.last_played
                          ? t("stats_page.last_played_relative", {
                              days: Math.floor((Date.now() / 1000 - g.last_played) / 86400),
                            })
                          : t("stats_page.never_played")}
                      </p>
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      </main>
    </div>
  );
}

interface SessionsLineChartProps {
  data: SessionsDayBucket[];
  metric: "sessions" | "avg";
}

function SessionsLineChart({ data, metric }: SessionsLineChartProps) {
  const { t, i18n } = useTranslation();
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [hovered, setHovered] = useState<number | null>(null);

  if (data.length === 0) {
    return (
      <div className="h-40 flex items-center justify-center text-[12px] text-text-muted">
        {t("stats_page.no_session_data")}
      </div>
    );
  }

  const values = data.map((d) => (metric === "sessions" ? d.sessions_count : d.avg_seconds));
  const max = Math.max(1, ...values);
  const width = 880;
  const height = 160;
  const padX = 8;
  const stepX = data.length > 1 ? (width - padX * 2) / (data.length - 1) : 0;
  const pointCoords = values.map((v, i) => ({
    x: padX + i * stepX,
    y: height - (v / max) * (height - 20) - 4,
  }));
  const pathPoints = pointCoords.map((p) => `${p.x},${p.y}`).join(" ");
  const areaPath = `M ${padX},${height} L ${pathPoints.split(" ").join(" L ")} L ${padX + (data.length - 1) * stepX},${height} Z`;

  const handleMove = (e: React.MouseEvent<HTMLDivElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    const relX = ((e.clientX - rect.left) / rect.width) * width;
    // Snap to nearest data point.
    let best = 0;
    let bestDist = Number.POSITIVE_INFINITY;
    for (let i = 0; i < pointCoords.length; i++) {
      const d = Math.abs(pointCoords[i].x - relX);
      if (d < bestDist) {
        bestDist = d;
        best = i;
      }
    }
    setHovered(best);
  };

  const hoveredPoint = hovered !== null ? pointCoords[hovered] : null;
  const hoveredData = hovered !== null ? data[hovered] : null;

  const formatDate = (iso: string) => {
    try {
      const d = new Date(`${iso}T00:00:00Z`);
      return d.toLocaleDateString(i18n.language, {
        weekday: "short",
        month: "short",
        day: "numeric",
      });
    } catch {
      return iso;
    }
  };
  const formatValue = (b: SessionsDayBucket) => {
    if (metric === "sessions") {
      return t("stats_page.metric_sessions_value", { count: b.sessions_count });
    }
    const mins = Math.round(b.avg_seconds / 60);
    return mins >= 60
      ? `${Math.floor(mins / 60)}h ${mins % 60}m`
      : `${mins}m`;
  };

  return (
    <div
      ref={containerRef}
      className="relative"
      onMouseMove={handleMove}
      onMouseLeave={() => setHovered(null)}
    >
      <svg
        viewBox={`0 0 ${width} ${height}`}
        className="w-full block"
        preserveAspectRatio="none"
      >
        <path d={areaPath} fill="rgba(255,70,51,0.12)" />
        <polyline
          points={pathPoints}
          fill="none"
          stroke="var(--color-accent, #FF4633)"
          strokeWidth={2}
          strokeLinejoin="round"
          strokeLinecap="round"
        />
        {hoveredPoint ? (
          <>
            <line
              x1={hoveredPoint.x}
              y1={0}
              x2={hoveredPoint.x}
              y2={height}
              stroke="var(--color-accent, #FF4633)"
              strokeOpacity={0.35}
              strokeDasharray="3,3"
              strokeWidth={1}
            />
            <circle
              cx={hoveredPoint.x}
              cy={hoveredPoint.y}
              r={4.5}
              fill="var(--color-accent, #FF4633)"
              stroke="#0F0F11"
              strokeWidth={2}
            />
          </>
        ) : null}
      </svg>

      {hoveredData && hoveredPoint ? (
        <div
          className="absolute pointer-events-none px-2.5 py-1.5 rounded-md bg-black/85 border border-white/[0.08] text-[11px] font-medium text-white shadow-lg whitespace-nowrap z-10"
          style={{
            left: `${(hoveredPoint.x / width) * 100}%`,
            top: `${(hoveredPoint.y / height) * 100}%`,
            transform: "translate(-50%, calc(-100% - 10px))",
          }}
        >
          <div className="text-text-muted text-[10px] uppercase tracking-wider mb-0.5">
            {formatDate(hoveredData.date)}
          </div>
          <div className="text-accent font-semibold">{formatValue(hoveredData)}</div>
        </div>
      ) : null}
    </div>
  );
}

function StatCard({
  title,
  value,
  sub,
  note,
  noteColor,
  delay,
}: {
  title: string;
  value: string;
  sub: string;
  note: string;
  noteColor?: "accent";
  delay: string;
}) {
  return (
    <div
      className="acrylic rounded-[14px] p-6 relative overflow-hidden group fade-in-up"
      style={{ animationDelay: delay }}
    >
      <h3 className="text-[12px] uppercase tracking-widest text-text-muted font-bold mb-3">
        {title}
      </h3>
      <div className="flex items-baseline gap-1">
        <span className="text-[32px] font-bold tracking-tighter text-white leading-none">
          {value}
        </span>
        <span className="text-[18px] font-semibold text-white/50">{sub}</span>
      </div>
      <p
        className={`text-[12px] font-medium mt-3 flex items-center gap-1.5 ${
          noteColor === "accent" ? "text-accent" : "text-text-sec"
        }`}
      >
        {noteColor === "accent" && (
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
            <polyline points="23 6 13.5 15.5 8.5 10.5 1 18" />
            <polyline points="17 6 23 6 23 12" />
          </svg>
        )}
        {note}
      </p>
    </div>
  );
}

interface HeatCell {
  x: number;
  y: number;
  color: string;
  date: string;
  seconds: number;
}

const HEATMAP_PALETTE = ["#1E1E22", "#4A201C", "#732A20", "#D93A2A", "#FF4633"];

function colorFor(seconds: number, max: number): string {
  if (seconds <= 0 || max <= 0) return HEATMAP_PALETTE[0];
  const ratio = seconds / max;
  if (ratio > 0.75) return HEATMAP_PALETTE[4];
  if (ratio > 0.5) return HEATMAP_PALETTE[3];
  if (ratio > 0.25) return HEATMAP_PALETTE[2];
  return HEATMAP_PALETTE[1];
}

function buildHeatmap(buckets: HeatmapBucket[]): HeatCell[] {
  const byDate = new Map<string, number>();
  let max = 0;
  for (const b of buckets) {
    byDate.set(b.date, b.total_seconds);
    if (b.total_seconds > max) max = b.total_seconds;
  }

  const cellSize = 12;
  const gap = 3;
  const xOffset = 30;

  // Find the most recent Sunday so the right edge aligns to "this week".
  const today = new Date();
  const todayUtc = new Date(
    Date.UTC(today.getUTCFullYear(), today.getUTCMonth(), today.getUTCDate())
  );
  const dayOfWeek = todayUtc.getUTCDay(); // 0 = Sunday
  const weeksBack = 52;
  // We render 53 columns total. Start = today - (weeksBack weeks) - dayOfWeek.
  const totalDays = weeksBack * 7 + dayOfWeek + 1;

  const cells: HeatCell[] = [];
  for (let i = 0; i < totalDays; i++) {
    const d = new Date(todayUtc);
    d.setUTCDate(todayUtc.getUTCDate() - (totalDays - 1 - i));
    const iso = d.toISOString().slice(0, 10);
    const seconds = byDate.get(iso) ?? 0;
    // column index from the left
    const col = Math.floor(i / 7);
    const row = d.getUTCDay();
    cells.push({
      x: xOffset + col * (cellSize + gap),
      y: 10 + row * (cellSize + gap),
      color: colorFor(seconds, max),
      date: iso,
      seconds,
    });
  }

  return cells;
}
