// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0066 (a device paired after a key rotation) — which account key a
 * device of THIS account is bound to inside conversations, and what follows
 * from it. Two pure rules, the twins of iOS `OwnAccountKeys` and Android
 * `DeviceReadmission.ownKeyPackageUsable`.
 *
 * After a proven rotation the conversation leaves stay bound to the account
 * key of before it (the OLDEST own predecessor, the "conversation key"). A
 * device paired afterwards learns that key from the pairing state bundle. A
 * device that was already signed in when it paired minted its binding, and
 * published its KeyPackage, under the CURRENT key; a leaf admitted from that
 * KeyPackage reads as another account to every gate of the conversation.
 *
 *   - the SENDER rule ({@link ownKeyPackageUsable}): an own device is not
 *     admitted from a KeyPackage bound to another account key than the one
 *     this device's own leaves are bound to. It waits for a re-published one.
 *   - the RECEIVER rule ({@link bindingVerdict}): the binding is minted
 *     again under the conversation key, but only while the device holds no
 *     conversation; a device that holds some keeps its binding (a re-mint
 *     would split it between two account keys).
 *
 * Keys are compared as standard base64 strings.
 */

/** What to do with a device's stored conversation binding. */
export type BindingVerdict =
  /** The binding is this account's and is kept. */
  | "keep"
  /** No usable binding, or one naming a key that is not this account's:
   *  mint one under the conversation key. */
  | "mint"
  /** The binding names the CURRENT key although the account has
   *  predecessors (it was minted before the pairing state bundle brought
   *  them) and this device holds no conversation: mint it again under the
   *  conversation key, before a KeyPackage naming the current key admits
   *  this device anywhere. */
  | "remint"
  /** The same, but this device already holds conversations whose leaves
   *  carry the current-key binding: kept. Such a device is not admitted to
   *  the account's older conversations. */
  | "keep-under-current-key";

export function bindingVerdict(input: {
  /** The account key the stored binding names; `null` when there is no
   *  binding or it did not verify. */
  readonly boundKeyB64: string | null;
  readonly currentKeyB64: string;
  /** The own predecessor keys, oldest first (the current key excluded). */
  readonly predecessorKeysB64: readonly string[];
  /** Whether this device is in any conversation. */
  readonly holdsConversations: boolean;
}): BindingVerdict {
  const bound = input.boundKeyB64;
  if (bound === null) return "mint";
  if (input.predecessorKeysB64.includes(bound)) return "keep";
  if (bound !== input.currentKeyB64) return "mint";
  if (input.predecessorKeysB64.length === 0) return "keep";
  return input.holdsConversations ? "keep-under-current-key" : "remint";
}

/**
 * Whether a binding verdict changes the key the device is bound to. The
 * live-session re-mint (a state bundle applied on a signed-in device) acts
 * only on `remint`: `mint` belongs to the session install, which runs before
 * anything is published.
 */
export function rebindsAfterStateBundle(verdict: BindingVerdict): boolean {
  return verdict === "remint";
}

/**
 * The account key a KeyPackage's device binding names, or `null` for an
 * unbound KeyPackage (its "account key" is its own leaf key) and for one
 * whose binding does not verify. `accountKeyB64` is the wasm
 * `keyPackageAccountKey` answer.
 */
export function boundAccountKeyB64(
  accountKeyB64: string | null | undefined,
  leafKeyB64: string,
): string | null {
  if (accountKeyB64 === null || accountKeyB64 === undefined) return null;
  return accountKeyB64 === leafKeyB64 ? null : accountKeyB64;
}

/**
 * Whether a KeyPackage may be used to admit its device to a conversation.
 *
 * `ownAccount` says whether the device is one of THIS account's (the
 * directory row was fetched under the own account id); a contact's device is
 * never affected. An own device is usable only when its binding names
 * `conversationKeyB64`, the account key this device's own leaves are bound
 * to. An unbound KeyPackage (`boundKeyB64` `null`) and an unknown
 * conversation key change nothing.
 */
export function ownKeyPackageUsable(input: {
  readonly ownAccount: boolean;
  readonly boundKeyB64: string | null;
  readonly conversationKeyB64: string | null;
}): boolean {
  if (!input.ownAccount) return true;
  if (input.boundKeyB64 === null || input.conversationKeyB64 === null) return true;
  return input.boundKeyB64 === input.conversationKeyB64;
}

/** The two wasm readers the sender rule needs. */
export interface KeyPackageReaders {
  keyPackageAccountKey(keyPackage: Uint8Array): Uint8Array | undefined;
  keyPackageSignatureKey(keyPackage: Uint8Array): Uint8Array;
}

function b64(bytes: Uint8Array): string {
  let s = "";
  for (const byte of bytes) s += String.fromCharCode(byte);
  return btoa(s);
}

/**
 * {@link ownKeyPackageUsable} for a wire KeyPackage of an OWN device. A
 * KeyPackage that cannot be read at all is left to the admission path, which
 * refuses it with its own error.
 */
export function ownDeviceKeyPackageUsable(
  readers: KeyPackageReaders,
  keyPackage: Uint8Array,
  conversationKeyB64: string | null,
): boolean {
  let bound: string | null;
  try {
    const account = readers.keyPackageAccountKey(keyPackage);
    bound = boundAccountKeyB64(
      account === undefined ? null : b64(account),
      b64(readers.keyPackageSignatureKey(keyPackage)),
    );
  } catch {
    return true;
  }
  return ownKeyPackageUsable({ ownAccount: true, boundKeyB64: bound, conversationKeyB64 });
}
