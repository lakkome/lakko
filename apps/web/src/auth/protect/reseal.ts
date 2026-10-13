// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The automatic re-seal of the email vault (ADR-0075, "Amended 2026-10-13:
 * the account's devices keep the address").
 *
 * After a key reset, a recall or a rotation the server has dropped the email
 * vault: the account's phrase changed and the old blob opens nothing. A
 * device that holds the address can put the vault back by itself, sealing
 * the CURRENT phrase under the username and that address, with no screen
 * and no notice. A device that does not hold it shows "Add your email
 * again" instead.
 *
 * Pure rules and a runner over ports. Every well-formed request spends one
 * of ten a day at the server, so one attempt is made per reading of the
 * status and nothing is retried in a loop.
 */
import type { EmailVaultRequest, EmailVaultResult } from "./email-vault-job";

/** Whether this device should put the vault back by itself. */
export function resealNeeded(
  enabled: boolean,
  vault: boolean,
  phraseSaved: boolean,
  addressHeld: boolean,
): boolean {
  return enabled && !vault && !phraseSaved && addressHeld;
}

/** What `PUT /recovery-email/vault` came to. */
export type ResealOutcome =
  /** The vault is stored. */
  | "done"
  /** Someone else got there first, or a phrase was saved meanwhile
   *  (`409 vault-exists`, `409 phrase-saved`): as good as done; the status
   *  is read again. */
  | "reread"
  /** `403 address-mismatch`: the address this device holds is not the
   *  account's any more. It is cleared, and the account falls to the
   *  add-again state. */
  | "clear-address"
  /** Anything else (offline, a limit, mail switched off, no stored key):
   *  tried again at the next landing. */
  | "later";

/** Read one answer. `status` is `null` when none arrived. */
export function resealOutcome(status: number | null, errorCode: string | null): ResealOutcome {
  if (status === null) return "later";
  if (status >= 200 && status < 300) return "done";
  if (status === 409 && (errorCode === "vault-exists" || errorCode === "phrase-saved")) return "reread";
  if (status === 403 && errorCode === "address-mismatch") return "clear-address";
  return "later";
}

/** The salt a re-seal chooses: 16 random bytes. */
export const RESEAL_SALT_LEN = 16;

export interface ResealPorts {
  /** The account's 12 words as this device holds them NOW, or `null`. */
  readonly phrase: () => Promise<string | null>;
  /** The username's login handle: its 32 bytes and its wire form. */
  readonly handle: () => Promise<{ readonly bytes: Uint8Array; readonly wire: string }>;
  readonly randomSalt: () => Uint8Array;
  /** Run one vault job off the main thread. */
  readonly seal: (job: EmailVaultRequest) => Promise<EmailVaultResult>;
  /** `PUT /recovery-email/vault`. Resolves the answer's status and code
   *  (`null` status: no answer). Never throws. */
  readonly put: (body: {
    readonly handle: string;
    readonly address: string;
    readonly vaultSalt: Uint8Array;
    readonly sealed: Uint8Array;
  }) => Promise<{ readonly status: number | null; readonly errorCode: string | null }>;
}

/**
 * One attempt. Nothing is sent when this device cannot seal (it holds no
 * phrase, or the seal failed): that reads as `later`.
 */
export async function runReseal(ports: ResealPorts, address: string): Promise<ResealOutcome> {
  let phrase: string | null;
  let handle: { readonly bytes: Uint8Array; readonly wire: string };
  try {
    phrase = await ports.phrase();
    handle = await ports.handle();
  } catch {
    return "later";
  }
  if (phrase === null) return "later";
  const vaultSalt = ports.randomSalt();
  if (vaultSalt.length !== RESEAL_SALT_LEN) return "later";
  let sealed: EmailVaultResult;
  try {
    sealed = await ports.seal({ kind: "seal", phrase, loginHandle: handle.bytes, address, vaultSalt });
  } catch {
    return "later";
  }
  if (sealed.kind !== "sealed") return "later";
  const answer = await ports.put({ handle: handle.wire, address, vaultSalt, sealed: sealed.sealed });
  return resealOutcome(answer.status, answer.errorCode);
}

/**
 * One attempt per (account, key generation, address) in a page: the guard
 * that keeps a status that does not change from spending the daily budget.
 */
export function resealAttemptKey(accountIdB64: string, keyGeneration: string, address: string): string {
  return `${accountIdB64}\n${keyGeneration}\n${address}`;
}
