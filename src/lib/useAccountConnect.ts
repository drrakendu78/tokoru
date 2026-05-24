// Shared connect-flow plumbing for the OAuth / OpenID platforms (Steam, Epic, GOG).
//
// All three providers expose the same two-step shape: `start()` opens a
// WebView and the backend emits `<provider>-login-success` with some token
// payload; `finish(token)` then exchanges / persists it and (for Epic/GOG)
// pulls the owned library. This hook owns the listen / cleanup / failure-event
// boilerplate so the pages only deal with their own UI state.
//
// Epic and GOG carry `{ auth_code: string }`; Steam carries `{ steam_id: number }`.
// The `extractToken` option lets the caller pick the right field — defaults
// to reading `auth_code` so the Epic/GOG call sites stay terse.

import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

type DefaultPayload = { auth_code?: string };
type FailurePayload = { message?: string };

export interface UseAccountConnectOptions<TResult, TToken = string, TPayload = DefaultPayload> {
  /// Provider label shown in error toasts ("Steam", "Epic", "GOG").
  label: string;
  /// Backend success event name (e.g. "epic-login-success").
  eventName: string;
  /// Optional backend failure event (e.g. "steam-login-failure"). When present,
  /// the hook listens for `{ message }` payloads and routes them to `onError`.
  failureEventName?: string;
  start: () => Promise<void>;
  finish: (token: TToken) => Promise<TResult>;
  /// Pull the token from the success event payload. Defaults to reading
  /// `auth_code` as a string — override for Steam (`steam_id`) or any other
  /// provider whose payload shape differs.
  extractToken?: (payload: TPayload | undefined) => TToken | undefined;
  onSuccess?: (result: TResult) => void;
  onError?: (message: string) => void;
}

const defaultExtractToken = (payload: DefaultPayload | undefined) =>
  payload?.auth_code;

export function useAccountConnect<TResult, TToken = string, TPayload = DefaultPayload>(
  opts: UseAccountConnectOptions<TResult, TToken, TPayload>
) {
  const [busy, setBusy] = useState(false);
  // Ref so we can read the current `busy` inside the listener without making
  // the listener itself a dependency (we want to register once per mount).
  const busyRef = useRef(false);
  busyRef.current = busy;

  // We keep the latest opts in a ref too — the listener should always call the
  // freshest `finish` / callbacks without re-subscribing.
  const optsRef = useRef(opts);
  optsRef.current = opts;

  useEffect(() => {
    let unlistenSuccess: UnlistenFn | null = null;
    let unlistenFailure: UnlistenFn | null = null;
    let cancelled = false;

    const extractToken =
      (opts.extractToken as ((p: TPayload | undefined) => TToken | undefined) | undefined) ??
      (defaultExtractToken as unknown as (p: TPayload | undefined) => TToken | undefined);

    void listen<TPayload>(opts.eventName, async (event) => {
      const token = extractToken(event.payload);
      if (token === undefined || token === null) return;
      if (!busyRef.current) {
        // Stale event from a previous attempt — ignore.
        return;
      }
      try {
        const result = await optsRef.current.finish(token);
        if (!cancelled) optsRef.current.onSuccess?.(result);
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        if (!cancelled) {
          optsRef.current.onError?.(
            `${optsRef.current.label} login failed: ${msg}`
          );
        }
      } finally {
        if (!cancelled) setBusy(false);
      }
    }).then((fn) => {
      if (cancelled) {
        fn();
      } else {
        unlistenSuccess = fn;
      }
    });

    if (opts.failureEventName) {
      void listen<FailurePayload>(opts.failureEventName, (event) => {
        if (!busyRef.current) return;
        const msg = event.payload?.message ?? "Sign-in failed.";
        optsRef.current.onError?.(`${optsRef.current.label}: ${msg}`);
        if (!cancelled) setBusy(false);
      }).then((fn) => {
        if (cancelled) {
          fn();
        } else {
          unlistenFailure = fn;
        }
      });
    }

    return () => {
      cancelled = true;
      unlistenSuccess?.();
      unlistenFailure?.();
    };
    // We deliberately depend only on the event names — `start`/`finish`/
    // callbacks / extractor are read via refs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [opts.eventName, opts.failureEventName]);

  const connect = useCallback(async () => {
    if (busyRef.current) return;
    setBusy(true);
    try {
      await opts.start();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      opts.onError?.(`${opts.label} login failed: ${msg}`);
      setBusy(false);
    }
  }, [opts]);

  return { busy, connect };
}
