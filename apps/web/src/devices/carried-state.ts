// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The pairing state bundle v3 (`state-bundle.ts`) on the RECEIVING device:
 * the decisions about the group names, the profiles and the pending
 * contacts it carries, free of IndexedDB, wasm and Solid so they can be
 * tested; and the hand-off through which the names reach the two session
 * stores (`chat/GroupNameContext.tsx`, `profile/ProfileContext.tsx`).
 *
 * ## Why a hand-off, not a direct write
 *
 * A bundle can be applied before the session lands (the restore route) or
 * while it is live (a signed-in device that paired). The names must reach
 * the store the chat list reads, reactively, and through the same entry
 * point a received envelope uses, under the "strictly newer stamp wins"
 * rule. So the bundle's names are OFFERED here, per account, and each
 * provider takes them when it is bound to that account (at once when it
 * already is). The profile provider is also the one place that knows this
 * account's own keys, which a carried profile is never filed under.
 *
 * What is offered lives in this page's memory only. The restore route lands
 * the session in the same page that applied the bundle; a page that went
 * away in between lands without the names, as before v3.
 */
import { registerAccountMemory } from "../auth/account-memory";
import { base64Decode } from "../auth/account-id";
import type { GroupNameDecoder } from "../chat/group-name-envelope";
import { sniffGroupName } from "../chat/group-name-envelope";
import type { StoredGroupName } from "../chat/group-name-store";
import type { CarriedCopy } from "../contacts/carried-copies";
import type { PendingContact } from "../contacts/pending-contacts";
import type { ProfileDecoder, ReceivedProfile } from "../profile/profile-envelope";
import { sniffProfile } from "../profile/profile-envelope";
import type { CarriedPendingContact, StateBundle } from "./state-bundle";

/** The group names a bundle carries, decoded. An entry that does not decode
 *  is skipped, never the bundle. Pure over the decoder. */
export function carriedGroupNamesOf(
  bundle: Pick<StateBundle, "groupNames">,
  decoder: GroupNameDecoder,
): Map<string, StoredGroupName> {
  const out = new Map<string, StoredGroupName>();
  for (const n of bundle.groupNames) {
    let bytes: Uint8Array;
    try {
      bytes = base64Decode(n.envelopeB64);
    } catch {
      continue;
    }
    const sniff = sniffGroupName(decoder, bytes);
    if (sniff.kind === "name") out.set(n.groupIdB64, sniff.value);
  }
  return out;
}

/** The profiles a bundle carries, decoded and keyed on the person key. An
 *  entry that does not decode is skipped; a key `isOwnKey` names is never
 *  returned. Pure over the decoder. */
export function carriedProfilesOf(
  bundle: Pick<StateBundle, "profiles">,
  decoder: ProfileDecoder,
  isOwnKey: (keyB64: string) => boolean,
): Map<string, ReceivedProfile> {
  const out = new Map<string, ReceivedProfile>();
  for (const p of bundle.profiles) {
    if (isOwnKey(p.personKeyB64)) continue;
    let bytes: Uint8Array;
    try {
      bytes = base64Decode(p.envelopeB64);
    } catch {
      continue;
    }
    const sniff = sniffProfile(decoder, bytes);
    if (sniff.kind === "profile") out.set(p.personKeyB64, sniff.profile);
  }
  return out;
}

/** The carried group names to store: those this device holds nothing for,
 *  or holds an older stamp of (`isNewer` is the core's `groupNameIsNewer`:
 *  strictly greater). Pure. */
export function groupNamesToStore(
  carried: ReadonlyMap<string, StoredGroupName>,
  held: (groupIdB64: string) => StoredGroupName | undefined,
  isNewer: (incoming: bigint, stored: bigint) => boolean,
): Map<string, StoredGroupName> {
  const out = new Map<string, StoredGroupName>();
  for (const [groupIdB64, value] of carried) {
    const stored = held(groupIdB64);
    if (stored === undefined || isNewer(value.updatedAtMs, stored.updatedAtMs)) out.set(groupIdB64, value);
  }
  return out;
}

function hexOfB64(b64: string): string {
  let out = "";
  for (const byte of base64Decode(b64)) out += byte.toString(16).padStart(2, "0");
  return out;
}

/** The local record of a carried pending contact. Pure. */
export function pendingContactOf(c: CarriedPendingContact): PendingContact {
  return {
    accountIdHex: c.accountIdHex,
    signaturePubkeyB64: c.signaturePubkeyB64,
    x25519PubkeyB64: c.x25519PubkeyB64,
    displayName: c.displayName,
    username: c.username,
    addedAtMs: c.addedAtMs,
    linkIdHex: hexOfB64(c.linkIdB64),
  };
}

/** The display-only copies of a carried pending contact's held messages,
 *  in queue order. Pure. */
export function copiesOf(c: CarriedPendingContact): CarriedCopy[] {
  return c.heldMessages.map((m) => ({
    text: m.text,
    attachments: m.attachments,
    replyTo: m.replyTo,
    heldAtMs: m.heldAtMs,
  }));
}

/**
 * The carried pending contacts this device adds: those it holds neither as
 * an accepted contact nor as a pending one, by signature key or account id.
 * Pure.
 */
export function pendingContactsToAdd(
  carried: readonly CarriedPendingContact[],
  accepted: readonly { readonly signaturePubkeyB64: string; readonly accountIdHex?: string | undefined }[],
  held: readonly PendingContact[],
): CarriedPendingContact[] {
  const keys = new Set<string>();
  const accounts = new Set<string>();
  for (const c of accepted) {
    keys.add(c.signaturePubkeyB64);
    if (c.accountIdHex !== undefined) accounts.add(c.accountIdHex.toLowerCase());
  }
  for (const p of held) {
    keys.add(p.signaturePubkeyB64);
    accounts.add(p.accountIdHex);
  }
  return carried.filter((c) => !keys.has(c.signaturePubkeyB64) && !accounts.has(c.accountIdHex));
}

// ---- the hand-off to the session stores ------------------------------------

interface Offered {
  groupNames: Map<string, StoredGroupName>;
  profiles: Map<string, ReceivedProfile>;
}

const offered = new Map<string, Offered>();
const listeners = new Set<() => void>();

/** Offer a bundle's decoded names for `accountIdB64`. A later offer for the
 *  same group or person replaces an earlier one still waiting. */
export function offerCarriedNames(
  accountIdB64: string,
  groupNames: ReadonlyMap<string, StoredGroupName>,
  profiles: ReadonlyMap<string, ReceivedProfile>,
): void {
  if (groupNames.size === 0 && profiles.size === 0) return;
  const slot = offered.get(accountIdB64) ?? { groupNames: new Map(), profiles: new Map() };
  for (const [k, v] of groupNames) slot.groupNames.set(k, v);
  for (const [k, v] of profiles) slot.profiles.set(k, v);
  offered.set(accountIdB64, slot);
  for (const fn of listeners) fn();
}

/** Take the group names waiting for `accountIdB64` (once). */
export function takeCarriedGroupNames(accountIdB64: string): Map<string, StoredGroupName> {
  const slot = offered.get(accountIdB64);
  if (slot === undefined) return new Map();
  const out = slot.groupNames;
  slot.groupNames = new Map();
  if (slot.profiles.size === 0) offered.delete(accountIdB64);
  return out;
}

/** Take the profiles waiting for `accountIdB64` (once). */
export function takeCarriedProfiles(accountIdB64: string): Map<string, ReceivedProfile> {
  const slot = offered.get(accountIdB64);
  if (slot === undefined) return new Map();
  const out = slot.profiles;
  slot.profiles = new Map();
  if (slot.groupNames.size === 0) offered.delete(accountIdB64);
  return out;
}

/** Called after every offer. Returns the unsubscribe. */
export function subscribeCarriedNames(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

// What waits for an account that is forgotten goes with it. Not at a plain
// sign-out: the restore route may sign a previous session out between the
// apply and the landing it is waiting for.
registerAccountMemory("carried-names", (ids) => {
  for (const id of ids) offered.delete(id);
});
