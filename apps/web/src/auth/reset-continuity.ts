// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 sections 1 to 5 - the RESET side of reaching an account after its
 * key reset: what the resetting device keeps so its conversations, groups,
 * channels, profile and history come back by themselves.
 *
 * After the attested publish (`key-reset-owed.ts`) the reset owes one more
 * step, the announcement, kept in its own record ({@link ResetContinuity},
 * `announced`) so that a failure here never withholds the new words the way
 * an owed publish does; it is retried at every session landing:
 *
 *   1. claim the CONTINUITY mailbox (`MailboxKey.continuityFromAccountIdentity`,
 *      claimed with `dsContinuityMailboxSigner`), never the real one;
 *   2. register on it the one reach key a peer that accepted the reset can
 *      derive, `continuityReachSeed(account id, key generation)`, expiring
 *      with the window;
 *   3. sign the binding (the continuity mailbox key and the expiry, signed by
 *      the new identity key) and upload it to discovery beside the bundle.
 *
 * The window is counted from the attestation's `signed_at`, the same instant
 * every peer counts the continuity key's usability from
 * (`reach/continuity.ts`), so the reset device and its peers agree on when it
 * ends. For the window the account's devices read the continuity mailbox
 * beside the real one with the same row handler; after it they stop.
 *
 * ## The previous own key
 *
 * A returned profile or channel is accepted silently only when it carries a
 * signature by the account key the person had BEFORE the reset (ADR-0101
 * sections 7 and 8). The KT log serves an account's latest entry only, so
 * the previous key is read BEFORE the reset is sent: the key the directory
 * serves for the account then, confirmed by the log's latest entry
 * ({@link previousKeyStanding}). A recall knows it outright (the device held
 * the old seed). When it cannot be established nothing returned is accepted
 * silently: a channel return is then shown as an ordinary invitation and a
 * returned profile is dropped.
 *
 * Public values only (a key, an expiry); no seed is stored here.
 */
import type { MarkerStorage } from "./phrase-gate";
import type { LogVerifyStatus } from "../transparency/verify";

/** The window (ADR-0101 section 2, the DS journal retention; chosen, not
 *  measured). Equal to the core's `continuityWindowMs()`. */
export const RESET_CONTINUITY_WINDOW_MS = 30 * 24 * 60 * 60 * 1000;

/** The binding's expiry for a reset signed at `signedAtMs`. */
export function resetContinuityExpiryMs(signedAtMs: number): number {
  return signedAtMs + RESET_CONTINUITY_WINDOW_MS;
}

/** The record one account keeps on the resetting device. */
export interface ResetContinuity {
  readonly v: 1;
  /** Unix ms: the continuity mailbox is read strictly before it. */
  readonly expiresAtMs: number;
  /** Standard base64 of the 32-byte account key of before the reset, or
   *  `null` when it could not be established. */
  readonly previousKeyB64: string | null;
  /** The generation the reset moved the account to (decimal string): the
   *  continuity reach key is derived from it. */
  readonly keyGeneration: string;
  /** The announcement (claim, reach key, binding) landed. */
  readonly announced: boolean;
}

/** The record with the announcement marked done. */
export function withAnnounced(record: ResetContinuity): ResetContinuity {
  return { ...record, announced: true };
}

/** Whether the announcement is still owed at `nowMs` (never after the
 *  window: a binding for a past window is refused by everyone). */
export function announcementOwed(record: ResetContinuity | null, nowMs: number): boolean {
  return record !== null && !record.announced && nowMs < record.expiresAtMs;
}

function markerKey(accountIdB64: string): string {
  return `lakko:reset-continuity:${accountIdB64}`;
}

/** Write (replace) the record. Never throws. */
export function writeResetContinuity(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  record: ResetContinuity,
): void {
  try {
    storage?.setItem(markerKey(accountIdB64), JSON.stringify(record));
  } catch {
    /* storage blocked: the read loop then asks the directory instead */
  }
}

/** The record, or `null` (absent or damaged). */
export function readResetContinuity(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
): ResetContinuity | null {
  try {
    const raw = storage?.getItem(markerKey(accountIdB64));
    if (raw === null || raw === undefined) return null;
    const p = JSON.parse(raw) as Partial<ResetContinuity>;
    if (p.v !== 1 || typeof p.expiresAtMs !== "number" || !Number.isFinite(p.expiresAtMs)) return null;
    const previousKeyB64 = typeof p.previousKeyB64 === "string" ? p.previousKeyB64 : null;
    if (typeof p.keyGeneration !== "string" || !/^[0-9]+$/.test(p.keyGeneration)) return null;
    return {
      v: 1,
      expiresAtMs: p.expiresAtMs,
      previousKeyB64,
      keyGeneration: p.keyGeneration,
      announced: p.announced === true,
    };
  } catch {
    return null;
  }
}

/** Forget it (a wipe, or a later reset that writes a new one). */
export function clearResetContinuity(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(markerKey(accountIdB64));
  } catch {
    /* nothing recorded either */
  }
}

/** Whether the continuity mailbox is read at `nowMs`. */
export function continuityMailboxReadable(record: { readonly expiresAtMs: number } | null, nowMs: number): boolean {
  return record !== null && nowMs < record.expiresAtMs;
}

/**
 * The previous own key a device may rely on, from what it read before the
 * reset was sent:
 *
 * | directory served | log verdict      | previous key |
 * |------------------|------------------|--------------|
 * | none             | -                | none         |
 * | key K            | `confirmed`      | K            |
 * | key K            | `off` (no log pinned in this build: the client's
 * |                  |  posture everywhere is then the directory) | K |
 * | key K            | anything else    | none         |
 * | key K == new key | -                | none (nothing changed) |
 */
export function previousKeyStanding(facts: {
  readonly servedKeyB64: string | null;
  /** `null`: the log could not be asked (a thrown check). */
  readonly logStatus: LogVerifyStatus | null;
  readonly newKeyB64: string;
}): string | null {
  if (facts.servedKeyB64 === null || facts.servedKeyB64 === facts.newKeyB64) return null;
  if (facts.logStatus === "confirmed" || facts.logStatus === "off") return facts.servedKeyB64;
  return null;
}
