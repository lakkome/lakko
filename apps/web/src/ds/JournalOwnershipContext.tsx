// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 - the signed-in session's journal ownership: which journals this
 * account owns, the registrar seeds the owners of other groups handed it, and
 * the owner's side of appointing registrars.
 *
 * Mounted once, inside `<ProfileProvider>` (it reads the account ids profiles
 * carry). It installs, for the life of the session:
 *
 *   - the statement-signer factory in the DS access registry (root from the
 *     ACCOUNT identity, registrar from the seed store) - `journal-authority.ts`;
 *   - the owner source for conversation groups: the live role table's owner
 *     is this account's key;
 *   - the registrar-seed receive handler the chat receive path hands envelopes
 *     to (`registrar-seeds.ts`), and the encrypted per-account seed store;
 *   - the appointment reconcile (`appointments.ts`), run for every group this
 *     account owns whenever that group's MLS queue drains (every poll, every
 *     commit, every sweep) and once per group when the session starts: it
 *     converges the DS registrar set to the roster (`registrars-set`) and
 *     sends the seed envelopes still owed;
 *   - the owner's claim on opening (`owner-claim.ts`): the reconcile of an
 *     owned group first claims its journal, once per session, because an
 *     unclaimed journal never answers the `401` the DS client's lazy claim
 *     waits for (the live walk of 2026-09-29); a claim that lands runs the
 *     reconcile again, wherever it was sent from;
 *   - the "root held" marker (`root-held.ts`) and the probe that sets it for
 *     an owned group that lacks it (the second security pass, finding 4);
 *   - both halves of the root handover (`root-handover.ts`): the outgoing
 *     owner's re-sent envelope until its probe answers 401, and the new
 *     owner's takeover with the received seed, kept encrypted until it lands;
 *   - the journal repair scheduler's ports (`journal-repair.ts`).
 *
 * Renders nothing of its own.
 */
import {
  installKnockSeedHandler,
  INITIAL_KNOCK_VERSION,
  installOwnerKnockSeedSource,
  knockSeeds,
  knockSeedSent,
  knockSeedVerdict,
  noteKnockSeedSent,
  type KnockSeedHeader,
} from "./knock-seeds";
import { createEffect, onCleanup, untrack, type JSX } from "solid-js";

import type { Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { useAuth } from "../auth/AuthContext";
import { base64Decode, base64Encode } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { useChats } from "../chat/ChatContext";
import { ChatClient } from "../chat/client";
import { readIsActive } from "../chat/membership-ended";
import { sweepBackoff } from "../chat/sweep-backoff";
import { DiscoveryClient } from "../discovery/client";
import { onGroupMlsIdle } from "../mls/group-lock";
import { useProfiles } from "../profile/ProfileContext";
import { loadWasm, type Wasm } from "../wasm";
import { installRotationKnockKeys, installRotationTakeOver } from "./rotation-takeover";
import { dsAccess } from "./access";
import {
  clearAppointmentRecord,
  desiredSetFor,
  loadAppointmentRecord,
  reconcileAppointments,
  saveAppointmentRecord,
  type AppointmentRoster,
} from "./appointments";
import { DsClient } from "./client";
import { wasmJournalAuthorityFactory, wasmRegistrarDerivation } from "./journal-authority";
import { journalRepair } from "./journal-repair";
import { probeRoot, releaseAuthority } from "./ownership";
import { bundleServesRosterKey, openSealedToOwnAccount } from "./rotated-member-seal";
import { bindRegistrarListAccount, noteRegistrarListArrival } from "./registrar-list";
import { IdbPendingRootSeedBackend, IdbRegistrarSeedBackend } from "./registrar-seed-idb";
import { clearRootHeld, isRootHeld, markRootHeld } from "./root-held";
import {
  installRegistrarSeedHandler,
  receiveRegistrarSeed,
  registrarSeeds,
  type RegistrarSeedHeader,
} from "./registrar-seeds";
import {
  HANDOVER_RESEND_MS,
  RANK_ROOT_HANDOVER,
  clearHandoverOwed,
  installOwnerTransitionHandler,
  loadOwedHandovers,
  markHandoverOwed,
  ownerMovedAwayFrom,
  pendingRootSeeds,
  receiveRootHandover,
  runOwedHandover,
  takeOverJournal,
} from "./root-handover";
import { durableSnapshotHook } from "../chat/persist-hooks";
import { noteKnockKeyChanged } from "../chat/handle-owed";
import { ownsGroup } from "../chat/group-owner";
import { openKnockOrigins } from "../chat/knock-origins";
import { openJoinReplies } from "../reach/join-replies";

/** How long a reconcile that left members pending waits before trying the
 *  unresolved ones again (chosen, not measured: a member's profile, which
 *  carries its account id, normally arrives within one poll of its join). */
const PENDING_RETRY_MS = 30_000;

/** How long an owned group whose root probe answered 401 (a takeover not yet
 *  landed) waits before probing again. Chosen, not measured. */
const ROOT_PROBE_RETRY_MS = 60_000;

function sameBytes(a: Uint8Array | undefined, b: Uint8Array | undefined): boolean {
  if (a === undefined || b === undefined || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

function hexToBytes(hex: string): Uint8Array | null {
  if (hex.length % 2 !== 0 || !/^[0-9a-f]*$/i.test(hex)) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** The roster the reconcile reads, off the live group. */
function rosterOf(group: {
  members(): unknown[];
  roleTableOwner(): Uint8Array | undefined;
  roleTableAdmins(): Uint8Array;
}): AppointmentRoster | null {
  try {
    const members = group.members() as { signaturePublicKey: Uint8Array; accountKey?: Uint8Array }[];
    const keys = new Set<string>();
    // ADR-0057 item 2 - the appointment record is kept per device leaf.
    const devices = new Map<string, string[]>();
    for (const m of members) {
      const key = m.accountKey !== undefined && m.accountKey.length === 32 ? m.accountKey : m.signaturePublicKey;
      const accountB64 = base64Encode(key);
      keys.add(accountB64);
      const list = devices.get(accountB64) ?? [];
      list.push(base64Encode(m.signaturePublicKey));
      devices.set(accountB64, list);
    }
    const owner = group.roleTableOwner();
    const flat = group.roleTableAdmins();
    const admins = new Set<string>();
    for (let i = 0; i + 32 <= flat.length; i += 32) admins.add(base64Encode(flat.slice(i, i + 32)));
    return {
      memberAccountKeysB64: [...keys],
      ownerAccountKeyB64: owner === undefined ? undefined : base64Encode(owner),
      adminAccountKeysB64: admins,
      deviceKeysByAccountB64: devices,
    };
  } catch {
    return null;
  }
}

export function JournalOwnershipProvider(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  // The account identity: the session-long instance when ChatContext holds
  // one, else one derived from the phrase seed and kept for the session.
  let derived: { account: string; identity: WasmIdentity } | null = null;
  let wasmNow: Wasm | null = null;
  function accountIdentityNow(): WasmIdentity | null {
    const held = chats.accountIdentity();
    if (held !== null) return held;
    const account = accountId();
    const secrets = auth.identitySecrets();
    if (account === null || secrets === null || wasmNow === null) return null;
    if (derived !== null && derived.account === account) return derived.identity;
    dropDerived();
    derived = { account, identity: wasmNow.Identity.fromSeed(secrets.accountIdentitySeed) };
    return derived.identity;
  }
  /** ADR-0074 section 3.5: the account key this account holds INSIDE a
   *  conversation (the role table's owner slot, the member an envelope is
   *  sealed to): after a proven rotation the predecessor, else the account
   *  key. The journal ROOT is always derived from the account identity. */
  function conversationIdentityNow(): WasmIdentity | null {
    return chats.predecessorAccountIdentity() ?? accountIdentityNow();
  }
  /** ADR-0074 section 3.7 (2026-11-20): the identity the directory serves
   *  when it is NOT the conversation identity (after a rotation), else
   *  `null`. An owner seals to the X25519 key of the served bundle, so an
   *  envelope addressed to the conversation key may open only with this. */
  function currentIdentityIfRotated(): WasmIdentity | null {
    return chats.predecessorAccountIdentity() === null ? null : accountIdentityNow();
  }
  /** Open an envelope addressed to this account's conversation key: sealed
   *  to the conversation identity's X25519 key, else to the current one's. */
  function openOwnSealed(
    openOwn: (addressee: WasmIdentity) => Uint8Array,
    openSealedTo: (addressee: WasmIdentity, sealedTo: WasmIdentity) => Uint8Array,
  ): Uint8Array | null {
    const conversation = conversationIdentityNow();
    if (conversation === null) return null;
    return openSealedToOwnAccount({
      conversation,
      current: currentIdentityIfRotated(),
      openOwn,
      openSealedTo,
    });
  }
  function dropDerived(): void {
    if (derived === null) return;
    try {
      derived.identity.free();
    } catch {
      /* already freed */
    }
    derived = null;
  }

  // The owner source for conversation groups: the live role table names this
  // account as owner.
  const unregisterOwner = dsAccess.registerOwnerSource((groupId) => {
    const entry = chats.get(base64Encode(groupId));
    if (entry === undefined) return null;
    const account = conversationIdentityNow();
    if (account === null) return false;
    let owner: Uint8Array | undefined;
    try {
      owner = entry.group.roleTableOwner();
    } catch {
      return false;
    }
    return sameBytes(owner, account.signaturePublicKey);
  });

  const ds = new DsClient();
  const discovery = new DiscoveryClient();
  // Publishes the seed envelopes. Its echoes are shared with every other
  // ChatClient on the page (`chat/own-published.ts`).
  const publisher = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: durableSnapshotHook(chats, accountId),
  });

  /** The member's account X25519 key from its identity bundle, verified to
   *  be that account's bundle. ADR-0074 section 3.7 (2026-11-20): after a
   *  rotation the bundle serves the CURRENT key while the roster names the
   *  conversation key; it is accepted when its two-way conversation link
   *  verifies against the served key and names the roster key. The envelope
   *  stays addressed to the roster key and is sealed to this X25519 key,
   *  which the member opens with its current identity. */
  async function memberX25519(accountKeyB64: string): Promise<Uint8Array | null> {
    const hex = profiles.accountIdHexOf(accountKeyB64);
    if (hex === null) return null;
    const accountIdBytes = hexToBytes(hex);
    if (accountIdBytes === null || accountIdBytes.length !== 16) return null;
    const bundle = await discovery.identityFetch(accountIdBytes);
    if (bundle === null) return null;
    const wasm = wasmNow ?? (await loadWasm());
    let serves: boolean;
    try {
      const link = bundle.conversation_link;
      serves = bundleServesRosterKey({
        servedKey: base64Decode(bundle.signature_pubkey),
        conversationLink: link === undefined || link.length === 0 ? null : base64Decode(link),
        rosterKey: base64Decode(accountKeyB64),
        verifyLink: (l, current) => wasm.verifyConversationLink(l, current),
      });
    } catch {
      serves = false;
    }
    if (!serves) {
      // The directory served a bundle for another key: never seal to it.
      // eslint-disable-next-line no-console
      console.warn("ds: an identity bundle did not match the member's account key; not appointed");
      return null;
    }
    const x = base64Decode(bundle.x25519_pubkey);
    return x.length === 32 ? x : null;
  }

  // Per-group reconcile scheduling.
  const running = new Map<string, { rerun: boolean }>();
  const notBefore = new Map<string, number>();
  // The registrar set this session last sent per group (finding 2: sent
  // once per group per session and on every change).
  const sentSets = new Map<string, string>();
  const probeNotBefore = new Map<string, number>();

  /** Finding 4 - the probe with this account's own root: `root` on 204. */
  async function probeOwnRoot(groupId: Uint8Array): Promise<"root" | "not-root"> {
    const authority = dsAccess.rootAuthority(groupId);
    try {
      if (authority.kind !== "root") throw new Error("no root");
      return await probeRoot(ds, groupId, authority.signer);
    } finally {
      releaseAuthority(authority);
    }
  }

  /** An owned group without the "root held" marker probes (at most every
   *  {@link ROOT_PROBE_RETRY_MS}); a 204 sets the marker. */
  async function probeIfUnheld(account: string, groupIdB64: string, roster: AppointmentRoster | null, ownB64: string): Promise<void> {
    if (roster?.ownerAccountKeyB64 !== ownB64) return;
    if (isRootHeld(account, groupIdB64)) return;
    const wait = probeNotBefore.get(groupIdB64);
    if (wait !== undefined && Date.now() < wait) return;
    probeNotBefore.set(groupIdB64, Date.now() + ROOT_PROBE_RETRY_MS);
    try {
      const probe = await probeOwnRoot(base64Decode(groupIdB64));
      if (probe === "root") {
        markRootHeld(account, groupIdB64);
        probeNotBefore.delete(groupIdB64);
        // eslint-disable-next-line no-console
        console.info("ds: root probe answered 204; this account holds the root");
      } else {
        // eslint-disable-next-line no-console
        console.info("ds: root probe answered 401; still using the registrar seed");
      }
    } catch (err) {
      // eslint-disable-next-line no-console
      console.info("ds: root probe failed; retried later", err instanceof Error ? err.name : "error");
    }
  }

  function scheduleReconcile(groupIdB64: string): void {
    const account = accountId();
    if (account === null || wasmNow === null) return;
    const inFlight = running.get(groupIdB64);
    if (inFlight !== undefined) {
      inFlight.rerun = true;
      return;
    }
    const wait = notBefore.get(groupIdB64);
    if (wait !== undefined && Date.now() < wait) return;
    const state = { rerun: false };
    running.set(groupIdB64, state);
    void (async () => {
      try {
        do {
          state.rerun = false;
          await reconcileOnce(account, groupIdB64);
        } while (state.rerun && accountId() === account);
      } finally {
        running.delete(groupIdB64);
      }
    })();
  }

  async function reconcileOnce(account: string, groupIdB64: string): Promise<void> {
    const wasm = wasmNow;
    const entry = chats.get(groupIdB64);
    if (wasm === null || entry === undefined) return;
    if (readIsActive(entry.group) === false) return;
    const own = conversationIdentityNow();
    if (own === null) return;
    const groupId = base64Decode(groupIdB64);
    const ownB64 = base64Encode(own.signaturePublicKey);
    const rosterNow = rosterOf(entry.group);
    // ADR-0057 obligation 2 - the owner claims on open (once per session). A
    // `204` marks the root held, so the probe below is skipped and the
    // appointments proceed in this same run.
    if (rosterNow?.ownerAccountKeyB64 === ownB64) {
      const claim = await ds.ensureOwnerClaim(groupId);
      if (claim === "claimed") {
        // eslint-disable-next-line no-console
        console.info("ds: the owner claimed its journal on open");
      }
      if (accountId() !== account) return;
    }
    await probeIfUnheld(account, groupIdB64, rosterNow, ownB64);
    if (accountId() !== account) return;
    // An owner whose root is not confirmed yet (a takeover still to land)
    // appoints nothing: the DS would refuse its root.
    if (rosterNow?.ownerAccountKeyB64 === ownB64 && !isRootHeld(account, groupIdB64)) return;
    try {
      const outcome = await reconcileAppointments({
        groupId,
        roster: () => rosterOf(entry.group),
        ownAccountKeyB64: () => ownB64,
        authority: () => dsAccess.journalAuthority(groupId),
        derive: wasmRegistrarDerivation(wasm),
        sender: ds,
        loadRecord: () => loadAppointmentRecord(account, groupIdB64),
        saveRecord: (record) => saveAppointmentRecord(account, groupIdB64, record),
        lastSentSet: () => sentSets.get(groupIdB64) ?? null,
        noteSentSet: (fingerprint) => sentSets.set(groupIdB64, fingerprint),
        memberX25519,
        composeEnvelope: (memberAccountKey, x25519, rank, seed) =>
          wasm.composeRegistrarSeedEnvelope(groupId, memberAccountKey, x25519, rank, seed),
        sendEnvelope: async (envelope) => {
          const secrets = auth.identitySecrets();
          if (secrets === null) throw new Error("signed out");
          await publisher.broadcastControl(secrets, entry.group, envelope);
        },
      });
      // ADR-0087 section 5 - the owner hands each admin the current knock
      // seed, sealed to the admin's account key, once per session per version.
      if (rosterNow?.ownerAccountKeyB64 === ownB64 && rosterNow !== undefined) {
        await sendKnockSeedsToAdmins(wasm, entry, groupId, groupIdB64, rosterNow.adminAccountKeysB64, ownB64);
      }
      if (outcome.kind === "reconciled" && outcome.pending.length > 0) {
        notBefore.set(groupIdB64, Date.now() + PENDING_RETRY_MS);
      } else {
        notBefore.delete(groupIdB64);
      }
    } catch (err) {
      notBefore.set(groupIdB64, Date.now() + PENDING_RETRY_MS);
      // eslint-disable-next-line no-console
      console.warn("ds: registrar appointment failed; retried later", err);
    }
  }

  /** Every account identity this device holds, current first. */
  function ownIdentitiesNow(): WasmIdentity[] {
    const out: WasmIdentity[] = [];
    const current = accountIdentityNow();
    if (current !== null) out.push(current);
    for (const p of [...chats.ownPredecessorIdentities()].reverse()) out.push(p);
    return out;
  }

  /**
   * ADR-0087 section 5 - the owner's knock seeds of `version`: from the root
   * of the identity the recorded boundary names (`null`: the current account
   * identity), then the same version under every other own identity, for an
   * owner device that never saw the boundary. Empty when this account does
   * not own the group. The caller wipes them.
   */
  function ownerKnockSeeds(groupId: Uint8Array, version: number, rootKeyB64: string | null): Uint8Array[] {
    const wasm = wasmNow;
    const entry = chats.get(base64Encode(groupId));
    if (wasm === null || entry === undefined) return [];
    const identities = ownIdentitiesNow();
    if (identities.length === 0) return [];
    if (!ownsGroup(entry.group, [conversationIdentityNow()?.signaturePublicKey, ...identities.map((i) => i.signaturePublicKey)])) {
      return [];
    }
    const named = rootKeyB64 === null ? identities[0] : identities.find((i) => base64Encode(i.signaturePublicKey) === rootKeyB64);
    const order = named === undefined ? identities : [named, ...identities.filter((i) => i !== named)];
    const out: Uint8Array[] = [];
    for (const identity of order) {
      const root = wasm.dsJournalRootSeed(identity.ed25519Seed, groupId);
      try {
        out.push(wasm.groupKnockSeed(root, version));
      } finally {
        root.fill(0);
      }
    }
    return out;
  }

  async function sendKnockSeedsToAdmins(
    wasm: Awaited<ReturnType<typeof loadWasm>>,
    entry: NonNullable<ReturnType<typeof chats.get>>,
    groupId: Uint8Array,
    groupIdB64: string,
    admins: ReadonlySet<string>,
    ownAccountKeyB64: string,
  ): Promise<void> {
    const account = accountIdentityNow();
    const accountIdB64 = accountId();
    if (account === null || accountIdB64 === null) return;
    // Removing (or demoting) an admin bumps the version; only the admins
    // left get the new seed. Older versions stay derivable here, so a
    // request sealed to an older key still opens.
    // The public record names the knock key: its republish is recorded as
    // owed, durably, before the bump itself is written.
    const { version, bumped } = await knockSeeds.reconcileOwnerAdmins(accountIdB64, groupIdB64, admins, () =>
      noteKnockKeyChanged(accountIdB64, groupIdB64),
    );
    if (bumped) {
      // eslint-disable-next-line no-console
      console.info("ds: an admin left; the group's knock key moved to a new version");
    }
    // The owner's other devices learn the version from an envelope addressed
    // to this account, once there is more than the initial one.
    // The admins get the current version. The owner's own other devices get
    // every version from the first to the current one (another platform
    // stores the seeds it is given and derives none), sealed to this
    // account's own key.
    const sends: { readonly to: string; readonly version: number }[] = [];
    for (const adminB64 of admins) sends.push({ to: adminB64, version });
    if (version > INITIAL_KNOCK_VERSION) {
      for (let v = INITIAL_KNOCK_VERSION; v <= version; v++) sends.push({ to: ownAccountKeyB64, version: v });
    }
    for (const send of sends) {
      if (knockSeedSent(groupIdB64, send.to, send.version)) continue;
      const x =
        send.to === ownAccountKeyB64
          ? (conversationIdentityNow()?.x25519PublicKey ?? null)
          : await memberX25519(send.to);
      if (x === null) continue;
      const seeds = ownerKnockSeeds(groupId, send.version, await knockSeeds.ownerRootKey(groupIdB64, send.version));
      const seed = seeds[0];
      for (const other of seeds.slice(1)) other.fill(0);
      if (seed === undefined) return;
      try {
        const envelope = wasm.composeKnockSeedEnvelope(groupId, base64Decode(send.to), x, send.version, seed);
        const secrets = auth.identitySecrets();
        if (secrets === null) return;
        await publisher.broadcastControl(secrets, entry.group, envelope);
        noteKnockSeedSent(groupIdB64, send.to, send.version);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("ds: knock seed not sent; retried at the next reconcile", err instanceof Error ? err.name : "");
      } finally {
        seed.fill(0);
      }
    }
  }

  // ADR-0057 root handover, sending side: a commit moved ownership away from
  // this account. Recorded as owed (group ids only, so a closed tab resumes),
  // the appointment record for the group forgotten (this device no longer
  // appoints under its root), and the handover scheduled outside the lock the
  // apply path holds. The record stays until this account's probe answers 401
  // (the root moved); meanwhile the envelope is re-sent at most every
  // HANDOVER_RESEND_MS, on session start and on group activity.
  const handoverRunning = new Map<string, { rerun: boolean }>();
  const handoverNotBefore = new Map<string, number>();
  const handoverSentAt = new Map<string, number>();

  function scheduleHandover(groupIdB64: string): void {
    const account = accountId();
    if (account === null || wasmNow === null) return;
    const inFlight = handoverRunning.get(groupIdB64);
    if (inFlight !== undefined) {
      inFlight.rerun = true;
      return;
    }
    const wait = handoverNotBefore.get(groupIdB64);
    if (wait !== undefined && Date.now() < wait) return;
    const state = { rerun: false };
    handoverRunning.set(groupIdB64, state);
    void (async () => {
      try {
        do {
          state.rerun = false;
          await handoverOnce(account, groupIdB64);
        } while (state.rerun && accountId() === account);
      } finally {
        handoverRunning.delete(groupIdB64);
      }
    })();
  }

  async function handoverOnce(account: string, groupIdB64: string): Promise<void> {
    const wasm = wasmNow;
    if (wasm === null) return;
    if (!loadOwedHandovers(account).has(groupIdB64)) return;
    const entry = chats.get(groupIdB64);
    if (entry === undefined) return;
    if (readIsActive(entry.group) === false) {
      clearHandoverOwed(account, groupIdB64);
      return;
    }
    const groupId = base64Decode(groupIdB64);
    const outcome = await runOwedHandover({
      groupId,
      ownAccountKey: () => conversationIdentityNow()?.signaturePublicKey ?? null,
      liveOwner: () => {
        try {
          return entry.group.roleTableOwner();
        } catch {
          return undefined;
        }
      },
      ownerX25519: memberX25519,
      rootSeed: () => {
        const own = accountIdentityNow();
        if (own === null) throw new Error("no account identity");
        return wasm.dsJournalRootSeed(own.ed25519Seed, groupId);
      },
      compose: (newOwner, x25519, rootSeed) =>
        wasm.composeRootHandoverEnvelope(groupId, newOwner, x25519, rootSeed),
      sendEnvelope: async (envelope) => {
        const secrets = auth.identitySecrets();
        if (secrets === null) throw new Error("signed out");
        await publisher.broadcastControl(secrets, entry.group, envelope);
      },
      probeOwnRoot: () => probeOwnRoot(groupId),
      lastSentAt: () => handoverSentAt.get(groupIdB64) ?? null,
      noteSent: (at) => handoverSentAt.set(groupIdB64, at),
      now: () => Date.now(),
    });
    if (accountId() !== account) return;
    if (outcome === "done" || outcome === "cancelled") {
      clearHandoverOwed(account, groupIdB64);
      handoverNotBefore.delete(groupIdB64);
      handoverSentAt.delete(groupIdB64);
    } else if (outcome === "sent" || outcome === "throttled") {
      handoverNotBefore.set(groupIdB64, (handoverSentAt.get(groupIdB64) ?? Date.now()) + HANDOVER_RESEND_MS);
    } else {
      handoverNotBefore.set(groupIdB64, Date.now() + PENDING_RETRY_MS);
    }
  }

  const unsubscribeTransition = installOwnerTransitionHandler((t) => {
    const account = accountId();
    if (account === null) return;
    const own = conversationIdentityNow()?.signaturePublicKey ?? null;
    if (own === null) return;
    // The marker describes the DS, which moves only at the takeover: a
    // transition to or from this account clears it (a new owner signs with
    // its registrar seed until its takeover or probe confirms the root).
    if (sameBytes(t.before, own) || sameBytes(t.after, own)) {
      clearRootHeld(account, t.groupIdB64);
      probeNotBefore.delete(t.groupIdB64);
    }
    if (ownerMovedAwayFrom(own, t.before, t.after) === null) return;
    markHandoverOwed(account, t.groupIdB64);
    clearAppointmentRecord(account, t.groupIdB64);
    sentSets.delete(t.groupIdB64);
    notBefore.delete(t.groupIdB64);
    handoverNotBefore.delete(t.groupIdB64);
    handoverSentAt.delete(t.groupIdB64);
    scheduleHandover(t.groupIdB64);
  });

  // ADR-0057 root handover, receiving side: this account became owner and a
  // device of the old owner handed the root seed over. The seed is kept
  // encrypted under the device key (`pendingRootSeeds`) until the takeover
  // lands or answers 401; a network failure retries on group activity and on
  // the next session start. Runs detached from the receive loop (which holds
  // the group's MLS lock).
  const takeoverRunning = new Set<string>();
  const takeoverNotBefore = new Map<string, number>();

  function takeOver(_groupId: Uint8Array, groupIdB64: string, oldRootSeed: Uint8Array): void {
    const account = accountId();
    if (account === null) {
      oldRootSeed.fill(0);
      return;
    }
    void (async () => {
      try {
        await pendingRootSeeds.set(groupIdB64, oldRootSeed);
      } catch (err) {
        // Kept in memory for this session all the same.
        // eslint-disable-next-line no-console
        console.warn("ds: pending root seed not persisted", err instanceof Error ? err.name : "error");
      } finally {
        oldRootSeed.fill(0);
      }
      takeoverNotBefore.delete(groupIdB64);
      scheduleTakeover(groupIdB64);
    })();
  }

  // ADR-0074 section 3.5: after a key rotation every conversation journal
  // this account owns is taken over by the root of the new seed, exactly as a
  // handover to oneself.
  const uninstallRotationTakeOver = installRotationTakeOver(async (oldAccountEd25519Seed) => {
    const wasm = wasmNow ?? (await loadWasm());
    const own = conversationIdentityNow();
    if (own === null) return 0;
    let started = 0;
    for (const entry of chats.groups()) {
      let owner: Uint8Array | undefined;
      try {
        owner = entry.group.roleTableOwner();
      } catch {
        continue;
      }
      if (!sameBytes(owner, own.signaturePublicKey)) continue;
      const groupId = base64Decode(entry.groupIdB64);
      takeOver(groupId, entry.groupIdB64, wasm.dsJournalRootSeed(oldAccountEd25519Seed, groupId));
      started += 1;
    }
    return started;
  });
  onCleanup(uninstallRotationTakeOver);

  // ADR-0087 section 5 - the rotation moved the journal root, and every knock
  // seed with it: each owned group's knock version goes one up under the new
  // root, the boundary is recorded (earlier versions stay with the
  // predecessor's root, which this device keeps), the public record's
  // republish is recorded first, and the reconcile hands the new seed to the
  // admins. A failure throws, which keeps the rotation step owed.
  const uninstallRotationKnockKeys = installRotationKnockKeys(async (oldAccountKeyB64) => {
    const account = accountId();
    const current = accountIdentityNow();
    if (account === null || current === null) throw new Error("no session");
    const newKeyB64 = base64Encode(current.signaturePublicKey);
    if (newKeyB64 === oldAccountKeyB64) throw new Error("the rotation has not landed here yet");
    let moved = 0;
    for (const entry of chats.groups()) {
      if (readIsActive(entry.group) === false) continue;
      if (
        !ownsGroup(entry.group, [
          conversationIdentityNow()?.signaturePublicKey,
          current.signaturePublicKey,
          base64Decode(oldAccountKeyB64),
        ])
      ) {
        continue;
      }
      const result = await knockSeeds.rotateOwnerRoot(account, entry.groupIdB64, oldAccountKeyB64, newKeyB64, () =>
        noteKnockKeyChanged(account, entry.groupIdB64),
      );
      if (result.bumped) moved += 1;
      scheduleReconcile(entry.groupIdB64);
    }
    return moved;
  });
  onCleanup(uninstallRotationKnockKeys);

  function scheduleTakeover(groupIdB64: string): void {
    const account = accountId();
    if (account === null || wasmNow === null) return;
    if (takeoverRunning.has(groupIdB64)) return;
    const wait = takeoverNotBefore.get(groupIdB64);
    if (wait !== undefined && Date.now() < wait) return;
    const seed = pendingRootSeeds.get(groupIdB64);
    if (seed === undefined) return;
    takeoverRunning.add(groupIdB64);
    void (async () => {
      try {
        await takeoverOnce(account, groupIdB64, seed);
      } finally {
        seed.fill(0);
        takeoverRunning.delete(groupIdB64);
      }
    })();
  }

  async function takeoverOnce(account: string, groupIdB64: string, oldRootSeed: Uint8Array): Promise<void> {
    const wasm = wasmNow;
    const entry = chats.get(groupIdB64);
    if (wasm === null || entry === undefined) return;
    const groupId = base64Decode(groupIdB64);
    const outcome = await takeOverJournal(
      {
        groupId,
        sender: ds,
        signerOf: (seed) => wasm.dsStatementSigner(seed),
        ownRootPublicKey: () => {
          const own = accountIdentityNow();
          if (own === null) throw new Error("no account identity");
          const signer = wasm.dsJournalRootSigner(own, groupId);
          try {
            return signer.publicKey();
          } finally {
            signer.free();
          }
        },
        stillOwner: () => dsAccess.ownsJournal(groupId),
        desiredRegistrars: () => {
          const roster = rosterOf(entry.group);
          const own = conversationIdentityNow();
          if (roster === null || own === null) return null;
          return desiredSetFor(roster, base64Encode(own.signaturePublicKey));
        },
        derive: wasmRegistrarDerivation(wasm),
        markRootHeld: () => markRootHeld(account, groupIdB64),
        probeOwnRoot: () => probeOwnRoot(groupId),
        authority: () => dsAccess.journalAuthority(groupId),
        resetAppointments: async () => {
          clearAppointmentRecord(account, groupIdB64);
          sentSets.delete(groupIdB64);
          notBefore.delete(groupIdB64);
          await registrarSeeds.delete(groupIdB64);
        },
        reconcile: () => scheduleReconcile(groupIdB64),
      },
      oldRootSeed,
    );
    if (accountId() !== account) return;
    if (outcome === "failed") {
      takeoverNotBefore.set(groupIdB64, Date.now() + PENDING_RETRY_MS);
      // eslint-disable-next-line no-console
      console.info("ds: root takeover failed; retried later");
      return;
    }
    // Landed, done elsewhere, or answered 401: the seed is no longer needed.
    takeoverNotBefore.delete(groupIdB64);
    try {
      await pendingRootSeeds.delete(groupIdB64);
    } catch {
      /* best effort: a stale seed only earns another 401 */
    }
    if (outcome !== "took-over") {
      // eslint-disable-next-line no-console
      console.info(`ds: root handover ended ${outcome}`);
    }
  }

  // A claim that landed anywhere (on open, before a registration, or lazily
  // after a refused request) runs the registrar reconcile for that group.
  // It also ends that group's sweep backoff: the refusal it was waiting out
  // may have been the missing claim (`chat/sweep-backoff.ts`).
  const unsubscribeClaimed = dsAccess.onOwnerClaimed((groupId) => {
    const groupIdB64 = base64Encode(groupId);
    sweepBackoff.reset(groupIdB64);
    scheduleReconcile(groupIdB64);
  });

  const unsubscribeIdle = onGroupMlsIdle((groupIdB64) => {
    scheduleReconcile(groupIdB64);
    const account = accountId();
    if (account !== null && loadOwedHandovers(account).has(groupIdB64)) scheduleHandover(groupIdB64);
    if (pendingRootSeeds.get(groupIdB64) !== undefined) scheduleTakeover(groupIdB64);
  });

  let uninstallHandler: (() => void) | null = null;
  let uninstallKnockHandler: (() => void) | null = null;
  let boundAccount: string | null = null;
  createEffect(() => {
    const account = accountId();
    untrack(() => {
      if (account === boundAccount) return;
      boundAccount = account;
      uninstallHandler?.();
    uninstallKnockHandler?.();
      uninstallHandler = null;
      uninstallKnockHandler?.();
      uninstallKnockHandler = null;
      installOwnerKnockSeedSource(null);
      knockSeeds.close();
      running.clear();
      notBefore.clear();
      sentSets.clear();
      probeNotBefore.clear();
      handoverRunning.clear();
      handoverNotBefore.clear();
      handoverSentAt.clear();
      takeoverRunning.clear();
      takeoverNotBefore.clear();
      sweepBackoff.clear();
      dropDerived();
      journalRepair.install(null);
      bindRegistrarListAccount(account);
      if (account === null) {
        registrarSeeds.close();
        pendingRootSeeds.close();
        dsAccess.setAuthorityFactory(null);
        return;
      }
      void (async () => {
        const wasm = await loadWasm();
        if (accountId() !== account) return;
        wasmNow = wasm;
        await registrarSeeds.open(account, new IdbRegistrarSeedBackend(getDeviceKey));
        if (accountId() !== account) return;
        await pendingRootSeeds.open(account, new IdbPendingRootSeedBackend(getDeviceKey));
        if (accountId() !== account) return;
        await knockSeeds.open(account);
        // UX section 8: where each request to join came from.
        await openKnockOrigins(account);
        // A joiner's reply grant, kept until the Welcome is written (C-69).
        await openJoinReplies(account);
        if (accountId() !== account) return;
        dsAccess.setAuthorityFactory(
          wasmJournalAuthorityFactory(wasm, accountIdentityNow, registrarSeeds, {
            held: (groupIdB64) => isRootHeld(account, groupIdB64),
            mark: (groupIdB64) => markRootHeld(account, groupIdB64),
          }),
        );
        // Finding 1 - the repair check signs with this device's authority
        // and reads the key of the epoch the group is at.
        journalRepair.install({
          authority: (groupId) => dsAccess.journalAuthority(groupId),
          currentEpochKey: (groupId) => {
            const { current } = dsAccess.groupSigners(groupId);
            return current === null ? null : { epoch: current.epoch, key: current.verifyingKey() };
          },
          sender: ds,
        });
        const decode = (plaintext: Uint8Array): RegistrarSeedHeader | null => {
          try {
            const d = wasm.decodeRegistrarSeedEnvelope(plaintext);
            try {
              return {
                journalId: d.journalId,
                memberAccountKey: d.memberAccountKey,
                rank: d.rank,
                registrarPublicKey: d.registrarPublicKey,
              };
            } finally {
              d.free();
            }
          } catch {
            return null;
          }
        };
        const open = (plaintext: Uint8Array): Uint8Array => {
          // Addressed to the member's account key as the roster names it;
          // sealed to that identity's X25519 key, or after a rotation to the
          // current identity's (ADR-0074 section 3.7).
          const seed = openOwnSealed(
            (a) => wasm.openRegistrarSeedEnvelope(plaintext, a),
            (a, c) => wasm.openRegistrarSeedEnvelopeSealedTo(plaintext, a, c),
          );
          if (seed === null) throw new Error("registrar seed did not open");
          return seed;
        };
        const ownAccountKey = (): Uint8Array | null =>
          conversationIdentityNow()?.signaturePublicKey ?? null;
        uninstallHandler = installRegistrarSeedHandler(async (arrival) => {
          const header = decode(arrival.plaintext);
          // ADR-0057 (amended 2026-10-01) item 1 - every envelope the owner
          // sent, whoever it is addressed to, adds to the registrar list an
          // admin revokes from when it commits a removal.
          noteRegistrarListArrival(arrival.groupIdB64, {
            header,
            groupId: arrival.groupId,
            senderAccountKey: arrival.senderAccountKey,
            ownerAccountKey: arrival.ownerAccountKey,
          });
          // A rank-root envelope is a root handover, never a registrar seed
          // to keep (the registrar gate refuses its rank as well).
          if (header !== null && header.rank === RANK_ROOT_HANDOVER) {
            return receiveRootHandover(
              { open, ownAccountKey, takeOver },
              {
                header,
                groupId: arrival.groupId,
                groupIdB64: arrival.groupIdB64,
                plaintext: arrival.plaintext,
                ownerAccountKey: arrival.ownerAccountKey,
              },
            );
          }
          return receiveRegistrarSeed(
            {
              decode: () => header,
              open,
              ownAccountKey,
              keep: (groupIdB64, seed) => registrarSeeds.set(groupIdB64, seed),
            },
            arrival,
          );
        });
        // ADR-0087 section 5 - the owner's own knock seeds, derived.
        installOwnerKnockSeedSource((groupId, version, rootKeyB64) => ownerKnockSeeds(groupId, version, rootKeyB64));
        // ADR-0087 section 5 - a knock seed the owner handed this admin.
        uninstallKnockHandler = installKnockSeedHandler(async (arrival) => {
          let header: KnockSeedHeader | null = null;
          try {
            const d = wasm.decodeKnockSeedEnvelope(arrival.plaintext);
            try {
              header = {
                journalId: d.journalId,
                memberAccountKey: d.memberAccountKey,
                knockVersion: d.knockVersion,
                knockPublicKey: d.knockPublicKey,
              };
            } finally {
              d.free();
            }
          } catch {
            header = null;
          }
          const ownKey = ownAccountKey();
          if (ownKey === null) return "not-addressed";
          // Another device of the owner moved the group's knock version (an
          // admin left, or a key rotation moved the root): the clear header
          // says to which version, and its public key which of this account's
          // journal roots it was made under, whoever it is addressed to.
          if (header !== null && base64Encode(header.journalId) === arrival.groupIdB64) {
            const identities = ownIdentitiesNow();
            const ownKeys = new Set(identities.map((i) => base64Encode(i.signaturePublicKey)));
            const conv = conversationIdentityNow();
            if (conv !== null) ownKeys.add(base64Encode(conv.signaturePublicKey));
            const sender = arrival.senderAccountKey === undefined ? null : base64Encode(arrival.senderAccountKey);
            const owner = arrival.ownerAccountKey === undefined ? null : base64Encode(arrival.ownerAccountKey);
            if (sender !== null && owner !== null && ownKeys.has(sender) && ownKeys.has(owner)) {
              let rootKeyB64: string | null = null;
              for (const identity of identities) {
                const root = wasm.dsJournalRootSeed(identity.ed25519Seed, arrival.groupId);
                const seed = wasm.groupKnockSeed(root, header.knockVersion);
                root.fill(0);
                const pk = wasm.groupKnockPublicKey(seed);
                seed.fill(0);
                if (base64Encode(pk) === base64Encode(header.knockPublicKey)) {
                  rootKeyB64 = base64Encode(identity.signaturePublicKey);
                  break;
                }
              }
              const current = identities[0];
              if (current !== undefined) {
                await knockSeeds.learnOwnerRoot(
                  account,
                  arrival.groupIdB64,
                  header.knockVersion,
                  rootKeyB64,
                  base64Encode(current.signaturePublicKey),
                );
              }
            }
          }
          const verdict = knockSeedVerdict({
            header,
            groupId: arrival.groupId,
            ownAccountKey: ownKey,
            senderAccountKey: arrival.senderAccountKey,
            ownerAccountKey: arrival.ownerAccountKey,
          });
          if (verdict !== "accept" || header === null) return verdict;
          if (conversationIdentityNow() === null) return "not-addressed";
          // ADR-0074 section 3.7 - after a rotation the owner seals to the
          // current identity's X25519 key (the directory's).
          const seed = openOwnSealed(
            (a) => wasm.openKnockSeedEnvelope(arrival.plaintext, a),
            (a, c) => wasm.openKnockSeedEnvelopeSealedTo(arrival.plaintext, a, c),
          );
          if (seed === null) return "malformed";
          if (seed.length !== 32) return "malformed";
          await knockSeeds.keep(account, arrival.groupIdB64, header.knockVersion, seed);
          return "accept";
        });
        // Once per group at session start: a join that landed while this
        // tab was closed still gets its registrar, and a handover this
        // account still owes is sent.
        for (const g of chats.groups()) scheduleReconcile(g.groupIdB64);
        for (const g of loadOwedHandovers(account)) scheduleHandover(g);
        for (const g of pendingRootSeeds.groups()) scheduleTakeover(g);
      })();
    });
  });

  onCleanup(() => {
    journalRepair.install(null);
    bindRegistrarListAccount(null);
    unsubscribeClaimed();
    unsubscribeIdle();
    unsubscribeTransition();
    unregisterOwner();
    uninstallHandler?.();
    dropDerived();
  });

  return props.children;
}
