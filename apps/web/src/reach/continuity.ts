// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 "Phase 2 revised", R.4 - the CONTINUITY reach key.
 *
 * A reset changes every reach key and the mailbox pseudonym (they derive
 * from the account seed), and the new identity knows none of its former
 * contacts, so nobody holds a grant for its new mailbox. The continuity key
 * is what lets a peer that ACCEPTED the reset deliver a Welcome there:
 *
 *     continuity seed = continuityReachSeed(account id, key generation)
 *
 * Both sides derive it from public values the peer verified (the account id
 * and the attested generation):
 *
 *   * the RESETTING device registers the key's public half on its new
 *     account mailbox in its reset order, with an expiry (this module's
 *     marker; `reach-service.ts` keeps it registered until then);
 *   * a PEER signs its Welcome with `DsProofSigner.forReach(seed)`
 *     (`mailbox-write.ts`).
 *
 * It is a write permission, not a secret that protects content: anyone who
 * knows the account id and the generation can derive it (R.4 "What it
 * admits"). Once the new identity is in a conversation its profile hands
 * out the ordinary grants and the peer does not use this key again.
 *
 * The marker is `localStorage`, per account, public values only; it is
 * removed with the account's data (`auth/wipe-account.ts`).
 */

/** How long the continuity key stays registered, from the reset's
 *  `signed_at`: the DS journal retention (decision D2). Chosen for parity
 *  with how long an offline peer's traffic survives anyway, not measured. */
export const CONTINUITY_KEY_TTL_MS = 30 * 24 * 60 * 60 * 1000;

/** The expiry the key is registered with. */
export function continuityExpiryMs(signedAtMs: number): number {
  return signedAtMs + CONTINUITY_KEY_TTL_MS;
}

/** What the resetting device remembers about its own last reset. */
export interface ContinuityMarker {
  /** The generation the reset moved the account to. */
  readonly keyGeneration: number;
  /** The attestation's `signed_at`, UNIX milliseconds. */
  readonly signedAtMs: number;
}

/** The slice of `Storage` this needs. */
export interface ContinuityStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/** The marker's key; the account id (standard base64) follows the prefix. */
export const CONTINUITY_MARKER_PREFIX = "lakko:reset-continuity:";

function markerKey(accountIdB64: string): string {
  return `${CONTINUITY_MARKER_PREFIX}${accountIdB64}`;
}

function isMarker(v: unknown): v is ContinuityMarker {
  return (
    typeof v === "object" &&
    v !== null &&
    Number.isSafeInteger((v as ContinuityMarker).keyGeneration) &&
    (v as ContinuityMarker).keyGeneration > 0 &&
    typeof (v as ContinuityMarker).signedAtMs === "number" &&
    Number.isFinite((v as ContinuityMarker).signedAtMs)
  );
}

/** Record this device's own reset (the reset's client order). Never throws:
 *  with storage blocked the key is not registered and former contacts reach
 *  the account only through an open mailbox or a new contact request. */
export function writeContinuityMarker(
  storage: ContinuityStorage | undefined,
  accountIdB64: string,
  marker: ContinuityMarker,
): void {
  try {
    storage?.setItem(markerKey(accountIdB64), JSON.stringify(marker));
  } catch {
    /* storage blocked */
  }
}

/** The marker, or `null` (none, or an unreadable row). */
export function readContinuityMarker(
  storage: ContinuityStorage | undefined,
  accountIdB64: string,
): ContinuityMarker | null {
  try {
    const raw = storage?.getItem(markerKey(accountIdB64));
    if (raw === null || raw === undefined) return null;
    const parsed: unknown = JSON.parse(raw);
    return isMarker(parsed) ? { keyGeneration: parsed.keyGeneration, signedAtMs: parsed.signedAtMs } : null;
  } catch {
    return null;
  }
}

/** Forget the marker (the account's data is wiped). */
export function clearContinuityMarker(
  storage: ContinuityStorage | undefined,
  accountIdB64: string,
): void {
  try {
    storage?.removeItem(markerKey(accountIdB64));
  } catch {
    /* nothing to clear */
  }
}

/** The key the resetting device wants registered now, or `null`. */
export interface WantedContinuityKey {
  readonly keyGeneration: number;
  readonly expiresAtMs: number;
}

/**
 * Whether the continuity key is still to be kept registered:
 *
 * | marker                         | wanted                         |
 * |--------------------------------|--------------------------------|
 * | none                           | no                             |
 * | present, before the expiry     | yes, with that expiry          |
 * | present, at or past the expiry | no (the reach sync removes it) |
 */
export function continuityKeyWanted(
  marker: ContinuityMarker | null,
  nowMs: number,
): WantedContinuityKey | null {
  if (marker === null) return null;
  const expiresAtMs = continuityExpiryMs(marker.signedAtMs);
  if (nowMs >= expiresAtMs) return null;
  return { keyGeneration: marker.keyGeneration, expiresAtMs };
}

/**
 * Whether a PEER still signs a Welcome with the continuity key: inside the
 * same window, counted from the attestation's `signed_at` it verified. Past
 * it the key is no longer registered, and the write would be refused (a
 * peer that returns later cannot re-establish silently, R.4 "Expiry").
 */
export function continuityKeyUsable(signedAtMs: number, nowMs: number): boolean {
  return nowMs < continuityExpiryMs(signedAtMs);
}
