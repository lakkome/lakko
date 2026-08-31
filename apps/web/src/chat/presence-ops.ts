// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0024 — pure, DOM-free presence logic for the chat timeline
 * (typing indicators + read receipts).
 *
 * These two signals ride the same JSON `Envelope` as the ADR-0023
 * ops, but unlike edit/delete/reactions they never mutate a
 * persistent message — they drive *ephemeral status*. This module
 * owns the security- + correctness-relevant pieces so they can be
 * unit-tested without the SolidJS / wasm surface:
 *
 *   1. [`foldTyping`] — fold an incoming `typing` signal into the
 *      per-`(group, sender)` ephemeral state, applying the
 *      **stale-ignore** re-sync guard (drop a `typing:true` whose
 *      DS `created_at` is older than the expiry window) and stamping
 *      the wall-clock receive time for client-side auto-expiry.
 *
 *   2. [`activeTypers`] — read the set of members currently shown as
 *      typing, dropping any whose last fresh `true` is older than
 *      the expiry window (the auto-expire sweep, also driven by a
 *      timer in the UI).
 *
 *   3. [`foldReadWatermark`] — advance the **monotone** per-`(group,
 *      sender)` `readUpTo` watermark; a lower value never lowers a
 *      higher one, so a full journal re-download (which replays old
 *      read-receipt rows) is idempotent.
 *
 *   4. [`isReadByEveryOther`] — render-side helper: is a given
 *      messageId read by EVERY other member (every member in the
 *      watermark map except ourselves)? Drives the "Read" vs "Sent"
 *      indicator on our own messages.
 *
 *   5. [`shouldSendTyping`] / [`shouldSendReadReceipt`] — the
 *      send-side throttle + monotone decisions, so the send cadence
 *      is testable without a timer.
 *
 * All folds are total + pure — no throw, no I/O — and return a NEW
 * map (replace-to-rerender for SolidJS); the previous map is never
 * mutated, so a stale signal value stays valid.
 *
 * ## Both registries are keyed on a signature key, never a leaf
 *
 * OpenMLS hands a vacated leaf to the next member added, so a
 * leaf-keyed registry silently transfers the departed member's
 * state to the newcomer. For the read watermark — which is monotone
 * and never reset on a membership change — that meant the newcomer
 * inherited the departed member's `readUpTo` outright, and
 * [`isReadByEveryOther`] then reported messages as read by someone
 * who never saw them. Typing has the same shape with a ~6 s
 * lifetime; it is keyed the same way so there is one rule to
 * remember rather than two. See `author-identity.ts`.
 */

import type { MemberIdentity } from "./author-identity";

/**
 * The expiry / stale window for typing signals (ADR-0024 ~6 s). A
 * `typing:true` whose DS `created_at` is older than this on receipt
 * is dropped (re-sync replay guard); a member's typing state
 * auto-clears this long after its last fresh `true`.
 */
export const TYPING_EXPIRY_MS = 6_000;

/**
 * Minimum gap between two outbound `typing:true` signals while the
 * user keeps editing (ADR-0024 ~4 s throttle).
 */
export const TYPING_SEND_THROTTLE_MS = 4_000;

/** One member's ephemeral typing state. */
export interface TypingEntry {
  /** Wall-clock ms (`Date.now()`) when the last fresh `true`
   *  landed. The member shows as typing until `lastTrueAtMs +
   *  TYPING_EXPIRY_MS`. */
  readonly lastTrueAtMs: number;
}

/** The per-group typing registry, keyed on the sender's signature
 *  key (see the module doc — never the MLS leaf). */
export type TypingMap = Map<MemberIdentity, TypingEntry>;

/** The per-group read-watermark registry: sender identity → highest
 *  messageId that member has read (monotone). */
export type ReadWatermarkMap = Map<MemberIdentity, number>;

/**
 * Fold one incoming `typing` signal into the registry, returning a
 * NEW map (replace-to-rerender). `===` to the input when nothing
 * changed, so the caller can skip a needless signal write.
 *
 * Rules (ADR-0024):
 *   - `isTyping === false` → clear the member immediately (an
 *     explicit stop).
 *   - `isTyping === true` but the DS row `createdAtMs` is older than
 *     `TYPING_EXPIRY_MS` before `nowMs` → DROP (re-sync replay
 *     guard; a re-downloaded old `true` must never resurrect).
 *   - `isTyping === true` and fresh → stamp `lastTrueAtMs = nowMs`
 *     (the *receive* wall-clock, NOT the DS timestamp — the UI's
 *     auto-expire counts from when we observed it).
 *
 * @param createdAtMs  The DS row's `created_at` as epoch ms.
 * @param nowMs        `Date.now()` at receive time.
 */
export function foldTyping(
  prev: TypingMap,
  sender: MemberIdentity,
  isTyping: boolean,
  createdAtMs: number,
  nowMs: number,
): TypingMap {
  if (!isTyping) {
    // Explicit stop — clear if present.
    if (!prev.has(sender)) return prev;
    const next = new Map(prev);
    next.delete(sender);
    return next;
  }
  // Stale-ignore: a re-synced old `true` must not resurrect typing.
  if (nowMs - createdAtMs >= TYPING_EXPIRY_MS) return prev;
  const next = new Map(prev);
  next.set(sender, { lastTrueAtMs: nowMs });
  return next;
}

/**
 * The members currently shown as typing at `nowMs` — every entry
 * whose `lastTrueAtMs` is within `TYPING_EXPIRY_MS` of now. Pure
 * read; does not mutate the map (the UI's timer drives the actual
 * prune via [`pruneTyping`]).
 */
export function activeTypers(
  map: TypingMap,
  nowMs: number,
): MemberIdentity[] {
  const out: MemberIdentity[] = [];
  for (const [member, entry] of map) {
    if (nowMs - entry.lastTrueAtMs < TYPING_EXPIRY_MS) out.push(member);
  }
  return out;
}

/**
 * Drop every expired typing entry, returning a NEW map (or `===`
 * the input when nothing expired). The UI's auto-expire interval
 * calls this so an entry whose last `true` aged out vanishes from
 * the indicator even with no further traffic.
 */
export function pruneTyping(map: TypingMap, nowMs: number): TypingMap {
  let anyExpired = false;
  for (const [, entry] of map) {
    if (nowMs - entry.lastTrueAtMs >= TYPING_EXPIRY_MS) {
      anyExpired = true;
      break;
    }
  }
  if (!anyExpired) return map;
  const next: TypingMap = new Map();
  for (const [member, entry] of map) {
    if (nowMs - entry.lastTrueAtMs < TYPING_EXPIRY_MS) next.set(member, entry);
  }
  return next;
}

/**
 * Advance the monotone read watermark for `sender`, returning a NEW
 * map (or `===` the input when the value didn't advance). A
 * lower-or-equal `upTo` never lowers a higher one, so re-sync replay
 * of an older read-receipt row is idempotent (ADR-0024).
 *
 * `sender` is a signature key: the watermark is monotone and is
 * never reset on a membership change, so keying it on a leaf handed
 * a departed member's entire read history to the next member added.
 */
export function foldReadWatermark(
  prev: ReadWatermarkMap,
  sender: MemberIdentity,
  upTo: number,
): ReadWatermarkMap {
  const cur = prev.get(sender);
  if (cur !== undefined && cur >= upTo) return prev;
  const next = new Map(prev);
  next.set(sender, upTo);
  return next;
}

/**
 * Has `messageId` been read by EVERY other member? `true` only when
 * at least one *other* member has a watermark AND every other
 * member's watermark is `>= messageId`.
 *
 * `own` is excluded — our own read state is implicit. When the map
 * holds no other member's watermark at all (no peer has reported a
 * read yet) the answer is `false` → the message stays "Sent". An
 * `undefined` `own` excludes nobody, which is the fail-closed
 * direction here: it can only make the answer stricter.
 *
 * For a 1×1 chat the map has at most one other member, so this
 * reduces to "the other person's watermark >= messageId".
 */
export function isReadByEveryOther(
  map: ReadWatermarkMap,
  messageId: number,
  own: MemberIdentity | undefined,
): boolean {
  let sawOther = false;
  for (const [member, upTo] of map) {
    if (member === own) continue;
    sawOther = true;
    if (upTo < messageId) return false;
  }
  return sawOther;
}

/**
 * Send-side typing throttle decision (pure). Returns `true` when a
 * fresh `typing:true` should be published now:
 *   - the user is actively editing (`isEditing`), AND
 *   - at least `TYPING_SEND_THROTTLE_MS` has elapsed since the last
 *     published `true` (`lastSentTrueAtMs === null` ⇒ never sent).
 *
 * The `false` clear (on blur / on send) is published unconditionally
 * by the caller — it is not throttled here (ADR-0024 says do NOT
 * spam `false`, which the caller enforces by sending it once on the
 * stop transition, not on a timer).
 */
export function shouldSendTyping(
  isEditing: boolean,
  lastSentTrueAtMs: number | null,
  nowMs: number,
): boolean {
  if (!isEditing) return false;
  if (lastSentTrueAtMs === null) return true;
  return nowMs - lastSentTrueAtMs >= TYPING_SEND_THROTTLE_MS;
}

/**
 * Send-side read-receipt decision (pure + monotone). Returns the
 * messageId to publish when the highest received id advances past
 * our own last-published watermark, else `null` (nothing to send).
 *
 * `highestReceivedId === null` ⇒ no server-assigned message yet →
 * `null`. `lastPublished === null` ⇒ never published → send the
 * highest. Otherwise send only when it strictly advances.
 */
export function shouldSendReadReceipt(
  highestReceivedId: number | null,
  lastPublished: number | null,
): number | null {
  if (highestReceivedId === null) return null;
  if (lastPublished !== null && highestReceivedId <= lastPublished) return null;
  return highestReceivedId;
}
