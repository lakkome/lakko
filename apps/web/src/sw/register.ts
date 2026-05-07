// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Service-worker registration entry point (Phase 1.16f2).
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
 *   * Listen for `updatefound` and log when a new SW is waiting.
 *     The actual "click to refresh" UI is a follow-up; for now we
 *     only emit a console message + post a `SKIP_WAITING`
 *     handshake to the new worker so a refresh picks it up
 *     without manual cache-clearing.
 *
 * The function is idempotent — calling it twice in the same page
 * session is a no-op past the first registration.
 */

const SW_URL = "/sw.js";
const SW_SCOPE = "/";

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
    const registration = await navigator.serviceWorker.register(SW_URL, {
      scope: SW_SCOPE,
    });
    registered = true;

    // The browser fires `updatefound` whenever the SW byte content
    // at SW_URL has changed since the last registered version.
    // The new worker installs in the background; once `installed`
    // we send it the SKIP_WAITING message so it activates without
    // waiting for every Lakko tab to close. The page itself does
    // not auto-reload — a future banner UI will prompt the user
    // for that explicitly.
    registration.addEventListener("updatefound", () => {
      const incoming = registration.installing;
      if (incoming === null) return;
      incoming.addEventListener("statechange", () => {
        if (
          incoming.state === "installed" &&
          navigator.serviceWorker.controller !== null
        ) {
          // eslint-disable-next-line no-console
          console.info(
            "lakko: a new service worker is ready; refresh to activate",
          );
          incoming.postMessage({ type: "SKIP_WAITING" });
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
