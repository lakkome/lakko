// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Background receive: every joined group whose chat view is NOT open, for the
 * whole signed-in session.
 *
 * Before this, a group was received only while its chat view was mounted, so a
 * 1x1 call offer, a group-call announcement or a message for any other chat sat
 * on the DS until the user opened it. The scheduling rules — short polls in a
 * bounded round robin rather than a long poll per group, and why — live in
 * `background-receive-scheduler.ts`; the hand-off with the chat view (one
 * shared cursor per group, who polls, and the decrypted events a view has not
 * seen yet) in `receive-coordinator.ts`; the destination of every event kind in
 * `background-events.ts`.
 *
 * Correctness rides on the SAME path the chat view uses:
 * `ChatClient.pollMessages` applies under `withGroupMlsLock`, re-reads the
 * shared cursor under that lock (a batch another path consumed is dropped as
 * `cursor-moved`), and advances the cursor only once the MLS snapshot is
 * durable (C-67: the cursor may lag the durable state, never lead it). Rows are
 * then filed into the message log through the serialized
 * `updateMessageLog` + `appendToGroupSlot` merge, so a chat view's later write
 * for the same group cannot erase them.
 *
 * Runs while signed in (with identity secrets and the canonical identity) and
 * the loop is not allowed to park. When it may park is `receive-activity.ts`:
 * only a hidden tab that a push wake is KNOWN to reach, so a tab with
 * notifications denied — which has no other way to learn of an incoming call —
 * keeps receiving. A call ringing, starting or live overrides that in any
 * case, because the call rides a group journal. That call's group gets a dedicated long poll
 * for as long as the call lasts and its chat is not open: one connection, held
 * only then. For a 1x1 call an answer or ICE candidate that waits for the next
 * sweep stalls the connection; for a group (SFU) call a commit that waits leaves
 * this device sealing media under a key the other participants have already
 * rotated away from (the mid-call rekey, ADR-0029, fires when the poll that
 * applies the commit drains the group's MLS lock).
 */
import {
  createContext,
  createEffect,
  createSignal,
  onCleanup,
  untrack,
  useContext,
  type JSX,
} from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { getDeviceKey } from "../auth/key-store";
import { useCallHost } from "../calls/CallHostContext";
import { isDsNetworkFailure } from "../ds/client";
import {
  createWakeLatch,
  LONG_POLL_WAIT_SECS,
  runLongPollLoop,
  sleepCancellable,
} from "../ds/long-poll-loop";
import { useProfiles } from "../profile/ProfileContext";
import { observeMembers } from "../profile/membership-baseline";
import { isOwnKeyB64 } from "../profile/own-key";
import {
  memberSignatureKeys,
  readPushRotationRole,
  receivePushRoutingSecretGated,
  rotatePushRoutingSecretIfMembershipShrank,
} from "../push/group-routing";
import { createPushWakeReach } from "../push/wake-reach";
import { backgroundReceiveIsActive } from "./receive-activity";
import { base64Decode, base64Encode } from "../auth/account-id";
import { ContactsClient } from "../contacts/client";
import { DiscoveryClient } from "../discovery/client";
import { loadWasm } from "../wasm";
import {
  appendToGroupSlot,
  eventToPersistedMessage,
} from "./background-log";
import { planBackgroundEvents, type MessageOpEvent } from "./background-events";
import { foldOpsIntoStored } from "./message-ops-store";
import {
  BACKGROUND_CONCURRENCY,
  BACKGROUND_SWEEP_INTERVAL_MS,
  BACKGROUND_SWEEP_JITTER_MS,
  GroupBackoff,
  MAX_IMMEDIATE_REPOLLS,
  runBackgroundReceiveLoop,
  runSweep,
  type GroupPollResult,
  type SweepDeps,
} from "./background-receive-scheduler";
import { onGroupCheckRequested, reportGroupCheck, resetFeedRefresh } from "../feed/feed-refresh-store";
import { useChats, type ActiveGroup } from "./ChatContext";
import { ChatClient, type IncomingChatEvent } from "./client";
import {
  decideActingCommitter,
  recordBeaconObservation,
  type ActingCommitter,
} from "./committer-beacon";
import { useGroupNames } from "./GroupNameContext";
import { usePins } from "./PinContext";
import { useSharedChannels } from "../discover/SharedChannelsContext";
import { useReadWatermarks } from "./ReadWatermarkContext";
import { readIsActive } from "./membership-ended";
import {
  loadMessageLog,
  MESSAGE_LOG_VERSION,
  updateMessageLog,
} from "./message-log-store";
import {
  clearReadmissionSlots,
  planReadmission,
  recordRemovedDevices,
  removedDevices,
  takeReadmissionSlot,
  type AccountDevices,
  type RosterLeafKeys,
} from "./device-readmission";
import { markOwnPublished } from "./own-published";
import { ownPersonKeyB64, personKeyOf, personKeysOf, type RosterLeaf } from "./person-key";
import { loadAccepted } from "../contacts/local-store";
import { proposalKey, recordObservation } from "./pending-requests";
import { ReceiveCoordinator } from "./receive-coordinator";
import type { SweepOutcome } from "./sweep";

export interface BackgroundReceiveApi {
  /** Shared with the chat view — see `receive-coordinator.ts`. */
  readonly coordinator: ReceiveCoordinator<IncomingChatEvent>;
  /**
   * Poll one group now, through the same path as the sweep, and settle what it
   * returns. For a caller that needs the group current before acting on it —
   * joining a group call from outside its chat, where the call seed has to be
   * derived at the epoch the call is at. A group whose chat view is open is
   * left to that view's own long poll (resolves at once). Rejects on a poll
   * failure.
   */
  syncGroupNow(groupIdB64: string): Promise<void>;
  /**
   * ADR-0050 section 2.8 — admit a device this device just added to the
   * DeviceGroup (QR pairing, the ADR-0049 confirmation prompt) to EVERY chat
   * this device is in. Waits, bounded, for the new device's conversation
   * KeyPackage to appear in the directory (it publishes it once it has landed),
   * then per chat: a self-service Add proposal, swept here when this device is
   * the acting committer, or a direct commit on a table-less group. Chats whose
   * view is open are proposed to and left to that view's own sweep.
   *
   * Resolves with a tally once every chat has been offered; rejects when the
   * KeyPackage never appeared within the wait (the device is then admitted by
   * a later re-admission, not by this call).
   */
  admitDevice(deviceKey: Uint8Array): Promise<DeviceFanOut>;
  /**
   * ADR-0050 section 2.10 — retire a device just removed from the DeviceGroup
   * from EVERY chat this device is in (its one leaf per chat, the account's
   * other devices untouched), then delete its directory KeyPackage.
   */
  retireDevice(deviceKey: Uint8Array): Promise<DeviceFanOut>;
}

/** What a device fan-out did, per chat. */
export interface DeviceFanOut {
  /** A proposal was published or a commit landed. */
  readonly offered: number;
  /** Nothing to do: already admitted / not present, or the membership ended. */
  readonly skipped: number;
  /** The chat threw; logged, the rest continued. */
  readonly failed: number;
}

/** How long `admitDevice` waits for the new device's KeyPackage to appear in
 *  the directory, and how often it looks. The fresh device publishes it after
 *  it has landed a session, which includes a proof-of-work solve. */
export const ADMIT_DEVICE_WAIT_MS = 180_000;
export const ADMIT_DEVICE_POLL_MS = 3_000;

const BackgroundReceiveContext = createContext<BackgroundReceiveApi>();

function hexToBytes(hex: string): Uint8Array | null {
  if (hex.length % 2 !== 0 || !/^[0-9a-f]*$/i.test(hex)) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

export function BackgroundReceiveProvider(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const groupNames = useGroupNames();
  const pins = usePins();
  const sharedChannels = useSharedChannels();
  const readWatermarks = useReadWatermarks();
  const callHost = useCallHost();

  const coordinator = new ReceiveCoordinator<IncomingChatEvent>();
  const backoff = new GroupBackoff();
  const chat = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: async () => {
      const s = auth.state();
      if (s.kind !== "session") return;
      await chats.persistSnapshot(s.account.accountId);
    },
  });
  // ADR-0045 / ADR-0050 — the membership half for chats whose view is not
  // open: the sweep at the tail of every background poll, and the device
  // fan-outs. Same persist hooks the chat view's client uses, so a commit
  // staged here is as durable as one staged there.
  const discovery = new DiscoveryClient();
  const contacts = new ContactsClient(discovery, undefined, {
    identityProvider: () => chats.identity(),
    accountIdentityProvider: () => chats.accountIdentity(),
    onPersist: async () => {
      const s = auth.state();
      if (s.kind !== "session") return;
      await chats.persistSnapshot(s.account.accountId);
    },
    onPersistWithCommit: async (entry) => {
      const s = auth.state();
      if (s.kind !== "session") {
        throw new Error("no active session — commit aborted (ADR-0040)");
      }
      await chats.persistSnapshotWithCommit(s.account.accountId, entry);
    },
  });

  // Mirrors the coordinator's claims into Solid so the dedicated call lane
  // re-evaluates when the call's chat is opened or closed.
  const [foregroundVersion, setForegroundVersion] = createSignal(0);
  coordinator.onForegroundChange(() => setForegroundVersion((v) => v + 1));

  const resume = createWakeLatch();
  let loopAbort: AbortController | null = null;
  let activeAccount: string | null = null;
  /** The journal cursors the message log holds, by group hex, read once per
   *  session. A group the coordinator has no cursor for yet starts from here —
   *  where the chat view last persisted it — rather than at 0, which would
   *  re-fetch a journal whose rows were all consumed long ago. */
  let logCursors: Promise<Map<string, number>> | null = null;
  /** The group the dedicated call lane owns, excluded from the sweep. */
  let hotGroup: string | null = null;

  function loadLogCursors(accountIdB64: string): Promise<Map<string, number>> {
    if (logCursors !== null) return logCursors;
    const attempt = (async () => {
      const out = new Map<string, number>();
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey === null) return out;
      const log = await loadMessageLog(accountIdB64, deviceKey);
      for (const [hex, slot] of Object.entries(log?.groups ?? {})) {
        const cursor = Number(slot.cursor);
        if (Number.isSafeInteger(cursor) && cursor >= 0) out.set(hex, cursor);
      }
      return out;
    })();
    logCursors = attempt;
    // A failed read is retried on the next poll instead of pinning every group
    // to 0 for the rest of the session.
    attempt.catch(() => {
      if (logCursors === attempt) logCursors = null;
    });
    return attempt;
  }

  /** The cursor a poll of `entry` starts from, seeding the coordinator from the
   *  log the first time. */
  async function startCursor(accountIdB64: string, entry: ActiveGroup): Promise<number> {
    const known = coordinator.cursor(entry.groupIdB64);
    if (known !== undefined) return known;
    const fromLog = (await loadLogCursors(accountIdB64)).get(hexOf(entry)) ?? 0;
    // `noteCursor` keeps the maximum, so a chat view that noted a later
    // position while the log was being read still wins.
    return coordinator.noteCursor(entry.groupIdB64, fromLog);
  }

  function isHidden(): boolean {
    return globalThis.document?.visibilityState === "hidden";
  }

  /**
   * How long a push wake keeps a HIDDEN tab sweeping.
   *
   * A hidden tab that push CAN reach parks its sweep — it is nobody's
   * foreground and polling it burns battery for nothing — and the wake is
   * what brings it back. Without this window the wake would be inert in the
   * one case it exists for, and for a call that is a defect rather than a
   * delay: the offer never reaches the tab, so nothing rings in-app, and
   * Decline pressed on the background notification (which deliberately does
   * NOT raise the window — see `public/sw-call-notification.js`) has no call
   * to refuse and the caller goes on ringing for the full 45 s.
   *
   * Sized to the stale-offer window: past 60 s an offer no longer rings
   * anyway, so a longer window could only poll for a call nobody would
   * answer. Accept does not depend on this — focusing the window makes the
   * tab visible and the sweep resumes on its own — which is exactly why the
   * window exists for the button that does not focus.
   *
   * A tab push CANNOT reach never parks in the first place, so this window
   * never applies to it; see `chat/receive-activity.ts`.
   */
  const WAKE_ACTIVE_MS = 60_000;
  /** Wall-clock deadline of the current wake window; 0 when none is open. */
  let wakeUntilMs = 0;

  /** Whether a push wake can reach this tab at all. Cached; see
   *  `push/wake-reach.ts` for why the answer has three values and not two. */
  const pushWakeReach = createPushWakeReach();

  function isActive(): boolean {
    return backgroundReceiveIsActive({
      signedIn: activeAccount !== null,
      documentHidden: isHidden(),
      callInProgress: callHost.anyCallInProgress(),
      wakeWindowOpen: Date.now() < wakeUntilMs,
      pushWake: pushWakeReach.read(),
    });
  }

  function isExcluded(groupIdB64: string): boolean {
    if (coordinator.isForeground(groupIdB64)) return true;
    if (groupIdB64 === hotGroup) return true;
    const entry = chats.get(groupIdB64);
    // ADR-0045 UX §6.2.8 — a group this device is no longer in is never polled:
    // every row would be refused and logged, forever.
    return entry === undefined || readIsActive(entry.group) === false;
  }

  /**
   * One poll of one group, through the chat view's own path, and everything
   * hung off the batch. Returns whether journal rows were consumed.
   */
  async function pollGroup(
    groupIdB64: string,
    waitSecs: number,
    signal: AbortSignal,
  ): Promise<GroupPollResult> {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    const entry = chats.get(groupIdB64);
    if (s.kind !== "session" || secrets === null || entry === undefined) {
      return { yieldedRows: false };
    }
    const account = s.account.accountId;
    const start = await startCursor(account, entry);
    let membersBefore = memberSignatureKeys(entry.group);
    const result = await chat.pollMessages(
      secrets,
      entry.group,
      start,
      waitSecs,
      {
        read: () => coordinator.cursor(groupIdB64) ?? start,
        advance: (to) => {
          coordinator.noteCursor(groupIdB64, to);
        },
        beforeApply: () => {
          membersBefore = memberSignatureKeys(entry.group);
        },
      },
      signal,
    );
    if (result.outcome === "cursor-moved") return { yieldedRows: false };
    const end = coordinator.cursor(groupIdB64) ?? start;
    await settle(account, entry, result.events, end > start, membersBefore);
    // ADR-0045 — the sweep trigger, exactly as at the tail of the chat view's
    // poll: it is the only thing that makes a pending proposal ever land, and
    // before this a proposal in a chat nobody had open waited until somebody
    // opened it. Cheap when nothing is pending; never awaited by the poll's
    // own error path — a sweep failure is not a poll failure.
    void sweepGroup(entry).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("background receive: tail sweep failed", err);
    });
    // ADR-0050 section 2.4 — the scheduled re-admission check, bounded per
    // chat; see `device-readmission.ts`.
    void readmitFor(entry).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("ADR-0050 re-admission check failed", err);
    });
    return { yieldedRows: result.outcome === "applied" && end > start };
  }

  /**
   * ADR-0050 section 2.4 — re-check the device lists of the accounts in
   * `entry` against its roster: propose an Add for a listed device with no
   * leaf (a restored phone, a pairing whose fan-out was cut short), and — for
   * this account only — a Remove for a leaf whose directory row is gone
   * (`forgetDevice`). Accounts are resolved to directory ids through this
   * device's own account and its accepted contacts; a member that is neither
   * is left as is. Each chat is checked at most once per
   * `READMISSION_INTERVAL_MS`.
   */
  async function readmitFor(entry: ActiveGroup): Promise<void> {
    const secrets = auth.identitySecrets();
    if (secrets === null) return;
    if (readIsActive(entry.group) === false) return;
    if (!takeReadmissionSlot(entry.groupIdB64, Date.now())) return;
    const s = auth.state();
    if (s.kind !== "session") return;
    const account = s.account.accountId;
    const own = ownPersonKeyB64(chats.identity());
    if (own === null) return;
    let leaves: RosterLeaf[];
    try {
      leaves = entry.group.members() as unknown as RosterLeaf[];
    } catch {
      return;
    }
    const roster: RosterLeafKeys[] = leaves.map((m) => ({
      leafKeyB64: base64Encode(m.signaturePublicKey),
      personKeyB64: base64Encode(personKeyOf(m)),
    }));
    const people = new Set(roster.map((l) => l.personKeyB64));
    // The accounts we can look up: ours, and every accepted contact in the
    // chat whose account id was captured at accept.
    const lookups: { personKeyB64: string; accountId: Uint8Array }[] = [
      { personKeyB64: own, accountId: secrets.accountId },
    ];
    for (const c of loadAccepted()) {
      if (c.accountIdHex === undefined || !people.has(c.signaturePubkeyB64)) continue;
      const id = hexToBytes(c.accountIdHex);
      if (id !== null) lookups.push({ personKeyB64: c.signaturePubkeyB64, accountId: id });
    }
    const accounts: AccountDevices[] = [];
    for (const l of lookups) {
      try {
        const list = await discovery.fetchKeyPackages(l.accountId);
        accounts.push({
          personKeyB64: l.personKeyB64,
          devices:
            list === null
              ? []
              : list.map((d) => ({ deviceKeyB64: base64Encode(d.deviceId), keyPackage: d.keyPackage })),
        });
      } catch {
        accounts.push({ personKeyB64: l.personKeyB64, devices: null });
      }
    }
    const plan = planReadmission({
      roster,
      ownPersonKeyB64: own,
      accounts,
      removed: removedDevices(account, entry.groupIdB64),
    });
    if (plan.admit.length === 0 && plan.retire.length === 0) return;
    let proposed = 0;
    for (const d of plan.admit) {
      try {
        const outcome = await contacts.admitDeviceToGroup(secrets, entry.group, d.keyPackage);
        if (outcome === null) continue;
        proposed += 1;
        if (outcome.kind === "proposed") {
          markOwnPublished(entry.groupIdB64, outcome.proposalMessageId);
        }
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("ADR-0050 re-admission: add not proposed", err);
      }
    }
    for (const leafKeyB64 of plan.retire) {
      try {
        const outcome = await contacts.retireDeviceFromGroup(
          secrets,
          entry.group,
          base64Decode(leafKeyB64),
        );
        if (outcome === null) continue;
        proposed += 1;
        if (outcome.kind === "proposed") {
          markOwnPublished(entry.groupIdB64, outcome.proposalMessageId);
        }
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("ADR-0050 re-admission: retire not proposed", err);
      }
    }
    if (proposed > 0) {
      // eslint-disable-next-line no-console
      console.info(
        `ADR-0050 re-admission: ${plan.admit.length} to admit, ${plan.retire.length} to retire in ${entry.groupIdB64}`,
      );
      await sweepGroup(entry);
    }
  }

  /** ADR-0045 S4b — the chat view's `actingCommitterNow`, for a group whose
   *  view is not open: the beacon fold over this device's observations, ranks
   *  re-derived through `committerRank` at verdict time. */
  async function actingCommitterFor(entry: ActiveGroup, ownRank: number): Promise<ActingCommitter> {
    const s = auth.state();
    if (s.kind !== "session") return { kind: "defer", toRank: undefined };
    const wasm = await loadWasm();
    return decideActingCommitter({
      accountIdB64: s.account.accountId,
      groupIdB64: entry.groupIdB64,
      ownRank,
      rankOf: (senderKeyB64) => {
        try {
          return entry.group.committerRank(base64Decode(senderKeyB64));
        } catch {
          return undefined;
        }
      },
      nowMs: Date.now(),
      freshnessWindowMs: wasm.committerBeaconFreshnessWindowMs(),
      fold: (rank, observed, windowMs) => {
        const verdict = wasm.actingCommitterVerdict(
          rank,
          observed.map((o) => new wasm.BeaconObservation(o.rank, o.ageMs)),
          windowMs,
        );
        try {
          if (verdict.kind === "act") return { kind: "act" };
          if (verdict.kind === "defer") {
            return { kind: "defer", toRank: verdict.deferToRank };
          }
          return { kind: "not-a-candidate" };
        } finally {
          verdict.free();
        }
      },
    });
  }

  /**
   * One committer sweep of `entry`, from outside its chat view. The rows the
   * verdict applied below this device's own commit are settled through the
   * same path as a poll's, so they reach the log and the view. A group whose
   * view is open is left alone: that view sweeps at the tail of its own poll,
   * over its own cursor, and two sweepers over two cursors would re-feed
   * consumed rows.
   */
  async function sweepGroup(entry: ActiveGroup): Promise<SweepOutcome | null> {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    if (s.kind !== "session" || secrets === null) return null;
    if (coordinator.isForeground(entry.groupIdB64)) {
      // The view sweeps at the tail of its own poll; ask it to poll now, since
      // its long poll may be parked while the tab is hidden.
      coordinator.requestPoll(entry.groupIdB64);
      return null;
    }
    if (readIsActive(entry.group) === false) return null;
    const account = s.account.accountId;
    const start = await startCursor(account, entry);
    const before = memberSignatureKeys(entry.group);
    const swept: IncomingChatEvent[] = [];
    let outcome: SweepOutcome | null = null;
    try {
      outcome = await contacts.sweepIfCommitter(
        secrets,
        entry.group,
        {
          read: () => coordinator.cursor(entry.groupIdB64) ?? start,
          advance: (to) => {
            coordinator.noteCursor(entry.groupIdB64, to);
          },
        },
        // Named group, not a bare id: this one `ChatClient` polls every chat,
        // so a group-blind mark would suppress another chat's row of the same
        // DS id (`chat/own-published.ts`).
        (id) => chat.markOwnPublishedCommit(entry.groupIdB64, id),
        (rank) => actingCommitterFor(entry, rank),
        (event) => swept.push(event),
      );
    } finally {
      // Applied and cursor-advanced whether or not our commit won: the only
      // question left is whether they reach the log and the view.
      if (swept.length > 0) await settle(account, entry, swept, true, before);
    }
    if (outcome?.kind === "committed") {
      // ADR-0043 — a removal this sweep committed owes the push-routing
      // rotation, as the chat view's sweep would.
      void rotateIfShrank(account, entry, before);
    }
    return outcome;
  }

  /** Poll the directory until `deviceKey`'s KeyPackage is there, bounded. */
  async function awaitDeviceKeyPackage(
    accountId: Uint8Array,
    deviceKey: Uint8Array,
  ): Promise<Uint8Array> {
    const deadline = Date.now() + ADMIT_DEVICE_WAIT_MS;
    for (;;) {
      const list = await discovery.fetchKeyPackages(accountId);
      const hit = list?.find((d) => bytesEqual(d.deviceId, deviceKey));
      if (hit !== undefined) return hit.keyPackage;
      if (Date.now() >= deadline) {
        throw new Error(
          "the new device has not published a KeyPackage yet; it will be admitted to your chats later",
        );
      }
      await sleepCancellable(ADMIT_DEVICE_POLL_MS, new AbortController().signal);
    }
  }

  async function admitDevice(deviceKey: Uint8Array): Promise<DeviceFanOut> {
    const secrets = auth.identitySecrets();
    if (secrets === null) throw new Error("admitDevice: no session");
    const keyPackage = await awaitDeviceKeyPackage(secrets.accountId, deviceKey);
    let offered = 0;
    let skipped = 0;
    let failed = 0;
    for (const entry of chats.groups()) {
      if (readIsActive(entry.group) === false) {
        skipped += 1;
        continue;
      }
      try {
        const outcome = await contacts.admitDeviceToGroup(secrets, entry.group, keyPackage);
        if (outcome === null) {
          skipped += 1;
          continue;
        }
        offered += 1;
        if (outcome.kind === "proposed") {
          // Our own row: the next poll of this group (any client) drops it
          // instead of handing it to OpenMLS as an incoming proposal.
          markOwnPublished(entry.groupIdB64, outcome.proposalMessageId);
          await sweepGroup(entry);
        }
      } catch (err) {
        failed += 1;
        // eslint-disable-next-line no-console
        console.warn("ADR-0050 admit device: chat skipped", entry.groupIdB64, err);
      }
    }
    return { offered, skipped, failed };
  }

  async function retireDevice(deviceKey: Uint8Array): Promise<DeviceFanOut> {
    const secrets = auth.identitySecrets();
    if (secrets === null) throw new Error("retireDevice: no session");
    let offered = 0;
    let skipped = 0;
    let failed = 0;
    for (const entry of chats.groups()) {
      if (readIsActive(entry.group) === false) {
        skipped += 1;
        continue;
      }
      try {
        const outcome = await contacts.retireDeviceFromGroup(secrets, entry.group, deviceKey);
        if (outcome === null) {
          skipped += 1;
          continue;
        }
        offered += 1;
        if (outcome.kind === "proposed") {
          markOwnPublished(entry.groupIdB64, outcome.proposalMessageId);
          await sweepGroup(entry);
        }
      } catch (err) {
        failed += 1;
        // eslint-disable-next-line no-console
        console.warn("ADR-0050 retire device: chat skipped", entry.groupIdB64, err);
      }
    }
    // The directory row last: a peer that fetched it meanwhile only produced a
    // Welcome nobody will open.
    discovery.setSessionToken(secrets.sessionToken);
    try {
      await discovery.deleteKeyPackage(deviceKey);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ADR-0050 retire device: directory KeyPackage not deleted", err);
    } finally {
      discovery.setSessionToken(null);
    }
    return { offered, skipped, failed };
  }

  async function settle(
    account: string,
    entry: ActiveGroup,
    events: readonly IncomingChatEvent[],
    cursorMoved: boolean,
    membersBefore: string[] | null,
  ): Promise<void> {
    const groupIdB64 = entry.groupIdB64;
    const plan = planBackgroundEvents(events);

    // Call signaling first: an offer should start ringing before the log
    // write below spends its round trip.
    for (const row of plan.signaling) callHost.ingestSignaling(groupIdB64, row);

    for (const p of plan.profiles) {
      if (p.profile !== undefined && p.senderKey !== undefined) profiles.ingest(p.senderKey, p.profile);
    }
    // ADR-0048 §2 trigger 4 — another member asked this group to introduce
    // itself; answer with the own profile through the throttled growth path.
    // A request this device published itself (re-fetched after a reload, when
    // the own-echo registry is empty) is not a question for it; an
    // unattributable sender is dropped. One answer covers the whole batch —
    // the scheduler coalesces anyway.
    {
      const own = chats.identity();
      for (const r of plan.profileRequests) {
        if (r.senderKey === undefined) continue;
        if (own !== null && isOwnKeyB64(own, r.senderKey)) continue;
        profiles.requested(groupIdB64);
        break;
      }
    }
    // ADR-0024 — read receipts land in the durable store whether or not the
    // chat is open; the view reads its "Read" ticks from there.
    for (const r of plan.readReceipts) readWatermarks.ingest(groupIdB64, r.senderKey, r.target);
    // ADR-0051 — the shared group name, through the role-gated verdict under
    // the per-group lock; the chat list re-titles from the store.
    for (const n of plan.groupNames) {
      void groupNames.receive(entry, n).catch((err) => {
        // eslint-disable-next-line no-console
        console.warn("background receive: group name receive failed", err);
      });
    }
    // ADR-0052 decision 3 — a shared pin, through the same gate and lock;
    // the bar re-derives when the chat is next opened.
    for (const p of plan.pins) {
      void pins.receive(entry, p).catch((err) => {
        // eslint-disable-next-line no-console
        console.warn("background receive: pin receive failed", err);
      });
    }
    // ADR-0059 section 6 — members' shared channel lists, filed per member.
    for (const l of plan.sharedChannels) {
      void sharedChannels.receive(entry, l).catch((err) => {
        // eslint-disable-next-line no-console
        console.warn("background receive: shared channels receive failed", err);
      });
    }
    for (const secret of plan.pushSecrets) {
      void receivePushRoutingSecretGated(
        account,
        entry.group,
        groupIdB64,
        secret.senderLeaf,
        secret.secret,
        secret.mintEpoch,
      ).catch((err) => {
        // eslint-disable-next-line no-console
        console.warn("background receive: push-routing secret receive failed", err);
      });
    }
    for (const b of plan.beacons) {
      if (b.senderLeaf === undefined) continue;
      const key = signatureKeyOfLeaf(entry, b.senderLeaf);
      if (key !== undefined) {
        recordBeaconObservation(account, groupIdB64, base64Encode(key), Date.parse(b.createdAt));
      }
    }
    for (const p of plan.storedProposals) {
      recordObservation(
        account,
        groupIdB64,
        proposalKey({
          kind: p.proposalKind,
          addSignatureKey: p.addSignatureKey,
          removeTargetLeaf: p.removeTargetLeaf,
          senderLeaf: p.senderLeaf,
          stored: p.stored,
        }),
        Date.parse(p.createdAt),
        false,
      );
    }

    // ADR-0043 — somebody else's removal landed in a chat that is not open:
    // an owner / admin device still owes the push-routing rotation, exactly as
    // the chat view's poll would have done.
    if (plan.sawCommit) {
      // ADR-0050 — remember the leaves that left, so the scheduled re-admission
      // never puts back a device an admin removed.
      recordRemovedDevices(account, groupIdB64, membersBefore, memberSignatureKeys(entry.group));
      void rotateIfShrank(account, entry, membersBefore);
    }

    // ADR-0048 §2 trigger 3 — did the group GAIN a member? Against the
    // baseline persisted per (account, group), not a per-session variable, so
    // a join that landed while this tab was closed is still growth to it, and
    // a chat nobody has open still re-sends its name (with the ADR-0051 group
    // name, same throttle). Every batch, not only one carrying a commit row:
    // an Add this device swept itself lands with no commit event of its own.
    if (observeMembers(account, groupIdB64, new Set(personKeysOf(entry.group)))) {
      profiles.groupGrew(groupIdB64);
      groupNames.groupGrew(groupIdB64);
      pins.groupGrew(groupIdB64);
      sharedChannels.groupGrew(groupIdB64);
    }

    // The chat view gets the rest, now or when it next mounts.
    coordinator.deliver(groupIdB64, plan.toView);

    // ADR-0023 — an op with no timeline row of its own still has to reach the
    // log: a delete that arrives while its chat is closed is otherwise lost
    // with the in-memory buffer, and its row can never be decrypted again.
    if (plan.timeline.length > 0 || plan.messageOps.length > 0 || cursorMoved) {
      await fileIntoLog(
        account,
        entry,
        plan.timeline.map((e) => eventToPersistedMessage(e)),
        plan.messageOps,
      );
    }
  }

  async function fileIntoLog(
    account: string,
    entry: ActiveGroup,
    messages: ReturnType<typeof eventToPersistedMessage>[],
    /** ADR-0023 — edit / delete / reaction, applied to the slot after the
     *  append so an op and the row it targets can arrive in one batch. */
    ops: readonly MessageOpEvent[],
  ): Promise<void> {
    try {
      const deviceKey = await getDeviceKey(account);
      if (deviceKey === null) return;
      const hex = hexOf(entry);
      const cursor = coordinator.cursor(entry.groupIdB64) ?? 0;
      // ADR-0052 decision 3 — a message deleted while its chat was closed
      // takes its pin records with it (section 4.1). Which deletes the
      // author gate accepted is read off the fold's result, not off the ops.
      const newlyDeleted: number[] = [];
      await updateMessageLog(account, deviceKey, (current) => {
        const groups = current?.groups ?? {};
        const appended = appendToGroupSlot(groups[hex], messages, cursor);
        const folded = foldOpsIntoStored(appended.messages, ops);
        newlyDeleted.length = 0;
        if (folded !== appended.messages) {
          const before = new Set(
            appended.messages.filter((m) => m.deleted === true).map((m) => Number(m.messageId)),
          );
          for (const m of folded) {
            const id = Number(m.messageId);
            if (m.deleted === true && Number.isFinite(id) && !before.has(id)) newlyDeleted.push(id);
          }
        }
        return {
          version: MESSAGE_LOG_VERSION,
          groups: {
            ...groups,
            [hex]:
              folded === appended.messages ? appended : { ...appended, messages: folded },
          },
        };
      });
      for (const id of newlyDeleted) {
        void pins.messageDeleted(entry.groupIdB64, id).catch((err) => {
          // eslint-disable-next-line no-console
          console.warn("background receive: dropping pins of a deleted message failed", err);
        });
      }
    } catch (err) {
      // The rows still reach the chat view through the coordinator for this
      // session; only their survival across a reload is lost.
      // eslint-disable-next-line no-console
      console.warn("background receive: filing rows into the message log failed", err);
    }
  }

  async function rotateIfShrank(
    account: string,
    entry: ActiveGroup,
    membersBefore: string[] | null,
  ): Promise<void> {
    const secrets = auth.identitySecrets();
    if (secrets === null) return;
    try {
      const currentMemberKeys = memberSignatureKeys(entry.group);
      const selfEvicted = readIsActive(entry.group) === false;
      const wasm = await loadWasm();
      const identity = wasm.Identity.fromSeed(secrets.accountIdentitySeed);
      let ownRole;
      try {
        ownRole = await readPushRotationRole(entry.group, identity);
      } finally {
        identity.free();
      }
      await rotatePushRoutingSecretIfMembershipShrank({
        accountId: account,
        group: entry.group,
        ownRole,
        previousMemberKeys: membersBefore,
        currentMemberKeys,
        broadcast: async (bytes) => {
          await chat.broadcastControl(auth.identitySecrets(), entry.group, bytes);
        },
        selfEvicted,
      });
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("background receive: push-routing rotation failed", err);
    }
  }

  async function syncGroupNow(groupIdB64: string): Promise<void> {
    if (activeAccount === null || coordinator.isForeground(groupIdB64)) return;
    // A one-off poll is not tied to the sweep's lifetime; the cursor re-read
    // under the MLS lock makes a race with the sweep or the call lane benign.
    await pollGroup(groupIdB64, 0, new AbortController().signal);
  }

  let unregisterGroupCheck: (() => void) | null = null;

  function start(accountIdB64: string): void {
    if (activeAccount === accountIdB64 && loopAbort !== null) return;
    stop();
    activeAccount = accountIdB64;
    const controller = new AbortController();
    loopAbort = controller;
    const sweepDeps: SweepDeps = {
      groups: () => chats.groups().map((g) => g.groupIdB64),
      isExcluded,
      poll: (groupIdB64, signal) => pollGroup(groupIdB64, 0, signal),
      concurrency: BACKGROUND_CONCURRENCY,
      maxImmediateRepolls: MAX_IMMEDIATE_REPOLLS,
      backoff,
      now: () => Date.now(),
      onPollFailed: (groupIdB64, err) => {
        const delay = backoff.failed(
          groupIdB64,
          Date.now(),
          isDsNetworkFailure(err) ? "network" : "other",
        );
        // eslint-disable-next-line no-console
        console.warn(`background receive: poll failed; group backed off ${delay} ms`, err);
      },
      // docs/ux/feed.md — the feed's "Couldn't check for new messages."
      onSweepDone: reportGroupCheck,
    };
    // The feed's Retry: one sweep now, past the per-group backoff (the person
    // asked), outside the loop's pause. A race with the loop's own sweep is
    // benign: the cursor re-read under the MLS lock drops the loser.
    let retrying = false;
    unregisterGroupCheck?.();
    unregisterGroupCheck = onGroupCheckRequested(() => {
      if (retrying || controller.signal.aborted) return;
      retrying = true;
      const { backoff: _skipBackoff, ...unbackedOff } = sweepDeps;
      void runSweep(
        {
          ...unbackedOff,
          poll: async (groupIdB64, signal) => {
            const result = await pollGroup(groupIdB64, 0, signal);
            backoff.succeeded(groupIdB64);
            return result;
          },
        },
        controller.signal,
      ).finally(() => {
        retrying = false;
      });
    });
    void runBackgroundReceiveLoop(controller.signal, {
      ...sweepDeps,
      isActive,
      waitUntilActive: (signal) => resume.wait(signal),
      sleep: sleepCancellable,
      random: Math.random,
      intervalMs: BACKGROUND_SWEEP_INTERVAL_MS,
      jitterMs: BACKGROUND_SWEEP_JITTER_MS,
    });
  }

  function stop(): void {
    if (loopAbort !== null) {
      loopAbort.abort();
      loopAbort = null;
    }
    unregisterGroupCheck?.();
    unregisterGroupCheck = null;
    resetFeedRefresh();
    stopCallLane();
    activeAccount = null;
    logCursors = null;
    backoff.clear();
    coordinator.reset();
    clearReadmissionSlots();
  }

  createEffect(() => {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    const identity = chats.identity();
    const account = s.kind === "session" ? s.account.accountId : null;
    untrack(() => {
      if (account !== null && secrets !== null && identity !== null) start(account);
      else stop();
    });
  });

  // ----- the dedicated lane for the group of the call in progress ------------

  let laneAbort: AbortController | null = null;

  function stopCallLane(): void {
    if (laneAbort !== null) {
      laneAbort.abort();
      laneAbort = null;
    }
    hotGroup = null;
  }

  function startCallLane(groupIdB64: string): void {
    if (hotGroup === groupIdB64 && laneAbort !== null) return;
    stopCallLane();
    hotGroup = groupIdB64;
    const controller = new AbortController();
    laneAbort = controller;
    void runLongPollLoop(controller.signal, {
      shouldContinue: () => hotGroup === groupIdB64,
      isPaused: () => false,
      waitUntilResumed: async () => {},
      tick: async (signal) => {
        await pollGroup(groupIdB64, LONG_POLL_WAIT_SECS, signal);
        return 0;
      },
      classifyFailure: (err) => (isDsNetworkFailure(err) ? "network" : "other"),
      sleep: sleepCancellable,
      now: () => Date.now(),
      onFailure: (err, backoffMs) => {
        // eslint-disable-next-line no-console
        console.warn(`call lane poll failed; retrying in ${backoffMs} ms`, err);
      },
    });
  }

  createEffect(() => {
    const group = callHost.callLaneGroupIdB64();
    foregroundVersion();
    const signedIn = auth.state().kind === "session";
    untrack(() => {
      if (signedIn && group !== null && activeAccount !== null && !coordinator.isForeground(group)) {
        startCallLane(group);
      } else {
        stopCallLane();
      }
    });
  });

  // A call starting in a hidden tab has to un-park the sweep at once.
  createEffect(() => {
    if (callHost.anyCallInProgress()) resume.wake();
  });

  const onVisibility = (): void => resume.wake();
  globalThis.document?.addEventListener("visibilitychange", onVisibility);

  // A push wake means something landed that this tab has not seen. Open the
  // window above and un-park the sweep, so a hidden tab actually catches up
  // instead of ignoring the wake it was sent. Both of the worker's messages
  // count: the arrival broadcast, and a call button the user pressed (which
  // for Decline is the only signal the tab gets, since that press
  // deliberately leaves the window unfocused).
  //
  // Read-only and idempotent: it moves a deadline and pokes a latch, nothing
  // else. Nothing here trusts the message's contents — an unknown `type`
  // simply does not open the window.
  const swContainer = globalThis.navigator?.serviceWorker;
  const onSwMessage = (event: MessageEvent): void => {
    const type = (event.data as { type?: unknown } | null)?.type;
    if (type !== "LAKKO_PUSH_WAKE" && type !== "LAKKO_CALL_ACTION") return;
    wakeUntilMs = Date.now() + WAKE_ACTIVE_MS;
    resume.wake();
  };
  swContainer?.addEventListener("message", onSwMessage);
  // A `ServiceWorkerContainer` only starts dispatching queued messages once
  // `onmessage` is assigned or `startMessages()` is called; we register with
  // `addEventListener` so as not to clobber another listener (the same
  // load-bearing call `WakeNavigator` and `CallActionListener` make).
  if (typeof swContainer?.startMessages === "function") swContainer.startMessages();

  onCleanup(() => {
    globalThis.document?.removeEventListener("visibilitychange", onVisibility);
    swContainer?.removeEventListener("message", onSwMessage);
    stop();
  });

  return (
    <BackgroundReceiveContext.Provider
      value={{ coordinator, syncGroupNow, admitDevice, retireDevice }}
    >
      {props.children}
    </BackgroundReceiveContext.Provider>
  );
}

export function useBackgroundReceive(): BackgroundReceiveApi {
  const ctx = useContext(BackgroundReceiveContext);
  if (ctx === undefined) {
    throw new Error("useBackgroundReceive() called outside <BackgroundReceiveProvider>");
  }
  return ctx;
}

/** Lowercase hex of a group id — the message log's slot key. */
function hexOf(entry: ActiveGroup): string {
  let out = "";
  for (const b of entry.group.groupId) out += b.toString(16).padStart(2, "0");
  return out;
}

/** The roster member at `leaf`, resolved at receipt (C-68). */
function signatureKeyOfLeaf(entry: ActiveGroup, leaf: number): Uint8Array | undefined {
  try {
    const roster = entry.group.members() as Array<{
      leafIndex: number;
      signaturePublicKey: Uint8Array;
    }>;
    return roster.find((m) => m.leafIndex === leaf)?.signaturePublicKey;
  } catch {
    return undefined;
  }
}
