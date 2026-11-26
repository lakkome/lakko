// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 - the channel runtime of a key reset: the recovery key
 * every owned channel registers, the returns a peer sends a reset person,
 * and what the reset device does with the returns it read. The rules are in
 * `channel-return.ts`; this module is the I/O around them, driven from the
 * publisher pass (`ChannelPublisherLoops.tsx`).
 */
import { AuthClient, AuthHttpError } from "../auth/client";
import { base64Decode, base64Encode } from "../auth/account-id";
import { sessionTokens } from "../auth/session-token-provider";
import { readResetContinuity } from "../auth/reset-continuity";
import { newestHeldEnvelope } from "../profile/returned-profile";
import { DiscoveryClient } from "../discovery/client";
import { DsClient } from "../ds/client";
import { acceptedResetOfKey, loadSupersededKeys, readPin } from "../contacts/local-store";
import { sealRowFor } from "../contacts/mailbox-row";
import { continuityCandidate, continuityFor, writeThroughCandidates } from "../reach/mailbox-write";
import { refreshContinuityTarget } from "../reach/continuity-target";
import { publicOrigin } from "../platform";
import { loadWasm, type Wasm } from "../wasm";
import type {
  ChannelRecoveryKey as RecoveryKey,
  ChannelReturn as ReturnHandle,
} from "../../wasm/lakko_ffi_wasm";
import { channelsActions, channelsState } from "../state/channels-state";
import { channelRotatesHere, subscribeToChannel } from "./channel-access";
import {
  channelOwnerKeyOf,
  listMemberCards,
  listSubscriberPage,
  putChannelOwnerByRecovery,
  putChannelRecoveryKey,
} from "./api";
import {
  clearRecoveredChannel,
  highestSeq,
  markRecoveredChannel,
  readRecoveredChannels,
  recoveredVisibility,
} from "./recovered-channels";
import { channelIdFromHex, freshSubscriberKeys, type WasmIdentity } from "./composer";
import { mintChannelInviteCard, openChannelInviteCardWith } from "./channel-invites";
import { acceptPublisherInvite, mintPublisherInviteLink, removePublisher } from "./channel-publishers";
import { keepReturnedInvitation } from "./returned-invitations";
import { channelSubscriptionPort } from "./channel-subscription-register";
import { markFoundedAlone } from "./founded-alone-marker";
import {
  channelReturnAction,
  channelReturnsFor,
  dropHeldReturns,
  newestSeeds,
  readHeldReturns,
  sentReturnKey,
  unsentReturns,
  type ChannelReturnRoleName,
  type PlannedReturn,
  type ReturnCard,
  type ReturnChannel,
  type SentReturns,
} from "./channel-return";

// ----------------------------------------------------------------------------
// Wake-up of the runner
// ----------------------------------------------------------------------------

let runner: (() => void) | null = null;
let pendingWake = false;

/** The mailbox kept a channel return: run the reset side now. */
export function notifyChannelReturnsHeld(): void {
  if (runner === null) {
    pendingWake = true;
    return;
  }
  runner();
}

/** Install (or remove) what runs on a wake. */
export function installChannelReturnRunner(fn: (() => void) | null): void {
  runner = fn;
  if (fn !== null && pendingWake) {
    pendingWake = false;
    fn();
  }
}

function storage(): Storage | undefined {
  try {
    return globalThis.localStorage ?? undefined;
  } catch {
    return undefined;
  }
}

// ----------------------------------------------------------------------------
// The channel recovery key
// ----------------------------------------------------------------------------

/** Thrown when auth has no recovery master configured. */
export class ChannelRecoveryDisabledError extends Error {
  override name = "ChannelRecoveryDisabledError";
}

/**
 * The channel's recovery key: the channel recovery input of `idHex`
 * blinded here, evaluated by auth under this account's per-account key
 * (auth never learns the channel id), finalised here. Throws
 * {@link ChannelRecoveryDisabledError} on `503`, the auth error otherwise.
 */
export async function deriveChannelRecoveryKey(idHex: string): Promise<RecoveryKey> {
  const wasm = await loadWasm();
  const token = sessionTokens.peek();
  if (token === null) throw new Error("no session for the channel recovery key");
  const blinder = wasm.OprfBlinder.blindChannelRecovery(channelIdFromHex(idHex));
  let evaluation: Uint8Array;
  try {
    evaluation = await new AuthClient().channelRecoveryEvaluate(token, base64Decode(blinder.blindedB64));
  } catch (err) {
    blinder.free();
    if (err instanceof AuthHttpError && err.status === 503) throw new ChannelRecoveryDisabledError();
    throw err;
  }
  const result = blinder.finalize(base64Encode(evaluation));
  try {
    const output = result.output;
    try {
      return wasm.ChannelRecoveryKey.fromOprfOutput(output);
    } finally {
      output.fill(0);
    }
  } finally {
    result.free();
  }
}

function registeredKey(accountIdB64: string): string {
  return `lakko:channel-recovery-registered:${accountIdB64}`;
}

/** The owned channels whose recovery key landed, per account. */
export function readRecoveryRegistered(s: Storage | undefined, accountIdB64: string): Set<string> {
  try {
    const raw = s?.getItem(registeredKey(accountIdB64));
    const parsed = raw === null || raw === undefined ? [] : (JSON.parse(raw) as unknown);
    return new Set(Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string") : []);
  } catch {
    return new Set();
  }
}

function markRecoveryRegistered(s: Storage | undefined, accountIdB64: string, idHex: string): void {
  const held = readRecoveryRegistered(s, accountIdB64);
  held.add(idHex);
  try {
    s?.setItem(registeredKey(accountIdB64), JSON.stringify([...held].sort()));
  } catch {
    /* registered again next pass: the PUT is idempotent */
  }
}

/** Wipe. */
export function clearRecoveryRegistered(s: Storage | undefined, accountIdB64: string): void {
  try {
    s?.removeItem(registeredKey(accountIdB64));
  } catch {
    /* nothing recorded either */
  }
}

/** The owned channels still owing a registration. Pure. */
export function channelsOwingRecoveryKey(owned: readonly string[], registered: ReadonlySet<string>): string[] {
  return owned.filter((id) => !registered.has(id)).sort();
}

/**
 * ADR-0101 section 7 - register the recovery key of every channel this
 * device owns that has none recorded (at creation the first pass after it
 * does it, so creation and existing channels take one path). A `503` (no
 * recovery configured on this server) stops the pass quietly; any other
 * failure leaves the channel owing. Never throws.
 */
export async function registerOwedRecoveryKeys(accountIdB64: string, owner: WasmIdentity): Promise<void> {
  const owned = [...channelsState.keysByChannel().keys()].filter((id) => channelRotatesHere(id));
  const owing = channelsOwingRecoveryKey(owned, readRecoveryRegistered(storage(), accountIdB64));
  for (const idHex of owing) {
    let key: RecoveryKey;
    try {
      key = await deriveChannelRecoveryKey(idHex);
    } catch (err) {
      if (err instanceof ChannelRecoveryDisabledError) return;
      continue;
    }
    try {
      const status = await putChannelRecoveryKey(idHex, owner, key.verifyingKey());
      if (status === 204) markRecoveryRegistered(storage(), accountIdB64, idHex);
    } finally {
      key.free();
    }
  }
}

// ----------------------------------------------------------------------------
// The peer side: returns to send
// ----------------------------------------------------------------------------

/** One card this device opened, with what a return carries of it. */
interface OpenedReturnCard extends ReturnCard {
  readonly bindingSig: Uint8Array;
  readonly profileHash: Uint8Array;
  readonly subscriberPk: Uint8Array;
}

/** How many card pages a channel is read for returns (chosen). */
const RETURN_CARD_PAGES = 10;

async function openCards(wasm: Wasm, idHex: string): Promise<OpenedReturnCard[]> {
  const out: OpenedReturnCard[] = [];
  const channelId = channelIdFromHex(idHex);
  let after: string | null = null;
  for (let page = 0; page < RETURN_CARD_PAGES; page++) {
    const res = await listMemberCards(idHex, after);
    for (const m of res.members) {
      try {
        const seed = channelsActions.currentSeed(idHex, wasm.memberCardEpoch(m.card));
        if (seed === undefined) continue;
        const opened = wasm.openMemberCard(seed, m.card, channelId, m.subscriberPk);
        try {
          out.push({
            identityPkB64: base64Encode(opened.identityPk),
            subscriberPkB64: base64Encode(m.subscriberPk),
            subscriberPk: m.subscriberPk,
            bindingSig: opened.bindingSig,
            profileHash: opened.profileHash,
          });
        } finally {
          opened.free();
        }
      } catch {
        /* a card this device cannot open names nobody */
      }
    }
    after = res.nextAfter;
    if (after === null) break;
  }
  return out;
}

function sentKey(accountIdB64: string): string {
  return `lakko:channel-returns-sent:${accountIdB64}`;
}

function readSent(s: Storage | undefined, accountIdB64: string): SentReturns {
  try {
    const raw = s?.getItem(sentKey(accountIdB64));
    const parsed = raw === null || raw === undefined ? {} : (JSON.parse(raw) as Record<string, unknown>);
    const out: Record<string, number> = {};
    for (const [k, v] of Object.entries(parsed)) if (typeof v === "number") out[k] = v;
    return out;
  } catch {
    return {};
  }
}

function writeSent(s: Storage | undefined, accountIdB64: string, sent: SentReturns): void {
  try {
    s?.setItem(sentKey(accountIdB64), JSON.stringify(sent));
  } catch {
    /* sent again next pass; the receiver handles a return twice the same */
  }
}

/** Wipe. */
export function clearSentReturns(s: Storage | undefined, accountIdB64: string): void {
  try {
    s?.removeItem(sentKey(accountIdB64));
  } catch {
    /* nothing recorded either */
  }
}

/** The accepted reset's generation for an account, while its continuity key
 *  is still usable; `null` otherwise. */
function liveGeneration(accountIdB64: string): number | null {
  const pin = readPin(accountIdB64);
  const key = pin?.pinnedSignaturePubkeyB64;
  if (key === undefined) return null;
  const reset = acceptedResetOfKey(key);
  if (reset === null || continuityFor(base64Decode(key), Date.now()) === null) return null;
  return reset.keyGeneration;
}

/**
 * ADR-0101 section 7, peer side - one pass: for every channel this device
 * publishes or reads, and every reset it accepted whose window is open,
 * send the returns {@link channelReturnsFor} plans and were not sent for
 * that reset yet. Cards are read only when there is a live reset at all.
 * Never throws.
 */
export async function sendChannelReturns(
  accountIdB64: string,
  identity: WasmIdentity,
  ownKeysB64: ReadonlySet<string>,
): Promise<void> {
  const superseded = loadSupersededKeys();
  const live = new Map<string, string>();
  for (const [key, acct] of superseded) if (liveGeneration(acct) !== null) live.set(key, acct);
  if (live.size === 0) return;
  const wasm = await loadWasm();
  const sent = { ...readSent(storage(), accountIdB64) };
  const discovery = new DiscoveryClient();
  const ds = new DsClient();
  for (const ch of channelsState.channels()) {
    const idHex = ch.id;
    const access = channelsActions.accessOf(idHex);
    const publishing = channelsState.keysByChannel().has(idHex);
    const ownRole: ReturnChannel["ownRole"] = publishing ? (channelRotatesHere(idHex) ? "owner" : "publisher") : "subscriber";
    const reg = channelSubscriptionPort()?.get(idHex);
    const ownerKeyB64 =
      reg?.subscribed === true && reg.ownerAccountKey.length === 32 ? base64Encode(reg.ownerAccountKey) : null;
    // A subscriber only ever sends an owner return: skip the card read when
    // the owner is not a reset this device accepted.
    if (ownRole === "subscriber" && (ownerKeyB64 === null || !live.has(ownerKeyB64))) continue;
    let cards: OpenedReturnCard[];
    try {
      cards = await openCards(wasm, idHex);
    } catch {
      continue;
    }
    const planned = unsentReturns(
      channelReturnsFor(
        [{ idHex, ownRole, cards, ownerKeyB64, publisherKeysB64: access.publishers }],
        live,
        ownKeysB64,
      ),
      sent,
      liveGeneration,
    );
    for (const r of planned) {
      const card = cards.find((c) => c.identityPkB64 === r.card?.identityPkB64);
      if (card === undefined) continue;
      const ok = await sendOneReturn(wasm, discovery, ds, identity, r, card).catch(() => false);
      const gen = liveGeneration(r.accountIdB64);
      if (ok && gen !== null) {
        sent[sentReturnKey(r)] = gen;
        writeSent(storage(), accountIdB64, sent);
      }
    }
  }
}

/** The raw profile envelope this device holds under one key, with its
 *  stamp (`ProfileApi.heldEnvelopeOf`); installed by the publisher loops. */
type HeldProfileSource = (keyB64: string) => { readonly envelope: Uint8Array; readonly updatedAtMs: bigint } | null;
let heldProfiles: HeldProfileSource | null = null;

/** Install (or remove) where a return's profile is read from. */
export function installHeldProfileSource(fn: HeldProfileSource | null): void {
  heldProfiles = fn;
}

/**
 * ADR-0101 section 8 - the profile a return carries: the newest RAW envelope
 * this device holds under any key the reset account had (its retired keys),
 * verbatim, or `undefined`. The reset device keeps it only when it verifies
 * under its previous key. Pure over the source.
 */
export function returnProfileFor(
  accountIdB64: string,
  superseded: ReadonlyMap<string, string>,
  source: HeldProfileSource | null,
): Uint8Array | undefined {
  if (source === null) return undefined;
  const keys = [...superseded].filter(([, acct]) => acct === accountIdB64).map(([k]) => k);
  return newestHeldEnvelope(keys, source) ?? undefined;
}

async function sendOneReturn(
  wasm: Wasm,
  discovery: DiscoveryClient,
  ds: DsClient,
  identity: WasmIdentity,
  r: PlannedReturn,
  card: OpenedReturnCard,
): Promise<boolean> {
  const newKeyB64 = readPin(r.accountIdB64)?.pinnedSignaturePubkeyB64;
  if (newKeyB64 === undefined) return false;
  const newKey = base64Decode(newKeyB64);
  const continuity = continuityFor(newKey, Date.now());
  if (continuity === null) return false;
  if ((await refreshContinuityTarget(discovery, r.accountIdB64, newKeyB64)) === null) return false;
  const candidate = continuityCandidate(newKey, continuity);
  if (candidate === null) return false;
  let invitation: Uint8Array = new Uint8Array(0);
  try {
    if (r.role === "subscriber") invitation = (await mintChannelInviteCard(identity, r.idHex)).card;
    // A one-use publisher invitation, recorded so the request it brings is
    // approved without asking (the owner's "Add publisher").
    else if (r.role === "publisher") invitation = (await mintPublisherInviteLink(identity, r.idHex)).card;
  } catch {
    return false;
  }
  const held = new Map<number, Uint8Array>(channelsState.subscribedSeedsByChannel().get(r.idHex) ?? []);
  const keys = channelsState.keysByChannel().get(r.idHex);
  if (keys?.seedEpoch !== undefined && !held.has(keys.seedEpoch)) held.set(keys.seedEpoch, keys.seed);
  const seeds = newestSeeds([...held].map(([epoch, seed]) => ({ epoch, seed })));
  const envelope = wasm.composeChannelReturnEnvelope(
    channelIdFromHex(r.idHex),
    roleOf(wasm, r.role),
    card.bindingSig,
    card.subscriberPk,
    card.profileHash,
    invitation,
    Uint32Array.from(seeds.epochs),
    seeds.seeds,
    returnProfileFor(r.accountIdB64, loadSupersededKeys(), heldProfiles),
  );
  await writeThroughCandidates([candidate], async (target, auth) =>
    ds.publishMailbox(target.address, sealRowFor(wasm, target, "channel-return", envelope), auth),
  );
  if (r.role === "publisher") {
    // The retired identity's leaves leave the PublisherGroup (a commit and a
    // rotation): its devices are gone, and its key must not publish again.
    // A failure is retried by nobody here; the owner can remove it by hand.
    await removePublisher(identity, r.idHex, card.identityPkB64).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("[channels] a reset publisher's old leaves were not removed", err instanceof Error ? err.name : "");
    });
  }
  return true;
}

function roleOf(wasm: Wasm, role: ChannelReturnRoleName): number {
  switch (role) {
    case "subscriber":
      return wasm.ChannelReturnRole.Subscriber;
    case "publisher":
      return wasm.ChannelReturnRole.Publisher;
    case "owner":
      return wasm.ChannelReturnRole.Owner;
  }
}

function roleName(wasm: Wasm, role: number): ChannelReturnRoleName {
  if (role === wasm.ChannelReturnRole.Owner) return "owner";
  if (role === wasm.ChannelReturnRole.Publisher) return "publisher";
  return "subscriber";
}

// ----------------------------------------------------------------------------
// The reset side: returns read from the continuity mailbox
// ----------------------------------------------------------------------------

/** How long a publisher Accept is watched before the return is counted
 *  handled (its request is recorded by then; chosen). */
const ACCEPT_SETTLE_MS = 20_000;

/** What the reset side needs from its caller. */
export interface ResetSidePorts {
  readonly accountIdB64: string;
  /** The current account identity (owns a recovered channel). */
  readonly identity: WasmIdentity;
  /** Offer a returned profile envelope (verified by the caller against the
   *  previous key). */
  readonly offerProfile?: (envelope: Uint8Array) => void;
}

/**
 * ADR-0101 section 7, reset side - act on every held return. A return that
 * needs the network and could not reach it (`retry`) is kept; every other is
 * dropped once handled. Never throws.
 */
export async function runHeldChannelReturns(ports: ResetSidePorts): Promise<void> {
  const held = readHeldReturns(storage(), ports.accountIdB64);
  if (held.length === 0) return;
  const wasm = await loadWasm();
  const previousKeyB64 = readResetContinuity(storage(), ports.accountIdB64)?.previousKeyB64 ?? null;
  const done: string[] = [];
  for (const envB64 of held) {
    let env: Uint8Array;
    try {
      env = base64Decode(envB64);
    } catch {
      done.push(envB64);
      continue;
    }
    let decoded: ReturnHandle;
    let verified = false;
    try {
      if (previousKeyB64 !== null) {
        try {
          decoded = wasm.verifyChannelReturnCardBinding(env, base64Decode(previousKeyB64));
          verified = true;
        } catch {
          decoded = wasm.decodeChannelReturnEnvelope(env);
        }
      } else {
        decoded = wasm.decodeChannelReturnEnvelope(env);
      }
    } catch {
      done.push(envB64);
      continue;
    }
    try {
      const outcome = await actOnReturn(wasm, ports, decoded, verified);
      if (outcome !== "retry") done.push(envB64);
    } catch {
      /* kept: retried at the next wake */
    } finally {
      decoded.free();
    }
  }
  dropHeldReturns(storage(), ports.accountIdB64, done);
}

async function actOnReturn(
  wasm: Wasm,
  ports: ResetSidePorts,
  ret: ReturnHandle,
  verified: boolean,
): Promise<"done" | "retry"> {
  const idHex = [...ret.channelId].map((b) => b.toString(16).padStart(2, "0")).join("");
  const invitation = ret.invitation;
  const reg = channelSubscriptionPort()?.get(idHex);
  const action = channelReturnAction({
    verified,
    role: roleName(wasm, ret.role),
    hasInvitation: invitation.length > 0,
    alreadyInRole:
      roleName(wasm, ret.role) === "publisher"
        ? channelsState.keysByChannel().has(idHex)
        : channelsState.channels().some((c) => c.id === idHex),
    ownsAlready: reg?.subscribed === true && reg.role === "owner",
  });
  const epochs = ret.epochs;
  const seeds = ret.seeds;
  const addSeeds = (): void => {
    epochs.forEach((e: number, i: number) => {
      if (channelsActions.currentSeed(idHex, e) === undefined) {
        channelsActions.setSubscribedSeedForEpoch(idHex, e, seeds.slice(i * 32, i * 32 + 32));
      }
    });
  };
  if (verified) {
    const profile = ret.profile;
    if (profile !== undefined) ports.offerProfile?.(profile);
  }
  switch (action) {
    case "drop":
      return "done";
    case "show-invitation":
      // Shown as an ordinary invitation card (`returned-invitations.ts`);
      // a storage refusal keeps the return held.
      keepReturnedInvitation(ports.accountIdB64, idHex, base64Encode(invitation));
      return "done";
    case "add-seeds":
      addSeeds();
      return "done";
    case "accept-publisher": {
      const opened = openChannelInviteCardWith(wasm, invitation, publicOrigin());
      if (opened === null || !opened.publisherInvite || opened.channelIdHex !== idHex) return "done";
      addSeeds();
      // The ordinary Accept: the request is recorded and knocked, the
      // owner's device approves it (the link was minted for this person),
      // and the Welcome lands through the mailbox like any approved request.
      // Its own wait is not waited for here; a failure inside the first
      // seconds (offline) keeps the return for the next pass.
      const accepting = acceptPublisherInvite(ports.identity, opened, ports.accountIdB64);
      const settled = await Promise.race([
        accepting.then(
          () => "done" as const,
          () => "retry" as const,
        ),
        new Promise<"done">((resolve) => setTimeout(() => resolve("done"), ACCEPT_SETTLE_MS)),
      ]);
      void accepting.catch(() => undefined);
      return settled;
    }
    case "subscribe": {
      const opened = openChannelInviteCardWith(wasm, invitation, publicOrigin());
      if (opened === null || opened.publisherInvite || opened.channelIdHex !== idHex) return "done";
      try {
        await subscribeToChannel(idHex, opened.access, {
          name: opened.name,
          description: opened.description,
          avatar: opened.avatar,
        });
      } catch {
        return "retry";
      }
      addSeeds();
      return "done";
    }
    case "recover-owner": {
      const outcome = await recoverOwnedChannel(ports, idHex);
      if (outcome === "owned") addSeeds();
      return outcome === "retry" ? "retry" : "done";
    }
  }
}

/**
 * ADR-0101 section 7, owner - take a channel back with its recovery key:
 * the key is evaluated again for this account, the owner key moved to the
 * current identity's with the recovery proof, and on `204` the channel goes
 * into the list and the register as owned, so the publisher pass founds its
 * PublisherGroup again at the id the new seed derives (ADR-0090 decision A)
 * and reseals. `401` (not this account's, or no recovery key registered):
 * dropped silently. Never throws.
 */
export async function recoverOwnedChannel(
  ports: Pick<ResetSidePorts, "accountIdB64" | "identity">,
  idHex: string,
): Promise<"owned" | "refused" | "retry"> {
  let key: RecoveryKey;
  try {
    key = await deriveChannelRecoveryKey(idHex);
  } catch (err) {
    return err instanceof ChannelRecoveryDisabledError ? "refused" : "retry";
  }
  let status: number | null;
  try {
    const newOwnerPk = await channelOwnerKeyOf(ports.identity, idHex);
    const header = key.signOwnerChange(channelIdFromHex(idHex), newOwnerPk, BigInt(Date.now()));
    status = await putChannelOwnerByRecovery(idHex, header, ports.identity);
  } finally {
    key.free();
  }
  if (status === null || status >= 500) return "retry";
  if (status !== 204) return "refused";
  if (!channelsState.channels().some((c) => c.id === idHex)) {
    channelsActions.addChannel({ id: idHex, localName: "", localDescription: "", addedAtMs: Date.now() });
  }
  // Every reader the service lists now stays let in: the re-founding's
  // reseal reaches each of them (`recovered-channels.ts`). A fresh admission
  // seed verifies the tickets of invitations made from now on.
  let throughSeq = 0;
  try {
    for (let after = 0; ; ) {
      const page = await listSubscriberPage(idHex, ports.identity, after);
      throughSeq = highestSeq(page.entries, throughSeq);
      if (page.entries.length === 0 || page.nextAfter <= after) break;
      after = page.nextAfter;
    }
  } catch {
    // The owner key is ours now; the listing is tried again by the next
    // return of this channel (the mark only ever rises).
  }
  markRecoveredChannel(ports.accountIdB64, idHex, throughSeq);
  const wasm = await loadWasm();
  await channelsActions.updateAccessDurable(idHex, (r) => ({
    ...r,
    admittedThroughSeq: Math.max(r.admittedThroughSeq, throughSeq),
    admissionMarkSet: true as const,
    admissionSeeds:
      r.admissionSeeds.length > 0
        ? r.admissionSeeds
        : [{ version: 1, seedB64: base64Encode(wasm.generateChannelAdmissionSeed()) }],
  }));
  const port = channelSubscriptionPort();
  if (port !== null) {
    const fresh = await freshSubscriberKeys();
    await port
      .set(idHex, {
        role: "owner",
        subscriberSecret: fresh.secret,
        readKey: new Uint8Array(0),
        publisherGroupIdHex: "",
        ownerAccountKey: new Uint8Array(0),
        name: "",
        handle: "",
      })
      .catch(() => false);
  }
  // This device holds the account's DeviceGroup alone after the reset: the
  // pass may found the channel's group again here.
  markFoundedAlone(ports.accountIdB64);
  return "owned";
}

/**
 * ADR-0101 section 7 - a recovered channel takes its type from its newest
 * metadata record once one is held (a public channel's read key is the one
 * the current account seed derives; its old readers keep their per-key
 * envelopes), and the recovery mark goes. Never throws.
 */
export async function settleRecoveredChannels(accountIdB64: string, lineage: WasmIdentity): Promise<void> {
  const recovered = readRecoveredChannels(accountIdB64);
  const ids = Object.keys(recovered);
  if (ids.length === 0) return;
  const wasm = await loadWasm();
  for (const idHex of ids) {
    try {
      const access = channelsActions.accessOf(idHex);
      if (access.visibility !== null) {
        clearRecoveredChannel(accountIdB64, idHex);
        continue;
      }
      const visibility = recoveredVisibility(channelsState.metaByChannel().get(idHex)?.listed);
      if (visibility === null) continue;
      const readKeyB64 =
        visibility === "public" ? base64Encode(wasm.channelReadKey(lineage, channelIdFromHex(idHex))) : null;
      await channelsActions.updateAccessDurable(idHex, (r) => ({ ...r, visibility, readKeyB64: readKeyB64 ?? r.readKeyB64 }));
      clearRecoveredChannel(accountIdB64, idHex);
    } catch {
      /* settled at a later pass */
    }
  }
}
