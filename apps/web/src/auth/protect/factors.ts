// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The signed-in account's second ways in (ADR-0073 and ADR-0075, amended
 * 2026-11-26; `docs/ux/identity-recovery.md` 19.1a and 19.1c): a recovery
 * email, a confirmed authenticator app, a passkey. A key reset asks for the
 * password and one of them, so every account with a username keeps at least
 * one; the recovery phrase is not one of the three.
 *
 * The rules are pure. The holder is one reactive value for the page: the
 * email and the authenticator come from `GET /recovery-email`
 * (`protection-state.ts`), the passkey count from the passkey list.
 */
import { createSignal } from "solid-js";

import { registerAccountMemory } from "../account-memory";
import { WebauthnClient } from "../webauthn-client";
import type { RecoveryEmailStatus } from "./account-protection";
import { protectionStatus, refreshProtection } from "./protection-state";

export interface AccountFactors {
  /** The account has a recovery email. */
  readonly email: boolean;
  /** The account has a confirmed authenticator app. */
  readonly totp: boolean;
  /** How many passkeys the account has. */
  readonly passkeys: number;
  /** This server can send mail (the email row exists at all). */
  readonly mailAvailable: boolean;
}

/** A second way in that can be removed (the authenticator cannot be). */
export type RemovableFactor = "email" | "passkey";

/** Whether the account has at least one of the three. */
export function hasSecondFactor(f: AccountFactors): boolean {
  return f.email || f.totp || f.passkeys > 0;
}

/** Whether the authenticator app is the account's only second way in. */
export function totpIsOnlyFactor(f: AccountFactors): boolean {
  return f.totp && !f.email && f.passkeys === 0;
}

/** The factors as they would be after removing `which` (one passkey, or the
 *  email). Removing something the account does not have changes nothing. */
export function factorsAfterRemoving(f: AccountFactors, which: RemovableFactor): AccountFactors {
  return which === "email"
    ? { ...f, email: false }
    : { ...f, passkeys: Math.max(0, f.passkeys - 1) };
}

function has(f: AccountFactors, which: RemovableFactor): boolean {
  return which === "email" ? f.email : f.passkeys > 0;
}

/**
 * Whether removing `which` would leave the account with none of the three:
 * the removal is refused with the app's own dialog (`protect.last_factor`).
 * The server refuses it too (`409 last-factor`). A factor the account does
 * not have removes nothing and is never "the last".
 */
export function removingLeavesNone(f: AccountFactors, which: RemovableFactor): boolean {
  return has(f, which) && !hasSecondFactor(factorsAfterRemoving(f, which));
}

/**
 * Whether removing `which` would leave the authenticator app as the only
 * second way in: the removal's confirmation then carries the
 * `protect.totp_only.remove_notice` line.
 */
export function removingLeavesTotpOnly(f: AccountFactors, which: RemovableFactor): boolean {
  return has(f, which) && !totpIsOnlyFactor(f) && totpIsOnlyFactor(factorsAfterRemoving(f, which));
}

/** The line a removal's confirmation adds, or `null` for none. */
export function removeNoticeKey(
  f: AccountFactors | null,
  which: RemovableFactor,
  phraseSaved: boolean,
): string | null {
  if (f === null || !removingLeavesTotpOnly(f, which)) return null;
  return phraseSaved ? "protect.totp_only.remove_notice_phrase" : "protect.totp_only.remove_notice";
}

/**
 * The authenticator-only warning of 19.1a and 19.1c: shown while the
 * authenticator is the only one of the three; the wording depends on
 * whether a recovery phrase is saved as well. `null`: no warning (also
 * while the factors are not known).
 */
export function totpOnlyWarningKey(f: AccountFactors | null, phraseSaved: boolean): string | null {
  if (f === null || !totpIsOnlyFactor(f)) return null;
  return phraseSaved ? "protect.totp_only.warning_phrase" : "protect.totp_only.warning";
}

/** The machine code of the server's `409` on removing the last factor. */
export const CODE_LAST_FACTOR = "last-factor";

/** Whether a server answer is the last-factor refusal. */
export function isLastFactorRefusal(answer: {
  readonly status: number | null;
  readonly errorCode: string | null;
}): boolean {
  return answer.status === 409 && answer.errorCode === CODE_LAST_FACTOR;
}

/** Put the two reads together. */
export function factorsOf(status: RecoveryEmailStatus, passkeys: number): AccountFactors {
  return {
    email: status.enabled,
    totp: status.totp,
    passkeys: Math.max(0, Math.floor(passkeys)),
    mailAvailable: status.available,
  };
}

// ---------------------------------------------------------------------------
// The holder
// ---------------------------------------------------------------------------

interface HeldCount {
  readonly accountIdB64: string;
  readonly passkeys: number;
}

const [heldCount, setHeldCount] = createSignal<HeldCount | null>(null);

/** Reactive: the factors of `accountIdB64`, or `null` while either read is
 *  not known yet. */
export function accountFactors(accountIdB64: string | null): AccountFactors | null {
  const status = protectionStatus(accountIdB64);
  const count = heldCount();
  if (status === null || count === null || count.accountIdB64 !== accountIdB64) return null;
  return factorsOf(status, count.passkeys);
}

/** Record how many passkeys the account has (the passkey list was just
 *  read, or one was added or removed on this device). */
export function notePasskeyCount(accountIdB64: string, passkeys: number): void {
  setHeldCount({ accountIdB64, passkeys: Math.max(0, Math.floor(passkeys)) });
}

export interface RefreshFactorsDeps {
  /** `GET /recovery-email`, adopted into the page's holder; `null` when it
   *  could not be read. */
  readonly status: (accountIdB64: string, sessionToken: string) => Promise<RecoveryEmailStatus | null>;
  /** How many passkeys the account has; throws when the list is unreadable. */
  readonly passkeyCount: (sessionToken: string) => Promise<number>;
}

const DEFAULT_DEPS: RefreshFactorsDeps = {
  status: (accountIdB64, sessionToken) => refreshProtection(accountIdB64, sessionToken),
  passkeyCount: async (sessionToken) => {
    const client = new WebauthnClient();
    client.setSessionToken(sessionToken);
    return (await client.listCredentials()).credentials.length;
  },
};

let inflight: { readonly accountIdB64: string; readonly task: Promise<AccountFactors | null> } | null = null;

/**
 * Read both facts from the server. Resolves the factors, or `null` when
 * either could not be read (offline, a server error): what was known stays.
 * Never throws.
 */
export function refreshFactors(
  accountIdB64: string,
  sessionToken: string,
  deps: RefreshFactorsDeps = DEFAULT_DEPS,
): Promise<AccountFactors | null> {
  if (deps === DEFAULT_DEPS && inflight !== null && inflight.accountIdB64 === accountIdB64) {
    return inflight.task;
  }
  const run = async (): Promise<AccountFactors | null> => {
    try {
      const [status, passkeys] = await Promise.all([
        deps.status(accountIdB64, sessionToken),
        deps.passkeyCount(sessionToken).catch(() => null),
      ]);
      if (passkeys !== null) notePasskeyCount(accountIdB64, passkeys);
      if (status === null || passkeys === null) return null;
      return factorsOf(status, passkeys);
    } catch {
      return null;
    }
  };
  if (deps !== DEFAULT_DEPS) return run();
  const entry = { accountIdB64, task: run() };
  inflight = entry;
  void entry.task.finally(() => {
    if (inflight === entry) inflight = null;
  });
  return entry.task;
}

// A count held from before Forget device belongs to the wiped account; a
// sign-out drops it too (it is read again at the next landing).
registerAccountMemory(
  "account-factors",
  (ids) => {
    const c = heldCount();
    if (c !== null && ids.includes(c.accountIdB64)) setHeldCount(null);
    if (inflight !== null && ids.includes(inflight.accountIdB64)) inflight = null;
  },
  { signOut: true },
);
