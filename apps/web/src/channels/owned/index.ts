// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 - the owned-channels list and its locks: what the
 * account paths call. Each function takes everything it needs as arguments,
 * never throws, and answers an outcome; `"retry"` means "call again at the
 * next landing that holds the same inputs". All byte-string arguments are
 * secret and are not kept by the callee beyond the list key and the server
 * half it holds in memory for the session.
 *
 * | When                                                        | Call                                  |
 * |-------------------------------------------------------------|---------------------------------------|
 * | sign-up; a guest's claim (its first password)               | {@link makeOwnedChannelsVaults}       |
 * | a password sign-in on a device with the keys; after a       | {@link ensureOwnedPasswordLock}       |
 * | password change on it                                       |                                       |
 * | a passkey that keeps the chats was added                    | {@link addOwnedPasskeyLock}           |
 * | a passkey was removed                                       | {@link removeOwnedPasskeyLock}        |
 * | after a key reset on this device, the new account landed    | {@link retakeOwnedChannelsAfterReset} |
 * | before `DELETE /account`                                    | {@link deleteOwnedChannelsVaults}     |
 * | sign-out with Forget device                                 | {@link clearOwnedChannelsFacts}       |
 * | any sign-out (the list key leaves memory)                   | {@link dropOwnedChannelsSession}      |
 *
 * The publisher pass does the rest by itself (`owned-runtime.ts`): recovery
 * keys registered from the list key, the list written on create, delete,
 * rotation and a new admission seed, and a retake a reload interrupted.
 */
export {
  addOwnedPasskeyLock,
  clearOwnedChannelsFacts,
  deleteOwnedChannelsVaults,
  dropOwnedChannelsSession,
  ensureOwnedPasswordLock,
  makeOwnedChannelsVaults,
  removeOwnedPasskeyLock,
  retakeOwnedChannelsAfterReset,
} from "./owned-runtime";
export type { ResetSealedBy, RetakeOutcome } from "./owned-retake";
