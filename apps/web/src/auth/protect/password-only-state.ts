// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One fact for the notice "Your chats depend on your password"
 * (`docs/ux/identity-recovery.md` 19.1d): on this page a passkey was just
 * added that signs in but does not keep the chats, so the notice gains its
 * `protect.password_only.passkey_no_seal` line. In memory only: after a
 * reload the passkey's own row says it (`settings.passkeys.no_seal`).
 */
import { createSignal } from "solid-js";

import { registerAccountMemory } from "../account-memory";

const [noSealFor, setNoSealFor] = createSignal<string | null>(null);

/** Record what adding a passkey came to (`auth.passkeySealAdded`). */
export function notePasskeyAdded(accountIdB64: string, keepsChats: boolean): void {
  setNoSealFor(keepsChats ? null : accountIdB64);
}

/** Reactive: the last passkey added on this page for the account does not
 *  keep the chats. */
export function passkeyAddedWithoutSeal(accountIdB64: string | null): boolean {
  const held = noSealFor();
  return held !== null && held === accountIdB64;
}

registerAccountMemory(
  "password-only-passkey-line",
  (ids) => {
    const held = noSealFor();
    if (held !== null && ids.includes(held)) setNoSealFor(null);
  },
  { signOut: true },
);
