// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.2 - "a reset was accepted", as an in-process signal.
 *
 * The identity gate (`contacts/client.ts`) accepts a reset wherever a
 * bundle is fetched; the work that follows (moving the shared conversations
 * onto keys the old devices do not hold, R.5 to R.7) belongs to the chat
 * layer. This is the one seam between them. It carries no UI: nothing that
 * listens may show anything (R.0).
 *
 * Module-level, like the contacts store's own change listeners: every
 * `ContactsClient` instance in the tab reports to the same set.
 */

/** One accepted reset. */
export interface ResetAccepted {
  /** Standard base64 of the 16-byte account id. */
  readonly accountIdB64: string;
  /** The key this device held for the account before the re-pin. */
  readonly previousKeyB64: string;
  readonly newKeyB64: string;
  readonly keyGeneration: number;
  /** The attestation's `signed_at`, UNIX milliseconds. */
  readonly signedAtMs: number;
}

const listeners = new Set<(event: ResetAccepted) => void>();

/** Listen for accepted resets. Returns the unsubscribe. */
export function subscribeResetAccepted(fn: (event: ResetAccepted) => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** Tell every listener. A listener that throws does not stop the others,
 *  and never reaches the gate that accepted the reset. */
export function notifyResetAccepted(event: ResetAccepted): void {
  for (const fn of [...listeners]) {
    try {
      fn(event);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("reset-accepted listener failed", err);
    }
  }
}
