// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Lakko service worker (Phase 1.16f2).
 *
 * Vanilla SW — no Workbox, no third-party deps. The whole flow is
 * ~150 lines and stays auditable end-to-end. Vite copies this file
 * verbatim from `apps/web/public/sw.js` into `dist/sw.js` during
 * production build; `apps/web/scripts/build-sw.mjs` then rewrites
 * the two placeholders below with the build-specific asset list +
 * version stamp.
 *
 * ## Cache strategy
 *
 *   * **Hashed assets** under `/assets/*.{js,css,wasm}` — content-
 *     addressable filenames change with every release. Cache-first
 *     with no expiry: a hit means the bytes are immutable.
 *   * **Navigation** (the HTML shell, `/` and any future routed
 *     paths served by index.html) — network-first with a cache
 *     fallback. We MUST hit the network when the user is online so
 *     a fresh deploy of `index.html` brings in the latest hashed
 *     asset references; the cache fallback only runs when the
 *     fetch throws (offline).
 *   * **`/api/*`** (auth / discovery / DS) — **network-only**. A
 *     stale OPAQUE round, a cached message journal, or a frozen
 *     KeyPackage list would all be active footguns. Offline = fail.
 *   * **Everything else** (manifest, favicons, `sw.js` itself) —
 *     stale-while-revalidate is fine. Browser default.
 *
 * ## Update lifecycle
 *
 *   * On `install`, precache every asset in `PRECACHE_MANIFEST`.
 *   * On `activate`, sweep any cache whose name doesn't match the
 *     current `BUILD_VERSION` so old releases don't accumulate.
 *   * `skipWaiting` runs inside `install`, so a new SW activates as
 *     soon as the prior page session is gone — no manual user
 *     gesture needed for the first round of updates. A future
 *     iteration can swap to "wait for user click" once we ship a
 *     "new version available" banner UI.
 *
 * ## Push events (placeholder)
 *
 *   * 1.18 (encrypted-push receive path) wires the `push` event
 *     handler — payload is `(group_pseudonym, envelope_pointer)`,
 *     SW fetches the ciphertext, hands it off to the WASM core via
 *     a clients-postMessage round, and renders a local
 *     notification. We expose the empty handler now so the SW
 *     subscription surface is in place.
 */

// Replaced by `scripts/build-sw.mjs` after `vite build`.
const BUILD_VERSION = "__BUILD_VERSION__";
const PRECACHE_MANIFEST = __PRECACHE_MANIFEST__;

const CACHE_NAME = `lakko-app-shell-${BUILD_VERSION}`;

// Single source of truth for "is this a backend API call". Keep
// in lock-step with `vite.config.ts` proxy rewrites — every path
// listed there must be matched here so we never serve stale.
const API_PREFIXES = [
  "/api/auth",
  "/api/discovery",
  "/api/ds",
  "/api/storage",
  "/api/push",
];

// ----- install -------------------------------------------------

self.addEventListener("install", (event) => {
  event.waitUntil(
    (async () => {
      const cache = await caches.open(CACHE_NAME);
      // `addAll` is atomic — if any single fetch fails, the entire
      // install fails and the SW does not progress to "waiting".
      // That's intentional: a partially-cached shell would serve
      // a broken offline experience.
      await cache.addAll(PRECACHE_MANIFEST);
      // 1.18f — `skipWaiting()` is NOT called here any more. The
      // first-install case (no controller present) progresses to
      // `activating` on its own without a prior worker to wait
      // out, so the very first registration still takes effect
      // immediately. **Updates** (controller is set) now stay in
      // `waiting` — the page detects this via `registration.waiting`
      // and surfaces a "new version available — reload" banner.
      // The user clicks the banner, which posts `SKIP_WAITING`
      // (handler below) and listens for `controllerchange` before
      // reloading. Silent auto-update was a footgun: the active
      // page kept running JS that no longer matched the cached
      // bytes the new SW just installed.
    })(),
  );
});

// ----- activate ------------------------------------------------

self.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      // Sweep prior versions. We only own caches that begin with
      // `lakko-app-shell-`; anything else is owned by another SW
      // (none expected today, but defensive).
      const names = await caches.keys();
      await Promise.all(
        names
          .filter((n) => n.startsWith("lakko-app-shell-") && n !== CACHE_NAME)
          .map((n) => caches.delete(n)),
      );
      // Take control of any open Lakko tabs immediately so the
      // first request after activation already routes through
      // this SW's fetch handler.
      await self.clients.claim();
    })(),
  );
});

// ----- fetch ---------------------------------------------------

self.addEventListener("fetch", (event) => {
  const req = event.request;

  // SW only intercepts same-origin GETs. Cross-origin (CDN, third-
  // party fetches if any) and non-GET (POST to /api/*) skip the SW
  // and hit the network directly.
  if (req.method !== "GET") return;

  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;

  // Network-only for backend API. Stale auth / DS bytes would be a
  // security footgun, and we never want to mask a real failure.
  if (API_PREFIXES.some((p) => url.pathname.startsWith(p))) {
    return; // implicit network-only — no event.respondWith
  }

  // Cache-first for hashed assets. Filenames carry a content hash,
  // so a cache hit is by definition the right bytes.
  if (url.pathname.startsWith("/assets/")) {
    event.respondWith(cacheFirst(req));
    return;
  }

  // Navigation (and fallthrough HTML) — network-first with cache
  // fallback. The browser uses `mode === "navigate"` for top-level
  // documents; routed sub-paths like `/chat/abc` also count.
  if (req.mode === "navigate" || req.destination === "document") {
    event.respondWith(networkFirstWithCacheFallback(req));
    return;
  }

  // Everything else (manifest, favicons, the SW itself when it
  // checks for updates) — let the browser default kick in.
});

async function cacheFirst(req) {
  const cache = await caches.open(CACHE_NAME);
  const hit = await cache.match(req);
  if (hit !== undefined) return hit;
  // Not in cache — fetch + cache for next time. We do NOT cache
  // error responses (5xx, opaque CORS failures), only 200-OK.
  const fresh = await fetch(req);
  if (fresh.ok) {
    cache.put(req, fresh.clone());
  }
  return fresh;
}

async function networkFirstWithCacheFallback(req) {
  try {
    const fresh = await fetch(req);
    if (fresh.ok) {
      const cache = await caches.open(CACHE_NAME);
      cache.put(req, fresh.clone());
    }
    return fresh;
  } catch (err) {
    // Offline — fall back to cached HTML if we have it. The
    // precache always seeds `index.html` so a cold-start offline
    // load works after the first online visit.
    const cache = await caches.open(CACHE_NAME);
    const fallback = (await cache.match(req)) ?? (await cache.match("/index.html"));
    if (fallback !== undefined) return fallback;
    throw err;
  }
}

// ----- push (1.20b) --------------------------------------------

/**
 * Decode the wake payload that `services/push-gw` ships through
 * Web Push (RFC 8291 aes128gcm — the browser does the AEAD on our
 * behalf, we receive the plaintext JSON). The body shape is
 * defined by `WakeBody` in `services/push-gw/src/providers/webpush.rs`:
 *
 *     { v: 1, d: <device_hex>, g: <group_hex>?, p: <pointer>? }
 *
 * Unknown versions / malformed JSON / missing fields collapse into
 * a generic notification — the user still gets a wake, just without
 * the per-message hint.
 */
function parseWakeBody(data) {
  if (data === null || data === undefined) return null;
  try {
    const json = data.json();
    if (json === null || typeof json !== "object") return null;
    if (json.v !== 1) return null;
    return {
      deviceHex: typeof json.d === "string" ? json.d : null,
      groupHex: typeof json.g === "string" ? json.g : null,
      pointer: typeof json.p === "number" ? json.p : null,
    };
  } catch {
    return null;
  }
}

self.addEventListener("push", (event) => {
  // Decode + dispatch. The SW deliberately does NOT attempt to
  // decrypt the MLS message itself: doing so would require the
  // non-extractable device key (which the SW *can* access, since
  // the IDB-stored CryptoKey is keyed by origin), the cached MLS
  // group state, and ~megabytes of WASM glue — all running inside
  // a ≤30-second budget the OS hands the SW before killing it.
  //
  // Instead we:
  //
  //   1. Decode the wake payload (no crypto, just JSON).
  //   2. Render a generic notification ("New message"). The user
  //      sees the wake; the preview body is intentionally
  //      content-free because the SW context can't safely render
  //      decrypted plaintext without holding the full MLS state.
  //   3. Forward the wake hint to any open tabs via postMessage
  //      so they can refresh the visible thread without waiting
  //      for the next poll tick.
  //
  // Click-through: `notificationclick` (below) opens the right
  // route — the SPA itself drives the actual decrypt.
  const wake = parseWakeBody(event.data);
  event.waitUntil(
    (async () => {
      // (a) Tell every open Lakko tab about the wake — they
      //     handle the foreground refresh path natively + can
      //     surface a real (decrypted) preview if the page is
      //     visible.
      try {
        const clients = await self.clients.matchAll({
          type: "window",
          includeUncontrolled: false,
        });
        for (const c of clients) {
          c.postMessage({ type: "LAKKO_PUSH_WAKE", wake });
        }
      } catch {
        // Best effort — a closed tab is the common case.
      }

      // (b) Build a local notification. Generic body so we don't
      //     leak any decrypted content from the SW context (which
      //     would need to hydrate the full MLS state).
      const title = "Lakko";
      const body = "New message";
      // Click-action carries the wake hint so the route layer can
      // jump to the right chat without re-polling.
      const data = wake !== null ? { wake } : {};
      try {
        await self.registration.showNotification(title, {
          body,
          tag: wake?.groupHex ?? "lakko-default",
          // Replacing-tag behavior: a quick burst of messages in
          // the same group collapses into one OS notification —
          // matches the platform-native chat-app pattern.
          renotify: false,
          data,
          icon: "/icon-192.png",
          badge: "/icon-96.png",
        });
      } catch {
        // Some browsers + private modes don't allow showing
        // notifications from a push event under certain
        // conditions — best-effort, don't throw past `waitUntil`.
      }
    })(),
  );
});

// ----- notificationclick (1.20b) -------------------------------

self.addEventListener("notificationclick", (event) => {
  // Close the OS notification immediately so the user doesn't see
  // it linger after the click.
  event.notification.close();

  const wake = event.notification.data?.wake;
  // Default landing: the app shell. When we know the group, route
  // to the per-chat page so the user lands on the unread thread.
  // The `paths.chat(...)` slug is base64url-no-pad(groupId) — the
  // SW doesn't have the helper, so we just open the root and let
  // the SPA route via its own deep-link reader (set in 1.18g).
  let url = "/";
  if (wake?.groupHex) {
    url = `/?wake=${encodeURIComponent(wake.groupHex)}`;
  }

  event.waitUntil(
    (async () => {
      // If a Lakko tab is already open, focus it (and let the SPA
      // pull the wake out of the postMessage we already sent).
      const clients = await self.clients.matchAll({
        type: "window",
        includeUncontrolled: true,
      });
      for (const c of clients) {
        if (c.url.startsWith(self.location.origin)) {
          await c.focus();
          return;
        }
      }
      // No open tab — open a fresh one.
      await self.clients.openWindow(url);
    })(),
  );
});

// ----- message (skip-waiting handshake) ------------------------

self.addEventListener("message", (event) => {
  // Live since 1.18f: the page's "new version available" banner
  // posts this when the user clicks Reload. The SW transitions
  // out of `waiting` into `activating` → `activated`; the page
  // listens for `controllerchange` and reloads itself once the
  // swap is observable. See `apps/web/src/sw/register.ts` for
  // the page-side counterpart.
  if (event.data && event.data.type === "SKIP_WAITING") {
    void self.skipWaiting();
  }
});
