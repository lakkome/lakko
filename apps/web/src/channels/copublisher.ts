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
 * The end-to-end correctness of the reseal under a real 2-publisher topology
 * (the co-publisher derives the same post-approve seed the subscribers were
 * resealed to) needs a live 2-publisher e2e against a deployed channels backend
 * — deferred here (no deployed environment), the same posture as every other
 * live-channel gate in the codebase.
 */

import type { Wasm } from "../wasm";
import { loadWasm } from "../wasm";
import { DsClient } from "../ds/client";
import {
  fetchEpoch,
  listSubscribers,
  rotateChannel,
  type RotateBatchEntry,
} from "./api";
import {
  base64UrlToBytes,
  bytesToHex,
  freshPublisherKeys,
  type ChannelPublisherKeys,
} from "./composer";

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
  return wasm.PublisherGroup.openOrCreate(identity, channelId);
}

/** Mint a co-publisher invite link over the PublisherGroup's MLS `group_id`. A
 *  non-member opens it, requests to join, and the owner approves (reusing the
 *  ADR-0027/0028 external-join machinery). Inline link — the channel owner
 *  shares it out-of-band; `0n` = never expires. */
export async function createCoPublisherLink(
  identity: Identity,
  publisherGroup: PublisherGroup,
  origin: string,
): Promise<string> {
  const wasm = await loadWasm();
  const groupId = publisherGroup.groupId();
  const wire = wasm.composeGroupInviteCapability(identity.ed25519Seed, groupId, 0n);
  return `${origin}${GROUP_INVITE_URL_PREFIX}${bytesToBase64Url(wire)}`;
}

/** Poll the PublisherGroup's DS journal and apply incoming MLS messages
 *  (external-join proposals from prospective co-publishers + any co-publisher
 *  commits) via the role-gated path. Surfaces external-join proposals into the
 *  PublisherGroup's retained pending set. Non-MLS entries (Welcome rendezvous)
 *  and our own already-applied commits throw on `processIncomingVerifyingRoles`
 *  and are skipped. Returns the number of pending requests after the poll. */
export async function pollCoPublisherJournal(
  ds: DsClient,
  identity: Identity,
  publisherGroup: PublisherGroup,
): Promise<number> {
  const groupId = publisherGroup.groupId();
  const raw = await ds.fetchGroup(groupId, { after: 0, waitSecs: 0 });
  for (const m of raw.messages) {
    try {
      publisherGroup.processIncomingVerifyingRoles(identity, m.payload);
    } catch {
      // Non-MLS entry (a Welcome rendezvous) or a commit we have already
      // merged (epoch mismatch) — skip, the FIFO journal lets the next poll
      // catch up on any genuinely-new commit.
    }
  }
  return pendingCoPublishers(publisherGroup).length;
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
 *  epoch), broadcasts the commit + a `WelcomeRendezvous` to the journal (the
 *  not-yet-member joiner polls for it), then RESEALS the new-epoch channel seed
 *  to all subscribers — the Add changed the exporter output, so existing
 *  subscribers must get the new seed or they break (see the module docs). */
export async function approveCoPublisher(
  ds: DsClient,
  identity: Identity,
  publisherGroup: PublisherGroup,
  proposalRef: Uint8Array,
  channelIdHex: string,
  apiBase?: string,
): Promise<void> {
  const wasm = await loadWasm();
  const groupId = publisherGroup.groupId();
  const added = publisherGroup.approveExternalJoin(identity, proposalRef);
  await ds.publishGroup(groupId, added.commit);
  const rendezvous = wasm.wrapWelcomeRendezvous(proposalRef, added.welcome);
  await ds.publishGroup(groupId, rendezvous);
  await resealSubscribers(publisherGroup, identity, channelIdHex, apiBase);
}

/** Reject a pending co-publisher — local drop, no commit, no epoch advance. */
export function rejectCoPublisher(
  publisherGroup: PublisherGroup,
  proposalRef: Uint8Array,
): void {
  publisherGroup.rejectExternalJoin(proposalRef);
}

// ---------------------------------------------------------
// Joiner side — a prospective co-publisher requests + joins
// ---------------------------------------------------------

/** ADR-0028 OQ3 — joiner side: request to become a co-publisher of a channel
 *  from a `/g/` co-publisher link. Decodes + verifies the capability, recovers
 *  the channel id from the PublisherGroup `group_id` (the link conveys only the
 *  `group_id`), reads the group's current epoch from its DS journal, crafts an
 *  external-join proposal (ADR-0027), and posts it + a self-signed
 *  join-attribution (OQ2) to the journal. Returns the handles the caller polls
 *  with via `pollForCoPublishWelcome`. */
export async function requestToCoPublish(
  ds: DsClient,
  identity: Identity,
  link: string,
): Promise<{ groupId: Uint8Array; channelIdHex: string; joinerKey: Uint8Array }> {
  const wasm = await loadWasm();
  const idx = link.lastIndexOf(GROUP_INVITE_URL_PREFIX);
  if (idx < 0) throw new Error("not a /g/ co-publisher link");
  const slug = link.slice(idx + GROUP_INVITE_URL_PREFIX.length);
  const wire = base64UrlToBytes(slug);
  const cap = wasm.GroupInviteCapability.decodeAndVerify(wire);
  let groupId: Uint8Array;
  try {
    groupId = cap.groupId;
  } finally {
    cap.free();
  }
  const channelIdBytes = wasm.channelIdFromPublisherGroupId(groupId);
  if (channelIdBytes === undefined) {
    throw new Error("co-publisher link does not target a channel PublisherGroup");
  }
  const channelIdHex = bytesToHex(channelIdBytes);
  // Read the current epoch from the PublisherGroup journal head (public framing;
  // a non-member can do this). Empty / unreachable journal => epoch 0.
  let epoch = 0n;
  try {
    const raw = await ds.fetchGroup(groupId, { after: 0, waitSecs: 0 });
    for (const m of raw.messages) {
      const e = wasm.epochOfMessage(m.payload);
      if (e !== undefined) epoch = e;
    }
  } catch {
    // keep epoch 0
  }
  const proposal = identity.createExternalJoinProposal(groupId, epoch);
  await ds.publishGroup(groupId, proposal);
  // Self-signed join attribution (OQ2) — advisory; best-effort.
  try {
    const attribution = wasm.composeGroupJoinAttribution(identity.ed25519Seed, wire);
    await ds.publishGroup(groupId, attribution);
  } catch {
    // advisory only — ignore
  }
  return { groupId, channelIdHex, joinerKey: identity.signaturePublicKey };
}

/** ADR-0028 OQ3 — joiner side: poll the PublisherGroup journal for the Welcome
 *  the owner posted after approving, join from it, and mint local publisher keys
 *  so this device can now publish to the channel. The joined group lands at the
 *  post-approve epoch — the same one the subscribers were resealed to — so the
 *  derived `channelSeed` matches. Returns the keys on success, or `null` if no
 *  Welcome has arrived yet (keep polling). */
export async function pollForCoPublishWelcome(
  ds: DsClient,
  identity: Identity,
  groupId: Uint8Array,
  joinerKey: Uint8Array,
  channelIdHex: string,
): Promise<ChannelPublisherKeys | null> {
  const wasm = await loadWasm();
  const raw = await ds.fetchGroup(groupId, { after: 0, waitSecs: 0 });
  for (const m of raw.messages) {
    const welcome = wasm.welcomeRendezvousFor(m.payload, joinerKey);
    if (welcome !== undefined) {
      const group = wasm.Group.joinFromWelcome(welcome, identity);
      group.free();
      // Now an MLS member of the PublisherGroup — `freshPublisherKeys` opens the
      // (now-joined) group, derives the shared content-key seed, and mints a
      // fresh per-device signing key so this device can publish.
      return await freshPublisherKeys(identity, channelIdHex);
    }
  }
  return null;
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
  const subs = await listSubscribers(channelIdHex, apiBase);
  const batch: RotateBatchEntry[] = [];
  for (const subscriberHex of subs.subscribers) {
    const subscriberBytes = hexToBytes(subscriberHex);
    if (subscriberBytes === null) {
      throw new Error(`subscriber pubkey hex is not valid: ${subscriberHex}`);
    }
    const envelope = wasm.sealSubscriberSeedEnvelope(
      subscriberBytes,
      channelIdBytes,
      newServerEpoch,
      newSeed,
    );
    batch.push({
      subscriber_pubkey_hex: subscriberHex,
      envelope_b64: base64Encode(envelope),
    });
  }
  await rotateChannel(channelIdHex, batch, apiBase);
}

// ---------------------------------------------------------
// helpers (mirror rotation-cron.ts)
// ---------------------------------------------------------

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

function base64Encode(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; ++i) {
    const b = bytes[i];
    if (b === undefined) continue;
    s += String.fromCharCode(b);
  }
  return btoa(s);
}

function bytesToBase64Url(bytes: Uint8Array): string {
  return base64Encode(bytes)
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}
