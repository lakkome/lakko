// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// @ts-check

/**
 * Read-only throwaway MLS decrypt for the service-worker push path.
 *
 * The companion to `public/sw.js`. When a Web Push wake lands and NO
 * app tab is open+focused, `sw.js` builds the real (IDB / wasm / DS)
 * dependency object and calls {@link decryptPushPreview} to turn the
 * `(group_pseudonym, envelope_pointer)` wake hint into the REAL
 * decrypted text of the newest message, so the notification body says
 * what actually arrived instead of a content-free "New message".
 *
 * ## Why this file is a plain `.js` ESM in `public/`
 *
 * `public/sw.js` is copied VERBATIM into `dist/` — it is NOT part of
 * Vite's build graph, so it can neither `import` a `src/*.ts` module
 * nor resolve a hashed app chunk. A sibling `public/*.js` ESM is the
 * one shape that is reachable from BOTH sides:
 *
 *   * the module service worker `import('/sw-push-decrypt.js')`s it at
 *     push time (a stable, non-hashed dist URL), and
 *   * the vitest suite (`src/sw/push-decrypt.test.ts`) imports it
 *     directly and drives it with mock deps.
 *
 * Every capability it touches — IndexedDB reads, the wasm core, the DS
 * fetch — is INJECTED via the `deps` argument. The module holds no
 * browser globals at import scope, so it loads cleanly in Node for the
 * unit tests.
 *
 * ## Load-bearing correctness: READ-ONLY THROWAWAY (see sw.js too)
 *
 * This module MUST NOT corrupt the MLS ratchet state the foreground
 * app relies on. It:
 *
 *   1. reads the device key + the last-persisted MLS snapshot + the
 *      account identity seed (all READ-ONLY),
 *   2. restores a TEMPORARY in-memory `Group` from that snapshot,
 *   3. GETs the DS journal after a read-only cursor and processes the
 *      rows through the TEMP group ONLY to extract the newest message's
 *      plaintext preview,
 *   4. returns the preview and DISCARDS the temp group + identity.
 *
 * It NEVER persists the advanced snapshot, NEVER advances the app's DS
 * cursor, NEVER writes any MLS state. `deps` deliberately exposes NO
 * write capability — the forbidden `saveSnapshot` / `saveMessageLog` /
 * `advanceCursor` hooks in {@link DecryptPushDeps} exist ONLY so the
 * test can assert they are never called (a regression guard). Were the
 * SW to persist the advanced state, the foreground app — which restores
 * from the SAME last-persisted snapshot and does the real durable
 * processing from its OWN cursor — would desync its ratchet (the G-34
 * nonce-reuse-on-crash class). The DS journal is append-only + cursor-
 * based, so the app re-processing from its own cursor is idempotent.
 *
 * ## Fail-closed (security invariant #4)
 *
 * A frame that will not decrypt / authenticate is DROPPED, never shown
 * as a plaintext guess. Any missing input (no account match, no device
 * key, no seed, no snapshot, DS failure, zero decryptable frames)
 * returns `null`, and the SW falls back to the generic notification —
 * it never throws out of the push handler.
 */

/** Max characters of decrypted text shown in the notification body. */
const PREVIEW_MAX_CHARS = 140;

/**
 * @typedef {object} PushWake
 * @property {string | null} deviceHex   Device pseudonym (unused here — routing only).
 * @property {string | null} groupHex    64 lowercase-hex chars. Post-ADR-0042
 *   (Sprint 3) this is the OPAQUE 32-byte `push_gid`; a legacy wake carries
 *   the raw 32-byte group_id. Either way the shape is identical — the
 *   `lookupRoute` reverse-map (Sprint 4) disambiguates.
 * @property {number | null} pointer     DS message id of the message that triggered the wake.
 */

/**
 * @typedef {object} ResolvedAccount
 * @property {string}     accountId      base64(account_id) — the IDB row key.
 * @property {Uint8Array} groupIdBytes   raw 32-byte group_id (hex-decoded `groupHex`).
 */

/**
 * @typedef {object} LoadedSnapshot
 * @property {Uint8Array}       snapshotBytes  decrypted MLS provider snapshot.
 * @property {readonly string[]} knownGroupIds base64 group_ids the snapshot carries.
 */

/**
 * The minimal wasm surface this module drives. In production it is the
 * loaded `lakko_ffi_wasm` namespace; in tests it is a hand-mock.
 * @typedef {object} DecryptWasm
 * @property {{ fromSeed: (seed: Uint8Array) => any }} Identity
 * @property {{ load: (identity: any, groupId: Uint8Array) => any }} Group
 * @property {(bytes: Uint8Array) => any} parseBodyEnvelope
 */

/**
 * Injected capabilities. Every one is a READ or a pure compute — there
 * is intentionally no write path. `saveSnapshot` / `saveMessageLog` /
 * `advanceCursor` are the FORBIDDEN hooks: production never wires them;
 * the test wires spies and asserts zero calls.
 *
 * @typedef {object} DecryptPushDeps
 * @property {(pushGidHex: string) => Promise<{ accountId: string, groupIdB64: string } | null>} [lookupRoute]
 *   ADR-0042 Sprint 4 — READ-ONLY reverse-map of an opaque `push_gid`
 *   (lowercase hex) to the owning `(accountId, base64 group_id)` via the
 *   plaintext `lakko-push-routing`/`routes` IDB index. `null` on a miss
 *   (legacy wake / not-yet-provisioned group). Optional so older deps
 *   objects keep working; any throw is treated as a miss (fail-closed).
 * @property {(groupHex: string) => Promise<ResolvedAccount | null>} resolveAccount
 *   Match `groupHex` to a locally-stored account+group (returns null if none).
 *   The LEGACY fallback path — runs only when `lookupRoute` misses.
 * @property {(accountId: string) => Promise<any | null>} getDeviceKey
 *   The non-extractable AES-GCM device root CryptoKey, or null.
 * @property {(accountId: string, deviceKey: any) => Promise<Uint8Array | null>} getIdentitySeed
 *   The 64-byte AccountIdentity (BIP39) seed, decrypted under the device key, or null.
 * @property {(accountId: string, deviceKey: any) => Promise<LoadedSnapshot | null>} loadSnapshot
 *   The decrypted MLS provider snapshot + knownGroupIds, or null.
 * @property {DecryptWasm} wasm
 *   The loaded wasm core (or a mock).
 * @property {(groupIdBytes: Uint8Array, afterId: number) => Promise<{ messages: Array<{ id: number, payload: Uint8Array, createdAt: string }> }>} dsFetchGroup
 *   READ-ONLY DS group-journal fetch of rows strictly after `afterId`.
 * @property {(accountId: string, groupHex: string) => Promise<number | null>} [startCursor]
 *   The app's last-persisted per-group cursor (READ-ONLY), if available. Used as
 *   the fetch start so intervening commits are replayed and the epoch stays right.
 * @property {(...args: any[]) => any} [saveSnapshot]     FORBIDDEN — never called (test guard).
 * @property {(...args: any[]) => any} [saveMessageLog]   FORBIDDEN — never called (test guard).
 * @property {(...args: any[]) => any} [advanceCursor]    FORBIDDEN — never called (test guard).
 */

/**
 * @typedef {object} PushPreview
 * @property {string} title  Notification title.
 * @property {string} body   Decrypted preview body (truncated).
 * @property {string} tag    Notification tag (the group hex — collapses a burst).
 */

/**
 * Produce a real decrypted notification preview for a GroupMessage
 * wake, or `null` when the background path cannot (safely) yield one —
 * in which case the SW shows its generic fallback.
 *
 * Never throws for a "cannot decrypt" reason; every such path returns
 * `null`. (A programming error in a mock could still throw in tests —
 * the SW wraps the whole call in try/catch regardless.)
 *
 * @param {PushWake | null} wake
 * @param {DecryptPushDeps} deps
 * @returns {Promise<PushPreview | null>}
 */
export async function decryptPushPreview(wake, deps) {
  // 1. Only GroupMessage wakes carry a group + pointer. A Wakeup /
  //    MailboxMessage wake (no groupHex / no pointer) is not handled in
  //    this slice — fall back to generic.
  if (
    wake === null ||
    wake === undefined ||
    typeof wake.groupHex !== "string" ||
    wake.groupHex.length !== 64 ||
    !isHex(wake.groupHex) ||
    typeof wake.pointer !== "number" ||
    !Number.isFinite(wake.pointer)
  ) {
    return null;
  }
  const wakeHex = wake.groupHex;
  const pointer = wake.pointer;

  // 2. Resolve the wake's `g` to the owning account + REAL group_id.
  //    Post-ADR-0042 (Sprint 3) notify routes under the OPAQUE
  //    `push_gid`, so FIRST reverse-map it through the plaintext
  //    routes index (Sprint 4). Everything downstream (snapshot load,
  //    Group.load, DS fetch, cursor, tag) then uses the REAL group_id.
  //    A miss — a legacy raw-group_id wake or a group whose route this
  //    device has not stored yet — falls back to the original
  //    treat-`g`-as-group_id path, and ANY lookup error degrades the
  //    same way (fail-closed: worst case is the generic notification).
  //    The lookup is a READ; this path never writes push-routing state.
  let accountId = null;
  let groupIdBytes = null;
  let groupHex = wakeHex; // becomes the REAL group hex on a route hit
  if (typeof deps.lookupRoute === "function") {
    let route = null;
    try {
      route = await deps.lookupRoute(wakeHex);
    } catch {
      route = null; // lookup error → legacy path → generic at worst
    }
    if (
      route !== null &&
      route !== undefined &&
      typeof route.accountId === "string" &&
      typeof route.groupIdB64 === "string"
    ) {
      const bytes = base64DecodeQuietly(route.groupIdB64);
      if (bytes !== null && bytes.length === 32) {
        accountId = route.accountId;
        groupIdBytes = bytes;
        groupHex = bytesToHex(bytes);
      }
      // A malformed route row falls through to the legacy path below.
    }
  }

  //    Legacy fallback: treat `g` as the raw group_id hex. (Also
  //    validates it is a group we actually know — a stray wake → null.)
  if (accountId === null) {
    const account = await deps.resolveAccount(wakeHex);
    if (account === null || account === undefined) return null;
    accountId = account.accountId;
    groupIdBytes = account.groupIdBytes;
    groupHex = wakeHex;
  }

  // 3. The device root key (a non-extractable CryptoKey handle usable
  //    for AES-GCM decrypt without extraction). No key → can't unwrap
  //    the seed/snapshot → generic.
  const deviceKey = await deps.getDeviceKey(accountId);
  if (deviceKey === null || deviceKey === undefined) return null;

  // 4. The AccountIdentity seed → the throwaway Identity.
  const identitySeed = await deps.getIdentitySeed(accountId, deviceKey);
  if (identitySeed === null || identitySeed === undefined) return null;

  // 5. The last-persisted MLS provider snapshot (the ratchet state the
  //    foreground app relies on — we only READ it).
  const snapshot = await deps.loadSnapshot(accountId, deviceKey);
  if (snapshot === null || snapshot === undefined) return null;

  // 6. Build a TEMPORARY, in-memory Identity + Group from the snapshot.
  //    Everything from here is throwaway — never snapshotted back.
  let identity;
  let group;
  try {
    identity = deps.wasm.Identity.fromSeed(identitySeed);
    identity.restoreStorage(snapshot.snapshotBytes);
    group = deps.wasm.Group.load(identity, groupIdBytes);
  } catch {
    freeQuietly(group);
    freeQuietly(identity);
    return null;
  }
  if (group === null || group === undefined) {
    freeQuietly(identity);
    return null;
  }

  try {
    // 7. Fetch DS rows after a READ-ONLY start cursor. Prefer the app's
    //    persisted per-group cursor (so intervening commits since the
    //    snapshot are replayed and the epoch stays correct); otherwise
    //    just the message that triggered the wake (`pointer - 1`).
    let after = pointer - 1;
    if (deps.startCursor !== undefined) {
      let persisted = null;
      try {
        persisted = await deps.startCursor(accountId, groupHex);
      } catch {
        persisted = null;
      }
      if (
        persisted !== null &&
        persisted !== undefined &&
        Number.isFinite(persisted) &&
        persisted < pointer
      ) {
        after = persisted;
      }
    }
    if (after < 0) after = 0;

    let fetched;
    try {
      fetched = await deps.dsFetchGroup(groupIdBytes, after);
    } catch {
      return null;
    }
    const messages = fetched?.messages ?? [];

    // 8. Process each row through the TEMP group and keep the newest
    //    frame that yields a user-visible preview. A frame that will
    //    not decrypt/authenticate (wrong epoch, our own echo, a control
    //    frame, a role-refused commit) is skipped — never guessed.
    /** @type {PushPreview | null} */
    let latest = null;
    for (const message of messages) {
      let processed;
      try {
        processed = group.processIncomingVerifyingRoles(identity, message.payload);
      } catch {
        // Bad frame / wrong epoch / own echo / role-refused commit —
        // fail-closed skip. NOTE: this mutates the THROWAWAY provider
        // only; it is never persisted (read-only-throwaway discipline).
        continue;
      }
      const preview = previewFromProcessed(deps.wasm, processed);
      if (preview !== null) {
        latest = { title: "Lakko", body: preview, tag: groupHex };
      }
    }
    return latest;
  } finally {
    // Discard the throwaway MLS state. Freeing is best-effort — the GC
    // reclaims it regardless; we NEVER snapshot it back to IDB.
    freeQuietly(group);
    freeQuietly(identity);
  }
}

/**
 * Extract a user-visible preview string from a processed MLS message,
 * or `null` for anything that is not a renderable chat/edit body.
 *
 * @param {DecryptWasm} wasm
 * @param {any} processed
 * @returns {string | null}
 */
function previewFromProcessed(wasm, processed) {
  if (
    processed === null ||
    processed === undefined ||
    processed.kind !== "Application" ||
    processed.applicationData === undefined ||
    processed.applicationData === null
  ) {
    return null;
  }
  let parsed;
  try {
    parsed = wasm.parseBodyEnvelope(processed.applicationData);
  } catch {
    return null;
  }
  try {
    const kind = parsed.kind;
    // A normal chat message → its text (or an attachment hint).
    if (kind === "chat") {
      const text = typeof parsed.text === "string" ? parsed.text : "";
      if (text.length > 0) return truncate(text);
      const count = typeof parsed.attachmentCount === "number" ? parsed.attachmentCount : 0;
      if (count > 0) return count === 1 ? "Sent an attachment" : `Sent ${count} attachments`;
      return null;
    }
    // An edit rewrites a prior message → surface the new text.
    if (kind === "edit") {
      const text = typeof parsed.text === "string" ? parsed.text : "";
      return text.length > 0 ? truncate(text) : null;
    }
    // delete / reaction / typing / read-receipt carry no standalone
    // preview — skip so an older real message stays the preview.
    return null;
  } finally {
    freeQuietly(parsed);
  }
}

/** Truncate a preview body to a sane notification length. */
function truncate(text) {
  if (text.length <= PREVIEW_MAX_CHARS) return text;
  return `${text.slice(0, PREVIEW_MAX_CHARS - 1)}…`;
}

/** `true` when every char of `s` is a lowercase hex digit. */
function isHex(s) {
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    const isDigit = c >= 48 && c <= 57; // 0-9
    const isLower = c >= 97 && c <= 102; // a-f
    if (!isDigit && !isLower) return false;
  }
  return true;
}

/** Lowercase-hex encode (matches `sw.js` / `ds/client.ts`). */
function bytesToHex(bytes) {
  let out = "";
  for (let i = 0; i < bytes.length; i++) {
    out += (bytes[i] ?? 0).toString(16).padStart(2, "0");
  }
  return out;
}

/**
 * Standard-alphabet base64 → bytes, or `null` on any decode failure
 * (a malformed route row must degrade to the legacy path, never throw).
 * `atob` exists in both the SW global scope and Node (tests).
 *
 * @param {string} b64
 * @returns {Uint8Array | null}
 */
function base64DecodeQuietly(b64) {
  try {
    const binary = atob(b64);
    const out = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

/** Free a wasm handle if it exposes `.free()`; ignore anything else. */
function freeQuietly(handle) {
  if (handle !== null && handle !== undefined && typeof handle.free === "function") {
    try {
      handle.free();
    } catch {
      // Best-effort — a double-free / mock without free is harmless.
    }
  }
}
