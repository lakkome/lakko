// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0066 section 3.5a rule 3 - after a rotation of the account keys the
 * profile envelope is re-sent to every one-to-one and group, because the
 * grants it carries are derived from the account seed: the ones peers hold
 * open the mailbox that was left, and the new mailbox is reachable only with
 * the new ones.
 *
 * What is owed is not a separate list. A conversation's last PUBLISHED grant
 * signature (`grant-signatures.ts`) is recorded only once a publish into it
 * succeeded, and the signature names the account key the grants were derived
 * under; so after a re-key every conversation's recorded signature differs
 * from the current one, and stays different until its profile landed. That
 * difference is the owed set, on the rotating device and on a kept one.
 *
 * On a kept device the profile publisher's own effect sends them (throttled,
 * with its retries). On the ROTATING device it is a step of the owed
 * rotation record (`devices/key-rotation.ts`, `profile`): the step is done
 * only when nothing is owed, and is retried with the record otherwise.
 *
 * `ProfileProvider` installs the sender; this module holds no state else.
 */

/** One conversation as the owed rule reads it. */
export interface ResendConversation {
  readonly groupIdB64: string;
  /** `false` only for a group this device is known to be out of. */
  readonly active: boolean;
  /** The signature last published there, `null` when never. */
  readonly publishedSignature: string | null;
  /** The signature a profile sent there now would carry. */
  readonly currentSignature: string;
}

/** The conversations whose profile (and grants) is still owed. Pure. */
export function owedProfileConversations(conversations: readonly ResendConversation[]): string[] {
  return conversations
    .filter((c) => c.active && c.publishedSignature !== c.currentSignature)
    .map((c) => c.groupIdB64);
}

/** The step's verdict over what is still owed after a send. Pure. */
export function profileResendVerdict(stillOwed: number | null): "done" | "later" {
  // `null`: the publisher or the reach session of the new key is not there
  // yet, so nothing can be said to have been sent.
  return stillOwed === 0 ? "done" : "later";
}

/** Sends the owed profiles and answers how many are still owed, or `null`
 *  when it cannot run yet. */
export type ProfileResend = () => Promise<number | null>;

let installed: ProfileResend | null = null;

export function installProfileResend(resend: ProfileResend | null): void {
  installed = resend;
}

/**
 * The rotation step: send every owed profile now and answer whether the
 * step is done. Never throws; a failure is `later` (the record keeps the
 * step and retries it).
 */
export async function resendProfileAfterRotation(): Promise<"done" | "later"> {
  if (installed === null) return "later";
  try {
    return profileResendVerdict(await installed());
  } catch {
    return "later";
  }
}
