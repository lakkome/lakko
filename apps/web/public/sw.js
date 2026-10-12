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
 *     via the `lakko-push-routing`/`account_routes` IDB index, for the
 *     BOUND account only (legacy raw-group_id wakes resolve among that
 *     account's own known groups).
 *   * ADR-0042, "A wake is shown only for the signed-in account": a wake is
 *     handled only when the `lakko-push-routing`/`wake_binding` row exists
 *     and the wake's `d` equals the pseudonym it names, and only as that
 *     row's account; its `g` must name one of that account's groups. A wake
 *     that fails any of this is dropped whole: no message to open tabs, no
 *     notification. (A browser may then show its own "updated in the
 *     background" notice; that names no account and carries no content.)
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
 *   * A decrypted CALL offer rings (`docs/ux/calls.md` §2.1, §7) — and
 *     is announced WITHOUT ringing when the conversation is muted. The
 *     mute flag is read from the app's encrypted `lakko-device-sync`
 *     row, unwrapped under the same device key, and is the one read
 *     here that FAILS OPEN: a miss rings rather than silences.
 *
 * ## Every store this worker touches, and in which direction
 *
 * All READS, in `readonly` transactions, opened without a version so
 * the worker can never trigger a schema change (C-59 / ADR-0041):
 * `lakko-push-routing`/`wake_binding` and `/account_routes`,
 * `lakko-mls`/`snapshots`,
 * `lakko-auth`/`device_keys`, `lakko-account-identity-seed`/`seeds`,
 * `lakko-message-log`/`logs`, `lakko-profiles`/`rows` and
 * `lakko-device-sync`/`rows`. There is NO write path anywhere in this
 * file, and the decrypt core is handed no capability that could
 * introduce one.
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
 *     { v: 1, d: <device_hex>, g: <push_gid_or_legacy_group_hex>?, p: <pointer>?, k: "c"? }
 *
 * `k` is the wake KIND: `"c"` marks a call, absent marks a message.
 * It rides inside the RFC 8291 encryption, so the browser's push
 * service never sees it. The kind alone cannot ring — it names no
 * call, no caller and no media — so the ring still comes from the
 * decrypt of the journal row it points at. What it is good for is
 * honesty when that decrypt yields nothing: a call must not be
 * announced as "New message".
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
      // Anything that is not the call tag is a message, so an older
      // push-gw (which sends no `k` at all) behaves exactly as before.
      isCall: json.k === "c",
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
  // (0) The gate. Nothing below runs for a wake that is not the bound
  //     account's: no postMessage (the open app belongs to the signed-in
  //     account), no decrypt, no notification.
  let admission = null;
  try {
    admission = await admitWake(wake);
  } catch {
    admission = null;
  }
  if (admission === null) return;

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

  // (c) The REAL group_id for the TAP deep-link (ADR-0042), already
  //     resolved by the gate for the bound account. Resolved at push time
  //     rather than inside `notificationclick` on purpose: the click
  //     handler must reach `clients.openWindow` with as few awaits as
  //     possible so the notification's user activation survives. `null`
  //     only for a wake that names no group. Note this deliberately does
  //     NOT touch `tag` below — `push_gid` is stable per group, so the
  //     existing grouping is already right.
  const tapGroupHex = admission.groupHex;

  // (c2) A call the decrypt classified: ring, or take a ring down. Handled
  //      before the message path because a call is neither a message nor a
  //      preview of one — it is an interruption with its own shape, its own
  //      tag (keyed on the call id, not the group) and its own buttons.
  //      Everything about it is decided in `sw-call-notification.js`, which
  //      the decrypt core re-exports so this costs no second import.
  if (preview?.call != null) {
    await handleCallNotice(preview.call);
    return;
  }

  // (d) Show the notification. Real preview when we have one; the
  //     generic fallback otherwise (unchanged behavior, except that a
  //     wake tagged as a call says so rather than claiming a message
  //     arrived — reached when the decrypt could not run at all, e.g. a
  //     classic service worker, or could not name the call). It carries no
  //     Accept / Decline: without the decrypt there is no call id to
  //     address, so the honest offer is to open the app, where the
  //     full-screen incoming call is waiting.
  const title = preview?.title ?? "Lakko";
  const body =
    preview?.body ??
    (wake?.isCall === true
      ? storedIncomingCallWord(await readNotificationStrings())
      : storedNewMessageWord(await readNotificationStrings()));
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
 * ADR-0061 — the notification words the page stored in the interface
 * language, as a raw parsed value, or `null` when there are none (no page has
 * run since install, storage was cleared, or it is unreadable).
 *
 * The cache name and key are `NOTIFICATION_STRINGS_CACHE` /
 * `NOTIFICATION_STRINGS_URL` in `sw-call-notification.js`; repeated here
 * because this worker may run as a classic script and cannot import it before
 * the decrypt core loads. `src/l10n/notification-strings.test.ts` pins the
 * page's writer to the same pair.
 *
 * @returns {Promise<unknown>}
 */
async function readNotificationStrings() {
  try {
    const cache = await caches.open("lakko-l10n");
    const hit = await cache.match("/__lakko/l10n/notification-strings.json");
    return hit === undefined ? null : await hit.json();
  } catch {
    return null;
  }
}

/**
 * "Incoming call" in the stored language, for the generic notification the
 * worker shows without the decrypt core; English when nothing usable is
 * stored.
 *
 * @param {unknown} raw
 * @returns {string}
 */
function storedIncomingCallWord(raw) {
  const v = typeof raw === "object" && raw !== null ? /** @type {Record<string, unknown>} */ (raw).incomingCall : undefined;
  return typeof v === "string" && v.length > 0 && v.length <= 200 ? v : "Incoming call";
}

/**
 * "New message" in the stored language, for the generic message notification
 * the worker shows when no preview could be decrypted; English when nothing
 * usable is stored.
 *
 * @param {unknown} raw
 * @returns {string}
 */
function storedNewMessageWord(raw) {
  const v = typeof raw === "object" && raw !== null ? /** @type {Record<string, unknown>} */ (raw).newMessage : undefined;
  return typeof v === "string" && v.length > 0 && v.length <= 200 ? v : "New message";
}

/**
 * Ring, or stop ringing.
 *
 * `docs/ux/calls.md` §2.1: on web a call arrives as a notification with
 * Accept and Decline. Two kinds reach here:
 *
 *   * **ring** — an offer. The shape (title, `Voice call` / `Video call`,
 *     `requireInteraction`, vibration, the two buttons, and the precomputed
 *     per-button intents) comes from `callRingNotification`, so what a button
 *     means is decided in a pure, tested function rather than in the click
 *     handler. An offer the decrypt classified as SILENT — the conversation
 *     is muted (`docs/ux/calls.md` section 2.1) — takes the same path through
 *     `callSilentNotification` instead: announced, never interrupting.
 *   * **end** — the caller cancelled, the callee answered elsewhere, or the
 *     call was declined. Any ring showing for THAT call id is closed. Nothing
 *     new is shown: a notification saying a call is over is not something
 *     anyone can act on, and a stuck ring is the one failure
 *     `requireInteraction: true` would otherwise make permanent.
 *
 * Caveat worth stating: a push that shows no notification at all can make
 * some browsers substitute their own "this site was updated in the
 * background" notice. An end wake takes that risk deliberately — the
 * alternative is leaving a ring on screen for a call that has ended.
 *
 * Never throws: like every other path out of `push`, a failure here must not
 * drop the event.
 */
async function handleCallNotice(call) {
  let core;
  try {
    core = await loadDecryptModule();
  } catch {
    return; // Cannot shape a ring without the module; stay silent.
  }
  try {
    if (call.kind === "end") {
      const tag = core.callNotificationTag(call.callIdHex);
      const showing = await self.registration.getNotifications({ tag });
      for (const n of showing) n.close();
      return;
    }
    // `docs/ux/calls.md` section 2.1 — a muted conversation is announced
    // without being allowed to interrupt: silent, no vibration, no buttons,
    // nothing that demands interaction. Still the same tag, so the end wake
    // above takes it down either way.
    // ADR-0061: the words in the interface language, as the page stored
    // them; English for anything missing.
    const strings = core.notificationStrings(await readNotificationStrings());
    const { title, options } = call.silent === true
      ? core.callSilentNotification(call, strings)
      : core.callRingNotification(call, strings);
    await self.registration.showNotification(title, options);
  } catch {
    // Best-effort, exactly as the message path is.
  }
}

/**
 * ADR-0042, "A wake is shown only for the signed-in account" — decide
 * whether a wake is handled at all, and as whom.
 *
 * Returns `{ accountId, groupHex }` — `groupHex` the REAL DS group id
 * (lowercase hex) of one of the bound account's groups, or `null` for a
 * wake that names no group (a device wake) — or `null` when the wake must be
 * dropped:
 *
 *   * no binding row (no account signed in for push on this browser);
 *   * the wake's `d` is not the bound device pseudonym;
 *   * its `g` names none of the bound account's groups: neither the
 *     account-scoped reverse map nor that account's own known groups.
 *
 * A pure mirror of `wakeBoundAccount` + `resolveWakeTarget` in
 * `public/sw-push-decrypt.js` (which the tests pin), kept inline so a CLASSIC
 * service worker — which cannot `import()` that module — applies the same
 * rule. The two must stay in step. READ-ONLY. Never throws.
 */
async function admitWake(wake) {
  if (wake === null || wake === undefined) return null;
  let binding = null;
  try {
    binding = await readWakeBinding();
  } catch {
    binding = null;
  }
  if (binding === null || typeof binding.accountId !== "string") return null;
  const bound = normalisedPseudonymHex(binding.devicePseudonymHex);
  const presented = normalisedPseudonymHex(wake.deviceHex);
  if (bound === null || presented === null || bound !== presented) return null;
  const accountId = binding.accountId;
  if (typeof wake.groupHex !== "string") return { accountId, groupHex: null };
  const wakeHex = wake.groupHex.toLowerCase();
  let route = null;
  try {
    route = await lookupRouteFor(accountId, wakeHex);
  } catch {
    route = null;
  }
  if (route !== null) {
    try {
      const bytes = base64Decode(route.groupIdB64);
      if (bytes.length === 32) return { accountId, groupHex: bytesToHex(bytes) };
    } catch {
      // A malformed row falls through to the legacy read below.
    }
  }
  let legacy = null;
  try {
    legacy = await resolveAccountFor(accountId, wakeHex);
  } catch {
    legacy = null;
  }
  if (legacy === null) return null;
  return { accountId, groupHex: wakeHex };
}

/** The `wake_binding` row, or `null`. READ-ONLY. */
async function readWakeBinding() {
  const row = await idbGet("lakko-push-routing", "wake_binding", "current");
  if (
    row === undefined ||
    typeof row.devicePseudonymHex !== "string" ||
    typeof row.accountId !== "string"
  ) {
    return null;
  }
  return { devicePseudonymHex: row.devicePseudonymHex, accountId: row.accountId };
}

/**
 * `(account, push_gid) → { accountId, groupIdB64 }` from the plaintext
 * `account_routes` index, or `null`. The row key is built exactly as
 * `routeRowKey` in `src/push/group-routing-store.ts` builds it; a row naming
 * another account is a miss. READ-ONLY.
 */
async function lookupRouteFor(accountId, pushGidHex) {
  const row = await idbGet(
    "lakko-push-routing",
    "account_routes",
    routeRowKey(accountId, pushGidHex),
  );
  if (
    row === undefined ||
    row.accountId !== accountId ||
    typeof row.groupIdB64 !== "string"
  ) {
    return null;
  }
  return { accountId: row.accountId, groupIdB64: row.groupIdB64 };
}

/** Mirror of `routeRowKey` in `src/push/group-routing-store.ts`. */
function routeRowKey(accountId, pushGidHex) {
  return `${accountId}\0${pushGidHex.toLowerCase()}`;
}

/**
 * LEGACY — does the bound account itself know `groupHex` as a raw group
 * id? Reads ONLY that account's snapshot row (`knownGroupIds` is stored
 * unencrypted), never another account's. READ-ONLY.
 */
async function resolveAccountFor(accountId, groupHex) {
  const row = await idbGet("lakko-mls", "snapshots", accountId);
  const known = Array.isArray(row?.knownGroupIds) ? row.knownGroupIds : [];
  for (const kg of known) {
    let bytes;
    try {
      bytes = base64Decode(kg);
    } catch {
      continue;
    }
    if (bytesToHex(bytes) === groupHex) {
      return { accountId, groupIdBytes: bytes };
    }
  }
  return null;
}

/** A 64-char hex pseudonym, lowercased, or `null`. */
function normalisedPseudonymHex(s) {
  if (typeof s !== "string" || s.length !== 64) return null;
  const lower = s.toLowerCase();
  return /^[0-9a-f]{64}$/.test(lower) ? lower : null;
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
  const deps = buildDecryptDeps(wasm, core);
  // ADR-0061 — the words the page stored, so an attachment-only preview
  // reads in the interface language.
  deps.notificationWords = await readNotificationStrings();
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
function buildDecryptDeps(wasm, core) {
  return {
    // ADR-0048 — the name this device already knows for a sender, from the
    // encrypted profile row the app wrote. READ-ONLY: the SW never stores a
    // profile and never looks a username up (C-59).
    lookupSenderName: async (accountId, deviceKey, senderKeyB64) => {
      const row = await idbGet("lakko-profiles", "rows", accountId);
      if (row === undefined || row.ciphertext === undefined) return null;
      try {
        const plain = await aesGcmUnwrap(deviceKey, row.ciphertext);
        const payload = JSON.parse(new TextDecoder().decode(plain));
        return core.senderNameFromProfileRow(payload, senderKeyB64);
      } catch {
        return null;
      }
    },
    // ADR-0042 (amended 2026-09-30) — the binding the core's gate reads.
    readWakeBinding,
    // ADR-0042 — reverse-map `(bound account, opaque push_gid)` back to
    // the base64 group_id. The `account_routes` store is PLAINTEXT by
    // design (push_gid is a one-way derivation; group_id is a public DS
    // routing address — see `src/push/group-routing-store.ts`) precisely
    // so this read needs no device key. READ-ONLY: the SW never writes
    // push-routing state. `null` on a miss → the core falls back to the
    // legacy raw-group_id read; a throw is treated as a miss by the core.
    lookupRoute: lookupRouteFor,
    // LEGACY fallback — does the BOUND account know `g` as a raw group id?
    // `knownGroupIds` is stored UNENCRYPTED, so this needs no device key.
    // Never searches another account's snapshot.
    resolveAccount: resolveAccountFor,
    // `docs/ux/calls.md` section 2.1 — is this conversation muted? READ-ONLY:
    // the encrypted cross-device sync row the app wrote
    // (`src/devices/sync/sync-state-idb.ts`), unwrapped under the same device
    // key the snapshot and the message log already use here. The SW never
    // writes it and never publishes a flag (C-59).
    //
    // FAILS TOWARD RINGING — `null` on every miss, and the core treats a
    // throw the same way. The muted set is the ONE read on this path whose
    // failure must not be fail-closed: a call silenced because IndexedDB did
    // not open is a call the person never learns about, which is far worse
    // than a ring they did not want.
    lookupMuted: async (accountId, deviceKey, groupHex) => {
      const row = await idbGet("lakko-device-sync", "rows", accountId);
      if (row === undefined || row.ciphertext === undefined) return null;
      try {
        const plain = await aesGcmUnwrap(deviceKey, row.ciphertext);
        const payload = JSON.parse(new TextDecoder().decode(plain));
        return core.mutedFromSyncStateRow(payload, groupHex, wasm);
      } catch {
        return null;
      }
    },
    // ADR-0065 R.5 — which conversation a group belongs to, from the same
    // row. READ-ONLY; `null` on every miss (the notification is then grouped
    // under the group id).
    lookupConversation: async (accountId, deviceKey, groupHex) => {
      const row = await idbGet("lakko-device-sync", "rows", accountId);
      if (row === undefined || row.ciphertext === undefined) return null;
      try {
        const plain = await aesGcmUnwrap(deviceKey, row.ciphertext);
        const payload = JSON.parse(new TextDecoder().decode(plain));
        return core.conversationHexFromSyncStateRow(payload, groupHex, wasm);
      } catch {
        return null;
      }
    },
    // ADR-0065 R.5 / R.8 — the account keys an accepted reset retired, from
    // the plaintext mirror the app writes (`src/push/superseded-mirror.ts`;
    // the pin store itself is `localStorage`, which a worker cannot read).
    // READ-ONLY (C-59). Every miss is "none retired": the wake is shown.
    lookupSupersededKeys: async (accountId) => {
      try {
        const row = await idbGet("lakko-superseded-keys", "rows", accountId);
        return Array.isArray(row?.keys) ? row.keys : null;
      } catch {
        return null;
      }
    },
    // ADR-0065 R.7 case 1 — is this a re-sent message whose original this
    // device already holds? READ-ONLY, from the two encrypted rows this
    // worker already reads: the message log (the held row and its author)
    // and the sync row (the lineage: which group is an older segment of
    // which conversation). Nothing is mirrored for it and nothing is
    // written (C-59). Every miss is "not held": the message is previewed.
    lookupResendHeld: async (accountId, deviceKey, question) => {
      const logRow = await idbGet("lakko-message-log", "logs", accountId);
      const syncRow = await idbGet("lakko-device-sync", "rows", accountId);
      if (logRow?.ciphertext === undefined || syncRow?.ciphertext === undefined) return false;
      try {
        const decode = async (ciphertext) =>
          JSON.parse(new TextDecoder().decode(await aesGcmUnwrap(deviceKey, ciphertext)));
        return core.resendHeldFromRows(
          await decode(logRow.ciphertext),
          await decode(syncRow.ciphertext),
          question,
          wasm,
        );
      } catch {
        return false;
      }
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
    //
    // The service path is `/ds/group/<hex>/messages` and the web prefix is
    // `/api/ds` (nginx and the Vite proxy strip it), so the URL is
    // `/api/ds/ds/group/...` exactly as `DsClient` builds it. This used to
    // request `/api/ds/group/...`, a route the DS does not have, so every
    // wake fell back to the generic notification.
    //
    // ADR-0053 — signed over the SERVICE path and the exact query string sent.
    dsFetchGroup: async (groupIdBytes, afterId, signProof) => {
      const hex = bytesToHex(groupIdBytes);
      const path = `/ds/group/${hex}/messages`;
      const query = `after=${afterId}&wait=0`;
      const headers = {};
      if (typeof signProof === "function") {
        headers["x-lakko-ds-proof"] = signProof("GET", path, query);
      }
      const url = `/api/ds${path}?${query}`;
      const resp = await fetch(url, { method: "GET", headers });
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

  // A ringing notification carries its own per-button meaning, decided at
  // PUSH time by `callNotificationIntents` and stored in `data`. Reading it
  // here is a plain lookup — no import, no decision — because this handler
  // must reach `clients.openWindow` with as few awaits as possible or the
  // browser withholds the user activation that permits it. The lookup mirrors
  // `intentForAction` in `public/sw-call-notification.js`, which is the
  // canonical definition and is what the tests pin; the two must stay in
  // step (the same convention `looksLikeCallSignaling` follows between
  // `src/chat/client.ts` and `sw-push-decrypt.js`).
  const callIntent =
    event.notification.data?.call?.intents?.[event.action ?? ""] ?? null;
  if (callIntent !== null) {
    event.waitUntil(handleCallClick(callIntent));
    return;
  }

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

/**
 * Act on a press of Accept / Decline, or a click on a ringing notification's
 * body.
 *
 * The worker CANNOT answer or decline a call itself: publishing either means
 * sealing an MLS application message, and this path is read-only by
 * construction (C-59 / ADR-0041). So every outcome routes to a page — the
 * question is only which page, and whether it has to be raised.
 *
 * `callClickPlan` decides that (it is pure and tested); this function is the
 * effects. The plan's three rules:
 *
 *   - **Decline** posts to whatever window exists and does NOT raise it.
 *     Refusing a call is a request to be left alone; opening the app to
 *     complete the refusal would be the opposite. Only with no window at all
 *     does a decline have to open one.
 *   - **Accept** posts and raises: answering means arriving in the call.
 *   - **Open** posts and raises without answering — the full-screen incoming
 *     call is mounted app-wide, so raising is enough to show who is calling.
 *
 * Posting happens BEFORE the focus, for the reason the tap path documents: a
 * `focus()` some browsers refuse without user activation must not swallow the
 * action itself.
 */
async function handleCallClick(intent) {
  let core = null;
  try {
    core = await loadDecryptModule();
  } catch {
    core = null;
  }

  let clients = [];
  try {
    clients = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
  } catch {
    clients = [];
  }
  const ours = clients.filter((c) => c.url.startsWith(self.location.origin));

  // Without the module the pure plan is unavailable; fall back to the shape
  // it would have produced for the commonest case rather than doing nothing.
  const plan =
    core !== null
      ? core.callClickPlan(intent, ours.length > 0)
      : {
          post: ours.length > 0,
          focus: ours.length > 0 && intent.kind !== "decline",
          openUrl: ours.length > 0 ? null : "/",
        };

  if (plan.post) {
    const message = {
      type: core !== null ? core.CALL_ACTION_MESSAGE : "LAKKO_CALL_ACTION",
      intent,
    };
    for (const c of ours) {
      try {
        c.postMessage(message);
      } catch {
        // The client went away between `matchAll` and here.
      }
    }
  }
  if (plan.focus) {
    for (const c of ours) {
      try {
        await c.focus();
        break;
      } catch {
        // Some browsers refuse `focus()` without user activation; the action
        // has already been posted, so the page still acts on it.
      }
    }
  }
  if (plan.openUrl !== null) {
    try {
      await self.clients.openWindow(plan.openUrl);
    } catch {
      // Nothing further we can do from here.
    }
  }
}

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
