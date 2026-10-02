// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "This device still owes the server the new recovery credential."
 *
 * The ADR-0017 fresh identity (`AuthContext.mintFreshIdentity`) mints a new
 * recovery phrase. The account's recovery credential on the server was
 * registered from the OLD phrase, so the new 12 words sign in nowhere until
 * `AuthClient.replaceRecoveryCredential` has run. That call comes after the
 * new identity is published and can fail on its own (offline, a limit).
 *
 * The marker is set before the first attempt and cleared only by a
 * successful replace, so the debt survives a reload: the next time a session
 * for the account lands on this device (which holds the new seed), the
 * replace is tried again in the background. Only the fact is stored, in
 * plain storage; the seed stays under the device key.
 */
import type { MarkerStorage } from "./phrase-gate";

function markerKey(accountIdB64: string): string {
  return `lakko:recovery-credential-owed:${accountIdB64}`;
}

export function markRecoveryCredentialOwed(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
): void {
  try {
    storage?.setItem(markerKey(accountIdB64), "1");
  } catch {
    // Storage blocked: the in-memory flag still holds for this page.
  }
}

export function isRecoveryCredentialOwed(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
): boolean {
  try {
    return storage?.getItem(markerKey(accountIdB64)) === "1";
  } catch {
    return false;
  }
}

export function clearRecoveryCredentialOwed(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
): void {
  try {
    storage?.removeItem(markerKey(accountIdB64));
  } catch {
    // Storage blocked: nothing was recorded either.
  }
}
