// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The recovery phrase on a device that holds the account's keys
 * (maintainer decision 2026-10-02, superseding ADR-0072 decision 5).
 *
 * A device that holds the keys also holds the 12-word phrase, wrapped under
 * its non-extractable device key (`phrase-store.ts`). Three things rest on
 * that:
 *
 *   - the local three-word check ("Three words from your phrase" on the
 *     recovery screen): the device compares the typed words with the phrase
 *     it holds, the server takes no part;
 *   - handing the phrase to a device added through "another device"
 *     (`devices/recovery-payload.ts` v2 carries its entropy);
 *   - "Show recovery phrase" in Settings and the encrypted backup.
 *
 * So every path that lands a device with keys writes the phrase store, and
 * `PHRASE_LANDING_PATHS` names them. `AuthContext` calls
 * `storeDevicePhrase(..., path)` once per path; `device-phrase.test.ts`
 * reads the source and fails when a named path has no call, or a call names
 * a path that is not listed.
 *
 * A path that lands keys WITHOUT the phrase would leave a device that
 * cannot offer three words, cannot hand keys to a new device and cannot
 * show the phrase. There is no such path: keys only ever arrive as the
 * phrase (typed, minted, opened from a passkey) or as its entropy (another
 * device).
 */
import { getDeviceKey } from "../auth/key-store";
import { clearThreeWordsSuspended } from "../auth/recover/three-words";
import { getPhrase, putPhrase } from "./phrase-store";

/** Every way a device comes to hold the account's keys. */
export const PHRASE_LANDING_PATHS = [
  /** `signup`: the phrase was just generated. */
  "signup",
  /** `signupGuest` (ADR-0032): generated for a username-less account, and
   *  kept under the guest's own key of the store (`guestPhraseKey`) until
   *  the person decides whether to keep the account. */
  "signup-guest",
  /** A guest account was kept (a username and a first password): the phrase
   *  moves under the account's own key. */
  "guest-kept",
  /** `recover`: the 12 words plus a new password. */
  "recover",
  /** `provideRecoveryPhrase`: typed in the restore prompt, or opened from a
   *  passkey that carries the keys (that route ends here too). */
  "provide-phrase",
  /** `completeFromPairing`: another device, on a signed-in device. */
  "device-pairing",
  /** `completeFromDeviceRestore`: another device, no password. */
  "device-restore",
  /** `mintFreshIdentity`: a new identity and a new phrase. */
  "fresh-identity",
  /** ADR-0074 part 3: the account keys were changed with the devices kept
   *  (on this device, or adopted from another one). */
  "key-rotation",
] as const;

export type PhraseLandingPath = (typeof PHRASE_LANDING_PATHS)[number];

/**
 * Store `phrase` for `accountIdB64` under `deviceKey`. Best-effort like the
 * other wrapped caches: a failed write is logged (with the path, never the
 * phrase) and the landing continues; the device then simply does not offer
 * what needs the phrase.
 */
export async function storeDevicePhrase(
  accountIdB64: string,
  phrase: string,
  deviceKey: CryptoKey,
  path: PhraseLandingPath,
): Promise<boolean> {
  const bytes = new TextEncoder().encode(phrase);
  try {
    await putPhrase(accountIdB64, bytes, deviceKey);
    // A landing path wrote the phrase again (checked against the published
    // identity where one applies): a three-word suspension is over.
    clearThreeWordsSuspended(
      typeof localStorage === "undefined" ? undefined : localStorage,
      accountIdB64,
    );
    return true;
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn(`recovery phrase store (${path}) failed`, err);
    return false;
  } finally {
    bytes.fill(0);
  }
}

/** The phrase stored for `accountIdB64` under `deviceKey`, or `null` when
 *  there is none or it does not open. Never throws. */
export async function readDevicePhraseWith(
  accountIdB64: string,
  deviceKey: CryptoKey,
): Promise<string | null> {
  try {
    const cached = await getPhrase(accountIdB64, deviceKey);
    if (cached === null) return null;
    const phrase = new TextDecoder().decode(cached.phrase);
    cached.phrase.fill(0);
    return phrase;
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("reading the recovery phrase stored on this device failed", err);
    return null;
  }
}

/** The phrase this device holds for `accountIdB64`, or `null`: no device
 *  key, no stored phrase, or storage that cannot be read. Never throws. */
export async function readDevicePhrase(accountIdB64: string): Promise<string | null> {
  let deviceKey: CryptoKey | null;
  try {
    deviceKey = await getDeviceKey(accountIdB64);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("device key not readable", err);
    return null;
  }
  if (deviceKey === null) return null;
  return readDevicePhraseWith(accountIdB64, deviceKey);
}
