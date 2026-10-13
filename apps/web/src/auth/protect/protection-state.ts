// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the signed-in account's protection is, as last read from
 * `GET /recovery-email` (ADR-0075): one reactive value for the page, shared
 * by Settings > Account, the "Protect your account" step, the recovery
 * surfaces and the notices. Read at every session landing
 * (`AccountProtectionRuntime`) and after every change this device makes.
 *
 * The read also keeps the per-device "phrase-protected" flag in step
 * (`account-protection.ts`), which is what decides the three-word surfaces
 * while signed out.
 */
import { plainStore } from "./local";
import { createSignal, type Accessor } from "solid-js";

import {
  isPhraseProtected,
  setPhraseProtected,
  type RecoveryEmailStatus,
} from "./account-protection";
import { RecoveryEmailClient } from "./recovery-email-client";

interface Held {
  readonly accountIdB64: string;
  readonly status: RecoveryEmailStatus;
}

const [held, setHeld] = createSignal<Held | null>(null);
const [flagVersion, setFlagVersion] = createSignal(0);

/** The status of `accountIdB64`, or `null` while it is not known. */
export function protectionStatus(accountIdB64: string | null): RecoveryEmailStatus | null {
  const h = held();
  return h !== null && h.accountIdB64 === accountIdB64 ? h.status : null;
}

/** Reactive: whether this device knows the account as phrase-protected. */
export function phraseProtectedHere(accountIdB64: string | null): boolean {
  flagVersion();
  return accountIdB64 !== null && isPhraseProtected(plainStore(), accountIdB64);
}

/** Record what this device just learned or did. */
export function adoptProtectionStatus(accountIdB64: string, status: RecoveryEmailStatus): void {
  setPhraseProtected(plainStore(), accountIdB64, status.phraseSaved);
  setHeld({ accountIdB64, status });
  setFlagVersion((v) => v + 1);
}

/** The account's phrase was saved on this device (the rotation's commit
 *  answered): the flag is set at once, before the next read. */
export function notePhraseSaved(accountIdB64: string): void {
  setPhraseProtected(plainStore(), accountIdB64, true);
  const h = held();
  if (h !== null && h.accountIdB64 === accountIdB64) {
    setHeld({ accountIdB64, status: { ...h.status, phraseSaved: true, vault: false } });
  }
  setFlagVersion((v) => v + 1);
}

let inflight: { readonly accountIdB64: string; readonly task: Promise<RecoveryEmailStatus | null> } | null =
  null;

/**
 * Read the status from the server. Resolves `null` when it could not be
 * read (offline, a server error): the last known value stays. Never throws.
 */
export function refreshProtection(
  accountIdB64: string,
  sessionToken: string,
  client: RecoveryEmailClient = new RecoveryEmailClient(),
): Promise<RecoveryEmailStatus | null> {
  if (inflight !== null && inflight.accountIdB64 === accountIdB64) return inflight.task;
  const task = (async (): Promise<RecoveryEmailStatus | null> => {
    try {
      const status = await client.status(sessionToken);
      adoptProtectionStatus(accountIdB64, status);
      return status;
    } catch {
      return null;
    } finally {
      inflight = null;
    }
  })();
  inflight = { accountIdB64, task };
  return task;
}

/** Forget the held value (sign-out). */
export function dropProtectionStatus(): void {
  setHeld(null);
  setFlagVersion((v) => v + 1);
}

/** For a component: the signed-in account's status as an accessor. */
export function useProtectionStatus(accountIdB64: Accessor<string | null>): Accessor<RecoveryEmailStatus | null> {
  return () => protectionStatus(accountIdB64());
}
