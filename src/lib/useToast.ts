// Tiny toast bus. A `<ToastProvider>` mounts the stack; anywhere in the tree
// `useToast()` returns helpers to push messages onto it.

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from "react";

export type ToastKind = "success" | "error" | "info";

/// Optional call-to-action attached to a toast. When set, the toast
/// renders a small button next to the dismiss icon; clicking it fires
/// `onClick` and then auto-dismisses the toast.
export interface ToastAction {
  label: string;
  onClick: () => void;
}

export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
  /// Optional action button (e.g. "Sync now" on the startup metadata
  /// reminder). When present the toast is also persistent — no auto-
  /// dismiss — so the user has time to act on it.
  action?: ToastAction;
}

interface PushOptions {
  action?: ToastAction;
  /// Skip the auto-dismiss timer. Default true when `action` is set,
  /// false otherwise.
  persistent?: boolean;
}

/// Subset of fields that `update` accepts. Anything omitted is left as-is
/// on the existing toast — passing `{ message: "..." }` swaps just the
/// text, leaving the action button intact, which is what the metadata
/// sync progress display needs.
interface ToastPatch {
  kind?: ToastKind;
  message?: string;
  action?: ToastAction;
}

interface ToastContextValue {
  toasts: Toast[];
  /// Returns the id of the newly-pushed toast so callers can `update`
  /// it later (e.g. a long-running sync that wants to live-edit its
  /// own progress message instead of pushing a fresh toast each tick).
  push: (kind: ToastKind, message: string, opts?: PushOptions) => number;
  success: (message: string) => number;
  error: (message: string) => number;
  info: (message: string, opts?: PushOptions) => number;
  update: (id: number, patch: ToastPatch) => void;
  dismiss: (id: number) => void;
}

export const ToastContext = createContext<ToastContextValue | null>(null);

export function useToast(): ToastContextValue {
  const ctx = useContext(ToastContext);
  if (!ctx) {
    throw new Error("useToast must be used inside <ToastProvider>");
  }
  return ctx;
}

// Auto-dismiss after this many ms.
const DEFAULT_TTL_MS = 4000;

export function useToastState(): ToastContextValue {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const counter = useRef(0);
  const timers = useRef<Record<number, ReturnType<typeof setTimeout>>>({});

  const dismiss = useCallback((id: number) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
    if (timers.current[id]) {
      clearTimeout(timers.current[id]);
      delete timers.current[id];
    }
  }, []);

  const push = useCallback(
    (kind: ToastKind, message: string, opts?: PushOptions): number => {
      counter.current += 1;
      const id = counter.current;
      setToasts((prev) => [...prev, { id, kind, message, action: opts?.action }]);
      const persistent = opts?.persistent ?? Boolean(opts?.action);
      if (!persistent) {
        timers.current[id] = setTimeout(() => dismiss(id), DEFAULT_TTL_MS);
      }
      return id;
    },
    [dismiss]
  );

  /// Patch an existing toast in place. Caller passes the id returned by
  /// `push` and any subset of `{ kind, message, action }` — omitted
  /// fields stay as they were. Used by the metadata sync to live-edit
  /// its own "Syncing X/Y..." message every few seconds without spawning
  /// a new toast each tick.
  ///
  /// Silently no-ops when the id isn't in the stack (already dismissed
  /// or never existed) so callers don't have to track lifecycle.
  const update = useCallback((id: number, patch: ToastPatch) => {
    setToasts((prev) =>
      prev.map((t) => (t.id === id ? { ...t, ...patch } : t))
    );
  }, []);

  // Cleanup on unmount.
  useEffect(() => {
    const t = timers.current;
    return () => {
      for (const id in t) clearTimeout(t[id]);
    };
  }, []);

  const success = useCallback((m: string) => push("success", m), [push]);
  const error = useCallback((m: string) => push("error", m), [push]);
  const info = useCallback(
    (m: string, opts?: PushOptions) => push("info", m, opts),
    [push]
  );

  return { toasts, push, success, error, info, update, dismiss };
}
