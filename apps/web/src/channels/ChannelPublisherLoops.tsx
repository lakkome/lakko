// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 section 12 — a channel works whether or not its owner opens it.
 * Renders nothing; mounted once in `App.tsx` (it replaced
 * `SubscriberAdmission`, and took over what the retired Manage screen's
 * panels ran while they were mounted).
 *
 * Every {@link PASS_INTERVAL_MS}, for each channel this device publishes to
 * (and only those):
 *
 * - an owed type change (UX 4.1) is run again until it lands;
 * - the rotation policy is checked (at most every {@link ROTATION_CHECK_MS})
 *   and a due rotation runs over the one shared PublisherGroup handle;
 * - a public channel's seed of the current epoch is filed (idempotent);
 * - new subscribers are let in (sealed tickets on a private channel);
 * - the owner reads the co-publisher knocks and approves a request that came
 *   by a publisher link of this account (minted here, or on another device
 *   and kept in the link register; UX 15.2), applies publishers' step-downs
 *   and sends each publisher the read key and the admission seeds; every
 *   publisher reads what the owner sent on the PublisherGroup journal;
 * - the publisher list is read from the PublisherGroup, and a publisher whose
 *   leaf was removed (by the owner, or after stepping down) ends the channel
 *   here.
 *
 * And for each channel this device waits to be let into (private), a
 * catch-up, so the channel opens by itself once a publisher's device ran.
 *
 * Unlike the post sync it does not stop in a hidden tab: someone may be
 * waiting to be let in. A pass is skipped while the previous one runs.
 */
import { createEffect, onCleanup, onMount, type Component } from "solid-js";

import { useChats } from "../chat/ChatContext";
import { useMembershipEnd } from "../chat/MembershipEndContext";
import { base64Encode } from "../auth/account-id";
import { DsClient } from "../ds/client";
import { loadWasm } from "../wasm";
import { channelsActions, channelsState } from "../state/channels-state";
import { admitNewSubscribers } from "./subscriber-admission";
import { runPublisherRemoval, runRotationOnce, type RotationContext } from "./rotation-cron";
import { loadRotationState, saveRotationState, type ChannelRotationState } from "./rotation-state-store";
import { republishChannelMeta } from "./channel-meta";
import {
  accountIdOfJoiner,
  approveCoPublisher,
  boundCoPublisherKey,
  installOwnProfileEnvelopeSource,
  capabilityOfJoiner,
  pendingCoPublishers,
  pollCoPublisherJournal,
  rejectCoPublisher,
} from "./copublisher";
import {
  releaseAllPublisherGroups,
  releasePublisherGroup,
  sharedPublisherGroup,
  withPublisherGroup,
} from "./publisher-group-registry";
import {
  catchUpChannel,
  ensurePublicSeed,
  installChannelRemovedHandler,
  installChannelRotateNow,
  installChannelStop,
  noteChannelRotated,
  runOwedChannelType,
} from "./channel-access";
import {
  afterPublisherRemoved,
  forgetPublisherGroupMembers,
  installPublisherRequestHandler,
  notePendingPublisherRequests,
  pendingPublisherRequests,
  installPublisherRemover,
  installSelfPublisherKey,
  notePublisherGroupMembers,
} from "./channel-publishers";
import { registeredLinks, revokeRegisteredLink } from "../devices/sync/link-register";
import { hexOf as linkHexOf, isLinkLive, type LinkRecordModel } from "../devices/sync/link-records";
import type { WasmIdentity } from "./composer";
import { b64ToBytes, bytesToB64 } from "../profile/avatar-model";
import { versionsToSend } from "./channel-access-record";
import {
  bindingVerdict,
  matchPublisherLink,
  persistThenPublish,
  removedPublisherFallback,
  withAppointedKey,
} from "./publisher-app-messages";
import { DiscoveryClient } from "../discovery/client";
import { sessionTokens } from "../auth/session-token-provider";
import { useProfiles } from "../profile/ProfileContext";

/** Between passes (chosen). Each pass touches the services once per
 *  channel this device publishes, so it is slow: a private channel's new
 *  subscriber waits at most this long for a publisher device to let it in,
 *  and a public channel needs no pass to be read at all. Was 15 s. */
export const PASS_INTERVAL_MS = 60_000;
/** Between rotation-policy checks of one channel (the retired cron's 5 min). */
export const ROTATION_CHECK_MS = 5 * 60 * 1000;

type Wasm = Awaited<ReturnType<typeof loadWasm>>;

/** Load the persisted rotation state, or seed "just rotated" on first sight
 *  so the first check does not rotate at once (the retired panel's rule). */
function loadOrSeedState(wasm: Wasm, idHex: string): ChannelRotationState {
  const existing = loadRotationState(idHex);
  if (existing !== null) return existing;
  const seeded = wasm.channelRotationStateAfterRotation(BigInt(Date.now()));
  saveRotationState(idHex, seeded);
  return seeded;
}

/**
 * Rotate one channel over the shared handle. `force` runs a rotation now
 * (removing a subscriber, going private); otherwise only when the policy says
 * so. After it: the publisher keys take the new seed, the per-epoch map keeps
 * it, the provider is flushed, and the metadata record is re-issued under the
 * new epoch so a late subscriber still finds the name.
 */
/** The rotation context of one channel over its shared handle; `onDone`
 *  runs after the new seed is in place. */
function rotationContext(
  identity: WasmIdentity,
  idHex: string,
  group: RotationContext["publisherGroup"],
  onDone: () => void,
): RotationContext {
  return {
    publisherGroup: group,
    identity,
    channelIdHex: idHex,
    saveState: (next) => saveRotationState(idHex, next),
    onRotated: (newEpoch) => {
      try {
        const prev = channelsState.keysByChannel().get(idHex);
        const seed = group.channelSeed(identity);
        if (prev !== undefined) channelsActions.setKeys(idHex, { seed, signingKeySeed: prev.signingKeySeed });
        noteChannelRotated(idHex, newEpoch, seed);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("[channels] post-rotation seed refresh failed", err instanceof Error ? err.name : "");
      }
      channelsActions.notePublisherGroupChanged();
      onDone();
    },
  };
}

export async function rotateChannel(
  identity: WasmIdentity,
  idHex: string,
  force: boolean,
  onProgress?: (done: number, total: number) => void,
): Promise<void> {
  const wasm = await loadWasm();
  let rotated = false;
  await withPublisherGroup(identity, idHex, async (group) => {
    const ctx = rotationContext(identity, idHex, group, () => {
      rotated = true;
    });
    const policy = wasm.channelRotationPolicyDefault();
    const state = force ? wasm.channelRotationStateNew() : loadOrSeedState(wasm, idHex);
    if (onProgress !== undefined) onProgress(0, 0);
    await runRotationOnce(ctx, policy, state);
  });
  if (rotated) {
    await republishChannelMeta(idHex, identity).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("[channels] post-rotation metadata re-issue failed", err instanceof Error ? err.name : "");
    });
  }
}

/** Runtime shape of a `PublisherGroup.members()` element. */
interface WasmGroupMember {
  readonly leafIndex: number;
  readonly accountKey: Uint8Array;
  free?: () => void;
}

/** The account keys of a PublisherGroup's members (call under its lock),
 *  the owner (the founder's lowest leaf) first. */
function memberAccounts(group: RotationContext["publisherGroup"]): string[] {
  const raw = group.members() as WasmGroupMember[];
  const sorted = [...raw].sort((a, b) => a.leafIndex - b.leafIndex);
  const out: string[] = [];
  for (const m of sorted) {
    const k = base64Encode(m.accountKey);
    if (!out.includes(k)) out.push(k);
    m.free?.();
  }
  return out;
}

/**
 * ADR-0087 section 15 (phase B) — the owner removes one account from the
 * PublisherGroup: every leaf in one commit, sent through the journal's commit
 * route and resealed to the subscribers (`runPublisherRemoval`). An account
 * that holds no leaf any more is a no-op, so a step-down applied twice does
 * nothing the second time. The owner's own key is refused by the core.
 */
export async function removePublisherNow(identity: WasmIdentity, idHex: string, accountKey: Uint8Array): Promise<void> {
  const keyB64 = base64Encode(accountKey);
  let removed = false;
  await withPublisherGroup(identity, idHex, async (group) => {
    if (!group.isOwnedBy(identity)) throw new Error("not the owner");
    if (!memberAccounts(group).includes(keyB64)) {
      notePublisherGroupMembers(idHex, group);
      return;
    }
    await runPublisherRemoval(
      rotationContext(identity, idHex, group, () => {
        removed = true;
      }),
      accountKey,
    );
    notePublisherGroupMembers(idHex, group);
  });
  if (removed) {
    await republishChannelMeta(idHex, identity).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("[channels] post-removal metadata re-issue failed", err instanceof Error ? err.name : "");
    });
  }
}

/** Base64url without padding (the local link records' ledger id form). */
function b64UrlOf(bytes: Uint8Array): string {
  return bytesToB64(bytes).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** The account's publisher link a request came by (`matchPublisherLink`):
 *  only a live one-use link, recorded here or in the link register. */
async function publisherLinkOf(
  idHex: string,
  cap: string,
): Promise<{ readonly localLedger: string | null; readonly record: LinkRecordModel | null } | null> {
  const records = registeredLinks().filter((r) => r.kind === "publisher" && linkHexOf(r.targetId) === idHex);
  const wasm = await loadWasm();
  const now = Date.now();
  const opened = records.map((r) => {
    let capabilityB64: string | null = null;
    if (r.capability.length > 0) {
      try {
        const c = wasm.GroupInviteCapability.openLink(r.capability, r.linkSecret);
        try {
          capabilityB64 = base64Encode(c.wire);
        } finally {
          c.free();
        }
      } catch {
        capabilityB64 = null;
      }
    }
    return {
      ledgerIdB64Url: b64UrlOf(r.ledgerId),
      maxUses: r.maxUses,
      revoked: r.revoked,
      live: isLinkLive(r, now),
      capabilityB64,
    };
  });
  const match = matchPublisherLink(cap, channelsActions.accessOf(idHex).inviteLinks, opened);
  if (match === null) return null;
  return { localLedger: match.localLedger, record: match.recordIndex >= 0 ? (records[match.recordIndex] ?? null) : null };
}

/** Approve one co-publisher request and record the new publisher; `link`
 *  is the one-use link it came by (marked used here and revoked in the
 *  register), or `null` for a request a person approved by hand. */
async function approveAndRecord(
  identity: WasmIdentity,
  idHex: string,
  joiner: Uint8Array,
  link: { readonly localLedger: string | null; readonly record: LinkRecordModel | null } | null,
): Promise<void> {
  const ds = new DsClient();
  const group = await sharedPublisherGroup(identity, idHex);
  const groupId = group.groupId();
  await approveCoPublisher(ds, identity, group, joiner, idHex);
  // A publisher link is for one person: once used it is revoked on every
  // device of the account, and its ledger row goes.
  if (link?.record != null) {
    await revokeRegisteredLink(link.record).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("[channels] used publisher link not revoked in the register", err instanceof Error ? err.name : "");
    });
  }
  const accountKeyB64 = base64Encode(joiner);
  const bound = boundCoPublisherKey(groupId, joiner);
  const accountId = accountIdOfJoiner(groupId, joiner);
  const usedLedger = link?.localLedger ?? null;
  await channelsActions.updateAccessDurable(idHex, (r) => ({
    ...r,
    inviteLinks: r.inviteLinks.map((l) => (usedLedger !== null && l.ledgerIdB64Url === usedLedger ? { ...l, used: true } : l)),
    publishers: r.publishers.includes(accountKeyB64) ? r.publishers : [...r.publishers, accountKeyB64],
    publisherKeys: r.publisherKeys.some((p) => p.accountKeyB64 === accountKeyB64)
      ? r.publisherKeys
      : [
          ...r.publisherKeys,
          {
            accountKeyB64,
            copublisherKeyB64: bound === undefined ? "" : bytesToB64(bound),
            accountIdHex: accountId === undefined ? "" : hexOf(accountId),
            sentVersions: [],
          },
        ],
  }));
  notePendingPublisherRequests(
    idHex,
    pendingPublisherRequests(idHex).filter((k) => k !== accountKeyB64),
  );
}

/** The owner's co-publisher step: read knocks, approve a request that came
 *  by a publisher link this device minted and did not use yet. */
async function ownerKnocks(
  identity: WasmIdentity,
  idHex: string,
  onApplication: (b: Uint8Array, sender: Uint8Array | undefined) => Promise<void>,
): Promise<void> {
  const ds = new DsClient();
  const group = await sharedPublisherGroup(identity, idHex);
  await pollCoPublisherJournal(ds, identity, group, onApplication);
  const groupId = group.groupId();
  const unmatched: string[] = [];
  for (const joiner of pendingCoPublishers(group)) {
    const cap = capabilityOfJoiner(groupId, joiner);
    const link = cap === undefined ? null : await publisherLinkOf(idHex, cap);
    if (link === null) {
      // Came by no live one-use publisher link of this account: waits for
      // the owner's Approve on the Publishers screen.
      unmatched.push(base64Encode(joiner));
      continue;
    }
    await approveAndRecord(identity, idHex, joiner, link);
  }
  notePendingPublisherRequests(idHex, unmatched);
  // A binding read after the approve fills the publisher's appointed key.
  const missing = channelsActions.accessOf(idHex).publisherKeys.filter((p) => p.copublisherKeyB64 === "");
  for (const p of missing) {
    const bound = boundCoPublisherKey(groupId, b64ToBytes(p.accountKeyB64));
    if (bound === undefined) continue;
    await channelsActions.updateAccessDurable(idHex, (r) => ({
      ...r,
      publisherKeys: r.publisherKeys.map((x) =>
        x.accountKeyB64 === p.accountKeyB64 ? { ...x, copublisherKeyB64: bytesToB64(bound) } : x,
      ),
    }));
  }
}

function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

function channelIdBytes(idHex: string): Uint8Array {
  return hexToBytes(idHex);
}

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/**
 * ADR-0087 section 7 — the owner hands each publisher every admission-seed
 * version it lacks, sealed to that person's ACCOUNT X25519 key, as an
 * application message on the PublisherGroup journal (so any publisher's
 * device can check tickets and let people in). The key comes from the
 * person's identity bundle in the directory, fetched by the account id
 * their request named and used only when the bundle's signature key IS the
 * publisher's account key (the same check the registrar and knock seeds
 * make). A version sent is recorded durably; a failure is retried next pass.
 */
async function sendAdmissionSeeds(wasm: Wasm, identity: WasmIdentity, idHex: string): Promise<void> {
  const record = channelsActions.accessOf(idHex);
  if (record.admissionSeeds.length === 0) return;
  const discovery = new DiscoveryClient(undefined, { sessionToken: () => sessionTokens.peek() });
  for (const p of record.publisherKeys) {
    const versions = versionsToSend(channelsActions.accessOf(idHex), p);
    if (versions.length === 0 || p.accountIdHex === "") continue;
    const bundle = await discovery.identityFetch(hexToBytes(p.accountIdHex)).catch(() => null);
    if (bundle === null || bundle.signature_pubkey !== p.accountKeyB64) continue;
    const x = b64ToBytes(bundle.x25519_pubkey);
    if (x.length !== 32) continue;
    const ds = new DsClient();
    await withPublisherGroup(identity, idHex, async (group) => {
      for (const v of versions) {
        const entry = channelsActions.accessOf(idHex).admissionSeeds.find((s) => s.version === v);
        if (entry === undefined) continue;
        const seed = b64ToBytes(entry.seedB64);
        let message: Uint8Array;
        try {
          const envelope = wasm.composeAdmissionSeedEnvelope(
            group.channelId(),
            b64ToBytes(p.accountKeyB64),
            x,
            v,
            seed,
          );
          message = group.createApplicationMessage(identity, envelope);
        } finally {
          seed.fill(0);
        }
        // The sending ratchet moved: record it before the message leaves (a
        // crash in between would reuse the generation, G-34 class).
        await persistThenPublish(
          () => channelsActions.persistPublisherGroupDurable(),
          () => ds.publishGroup(group.groupId(), message).then(() => undefined),
        );
        await channelsActions.updateAccessDurable(idHex, (r) => ({
          ...r,
          publisherKeys: r.publisherKeys.map((x2) =>
            x2.accountKeyB64 === p.accountKeyB64 && !x2.sentVersions.includes(v)
              ? { ...x2, sentVersions: [...x2.sentVersions, v] }
              : x2,
          ),
        }));
      }
    });
  }
}

/**
 * ADR-0087 phase B — the owner sends each publisher the channel's read key
 * (it never changes, so once), sealed to the person's ACCOUNT X25519 key
 * the same way as the admission seeds: the key from the person's identity
 * bundle, used only when that bundle's signature key IS the publisher's
 * account key. A publisher must hold it before the channel goes public.
 * Recorded durably; a failure is retried next pass.
 */
async function sendReadKeys(wasm: Wasm, identity: WasmIdentity, idHex: string): Promise<void> {
  const pending = channelsActions.accessOf(idHex).publisherKeys.filter((p) => p.readKeySent !== true && p.accountIdHex !== "");
  if (pending.length === 0) return;
  const discovery = new DiscoveryClient(undefined, { sessionToken: () => sessionTokens.peek() });
  for (const p of pending) {
    const bundle = await discovery.identityFetch(hexToBytes(p.accountIdHex)).catch(() => null);
    if (bundle === null || bundle.signature_pubkey !== p.accountKeyB64) continue;
    const x = b64ToBytes(bundle.x25519_pubkey);
    if (x.length !== 32) continue;
    const ds = new DsClient();
    await withPublisherGroup(identity, idHex, async (group) => {
      const channelId = group.channelId();
      const readKey = wasm.channelReadKey(identity, channelId);
      let message: Uint8Array;
      try {
        const envelope = wasm.composeChannelReadKeyEnvelope(channelId, b64ToBytes(p.accountKeyB64), x, readKey);
        message = group.createApplicationMessage(identity, envelope);
      } finally {
        readKey.fill(0);
      }
      // Persist before publishing, as for the admission seeds.
      await persistThenPublish(
        () => channelsActions.persistPublisherGroupDurable(),
        () => ds.publishGroup(group.groupId(), message).then(() => undefined),
      );
    });
    await channelsActions.updateAccessDurable(idHex, (r) => ({
      ...r,
      publisherKeys: r.publisherKeys.map((x2) => (x2.accountKeyB64 === p.accountKeyB64 ? { ...x2, readKeySent: true } : x2)),
    }));
  }
}

export const ChannelPublisherLoops: Component = () => {
  const chats = useChats();
  const membershipEnd = useMembershipEnd();
  const profiles = useProfiles();
  let running = false;
  const lastRotationCheck = new Map<string, number>();
  const publicSeedFiled = new Map<string, number>();
  /** Channels deleted this session: no pass touches them again. */
  const stopped = new Set<string>();

  // A sealed co-publisher request carries this account's profile.
  installOwnProfileEnvelopeSource(() => profiles.ownProfileEnvelope());
  onCleanup(() => installOwnProfileEnvelopeSource(null));

  // `deleteChannel` stops a channel: its handle goes, no pass runs for it.
  installChannelStop((idHex) => {
    stopped.add(idHex);
    releasePublisherGroup(idHex);
  });
  onCleanup(() => installChannelStop(null));

  // The owner's Remove publisher (UX 15.2) runs over the shared handle.
  installPublisherRemover((identity, idHex, accountKey) => removePublisherNow(identity, idHex, accountKey));
  onCleanup(() => installPublisherRemover(null));

  // A request that came by no one-use link: the owner approves or rejects it
  // by hand on the Publishers screen.
  installPublisherRequestHandler(async (identity, idHex, joinerKeyB64, approve) => {
    const joiner = b64ToBytes(joinerKeyB64);
    if (approve) {
      await approveAndRecord(identity, idHex, joiner, null);
      return;
    }
    await withPublisherGroup(identity, idHex, async (group) => {
      rejectCoPublisher(identity, group, joiner);
    });
    await channelsActions.persistPublisherGroupDurable();
    notePendingPublisherRequests(
      idHex,
      pendingPublisherRequests(idHex).filter((k) => k !== joinerKeyB64),
    );
  });
  onCleanup(() => installPublisherRequestHandler(null));

  // A publisher removed this device's key from a private channel (UX 9.5).
  installChannelRemovedHandler(async (idHex) => {
    const name = channelsState.metaByChannel().get(idHex)?.name ?? "";
    await membershipEnd.endChannel(idHex, name, true);
  });
  onCleanup(() => installChannelRemovedHandler(null));

  // Rotation on demand (remove a subscriber, going private) over the one
  // shared handle.
  installChannelRotateNow(async (idHex, onProgress) => {
    const identity = chats.conversationAccountIdentity();
    if (identity === null) throw new Error("signed out");
    await rotateChannel(identity, idHex, true, onProgress);
    lastRotationCheck.set(idHex, Date.now());
  });
  onCleanup(() => installChannelRotateNow(null));

  createEffect(() => {
    const identity = chats.conversationAccountIdentity();
    installSelfPublisherKey(identity === null ? null : base64Encode(identity.signaturePublicKey));
    if (identity === null) releaseAllPublisherGroups();
  });
  onCleanup(() => {
    installSelfPublisherKey(null);
    releaseAllPublisherGroups();
  });

  /** The epoch this device last sent its own binding at, per channel (in
   *  memory: after a reload it is sent once more, which files the same key). */
  const bindingSentAt = new Map<string, string>();

  /** ADR-0059 — a publisher sends its signed binding (its appointed key, under
   *  its account key) into the PublisherGroup once per epoch, so every owner
   *  device can file it. Persisted before it is published (G-34 class). */
  async function sendOwnBinding(wasm: Wasm, identity: WasmIdentity, idHex: string): Promise<void> {
    await withPublisherGroup(identity, idHex, async (group) => {
      if (!group.isActive() || group.isOwnedBy(identity)) return;
      const epoch = group.epoch().toString();
      if (bindingSentAt.get(idHex) === epoch) return;
      const binding = wasm.composeChannelCoPublisherBinding(identity, group.channelId());
      const message = group.createApplicationMessage(identity, binding);
      await persistThenPublish(
        () => channelsActions.persistPublisherGroupDurable(),
        () => new DsClient().publishGroup(group.groupId(), message).then(() => undefined),
      );
      bindingSentAt.set(idHex, epoch);
    });
  }

  /** Step-downs read this pass, applied after the journal poll (the apply
   *  runs inside the group's MLS lock, which a removal takes again). */
  const stepDowns = new Map<string, Set<string>>();

  async function onApplication(
    wasm: Wasm,
    identities: readonly WasmIdentity[],
    idHex: string,
    plaintext: Uint8Array,
    sender: Uint8Array | undefined,
    ownerKeyB64: string | null,
    isOwner: boolean,
  ): Promise<void> {
    // ADR-0087 section 15: a publisher steps down; the owner's device
    // removes the account the MLS sender resolved to at receipt (C-68).
    if (wasm.isPublisherStepDownEnvelope(plaintext)) {
      if (!isOwner || sender === undefined) return;
      if (!wasm.isPublisherStepDown(plaintext, channelIdBytes(idHex))) return;
      const key = base64Encode(sender);
      if (key === ownerKeyB64) return;
      const set = stepDowns.get(idHex) ?? new Set<string>();
      set.add(key);
      stepDowns.set(idHex, set);
      return;
    }
    // ADR-0059: a publisher's signed binding of its appointed key, sent into
    // the PublisherGroup every epoch so any owner device can revoke the
    // appointment later. Filed only when the binding's account IS the MLS
    // sender resolved at receipt, the channel matches and the sender is not
    // the owner.
    let binding: ReturnType<Wasm["decodeChannelCoPublisherBinding"]> | null = null;
    try {
      binding = wasm.decodeChannelCoPublisherBinding(plaintext);
    } catch {
      binding = null;
    }
    if (binding !== null) {
      try {
        const verdict = bindingVerdict({
          isOwner,
          senderB64: sender === undefined ? undefined : base64Encode(sender),
          ownerKeyB64,
          bindingAccountB64: base64Encode(binding.accountPubkey),
          bindingChannelHex: hexOf(binding.channelId),
          channelIdHex: idHex,
        });
        if (verdict !== "file" || sender === undefined) return;
        const senderB64 = base64Encode(sender);
        const appointedB64 = bytesToB64(binding.copublisherPubkey);
        await channelsActions.updateAccessDurable(idHex, (r) => {
          const next = withAppointedKey(r.publisherKeys, senderB64, appointedB64);
          return next === r.publisherKeys ? r : { ...r, publisherKeys: next };
        });
      } finally {
        binding.free();
      }
      return;
    }
    // ADR-0087 phase B: the channel's read key, kept only when it is
    // addressed to this account and the sender is the owner.
    let readKeyEnvelope = false;
    try {
      const env = wasm.decodeChannelReadKeyEnvelope(plaintext);
      env.free();
      readKeyEnvelope = true;
    } catch {
      /* not a read-key envelope */
    }
    if (readKeyEnvelope) {
      if (isOwner || sender === undefined || ownerKeyB64 === null || base64Encode(sender) !== ownerKeyB64) return;
      let readKey: Uint8Array | null = null;
      for (const identity of identities) {
        try {
          readKey = wasm.openChannelReadKeyEnvelope(plaintext, identity);
          break;
        } catch {
          /* addressed to another key of this account, or not to it */
        }
      }
      if (readKey === null) return;
      const readKeyB64 = bytesToB64(readKey);
      readKey.fill(0);
      await channelsActions.updateAccessDurable(idHex, (r) => (r.readKeyB64 === readKeyB64 ? r : { ...r, readKeyB64 }));
      return;
    }
    // ADR-0087 section 7: an admission seed the owner sent this account.
    let version: number;
    try {
      const env = wasm.decodeAdmissionSeedEnvelope(plaintext);
      try {
        version = env.admissionVersion;
      } finally {
        env.free();
      }
    } catch {
      return;
    }
    // Sealed to the account key the directory lists: the current identity
    // after a rotation, the lineage one before.
    let seed: Uint8Array | null = null;
    for (const identity of identities) {
      try {
        seed = wasm.openAdmissionSeedEnvelope(plaintext, identity);
        break;
      } catch {
        /* addressed to another key of this account, or not to it */
      }
    }
    if (seed === null) return;
    const seedB64 = bytesToB64(seed);
    seed.fill(0);
    await channelsActions.updateAccessDurable(idHex, (r) =>
      r.admissionSeeds.some((s) => s.version === version)
        ? r
        : {
            ...r,
            // Only a private channel has admission seeds.
            visibility: r.visibility ?? "private",
            admissionSeeds: [...r.admissionSeeds, { version, seedB64 }],
          },
    );
  }

  async function pass(): Promise<void> {
    const lineage = chats.conversationAccountIdentity();
    const current = chats.accountIdentity();
    if (lineage === null || current === null || !channelsState.hydrated()) return;
    const wasm = await loadWasm();
    const orphaned = channelsState.orphanedByKeyReset();
    for (const idHex of [...channelsState.keysByChannel().keys()]) {
      if (orphaned.has(idHex) || stopped.has(idHex)) continue;
      if (chats.conversationAccountIdentity() !== lineage) return;
      const owner = channelsActions.coPublisherGroupIdOf(idHex) === undefined;
      try {
        if (owner && channelsActions.accessOf(idHex).owedType !== null) {
          await runOwedChannelType(lineage, current, idHex).catch(() => undefined);
        }
        const now = Date.now();
        if (now - (lastRotationCheck.get(idHex) ?? 0) >= ROTATION_CHECK_MS) {
          lastRotationCheck.set(idHex, now);
          await rotateChannel(lineage, idHex, false);
        }
        const epoch = channelsState.epochByChannel().get(idHex);
        if (epoch !== undefined && publicSeedFiled.get(idHex) !== epoch) {
          await ensurePublicSeed(lineage, idHex);
          publicSeedFiled.set(idHex, epoch);
        }
        await admitNewSubscribers(idHex, lineage);
        // The owner's account key: the PublisherGroup's lowest leaf.
        const ownerKeyB64 = await withPublisherGroup(lineage, idHex, async (g) => {
          notePublisherGroupMembers(idHex, g);
          return memberAccounts(g)[0] ?? null;
        });
        const app = (b: Uint8Array, sender: Uint8Array | undefined): Promise<void> =>
          onApplication(wasm, current === lineage ? [lineage] : [current, lineage], idHex, b, sender, ownerKeyB64, owner);
        if (owner) {
          await ownerKnocks(lineage, idHex, app);
          for (const key of stepDowns.get(idHex) ?? []) {
            await removePublisherNow(lineage, idHex, b64ToBytes(key));
            await afterPublisherRemoved(lineage, idHex, key);
            stepDowns.get(idHex)?.delete(key);
          }
          await sendReadKeys(wasm, lineage, idHex);
          await sendAdmissionSeeds(wasm, lineage, idHex);
        } else {
          await pollCoPublisherJournal(new DsClient(), lineage, await sharedPublisherGroup(lineage, idHex), app);
          await sendOwnBinding(wasm, lineage, idHex);
          // Removed by the owner (or after stepping down): this device can
          // no longer post or read past the removal, so the channel ends here.
          const active = await withPublisherGroup(lineage, idHex, async (g) => {
            notePublisherGroupMembers(idHex, g);
            return g.isActive();
          });
          if (!active) {
            stopped.add(idHex);
            forgetPublisherGroupMembers(idHex);
            releasePublisherGroup(idHex);
            if (removedPublisherFallback(channelsState.subscriberKeysByChannel().has(idHex)) === "subscriber") {
              // UX 15.2: they stay subscribed if they were.
              channelsActions.dropPublisherRole(idHex);
              // No publisher pass runs for it now (its keys are gone); a later
              // invitation that makes them a publisher again starts passes anew.
              stopped.delete(idHex);
              await catchUpChannel(idHex).catch(() => undefined);
            } else {
              const name = channelsState.metaByChannel().get(idHex)?.name ?? "";
              await membershipEnd.endChannel(idHex, name, true);
            }
            continue;
          }
        }
        await withPublisherGroup(lineage, idHex, async (g) => {
          notePublisherGroupMembers(idHex, g);
        });
      } catch (err) {
        // A channel the services do not answer for this time.
        // eslint-disable-next-line no-console
        console.warn("[channels] publisher pass failed", err instanceof Error ? err.name : "");
      }
    }
    for (const [idHex, record] of channelsState.accessByChannel()) {
      if (!record.waiting || channelsState.keysByChannel().has(idHex)) continue;
      await catchUpChannel(idHex).catch(() => undefined);
    }
  }

  const run = (): void => {
    if (running) return;
    running = true;
    void pass().finally(() => {
      running = false;
    });
  };

  createEffect(() => {
    if (chats.conversationAccountIdentity() !== null && channelsState.hydrated()) run();
  });

  onMount(() => {
    const timer = setInterval(run, PASS_INTERVAL_MS);
    onCleanup(() => clearInterval(timer));
  });

  return null;
};
