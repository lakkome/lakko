// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 — the grant signature last published per conversation, by
 * account (versions only, e.g. `member:3|group:1`; no key material). Kept so
 * a profile is republished into a conversation only when its grant versions
 * changed. `localStorage`, one row per account; every access is guarded, and
 * a failure costs one republish next session.
 */

const GRANT_SIGNATURE_KEY = "lakko.reach.profile-grants.v1.";

function read(account: string): Record<string, string> {
  const raw = globalThis.localStorage?.getItem(`${GRANT_SIGNATURE_KEY}${account}`);
  return raw === null || raw === undefined ? {} : (JSON.parse(raw) as Record<string, string>);
}

/** The signature last published into `groupIdB64`, or `null`. */
export function grantSignatureOf(account: string, groupIdB64: string): string | null {
  try {
    const map = read(account);
    return typeof map[groupIdB64] === "string" ? map[groupIdB64] : null;
  } catch {
    return null;
  }
}

/** Record the signature just published into `groupIdB64`. */
export function recordGrantSignature(account: string, groupIdB64: string, signature: string): void {
  try {
    const map = read(account);
    map[groupIdB64] = signature;
    globalThis.localStorage?.setItem(`${GRANT_SIGNATURE_KEY}${account}`, JSON.stringify(map));
  } catch {
    /* storage unavailable: the profile is republished next session */
  }
}

/** Drop the account's row (Forget device, Delete account). */
export function clearGrantSignatures(account: string): void {
  try {
    globalThis.localStorage?.removeItem(`${GRANT_SIGNATURE_KEY}${account}`);
  } catch {
    /* storage blocked: nothing was stored either */
  }
}
