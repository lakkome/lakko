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
 *   - `createCoPublisherLink` — mint a `/g/<slug>` invite over the
 *     PublisherGroup's MLS `group_id` (an inline link; the owner shares it).
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
 * `approveCoPublisher` does this via `resealSubscribers` — the same audited
 * `sealSubscriberSeedEnvelope` + atomic `POST /v1/channels/:id/rotate` fanout as
 * `rotation-cron`, but WITHOUT a second `rotate()` (the Add already advanced the
 * epoch; the reseal binds to the post-approve epoch the co-publisher joins at).
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

import type { Wasm } from "../wasm";
import { sealEachSkippingInvalid } from "./member-card-rules";
import { loadWasm } from "../wasm";
import { DsClient } from "../ds/client";
import { scanJournal } from "../chat/journal-head-epoch";
import {
  dsAccess,
  knockToJoin,
  registerBeforePublish,
  takeSigner,
} from "../ds/access";
import { openOwnRow, sealRowFor } from "../contacts/mailbox-row";
import { takeKnockReadSlot } from "../chat/knock-cursor";
import { base64Encode } from "../auth/account-id";
import { withGroupMlsLock } from "../mls/group-lock";
import {
  appointCoPublisher,
  fetchEpoch,
  listSubscribers,
  rememberPublisherRole,
  rotateChannel,
  type RotateBatchEntry,
} from "./api";
import {
  bytesToHex,
  freshPublisherKeys,
  openOrFoundPublisherGroup,
  type ChannelPublisherKeys,
} from "./composer";
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

/** Open (or create) the persistent PublisherGroup handle for a channel. The
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
  // A channel minted under the retired v1 derivation refuses inside with the
  // readable copy. A group founded here claims its journal (ADR-0057).
  return await openOrFoundPublisherGroup(wasm, identity, channelId);
}

/**
 * ADR-0053 contract points 1 + 2 — let the DS client prove requests to this
 * PublisherGroup's journal under the group's current access key, for as long
 * as the caller holds the handle (a refused request makes the DS client
 * claim the journal with the channel owner's root, ADR-0057). Returns the function that
 * withdraws it; call that BEFORE freeing the handle.
 */
export function registerPublisherGroupAccess(
  identity: Identity,
  publisherGroup: PublisherGroup,
): () => void {
  const own = publisherGroup.groupId();
  const matches = (groupId: Uint8Array): boolean => {
    if (groupId.length !== own.length) return false;
    for (let i = 0; i < own.length; i++) if (groupId[i] !== own[i]) return false;
    return true;
  };
  const unregisterSource = dsAccess.registerGroupSource((groupId) =>
    matches(groupId) ? publisherGroup.dsAccessSigner(identity) : null,
  );
  // ADR-0057 — the channel owner holds the PublisherGroup journal's root
  // (the handle this panel holds is the owner's). A co-publisher on web holds
  // no registrar seed: a PublisherGroup carries no application messages to
  // deliver one in, so its own commits are not registered (stated residual).
  const unregisterOwner = dsAccess.registerOwnerSource((groupId) =>
    matches(groupId) ? true : null,
  );
  // ADR-0057 obligation 2 - the owner claims the journal on open (once per
  // session): an unclaimed journal never answers the 401 the lazy claim
  // waits for.
  void new DsClient().ensureOwnerClaim(own);
  return () => {
    unregisterSource();
    unregisterOwner();
  };
}

/** Mint a co-publisher invite link over the PublisherGroup's MLS `group_id`. A
 *  non-member opens it, requests to join, and the owner approves (reusing the
 *  ADR-0027/0028 external-join machinery). Inline link — the channel owner
 *  shares it out-of-band; `0n` = never expires. The capability is sealed under
 *  a fresh link secret carried in the fragment (2026-09-28 security pass):
 *  `<origin>/g/<sealed>#<secret>`. */
export async function createCoPublisherLink(
  identity: Identity,
  publisherGroup: PublisherGroup,
  origin: string,
): Promise<string> {
  const wasm = await loadWasm();
  const groupId = publisherGroup.groupId();
  // Sealed-link amendment of ADR-0028 (2026-09-28): the capability is sealed
  // under a fresh link secret that rides in the fragment. Always the inline
  // form (never uploaded to the ledger).
  const mint = wasm.composeGroupInviteCapability(identity.ed25519Seed, groupId, 0n);
  try {
    return composeSealedInviteLink({
      origin,
      prefix: GROUP_INVITE_URL_PREFIX,
      blobId: null,
      sealed: mint.sealed,
      secret: mint.linkSecret,
      widths: inviteLinkWidths(wasm),
    });
  } finally {
    mint.free();
  }
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
export async function pollCoPublisherJournal(
  ds: DsClient,
  identity: Identity,
  publisherGroup: PublisherGroup,
): Promise<number> {
  const groupId = publisherGroup.groupId();
  const groupIdB64 = base64Encode(groupId);
  // The FETCH — deliberately outside the lock.
  const raw = await ds.fetchGroup(groupId, { after: 0, waitSecs: 0 });
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
      knocks = await ds.fetchKnocks(groupId, { after: 0, waitSecs: 0 });
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("co-publisher: knock read failed; retried next tick", err);
  }
  // The APPLY — inside it.
  return withGroupMlsLock(groupIdB64, async () => {
    const wasm = await loadWasm();
    for (const m of raw.messages) {
      try {
        publisherGroup.processIncomingVerifyingRoles(identity, m.payload);
      } catch {
        // A commit we have already merged (epoch mismatch) or a non-MLS entry
        // — skip, the FIFO journal lets the next poll catch up on any
        // genuinely-new commit.
      }
    }
    if (knocks !== null) {
      const channelIdHex = bytesToHex(publisherGroup.channelId());
      for (const k of knocks.messages) {
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
          publisherGroup.processKnock(identity, k.payload);
        } catch {
          // Not a proposal: it may be the joiner's attribution, which carries
          // the `reply` grant the approve writes the Welcome under (ADR-0060
          // section 6). Anything else is skipped.
          rememberAttributionReply(wasm, groupId, k.payload);
        }
      }
    }
    // ADR-0057 obligation 6 — no heal on apply: whoever committed registered
    // its epoch before publishing.
    return pendingCoPublishers(publisherGroup).length;
  });
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
 *  epoch), registers the new epoch's DS access key and broadcasts the commit to
 *  the journal (ADR-0053: register before publish), delivers the Welcome to the
 *  joiner's own mailbox, then RESEALS the new-epoch channel seed to all
 *  subscribers — the Add changed the exporter output, so existing subscribers
 *  must get the new seed or they break (see the module docs).
 *
 *  A refused registration throws before anything is published. There is no
 *  durable rollback for a PublisherGroup commit (it has no ADR-0040 outbox),
 *  so the in-memory group stays at the unpublished epoch until the page is
 *  reloaded from the last channel-log snapshot; the caller shows the error. */
export async function approveCoPublisher(
  ds: DsClient,
  identity: Identity,
  publisherGroup: PublisherGroup,
  proposalRef: Uint8Array,
  channelIdHex: string,
  apiBase?: string,
): Promise<void> {
  const groupId = publisherGroup.groupId();
  // G-38 residual 3 — SERIALIZED per group against the 5 s interval poll above.
  // `approveExternalJoin` eagerly merges the Add into the shared provider, so
  // it must not run while a poll is mid-apply on the same group; and the reseal
  // is inside the region because it reads the POST-approve `channelSeed`, which
  // another applied commit would move under it.
  await withGroupMlsLock(base64Encode(groupId), async () => {
    // ADR-0053 — the key of the epoch the Add leaves, taken BEFORE the create.
    const signerN = takeSigner(() => publisherGroup.dsAccessSigner(identity));
    try {
      const added = publisherGroup.approveExternalJoin(identity, proposalRef);
      const signerN1 = takeSigner(() => publisherGroup.dsAccessSigner(identity));
      try {
        // ADR-0057 — registered with the channel owner's root.
        await registerBeforePublish(ds, groupId, signerN1);
      } finally {
        signerN1?.free();
      }
      await ds.publishGroup(groupId, added.commit, { signer: signerN });
      // ADR-0053 contract point 8 — the Welcome goes to the joiner's own
      // account mailbox, the way a direct add delivers one; never to the
      // journal. ADR-0060 section 6: the mailbox is closed, so the write is
      // signed under the `reply` grant the joiner's attribution carried (kept
      // by the poll). ADR-0078: the row is sealed as a Welcome to the mailbox
      // key that grant's binding names, verified against the joiner's account
      // key (`proposalRef`: a co-publisher's leaf IS its account key).
      // Rule 6: a refused write re-checks the address once.
      const wasm = await loadWasm();
      const welcome = added.welcome;
      await writeToPerson(
        proposalRef,
        { explicit: joinReplyFor(proposalRef) },
        (target, auth) => ds.publishMailbox(target.address, sealRowFor(wasm, target, "welcome-rendezvous", welcome), auth),
      );
      forgetJoinReply(proposalRef);
    } finally {
      signerN?.free();
    }
    await resealSubscribers(publisherGroup, identity, channelIdHex, apiBase);
    // ADR-0059 — appoint the newcomer's co-publisher key so its own rotation
    // fanout can read the subscriber list. After the load-bearing reseal, and
    // never failing the approve.
    await appointBound(identity, base64Encode(groupId), proposalRef, channelIdHex, apiBase);
  });
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
  publisherGroup.rejectExternalJoin(identity, proposalRef);
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
): Promise<{ groupId: Uint8Array; channelIdHex: string; joinerKey: Uint8Array }> {
  const wasm = await loadWasm();
  // The shared `/g/` shape rules (ADR-0028 short-link amendment): the slug
  // ends at the fragment, and a co-publisher link is always the inline form
  // (the owner never uploads it to the ledger).
  const widths = inviteLinkWidths(wasm);
  const split = splitGroupInviteLink(link, widths);
  if (split === null) throw new Error("not a /g/ co-publisher link");
  // A missing or malformed fragment is damaged before anything else runs.
  if (split.fragment.kind !== "secret") throw new InviteLinkDamagedError();
  const parsed = parseGroupInviteSlug(split.slug, widths);
  if (parsed.kind !== "inline") throw new Error("co-publisher link is not an inline capability");
  const cap = openSealedInvite(
    (sealed, secret) => wasm.GroupInviteCapability.openLink(sealed, secret),
    parsed.sealed,
    split.fragment,
  );
  let groupId: Uint8Array;
  // The PLAINTEXT wire: the join attribution carries it.
  let wire: Uint8Array;
  try {
    groupId = cap.groupId;
    wire = cap.wire;
  } finally {
    cap.free();
  }
  const channelIdBytes = wasm.channelIdFromPublisherGroupId(groupId);
  if (channelIdBytes === undefined) {
    // Two distinct reasons the id does not resolve, and they need different
    // copy: a link minted under the retired v1 derivation (recognisable by its
    // 41-byte shape) versus a `/g/` link that simply targets a chat group.
    if (isLegacyV1PublisherGroupId(groupId)) throw new LegacyChannelError("invite-link");
    throw new Error("co-publisher link does not target a channel PublisherGroup");
  }
  const channelIdHex = bytesToHex(channelIdBytes);
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
    buildProposal: (epoch) => identity.createExternalJoinProposal(groupId, epoch),
    // Self-signed join attribution (OQ2): advisory for the approval, and the
    // only carrier of our reply grant.
    buildAttribution: () => attribution,
    persistBeforeKnock,
  });
  return { groupId, channelIdHex, joinerKey: identity.signaturePublicKey };
}

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
  existingSigningSeed?: Uint8Array,
  currentIdentity?: Identity | null,
): Promise<ChannelPublisherKeys | null> {
  const wasm = await loadWasm();
  const owners: Identity[] = [];
  if (currentIdentity !== undefined && currentIdentity !== null && currentIdentity !== identity) {
    owners.push(currentIdentity);
  }
  owners.push(identity);
  // Paged to the end: the Welcome is among the newest rows.
  const candidates: Uint8Array[] = [];
  let scanned = 0;
  let scanError: unknown = null;
  for (const owner of owners) {
    const key = wasm.MailboxKey.fromAccountIdentity(owner);
    try {
      const address = key.address();
      await scanJournal<never>(
        (after, limit) => ds.fetchMailbox(address, { after, limit, waitSecs: 0 }),
        (m) => {
          const row = openOwnRow(wasm, key, m.payload);
          if (row === null) return undefined;
          if (row.kind === "welcome-rendezvous") {
            const rendezvous = wasm.welcomeRendezvousFor(row.body, joinerKey);
            if (rendezvous !== undefined) candidates.push(rendezvous);
          } else if (row.kind === "welcome") {
            candidates.push(row.body);
          }
          return undefined;
        },
      );
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
  // wait.
  let group: InstanceType<typeof wasm.Group> | null = null;
  for (const welcome of candidates.reverse()) {
    try {
      const joined = wasm.Group.joinFromWelcome(welcome, identity);
      // The mailbox is the account's and may hold a Welcome for another
      // channel this identity was invited to; only this group ends the wait.
      if (base64Encode(joined.groupId) !== base64Encode(groupId)) {
        joined.free();
        continue;
      }
      group = joined;
      break;
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("co-publish: a Welcome could not be opened; waiting for a newer one", err);
    }
  }
  if (group === null) return null;
  group.free();
  // The Welcome landed: the single-use reply key of this request goes.
  void reachSession()?.closeRepliesTo(journalHex(groupId));
  // Now an MLS member of the PublisherGroup — `freshPublisherKeys` opens the
  // (now-joined) group, derives the shared content-key seed, and reuses the
  // persisted per-device signing seed (or mints one) so this device can
  // publish.
  const keys = await freshPublisherKeys(identity, channelIdHex, existingSigningSeed);
  // ADR-0059 section 9: this account publishes here as a co-publisher, so
  // its writes are signed with the co-publisher key from the first one.
  await rememberPublisherRole(channelIdHex, identity, "copublisher");
  return keys;
}

/**
 * Reseal the CURRENT (post-approve) channel seed to all subscribers — the
 * rotation-cron fanout WITHOUT advancing the MLS epoch (the Add already did).
 * Binds the envelopes to the new server epoch the `/rotate` endpoint sets, so a
 * single envelope-seal failure aborts the whole batch (the same atomic posture
 * the server enforces).
 */
async function resealSubscribers(
  publisherGroup: PublisherGroup,
  identity: Identity,
  channelIdHex: string,
  apiBase?: string,
): Promise<void> {
  const wasm = await loadWasm();
  const newSeed = publisherGroup.channelSeed(identity);
  const channelIdBytes = publisherGroup.channelId();
  const current = await fetchEpoch(channelIdHex, apiBase);
  const newServerEpoch = current.epoch + 1;
  if (!Number.isInteger(newServerEpoch) || newServerEpoch < 0) {
    throw new Error(`unexpected server epoch ${current.epoch}; cannot reseal`);
  }
  const subs = await listSubscribers(channelIdHex, identity, apiBase);
  const bytesByHex = new Map<string, Uint8Array>();
  for (const subscriberHex of subs.subscribers) {
    const subscriberBytes = hexToBytes(subscriberHex);
    if (subscriberBytes === null) {
      throw new Error(`subscriber pubkey hex is not valid: ${subscriberHex}`);
    }
    bytesByHex.set(subscriberHex, subscriberBytes);
  }
  // ADR-0060: a subscriber key registered before the Ed25519 flag day may be
  // one the core refuses to seal to; that subscriber could not open the
  // envelope anyway, so it is skipped rather than aborting the reseal (the
  // same rule as the rotation cron).
  const { sealed, skipped } = sealEachSkippingInvalid([...bytesByHex.keys()], (hex) =>
    wasm.sealSubscriberSeedEnvelope(
      bytesByHex.get(hex) ?? new Uint8Array(0),
      channelIdBytes,
      newServerEpoch,
      newSeed,
    ),
  );
  if (skipped.length > 0) {
    // eslint-disable-next-line no-console
    console.warn(`[channels] reseal skipped ${skipped.length} subscriber key(s) the core refused`);
  }
  const batch: RotateBatchEntry[] = sealed.map((s) => ({
    subscriber_pubkey_hex: s.subscriberHex,
    envelope_b64: base64Encode(s.value),
  }));
  await rotateChannel(channelIdHex, batch, identity, apiBase);
}

// ---------------------------------------------------------
// helpers (mirror rotation-cron.ts)
// ---------------------------------------------------------

/** Keep the `reply` grant of a join attribution read from this group's knock
 *  queue (ADR-0060 section 6). Anything that is not an attribution for this
 *  group is ignored. */
function rememberAttributionReply(wasm: Wasm, groupId: Uint8Array, payload: Uint8Array): void {
  const session = reachSession();
  if (session === null) return;
  let attr: import("../../wasm/lakko_ffi_wasm").GroupJoinAttribution;
  try {
    attr = wasm.GroupJoinAttribution.parse(payload);
  } catch {
    return;
  }
  try {
    if (base64Encode(attr.groupId) !== base64Encode(groupId)) return;
    const reply = attr.replyGrant;
    try {
      // ADR-0078 - the reply grant's mailbox binding must verify against the
      // joiner's account key, which for a co-publisher IS the key the
      // attribution is self-signed with. A grant naming someone else's
      // mailbox is not kept (fail closed).
      if (!joinReplyBindingVerifies(reply, attr.joinerSignatureKey)) return;
      rememberJoinReply(attr.joinerSignatureKey, session.encodeGrant(reply));
    } finally {
      reply.free();
    }
  } finally {
    attr.free();
  }
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

