// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0050 section 2.9 — the state bundle a pairing hands to the new device
 * beside the history bundle: accepted contacts (with their local renames),
 * the per-contact identity pins and rotation markers, and the ids of the
 * groups the old device knows. Everything that lives in this browser's
 * `localStorage` and would otherwise reach a second device only through a
 * backup that runs at signup and recover.
 *
 * Carried as ONE more row on the pairing rendezvous mailbox, tagged by a
 * plaintext magic so the three polls (history, recovery, state) can each skip
 * the rows that are not theirs in whatever FIFO order the scanner published
 * them. Sealed under the DeviceGroup exporter at the post-add epoch, the
 * recovery row's construction: both devices derive the same key, the DS
 * stores ciphertext.
 *
 * ADR-0074 (v2): the bundle also carries the account's OWN predecessor seeds
 * and each contact's proven predecessor keys, so a device paired AFTER a
 * rotation of the account keys holds what a kept device that adopted holds:
 * its conversation leaves are bound to the same account key as the sending
 * device's, it reads the predecessor mailboxes for what is left of their
 * windows, and a contact's old-key leaves name the same person. The format
 * is hand-mirrored on iOS (`StateBundle.swift`) and Android
 * (`StateBundle.kt`).
 *
 * v3 (2026-10-28, flag day: v2 refused, no dual accept) adds three arrays,
 * because a device paired into an existing account is no membership growth,
 * so nothing else ever sends it the names the confirming device holds:
 *
 *   * `groupNames` — the ADR-0051 group-name envelope of each known group,
 *     composed with the name and the ORIGINAL stamp this device stores (a
 *     later rename must still win);
 *   * `profiles` — the ADR-0048 profile envelope of every person this device
 *     holds one for (contacts and group members), with the original stamp,
 *     never this account's own;
 *   * `pendingContacts` — the ADR-0079 section 5 pending contacts, each with
 *     the messages held for it, carried as DISPLAY-ONLY copies: the new
 *     device shows them waiting and never sends them (the device where they
 *     were typed does).
 *
 * The bundle comes from this account's own device under the DeviceGroup
 * exporter, so its names are trusted like the contacts it carries: the
 * ADR-0051 owner/admin check of the in-group path does not apply.
 *
 * v4 (2026-11-13, flag day: v3 refused, no dual accept) adds `channelSeeds`:
 * the newest epoch seeds this device holds (at most 32 per channel, the
 * oldest dropped first while the bundle is over its size cap: 32, 16, 8, 4,
 * then none) of every channel the account's channel-subscription register
 * holds live. The channel list itself, the account's one subscriber key per
 * channel and its read key are NOT here: they travel in that register (a
 * `lakko-sync` envelope over the DeviceGroup, also in the backup), which
 * reaches every device of the account whenever it subscribes, not only at a
 * pairing. The seeds are here because they change at every rotation and the
 * channels service keeps only the newest four per subscriber key; with
 * them the new device opens the older posts the confirming device could.
 * A receiver never replaces a seed it holds for an epoch.
 */
import type { DeviceCipher } from "../auth/device-cipher";
import { DsClient, DsHttpError, DsNetworkError } from "../ds/client";
import {
  boundContactsAccount,
  loadAccepted,
  readPin,
  recordCarriedProvenPredecessors,
  setPin,
  upsertAccepted,
  type AcceptedContact,
} from "../contacts/local-store";
import {
  readPredecessorAdoptedAtMs,
  stampPredecessorAdoption,
} from "../contacts/address-resolver";
import { base64Decode, base64Encode } from "../auth/account-id";
import {
  getKeyRotationState,
  updateKeyRotationState,
  type KeyRotationState,
} from "../auth/key-rotation-store";

/** Exporter label; the context is the 32-byte rendezvous pseudonym. */
export const STATE_EXPORTER_LABEL = "lakko/state-bundle/v1";
/** Exporter output length — the ChaCha20-Poly1305 key size. */
export const STATE_EXPORTER_KEY_LENGTH = 32;

const STATE_ROW_MAGIC = new TextEncoder().encode("lakko/state-bundle-row/v1\0");
const STATE_POLL_TIMEOUT_MS = 20_000;
const PER_REQUEST_LONG_POLL_SECS = 5;
/** Prefix of the per-account known-group list; the account id follows. */
const KNOWN_GROUPS_PREFIX = "lakko.pairing.known-groups.v2.";
/** The retired device-wide list. Never read; dropped by Forget device. */
const RETIRED_KNOWN_GROUPS_KEY = "lakko.pairing.known-groups.v1";
const MAX_BUNDLE_BYTES = 1 << 20;

/** The most own predecessors, and the most proven predecessors of one
 *  contact, a bundle carries. One more refuses the whole bundle. */
export const MAX_CARRIED_PREDECESSORS = 8;
const ACCOUNT_SEED_LENGTH = 64;
const SIGNATURE_KEY_LENGTH = 32;

/** A pinned account key as carried in the bundle. */
export interface CarriedPin {
  readonly accountIdB64: string;
  readonly pinnedSignaturePubkeyB64: string;
  readonly rotationMarker: number;
  /** The contact's proven predecessors as the sending device holds them:
   *  standard-base64 32-byte signature keys, oldest first (ADR-0074). */
  readonly provenPredecessorsB64: readonly string[];
}

/** One earlier account key of the OWN account (ADR-0074 Stage A). */
export interface CarriedOwnPredecessor {
  /** Standard base64 of the 64-byte account phrase seed of that key. Key
   *  material: never logged, stored only under the device key. */
  readonly seedB64: string;
  /** When the sending device adopted the key that replaced it. The
   *  predecessor mailbox's 30-day read window counts from here. */
  readonly adoptedAtMs: number;
}

/** ADR-0051 — one known group's shared name, as its envelope. */
export interface CarriedGroupName {
  /** Standard base64 group id. */
  readonly groupIdB64: string;
  /** Standard base64 of `composeGroupNameEnvelope(name, storedStamp)`. */
  readonly envelopeB64: string;
}

/** ADR-0048 — one person's profile, as its envelope. */
export interface CarriedProfile {
  /** Standard base64 of the 32-byte person (account) key it is filed under. */
  readonly personKeyB64: string;
  /** Standard base64 of the profile envelope composed with the stored stamp. */
  readonly envelopeB64: string;
}

/** One message held for a pending contact, as a display-only copy. */
export interface CarriedHeldMessage {
  readonly text: string;
  /** The sending client's own JSON form of the held attachment references
   *  (web: `PersistedAttachment`; any JSON value per element, since each
   *  client has its own). Only their number is read here. */
  readonly attachments: readonly unknown[];
  /** The DS message id the held message replies to, or `null`. */
  readonly replyTo: number | null;
  readonly heldAtMs: number;
}

/** ADR-0079 section 5 — a person this account added through their invite
 *  link whose app has not answered yet. */
export interface CarriedPendingContact {
  /** 32 lower-case hex characters. */
  readonly accountIdHex: string;
  readonly signaturePubkeyB64: string;
  readonly x25519PubkeyB64: string;
  readonly displayName: string;
  readonly username: string;
  readonly addedAtMs: number;
  /** Standard base64 of the 16-byte invite-link id. */
  readonly linkIdB64: string;
  /** What was written to them so far, in queue order. */
  readonly heldMessages: readonly CarriedHeldMessage[];
}

/** The most entries a bundle carries; one more refuses the whole bundle on
 *  decode, and a builder keeps the first ones in a stable order. Chosen,
 *  not measured. */
export const MAX_CARRIED_GROUP_NAMES = 1000;
export const MAX_CARRIED_PROFILES = 2000;
export const MAX_CARRIED_PENDING_CONTACTS = 500;
export const MAX_CARRIED_HELD_MESSAGES = 200;
/** v4: at most this many channels in `channelSeeds`, and this many seeds per
 *  channel (the newest held). Chosen with the register's client limit and
 *  the 30 days of posts at a rotation a day. */
export const MAX_CARRIED_SEED_CHANNELS = 1000;
export const MAX_CARRIED_SEEDS_PER_CHANNEL = 32;
/** The per-channel seed counts tried, in order, while the bundle is over its
 *  size cap; the same count for every channel at once. */
const SEED_LADDER = [32, 16, 8, 4, 0] as const;
const CHANNEL_ID_LENGTH = 16;
const CHANNEL_SEED_LENGTH = 32;
const MAX_EPOCH = 4_294_967_295;

/** One epoch seed of a channel (v4). Key material: never logged. */
export interface CarriedEpochSeed {
  readonly epoch: number;
  /** Standard base64 (with padding) of the 32-byte seed. */
  readonly seedB64: string;
}

/** The newest seeds a device holds for one channel (v4). */
export interface CarriedChannelSeeds {
  /** Standard base64 (with padding) of the 16-byte channel id. */
  readonly channelIdB64: string;
  /** 1..=32 seeds, strictly ascending by epoch. */
  readonly seeds: readonly CarriedEpochSeed[];
}

/** The bundle. `v` is the only version there is. */
export interface StateBundle {
  readonly v: 4;
  readonly contacts: readonly AcceptedContact[];
  readonly pins: readonly CarriedPin[];
  /** base64 group ids the old device holds; the new device counts the ones
   *  it has not joined yet as "still synchronizing". */
  readonly knownGroupIdsB64: readonly string[];
  /** The own account's earlier keys, OLDEST first; empty when the account
   *  never rotated. */
  readonly ownPredecessors: readonly CarriedOwnPredecessor[];
  /** At most one per group id, only for groups in `knownGroupIdsB64`. */
  readonly groupNames: readonly CarriedGroupName[];
  /** At most one per person key, never this account's own. */
  readonly profiles: readonly CarriedProfile[];
  /** At most one per account id. */
  readonly pendingContacts: readonly CarriedPendingContact[];
  /** v4: at most one entry per channel, ascending by channel-id bytes. */
  readonly channelSeeds: readonly CarriedChannelSeeds[];
}

/** What the sending device adds beside the localStorage state (read from its
 *  encrypted stores by `carried-state.ts`). */
export interface CarriedExtras {
  readonly groupNames?: readonly CarriedGroupName[];
  readonly profiles?: readonly CarriedProfile[];
  readonly pendingContacts?: readonly CarriedPendingContact[];
  /** v4: every epoch seed this device holds, per channel (hex id ->
   *  epoch -> seed), for the channels with a LIVE register; trimmed here. */
  readonly channelSeeds?: ReadonlyMap<string, ReadonlyMap<number, Uint8Array>>;
}

/** Read the account's contacts, pins and the given group ids into a bundle.
 *  `ownerAccountIdB64` is the account being paired (the contacts are that
 *  account's, never another account's on this browser). `ownPredecessors`
 *  is {@link collectOwnPredecessors}'s answer on the sending device;
 *  `extras` the names and pending contacts (bounded and de-duplicated here,
 *  and trimmed so the encoded bundle stays under its size cap). */
export function buildStateBundle(
  knownGroupIdsB64: readonly string[],
  ownerAccountIdB64: string,
  ownPredecessors: readonly CarriedOwnPredecessor[] = [],
  extras: CarriedExtras = {},
): StateBundle {
  const contacts = loadAccepted(ownerAccountIdB64);
  const pins: CarriedPin[] = [];
  for (const c of contacts) {
    const accountIdB64 = accountIdB64OfHex(c.accountIdHex);
    if (accountIdB64 === null) continue;
    const pin = readPin(accountIdB64, ownerAccountIdB64);
    if (pin === undefined) continue;
    pins.push({
      accountIdB64,
      pinnedSignaturePubkeyB64: pin.pinnedSignaturePubkeyB64,
      rotationMarker: pin.rotationMarker,
      provenPredecessorsB64: carriedKeys(pin.provenPredecessorsB64 ?? []),
    });
  }
  const seedsByChannel = extras.channelSeeds ?? new Map<string, ReadonlyMap<number, Uint8Array>>();
  return fitToSizeCap({
    v: 4,
    contacts,
    pins,
    knownGroupIdsB64: [...knownGroupIdsB64],
    ownPredecessors: boundedOwnPredecessors(ownPredecessors),
    groupNames: carriedGroupNames(extras.groupNames ?? [], knownGroupIdsB64),
    profiles: carriedProfiles(extras.profiles ?? []),
    pendingContacts: carriedPending(extras.pendingContacts ?? []),
    channelSeeds: carriedChannelSeeds(seedsByChannel, MAX_CARRIED_SEEDS_PER_CHANNEL),
  }, seedsByChannel);
}

/** v4: per channel the newest `perChannel` seeds held, ascending by epoch;
 *  channels ascending by id bytes (lower-case hex sorts the same), bounded.
 *  Pure. */
export function carriedChannelSeeds(
  seeds: ReadonlyMap<string, ReadonlyMap<number, Uint8Array>>,
  perChannel: number,
): CarriedChannelSeeds[] {
  if (perChannel <= 0) return [];
  const out: CarriedChannelSeeds[] = [];
  const ids = [...seeds.keys()].map((h) => h.toLowerCase()).filter((h) => /^[0-9a-f]{32}$/.test(h)).sort();
  for (const idHex of ids) {
    const inner = seeds.get(idHex);
    if (inner === undefined) continue;
    const epochs = [...inner.keys()]
      .filter((e) => Number.isSafeInteger(e) && e >= 0 && e <= MAX_EPOCH && inner.get(e)?.length === CHANNEL_SEED_LENGTH)
      .sort((a, b) => a - b);
    const kept = epochs.slice(Math.max(0, epochs.length - Math.min(perChannel, MAX_CARRIED_SEEDS_PER_CHANNEL)));
    if (kept.length === 0) continue;
    out.push({
      channelIdB64: base64Encode(hexBytes(idHex)),
      seeds: kept.map((epoch) => ({ epoch, seedB64: base64Encode(inner.get(epoch) as Uint8Array) })),
    });
    if (out.length === MAX_CARRIED_SEED_CHANNELS) break;
  }
  return out;
}

function hexBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** One name per KNOWN group, in the known-group order, bounded. */
function carriedGroupNames(
  names: readonly CarriedGroupName[],
  knownGroupIdsB64: readonly string[],
): CarriedGroupName[] {
  const byGroup = new Map<string, CarriedGroupName>();
  for (const n of names) {
    if (!byGroup.has(n.groupIdB64) && decodesNonEmpty(n.envelopeB64)) byGroup.set(n.groupIdB64, n);
  }
  const out: CarriedGroupName[] = [];
  for (const id of knownGroupIdsB64) {
    const n = byGroup.get(id);
    if (n === undefined) continue;
    byGroup.delete(id);
    out.push(n);
    if (out.length === MAX_CARRIED_GROUP_NAMES) break;
  }
  return out;
}

/** One profile per person key, ordered by key, bounded. */
function carriedProfiles(profiles: readonly CarriedProfile[]): CarriedProfile[] {
  const byKey = new Map<string, CarriedProfile>();
  for (const p of profiles) {
    if (byKey.has(p.personKeyB64)) continue;
    if (!decodesTo(p.personKeyB64, SIGNATURE_KEY_LENGTH) || !decodesNonEmpty(p.envelopeB64)) continue;
    byKey.set(p.personKeyB64, p);
  }
  return [...byKey.values()]
    .sort((a, b) => (a.personKeyB64 < b.personKeyB64 ? -1 : a.personKeyB64 > b.personKeyB64 ? 1 : 0))
    .slice(0, MAX_CARRIED_PROFILES);
}

/** One record per account id, in the order held, bounded; each with at most
 *  the first {@link MAX_CARRIED_HELD_MESSAGES} held messages. */
function carriedPending(list: readonly CarriedPendingContact[]): CarriedPendingContact[] {
  const out: CarriedPendingContact[] = [];
  const seen = new Set<string>();
  for (const c of list) {
    const bounded = { ...c, heldMessages: c.heldMessages.slice(0, MAX_CARRIED_HELD_MESSAGES) };
    if (seen.has(c.accountIdHex) || pendingContactProblem(bounded) !== null) continue;
    seen.add(c.accountIdHex);
    out.push(bounded);
    if (out.length === MAX_CARRIED_PENDING_CONTACTS) break;
  }
  return out;
}

/**
 * Keep the encoded bundle under {@link MAX_BUNDLE_BYTES}, which the
 * receiver refuses as a whole: the profiles go first (from the end of the
 * key order), then the group names (from the end of the known-group
 * order), then the held messages of the pending contacts. Contacts, pins and keys are never trimmed. Pure.
 */
function fitToSizeCap(
  bundle: StateBundle,
  seeds: ReadonlyMap<string, ReadonlyMap<number, Uint8Array>> = new Map(),
): StateBundle {
  const size = (b: StateBundle): number => encodeStateBundle(b).length;
  let b = bundle;
  if (size(b) <= MAX_BUNDLE_BYTES) return b;
  // v4: the seeds never push out another array. The other arrays are fitted
  // with no seeds, then the per-channel seed count goes down the ladder for
  // every channel at once until the bundle fits.
  if (b.channelSeeds.length > 0) {
    const fitted = fitToSizeCap({ ...b, channelSeeds: [] });
    for (const perChannel of SEED_LADDER) {
      const withSeeds: StateBundle = { ...fitted, channelSeeds: carriedChannelSeeds(seeds, perChannel) };
      if (size(withSeeds) <= MAX_BUNDLE_BYTES) return withSeeds;
    }
    return fitted;
  }
  const entryBytes = (x: unknown): number => new TextEncoder().encode(JSON.stringify(x)).length + 1;
  let over = size(b) - MAX_BUNDLE_BYTES;
  const profiles = [...b.profiles];
  while (over > 0 && profiles.length > 0) over -= entryBytes(profiles.pop());
  b = { ...b, profiles };
  const groupNames = [...b.groupNames];
  while (over > 0 && groupNames.length > 0) over -= entryBytes(groupNames.pop());
  b = { ...b, groupNames };
  if (over > 0) b = { ...b, pendingContacts: b.pendingContacts.map((c) => ({ ...c, heldMessages: [] })) };
  return b;
}

function decodesNonEmpty(b64: string): boolean {
  try {
    return base64Decode(b64).length > 0;
  } catch {
    return false;
  }
}

const ACCOUNT_ID_HEX_RE = /^[0-9a-f]{32}$/;
const LINK_ID_LENGTH = 16;

/** `null` when one carried held message is well-formed, else why not. */
function heldMessageProblem(m: unknown): string | null {
  if (typeof m !== "object" || m === null) return "not an object";
  const r = m as Record<string, unknown>;
  if (typeof r["text"] !== "string") return "text";
  if (!Array.isArray(r["attachments"])) return "attachments";
  const replyTo = r["replyTo"];
  if (replyTo !== null && (typeof replyTo !== "number" || !Number.isSafeInteger(replyTo))) return "replyTo";
  const heldAtMs = r["heldAtMs"];
  if (typeof heldAtMs !== "number" || !Number.isSafeInteger(heldAtMs)) return "heldAtMs";
  return null;
}

/** `null` when one carried pending contact is well-formed, else why not. */
function pendingContactProblem(c: unknown): string | null {
  if (typeof c !== "object" || c === null) return "not an object";
  const r = c as Record<string, unknown>;
  const accountIdHex = r["accountIdHex"];
  if (typeof accountIdHex !== "string" || !ACCOUNT_ID_HEX_RE.test(accountIdHex)) return "accountIdHex";
  const sig = r["signaturePubkeyB64"];
  if (typeof sig !== "string" || !decodesTo(sig, SIGNATURE_KEY_LENGTH)) return "signaturePubkeyB64";
  const x = r["x25519PubkeyB64"];
  if (typeof x !== "string" || !decodesTo(x, SIGNATURE_KEY_LENGTH)) return "x25519PubkeyB64";
  if (typeof r["displayName"] !== "string") return "displayName";
  if (typeof r["username"] !== "string") return "username";
  const addedAtMs = r["addedAtMs"];
  if (typeof addedAtMs !== "number" || !Number.isSafeInteger(addedAtMs)) return "addedAtMs";
  const link = r["linkIdB64"];
  if (typeof link !== "string" || !decodesTo(link, LINK_ID_LENGTH)) return "linkIdB64";
  const held = r["heldMessages"];
  if (!Array.isArray(held) || held.length > MAX_CARRIED_HELD_MESSAGES) return "heldMessages";
  for (const m of held as unknown[]) if (heldMessageProblem(m) !== null) return "heldMessage";
  return null;
}

function decodesTo(b64: string, length: number): boolean {
  try {
    return base64Decode(b64).length === length;
  } catch {
    return false;
  }
}

/** What a sender puts on the wire for one contact: well-formed keys only,
 *  and never more than the bound (the NEWEST are kept: they are the ones a
 *  leaf or a grant met today is most likely bound to). */
function carriedKeys(keysB64: readonly string[]): string[] {
  const valid = keysB64.filter((k) => decodesTo(k, SIGNATURE_KEY_LENGTH));
  return valid.slice(Math.max(0, valid.length - MAX_CARRIED_PREDECESSORS));
}

/** The own chain a sender puts on the wire. Over the bound, the OLDEST (the
 *  key the conversation leaves are bound to) and the newest are kept. */
export function boundedOwnPredecessors(
  own: readonly CarriedOwnPredecessor[],
): CarriedOwnPredecessor[] {
  const valid = own.filter(
    (p) => decodesTo(p.seedB64, ACCOUNT_SEED_LENGTH) && Number.isSafeInteger(p.adoptedAtMs),
  );
  if (valid.length <= MAX_CARRIED_PREDECESSORS) return valid;
  const first = valid[0];
  const newest = valid.slice(valid.length - (MAX_CARRIED_PREDECESSORS - 1));
  return first === undefined ? newest : [first, ...newest];
}

export function encodeStateBundle(bundle: StateBundle): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(bundle));
}

/** Decode a bundle, refusing anything that is not the shape above. A refused
 *  bundle is `null` — the joiner then lands without contacts, as before.
 *  A bundle of any other version is refused (no dual accept). */
export function decodeStateBundle(bytes: Uint8Array): StateBundle | null {
  if (bytes.length === 0 || bytes.length > MAX_BUNDLE_BYTES) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const o = parsed as Record<string, unknown>;
  if (o["v"] !== 4) return null;
  if (!Array.isArray(o["contacts"]) || !Array.isArray(o["pins"])) return null;
  if (!Array.isArray(o["knownGroupIdsB64"])) return null;
  if (!Array.isArray(o["ownPredecessors"])) return null;
  if (!Array.isArray(o["groupNames"]) || !Array.isArray(o["profiles"])) return null;
  if (!Array.isArray(o["pendingContacts"])) return null;
  if (!Array.isArray(o["channelSeeds"])) return null;
  const contacts: AcceptedContact[] = [];
  for (const c of o["contacts"] as unknown[]) {
    if (typeof c !== "object" || c === null) return null;
    const r = c as Record<string, unknown>;
    if (typeof r["signaturePubkeyB64"] !== "string") return null;
    if (typeof r["displayName"] !== "string") return null;
    if (typeof r["acceptedAtMs"] !== "number") return null;
    contacts.push(c as AcceptedContact);
  }
  const pins: CarriedPin[] = [];
  for (const p of o["pins"] as unknown[]) {
    if (typeof p !== "object" || p === null) return null;
    const r = p as Record<string, unknown>;
    if (
      typeof r["accountIdB64"] !== "string" ||
      typeof r["pinnedSignaturePubkeyB64"] !== "string" ||
      typeof r["rotationMarker"] !== "number"
    ) {
      return null;
    }
    const proven = r["provenPredecessorsB64"];
    if (!Array.isArray(proven) || proven.length > MAX_CARRIED_PREDECESSORS) return null;
    const provenPredecessorsB64: string[] = [];
    for (const k of proven as unknown[]) {
      if (typeof k !== "string" || !decodesTo(k, SIGNATURE_KEY_LENGTH)) return null;
      provenPredecessorsB64.push(k);
    }
    pins.push({
      accountIdB64: r["accountIdB64"],
      pinnedSignaturePubkeyB64: r["pinnedSignaturePubkeyB64"],
      rotationMarker: r["rotationMarker"],
      provenPredecessorsB64,
    });
  }
  const knownGroupIdsB64: string[] = [];
  for (const g of o["knownGroupIdsB64"] as unknown[]) {
    if (typeof g !== "string") return null;
    knownGroupIdsB64.push(g);
  }
  const own = o["ownPredecessors"] as unknown[];
  if (own.length > MAX_CARRIED_PREDECESSORS) return null;
  const ownPredecessors: CarriedOwnPredecessor[] = [];
  for (const p of own) {
    if (typeof p !== "object" || p === null) return null;
    const r = p as Record<string, unknown>;
    const seedB64 = r["seedB64"];
    const adoptedAtMs = r["adoptedAtMs"];
    if (typeof seedB64 !== "string" || !decodesTo(seedB64, ACCOUNT_SEED_LENGTH)) return null;
    if (typeof adoptedAtMs !== "number" || !Number.isSafeInteger(adoptedAtMs)) return null;
    ownPredecessors.push({ seedB64, adoptedAtMs });
  }
  const names = o["groupNames"] as unknown[];
  if (names.length > MAX_CARRIED_GROUP_NAMES) return null;
  const groupNames: CarriedGroupName[] = [];
  const namedGroups = new Set<string>();
  for (const n of names) {
    if (typeof n !== "object" || n === null) return null;
    const r = n as Record<string, unknown>;
    const groupIdB64 = r["groupIdB64"];
    const envelopeB64 = r["envelopeB64"];
    if (typeof groupIdB64 !== "string" || !decodesNonEmpty(groupIdB64)) return null;
    if (typeof envelopeB64 !== "string" || !decodesNonEmpty(envelopeB64)) return null;
    if (namedGroups.has(groupIdB64)) return null;
    namedGroups.add(groupIdB64);
    groupNames.push({ groupIdB64, envelopeB64 });
  }
  const carried = o["profiles"] as unknown[];
  if (carried.length > MAX_CARRIED_PROFILES) return null;
  const profiles: CarriedProfile[] = [];
  const profiled = new Set<string>();
  for (const p of carried) {
    if (typeof p !== "object" || p === null) return null;
    const r = p as Record<string, unknown>;
    const personKeyB64 = r["personKeyB64"];
    const envelopeB64 = r["envelopeB64"];
    if (typeof personKeyB64 !== "string" || !decodesTo(personKeyB64, SIGNATURE_KEY_LENGTH)) return null;
    if (typeof envelopeB64 !== "string" || !decodesNonEmpty(envelopeB64)) return null;
    if (profiled.has(personKeyB64)) return null;
    profiled.add(personKeyB64);
    profiles.push({ personKeyB64, envelopeB64 });
  }
  const pendingRaw = o["pendingContacts"] as unknown[];
  if (pendingRaw.length > MAX_CARRIED_PENDING_CONTACTS) return null;
  const pendingContacts: CarriedPendingContact[] = [];
  const pendingIds = new Set<string>();
  for (const c of pendingRaw) {
    if (pendingContactProblem(c) !== null) return null;
    const r = c as Record<string, unknown>;
    const accountIdHex = r["accountIdHex"] as string;
    if (pendingIds.has(accountIdHex)) return null;
    pendingIds.add(accountIdHex);
    pendingContacts.push({
      accountIdHex,
      signaturePubkeyB64: r["signaturePubkeyB64"] as string,
      x25519PubkeyB64: r["x25519PubkeyB64"] as string,
      displayName: r["displayName"] as string,
      username: r["username"] as string,
      addedAtMs: r["addedAtMs"] as number,
      linkIdB64: r["linkIdB64"] as string,
      heldMessages: (r["heldMessages"] as Record<string, unknown>[]).map((m) => ({
        text: m["text"] as string,
        attachments: m["attachments"] as unknown[],
        replyTo: m["replyTo"] as number | null,
        heldAtMs: m["heldAtMs"] as number,
      })),
    });
  }
  const channelSeeds = decodeChannelSeeds(o["channelSeeds"] as unknown[]);
  if (channelSeeds === null) return null;
  return {
    v: 4,
    contacts,
    pins,
    knownGroupIdsB64,
    ownPredecessors,
    groupNames,
    profiles,
    pendingContacts,
    channelSeeds,
  };
}

/** v4: the `channelSeeds` array, or `null` (the whole bundle is refused) on
 *  any violation: more than 1000 entries, a channel id twice or not 16 bytes,
 *  `seeds` empty or over 32, an epoch not an integer in 0..=4294967295 or not
 *  strictly ascending, a seed not 32 bytes. */
function decodeChannelSeeds(raw: readonly unknown[]): CarriedChannelSeeds[] | null {
  if (raw.length > MAX_CARRIED_SEED_CHANNELS) return null;
  const out: CarriedChannelSeeds[] = [];
  const seen = new Set<string>();
  for (const e of raw) {
    if (typeof e !== "object" || e === null) return null;
    const r = e as Record<string, unknown>;
    const channelIdB64 = r["channelIdB64"];
    if (typeof channelIdB64 !== "string" || !decodesTo(channelIdB64, CHANNEL_ID_LENGTH)) return null;
    const idKey = bytesHex(base64Decode(channelIdB64));
    if (seen.has(idKey)) return null;
    seen.add(idKey);
    const list = r["seeds"];
    if (!Array.isArray(list) || list.length === 0 || list.length > MAX_CARRIED_SEEDS_PER_CHANNEL) return null;
    const seeds: CarriedEpochSeed[] = [];
    let last = -1;
    for (const s of list as unknown[]) {
      if (typeof s !== "object" || s === null) return null;
      const sr = s as Record<string, unknown>;
      const epoch = sr["epoch"];
      const seedB64 = sr["seedB64"];
      if (typeof epoch !== "number" || !Number.isInteger(epoch) || epoch < 0 || epoch > MAX_EPOCH) return null;
      if (epoch <= last) return null;
      if (typeof seedB64 !== "string" || !decodesTo(seedB64, CHANNEL_SEED_LENGTH)) return null;
      last = epoch;
      seeds.push({ epoch, seedB64 });
    }
    out.push({ channelIdB64, seeds });
  }
  return out;
}

function bytesHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/** v4, receiving device: the carried seeds per channel (hex id -> epoch ->
 *  seed). Pure. */
export function carriedSeedsOf(bundle: Pick<StateBundle, "channelSeeds">): Map<string, Map<number, Uint8Array>> {
  const out = new Map<string, Map<number, Uint8Array>>();
  for (const c of bundle.channelSeeds) {
    const inner = new Map<number, Uint8Array>();
    for (const s of c.seeds) inner.set(s.epoch, base64Decode(s.seedB64));
    out.set(bytesHex(base64Decode(c.channelIdB64)), inner);
  }
  return out;
}

/** Merge a bundle into this browser, into the list of `ownerAccountIdB64`
 *  (the account the new device joined): contacts that are not here yet, pins
 *  through the monotone setter, the known group ids for the chat list. The
 *  new device may run this before its session lands, so the account is
 *  named, not taken from the binding. */
export function applyStateBundle(
  bundle: StateBundle,
  ownerAccountIdB64: string,
): { contacts: number; pins: number } {
  const have = new Set(loadAccepted(ownerAccountIdB64).map((c) => c.signaturePubkeyB64));
  let contacts = 0;
  for (const c of bundle.contacts) {
    if (have.has(c.signaturePubkeyB64)) continue;
    upsertAccepted(c, ownerAccountIdB64);
    contacts += 1;
  }
  let pins = 0;
  for (const p of bundle.pins) {
    try {
      if (setPin(p.accountIdB64, p.pinnedSignaturePubkeyB64, p.rotationMarker, ownerAccountIdB64) === "set") {
        pins += 1;
      }
    } catch {
      // A pin the setter refuses (a lower marker than one already held) is
      // not ours to force.
    }
    // ADR-0074: the contact's proven predecessors, as the sending device
    // holds them. Recorded only beside the very key they were proven for;
    // a pin this device holds under another key keeps its own chain.
    try {
      recordCarriedProvenPredecessors(
        p.accountIdB64,
        p.pinnedSignaturePubkeyB64,
        p.provenPredecessorsB64,
        ownerAccountIdB64,
      );
    } catch {
      /* an unreadable pin store proves nothing; the gate records it later */
    }
  }
  saveKnownGroupIds(bundle.knownGroupIdsB64, ownerAccountIdB64);
  return { contacts, pins };
}

// ---- the own predecessors (ADR-0074, a device paired after a rotation) -----

/**
 * The predecessor seeds a device holds after a bundle arrived: the carried
 * chain (the sending device's, oldest first) with any seed this device held
 * and the bundle does not name appended. A device that already holds every
 * carried seed keeps its own list unchanged. Pure.
 */
export function mergeCarriedPredecessorSeeds(
  heldB64: readonly string[],
  carriedB64: readonly string[],
): string[] {
  if (carriedB64.every((seed) => heldB64.includes(seed))) return [...heldB64];
  const out: string[] = [];
  for (const seed of [...carriedB64, ...heldB64]) if (!out.includes(seed)) out.push(seed);
  return out;
}

/** The lower-case hex account signature key of a 64-byte account seed. */
export function accountKeyHexOfSeed(
  wasm: {
    readonly Identity: {
      fromSeed(seed: Uint8Array): { readonly signaturePublicKey: Uint8Array; free(): void };
    };
  },
  seed: Uint8Array,
): string {
  const identity = wasm.Identity.fromSeed(seed);
  try {
    let out = "";
    for (const byte of identity.signaturePublicKey) out += byte.toString(16).padStart(2, "0");
    return out;
  } finally {
    identity.free();
  }
}

/**
 * Sending device: its own predecessor chain for the bundle, oldest first.
 * A predecessor with no adoption time stored gets one now (the same rule
 * the mailbox poller applies). `keyHexOfSeed` answers the lower-case hex
 * account signature key of a 64-byte seed. Seeds are not logged.
 */
export async function collectOwnPredecessors(
  accountIdB64: string,
  deviceKey: DeviceCipher,
  keyHexOfSeed: (seed: Uint8Array) => string,
  nowMs: number = Date.now(),
): Promise<CarriedOwnPredecessor[]> {
  const state = await getKeyRotationState(accountIdB64, deviceKey);
  const out: CarriedOwnPredecessor[] = [];
  for (const seedB64 of state.predecessorSeedsB64) {
    const seed = base64Decode(seedB64);
    let keyHex: string;
    try {
      keyHex = keyHexOfSeed(seed);
    } finally {
      seed.fill(0);
    }
    stampPredecessorAdoption(accountIdB64, keyHex, nowMs);
    out.push({
      seedB64,
      adoptedAtMs: readPredecessorAdoptedAtMs(accountIdB64, keyHex) ?? Math.floor(nowMs),
    });
  }
  return out;
}

/**
 * Receiving device (a pairing state bundle or a restored backup, ADR-0074
 * section 3.7): store the own predecessors exactly as a kept device
 * that adopted holds them. The seeds go into the key-rotation row sealed
 * under the device key (the store `adoptRotatedKeys` writes), so the next
 * session install binds the device's conversation identity under the OLDEST
 * one, reads the chain into the own-key resolver, and the predecessor
 * mailboxes are polled; each mailbox's window counts from the SENDING
 * device's adoption time (an existing local time is kept).
 *
 * Returns how many seeds this device did not hold before. Must run BEFORE
 * the session's identities are installed; a caller that runs it later must
 * install them again before any KeyPackage is published.
 */
export async function applyOwnPredecessors(
  bundle: Pick<StateBundle, "ownPredecessors">,
  accountIdB64: string,
  deviceKey: DeviceCipher,
  keyHexOfSeed: (seed: Uint8Array) => string,
): Promise<number> {
  if (bundle.ownPredecessors.length === 0) return 0;
  const carried = bundle.ownPredecessors.map((p) => p.seedB64);
  let added = 0;
  await updateKeyRotationState(accountIdB64, deviceKey, (state): KeyRotationState => {
    const next = mergeCarriedPredecessorSeeds(state.predecessorSeedsB64, carried);
    added = next.filter((seed) => !state.predecessorSeedsB64.includes(seed)).length;
    return added === 0 ? state : { ...state, predecessorSeedsB64: next };
  });
  for (const p of bundle.ownPredecessors) {
    const seed = base64Decode(p.seedB64);
    try {
      stampPredecessorAdoption(accountIdB64, keyHexOfSeed(seed), p.adoptedAtMs);
    } finally {
      seed.fill(0);
    }
  }
  return added;
}

// ---- known group ids (drives the one "Synchronizing..." label) -------------
//
// The list belongs to the account the bundle was paired into, not to the
// browser: `lakko.pairing.known-groups.v2.<accountIdB64>`. It used to be one
// device-wide row, so a second account signed in on the same browser showed
// "Synchronizing..." for the first account's groups (2026-09-30).
//
// Which account a call means: the trailing `owner` argument, otherwise the
// signed-in account bound through `bindContactsAccount`. With no account a
// read is empty and a write is a no-op. Log out keeps the list; Forget device
// drops it ({@link clearKnownGroupIds}).

function knownGroupsOwner(owner: string | undefined): string | null {
  const account = owner ?? boundContactsAccount();
  return account === null || account.length === 0 ? null : account;
}

export function saveKnownGroupIds(idsB64: readonly string[], owner?: string): void {
  const account = knownGroupsOwner(owner);
  if (account === null) return;
  try {
    if (idsB64.length === 0) localStorage.removeItem(KNOWN_GROUPS_PREFIX + account);
    else localStorage.setItem(KNOWN_GROUPS_PREFIX + account, JSON.stringify(idsB64));
  } catch {
    /* storage unavailable: the label falls back to "no chats yet" */
  }
}

export function loadKnownGroupIds(owner?: string): string[] {
  const account = knownGroupsOwner(owner);
  if (account === null) return [];
  try {
    const raw = localStorage.getItem(KNOWN_GROUPS_PREFIX + account);
    if (raw === null) return [];
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

/** The known groups this device has not joined yet, for `owner` (default:
 *  the signed-in account). Clears the stored list once every known group is
 *  held, so the label cannot outlive its cause. */
export function pendingKnownGroups(joinedIdsB64: readonly string[], owner?: string): number {
  const account = knownGroupsOwner(owner);
  if (account === null) return 0;
  const known = loadKnownGroupIds(account);
  if (known.length === 0) return 0;
  const joined = new Set(joinedIdsB64);
  const pending = known.filter((id) => !joined.has(id));
  if (pending.length === 0) saveKnownGroupIds([], account);
  return pending.length;
}

/** Forget device: drop `accountIdB64`'s list and the retired device-wide
 *  one. Another account's list on this browser is left alone. */
export function clearKnownGroupIds(accountIdB64: string): void {
  try {
    localStorage.removeItem(KNOWN_GROUPS_PREFIX + accountIdB64);
    localStorage.removeItem(RETIRED_KNOWN_GROUPS_KEY);
  } catch {
    /* storage unavailable: nothing to clear */
  }
}

// ---- the tagged mailbox row ------------------------------------------------

export function wrapStateRow(sealed: Uint8Array): Uint8Array {
  const out = new Uint8Array(STATE_ROW_MAGIC.length + sealed.length);
  out.set(STATE_ROW_MAGIC, 0);
  out.set(sealed, STATE_ROW_MAGIC.length);
  return out;
}

export function isStateRow(row: Uint8Array): boolean {
  if (row.length <= STATE_ROW_MAGIC.length) return false;
  for (let i = 0; i < STATE_ROW_MAGIC.length; i += 1) {
    if (row[i] !== STATE_ROW_MAGIC[i]) return false;
  }
  return true;
}

export function unwrapStateRow(row: Uint8Array): Uint8Array | null {
  return isStateRow(row) ? row.slice(STATE_ROW_MAGIC.length) : null;
}

export async function publishStateBundle(
  rendezvousPseudonym: Uint8Array,
  sealed: Uint8Array,
  options: { ds?: DsClient } = {},
): Promise<void> {
  const ds = options.ds ?? new DsClient();
  await ds.publishRendezvous(rendezvousPseudonym, wrapStateRow(sealed));
}

export type StateBundleOutcome =
  | { readonly kind: "received"; readonly sealed: Uint8Array }
  | { readonly kind: "timeout" }
  | { readonly kind: "cancelled" };

/** Joiner side: the state row, past the Welcome cursor, skipping the history
 *  and recovery rows. Soft timeout; an older scanner never sends one. */
export async function pollForStateBundle(
  rendezvousPseudonym: Uint8Array,
  afterCursor: number,
  options: { ds?: DsClient; signal?: AbortSignal; timeoutMs?: number } = {},
): Promise<StateBundleOutcome> {
  const ds = options.ds ?? new DsClient();
  const signal = options.signal;
  const timeoutMs = options.timeoutMs ?? STATE_POLL_TIMEOUT_MS;
  const deadline = performance.now() + timeoutMs;
  let cursor = afterCursor;
  while (true) {
    if (signal?.aborted) return { kind: "cancelled" };
    const remainingMs = deadline - performance.now();
    if (remainingMs <= 0) return { kind: "timeout" };
    const waitSecs = Math.min(
      PER_REQUEST_LONG_POLL_SECS,
      Math.max(1, Math.floor(remainingMs / 1000)),
    );
    let result;
    try {
      result = await ds.fetchRendezvous(rendezvousPseudonym, { after: cursor, limit: 1, waitSecs });
    } catch (err) {
      if (err instanceof DsHttpError && err.status === 404) continue;
      if (err instanceof DsNetworkError) continue;
      throw err;
    }
    if (result.messages.length > 0) {
      const first = result.messages[0];
      if (first === undefined) continue;
      const sealed = unwrapStateRow(first.payload);
      if (sealed !== null) return { kind: "received", sealed };
      cursor = first.id;
      continue;
    }
    cursor = result.messages.reduce((acc, m) => Math.max(acc, m.id), cursor);
  }
}

function accountIdB64OfHex(hex: string | undefined): string | null {
  if (hex === undefined || hex.length !== 32 || !/^[0-9a-f]+$/i.test(hex)) return null;
  const out = new Uint8Array(16);
  for (let i = 0; i < 16; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return base64Encode(out);
}
