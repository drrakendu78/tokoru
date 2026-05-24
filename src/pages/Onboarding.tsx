import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { WindowControls } from "../components/WindowControls";
import { Toggle } from "../components/Toggle";
import { CoralButton } from "../components/CoralButton";
import { useRouter } from "../router";
import {
  detectInstalledCountsPerSource,
  epicGetConnection,
  epicLoginFinish,
  epicLoginStart,
  epicSyncLibrary,
  fullScan,
  getSteamConnection,
  gogGetConnection,
  gogLoginFinish,
  gogLoginStart,
  gogSyncLibrary,
  steamLoginFinish,
  steamLoginStart,
} from "../lib/api";
import { SUPPORTED_LOCALES } from "../i18n";
import type {
  EpicConnectedInfo,
  GogConnectedInfo,
  SteamConnectedInfo,
} from "../lib/types";
import { useAccountConnect } from "../lib/useAccountConnect";
import { useToast } from "../components/Toast";

interface DetectedSource {
  // Source key matches the Rust `detect_for_source` enum so we can read
  // counts straight from the API response without mapping.
  key: string;
  name: string;
  // null = source not detected on this install (greyed out). Filled by
  // the live detection API call on the Detection step mount.
  count: number | null;
  path: string;
  bg: string;
  iconColor?: string;
}

/// Static metadata for each source — name, install path hint, colour
/// scheme. `count` starts null and gets filled by the
/// `detect_installed_counts_per_source` Rust command when the user lands
/// on the Detection step. Heroic / Custom .exe stay null since we don't
/// auto-scan them (yet).
const SOURCE_TILES: DetectedSource[] = [
  { key: "steam", name: "Steam", count: null, path: "C:\\Program Files (x86)\\Steam", bg: "#171A21" },
  { key: "epic", name: "Epic Games", count: null, path: "C:\\Program Files\\Epic Games", bg: "#2A2A2A" },
  { key: "gog", name: "GOG Galaxy", count: null, path: "C:\\Program Files (x86)\\GOG Galaxy", bg: "#30164A", iconColor: "#C173FF" },
  { key: "ubi", name: "Ubisoft Connect", count: null, path: "C:\\Program Files (x86)\\Ubisoft", bg: "#051C3B", iconColor: "#0088F0" },
  { key: "ea", name: "EA App", count: null, path: "C:\\Program Files\\Electronic Arts\\EA Desktop", bg: "#202020" },
  { key: "itch", name: "Itch.io", count: null, path: "%APPDATA%\\itch", bg: "#40121A", iconColor: "#FA5C5C" },
  // Heroic intentionally omitted — Tokoru does the same job (scans Epic / GOG
  // natively), so Heroic as a separate source is redundant noise.
  { key: "custom", name: "Custom .exe", count: null, path: "None added yet", bg: "#1c1c1c" },
];

type Step = 1 | 2 | 3;

export function Onboarding() {
  const { t, i18n } = useTranslation();
  const [step, setStep] = useState<Step>(1);
  const [trackBg, setTrackBg] = useState(true);
  const [syncSteam, setSyncSteam] = useState(true);
  const [scanning, setScanning] = useState(false);
  const [steamConnectedId, setSteamConnectedId] = useState<number | null>(null);
  const [epicConnected, setEpicConnected] = useState<string | null>(null);
  const [gogConnected, setGogConnected] = useState<string | null>(null);
  // Live counts per source — filled when the user enters step 2 so the
  // Detection grid shows real numbers (Steam: 403, Epic: 23, …) instead
  // of placeholders. null while loading.
  const [liveCounts, setLiveCounts] = useState<Record<string, number> | null>(null);
  const { setRoute } = useRouter();
  const toast = useToast();

  // Language picker (step 1) — same wiring as Settings → Language, but
  // local here because the user hasn't reached Settings yet.
  const currentLocale = (i18n.resolvedLanguage ?? i18n.language ?? "en").split("-")[0];
  const changeLocale = async (code: string) => {
    try {
      await i18n.changeLanguage(code);
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("set_app_locale", { locale: code });
    } catch (e) {
      console.error("Onboarding: changeLocale failed", e);
    }
  };

  // Load real per-source counts the first time we land on step 2.
  useEffect(() => {
    if (step !== 2 || liveCounts !== null) return;
    void detectInstalledCountsPerSource()
      .then(setLiveCounts)
      .catch((e) => {
        console.error("Onboarding: detect counts failed", e);
        setLiveCounts({});
      });
  }, [step, liveCounts]);

  const steam = useAccountConnect<SteamConnectedInfo, number, { steam_id?: number; account_name?: string | null }>({
    label: "Steam",
    eventName: "steam-login-success",
    failureEventName: "steam-login-failure",
    start: steamLoginStart,
    finish: steamLoginFinish,
    extractToken: (p) => p?.steam_id,
    onSuccess: (r) => {
      setSteamConnectedId(r.steam_id);
      toast.success(
        r.account_name ? `Steam connected as ${r.account_name}` : `Steam connected (#${r.steam_id})`
      );
    },
    onError: (m) => toast.error(m),
  });

  const epic = useAccountConnect<EpicConnectedInfo>({
    label: "Epic",
    eventName: "epic-login-success",
    start: epicLoginStart,
    finish: epicLoginFinish,
    onSuccess: (r) => {
      setEpicConnected(r.account_name);
      toast.success(`Epic connected as ${r.account_name}`);
      // Pull the library now that the token is persisted (login_finish is
      // intentionally fast — sync runs as a separate call so the badge updates
      // immediately and the user sees the live "Syncing…" state).
      void epicSyncLibrary()
        .then((res) =>
          toast.success(`Epic library synced (+${res.added} new / ${res.total_owned} total)`)
        )
        .catch((e) =>
          toast.error(`Epic library sync failed: ${e instanceof Error ? e.message : String(e)}`)
        );
    },
    onError: (m) => toast.error(m),
  });

  const gog = useAccountConnect<GogConnectedInfo>({
    label: "GOG",
    eventName: "gog-login-success",
    start: gogLoginStart,
    finish: gogLoginFinish,
    onSuccess: (r) => {
      setGogConnected(r.account_name);
      toast.success(`GOG connected as ${r.account_name}`);
      void gogSyncLibrary()
        .then((res) =>
          toast.success(`GOG library synced (+${res.added} new / ${res.total_owned} total)`)
        )
        .catch((e) =>
          toast.error(`GOG library sync failed: ${e instanceof Error ? e.message : String(e)}`)
        );
    },
    onError: (m) => toast.error(m),
  });

  useEffect(() => {
    // If the user already connected accounts in a previous session, reflect it.
    void getSteamConnection()
      .then((c) => setSteamConnectedId(c.steam_id))
      .catch((e) => console.error("Onboarding: get_steam_connection failed", e));
    void epicGetConnection()
      .then((c) => setEpicConnected(c.account_name))
      .catch((e) => console.error("Onboarding: epic_get_connection failed", e));
    void gogGetConnection()
      .then((c) => setGogConnected(c.account_name))
      .catch((e) => console.error("Onboarding: gog_get_connection failed", e));
  }, []);

  const openLibrary = async () => {
    setScanning(true);
    try {
      await fullScan();
    } catch (e) {
      // Non-fatal — user still lands on the empty Library page and can rescan.
      console.error("Onboarding: full_scan failed", e);
    } finally {
      setScanning(false);
      // Persist completion so the next App boot routes straight to
      // `library` instead of replaying this wizard. Silent on failure —
      // the user can still navigate manually via the nav pills.
      try {
        const { markOnboardingDone } = await import("../lib/api");
        await markOnboardingDone();
      } catch (e) {
        console.error("Onboarding: mark_onboarding_done failed", e);
      }
      setRoute("library");
    }
  };

  // Merge static tile metadata with the live per-source counts. While
  // `liveCounts` is null we show the tiles in a neutral "loading" state
  // (count = null). Once the API resolves, each tile flips to its real
  // number; zero stays as "Not detected" (handled by DetectionTile).
  const detectedSources: DetectedSource[] = SOURCE_TILES.map((tile) => {
    const live = liveCounts?.[tile.key];
    return {
      ...tile,
      count: live === undefined ? null : live === 0 ? null : live,
    };
  });
  const totalCount = detectedSources
    .filter((d) => d.count && d.count > 0)
    .reduce((a, b) => a + (b.count ?? 0), 0);
  const activeSourceCount = detectedSources.filter(
    (d) => d.count && d.count > 0
  ).length;

  return (
    <div className="h-screen w-screen flex flex-col relative overflow-hidden">
      <div className="ambient-coral" />
      <header className="relative z-50 h-[56px] w-full flex items-center justify-between px-4 window-drag">
        <div />
        <WindowControls />
      </header>

      <main className="flex-1 flex items-center justify-center px-4 relative z-10 pb-12 overflow-auto">
        <div
          className="w-full max-w-[720px] acrylic rounded-2xl relative flex flex-col pt-10 pb-8 px-10 border border-white/[0.06] shadow-premium fade-in-up"
          style={{ animationDelay: "0.15s" }}
        >
          {/* Coral top accent */}
          <div className="absolute top-0 left-0 right-10 h-[1.5px] bg-gradient-to-r from-accent via-accent/40 to-transparent rounded-tl-2xl opacity-80" />

          {/* Hero — branding stays on every step */}
          <div className="mb-8 text-center flex flex-col items-center">
            <div className="flex items-center gap-2 text-text-main mb-5">
              <img
                src="/logo-dark.png"
                alt="Tokoru"
                width={32}
                height={32}
                className="select-none"
                draggable={false}
                data-theme-asset="logo"
              />
              <span className="font-semibold tracking-wide text-base">
                Tokoru
              </span>
            </div>
            <h1 className="text-[28px] md:text-[32px] font-semibold tracking-[-0.02em] leading-tight text-white mb-3">
              {step === 1
                ? t("onboarding.welcome")
                : step === 2
                  ? t("onboarding.step_launchers")
                  : t("onboarding.step_done")}
            </h1>
            <p className="text-[13px] md:text-sm text-text-sec leading-relaxed max-w-[90%] mx-auto">
              {step === 1
                ? t("onboarding.hero_desc")
                : step === 2
                  ? t("onboarding.step_launchers_desc")
                  : t("onboarding.step_done_desc")}
            </p>
          </div>

          {/* ── STEP 1 — Language ── */}
          {step === 1 && (
            <div className="mb-8">
              <label className="block text-[13px] font-semibold text-white mb-3 text-center">
                {t("settings.language.title")}
              </label>
              <div className="grid grid-cols-2 sm:grid-cols-3 gap-2 max-w-[480px] mx-auto">
                {SUPPORTED_LOCALES.map((l) => {
                  const active = currentLocale === l.code;
                  return (
                    <button
                      key={l.code}
                      onClick={() => void changeLocale(l.code)}
                      className={`px-3 py-2.5 rounded-lg text-[13px] font-medium transition-colors border ${
                        active
                          ? "bg-accent/15 border-accent/40 text-white shadow-glow"
                          : "bg-white/[0.03] border-white/[0.06] text-text-sec hover:text-white hover:bg-white/[0.06]"
                      }`}
                    >
                      {l.native}
                    </button>
                  );
                })}
              </div>
            </div>
          )}

          {/* ── STEP 2 — Detection + Accounts ── */}
          {step === 2 && (
          <div className="mb-8">
            <div className="grid grid-cols-1 md:grid-cols-2 gap-3 mb-4">
              {detectedSources.map((d, i) => (
                <DetectionTile key={d.key} item={d} index={i} />
              ))}
            </div>

            <div
              className="w-full bg-white/[0.03] border border-white/[0.05] rounded-xl p-3 flex items-center justify-center gap-2 shimmer-bg fade-in-up"
              style={{ animationDelay: "0.3s" }}
            >
              <div className="w-2 h-2 rounded-full bg-accent pulse-slow" />
              <span className="text-[13px] md:text-sm font-medium text-white/90">
                <strong className="text-white font-semibold">
                  {t("onboarding.games_found", { count: totalCount })}
                </strong>{" "}
                {t("onboarding.across_sources", { count: activeSourceCount })}
              </span>
            </div>
          </div>
          )}

          {/* ── STEP 2 (cont'd) — Connect accounts ── */}
          {step === 2 && (
          <div
            className="mb-8 pt-6 border-t border-white/[0.06] fade-in-up"
            style={{ animationDelay: "0.4s" }}
          >
            <div className="flex items-start justify-between gap-6">
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2 mb-1.5">
                  <h3 className="text-[14px] font-semibold text-white">
                    {t("onboarding.connect_steam")}
                  </h3>
                  <span className="text-[10px] font-medium uppercase tracking-wider px-1.5 py-0.5 rounded bg-white/[0.05] text-text-muted border border-white/[0.05]">
                    {t("onboarding.recommended")}
                  </span>
                </div>
                <p className="text-[12px] text-text-muted leading-relaxed">
                  {t("onboarding.connect_steam_desc")}
                </p>
              </div>
              <div className="shrink-0">
                {steamConnectedId ? (
                  <span className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-md bg-emerald-500/10 border border-emerald-500/20 text-emerald-300 text-[11px] font-semibold">
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3">
                      <polyline points="20 6 9 17 4 12" />
                    </svg>
                    {t("onboarding.connected")}
                  </span>
                ) : (
                  <CoralButton
                    variant="secondary"
                    size="sm"
                    onClick={steam.connect}
                    disabled={steam.busy}
                  >
                    {steam.busy ? t("onboarding.waiting_steam") : t("onboarding.connect_steam")}
                  </CoralButton>
                )}
              </div>
            </div>
            {steam.busy && !steamConnectedId && (
              <p className="text-[11px] text-text-muted mt-3">
                {t("onboarding.steam_window_hint")}
              </p>
            )}

            {/* Epic */}
            <div className="flex items-start justify-between gap-6 mt-6 pt-6 border-t border-white/[0.04]">
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2 mb-1.5">
                  <h3 className="text-[14px] font-semibold text-white">
                    {t("onboarding.connect_epic")}
                  </h3>
                  <span className="text-[10px] font-medium uppercase tracking-wider px-1.5 py-0.5 rounded bg-white/[0.05] text-text-muted border border-white/[0.05]">
                    {t("onboarding.recommended")}
                  </span>
                </div>
                <p className="text-[12px] text-text-muted leading-relaxed">
                  {t("onboarding.connect_epic_desc")}
                </p>
              </div>
              <div className="shrink-0">
                {epicConnected ? (
                  <span className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-md bg-emerald-500/10 border border-emerald-500/20 text-emerald-300 text-[11px] font-semibold">
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3">
                      <polyline points="20 6 9 17 4 12" />
                    </svg>
                    {epicConnected}
                  </span>
                ) : (
                  <CoralButton
                    variant="secondary"
                    size="sm"
                    onClick={epic.connect}
                    disabled={epic.busy}
                  >
                    {epic.busy ? t("onboarding.waiting_epic") : t("onboarding.connect_epic")}
                  </CoralButton>
                )}
              </div>
            </div>

            {/* GOG */}
            <div className="flex items-start justify-between gap-6 mt-6 pt-6 border-t border-white/[0.04]">
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2 mb-1.5">
                  <h3 className="text-[14px] font-semibold text-white">
                    {t("onboarding.connect_gog")}
                  </h3>
                  <span className="text-[10px] font-medium uppercase tracking-wider px-1.5 py-0.5 rounded bg-white/[0.05] text-text-muted border border-white/[0.05]">
                    {t("onboarding.recommended")}
                  </span>
                </div>
                <p className="text-[12px] text-text-muted leading-relaxed">
                  {t("onboarding.connect_gog_desc")}
                </p>
              </div>
              <div className="shrink-0">
                {gogConnected ? (
                  <span className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-md bg-emerald-500/10 border border-emerald-500/20 text-emerald-300 text-[11px] font-semibold">
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3">
                      <polyline points="20 6 9 17 4 12" />
                    </svg>
                    {gogConnected}
                  </span>
                ) : (
                  <CoralButton
                    variant="secondary"
                    size="sm"
                    onClick={gog.connect}
                    disabled={gog.busy}
                  >
                    {gog.busy ? t("onboarding.waiting_gog") : t("onboarding.connect_gog")}
                  </CoralButton>
                )}
              </div>
            </div>
          </div>
          )}

          {/* ── STEP 3 — Consent + Open ── */}
          {step === 3 && (
          <div
            className="mb-8 fade-in-up"
            style={{ animationDelay: "0.2s" }}
          >
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-6">
              <ConsentRow
                title={t("onboarding.consent_track_title")}
                desc={t("onboarding.consent_track_desc")}
                value={trackBg}
                onChange={setTrackBg}
              />
              <ConsentRow
                title={t("onboarding.consent_sync_title")}
                desc={t("onboarding.consent_sync_desc")}
                value={syncSteam}
                onChange={setSyncSteam}
              />
            </div>
            <div className="flex items-center justify-center gap-1.5 mt-8 text-[11px] text-text-sec opacity-80">
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
              </svg>
              {t("onboarding.privacy_note")}
            </div>
          </div>
          )}

          {/* Footer nav — step indicator + Back/Next buttons. The
              right-hand button is "Open my library" on the last step,
              "Next" otherwise. Configure manually exit is only shown on
              step 1 so the user can skip the whole wizard. */}
          <div className="relative pt-4 border-t border-white/[0.06]">
            <div className="flex items-center justify-center gap-1.5 mb-3">
              {[1, 2, 3].map((s) => (
                <div
                  key={s}
                  className={`h-1 w-8 rounded-full transition-colors ${
                    s <= step ? "bg-accent" : "bg-white/[0.08]"
                  }`}
                />
              ))}
            </div>
            <div className="flex items-center justify-between">
              {step === 1 ? (
                <button
                  onClick={() => setRoute("settings")}
                  className="px-5 py-2.5 rounded-lg text-[13px] font-medium text-text-sec hover:text-white hover:bg-white/[0.04] transition-colors border border-transparent hover:border-white/[0.05]"
                >
                  {t("onboarding.configure_manually")}
                </button>
              ) : (
                <button
                  onClick={() => setStep((s) => (s > 1 ? ((s - 1) as Step) : s))}
                  className="px-5 py-2.5 rounded-lg text-[13px] font-medium text-text-sec hover:text-white hover:bg-white/[0.04] transition-colors border border-transparent hover:border-white/[0.05]"
                >
                  {t("onboarding.back")}
                </button>
              )}
              {step < 3 ? (
                <CoralButton
                  variant="primary"
                  size="lg"
                  onClick={() => setStep((s) => (s < 3 ? ((s + 1) as Step) : s))}
                >
                  {t("onboarding.next")}
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
                    <path d="M5 12h14" />
                    <path d="m12 5 7 7-7 7" />
                  </svg>
                </CoralButton>
              ) : (
                <CoralButton
                  variant="primary"
                  size="lg"
                  onClick={openLibrary}
                  disabled={scanning}
                >
                  {scanning ? t("onboarding.scanning") : t("onboarding.open_library")}
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
                    <path d="M5 12h14" />
                    <path d="m12 5 7 7-7 7" />
                  </svg>
                </CoralButton>
              )}
            </div>
          </div>
        </div>
      </main>
    </div>
  );
}

function ConsentRow({
  title,
  desc,
  value,
  onChange,
}: {
  title: string;
  desc: string;
  value: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <label className="flex items-start gap-3 cursor-pointer group">
      <div className="shrink-0 mt-0.5">
        <Toggle checked={value} onChange={onChange} ariaLabel={title} />
      </div>
      <div className="flex-1 flex flex-col">
        <span className="text-[13px] font-medium text-white/95 group-hover:text-white transition-colors">
          {title}
        </span>
        <span className="text-[11px] text-text-muted mt-1 leading-relaxed pr-2">
          {desc}
        </span>
      </div>
    </label>
  );
}

function DetectionTile({
  item,
  index,
}: {
  item: DetectedSource;
  index: number;
}) {
  const { t } = useTranslation();
  const isUndetected = item.count === null;
  const isSteam = item.count === -1;
  const initials = item.name
    .split(" ")
    .map((s) => s[0])
    .join("")
    .slice(0, 3)
    .toUpperCase();

  return (
    <div
      className={`fade-in-up flex items-center p-3 rounded-xl border gap-3 transition-colors ${
        isUndetected
          ? "border-white/[0.02] bg-transparent opacity-60 grayscale"
          : "border-white/[0.04] bg-white/[0.015] hover:bg-white/[0.03]"
      }`}
      style={{ animationDelay: `${0.1 + index * 0.07}s` }}
    >
      <div
        className="w-9 h-9 rounded-full flex items-center justify-center shrink-0 border border-white/[0.08] shadow-sm text-[10px] font-bold"
        style={{
          background: item.bg,
          color: item.iconColor ?? "#ffffff",
        }}
      >
        {initials}
      </div>
      <div className="flex-1 min-w-0 flex flex-col justify-center">
        <div className="flex items-center justify-between gap-1">
          <h3
            className={`text-[13px] font-medium ${
              isUndetected ? "text-white/70" : "text-white/95"
            }`}
          >
            {item.name}
          </h3>
          {isSteam ? (
            <span className="text-[10px] font-medium text-emerald-400 bg-emerald-400/10 px-2 py-0.5 rounded-full shrink-0 flex items-center gap-1">
              <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5">
                <polyline points="20 6 9 17 4 12" />
              </svg>
              {t("onboarding.detected")}
            </span>
          ) : isUndetected ? (
            <span className="text-[10px] font-medium text-text-muted bg-white/[0.05] px-2 py-0.5 rounded-full shrink-0">
              {t("onboarding.not_detected")}
            </span>
          ) : (
            <span className="text-[11px] font-medium text-white/80 shrink-0">
              {t("onboarding.tile_games_count", { count: item.count })}
            </span>
          )}
        </div>
        <p className="text-[10px] text-text-muted font-mono truncate mt-0.5" title={item.path}>
          {item.path}
        </p>
      </div>
    </div>
  );
}
