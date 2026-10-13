// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The last step of adding a recovery email (ADR-0075 section 6): seal the
 * email vault when the account needs one, then `enrol/finish`. Over ports,
 * so the rule is testable without a server, a worker or the wasm core.
 *
 * The vault is sealed AFTER `enrol/start` (its salt comes from there) and
 * only for an account without a saved phrase ({@link enrolSealsVault}); a
 * phrase-protected account sends none. The sealed blob is bound to the
 * username's login handle and the address being confirmed.
 */
import { enrolSealsVault } from "./email-flow";
import type { EmailVaultRequest, EmailVaultResult } from "./email-vault-job";

/** The vault could not be sealed on this device: nothing was sent. */
export class EnrolSealError extends Error {
  override name = "EnrolSealError";
  constructor(readonly reason: "no-phrase" | "seal-failed") {
    super(`the email vault was not sealed: ${reason}`);
  }
}

export interface EnrolFinishPorts {
  /** The account's 12 words as this device holds them, or `null`. */
  readonly phrase: () => Promise<string | null>;
  /** Run one vault job off the main thread. */
  readonly seal: (job: EmailVaultRequest) => Promise<EmailVaultResult>;
  /** `POST /recovery-email/enrol/finish`; `sealed` is the 44-byte blob or
   *  `null` for none. Throws the transport's errors. */
  readonly finish: (sealed: Uint8Array | null) => Promise<{ readonly vault: boolean }>;
  /** Whether a thrown error is the server's `409 phrase-saved`. */
  readonly isPhraseSaved: (err: unknown) => boolean;
}

export async function finishEnrolment(
  ports: EnrolFinishPorts,
  input: {
    /** What the device last read from `GET /recovery-email`. */
    readonly phraseSaved: boolean;
    readonly loginHandle: Uint8Array;
    readonly address: string;
    readonly vaultSalt: Uint8Array;
  },
): Promise<{ readonly vault: boolean }> {
  if (!enrolSealsVault(input.phraseSaved)) return ports.finish(null);
  const phrase = await ports.phrase();
  if (phrase === null) throw new EnrolSealError("no-phrase");
  const sealed = await ports.seal({
    kind: "seal",
    phrase,
    loginHandle: input.loginHandle,
    address: input.address,
    vaultSalt: input.vaultSalt,
  });
  if (sealed.kind !== "sealed") throw new EnrolSealError("seal-failed");
  try {
    return await ports.finish(sealed.sealed);
  } catch (err) {
    // A phrase was saved on another device since this one last looked: the
    // account takes no vault. The same code is confirmed without one.
    if (ports.isPhraseSaved(err)) return ports.finish(null);
    throw err;
  }
}
