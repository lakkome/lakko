// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Call rows in the history bundle (`docs/ux/calls.md` section 6, history
 * placeholder version 7).
 *
 * Web keeps call rows in their own log (`call-log-store.ts`), not in the
 * message log, so the history bundle carries them as placeholder rows with a
 * call record and the joining device files them back into its call log. This
 * module is that translation, plus the write into the joiner's call log.
 *
 * ## The ids
 *
 * A bundled row carries a `message_id` (u64), and a call has none: its id is
 * the 16-byte call id, which does not fit. So:
 *
 * - **On the wire** a call row's `message_id` is the top bit set over 63 bits
 *   of the call id (its first 8 bytes, in hex order): {@link callHistoryMessageId}.
 *   The top bit keeps it apart from every Delivery Service id (positive and
 *   below 2^63), and read as a signed 64-bit number it is negative, which is
 *   how iOS and Android number their own local call rows. It is the same for a
 *   call however many times the bundle is built.
 * - **In the joiner's call log** the row's id is `h-<message_id in decimal>`
 *   ({@link historyCallEntryId}). A live call id is 32 lower-case hex
 *   characters, so `h-` can never collide with one, and re-applying a bundle
 *   (or the same call arriving again through a device that itself got it from
 *   a bundle) folds onto the same row: the call log is keyed by id. A row with
 *   an `h-` id is sent on with the message id it came with.
 *
 * ## The seat
 *
 * A call row's outcome is from the seat of the account that recorded it
 * (`Outgoing call` / `Incoming call`), so only a bundle between devices of one
 * account may carry them. The pairing bundle is; a gap bundle (ADR-0091) is
 * sealed by whoever re-admitted the device and carries none.
 */

import type { DeviceCipher } from "../auth/device-cipher";
import type { CallRecordOutcome } from "../../wasm/lakko_ffi_wasm";
import type { Wasm } from "../wasm";
import type { CallLogEntry, CallOutcome } from "./call-log";
import { IdbCallLogBackend, loadCallLogRows } from "./call-log-idb";
import { mergeCallLogEntries, type CallLogData, type CallLogStore } from "./call-log-store";

/** The prefix of a call-log id that came from a history bundle. */
export const HISTORY_CALL_ID_PREFIX = "h-";

const TOP_BIT = 0x8000_0000_0000_0000n;
const LOW_63 = 0x7fff_ffff_ffff_ffffn;

/** The wire outcome of a web outcome (by name). */
export function callRecordOutcomeOf(wasm: Wasm, outcome: CallOutcome): CallRecordOutcome {
  switch (outcome) {
    case "outgoing":
      return wasm.CallRecordOutcome.Outgoing;
    case "incoming":
      return wasm.CallRecordOutcome.Incoming;
    case "missed":
      return wasm.CallRecordOutcome.Missed;
    case "declined":
      return wasm.CallRecordOutcome.Declined;
    case "failed":
      return wasm.CallRecordOutcome.Failed;
  }
}

/** The web outcome of a wire outcome, or `null` for a value this build does
 *  not know. */
export function callOutcomeFromRecord(wasm: Wasm, outcome: CallRecordOutcome): CallOutcome | null {
  switch (outcome) {
    case wasm.CallRecordOutcome.Outgoing:
      return "outgoing";
    case wasm.CallRecordOutcome.Incoming:
      return "incoming";
    case wasm.CallRecordOutcome.Missed:
      return "missed";
    case wasm.CallRecordOutcome.Declined:
      return "declined";
    case wasm.CallRecordOutcome.Failed:
      return "failed";
    default:
      return null;
  }
}

/** Whole seconds of media for the wire: `undefined` when none flowed. */
export function callDurationSecsForBundle(entry: CallLogEntry): number | undefined {
  if (entry.durationMs === null || entry.durationMs < 0) return undefined;
  if (entry.outcome !== "outgoing" && entry.outcome !== "incoming") return undefined;
  return Math.min(0xffff_ffff, Math.floor(entry.durationMs / 1000));
}

/** 64-bit FNV-1a of a string, for an id that is neither a call id nor a
 *  history id (never written by this build; kept deterministic anyway). */
function fnv1a64(s: string): bigint {
  let h = 0xcbf2_9ce4_8422_2325n;
  for (let i = 0; i < s.length; i += 1) {
    h ^= BigInt(s.charCodeAt(i) & 0xffff);
    h = (h * 0x0000_0100_0000_01b3n) & 0xffff_ffff_ffff_ffffn;
  }
  return h;
}

/**
 * The `message_id` a call row carries in the bundle. Deterministic in the
 * call-log id; always has the top bit set (see the module doc).
 */
export function callHistoryMessageId(entryId: string): bigint {
  if (entryId.startsWith(HISTORY_CALL_ID_PREFIX)) {
    const digits = entryId.slice(HISTORY_CALL_ID_PREFIX.length);
    if (/^[0-9]{1,20}$/.test(digits)) {
      const v = BigInt(digits);
      if (v >= TOP_BIT && v <= 0xffff_ffff_ffff_ffffn) return v;
    }
  }
  if (/^[0-9a-f]{16,}$/i.test(entryId)) {
    return TOP_BIT | (BigInt(`0x${entryId.slice(0, 16)}`) & LOW_63);
  }
  return TOP_BIT | (fnv1a64(entryId) & LOW_63);
}

/** The joiner's call-log id for a call row the bundle carried under
 *  `messageId`. */
export function historyCallEntryId(messageId: bigint): string {
  return `${HISTORY_CALL_ID_PREFIX}${messageId.toString()}`;
}

/**
 * The call-log entry a bundled call row describes, or `null` when the row is
 * not usable (an outcome this build does not know, no time).
 */
export function callEntryFromHistory(
  wasm: Wasm,
  messageId: bigint,
  createdAt: string,
  outcome: CallRecordOutcome,
  video: boolean | undefined,
  durationSecs: number | undefined,
): CallLogEntry | null {
  const mapped = callOutcomeFromRecord(wasm, outcome);
  if (mapped === null) return null;
  const startedAtMs = Date.parse(createdAt);
  if (!Number.isFinite(startedAtMs)) return null;
  const durationMs =
    (mapped === "outgoing" || mapped === "incoming") && durationSecs !== undefined && durationSecs >= 0
      ? durationSecs * 1000
      : null;
  return {
    id: historyCallEntryId(messageId),
    outcome: mapped,
    video: video === true,
    startedAtMs,
    durationMs,
  };
}

// ---------------------------------------------------------------------------
// The live store, and the joiner's write
// ---------------------------------------------------------------------------

/** The session's call-log store (`CallLogContext.tsx`), when one is mounted. */
const liveStores = new Set<CallLogStore>();

/** Make `store` the target of history writes while it is bound. Returns the
 *  unregister function. */
export function registerLiveCallLogStore(store: CallLogStore): () => void {
  liveStores.add(store);
  return () => {
    liveStores.delete(store);
  };
}

function liveStoreFor(accountId: string): CallLogStore | null {
  for (const store of liveStores) {
    if (store.boundAccount() === accountId) return store;
  }
  return null;
}

/** One account's call rows as stored on this device, for the sender. */
export function loadCallLogForBundle(
  accountId: string,
  deviceKey: DeviceCipher,
): Promise<CallLogData | null> {
  return loadCallLogRows(accountId, deviceKey);
}

/**
 * File the call rows a history bundle carried into `accountId`'s call log,
 * keyed by group id in standard base64. Returns how many rows were new.
 *
 * When the session's store is bound to the account, the rows go through it
 * (it writes the whole row on its next save, so writing the row here too
 * would race it). Otherwise the stored row is read, merged and written, under
 * the account store gate (`IdbCallLogBackend.save`), and the rows are also
 * handed to a store that bound meanwhile. Idempotent: the log is keyed by id.
 */
export async function fileHistoryCallEntries(
  accountId: string,
  deviceKey: DeviceCipher,
  byGroup: ReadonlyMap<string, readonly CallLogEntry[]>,
): Promise<number> {
  if (byGroup.size === 0) return 0;
  const live = liveStoreFor(accountId);
  if (live !== null) {
    let added = 0;
    for (const [groupIdB64, entries] of byGroup) {
      for (const entry of entries) {
        const had = live.of(groupIdB64).some((e) => e.id === entry.id);
        if (live.record(accountId, groupIdB64, entry) && !had) added += 1;
      }
    }
    return added;
  }
  const backend = new IdbCallLogBackend(async () => deviceKey);
  const current = await backend.load(accountId);
  const groups = new Map(current?.groups ?? []);
  let added = 0;
  for (const [groupIdB64, entries] of byGroup) {
    if (groupIdB64.length === 0) continue;
    const prev = groups.get(groupIdB64) ?? [];
    const known = new Set(prev.map((e) => e.id));
    for (const entry of entries) if (!known.has(entry.id)) added += 1;
    groups.set(groupIdB64, mergeCallLogEntries(prev, entries));
  }
  await backend.save(accountId, { groups, standalone: current?.standalone ?? [] });
  const bound = liveStoreFor(accountId);
  if (bound !== null) {
    for (const [groupIdB64, entries] of byGroup) {
      for (const entry of entries) bound.record(accountId, groupIdB64, entry);
    }
  }
  return added;
}
