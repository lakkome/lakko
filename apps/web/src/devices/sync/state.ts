// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Cross-device sync state types (Phase 2a3e2b).
 *
 * Reactive surface for the four DeviceGroup application payload
 * kinds the schema in `schemas/mls-app/devicegroup_v1.proto`
 * defines:
 *
 *   * [`DraftState`] — composer text per group (LWW by HLC).
 *   * [`FlagsState`] — pin / mute / archive / unread-override
 *     snapshot per group (LWW by HLC).
 *   * [`ReadCursorState`] — last-read pointer per group (LWW by
 *     HLC).
 *   * [`OrderState`] — flat list of group_ids in display order
 *     (LWW by HLC; one entry per account).
 *
 * Every state record carries its own `updatedAtHlc`; incoming
 * payloads with a strictly-lower HLC than the current snapshot
 * are ignored (last-write-wins semantics from the plan).
 */

/** Per-group draft (composer text). Empty `text` is the
 *  canonical "clear" sentinel — receivers wipe their local
 *  copy when an empty-text update lands. */
export interface DraftState {
  /** Composer text. UTF-8, may be empty. */
  readonly text: string;
  /** Non-empty when the draft is a reply to that message. */
  readonly replyToId: Uint8Array;
  /** HLC of the payload that produced this snapshot. */
  readonly updatedAtHlc: bigint;
}

/** Per-group UI flags. Last-write-wins for the WHOLE struct
 *  (callers always send a complete snapshot, not a diff). */
export interface FlagsState {
  readonly pinned: boolean;
  readonly muted: boolean;
  readonly archived: boolean;
  readonly unreadOverride: boolean;
  readonly updatedAtHlc: bigint;
}

/** Per-group read cursor. */
export interface ReadCursorState {
  /** Message-id the sender claims to have read up to. */
  readonly upToMessageId: Uint8Array;
  /** Wall-clock UNIX millis when the read happened (display
   *  only — `updatedAtHlc` is the source of truth for ordering). */
  readonly readAtMs: bigint;
  readonly updatedAtHlc: bigint;
}

/** Whole-account explicit conversation ordering. */
export interface OrderState {
  /** Conversation group_ids in display order, top-to-bottom. */
  readonly groupIds: readonly Uint8Array[];
  readonly updatedAtHlc: bigint;
}

/**
 * Last-write-wins resolver. Returns `true` when `candidate`
 * should replace `existing` (higher HLC, or no existing record),
 * `false` when it should be dropped (older or duplicate).
 *
 * Strict greater-than: equal HLCs from different devices would
 * be the same logical event, so we drop the duplicate. Real
 * HLC implementations include a node-id tie-breaker, but for
 * Lakko's "one user, ≤ tens of devices" cardinality the strict
 * < comparison is good enough.
 */
export function shouldApply(
  candidateHlc: bigint,
  existingHlc: bigint | undefined,
): boolean {
  if (existingHlc === undefined) return true;
  return candidateHlc > existingHlc;
}

/**
 * Hex-encode a `group_id` byte slice for use as a map key.
 * Stable lowercase output, no `0x` prefix. Mirror of the
 * `DsClient.pseudonymHex` shape so the same id flows through
 * every layer in the same encoding.
 */
export function groupIdHex(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i += 1) {
    out += (bytes[i] as number).toString(16).padStart(2, "0");
  }
  return out;
}
