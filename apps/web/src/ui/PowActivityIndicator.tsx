// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A quiet "still working" status while a proof-of-work solve runs.
 *
 * Signup, login and every directory publish solve an Equix PoW before the
 * server accepts the request. At the production effort that can take minutes,
 * and the solver reports no progress of its own — it is one call that returns
 * when it has found a proof. The solve now runs in a Web Worker, so the page
 * stays responsive; this line is what tells the user the app is working
 * rather than stuck, with the elapsed time as the only honest progress
 * measure available.
 *
 * It appears only after a short delay, so the common fast solve never flashes
 * a banner.
 */
import { createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";

import { subscribePowActivity } from "../auth/pow";

/** How long a solve must run before the status appears. */
const SHOW_AFTER_MS = 1_500;

export function PowActivityIndicator(): JSX.Element {
  const [startedAt, setStartedAt] = createSignal<number | null>(null);
  const [now, setNow] = createSignal(Date.now());

  onMount(() => {
    const unsubscribe = subscribePowActivity((inFlight) => {
      if (inFlight > 0) {
        if (startedAt() === null) setStartedAt(Date.now());
      } else {
        setStartedAt(null);
      }
    });
    const timer = setInterval(() => setNow(Date.now()), 1_000);
    onCleanup(() => {
      unsubscribe();
      clearInterval(timer);
    });
  });

  const elapsedSecs = (): number | null => {
    const start = startedAt();
    if (start === null) return null;
    const elapsed = now() - start;
    return elapsed >= SHOW_AFTER_MS ? Math.floor(elapsed / 1_000) : null;
  };

  return (
    <Show when={elapsedSecs() !== null}>
      <div
        role="status"
        aria-live="polite"
        style={{
          position: "fixed",
          bottom: "1rem",
          right: "1rem",
          "max-width": "20rem",
          "z-index": 999,
          background: "var(--surface-elevated)",
          color: "var(--surface-fg)",
          border: "1px solid var(--surface-border)",
          "border-radius": "var(--radius-md)",
          "box-shadow": "var(--shadow-1)",
          padding: "0.5rem 0.75rem",
          "font-size": "0.85rem",
        }}
      >
        Completing an anti-abuse check… {elapsedSecs()} s
      </div>
    </Show>
  );
}
