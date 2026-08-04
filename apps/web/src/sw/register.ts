// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Service-worker registration entry point (Phase 1.16f2 + 1.18f).
 *
 * Called from `main.tsx` after the Solid app mounts. Three
 * responsibilities:
 *
 *   * Skip registration in dev. The SW intercepts every fetch on
 *     activation, which conflicts with Vite's HMR socket and the
 *     dev-server proxy for `/api/*`. Dev builds set
 *     `import.meta.env.DEV === true` (Vite default).
 *   * Skip registration in browsers that don't expose
 *     `navigator.serviceWorker` (older Safari, sandboxed iframes).
 *     Lakko is meant to be a PWA-class app on every supported
 *     platform, but a missing SW does not break core function —
 *     only offline + push are unavailable.
 *   * Listen for `updatefound` and surface a `lakko:sw-update-available`
 *     CustomEvent on `window` carrying the waiting `ServiceWorker`.
 *     The `SwUpdateBanner` component in the UI layer subscribes to
 *     this event and renders the "Reload to update" prompt. Silent
 *     auto-update was a footgun: the active page kept running JS
 *     that no longer matched the cached bytes the new SW just
 *     installed.
 *
 * The function is idempotent — calling it twice in the same page
 * session is a no-op past the first registration.
 */

const SW_URL = "/sw.js";
const SW_SCOPE = "/";

/** Event name dispatched on `window` when the SW reaches `installed`
 *  AND a prior SW is already controlling the page (i.e. an update,
 *  not the first registration). Carries the waiting `ServiceWorker`
 *  in `event.detail` so the UI can post the SKIP_WAITING handshake
 *  when the user clicks the banner's Reload button. */
export const SW_UPDATE_EVENT = "lakko:sw-update-available";

let registered = false;

/**
 * Register the Lakko service worker. Best-effort: errors are
 * logged via `console.warn` and swallowed — the app still works
 * without offline / push, just degrades to network-only.
 *
 * Returns `true` on a successful registration (or when one was
 * already in place from a prior call), `false` when the SW path
 * is intentionally skipped (dev build, unsupported browser).
 */
export async function registerServiceWorker(): Promise<boolean> {
  if (registered) return true;

  // 1. Dev gate. Vite injects `import.meta.env.DEV`.
  if (import.meta.env.DEV) {
    return false;
  }

  // 2. Capability gate.
  if (typeof navigator === "undefined" || !("serviceWorker" in navigator)) {
    return false;
  }

  try {
    // Register as a MODULE service worker: the `push` handler
    // dynamic-`import()`s the pure decrypt core + the wasm glue for the
    // 1.21 silent background decrypt, and `import()` is only permitted
    // in a module worker. Browsers that don't support module SWs
    // (older Safari/Firefox) reject `type: "module"`; we fall back to a
    // classic registration so offline caching + generic push still
    // work — on those the background `import()` throws and the push
    // path degrades to the generic notification (no regression).
    let registration: ServiceWorkerRegistration;
    try {
      registration = await navigator.serviceWorker.register(SW_URL, {
        scope: SW_SCOPE,
        type: "module",
      });
    } catch (moduleErr) {
      // eslint-disable-next-line no-console
      console.warn(
        "lakko: module service worker registration failed; falling back to classic (silent push decrypt disabled)",
        moduleErr,
      );
      registration = await navigator.serviceWorker.register(SW_URL, {
        scope: SW_SCOPE,
      });
    }
    registered = true;

    // Edge case: a previous load already left a worker in the
    // `waiting` state. Common when the user navigates away after
    // the SW installed but before they clicked Reload. Surface
    // the update banner on this mount too.
    if (registration.waiting !== null && navigator.serviceWorker.controller !== null) {
      dispatchUpdateAvailable(registration.waiting);
    }

    // Standard `updatefound` path: a new SW byte-content has
    // shown up at SW_URL. We watch its statechange transitions;
    // when it reaches `installed` AND a prior worker is already
    // controlling the page, this is an UPDATE (not first install)
    // and we surface the banner.
    registration.addEventListener("updatefound", () => {
      const incoming = registration.installing;
      if (incoming === null) return;
      incoming.addEventListener("statechange", () => {
        if (
          incoming.state === "installed" &&
          navigator.serviceWorker.controller !== null
        ) {
          dispatchUpdateAvailable(incoming);
        }
      });
    });

    return true;
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("lakko: service worker registration failed", err);
    return false;
  }
}

/** Internal — dispatch the update-available event on `window` so
 *  the `SwUpdateBanner` component can pick it up regardless of
 *  whether it's mounted yet (it listens on the global). */
function dispatchUpdateAvailable(worker: ServiceWorker): void {
  // eslint-disable-next-line no-console
  console.info(
    "lakko: a new service worker is ready; reload to activate",
  );
  window.dispatchEvent(
    new CustomEvent<ServiceWorker>(SW_UPDATE_EVENT, { detail: worker }),
  );
}
