// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 "Phase 2 revised", R.4 - the CONTINUITY reach key's window.
 *
 * A reset changes every reach key and the mailbox (they derive from the
 * account seed), and the new identity knows none of its former contacts, so
 * nobody holds a grant for its new mailbox. The continuity key was what let a
 * peer that ACCEPTED the reset deliver a Welcome there:
 *
 *     continuity seed = continuityReachSeed(account id, key generation)
 *
 * ADR-0078 open question: a mailbox address is no longer derivable from the
 * identity key or the account id, and no carrier names the reset account's
 * new mailbox. So the peer-side write is a recorded no-op
 * (`mailbox-write.ts`), and the resetting device no longer registers the
 * key's public half on its own mailbox (nobody could address a write under
 * it). What stays here is the window a peer reads to decide whether the
 * skipped write would still have been made; the core derivation
 * `continuityReachSeed` stays for the carrier that will replace this.
 */

/** How long the continuity key stays usable, from the reset's
 *  `signed_at`: the DS journal retention (decision D2). Chosen for parity
 *  with how long an offline peer's traffic survives anyway, not measured. */
export const CONTINUITY_KEY_TTL_MS = 30 * 24 * 60 * 60 * 1000;

/** When the key stops being usable. */
export function continuityExpiryMs(signedAtMs: number): number {
  return signedAtMs + CONTINUITY_KEY_TTL_MS;
}

/**
 * Whether a PEER would still sign a Welcome with the continuity key: inside
 * the window, counted from the attestation's `signed_at` it verified (R.4
 * "Expiry"). ADR-0078: only used to record the skipped write.
 */
export function continuityKeyUsable(signedAtMs: number, nowMs: number): boolean {
  return nowMs < continuityExpiryMs(signedAtMs);
}
