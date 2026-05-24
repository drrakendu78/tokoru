import type { ReactNode } from "react";
import {
  ToastContext,
  useToast as useToastInternal,
  useToastState,
  type Toast,
} from "../lib/useToast";

/// Re-export so other files only need `from "../components/Toast"`.
export const useToast = useToastInternal;

interface ToastProviderProps {
  children: ReactNode;
}

export function ToastProvider({ children }: ToastProviderProps) {
  const state = useToastState();
  return (
    <ToastContext.Provider value={state}>
      {children}
      <ToastStack />
    </ToastContext.Provider>
  );
}

function ToastStack() {
  const { toasts, dismiss } = useToastInternal();
  return (
    <div className="fixed top-12 right-4 z-[60] flex flex-col gap-2 pointer-events-none w-[320px]">
      {toasts.map((t) => (
        <ToastItem key={t.id} toast={t} onDismiss={() => dismiss(t.id)} />
      ))}
    </div>
  );
}

function ToastItem({ toast, onDismiss }: { toast: Toast; onDismiss: () => void }) {
  const accent =
    toast.kind === "success"
      ? "border-accent/40 text-accent"
      : toast.kind === "error"
      ? "border-red-400/40 text-red-300"
      : "border-white/15 text-text-sec";

  const icon =
    toast.kind === "success" ? (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
        <polyline points="20 6 9 17 4 12" />
      </svg>
    ) : toast.kind === "error" ? (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5">
        <circle cx="12" cy="12" r="10" />
        <line x1="12" y1="8" x2="12" y2="12" />
        <line x1="12" y1="16" x2="12.01" y2="16" />
      </svg>
    ) : (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5">
        <circle cx="12" cy="12" r="10" />
        <line x1="12" y1="16" x2="12" y2="12" />
        <line x1="12" y1="8" x2="12.01" y2="8" />
      </svg>
    );

  return (
    <div
      className={`acrylic rounded-xl border ${accent} px-3 py-2.5 shadow-premium pointer-events-auto flex items-start gap-2.5`}
      style={{
        animation: "fadeUp 0.2s cubic-bezier(0.2, 0.8, 0.2, 1)",
      }}
    >
      <span className="mt-0.5 shrink-0">{icon}</span>
      <p className="text-[12px] font-medium text-white/90 flex-1 leading-snug">
        {toast.message}
      </p>
      {toast.action && (
        <button
          onClick={() => {
            toast.action?.onClick();
            onDismiss();
          }}
          className="shrink-0 text-[11px] font-semibold text-accent hover:text-accent-hover bg-accent/10 hover:bg-accent/20 px-2 py-1 rounded-md transition-colors"
        >
          {toast.action.label}
        </button>
      )}
      <button
        onClick={onDismiss}
        className="shrink-0 text-text-muted hover:text-white transition-colors"
        aria-label="Dismiss"
      >
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5">
          <line x1="18" y1="6" x2="6" y2="18" />
          <line x1="6" y1="6" x2="18" y2="18" />
        </svg>
      </button>
    </div>
  );
}
