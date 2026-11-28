// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 sections 2, 3, 4, 8 and 11 — a group's `@name` at discovery and
 * inside the group.
 *
 * The write key names no account: `handle_seed = handleWriteSeed(journal root
 * seed)`, the journal root seed being ADR-0057's, derived from the OWNER's
 * account seed and the group id. So only the owner writes; every device of the
 * owner derives the same key with nothing to transfer.
 *
 * - {@link claimGroupHandle}: `POST /discovery/handle/claim` (one
 *   `discovery-register` token). Public: the sealed record and the name terms
 *   go with it. The answer's certificate binds the name to the write key.
 * - {@link updateGroupHandle}: `PUT /discovery/handle` (flip public / private,
 *   republish the record, renew the lease).
 * - {@link replaceGroupHandle}: `POST /discovery/handle/replace` (a new name).
 * - {@link retireGroupHandle}: `DELETE /discovery/handle`.
 *
 * After a write the owner signs the in-group envelope (`ConversationHandle`)
 * over the group id and the fields; any member relays it to a newcomer.
 *
 * Refusals: {@link HandleWriteError} with `name-taken` (the held state, "That
 * @name isn't available. Try another."), a `429` from the token pool ("Too
 * many today"), a network error. The caller maps them to copy.
 */

import type { Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { handleWrite, lookupHandle, type HandleWriteResult } from "../discovery/handle-lookup";
import { namePair, noteHandleHeld, termPairsInBatch } from "../discovery/handle-check";
import { ScopedDiscoveryClient } from "../reach/scoped-discovery";
import { avatarToWasm, type AvatarData } from "../profile/avatar-model";
import type { ProfileAvatar as WasmProfileAvatar } from "../../wasm/lakko_ffi_wasm";
import type { Wasm } from "../wasm";
import type { StoredConversationHandle } from "./group-settings-envelope";

/** What a public group's record says (section 4). */
export interface PublicGroupFacts {
  readonly name: string;
  /** Member count; the record carries its band. */
  readonly memberCount: number;
  /** The group's knock public key (section 5). */
  readonly knockPublicKey: Uint8Array;
  /** Its version: a searcher seals its request naming it. */
  readonly knockVersion: number;
  readonly avatar: AvatarData | null;
}

/** The ADR-0059 size band of a member count: `<10`, `10+`, `100+`, `1k+`,
 *  `10k+`, `100k+` as 0..5. */
export function sizeBandOf(count: number): number {
  if (count >= 100_000) return 5;
  if (count >= 10_000) return 4;
  if (count >= 1_000) return 3;
  if (count >= 100) return 2;
  if (count >= 10) return 1;
  return 0;
}

/** The handle write seed of a group this account owns. The caller wipes it. */
export function groupHandleSeed(wasm: Wasm, accountIdentity: WasmIdentity, groupId: Uint8Array): Uint8Array {
  const root = wasm.dsJournalRootSeed(accountIdentity.ed25519Seed, groupId);
  try {
    return wasm.handleWriteSeed(root);
  } finally {
    root.fill(0);
  }
}

function concat(parts: readonly Uint8Array[]): Uint8Array {
  const len = parts.reduce((n, p) => n + p.length, 0);
  const out = new Uint8Array(len);
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

/** What a public record describes: a group (32-byte id, knock key) or a
 *  channel (16-byte id, read key). */
interface RecordSubject {
  readonly kind: "group" | "channel";
  readonly id: Uint8Array;
  readonly key: Uint8Array;
  readonly name: string;
  readonly description: string;
  readonly sizeBand: number;
  readonly avatar: AvatarData | null;
  /** The knock key's version (a group); 0 for a channel. */
  readonly knockVersion: number;
}

/** The record and terms a public name publishes; nothing for a private one. */
async function recordContent(
  wasm: Wasm,
  subject: RecordSubject,
  handle: string,
  handleOutput: Uint8Array,
  handleLookupKey: Uint8Array,
): Promise<{ record: Uint8Array; terms: Uint8Array[] }> {
  const scoped = new ScopedDiscoveryClient(() => null);
  const nameTerms = wasm.nameSearchTerms(subject.name).slice(0, Math.max(0, wasm.maxHandleTerms() - 1));
  const outputs: Uint8Array[] = [handleOutput];
  const lookupKeys: Uint8Array[] = [handleLookupKey];
  // The record's own words in ONE batch round (`POST /discovery/oprf/batch`,
  // one register token), kept for the session and asked once more after a
  // rate limit (`discovery/handle-check.ts`).
  const pairs = await termPairsInBatch(nameTerms, (missing) =>
    scoped.ownTermsOprf(missing.map((value) => ({ kind: "term" as const, value }))),
  );
  for (const pair of pairs) {
    outputs.push(pair.output);
    lookupKeys.push(pair.lookupKey);
  }
  const avatar = subject.avatar === null ? undefined : avatarToWasm<WasmProfileAvatar>(wasm, subject.avatar);
  const payload = new wasm.HandleRecordPayload(
    subject.kind,
    BigInt(Date.now()),
    subject.id,
    subject.key,
    handle,
    subject.name,
    subject.description,
    subject.sizeBand,
    avatar,
    subject.knockVersion,
  );
  try {
    const record = wasm.composeHandleRecord(payload, concat(outputs), BigInt(Date.now()));
    return { record, terms: lookupKeys };
  } finally {
    payload.free();
  }
}

async function publicContent(
  wasm: Wasm,
  groupId: Uint8Array,
  handle: string,
  handleOutput: Uint8Array,
  handleLookupKey: Uint8Array,
  facts: PublicGroupFacts,
): Promise<{ record: Uint8Array; terms: Uint8Array[] }> {
  return recordContent(
    wasm,
    {
      kind: "group",
      id: groupId,
      key: facts.knockPublicKey,
      name: facts.name,
      // A group has no description (ADR-0087 section 4: only channels do).
      description: "",
      sizeBand: sizeBandOf(facts.memberCount),
      avatar: facts.avatar,
      knockVersion: facts.knockVersion,
    },
    handle,
    handleOutput,
    handleLookupKey,
  );
}

/** The signed in-group envelope for a name the server just certified. */
function signedEnvelope(
  wasm: Wasm,
  seed: Uint8Array,
  groupId: Uint8Array,
  handle: string,
  isPublic: boolean,
  result: HandleWriteResult,
): StoredConversationHandle {
  const stamp = BigInt(Date.now());
  const h = wasm.ConversationHandle.sign(seed, groupId, stamp, isPublic, handle, result.certificate);
  try {
    const envelope = wasm.composeConversationHandleEnvelope(h);
    return { handle, isPublic, updatedAtMs: stamp, certificate: result.certificate, envelope };
  } finally {
    h.free();
  }
}

/** Take `handle` for a group this account owns (Create, or a lapsed name). */
export async function claimGroupHandle(args: {
  readonly wasm: Wasm;
  readonly accountIdentity: WasmIdentity;
  readonly groupId: Uint8Array;
  readonly handle: string;
  readonly isPublic: boolean;
  readonly facts: PublicGroupFacts | null;
}): Promise<StoredConversationHandle> {
  const { wasm, groupId } = args;
  const pair = await namePair(args.handle);
  const seed = groupHandleSeed(wasm, args.accountIdentity, groupId);
  try {
    let record: Uint8Array | null = null;
    let terms: Uint8Array[] = [];
    if (args.isPublic && args.facts !== null) {
      ({ record, terms } = await publicContent(wasm, groupId, pair.normalized, pair.output, pair.lookupKey, args.facts));
    }
    const hash = wasm.handleContentHash(record ?? undefined, concat(terms));
    const statement = wasm.signHandleClaim(seed, pair.lookupKey, args.isPublic, hash);
    const result = await handleWrite("claim", statement, record, terms);
    if (result === null) throw new Error("handle claim answered no certificate");
    noteHandleHeld(pair.normalized);
    return signedEnvelope(wasm, seed, groupId, pair.normalized, args.isPublic, result);
  } finally {
    seed.fill(0);
  }
}

/** Flip public / private, or republish (the record and lease). */
export async function updateGroupHandle(args: {
  readonly wasm: Wasm;
  readonly accountIdentity: WasmIdentity;
  readonly groupId: Uint8Array;
  readonly handle: string;
  readonly isPublic: boolean;
  readonly facts: PublicGroupFacts | null;
}): Promise<StoredConversationHandle> {
  const { wasm, groupId } = args;
  const pair = await namePair(args.handle);
  const seed = groupHandleSeed(wasm, args.accountIdentity, groupId);
  try {
    let record: Uint8Array | null = null;
    let terms: Uint8Array[] = [];
    if (args.isPublic && args.facts !== null) {
      ({ record, terms } = await publicContent(wasm, groupId, pair.normalized, pair.output, pair.lookupKey, args.facts));
    }
    const hash = wasm.handleContentHash(record ?? undefined, concat(terms));
    const statement = wasm.signHandleUpdate(seed, pair.lookupKey, args.isPublic, hash);
    const result = await handleWrite("update", statement, record, terms);
    if (result === null) throw new Error("handle update answered no certificate");
    return signedEnvelope(wasm, seed, groupId, pair.normalized, args.isPublic, result);
  } finally {
    seed.fill(0);
  }
}

/** Move to a new name; the old one is retired (held 30 days). */
export async function replaceGroupHandle(args: {
  readonly wasm: Wasm;
  readonly accountIdentity: WasmIdentity;
  readonly groupId: Uint8Array;
  readonly oldHandle: string;
  readonly newHandle: string;
  readonly isPublic: boolean;
  readonly facts: PublicGroupFacts | null;
}): Promise<StoredConversationHandle> {
  const { wasm, groupId } = args;
  const old = await namePair(args.oldHandle);
  const next = await namePair(args.newHandle);
  const seed = groupHandleSeed(wasm, args.accountIdentity, groupId);
  try {
    let record: Uint8Array | null = null;
    let terms: Uint8Array[] = [];
    if (args.isPublic && args.facts !== null) {
      ({ record, terms } = await publicContent(wasm, groupId, next.normalized, next.output, next.lookupKey, args.facts));
    }
    const hash = wasm.handleContentHash(record ?? undefined, concat(terms));
    const statement = wasm.signHandleReplace(seed, old.lookupKey, next.lookupKey, args.isPublic, hash);
    const result = await handleWrite("replace", statement, record, terms);
    if (result === null) throw new Error("handle replace answered no certificate");
    return signedEnvelope(wasm, seed, groupId, next.normalized, args.isPublic, result);
  } finally {
    seed.fill(0);
  }
}

/** ADR-0087 sections 3 and 5 — after an ADR-0074 rotation moved the journal
 *  root, move the name's write key to the handle key of the new root (the
 *  `rekey` statement, signed by the old one), and sign the in-group envelope
 *  with the new key. */
export async function rekeyGroupHandle(args: {
  readonly wasm: Wasm;
  readonly oldIdentity: WasmIdentity;
  readonly accountIdentity: WasmIdentity;
  readonly groupId: Uint8Array;
  readonly handle: string;
  readonly isPublic: boolean;
}): Promise<StoredConversationHandle> {
  const { wasm, groupId } = args;
  const pair = await namePair(args.handle);
  const oldSeed = groupHandleSeed(wasm, args.oldIdentity, groupId);
  const seed = groupHandleSeed(wasm, args.accountIdentity, groupId);
  try {
    const statement = wasm.signHandleRekey(oldSeed, pair.lookupKey, wasm.handleWritePublicKey(seed));
    const result = await handleWrite("rekey", statement);
    if (result === null) throw new Error("handle rekey answered no certificate");
    return signedEnvelope(wasm, seed, groupId, pair.normalized, args.isPublic, result);
  } finally {
    oldSeed.fill(0);
    seed.fill(0);
  }
}

/** A retry of a New group: move the name from the key of an earlier attempt's
 *  group to this attempt's group (the `rekey` statement, signed by the earlier
 *  key). The caller then writes this group's content with
 *  {@link updateGroupHandle}. */
export async function moveGroupHandle(args: {
  readonly wasm: Wasm;
  readonly accountIdentity: WasmIdentity;
  readonly fromGroupId: Uint8Array;
  readonly toGroupId: Uint8Array;
  readonly handle: string;
}): Promise<void> {
  const { wasm } = args;
  const pair = await namePair(args.handle);
  const oldSeed = groupHandleSeed(wasm, args.accountIdentity, args.fromGroupId);
  const seed = groupHandleSeed(wasm, args.accountIdentity, args.toGroupId);
  try {
    const statement = wasm.signHandleRekey(oldSeed, pair.lookupKey, wasm.handleWritePublicKey(seed));
    const result = await handleWrite("rekey", statement);
    if (result === null) throw new Error("handle rekey answered no certificate");
  } finally {
    oldSeed.fill(0);
    seed.fill(0);
  }
}

/** Retire the name (Delete group, section 11). */
export async function retireGroupHandle(args: {
  readonly wasm: Wasm;
  readonly accountIdentity: WasmIdentity;
  readonly groupId: Uint8Array;
  readonly handle: string;
}): Promise<void> {
  const { wasm, groupId } = args;
  const pair = await namePair(args.handle);
  const seed = groupHandleSeed(wasm, args.accountIdentity, groupId);
  try {
    await handleWrite("retire", wasm.signHandleRetire(seed, pair.lookupKey));
  } finally {
    seed.fill(0);
  }
}

/** A short random number for {@link suggestedHandle}. */
export function handleRandom(): bigint {
  const b = crypto.getRandomValues(new Uint8Array(8));
  let v = 0n;
  for (const x of b) v = (v << 8n) | BigInt(x);
  return v;
}

/** Section 2.1 — the proposed `@name` (`suggestHandle`, one rule). */
export function suggestedHandle(wasm: Wasm, name: string, isPrivate: boolean, random: bigint): string {
  try {
    return wasm.suggestHandle(name, "group", isPrivate, random);
  } catch {
    return "";
  }
}

/** Whether a typed `@name` is well formed (the username rule). */
export function handleWellFormed(wasm: Wasm, typed: string): boolean {
  try {
    wasm.validateUsername(typed.replace(/^@/, ""));
    return true;
  } catch {
    return false;
  }
}


// ---- channels (ADR-0087 sections 1 to 4, 7, 8; phase B)

/** What a channel's public record says. */
export interface PublicChannelFacts {
  readonly name: string;
  readonly description: string;
  /** The channel's photo (section 18.3); absent = none. */
  readonly avatar?: AvatarData | null;
}

/**
 * The handle write seed of a channel this account owns. The caller wipes it.
 *
 * The one rule on every client (coordinator decision, ADR-0087 section 3 and
 * ADR-0074): the PublisherGroup id comes from the LINEAGE (oldest) account
 * identity, `publisherGroupIdFor(lineage, channelId)`, and the handle key
 * from the CURRENT account identity's journal root over that id,
 * `handleWriteSeed(dsJournalRootSeed(current.ed25519Seed, journalId))`. An
 * ADR-0074 rotation moves the name with one `rekey` signed by the previous
 * current identity's key.
 */
export function channelHandleSeed(
  wasm: Wasm,
  lineageIdentity: WasmIdentity,
  currentIdentity: WasmIdentity,
  channelId: Uint8Array,
): Uint8Array {
  const journal = wasm.publisherGroupIdFor(lineageIdentity, channelId);
  return groupHandleSeed(wasm, currentIdentity, journal);
}

/** The subject of a public channel's record. Exported for its test. */
export function channelSubject(
  wasm: Wasm,
  lineageIdentity: WasmIdentity,
  channelId: Uint8Array,
  facts: PublicChannelFacts,
  /** The read key the channel actually uses, when the caller knows it. */
  readKey?: Uint8Array | null,
): RecordSubject {
  return {
    kind: "channel",
    id: channelId,
    // Section 6: the read key a public record carries. The channel's read
    // key never changes, so a channel this account took back after a key
    // reset (ADR-0101 section 7) keeps the key its FIRST owner seed derived:
    // the caller passes the key this device holds for the channel
    // (`channelReadKeyHere`). Without one it is derived from the lineage
    // seed, which is the same value for a channel this identity founded.
    key: readKey ?? wasm.channelReadKey(lineageIdentity, channelId),
    name: facts.name,
    description: facts.description,
    // A channel's band is counted by the channels service (unused here).
    sizeBand: 0,
    avatar: facts.avatar ?? null,
    knockVersion: 0,
  };
}

/** What a channel handle write answers. */
export interface ChannelHandleWrite {
  readonly handle: string;
  readonly isPublic: boolean;
  readonly leaseUntilMs: number;
  /** The handle certificate the server signed, or `null` when it answered
   *  none (a retire, a 204). */
  readonly certificate: Uint8Array | null;
}

/** The arguments every channel handle write takes. */
export interface ChannelHandleArgs {
  readonly wasm: Wasm;
  /** The identity the channel's PublisherGroup id derives from. */
  readonly lineageIdentity: WasmIdentity;
  /** The current account identity: its journal root signs the handle. */
  readonly currentIdentity: WasmIdentity;
  readonly channelId: Uint8Array;
  readonly handle: string;
  /** Section 7: a private channel's name answers no lookup and has no record. */
  readonly isPublic: boolean;
  /** The read key this device holds for the channel
   *  (`channels/channel-access.ts` `channelReadKeyHere`), which a public
   *  record must carry. Absent or `null`: derived from the lineage seed
   *  (wrong for a channel taken back after a key reset, whose read key is
   *  the one its first owner seed derived). */
  readonly readKey?: Uint8Array | null;
  readonly facts: PublicChannelFacts;
}

/** Take `handle` for a channel this account owns (private or public). */
export async function claimChannelHandle(args: ChannelHandleArgs): Promise<ChannelHandleWrite> {
  return channelWrite("claim", args);
}

/** Flip public / private, republish the record, renew the lease. */
export async function updateChannelHandle(args: ChannelHandleArgs): Promise<ChannelHandleWrite> {
  return channelWrite("update", args);
}

/** Move a channel to a new name; the old one is retired (held 30 days). */
export async function replaceChannelHandle(
  args: ChannelHandleArgs & { readonly oldHandle: string },
): Promise<ChannelHandleWrite> {
  const { wasm } = args;
  const old = await namePair(args.oldHandle);
  const next = await namePair(args.handle);
  const seed = channelHandleSeed(wasm, args.lineageIdentity, args.currentIdentity, args.channelId);
  try {
    let record: Uint8Array | null = null;
    let terms: Uint8Array[] = [];
    if (args.isPublic) {
      const subject = channelSubject(wasm, args.lineageIdentity, args.channelId, args.facts, args.readKey);
      ({ record, terms } = await recordContent(wasm, subject, next.normalized, next.output, next.lookupKey));
    }
    const hash = wasm.handleContentHash(record ?? undefined, concat(terms));
    const statement = wasm.signHandleReplace(seed, old.lookupKey, next.lookupKey, args.isPublic, hash);
    const result = await handleWrite("replace", statement, record, terms);
    noteHandleHeld(next.normalized);
    return {
      handle: next.normalized,
      isPublic: args.isPublic,
      leaseUntilMs: result?.leaseUntilMs ?? 0,
      certificate: result?.certificate ?? null,
    };
  } finally {
    seed.fill(0);
  }
}

/** ADR-0074 — after a rotation, move the channel's name from the previous
 *  current identity's handle key to the new one's (`rekey`, signed by the
 *  old). */
export async function rekeyChannelHandle(args: {
  readonly wasm: Wasm;
  readonly lineageIdentity: WasmIdentity;
  readonly oldIdentity: WasmIdentity;
  readonly currentIdentity: WasmIdentity;
  readonly channelId: Uint8Array;
  readonly handle: string;
  readonly isPublic: boolean;
}): Promise<ChannelHandleWrite> {
  const { wasm } = args;
  const pair = await namePair(args.handle);
  const oldSeed = channelHandleSeed(wasm, args.lineageIdentity, args.oldIdentity, args.channelId);
  const seed = channelHandleSeed(wasm, args.lineageIdentity, args.currentIdentity, args.channelId);
  try {
    const statement = wasm.signHandleRekey(oldSeed, pair.lookupKey, wasm.handleWritePublicKey(seed));
    const result = await handleWrite("rekey", statement);
    return {
      handle: pair.normalized,
      isPublic: args.isPublic,
      leaseUntilMs: result?.leaseUntilMs ?? 0,
      certificate: result?.certificate ?? null,
    };
  } finally {
    oldSeed.fill(0);
    seed.fill(0);
  }
}

/**
 * ADR-0101 section 7 item 4 - after a retake, move a channel's name from the
 * handle key its owned-channels list entry carried (`oldSeed`, the previous
 * account's) to the key the new account's root derives. The caller wipes
 * `oldSeed`.
 */
export async function rekeyChannelHandleFromSeed(args: {
  readonly wasm: Wasm;
  readonly oldSeed: Uint8Array;
  readonly lineageIdentity: WasmIdentity;
  readonly currentIdentity: WasmIdentity;
  readonly channelId: Uint8Array;
  readonly handle: string;
  readonly isPublic: boolean;
}): Promise<ChannelHandleWrite> {
  const { wasm } = args;
  const pair = await namePair(args.handle);
  const seed = channelHandleSeed(wasm, args.lineageIdentity, args.currentIdentity, args.channelId);
  try {
    const statement = wasm.signHandleRekey(args.oldSeed, pair.lookupKey, wasm.handleWritePublicKey(seed));
    const result = await handleWrite("rekey", statement);
    return {
      handle: pair.normalized,
      isPublic: args.isPublic,
      leaseUntilMs: result?.leaseUntilMs ?? 0,
      certificate: result?.certificate ?? null,
    };
  } finally {
    seed.fill(0);
  }
}

/** Retire a channel's name. */
export async function retireChannelHandle(args: {
  readonly wasm: Wasm;
  readonly lineageIdentity: WasmIdentity;
  readonly currentIdentity: WasmIdentity;
  readonly channelId: Uint8Array;
  readonly handle: string;
}): Promise<void> {
  const { wasm } = args;
  const pair = await namePair(args.handle);
  const seed = channelHandleSeed(wasm, args.lineageIdentity, args.currentIdentity, args.channelId);
  try {
    await handleWrite("retire", wasm.signHandleRetire(seed, pair.lookupKey));
  } finally {
    seed.fill(0);
  }
}

/**
 * The signed `ConversationHandle` a channel's metadata record v3 carries
 * (section 11): signed by the handle key over the PublisherGroup id, binding
 * the name to the certificate the server answered.
 */
export function signChannelHandle(args: {
  readonly wasm: Wasm;
  readonly lineageIdentity: WasmIdentity;
  readonly currentIdentity: WasmIdentity;
  readonly channelId: Uint8Array;
  readonly handle: string;
  readonly isPublic: boolean;
  readonly certificate: Uint8Array;
  readonly stampMs: bigint;
}): Wasm["ConversationHandle"]["prototype"] {
  const { wasm } = args;
  const seed = channelHandleSeed(wasm, args.lineageIdentity, args.currentIdentity, args.channelId);
  try {
    const journal = wasm.publisherGroupIdFor(args.lineageIdentity, args.channelId);
    return wasm.ConversationHandle.sign(seed, journal, args.stampMs, args.isPublic, args.handle, args.certificate);
  } finally {
    seed.fill(0);
  }
}

async function channelWrite(route: "claim" | "update", args: ChannelHandleArgs): Promise<ChannelHandleWrite> {
  const { wasm } = args;
  const pair = await namePair(args.handle);
  const seed = channelHandleSeed(wasm, args.lineageIdentity, args.currentIdentity, args.channelId);
  try {
    let record: Uint8Array | null = null;
    let terms: Uint8Array[] = [];
    if (args.isPublic) {
      const subject = channelSubject(wasm, args.lineageIdentity, args.channelId, args.facts, args.readKey);
      ({ record, terms } = await recordContent(wasm, subject, pair.normalized, pair.output, pair.lookupKey));
    }
    const hash = wasm.handleContentHash(record ?? undefined, concat(terms));
    const statement =
      route === "claim"
        ? wasm.signHandleClaim(seed, pair.lookupKey, args.isPublic, hash)
        : wasm.signHandleUpdate(seed, pair.lookupKey, args.isPublic, hash);
    const result = await handleWrite(route, statement, record, terms);
    noteHandleHeld(pair.normalized);
    return {
      handle: pair.normalized,
      isPublic: args.isPublic,
      leaseUntilMs: result?.leaseUntilMs ?? 0,
      certificate: result?.certificate ?? null,
    };
  } finally {
    seed.fill(0);
  }
}

/** A channel found by its exact `@name`: its id, or `null`. One OPRF round
 *  and one lookup; the record is opened with the name's own OPRF output. */
export async function lookupChannelByHandle(wasm: Wasm, handle: string): Promise<Uint8Array | null> {
  const pair = await namePair(handle);
  const record = await lookupHandle(pair.lookupKey);
  if (record === null) return null;
  let opened;
  try {
    opened = wasm.openSearchRecord(record, pair.output);
  } catch {
    return null;
  }
  if (opened === undefined) return null;
  try {
    const c = opened.conversation;
    if (c === undefined) return null;
    try {
      if (c.kind !== "channel" || c.handle !== pair.normalized) return null;
      return c.id;
    } finally {
      c.free();
    }
  } finally {
    opened.free();
  }
}

// ---- the lease (ADR-0087 section 3)

/** A handle is held 180 days from its last write (chosen in the ADR). */
export const HANDLE_LEASE_MS = 180 * 24 * 60 * 60 * 1000;
/** An owner device renews a handle with fewer than 90 days left. */
export const HANDLE_RENEW_WHEN_LEFT_MS = 90 * 24 * 60 * 60 * 1000;
/** At most this many renewals per foreground session. */
export const HANDLE_RENEWALS_PER_SESSION = 20;

/** Whether a handle last written at `lastWriteMs` (or answered a lease until
 *  `leaseUntilMs`, 0 when unknown) is due for renewal. Pure. */
export function handleNeedsRenewal(lastWriteMs: number, leaseUntilMs: number, nowMs: number): boolean {
  const until = Math.max(leaseUntilMs, lastWriteMs > 0 ? lastWriteMs + HANDLE_LEASE_MS : 0);
  if (until <= 0) return true;
  return until - nowMs < HANDLE_RENEW_WHEN_LEFT_MS;
}
