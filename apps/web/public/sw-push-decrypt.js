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

/**
 * The ringing-notification shape and its button intents, re-exported so
 * `public/sw.js` reaches them through the ONE dynamic import it already
 * performs at push time rather than a second one.
 */
export {
  callRingNotification,
  callNotificationTag,
  isCallNotificationTag,
  callClickPlan,
  callIntentUrl,
  intentForAction,
  CALL_ACTION_MESSAGE,
} from "./sw-call-notification.js";

/** Max characters of decrypted text shown in the notification body. */
const PREVIEW_MAX_CHARS = 140;

/**
 * An offer whose DS row is older than this does not ring.
 *
 * Mirrors `STALE_OFFER_MS` in `src/calls/call-signaling-dispatch.ts`, which
 * this file cannot import (see the header on why it lives in `public/`), and
 * exists for the same reason: a wake can be delivered late, or replayed to a
 * device that was offline, and a phone must not start ringing for a call that
 * ended minutes ago. Judged on the DS row's SERVER timestamp, never on
 * anything the caller stamped.
 */
const STALE_OFFER_MS = 60_000;

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
 * @property {(bytes: Uint8Array) => any} [decodePushRoutingSecret]
 *   ADR-0042 / ADR-0044 — the fail-closed push-routing-secret envelope
 *   codec. THROWS when the bytes are not that envelope. Used only to
 *   classify a service envelope out of the preview path (never to act on
 *   the secret — the SW writes no routing state). Optional so an older
 *   deps object still loads; the cheap byte peek then decides alone.
 * @property {(bytes: Uint8Array) => void} [decodeCommitterBeacon]
 *   ADR-0045 — the fail-closed committer-liveness-beacon codec. THROWS
 *   when the bytes are not that envelope; returning is the whole result
 *   (the beacon is field-less). Used ONLY to classify the row out of the
 *   preview path — the SW never emits a beacon and never folds a
 *   committer verdict, which it structurally cannot (C-59). Optional so
 *   an older deps object still loads.
 * @property {(bytes: Uint8Array) => any} [decodeProfileEnvelope]
 *   ADR-0048 — the fail-closed profile-envelope codec. THROWS an `Error`
 *   carrying a `profileError` code when the bytes are not a well-formed
 *   profile; `wrongDomain` is the only "not a profile" answer. Used ONLY to
 *   classify the row out of the preview path — the SW never stores a
 *   received profile (C-59). Optional so an older deps object still loads.
 * @property {(bytes: Uint8Array) => any} [decodeGroupNameEnvelope]
 *   ADR-0051 — the fail-closed group-name-envelope codec. THROWS an `Error`
 *   carrying a `groupNameError` code when the bytes are not a well-formed
 *   group name; `wrongDomain` is the only "not a group name" answer. Used
 *   ONLY to classify the row out of the preview path — the SW never stores
 *   a received name (C-59). Optional so an older deps object still loads.
 * @property {(bytes: Uint8Array) => any} [parseCallSignaling]
 *   Phase 3a3 — the prost `CallSignalingPayload` codec. THROWS on bytes that
 *   are not one. Used to tell an OFFER (ring) and an END (stop ringing) apart
 *   from the answer / candidate / group-control frames, which stay
 *   suppressed. Classification ONLY: the worker reads the call id and the
 *   media flags to shape a notification and never applies, answers or stores
 *   anything (C-59). Optional — absent means every call frame is suppressed,
 *   i.e. exactly the pre-ring behaviour.
 * @property {() => number} [committerBeaconEnvelopeLen]
 *   ADR-0045 — the exact on-wire length of a beacon envelope, read from
 *   the FFI rather than hand-copied into each client (the drift class
 *   ADR-0044 documents). Optional; absent means the peek skips the
 *   length test and leans on the codec alone.
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
 * @property {(accountId: string, deviceKey: any, senderKeyB64: string) => Promise<string | null>} [lookupSenderName]
 *   ADR-0048 — READ-ONLY: the name this device ALREADY resolves for a sender
 *   (local rename, profile name, verified username), from the encrypted
 *   profile row the app wrote. `null` when nothing is known. Never fetches or
 *   verifies anything. Optional; any throw is treated as unknown.
 * @property {() => number} [nowMs]
 *   Wall clock, for the stale-offer gate. Defaults to `Date.now()`; injected
 *   only so the tests can place a row on either side of the window without
 *   sleeping. A READ of the clock, nothing else.
 * @property {(...args: any[]) => any} [saveSnapshot]     FORBIDDEN — never called (test guard).
 * @property {(...args: any[]) => any} [saveMessageLog]   FORBIDDEN — never called (test guard).
 * @property {(...args: any[]) => any} [advanceCursor]    FORBIDDEN — never called (test guard).
 */

/**
 * A call this wake's rows announced, when one did.
 *
 * `"ring"` is an offer the device should ring for; `"end"` is that same call
 * being cancelled, declined or hung up, and asks the worker to take any ring
 * for it down. Only ever a classification — the worker never answers, never
 * publishes, never stores (C-59).
 *
 * @typedef {object} CallNotice
 * @property {"ring" | "end"} kind
 * @property {string} callIdHex  Lowercase hex of the 16-byte call id.
 * @property {string} groupHex   REAL DS group hex, for the deep link.
 * @property {boolean} video     The kind the CALLER chose, off the offer's own
 *   media flags. Meaningless for `"end"`.
 * @property {string | null} callerName  A name this device already resolves for
 *   the sender, or `null`. Never looked up, never verified here.
 */

/**
 * @typedef {object} PushPreview
 * @property {string} title  Notification title.
 * @property {string} body   Decrypted preview body (truncated).
 * @property {string} tag    Notification tag (the group hex — collapses a burst).
 * @property {CallNotice} [call]  Present ONLY when the newest relevant row was
 *   call signalling. A message preview carries no `call` key at all, so the
 *   message shape is byte-for-byte what it was before calls could ring.
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
    //
    //    Call signalling is classified rather than dropped (it used to be
    //    suppressed wholesale, which is why a closed tab never rang): an
    //    offer becomes a ring, an end cancels one, and the four other
    //    bodies stay silent. `latestCall` keeps the LAST such row of the
    //    batch, so an end that arrives after its own offer — the ordinary
    //    cancelled-call shape, and both rows can land in one wake — wins
    //    and the device does not start ringing for a call that is over.
    /** @type {PushPreview | null} */
    let latest = null;
    /** @type {string | null} */
    let latestSenderKeyB64 = null;
    /** @type {{ notice: CallNotice, senderKeyB64: string | null } | null} */
    let latestCall = null;
    // One reading for the whole batch, so two rows a millisecond apart cannot
    // be judged stale differently.
    const now = typeof deps.nowMs === "function" ? deps.nowMs() : Date.now();
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
      const call = callNoticeFrom(deps.wasm, processed, message, groupHex, now);
      if (call !== null) {
        latestCall = { notice: call, senderKeyB64: senderKeyOf(group, processed) };
        continue;
      }
      const preview = previewFromProcessed(deps.wasm, processed);
      if (preview !== null) {
        latest = { title: "Lakko", body: preview, tag: groupHex };
        // ADR-0048 / C-68 — resolve the sender's KEY now, against the roster
        // of the epoch this frame decrypted at; a leaf resolved later could
        // name whoever took the slot.
        latestSenderKeyB64 = senderKeyOf(group, processed);
      }
    }

    // A call outranks a message preview: one is an interruption the user is
    // expected to answer within seconds, the other waits. The ring's title is
    // the caller's name by the same read the message path uses.
    if (latestCall !== null) {
      let callerName = null;
      if (
        latestCall.senderKeyB64 !== null &&
        typeof deps.lookupSenderName === "function"
      ) {
        try {
          callerName = await deps.lookupSenderName(
            accountId,
            deviceKey,
            latestCall.senderKeyB64,
          );
        } catch {
          callerName = null;
        }
      }
      const notice = {
        ...latestCall.notice,
        callerName: typeof callerName === "string" && callerName.length > 0 ? callerName : null,
      };
      // `title` / `body` / `tag` are carried for a caller that ignores `call`
      // entirely; `public/sw.js` reads `call` and builds the real ring shape
      // through `callRingNotification`.
      return {
        title: notice.callerName ?? "Lakko",
        body: notice.video ? "Video call" : "Voice call",
        tag: groupHex,
        call: notice,
      };
    }

    // ADR-0048 UX §5 — title the notification with a name this device
    // already knows. A read of what the app stored; never a lookup.
    if (
      latest !== null &&
      latestSenderKeyB64 !== null &&
      typeof deps.lookupSenderName === "function"
    ) {
      let name = null;
      try {
        name = await deps.lookupSenderName(accountId, deviceKey, latestSenderKeyB64);
      } catch {
        name = null;
      }
      if (typeof name === "string" && name.length > 0) {
        latest = { ...latest, title: name };
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
 * Total length (bytes) of an ADR-0044 push-routing-secret distribution
 * envelope: a 29-byte domain separator + 1 version byte + the 8-byte
 * big-endian `mint_epoch` + the 32-byte secret. Mirrors
 * `PUSH_ROUTING_ENVELOPE_LEN` in `src/push/group-routing.ts` (which this
 * file cannot import — see the header note on why it lives in `public/`).
 * Only a cheap pre-filter; the wasm codec is the authoritative gate.
 */
const PUSH_ROUTING_ENVELOPE_LEN = 70;

/**
 * First byte of that envelope's domain separator (`'l'` of
 * `"lakko/push-routing-secret/v1\0"`). A chat envelope is JSON and starts
 * with `{` (0x7B), so the sets cannot overlap.
 */
const PUSH_ROUTING_ENVELOPE_FIRST_BYTE = 0x6c;

/**
 * First byte of the ADR-0045 committer-beacon domain separator
 * (`'l'` of `"lakko/committer-beacon/v1\0"`). The same `'l'` every
 * in-group envelope domain starts with — deliberately NOT shared with
 * the constant above, so retiring one wire format cannot silently
 * re-point the other's peek. A chat envelope is JSON and starts with
 * `{` (0x7B), so again no overlap.
 *
 * The beacon's LENGTH is deliberately not a constant here: it comes
 * from `wasm.committerBeaconEnvelopeLen()`, which the FFI exports for
 * exactly this reason.
 */
const COMMITTER_BEACON_FIRST_BYTE = 0x6c;

/**
 * Cheap discriminator: are these bytes a prost-encoded
 * `CallSignalingPayload`? Mirrors `looksLikeCallSignaling` in
 * `src/chat/client.ts` — the two tables must stay in step. `true` when
 * the leading bytes are one of the `body` oneof field tags
 * (wire type 2, `(field << 3) | 2`):
 *
 *   field 10 (offer)       → 0x52
 *   field 11 (answer)      → 0x5A
 *   field 12 (candidate)   → 0x62
 *   field 13 (end)         → 0x6A
 *   field 14 (group_start) → 0x72
 *   field 15 (group_join)  → 0x7A
 *   field 16 (group_leave) → 0x82 0x01 (tag 130, a two-byte varint)
 *
 * Without the three group-control arms a group-call start / join /
 * leave fell through to `parseBodyEnvelope`'s lossy-UTF-8 fallback and
 * surfaced as a notification of binary garbage.
 *
 * Chat envelopes are JSON and start with `{` (0x7B) — no overlap.
 *
 * @param {Uint8Array} bytes
 * @returns {boolean}
 */
function looksLikeCallSignaling(bytes) {
  if (bytes.length === 0) return false;
  const first = bytes[0];
  if (
    first === 0x52 ||
    first === 0x5a ||
    first === 0x62 ||
    first === 0x6a ||
    first === 0x72 ||
    first === 0x7a
  ) {
    return true;
  }
  return first === 0x82 && bytes.length > 1 && bytes[1] === 0x01;
}

/**
 * Classify a processed row as a call this device should ring for, or stop
 * ringing for, or `null` for everything else.
 *
 * ## What changed, and why it is a branch rather than a filter
 *
 * Call signalling used to be suppressed outright by {@link isServiceEnvelope}
 * — correct for the preview path, because a raw prost frame rendered through
 * `parseBodyEnvelope`'s lossy-UTF-8 fallback is binary garbage in a
 * notification body, but it also meant a closed or backgrounded tab never
 * learned a call was happening (`docs/ux/calls.md` §2.1, §7). The frames are
 * now classified instead: an offer rings, an end cancels, and answer,
 * candidate and the three group-control bodies stay exactly as silent as
 * they were. `isServiceEnvelope` is untouched, so any call frame that gets
 * past this function still cannot render as text.
 *
 * ## Fail-closed, in the ringing direction
 *
 * Every uncertainty resolves to "do not ring": no codec, a frame that will
 * not parse, a call id that is not usable as a tag, an unreadable or missing
 * DS timestamp, an offer older than {@link STALE_OFFER_MS}. A missed ring
 * degrades to the pre-existing behaviour (the callee sees the call on its
 * next foreground poll); a spurious ring would wake someone for a call that
 * is not there, which is worse.
 *
 * An END is deliberately NOT stale-gated: cancelling a ring that is no longer
 * showing is a no-op, and refusing a late end would leave a stuck ring on
 * screen — which is the one outcome `requireInteraction: true` makes
 * permanent.
 *
 * Classification ONLY: nothing is applied, answered, persisted, or
 * cursor-advanced (C-59 / ADR-0041).
 *
 * @param {DecryptWasm} wasm
 * @param {any} processed
 * @param {{ createdAt?: string }} message
 * @param {string} groupHex
 * @param {number} nowMs
 * @returns {CallNotice | null}
 */
function callNoticeFrom(wasm, processed, message, groupHex, nowMs) {
  if (
    processed === null ||
    processed === undefined ||
    processed.kind !== "Application" ||
    !(processed.applicationData instanceof Uint8Array)
  ) {
    return null;
  }
  const bytes = processed.applicationData;
  if (!looksLikeCallSignaling(bytes)) return null;
  if (typeof wasm.parseCallSignaling !== "function") return null;

  let parsed;
  try {
    parsed = wasm.parseCallSignaling(bytes);
  } catch {
    return null;
  }
  try {
    const kind = parsed?.kind;
    if (kind !== "offer" && kind !== "end") return null;
    const callId = parsed.callId;
    if (!(callId instanceof Uint8Array) || callId.length === 0) return null;
    const callIdHex = bytesToHex(callId);
    if (kind === "end") {
      return { kind: "end", callIdHex, groupHex, video: false, callerName: null };
    }
    // The DS row's SERVER timestamp, never `parsed.createdAtMs`, which the
    // caller stamped and could be anything.
    const createdAt = message?.createdAt;
    if (typeof createdAt !== "string") return null;
    const createdMs = Date.parse(createdAt);
    if (Number.isNaN(createdMs)) return null;
    if (nowMs - createdMs > STALE_OFFER_MS) return null;
    return {
      kind: "ring",
      callIdHex,
      groupHex,
      video: parsed.mediaVideo === true,
      callerName: null,
    };
  } catch {
    return null;
  } finally {
    freeQuietly(parsed);
  }
}

/**
 * Is this Application plaintext a SERVICE envelope rather than a
 * user-visible message?
 *
 * Load-bearing: `parseBodyEnvelope` degrades non-JSON bytes to a `chat`
 * body via `String::from_utf8_lossy` (a deliberate pre-1.19 wire-compat
 * fallback in `lakko-attachments`), so WITHOUT this filter a service
 * envelope surfaces as a notification containing its domain string plus
 * raw binary. The foreground (`src/chat/client.ts`) already sniffs these
 * kinds before its chat parse; this is the background mirror of that
 * same two-step peek, in the same order.
 *
 * Five kinds, each its own arm below: call signaling, the ADR-0042 /
 * ADR-0044 push-routing secret, the ADR-0045 committer beacon, the
 * ADR-0048 profile and the ADR-0051 group name. Each arm is a cheap byte
 * peek gating an authoritative codec.
 *
 * Classification ONLY — no writes, no cursor advance (C-59 / ADR-0041).
 *
 * @param {DecryptWasm} wasm
 * @param {Uint8Array} bytes
 * @returns {boolean}
 */
function isServiceEnvelope(wasm, bytes) {
  // 1. Call signaling (prost `CallSignalingPayload`) — a pure byte check.
  //    Still suppressed HERE, and that is still right: a prost frame has no
  //    text to preview. What changed is that `callNoticeFrom` runs BEFORE
  //    this and may have already turned the frame into a ring; anything it
  //    declined (an answer, a candidate, a group-control body, a stale
  //    offer) falls through to here and stays silent, as it always did.
  if (looksLikeCallSignaling(bytes)) return true;
  // 2. Push-routing secret (ADR-0042 / ADR-0044).
  if (isPushRoutingSecretEnvelope(wasm, bytes)) return true;
  // 3. Committer-liveness beacon (ADR-0045).
  if (isCommitterBeaconEnvelope(wasm, bytes)) return true;
  // 4. Profile (ADR-0048).
  if (isProfileEnvelope(wasm, bytes)) return true;
  // 5. Group name (ADR-0051).
  if (isGroupNameEnvelope(wasm, bytes)) return true;
  return false;
}

/**
 * First byte of the ADR-0051 group-name domain separator (`'l'` of
 * `"lakko/group-name/v1\0"`). Its own constant, like the ones above.
 */
const GROUP_NAME_ENVELOPE_FIRST_BYTE = 0x6c;

/**
 * Arm 5 of {@link isServiceEnvelope} — the ADR-0051 group-name envelope.
 *
 * Unfiltered, a rename would be previewed as a notification body reading
 * `lakko/group-name/v1` plus the stamp bytes and the name. Same verdict rule
 * as the profile arm: a decode that succeeds is a group name; a throw with
 * any `groupNameError` code but `wrongDomain` is a domain match refused as
 * a whole, suppressed here too; `wrongDomain` or a code-less throw falls
 * through to the normal parse.
 *
 * The name is never kept: classification ONLY (C-59).
 *
 * @param {DecryptWasm} wasm
 * @param {Uint8Array} bytes
 * @returns {boolean}
 */
function isGroupNameEnvelope(wasm, bytes) {
  if (bytes.length === 0 || bytes[0] !== GROUP_NAME_ENVELOPE_FIRST_BYTE) return false;
  if (typeof wasm.decodeGroupNameEnvelope !== "function") return false;
  let decoded;
  try {
    decoded = wasm.decodeGroupNameEnvelope(bytes);
  } catch (err) {
    const code =
      err !== null && typeof err === "object" ? /** @type {any} */ (err).groupNameError : undefined;
    return typeof code === "string" && code !== "wrongDomain";
  }
  freeQuietly(decoded);
  return true;
}

/**
 * First byte of the ADR-0048 profile domain separator (`'l'` of
 * `"lakko/profile/v1\0"`). Deliberately its own constant, like the two above.
 */
const PROFILE_ENVELOPE_FIRST_BYTE = 0x6c;

/**
 * Arm 4 of {@link isServiceEnvelope} — the ADR-0048 profile envelope.
 *
 * Unfiltered, the lossy-UTF-8 fallback would put the domain string, the
 * sender's name and username into a notification body.
 *
 * The verdict is the codec's, read through its stable `profileError` code
 * rather than a hand-copied domain: a decode that SUCCEEDS is a profile, and a
 * decode that throws anything but `wrongDomain` is a domain match with a body
 * out of bounds — which the core refuses as a whole, and which is suppressed
 * here too rather than rendered. A throw with no code, or `wrongDomain`, is
 * not a profile and falls through to the normal parse.
 *
 * The profile is never kept: classification ONLY (C-59).
 *
 * @param {DecryptWasm} wasm
 * @param {Uint8Array} bytes
 * @returns {boolean}
 */
function isProfileEnvelope(wasm, bytes) {
  if (bytes.length === 0 || bytes[0] !== PROFILE_ENVELOPE_FIRST_BYTE) return false;
  if (typeof wasm.decodeProfileEnvelope !== "function") return false;
  let decoded;
  try {
    decoded = wasm.decodeProfileEnvelope(bytes);
  } catch (err) {
    const code =
      err !== null && typeof err === "object" ? /** @type {any} */ (err).profileError : undefined;
    return typeof code === "string" && code !== "wrongDomain";
  }
  freeQuietly(decoded);
  return true;
}

/**
 * ADR-0048 / C-68 — the base64 signature key of a processed frame's sender,
 * read from the throwaway group's roster, or `null`.
 *
 * @param {any} group
 * @param {any} processed
 * @returns {string | null}
 */
function senderKeyOf(group, processed) {
  const leaf = processed?.senderLeaf;
  if (typeof leaf !== "number") return null;
  let members;
  try {
    members = group.members();
  } catch {
    return null;
  }
  if (!Array.isArray(members)) return null;
  for (const m of members) {
    if (m?.leafIndex === leaf && m.signaturePublicKey instanceof Uint8Array) {
      return bytesToBase64(m.signaturePublicKey);
    }
  }
  return null;
}

/**
 * ADR-0048 §5 — the name a notification may show for `senderKeyB64`, from the
 * decrypted profile-row payload the app wrote (`src/profile/profile-idb.ts`),
 * or `null`. The same precedence as `src/profile/resolver.ts` minus its last
 * step: local rename → profile name → VERIFIED username. There is no
 * fingerprint fallback here — an unknown sender keeps the generic title.
 *
 * Pure; exported for `sw.js` and the tests.
 *
 * @param {any} payload
 * @param {string} senderKeyB64
 * @returns {string | null}
 */
export function senderNameFromProfileRow(payload, senderKeyB64) {
  if (payload === null || typeof payload !== "object" || payload.v !== 1) return null;
  const rename = payload.r?.[senderKeyB64];
  if (typeof rename === "string" && rename.trim().length > 0) return rename.trim();
  const profile = payload.p?.[senderKeyB64];
  if (profile === null || typeof profile !== "object") return null;
  if (typeof profile.d === "string" && profile.d.length > 0) return profile.d;
  if (typeof profile.u === "string" && profile.u.length > 0) {
    const verdict = payload.vf?.[`${senderKeyB64}|${profile.u}`];
    if (verdict !== null && typeof verdict === "object" && verdict.s === "verified") {
      return `@${profile.u}`;
    }
  }
  return null;
}

/**
 * Arm 2 of {@link isServiceEnvelope} — the ADR-0042 / ADR-0044
 * push-routing-secret distribution envelope.
 *
 * The cheap length + first-byte peek runs FIRST so a real chat message
 * never pays for the throwing codec call. The peek alone is NOT a
 * verdict here: a legacy plain-UTF-8 body that happens to be 70 bytes
 * and start with `l` clears it, and suppressing that would swallow a
 * real message — so the authoritative codec decides.
 *
 * @param {DecryptWasm} wasm
 * @param {Uint8Array} bytes
 * @returns {boolean}
 */
function isPushRoutingSecretEnvelope(wasm, bytes) {
  if (
    bytes.length !== PUSH_ROUTING_ENVELOPE_LEN ||
    bytes[0] !== PUSH_ROUTING_ENVELOPE_FIRST_BYTE
  ) {
    return false;
  }
  // The wasm codec is the authoritative fail-closed gate (domain +
  // version + exact-length; a retired v1 envelope is refused here too
  // — ADR-0044 is a hard flag day). It THROWS on a non-envelope, so a
  // throw means "not this kind" and the caller parses normally.
  if (typeof wasm.decodePushRoutingSecret !== "function") {
    // No codec available to confirm. The peek is already exact (a
    // 70-byte plaintext starting with `l`, which a JSON chat envelope
    // can never be), so suppress: a generic notification is strictly
    // better than rendering the envelope's binary as text.
    return true;
  }
  let decoded;
  try {
    decoded = wasm.decodePushRoutingSecret(bytes);
  } catch {
    return false;
  }
  freeQuietly(decoded);
  return true;
}

/**
 * Arm 3 of {@link isServiceEnvelope} — the ADR-0045 committer-liveness
 * beacon.
 *
 * ## Why this arm is MORE load-bearing than arm 2, not less
 *
 * A routing secret is broadcast only on a membership change. A beacon
 * is emitted by EVERY owner and admin every 60 s, so in an active group
 * the newest journal row — the very row this handler previews — is
 * frequently a beacon. Unfiltered it renders as the literal text
 * `lakko/committer-beacon/v1` plus two control bytes.
 *
 * ## The exact-equality simplification, taken deliberately
 *
 * The beacon carries no fields — no epoch, no identity, no timestamp
 * (see the module docs in `crates/lakko-contacts/src/committer_beacon.rs`
 * for why each was excluded). So EVERY beacon on the wire is the SAME
 * 27 bytes, and the length + first-byte peek is a COMPLETE test rather
 * than merely a fast path — the opposite of arm 2, where a 70-byte
 * legacy plaintext can clear the peek. The codec call below is
 * therefore belt-and-braces confirmation, not a necessity; it stays
 * because it is the single definition of this wire format, so a version
 * bump moves the verdict without an edit here.
 *
 * ## The length is READ, never written down
 *
 * `committerBeaconEnvelopeLen()` exists on the FFI precisely so three
 * clients do not each hand-copy `27` — the drift shape ADR-0044 was
 * written about. An older deps object without it skips the length test
 * and leans on the codec, which is still authoritative.
 *
 * Classification ONLY — no writes, no cursor advance (C-59 / ADR-0041).
 * The SW never EMITS a beacon either: a beacon means "able to sweep",
 * and this path structurally cannot commit.
 *
 * @param {DecryptWasm} wasm
 * @param {Uint8Array} bytes
 * @returns {boolean}
 */
function isCommitterBeaconEnvelope(wasm, bytes) {
  if (bytes.length === 0 || bytes[0] !== COMMITTER_BEACON_FIRST_BYTE) return false;
  if (typeof wasm.committerBeaconEnvelopeLen === "function") {
    let expected;
    try {
      expected = wasm.committerBeaconEnvelopeLen();
    } catch {
      expected = undefined;
    }
    if (typeof expected === "number" && bytes.length !== expected) return false;
  }
  // No codec to confirm with. Unlike arm 2 there is no safe suppress
  // here: without the length constant the peek is a single byte, which
  // a real "let's meet at six" would clear. Fall through and let the
  // message render — a missed filter costs one ugly notification, a
  // false positive costs a swallowed message.
  if (typeof wasm.decodeCommitterBeacon !== "function") return false;
  try {
    // Returns nothing on success — the beacon is field-less, so there
    // is no handle to free.
    wasm.decodeCommitterBeacon(bytes);
  } catch {
    return false;
  }
  return true;
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
  // A service envelope is not a message — returning null lets the
  // caller's loop keep an OLDER real chat row as the preview, and the
  // SW's generic fallback covers the case where there is none. Same
  // shape as the delete / reaction / typing / read-receipt skips below.
  if (isServiceEnvelope(wasm, processed.applicationData)) return null;
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

/** Standard-alphabet base64 of raw bytes (matches `auth/account-id`). */
function bytesToBase64(bytes) {
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i] ?? 0);
  return btoa(s);
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
