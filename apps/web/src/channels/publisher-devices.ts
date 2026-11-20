// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 section 2 — the devices of an account that publishes, on web.
 *
 * - **The owner's devices (2.1).** At every publisher pass an owner DEVICE
 *   that holds the PublisherGroup compares the owner-account leaves with the
 *   account's DeviceGroup roster (`devices/own-device-roster.ts`, the
 *   authority on which devices belong to the account): a roster device with
 *   no leaf is admitted from its directory KeyPackage (bound to the owner's
 *   account, leaf key in the roster), an owner leaf no longer in the roster
 *   is retired, all in ONE staged commit ({@link reconcileOwnDevices}).
 * - **A co-publisher's devices (2.2, decision B).** A co-publisher's
 *   publishing device runs the same comparison for its own account but never
 *   commits: it sends the owner a `publisher-device-request` per device
 *   ({@link sendOwnDeviceRequests}); an owner device applies a request whose
 *   sender (resolved at receipt, C-68) is a current publisher and whose
 *   KeyPackage (admit) or leaf (retire) is that sender's
 *   ({@link takeDeviceRequest}, {@link applyQueuedDeviceRequests}).
 * - **The Welcome (2.3).** To the added account's mailbox: the own account
 *   with its own reach keys; a co-publisher with the delegation its request
 *   carried, else a held grant; else it stays owed ({@link admitExtras}).
 * - **Landing.** An admitted device of this account joins from the Welcome
 *   in its mailbox ({@link landPublisherDeviceWelcome}) and becomes a
 *   publisher; nothing is announced (UX section 8).
 * - **Leaving (2.4).** Before a wipe, a retire request for this device's own
 *   leaf of every PublisherGroup it holds, best effort
 *   ({@link retireOwnPublisherLeaves}).
 * - **The admission mark (section 6).** A publishing device that advances its
 *   `admittedThroughSeq` sends it; every publishing device keeps the maximum
 *   from a current publisher ({@link sendAdmissionMark}, {@link takeAdmissionMark}).
 *
 * Nothing here logs a key.
 */

import { loadWasm, type Wasm } from "../wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import { DsClient } from "../ds/client";
import type { ProofSigner } from "../ds/access";
import { DiscoveryClient } from "../discovery/client";
import { sessionTokens } from "../auth/session-token-provider";
import { boundContactsAccount } from "../contacts/local-store";
import { sealRowFor } from "../contacts/mailbox-row";
import { ownDeviceRoster } from "../devices/own-device-roster";
import { reachSession } from "../reach/reach-service";
import { verifiedTargetOfBinding, verifiedTargetOfGrant, writeToPerson } from "../reach/mailbox-write";
import { DELEGATION_TTL_MS } from "../reach/delegation-rules";
import { channelsActions, channelsState } from "../state/channels-state";
import type { PublisherCommitPorts } from "./publisher-commit-outbox";
import { persistThenPublish } from "./publisher-app-messages";
import { resealToSubscribers, runStagedPublisherCommit } from "./rotation-cron";
import {
  releasePublisherGroup,
  sharedPublisherDevice,
  sharedPublisherGroup,
  withPublisherGroup,
} from "./publisher-group-registry";
import { publisherDevice, publisherDeviceOrNull } from "./publisher-device";
import { channelSubscriptionPort } from "./channel-subscription-register";
import { effectiveAdmissionMark } from "./channel-access-record";
import {
  joinBelowDeletedGroup,
  NO_REPLAY_GUARD,
  replayGuardOf,
  welcomeHashHex,
  withJoinedWelcome,
  type WelcomeReplayGuard,
} from "./welcome-replay";
import { channelIdFromHex, foundPublisherGroup, heldPublisherKeys, type WasmIdentity } from "./composer";
import { fetchEpoch, rememberPublisherRole } from "./api";
import { abandonUnrecordedEntry, entryPendingFor, entryVerdict } from "./channel-entry";
import { writeCurrentHead, type EntryResult } from "./channel-vault-head";
import {
  heldChannelWelcomeAction,
  heldChannelWelcomeExpired,
  loadHeldChannelWelcomes,
  saveHeldChannelWelcomes,
  withHeldChannelWelcome,
  type HeldChannelWelcome,
} from "./held-channel-welcomes";

type PublisherGroup = InstanceType<Wasm["PublisherGroup"]>;

/** Runtime shape of a `PublisherGroup.members()` element. */
interface WasmGroupMember {
  readonly leafIndex: number;
  readonly signaturePublicKey: Uint8Array;
  readonly accountKey: Uint8Array;
  free?: () => void;
}

/** One leaf of a PublisherGroup, read under its lock. */
export interface PublisherLeaf {
  readonly leafKeyHex: string;
  readonly accountKeyHex: string;
}

export function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

function hexBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** The leaves of a PublisherGroup (call under its lock). */
export function publisherLeaves(group: Pick<PublisherGroup, "members">): PublisherLeaf[] {
  const raw = group.members() as WasmGroupMember[];
  const out = raw.map((m) => ({ leafKeyHex: hexOf(m.signaturePublicKey), accountKeyHex: hexOf(m.accountKey) }));
  for (const m of raw) m.free?.();
  return out;
}

/**
 * ADR-0090 section 2.1, pure: for the account `accountKeyHex`, the roster
 * devices that hold no leaf (`missing`) and the leaves whose device left the
 * roster (`retire`). A device never retires its own leaf (OpenMLS cannot:
 * another device of the account does), so `selfKeyHex` is never in `retire`.
 * With no roster (`null`) nothing is decided.
 */
export function ownDeviceDiff(input: {
  readonly accountKeyHex: string;
  readonly leaves: readonly PublisherLeaf[];
  readonly rosterKeysHex: readonly string[] | null;
  readonly selfKeyHex: string;
}): { readonly missing: string[]; readonly retire: string[] } {
  if (input.rosterKeysHex === null || input.rosterKeysHex.length === 0) return { missing: [], retire: [] };
  const roster = new Set(input.rosterKeysHex.map((k) => k.toLowerCase()));
  const own = input.leaves.filter((l) => l.accountKeyHex === input.accountKeyHex).map((l) => l.leafKeyHex);
  const held = new Set(own);
  const anyLeaf = new Set(input.leaves.map((l) => l.leafKeyHex));
  const missing = [...roster].filter((k) => !held.has(k) && !anyLeaf.has(k));
  const retire = own.filter((k) => !roster.has(k) && k !== input.selfKeyHex);
  return { missing: missing.sort(), retire: retire.sort() };
}

/** The KeyPackages of `missing`, filtered: bound to `accountKeyHex` and with
 *  a leaf key `missing` names. A device is never added on a KeyPackage
 *  alone: the roster named it first. */
export function admissibleKeyPackages(
  candidates: readonly { readonly accountKeyHex: string | null; readonly leafKeyHex: string; readonly keyPackage: Uint8Array }[],
  accountKeyHex: string,
  missing: readonly string[],
): Uint8Array[] {
  const want = new Set(missing);
  const out: Uint8Array[] = [];
  const seen = new Set<string>();
  for (const c of candidates) {
    if (c.accountKeyHex !== accountKeyHex || !want.has(c.leafKeyHex) || seen.has(c.leafKeyHex)) continue;
    seen.add(c.leafKeyHex);
    out.push(c.keyPackage);
  }
  return out;
}

async function ownKeyPackages(wasm: Wasm): Promise<
  { readonly accountKeyHex: string | null; readonly leafKeyHex: string; readonly keyPackage: Uint8Array }[]
> {
  const account = boundContactsAccount();
  if (account === null) return [];
  const discovery = new DiscoveryClient(undefined, { sessionToken: () => sessionTokens.peek() });
  const list = await discovery.fetchKeyPackages(base64Decode(account)).catch(() => null);
  const out = [];
  for (const d of list ?? []) {
    try {
      const bound = wasm.keyPackageAccountKey(d.keyPackage);
      out.push({
        accountKeyHex: bound === undefined ? null : hexOf(bound),
        leafKeyHex: hexOf(wasm.keyPackageSignatureKey(d.keyPackage)),
        keyPackage: d.keyPackage,
      });
    } catch {
      /* a KeyPackage that does not decode is no device */
    }
  }
  return out;
}

function rosterHex(): string[] | null {
  const account = boundContactsAccount();
  if (account === null) return null;
  const roster = ownDeviceRoster(account);
  return roster === null ? null : [...roster.keysHex].map((k) => k.toLowerCase());
}

/** What follows a landed commit of this module: the composer's seed and the
 *  channel record under the new epoch (the loops' rotation context). */
export type AfterCommitLanded = (idHex: string, newEpoch: number) => Promise<void>;

/**
 * ADR-0090 section 2.1 — on an owner device holding the group: admit the
 * account's roster devices that hold no leaf and retire the owner leaves that
 * left the roster, in ONE staged commit (the journal decides; on a 409 the
 * stage is abandoned and the winner applied by the next poll). The
 * KeyPackages are fetched outside the group's lock, the diff is taken again
 * inside it. Returns whether a commit landed. Never founds anything.
 */
export async function reconcileOwnDevices(
  identity: WasmIdentity,
  idHex: string,
  after: AfterCommitLanded,
): Promise<boolean> {
  const roster = rosterHex();
  if (roster === null) return false;
  const wasm = await loadWasm();
  const first = await withPublisherGroup(identity, idHex, async (group, device) => {
    if (!group.isActive() || !group.isOwnerDevice(device) || group.hasStagedCommit()) return null;
    return ownDeviceDiff({
      accountKeyHex: hexOf(group.ownerAccountKey()),
      leaves: publisherLeaves(group),
      rosterKeysHex: roster,
      selfKeyHex: hexOf(device.signaturePublicKey),
    });
  });
  if (first === null || (first.missing.length === 0 && first.retire.length === 0)) return false;
  const candidates = first.missing.length > 0 ? await ownKeyPackages(wasm) : [];
  let landedEpoch: number | null = null;
  await withPublisherGroup(identity, idHex, async (group, device) => {
    if (!group.isActive() || !group.isOwnerDevice(device) || group.hasStagedCommit()) return;
    const ownerHex = hexOf(group.ownerAccountKey());
    const diff = ownDeviceDiff({
      accountKeyHex: ownerHex,
      leaves: publisherLeaves(group),
      rosterKeysHex: rosterHex(),
      selfKeyHex: hexOf(device.signaturePublicKey),
    });
    const admit = admissibleKeyPackages(candidates, ownerHex, diff.missing);
    if (admit.length === 0 && diff.retire.length === 0) return;
    const retireFlat = new Uint8Array(32 * diff.retire.length);
    diff.retire.forEach((k, i) => retireFlat.set(hexBytes(k), 32 * i));
    // eslint-disable-next-line no-console
    console.info(`[channels] own devices: admitting ${admit.length}, retiring ${diff.retire.length}`);
    const ownerKey = group.ownerAccountKey();
    await ensureOwnHandoverEntry(idHex, ownerKey);
    const run = await runStagedPublisherCommit(
      { publisherGroup: group, identity, device, channelIdHex: idHex },
      {
        kind: "admit",
        stage: () => {
          const c = group.stageCommit(device, admit, retireFlat, false);
          try {
            return { commit: c.commit, welcome: c.welcome ?? null };
          } finally {
            c.free();
          }
        },
        joinerKeyB64: admit.length > 0 ? base64Encode(ownerKey) : null,
        joinerAccountKeyB64: admit.length > 0 ? base64Encode(ownerKey) : null,
      },
      admitExtras(new DsClient(), idHex),
    );
    if (run.outcome === "landed") landedEpoch = run.serverTargetEpoch;
  });
  if (landedEpoch === null) return false;
  await after(idHex, landedEpoch);
  return true;
}

/**
 * The own-account leaves `after` holds that `before` did not (the leaf keys,
 * hex). A new one after an applied commit is an entry from the saved copy
 * (ADR-0064 section 7) or a sibling's admission. Pure.
 */
export function newOwnLeaves(
  ownerKeyHex: string,
  before: readonly PublisherLeaf[],
  after: readonly PublisherLeaf[],
): string[] {
  const had = new Set(before.filter((l) => l.accountKeyHex === ownerKeyHex).map((l) => l.leafKeyHex));
  return after.filter((l) => l.accountKeyHex === ownerKeyHex && !had.has(l.leafKeyHex)).map((l) => l.leafKeyHex).sort();
}

/**
 * After this owner device applied commits another device made: save the
 * current epoch in the head and, when a leaf of this account appeared (an
 * entry from the saved copy, by a device that may not be in the account's
 * DeviceGroup roster; security pass), run the own-device reconcile of
 * section 2.1 in this same pass, so such a leaf is retired at once by the
 * existing roster rule. Never throws. Returns whether the reconcile ran.
 */
export async function afterPeerCommitsApplied(
  input: { readonly applied: number; readonly ownLeavesAdded: readonly string[] },
  ports: { readonly writeHead: () => Promise<unknown>; readonly reconcile: () => Promise<unknown> },
): Promise<boolean> {
  if (input.applied === 0) return false;
  try {
    await ports.writeHead();
  } catch {
    /* the head only helps a device that lost everything */
  }
  if (input.ownLeavesAdded.length === 0) return false;
  try {
    await ports.reconcile();
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[channels] the own-device reconcile after an entry failed; the next pass runs it", err instanceof Error ? err.name : "");
  }
  return true;
}

/** The owner's own hand-over record exists (ADR-0090 section 6). */
async function ensureOwnHandoverEntry(idHex: string, ownerKey: Uint8Array): Promise<void> {
  const keyB64 = base64Encode(ownerKey);
  if (channelsActions.accessOf(idHex).ownHandover?.accountKeyB64 === keyB64) return;
  await channelsActions.updateAccessDurable(idHex, (r) =>
    r.ownHandover?.accountKeyB64 === keyB64
      ? r
      : { ...r, ownHandover: { accountKeyB64: keyB64, copublisherKeyB64: "", accountIdHex: "", sentVersions: [] } },
  );
}

/**
 * The Welcome of an admission (ADR-0090 section 2.3): to the added account's
 * mailbox. This account: its own reach keys. A co-publisher: the committer
 * delegation its request carried (`auth`, the delegation envelope) naming one
 * of this account's keys, to the mailbox its signed binding names (verified
 * against the co-publisher's account key); else a grant this account holds
 * for them; else it stays owed (`MailboxUnreachableError`, retried).
 */
export function admitExtras(ds: DsClient, channelIdHex: string): Partial<Pick<PublisherCommitPorts, "deliverWelcome">> {
  void channelIdHex;
  return {
    async deliverWelcome(welcome, accountKey, auth) {
      const wasm = await loadWasm();
      const session = reachSession();
      if (auth !== null && session !== null && session.accountKeyHex !== hexOf(accountKey)) {
        let env: ReturnType<Wasm["decodeCommitterDelegationEnvelope"]> | null = null;
        try {
          env = wasm.decodeCommitterDelegationEnvelope(base64Decode(auth));
          wasm.checkCommitterDelegations(env);
          const target = await verifiedTargetOfBinding(env.addedMailboxBinding, [accountKey]);
          if (target !== null) {
            for (const delegate of session.delegateKeys()) {
              const header = wasm.committerDelegationFor(env, delegate.accountKey);
              if (header === undefined) continue;
              const signer = await delegate.signer();
              try {
                await ds.publishMailbox(target.address, sealRowFor(wasm, target, "welcome", welcome), {
                  signer,
                  delegation: header,
                });
                return;
              } finally {
                signer.free();
              }
            }
          }
        } catch {
          /* the delegation does not reach: a held grant below */
        } finally {
          env?.free();
        }
      }
      await writeToPerson(accountKey, { neverUnsigned: true }, (target, a) =>
        ds.publishMailbox(target.address, sealRowFor(wasm, target, "welcome", welcome), a),
      );
    },
  };
}

// ---------------------------------------------------------------------------
// A co-publisher's devices (section 2.2)
// ---------------------------------------------------------------------------

/** How often one device request is sent again while not acted on (chosen,
 *  not measured, the same on every client: well inside the 23 h a delegation
 *  lives). */
export const DEVICE_REQUEST_RESEND_MS = 30 * 60 * 1000;

const sentRequests = new Map<string, number>();

/** Whether a request for `key` on `idHex` is due now (in memory: after a
 *  reload it is sent once more, which the owner treats as the same ask). */
export function deviceRequestDue(sent: ReadonlyMap<string, number>, key: string, nowMs: number): boolean {
  const at = sent.get(key);
  return at === undefined || nowMs - at >= DEVICE_REQUEST_RESEND_MS;
}

/** Committer delegations for this account's own mailbox, naming the
 *  channel's owner account, with the 97-byte mailbox binding they are checked
 *  against; `null` when the session holds no grant of its own. */
async function ownDelegations(
  wasm: Wasm,
  ownerAccountKey: Uint8Array,
  ownAccountKey: Uint8Array,
): Promise<{ readonly binding: Uint8Array; readonly blobs: Uint8Array[] } | null> {
  const session = reachSession();
  if (session === null) return null;
  const grant = session.grant("contact");
  try {
    const target = await verifiedTargetOfGrant(grant, [ownAccountKey]);
    if (target === null) return null;
    const seed = grant.seed;
    try {
      const blob = wasm.signDelegationBlob(
        seed,
        target.address,
        ownerAccountKey,
        BigInt(Date.now() + DELEGATION_TTL_MS),
        crypto.getRandomValues(new Uint8Array(16)),
      );
      return { binding: grant.binding, blobs: [blob] };
    } finally {
      seed.fill(0);
    }
  } finally {
    grant.free();
  }
}

/**
 * ADR-0090 section 2.2 — on a co-publisher's device holding the group: ask
 * the owner to admit the account's roster devices with no leaf (the device's
 * directory KeyPackage, bound to this account, with delegations for this
 * account's mailbox) and to retire this account's leaves that left the
 * roster. Each request is sent again every {@link DEVICE_REQUEST_RESEND_MS}
 * while not acted on. The sending ratchet moves: persisted before published.
 */
export async function sendOwnDeviceRequests(identity: WasmIdentity, idHex: string): Promise<void> {
  const roster = rosterHex();
  if (roster === null) return;
  const wasm = await loadWasm();
  const plan = await withPublisherGroup(identity, idHex, async (group, device) => {
    if (!group.isActive() || group.isOwnerDevice(device)) return null;
    const own = device.accountPublicKey;
    if (own === undefined) return null;
    return {
      ownKey: own,
      ownerKey: group.ownerAccountKey(),
      diff: ownDeviceDiff({
        accountKeyHex: hexOf(own),
        leaves: publisherLeaves(group),
        rosterKeysHex: roster,
        selfKeyHex: hexOf(device.signaturePublicKey),
      }),
    };
  });
  if (plan === null) return;
  const now = Date.now();
  const missing = plan.diff.missing.filter((k) => deviceRequestDue(sentRequests, `${idHex}:a:${k}`, now));
  const retire = plan.diff.retire.filter((k) => deviceRequestDue(sentRequests, `${idHex}:r:${k}`, now));
  if (missing.length === 0 && retire.length === 0) return;
  const channelId = channelIdFromHex(idHex);
  const envelopes: { readonly key: string; readonly bytes: Uint8Array }[] = [];
  if (missing.length > 0) {
    const kps = admissibleKeyPackages(await ownKeyPackages(wasm), hexOf(plan.ownKey), missing);
    const delegations = await ownDelegations(wasm, plan.ownerKey, plan.ownKey);
    if (delegations !== null) {
      for (const kp of kps) {
        envelopes.push({
          key: `${idHex}:a:${hexOf(wasm.keyPackageSignatureKey(kp))}`,
          bytes: wasm.composePublisherDeviceAdmit(channelId, kp, delegations.binding, delegations.blobs),
        });
      }
    }
  }
  for (const k of retire) {
    envelopes.push({ key: `${idHex}:r:${k}`, bytes: wasm.composePublisherDeviceRetire(channelId, hexBytes(k)) });
  }
  if (envelopes.length === 0) return;
  const ds = new DsClient();
  await withPublisherGroup(identity, idHex, async (group, device) => {
    for (const e of envelopes) {
      const message = group.createApplicationMessage(device, e.bytes);
      await persistThenPublish(
        () => channelsActions.persistPublisherGroupDurable(),
        () => ds.publishGroup(group.groupId(), message).then(() => undefined),
      );
      sentRequests.set(e.key, Date.now());
    }
  });
  // eslint-disable-next-line no-console
  console.info(`[channels] device requests sent: ${envelopes.length}`);
}

/** One device request an owner device took, applied after the poll. */
export interface QueuedDeviceRequest {
  readonly action: "admit" | "retire";
  readonly senderB64: string;
  readonly keyPackage?: Uint8Array;
  readonly deviceKey?: Uint8Array;
  /** Admit: the delegation envelope, standard base64, or `null`. */
  readonly delegationsB64: string | null;
}

const queued = new Map<string, QueuedDeviceRequest[]>();

/**
 * The receiver's rule for a device request (ADR-0090 section 2.2), pure:
 * only on an owner device, for this channel, from a current publisher
 * account (resolved at receipt); an admit from a co-publisher whose
 * KeyPackage binds that same account; a retire whose named leaf belongs to
 * the sender. The owner's own account asks only to retire (its admissions
 * follow the roster).
 */
export function takesDeviceRequest(input: {
  readonly deviceIsOwner: boolean;
  readonly channelIdHex: string;
  readonly requestChannelHex: string;
  readonly action: "admit" | "retire";
  readonly senderHex: string | undefined;
  readonly ownerHex: string;
  readonly memberAccountsHex: readonly string[];
  /** Admit: the KeyPackage's bound account. */
  readonly keyPackageAccountHex?: string | null;
  /** Retire: the account of the named leaf, or `null` when no such leaf. */
  readonly leafAccountHex?: string | null;
}): boolean {
  if (!input.deviceIsOwner || input.requestChannelHex !== input.channelIdHex) return false;
  if (input.senderHex === undefined || !input.memberAccountsHex.includes(input.senderHex)) return false;
  if (input.action === "admit") {
    if (input.senderHex === input.ownerHex) return false;
    return input.keyPackageAccountHex === input.senderHex;
  }
  return input.leafAccountHex === input.senderHex;
}

/**
 * Read one PublisherGroup application message as a device request (on an
 * owner device, inside the poll's lock). `true` when it was one (taken or
 * dropped). Queued for {@link applyQueuedDeviceRequests}.
 */
export function takeDeviceRequest(
  wasm: Wasm,
  idHex: string,
  group: PublisherGroup,
  device: WasmIdentity,
  plaintext: Uint8Array,
  sender: Uint8Array | undefined,
): boolean {
  if (plaintext.length === 0 || plaintext[0] !== 0x6c) return false;
  let req: ReturnType<Wasm["decodePublisherDeviceRequestEnvelope"]>;
  try {
    req = wasm.decodePublisherDeviceRequestEnvelope(plaintext);
  } catch {
    return false;
  }
  try {
    const action = req.action === "admit" ? "admit" : "retire";
    const leaves = publisherLeaves(group);
    const kp = req.keyPackage;
    const leafKey = req.deviceKey;
    const take = takesDeviceRequest({
      deviceIsOwner: group.isOwnerDevice(device),
      channelIdHex: idHex,
      requestChannelHex: hexOf(req.channelId),
      action,
      senderHex: sender === undefined ? undefined : hexOf(sender),
      ownerHex: hexOf(group.ownerAccountKey()),
      memberAccountsHex: [...new Set(leaves.map((l) => l.accountKeyHex))],
      keyPackageAccountHex: kp === undefined ? null : ((): string | null => {
        const a = wasm.keyPackageAccountKey(kp);
        return a === undefined ? null : hexOf(a);
      })(),
      leafAccountHex:
        leafKey === undefined ? null : (leaves.find((l) => l.leafKeyHex === hexOf(leafKey))?.accountKeyHex ?? null),
    });
    if (!take || sender === undefined) return true;
    let delegationsB64: string | null = null;
    if (action === "admit") {
      const env = req.committerDelegations(sender);
      if (env !== undefined) {
        try {
          delegationsB64 = base64Encode(wasm.composeCommitterDelegationEnvelope(env));
        } finally {
          env.free();
        }
      }
    }
    const list = queued.get(idHex) ?? [];
    list.push({
      action,
      senderB64: base64Encode(sender),
      ...(kp === undefined ? {} : { keyPackage: kp }),
      ...(leafKey === undefined ? {} : { deviceKey: leafKey }),
      delegationsB64,
    });
    queued.set(idHex, list);
    return true;
  } finally {
    req.free();
  }
}

/**
 * Apply the device requests taken this pass, one staged commit each (the
 * journal decides). A request that no longer applies (the device holds a
 * leaf already, the leaf is gone, the cap) is dropped; the requesting device
 * asks again if it still has reason to.
 */
export async function applyQueuedDeviceRequests(
  identity: WasmIdentity,
  idHex: string,
  after: AfterCommitLanded,
): Promise<void> {
  const list = queued.get(idHex) ?? [];
  queued.delete(idHex);
  for (const r of list) {
    let landedEpoch: number | null = null;
    try {
      await withPublisherGroup(identity, idHex, async (group, device) => {
        if (!group.isActive() || !group.isOwnerDevice(device) || group.hasStagedCommit()) return;
        const leaves = publisherLeaves(group);
        const wasm = await loadWasm();
        if (r.action === "admit" && r.keyPackage !== undefined) {
          const kp = r.keyPackage;
          if (leaves.some((l) => l.leafKeyHex === hexOf(wasm.keyPackageSignatureKey(kp)))) return;
          const run = await runStagedPublisherCommit(
            { publisherGroup: group, identity, device, channelIdHex: idHex },
            {
              kind: "admit",
              stage: () => {
                const added = group.stageAdmitDevices(device, [kp]);
                try {
                  return { commit: added.commit, welcome: added.welcome };
                } finally {
                  added.free();
                }
              },
              joinerKeyB64: r.senderB64,
              joinerAccountKeyB64: r.senderB64,
              replyGrantB64: r.delegationsB64,
            },
            admitExtras(new DsClient(), idHex),
          );
          if (run.outcome === "landed") landedEpoch = run.serverTargetEpoch;
        } else if (r.action === "retire" && r.deviceKey !== undefined) {
          const key = r.deviceKey;
          if (!leaves.some((l) => l.leafKeyHex === hexOf(key))) return;
          if (hexOf(key) === hexOf(device.signaturePublicKey)) return;
          const run = await runStagedPublisherCommit(
            { publisherGroup: group, identity, device, channelIdHex: idHex },
            { kind: "rotation", stage: () => ({ commit: group.stageRetireDevices(device, key), welcome: null }) },
          );
          if (run.outcome === "landed") landedEpoch = run.serverTargetEpoch;
        }
      });
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[channels] a device request was not applied", err instanceof Error ? err.name : "");
    }
    if (landedEpoch !== null) await after(idHex, landedEpoch);
  }
}

// ---------------------------------------------------------------------------
// Landing an admission (section 2.3)
// ---------------------------------------------------------------------------

/** What an owner or co-publisher channel of the register needs to try a
 *  Welcome against it. */
export interface PublisherWelcomeTarget {
  readonly channelIdHex: string;
  readonly groupId: Uint8Array;
  readonly ownerKey: Uint8Array;
  readonly role: "owner" | "co-publisher";
}

/**
 * The channels a Welcome in this account's mailbox may admit this device
 * to: the register's Owner channels (group id and owner key from the account
 * seed) and its co-publisher channels (group id and the owner key the
 * register RECORDED: the `/g/` capability issuer the joining device
 * verified). A co-publisher channel with no recorded owner key is not a
 * target: the owner key is never taken from the Welcome. Pure over the views.
 *
 * A channel this device still holds a group (or keys) for IS a target (walk
 * of 2026-11-14): the device that comes back after another owner device
 * entered the channel again and retired its leaf still holds its old group
 * and its old keys, and the Welcome that admits it again is exactly the one
 * {@link joinReplacingHeldGroup} was written for. Whether a held group is
 * replaced is that rule's (same group id, sent by a device of the owner's
 * account), never this list's.
 */
export function publisherWelcomeTargetsOf(
  wasm: Pick<Wasm, "publisherGroupIdForSeed" | "publisherOwnerAccountKey">,
  ownerSeed: Uint8Array,
  registers: readonly {
    readonly channelIdHex: string;
    readonly subscribed: boolean;
    readonly role: "owner" | "co-publisher" | "subscriber";
    readonly publisherGroupIdHex: string;
    readonly ownerAccountKey: Uint8Array;
  }[],
): PublisherWelcomeTarget[] {
  const out: PublisherWelcomeTarget[] = [];
  for (const r of registers) {
    if (!r.subscribed) continue;
    if (r.role === "owner") {
      const cid = channelIdFromHex(r.channelIdHex);
      out.push({
        channelIdHex: r.channelIdHex,
        groupId: wasm.publisherGroupIdForSeed(ownerSeed, cid),
        ownerKey: wasm.publisherOwnerAccountKey(ownerSeed),
        role: "owner",
      });
    } else if (r.role === "co-publisher" && r.publisherGroupIdHex.length === 64 && r.ownerAccountKey.length === 32) {
      out.push({
        channelIdHex: r.channelIdHex,
        groupId: hexBytes(r.publisherGroupIdHex),
        ownerKey: r.ownerAccountKey.slice(),
        role: "co-publisher",
      });
    }
  }
  return out;
}

async function publisherWelcomeTargets(lineage: WasmIdentity): Promise<PublisherWelcomeTarget[]> {
  const p = channelSubscriptionPort();
  if (p === null) return [];
  const wasm = await loadWasm();
  return publisherWelcomeTargetsOf(wasm, lineage.ed25519Seed, p.all());
}

/**
 * One mailbox Welcome against the channels this device was admitted to by a
 * sibling (an owner device) or by the owner (a co-publisher's device): the
 * PublisherGroup is joined with this DEVICE, refused unless the Welcome's
 * sender is a device of the owner account. On a join the device becomes a
 * publisher (keys, role), durably. Returns whether a channel was joined.
 */
export async function landPublisherDeviceWelcome(lineage: WasmIdentity, welcome: Uint8Array): Promise<boolean> {
  return (await landPublisherDeviceWelcomeOutcome(lineage, welcome)) !== "not-taken";
}

/** What this device holds for a target's group: nothing, an active group or
 *  an inactive one. Never throws (an unreadable group reads as none). */
export function heldGroupStateFor(
  wasm: Wasm,
  device: WasmIdentity,
  lineage: WasmIdentity,
  t: PublisherWelcomeTarget,
  channelId: Uint8Array,
): "none" | "active" | "inactive" {
  let held: PublisherGroup | undefined;
  try {
    held =
      t.role === "owner"
        ? wasm.PublisherGroup.load(device, lineage.ed25519Seed, channelId)
        : wasm.PublisherGroup.loadJoined(device, t.groupId, channelId);
  } catch {
    return "none";
  }
  if (held === undefined) return "none";
  try {
    return held.isActive() ? "active" : "inactive";
  } catch {
    return "none";
  } finally {
    held.free();
  }
}

/**
 * Whether a Welcome came too early: it is an owner device's Welcome for the
 * group this device holds (the peek), but the held group is still active and
 * unsuspected, so the replace rule cannot take it YET (the commit that
 * removed this device's leaf has not been applied). Pure over the core's
 * read; never throws.
 */
export function welcomeCameEarly(
  wasm: Pick<Wasm, "peekWelcomeFounder">,
  device: WasmIdentity,
  welcome: Uint8Array,
  t: PublisherWelcomeTarget,
  heldGroup: "none" | "active" | "inactive",
  suspected: boolean,
): boolean {
  return heldGroup === "active" && !suspected && welcomeIsOwnersFor(wasm, device, welcome, t.groupId, t.ownerKey);
}

function holdEarlyWelcome(channelIdHex: string, welcome: Uint8Array): void {
  const account = channelSubscriptionPort()?.accountIdB64;
  if (account === undefined) {
    earlyInMemory.set(`${channelIdHex}:${base64Encode(welcome)}`, welcome);
    return;
  }
  const next = withHeldChannelWelcome(loadHeldChannelWelcomes(account), {
    channelIdHex,
    welcomeB64: base64Encode(welcome),
    heldAtMs: Date.now(),
  });
  if (!saveHeldChannelWelcomes(account, next)) {
    // eslint-disable-next-line no-console
    console.warn("[channels] an early Welcome could not be stored; it is tried again only in this page");
    earlyInMemory.set(`${channelIdHex}:${base64Encode(welcome)}`, welcome);
  }
  // eslint-disable-next-line no-console
  console.info("[channels] a channel Welcome came before this device's removal was applied; held");
}

/** Held Welcomes whose durable write failed, for this page's life. */
const earlyInMemory = new Map<string, Uint8Array>();

function dropHeld(predicate: (e: HeldChannelWelcome) => boolean): void {
  const account = channelSubscriptionPort()?.accountIdB64;
  if (account !== undefined) {
    const list = loadHeldChannelWelcomes(account);
    const next = list.filter((e) => !predicate(e));
    if (next.length !== list.length) saveHeldChannelWelcomes(account, next);
  }
  for (const [key, bytes] of earlyInMemory) {
    const [channelIdHex] = key.split(":");
    if (predicate({ channelIdHex: channelIdHex ?? "", welcomeB64: base64Encode(bytes), heldAtMs: 0 })) {
      earlyInMemory.delete(key);
    }
  }
}

/**
 * {@link landPublisherDeviceWelcome} with the outcome named: `joined`,
 * `held` (an owner device's Welcome that came before this device applied its
 * removal, kept for {@link retryHeldChannelWelcomes}), or `not-taken`.
 */
export async function landPublisherDeviceWelcomeOutcome(
  lineage: WasmIdentity,
  welcome: Uint8Array,
): Promise<"joined" | "held" | "not-taken"> {
  const device = publisherDeviceOrNull();
  if (device === null) return "not-taken";
  const wasm = await loadWasm();
  // F2: a Welcome this device already joined is never joined again.
  const hashHex = await welcomeHashHex(welcome);
  for (const t of await publisherWelcomeTargets(lineage)) {
    const channelId = channelIdFromHex(t.channelIdHex);
    // ADR-0090 section 2.5 (security pass): a group this device still holds
    // for the channel (its leaf retired, or kept after a suspected reset) is
    // replaced ONLY by a Welcome for the same group whose signer verifies as a
    // device of the owner, deleted immediately before the join; for an
    // inactive held group, only when the join lands at a newer epoch.
    const suspected = channelsActions.accessOf(t.channelIdHex).resetSuspected === true;
    const heldGroup = heldGroupStateFor(wasm, device, lineage, t, channelId);
    if (welcomeCameEarly(wasm, device, welcome, t, heldGroup, suspected)) {
      holdEarlyWelcome(t.channelIdHex, welcome);
      return "held";
    }
    const guard = replayGuardOf(channelsActions.accessOf(t.channelIdHex), hashHex);
    const group = joinReplacingHeldGroup(wasm, device, lineage, t, channelId, welcome, suspected, undefined, guard);
    if (group === null) continue;
    dropHeld((e) => e.channelIdHex === t.channelIdHex);
    await channelsActions.updateAccessDurable(t.channelIdHex, (r) => {
      const noted = withJoinedWelcome(r, hashHex);
      if (noted.resetSuspected !== true) return noted;
      const { resetSuspected: _gone, ...rest } = noted;
      void _gone;
      return rest;
    });
    let seed: Uint8Array;
    let seedEpoch: number;
    try {
      seed = group.channelSeed(device);
      seedEpoch = Number(group.epoch());
    } finally {
      group.free();
    }
    if (t.role === "co-publisher") {
      channelsActions.setCoPublisherGroupId(t.channelIdHex, hexOf(t.groupId));
      const ownerKeyB64 = base64Encode(t.ownerKey);
      await channelsActions.updateAccessDurable(t.channelIdHex, (r) => ({ ...r, ownerAccountKeyB64: ownerKeyB64 }));
    }
    const held = channelsState.keysByChannel().get(t.channelIdHex)?.signingKeySeed;
    const keys =
      t.role === "owner"
        ? await heldPublisherKeys(lineage, t.channelIdHex, held)
        : { seed, signingKeySeed: freshSigningSeed(wasm, held), seedEpoch };
    if (keys === null) continue;
    const offset = await learnEpochOffset(t.channelIdHex, keys.seedEpoch ?? seedEpoch);
    channelsActions.setKeys(t.channelIdHex, { ...keys, seedEpoch: (keys.seedEpoch ?? seedEpoch) + offset });
    await channelsActions.persistPublisherGroupDurable();
    if (t.role === "co-publisher") await rememberPublisherRole(t.channelIdHex, lineage, "copublisher").catch(() => undefined);
    // eslint-disable-next-line no-console
    console.info(`[channels] admitted to a channel's PublisherGroup by a sibling (${t.role})`);
    return "joined";
  }
  return "not-taken";
}

/**
 * Every Welcome held by {@link landPublisherDeviceWelcomeOutcome}, tried
 * again (each publisher pass, and right after a journal poll found this
 * device's leaf removed): {@link heldChannelWelcomeAction} decides per entry.
 * A tried entry goes whatever the outcome (joined: the channel's entries go;
 * not joined: the replace rule refused it). Returns the channels joined.
 * Never throws.
 */
export async function retryHeldChannelWelcomes(lineage: WasmIdentity): Promise<string[]> {
  const account = channelSubscriptionPort()?.accountIdB64;
  const device = publisherDeviceOrNull();
  if (device === null) return [];
  const entries: { channelIdHex: string; bytes: Uint8Array; heldAtMs: number }[] = [];
  if (account !== undefined) {
    for (const e of loadHeldChannelWelcomes(account)) {
      try {
        entries.push({ channelIdHex: e.channelIdHex, bytes: base64Decode(e.welcomeB64), heldAtMs: e.heldAtMs });
      } catch {
        dropHeld((x) => x.welcomeB64 === e.welcomeB64);
      }
    }
  }
  for (const [key, bytes] of earlyInMemory) {
    const [channelIdHex] = key.split(":");
    if (channelIdHex !== undefined) entries.push({ channelIdHex, bytes, heldAtMs: Date.now() });
  }
  if (entries.length === 0) return [];
  // Newest first; one landed Welcome ends its channel's holds (an older one
  // tried after it would find the joined group active and be held again).
  entries.sort((x, y) => y.heldAtMs - x.heldAtMs);
  const joined: string[] = [];
  try {
    const wasm = await loadWasm();
    const targets = await publisherWelcomeTargets(lineage);
    for (const e of entries) {
      if (joined.includes(e.channelIdHex)) continue;
      const b64 = base64Encode(e.bytes);
      const t = targets.find((x) => x.channelIdHex === e.channelIdHex);
      const action = heldChannelWelcomeAction({
        channelStillOurs: t !== undefined,
        heldGroup: t === undefined ? "none" : heldGroupStateFor(wasm, device, lineage, t, channelIdFromHex(t.channelIdHex)),
        suspected: channelsActions.accessOf(e.channelIdHex).resetSuspected === true,
        expired: heldChannelWelcomeExpired(
          { channelIdHex: e.channelIdHex, welcomeB64: b64, heldAtMs: e.heldAtMs },
          Date.now(),
        ),
      });
      if (action === "keep") continue;
      dropHeld((x) => x.channelIdHex === e.channelIdHex && x.welcomeB64 === b64);
      if (action === "drop") continue;
      if ((await landPublisherDeviceWelcomeOutcome(lineage, e.bytes)) === "joined") joined.push(e.channelIdHex);
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[channels] held channel Welcomes not retried", err instanceof Error ? err.name : "");
  }
  return joined;
}

/**
 * ADR-0090 — at landing, the channel epoch this device's group epoch serves:
 * the channels service's current epoch minus the group's (the commit that
 * added this device was resealed before its Welcome was sent, so the two
 * meet). Recorded in the access record; 0 when they agree or the service
 * does not answer.
 */
export async function learnEpochOffset(idHex: string, groupEpoch: number): Promise<number> {
  let offset = 0;
  try {
    const svc = (await fetchEpoch(idHex)).epoch;
    offset = epochOffsetAt(svc, groupEpoch);
  } catch {
    offset = channelsActions.accessOf(idHex).epochOffset ?? 0;
  }
  await channelsActions.updateAccessDurable(idHex, (r) =>
    (r.epochOffset ?? 0) === offset ? r : offset === 0 ? withoutOffset(r) : { ...r, epochOffset: offset },
  );
  return offset;
}

/** The offset a service epoch and a group epoch say. Below 0 when the group
 *  ran ahead of the service (commits whose reseal never landed; 2026-11-20),
 *  as iOS and Android learn it. Pure. */
export function epochOffsetAt(serviceEpoch: number, groupEpoch: number): number {
  return serviceEpoch - groupEpoch;
}

function withoutOffset<T extends { readonly epochOffset?: number }>(r: T): T {
  const { epochOffset: _drop, ...rest } = r;
  void _drop;
  return rest as T;
}

/**
 * What {@link replaceHeldGroupFor} took away, to put back when the join that
 * follows fails: the device provider exactly as it was before the delete.
 * `restore` is a no-op when nothing was held.
 */
export interface HeldGroupReplacement {
  restore(): void;
  /** Nothing was held under the id (the join is a first one, or follows a
   *  deletion: F3's floor applies). */
  readonly heldNothing?: true;
  /** The deleted group's epoch when it was INACTIVE; `null` when nothing was
   *  held or the held group was active (replaced under reset suspicion,
   *  where no epoch is compared). A join must land strictly above it, else
   *  the held group is put back. */
  readonly heldEpoch: bigint | null;
}

/**
 * Whether a Welcome may be joined for target `t`: a replacement handle when
 * this device holds no group under the id, or after deleting the one it holds
 * because the Welcome is for that same group and its sender is a device of
 * the owner's account (`peekWelcomeFounder`, which writes nothing); `null`
 * otherwise, and the held group stays.
 *
 * Replay guard (a device's KeyPackage is last-resort, so the Delivery Service
 * can replay an OLD owner-signed Welcome for the same group): a held group is
 * replaced only when it is NOT ACTIVE (this device's leaf was removed) or the
 * channel is under reset suspicion (`resetSuspected`). An active,
 * unsuspected held group is never replaced: `null`. An inactive held
 * group's epoch rides in the handle for {@link joinReplacingHeldGroup}'s
 * epoch check; an active, suspected one is replaced with no epoch check (a
 * channel founded again starts at a low epoch). Before the delete the whole device
 * provider is snapshotted, so a join that then fails puts the held group back
 * ({@link joinReplacingHeldGroup}); a snapshot that cannot be taken refuses
 * the replacement. Synchronous: nothing else can write the provider between
 * the snapshot and a restore. Never throws.
 */
export function replaceHeldGroupFor(
  wasm: Wasm,
  device: WasmIdentity,
  lineage: WasmIdentity,
  t: PublisherWelcomeTarget,
  channelId: Uint8Array,
  welcome: Uint8Array,
  resetSuspected: boolean,
): HeldGroupReplacement | null {
  let held: PublisherGroup | undefined;
  try {
    held =
      t.role === "owner"
        ? wasm.PublisherGroup.load(device, lineage.ed25519Seed, channelId)
        : wasm.PublisherGroup.loadJoined(device, t.groupId, channelId);
  } catch {
    held = undefined;
  }
  if (held === undefined) return { restore: () => undefined, heldEpoch: null, heldNothing: true };
  try {
    if (held.isActive() && !resetSuspected) return null;
    if (!welcomeIsOwnersFor(wasm, device, welcome, t.groupId, t.ownerKey)) return null;
    // Rule 2 (decided 2026-11-14, as on Android): the epoch is compared only
    // for an INACTIVE held group. An active one replaced under reset suspicion
    // is not compared: a channel founded again elsewhere (ADR-0090 decision
    // A) starts again at a low epoch, and a strict check would refuse its
    // legitimate Welcome for ever.
    const heldEpoch = held.isActive() ? null : held.epoch();
    const before = device.snapshotStorage();
    held.deleteLocally(device);
    // eslint-disable-next-line no-console
    console.info("[channels] an owner device's Welcome replaces this device's old PublisherGroup");
    return {
      heldEpoch,
      restore: () => {
        device.restoreStorage(before);
        // eslint-disable-next-line no-console
        console.warn("[channels] the replacing Welcome did not join; this device's old PublisherGroup was put back");
      },
    };
  } catch {
    return null;
  } finally {
    held.free();
  }
}

/**
 * The join of a Welcome for target `t`, replacing a group this device holds
 * for it ({@link replaceHeldGroupFor}): `null` when the Welcome may not
 * replace it, when the join fails, or when the held group was inactive and
 * the joined group's epoch is not strictly above it (a replayed old
 * Welcome); in those last two
 * cases the held group is put back as it was (the device keeps the group it
 * had, and any reset suspicion on it stays). `join` is the core's join,
 * replaceable in tests. Never throws.
 *
 * Replayed Welcomes (lakko-security F2 / F3, `welcome-replay.ts`): a Welcome
 * this device already joined (`guard.alreadyJoined`) is refused before
 * anything is touched, also under reset suspicion; and a join that replaced
 * NO held group is refused, its group deleted again, when it lands at an
 * epoch not above the last inactive group this device deleted, unless the
 * channel is under reset suspicion.
 */
export function joinReplacingHeldGroup(
  wasm: Wasm,
  device: WasmIdentity,
  lineage: WasmIdentity,
  t: PublisherWelcomeTarget,
  channelId: Uint8Array,
  welcome: Uint8Array,
  resetSuspected: boolean,
  join: () => PublisherGroup = () =>
    wasm.PublisherGroup.joinFromWelcome(device, welcome, t.groupId, channelId, t.ownerKey),
  guard: WelcomeReplayGuard = NO_REPLAY_GUARD,
): PublisherGroup | null {
  if (guard.alreadyJoined) {
    // eslint-disable-next-line no-console
    console.warn("[channels] a Welcome this device already joined was not taken again");
    return null;
  }
  const replacement = replaceHeldGroupFor(wasm, device, lineage, t, channelId, welcome, resetSuspected);
  if (replacement === null) return null;
  try {
    const joined = join();
    if (
      replacement.heldNothing === true &&
      joinBelowDeletedGroup({ joinedEpoch: joined.epoch(), deletedGroupEpoch: guard.deletedGroupEpoch, resetSuspected })
    ) {
      try {
        joined.deleteLocally(device);
      } finally {
        joined.free();
      }
      // eslint-disable-next-line no-console
      console.warn("[channels] a Welcome at an epoch not above the last deleted group's was not taken");
      return null;
    }
    if (replacement.heldEpoch !== null && !(joined.epoch() > replacement.heldEpoch)) {
      joined.free();
      // eslint-disable-next-line no-console
      console.warn("[channels] a Welcome at an epoch not above the held group's was not taken");
      throw new Error("welcome not newer than the held group");
    }
    return joined;
  } catch {
    try {
      replacement.restore();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[channels] the old PublisherGroup could not be put back", err instanceof Error ? err.name : "");
    }
    return null;
  }
}

/** Whether `welcome` is for `groupId` and was sent by a device of
 *  `ownerKey`'s account. Pure over the core's read; never throws. */
export function welcomeIsOwnersFor(
  wasm: Pick<Wasm, "peekWelcomeFounder">,
  device: WasmIdentity,
  welcome: Uint8Array,
  groupId: Uint8Array,
  ownerKey: Uint8Array,
): boolean {
  try {
    const f = wasm.peekWelcomeFounder(welcome, device);
    try {
      return hexOf(f.groupId) === hexOf(groupId) && hexOf(f.founderAccountKey) === hexOf(ownerKey);
    } finally {
      f.free();
    }
  } catch {
    return false;
  }
}

function freshSigningSeed(wasm: Wasm, held: Uint8Array | undefined): Uint8Array {
  if (held !== undefined && held.length === 32) return held.slice();
  const signer = wasm.ChannelSigningKey.generate();
  try {
    return signer.toSeed();
  } finally {
    signer.free();
  }
}

/**
 * ADR-0090 section 2.4 — before a wipe (Forget device, removed from the
 * account): a retire request for this device's own leaf of every
 * PublisherGroup it holds, best effort; another device of the account (the
 * owner's other device, or for a co-publisher the owner's device) applies
 * it. Never throws.
 */
export async function retireOwnPublisherLeaves(identity: WasmIdentity): Promise<void> {
  const wasm = await loadWasm().catch(() => null);
  if (wasm === null) return;
  const ds = new DsClient();
  for (const idHex of [...channelsState.keysByChannel().keys()]) {
    try {
      await withPublisherGroup(identity, idHex, async (group, device) => {
        if (!group.isActive()) return;
        const envelope = wasm.composePublisherDeviceRetire(channelIdFromHex(idHex), device.signaturePublicKey);
        const message = group.createApplicationMessage(device, envelope);
        await ds.publishGroup(group.groupId(), message);
      });
    } catch {
      /* best effort: the reconcile of 2.1 / 2.2 retires the leaf otherwise */
    }
  }
}

// ---------------------------------------------------------------------------
// The admission mark (section 6)
// ---------------------------------------------------------------------------

const markSent = new Map<string, number>();

/** Send this device's admission mark when it moved past what was last sent
 *  here. Persisted before published. Never throws. */
export async function sendAdmissionMark(identity: WasmIdentity, idHex: string): Promise<void> {
  const through = channelsActions.accessOf(idHex).admittedThroughSeq;
  if (through <= (markSent.get(idHex) ?? 0)) return;
  try {
    const wasm = await loadWasm();
    await withPublisherGroup(identity, idHex, async (group, device) => {
      if (!group.isActive()) return;
      const envelope = wasm.composeChannelAdmissionMarkEnvelope(channelIdFromHex(idHex), BigInt(through));
      const message = group.createApplicationMessage(device, envelope);
      await persistThenPublish(
        () => channelsActions.persistPublisherGroupDurable(),
        () => new DsClient().publishGroup(group.groupId(), message).then(() => undefined),
      );
    });
    markSent.set(idHex, through);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[channels] admission mark not sent; tried again next pass", err instanceof Error ? err.name : "");
  }
}

/**
 * The receiver's rule for an admission mark (section 6, tightened by the
 * security pass), pure: taken only for this channel and only from the
 * channel OWNER's account (resolved at receipt, C-68): the owner's devices
 * are the ones that admit; a co-publisher's mark is ignored. Returns the new
 * pending mark (the maximum of what was pending and this one), or `null`.
 * What counts is {@link effectiveAdmissionMark}: never past the rows this
 * device has itself read.
 */
export function admissionMarkVerdict(input: {
  readonly channelIdHex: string;
  readonly markChannelHex: string;
  readonly throughSeq: bigint;
  readonly pendingSeq: number;
  readonly senderHex: string | undefined;
  readonly ownerHex: string | null;
}): number | null {
  if (input.markChannelHex !== input.channelIdHex) return null;
  if (input.senderHex === undefined || input.ownerHex === null || input.senderHex !== input.ownerHex) return null;
  if (input.throughSeq > BigInt(Number.MAX_SAFE_INTEGER)) return null;
  const through = Number(input.throughSeq);
  return through > input.pendingSeq ? through : null;
}

/**
 * Read one PublisherGroup application message as an admission mark; `true`
 * when it was one (kept or dropped). Kept as the owner's pending mark, and
 * counted (`admittedThroughSeq`) only up to the newest subscriber row this
 * device has read itself; the rest counts once this device has read that
 * far (`withRowsRead`). A mark from the owner also counts as this device's
 * own mark (the owner device that set it removed every invalid row at or
 * below it).
 */
export async function takeAdmissionMark(
  wasm: Wasm,
  idHex: string,
  ownerHex: string | null,
  plaintext: Uint8Array,
  sender: Uint8Array | undefined,
): Promise<boolean> {
  if (plaintext.length !== wasm.channelAdmissionMarkEnvelopeLen() || plaintext[0] !== 0x6c) return false;
  let mark: ReturnType<Wasm["decodeChannelAdmissionMarkEnvelope"]>;
  try {
    mark = wasm.decodeChannelAdmissionMarkEnvelope(plaintext);
  } catch {
    return false;
  }
  let pending: number | null;
  try {
    pending = admissionMarkVerdict({
      channelIdHex: idHex,
      markChannelHex: hexOf(mark.channelId),
      throughSeq: mark.throughSeq,
      pendingSeq: channelsActions.accessOf(idHex).markPendingSeq ?? 0,
      senderHex: sender === undefined ? undefined : hexOf(sender),
      ownerHex,
    });
  } finally {
    mark.free();
  }
  if (pending === null) return true;
  const p = pending;
  await channelsActions.updateAccessDurable(idHex, (r) => ({
    ...r,
    markPendingSeq: Math.max(r.markPendingSeq ?? 0, p),
    admittedThroughSeq: effectiveAdmissionMark(r.admittedThroughSeq, Math.max(r.markPendingSeq ?? 0, p), r.rowsReadThroughSeq ?? 0),
    admissionMarkSet: true as const,
  }));
  markSent.set(idHex, Math.max(markSent.get(idHex) ?? 0, channelsActions.accessOf(idHex).admittedThroughSeq));
  return true;
}

/** The owner's account key of a held group, or `null`. */
export async function ownerKeyHexOf(identity: WasmIdentity, idHex: string): Promise<string | null> {
  try {
    const g = await sharedPublisherGroup(identity, idHex);
    return hexOf(g.ownerAccountKey());
  } catch {
    return null;
  }
}

/** The device the shared handle of `idHex` lives in (for a caller outside
 *  the lock that only reads a key). */
export async function deviceOf(identity: WasmIdentity, idHex: string): Promise<WasmIdentity> {
  try {
    return await sharedPublisherDevice(identity, idHex);
  } catch {
    return publisherDevice();
  }
}

// ---------------------------------------------------------------------------
// Reader devices: admitted, re-founded (decision A), or come back (2.5)
// ---------------------------------------------------------------------------

/**
 * ADR-0090 section 2.5, decision A, pure: whether this device founds a
 * channel's PublisherGroup again. Only for a channel the account OWNS, that
 * this device holds no group for, when this device founded the account's
 * DeviceGroup because no other device answered its rejoin knock
 * (the `founded-alone-marker`, kept until every owned channel is
 * re-founded or held here), and its DeviceGroup roster at re-found time is
 * exactly this device. A device with siblings never re-founds; it waits.
 */
export function refoundVerdict(input: {
  readonly registerRole: "owner" | "co-publisher" | "subscriber" | null;
  readonly holdsGroup: boolean;
  readonly foundedAlone: boolean;
  readonly rosterKeysHex: readonly string[] | null;
  readonly selfKeyHex: string;
}): boolean {
  if (input.registerRole !== "owner" || input.holdsGroup || !input.foundedAlone) return false;
  // The DeviceGroup roster at re-found time is exactly this device.
  if (input.rosterKeysHex === null) return false;
  return input.rosterKeysHex.length === 1 && input.rosterKeysHex[0]?.toLowerCase() === input.selfKeyHex;
}

/**
 * The account's channels this device lists but does not publish in (the
 * register says Owner or co-publisher): a group this device already holds
 * (a join that landed before a crash, a re-found that did) gets its keys; an
 * owned channel whose decision A verdict holds is founded again. Returns the
 * channels that became publishable here. Never throws.
 */
export async function settleReaderDevices(
  lineage: WasmIdentity,
  refound: (idHex: string) => boolean,
  onRefounded: (idHex: string) => Promise<void>,
  /** ADR-0064 section 7 (amendment of 2026-11-14): enter an owned channel
   *  from its saved copy, tried before decision A. Absent: never. */
  enterSaved?: (idHex: string) => Promise<EntryResult>,
): Promise<string[]> {
  const p = channelSubscriptionPort();
  const device = publisherDeviceOrNull();
  if (p === null || device === null) return [];
  const wasm = await loadWasm();
  const out: string[] = [];
  for (const r of p.all()) {
    if (!r.subscribed || channelsState.keysByChannel().has(r.channelIdHex)) continue;
    if (r.role !== "owner") continue;
    // A suspected reset waits for an owner device's Welcome; its kept group
    // is not taken up again.
    if (channelsActions.accessOf(r.channelIdHex).resetSuspected === true) continue;
    // An entry from the saved copy that awaits the DS: the replay settles it.
    const entryPending = entryPendingFor(r.channelIdHex);
    if (entryPending) continue;
    try {
      // A held entry nothing recorded (a reload between building and
      // recording it) was never sent: abandoned before anything reads it.
      await abandonUnrecordedEntry(lineage, r.channelIdHex);
      const keys = await heldPublisherKeys(lineage, r.channelIdHex);
      if (keys !== null) {
        const offset = channelsActions.accessOf(r.channelIdHex).epochOffset ?? 0;
        channelsActions.setKeys(r.channelIdHex, { ...keys, seedEpoch: (keys.seedEpoch ?? 0) + offset });
        out.push(r.channelIdHex);
        continue;
      }
      // ADR-0064 section 7 (amendment of 2026-11-14): enter from the saved
      // copy first; decision A only when there is none to enter from.
      if (
        enterSaved !== undefined &&
        entryVerdict({ registerRole: r.role, holdsGroup: false, resetSuspected: false, entryPending })
      ) {
        const entered = await enterSaved(r.channelIdHex);
        if (entered === "entered") {
          await onRefounded(r.channelIdHex);
          out.push(r.channelIdHex);
          continue;
        }
        if (entered === "owed") continue;
      }
      if (!refound(r.channelIdHex)) continue;
      await refoundOwnedChannel(wasm, lineage, r.channelIdHex);
      await onRefounded(r.channelIdHex);
      out.push(r.channelIdHex);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[channels] a reader device's channel was not settled", err instanceof Error ? err.name : "");
    }
  }
  return out;
}

/**
 * Decision A — found the PublisherGroup again at the same id on this device
 * (any local state of it deleted first), reset the journal under the same
 * root (`foundPublisherGroup`'s claim-or-reset), then reseal the new seed for
 * the channels service's next epoch (the public seed, the account's own
 * reader key; each private subscriber whose ticket this device can open is
 * let in again by the next admission pass, with the admission seeds the
 * register carried). Co-publishers are no longer in the group and are
 * invited again by the person. The epoch offset is recorded.
 */
async function refoundOwnedChannel(wasm: Wasm, lineage: WasmIdentity, idHex: string): Promise<void> {
  const device = publisherDevice();
  const channelId = channelIdFromHex(idHex);
  try {
    const old = wasm.PublisherGroup.load(device, lineage.ed25519Seed, channelId);
    if (old !== undefined) {
      old.deleteLocally(device);
      old.free();
    }
  } catch {
    /* nothing held, or unreadable: create refuses below if anything is */
  }
  const group = await foundPublisherGroup(wasm, lineage, channelId);
  try {
    const serviceEpoch = (await fetchEpoch(idHex)).epoch;
    const target = serviceEpoch + 1;
    const offset = epochOffsetAt(target, Number(group.epoch()));
    await channelsActions.updateAccessDurable(idHex, (r) => ({
      ...r,
      epochOffset: offset,
      // The co-publishers were in the old group; they are invited again.
      publishers: [],
      publisherKeys: [],
      ownHandover: null,
      // Nothing was let in by this device: the admission pass judges every
      // row again from the start, leaving a row with no ticket alone.
      admittedThroughSeq: 0,
    }));
    const keys = await heldPublisherKeys(lineage, idHex);
    if (keys === null) throw new Error("the founded group did not load");
    await resealToSubscribers(group, device, lineage, idHex, target);
    // ADR-0064 section 7 (amendment of 2026-11-14): the founded-again group's
    // saved copy, so another owner device can enter it. Never fails this.
    // REPLACED, not merged: the new group starts at a low epoch, and the
    // two newest by number would keep the dead group's GroupInfos.
    await writeCurrentHead(group, device, lineage, "replace");
    channelsActions.setKeys(idHex, { ...keys, seedEpoch: Number(group.epoch()) + offset });
    channelsActions.setEpoch(idHex, target);
    await channelsActions.persistPublisherGroupDurable();
    // eslint-disable-next-line no-console
    console.info("[channels] decision A: an owned channel was founded again on this device");
  } finally {
    group.free();
  }
}

/**
 * ADR-0090 section 2.5, pure: whether an owner device's held PublisherGroup
 * looks like one the journal no longer carries (a SUSPICION only: it stops
 * publishing, it deletes nothing, see {@link suspectForeignChain}) (the account's channel was founded
 * again by another device): its signed read was refused with 401 on
 * `refusals` consecutive passes AND the journal's latest epoch, read by an
 * empty knock, is not this group's.
 */
export function returningDeviceVerdict(input: {
  readonly refusals: number;
  readonly groupEpoch: bigint;
  readonly journalEpoch: number | null;
}): boolean {
  if (input.refusals < RETURNING_REFUSALS || input.journalEpoch === null) return false;
  // Another epoch, a DS latest epoch below the group's among them.
  return BigInt(input.journalEpoch) !== input.groupEpoch;
}

/** Consecutive refused reads before the journal is asked (chosen). */
export const RETURNING_REFUSALS = 2;

/**
 * ADR-0090 section 2.5 (security pass): the DS suggests this device's
 * channel was founded again elsewhere. A refused read and an epoch number
 * from the DS are not authenticated, so nothing is deleted: this device stops
 * publishing and rotating under the old seed (its keys go, the channel reads
 * as a waiting reader) and KEEPS the group state. Only an owner-signed
 * Welcome for the same group replaces it ({@link landPublisherDeviceWelcome}),
 * or the account wipe removes it.
 */
export async function suspectForeignChain(idHex: string): Promise<void> {
  // The handle is released (no pass uses it), the state stays in the provider.
  releasePublisherGroup(idHex);
  channelsActions.dropPublisherRole(idHex);
  await channelsActions.updateAccessDurable(idHex, (r) => ({ ...r, resetSuspected: true as const }));
  // eslint-disable-next-line no-console
  console.info("[channels] the journal may have been founded again elsewhere; reading until an owner device admits this one");
}

/**
 * What a device does once the channel's PublisherGroup it holds is no longer
 * active: its own leaf was removed by a commit it applied (walk of
 * 2026-11-14: the owner device that came back after another owner device
 * entered the channel again and retired its leaf kept the composer). Pure.
 *
 * - `wait-for-welcome`: the leaf was this DEVICE's, not the account's role:
 *   an owner device (an owner's account is never removed from its own
 *   channel), or a co-publisher's device whose account still holds a leaf.
 *   It stops publishing, shows the reader line, keeps the old group, and
 *   waits for the owner device's Welcome that admits it again (the replace
 *   rule of {@link joinReplacingHeldGroup}).
 * - `reader`: an owner device with no other device of the account to admit
 *   it: it stops publishing and the reader-device settle takes the channel
 *   (entry from the saved copy, else decision A), never waiting forever.
 *   A leaf of the owner's account still in the inactive group counts as such
 *   a device (lakko-security F4, 2026-11-15): the commit that removed this
 *   device's leaf came from a device of the owner's account (the peer gate
 *   C-90), so another owner device exists even when this device's DeviceGroup
 *   roster (stale after a DeviceGroup founded again) names only itself.
 * - `removed`: the account itself was removed as a publisher (or nothing
 *   says otherwise): the existing removal path (UX 15.2).
 * - `undecided`: the account's device roster is not loaded yet (and, for an
 *   owner device, no other leaf of the account is seen in the group): nothing is
 *   changed this pass (an owner device must not take the channel alone on a
 *   roster it has not read).
 */
export function inactivePublisherGroupVerdict(input: {
  readonly ownerDevice: boolean;
  /** Whether the inactive group's roster still holds a leaf of this account
   *  other than this device's own; `null` when it could not be read. */
  readonly ownAccountHasLeaf: boolean | null;
  /** Whether the account's DeviceGroup roster names a device besides this
   *  one; `null` when the roster is not loaded. */
  readonly otherOwnDevices: boolean | null;
}): "wait-for-welcome" | "reader" | "removed" | "undecided" {
  // F4: another leaf of the owner's account decides before the roster (it is
  // the device that removed this one, and it can admit it again).
  if (input.ownerDevice && input.ownAccountHasLeaf === true) return "wait-for-welcome";
  if (input.otherOwnDevices === null) return "undecided";
  if (input.ownerDevice) return input.otherOwnDevices ? "wait-for-welcome" : "reader";
  return input.ownAccountHasLeaf === true && input.otherOwnDevices ? "wait-for-welcome" : "removed";
}

/**
 * Pure: whether the DS signals may be acted on at all: never on the
 * account's only device (its group may be the only one left), never on a
 * refusal that was this device's clock.
 */
export function mayActOnResetSignal(input: {
  readonly rosterKeysHex: readonly string[] | null;
  readonly clockSkewed: boolean;
}): boolean {
  if (input.clockSkewed) return false;
  return input.rosterKeysHex !== null && input.rosterKeysHex.length > 1;
}

/**
 * ADR-0090 section 2.5, pure: whether the DS signals that raised a reset
 * suspicion have cleared: a read signed with the held group's current epoch
 * key was answered, and the DS's latest epoch for the journal is the held
 * group's epoch.
 */
export function resetSuspicionCleared(input: {
  readonly readOk: boolean;
  readonly latestEpoch: number | null;
  readonly groupEpoch: bigint;
}): boolean {
  return input.readOk && input.latestEpoch !== null && BigInt(input.latestEpoch) === input.groupEpoch;
}

/** One journal read signed by `signer`; throws on a refusal. Answers the
 *  DS's latest epoch for the journal (`null` when it named none). */
export type HeldEpochRead = (
  groupId: Uint8Array,
  signer: ProofSigner,
) => Promise<{ readonly latestEpoch: number | null }>;

const readAtHeldEpoch: HeldEpochRead = async (groupId, signer) => {
  const r = await new DsClient().fetchGroup(groupId, { limit: 1, waitSecs: 0, signer });
  return { latestEpoch: r.latestEpoch ?? null };
};

/**
 * ADR-0090 section 2.5 (security pass, false-alarm recovery): a channel
 * suspected of a reset becomes a publisher here again when the signals clear
 * ({@link resetSuspicionCleared}): its kept group's signed read is answered
 * and the journal is at the kept group's epoch. The suspicion is lifted
 * durably and the keys are taken up as the reader pass takes them for a
 * healthy group. Returns whether it was lifted. Never throws.
 */
export async function liftResetSuspicionIfClear(
  lineage: WasmIdentity,
  idHex: string,
  read: HeldEpochRead = readAtHeldEpoch,
): Promise<boolean> {
  if (channelsActions.accessOf(idHex).resetSuspected !== true) return false;
  const device = publisherDeviceOrNull();
  if (device === null) return false;
  try {
    const wasm = await loadWasm();
    const held = wasm.PublisherGroup.load(device, lineage.ed25519Seed, channelIdFromHex(idHex));
    if (held === undefined) return false;
    let cleared = false;
    try {
      if (!held.isActive()) return false;
      const groupEpoch = held.epoch();
      const signer = held.dsAccessSigner(device);
      let readOk = false;
      let latestEpoch: number | null = null;
      try {
        latestEpoch = (await read(held.groupId(), signer)).latestEpoch;
        readOk = true;
      } catch {
        readOk = false;
      } finally {
        signer.free();
      }
      cleared = resetSuspicionCleared({ readOk, latestEpoch, groupEpoch });
    } finally {
      held.free();
    }
    if (!cleared) return false;
    await channelsActions.updateAccessDurable(idHex, (r) => {
      if (r.resetSuspected !== true) return r;
      const { resetSuspected: _gone, ...rest } = r;
      void _gone;
      return rest;
    });
    // eslint-disable-next-line no-console
    console.info("[channels] the reset suspicion cleared; this device publishes in the channel again");
    if (channelsState.keysByChannel().has(idHex)) return true;
    const keys = await heldPublisherKeys(lineage, idHex);
    if (keys !== null) {
      const offset = channelsActions.accessOf(idHex).epochOffset ?? 0;
      channelsActions.setKeys(idHex, { ...keys, seedEpoch: (keys.seedEpoch ?? 0) + offset });
    }
    return true;
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[channels] a reset suspicion was not checked", err instanceof Error ? err.name : "");
    return false;
  }
}

/** {@link liftResetSuspicionIfClear} for every owned channel this device
 *  suspects; returns the channels lifted. Never throws. */
export async function liftClearedResetSuspicions(lineage: WasmIdentity): Promise<string[]> {
  const p = channelSubscriptionPort();
  if (p === null) return [];
  const out: string[] = [];
  for (const r of p.all()) {
    if (!r.subscribed || r.role !== "owner") continue;
    if (channelsActions.accessOf(r.channelIdHex).resetSuspected !== true) continue;
    if (await liftResetSuspicionIfClear(lineage, r.channelIdHex)) out.push(r.channelIdHex);
  }
  return out;
}
