// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The signed-in moderator, in memory only.
 *
 * The bearer token and the report secret (the security key's PRF output,
 * ADR-0089 section 3) live in this module's signal and nowhere else: no
 * storage, no cookie, no URL. Reloading the page signs out; that is the
 * intended cost of never writing the secret down.
 */

import { createSignal } from "solid-js";

export interface PanelSession {
  readonly token: string;
  readonly expiresAtMs: number;
  readonly moderatorId: Uint8Array;
  readonly moderatorIdB64: string;
  readonly name: string;
  /** The 32-byte PRF output the report and appeal keys are derived from. */
  readonly reportSecret: Uint8Array;
  readonly rpId: string;
  /** This moderator's WebAuthn credential id, for decision signatures. */
  readonly credentialId: Uint8Array;
}

const [session, setSessionSignal] = createSignal<PanelSession | null>(null);

export { session };

export function setSession(s: PanelSession): void {
  setSessionSignal(s);
}

/** Forget everything, overwriting the report secret first. */
export function signOut(): void {
  const s = session();
  if (s !== null) s.reportSecret.fill(0);
  setSessionSignal(null);
}

/** The session, or a throw for code that runs only when signed in. */
export function requireSession(): PanelSession {
  const s = session();
  if (s === null) throw new Error("You were signed out. Sign in again with your security key.");
  return s;
}
