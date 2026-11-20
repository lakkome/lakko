// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 (a device paired after a key rotation) — which account key a
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
 *   - the RECEIVER rule ({@link bindingVerdict}, written once in Rust): the
 *     binding is minted under the conversation key; a device that does not
 *     hold that key yet WAITS (2026-11-15, "Joining after a rotation": it
 *     binds nothing, publishes no KeyPackage and joins nothing until the
 *     earlier keys reach it); a device that holds conversations keeps its
 *     binding (a re-mint would split it between two account keys).
 *
 * The sender rule compares keys as standard base64 strings; the receiver
 * rule takes raw 32-byte keys.
 */

/**
 * The account's own conversation link as this device read it (ADR-0074,
 * "Joining after a rotation"): `absent` (no link, or one that does not
 * verify against this device's current key), `key` (the conversation key
 * `C` the link names), `unknown` (the bundle could not be read).
 */
export type OwnLink =
  | { readonly kind: "absent" }
  | { readonly kind: "key"; readonly key: Uint8Array }
  | { readonly kind: "unknown" };

export const OWN_LINK_ABSENT: OwnLink = { kind: "absent" };
export const OWN_LINK_UNKNOWN: OwnLink = { kind: "unknown" };

/** What to do with a device's stored conversation binding. */
export type BindingVerdictKind = "keep" | "keep-under-current-key" | "wait" | "mint";

export interface BindingDecision {
  readonly kind: BindingVerdictKind;
  /** The 32-byte account key to mint under, for `mint`; `null` otherwise. */
  readonly mintKey: Uint8Array | null;
  /** `keep` or `keep-under-current-key`. */
  readonly isKept: boolean;
}

/** The core calls this module needs (`lakko_contacts::own_binding` and
 *  `lakko_transparency::own_conversation_key` through the wasm surface). */
export interface OwnBindingCore {
  ownBindingVerdict(
    boundKey: Uint8Array | null | undefined,
    currentKey: Uint8Array,
    predecessorKeys: Uint8Array,
    linkKind: number,
    linkKey: Uint8Array | null | undefined,
    holdsConversations: boolean,
  ): { readonly kind: string; readonly mintKey: Uint8Array | undefined; readonly isKept: boolean; free(): void };
  ownConversationKey(link: Uint8Array | null | undefined, currentKey: Uint8Array): Uint8Array | undefined;
}

/** `OwnLinkKind` of the wasm surface (`Absent 0`, `Key 1`, `Unknown 2`). */
const LINK_KIND = { absent: 0, key: 1, unknown: 2 } as const;

/**
 * The binding verdict, written once in Rust (`lakko_contacts::own_binding`,
 * the 17 shared vectors). Keys are raw 32-byte account keys;
 * `predecessorKeys` are the own predecessor keys this device holds, oldest
 * first, current excluded. Run it with the recorded link (or `absent`)
 * first and read the bundle only when the answer is not kept.
 */
export function bindingVerdict(
  core: OwnBindingCore,
  input: {
    /** The account key the stored binding names; `null` when there is no
     *  binding or it did not verify. */
    readonly boundKey: Uint8Array | null;
    readonly currentKey: Uint8Array;
    readonly predecessorKeys: readonly Uint8Array[];
    readonly link: OwnLink;
    /** Whether this device is in any conversation. */
    readonly holdsConversations: boolean;
  },
): BindingDecision {
  const flat = new Uint8Array(input.predecessorKeys.length * 32);
  input.predecessorKeys.forEach((k, i) => flat.set(k, i * 32));
  const verdict = core.ownBindingVerdict(
    input.boundKey ?? undefined,
    input.currentKey,
    flat,
    LINK_KIND[input.link.kind],
    input.link.kind === "key" ? input.link.key : undefined,
    input.holdsConversations,
  );
  try {
    const kind = verdict.kind as BindingVerdictKind;
    const mintKey = kind === "mint" ? (verdict.mintKey ?? null) : null;
    return { kind, mintKey, isKept: verdict.isKept };
  } finally {
    verdict.free();
  }
}

/**
 * Whether a verdict in a RUNNING session re-mints the binding (a pairing
 * state bundle or a restored backup just stored the account's earlier keys):
 * only a `mint` over a stored binding under the CURRENT key (what used to be
 * called `remint`). A `mint` with no binding or a foreign one belongs to the
 * session install, which runs before anything is published.
 */
export function rebindsAfterStateBundle(
  decision: BindingDecision,
  boundKey: Uint8Array | null,
  currentKey: Uint8Array,
): boolean {
  return decision.kind === "mint" && boundKey !== null && keysEqual(boundKey, currentKey);
}

/**
 * The link answer of a fetched own bundle: `conversationLinkB64` is the
 * bundle's `conversation_link` (standard base64, absent for none), verified
 * against THIS device's current key (never the fetched one). A link that
 * does not decode or does not verify is `absent`.
 */
export function ownLinkOfBundle(
  core: Pick<OwnBindingCore, "ownConversationKey">,
  conversationLinkB64: string | undefined,
  currentKey: Uint8Array,
): OwnLink {
  if (conversationLinkB64 === undefined || conversationLinkB64.length === 0) return OWN_LINK_ABSENT;
  let link: Uint8Array;
  try {
    link = Uint8Array.from(atob(conversationLinkB64), (c) => c.charCodeAt(0));
  } catch {
    return OWN_LINK_ABSENT;
  }
  const key = core.ownConversationKey(link, currentKey);
  return key === undefined ? OWN_LINK_ABSENT : { kind: "key", key };
}

function keysEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) diff |= (a[i] ?? 0) ^ (b[i] ?? 0);
  return diff === 0;
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
