// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The confirming device's half of the recovery payload v2
 * (`recovery-payload.ts`): what it hands `runScanFlow` so the new device
 * receives a session grant and the recovery phrase.
 *
 * The phrase comes from this device's own store (`backup/device-phrase.ts`)
 * and crosses as its 16-byte entropy. A device that does not hold the
 * phrase returns `undefined`: no recovery row is published, the new device
 * is still added to the account's devices, and it restores its keys another
 * way. The caller says so on screen (`ScanResult.recoveryPublished`).
 */
import type { DeviceCipher } from "../auth/device-cipher";
import { readDevicePhraseWith } from "../backup/device-phrase";
import { loadWasm } from "../wasm";

export interface RecoveryHalf {
  /** 16 bytes; consumed (zeroized) by `runScanFlow`. */
  readonly phraseEntropy: Uint8Array;
  readonly deviceAuthorizeStart: () => Promise<string>;
}

export async function recoveryHalfFor(args: {
  readonly accountIdB64: string;
  readonly deviceKey: DeviceCipher;
  readonly deviceAuthorizeStart: () => Promise<string>;
}): Promise<RecoveryHalf | undefined> {
  const phrase = await readDevicePhraseWith(args.accountIdB64, args.deviceKey);
  if (phrase === null) return undefined;
  try {
    const wasm = await loadWasm();
    const mnemonic = wasm.RecoveryMnemonic.fromPhrase(phrase);
    try {
      return {
        phraseEntropy: mnemonic.toEntropy(),
        deviceAuthorizeStart: args.deviceAuthorizeStart,
      };
    } finally {
      mnemonic.free();
    }
  } catch (err) {
    // A stored phrase the core refuses (not 12 listed words with a valid
    // checksum): treated as no phrase.
    // eslint-disable-next-line no-console
    console.warn("the recovery phrase stored on this device could not be read as a phrase", err);
    return undefined;
  }
}
