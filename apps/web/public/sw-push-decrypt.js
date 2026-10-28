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
  callSilentNotification,
  notificationStrings,
  callNotificationTag,
  isCallNotificationTag,
  callClickPlan,
  callIntentUrl,
  intentForAction,
  CALL_ACTION_MESSAGE,
} from "./sw-call-notification.js";

import { attachmentsLine, notificationStrings as readWords } from "./sw-call-notification.js";

/** Max characters of decrypted text shown in the notification body. */
const PREVIEW_MAX_CHARS = 140;

/**
 * How long a call rings before both sides give up, in milliseconds.
 *
 * A DELIBERATE COPY, and the only one left on this client. Its one home is
 * `lakko_ui_rules::ring::CALL_RING_WINDOW_MS`, and this file asks the real
 * rule (`wasm.ringDecision`) whenever it can — see {@link ringVerdict}. The
 * copy exists because this file is reached through a dynamic import in a
 * service worker whose wasm bundle may predate the rule, and because the
 * cached `sw-wasm/` output is versioned independently of this module. When
 * the rule is there it is asked; this number is what "could not ask" falls
 * back to, not a second opinion.
 *
 * It replaces a `STALE_OFFER_MS = 60_000` that mirrored a constant in
 * `src/calls/call-signaling-dispatch.ts` — which itself disagreed with the
 * 45 s the call session armed its timers from, so an offer in between rang
 * here for a call the caller had already recorded as missed.
 */
const CALL_RING_WINDOW_MS = 45_000;

/** {@link ringVerdict} answers, mirroring `lakko_ui_rules::ring`. */
const RING = /** @type {const} */ ({ ring: "ring", silent: "silent", tooLate: "tooLate" });

/**
 * What an arriving offer does on this device: the shared rule's answer.
 *
 * Asked of the wasm core when it exports the rule; otherwise decided from
 * {@link CALL_RING_WINDOW_MS} by the same two clauses the rule states — age
 * first, then mute — so an older cached bundle degrades to the same answers
 * rather than to no mute rule at all.
 *
 * `offerAgeMs` comes from the DS row's own SERVER timestamp, never a local
 * clock and never anything the caller stamped, and is clamped non-negative
 * because the rule takes an unsigned duration.
 *
 * @param {DecryptWasm} wasm
 * @param {boolean} muted
 * @param {number} offerAgeMs
 * @returns {"ring" | "silent" | "tooLate"}
 */
function ringVerdict(wasm, muted, offerAgeMs) {
  const age = Math.max(0, Math.trunc(offerAgeMs));
  const decisions = wasm?.RingDecision;
  if (typeof wasm?.ringDecision === "function" && decisions !== undefined) {
    try {
      const d = wasm.ringDecision(muted === true, BigInt(age));
      if (d === decisions.TooLate) return RING.tooLate;
      if (d === decisions.Silent) return RING.silent;
      return RING.ring;
    } catch {
      // Fall through to the mirror rather than dropping a live call.
    }
  }
  if (age >= CALL_RING_WINDOW_MS) return RING.tooLate;
  return muted === true ? RING.silent : RING.ring;
}

/**
 * @typedef {object} PushWake
 * @property {string | null} subHex      The push-gw subscription id the wake was
 *   delivered for (`s`, 32 hex chars, ADR-0078). The wake is handled only when
 *   the bound account's route row for `groupHex` holds that id.
 * @property {string | null} groupHex    64 lowercase-hex chars: the OPAQUE
 *   32-byte `push_gid` (ADR-0042), reverse-mapped through `lookupRoute`.
 * @property {number | null} pointer     DS message id of the message that triggered the wake.
 */

/**
 * The account the app recorded when it registered this browser for push
 * (`src/push/group-routing-store.ts`, `wake_binding`).
 * @typedef {object} WakeBinding
 * @property {string} accountId           base64(account_id).
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
 * @property {(bytes: Uint8Array) => void} [decodeProfileRequest]
 *   ADR-0048 §2 trigger 4 — the fail-closed profile-request codec. THROWS
 *   when the bytes are not that envelope; returning is the whole result
 *   (the request is field-less). Used ONLY to classify the row out of the
 *   preview path — the SW never answers a request (an answer is an MLS
 *   application message, C-59). Optional so an older deps object still
 *   loads.
 * @property {() => number} [profileRequestEnvelopeLen]
 *   ADR-0048 — the exact on-wire length of a profile request, read from the
 *   FFI like `committerBeaconEnvelopeLen`. Optional; absent means the arm
 *   is off (see `isProfileRequestEnvelope` for why it cannot fall back to
 *   the codec alone).
 * @property {(bytes: Uint8Array) => any} [decodeGroupNameEnvelope]
 *   ADR-0051 — the fail-closed group-name-envelope codec. THROWS an `Error`
 *   carrying a `groupNameError` code when the bytes are not a well-formed
 *   group name; `wrongDomain` is the only "not a group name" answer. Used
 *   ONLY to classify the row out of the preview path — the SW never stores
 *   a received name (C-59). Optional so an older deps object still loads.
 * @property {(bytes: Uint8Array) => any} [decodeGroupClosedEnvelope]
 *   ADR-0073 phase 2 — the fail-closed group-closed-envelope codec. THROWS
 *   an `Error` carrying a `groupClosedError` code when the bytes are not a
 *   well-formed notice; `wrongDomain` is the only "not a notice" answer.
 *   Used ONLY to classify the row out of the preview path — the SW never
 *   closes a group (C-59). Optional so an older deps object still loads.
 * @property {(bytes: Uint8Array) => any} [decodePinEnvelope]
 *   ADR-0052 decision 3 — the fail-closed shared-pin-envelope codec. THROWS
 *   an `Error` carrying a `pinError` code when the bytes are not a
 *   well-formed pin; `wrongDomain` is the only "not a pin" answer. Used ONLY
 *   to classify the row out of the preview path — the SW never stores a
 *   received pin (C-59). Optional so an older deps object still loads.
 * @property {(bytes: Uint8Array) => any} [decodeSharedChannelsEnvelope]
 *   ADR-0059 section 6 — the fail-closed shared-channels codec. THROWS an
 *   `Error` carrying a `sharedChannelsError` code when the bytes are not a
 *   well-formed list; `wrongDomain` is the only "not a list" answer. Used
 *   ONLY to classify the row out of the preview path — the SW never stores
 *   a received list (C-59). Optional so an older deps object still loads.
 * @property {(bytes: Uint8Array) => any} [dispatchIncomingEnvelope]
 *   ADR-0060 — the core's service-envelope dispatcher, whose `kind` names the
 *   scoped-discovery envelopes (introduction offer / bundle, visibility,
 *   reach state, committer delegation) and the ADR-0057 registrar seed. Used
 *   ONLY to classify the row out of the preview path — the SW keeps no
 *   grant, record or delegation (C-59). Optional so an older deps object
 *   still loads.
 * @property {Record<string, number>} [IncomingEnvelopeKind]
 *   The dispatcher's kind enum (the same wasm namespace).
 * @property {(bytes: Uint8Array) => any} [parseCallSignaling]
 *   Phase 3a3 — the prost `CallSignalingPayload` codec. THROWS on bytes that
 *   are not one. Used to tell an OFFER (ring) and an END (stop ringing) apart
 *   from the answer / candidate / group-control frames, which stay
 *   suppressed. Classification ONLY: the worker reads the call id and the
 *   media flags to shape a notification and never applies, answers or stores
 *   anything (C-59). Optional — absent means every call frame is suppressed,
 *   i.e. exactly the pre-ring behaviour.
 * @property {(bytes: Uint8Array) => any} [decodeRegistrarSeedEnvelope]
 *   ADR-0057 — the fail-closed registrar-seed-envelope codec (the seed stays
 *   sealed). THROWS when the bytes are not that envelope. Used ONLY to
 *   classify the row out of the preview path — the SW never opens or keeps a
 *   registrar seed (C-59). Optional so an older deps object still loads.
 * @property {() => number} [registrarSeedEnvelopeLen]
 *   ADR-0057 — the one on-wire length of a registrar-seed envelope, read from
 *   the FFI. Optional; absent means the arm is off.
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
 * @property {() => Promise<WakeBinding | null>} [readWakeBinding]
 *   READ-ONLY: the binding row. ABSENT or `null` means no account is signed in
 *   for push on this browser, and no wake is decrypted — the gate
 *   ({@link wakeBoundAccount}) fails closed. Any throw reads as `null`.
 * @property {(accountId: string, pushGidHex: string) => Promise<{ accountId: string, groupIdB64: string, subIdHex?: string } | null>} lookupRoute
 *   ADR-0042 — READ-ONLY reverse-map of `(bound account, opaque push_gid)`
 *   (lowercase hex) to the base64 group_id and this device's subscription id
 *   via the plaintext `lakko-push-routing`/`account_routes` IDB index. `null`
 *   on a miss; a row naming another account is a miss; any throw is treated
 *   as a miss (fail-closed).
 * @property {(accountId: string) => Promise<any | null>} getDeviceKey
 *   The non-extractable AES-GCM device root CryptoKey, or null.
 * @property {(accountId: string, deviceKey: any) => Promise<Uint8Array | null>} getIdentitySeed
 *   The 64-byte AccountIdentity (BIP39) seed, decrypted under the device key, or null.
 * @property {(accountId: string, deviceKey: any) => Promise<LoadedSnapshot | null>} loadSnapshot
 *   The decrypted MLS provider snapshot + knownGroupIds, or null.
 * @property {DecryptWasm} wasm
 *   The loaded wasm core (or a mock).
 * @property {(groupIdBytes: Uint8Array, afterId: number, signProof: ((method: string, path: string, query: string) => string) | null) => Promise<{ messages: Array<{ id: number, payload: Uint8Array, createdAt: string }> }>} dsFetchGroup
 *   READ-ONLY DS group-journal fetch of rows strictly after `afterId`.
 *   ADR-0053 — `signProof`, when present, returns the `x-lakko-ds-proof` header
 *   value for the request (service path + exact query, empty body), signed with
 *   the throwaway group's access key; a claimed journal answers nothing else.
 * @property {(accountId: string, groupHex: string) => Promise<number | null>} [startCursor]
 *   The app's last-persisted per-group cursor (READ-ONLY), if available. Used as
 *   the fetch start so intervening commits are replayed and the epoch stays right.
 * @property {(accountId: string, deviceKey: any, senderKeyB64: string) => Promise<string | null>} [lookupSenderName]
 *   ADR-0048 — READ-ONLY: the name this device ALREADY resolves for a sender
 *   (local rename, profile name, verified username), from the encrypted
 *   profile row the app wrote. `null` when nothing is known. Never fetches or
 *   verifies anything. Optional; any throw is treated as unknown.
 * @property {(accountId: string, deviceKey: any, groupHex: string) => Promise<boolean | null>} [lookupMuted]
 *   READ-ONLY: is this conversation muted? From the encrypted cross-device
 *   sync row the app wrote (`src/devices/sync/sync-state-idb.ts`), the same
 *   flag the conversation list draws and that travels between a person's
 *   devices. `null` when unknown.
 *
 *   FAILS TOWARD RINGING, deliberately and in the opposite direction to
 *   everything else on this path: a missing row, an unopenable blob, an
 *   absent dep or a throw all mean "not muted", so a call is never silenced
 *   because a database did not open. Silently swallowing calls is a far worse
 *   defect than an unwanted ring.
 * @property {(accountId: string, deviceKey: any, groupHex: string) => Promise<string | null>} [lookupConversation]
 *   ADR-0073 R.5 — READ-ONLY: the conversation `groupHex` belongs to (hex of
 *   the lineage's first group), from the same sync row as the mute flag.
 *   `null` when unknown: the notification is then grouped under the group
 *   id, as before. Optional; a throw is treated as unknown.
 * @property {(accountId: string, deviceKey: any, question: ResendQuestion) => Promise<boolean>} [lookupResendHeld]
 *   ADR-0073 R.7 case 1 — READ-ONLY: is this message a RE-SEND whose
 *   original this device already holds? Answered from two rows the app wrote
 *   and the worker already reads (the encrypted message log and the
 *   encrypted sync row), through {@link resendHeldFromRows}. `true` posts
 *   nothing for the message. Optional; absent, `false` and a throw all mean
 *   "not held", so the message is previewed as any other (the marker's
 *   fail-safe direction: a notification too many, never one too few).
 * @property {(accountId: string) => Promise<readonly string[] | null>} [lookupSupersededKeys]
 *   ADR-0073 R.5 / R.8 — READ-ONLY: the account keys (standard base64) an
 *   accepted reset retired, from the plaintext mirror the app writes
 *   (`src/push/superseded-mirror.ts`). A frame whose MLS-authenticated
 *   sender is bound to one of them (a removed device that still holds the
 *   old keys) neither rings nor becomes a preview. Read once per wake.
 *   Optional; absent, `null` and a throw mean "none retired".
 * @property {unknown} [notificationWords]
 *   ADR-0061 — the notification words the page stored in Cache Storage (raw,
 *   as read), so an attachment-only preview reads in the interface language.
 *   Absent or malformed means English.
 * @property {(accountId: string, deviceKey: any, groupHex: string) => Promise<Iterable<number> | null>} [lookupHeldMessageIds]
 *   READ-ONLY: the DS message ids the app's encrypted message log already
 *   holds for this group (`src/chat/message-log-store.ts`), through
 *   {@link heldMessageIdsFromLogRow}. Asked at most once per wake, and only
 *   when a row does not decrypt: a sender cannot open its own message, so a
 *   row that does not decrypt but is held is this device's own message
 *   coming back (or one it already received) and is quiet. Optional; absent,
 *   `null` and a throw all mean "nothing held", so such a row counts as
 *   unreadable and the generic notification stands.
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
 * @property {boolean} silent  The conversation is MUTED, so the call must not
 *   interrupt (`docs/ux/calls.md` section 2.1): no ringtone, no vibration,
 *   nothing that demands interaction. NOT the same as not ringing at all —
 *   the person still gets to find out somebody tried to reach them.
 *   Meaningless for `"end"`.
 * @property {number} offerAgeMs  Age of the offer at classification, off the
 *   DS row's own server timestamp. Carried so the mute verdict is taken from
 *   the same reading the staleness gate used. Meaningless for `"end"`.
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
 * What the background read of one wake found.
 *
 * @typedef {object} PushWakeOutcome
 * @property {PushPreview | null} preview  The preview or the call notice, as
 *   {@link decryptPushPreview} returns it.
 * @property {boolean} readRan  The read ran to the end: the snapshot was
 *   restored and the rows were fetched and processed. `false` on every early
 *   return, where nothing is known about the wake.
 * @property {number} unreadableRows  Rows that did not decrypt and that the
 *   app's message log does not hold: a message this device cannot read. A row
 *   that does not decrypt but IS held is this device's own message coming
 *   back (a sender cannot open its own message) and is not counted.
 */

/** @type {PushWakeOutcome} */
const READ_DID_NOT_RUN = Object.freeze({ preview: null, readRan: false, unreadableRows: 0 });

/**
 * Whether a wake that produced no preview and no call notice still shows the
 * generic "New message" notification.
 *
 * A notification means a message from someone else. A wake that brought only
 * this device's own message, a message from another device of the same
 * account, or service traffic (receipts, typing, reactions, profiles, ...)
 * shows nothing. The generic notification is kept for what cannot be told
 * apart: a read that did not run, or a row that did not decrypt and that the
 * message log does not hold. The same rule as Android's
 * `PushWakeOutcome.genericBannerWanted` (the "app on screen" half is
 * `public/sw.js`'s, which does not run the read for a visible tab).
 *
 * Pure; exported for `public/sw.js` and the tests.
 *
 * @param {PushWakeOutcome | null | undefined} outcome
 * @returns {boolean}
 */
export function genericNotificationWanted(outcome) {
  if (outcome === null || outcome === undefined) return true;
  if (outcome.preview !== null && outcome.preview !== undefined) return false;
  return outcome.readRan !== true || outcome.unreadableRows > 0;
}

/**
 * The DS message ids the decrypted message-log payload holds for `groupHex`,
 * as numbers. The log stores ids as strings (`PersistedMessage.messageId`);
 * anything malformed is skipped. Pure; exported for `public/sw.js` and the
 * tests.
 *
 * @param {any} logPayload  the decrypted message-log row.
 * @param {string} groupHex lowercase hex of the REAL group id.
 * @returns {Set<number>}
 */
export function heldMessageIdsFromLogRow(logPayload, groupHex) {
  /** @type {Set<number>} */
  const out = new Set();
  const slot = logPayload?.groups?.[groupHex];
  if (slot === null || typeof slot !== "object" || !Array.isArray(slot.messages)) return out;
  for (const m of slot.messages) {
    const id = Number(m?.messageId);
    if (Number.isSafeInteger(id) && id >= 0) out.add(id);
  }
  return out;
}

/**
 * Produce a real decrypted notification preview for a GroupMessage
 * wake, or `null` when the background path cannot (safely) yield one.
 * {@link decryptPushWake} with the outcome reduced to its preview.
 *
 * @param {PushWake | null} wake
 * @param {DecryptPushDeps} deps
 * @returns {Promise<PushPreview | null>}
 */
export async function decryptPushPreview(wake, deps) {
  return (await decryptPushWake(wake, deps)).preview;
}

/**
 * The wake gate (ADR-0042, "A wake is shown only for the signed-in
 * account"): the account a wake may be handled as, or `null`.
 *
 * A wake is handled only when a binding exists and the wake carries a
 * well-formed subscription id `s` (32 hex chars, ADR-0078). The binding is
 * cleared first at Log out and Forget device, so a wake for a signed-out
 * account never passes; {@link resolveWakeTarget} then requires the bound
 * account's own route row to hold that id, so a wake for another account
 * that once used this browser never passes either. The account used is
 * always the binding's.
 *
 * Pure; exported for `public/sw.js` (which mirrors it inline so a classic
 * worker applies the same rule) and the tests. The two must stay in step.
 *
 * @param {PushWake | null | undefined} wake
 * @param {WakeBinding | null | undefined} binding
 * @returns {string | null}
 */
export function wakeBoundAccount(wake, binding) {
  if (wake === null || wake === undefined || binding === null || binding === undefined) {
    return null;
  }
  if (typeof binding.accountId !== "string" || binding.accountId.length === 0) return null;
  if (normalisedSubIdHex(wake.subHex) === null) return null;
  return binding.accountId;
}

/**
 * Resolve a wake's `g` (an opaque `push_gid`) to a group of the BOUND
 * account, or `null`: the account-scoped reverse map must hold a row for it
 * AND that row's subscription id must equal the wake's `s` (ADR-0078).
 * Another account's route row, a row with no or another id, or a malformed
 * row is a miss. There is no fallback read of `g` as a raw group id.
 *
 * Exported for `public/sw.js`, which mirrors the same read inline so a
 * classic service worker (no dynamic import) applies the same rule; the two
 * must stay in step.
 *
 * @param {string} wakeHex  64 lowercase hex chars.
 * @param {string} subHex   32 hex chars, the wake's `s`.
 * @param {string} accountId  the bound account.
 * @param {Pick<DecryptPushDeps, "lookupRoute">} deps
 * @returns {Promise<{ groupIdBytes: Uint8Array, groupHex: string } | null>}
 */
export async function resolveWakeTarget(wakeHex, subHex, accountId, deps) {
  const presented = normalisedSubIdHex(subHex);
  if (presented === null || typeof deps.lookupRoute !== "function") return null;
  let route = null;
  try {
    route = await deps.lookupRoute(accountId, wakeHex);
  } catch {
    return null;
  }
  if (
    route === null ||
    route === undefined ||
    route.accountId !== accountId ||
    typeof route.groupIdB64 !== "string" ||
    normalisedSubIdHex(route.subIdHex) !== presented
  ) {
    return null;
  }
  const bytes = base64DecodeQuietly(route.groupIdB64);
  if (bytes === null || bytes.length !== 32) return null;
  return { groupIdBytes: bytes, groupHex: bytesToHex(bytes) };
}

/**
 * The background read of one wake: the preview (or call notice) and what the
 * read could tell about the rows that gave none. Never throws for a "cannot
 * decrypt" reason; every such early return is {@link READ_DID_NOT_RUN}. (A
 * programming error in a mock could still throw in tests — the SW wraps the
 * whole call in try/catch regardless.)
 *
 * @param {PushWake | null} wake
 * @param {DecryptPushDeps} deps
 * @returns {Promise<PushWakeOutcome>}
 */
export async function decryptPushWake(wake, deps) {
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
    return READ_DID_NOT_RUN;
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
  //
  //    ADR-0042 (amended 2026-09-30): ONLY as the bound account. The gate
  //    runs first; the reverse map and the legacy scan are asked about that
  //    account alone.
  let binding = null;
  if (typeof deps.readWakeBinding === "function") {
    try {
      binding = await deps.readWakeBinding();
    } catch {
      binding = null;
    }
  }
  const accountId = wakeBoundAccount(wake, binding);
  if (accountId === null) return READ_DID_NOT_RUN;
  const target = await resolveWakeTarget(wakeHex, wake.subHex, accountId, deps);
  if (target === null) return READ_DID_NOT_RUN;
  const groupIdBytes = target.groupIdBytes;
  const groupHex = target.groupHex;

  // 3. The device root key (a non-extractable CryptoKey handle usable
  //    for AES-GCM decrypt without extraction). No key → can't unwrap
  //    the seed/snapshot → generic.
  const deviceKey = await deps.getDeviceKey(accountId);
  if (deviceKey === null || deviceKey === undefined) return READ_DID_NOT_RUN;

  // 4. The AccountIdentity seed → the throwaway Identity.
  const identitySeed = await deps.getIdentitySeed(accountId, deviceKey);
  if (identitySeed === null || identitySeed === undefined) return READ_DID_NOT_RUN;

  // 5. The last-persisted MLS provider snapshot (the ratchet state the
  //    foreground app relies on — we only READ it).
  const snapshot = await deps.loadSnapshot(accountId, deviceKey);
  if (snapshot === null || snapshot === undefined) return READ_DID_NOT_RUN;

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
    return READ_DID_NOT_RUN;
  }
  if (group === null || group === undefined) {
    freeQuietly(identity);
    return READ_DID_NOT_RUN;
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

    // ADR-0053 contract point 9 — a claimed journal answers only a request
    // proven under the group's access key. Deriving it is one exporter read on
    // the same throwaway group, and signing a GET writes nothing (C-59): no
    // claim, no registration from here. No key (an inactive group, a mock) ->
    // an unsigned read, which an unclaimed journal still answers and a claimed
    // one refuses -> the generic notification below.
    let signer = null;
    try {
      signer =
        typeof group.dsAccessSigner === "function" ? group.dsAccessSigner(identity) : null;
    } catch {
      signer = null;
    }
    const signProof =
      signer === null
        ? null
        : (method, path, query) => signer.sign(method, path, query, new Uint8Array(0));
    let fetched;
    try {
      fetched = await deps.dsFetchGroup(groupIdBytes, after, signProof);
    } catch {
      return READ_DID_NOT_RUN;
    } finally {
      freeQuietly(signer);
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
    const words = readWords(deps.notificationWords ?? null);
    // ADR-0073 R.8 — the retired keys, read once for the batch.
    const retired = await supersededKeySet(deps, accountId);
    // The ids the app's message log holds for this group, read at most once
    // and only when a row does not decrypt.
    /** @type {Set<number> | null} */
    let held = null;
    let unreadableRows = 0;
    for (const message of messages) {
      let processed;
      try {
        processed = group.processIncomingVerifyingRoles(identity, message.payload);
      } catch (err) {
        // Bad frame / wrong epoch / own echo / role-refused commit —
        // fail-closed skip. NOTE: this mutates the THROWAWAY provider
        // only; it is never persisted (read-only-throwaway discipline).
        //
        // A sender cannot open its own message. The app's log holds a row
        // it sent (or one it already received), so a held id is quiet;
        // anything else is a message this device cannot read, which keeps
        // the generic notification.
        if (isOwnMessageEcho(err)) continue;
        if (held === null) held = await heldMessageIds(deps, accountId, deviceKey, groupHex);
        if (!held.has(message.id)) unreadableRows += 1;
        continue;
      }
      // Nothing from a superseded sender acts: no ring, no preview. The
      // sender is resolved to its PERSON key against the roster of the
      // epoch the frame decrypted at.
      if (retired.size > 0) {
        const senderPerson = personKeyAtLeaf(group, processed?.senderLeaf);
        if (senderPerson !== null && retired.has(senderPerson)) continue;
      }
      const call = callNoticeFrom(deps.wasm, processed, message, groupHex, now);
      if (call !== null) {
        latestCall = { notice: call, senderKeyB64: senderKeyOf(group, processed) };
        continue;
      }
      // ADR-0073 R.7 case 1 — a re-sent message whose original this device
      // already holds is not a new message here: it posts nothing, and an
      // older real row of the batch stays the preview. A READ (C-59).
      if (await resendAlreadyHeld(deps, accountId, deviceKey, group, processed, groupHex)) {
        continue;
      }
      // A message from this device's own account (this device, or another
      // device of the same person) is not a notification here. Both keys are
      // resolved against the roster of the epoch the frame decrypted at.
      const senderPerson = personKeyAtLeaf(group, processed?.senderLeaf);
      if (senderPerson !== null && senderPerson === ownPersonKey(group)) continue;
      const preview = previewFromProcessed(deps.wasm, processed, words);
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
      // `docs/ux/calls.md` section 2.1 — "a muted chat never rings". Read
      // ONCE per wake, for the one call the batch settled on, and only for a
      // ring: an `end` takes a notification down, which mute has no say over.
      //
      // Every failure here reads as NOT muted. That is the opposite of the
      // fail-closed default the rest of this file follows, and it is the
      // right way round: the cost of getting it wrong in this direction is a
      // ring the person did not want, and in the other it is a call they
      // never learn about.
      let muted = false;
      if (latestCall.notice.kind === "ring" && typeof deps.lookupMuted === "function") {
        try {
          muted = (await deps.lookupMuted(accountId, deviceKey, groupHex)) === true;
        } catch {
          muted = false;
        }
      }
      const notice = {
        ...latestCall.notice,
        callerName: typeof callerName === "string" && callerName.length > 0 ? callerName : null,
        silent:
          latestCall.notice.kind === "ring" &&
          ringVerdict(deps.wasm, muted, latestCall.notice.offerAgeMs) === RING.silent,
      };
      // `title` / `body` / `tag` are carried for a caller that ignores `call`
      // entirely; `public/sw.js` reads `call` and builds the real ring shape
      // through `callRingNotification`.
      return {
        preview: {
          title: notice.callerName ?? "Lakko",
          body: notice.video ? words.videoCall : words.voiceCall,
          tag: groupHex,
          call: notice,
        },
        readRan: true,
        unreadableRows,
      };
    }

    // ADR-0073 R.5 — group the notification under the CONVERSATION, so a
    // successor group's message replaces the conversation's earlier
    // notification rather than stacking beside it. Unknown reads as the
    // group id (the grouping every build had).
    if (latest !== null && typeof deps.lookupConversation === "function") {
      try {
        const conversationHex = await deps.lookupConversation(accountId, deviceKey, groupHex);
        if (typeof conversationHex === "string" && conversationHex.length > 0) {
          latest = { ...latest, tag: conversationHex };
        }
      } catch {
        // keep the group id
      }
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
    return { preview: latest, readRan: true, unreadableRows };
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
      return {
        kind: "end",
        callIdHex,
        groupHex,
        video: false,
        callerName: null,
        silent: false,
        offerAgeMs: 0,
      };
    }
    // The DS row's SERVER timestamp, never `parsed.createdAtMs`, which the
    // caller stamped and could be anything.
    const createdAt = message?.createdAt;
    if (typeof createdAt !== "string") return null;
    const createdMs = Date.parse(createdAt);
    if (Number.isNaN(createdMs)) return null;
    const offerAgeMs = Math.max(0, nowMs - createdMs);
    // Asked with `muted: false`, because whether the call is OVER does not
    // depend on mute and this runs per row, before the one mute read the
    // batch makes. `silent` is settled once, after the loop.
    if (ringVerdict(wasm, false, offerAgeMs) === RING.tooLate) return null;
    return {
      kind: "ring",
      callIdHex,
      groupHex,
      video: parsed.mediaVideo === true,
      callerName: null,
      silent: false,
      offerAgeMs,
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
 * Twelve arms below (the last is the ADR-0074 key-rotation envelope): call signaling, the ADR-0042 / ADR-0044 push-routing
 * secret, the ADR-0045 committer beacon, the ADR-0048 profile, the ADR-0048
 * profile request, the ADR-0051 group name, the ADR-0073 group-closed
 * notice, the ADR-0052 shared pin, the ADR-0059 shared channels, the
 * ADR-0057 registrar seed, and the ADR-0060 scoped-discovery envelopes.
 * Each arm is a cheap byte peek gating an authoritative codec.
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
  // 5b. Group closed (ADR-0073 phase 2) — the name's twin.
  if (isGroupClosedEnvelope(wasm, bytes)) return true;
  // 6. Profile request (ADR-0048 §2 trigger 4).
  if (isProfileRequestEnvelope(wasm, bytes)) return true;
  // 7. Shared pin (ADR-0052 decision 3).
  if (isPinEnvelope(wasm, bytes)) return true;
  // 8. Shared channels (ADR-0059 section 6).
  if (isSharedChannelsEnvelope(wasm, bytes)) return true;
  // 9. Registrar seed (ADR-0057).
  if (isRegistrarSeedEnvelope(wasm, bytes)) return true;

  // 10. Scoped discovery (ADR-0060).
  if (isScopedDiscoveryEnvelope(wasm, bytes)) return true;
  // 11. Key rotation (ADR-0074 section 3.3). It means something only in the
  //     account's own DeviceGroup, which this worker never reads; in a
  //     conversation it is somebody's stray bytes and is never rendered.
  if (isKeyRotationEnvelope(wasm, bytes)) return true;
  return false;
}

/**
 * Arm 11 of {@link isServiceEnvelope} — the ADR-0074 `key-rotation`
 * envelope, classified by the core's `dispatchIncomingEnvelope`. Same cheap
 * first-byte peek as the other `lakko/...` arms. Classification ONLY: the
 * envelope's fields (the account's new entropy) are never read here (C-59).
 *
 * @param {DecryptWasm} wasm
 * @param {Uint8Array} bytes
 * @returns {boolean}
 */
function isKeyRotationEnvelope(wasm, bytes) {
  if (bytes.length === 0 || bytes[0] !== 0x6c) return false;
  if (typeof wasm.dispatchIncomingEnvelope !== "function" || wasm.IncomingEnvelopeKind === undefined) {
    return false;
  }
  let env;
  try {
    env = wasm.dispatchIncomingEnvelope(bytes);
  } catch {
    return false;
  }
  const kind = env === null || env === undefined ? undefined : env.kind;
  freeQuietly(env);
  const kinds = /** @type {Record<string, number>} */ (wasm.IncomingEnvelopeKind);
  return kinds["KeyRotation"] !== undefined && kinds["KeyRotation"] === kind;
}

/**
 * First byte of the ADR-0059 shared-channels domain separator (`'l'` of
 * `"lakko/shared-channels/v1\0"`). Its own constant, like the ones below.
 */
const SHARED_CHANNELS_FIRST_BYTE = 0x6c;

/**
 * Arm 8 of {@link isServiceEnvelope} — the ADR-0059 shared-channels list.
 *
 * Unfiltered, it would be previewed as a notification body reading the
 * domain string and the channel names. Same verdict rule as the pin arm: a
 * decode that succeeds is a list; a throw with any `sharedChannelsError`
 * code but `wrongDomain` is a domain match refused as a whole, suppressed
 * here too; `wrongDomain` or a code-less throw falls through.
 *
 * The list is never kept: classification ONLY (C-59).
 *
 * @param {DecryptWasm} wasm
 * @param {Uint8Array} bytes
 * @returns {boolean}
 */
function isSharedChannelsEnvelope(wasm, bytes) {
  if (bytes.length === 0 || bytes[0] !== SHARED_CHANNELS_FIRST_BYTE) return false;
  if (typeof wasm.decodeSharedChannelsEnvelope !== "function") return false;
  let decoded;
  try {
    decoded = wasm.decodeSharedChannelsEnvelope(bytes);
  } catch (err) {
    const code =
      err !== null && typeof err === "object" ? /** @type {any} */ (err).sharedChannelsError : undefined;
    return typeof code === "string" && code !== "wrongDomain";
  }
  freeQuietly(decoded);
  return true;
}

/**
 * First byte of the ADR-0057 registrar-seed domain separator (`'l'`). Its own
 * constant, like the ones below.
 */
const REGISTRAR_SEED_FIRST_BYTE = 0x6c;

/**
 * Arm 9 of {@link isServiceEnvelope} — the ADR-0057 registrar-seed envelope a
 * group's owner sends one member. Fixed length (read from
 * `registrarSeedEnvelopeLen()`, never written down here), so the peek is the
 * length and the first byte, and the codec confirms. Without the length there
 * is no safe suppress, so the arm is off rather than guessing.
 *
 * Classification ONLY: the SW never opens the seed and never stores it (C-59).
 *
 * @param {DecryptWasm} wasm
 * @param {Uint8Array} bytes
 * @returns {boolean}
 */
function isRegistrarSeedEnvelope(wasm, bytes) {
  if (bytes.length === 0 || bytes[0] !== REGISTRAR_SEED_FIRST_BYTE) return false;
  if (typeof wasm.registrarSeedEnvelopeLen !== "function") return false;
  let expected;
  try {
    expected = wasm.registrarSeedEnvelopeLen();
  } catch {
    return false;
  }
  if (typeof expected !== "number" || bytes.length !== expected) return false;
  if (typeof wasm.decodeRegistrarSeedEnvelope !== "function") return false;
  let decoded;
  try {
    decoded = wasm.decodeRegistrarSeedEnvelope(bytes);
  } catch {
    return false;
  }
  freeQuietly(decoded);
  return true;
}

/**
 * The dispatcher kinds arm 10 suppresses. Each carries secrets (a delegation,
 * an introduction record, a registrar seed) or DeviceGroup-only state, and
 * would otherwise be previewed as a notification body of domain bytes.
 */
const SCOPED_DISCOVERY_KINDS = [
  "IntroductionOffer",
  "Introductions",
  "Visibility",
  "ReachState",
  "CommitterDelegation",
  "RegistrarSeed",
];

/**
 * Arm 10 of {@link isServiceEnvelope} — the ADR-0060 scoped-discovery
 * envelopes, classified by the core's `dispatchIncomingEnvelope` (which
 * answers `IntroMessage` for anything it does not recognise). Same cheap
 * first-byte peek as the other `lakko/...` arms. Classification ONLY (C-59).
 *
 * @param {DecryptWasm} wasm
 * @param {Uint8Array} bytes
 * @returns {boolean}
 */
function isScopedDiscoveryEnvelope(wasm, bytes) {
  if (bytes.length === 0 || bytes[0] !== 0x6c) return false;
  if (typeof wasm.dispatchIncomingEnvelope !== "function" || wasm.IncomingEnvelopeKind === undefined) {
    return false;
  }
  let env;
  try {
    env = wasm.dispatchIncomingEnvelope(bytes);
  } catch {
    return false;
  }
  const kind = env === null || env === undefined ? undefined : env.kind;
  freeQuietly(env);
  const kinds = /** @type {Record<string, number>} */ (wasm.IncomingEnvelopeKind);
  return SCOPED_DISCOVERY_KINDS.some((name) => kinds[name] !== undefined && kinds[name] === kind);
}

/**
 * First byte of the ADR-0052 pin domain separator (`'l'` of
 * `"lakko/pin/v1\0"`). Its own constant, like the ones above.
 */
const PIN_ENVELOPE_FIRST_BYTE = 0x6c;

/**
 * Arm 7 of {@link isServiceEnvelope} — the ADR-0052 shared-pin envelope.
 *
 * Unfiltered, a pin would be previewed as a notification body reading
 * `lakko/pin/v1` plus binary. Same verdict rule as the group-name arm: a
 * decode that succeeds is a pin; a throw with any `pinError` code but
 * `wrongDomain` is a domain match refused as a whole, suppressed here too;
 * `wrongDomain` or a code-less throw falls through to the normal parse.
 *
 * The pin is never kept: classification ONLY (C-59).
 *
 * @param {DecryptWasm} wasm
 * @param {Uint8Array} bytes
 * @returns {boolean}
 */
function isPinEnvelope(wasm, bytes) {
  if (bytes.length === 0 || bytes[0] !== PIN_ENVELOPE_FIRST_BYTE) return false;
  if (typeof wasm.decodePinEnvelope !== "function") return false;
  let decoded;
  try {
    decoded = wasm.decodePinEnvelope(bytes);
  } catch (err) {
    const code = err !== null && typeof err === "object" ? /** @type {any} */ (err).pinError : undefined;
    return typeof code === "string" && code !== "wrongDomain";
  }
  freeQuietly(decoded);
  return true;
}

/**
 * First byte of the ADR-0048 profile-request domain separator (`'l'` of
 * `"lakko/profile-request/v1\0"`). Its own constant, like the ones above.
 */
const PROFILE_REQUEST_FIRST_BYTE = 0x6c;

/**
 * Arm 6 of {@link isServiceEnvelope} — the ADR-0048 profile request, the
 * newcomer's "introduce yourselves".
 *
 * Field-less like the beacon, so the same shape: every request on the wire
 * is the same 26 bytes, the length + first-byte peek is a complete test, and
 * the codec behind it is confirmation that also makes the arm fail-closed on
 * an unknown version (a v2 this build does not know falls through and
 * renders — one ugly notification beats a swallowed message).
 *
 * The length is READ from `profileRequestEnvelopeLen()`, never written down
 * here (the ADR-0044 drift class). Without it there is no safe suppress —
 * the peek alone is a single byte a real "let's meet at six" would clear —
 * so the arm is off rather than guessing.
 *
 * Classification ONLY — no writes, no cursor advance, and no ANSWER: the
 * answer is the device's profile envelope, an MLS application message this
 * path structurally cannot send (C-59). The foreground answers when it next
 * reads the row.
 *
 * @param {DecryptWasm} wasm
 * @param {Uint8Array} bytes
 * @returns {boolean}
 */
function isProfileRequestEnvelope(wasm, bytes) {
  if (bytes.length === 0 || bytes[0] !== PROFILE_REQUEST_FIRST_BYTE) return false;
  if (typeof wasm.profileRequestEnvelopeLen !== "function") return false;
  let expected;
  try {
    expected = wasm.profileRequestEnvelopeLen();
  } catch {
    return false;
  }
  if (typeof expected !== "number" || bytes.length !== expected) return false;
  if (typeof wasm.decodeProfileRequest !== "function") return false;
  try {
    // Returns nothing on success — field-less, so no handle to free.
    wasm.decodeProfileRequest(bytes);
  } catch {
    return false;
  }
  return true;
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
 * First byte of the ADR-0073 group-closed domain separator (`'l'` of
 * `"lakko/group-closed/v1\0"`). Its own constant, like the ones above.
 */
const GROUP_CLOSED_ENVELOPE_FIRST_BYTE = 0x6c;

/**
 * Arm 5b of {@link isServiceEnvelope} — the ADR-0073 phase-2 group-closed
 * notice, the group name's twin: an admin closed the group after recreating
 * it. Unfiltered it would be previewed as a notification body reading
 * `lakko/group-closed/v1` plus the stamp and the successor id. Same verdict
 * rule as the name arm.
 *
 * Classification ONLY (C-59): the SW never closes a group.
 *
 * @param {DecryptWasm} wasm
 * @param {Uint8Array} bytes
 * @returns {boolean}
 */
function isGroupClosedEnvelope(wasm, bytes) {
  if (bytes.length === 0 || bytes[0] !== GROUP_CLOSED_ENVELOPE_FIRST_BYTE) return false;
  if (typeof wasm.decodeGroupClosedEnvelope !== "function") return false;
  let decoded;
  try {
    decoded = wasm.decodeGroupClosedEnvelope(bytes);
  } catch (err) {
    const code =
      err !== null && typeof err === "object" ? /** @type {any} */ (err).groupClosedError : undefined;
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
 * What {@link DecryptPushDeps.lookupResendHeld} is asked.
 *
 * @typedef {object} ResendQuestion
 * @property {string} receivedGroupHex  the group the wake is for, lowercase hex.
 * @property {string} originGroupB64    the marker's original group, standard base64.
 * @property {number} originMessageId   the original's DS message id there.
 * @property {string | null} senderAccountKeyB64  the PERSON key of the MLS-authenticated sender.
 * @property {string | null} ownAccountKeyB64     this account's PERSON key in the group.
 */

/**
 * ADR-0050 — base64 of the PERSON key (the account key, else the legacy leaf
 * key) of the member at `leaf` in the throwaway group's roster, or `null`.
 *
 * @param {any} group
 * @param {unknown} leaf
 * @returns {string | null}
 */
function personKeyAtLeaf(group, leaf) {
  if (typeof leaf !== "number") return null;
  let members;
  try {
    members = group.members();
  } catch {
    return null;
  }
  if (!Array.isArray(members)) return null;
  for (const m of members) {
    if (m?.leafIndex !== leaf) continue;
    if (m.accountKey instanceof Uint8Array && m.accountKey.length > 0) {
      return bytesToBase64(m.accountKey);
    }
    return m.signaturePublicKey instanceof Uint8Array ? bytesToBase64(m.signaturePublicKey) : null;
  }
  return null;
}

/**
 * The retired keys for this wake, as a set. Never throws; every miss is the
 * empty set (nothing is dropped).
 *
 * @param {DecryptPushDeps} deps
 * @param {string} accountId
 * @returns {Promise<Set<string>>}
 */
async function supersededKeySet(deps, accountId) {
  const out = new Set();
  if (typeof deps.lookupSupersededKeys !== "function") return out;
  try {
    const keys = await deps.lookupSupersededKeys(accountId);
    if (!Array.isArray(keys)) return out;
    for (const k of keys) if (typeof k === "string" && k.length > 0) out.add(k);
  } catch {
    out.clear();
  }
  return out;
}

/**
 * This device's own PERSON key in the throwaway group, read from its own
 * leaf, or `null`.
 *
 * @param {any} group
 * @returns {string | null}
 */
function ownPersonKey(group) {
  let ownLeaf;
  try {
    ownLeaf = typeof group.ownLeafIndex === "function" ? group.ownLeafIndex() : group.ownLeafIndex;
  } catch {
    return null;
  }
  return personKeyAtLeaf(group, ownLeaf);
}

/**
 * Whether a row's decrypt failure is this device's own message coming back:
 * a sender cannot open what it sent. The core names the refusal: openmls's
 * `ValidationError::CannotDecryptOwnMessage` reaches the client inside the
 * error text (`process_message: ValidationError(CannotDecryptOwnMessage)`).
 * It covers every row this device published (its typing, receipts and
 * commits, which the message log never holds). A text that does not match
 * (another openmls wording) only falls back to the held-id check.
 *
 * @param {unknown} err
 * @returns {boolean}
 */
export function isOwnMessageEcho(err) {
  const text = err instanceof Error ? err.message : String(err);
  return text.includes("CannotDecryptOwnMessage");
}

/**
 * The held message ids for this wake's group, as a set. Never throws; every
 * miss is the empty set (each undecryptable row then counts as unreadable).
 *
 * @param {DecryptPushDeps} deps
 * @param {string} accountId
 * @param {any} deviceKey
 * @param {string} groupHex
 * @returns {Promise<Set<number>>}
 */
async function heldMessageIds(deps, accountId, deviceKey, groupHex) {
  /** @type {Set<number>} */
  const out = new Set();
  if (typeof deps.lookupHeldMessageIds !== "function") return out;
  try {
    const ids = await deps.lookupHeldMessageIds(accountId, deviceKey, groupHex);
    if (ids === null || ids === undefined) return out;
    for (const id of ids) if (typeof id === "number" && Number.isSafeInteger(id)) out.add(id);
  } catch {
    out.clear();
  }
  return out;
}

/**
 * The re-send marker of a processed frame, or `null`: both halves present
 * and well-formed, read from the same parse the preview uses.
 *
 * @param {DecryptWasm} wasm
 * @param {any} processed
 * @returns {{ originGroupB64: string, originMessageId: number } | null}
 */
function resendMarkerOf(wasm, processed) {
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
    if (parsed.kind !== "chat") return null;
    const group = parsed.resendOriginGroupB64;
    const id = parsed.resendOriginMessageId;
    if (typeof group !== "string" || group.length === 0) return null;
    if (typeof id !== "bigint" && typeof id !== "number") return null;
    const messageId = Number(id);
    if (!Number.isSafeInteger(messageId) || messageId < 0) return null;
    return { originGroupB64: group, originMessageId: messageId };
  } catch {
    return null;
  } finally {
    freeQuietly(parsed);
  }
}

/**
 * Whether this frame is a re-send whose original the device holds. Never
 * throws; every miss is `false`.
 *
 * @param {DecryptPushDeps} deps
 * @param {string} accountId
 * @param {any} deviceKey
 * @param {any} group
 * @param {any} processed
 * @param {string} groupHex
 * @returns {Promise<boolean>}
 */
async function resendAlreadyHeld(deps, accountId, deviceKey, group, processed, groupHex) {
  if (typeof deps.lookupResendHeld !== "function") return false;
  const marker = resendMarkerOf(deps.wasm, processed);
  if (marker === null) return false;
  let ownLeaf;
  try {
    ownLeaf = typeof group.ownLeafIndex === "function" ? group.ownLeafIndex() : group.ownLeafIndex;
  } catch {
    ownLeaf = undefined;
  }
  try {
    return (
      (await deps.lookupResendHeld(accountId, deviceKey, {
        receivedGroupHex: groupHex,
        originGroupB64: marker.originGroupB64,
        originMessageId: marker.originMessageId,
        senderAccountKeyB64: personKeyAtLeaf(group, processed?.senderLeaf),
        ownAccountKeyB64: personKeyAtLeaf(group, ownLeaf),
      })) === true
    );
  } catch {
    return false;
  }
}

/**
 * ADR-0073 R.7 case 1 — the service worker's copy of the re-send rule
 * (`src/chat/resend-marker.ts` `resendVerdict`, which this file cannot
 * import): `true` exactly when
 *
 *   - the marker's group is an OLDER segment of the SAME conversation as the
 *     group the message was received in (from the decrypted sync row, the
 *     lineage registers of the ADR-0058 fold), and
 *   - the decrypted message log holds the named row in that group, and
 *   - that row's author is the account of the MLS-authenticated sender: a
 *     row this device sent itself counts as this account's.
 *
 * Anything unreadable, unknown or mismatched is `false`: the message is
 * previewed. Keys are compared as stored; a key alias the app would resolve
 * (a proven rotation) is not known here and reads as a mismatch, which errs
 * toward a notification.
 *
 * Pure apart from the wasm read; exported for `sw.js` and the tests.
 *
 * @param {any} logPayload   the decrypted message-log row (`message-log-store.ts`).
 * @param {any} syncPayload  the decrypted sync row (`sync-state-store.ts`).
 * @param {ResendQuestion} question
 * @param {any} wasm
 * @returns {boolean}
 */
export function resendHeldFromRows(logPayload, syncPayload, question, wasm) {
  if (question === null || typeof question !== "object") return false;
  const sender = question.senderAccountKeyB64;
  if (typeof sender !== "string" || sender.length === 0) return false;
  const receivedHex = question.receivedGroupHex;
  if (typeof receivedHex !== "string" || !isHex(receivedHex) || receivedHex.length === 0) return false;
  const originBytes = base64DecodeQuietly(question.originGroupB64);
  if (originBytes === null || originBytes.length === 0) return false;
  const originHex = bytesToHex(originBytes);
  if (originHex === receivedHex) return false;

  // The held row and its author.
  const slot = logPayload?.groups?.[originHex];
  if (slot === null || typeof slot !== "object" || !Array.isArray(slot.messages)) return false;
  const row = slot.messages.find((m) => Number(m?.messageId) === question.originMessageId);
  if (row === undefined || row === null) return false;
  let author = null;
  if (row.direction === "outgoing") author = question.ownAccountKeyB64;
  else if (row.direction === "incoming") author = row.authorKeyB64;
  if (typeof author !== "string" || author.length === 0 || author !== sender) return false;

  // The segment order, from the lineage registers.
  if (syncPayload === null || typeof syncPayload !== "object" || syncPayload.v !== 2) return false;
  if (typeof syncPayload.s !== "string" || syncPayload.s.length === 0) return false;
  if (wasm === null || typeof wasm !== "object" || typeof wasm.SyncedState !== "function") {
    return false;
  }
  let state = null;
  /** @type {any[]} */
  const registers = [];
  try {
    const stateBytes = base64DecodeQuietly(syncPayload.s);
    if (stateBytes === null) return false;
    const receivedBytes = new Uint8Array(receivedHex.length / 2);
    for (let i = 0; i < receivedBytes.length; i += 1) {
      receivedBytes[i] = parseInt(receivedHex.slice(i * 2, i * 2 + 2), 16);
    }
    state = wasm.SyncedState.fromBytes(stateBytes);
    // The received group must be a successor: a first group has no older
    // segment to hold an original in.
    const received = state.lineageRoot(receivedBytes);
    if (received === undefined || received === null) return false;
    registers.push(received);
    const rootHex = bytesToHex(received.rootConversationId);
    if (originHex === rootHex) return true;
    const origin = state.lineageRoot(originBytes);
    if (origin === undefined || origin === null) return false;
    registers.push(origin);
    if (bytesToHex(origin.rootConversationId) !== rootHex) return false;
    // Segments are ordered by stamp, ties broken on the successor id
    // (`src/chat/lineage.ts`).
    if (origin.hlc !== received.hlc) return origin.hlc < received.hlc;
    return originHex < receivedHex;
  } catch {
    return false;
  } finally {
    for (const r of registers) freeQuietly(r);
    freeQuietly(state);
  }
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
 * `docs/ux/calls.md` section 2.1 — is this conversation muted, according to
 * the decrypted cross-device sync row the app wrote
 * (`src/devices/sync/sync-state-idb.ts` + `sync-state-store.ts`)?
 *
 * The row's shape is that module's: `{ v: 2, s: <base64>, p: [...] }`, where
 * `s` is `SyncedState.toBytes()` - the ADR-0058 fold, shared with iOS and
 * Android - and `p` the folder edits not yet sent (not read here). The mute
 * flag is read out of the fold through the wasm core the worker already
 * loaded, keyed on the REAL DS `group_id`, given here as lowercase hex.
 *
 * Anything else — a schema this build does not know, bytes the fold refuses,
 * no flags for the group, a malformed id — is `false`, NOT muted. This is the
 * one read on the push path that fails toward ringing; see `lookupMuted` on
 * {@link DecryptPushDeps} for why that direction is the safe one here.
 *
 * Pure apart from the wasm read; exported for `sw.js` and the tests.
 *
 * @param {any} payload
 * @param {string} groupHex
 * @param {any} wasm the wasm namespace (`SyncedState`)
 * @returns {boolean}
 */
export function mutedFromSyncStateRow(payload, groupHex, wasm) {
  if (payload === null || typeof payload !== "object" || payload.v !== 2) return false;
  if (typeof payload.s !== "string" || payload.s.length === 0) return false;
  if (typeof groupHex !== "string" || groupHex.length === 0 || groupHex.length % 2 !== 0) {
    return false;
  }
  if (!/^[0-9a-f]+$/.test(groupHex)) return false;
  if (wasm === null || typeof wasm !== "object" || typeof wasm.SyncedState !== "function") {
    return false;
  }
  let state = null;
  try {
    const raw = atob(payload.s);
    const bytes = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i += 1) bytes[i] = raw.charCodeAt(i);
    const groupId = new Uint8Array(groupHex.length / 2);
    for (let i = 0; i < groupId.length; i += 1) {
      groupId[i] = parseInt(groupHex.slice(i * 2, i * 2 + 2), 16);
    }
    state = wasm.SyncedState.fromBytes(bytes);
    // ADR-0073 R.5 — the mute flag is the CONVERSATION's: keyed on the id of
    // the lineage's first group, never rewritten when the conversation
    // continues in a successor group. A wake for a successor is therefore
    // asked under the conversation it belongs to. For a group that is its
    // own conversation this is the same id.
    let conversationId = groupId;
    if (typeof state.conversationIdOf === "function") {
      try {
        const resolved = state.conversationIdOf(groupId);
        if (resolved instanceof Uint8Array && resolved.length > 0) conversationId = resolved;
      } catch {
        conversationId = groupId;
      }
    }
    const flags = state.flags(conversationId);
    if (flags === undefined || flags === null) return false;
    const muted = flags.muted === true;
    if (typeof flags.free === "function") flags.free();
    return muted;
  } catch {
    return false;
  } finally {
    if (state !== null && typeof state.free === "function") state.free();
  }
}

/**
 * ADR-0073 R.5 — the CONVERSATION a group belongs to, as lowercase hex, read
 * from the same decrypted sync row as the mute flag: the lineage's first
 * group for a successor, the group itself otherwise. What a notification is
 * grouped under, so a conversation that continued in a successor group
 * replaces its own earlier notification instead of opening a second stack.
 *
 * `null` on anything unreadable; the caller then groups by the group id, as
 * before.
 *
 * @param {any} payload
 * @param {string} groupHex
 * @param {any} wasm
 * @returns {string | null}
 */
export function conversationHexFromSyncStateRow(payload, groupHex, wasm) {
  if (payload === null || typeof payload !== "object" || payload.v !== 2) return null;
  if (typeof payload.s !== "string" || payload.s.length === 0) return null;
  if (typeof groupHex !== "string" || groupHex.length === 0 || groupHex.length % 2 !== 0) {
    return null;
  }
  if (!/^[0-9a-f]+$/.test(groupHex)) return null;
  if (wasm === null || typeof wasm !== "object" || typeof wasm.SyncedState !== "function") {
    return null;
  }
  let state = null;
  try {
    const raw = atob(payload.s);
    const bytes = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i += 1) bytes[i] = raw.charCodeAt(i);
    const groupId = new Uint8Array(groupHex.length / 2);
    for (let i = 0; i < groupId.length; i += 1) {
      groupId[i] = parseInt(groupHex.slice(i * 2, i * 2 + 2), 16);
    }
    state = wasm.SyncedState.fromBytes(bytes);
    if (typeof state.conversationIdOf !== "function") return null;
    const resolved = state.conversationIdOf(groupId);
    if (!(resolved instanceof Uint8Array) || resolved.length === 0) return null;
    let hex = "";
    for (const b of resolved) hex += b.toString(16).padStart(2, "0");
    return hex;
  } catch {
    return null;
  } finally {
    if (state !== null && typeof state.free === "function") state.free();
  }
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
 * @param {{ attachments: readonly string[], unsupportedMessage: string }} words
 * @returns {string | null}
 */
function previewFromProcessed(wasm, processed, words) {
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
    // A `lakko/…` service envelope none of the arms of `isServiceEnvelope`
    // recognised (a newer version of a known kind, or an unknown kind): the
    // core classifies it rather than degrading it to lossy-UTF-8 chat text.
    // Never a preview — null keeps an older real row, or the generic banner.
    if (kind === "service-envelope") return null;
    // ADR-0061 section 6 — a chat message from a newer Lakko whose wire
    // version this build cannot read. It is a message, so it is the preview,
    // in the words the page stored; never the core's English placeholder.
    // (`notificationStrings` falls back to English word by word.)
    if (kind === "unsupported") return words.unsupportedMessage;
    // A normal chat message → its text (or an attachment hint).
    if (kind === "chat") {
      const text = typeof parsed.text === "string" ? parsed.text : "";
      if (text.length > 0) return truncate(text);
      const count = typeof parsed.attachmentCount === "number" ? parsed.attachmentCount : 0;
      if (count > 0) return attachmentsLine(words, count);
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
/**
 * A 32-char hex subscription id (ADR-0078), lowercased, or `null` for
 * anything else.
 * @param {unknown} s
 * @returns {string | null}
 */
function normalisedSubIdHex(s) {
  if (typeof s !== "string" || s.length !== 32) return null;
  const lower = s.toLowerCase();
  return isHex(lower) ? lower : null;
}

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
