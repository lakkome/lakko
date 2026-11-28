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
import { sendPublisherReaderKey, takePublisherReaderKey } from "./publisher-reader-keys";
import { accountSuspended } from "../auth/suspension-state";
import { createEffect, onCleanup, onMount, type Component } from "solid-js";

import { useChats } from "../chat/ChatContext";
import { useMembershipEnd } from "../chat/MembershipEndContext";
import { base64Encode } from "../auth/account-id";
import { DsClient, DsHttpError } from "../ds/client";
import { loadWasm } from "../wasm";
import { channelsActions, channelsState } from "../state/channels-state";
import { noteChannelIntent } from "./channel-subscription-register";
import {
  accountReaderKeyRefused,
  clearAccountReaderKeyHold,
  ensureAccountReaderKey,
  resetAccountReaderKeyHolds,
} from "./account-reader-key";
import { repairChannelOwnerIfRefused, resetChannelOwnerRepairs } from "./channel-owner-repair";
import { admitNewSubscribers } from "./subscriber-admission";
import { runPublisherRemoval, runRotationOnce, type RotationContext } from "./rotation-cron";
import { ensureEntryOwnerSource, enterOwnedChannelFromSaved, finishEntry } from "./channel-entry";
import { installOwnedChannelVaults, writeCurrentHead } from "./channel-vault-head";
import {
  loadRotationState,
  noteEpochStart,
  rebuiltRotationState,
  saveRotationState,
  type ChannelRotationState,
} from "./rotation-state-store";
import { oweRecordIfContradicted, republishChannelMeta } from "./channel-meta";
import { settleOwedRecord } from "./owed-record";
import { epochSeedsToAdd, epochSeedsToSend, keepsEpochSeeds, takesRotateRequest } from "./publisher-handover";
import {
  accountIdOfJoiner,
  accountKeyOfJoiner,
  approveCoPublisher,
  boundCoPublisherKey,
  installOwnProfileEnvelopeSource,
  capabilityOfJoiner,
  pendingCoPublishers,
  pollCoPublisherJournal,
  rejectCoPublisher,
  replayPublisherCommits,
} from "./copublisher";
import {
  releaseAllPublisherGroups,
  releasePublisherGroup,
  sharedPublisherDevice,
  sharedPublisherGroup,
  withPublisherGroup,
} from "./publisher-group-registry";
import type { PublisherGroupHandle } from "./publisher-group-handle";
import {
  applyQueuedDeviceRequests,
  inactivePublisherGroupVerdict,
  retryHeldChannelWelcomes,
  mayActOnResetSignal,
  suspectForeignChain,
  hexOf as devHexOf,
  reconcileOwnDevices,
  afterPeerCommitsApplied,
  newOwnLeaves,
  publisherLeaves,
  type PublisherLeaf,
  refoundVerdict,
  returningDeviceVerdict,
  sendAdmissionMark,
  sendOwnDeviceRequests,
  settleReaderDevices,
  liftClearedResetSuspicions,
  takeAdmissionMark,
  takeDeviceRequest,
} from "./publisher-devices";
import { syncAdmissionSeeds } from "./channel-admission-seed-register";
import { clearFoundedAlone, foundedAloneDone, isFoundedAlone } from "./founded-alone-marker";
import { channelSubscriptionPort } from "./channel-subscription-register";
import { boundContactsAccount } from "../contacts/local-store";
import {
  installChannelReturnRunner,
  installHeldProfileSource,
  runHeldChannelReturns,
  sendChannelReturns,
  settleRecoveredChannels,
} from "./channel-return-runtime";
import { dropOwnedChannelsSession, runOwnedChannelsPass } from "./owned/owned-runtime";
import { installOwnedChannelsWake } from "./owned/owned-wake";
import { offerReturnedOwnProfile } from "../profile/returned-profile";
import { base64Encode as b64 } from "../auth/account-id";
import { ownDeviceRoster } from "../devices/own-device-roster";
import { isStatus } from "../ds/ownership";
import { resealToSubscribers } from "./rotation-cron";
import {
  hasOwedPublisherCommit,
  publisherCommitFailureLabel,
  publisherCommitRetry,
  PublisherCommitRetry,
} from "./publisher-commit-outbox";

/** The holds of the subscriber admission pass (2026-11-20), apart from the
 *  recorded commits' so a transient failure here never delays those. */
const subscriberAdmissionRetry = new PublisherCommitRetry();
import { fetchEpoch, ownerProofIdentity } from "./api";
import {
  catchUpChannel,
  ensurePublicSeed,
  installChannelRemovedHandler,
  installChannelRotateNow,
  installChannelRotateRequest,
  settleOwedRotateRequest,
  installChannelStop,
  notePublisherCommitLanded,
  notePublisherSeedAt,
  anchorChannelEpoch,
  channelRotatesHere,
  channelReadKeyHere,
  channelSeedFor,
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
import {
  admissionSeedId,
  channelEpochOfGroup,
  reanchoredEpochOffset,
  seedsToSend,
  versionsToSend,
  type PublisherEntry,
} from "./channel-access-record";
import {
  bindingVerdict,
  matchPublisherLink,
  persistThenPublish,
  readKeyRecipients,
  removedPublisherFallback,
  resolvePublisherAccountIdHex,
  withAppointedKey,
  withResolvedAccountIds,
} from "./publisher-app-messages";
import { accountIdB64OfKey } from "../contacts/local-store";
import { DiscoveryClient } from "../discovery/client";
import { boundX25519With } from "../discovery/x25519-binding";
import { sessionTokens } from "../auth/session-token-provider";
import { useProfiles } from "../profile/ProfileContext";

/** Between passes (chosen). Each pass touches the services once per
 *  channel this device publishes, so it is slow: a private channel's new
 *  subscriber waits at most this long for a publisher device to let it in,
 *  and a public channel needs no pass to be read at all. Was 15 s. */
export const PASS_INTERVAL_MS = 60_000;

/** How long changes to the owned-channels list are gathered before one
 *  write (chosen, not measured). */
const OWNED_WAKE_DELAY_MS = 500;
/** Between rotation-policy checks of one channel (the retired cron's 5 min). */
export const ROTATION_CHECK_MS = 5 * 60 * 1000;
/** ADR-0090 section 4: how long an owner device lets the channels service
 *  trail its group before it reseals itself (chosen, not measured). */
export const RESEAL_TAKEOVER_MS = 5 * 60 * 1000;

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
  device: WasmIdentity,
  idHex: string,
  group: RotationContext["publisherGroup"],
  onDone: () => void,
): RotationContext {
  return {
    publisherGroup: group,
    identity,
    device,
    channelIdHex: idHex,
    saveState: (next) => saveRotationState(idHex, next),
    onRotated: (newEpoch) => {
      try {
        notePublisherCommitLanded(idHex, newEpoch, group.channelSeed(device));
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("[channels] post-rotation seed refresh failed", err instanceof Error ? err.name : "");
      }
      channelsActions.notePublisherGroupChanged();
      onDone();
    },
  };
}

/** ADR-0090 section 4: the wait before a due rotation is staged, random in
 *  [0, ROTATION_JITTER_MAX_MS) (chosen, not measured), so two owner devices
 *  that find it due at once rarely stage on the same epoch. */
export const ROTATION_JITTER_MAX_MS = 60_000;

/** The random wait of one due rotation. */
export function rotationJitterMs(random: () => number = Math.random): number {
  return Math.floor(Math.max(0, Math.min(0.999999, random())) * ROTATION_JITTER_MAX_MS);
}

/** Bring the shared handle up to the journal head (applying another owner
 *  device's commits), outside the group's lock. Never throws. */
async function readJournalHead(identity: WasmIdentity, idHex: string): Promise<void> {
  try {
    await pollCoPublisherJournal(new DsClient(), identity, await sharedPublisherGroup(identity, idHex));
  } catch {
    /* the next pass reads it */
  }
}

/**
 * Rotate one channel over the shared handle (ADR-0090 section 4, decision
 * C: any owner DEVICE rotates; the DS takes one commit per epoch). `force`
 * runs a rotation now (removing a subscriber, going private: the person's
 * own action, no wait); otherwise only when the policy says so, after a
 * random wait of up to {@link ROTATION_JITTER_MAX_MS} and a re-read of the
 * journal head, re-evaluated then. `requestedAtEpoch` (a co-publisher's
 * rotate request): waits like a due rotation and stands down when the group
 * moved past that epoch meanwhile (another owner device rotated). The
 * commit-route outcome is logged (open question 1).
 */
export async function rotateChannel(
  identity: WasmIdentity,
  idHex: string,
  force: boolean,
  onProgress?: (done: number, total: number) => void,
  requestedAtEpoch?: bigint,
): Promise<void> {
  // ADR-0099 step 3: a suspended account rotates nothing (every step of a
  // rotation writes to the channels service and the DS).
  if (accountSuspended()) return;
  // Only an owner device rotates the channel key; a co-publisher's never.
  if (!channelRotatesHere(idHex)) return;
  // 2026-11-20: no new rotation while a recorded commit of the channel still
  // owes its reseal (seen live on iOS: one more epoch merged every pass, each
  // reseal refused 401). That reseal, when it lands, seals a seed no
  // subscriber holds yet to the subscribers listed then, so a forced rotation
  // (a removed subscriber) is covered by it too.
  if (hasOwedPublisherCommit(idHex, channelsActions.pendingPublisherCommits())) {
    // eslint-disable-next-line no-console
    console.info("[channels] rotation: a recorded commit still owes its reseal; none staged");
    return;
  }
  const wasm = await loadWasm();
  const policy = wasm.channelRotationPolicyDefault();
  if (!force || requestedAtEpoch !== undefined) {
    if (!force && !wasm.channelShouldRotate(policy, rebuiltState(wasm, idHex), BigInt(Date.now()))) return;
    await new Promise((r) => setTimeout(r, rotationJitterMs()));
    await readJournalHead(identity, idHex);
  }
  let rotated = false;
  let outcome = "not-due";
  await withPublisherGroup(identity, idHex, async (group, device) => {
    if (!group.isOwnerDevice(device) || !group.isActive()) return;
    if (requestedAtEpoch !== undefined && group.epoch() > requestedAtEpoch) {
      outcome = "done-elsewhere";
      return;
    }
    const ctx = rotationContext(identity, device, idHex, group, () => {
      rotated = true;
    });
    const state = force ? wasm.channelRotationStateNew() : rebuiltState(wasm, idHex);
    if (onProgress !== undefined) onProgress(0, 0);
    const result = await runRotationOnce(ctx, policy, state);
    outcome = result.rotated ? "landed" : "not-landed";
  });
  // eslint-disable-next-line no-console
  console.info(`[channels] rotation: ${outcome}`);
  if (!rotated) await readJournalHead(identity, idHex);
  if (rotated) await settleOwedRecord(idHex, () => republishChannelMeta(idHex, identity));
}

/**
 * ADR-0090 section 4 — the policy's inputs as every owner device sees them:
 * the epoch start is the DS row time of the commit that began the epoch
 * (recorded when that commit is applied or merged here), and the post count
 * is the number of the channel's posts since then that this device listed.
 * Never fewer posts than the stored count.
 */
function rebuiltState(wasm: Wasm, idHex: string): ChannelRotationState {
  const stored = loadOrSeedState(wasm, idHex);
  const since = Number(stored.lastRotationMs);
  const listed = (channelsState.postsByChannel().get(idHex) ?? []).filter((p) => p.serverTsMs >= since).length;
  return rebuiltRotationState(stored, listed);
}

/** Runtime shape of a `PublisherGroup.members()` element. */
interface WasmGroupMember {
  readonly leafIndex: number;
  readonly accountKey: Uint8Array;
  free?: () => void;
}

/** The account keys of a PublisherGroup's members (call under its lock),
 *  the owner first (ADR-0090: the group records the owner's account key; a
 *  leaf index says nothing since the leaves are devices). */
function memberAccounts(group: RotationContext["publisherGroup"]): string[] {
  const raw = group.members() as WasmGroupMember[];
  const sorted = [...raw].sort((a, b) => a.leafIndex - b.leafIndex);
  const out: string[] = [base64Encode(group.ownerAccountKey())];
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
  let landed: number | null = null;
  await withPublisherGroup(identity, idHex, async (group, device) => {
    if (!group.isOwnerDevice(device)) throw new Error("not the owner");
    if (!memberAccounts(group).includes(keyB64)) {
      notePublisherGroupMembers(idHex, group);
      return;
    }
    landed = await runPublisherRemoval(
      rotationContext(identity, device, idHex, group, () => {
        removed = true;
      }),
      accountKey,
    );
    notePublisherGroupMembers(idHex, group);
  });
  // ADR-0090: a removal another owner device's commit beat (409) is not
  // done; the next pass reads the winner and the person asks again.
  if (landed === null && !removed) {
    await readJournalHead(identity, idHex);
    await withPublisherGroup(identity, idHex, async (group) => {
      notePublisherGroupMembers(idHex, group);
      if (memberAccounts(group).includes(keyB64)) throw new Error("removal not taken by the journal");
    });
  }
  if (removed) await settleOwedRecord(idHex, () => republishChannelMeta(idHex, identity));
}

/**
 * A co-publisher's composer follows the PublisherGroup: after the owner's
 * commits were applied, the seed of the group's epoch serves that channel
 * epoch (`notePublisherSeedAt`). A no-op when nothing moved.
 */
export async function refreshCoPublisherSeed(identity: WasmIdentity, idHex: string): Promise<void> {
  await withPublisherGroup(identity, idHex, async (g, device) => {
    if (!g.isActive()) return;
    // 2026-11-20: a group more than one epoch ahead of the service (a run of
    // commits whose reseal never landed) maps its current epoch to the
    // service's next one, where the reseal that lands will seal it.
    const svc = channelsState.epochByChannel().get(idHex);
    if (
      svc !== undefined &&
      !g.hasStagedCommit() &&
      reanchoredEpochOffset(g.epoch(), channelsActions.accessOf(idHex).epochOffset, svc) !== null
    ) {
      const seed = g.channelSeed(device);
      // The service's epoch when it already serves this seed (a device that
      // applied the whole run after the reseal landed), else the next one.
      const atService = channelsActions.currentSeed(idHex, svc);
      const target =
        atService !== undefined && atService.length === seed.length && atService.every((v, i) => v === seed[i])
          ? svc
          : svc + 1;
      notePublisherSeedAt(idHex, target, seed);
      anchorChannelEpoch(idHex, g.epoch(), target, seed);
      channelsActions.notePublisherGroupChanged();
      // eslint-disable-next-line no-console
      console.info("[channels] a publisher group was ahead of the channel by more than one epoch; re-mapped");
      return;
    }
    // ADR-0090: an owner device follows its siblings' commits the same way.
    const epoch = channelEpochOfGroup(channelsActions.accessOf(idHex), g.epoch());
    const held = channelsState.keysByChannel().get(idHex);
    if (held === undefined || held.seedEpoch === epoch) return;
    notePublisherSeedAt(idHex, epoch, g.channelSeed(device));
    channelsActions.notePublisherGroupChanged();
  });
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
  const device = await sharedPublisherDevice(identity, idHex);
  const groupId = group.groupId();
  const account = accountKeyOfJoiner(groupId, joiner) ?? joiner;
  const landedAt = await approveCoPublisher(ds, identity, group, joiner, idHex);
  if (landedAt === null) {
    // ADR-0090: another owner device's commit took the epoch (or the journal
    // did not answer): read the head; the request is approved again on a
    // later pass if it is still pending.
    await readJournalHead(identity, idHex);
    return;
  }
  // The Add moved the content seed: the composer takes the new one, the old
  // one stays under its epoch, and the record is issued again under the new
  // epoch so the new publisher (and every reader let in later) finds the
  // channel's name it can open.
  try {
    notePublisherCommitLanded(idHex, landedAt, group.channelSeed(device));
    channelsActions.notePublisherGroupChanged();
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[channels] post-approve seed refresh failed", err instanceof Error ? err.name : "");
  }
  await settleOwedRecord(idHex, () => republishChannelMeta(idHex, identity));
  // A publisher link is for one person: once used it is revoked on every
  // device of the account, and its ledger row goes.
  if (link?.record != null) {
    await revokeRegisteredLink(link.record).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("[channels] used publisher link not revoked in the register", err instanceof Error ? err.name : "");
    });
  }
  const accountKeyB64 = base64Encode(account);
  const bound = boundCoPublisherKey(groupId, account);
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
            // A request carries the id only in a profile; a person with no
            // name sends none, and the owner's contact row names them.
            accountIdHex: resolvePublisherAccountIdHex(
              accountId === undefined ? "" : hexOf(accountId),
              accountKeyB64,
              (k) => accountIdB64OfKey(k),
            ),
            sentVersions: [],
          },
        ],
  }));
  notePendingPublisherRequests(
    idHex,
    pendingPublisherRequests(idHex).filter((k) => k !== accountKeyB64),
  );
}

/** A pending request's device leaf key, by `<channel>:<account key b64>`
 *  (the Publishers screen lists the account; the approve needs the leaf). */
const pendingJoinerDevices = new Map<string, Uint8Array>();

/** The owner's co-publisher step: read knocks, approve a request that came
 *  by a publisher link this device minted and did not use yet. */
async function ownerKnocks(
  identity: WasmIdentity,
  idHex: string,
  onApplication: (b: Uint8Array, sender: Uint8Array | undefined) => Promise<void>,
  /** ADR-0090 section 2.1, the own-device reconcile of this channel. */
  reconcile: () => Promise<unknown>,
): Promise<void> {
  const ds = new DsClient();
  const group = await sharedPublisherGroup(identity, idHex);
  const leavesNow = (): Promise<{ owner: string; leaves: PublisherLeaf[] }> =>
    withPublisherGroup(identity, idHex, async (g) => ({ owner: devHexOf(g.ownerAccountKey()), leaves: publisherLeaves(g) }));
  const before = await leavesNow().catch(() => null);
  let applied = 0;
  await pollCoPublisherJournal(ds, identity, group, onApplication, (createdAt) => {
    applied += 1;
    noteEpochStart(idHex, createdAt);
  });
  // ADR-0064 section 7 (amendment of 2026-11-14): a commit another owner
  // device made (or an entry) was applied here: the head saves the current
  // epoch, and a new leaf of this account (an entry) is reconciled against
  // the DeviceGroup roster in this same pass. Never fails the pass.
  if (applied > 0) {
    const after = await leavesNow().catch(() => null);
    await afterPeerCommitsApplied(
      {
        applied,
        ownLeavesAdded: before === null || after === null ? [] : newOwnLeaves(after.owner, before.leaves, after.leaves),
      },
      {
        writeHead: () => withPublisherGroup(identity, idHex, (g, device) => writeCurrentHead(g, device, identity)),
        reconcile,
      },
    );
  }
  const groupId = group.groupId();
  const unmatched: string[] = [];
  for (const joiner of pendingCoPublishers(group)) {
    const cap = capabilityOfJoiner(groupId, joiner);
    const link = cap === undefined ? null : await publisherLinkOf(idHex, cap);
    if (link === null) {
      // Came by no live one-use publisher link of this account: waits for
      // the owner's Approve on the Publishers screen, listed by the person's
      // ACCOUNT key (ADR-0090: the request's leaf names a device).
      const account = accountKeyOfJoiner(groupId, joiner) ?? joiner;
      pendingJoinerDevices.set(`${idHex}:${base64Encode(account)}`, joiner);
      unmatched.push(base64Encode(account));
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
  // ADR-0090 section 6: the owner's own account first (its devices admitted
  // by a sibling need them too), sealed to the account's own X25519 key.
  const own = record.ownHandover ?? null;
  if (own !== null) {
    const pending = seedsToSend(record, own);
    if (pending.length > 0) {
      await sendSeedsTo(wasm, identity, idHex, own, identity.x25519PublicKey, pending, (seedId) =>
        channelsActions.updateAccessDurable(idHex, (r) =>
          r.ownHandover === undefined || r.ownHandover === null ? r : { ...r, ownHandover: withSeedSent(r.ownHandover, seedId) },
        ),
      );
    }
  }
  for (const p of record.publisherKeys) {
    const versions = versionsToSend(channelsActions.accessOf(idHex), p);
    if (versions.length === 0 || p.accountIdHex === "") continue;
    const bundle = await discovery.identityFetch(hexToBytes(p.accountIdHex)).catch(() => null);
    if (bundle === null || bundle.signature_pubkey !== p.accountKeyB64) continue;
    // Security review 2026-11-21: only an X25519 key bound to the account
    // key is sealed to; an unbound one is retried next pass.
    const x = boundX25519With((b, e, k) => wasm.verifyX25519Binding(b, e, k), bundle);
    if (x === null) continue;
    await sendSeedsTo(wasm, identity, idHex, p, x, seedsToSend(channelsActions.accessOf(idHex), p), (seedId) =>
      channelsActions.updateAccessDurable(idHex, (r) => ({
        ...r,
        publisherKeys: r.publisherKeys.map((x2) => (x2.accountKeyB64 === p.accountKeyB64 ? withSeedSent(x2, seedId) : x2)),
      })),
    );
  }
}

/** Every admission seed of `seeds` to one account (ADR-0090 open question
 *  4: every seed of a version, not one per version), each recorded after it
 *  went out. */
async function sendSeedsTo(
  wasm: Wasm,
  identity: WasmIdentity,
  idHex: string,
  p: PublisherEntry,
  x: Uint8Array,
  seeds: readonly { readonly version: number; readonly seedB64: string }[],
  recordSent: (seedId: string) => Promise<void>,
): Promise<void> {
  const ds = new DsClient();
  await withPublisherGroup(identity, idHex, async (group, device) => {
    for (const s of seeds) {
      const seed = b64ToBytes(s.seedB64);
      let message: Uint8Array;
      try {
        const envelope = wasm.composeAdmissionSeedEnvelope(group.channelId(), b64ToBytes(p.accountKeyB64), x, s.version, seed);
        message = group.createApplicationMessage(device, envelope);
      } finally {
        seed.fill(0);
      }
      // The sending ratchet moved: record it before the message leaves (a
      // crash in between would reuse the generation, G-34 class).
      await persistThenPublish(
        () => channelsActions.persistPublisherGroupDurable(),
        () => ds.publishGroup(group.groupId(), message).then(() => undefined),
      );
      await recordSent(admissionSeedId(s.seedB64));
    }
  });
}

function withSeedSent(p: PublisherEntry, seedId: string): PublisherEntry {
  const ids = p.sentSeedIds ?? [];
  return ids.includes(seedId) ? p : { ...p, sentSeedIds: [...ids, seedId] };
}

/**
 * ADR-0087 section 7 — a publisher recorded with no account id (its request
 * carried no profile: a person with no display name; or an entry a binding
 * created) gets the id from this account's contact store, so the read key,
 * the earlier seeds and the admission seeds reach it. Written once found.
 */
async function fillPublisherAccountIds(idHex: string): Promise<void> {
  const resolve = (k: string): string | null => accountIdB64OfKey(k);
  const current = channelsActions.accessOf(idHex).publisherKeys;
  if (withResolvedAccountIds(current, resolve) === current) return;
  await channelsActions.updateAccessDurable(idHex, (r) => ({
    ...r,
    publisherKeys: withResolvedAccountIds(r.publisherKeys, resolve),
  }));
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
  const pending = readKeyRecipients(channelsActions.accessOf(idHex).publisherKeys);
  if (pending.length === 0) return;
  const discovery = new DiscoveryClient(undefined, { sessionToken: () => sessionTokens.peek() });
  for (const p of pending) {
    const bundle = await discovery.identityFetch(hexToBytes(p.accountIdHex)).catch(() => null);
    if (bundle === null || bundle.signature_pubkey !== p.accountKeyB64) continue;
    // Security review 2026-11-21: only an X25519 key bound to the account
    // key is sealed to; an unbound one is retried next pass.
    const x = boundX25519With((b, e, k) => wasm.verifyX25519Binding(b, e, k), bundle);
    if (x === null) continue;
    const ds = new DsClient();
    await withPublisherGroup(identity, idHex, async (group, device) => {
      const channelId = group.channelId();
      // The channel's read key as this device holds it: the one the account
      // seed derives, or, for a channel the account took back after a key
      // reset, the one its owned-channels list carried (ADR-0101 section 7:
      // the channel's readers and its public record hold that one).
      const readKey = (await channelReadKeyHere(identity, idHex)) ?? wasm.channelReadKey(identity, channelId);
      let message: Uint8Array;
      try {
        const envelope = wasm.composeChannelReadKeyEnvelope(channelId, b64ToBytes(p.accountKeyB64), x, readKey);
        message = group.createApplicationMessage(device, envelope);
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

/** The two 2026-11-08 PublisherGroup application messages, told apart by
 *  the core's dispatcher; anything else is "other". */
function applicationKind(wasm: Wasm, plaintext: Uint8Array): "epoch-seeds" | "rotate-request" | "other" {
  if (plaintext.length === 0 || plaintext[0] !== 0x6c) return "other";
  let env: ReturnType<Wasm["dispatchIncomingEnvelope"]>;
  try {
    env = wasm.dispatchIncomingEnvelope(plaintext);
  } catch {
    return "other";
  }
  const kind = env.kind;
  env.free();
  if (kind === wasm.IncomingEnvelopeKind.ChannelEpochSeeds) return "epoch-seeds";
  if (kind === wasm.IncomingEnvelopeKind.ChannelRotateRequest) return "rotate-request";
  return "other";
}

/** The seeds this owner device holds for a channel, by epoch: the kept
 *  per-epoch seeds and the composer's own. */
function heldChannelSeeds(idHex: string): Map<number, Uint8Array> {
  const out = new Map<number, Uint8Array>(channelsState.subscribedSeedsByChannel().get(idHex) ?? []);
  const keys = channelsState.keysByChannel().get(idHex);
  if (keys?.seedEpoch !== undefined && !out.has(keys.seedEpoch)) out.set(keys.seedEpoch, keys.seed);
  return out;
}

/**
 * ADR-0087 section 7 (amended 2026-11-08), UX 15.2.1 — the owner hands each
 * publisher the seeds of the channel's earlier epochs it holds (the newest
 * 64 not yet sent), sealed to the person's ACCOUNT X25519 key exactly as the
 * read key, as one PublisherGroup application message. Recorded per epoch
 * after the publish; a failure is retried by the next pass. Owner only.
 */
async function sendEpochSeeds(wasm: Wasm, identity: WasmIdentity, idHex: string): Promise<void> {
  const held = heldChannelSeeds(idHex);
  const access = channelsActions.accessOf(idHex);
  // ADR-0090 section 6: the owner's own account too (an admitted own device
  // reads the channel's earlier posts with these).
  const own = access.ownHandover ?? null;
  const pending = [
    ...(own !== null && epochSeedsToSend([...held.keys()], own.sentEpochs ?? []).length > 0 ? [own] : []),
    ...access.publisherKeys.filter(
      (p) => p.accountIdHex !== "" && epochSeedsToSend([...held.keys()], p.sentEpochs ?? []).length > 0,
    ),
  ];
  if (pending.length === 0) return;
  const discovery = new DiscoveryClient(undefined, { sessionToken: () => sessionTokens.peek() });
  for (const p of pending) {
    const isOwn = own !== null && p === own;
    const epochs = epochSeedsToSend([...held.keys()], p.sentEpochs ?? []);
    let x: Uint8Array;
    if (isOwn) {
      x = identity.x25519PublicKey;
    } else {
      const bundle = await discovery.identityFetch(hexToBytes(p.accountIdHex)).catch(() => null);
      if (bundle === null || bundle.signature_pubkey !== p.accountKeyB64) continue;
      // Security review 2026-11-21: never seal to an unbound X25519 key.
      const bound = boundX25519With((b, e, k) => wasm.verifyX25519Binding(b, e, k), bundle);
      if (bound === null) continue;
      x = bound;
    }
    if (x.length !== 32) continue;
    const flat = new Uint8Array(32 * epochs.length);
    epochs.forEach((e, i) => flat.set(held.get(e) ?? new Uint8Array(32), 32 * i));
    const ds = new DsClient();
    await withPublisherGroup(identity, idHex, async (group, device) => {
      let message: Uint8Array;
      try {
        const envelope = wasm.composeChannelEpochSeedsEnvelope(
          group.channelId(),
          b64ToBytes(p.accountKeyB64),
          x,
          Uint32Array.from(epochs),
          flat,
        );
        message = group.createApplicationMessage(device, envelope);
      } finally {
        flat.fill(0);
      }
      await persistThenPublish(
        () => channelsActions.persistPublisherGroupDurable(),
        () => ds.publishGroup(group.groupId(), message).then(() => undefined),
      );
    });
    const sent = (x2: PublisherEntry): PublisherEntry => ({
      ...x2,
      sentEpochs: [...new Set([...(x2.sentEpochs ?? []), ...epochs])],
    });
    await channelsActions.updateAccessDurable(idHex, (r) =>
      isOwn
        ? r.ownHandover === undefined || r.ownHandover === null
          ? r
          : { ...r, ownHandover: sent(r.ownHandover) }
        : {
            ...r,
            publisherKeys: r.publisherKeys.map((x2) => (x2.accountKeyB64 === p.accountKeyB64 ? sent(x2) : x2)),
          },
    );
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
    const joiner = pendingJoinerDevices.get(`${idHex}:${joinerKeyB64}`) ?? b64ToBytes(joinerKeyB64);
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

  // A co-publisher removed a subscriber: it asks the owner's device to rotate
  // (ADR-0087 section 15, amended 2026-11-08). Persisted before published.
  installChannelRotateRequest(async (idHex) => {
    const identity = chats.conversationAccountIdentity();
    if (identity === null) throw new Error("signed out");
    const wasm = await loadWasm();
    await withPublisherGroup(identity, idHex, async (group, device) => {
      if (!group.isActive() || group.isOwnerDevice(device)) return;
      const message = group.createApplicationMessage(device, wasm.composeChannelRotateRequestEnvelope(group.channelId()));
      await persistThenPublish(
        () => channelsActions.persistPublisherGroupDurable(),
        () => new DsClient().publishGroup(group.groupId(), message).then(() => undefined),
      );
    });
  });
  onCleanup(() => installChannelRotateRequest(null));

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
    await withPublisherGroup(identity, idHex, async (group, device) => {
      if (!group.isActive() || group.isOwnerDevice(device)) return;
      const epoch = group.epoch().toString();
      if (bindingSentAt.get(idHex) === epoch) return;
      const binding = wasm.composeChannelCoPublisherBinding(identity, group.channelId());
      const message = group.createApplicationMessage(device, binding);
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

  /** Channels a current publisher asked the owner to rotate (read this
   *  session; a request read again after a reload rotates once more, which
   *  costs a reseal and loses nothing). */
  const rotateRequested = new Map<string, bigint>();

  async function onApplication(
    wasm: Wasm,
    identities: readonly WasmIdentity[],
    idHex: string,
    plaintext: Uint8Array,
    sender: Uint8Array | undefined,
    ownerKeyB64: string | null,
    isOwner: boolean,
    memberKeys: readonly string[] = [],
    currentEpoch?: bigint,
    group?: PublisherGroupHandle,
    device?: WasmIdentity,
  ): Promise<void> {
    // ADR-0090 section 2.2: a co-publisher's (or a leaving own device's)
    // device request, taken on an owner device and applied after the poll.
    if (group !== undefined && device !== undefined && takeDeviceRequest(wasm, idHex, group, device, plaintext, sender)) {
      return;
    }
    // UX 15.1 (2026-11-24): another publishing account's reader key, left
    // out of the subscriber list and count.
    if (await takePublisherReaderKey(wasm, idHex, plaintext, sender, memberKeys)) {
      return;
    }
    // ADR-0090 section 6: the admission mark, kept as the maximum.
    if (await takeAdmissionMark(wasm, idHex, ownerKeyB64 === null ? null : hexOf(b64ToBytes(ownerKeyB64)), plaintext, sender)) {
      return;
    }
    const kind = applicationKind(wasm, plaintext);
    // ADR-0087 section 15 (amended 2026-11-08): a publisher asks the owner
    // to rotate after removing a subscriber.
    if (kind === "rotate-request") {
      let requestChannel: Uint8Array;
      try {
        requestChannel = wasm.decodeChannelRotateRequestEnvelope(plaintext);
      } catch {
        return;
      }
      if (
        takesRotateRequest({
          deviceIsOwner: isOwner,
          channelIdHex: idHex,
          requestChannelHex: hexOf(requestChannel),
          senderB64: sender === undefined ? undefined : base64Encode(sender),
          ownerKeyB64,
          memberKeysB64: memberKeys,
        })
      ) {
        if (!rotateRequested.has(idHex)) rotateRequested.set(idHex, currentEpoch ?? 0n);
      }
      return;
    }
    // ADR-0087 section 7 (amended 2026-11-08), UX 15.2.1: the seeds of the
    // channel's earlier epochs, from the owner, to this new publisher.
    if (kind === "epoch-seeds") {
      await keepEpochSeeds(wasm, identities, idHex, plaintext, sender, ownerKeyB64, isOwner);
      return;
    }
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
      // ADR-0090 section 6: from any owner device. An owner device derives
      // the read key itself and needs none.
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
    // ADR-0090: from any owner device, and to the owner's own account too.
    if (sender === undefined || ownerKeyB64 === null || base64Encode(sender) !== ownerKeyB64) return;
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
      r.admissionSeeds.some((s) => s.seedB64 === seedB64)
        ? r
        : {
            ...r,
            // Only a private channel has admission seeds.
            visibility: r.visibility ?? "private",
            admissionSeeds: [...r.admissionSeeds, { version, seedB64 }],
          },
    );
  }

  async function keepEpochSeeds(
    wasm: Wasm,
    identities: readonly WasmIdentity[],
    idHex: string,
    plaintext: Uint8Array,
    sender: Uint8Array | undefined,
    ownerKeyB64: string | null,
    isOwner: boolean,
  ): Promise<void> {
    let env: ReturnType<Wasm["decodeChannelEpochSeedsEnvelope"]>;
    try {
      env = wasm.decodeChannelEpochSeedsEnvelope(plaintext);
    } catch {
      return;
    }
    let keep: boolean;
    try {
      keep = keepsEpochSeeds({
        deviceIsOwner: isOwner,
        channelIdHex: idHex,
        envelopeChannelHex: hexOf(env.channelId),
        memberAccountKeyB64: base64Encode(env.memberAccountKey),
        ownAccountKeysB64: identities.map((i) => base64Encode(i.signaturePublicKey)),
        senderB64: sender === undefined ? undefined : base64Encode(sender),
        ownerKeyB64,
      });
    } finally {
      env.free();
    }
    if (!keep) return;
    // Sealed to the account key the directory lists: the current identity
    // after a rotation, the lineage one before.
    for (const identity of identities) {
      let opened: ReturnType<Wasm["openChannelEpochSeedsEnvelope"]>;
      try {
        opened = wasm.openChannelEpochSeedsEnvelope(plaintext, identity);
      } catch {
        continue;
      }
      try {
        const epochs = Array.from(opened.epochs);
        const flat = opened.seeds;
        const seeds = epochs.map((epoch, i) => ({ epoch, seed: flat.slice(32 * i, 32 * (i + 1)) }));
        flat.fill(0);
        for (const add of epochSeedsToAdd(seeds, (e) => channelSeedFor(idHex, e) !== undefined)) {
          channelsActions.setSubscribedSeedForEpoch(idHex, add.epoch, add.seed);
        }
        await channelsActions.persistPublisherGroupDurable().catch(() => undefined);
      } finally {
        opened.free();
      }
      return;
    }
  }

  /** ADR-0090 section 4: the rotations running in the background (each
   *  waits up to a minute before it stages), one per channel. */
  const rotating = new Set<string>();
  /** ADR-0090 section 4: since when each channel's service epoch trails this
   *  owner device's group (the unfinished-reseal takeover). */
  const trailingSince = new Map<string, number>();
  /** ADR-0090 section 2.5: consecutive refused journal reads per channel. */
  const refusedReads = new Map<string, number>();

  /** What follows a commit of this device that landed (an admission, a
   *  retirement, a device request): the composer's seed and the record under
   *  the new channel epoch. */
  async function afterCommitLanded(lineage: WasmIdentity, idHex: string, newEpoch: number): Promise<void> {
    await withPublisherGroup(lineage, idHex, async (g, device) => {
      if (!g.isActive()) return;
      notePublisherCommitLanded(idHex, newEpoch, g.channelSeed(device));
      notePublisherGroupMembers(idHex, g);
    });
    channelsActions.notePublisherGroupChanged();
    saveRotationState(idHex, (await loadWasm()).channelRotationStateAfterRotation(BigInt(Date.now())));
    await settleOwedRecord(idHex, () => republishChannelMeta(idHex, lineage));
  }

  /** ADR-0090 section 4 — the unfinished-reseal takeover: this owner device
   *  holds a group epoch whose seed the channels service has not taken (its
   *  epoch trails the group's by one) for {@link RESEAL_TAKEOVER_MS}; it
   *  reseals it itself. The service takes a staged epoch only at current+1,
   *  so a second device doing the same is refused there. */
  async function takeOverUnfinishedReseal(lineage: WasmIdentity, idHex: string): Promise<void> {
    if (channelsActions.pendingPublisherCommits().some((e) => e.channelIdHex === idHex)) {
      trailingSince.delete(idHex);
      return;
    }
    // A reseal of this channel refused or failed waits out its hold
    // (2026-11-20), instead of being tried again every pass.
    if (!publisherCommitRetry.mayAttempt(idHex)) return;
    const svc = (await fetchEpoch(idHex)).epoch;
    await withPublisherGroup(lineage, idHex, async (g, device) => {
      if (!g.isActive() || !g.isOwnerDevice(device) || g.hasStagedCommit()) return;
      const groupAt = channelEpochOfGroup(channelsActions.accessOf(idHex), g.epoch());
      if (groupAt !== svc + 1) {
        trailingSince.delete(idHex);
        return;
      }
      const since = trailingSince.get(idHex) ?? Date.now();
      trailingSince.set(idHex, since);
      if (Date.now() - since < RESEAL_TAKEOVER_MS) return;
      // eslint-disable-next-line no-console
      console.info("[channels] taking over an unfinished reseal");
      try {
        await resealToSubscribers(g, device, lineage, idHex, groupAt);
      } catch (err) {
        publisherCommitRetry.noteFailure(idHex, err);
        throw err;
      }
      publisherCommitRetry.noteSuccess(idHex);
      notePublisherCommitLanded(idHex, groupAt, g.channelSeed(device));
      trailingSince.delete(idHex);
    });
  }

  /** Decision A's verdict for one owned channel this device does not hold. */
  function refoundHere(idHex: string): boolean {
    const account = boundContactsAccount();
    const device = chats.identity();
    if (account === null || device === null) return false;
    const roster = ownDeviceRoster(account);
    return refoundVerdict({
      registerRole: "owner",
      holdsGroup: channelsState.keysByChannel().has(idHex),
      foundedAlone: isFoundedAlone(account),
      rosterKeysHex: roster === null ? null : [...roster.keysHex].map((k) => k.toLowerCase()),
      selfKeyHex: devHexOf(device.signaturePublicKey),
    });
  }

  /** ADR-0090 decision A: the marker goes once every owned channel of the
   *  register is held here (founded again, or held already). */
  function settleFoundedAlone(): void {
    const account = boundContactsAccount();
    if (account === null || !isFoundedAlone(account)) return;
    const port = channelSubscriptionPort();
    const owned =
      port === null ? null : port.all().filter((r) => r.subscribed && r.role === "owner").map((r) => r.channelIdHex);
    if (foundedAloneDone(owned, (id) => channelsState.keysByChannel().has(id))) clearFoundedAlone(account);
  }

  /**
   * The channel's PublisherGroup this device holds is no longer active (its
   * own leaf was removed by a commit it applied): `true` when this device
   * stopped publishing here and waits (or reads) as
   * {@link inactivePublisherGroupVerdict} says (or, roster not read yet, was
   * left as it is for this pass), `false` when the account itself was
   * removed (the caller's removal path) or the group is active.
   */
  async function settleInactiveGroup(lineage: WasmIdentity, idHex: string, ownerDevice: boolean): Promise<boolean> {
    const ownHex = devHexOf(lineage.signaturePublicKey);
    const state = await withPublisherGroup(lineage, idHex, async (g, device) => {
      notePublisherGroupMembers(idHex, g);
      if (g.isActive()) return null;
      try {
        // F4: another leaf of the account (never this device's own) means
        // another device of the account removed this one and can admit it.
        const selfLeafHex = devHexOf(device.signaturePublicKey);
        return {
          ownAccountHasLeaf: publisherLeaves(g).some(
            (l) => l.accountKeyHex === ownHex && l.leafKeyHex !== selfLeafHex,
          ) as boolean | null,
        };
      } catch {
        return { ownAccountHasLeaf: null };
      }
    });
    if (state === null) return false;
    const account = boundContactsAccount();
    const roster = account === null ? null : ownDeviceRoster(account);
    const verdict = inactivePublisherGroupVerdict({
      ownerDevice,
      ownAccountHasLeaf: state.ownAccountHasLeaf,
      otherOwnDevices: roster === null ? null : [...roster.keysHex].length > 1,
    });
    if (verdict === "removed") return false;
    // The group signs nothing any more; decided once the roster is read.
    if (verdict === "undecided") return true;
    stopped.delete(idHex);
    forgetPublisherGroupMembers(idHex);
    if (verdict === "wait-for-welcome") {
      // The old group stays until an owner device's Welcome replaces it
      // (`landPublisherDeviceWelcome`); the reader line shows meanwhile.
      await suspectForeignChain(idHex);
    } else {
      releasePublisherGroup(idHex);
      channelsActions.dropPublisherRole(idHex);
    }
    // eslint-disable-next-line no-console
    console.info(`[channels] this device's leaf left the channel's group; it publishes again once admitted (${verdict})`);
    // The Welcome that admits it again may already have come (held early).
    await retryHeldChannelWelcomes(lineage);
    return true;
  }

  async function pass(): Promise<void> {
    // ADR-0099 step 3: no publisher work (rotation, approvals, seeds) while
    // the account is suspended; the next pass after it ends picks it up.
    if (accountSuspended()) return;
    const lineage = chats.conversationAccountIdentity();
    const current = chats.accountIdentity();
    if (lineage === null || current === null || !channelsState.hydrated()) return;
    const wasm = await loadWasm();
    // ADR-0090 section 3: a recorded commit the journal has not answered is
    // sent again (owed), a merged one finishes its steps.
    // ADR-0064 section 7: a recorded entry signs its commit with the root.
    ensureEntryOwnerSource();
    // 2026-11-20: an owned channel whose owner proof was refused moves its
    // owner key to the current identity's first (`channel-owner-repair.ts`).
    // Each channel is tried at most once per load.
    const repairRefusedOwners = async (): Promise<void> => {
      const predecessor = chats.predecessorAccountIdentity();
      for (const idHex of [...channelsState.keysByChannel().keys()].sort()) {
        if (!channelRotatesHere(idHex)) continue;
        await repairChannelOwnerIfRefused({
          channelIdHex: idHex,
          refused:
            publisherCommitRetry.hold(idHex)?.refused === true ||
            subscriberAdmissionRetry.hold(idHex)?.refused === true ||
            accountReaderKeyRefused(idHex),
          predecessors: predecessor === null ? [] : [predecessor],
          current,
          onOwned: () => {
            publisherCommitRetry.noteSuccess(idHex);
            subscriberAdmissionRetry.noteSuccess(idHex);
            clearAccountReaderKeyHold(idHex);
          },
        }).catch(() => undefined);
      }
    };
    await repairRefusedOwners();
    if (channelsActions.pendingPublisherCommits().length > 0) {
      const landed = await replayPublisherCommits(lineage).catch(() => []);
      for (const l of landed) {
        if (l.kind === "entry") {
          // An entry from the saved copy landed after a reload: this device
          // publishes now (keys, epoch, the head), like a live entry.
          if (await finishEntry(lineage, l.channelIdHex, l.serverTargetEpoch).catch(() => false)) {
            lastRotationCheck.set(l.channelIdHex, Date.now());
            await settleOwedRecord(l.channelIdHex, () => republishChannelMeta(l.channelIdHex, lineage)).catch(() => undefined);
          }
          continue;
        }
        await afterCommitLanded(lineage, l.channelIdHex, l.serverTargetEpoch).catch(() => undefined);
      }
    }
    // ADR-0101 section 7 item 8: a retake owed after a key reset on this
    // device continues first, so a channel it takes back is founded again by
    // the step below in this same pass. Never throws.
    const ownedAccount = boundContactsAccount();
    if (ownedAccount !== null) await runOwnedChannelsPass({ accountIdB64: ownedAccount, lineage, current });
    // ADR-0090: an owned channel this device lists but holds no group for
    // takes its keys once a sibling admitted it, or is founded again here
    // (decision A); otherwise it stays a reader and waits. A reset that was
    // only suspected and whose signals cleared publishes here again first.
    await liftClearedResetSuspicions(lineage);
    await settleReaderDevices(
      lineage,
      refoundHere,
      async (idHex) => {
        lastRotationCheck.set(idHex, Date.now());
        await settleOwedRecord(idHex, () => republishChannelMeta(idHex, lineage)).catch(() => undefined);
      },
      // ADR-0064 section 7 (amendment of 2026-11-14): an owned channel this
      // device holds no group for is entered from its saved copy first.
      (idHex) => enterOwnedChannelFromSaved(lineage, idHex),
    );
    settleFoundedAlone();
    // A channel Welcome that came before this device applied its removal.
    await retryHeldChannelWelcomes(lineage);
    const orphaned = channelsState.orphanedByKeyReset();
    for (const idHex of [...channelsState.keysByChannel().keys()]) {
      if (orphaned.has(idHex) || stopped.has(idHex)) continue;
      if (chats.conversationAccountIdentity() !== lineage) return;
      // 2026-11-13: decided on group presence (this loop runs only for the
      // channels whose PublisherGroup this device holds) and the owner rule,
      // never on "has keys and no co-publisher mark" alone. ADR-0090: every
      // owner DEVICE holding the group is an owner here.
      const owner = channelRotatesHere(idHex);
      try {
        // A group whose own leaf a commit removed (applied in an earlier pass
        // whose remaining steps then failed on it) reads and signs nothing
        // more: settled before anything below touches it.
        if (await settleInactiveGroup(lineage, idHex, owner)) continue;
        if (owner && channelsActions.accessOf(idHex).owedType !== null) {
          await runOwedChannelType(lineage, current, idHex).catch(() => undefined);
        }
        const now = Date.now();
        if (owner && !rotating.has(idHex) && now - (lastRotationCheck.get(idHex) ?? 0) >= ROTATION_CHECK_MS) {
          lastRotationCheck.set(idHex, now);
          // ADR-0090 section 4: in the background (it waits before staging).
          rotating.add(idHex);
          void rotateChannel(lineage, idHex, false)
            .catch((err: unknown) => {
              // eslint-disable-next-line no-console
              console.warn("[channels] rotation failed", err instanceof Error ? err.name : "");
            })
            .finally(() => rotating.delete(idHex));
        }
        // A channel record owed after a commit (a re-post that failed, or a
        // reload before it): retried every pass until it lands. A held record
        // that contradicts the owner's own setting is owed too (seen live
        // 2026-11-13: a public channel recorded as not public).
        if (owner) oweRecordIfContradicted(idHex);
        await settleOwedRecord(idHex, () => republishChannelMeta(idHex, lineage));
        const epoch = channelsState.epochByChannel().get(idHex);
        if (epoch !== undefined && publicSeedFiled.get(idHex) !== epoch) {
          // Tried again next pass on a failure; the steps below (letting
          // subscribers in, the owner's approvals) do not wait on it.
          try {
            await ensurePublicSeed(lineage, idHex);
            publicSeedFiled.set(idHex, epoch);
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("[channels] a public seed was not written; tried again next pass", err instanceof Error ? err.name : "");
          }
        }
        // ADR-0090 open question 4: the admission seeds this device holds and
        // the account's register, brought together (every seed of a version).
        const seeds = channelsActions.accessOf(idHex).admissionSeeds;
        const merged = await syncAdmissionSeeds(idHex, seeds);
        if (merged.length !== seeds.length) {
          await channelsActions.updateAccessDurable(idHex, (r) => ({ ...r, admissionSeeds: merged }));
        }
        // 2026-11-13: the account's reader key (the register's) is a real
        // subscriber of the channel, registered once by this device.
        await ensureAccountReaderKey(lineage, idHex);
        // 2026-11-20: a refused or failing admission waits out its hold (a
        // refusal: until the next start) instead of running every pass.
        if (subscriberAdmissionRetry.mayAttempt(idHex)) {
          try {
            await admitNewSubscribers(idHex, lineage);
            subscriberAdmissionRetry.noteSuccess(idHex);
          } catch (err) {
            const hold = subscriberAdmissionRetry.noteFailure(idHex, err);
            // eslint-disable-next-line no-console
            console.warn(
              `[channels] subscriber admission failed (${publisherCommitFailureLabel(err)}); ${
                hold.refused ? "not tried again until the next start" : "tried again after a wait"
              }`,
            );
          }
        }
        // The owner's account key (ADR-0090: recorded beside the group).
        const memberKeys = await withPublisherGroup(lineage, idHex, async (g) => {
          notePublisherGroupMembers(idHex, g);
          return memberAccounts(g);
        });
        const ownerKeyB64 = memberKeys[0] ?? null;
        const group = await sharedPublisherGroup(lineage, idHex);
        const device = await sharedPublisherDevice(lineage, idHex);
        const app = (b: Uint8Array, sender: Uint8Array | undefined): Promise<void> =>
          onApplication(
            wasm,
            current === lineage ? [lineage] : [current, lineage],
            idHex,
            b,
            sender,
            ownerKeyB64,
            owner,
            memberKeys,
            group.epoch(),
            group,
            device,
          );
        try {
          if (owner) {
            await ownerKnocks(lineage, idHex, app, () =>
              reconcileOwnDevices(lineage, idHex, (id, e) => afterCommitLanded(lineage, id, e)),
            );
          }
          else await pollCoPublisherJournal(new DsClient(), lineage, group, app, (at) => noteEpochStart(idHex, at));
          refusedReads.delete(idHex);
        } catch (err) {
          // ADR-0090 section 2.5: the old device that comes back after its
          // channel was founded again elsewhere: its reads are refused and
          // the journal is at an epoch chain this group is not on.
          if (!isStatus(err, 401)) throw err;
          // Security pass: a refusal that was this device's clock does not
          // count, and nothing is acted on on the account's only device.
          const account = boundContactsAccount();
          const roster = account === null ? null : ownDeviceRoster(account);
          if (
            !mayActOnResetSignal({
              rosterKeysHex: roster === null ? null : [...roster.keysHex],
              clockSkewed: err instanceof DsHttpError && err.clockSkewed,
            })
          ) {
            throw err;
          }
          const refusals = (refusedReads.get(idHex) ?? 0) + 1;
          refusedReads.set(idHex, refusals);
          if (owner && refusals >= 2) {
            const probe = await new DsClient().knock(group.groupId(), []).catch(() => null);
            if (returningDeviceVerdict({ refusals, groupEpoch: group.epoch(), journalEpoch: probe?.epoch ?? null })) {
              // A suspicion only: this device stops publishing and rotating,
              // the group state stays until an owner-signed Welcome replaces it.
              forgetPublisherGroupMembers(idHex);
              await suspectForeignChain(idHex);
              refusedReads.delete(idHex);
              continue;
            }
          }
          throw err;
        }
        // Walk of 2026-11-14: an owner device whose leaf another owner device
        // retired (it applied that commit just now) no longer publishes here.
        if (owner && (await settleInactiveGroup(lineage, idHex, true))) continue;
        if (owner) {
          for (const key of stepDowns.get(idHex) ?? []) {
            await removePublisherNow(lineage, idHex, b64ToBytes(key));
            await afterPublisherRemoved(lineage, idHex, key);
            stepDowns.get(idHex)?.delete(key);
          }
          // ADR-0087 section 15 (amended 2026-11-08): a publisher removed a
          // subscriber and asked for a rotation; every request read before
          // it is this one rotation. ADR-0090: whichever owner device reads
          // it first; the others see the group moved and stand down.
          const requestedAt = rotateRequested.get(idHex);
          if (requestedAt !== undefined && !rotating.has(idHex)) {
            rotateRequested.delete(idHex);
            rotating.add(idHex);
            void rotateChannel(lineage, idHex, true, undefined, requestedAt)
              .catch(() => undefined)
              .finally(() => {
                rotating.delete(idHex);
                lastRotationCheck.set(idHex, Date.now());
              });
          }
          // ADR-0090 section 2.1: this account's own devices.
          await reconcileOwnDevices(lineage, idHex, (id, e) => afterCommitLanded(lineage, id, e));
          // ADR-0090 section 2.2: the co-publishers' device requests.
          await applyQueuedDeviceRequests(lineage, idHex, (id, e) => afterCommitLanded(lineage, id, e));
          await takeOverUnfinishedReseal(lineage, idHex).catch(() => undefined);
          await fillPublisherAccountIds(idHex);
          await sendReadKeys(wasm, lineage, idHex);
          await sendEpochSeeds(wasm, lineage, idHex);
          await sendAdmissionSeeds(wasm, lineage, idHex);
        } else {
          await sendOwnBinding(wasm, lineage, idHex);
          // Removed by the owner (or after stepping down): this device can
          // no longer post or read past the removal, so the channel ends here.
          // Only this DEVICE's leaf retired while the account still publishes
          // (another of its devices holds a leaf): it waits to be admitted
          // again instead (walk of 2026-11-14).
          if (await settleInactiveGroup(lineage, idHex, false)) continue;
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
              // 2026-11-13: the account's register says Subscriber again.
              noteChannelIntent(idHex, "demoted");
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
          // ADR-0090: the owner key the core recorded beside the group at the
          // join (the `/g/` issuer), for the account's register.
          if (channelsActions.accessOf(idHex).ownerAccountKeyB64 === undefined) {
            const ownerKeyB64 = base64Encode(group.ownerAccountKey());
            await channelsActions.updateAccessDurable(idHex, (r) => ({ ...r, ownerAccountKeyB64: ownerKeyB64 }));
          }
          // ADR-0090 section 2.2: ask the owner to admit / retire this
          // account's devices.
          await sendOwnDeviceRequests(lineage, idHex).catch(() => undefined);
          // A rotate request owed after this device removed a subscriber.
          await settleOwedRotateRequest(idHex);
        }
        // Another device's commits this poll applied (the owner's, or for an
        // owner device a sibling's: a rotation, an approve, an admission)
        // moved the content seed: the composer takes it now, the old one
        // stays under its epoch, and posts keep going out under the
        // committed epoch until the reseal moves the channel there.
        await refreshCoPublisherSeed(lineage, idHex);
        // ADR-0090 section 6: the admission mark, to every publishing device.
        if (channelsActions.accessOf(idHex).visibility === "private") await sendAdmissionMark(lineage, idHex);
        // UX 15.1 (2026-11-24): this account's reader key, to every other
        // publisher (again when the group grew).
        await sendPublisherReaderKey(lineage, idHex);
        await withPublisherGroup(lineage, idHex, async (g) => {
          notePublisherGroupMembers(idHex, g);
        });
      } catch (err) {
        // A channel the services do not answer for this time.
        // eslint-disable-next-line no-console
        console.warn("[channels] publisher pass failed", err instanceof Error ? err.name : "");
      }
    }
    // ADR-0101 section 7: every owned channel registers its recovery key
    // (derived from the owned-channels list key) and the list takes what
    // this pass changed (a rotation, an admission seed); returns read from
    // the continuity mailbox are acted on; returns this device owes reset
    // people are sent. Each step never throws.
    const returnAccount = boundContactsAccount();
    if (returnAccount !== null) {
      await runOwnedChannelsPass({ accountIdB64: returnAccount, lineage, current });
      await runHeldChannelReturns({
        accountIdB64: returnAccount,
        identity: ownerProofIdentity(current),
        offerProfile: (envelope) => void offerReturnedOwnProfile(returnAccount, envelope),
      });
      const ownKeys = new Set([b64(current.signaturePublicKey), b64(lineage.signaturePublicKey)]);
      await sendChannelReturns(returnAccount, lineage, ownKeys);
      await settleRecoveredChannels(returnAccount, lineage);
    }
    // A refusal recorded during this pass is repaired now, not a pass later.
    await repairRefusedOwners();
    for (const [idHex, record] of channelsState.accessByChannel()) {
      if (!record.waiting || channelsState.keysByChannel().has(idHex)) continue;
      await catchUpChannel(idHex).catch(() => undefined);
    }
  }

  // A wake that comes while a pass runs is not lost: one more pass follows
  // (a channel the account took back during the pass is founded again then,
  // not a minute later).
  let again = false;
  const run = (): void => {
    if (running) {
      again = true;
      return;
    }
    running = true;
    void pass().finally(() => {
      running = false;
      if (again) {
        again = false;
        run();
      }
    });
  };

  // ADR-0101 section 7 item 6: a created or deleted channel, a rotation and
  // a new admission seed write the owned-channels list now (the list step
  // alone, coalesced), not at the next full pass.
  let ownedWake: ReturnType<typeof setTimeout> | undefined;
  const runOwned = (): void => {
    if (ownedWake !== undefined) return;
    ownedWake = setTimeout(() => {
      ownedWake = undefined;
      const lineage = chats.conversationAccountIdentity();
      const current = chats.accountIdentity();
      const account = boundContactsAccount();
      if (lineage === null || current === null || account === null) return;
      if (!channelsState.hydrated() || accountSuspended()) return;
      void runOwnedChannelsPass({ accountIdB64: account, lineage, current });
    }, OWNED_WAKE_DELAY_MS);
  };

  // The list key leaves memory with the session.
  createEffect(() => {
    if (chats.conversationAccountIdentity() === null) dropOwnedChannelsSession();
  });

  createEffect(() => {
    if (chats.conversationAccountIdentity() !== null && channelsState.hydrated()) run();
  });

  onMount(() => {
    const timer = setInterval(run, PASS_INTERVAL_MS);
    // ADR-0101: a channel return the mailbox kept runs the pass now.
    installChannelReturnRunner(run);
    onCleanup(() => installChannelReturnRunner(null));
    installOwnedChannelsWake(runOwned);
    onCleanup(() => {
      installOwnedChannelsWake(null);
      if (ownedWake !== undefined) clearTimeout(ownedWake);
      dropOwnedChannelsSession();
    });
    // ADR-0101 section 8: a return carries the reset person's newest raw
    // profile this device holds.
    installHeldProfileSource((k) => profiles.heldEnvelopeOf(k));
    onCleanup(() => installHeldProfileSource(null));
    onCleanup(() => clearInterval(timer));
    // A new session (another account, a sign-in) tries a held channel again.
    const resetHolds = (): void => {
      publisherCommitRetry.reset();
      subscriberAdmissionRetry.reset();
      resetAccountReaderKeyHolds();
      resetChannelOwnerRepairs();
    };
    resetHolds();
    onCleanup(resetHolds);
    // ADR-0064 section 1: account deletion deletes the vaults of the
    // channels the account owns (their saved copies, section 7).
    installOwnedChannelVaults({
      lineage: () => chats.conversationAccountIdentity(),
      ownedChannelIds: () =>
        (channelSubscriptionPort()?.all() ?? [])
          .filter((r) => r.subscribed && r.role === "owner")
          .map((r) => r.channelIdHex),
    });
    onCleanup(() => installOwnedChannelVaults(null));
  });

  return null;
};
