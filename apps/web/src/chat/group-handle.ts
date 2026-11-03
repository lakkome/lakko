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
import { handleWrite, lookupHandle, oprfForName, type HandleWriteResult } from "../discovery/handle-lookup";
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
  for (const term of nameTerms) {
    const pair = await scoped.nameTermOprf(term);
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
  const pair = await oprfForName(args.handle);
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
  const pair = await oprfForName(args.handle);
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
  const old = await oprfForName(args.oldHandle);
  const next = await oprfForName(args.newHandle);
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
  const pair = await oprfForName(args.handle);
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

/** Retire the name (Delete group, section 11). */
export async function retireGroupHandle(args: {
  readonly wasm: Wasm;
  readonly accountIdentity: WasmIdentity;
  readonly groupId: Uint8Array;
  readonly handle: string;
}): Promise<void> {
  const { wasm, groupId } = args;
  const pair = await oprfForName(args.handle);
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


// ---- channels (ADR-0087 sections 1 to 4; the rest of phase B is not built)

/** What a channel's public record says. */
export interface PublicChannelFacts {
  readonly name: string;
  readonly description: string;
}

/** The handle write seed of a channel this account owns: from the journal
 *  root of its PublisherGroup. The caller wipes it. */
export function channelHandleSeed(wasm: Wasm, ownerIdentity: WasmIdentity, channelId: Uint8Array): Uint8Array {
  const journal = wasm.publisherGroupIdFor(ownerIdentity, channelId);
  return groupHandleSeed(wasm, ownerIdentity, journal);
}

function channelSubject(wasm: Wasm, ownerIdentity: WasmIdentity, channelId: Uint8Array, facts: PublicChannelFacts): RecordSubject {
  return {
    kind: "channel",
    id: channelId,
    key: wasm.channelReadKey(ownerIdentity, channelId),
    name: facts.name,
    description: facts.description,
    // A channel's band is counted by the channels service (unused here).
    sizeBand: 0,
    avatar: null,
    knockVersion: 0,
  };
}

/** What a channel handle write answers. */
export interface ChannelHandleWrite {
  readonly handle: string;
  readonly leaseUntilMs: number;
}

/** Take `handle` for a channel this account owns, public: its record is what
 *  a search by `@name` and by name finds. */
export async function claimChannelHandle(args: {
  readonly wasm: Wasm;
  readonly ownerIdentity: WasmIdentity;
  readonly channelId: Uint8Array;
  readonly handle: string;
  readonly facts: PublicChannelFacts;
}): Promise<ChannelHandleWrite> {
  return channelWrite("claim", args);
}

/** Republish a channel's record and renew its lease. */
export async function updateChannelHandle(args: {
  readonly wasm: Wasm;
  readonly ownerIdentity: WasmIdentity;
  readonly channelId: Uint8Array;
  readonly handle: string;
  readonly facts: PublicChannelFacts;
}): Promise<ChannelHandleWrite> {
  return channelWrite("update", args);
}

async function channelWrite(
  route: "claim" | "update",
  args: {
    readonly wasm: Wasm;
    readonly ownerIdentity: WasmIdentity;
    readonly channelId: Uint8Array;
    readonly handle: string;
    readonly facts: PublicChannelFacts;
  },
): Promise<ChannelHandleWrite> {
  const { wasm } = args;
  const pair = await oprfForName(args.handle);
  const seed = channelHandleSeed(wasm, args.ownerIdentity, args.channelId);
  try {
    const subject = channelSubject(wasm, args.ownerIdentity, args.channelId, args.facts);
    const { record, terms } = await recordContent(wasm, subject, pair.normalized, pair.output, pair.lookupKey);
    const hash = wasm.handleContentHash(record, concat(terms));
    const statement =
      route === "claim"
        ? wasm.signHandleClaim(seed, pair.lookupKey, true, hash)
        : wasm.signHandleUpdate(seed, pair.lookupKey, true, hash);
    const result = await handleWrite(route, statement, record, terms);
    return { handle: pair.normalized, leaseUntilMs: result?.leaseUntilMs ?? 0 };
  } finally {
    seed.fill(0);
  }
}

/** A channel found by its exact `@name`: its id, or `null`. One OPRF round
 *  and one lookup; the record is opened with the name's own OPRF output. */
export async function lookupChannelByHandle(wasm: Wasm, handle: string): Promise<Uint8Array | null> {
  const pair = await oprfForName(handle);
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
