// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What `GET /recovery-email` last said about the signed-in account
 * (ADR-0075 as amended 2026-11-26: `{available, enabled, totp}`): one
 * reactive value for the page, shared by Settings > Account, the "Protect
 * your account" step, the recovery surfaces and the notices. Read at every
 * session landing (`AccountNotices`) and after every change this device
 * makes.
 *
 * The per-device "phrase-protected" flag (`account-protection.ts`), which
 * decides the three-word surfaces while signed out, is fed from the
 * account's published protection ({@link syncPhraseFlagFromSeals}): the
 * server keeps no "phrase saved" flag any more.
 */
import { plainStore } from "./local";
import { createSignal, type Accessor } from "solid-js";

import {
  isPhraseProtected,
  markProtectionLooked,
  protectionLookedBefore,
  setPhraseProtected,
  type RecoveryEmailStatus,
} from "./account-protection";
import { phraseSavedElsewhere } from "./notices";
import { RecoveryEmailClient } from "./recovery-email-client";
import { registerAccountMemory } from "../account-memory";
import { isSignupPhraseOwed, type MarkerStorage } from "../phrase-gate";
import type { OwnSealFacts } from "../seal/own-seals";

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
  setHeld({ accountIdB64, status });
}

/** The account's phrase was saved on this device (its protection was
 *  published with the phrase in it): the flag is set at once. */
export function notePhraseSaved(accountIdB64: string): void {
  setPhraseProtected(plainStore(), accountIdB64, true);
  markProtectionLooked(plainStore(), accountIdB64);
  setFlagVersion((v) => v + 1);
}

/**
 * What the per-device "phrase-protected" flag becomes when the account's
 * published protection is read. Pure.
 *
 *   - unknown facts change nothing;
 *   - a listed phrase sets it;
 *   - no listed phrase clears it, EXCEPT while new recovery words are owed
 *     on this device: after a key reset or a rotation the new protection
 *     lists no phrase yet, and the flag is what decides that the words are
 *     shown at all (`newWordsShown`), so it must outlive the change until
 *     they were acknowledged.
 */
export function phraseFlagAfterSeals(args: {
  readonly facts: OwnSealFacts | null;
  readonly flag: boolean;
  readonly newWordsOwed: boolean;
}): boolean {
  if (args.facts === null) return args.flag;
  if (args.facts.phrase) return true;
  return args.newWordsOwed ? args.flag : false;
}

/**
 * Keep the per-device flag in step with the published protection. Resolves
 * whether a recovery phrase was saved on ANOTHER device since this one last
 * looked (`notices.ts`, `phraseSavedElsewhere`): the caller shows the
 * one-line notice once.
 */
export function syncPhraseFlagFromSeals(
  accountIdB64: string,
  facts: OwnSealFacts | null,
  storage: MarkerStorage | undefined = plainStore(),
): boolean {
  if (facts === null) return false;
  const flag = isPhraseProtected(storage, accountIdB64);
  const savedElsewhere = phraseSavedElsewhere({
    lookedBefore: protectionLookedBefore(storage, accountIdB64),
    flag,
    phraseListed: facts.phrase,
  });
  markProtectionLooked(storage, accountIdB64);
  const next = phraseFlagAfterSeals({
    facts,
    flag,
    newWordsOwed: isSignupPhraseOwed(storage, accountIdB64),
  });
  if (next !== flag) {
    setPhraseProtected(storage, accountIdB64, next);
    setFlagVersion((v) => v + 1);
  }
  return savedElsewhere;
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

// The status the server last gave for an account: shown, and the source of
// the "phrase-protected" flag's next write. A status held from before Forget
// device belongs to the wiped account; a sign-out drops it too (it is
// re-asked at the next session). A refresh in flight for the account is
// forgotten so the next one asks again.
registerAccountMemory(
  "protection-status",
  (ids) => {
    const h = held();
    if (h !== null && ids.includes(h.accountIdB64)) setHeld(null);
    if (inflight !== null && ids.includes(inflight.accountIdB64)) inflight = null;
    setFlagVersion((v) => v + 1);
  },
  { signOut: true },
);

/** For a component: the signed-in account's status as an accessor. */
export function useProtectionStatus(accountIdB64: Accessor<string | null>): Accessor<RecoveryEmailStatus | null> {
  return () => protectionStatus(accountIdB64());
}
