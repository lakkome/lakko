// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A passkey that restores keys (ADR-0072 decision 6).
 *
 * A passkey signs the person in; with the WebAuthn PRF extension it can also
 * carry the account's keys. The PRF output for a fixed salt is a stable
 * 32-byte secret per credential that follows a synced passkey to every
 * device the passkey syncs to. The client seals the phrase entropy under a
 * key derived from it (`passkeyVaultSeal` in wasm) and stores the blob
 * server-side per credential; a passkey sign-in on a new device asks for the
 * same PRF evaluation, fetches the blob, opens it into the full 12-word
 * phrase and hands it to the ordinary phrase path, which checks it against
 * the account's published identity (fail-closed).
 *
 * Every passkey stores the blob wherever the platform gives a PRF output
 * (docs/ux/identity-recovery.md section 10.2, amended 2026-11-11: there is
 * no choice any more); the cost, that key safety then also rests on whoever
 * syncs the passkey, is stated in ADR-0072 decision 6. The store itself is
 * `auth/passkey-keys-store.ts`. The PRF output never leaves this device:
 * `webauthn-client.ts` strips it from what the server receives.
 *
 * Authenticator support for PRF is uneven. No PRF output means the passkey
 * signs in only, and the UI says so.
 */
import type { AuthClient } from "../auth/client";
import { AuthHttpError } from "../auth/client";
import type { Wasm } from "../wasm";

/** The PRF salt every Lakko client evaluates. Fixed by design: the output
 *  is already per-credential, and a per-account salt would have to be
 *  fetched before the login that needs it. */
export const PASSKEY_PRF_SALT = new TextEncoder().encode("lakko/passkey-vault/prf/v1");

/** Seal `phrase` under the PRF output and store it for `credentialId`.
 *  Returns false when the server has no vault key. */
export async function enrollPasskeyVault(args: {
  readonly wasm: Wasm;
  readonly client: AuthClient;
  readonly sessionToken: string;
  readonly credentialId: Uint8Array;
  readonly prfOutput: Uint8Array;
  readonly phrase: string;
}): Promise<boolean> {
  const mnemonic = args.wasm.RecoveryMnemonic.fromPhrase(args.phrase);
  try {
    const sealed = args.wasm.passkeyVaultSeal(args.prfOutput, mnemonic);
    try {
      await args.client.passkeyVaultPut(args.sessionToken, args.credentialId, sealed);
      return true;
    } catch (err) {
      if (err instanceof AuthHttpError && err.status === 503) return false;
      throw err;
    }
  } finally {
    mnemonic.free();
  }
}

/** Fetch the blob for `credentialId` and open it with the PRF output.
 *  `null` when no blob is stored (or the server has no vault key); throws
 *  `PasskeyVaultUnusableError` when a stored blob does not open (a tampered
 *  blob or a PRF output that is not this credential's). */
export async function openPasskeyVault(args: {
  readonly wasm: Wasm;
  readonly client: AuthClient;
  readonly sessionToken: string;
  readonly credentialId: Uint8Array;
  readonly prfOutput: Uint8Array;
}): Promise<string | null> {
  let sealed: Uint8Array | null;
  try {
    sealed = await args.client.passkeyVaultGet(args.sessionToken, args.credentialId);
  } catch (err) {
    if (err instanceof AuthHttpError && err.status === 503) return null;
    throw err;
  }
  if (sealed === null) return null;
  let mnemonic;
  try {
    mnemonic = args.wasm.passkeyVaultOpen(args.prfOutput, sealed);
  } catch (err) {
    throw new PasskeyVaultUnusableError(err);
  }
  try {
    return mnemonic.phrase;
  } finally {
    mnemonic.free();
  }
}

/** A blob is stored for the passkey but it did not open under this
 *  ceremony's PRF output. Nothing was changed. */
export class PasskeyVaultUnusableError extends Error {
  override name = "PasskeyVaultUnusableError";
  constructor(cause: unknown) {
    super("the keys stored for this passkey did not open");
    this.cause = cause;
  }
}

/** What a passkey row says and offers in Settings (UX spec 10.2). */
export type PasskeyKeysLabel = "restores-keys" | "signin-only";

/** The label for one passkey: it restores keys exactly when the server
 *  holds a blob for its credential id. `withKeys` is `GET /vault/passkeys`;
 *  `null` (not loaded, or the server has no vault key) reads as sign-in
 *  only, which is what such a passkey can do. */
export function passkeyKeysLabel(
  credentialIdB64: string,
  withKeys: readonly string[] | null,
): PasskeyKeysLabel {
  return withKeys !== null && withKeys.includes(credentialIdB64) ? "restores-keys" : "signin-only";
}

/** What a passkey sign-in on a device without the account's keys found
 *  (UX spec section 4), which decides the first line of the restore prompt. */
export type PasskeyKeysOutcome =
  /** The blob opened and the phrase matched the published identity. */
  | "restored"
  /** No PRF output, or no blob for this passkey: it signs in only. */
  | "no-keys"
  /** A blob exists but did not open, or the phrase it gave is not the
   *  account's: nothing was changed. */
  | "unusable";

/** The outcome for what the ceremony and the vault returned. `opened` is
 *  whether the blob opened into a phrase; `matched` is whether that phrase
 *  passed the published-identity check. */
export function passkeyKeysOutcome(args: {
  readonly prfOutput: boolean;
  readonly blobStored: boolean;
  readonly opened: boolean;
  readonly matched: boolean;
}): PasskeyKeysOutcome {
  if (!args.prfOutput || !args.blobStored) return "no-keys";
  return args.opened && args.matched ? "restored" : "unusable";
}

/** What follows creating a passkey (UX spec 10.2, steps 2 and 3). */
export type PasskeyAddOutcome =
  /** The platform can protect keys with this passkey: store them for it. */
  | "store-keys"
  /** It cannot: the passkey signs in only, and the result line says so. */
  | "signin-only";

/**
 * Whether storing the keys follows a registration. `prfOutput` is whether
 * the registration already yielded the PRF output; `prfEnabled` is what the
 * browser reported for the new credential (`true`: supported, the output
 * comes when the passkey is next used; `false`: not supported; `null`: the
 * browser said nothing). The second passkey prompt is skipped only on an
 * explicit `false` (UX spec 10.2, amended 2026-11-11): when the browser
 * said nothing, the prompt runs and its answer decides.
 */
export function passkeyAddOutcome(args: {
  readonly prfOutput: boolean;
  readonly prfEnabled: boolean | null;
}): PasskeyAddOutcome {
  return args.prfOutput || args.prfEnabled !== false ? "store-keys" : "signin-only";
}
