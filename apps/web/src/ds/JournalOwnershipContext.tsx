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
    const account = accountIdentityNow();
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
    onPersist: async () => {
      const account = accountId();
      if (account === null) return;
      await chats.persistSnapshot(account);
    },
  });

  /** The member's account X25519 key from its identity bundle, verified to
   *  be that account's bundle. */
  async function memberX25519(accountKeyB64: string): Promise<Uint8Array | null> {
    const hex = profiles.accountIdHexOf(accountKeyB64);
    if (hex === null) return null;
    const accountIdBytes = hexToBytes(hex);
    if (accountIdBytes === null || accountIdBytes.length !== 16) return null;
    const bundle = await discovery.identityFetch(accountIdBytes);
    if (bundle === null) return null;
    if (bundle.signature_pubkey !== accountKeyB64) {
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
    const own = accountIdentityNow();
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
      ownAccountKey: () => accountIdentityNow()?.signaturePublicKey ?? null,
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
    const own = accountIdentityNow()?.signaturePublicKey ?? null;
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
          const own = accountIdentityNow();
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
  let boundAccount: string | null = null;
  createEffect(() => {
    const account = accountId();
    untrack(() => {
      if (account === boundAccount) return;
      boundAccount = account;
      uninstallHandler?.();
      uninstallHandler = null;
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
          const identity = accountIdentityNow();
          if (identity === null) throw new Error("no account identity");
          return wasm.openRegistrarSeedEnvelope(plaintext, identity);
        };
        const ownAccountKey = (): Uint8Array | null => accountIdentityNow()?.signaturePublicKey ?? null;
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
