// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0028 OQ3 — channel co-publisher management (owner + joiner side, web).
 *
 * A channel's PublisherGroup ([ADR-0010](../../../docs/adr/0010-channels-two-tier-broadcast.md)
 * Tier 1) is a vanilla MLS group, so adding a *co-publisher* reuses the group
 * join-ticket + external-join machinery (ADR-0027/0028) verbatim — exactly what
 * the OQ3 FFI surface on `PublisherGroup` exposes.
 *
 * This module is the owner-side glue:
 *
 *   - `openPublisherGroup` — open/create the persistent PublisherGroup handle.
 *   - `mintOneUseCoPublisherLink` — mint a one-use `/g/<slug>` invite over
 *     the PublisherGroup's MLS `group_id` for one person (UX 15.2: the
 *     shareable multi-use co-publisher link is gone).
 *   - `pollCoPublisherJournal` — poll the PublisherGroup's DS journal and apply
 *     incoming MLS messages (external-join proposals + co-publisher commits) via
 *     the role-gated path, surfacing pending requests.
 *   - `pendingCoPublishers` — the retained external-join proposals.
 *   - `approveCoPublisher` / `rejectCoPublisher` — the role-gated decision.
 *
 * **ADR-0053.** The PublisherGroup journal is claimed like every group journal:
 * `registerPublisherGroupAccess` gives the DS client the group's access key for
 * as long as a panel holds the handle, the approve registers the next epoch's
 * key before its commit is published, a joiner asks through the knock (it
 * cannot read the journal) and the owner reads the knock queue, and the
 * co-publisher's Welcome goes to the joiner's own mailbox (its ACCOUNT mailbox:
 * the PublisherGroup lives in the account identity's provider), never to the
 * journal.
 *
 * **Reseal-after-add (load-bearing).** `approveExternalJoin` commits the Add,
 * which advances the PublisherGroup MLS epoch — so the channel content-key seed
 * (the epoch-keyed exporter output) CHANGES. Every existing subscriber holds the
 * OLD seed, so the owner MUST reseal the new seed to all subscribers (the
 * ADR-0010 / C-54 rotation fanout) or those subscribers can no longer decrypt.
 * `approveCoPublisher` does this through the same reseal as `rotation-cron`
 * (`resealToSubscribers`: the audited `sealSubscriberSeedEnvelope` and the
 * ADR-0030 staged fanout), but WITHOUT a second `rotate()` (the Add already
 * advanced the epoch; the reseal binds to the post-approve epoch the
 * co-publisher joins at).
 *
 * **G-50.** The approve eager-merges the Add; its effects (the N+1 epoch key,
 * the commit on the journal, the Welcome, the reseal, the appointment) are
 * recorded durably WITH the N+1 provider snapshot before any of them runs
 * (`publisher-commit-outbox.ts`), and resumed from that record after a reload
 * (`replayPublisherCommits`).
 *
 * **ADR-0078 section 6.** The PublisherGroup id is derived from the owner's
 * seed, so a co-publisher cannot compute it and nothing inverts it: the `/g/`
 * link carries the channel id in its sealed preview, and the co-publisher
 * keeps the group id the capability conveyed (`setCoPublisherGroupId`) and
 * opens the group it joined under it.
 *
 * **Co-publisher keys (ADR-0059 section 1, 2026-09-29).** A co-publisher
 * cannot derive the owner key, so its rotation fanout reads the subscriber
 * list with a key of its own the owner appoints on `services/channels`. The
 * joiner sends that key with its knock as a binding signed by its account
 * identity key; the owner reads it off the knock queue (`pollCoPublisherJournal`)
 * and appoints it after approving (`approveCoPublisher`) only when the
 * binding's account key IS the joiner key it approved, so neither the DS nor
 * another knocker can substitute a key. Only the owner can appoint: when a
 * co-publisher approves another, the approve and reseal still run (under the
 * approver's own appointed key) but the newcomer is not appointed, and its
 * rotation fanout is refused until the owner appoints it (not built: this
 * client has no path that notices a co-publisher it did not approve).
 *
 * The end-to-end correctness of the reseal under a real 2-publisher topology
 * (the co-publisher derives the same post-approve seed the subscribers were
 * resealed to) needs a live 2-publisher e2e against a deployed channels backend
 * — deferred here (no deployed environment), the same posture as every other
 * live-channel gate in the codebase.
 */

import { INITIAL_KNOCK_VERSION } from "../ds/knock-seeds";
import type { Wasm } from "../wasm";
import { loadWasm } from "../wasm";
import { DsClient } from "../ds/client";
import { scanJournal } from "../chat/journal-head-epoch";
import { createWelcomeScanState } from "./co-publish-welcome-scan";
import {
  dsAccess,
  knockToJoin,
} from "../ds/access";
import { openOwnRow, sealRowFor } from "../contacts/mailbox-row";
import { takeKnockReadSlot } from "../chat/knock-cursor";
import { base64Encode } from "../auth/account-id";
import type { DiscoveryClient } from "../discovery/client";
import { newRevocationKey } from "../contacts/own-links";
import { withGroupMlsLock } from "../mls/group-lock";
import { failedApplyKind, failedRowLine } from "../chat/registered-commit-hold";
import { appointCoPublisher, rememberPublisherRole } from "./api";
import { channelsActions, channelsState } from "../state/channels-state";
import { commitInFlight, publisherCommitPorts, runStagedPublisherCommit } from "./rotation-cron";
import {
  remainingSteps,
  replayVerdict,
  runPublisherCommit,
  withStepDone,
  type PublisherCommitEntry,
  type PublisherCommitPorts,
} from "./publisher-commit-outbox";
import { admitExtras, learnEpochOffset } from "./publisher-devices";
import {
  bytesToHex,
  freshPublisherKeys,
  openHeldPublisherGroup,
  PublisherGroupNotHeldError,
  type ChannelPublisherKeys,
} from "./composer";
import { publisherDevice } from "./publisher-device";
import { joinBelowDeletedGroup, welcomeAlreadyJoined, welcomeHashHex, withJoinedWelcome } from "./welcome-replay";
import { LegacyChannelError, isLegacyV1PublisherGroupId } from "./legacy-v1";
import {
  InviteLinkDamagedError,
  composeSealedInviteLink,
  inviteLinkWidths,
  openSealedInvite,
} from "../contacts/invite-link";
import { parseGroupInviteSlug, splitGroupInviteLink } from "../contacts/group-invite";
import { reachSession } from "../reach/reach-service";
import { writeToPerson } from "../reach/mailbox-write";
import {
  forgetJoinReply,
  joinRepliesOpened,
  joinReplyBindingVerifies,
  joinReplyFor,
  rememberJoinReply,
} from "../reach/join-replies";
import { journalHex } from "../ds/access";
import { KeyedError } from "../l10n/keyed-error";

type Identity = InstanceType<Wasm["Identity"]>;
type PublisherGroup = InstanceType<Wasm["PublisherGroup"]>;

/** Path prefix every group/co-publisher invite URL starts with. */
const GROUP_INVITE_URL_PREFIX = "/g/";

/** Open the persistent PublisherGroup handle this device holds for a channel
 *  (never founds one: see `openHeldPublisherGroup`). The
 *  caller owns the returned handle and MUST `.free()` it when done. */
export async function openPublisherGroup(
  identity: Identity,
  channelIdHex: string,
): Promise<PublisherGroup> {
  const wasm = await loadWasm();
  const channelId = hexToBytes(channelIdHex);
  if (channelId === null) {
    throw new Error(`channel id hex is not valid: ${channelIdHex}`);
  }
  // ADR-0078 section 6: a channel this account co-publishes is opened under
  // the group id its link conveyed; it is never founded here (that would fork
  // the channel onto a group nobody else is in).
  const joinedIdHex = channelsActions.coPublisherGroupIdOf(channelIdHex);
  if (joinedIdHex !== undefined) {
    const joinedId = hexToBytes(joinedIdHex);
    const joined =
      joinedId === null ? undefined : wasm.PublisherGroup.loadJoined(publisherDevice(), joinedId, channelId);
    if (joined === undefined) throw new PublisherGroupNotHeldError();
    return joined;
  }
  // 2026-11-13, the trap rule: this is every path but New channel, so the
  // group is loaded, never founded. A device of the owner that holds no group
  // state is a reader of the channel (PublisherGroupNotHeldError); founding
  // here would claim-reset the live journal. A channel minted under the
  // retired v1 derivation refuses inside with the readable copy.
  return await openHeldPublisherGroup(wasm, identity, channelId);
}

/**
 * ADR-0053 contract points 1 + 2 — let the DS client prove requests to this
 * PublisherGroup's journal under the group's current access key, for as long
 * as the caller holds the handle (a refused request makes the DS client
 * claim the journal with the channel owner's root, ADR-0057). Returns the function that
 * withdraws it; call that BEFORE freeing the handle.
 */
export function registerPublisherGroupAccess(
  device: Identity,
  publisherGroup: PublisherGroup,
): () => void {
  const own = publisherGroup.groupId();
  const matches = (groupId: Uint8Array): boolean => {
    if (groupId.length !== own.length) return false;
    for (let i = 0; i < own.length; i++) if (groupId[i] !== own[i]) return false;
    return true;
  };
  const unregisterSource = dsAccess.registerGroupSource((groupId) =>
    matches(groupId) ? publisherGroup.dsAccessSigner(device) : null,
  );
  // ADR-0057 — every device of the channel owner holds the PublisherGroup
  // journal's root (ADR-0090: an owner DEVICE commits; a co-publisher's
  // device never does, and holds no root).
  const ownerDevice = ((): boolean => {
    try {
      return publisherGroup.isOwnerDevice(device);
    } catch {
      return false;
    }
  })();
  const unregisterOwner = dsAccess.registerOwnerSource((groupId) =>
    matches(groupId) ? ownerDevice : null,
  );
  // ADR-0057 obligation 2 - the owner claims the journal on open (once per
  // session): an unclaimed journal never answers the 401 the lazy claim
  // waits for.
  if (ownerDevice) void new DsClient().ensureOwnerClaim(own);
  return () => {
    unregisterSource();
    unregisterOwner();
  };
}

/** What a minted co-publisher link is, for the minting device's record. */
export interface MintedCoPublisherLink {
  readonly url: string;
  readonly ledgerId: Uint8Array;
  readonly revocationSeed: Uint8Array;
  /** The plaintext capability wire: a sealed knock carrying it came by this
   *  link (ADR-0087 section 15, "the owner's device approves that person's
   *  request without asking, since it named them"). */
  readonly capabilityWire: Uint8Array;
  /** The sealed `/g/` blob the ledger holds (for the link register). */
  readonly sealed: Uint8Array;
  /** The 12-byte link secret (the URL fragment). SECRET. */
  readonly linkSecret: Uint8Array;
  /** The invitation card the one-to-one message carries beside the URL
   *  (ADR-0087 section 14, UX 15.2): the ledger id, the secret and the sealed
   *  blob, so every client draws "... invites you to be a publisher of
   *  {channel}." with `Accept` from the message alone. SECRET (it holds the
   *  link secret); it travels only inside the end-to-end encrypted message. */
  readonly card: Uint8Array;
}

/**
 * The channel name a co-publisher link carries in its preview: the name the
 * channel's own record gives, else the name this device keeps for it, else
 * its @name. `undefined` when it has none of them (the link then carries no
 * name and the receiving client says "Channel"); never the generic word.
 */
export function coPublisherLinkChannelName(
  sharedName: string | undefined,
  localName: string | undefined,
  localHandle: string | undefined,
): string | undefined {
  for (const candidate of [sharedName, localName]) {
    const name = (candidate ?? "").trim();
    if (name.length > 0) return name;
  }
  const handle = (localHandle ?? "").trim().replace(/^@/, "");
  return handle.length > 0 ? handle : undefined;
}

/**
 * ADR-0087 section 15 — `Add publisher`: a co-publisher `/g/` link (kind 2)
 * for one person, one use, uploaded to the group-invite ledger. Its preview
 * carries the channel id, the name and the knock key the joiner seals its
 * request to. The caller records it (the capability wire is what its request
 * is recognised by) and sends the URL in the one-to-one.
 */
export async function mintOneUseCoPublisherLink(
  identity: Identity,
  publisherGroup: PublisherGroup,
  origin: string,
  discovery: Pick<DiscoveryClient, "uploadGroupInvite">,
): Promise<MintedCoPublisherLink> {
  const wasm = await loadWasm();
  const groupId = publisherGroup.groupId();
  const channelId = publisherGroup.channelId();
  const channelIdHex = bytesToHex(channelId);
  const handle = channelsState.channels().find((c) => c.id === channelIdHex);
  const name = coPublisherLinkChannelName(
    channelsState.metaByChannel().get(channelIdHex)?.name,
    handle?.localName,
    handle?.localHandle,
  );
  const inviterName = await ownInviterName(wasm);
  const root = wasm.dsJournalRootSeed(identity.ed25519Seed, groupId);
  const knockSeed = wasm.groupKnockSeed(root, INITIAL_KNOCK_VERSION);
  root.fill(0);
  const mint = wasm.composeCoPublisherInviteCapability(
    identity.ed25519Seed,
    groupId,
    0n,
    channelId,
    name,
    wasm.groupKnockPublicKey(knockSeed),
    INITIAL_KNOCK_VERSION,
    undefined,
    inviterName,
  );
  knockSeed.fill(0);
  const sealed = mint.sealed;
  const linkSecret = mint.linkSecret;
  mint.free();
  const cap = wasm.GroupInviteCapability.openLink(sealed, linkSecret);
  let capabilityWire: Uint8Array;
  try {
    capabilityWire = cap.wire;
  } finally {
    cap.free();
  }
  const revocation = await newRevocationKey();
  // One use: the person named is the only one meant to ask.
  const ledgerId = await discovery.uploadGroupInvite({
    sealed,
    revocationPubkey: revocation.publicKey,
    expiryMs: 0n,
    maxUses: 0,
  });
  const url = composeSealedInviteLink({
    origin,
    prefix: GROUP_INVITE_URL_PREFIX,
    blobId: ledgerId,
    secret: linkSecret,
    widths: inviteLinkWidths(wasm),
  });
  const card = wasm.composeChannelInviteCard(ledgerId, linkSecret, sealed);
  return { url, ledgerId, revocationSeed: revocation.seed, capabilityWire, sealed, linkSecret, card };
}

/** Capability wires read off sealed knock rows, keyed like the bindings
 *  (`groupIdB64:joinerKeyHex`), base64. In memory: the knock queue is re-read
 *  from the start every poll. */
const joinerCapabilities = new Map<string, string>();

/** The 16-byte account id a pending joiner's sealed request carried in its
 *  profile (its claim), keyed like the bindings. The owner fetches the
 *  joiner's identity bundle by it and seals the admission seed to the bundle's
 *  X25519 key only when the bundle's signature key IS the joiner's key. */
const joinerAccountIds = new Map<string, Uint8Array>();

/** ADR-0090: the account key a pending joiner's KeyPackage is bound to,
 *  keyed like the bindings (`groupIdB64:deviceKeyHex`). */
const joinerAccounts = new Map<string, Uint8Array>();

/** The account key a pending joiner's device leaf is bound to (ADR-0050
 *  binding of its KeyPackage), or `undefined` before its request was read. */
export function accountKeyOfJoiner(groupId: Uint8Array, joinerKey: Uint8Array): Uint8Array | undefined {
  return joinerAccounts.get(bindingKey(base64Encode(groupId), joinerKey));
}

/** The account id a pending joiner's request named, or `undefined`. */
export function accountIdOfJoiner(groupId: Uint8Array, joinerKey: Uint8Array): Uint8Array | undefined {
  return joinerAccountIds.get(bindingKey(base64Encode(groupId), joinerKey));
}

/** The own profile envelope a sealed request carries (ADR-0087 section 5:
 *  the request carries the person's name and account id), installed by the
 *  session (`ChannelPublisherLoops`). */
let ownProfileEnvelopeSource: (() => Promise<Uint8Array | null>) | null = null;

/** Install where the own profile envelope comes from (or `null`). */
export function installOwnProfileEnvelopeSource(fn: (() => Promise<Uint8Array | null>) | null): void {
  ownProfileEnvelopeSource = fn;
}

/** ADR-0087 phase B — this account's display name for a co-publisher link's
 *  preview (`inviterName`), read off the own profile envelope; `undefined`
 *  when there is none or it does not decode. Never throws. */
async function ownInviterName(wasm: Wasm): Promise<string | undefined> {
  try {
    const envelope = await ownProfileEnvelopeSource?.();
    if (envelope === null || envelope === undefined) return undefined;
    const profile = wasm.decodeProfileEnvelope(envelope);
    try {
      const name = (profile.displayName ?? "").trim();
      return name.length > 0 ? name : undefined;
    } finally {
      profile.free();
    }
  } catch {
    return undefined;
  }
}

/** The capability wire (standard base64) a pending joiner's sealed request
 *  carried, or `undefined`. */
export function capabilityOfJoiner(groupId: Uint8Array, joinerKey: Uint8Array): string | undefined {
  return joinerCapabilities.get(bindingKey(base64Encode(groupId), joinerKey));
}

/** The co-publisher key a joiner bound beside its request (ADR-0059), or
 *  `undefined` before its binding row was read. */
export function boundCoPublisherKey(groupId: Uint8Array, joinerKey: Uint8Array): Uint8Array | undefined {
  return copublisherBindings.get(bindingKey(base64Encode(groupId), joinerKey));
}

/** Co-publisher keys read off knock queues, keyed `groupIdB64:joinerKeyHex`
 *  (ADR-0059 section 1). Each value was verified under the joiner's account
 *  key, which is the key it is filed under; the approve looks it up by the
 *  joiner key it approves. In memory: the knock queue is re-read from the
 *  start every poll, so a reload refills it. */
const copublisherBindings = new Map<string, Uint8Array>();
/** Joiners approved before their binding was read, keyed like the map above,
 *  with what the appointment needs. Appointed when the binding arrives. */
const awaitingBinding = new Map<string, { identity: Identity; channelIdHex: string }>();

function bindingKey(groupIdB64: string, joinerKey: Uint8Array): string {
  return `${groupIdB64}:${bytesToHex(joinerKey)}`;
}

/** Record a knock row that is a co-publisher binding for this channel. */
function recordBinding(
  wasm: Wasm,
  groupIdB64: string,
  channelIdHex: string,
  payload: Uint8Array,
): string | null {
  let binding: Wasm["ChannelCoPublisherBinding"]["prototype"];
  try {
    binding = wasm.decodeChannelCoPublisherBinding(payload);
  } catch {
    return null;
  }
  try {
    if (bytesToHex(binding.channelId) !== channelIdHex.toLowerCase()) return null;
    const key = bindingKey(groupIdB64, binding.accountPubkey);
    copublisherBindings.set(key, binding.copublisherPubkey);
    return key;
  } finally {
    binding.free();
  }
}

/** Appoint the co-publisher key bound to `joinerKey`, when its binding has
 *  been read; otherwise remember to do it when it arrives. Never throws: a
 *  refused appointment (this account is not the owner, the cap is reached,
 *  the service is down) is logged and leaves the co-publisher unable to read
 *  the subscriber list until the owner appoints it. */
async function appointBound(
  identity: Identity,
  groupIdB64: string,
  joinerKey: Uint8Array,
  channelIdHex: string,
  apiBase?: string,
): Promise<void> {
  const key = bindingKey(groupIdB64, joinerKey);
  const copublisherKey = copublisherBindings.get(key);
  if (copublisherKey === undefined) {
    awaitingBinding.set(key, { identity, channelIdHex });
    return;
  }
  awaitingBinding.delete(key);
  try {
    await appointCoPublisher(channelIdHex, identity, copublisherKey, apiBase);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("co-publisher: appointing the key failed; only the owner can appoint", err);
  }
}

/** Poll the PublisherGroup's DS journal and apply incoming MLS messages
 *  (external-join proposals from prospective co-publishers + any co-publisher
 *  commits) via the role-gated path. Surfaces external-join proposals into the
 *  PublisherGroup's retained pending set. Since ADR-0053 the external-join
 *  proposals arrive in the group's KNOCK queue (a non-member cannot write the
 *  journal), read here beside the journal with its own cursor. Our own
 *  already-applied commits and non-MLS rows throw on
 *  `processIncomingVerifyingRoles` and are skipped. Returns the number of
 *  pending requests after the poll.
 *
 *  G-38 residual 3 — `CoPublishersPanel` drives this from a 5 s `setInterval`
 *  CONCURRENTLY with its own `approveCoPublisher`, which eagerly merges an Add
 *  into the same PublisherGroup. That is the ADR-0040 interleave on a
 *  PublisherGroup instead of a chat group, so the apply half takes the SAME
 *  per-group `withGroupMlsLock` the approve does. The fetch stays outside it
 *  (it touches no MLS state), mirroring `ChatClient.pollMessages`.
 *
 *  No cursor gate here, unlike the chat poll: this fetch is unconditionally
 *  `after: 0`, so it re-reads the whole journal every tick and holds no position
 *  that a concurrent poll could wind backward. Rows already merged simply throw
 *  and are skipped, which is the pre-existing (and still correct) behaviour. */
/** This session's read position in each PublisherGroup journal. */
const journalCursors = new Map<string, number>();

/** At most this many pages per read (100 rows each on the DS). */
const MAX_PAGES_PER_READ = 50;

/** Every row after `after`, page by page to the end of the journal. */
async function readPagesFrom<R extends { readonly id: number }>(
  after: number,
  fetchPage: (after: number) => Promise<{ readonly messages: readonly R[]; readonly hasMore?: boolean }>,
): Promise<R[]> {
  const rows: R[] = [];
  let cursor = after;
  for (let page = 0; page < MAX_PAGES_PER_READ; page++) {
    const raw = await fetchPage(cursor);
    rows.push(...raw.messages);
    const last = raw.messages.at(-1);
    if (last === undefined || raw.hasMore === false || last.id <= cursor) break;
    cursor = last.id;
  }
  return rows;
}

export async function pollCoPublisherJournal(
  ds: DsClient,
  identity: Identity,
  publisherGroup: PublisherGroup,
  /** An application message on the PublisherGroup journal (an admission
   *  seed, a read key, a step-down; ADR-0087 sections 7 and 15), with the
   *  account key its MLS sender resolved to at receipt (C-68). Runs inside
   *  the group's MLS lock: it must not take it again. */
  onApplication?: (plaintext: Uint8Array, senderAccount: Uint8Array | undefined) => void | Promise<void>,
  /** ADR-0090 section 4: a commit row was applied (its DS `created_at`). */
  onCommit?: (createdAt: string) => void,
): Promise<number> {
  const groupId = publisherGroup.groupId();
  const groupIdB64 = base64Encode(groupId);
  // The FETCH — deliberately outside the lock. From this session's cursor,
  // paged to the end: the DS answers 100 rows a page, and a read from 0 every
  // pass never reached a commit or an admission seed past the first page once
  // the journal outgrew it (found on Android, 2026-11-05). The cursor is in
  // memory: after a reload the journal is read from the start again, which is
  // safe (an applied commit is skipped) and keeps the cursor from ever leading
  // the durable group state.
  const raw = { messages: await readPagesFrom(journalCursors.get(groupIdB64) ?? 0, (after) => ds.fetchGroup(groupId, { after, waitSecs: 0 })) };
  // ADR-0053 contract point 7 — a prospective co-publisher cannot write the
  // journal of a claimed group, so its external-join proposal arrives as a
  // KNOCK. Read from the start every tick, exactly like the journal above and
  // for the same reason: this poll holds no durable position, and the
  // retained request it surfaces reaches disk only on the caller's next
  // channel-log flush, so a cursor moved past a knock could lose the request
  // to a reload. A re-applied knock is deduped by the quarantine; a stale-epoch
  // one fails and is skipped. Knocks are pruned after seven days, so the
  // re-read stays small. A failed read is retried next tick.
  let knocks: Awaited<ReturnType<DsClient["fetchKnocks"]>> | null = null;
  // The panel polls every 5 s; knocks are read at most once per 10 s per
  // group (`KNOCK_READ_MIN_INTERVAL_MS`), the floor every client shares.
  try {
    if (takeKnockReadSlot(groupIdB64, Date.now())) {
      knocks = { messages: await readPagesFrom(0, (after) => ds.fetchKnocks(groupId, { after, waitSecs: 0 })) } as Awaited<
        ReturnType<DsClient["fetchKnocks"]>
      >;
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("co-publisher: knock read failed; retried next tick", err);
  }
  // The APPLY — inside it.
  return withGroupMlsLock(groupIdB64, async () => {
    const wasm = await loadWasm();
    const device = publisherDevice();
    const lastRow = raw.messages.at(-1);
    if (lastRow !== undefined) {
      journalCursors.set(groupIdB64, Math.max(journalCursors.get(groupIdB64) ?? 0, lastRow.id));
    }
    for (const m of raw.messages) {
      let localEpoch: bigint | undefined;
      try {
        localEpoch = publisherGroup.epoch();
      } catch {
        localEpoch = undefined;
      }
      try {
        const processed = publisherGroup.processIncomingVerifyingRoles(device, m.payload);
        try {
          const data = processed.applicationData;
          if (processed.kind === "Application" && data !== undefined && onApplication !== undefined) {
            await onApplication(data, processed.senderAccount);
          } else if (processed.kind === "Commit") {
            // ADR-0090 section 4: the epoch began with this row; every owner
            // device restarts its rotation clock from the DS row time.
            onCommit?.(m.createdAt);
          }
        } finally {
          processed.free();
        }
      } catch (err) {
        // A commit we have already merged (epoch mismatch), a commit the
        // ADR-0090 gate refused (not merged), or a non-MLS entry — skip, the
        // FIFO journal lets the next poll catch up on any genuinely-new commit.
        // ADR-0091 phase 1: logged without identifiers, never silently.
        let sealed: bigint | undefined;
        let after: bigint | undefined;
        try {
          sealed = wasm.epochOfMessage(m.payload);
          after = wasm.epochAfterMessage(m.payload);
        } catch {
          sealed = undefined;
          after = undefined;
        }
        // eslint-disable-next-line no-console
        console.warn(
          failedRowLine({
            kind: failedApplyKind(err),
            sealed,
            after,
            local: localEpoch,
            latest: undefined,
            verdict: "skip",
            attempt: undefined,
          }),
        );
      }
    }
    if (knocks !== null) {
      const channelIdHex = bytesToHex(publisherGroup.channelId());
      for (const k of knocks.messages) {
        // ADR-0087 section 5 — a sealed KeyPackage row (the co-publisher
        // request since phase B): opened with this owner's knock seed,
        // retained, and approved later from the retained package.
        if (await applySealedCoPublisherRow(wasm, identity, device, publisherGroup, k.payload)) continue;
        // ADR-0059 — a co-publisher binding rides beside the proposal. It is
        // not an MLS message, so it is read here and never processed.
        const bound = recordBinding(wasm, groupIdB64, channelIdHex, k.payload);
        if (bound !== null) {
          const waiting = awaitingBinding.get(bound);
          if (waiting !== undefined) {
            const joinerHex = bound.slice(bound.indexOf(":") + 1);
            const joiner = hexToBytes(joinerHex);
            if (joiner !== null) {
              void appointBound(waiting.identity, groupIdB64, joiner, waiting.channelIdHex);
            }
          }
          continue;
        }
        try {
          // Surfaces an external-join proposal into the retained pending set.
          // `processKnock`, never `processIncomingVerifyingRoles`: the knock
          // queue is an unauthenticated write, and the core refuses anything
          // but a stranger's external-join proposal for this group before
          // processing it (a member's commit posted here would otherwise be
          // applied by this owner alone). A refused row, an attribution or a
          // stale-epoch proposal throws and is skipped.
          publisherGroup.processKnock(device, k.payload);
        } catch {
          // Not a proposal: it may be the joiner's attribution, which carries
          // the `reply` grant the approve writes the Welcome under (ADR-0060
          // section 6). Anything else is skipped.
          await rememberAttributionReply(wasm, groupId, k.payload);
        }
      }
    }
    // ADR-0090: applied commits and retained requests live in the device
    // provider: written (best effort) after the apply.
    if (raw.messages.length > 0 || (knocks?.messages.length ?? 0) > 0) channelsActions.notePublisherGroupChanged();
    // ADR-0057 obligation 6 — no heal on apply: whoever committed registered
    // its epoch before publishing.
    return pendingCoPublishers(publisherGroup).length;
  });
}

/**
 * One knock row in the ADR-0087 sealed form. `true` when the row was of that
 * shape (retained or dropped), `false` for any other row. Only the owner
 * holds the knock seed (its journal root), so a co-publisher's poll opens
 * nothing here. Never throws.
 */
async function applySealedCoPublisherRow(
  wasm: Wasm,
  identity: Identity,
  device: Identity,
  publisherGroup: PublisherGroup,
  payload: Uint8Array,
): Promise<boolean> {
  try {
    if (!wasm.isKnockRowLen(payload.length) || wasm.knockRowVersion(payload) === undefined) return false;
  } catch {
    return false;
  }
  const groupId = publisherGroup.groupId();
  const root = wasm.dsJournalRootSeed(identity.ed25519Seed, groupId);
  const seed = wasm.groupKnockSeed(root, INITIAL_KNOCK_VERSION);
  root.fill(0);
  let request: ReturnType<typeof wasm.openKnockRow> | undefined;
  try {
    request = wasm.openKnockRow(seed, payload);
  } catch {
    request = undefined;
  } finally {
    seed.fill(0);
  }
  if (request === undefined) return true;
  try {
    // Only a request that came by one of this group's links is taken.
    const cap = request.capability;
    if (cap === undefined || cap.length === 0) return true;
    try {
      const c = wasm.GroupInviteCapability.decodeAndVerify(cap);
      const same = base64Encode(c.groupId) === base64Encode(groupId);
      c.free();
      if (!same) return true;
    } catch {
      return true;
    }
    let joined;
    try {
      joined = publisherGroup.retainJoinRequest(device, request.keyPackage);
    } catch {
      return true;
    }
    const outcome = joined.outcome;
    const joinerKey = joined.joinerSignatureKey;
    joined.free();
    if (outcome !== "retained" && outcome !== "already-pending") return true;
    joinerCapabilities.set(bindingKey(base64Encode(groupId), joinerKey), base64Encode(cap));
    // ADR-0090: the leaf is a DEVICE; the account it is bound to is what
    // the approve records, appoints and seals to.
    const accountKey = wasm.keyPackageAccountKey(request.keyPackage);
    if (accountKey !== undefined) joinerAccounts.set(bindingKey(base64Encode(groupId), joinerKey), accountKey);
    const profile = request.profile;
    if (profile !== undefined) {
      try {
        const accountId = profile.accountId;
        if (accountId !== undefined && accountId.length === 16) {
          joinerAccountIds.set(bindingKey(base64Encode(groupId), joinerKey), accountId);
        }
      } finally {
        profile.free();
      }
    }
    const session = reachSession();
    const reply = request.replyGrant;
    try {
      // ADR-0090: the reply grant's binding is the joiner ACCOUNT's; kept
      // under the device leaf key the approve looks it up by.
      const account = joinerAccounts.get(bindingKey(base64Encode(groupId), joinerKey)) ?? joinerKey;
      if (session !== null && joinReplyBindingVerifies(reply, account)) {
        await rememberJoinReply(groupId, joinerKey, session.encodeGrant(reply));
      }
    } finally {
      reply.free();
    }
    return true;
  } finally {
    request.free();
  }
}

/**
 * ADR-0087 section 15 — joiner side since phase B: ask to become a publisher
 * from a co-publisher `/g/` link with a SEALED knock row (a fresh KeyPackage,
 * the reply grant the Welcome needs, the link's plaintext capability), sealed
 * to the knock key the link's capability carries; the ADR-0059 binding rides
 * in the same knock. No epoch is needed (the request survives commits). The
 * KeyPackage's private key lives in `identity`'s provider and is the only
 * thing that opens the Welcome, so it is persisted BEFORE the knock.
 */
export async function requestToCoPublishSealed(
  ds: DsClient,
  identity: Identity,
  link: string,
  persistBeforeKnock: (request: {
    readonly groupId: Uint8Array;
    readonly channelIdHex: string;
    readonly joinerKey: Uint8Array;
    readonly ownerKey: Uint8Array;
  }) => Promise<void>,
  discovery: Pick<DiscoveryClient, "fetchGroupInvite">,
): Promise<{ groupId: Uint8Array; channelIdHex: string; joinerKey: Uint8Array; ownerKey: Uint8Array }> {
  const wasm = await loadWasm();
  const device = publisherDevice();
  const widths = inviteLinkWidths(wasm);
  const split = splitGroupInviteLink(link, widths);
  if (split === null) throw new Error("not a /g/ co-publisher link");
  if (split.fragment.kind !== "secret") throw new InviteLinkDamagedError();
  const parsed = parseGroupInviteSlug(split.slug, widths);
  const sealed = await discovery.fetchGroupInvite(parsed.blobId);
  if (sealed === null) throw new Error("co-publisher link not found or revoked");
  const cap = openSealedInvite(
    (b, secret) => wasm.GroupInviteCapability.openLink(b, secret),
    sealed,
    split.fragment,
  );
  let groupId: Uint8Array;
  let wire: Uint8Array;
  let knockKey: Uint8Array | undefined;
  let knockVersion: number | undefined;
  let channelIdBytes: Uint8Array | undefined;
  let issuerKey: Uint8Array;
  try {
    groupId = cap.groupId;
    wire = cap.wire;
    issuerKey = cap.issuerSignaturePubkey;
    knockKey = cap.knockPublicKey;
    knockVersion = cap.knockVersion;
    let preview: ReturnType<typeof cap.openPreview> | undefined;
    try {
      preview = cap.openPreview(split.fragment.secret);
    } catch {
      preview = undefined;
    }
    try {
      channelIdBytes = preview?.channelId;
    } finally {
      preview?.free();
    }
  } finally {
    cap.free();
  }
  if (channelIdBytes === undefined) {
    if (isLegacyV1PublisherGroupId(groupId)) throw new LegacyChannelError("invite-link");
    throw new Error("co-publisher link does not target a channel PublisherGroup");
  }
  if (knockKey === undefined || knockVersion === undefined) {
    throw new Error("co-publisher link carries no knock key");
  }
  const channelIdHex = bytesToHex(channelIdBytes);
  channelsActions.setCoPublisherGroupId(channelIdHex, bytesToHex(groupId));
  const binding = wasm.composeChannelCoPublisherBinding(identity, channelIdBytes);
  const session = reachSession();
  if (session === null) throw new KeyedError("common.error.starting_up");
  const replyGrant = (await session.newReply("group-join", journalHex(groupId))).grant;
  let row: Uint8Array;
  try {
    // The profile carries the name and the account id the owner seals the
    // channel's admission seed to (ADR-0087 section 7).
    let profile: Uint8Array = new Uint8Array();
    try {
      profile = (await ownProfileEnvelopeSource?.()) ?? new Uint8Array();
    } catch {
      profile = new Uint8Array();
    }
    // ADR-0090: the leaf is this DEVICE (bound to the account), so the
    // KeyPackage is the device identity's, minted into its provider.
    row = wasm.sealKnockRow(knockKey, knockVersion, device.createKeyPackage(), replyGrant, profile, wire);
  } finally {
    replyGrant.free();
  }
  await persistBeforeKnock({ groupId, channelIdHex, joinerKey: device.signaturePublicKey, ownerKey: issuerKey });
  await ds.knock(groupId, [row, binding]);
  return { groupId, channelIdHex, joinerKey: device.signaturePublicKey, ownerKey: issuerKey };
}

/** The pending co-publisher external-join requests — each a 32-byte joiner leaf
 *  signature key (the `proposalRef` for approve / reject). */
export function pendingCoPublishers(publisherGroup: PublisherGroup): Uint8Array[] {
  const flat = publisherGroup.pendingExternalJoins();
  const out: Uint8Array[] = [];
  for (let i = 0; i + 32 <= flat.length; i += 32) {
    out.push(flat.slice(i, i + 32));
  }
  return out;
}

/** Approve a pending co-publisher. Commits the Add (advances the PublisherGroup
 *  epoch), registers the new epoch's DS access key together with the commit
 *  (ADR-0081: one request), delivers the Welcome to the
 *  joiner's own mailbox, then RESEALS the new-epoch channel seed to all
 *  subscribers — the Add changed the exporter output, so existing subscribers
 *  must get the new seed or they break (see the module docs).
 *
 *  A refused registration throws before anything is published. There is no
 *  durable rollback for a PublisherGroup commit (it has no ADR-0040 outbox),
 *  so the in-memory group stays at the unpublished epoch until the page is
 *  reloaded from the last channel-log snapshot; the caller shows the error.
 *  Resolves to the channel epoch the reseal landed at, which the caller
 *  hands the new seed to (`notePublisherCommitLanded`). */
export async function approveCoPublisher(
  ds: DsClient,
  identity: Identity,
  publisherGroup: PublisherGroup,
  proposalRef: Uint8Array,
  channelIdHex: string,
  apiBase?: string,
): Promise<number | null> {
  const groupId = publisherGroup.groupId();
  let landedAt: number | null = null;
  // G-38 residual 3 — SERIALIZED per group against the poll above. ADR-0090
  // section 3: the approve is STAGED; the journal decides, the stage is
  // merged on its acceptance, and the reseal reads the MERGED seed.
  await withGroupMlsLock(base64Encode(groupId), async () => {
    const device = publisherDevice();
    const account = accountKeyOfJoiner(groupId, proposalRef);
    const run = await runStagedPublisherCommit(
      { publisherGroup, identity, device, channelIdHex, ...(apiBase === undefined ? {} : { apiBase }), ds },
      {
        kind: "approve",
        stage: () => {
          const added = publisherGroup.stageApproveExternalJoin(device, proposalRef);
          try {
            return { commit: added.commit, welcome: added.welcome };
          } finally {
            added.free();
          }
        },
        joinerKeyB64: base64Encode(proposalRef),
        joinerAccountKeyB64: account === undefined ? null : base64Encode(account),
        replyGrantB64: await keptJoinReply(groupId, proposalRef),
      },
      approveExtras(ds, identity, groupId, channelIdHex, apiBase),
    );
    if (run.outcome === "landed") landedAt = run.serverTargetEpoch;
    // The grant stays until it expires (7 days): a replayed approve may need
    // to write the Welcome again.
  });
  return landedAt;
}

/** A joiner's kept reply grant, once the account's grants are loaded. */
async function keptJoinReply(groupId: Uint8Array, joinerKey: Uint8Array): Promise<string | null> {
  await joinRepliesOpened();
  return joinReplyFor(groupId, joinerKey);
}

/**
 * The approve-only steps of a recorded approve (the rotation ports supply
 * the journal, the merge and the reseal):
 *
 * - ADR-0053 contract point 8 — the Welcome goes to the joiner's own account
 *   mailbox, never to the journal. ADR-0060 section 6: the mailbox is closed,
 *   so the write is signed under the `reply` grant the joiner's request
 *   carried (kept in the record). ADR-0078 / ADR-0090: the row is sealed to
 *   the mailbox key that grant's binding names, verified against the joiner's
 *   ACCOUNT key (its leaf names a device). Rule 6: a refused write re-checks
 *   the address once.
 * - ADR-0059 — the appointment of the newcomer's co-publisher key, after the
 *   load-bearing reseal; never fails the approve.
 */
function approveExtras(
  ds: DsClient,
  identity: Identity,
  groupId: Uint8Array,
  channelIdHex: string,
  apiBase?: string,
): Partial<Pick<PublisherCommitPorts, "deliverWelcome" | "appoint">> {
  return {
    async deliverWelcome(welcome, joinerKey, replyGrantB64) {
      const wasm = await loadWasm();
      const account = accountKeyOfJoiner(groupId, joinerKey) ?? joinerKey;
      await writeToPerson(
        account,
        { explicit: replyGrantB64 ?? (await keptJoinReply(groupId, joinerKey)) },
        (target, auth) => ds.publishMailbox(target.address, sealRowFor(wasm, target, "welcome-rendezvous", welcome), auth),
      );
    },
    async appoint(joinerKey) {
      const account = accountKeyOfJoiner(groupId, joinerKey) ?? joinerKey;
      await appointBound(identity, base64Encode(groupId), account, channelIdHex, apiBase);
    },
  };
}

/**
 * G-50 — after the channel log hydrated (the provider restored), resume
 * every PublisherGroup commit whose steps had not all landed. An entry whose
 * group is below its target epoch never became durable with its record (a
 * torn write) and is dropped; every other entry runs its remaining steps.
 * Never throws: an entry that fails stays for the next hydration. Returns
 * the entries whose steps all landed, for what follows a commit
 * (`afterReplayedPublisherCommits`).
 */
export async function replayPublisherCommits(identity: Identity, apiBase?: string): Promise<LandedPublisherCommit[]> {
  const pending = channelsActions.pendingPublisherCommits();
  const landed: LandedPublisherCommit[] = [];
  const ds = new DsClient();
  for (const entry of pending) {
    // ADR-0090: an owed commit is re-sent by every pass; one a live run is
    // driving is left to it.
    if (commitInFlight(entry.id)) continue;
    if (remainingSteps(entry).length === 0) {
      // Every step landed before the reload; what follows a commit (the new
      // seed, the record) may not have.
      landed.push({ channelIdHex: entry.channelIdHex, kind: entry.kind, serverTargetEpoch: entry.serverTargetEpoch });
      await channelsActions.finishPublisherCommit(entry.id).catch(() => undefined);
      continue;
    }
    let group: PublisherGroup | null = null;
    try {
      group = await openPublisherGroupIfPresent(identity, entry.channelIdHex);
      const device = publisherDevice();
      await withGroupMlsLock(base64Encode(hexToBytes(entry.groupIdHex) ?? new Uint8Array(0)), async () => {
        const restored = group === null ? null : group.epoch();
        const verdict = replayVerdict(entry, restored, group?.hasStagedCommit() ?? false);
        if (verdict === "discard" || group === null) {
          // eslint-disable-next-line no-console
          console.info(`ADR-0090: a recorded ${entry.kind} commit was never staged on disk; dropped`);
          await channelsActions.finishPublisherCommit(entry.id);
          return;
        }
        // A merged stage: the DS took it before the reload.
        const resumed = verdict === "merged" ? withStepDone(entry, "journal") : entry;
        const ctx = {
          publisherGroup: group,
          identity,
          device,
          channelIdHex: entry.channelIdHex,
          ...(apiBase === undefined ? {} : { apiBase }),
          ds,
        };
        const ports = publisherCommitPorts(
          ctx,
          entry.kind === "approve"
            ? approveExtras(ds, identity, group.groupId(), entry.channelIdHex, apiBase)
            : entry.kind === "admit"
              ? admitExtras(ds, entry.channelIdHex)
              : {},
        );
        const outcome = await runPublisherCommit(resumed, ports);
        if (outcome === "landed") {
          landed.push({ channelIdHex: entry.channelIdHex, kind: entry.kind, serverTargetEpoch: entry.serverTargetEpoch });
        }
      });
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("G-50: a recorded PublisherGroup commit did not finish; it is retried next time", err);
    } finally {
      group?.free();
    }
  }
  await abandonUnrecordedStages(identity);
  return landed;
}

/**
 * ADR-0090 section 3 — a PublisherGroup this device holds with a staged
 * commit no recorded entry names: the entry is written before any commit
 * leaves the device, so nothing of it was sent. Abandoned, so the group's
 * membership operations are not frozen. Never throws.
 */
async function abandonUnrecordedStages(identity: Identity): Promise<void> {
  const recorded = new Set(channelsActions.pendingPublisherCommits().map((e) => e.channelIdHex));
  for (const idHex of channelsState.keysByChannel().keys()) {
    if (recorded.has(idHex)) continue;
    let group: PublisherGroup | null = null;
    try {
      group = await openPublisherGroupIfPresent(identity, idHex);
      if (group === null || !group.hasStagedCommit()) continue;
      const g = group;
      await withGroupMlsLock(base64Encode(g.groupId()), async () => {
        if (!g.hasStagedCommit()) return;
        g.abandonStagedCommit(publisherDevice());
        // eslint-disable-next-line no-console
        console.info("ADR-0090: an unrecorded staged PublisherGroup commit was abandoned");
        await channelsActions.persistPublisherGroupDurable().catch(() => undefined);
      });
    } catch {
      /* the group cannot be read here: nothing to abandon */
    } finally {
      group?.free();
    }
  }
}

/** A recorded PublisherGroup commit whose steps a replay ran to the end (its
 *  reseal landed the channel at `serverTargetEpoch`). */
export interface LandedPublisherCommit {
  readonly channelIdHex: string;
  readonly kind: PublisherCommitEntry["kind"];
  readonly serverTargetEpoch: number;
}

/** Open the PublisherGroup of a channel without founding one: `null` when the
 *  restored provider does not hold it. */
async function openPublisherGroupIfPresent(identity: Identity, channelIdHex: string): Promise<PublisherGroup | null> {
  const wasm = await loadWasm();
  const channelId = hexToBytes(channelIdHex);
  if (channelId === null) return null;
  const device = publisherDevice();
  const joinedIdHex = channelsActions.coPublisherGroupIdOf(channelIdHex);
  if (joinedIdHex !== undefined) {
    const joinedId = hexToBytes(joinedIdHex);
    return joinedId === null ? null : (wasm.PublisherGroup.loadJoined(device, joinedId, channelId) ?? null);
  }
  return wasm.PublisherGroup.load(device, identity.ed25519Seed, channelId) ?? null;
}

/** Reject a pending co-publisher — drops the retained request, no commit, no
 *  epoch advance. The retained set lives in the MLS storage snapshot (which is
 *  why the call takes the identity, like `approveExternalJoin`), so the drop is
 *  captured by the channel log's next snapshot flush and survives a restart. */
export function rejectCoPublisher(
  identity: Identity,
  publisherGroup: PublisherGroup,
  proposalRef: Uint8Array,
): void {
  void identity;
  publisherGroup.rejectExternalJoin(publisherDevice(), proposalRef);
  // Refused: the kept reply grant has nothing left to carry.
  void forgetJoinReply(publisherGroup.groupId(), proposalRef);
}

// ---------------------------------------------------------
// Joiner side — a prospective co-publisher requests + joins
// ---------------------------------------------------------

/** ADR-0028 OQ3 — joiner side: request to become a co-publisher of a channel
 *  from a `/g/` co-publisher link. Decodes + verifies the capability, recovers
 *  the channel id from the PublisherGroup `group_id` (the link conveys only the
 *  `group_id`), and knocks (ADR-0053 contract point 6): a token-gated probe
 *  returns the group's epoch, the external-join proposal (ADR-0027) is built
 *  for it, and a second knock carries a self-signed join-attribution (OQ2) and
 *  the proposal. No journal read. Returns the handles the caller polls with
 *  via `pollForCoPublishWelcome`.
 *
 *  `persistBeforeKnock` must record `identity`'s provider durably and throw
 *  when it could not (`channelsActions.persistPublisherGroupDurable`): the
 *  proposal's KeyPackage private key, minted into that provider, is the only
 *  thing that opens the owner's Welcome, so it is written before the knock
 *  that carries the proposal and a refused write sends nothing. */
export async function requestToCoPublish(
  ds: DsClient,
  identity: Identity,
  link: string,
  persistBeforeKnock: () => Promise<void>,
  discovery: Pick<DiscoveryClient, "fetchGroupInvite">,
): Promise<{ groupId: Uint8Array; channelIdHex: string; joinerKey: Uint8Array }> {
  const wasm = await loadWasm();
  // The shared `/g/` shape rules (ADR-0028 short-link amendment): the slug is
  // the 8-character ledger id and ends at the fragment. An inline slug (the
  // removed form) is refused here as malformed.
  const widths = inviteLinkWidths(wasm);
  const split = splitGroupInviteLink(link, widths);
  if (split === null) throw new Error("not a /g/ co-publisher link");
  // A missing or malformed fragment is damaged before anything else runs.
  if (split.fragment.kind !== "secret") throw new InviteLinkDamagedError();
  const parsed = parseGroupInviteSlug(split.slug, widths);
  const sealed = await discovery.fetchGroupInvite(parsed.blobId);
  if (sealed === null) throw new Error("co-publisher link not found or revoked");
  const cap = openSealedInvite(
    (b, secret) => wasm.GroupInviteCapability.openLink(b, secret),
    sealed,
    split.fragment,
  );
  let groupId: Uint8Array;
  // The PLAINTEXT wire: the join attribution carries it.
  let wire: Uint8Array;
  // ADR-0078 section 6: the channel id rides the capability's sealed
  // preview, opened with the same fragment secret.
  let channelIdBytes: Uint8Array | undefined;
  try {
    groupId = cap.groupId;
    wire = cap.wire;
    let preview: ReturnType<typeof cap.openPreview> | undefined;
    try {
      preview = cap.openPreview(split.fragment.secret);
    } catch {
      preview = undefined;
    }
    try {
      channelIdBytes = preview?.channelId;
    } finally {
      preview?.free();
    }
  } finally {
    cap.free();
  }
  if (channelIdBytes === undefined) {
    // A link minted under the retired v1 derivation (recognisable by its
    // 41-byte group id) has its own copy; any other `/g/` link without a
    // channel in its preview targets a chat group.
    if (isLegacyV1PublisherGroupId(groupId)) throw new LegacyChannelError("invite-link");
    throw new Error("co-publisher link does not target a channel PublisherGroup");
  }
  const channelIdHex = bytesToHex(channelIdBytes);
  // The group id this device opens the joined group under; written durably
  // with the provider before the knock (`persistBeforeKnock`).
  channelsActions.setCoPublisherGroupId(channelIdHex, bytesToHex(groupId));
  // ADR-0059 section 1 — this account's co-publisher key for the channel,
  // signed by its account identity key (the key the owner approves), rides
  // beside the proposal so the owner can appoint it. Not on the empty probe.
  const binding = wasm.composeChannelCoPublisherBinding(identity, channelIdBytes);
  // ADR-0060 section 6 (C-69) — the owner writes the Welcome to this
  // account's closed mailbox under our `reply` grant, which rides the signed
  // attribution. Registered before the knock; closed when the Welcome joins.
  const session = reachSession();
  if (session === null) throw new KeyedError("common.error.starting_up");
  const replyGrant = (await session.newReply("group-join", journalHex(groupId))).grant;
  let attribution: Uint8Array | null;
  try {
    attribution = wasm.composeGroupJoinAttribution(identity.ed25519Seed, wire, replyGrant);
  } catch {
    attribution = null;
  } finally {
    replyGrant.free();
  }
  await knockToJoin({
    knock: (payloads) =>
      ds.knock(groupId, payloads.length === 0 ? payloads : [...payloads, binding]),
    // ADR-0090: the leaf is this DEVICE.
    buildProposal: (epoch) => publisherDevice().createExternalJoinProposal(groupId, epoch),
    // Self-signed join attribution (OQ2): advisory for the approval, and the
    // only carrier of our reply grant.
    buildAttribution: () => attribution,
    persistBeforeKnock,
  });
  return { groupId, channelIdHex, joinerKey: publisherDevice().signaturePublicKey };
}

/** What the co-publisher Welcome wait has read of each mailbox in this
 *  session (`co-publish-welcome-scan.ts`). */
const welcomeScan = createWelcomeScanState();

/** ADR-0028 OQ3 — joiner side: look in this identity's own mailbox for the
 *  Welcome the owner sent after approving, join from it, and mint local
 *  publisher keys so this device can now publish to the channel. The joined
 *  group lands at the post-approve epoch — the same one the subscribers were
 *  resealed to — so the derived `channelSeed` matches. Returns the keys on
 *  success, or `null` if no Welcome has arrived yet (keep polling).
 *
 *  ADR-0078 — the Welcome is in the account mailbox of `identity` (the
 *  identity whose provider holds the PublisherGroup), named after the
 *  mailbox key that identity's seed derives and read under that key's
 *  proof. Every row is opened with that key; a row that does not open is
 *  dropped. Both carriers a sending client may use are accepted: a Welcome
 *  row (what a direct add sends) and a Welcome-rendezvous row bound to this
 *  joiner's key. `joinerKey` is `identity`'s key.
 *
 *  ADR-0074 section 3.5a - an owner that has observed this account's proven
 *  rotation writes the Welcome to the CURRENT account mailbox; one that has
 *  not writes to the mailbox of `identity` (after a rotation: the
 *  predecessor the PublisherGroup's provider is bound to). `currentIdentity`
 *  (the current account identity, when it is another instance) is read too,
 *  first. Reads only. */
export async function pollForCoPublishWelcome(
  ds: DsClient,
  identity: Identity,
  groupId: Uint8Array,
  joinerKey: Uint8Array,
  channelIdHex: string,
  ownerKey: Uint8Array,
  existingSigningSeed?: Uint8Array,
  currentIdentity?: Identity | null,
): Promise<ChannelPublisherKeys | null> {
  // The mailbox rows are this account's own, so the joiner key is not
  // matched: a Welcome sealed to another KeyPackage simply does not open.
  void joinerKey;
  const wasm = await loadWasm();
  const owners: Identity[] = [];
  if (currentIdentity !== undefined && currentIdentity !== null && currentIdentity !== identity) {
    owners.push(currentIdentity);
  }
  owners.push(identity);
  // Only the rows after what an earlier pass of this session read: the
  // Welcome is among the newest rows, and the account mailbox holds every
  // Welcome of every device of the account (22 of them, seen live
  // 2026-11-07), none of which opens as a PublisherGroup.
  const candidates: { readonly key: string; readonly welcome: Uint8Array }[] = [];
  let scanned = 0;
  let scanError: unknown = null;
  for (const owner of owners) {
    const key = wasm.MailboxKey.fromAccountIdentity(owner);
    try {
      const address = key.address();
      const addressHex = bytesToHex(address);
      let newest = welcomeScan.cursorOf(addressHex);
      await scanJournal<never>(
        (after, limit) => ds.fetchMailbox(address, { after, limit, waitSecs: 0 }),
        (m) => {
          if (m.id > newest) newest = m.id;
          const row = openOwnRow(wasm, key, m.payload);
          if (row === null) return undefined;
          const rowKey = `${addressHex}:${m.id}`;
          // The owner's device seals the raw MLS Welcome as the row's body
          // (web `approvePorts`, Android `CoPublisher.kt`): the row is the
          // Welcome, not a versioned rendezvous envelope. The mailbox row is
          // already this account's, so no joiner key match is needed; a
          // Welcome sealed to another KeyPackage simply does not open.
          if (row.kind === "welcome-rendezvous" || row.kind === "welcome") {
            candidates.push({ key: rowKey, welcome: row.body });
          }
          return undefined;
        },
        newest,
      );
      welcomeScan.advance(addressHex, newest);
      scanned += 1;
    } catch (err) {
      scanError = err;
    } finally {
      key.free();
    }
  }
  // One mailbox that cannot be read (its 30 days are over, its signer is
  // gone) must not hide the other; none read at all is the caller's error.
  if (scanned === 0 && scanError !== null) throw scanError;
  // Newest first, and a Welcome sealed to a KeyPackage this device no longer
  // holds (or a Welcome for another group) is skipped rather than ending the
  // wait. Opening is local and deterministic, so a Welcome that failed once
  // never opens later: it is remembered and not tried again.
  // ADR-0081 amendment — a PublisherGroup carries no role table, so it joins
  // through its own entry point: `Group.joinFromWelcome` refuses its Welcome,
  // and this one refuses a Welcome for any other group (another channel this
  // identity was invited to, or a chat) before anything is written.
  let refused = 0;
  for (const c of candidates.reverse()) {
    if (welcomeScan.hasFailed(c.key)) continue;
    const keys = await joinCoPublishWelcome(identity, c.welcome, groupId, channelIdHex, ownerKey, existingSigningSeed);
    if (keys !== null) {
      if (refused > 0) logRefused(refused);
      return keys;
    }
    welcomeScan.markFailed(c.key);
    refused += 1;
  }
  if (refused > 0) logRefused(refused);
  return null;
}

function logRefused(n: number): void {
  // eslint-disable-next-line no-console
  console.info(`co-publish: ${n} Welcome(s) in the mailbox are not this request's; not tried again`);
}

/**
 * Join the PublisherGroup `groupId` of channel `channelIdHex` from one
 * Welcome, and become a publisher: the keys at the post-approve epoch, or
 * `null` when the Welcome is not this group's or not sealed to a KeyPackage
 * `identity` holds (nothing is written then).
 *
 * ADR-0081 amendment — a PublisherGroup carries no role table, so it joins
 * through its own entry point: `Group.joinFromWelcome` refuses its Welcome,
 * and this one refuses a Welcome for any other group (another channel this
 * identity was invited to, or a chat) before anything is written. Used by
 * the Accept page's wait and by the mailbox receive loop
 * (`landPendingPublisherWelcome`).
 */
export async function joinCoPublishWelcome(
  identity: Identity,
  welcome: Uint8Array,
  groupId: Uint8Array,
  channelIdHex: string,
  ownerKey: Uint8Array,
  existingSigningSeed?: Uint8Array,
): Promise<ChannelPublisherKeys | null> {
  const wasm = await loadWasm();
  const channelId = hexToBytes(channelIdHex);
  if (channelId === null) throw new Error(`channel id hex is not valid: ${channelIdHex}`);
  // lakko-security F2 / F3 (`welcome-replay.ts`): a Welcome this device
  // already joined is not joined again, and a join below the last group this
  // device deleted is undone (unless the channel is under reset suspicion).
  const hashHex = await welcomeHashHex(welcome);
  const access = channelsActions.accessOf(channelIdHex);
  if (welcomeAlreadyJoined(access, hashHex)) return null;
  const device = publisherDevice();
  let group: PublisherGroup;
  try {
    // ADR-0090: joined with this DEVICE; refused unless the Welcome's sender
    // is a device of `ownerKey` (the `/g/` capability's issuer).
    group = wasm.PublisherGroup.joinFromWelcome(device, welcome, groupId, channelId, ownerKey);
  } catch {
    return null;
  }
  try {
    if (
      joinBelowDeletedGroup({
        joinedEpoch: group.epoch(),
        deletedGroupEpoch: access.deletedGroupEpoch,
        resetSuspected: access.resetSuspected === true,
      })
    ) {
      group.deleteLocally(device);
      // eslint-disable-next-line no-console
      console.warn("co-publish: a Welcome at an epoch not above the last deleted group's was not taken");
      return null;
    }
  } finally {
    group.free();
  }
  await channelsActions.updateAccessDurable(channelIdHex, (r) => withJoinedWelcome(r, hashHex));
  // The Welcome landed: the single-use reply key of this request goes.
  void reachSession()?.closeRepliesTo(journalHex(groupId));
  // Now an MLS member of the PublisherGroup — `freshPublisherKeys` opens the
  // (now-joined) group, derives the shared content-key seed, and reuses the
  // persisted per-device signing seed (or mints one) so this device can
  // publish.
  const fresh = await freshPublisherKeys(identity, channelIdHex, existingSigningSeed, groupId);
  // ADR-0090: the channel epoch this group epoch serves, from the service.
  const offset = await learnEpochOffset(channelIdHex, fresh.seedEpoch ?? 0);
  const keys = { ...fresh, seedEpoch: (fresh.seedEpoch ?? 0) + offset };
  // ADR-0090: the owner key this device verified, for the account's register.
  const ownerKeyB64 = base64Encode(ownerKey);
  await channelsActions.updateAccessDurable(channelIdHex, (r) =>
    r.ownerAccountKeyB64 === ownerKeyB64 ? r : { ...r, ownerAccountKeyB64: ownerKeyB64 },
  );
  // ADR-0059 section 9: this account publishes here as a co-publisher, so
  // its writes are signed with the co-publisher key from the first one.
  await rememberPublisherRole(channelIdHex, identity, "copublisher");
  return keys;
}

// ---------------------------------------------------------
// helpers (mirror rotation-cron.ts)
// ---------------------------------------------------------

/** Keep the `reply` grant of a join attribution read from this group's knock
 *  queue (ADR-0060 section 6). Anything that is not an attribution for this
 *  group is ignored. */
async function rememberAttributionReply(wasm: Wasm, groupId: Uint8Array, payload: Uint8Array): Promise<void> {
  const session = reachSession();
  if (session === null) return;
  let attr: import("../../wasm/lakko_ffi_wasm").GroupJoinAttribution;
  try {
    attr = wasm.GroupJoinAttribution.parse(payload);
  } catch {
    return;
  }
  let kept: { readonly key: Uint8Array; readonly grant: string } | null = null;
  try {
    if (base64Encode(attr.groupId) !== base64Encode(groupId)) return;
    const reply = attr.replyGrant;
    try {
      // ADR-0078 - the reply grant's mailbox binding must verify against the
      // joiner's account key, which for a co-publisher IS the key the
      // attribution is self-signed with. A grant naming someone else's
      // mailbox is not kept (fail closed).
      if (!joinReplyBindingVerifies(reply, attr.joinerSignatureKey)) return;
      kept = { key: attr.joinerSignatureKey, grant: session.encodeGrant(reply) };
    } finally {
      reply.free();
    }
  } finally {
    attr.free();
  }
  // Durable before the knock cursor moves past the row (`join-replies.ts`).
  if (kept !== null) await rememberJoinReply(groupId, kept.key, kept.grant);
}

function hexToBytes(hex: string): Uint8Array | null {
  if (hex.length === 0 || hex.length % 2 !== 0) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; ++i) {
    const hi = hexNibble(hex.charCodeAt(2 * i));
    const lo = hexNibble(hex.charCodeAt(2 * i + 1));
    if (hi < 0 || lo < 0) return null;
    out[i] = (hi << 4) | lo;
  }
  return out;
}

function hexNibble(code: number): number {
  if (code >= 48 && code <= 57) return code - 48;
  if (code >= 97 && code <= 102) return 10 + code - 97;
  if (code >= 65 && code <= 70) return 10 + code - 65;
  return -1;
}

