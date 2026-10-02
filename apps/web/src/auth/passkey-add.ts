// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Adding a passkey to the signed-in account: the one sequence behind
 * Settings > Security > Passkeys and the passkey offer at the end of signup
 * (docs/ux/identity-recovery.md 9 step 3 and 10.2).
 *
 * It registers the passkey, turns on attested device removals (G-13,
 * ADR-0015), and says whether the keys choice can follow. Nothing is stored
 * for the passkey here: letting it restore keys is the person's own choice
 * (`PasskeyKeysChoice`).
 */
import { base64UrlEncodeNoPad } from "./account-id";
import { getDeviceKey } from "./key-store";
import { enableAttestedRemovalsIfNeeded } from "../devices/attested-removal-policy";
import { passkeyAddOutcome, PASSKEY_PRF_SALT, type PasskeyAddOutcome } from "../recovery/passkey-vault";
import type { WebauthnClient } from "./webauthn-client";

export interface AddedPasskey {
  readonly credentialIdB64: string;
  /** What the registration yielded for protecting keys, when it did. */
  readonly prfOutput: Uint8Array | null;
  /** Whether the keys choice follows, or the passkey signs in only. */
  readonly outcome: PasskeyAddOutcome;
}

export async function addPasskey(args: {
  readonly client: WebauthnClient;
  readonly username: string;
  readonly nickname: string;
  /** The signed-in account, for the attested-removal policy. */
  readonly account: { readonly accountIdBytes: Uint8Array; readonly accountIdB64: string } | null;
}): Promise<AddedPasskey> {
  const registered = await args.client.registerPasskey(
    { user_name: args.username, nickname: args.nickname },
    { prfSalt: PASSKEY_PRF_SALT },
  );
  // G-13 (ADR-0015): now that the account has a passkey, enable attested
  // removals so device removals require a peer-verified passkey gesture.
  // Non-fatal: the policy can be enabled on a later passkey action.
  if (args.account !== null) {
    try {
      const deviceKey = await getDeviceKey(args.account.accountIdB64);
      if (deviceKey !== null) {
        await enableAttestedRemovalsIfNeeded({
          accountIdBytes: args.account.accountIdBytes,
          accountIdB64: args.account.accountIdB64,
          deviceKey,
        });
      }
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("failed to enable attested removals", err);
    }
  }
  return {
    credentialIdB64: base64UrlEncodeNoPad(registered.credentialId),
    prfOutput: registered.prfOutput,
    outcome: passkeyAddOutcome({
      prfOutput: registered.prfOutput !== null,
      prfEnabled: registered.prfEnabled,
    }),
  };
}
