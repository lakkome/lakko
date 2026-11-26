// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.4 and ADR-0101 sections 1 to 6 - reaching an account after its
 * key reset, the PEER side.
 *
 * A reset changes every reach key and the mailbox (they derive from the
 * account seed), and the new identity knows none of its former contacts, so
 * nobody holds a grant for its new mailbox. ADR-0101: the resetting device
 * announces a second mailbox, the CONTINUITY mailbox, in a 105-byte binding
 * signed by its new identity key, served beside its identity bundle for a
 * window; it registers on that mailbox only the continuity reach key
 *
 *     continuity seed = continuityReachSeed(account id, key generation)
 *
 * which a peer that ACCEPTED the reset (ADR-0073 R.2) derives too. So the
 * peer:
 *
 *   1. verifies the binding under the attested new key it pinned, at its own
 *      clock ({@link peerContinuityTarget}, the core's
 *      `verifyContinuityBinding`), and keeps the verified mailbox key with
 *      its expiry ({@link rememberContinuityTarget});
 *   2. writes the successor's Welcome (and anything else it owes the person)
 *      to `mailboxAddress(continuity mailbox key)`, sealed to that key and
 *      signed with the continuity reach key (`mailbox-write.ts`, candidate
 *      kind `continuity`), after every ordinary grant it holds;
 *   3. stops at expiry: a binding past it is never used, and a peer that
 *      comes back after the window finds none (ADR-0101 section 6).
 *
 * The rules are pure; the store is `localStorage`, per signed-in account,
 * holding PUBLIC values only (a mailbox public key, an expiry).
 */

/** How long the continuity key stays usable, from the reset's
 *  `signed_at`: the DS journal retention (decision D2). Chosen for parity
 *  with how long an offline peer's traffic survives anyway, not measured.
 *  Equal to the core's `continuityWindowMs()`. */
export const CONTINUITY_KEY_TTL_MS = 30 * 24 * 60 * 60 * 1000;

/** When the key stops being usable. */
export function continuityExpiryMs(signedAtMs: number): number {
  return signedAtMs + CONTINUITY_KEY_TTL_MS;
}

/**
 * Whether a PEER would still sign a Welcome with the continuity key: inside
 * the window, counted from the attestation's `signed_at` it verified (R.4
 * "Expiry").
 */
export function continuityKeyUsable(signedAtMs: number, nowMs: number): boolean {
  return nowMs < continuityExpiryMs(signedAtMs);
}

// ----------------------------------------------------------------------------
// The binding (ADR-0101 section 2)
// ----------------------------------------------------------------------------

/** The binding's fixed length: `version(1) ‖ continuity_mailbox_pk(32) ‖
 *  expires_at_ms(8 BE) ‖ sig(64)`. */
export const CONTINUITY_BINDING_LEN = 105;

/** The binding's `expires_at_ms`, read off the layout (NOT a verification:
 *  the caller verifies before reading this), or `null` for a wrong length. */
export function continuityBindingExpiryMs(binding: Uint8Array): number | null {
  if (binding.length !== CONTINUITY_BINDING_LEN) return null;
  const view = new DataView(binding.buffer, binding.byteOffset + 33, 8);
  const ms = Number(view.getBigUint64(0, false));
  return Number.isSafeInteger(ms) ? ms : null;
}

/** The core's verifier, structurally (`verifyContinuityBinding`): returns the
 *  32-byte continuity mailbox key, throws on any refusal. */
export type ContinuityBindingVerifier = (
  binding: Uint8Array,
  identityPk: Uint8Array,
  nowMs: bigint,
) => Uint8Array;

/** A verified continuity mailbox. */
export interface ContinuityTarget {
  /** Standard base64 of the 32-byte continuity mailbox public key. */
  readonly mailboxPkB64: string;
  /** Unix ms; the target is not used at or after it. */
  readonly expiresAtMs: number;
}

/**
 * The continuity mailbox a served binding names, verified under the attested
 * new key `attestedKey` at `nowMs`, or `null` (absent, malformed, signed by
 * another key, expired, expiry too far). Never throws.
 */
export function peerContinuityTarget(
  bindingB64: string | undefined | null,
  attestedKey: Uint8Array,
  nowMs: number,
  verify: ContinuityBindingVerifier,
  decodeB64: (s: string) => Uint8Array,
  encodeB64: (b: Uint8Array) => string,
): ContinuityTarget | null {
  if (bindingB64 === undefined || bindingB64 === null || bindingB64.length === 0) return null;
  let binding: Uint8Array;
  try {
    binding = decodeB64(bindingB64);
  } catch {
    return null;
  }
  const expiresAtMs = continuityBindingExpiryMs(binding);
  if (expiresAtMs === null || expiresAtMs <= nowMs) return null;
  try {
    const mailboxPk = verify(binding, attestedKey, BigInt(nowMs));
    if (mailboxPk.length !== 32) return null;
    return { mailboxPkB64: encodeB64(mailboxPk), expiresAtMs };
  } catch {
    return null;
  }
}

/** Whether a held target may still be written to at `nowMs`. */
export function continuityTargetLive(target: ContinuityTarget | null | undefined, nowMs: number): boolean {
  return target !== null && target !== undefined && nowMs < target.expiresAtMs;
}

/**
 * ADR-0101 section 4 - `newIdentityReachable`: true when this device holds
 * an ordinary way in (a grant or a delegation the write plan offers), or a
 * verified, unexpired continuity binding AND the accepted reset's continuity
 * key is still usable (the two windows are counted from different clocks:
 * the binding's own expiry, the attestation's `signed_at`).
 */
export function newIdentityReachableVerdict(facts: {
  readonly ordinaryCandidates: number;
  readonly continuity: ContinuityTarget | null;
  readonly resetSignedAtMs: number | null;
  readonly nowMs: number;
}): boolean {
  if (facts.ordinaryCandidates > 0) return true;
  if (facts.resetSignedAtMs === null) return false;
  return (
    continuityTargetLive(facts.continuity, facts.nowMs) &&
    continuityKeyUsable(facts.resetSignedAtMs, facts.nowMs)
  );
}

// ----------------------------------------------------------------------------
// The store (per signed-in account, public values)
// ----------------------------------------------------------------------------

interface KeyValueStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function storage(): KeyValueStorage | undefined {
  try {
    return globalThis.localStorage ?? undefined;
  } catch {
    return undefined;
  }
}

let owner: string | null = null;

/** Bind the store to the signed-in account (`null` on sign-out). */
export function bindContinuityTargets(accountIdB64: string | null): void {
  owner = accountIdB64;
}

function keyOf(accountIdB64: string): string {
  return `lakko:continuity-targets:${accountIdB64}`;
}

function loadAll(s: KeyValueStorage | undefined, account: string): Record<string, ContinuityTarget> {
  try {
    const raw = s?.getItem(keyOf(account));
    if (raw === null || raw === undefined) return {};
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const out: Record<string, ContinuityTarget> = {};
    for (const [k, v] of Object.entries(parsed)) {
      const t = v as Partial<ContinuityTarget>;
      if (typeof t.mailboxPkB64 === "string" && typeof t.expiresAtMs === "number") {
        out[k] = { mailboxPkB64: t.mailboxPkB64, expiresAtMs: t.expiresAtMs };
      }
    }
    return out;
  } catch {
    return {};
  }
}

/** Keep (or, with `null`, forget) the verified continuity mailbox of the
 *  person whose NEW account key is `newKeyB64`. Expired entries are dropped
 *  on every write. */
export function rememberContinuityTarget(
  newKeyB64: string,
  target: ContinuityTarget | null,
  nowMs: number = Date.now(),
  s: KeyValueStorage | undefined = storage(),
  account: string | null = owner,
): void {
  if (account === null) return;
  const all = loadAll(s, account);
  for (const [k, v] of Object.entries(all)) if (v.expiresAtMs <= nowMs) delete all[k];
  if (target === null) delete all[newKeyB64];
  else all[newKeyB64] = target;
  try {
    s?.setItem(keyOf(account), JSON.stringify(all));
  } catch {
    /* storage blocked: the next reachability check fetches it again */
  }
}

/** The live continuity mailbox held for `newKeyB64`, or `null`. */
export function continuityTargetOf(
  newKeyB64: string,
  nowMs: number = Date.now(),
  s: KeyValueStorage | undefined = storage(),
  account: string | null = owner,
): ContinuityTarget | null {
  if (account === null) return null;
  const t = loadAll(s, account)[newKeyB64];
  return continuityTargetLive(t, nowMs) ? (t ?? null) : null;
}

/** Sign-out / wipe: drop the account's row. */
export function clearContinuityTargets(accountIdB64: string, s: KeyValueStorage | undefined = storage()): void {
  try {
    s?.removeItem(keyOf(accountIdB64));
  } catch {
    /* nothing recorded either */
  }
}
