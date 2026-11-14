// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 section 15 — a channel's publishers on web (UX 15.2).
 *
 * `Add publisher`: the owner mints a one-use co-publisher `/g/` link (kind
 * 2) and sends its URL in the one-to-one; the person's `Accept` knocks with a
 * sealed request carrying that link's capability; the owner's device
 * approves it without asking (it named that link), in `ChannelPublisherLoops`.
 *
 * Phase B (core gap fixes): the list is the PublisherGroup's own member list
 * (`members()`, refreshed on every publisher pass), the owner's
 * {@link removePublisher} removes every leaf of the person's account in one
 * commit and reseals, and a publisher's {@link stepDownAsPublisher} sends the
 * step-down message the owner's device applies the same way.
 */

import { createSignal } from "solid-js";

import { loadWasm } from "../wasm";
import { base64Encode } from "../auth/account-id";
import { DiscoveryClient } from "../discovery/client";
import { sessionTokens } from "../auth/session-token-provider";
import { publicOrigin } from "../platform";
import { DsClient } from "../ds/client";
import { channelsActions, channelsState } from "../state/channels-state";
import { revokeCoPublisher } from "./api";
import { b64ToBytes, bytesToB64 } from "../profile/avatar-model";
import {
  boundCoPublisherKey,
  mintOneUseCoPublisherLink,
  joinCoPublishWelcome,
  pollForCoPublishWelcome,
  requestToCoPublishSealed,
} from "./copublisher";
import { sharedPublisherGroup, withPublisherGroup } from "./publisher-group-registry";
import { publisherDeviceOrNull } from "./publisher-device";
import { ChannelError } from "./channel-access";
import type { OpenedChannelInvite } from "./channel-invites";
import { channelIdFromHex, type WasmIdentity } from "./composer";

import { recordGroupLink } from "../devices/sync/link-register";
import { persistThenPublish } from "./publisher-app-messages";
import { coPublishWaitMs } from "./co-publish-welcome-scan";
import {
  dropPendingPublisherRequest,
  loadPendingPublisherRequests,
  recordPendingPublisherRequest,
} from "./pending-publisher-requests";

type PublisherGroup = InstanceType<Awaited<ReturnType<typeof loadWasm>>["PublisherGroup"]>;

export interface ChannelPublisher {
  readonly accountKeyB64: string;
  readonly isOwner: boolean;
}

/** This account's own key, installed by `ChannelPublisherLoops` (the
 *  lineage identity the PublisherGroups live in). */
const [selfKey, setSelfKey] = createSignal<string | null>(null);

/** `ChannelPublisherLoops` installs this account's key (or `null`). */
export function installSelfPublisherKey(keyB64: string | null): void {
  setSelfKey(keyB64);
}

/** One publisher as the PublisherGroup lists it (ADR-0090: one row per
 *  DEVICE leaf; `isOwner` from the owner's account key the group records). */
interface GroupPublisher {
  readonly accountKeyB64: string;
  readonly leafIndex: number;
  readonly isOwner?: boolean;
}

/** The PublisherGroup's members per channel, as last read here. */
const [groupMembers, setGroupMembers] = createSignal<ReadonlyMap<string, readonly GroupPublisher[]>>(new Map());

/** Runtime shape of a `PublisherGroup.members()` element. */
interface WasmGroupMember {
  readonly leafIndex: number;
  readonly accountKey: Uint8Array;
  free?: () => void;
}

/** Read the publishers out of a PublisherGroup (call under its lock). The
 *  owner founded the group, so it holds the lowest leaf, which no removal
 *  touches (the core refuses to remove the owner). */
export function notePublisherGroupMembers(idHex: string, group: PublisherGroup): void {
  let list: GroupPublisher[];
  try {
    const raw = group.members() as WasmGroupMember[];
    const ownerB64 = base64Encode(group.ownerAccountKey());
    list = raw.map((m) => {
      const key = base64Encode(m.accountKey);
      const out = { accountKeyB64: key, leafIndex: m.leafIndex, isOwner: key === ownerB64 };
      m.free?.();
      return out;
    });
  } catch {
    return;
  }
  setGroupMembers((prev) => {
    const next = new Map(prev);
    next.set(idHex, list);
    return next;
  });
}

/** Drop a channel's member list (the channel ended here). */
export function forgetPublisherGroupMembers(idHex: string): void {
  setGroupMembers((prev) => {
    if (!prev.has(idHex)) return prev;
    const next = new Map(prev);
    next.delete(idHex);
    return next;
  });
}

/** Pure: the publishers of a PublisherGroup member list — the owner (the
 *  lowest leaf) first, then each other account once. */
export function publishersFromMembers(members: readonly GroupPublisher[]): ChannelPublisher[] {
  if (members.length === 0) return [];
  const ownerLeaf = Math.min(...members.map((m) => m.leafIndex));
  // ADR-0090: the group records the owner's account; the lowest leaf only
  // for a list read without it.
  const owner = members.find((m) => m.isOwner === true) ?? members.find((m) => m.leafIndex === ownerLeaf);
  const out: ChannelPublisher[] = [];
  if (owner !== undefined) out.push({ accountKeyB64: owner.accountKeyB64, isOwner: true });
  for (const m of [...members].sort((a, b) => a.leafIndex - b.leafIndex)) {
    if (!out.some((x) => x.accountKeyB64 === m.accountKeyB64)) out.push({ accountKeyB64: m.accountKeyB64, isOwner: false });
  }
  return out;
}

/** Reactive: the channel's publishers — from the PublisherGroup's member
 *  list once it was read, before that the publishers this device knows (the
 *  owner, the publishers approved here, this account when it publishes). */
export function channelPublishers(idHex: string): readonly ChannelPublisher[] {
  const fromGroup = groupMembers().get(idHex);
  if (fromGroup !== undefined && fromGroup.length > 0) return publishersFromMembers(fromGroup);
  const own = selfKey();
  const keys = channelsState.keysByChannel().has(idHex);
  const owner = keys && channelsActions.coPublisherGroupIdOf(idHex) === undefined;
  const a = channelsState.accessByChannel().get(idHex);
  const out: ChannelPublisher[] = [];
  if (owner && own !== null) out.push({ accountKeyB64: own, isOwner: true });
  for (const p of a?.publishers ?? []) {
    if (!out.some((x) => x.accountKeyB64 === p)) out.push({ accountKeyB64: p, isOwner: false });
  }
  if (keys && !owner && own !== null && !out.some((x) => x.accountKeyB64 === own)) {
    out.push({ accountKeyB64: own, isOwner: false });
  }
  return out;
}

/** Read the PublisherGroup's member list now (the Publishers screen on
 *  open). A device that holds no PublisherGroup for the channel reads none. */
export async function refreshChannelPublishers(identity: WasmIdentity, idHex: string): Promise<void> {
  if (!channelsState.keysByChannel().has(idHex)) return;
  await withPublisherGroup(identity, idHex, async (group) => {
    notePublisherGroupMembers(idHex, group);
  });
}

/** Co-publisher requests per channel that came by no live one-use link of
 *  this account (joiner account keys, standard base64): the owner decides
 *  them by hand. Set by the owner's publisher pass. */
const [pendingRequests, setPendingRequests] = createSignal<ReadonlyMap<string, readonly string[]>>(new Map());

/** Reactive: the requests waiting for the owner's Approve. */
export function pendingPublisherRequests(idHex: string): readonly string[] {
  return pendingRequests().get(idHex) ?? [];
}

/** The owner's pass: the requests that matched no link. */
export function notePendingPublisherRequests(idHex: string, keys: readonly string[]): void {
  setPendingRequests((prev) => {
    const have = prev.get(idHex) ?? [];
    if (have.length === keys.length && have.every((k, i) => k === keys[i])) return prev;
    const next = new Map(prev);
    if (keys.length === 0) next.delete(idHex);
    else next.set(idHex, [...keys]);
    return next;
  });
}

type PublisherRequestHandler = (identity: WasmIdentity, idHex: string, joinerKeyB64: string, approve: boolean) => Promise<void>;
let requestHandler: PublisherRequestHandler | null = null;

/** `ChannelPublisherLoops` installs the Approve / Reject of a request. */
export function installPublisherRequestHandler(fn: PublisherRequestHandler | null): void {
  requestHandler = fn;
}

/** Owner: approve (`true`) or reject a request that came by no link.
 *  ChannelError "failed" when nothing can run it. */
export async function decidePublisherRequest(
  identity: WasmIdentity,
  idHex: string,
  joinerKeyB64: string,
  approve: boolean,
): Promise<void> {
  if (requestHandler === null) throw new ChannelError("failed");
  try {
    await requestHandler(identity, idHex, joinerKeyB64, approve);
  } catch (err) {
    throw err instanceof ChannelError ? err : new ChannelError("offline", { cause: err });
  }
}

/** The owner's removal of one account from the PublisherGroup (commit,
 *  journal, reseal), installed by `ChannelPublisherLoops`, which holds the
 *  rotation state. */
type PublisherRemover = (identity: WasmIdentity, idHex: string, accountKey: Uint8Array) => Promise<void>;
let remover: PublisherRemover | null = null;

/** `ChannelPublisherLoops` installs the removal (or `null`). */
export function installPublisherRemover(fn: PublisherRemover | null): void {
  remover = fn;
}

/**
 * Owner: a one-use co-publisher link for one person (UX 15.2 `Add
 * publisher`), as the invitation the screen sends in the one-to-one: the
 * card every client draws ("{name} invites you to be a publisher of
 * {channel}." with `Accept`) and the URL as the message text. Recorded so
 * the request it brings is approved without asking.
 */
export async function mintPublisherInviteLink(
  identity: WasmIdentity,
  idHex: string,
): Promise<{ readonly kind: "card"; readonly url: string; readonly card: Uint8Array }> {
  if (!channelsState.keysByChannel().has(idHex) || channelsActions.coPublisherGroupIdOf(idHex) !== undefined) {
    throw new ChannelError("not-publisher");
  }
  const group = await sharedPublisherGroup(identity, idHex);
  const discovery = new DiscoveryClient(undefined, { sessionToken: () => sessionTokens.peek() });
  let minted;
  try {
    minted = await mintOneUseCoPublisherLink(identity, group, publicOrigin(), discovery);
  } catch (err) {
    throw new ChannelError("offline", { cause: err });
  }
  const ledgerIdB64Url = bytesToB64(minted.ledgerId).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  await channelsActions.updateAccessDurable(idHex, (r) => ({
    ...r,
    inviteLinks: [
      ...r.inviteLinks,
      {
        ledgerIdB64Url,
        url: minted.url,
        label: "",
        expiryMs: 0,
        maxUses: 0,
        inviteSeedB64: "",
        invitePkHex: "",
        revocationSeedB64: bytesToB64(minted.revocationSeed),
        kind: "publisher",
        revoked: false,
        mintedAtMs: Date.now(),
        capabilityB64: base64Encode(minted.capabilityWire),
      },
    ],
  }));
  // ADR-0087 phase B — the account's other devices list and revoke it too.
  await recordGroupLink({
    kind: "publisher",
    linkSecret: minted.linkSecret,
    ledgerId: minted.ledgerId,
    revocationSeed: minted.revocationSeed,
    expiryMs: 0,
    maxUses: 0,
    mintedAtMs: Date.now(),
    label: "",
    targetId: channelIdFromHex(idHex),
    capability: minted.sealed,
  });
  minted.revocationSeed.fill(0);
  minted.linkSecret.fill(0);
  return { kind: "card", url: minted.url, card: minted.card };
}

/** How long the Accept page itself watches for the owner's device (chosen):
 *  three minutes, the passes 3 s apart at first and backing off to 15 s
 *  (`coPublishWaitMs`). The REQUEST has no deadline (ADR-0045 decision 2):
 *  it is recorded (`pending-publisher-requests.ts`) and the mailbox receive
 *  loop joins its Welcome whenever the owner's device approves. */
const ACCEPT_WAIT_MS = 180_000;

/** What `Accept` came to: this device can post, or the request is recorded
 *  and waits for the owner's device (it lands by itself). */
export type AcceptOutcome = "joined" | "waiting";

/**
 * The person presses `Accept` on a publisher invitation (`publisherInvite`
 * of {@link OpenedChannelInvite}). Knocks with a sealed request, recorded
 * for the account first, then watches for the owner's device to approve and
 * the Welcome to arrive. Resolves `"joined"` when this device can post,
 * `"waiting"` when the owner's device has not answered in the page's wait
 * (the request stays; the mailbox receive loop lands it). ChannelError
 * "link-invalid", "offline", "failed" when the request could not be sent.
 *
 * Before the knock, durably: the PublisherGroup state holding the request's
 * private key (`channelsActions.persistPublisherGroupDurable`) and the
 * pending-request record.
 */
export async function acceptPublisherInvite(
  identity: WasmIdentity,
  opened: OpenedChannelInvite,
  accountIdB64: string,
): Promise<AcceptOutcome> {
  if (!opened.publisherInvite || opened.link === undefined) throw new ChannelError("link-invalid");
  const ds = new DsClient();
  const discovery = new DiscoveryClient(undefined, { sessionToken: () => sessionTokens.peek() });
  let req;
  try {
    req = await requestToCoPublishSealed(
      ds,
      identity,
      opened.link,
      async (r) => {
        await channelsActions.persistPublisherGroupDurable();
        recordPendingPublisherRequest(accountIdB64, {
          channelIdHex: r.channelIdHex,
          groupIdHex: bytesToHex(r.groupId),
          joinerKeyB64: base64Encode(r.joinerKey),
          ownerKeyB64: base64Encode(r.ownerKey),
          name: opened.name,
          description: opened.description,
          requestedAtMs: Date.now(),
        });
      },
      discovery,
    );
  } catch (err) {
    throw new ChannelError(err instanceof ChannelError ? err.code : "offline", { cause: err });
  }
  const deadline = Date.now() + ACCEPT_WAIT_MS;
  for (let pass = 0; Date.now() < deadline; pass += 1) {
    // Landed by the mailbox receive loop meanwhile.
    if (!isPending(accountIdB64, req.channelIdHex) && channelsState.keysByChannel().has(req.channelIdHex)) {
      return "joined";
    }
    const keys = await pollForCoPublishWelcome(
      ds,
      identity,
      req.groupId,
      req.joinerKey,
      req.channelIdHex,
      req.ownerKey,
      channelsState.keysByChannel().get(req.channelIdHex)?.signingKeySeed,
    ).catch(() => null);
    if (keys !== null) {
      await settleLandedRequest(accountIdB64, req.channelIdHex, opened.name, opened.description, keys);
      return "joined";
    }
    await new Promise((r) => setTimeout(r, coPublishWaitMs(pass)));
  }
  return isPending(accountIdB64, req.channelIdHex) ? "waiting" : "joined";
}

function isPending(accountIdB64: string, channelIdHex: string): boolean {
  return loadPendingPublisherRequests(accountIdB64).some((r) => r.channelIdHex === channelIdHex);
}

/** A request landed: the channel and its keys, durably, then the record goes. */
async function settleLandedRequest(
  accountIdB64: string,
  channelIdHex: string,
  name: string,
  description: string,
  keys: Parameters<typeof channelsActions.setKeys>[1],
): Promise<void> {
  if (!channelsState.channels().some((c) => c.id === channelIdHex)) {
    channelsActions.addChannel({
      id: channelIdHex,
      localName: name,
      localDescription: description,
      addedAtMs: Date.now(),
    });
  }
  channelsActions.setKeys(channelIdHex, keys);
  // The joined group and the keys on disk before the record goes (and before
  // the mailbox cursor moves past the Welcome).
  await channelsActions.persistPublisherGroupDurable();
  dropPendingPublisherRequest(accountIdB64, channelIdHex);
}

/**
 * The mailbox receive loop's half of a publisher request (ADR-0045 decision
 * 2: no deadline): try one Welcome from the account mailbox against every
 * pending request. `welcome` is the Welcome itself (a `welcome` row's body,
 * or the inner Welcome of a `welcome-rendezvous` row opened for this
 * account's key). Returns whether it landed a request. A Welcome that is no
 * request's changes nothing. Throws when a landed join could not be made
 * durable (the caller holds its mailbox cursor).
 */
export async function landPendingPublisherWelcome(
  identity: WasmIdentity,
  accountIdB64: string,
  welcome: Uint8Array,
): Promise<boolean> {
  const pending = loadPendingPublisherRequests(accountIdB64);
  if (pending.length === 0) return false;
  const device = publisherDeviceOrNull();
  if (device === null) return false;
  const ownKey = base64Encode(device.signaturePublicKey);
  for (const r of pending) {
    // The Welcome is sealed to the knocked KeyPackage, whose private key is
    // in this DEVICE's provider only when the request was made from it
    // (ADR-0090: the leaf is the device). An older record names no owner
    // key, so no Welcome lands it.
    if (r.joinerKeyB64 !== ownKey || r.ownerKeyB64 === undefined) continue;
    const groupId = hexBytes(r.groupIdHex);
    if (groupId === null) continue;
    const keys = await joinCoPublishWelcome(
      identity,
      welcome,
      groupId,
      r.channelIdHex,
      b64ToBytes(r.ownerKeyB64),
      channelsState.keysByChannel().get(r.channelIdHex)?.signingKeySeed,
    );
    if (keys === null) continue;
    await settleLandedRequest(accountIdB64, r.channelIdHex, r.name, r.description, keys);
    return true;
  }
  return false;
}

function hexBytes(hex: string): Uint8Array | null {
  if (hex.length === 0 || hex.length % 2 !== 0 || !/^[0-9a-fA-F]+$/.test(hex)) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function bytesToHex(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += b.toString(16).padStart(2, "0");
  return s;
}

/**
 * Owner: `Remove publisher` (UX 15.2). Removes every leaf of the person's
 * account from the PublisherGroup in one commit and reseals the new seed to
 * the subscribers, so the person cannot read past it; then
 * {@link afterPublisherRemoved}. ChannelError "not-publisher" on a device
 * that is not the owner's, "offline" when the service or the journal did not
 * answer.
 */
export async function removePublisher(identity: WasmIdentity, idHex: string, accountKeyB64: string): Promise<void> {
  if (channelsActions.coPublisherGroupIdOf(idHex) !== undefined) throw new ChannelError("not-publisher");
  if (remover === null) throw new ChannelError("failed");
  try {
    await remover(identity, idHex, b64ToBytes(accountKeyB64));
  } catch (err) {
    throw err instanceof ChannelError ? err : new ChannelError("offline", { cause: err });
  }
  await afterPublisherRemoved(identity, idHex, accountKeyB64);
}

/**
 * Owner, after a publisher's account left the PublisherGroup (a removal or
 * a step-down): revoke the person's appointment on the channels service, drop
 * them from the access record, and start a new admission-seed version for a
 * private channel (new links carry it, the remaining publishers get it on the
 * next pass).
 *
 * The appointed key is not derivable, so it is looked up: the key this
 * device recorded (it appointed them, or filed the signed binding they send
 * into the PublisherGroup every epoch), else a binding read from the knock
 * queue beside their request; with neither, the revoke is logged and skipped
 * (the removal already took the seed).
 */
export async function afterPublisherRemoved(identity: WasmIdentity, idHex: string, accountKeyB64: string): Promise<void> {
  const a = channelsActions.accessOf(idHex);
  let appointed: Uint8Array | undefined;
  const recorded = a.publisherKeys.find((p) => p.accountKeyB64 === accountKeyB64)?.copublisherKeyB64 ?? "";
  if (recorded.length > 0) appointed = b64ToBytes(recorded);
  if (appointed === undefined) {
    try {
      const group = await sharedPublisherGroup(identity, idHex);
      appointed = boundCoPublisherKey(group.groupId(), b64ToBytes(accountKeyB64));
    } catch {
      appointed = undefined;
    }
  }
  if (appointed === undefined) {
    console.warn("[channels] removed publisher's appointed key unknown here; appointment not revoked");
  } else {
    await revokeCoPublisher(idHex, identity, appointed).catch((err: unknown) => {
      console.warn("[channels] co-publisher appointment revoke failed", err instanceof Error ? err.name : "");
    });
  }
  const wasm = await loadWasm();
  const next = Math.max(0, ...a.admissionSeeds.map((s) => s.version)) + 1;
  await channelsActions.updateAccessDurable(idHex, (r) => ({
    ...r,
    publishers: r.publishers.filter((p) => p !== accountKeyB64),
    publisherKeys: r.publisherKeys.filter((p) => p.accountKeyB64 !== accountKeyB64),
    admissionSeeds:
      r.visibility === "private"
        ? [...r.admissionSeeds, { version: next, seedB64: bytesToB64(wasm.generateChannelAdmissionSeed()) }]
        : r.admissionSeeds,
  }));
}

/**
 * A publisher's `Step down` (UX 15.2): sends the step-down message on the
 * PublisherGroup journal. The owner's device removes this account when it
 * next runs; until then nothing changes here, and once the removal commit
 * arrives this device's publisher pass ends the channel here.
 * ChannelError "not-publisher" for the owner (who cannot step down),
 * "offline" when the journal did not take it.
 */
export async function stepDownAsPublisher(identity: WasmIdentity, idHex: string): Promise<void> {
  if (channelsActions.coPublisherGroupIdOf(idHex) === undefined) throw new ChannelError("not-publisher");
  const ds = new DsClient();
  await withPublisherGroup(identity, idHex, async (group, device) => {
    if (group.isOwnerDevice(device)) throw new ChannelError("not-publisher");
    const message = group.stepDown(device);
    // The sending ratchet moved: record it before the message leaves (a
    // crash in between would reuse the generation, G-34 class). A refused
    // write publishes nothing.
    await persistThenPublish(
      () => channelsActions.persistPublisherGroupDurable(),
      async () => {
        try {
          await ds.publishGroup(group.groupId(), message);
        } catch (err) {
          throw new ChannelError("offline", { cause: err });
        }
      },
    );
  });
}
