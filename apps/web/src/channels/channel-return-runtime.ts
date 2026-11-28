// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 - the channel runtime of a key reset for SUBSCRIBERS
 * and PUBLISHERS: the returns a peer sends a reset person, and what the
 * reset device does with the returns it read. The rules are in
 * `channel-return.ts`; this module is the I/O around them, driven from the
 * publisher pass (`ChannelPublisherLoops.tsx`).
 *
 * An OWNER comes back by itself from its owned-channels list (`owned/`,
 * rewritten 2026-11-26): no owner return is sent or acted on here, and the
 * recovery key is derived from the list key there. What stays here of the
 * owner's side is the last step of a retaken channel
 * ({@link settleRecoveredChannels}).
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import { readResetContinuity } from "../auth/reset-continuity";
import { newestHeldEnvelope } from "../profile/returned-profile";
import { DiscoveryClient } from "../discovery/client";
import { DsClient } from "../ds/client";
import { acceptedResetOfKey, loadMovableSupersededKeys, readPin } from "../contacts/local-store";
import { sealRowFor } from "../contacts/mailbox-row";
import { continuityCandidate, continuityFor, writeThroughCandidates } from "../reach/mailbox-write";
import { refreshContinuityTarget } from "../reach/continuity-target";
import { publicOrigin } from "../platform";
import { loadWasm, type Wasm } from "../wasm";
import type { ChannelReturn as ReturnHandle } from "../../wasm/lakko_ffi_wasm";
import { channelsActions, channelsState } from "../state/channels-state";
import { channelRotatesHere, subscribeToChannel } from "./channel-access";
import { listMemberCards } from "./api";
import { clearRecoveredChannel, readRecoveredChannels, recoveredVisibility } from "./recovered-channels";
import { channelIdFromHex, type WasmIdentity } from "./composer";
import { mintChannelInviteCard, openChannelInviteCardWith } from "./channel-invites";
import { acceptPublisherInvite, mintPublisherInviteLink, removePublisher } from "./channel-publishers";
import { keepReturnedInvitation } from "./returned-invitations";
import { channelSubscriptionPort } from "./channel-subscription-register";
import { clearRecoveryRegistered as clearRegisteredRecoveryKeys } from "./owned/owned-facts";
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
// The channel recovery key (moved to `owned/`)
// ----------------------------------------------------------------------------

/** Wipe the record of which owned channels registered their recovery key
 *  (kept in `owned/owned-facts.ts`; the rows of the removed OPRF-derived key
 *  go too). The account wipe calls it under this name. */
export function clearRecoveryRegistered(s: Storage | undefined, accountIdB64: string): void {
  clearRegisteredRecoveryKeys(s, accountIdB64);
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
  return liveGenerationAt(accountIdB64, Date.now());
}

function liveGenerationAt(accountIdB64: string, nowMs: number): number | null {
  const pin = readPin(accountIdB64);
  const key = pin?.pinnedSignaturePubkeyB64;
  // ADR-0101 section 11: the account started over since; nothing returns.
  if (key === undefined || pin?.startedOver !== undefined) return null;
  const reset = acceptedResetOfKey(key);
  if (reset === null || continuityFor(base64Decode(key), nowMs) === null) return null;
  return reset.keyGeneration;
}

/**
 * ADR-0101 sections 7 and 11 (walk of 2026-11-28): the retired keys a
 * return may be sent for right now, each mapped to its account. Only a
 * SEALED reset this device accepted puts a key here, and only while its
 * continuity key is usable; an unsealed reset (a started-over record, even
 * after an earlier sealed one), a refused one and one with no verdict yet
 * put nothing (`loadMovableSupersededKeys`, `acceptedResetOfKey`). Fail
 * closed: a key is here only on positive evidence. Reads the pin store.
 */
export function liveReturnTargets(nowMs: number = Date.now()): Map<string, string> {
  const live = new Map<string, string>();
  for (const [key, acct] of loadMovableSupersededKeys()) {
    if (liveGenerationAt(acct, nowMs) !== null) live.set(key, acct);
  }
  return live;
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
  const live = liveReturnTargets();
  if (live.size === 0) return;
  const wasm = await loadWasm();
  const sent = { ...readSent(storage(), accountIdB64) };
  const discovery = new DiscoveryClient();
  const ds = new DsClient();
  for (const ch of channelsState.channels()) {
    const idHex = ch.id;
    const access = channelsActions.accessOf(idHex);
    const publishing = channelsState.keysByChannel().has(idHex);
    // A subscriber's device sends no return (an owner takes its channels
    // back from its own list): no card is read for it.
    if (!publishing) continue;
    const ownRole: ReturnChannel["ownRole"] = channelRotatesHere(idHex) ? "owner" : "publisher";
    const reg = channelSubscriptionPort()?.get(idHex);
    const ownerKeyB64 =
      reg?.subscribed === true && reg.ownerAccountKey.length === 32 ? base64Encode(reg.ownerAccountKey) : null;
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
    returnProfileFor(r.accountIdB64, loadMovableSupersededKeys(), heldProfiles),
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
  }
}

/** The role of a decoded return (the core's decoder refuses every role but
 *  these two). */
function roleName(wasm: Wasm, role: number): ChannelReturnRoleName {
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
  /** The current account identity (a publisher request is made with it). */
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
  const action = channelReturnAction({
    verified,
    role: roleName(wasm, ret.role),
    hasInvitation: invitation.length > 0,
    alreadyInRole:
      roleName(wasm, ret.role) === "publisher"
        ? channelsState.keysByChannel().has(idHex)
        : channelsState.channels().some((c) => c.id === idHex),
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
  }
}

/**
 * ADR-0101 section 7 item 8 - the last step of a channel the account took
 * back from its owned-channels list (`owned/owned-runtime.ts` marks it
 * recovered): once its PublisherGroup is founded again here, the recovery
 * mark goes. The mark stays until then because the re-founding reads it
 * (which rows stay let in, `recovered-channels.ts`). A retaken channel whose
 * list entry said neither public nor private takes its type from its newest
 * metadata record once one is held (then a public channel's read key is the
 * one the current account seed derives). Never throws.
 */
export async function settleRecoveredChannels(accountIdB64: string, lineage: WasmIdentity): Promise<void> {
  const recovered = readRecoveredChannels(accountIdB64);
  const ids = Object.keys(recovered);
  if (ids.length === 0) return;
  const wasm = await loadWasm();
  for (const idHex of ids) {
    try {
      // Not founded again yet: the re-founding still needs the mark.
      if (!channelsState.keysByChannel().has(idHex)) continue;
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
