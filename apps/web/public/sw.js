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
 * ## Push events (1.21 — silent background decrypt)
 *
 *   * The `push` payload is `{ v, d, g, p }` (schema version, device
 *     pseudonym, group routing hex, DS envelope pointer — see
 *     `services/push-gw/src/providers/webpush.rs::WakeBody`). Post-
 *     ADR-0042 `g` is the OPAQUE `push_gid`; BOTH the decrypt path and
 *     the notification-TAP deep-link reverse-map it to the real group_id
 *     via the `lakko-push-routing`/`routes` IDB index (legacy raw-
 *     group_id wakes still resolve directly, and any miss keeps the raw
 *     `g` so behavior is unchanged for them).
 *   * When a Lakko tab is open + visible, the SW just postMessages the
 *     wake and shows a notification — the visible app drives the real
 *     preview + refresh itself.
 *   * When NO tab is open, the SW does a READ-ONLY THROWAWAY MLS
 *     decrypt: it reads the device key + last-persisted MLS snapshot +
 *     identity seed from IndexedDB (all same-origin), restores a
 *     TEMPORARY in-memory `Group`, GETs the DS journal, and shows the
 *     REAL newest-message text as the notification body. It NEVER
 *     persists the advanced state / advances the app's cursor (that
 *     would desync the app's ratchet — G-34 class); the append-only
 *     cursor-based journal makes the app's own re-processing on
 *     foreground idempotent. See `public/sw-push-decrypt.js`.
 *   * ANY failure in that path (unsupported module SW, wasm/IDB/DS/
 *     decrypt failure, an unhandled Wakeup / MailboxMessage kind)
 *     degrades to the generic "New message" notification. The push
 *     handler never throws (a thrown handler drops the notification).
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
 *     { v: 1, d: <device_hex>, g: <push_gid_or_legacy_group_hex>?, p: <pointer>? }
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
  // Decode the wake payload (no crypto — just JSON), then run the
  // dispatch. `parseWakeBody` never throws; `handlePush` wraps its
  // whole body so a thrown push handler can never drop the wake
  // notification entirely.
  const wake = parseWakeBody(event.data);
  event.waitUntil(handlePush(wake));
});

/**
 * The push dispatch:
 *
 *   1. Tell every open Lakko tab about the wake so a *visible* app
 *      handles the foreground refresh + its own preview natively.
 *   2. When NO visible tab is open, try a READ-ONLY THROWAWAY MLS
 *      decrypt (1.21) so the notification body carries the REAL
 *      newest-message text instead of a generic "New message". Any
 *      failure degrades to generic — the SW never throws out of the
 *      push handler (a thrown handler drops the notification).
 *   3. Show the notification (real preview if we got one, else
 *      generic).
 *
 * Click-through: `notificationclick` (below) opens the right route.
 */
async function handlePush(wake) {
  // (a) Notify open tabs + detect whether one is visible.
  let hasVisibleTab = false;
  try {
    const clients = await self.clients.matchAll({
      type: "window",
      includeUncontrolled: false,
    });
    for (const c of clients) {
      c.postMessage({ type: "LAKKO_PUSH_WAKE", wake });
      if (c.visibilityState === "visible") hasVisibleTab = true;
    }
  } catch {
    // Best effort — a closed tab is the common case (and the exact
    // case the background decrypt below exists for).
  }

  // (b) Background decrypt — only when there is no visible tab (a
  //     visible app drives the real preview itself). Never throws:
  //     `backgroundPreview` returns null on ANY failure.
  let preview = null;
  if (!hasVisibleTab) {
    try {
      preview = await backgroundPreview(wake);
    } catch {
      preview = null; // Belt-and-suspenders — degrade to generic.
    }
  }

  // (c) Reverse-map the opaque `push_gid` to the REAL group_id for the
  //     TAP deep-link (ADR-0042). Resolved HERE rather than inside
  //     `notificationclick` on purpose: the click handler must reach
  //     `clients.openWindow` with as few awaits as possible so the
  //     notification's user activation survives. A miss leaves the
  //     payload untouched, so the legacy path stays byte-for-byte.
  //     Note this deliberately does NOT touch `tag` below — `push_gid`
  //     is stable per group, so the existing grouping is already right.
  const tapGroupHex =
    wake?.groupHex != null ? await resolveTapGroupHex(wake.groupHex) : null;

  // (d) Show the notification. Real preview when we have one; the
  //     generic "New message" fallback otherwise (unchanged behavior).
  const title = preview?.title ?? "Lakko";
  const body = preview?.body ?? "New message";
  const tag = preview?.tag ?? wake?.groupHex ?? "lakko-default";
  const data =
    wake !== null
      ? tapGroupHex !== null
        ? { wake, tapGroupHex }
        : { wake }
      : {};
  try {
    await self.registration.showNotification(title, {
      body,
      // Replacing-tag behavior: a quick burst of messages in the
      // same group collapses into one OS notification.
      tag,
      renotify: false,
      data,
      icon: "/icon-192.png",
      badge: "/icon-96.png",
    });
  } catch {
    // Some browsers + private modes don't allow showing notifications
    // from a push event under certain conditions — best-effort, don't
    // throw past `waitUntil`.
  }
}

/**
 * ADR-0042 — reverse-map a wake's opaque `push_gid` (`g`) to the REAL
 * DS `group_id`, lowercase-hex, for the notification-TAP deep-link.
 *
 * Reads the SAME plaintext `lakko-push-routing`/`routes` reverse index
 * the background decrypt consults (see `buildDecryptDeps.lookupRoute`
 * and `src/push/group-routing-store.ts`): `push_gid` is a one-way
 * derivation and `group_id` is a public DS routing address, so the row
 * needs no device key — which is exactly why the SW can read it here.
 * READ-ONLY, like every other SW touch of app storage.
 *
 * Returns `null` on ANY miss — a legacy pre-ADR-0042 raw-group_id wake,
 * a group whose route was never provisioned, a malformed row, or an IDB
 * failure. The caller then leaves the tap payload on the raw `g`, i.e.
 * exactly today's behavior. NEVER throws: a rejection here would
 * propagate out of the `push` handler and drop the notification.
 */
async function resolveTapGroupHex(pushGidHex) {
  try {
    const row = await idbGet("lakko-push-routing", "routes", pushGidHex);
    if (row === undefined || typeof row.groupIdB64 !== "string") return null;
    return bytesToHex(base64Decode(row.groupIdB64));
  } catch {
    return null;
  }
}

// ----- background decrypt (1.21) -------------------------------
//
// READ-ONLY THROWAWAY MLS decrypt for the no-open-tab case. See the
// heavily-commented pure core in `public/sw-push-decrypt.js` for the
// load-bearing correctness discipline: this path reads the device
// key + last-persisted MLS snapshot + identity seed, restores a
// TEMPORARY in-memory `Group`, GETs the DS journal, extracts the
// newest message's plaintext preview, and DISCARDS everything. It
// NEVER persists the advanced snapshot, NEVER advances the app's DS
// cursor, NEVER writes MLS state — doing so would desync the
// foreground app's ratchet (the G-34 nonce-reuse-on-crash class). The
// DS journal is append-only + cursor-based, so the app re-processing
// from its OWN cursor on next foreground is idempotent.
//
// Stable dist URLs (build-sw.mjs copies the wasm-pack output into
// `dist/sw-wasm/` and Vite copies `sw-push-decrypt.js` verbatim). Both
// are dynamic-imported, so this only works in a MODULE service worker
// (see `src/sw/register.ts`); a classic registration makes `import()`
// throw, which the try/catch above turns into the generic fallback.
const SW_DECRYPT_MODULE_URL = "/sw-push-decrypt.js";
const SW_WASM_GLUE_URL = "/sw-wasm/lakko_ffi_wasm.js";
const SW_WASM_BINARY_URL = "/sw-wasm/lakko_ffi_wasm_bg.wasm";

/** Cache the loaded wasm namespace across pushes in this SW lifetime. */
let swWasmPromise = null;

/** Cache the loaded pure-core module promise. */
let swDecryptModulePromise = null;

/**
 * Attempt the background decrypt. Returns `{ title, body, tag }` on
 * success, or `null` for ANY reason (unsupported module SW, wasm
 * unavailable, no local account/key/snapshot, DS failure, nothing
 * decryptable) so the caller shows the generic notification.
 */
async function backgroundPreview(wake) {
  // Only GroupMessage wakes carry a group + pointer; the pure core
  // also re-checks. Wakeup / MailboxMessage stay generic (follow-up).
  if (
    wake === null ||
    typeof wake.groupHex !== "string" ||
    typeof wake.pointer !== "number"
  ) {
    return null;
  }

  const [core, wasm] = await Promise.all([loadDecryptModule(), loadSwWasm()]);
  const deps = buildDecryptDeps(wasm);
  return core.decryptPushPreview(wake, deps);
}

/** Dynamic-import the pure decrypt core (module SW only). */
function loadDecryptModule() {
  if (swDecryptModulePromise === null) {
    swDecryptModulePromise = import(SW_DECRYPT_MODULE_URL).catch((err) => {
      // Reset so a transient failure can be retried on the next push.
      swDecryptModulePromise = null;
      throw err;
    });
  }
  return swDecryptModulePromise;
}

/** Dynamic-import + init the wasm core with an explicit `.wasm` URL. */
function loadSwWasm() {
  if (swWasmPromise === null) {
    swWasmPromise = (async () => {
      const mod = await import(SW_WASM_GLUE_URL);
      // `mod.default` is wasm-pack's `__wbg_init`; the explicit URL
      // avoids relying on `import.meta.url` resolution.
      await mod.default(SW_WASM_BINARY_URL);
      return mod;
    })().catch((err) => {
      swWasmPromise = null;
      throw err;
    });
  }
  return swWasmPromise;
}

/**
 * Build the READ-ONLY dependency object the pure core consumes. Every
 * capability here is a read or a pure compute — there is deliberately
 * NO write path (no `saveSnapshot` / `saveMessageLog` / cursor advance).
 */
function buildDecryptDeps(wasm) {
  return {
    // ADR-0042 Sprint 4 — reverse-map an opaque `push_gid` wake `g`
    // back to the owning `(accountId, base64 group_id)`. The `routes`
    // store is PLAINTEXT by design (push_gid is a one-way derivation;
    // group_id is a public DS routing address — see
    // `src/push/group-routing-store.ts`) precisely so this read needs
    // no device key. READ-ONLY: the SW never writes push-routing state.
    // `null` on a miss → the core falls back to the legacy raw-group_id
    // path; a throw is treated as a miss by the core (fail-closed).
    lookupRoute: async (pushGidHex) => {
      const row = await idbGet("lakko-push-routing", "routes", pushGidHex);
      if (
        row === undefined ||
        typeof row.accountId !== "string" ||
        typeof row.groupIdB64 !== "string"
      ) {
        return null;
      }
      return { accountId: row.accountId, groupIdB64: row.groupIdB64 };
    },
    // Which locally-stored account owns this group? `knownGroupIds` is
    // stored UNENCRYPTED (public group_id routing addresses), so this
    // needs no device key. Also validates the wake's group is known.
    // LEGACY fallback — runs only when the route lookup misses.
    resolveAccount: async (groupHex) => {
      const rows = await idbGetAll("lakko-mls", "snapshots");
      for (const row of rows) {
        const known = Array.isArray(row?.knownGroupIds) ? row.knownGroupIds : [];
        for (const kg of known) {
          let bytes;
          try {
            bytes = base64Decode(kg);
          } catch {
            continue;
          }
          if (bytesToHex(bytes) === groupHex) {
            return { accountId: row.accountId, groupIdBytes: bytes };
          }
        }
      }
      return null;
    },
    // Non-extractable AES-GCM device root key (usable for decrypt).
    getDeviceKey: async (accountId) => {
      const row = await idbGet("lakko-auth", "device_keys", accountId);
      return row?.key ?? null;
    },
    // 64-byte AccountIdentity seed, unwrapped under the device key.
    getIdentitySeed: async (accountId, deviceKey) => {
      const row = await idbGet(
        "lakko-account-identity-seed",
        "seeds",
        accountId,
      );
      if (row === undefined || row.ciphertext === undefined) return null;
      try {
        const seed = await aesGcmUnwrap(deviceKey, row.ciphertext);
        return seed.length === 64 ? seed : null;
      } catch {
        return null;
      }
    },
    // Last-persisted MLS provider snapshot, unwrapped under the device key.
    loadSnapshot: async (accountId, deviceKey) => {
      const row = await idbGet("lakko-mls", "snapshots", accountId);
      if (row === undefined || row.ciphertext === undefined) return null;
      try {
        const snapshotBytes = await aesGcmUnwrap(deviceKey, row.ciphertext);
        const knownGroupIds = Array.isArray(row.knownGroupIds)
          ? row.knownGroupIds
          : [];
        return { snapshotBytes, knownGroupIds };
      } catch {
        return null;
      }
    },
    wasm,
    // READ-ONLY DS group-journal fetch of rows strictly after `afterId`.
    dsFetchGroup: async (groupIdBytes, afterId) => {
      const hex = bytesToHex(groupIdBytes);
      const url = `/api/ds/group/${hex}/messages?after=${afterId}&wait=0`;
      const resp = await fetch(url, { method: "GET" });
      if (!resp.ok) throw new Error(`ds fetch ${resp.status}`);
      const json = await resp.json();
      const messages = (json?.messages ?? []).map((m) => ({
        id: m.id,
        payload: base64Decode(m.payload_b64),
        createdAt: m.created_at,
      }));
      return { messages };
    },
    // The app's last-persisted per-group cursor (READ-ONLY). Used as
    // the fetch start so intervening commits replay + the epoch stays
    // correct. Best-effort — null (→ core uses pointer-1) on any miss.
    startCursor: async (accountId, groupHex) => {
      const deviceKey = await idbGet(
        "lakko-auth",
        "device_keys",
        accountId,
      ).then((r) => r?.key ?? null);
      if (deviceKey === null) return null;
      const row = await idbGet("lakko-message-log", "logs", accountId);
      if (row === undefined || row.ciphertext === undefined) return null;
      try {
        const plain = await aesGcmUnwrap(deviceKey, row.ciphertext);
        const payload = JSON.parse(new TextDecoder().decode(plain));
        const cursorStr = payload?.groups?.[groupHex]?.cursor;
        if (typeof cursorStr !== "string") return null;
        const n = Number(cursorStr);
        return Number.isFinite(n) ? n : null;
      } catch {
        return null;
      }
    },
  };
}

// ----- minimal IDB + crypto helpers (SW context) ---------------
//
// The SW is same-origin, so it can open the very databases the app
// wrote. It CANNOT import the `src/*` store modules (not in the SW's
// module graph), so these are hand-rolled read-only mirrors of the
// `keyPath: "accountId"` stores. All READS.

function idbOpen(name) {
  return new Promise((resolve, reject) => {
    // Open WITHOUT a version so the SW never triggers an
    // `onupgradeneeded` schema change: it reads the app's DB at
    // whatever version the app created, never racing the app's own
    // migration. (A never-created DB opens empty; the `contains(store)`
    // guard below then returns undefined.) This also removes the
    // version-drift hazard of hardcoding each store's version here.
    const req = indexedDB.open(name);
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error ?? new Error("idb open failed"));
    req.onblocked = () => reject(new Error("idb open blocked"));
  });
}

async function idbGet(name, store, key) {
  const db = await idbOpen(name);
  try {
    if (!db.objectStoreNames.contains(store)) return undefined;
    return await new Promise((resolve, reject) => {
      const tx = db.transaction(store, "readonly");
      const req = tx.objectStore(store).get(key);
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error ?? new Error("idb get failed"));
    });
  } finally {
    db.close();
  }
}

async function idbGetAll(name, store) {
  const db = await idbOpen(name);
  try {
    if (!db.objectStoreNames.contains(store)) return [];
    return await new Promise((resolve, reject) => {
      const tx = db.transaction(store, "readonly");
      const req = tx.objectStore(store).getAll();
      req.onsuccess = () => resolve(req.result ?? []);
      req.onerror = () => reject(req.error ?? new Error("idb getAll failed"));
    });
  } finally {
    db.close();
  }
}

/** Unwrap `[12-byte IV | AES-GCM ciphertext+tag]` under `deviceKey`. */
async function aesGcmUnwrap(deviceKey, blob) {
  if (blob.length < 12 + 16) throw new Error("ciphertext too short");
  const iv = blob.slice(0, 12);
  const ct = blob.slice(12);
  return new Uint8Array(
    await crypto.subtle.decrypt({ name: "AES-GCM", iv }, deviceKey, ct),
  );
}

/** Lowercase-hex encode (matches `ds/client.ts` `pseudonymHex`). */
function bytesToHex(bytes) {
  let out = "";
  for (let i = 0; i < bytes.length; i++) {
    out += (bytes[i] ?? 0).toString(16).padStart(2, "0");
  }
  return out;
}

/** Standard-alphabet base64 → bytes (matches `auth/account-id`). */
function base64Decode(b64) {
  const binary = atob(b64);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

// ----- notificationclick (1.20b) -------------------------------

self.addEventListener("notificationclick", (event) => {
  // Close the OS notification immediately so the user doesn't see
  // it linger after the click.
  event.notification.close();

  const wake = event.notification.data?.wake;
  // ADR-0042 — `wake.groupHex` is the OPAQUE `push_gid`, which no SPA
  // surface can match against a group. `handlePush` already resolved it
  // to the real `group_id` hex via the `routes` reverse index and stashed
  // it as `tapGroupHex`; prefer that. On a miss (legacy wake / route not
  // provisioned) the field is absent and we fall back to the raw `g` —
  // today's behavior, which the SPA treats as an unknown group.
  const tapGroupHex = event.notification.data?.tapGroupHex ?? wake?.groupHex;
  // Default landing: the app shell. When we know the group, route
  // to the per-chat page so the user lands on the unread thread.
  // The `paths.chat(...)` slug is base64url-no-pad(groupId) — the
  // SW doesn't have that helper (nor the base64url alphabet swap),
  // so we open the root with the hex in `?wake=` and let the SPA's
  // deep-link reader (`src/push/WakeNavigator.tsx`) do the
  // conversion + the navigation. G-36: that reader did not exist
  // until now, which is why a tap used to land on the shell.
  let url = "/";
  if (tapGroupHex) {
    url = `/?wake=${encodeURIComponent(tapGroupHex)}`;
  }

  event.waitUntil(
    (async () => {
      // If a Lakko tab is already open, tell it which chat was tapped
      // and raise it.
      const clients = await self.clients.matchAll({
        type: "window",
        includeUncontrolled: true,
      });
      for (const c of clients) {
        if (c.url.startsWith(self.location.origin)) {
          // G-36 — focusing alone tells the tab nothing about WHICH
          // chat was tapped. The `push`-time `LAKKO_PUSH_WAKE`
          // broadcast can't stand in for this: it fires on message
          // ARRIVAL for every open tab (navigating on it would yank a
          // reading user around), and post-ADR-0042 its `wake.groupHex`
          // is the OPAQUE `push_gid`, which matches no group. So post
          // the already-resolved real `group_id` hex, on the TAP only.
          // `src/push/WakeNavigator.tsx` opens that chat when this
          // device knows it and ignores it otherwise.
          //
          // Posted BEFORE the focus so a `focus()` rejection (some
          // browsers refuse it without user activation) cannot swallow
          // the deep link — the tab still routes, it just isn't raised.
          if (tapGroupHex) {
            try {
              c.postMessage({
                type: "LAKKO_NOTIFICATION_TAP",
                groupHex: tapGroupHex,
              });
            } catch {
              // The client went away between `matchAll` and here.
            }
          }
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
