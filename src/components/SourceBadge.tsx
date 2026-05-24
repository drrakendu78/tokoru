import { SOURCE_LONG } from "../lib/types";

interface SourceBadgeProps {
  source: string;
  selected?: boolean;
  label?: string;
}

/// Per-source background + text colors. Mapped 1:1 from the aidesigner
/// mockup run `2026-05-24T12-32-16-757Z-redessine-les-boutons-d-action-en-ba`
/// (cards row top-left badges). Falls back to a neutral dark pill for
/// sources the mockup didn't draw (ubi/ea/itch/xbox/amazon/custom).
const SOURCE_BG: Record<string, string> = {
  steam: "bg-[#171a21] text-white border border-white/10",
  epic: "bg-zinc-200 text-zinc-900 border border-transparent",
  gog: "bg-[#8b2b9e] text-white border border-transparent",
  ubi: "bg-[#0c5fa3] text-white border border-transparent",
  ea: "bg-zinc-200 text-zinc-900 border border-transparent",
  itch: "bg-[#fa5c5c] text-white border border-transparent",
  xbox: "bg-[#107C10] text-white border border-transparent",
  amazon: "bg-[#FF9900] text-zinc-900 border border-transparent",
  custom: "bg-zinc-700 text-white border border-white/10",
  local: "bg-zinc-700 text-white border border-white/10",
};

/** Compact pill used on top-left of game cards. */
export function SourceBadge({ source, selected = false, label }: SourceBadgeProps) {
  const text = label ?? SOURCE_LONG[source] ?? source;
  if (selected) {
    return (
      <div className="h-6 px-2 rounded-md bg-accent text-white flex items-center justify-center shadow-glow font-bold text-[9px] uppercase tracking-wider gap-1.5">
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
        {text}
      </div>
    );
  }
  const bgClass = SOURCE_BG[source] ?? SOURCE_BG.custom;
  return (
    <span
      className={`dark-pill ${bgClass} text-[9px] font-bold px-1.5 py-0.5 rounded-[4px] uppercase tracking-wider backdrop-blur-md shadow-sm whitespace-nowrap`}
    >
      {text}
    </span>
  );
}
