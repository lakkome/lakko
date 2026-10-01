// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "New version available — Reload" banner (Phase 1.18f).
 *
 * Listens for the `lakko:sw-update-available` event that
 * `register.ts` dispatches when the SW reaches `installed` AND
 * a prior worker is already controlling the page (= update,
 * not first install). On click:
 *
 *   1. Subscribes to `controllerchange` on
 *      `navigator.serviceWorker` — this fires when the new SW
 *      transitions to controlling the page.
 *   2. Posts `{ type: "SKIP_WAITING" }` to the waiting SW.
 *   3. SW activates → `controllerchange` fires → `location.reload()`.
 *
 * The reload is the load-bearing piece: without it the page
 * would keep running the prior JS while a new SW serves new
 * bytes — exactly the silent-mismatch footgun the explicit
 * banner is meant to avoid.
 *
 * Mounted once at the AppShell level; same instance covers
 * every protected route + the unauthenticated `/login` /
 * `/signup` / `/i/:slug` paths (the auth-shell layout renders
 * the banner too via a sibling mount).
 */
import { createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";
import { SW_UPDATE_EVENT } from "./register";
import { t } from "../l10n";

export function SwUpdateBanner(): JSX.Element {
  const [waiting, setWaiting] = createSignal<ServiceWorker | null>(null);
  const [reloading, setReloading] = createSignal(false);

  onMount(() => {
    const onUpdate = (e: Event): void => {
      // CustomEvent<ServiceWorker> is the shape `register.ts`
      // dispatches; safe-cast here because we own both sides of
      // the contract.
      const detail = (e as CustomEvent<ServiceWorker>).detail;
      setWaiting(detail);
    };
    window.addEventListener(SW_UPDATE_EVENT, onUpdate);
    onCleanup(() => {
      window.removeEventListener(SW_UPDATE_EVENT, onUpdate);
    });
  });

  function reload(): void {
    const w = waiting();
    if (w === null) return;
    setReloading(true);

    // Subscribe to controllerchange BEFORE sending SKIP_WAITING
    // so the listener is in place when the swap completes. `once:
    // true` removes itself after the first fire — if the user
    // somehow triggers a second skip we don't double-reload.
    navigator.serviceWorker.addEventListener(
      "controllerchange",
      () => {
        window.location.reload();
      },
      { once: true },
    );

    // Hand the new SW the SKIP_WAITING handshake; the message
    // handler in `sw.js` calls `self.skipWaiting()`. The browser
    // then activates the worker, controllerchange fires, the
    // reload listener above kicks in.
    w.postMessage({ type: "SKIP_WAITING" });
  }

  return (
    <Show when={waiting() !== null}>
      <div
        role="status"
        aria-live="polite"
        style={{
          position: "fixed",
          bottom: "1rem",
          left: "50%",
          transform: "translateX(-50%)",
          "max-width": "32rem",
          "z-index": 1000,
          background: "var(--surface-elevated)",
          color: "var(--surface-fg)",
          border: "1px solid var(--surface-border)",
          "border-radius": "var(--radius-md)",
          "box-shadow": "var(--shadow-1)",
          padding: "0.75rem 1rem",
          display: "flex",
          "align-items": "center",
          gap: "1rem",
        }}
      >
        <span style={{ flex: 1, "font-size": "0.9rem" }}>
          {t("app.update.ready")}
        </span>
        <button
          class="primary"
          type="button"
          onClick={reload}
          disabled={reloading()}
        >
          {reloading() ? t("app.update.reloading") : t("app.update.reload")}
        </button>
      </div>
    </Show>
  );
}
