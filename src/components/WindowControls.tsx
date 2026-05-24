import { getCurrentWindow } from "@tauri-apps/api/window";

export function WindowControls() {
  const handle = async (action: "min" | "max" | "close") => {
    try {
      const w = getCurrentWindow();
      if (action === "min") await w.minimize();
      else if (action === "max") await w.toggleMaximize();
      else await w.close();
    } catch {
      // not in tauri context (e.g. plain vite preview) — no-op
    }
  };

  return (
    <div className="flex items-center no-drag ml-2 space-x-0.5">
      <button
        onClick={() => handle("min")}
        className="w-10 h-[32px] flex items-center justify-center text-text-sec hover:bg-white/10 hover:text-white transition-colors rounded-sm"
        aria-label="Minimize"
      >
        <svg width="11" height="11" viewBox="0 0 11 11" fill="none">
          <path d="M11 5.5H0V4.5H11V5.5Z" fill="currentColor" />
        </svg>
      </button>
      <button
        onClick={() => handle("max")}
        className="w-10 h-[32px] flex items-center justify-center text-text-sec hover:bg-white/10 hover:text-white transition-colors rounded-sm"
        aria-label="Maximize"
      >
        <svg width="11" height="11" viewBox="0 0 11 11" fill="none">
          <path
            d="M10.5 10.5H0.5V0.5H10.5V10.5ZM1.5 9.5H9.5V1.5H1.5V9.5Z"
            fill="currentColor"
          />
        </svg>
      </button>
      <button
        onClick={() => handle("close")}
        className="w-10 h-[32px] flex items-center justify-center text-text-sec hover:bg-[#E81123] hover:text-white transition-colors rounded-sm"
        aria-label="Close"
      >
        <svg width="11" height="11" viewBox="0 0 11 11" fill="none">
          <path
            d="M10.5 1.207L9.293 0L5.5 3.793L1.707 0L0.5 1.207L4.293 5L0.5 8.793L1.707 10L5.5 6.207L9.293 10L10.5 8.793L6.707 5L10.5 1.207Z"
            fill="currentColor"
          />
        </svg>
      </button>
    </div>
  );
}
