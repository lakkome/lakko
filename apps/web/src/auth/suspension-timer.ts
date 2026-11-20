// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * When suspended mode asks whether the suspension ended (ADR-0099 step 3):
 * every ten minutes, and each time the app comes back to the foreground. A
 * refresh answering a pair ends suspended mode (the provider raises `pair`);
 * answering the same refusal changes nothing. The clock and the foreground
 * signal are injected so a test can drive them.
 */

import { SUSPENDED_REFRESH_INTERVAL_MS } from "./suspension";

export interface SuspensionCheckPorts {
  /** One refresh (the provider's `refreshOnce`). */
  readonly refresh: () => Promise<unknown>;
  readonly setInterval: (fn: () => void, ms: number) => unknown;
  readonly clearInterval: (handle: unknown) => void;
  /** Subscribe to "the app is in the foreground again"; returns the
   *  unsubscribe. */
  readonly onForeground: (fn: () => void) => () => void;
}

/** Start the checks; the returned function stops them. */
export function startSuspensionChecks(ports: SuspensionCheckPorts): () => void {
  const check = (): void => {
    void ports.refresh().catch(() => {
      /* the provider keeps what it held on a failure */
    });
  };
  const handle = ports.setInterval(check, SUSPENDED_REFRESH_INTERVAL_MS);
  const off = ports.onForeground(check);
  return () => {
    ports.clearInterval(handle);
    off();
  };
}

/** The page becoming visible again, as the foreground signal. */
export function documentForeground(fn: () => void): () => void {
  if (typeof document === "undefined") return () => undefined;
  const listener = (): void => {
    if (document.visibilityState === "visible") fn();
  };
  document.addEventListener("visibilitychange", listener);
  return () => document.removeEventListener("visibilitychange", listener);
}
