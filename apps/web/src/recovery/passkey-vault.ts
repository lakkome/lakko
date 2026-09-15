// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Passkey-carried identity (ADR-0049 section 3).
 *
 * A passkey grants ACCESS; with the WebAuthn PRF extension it can also
 * carry IDENTITY. The PRF output for a fixed salt is a stable 32-byte
 * secret per credential that follows a synced passkey to every device the
 * passkey syncs to. The client seals the phrase entropy under a key derived
 * from it (`passkeyVaultSeal` in wasm) and stores the blob server-side per
 * credential; a passkey login on a new device asks for the same PRF
 * evaluation, fetches the blob, opens it into the full 24-word phrase and
 * hands it to the ordinary phrase path.
 *
 * Authenticator support for PRF is uneven. Every step here is optional: no
 * PRF output means no enrollment and no restore, and the caller falls back
 * to the three words or another device.
 */
import type { AuthClient } from "../auth/client";
import { AuthHttpError } from "../auth/client";
import { getDeviceKey } from "../auth/key-store";
import { getPhrase } from "../backup/phrase-store";
import type { Wasm } from "../wasm";

/** The PRF salt every Lakko client evaluates. Fixed by design: the output
 *  is already per-credential, and a per-account salt would have to be
 *  fetched before the login that needs it. */
export const PASSKEY_PRF_SALT = new TextEncoder().encode("lakko/passkey-vault/prf/v1");

/** Seal `phrase` under the PRF output and store it for `credentialId`.
 *  Returns false when the server has the vault disabled. */
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
 *  `null` when no blob is stored (or the vault is disabled server-side);
 *  throws when a stored blob does not open (a tampered blob or a PRF
 *  output that is not this credential's). */
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
  const mnemonic = args.wasm.passkeyVaultOpen(args.prfOutput, sealed);
  try {
    return mnemonic.phrase;
  } finally {
    mnemonic.free();
  }
}


/**
 * Make sure `credentialId` has a blob, when this device can build one. With
 * `phrase` given it seals outright; otherwise it asks the server and, if
 * nothing is stored, uses the phrase this device cached under its device
 * key. Best-effort: failures are logged and swallowed, since this runs
 * behind a ceremony that already succeeded.
 */
export async function ensurePasskeyVaultEnrolled(args: {
  readonly wasm: Wasm;
  readonly client: AuthClient;
  readonly sessionToken: string;
  readonly accountIdB64: string;
  readonly credentialId: Uint8Array;
  readonly prfOutput: Uint8Array;
  readonly phrase?: string;
}): Promise<boolean> {
  try {
    let phrase = args.phrase;
    if (phrase === undefined) {
      const existing = await args.client.passkeyVaultGet(args.sessionToken, args.credentialId);
      if (existing !== null) return true;
      const deviceKey = await getDeviceKey(args.accountIdB64);
      if (deviceKey === null) return false;
      const cached = await getPhrase(args.accountIdB64, deviceKey);
      if (cached === null) return false;
      phrase = new TextDecoder().decode(cached.phrase);
    }
    return await enrollPasskeyVault({
      wasm: args.wasm,
      client: args.client,
      sessionToken: args.sessionToken,
      credentialId: args.credentialId,
      prfOutput: args.prfOutput,
      phrase,
    });
  } catch (err) {
    if (err instanceof AuthHttpError && err.status === 503) return false;
    // eslint-disable-next-line no-console
    console.warn("passkey-vault enrollment skipped", err);
    return false;
  }
}
