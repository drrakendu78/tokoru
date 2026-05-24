import { useTranslation } from "react-i18next";

export type SteamStatus = "in-steam" | "not-in-steam" | "syncing" | "error";

export function StatusPill({ status }: { status: SteamStatus }) {
  const { t } = useTranslation();
  if (status === "in-steam") {
    return (
      <span className="dark-pill bg-emerald-500/20 border border-emerald-500/30 text-emerald-400 text-[9px] font-semibold px-2 py-0.5 rounded-full flex items-center gap-1 backdrop-blur-md whitespace-nowrap">
        <svg
          width="10"
          height="10"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="3"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <polyline points="20 6 9 17 4 12" />
        </svg>
        {t("gamecard.on_steam")}
      </span>
    );
  }
  if (status === "not-in-steam") {
    return (
      <div className="h-6 px-3 rounded-full bg-accent text-white shadow-glow flex items-center gap-1 text-[11px] font-bold">
        <svg
          width="12"
          height="12"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.5"
          strokeLinecap="round"
        >
          <line x1="12" y1="5" x2="12" y2="19" />
          <line x1="5" y1="12" x2="19" y2="12" />
        </svg>
        {t("gamecard.add")}
      </div>
    );
  }
  if (status === "syncing") {
    return (
      <div className="h-6 w-6 rounded-full bg-shell/80 backdrop-blur-md border border-white/[0.08] shadow-sm flex items-center justify-center text-white/80">
        <svg
          width="12"
          height="12"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.5"
          strokeLinecap="round"
          className="spinner"
        >
          <line x1="12" y1="2" x2="12" y2="6" />
          <line x1="12" y1="18" x2="12" y2="22" />
          <line x1="4.93" y1="4.93" x2="7.76" y2="7.76" />
          <line x1="16.24" y1="16.24" x2="19.07" y2="19.07" />
          <line x1="2" y1="12" x2="6" y2="12" />
          <line x1="18" y1="12" x2="22" y2="12" />
          <line x1="4.93" y1="19.07" x2="7.76" y2="16.24" />
          <line x1="16.24" y1="7.76" x2="19.07" y2="4.93" />
        </svg>
      </div>
    );
  }
  return (
    <div className="h-6 px-2.5 rounded-full bg-yellow-500/15 border border-yellow-500/30 text-yellow-400 text-[11px] font-medium flex items-center gap-1.5">
      <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5">
        <circle cx="12" cy="12" r="10" />
        <line x1="12" y1="8" x2="12" y2="12" />
        <line x1="12" y1="16" x2="12.01" y2="16" />
      </svg>
      {t("gamecard.error")}
    </div>
  );
}
