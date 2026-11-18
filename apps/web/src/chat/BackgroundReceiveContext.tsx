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
import { installHandoverPublisher } from "../attach/admission-hook";
import {
  createContext,
  createEffect,
  createSignal,
  onCleanup,
  untrack,
  useContext,
  type JSX,
} from "solid-js";

import { conversationSeedOf, useAuth } from "../auth/AuthContext";
import { getDeviceKey } from "../auth/key-store";
import { useCallHost } from "../calls/CallHostContext";
import { isDsNetworkFailure } from "../ds/client";
import { journalHex } from "../ds/access";
import { JOURNAL_REPAIR_MAX_POLLS, journalRepair } from "../ds/journal-repair";
import {
  createWakeLatch,
  LONG_POLL_WAIT_SECS,
  runLongPollLoop,
  sleepCancellable,
} from "../ds/long-poll-loop";
import { isOneToOne, useProfiles } from "../profile/ProfileContext";
import { intakeScopedEnvelope, noteIntroductionsAsk } from "../reach/scoped-intake";
import { onReceivedGrantsChanged } from "../reach/reach-service";
import { loadUnacked } from "./commit-outbox";
import { retryOwedWelcomes } from "./owed-welcomes";
import { observeMembership } from "../profile/membership-baseline";
import { isOwnKeyB64 } from "../profile/own-key";
import {
  memberSignatureKeys,
  readPushRotationRole,
  receivePushRoutingSecretGated,
  rotatePushRoutingSecretIfMembershipShrank,
  retireOldPushGid,
} from "../push/group-routing";
import { getGroupRoutingSecret } from "../push/group-routing-store";
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
import { markUnseenReaction } from "./unseen-reactions";
import {
  BACKGROUND_CONCURRENCY,
  BACKGROUND_SWEEP_INTERVAL_MS,
  backgroundSweepIntervalMs,
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
import { useMembershipEnd } from "./MembershipEndContext";
import { ChatClient, type IncomingChatEvent } from "./client";
import {
  decideActingCommitter,
  wasmActingCommitterFold,
  recordBeaconObservation,
  type ActingCommitter,
} from "./committer-beacon";
import { useGroupClosed } from "./GroupClosedContext";
import { useGroupNames } from "./GroupNameContext";
import { usePins } from "./PinContext";
import { useSharedChannels } from "../discover/SharedChannelsContext";
import { useReadWatermarks } from "./ReadWatermarkContext";
import { readIsActive } from "./membership-ended";
import {
  loadMessageLog,
  MESSAGE_LOG_VERSION,
  updateMessageLog,
  type PersistedMessage,
} from "./message-log-store";
import {
  clearReadmissionSlots,
  lookupStanding,
  type LookupStanding,
  planReadmission,
  readmissionCandidates,
  recordRemovedDevices,
  removedDevices,
  takeReadmissionSlot,
  type AccountDevices,
  type RosterLeafKeys,
} from "./device-readmission";
import { markOwnPublished } from "./own-published";
import { hexOfKey, ownDeviceAdmissible, ownDeviceRoster } from "../devices/own-device-roster";
import { ownDeviceKeyPackageUsable } from "../devices/own-conversation-key";
import { ownPersonKeyB64, personKeyOf, personKeysOf, type RosterLeaf } from "./person-key";
import {
  accountIdB64OfKey,
  loadAccepted,
  loadKeyAliases,
  loadSupersededKeys,
  readPin,
} from "../contacts/local-store";
import { readVerifiedBinding } from "../contacts/verified-binding";
import { subscribeResetAccepted } from "../contacts/reset-events";
import { installScopeLineage } from "../reach/mailbox-write";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { savedEntriesOf, unsaveEdit } from "./saved-messages";
import { useLineage } from "./use-lineage";
import { laterSegmentExists, oldSegmentPolled, retiresOldSegmentPush } from "./lineage";
import { groupIdHex } from "../devices/sync/state";
import { writeSupersededMirror } from "../push/superseded-mirror";
import {
  awaitedSwaps,
  conversationMovedAt,
  markPushRetired,
  advanceResendCursor,
  markResent,
  noteConversationMoved,
  resendCursorOf,
  setAwaitedSwaps,
  wasPushRetired,
  wasResent,
} from "./continuity-store";
import {
  continueConversation,
  type ContinuityFacts,
  type ContinuityPort,
} from "./continuity-runtime";
import { NO_CLAIMS } from "./group-closed-store";
import { messagesToResend, resendParts, resendRemaining } from "./reset-repair";
import { composeResendEnvelope } from "./attachments";
import { heldResendPredicate } from "./resend-receive";
import { flushVerdict, isDeadSession, replyForSend, sendRoute } from "./held-send";
import {
  composeHeldEnvelope,
  heldSends,
  notifyHeldFlushed,
  persistHeldSends,
  resetHeldSends,
  restoreHeldSends,
} from "./held-send-queue";
import { forwardRefFromPersisted, persistedFromForwardRef } from "./forward";
import { persistedFromReplyRef, replyRefFromPersisted } from "./reply-snapshot";
import { unrepairableBy } from "./successor-claim";
import { installDeadSessionGuard } from "./client";
import { attachmentRefFromPersisted, persistedFromAttachmentRef } from "./persisted-attachment";
import {
  adminRestoreStep,
  mayAddResetIdentity,
  repairPlan,
  resetCase,
  supersededSenderInert,
} from "./reset-repair";
import { conversationKindOf } from "./conversation-kind";
import {
  membershipChangeOfBatch,
  sweepBatchWithOwnCommit,
  withMembershipChange,
  type MembershipChange,
} from "./membership-rows";
import type { ProposalRecordView } from "./pending-requests";
import type { PushRotationRole } from "../push/group-routing";
import { proposalKey, recordObservation } from "./pending-requests";
import { ReceiveCoordinator } from "./receive-coordinator";
import { runSupersededRerequests } from "./membership-retry";
import { readIntentSnapshot, reissueOwnRequest } from "./rerequest-membership";
import { membershipRefusalRetryMaySucceed } from "../../wasm/lakko_ffi_wasm";
import { BindingEpoch, StaleBindingError } from "../auth/binding-epoch";
import type { SweepOutcome } from "./sweep";
import { durableSnapshotHook, sessionAccountOf } from "./persist-hooks";

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
   * the acting committer (a table-less group is refused, ADR-0081 amendment
   * of 2026-10-19). Chats whose
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
   * other devices untouched). The directory row is deleted before this, by
   * the removal operation (ADR-0074 section 2.3, `devices/removal.ts`), which
   * retries this fan-out from its owed record while `failed` is not 0.
   */
  retireDevice(deviceKey: Uint8Array): Promise<DeviceFanOut>;
  /**
   * ADR-0073 R.6 / R.7 — offer `groupIdB64` (and, throttled, every other
   * held group) a continuity pass now. The chat view calls it at the tail
   * of its own poll: the background sweep does not poll the group a view
   * has open, so without this a claim posted for the open chat would never
   * be decided while it stays open. Fire-and-forget; never throws.
   */
  continuityPass(groupIdB64: string): void;
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
/** How long `admitDevice` waits for the account's DeviceGroup roster to name
 *  the device (ADR-0074 section 2.3). The pairing added it there before this
 *  runs; the wait covers the sync runtime reloading its handle. Chosen. */
export const ADMIT_DEVICE_ROSTER_WAIT_MS = 30_000;
export const ADMIT_DEVICE_ROSTER_POLL_MS = 1_000;

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
  const groupClosed = useGroupClosed();
  const pins = usePins();
  const sharedChannels = useSharedChannels();
  const readWatermarks = useReadWatermarks();
  const callHost = useCallHost();
  const sync = useDeviceGroupSync();
  const lineageView = useLineage();
  const membershipEnd = useMembershipEnd();

  const coordinator = new ReceiveCoordinator<IncomingChatEvent>();
  const backoff = new GroupBackoff();
  const chat = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
  });
  // ADR-0090 section 3.9 — the quiet send the open-items hand-over uses after
  // any commit of this device that added a leaf (`attach/admission-hook.ts`).
  onCleanup(
    installHandoverPublisher((group, row) =>
      chat.broadcastControl(auth.identitySecrets(), group as Parameters<typeof chat.broadcastControl>[1], row),
    ),
  );
  // ADR-0045 / ADR-0050 — the membership half for chats whose view is not
  // open: the sweep at the tail of every background poll, and the device
  // fan-outs. Same persist hooks the chat view's client uses, so a commit
  // staged here is as durable as one staged there.
  const discovery = new DiscoveryClient();
  const contacts = new ContactsClient(discovery, undefined, {
    identityProvider: () => chats.identity(),
    accountIdentityProvider: () => chats.accountIdentity(),
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
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
  /**
   * Advanced by `stop()` whenever a session this provider was bound to ends.
   * A poll, a sweep or a device fan-out captures it when it starts. After
   * that session is over, what such work still holds (`entry`, its secrets,
   * the rows it decrypted) is the OLD account's, while everything this
   * provider reaches - the coordinator's cursors, the profile store, the call
   * host, the chat list - is the next account's or nobody's. So stale work
   * touches none of that; the one thing it still does is file the rows it
   * decrypted into the old account's own message log, which is durable,
   * keyed by that account, and cannot be decrypted a second time.
   */
  const binding = new BindingEpoch();
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
  async function startCursor(
    accountIdB64: string,
    entry: ActiveGroup,
    current: () => boolean,
  ): Promise<number> {
    const known = coordinator.cursor(entry.groupIdB64);
    if (known !== undefined) return known;
    const fromLog = (await loadLogCursors(accountIdB64)).get(hexOf(entry)) ?? 0;
    // The coordinator was reset for another session while the log was being
    // read: this account's position must not seed that session's cursor (two
    // accounts on one browser can share a group).
    if (!current()) return fromLog;
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
    if (entry === undefined || readIsActive(entry.group) === false) return true;
    // ADR-0087 section 9 — and neither is a group whose end is under way, a
    // heard one included: that handle is still active.
    if (membershipEnd.isEnding(groupIdB64)) return true;
    // ADR-0073 R.5 — an older group of a conversation that moved: a
    // one-to-one's old journal is not read at all (its only other sender is
    // the superseded identity); a group's is read for the retention window,
    // for the members who have not moved yet.
    const view = lineageView.resolve(groupIdB64);
    if (view === undefined || view.head.groupIdB64 === groupIdB64) return false;
    return !oldSegmentPolled({
      isHead: false,
      oneToOne: conversationKindOf(entry.group) === "chat",
      movedAtMs: activeAccount === null ? null : conversationMovedAt(activeAccount, groupIdB64),
      nowMs: Date.now(),
    });
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
    const current = binding.capture();
    const start = await startCursor(account, entry, current);
    if (!current()) return { yieldedRows: false };
    let membersBefore = memberSignatureKeys(entry.group);
    let peopleBefore: string[] | null = personKeysOrNull(entry);
    // Where this poll got to, kept apart from the coordinator: once the
    // session is over the coordinator's cursors are another session's.
    let reached = start;
    const result = await chat.pollMessages(
      secrets,
      entry.group,
      start,
      waitSecs,
      {
        read: () => (current() ? (coordinator.cursor(groupIdB64) ?? start) : reached),
        advance: (to) => {
          reached = Math.max(reached, to);
          if (current()) coordinator.noteCursor(groupIdB64, to);
        },
        beforeApply: () => {
          membersBefore = memberSignatureKeys(entry.group);
          peopleBefore = personKeysOrNull(entry);
        },
      },
      signal,
    );
    if (result.outcome === "cursor-moved") return { yieldedRows: false, cursorMoved: true };
    if (!current()) {
      // Signed out while the poll was out. Only the durable half.
      await settle(
        account,
        entry,
        result.events,
        reached > start,
        membersBefore,
        { current, reached },
        peopleBefore,
      );
      return { yieldedRows: false };
    }
    const end = coordinator.cursor(groupIdB64) ?? start;
    await settle(
      account,
      entry,
      result.events,
      end > start,
      membersBefore,
      { current, reached },
      peopleBefore,
    );
    if (!current()) return { yieldedRows: false };
    // ADR-0050 section 2.5a point 4 — re-request an own membership request
    // a commit this batch applied dropped, BEFORE the sweep so a committer
    // running here carries the re-request in the same breath. Decides nothing
    // unless the epoch moved past a recorded request, i.e. unless a commit
    // merged; never on a timer.
    //
    // ADR-0045 — then the sweep trigger, exactly as at the tail of the chat
    // view's poll: it is the only thing that makes a pending proposal ever
    // land, and before this a proposal in a chat nobody had open waited until
    // somebody opened it. Cheap when nothing is pending; never awaited by the
    // poll's own error path — a sweep failure is not a poll failure.
    //
    // ADR-0060 section 6 - last, a proposer's committer envelope for an Add
    // still waiting, after the sweep so an Add this device just landed sends
    // nothing. A failed sweep does not stop it.
    void rerequestSupersededFor(entry)
      .then(() => sweepGroup(entry))
      .catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("background receive: tail sweep failed", err);
      })
      .then(() => (current() ? resendCommitterEnvelopesFor(entry) : undefined))
      .catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("background receive: committer envelope re-send failed", err);
      });
    // ADR-0050 section 2.4 — the scheduled re-admission check, bounded per
    // chat; see `device-readmission.ts`.
    void readmitFor(entry).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("ADR-0050 re-admission check failed", err);
    });
    // ADR-0073 R.7 case 2 — an accepted reset reaches every shared group
    // here, chat open or not: where this account administers the group the
    // removal of the superseded leaves is proposed. Silent (R.0). The chat
    // view runs the same pass for the group it has open.
    void repairResetFor(entry).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("ADR-0073 reset repair pass failed", err);
    });
    // ADR-0073 R.6 / R.7 cases 1 and 3 — a conversation that cannot be
    // repaired in place continues in a successor: this group's own pass, and
    // (throttled) every other held group's, since an old one-to-one is no
    // longer polled once it moved.
    void continueFor(entry).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("[continuity] pass failed", err);
    });
    continueAll(false);
    return { yieldedRows: result.outcome === "applied" && end > start };
  }

  /** In-flight removals, keyed on `(group, superseded key)`. */
  const evictionInFlight = new Set<string>();

  /** This device's ADR-0022 role in `entry`, read off the live group. */
  async function ownRoleIn(entry: ActiveGroup): Promise<PushRotationRole> {
    const secrets = auth.identitySecrets();
    if (secrets === null) return null;
    const wasm = await loadWasm();
    const identity = wasm.Identity.fromSeed(conversationSeedOf(secrets));
    try {
      return await readPushRotationRole(entry.group, identity);
    } finally {
      identity.free();
    }
  }

  /**
   * ADR-0073 R.7 case 2 — the background half of the in-place repair
   * (`chat/reset-repair.ts`): in a role-table group this account
   * administers and the superseded identity does not own, propose the
   * removal of a leaf still bound to a superseded key. The new identity's
   * devices are added by the re-admission arm below. The one-to-one and the
   * owned group are continued in a successor (`continueFor`).
   */
  async function repairResetFor(entry: ActiveGroup, fromView = false): Promise<void> {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    if (s.kind !== "session" || secrets === null) return;
    // The open chat proposes the removals for its own group (it holds the
    // pending-request state they are shown from). The owner's admin restore
    // has no home in the view, so it runs from here for an open chat too:
    // it must not wait for the owner to close the conversation.
    const foreground = coordinator.isForeground(entry.groupIdB64);
    if (foreground && !fromView) return;
    if (readIsActive(entry.group) === false) return;
    const superseded = loadSupersededKeys();
    if (superseded.size === 0) return;
    let leaves: RosterLeaf[];
    try {
      leaves = entry.group.members() as unknown as RosterLeaf[];
    } catch {
      return;
    }
    const roster = leaves.map((l) => ({
      leafIndex: l.leafIndex,
      personKeyB64: base64Encode(personKeyOf(l)),
    }));
    const present = [...superseded.keys()].filter((k) => roster.some((l) => l.personKeyB64 === k));
    const kind = conversationKindOf(entry.group);
    const accountIdB64 = s.account.accountId;
    let ownerKeyB64: string | undefined;
    try {
      const o = entry.group.roleTableOwner() as Uint8Array | undefined;
      ownerKeyB64 = o === undefined ? undefined : base64Encode(o);
    } catch {
      return;
    }
    const ownRole = await ownRoleIn(entry);
    let pendingRemoveLeaves: number[] = [];
    try {
      pendingRemoveLeaves = (entry.group.pendingProposals() as ProposalRecordView[])
        .filter((r) => r.kind === "remove" && r.removeTargetLeaf !== undefined)
        .map((r) => r.removeTargetLeaf as number);
    } catch {
      return;
    }
    let proposed = 0;
    for (const supersededKeyB64 of foreground ? [] : present) {
      if (resetCase({ kind, ownerKeyB64, supersededKeyB64 }) !== "repair") continue;
      const key = `${entry.groupIdB64}:${supersededKeyB64}`;
      const plan = repairPlan({
        ownRole,
        roster,
        supersededKeyB64,
        pendingRemoveLeaves,
        removeInFlight: evictionInFlight.has(key),
        membershipEnded: false,
      });
      const leafIndex = plan.removeLeaf;
      if (leafIndex === null) continue;
      evictionInFlight.add(key);
      try {
        const outcome = await contacts.removeMemberFromGroup(secrets, entry.group, leafIndex);
        proposed += 1;
        if (outcome.kind === "proposed") {
          markOwnPublished(entry.groupIdB64, outcome.proposalMessageId);
          recordObservation(accountIdB64, entry.groupIdB64, `remove:${leafIndex}`, Date.now(), true);
        } else {
          markOwnPublished(entry.groupIdB64, outcome.commitMessageId);
        }
      } catch (err) {
        // Silent by design: the next pass tries again.
        // eslint-disable-next-line no-console
        console.warn("ADR-0073 reset repair: remove not proposed", err);
      } finally {
        evictionInFlight.delete(key);
      }
    }
    // R.7 case 2 — if the superseded key was an admin, the OWNER's device
    // puts the new key in its place: one role change per pass (C-65), the
    // new key first. Silent like the rest.
    if (kind === "group" && proposed === 0 && ownRole === "owner") {
      let adminKeysB64: string[] = [];
      let roleChangePending = false;
      try {
        const flat = entry.group.roleTableAdmins() as Uint8Array;
        for (let i = 0; i + 32 <= flat.length; i += 32) adminKeysB64.push(base64Encode(flat.slice(i, i + 32)));
        roleChangePending = (entry.group.pendingProposals() as ProposalRecordView[]).some(
          (r) => r.kind === "role-change",
        );
      } catch {
        adminKeysB64 = [];
        roleChangePending = true;
      }
      for (const [supersededKeyB64, accountOfKey] of superseded) {
        const newKeyB64 = readPin(accountOfKey)?.pinnedSignaturePubkeyB64;
        if (newKeyB64 === undefined) continue;
        const step = adminRestoreStep({
          ownRole,
          adminKeysB64,
          supersededKeyB64,
          newKeyB64,
          rosterPersonKeysB64: roster.map((l) => l.personKeyB64),
          roleChangePending,
        });
        if (step === null) continue;
        try {
          const outcome = await contacts.setMemberAdmin(
            secrets,
            entry.group,
            base64Decode(step.keyB64),
            step.kind === "promote",
          );
          const id = outcome.kind === "proposed" ? outcome.proposalMessageId : outcome.commitMessageId;
          markOwnPublished(entry.groupIdB64, id);
          proposed += 1;
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("ADR-0073 reset repair: admin not restored yet", err);
        }
        break;
      }
    }
    if (proposed > 0) await sweepGroup(entry);
  }

  // ---------------------------------------------------------------------
  // ADR-0073 R.5 to R.7 — a conversation that continues in a successor
  // ---------------------------------------------------------------------
  //
  // The rules are `successor-claim.ts` / `reset-repair.ts`, the order is
  // `continuity-runtime.ts`; what follows is the port that order drives.
  // None of it has a surface (R.0).

  /** Successors this device created and has not made a held conversation
   *  yet, by base64 id. In memory: a restart finds the group in the MLS
   *  snapshot instead (`Group.load`). */
  const preparedGroups = new Map<string, ActiveGroup["group"]>();
  /** One pass per old group at a time. */
  const continuityInFlight = new Set<string>();

  function continuityFacts(groupIdB64: string): ContinuityFacts | undefined {
    const entry = chats.get(groupIdB64);
    if (entry === undefined) return undefined;
    try {
      const owner = entry.group.roleTableOwner() as Uint8Array | undefined;
      const flat = entry.group.roleTableAdmins() as Uint8Array;
      const adminKeysB64: string[] = [];
      for (let i = 0; i + 32 <= flat.length; i += 32) adminKeysB64.push(base64Encode(flat.slice(i, i + 32)));
      return {
        kind: conversationKindOf(entry.group),
        rosterPersonKeysB64: personKeysOf(entry.group),
        ownerKeyB64: owner === undefined ? undefined : base64Encode(owner),
        adminKeysB64,
        active: readIsActive(entry.group) !== false,
      };
    } catch {
      return undefined;
    }
  }

  /**
   * The account id a person's CURRENT devices may be fetched under, or
   * `null`. An id is followed only when this device trusts the roster key
   * for it: the pin record, or a binding it verified itself against the
   * directory (`contacts/verified-binding.ts`). A claimed id (a profile's,
   * or a contact row adopted from one) is first put through the identity
   * check, which records a binding when the directory serves exactly this
   * key; when it does not, nobody is added under that id.
   */
  async function verifiedAccountIdOf(personKeyB64: string): Promise<Uint8Array | null> {
    const accountId = accountIdOfPerson(personKeyB64);
    if (accountId === null) return null;
    const idB64 = base64Encode(accountId);
    const standing = (): LookupStanding =>
      lookupStanding({
        rosterKeyB64: personKeyB64,
        pin: readPin(idB64),
        binding: readVerifiedBinding(idB64),
      });
    if (standing() === "same-person") return accountId;
    try {
      await contacts.checkContactIdentity(accountId, personKeyB64);
    } catch {
      return null;
    }
    return standing() === "same-person" ? accountId : null;
  }

  /** The account id (16 bytes) a person's key is CLAIMED or known under: a
   *  local record first, else the id their own profile carries. Not a
   *  statement of trust: see {@link verifiedAccountIdOf}. */
  function accountIdOfPerson(personKeyB64: string): Uint8Array | null {
    const trusted = accountIdB64OfKey(personKeyB64);
    if (trusted !== null) {
      try {
        return base64Decode(trusted);
      } catch {
        return null;
      }
    }
    const claimed = profiles.accountIdHexOf(personKeyB64);
    return claimed === null ? null : hexToBytes(claimed);
  }

  /** Publish what a membership call returned and sweep, as the chat view does. */
  async function settleMembership(
    entry: ActiveGroup,
    outcome: { kind: string; proposalMessageId?: number; commitMessageId?: number },
  ): Promise<void> {
    const id = outcome.kind === "proposed" ? outcome.proposalMessageId : outcome.commitMessageId;
    if (id !== undefined) markOwnPublished(entry.groupIdB64, id);
    await sweepGroup(entry);
  }

  const continuityPort: ContinuityPort = {
    ownPersonKeyB64: () => ownPersonKeyB64(chats.identity()),
    supersededKeys: () => loadSupersededKeys(),
    currentKeyOf: (accountIdB64) => readPin(accountIdB64)?.pinnedSignaturePubkeyB64 ?? null,
    facts: continuityFacts,
    claimState: (oldGroupIdB64) => groupClosed.stateOf(oldGroupIdB64) ?? NO_CLAIMS,
    cursorOf: (oldGroupIdB64) => coordinator.cursor(oldGroupIdB64),
    async newIdentityHasDevices(accountIdB64) {
      const wanted = readPin(accountIdB64)?.pinnedSignaturePubkeyB64;
      if (wanted === undefined) return false;
      try {
        const wasm = await loadWasm();
        const list = await discovery.fetchKeyPackages(base64Decode(accountIdB64));
        // Only a device bound to the key this device ACCEPTED counts: the
        // directory is not trusted to say whose devices these are.
        return (list ?? []).some((d) => {
          try {
            const bound = wasm.keyPackageAccountKey(d.keyPackage);
            return bound !== undefined && base64Encode(bound) === wanted;
          } catch {
            return false;
          }
        });
      } catch {
        return false;
      }
    },
    async prepareSuccessor(oldGroupIdB64, newIdentityAccountIdB64) {
      const secrets = auth.identitySecrets();
      const old = chats.get(oldGroupIdB64);
      if (secrets === null || old === undefined) throw new Error("no session");
      const prepared = await contacts.prepareGroupWith(
        secrets,
        base64Decode(newIdentityAccountIdB64),
        conversationKindOf(old.group),
      );
      const successorGroupIdB64 = base64Encode(prepared.group.groupId);
      preparedGroups.set(successorGroupIdB64, prepared.group);
      await groupClosed.setPrepared(oldGroupIdB64, {
        successorGroupIdB64,
        welcomeB64: base64Encode(prepared.welcome),
        targetKeysB64: prepared.targets.map((t) => base64Encode(t.accountKey)),
        delivered: false,
      });
      return successorGroupIdB64;
    },
    async postClaim(oldGroupIdB64, successorGroupIdB64) {
      const old = chats.get(oldGroupIdB64);
      if (old === undefined) throw new Error("the old group is not held");
      const published = await groupClosed.postClaim(old, base64Decode(successorGroupIdB64));
      return published.messageId;
    },
    abandonOwnClaim: (oldGroupIdB64) => groupClosed.abandonOwn(oldGroupIdB64),
    async discardSuccessor(successorGroupIdB64) {
      const group = preparedGroups.get(successorGroupIdB64);
      preparedGroups.delete(successorGroupIdB64);
      try {
        group?.free();
      } catch {
        /* already freed */
      }
      // The prepared record names a successor nobody will ever join.
      for (const g of chats.groups()) {
        const state = groupClosed.stateOf(g.groupIdB64);
        if (state?.prepared?.successorGroupIdB64 === successorGroupIdB64 && !state.prepared.delivered) {
          await groupClosed.setPrepared(g.groupIdB64, null);
        }
      }
    },
    async settle(oldGroupIdB64, winner) {
      const old = chats.get(oldGroupIdB64);
      if (old !== undefined) await groupClosed.settle(old, winner);
    },
    async recordLineage(oldGroupIdB64, successorGroupIdB64) {
      const old = chats.get(oldGroupIdB64);
      const account = activeAccount;
      if (old === undefined || account === null) return;
      noteConversationMoved(account, oldGroupIdB64, Date.now());
      await sync.setLineage(
        base64Decode(successorGroupIdB64),
        lineageView.conversationIdOf(old.group.groupId),
      );
    },
    async deliverSuccessor(oldGroupIdB64, successorGroupIdB64) {
      const s = auth.state();
      const secrets = auth.identitySecrets();
      const old = chats.get(oldGroupIdB64);
      if (s.kind !== "session" || secrets === null || old === undefined) return false;
      const prepared = groupClosed.stateOf(oldGroupIdB64)?.prepared ?? null;
      const founder = prepared !== null && prepared.successorGroupIdB64 === successorGroupIdB64;
      let held = chats.get(successorGroupIdB64);
      // A device of this account that did not create the successor joins it
      // from the Welcome the founder sent to the account's own mailbox.
      if (!founder) return held !== undefined;
      if (held === undefined) {
        let group = preparedGroups.get(successorGroupIdB64);
        if (group === undefined) {
          const identity = chats.identity();
          if (identity === null) return false;
          const wasm = await loadWasm();
          // A restart between the claim and the delivery: the successor is
          // in the MLS snapshot the creation persisted.
          const loaded = wasm.Group.load(identity, base64Decode(successorGroupIdB64));
          if (loaded === undefined) return false;
          group = loaded;
        }
        preparedGroups.delete(successorGroupIdB64);
        held = chats.add({ label: old.label, group });
        profiles.groupJoined(held.groupIdB64);
        // FAIL-CLOSED before the Welcomes below are published: a refused
        // persist (the session ended, Forget device sealed the stores)
        // means nothing is published. The successor's MLS state was
        // recorded at creation; what this records is the held list, and
        // the throw is what stops a delivery for an account that has left
        // this browser.
        await chats.persistSnapshotDurable(s.account.accountId);
      }
      if (!prepared.delivered) {
        if (prepared.welcomeB64 === null) return false;
        await contacts.deliverPreparedWelcomes(
          held.group.groupId,
          base64Decode(prepared.welcomeB64),
          prepared.targetKeysB64.map((k) => ({ accountKey: base64Decode(k), isRecipient: false })),
        );
        // Kept as the mark "this device founded it"; the Welcome is dropped.
        await groupClosed.setPrepared(oldGroupIdB64, {
          successorGroupIdB64,
          welcomeB64: null,
          targetKeysB64: [],
          delivered: true,
        });
      }
      return true;
    },
    async carryName(oldGroupIdB64, successorGroupIdB64) {
      const old = chats.get(oldGroupIdB64);
      const successor = chats.get(successorGroupIdB64);
      if (old !== undefined && successor !== undefined) await groupNames.carryOver(old, successor);
    },
    async addPerson(successorGroupIdB64, personKeyB64) {
      const secrets = auth.identitySecrets();
      const successor = chats.get(successorGroupIdB64);
      const accountId = await verifiedAccountIdOf(personKeyB64);
      if (secrets === null || successor === undefined || accountId === null) return false;
      try {
        const outcome = await contacts.addMemberToGroup(
          secrets,
          successor.group,
          accountId,
          base64Decode(personKeyB64),
        );
        await settleMembership(successor, outcome);
        return true;
      } catch (err) {
        // Not resolvable or not reachable now: they keep writing in the old
        // group and the completeness re-check tries again (R.7 case 3).
        // eslint-disable-next-line no-console
        console.info("[continuity] a member was not added to the successor yet", err instanceof Error ? err.name : "");
        return false;
      }
    },
    // Its own slot name, so the re-check and the ADR-0050 arm of the same
    // group do not consume each other's interval.
    recheckDue: (successorGroupIdB64) =>
      takeReadmissionSlot(`completeness:${successorGroupIdB64}`, Date.now()),
    async setAdmins(successorGroupIdB64, adminKeysB64) {
      const secrets = auth.identitySecrets();
      const successor = chats.get(successorGroupIdB64);
      if (secrets === null || successor === undefined) return;
      const have = continuityFacts(successorGroupIdB64)?.adminKeysB64 ?? [];
      // One role change per sweep (C-65): one admin per pass.
      const next = adminKeysB64.find((k) => !have.includes(k));
      if (next === undefined) return;
      try {
        const outcome = await contacts.setMemberAdmin(secrets, successor.group, base64Decode(next), true);
        await settleMembership(successor, outcome);
      } catch (err) {
        // A pending role change refuses a second one; the next pass tries.
        // eslint-disable-next-line no-console
        console.info("[continuity] an admin was not restored yet", err instanceof Error ? err.name : "");
      }
    },
    async transferOwnership(successorGroupIdB64, newOwnerKeyB64, stayAdmin) {
      const secrets = auth.identitySecrets();
      const successor = chats.get(successorGroupIdB64);
      if (secrets === null || successor === undefined) return;
      try {
        const outcome = await contacts.transferGroupOwnership(
          secrets,
          successor.group,
          base64Decode(newOwnerKeyB64),
          stayAdmin ? "admin" : "member",
        );
        await settleMembership(successor, outcome);
      } catch (err) {
        // Already pending, or a role change is in the way: the next pass.
        // eslint-disable-next-line no-console
        console.info("[continuity] the owner was not restored yet", err instanceof Error ? err.name : "");
      }
    },
    async resendOwnMessages(oldGroupIdB64, successorGroupIdB64, accountIdB64) {
      const s = auth.state();
      const secrets = auth.identitySecrets();
      const successor = chats.get(successorGroupIdB64);
      const old = chats.get(oldGroupIdB64);
      if (s.kind !== "session" || secrets === null || successor === undefined || old === undefined) return;
      const account = s.account.accountId;
      // The founder alone re-sends, once: another device of this account
      // that joined the successor holds no prepared record for it.
      const prepared = groupClosed.stateOf(oldGroupIdB64)?.prepared ?? null;
      if (prepared === null || prepared.successorGroupIdB64 !== successorGroupIdB64) return;
      if (wasResent(account, oldGroupIdB64)) return;
      const signedAtMs = readPin(accountIdB64)?.resetSignedAtMs;
      if (signedAtMs === undefined) return;
      const deviceKey = await getDeviceKey(account);
      if (deviceKey === null) return;
      const log = await loadMessageLog(account, deviceKey);
      const stored = log?.groups[hexOf(old)]?.messages ?? [];
      const byId = new Map(stored.map((m) => [Number(m.messageId), m]));
      const ids = messagesToResend(
        stored.map((m) => ({
          messageId: Number(m.messageId),
          own: m.direction === "outgoing",
          sentAtMs: Number.isNaN(Date.parse(m.createdAt)) ? null : Date.parse(m.createdAt),
          resendable: m.deleted !== true && (m.text.length > 0 || m.attachments.length > 0),
        })),
        signedAtMs,
      );
      // An interrupted re-send continues after the last message that LANDED
      // (the durable cursor), never from the first.
      for (const id of resendRemaining(ids, resendCursorOf(account, oldGroupIdB64))) {
        const m = byId.get(id);
        if (m === undefined) continue;
        // The same content, sealed again for the new session. The stored
        // attachment references are reused: it is the same conversation.
        // A reply keeps its quote (target 0: no jump) and a forward its
        // attribution and origin. The marker names the ORIGINAL group and
        // message id: a device of this account that holds the original
        // shows the message once (`resend-marker.ts`). Never drawn.
        const envelope = await composeResendEnvelope(
          m.text,
          m.attachments.map((a) => attachmentRefFromPersisted(a).ref),
          { groupIdB64: oldGroupIdB64, messageId: id },
          resendParts({
            reply: replyRefFromPersisted(m.reply),
            forward: forwardRefFromPersisted(m.forward),
          }),
        );
        // A throw leaves the cursor where it is: the next pass sends this
        // message again and nothing before it.
        await chat.sendMessage(secrets, successor.group, envelope);
        advanceResendCursor(account, oldGroupIdB64, id);
      }
      markResent(account, oldGroupIdB64);
    },
  };

  /** One continuity pass over one group this device holds. */
  async function continueFor(entry: ActiveGroup): Promise<void> {
    if (activeAccount === null || continuityInFlight.has(entry.groupIdB64)) return;
    if (loadSupersededKeys().size === 0) return;
    continuityInFlight.add(entry.groupIdB64);
    try {
      const step = await continueConversation(continuityPort, entry.groupIdB64);
      if (step !== "nothing" && step !== "adopted") {
        // eslint-disable-next-line no-console
        console.info(`[continuity] ${step}`);
      }
    } finally {
      continuityInFlight.delete(entry.groupIdB64);
    }
  }

  /** How often every held group is offered a continuity pass from the tail
   *  of ANY poll: an old one-to-one is no longer polled once it moved, so
   *  its own poll cannot be what resumes an interrupted build. Chosen. */
  const CONTINUITY_SWEEP_INTERVAL_MS = 30_000;
  let lastContinuitySweepAtMs = 0;
  /**
   * ADR-0073 R.5 — once this device is in the group that continues a
   * conversation, its push subscription for the OLDER group is retired: the
   * conversation's wakes come through the successor's own routing id. Once
   * per old group; best-effort like every push teardown (a failed
   * unsubscribe leaves a row nothing useful notifies).
   */
  async function retireMovedPush(entry: ActiveGroup): Promise<void> {
    const account = activeAccount;
    if (account === null) return;
    const view = lineageView.resolve(entry.groupIdB64);
    if (
      !retiresOldSegmentPush({
        groupHex: hexOf(entry),
        headHex: view === undefined ? null : hexOf(view.head),
        headActive: view !== undefined && readIsActive(view.head.group) !== false,
        alreadyRetired: wasPushRetired(account, entry.groupIdB64),
      })
    ) {
      return;
    }
    const deviceKey = await getDeviceKey(account);
    if (deviceKey === null) return;
    const secret = await getGroupRoutingSecret(account, entry.groupIdB64, deviceKey);
    if (secret !== null) await retireOldPushGid(account, secret);
    markPushRetired(account, entry.groupIdB64);
  }

  /** The key that makes `group` unrepairable for an accepted reset, or
   *  `null` (`successor-claim.ts` `unrepairableBy`, over the live group). */
  function unrepairableKeyOf(group: ActiveGroup["group"]): string | null {
    const superseded = loadSupersededKeys();
    if (superseded.size === 0) return null;
    let owner: string | undefined;
    try {
      const o = group.roleTableOwner() as Uint8Array | undefined;
      owner = o === undefined ? undefined : base64Encode(o);
    } catch {
      owner = undefined;
    }
    return unrepairableBy({
      kind: conversationKindOf(group),
      rosterPersonKeysB64: personKeysOf(group),
      ownPersonKeyB64: ownPersonKeyB64(chats.identity()),
      ownerKeyB64: owner,
      supersededKeys: superseded,
    });
  }

  // ADR-0073 R.7 case 1 — nothing is published into a one-to-one whose other
  // person's reset this device accepted, by ANY client in the tab; the
  // successor claim alone goes there.
  installDeadSessionGuard((group) =>
    isDeadSession({
      kind: conversationKindOf(group),
      unrepairableKeyB64: unrepairableKeyOf(group),
    }),
  );

  /** One flush of the held messages at a time. */
  let heldFlushRunning = false;

  /**
   * ADR-0073 R.7 case 1 — send the messages held for conversations whose
   * successor now exists on this device, when no view of the conversation is
   * mounted to do it (`held-send.ts` `flushVerdict`). Each goes into the
   * conversation's current group and into that group's slot of the message
   * log (the only record of an own message). A send that fails puts the rest
   * back, in order, for the next pass.
   */
  async function flushHeldSends(): Promise<void> {
    const account = activeAccount;
    const secrets = auth.identitySecrets();
    if (account === null || secrets === null || heldFlushRunning) return;
    heldFlushRunning = true;
    try {
      await restoreHeldSends(account);
      for (const [conversationHex, items] of heldSends.all()) {
        const view = lineageView.conversations().find((c) => c.conversationHex === conversationHex);
        if (view === undefined) continue;
        const verdict = flushVerdict({
          held: items.length,
          viewMounted: view.held.some((g) => coordinator.isForeground(g.groupIdB64)),
          headActive: readIsActive(view.head.group) !== false,
          headRoute: sendRoute({
            kind: conversationKindOf(view.head.group),
            unrepairableKeyB64: unrepairableKeyOf(view.head.group),
          }),
        });
        if (verdict !== "flush") continue;
        const taken = heldSends.take(conversationHex);
        const sent: PersistedMessage[] = [];
        for (let i = 0; i < taken.length; i += 1) {
          const held = taken[i] as (typeof taken)[number];
          const m = held.payload;
          // A reply composed in the old segment keeps its quote and loses
          // its jump target: the id belongs to the old journal.
          const reply = replyForSend(m.reply, m.replyGroupIdB64, view.head.groupIdB64);
          try {
            const envelope = await composeHeldEnvelope(m, reply);
            const result = await chat.sendMessage(secrets, view.head.group, envelope);
            sent.push({
              messageId: String(result.messageId),
              direction: "outgoing",
              text: m.text,
              attachments: m.attached.map((a) => persistedFromAttachmentRef(a)),
              createdAt: result.createdAt,
              ...(reply === undefined ? {} : { reply: persistedFromReplyRef(reply) }),
              ...(m.forward === undefined ? {} : { forward: persistedFromForwardRef(m.forward) }),
              ...(m.content === undefined ? {} : { content: m.content }),
            });
          } catch (err) {
            // Not sent: this one and everything after it stay held, in order.
            heldSends.putBack(conversationHex, taken.slice(i));
            // eslint-disable-next-line no-console
            console.info("[held-send] a held message was not sent yet", err instanceof Error ? err.name : "");
            break;
          }
        }
        if (sent.length > 0) {
          const deviceKey = await getDeviceKey(account);
          if (deviceKey !== null) {
            const hex = hexOf(view.head);
            await updateMessageLog(account, deviceKey, (current) => {
              const groups = current?.groups ?? {};
              return {
                version: MESSAGE_LOG_VERSION,
                groups: { ...groups, [hex]: appendToGroupSlot(groups[hex], sent, 0) },
              };
            });
          }
          // A view of the conversation that was mounting while this ran
          // read the log before the write above and found the queue empty:
          // it is told what went out and shows it now.
          notifyHeldFlushed({ groupIdB64: view.head.groupIdB64, messages: sent });
        }
        await persistHeldSends(account);
      }
    } finally {
      heldFlushRunning = false;
    }
  }

  function continueAll(force: boolean): void {
    const now = Date.now();
    if (!force && now - lastContinuitySweepAtMs < CONTINUITY_SWEEP_INTERVAL_MS) return;
    lastContinuitySweepAtMs = now;
    void flushHeldSends().catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("[held-send] flush failed", err);
    });
    // Not gated on a superseded key: a device that learned the lineage from
    // another device of the account retires the old subscription too.
    for (const entry of chats.groups()) {
      void retireMovedPush(entry).catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("[continuity] old push subscription not retired", err);
      });
    }
    if (loadSupersededKeys().size === 0) return;
    for (const entry of chats.groups()) {
      void groupClosed
        .rejudge(entry)
        .then(() => continueFor(entry))
        .catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("[continuity] pass failed", err);
        });
    }
  }

  function continuityPass(groupIdB64: string): void {
    const entry = chats.get(groupIdB64);
    if (entry !== undefined && loadSupersededKeys().size > 0) {
      // R.7 case 2 — the owner's admin restore for the OPEN chat.
      void repairResetFor(entry, true).catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("ADR-0073 reset repair pass failed", err);
      });
    }
    if (entry !== undefined && loadSupersededKeys().size > 0) {
      void groupClosed
        .rejudge(entry)
        .then(() => continueFor(entry))
        .catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("[continuity] pass failed", err);
        });
    }
    continueAll(false);
  }

  // An accepted reset (wherever the identity gate met it) starts the work at
  // once, for every conversation shared with that account.
  /** ADR-0073 R.8 — the service worker's copy of the retired keys
   *  (`push/superseded-mirror.ts`), so a push from a removed device that
   *  still holds old keys neither rings nor posts a preview. Best effort. */
  function mirrorSupersededKeys(): void {
    const account = activeAccount;
    if (account === null) return;
    void writeSupersededMirror(account, loadSupersededKeys().keys()).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("[continuity] the retired keys were not mirrored for the service worker", err);
    });
  }

  const unsubscribeResetAccepted = subscribeResetAccepted(() => {
    mirrorSupersededKeys();
    return continueAll(true);
  });
  // A claim kept for a later verdict triggers the identity check of the
  // account that would make the group unrepairable, then is judged again.
  groupClosed.onPendingClaim((entry) => {
    void (async () => {
      const facts = continuityFacts(entry.groupIdB64);
      const own = ownPersonKeyB64(chats.identity());
      if (facts === undefined) return;
      const candidates =
        facts.kind === "chat"
          ? facts.rosterPersonKeysB64.filter((k) => k !== own)
          : facts.ownerKeyB64 === undefined
            ? []
            : [facts.ownerKeyB64];
      for (const personKeyB64 of candidates) {
        const accountId = accountIdOfPerson(personKeyB64);
        if (accountId === null) continue;
        try {
          await contacts.checkContactIdentity(accountId, personKeyB64);
        } catch {
          // Refused or no verdict: the claim stays pending.
        }
      }
      await groupClosed.rejudge(entry);
      await continueFor(entry);
    })().catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("[continuity] pending-claim check failed", err);
    });
  });
  // An accepted claim is what tells a device that was in the old group which
  // group continues the conversation (R.5 (a)).
  groupClosed.onAcceptedClaim((entry, claim) => {
    void continuityPort
      .recordLineage(entry.groupIdB64, claim.successorGroupIdB64)
      .catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("[continuity] lineage not recorded", err);
      });
  });
  // A Welcome's grant scope is widened through the lineage (R.7 case 3).
  installScopeLineage((scopeHex) => lineageView.lineage().segmentsOf(lineageView.lineage().conversationOf(scopeHex)));

  /**
   * ADR-0050 section 2.5a point 4 — the background half of the superseded
   * re-request (`chat/membership-retry.ts`); the chat view runs the same pass
   * for the group it has open. Never throws: a failed pass leaves every
   * record where it was for the next merged commit.
   */
  async function rerequestSupersededFor(entry: ActiveGroup): Promise<void> {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    if (s.kind !== "session" || secrets === null) return;
    // The chat view runs this pass for the group it has open.
    if (coordinator.isForeground(entry.groupIdB64)) return;
    try {
      await runSupersededRerequests({
        accountIdB64: s.account.accountId,
        groupIdB64: entry.groupIdB64,
        snapshot: () =>
          readIntentSnapshot(entry.group, readIsActive(entry.group) === false),
        rerequest: (request) =>
          reissueOwnRequest(
            {
              contacts,
              identity: secrets,
              group: entry.group,
              markOwnPublished: (id) => markOwnPublished(entry.groupIdB64, id),
              // So the panel shows it as ours when the chat is opened (the UX
              // section 10 own-request exception), as the chat view does.
              noteOwnProposal: (key, transferToB64) =>
                recordObservation(
                  s.account.accountId,
                  entry.groupIdB64,
                  key,
                  Date.now(),
                  true,
                  transferToB64,
                ),
            },
            request,
          ),
        retryMaySucceed: (reason) => {
          try {
            return membershipRefusalRetryMaySucceed(reason);
          } catch {
            return false;
          }
        },
      });
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("background receive: superseded re-request pass failed", err);
    }
  }

  /**
   * ADR-0060 section 6 — the background half of the committer envelope
   * re-send (`reach/committer-envelope.ts`); the chat view runs the same pass
   * for the group it has open.
   */
  async function resendCommitterEnvelopesFor(entry: ActiveGroup): Promise<void> {
    const secrets = auth.identitySecrets();
    if (secrets === null) return;
    if (coordinator.isForeground(entry.groupIdB64)) return;
    await contacts.resendCommitterEnvelopes(
      secrets,
      entry.group,
      readIsActive(entry.group) === false,
    );
  }

  /**
   * ADR-0050 section 2.4 — re-check the device lists of the accounts in
   * `entry` against its roster: propose an Add for a listed device with no
   * leaf (a restored phone, a pairing whose fan-out was cut short), and — for
   * this account only — a Remove for a leaf whose directory row is gone
   * (`forgetDevice`). Accounts are resolved to directory ids through this
   * device's own account, its accepted contacts and the account id a
   * member's profile carries (ADR-0073 R.2); a member with none is left as
   * is. Each chat is checked at most once per
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
    const current = binding.capture();
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
    // The accounts we can look up: ours, and every OTHER person of the roster
    // whose account id this device knows: a contact row, else the id the
    // person's own profile carries (ADR-0073 R.2 "Triggers").
    const lookups: { personKeyB64: string; accountId: Uint8Array }[] = [
      { personKeyB64: own, accountId: secrets.accountId },
    ];
    const kind = conversationKindOf(entry.group);
    let ownerKeyB64: string | undefined;
    try {
      const o = entry.group.roleTableOwner() as Uint8Array | undefined;
      ownerKeyB64 = o === undefined ? undefined : base64Encode(o);
    } catch {
      ownerKeyB64 = undefined;
    }
    let ownRole: PushRotationRole | undefined;
    const candidates = readmissionCandidates({
      rosterPersonKeysB64: roster.map((l) => l.personKeyB64),
      ownPersonKeyB64: own,
      contacts: loadAccepted(),
      profileAccountIdHexOf: (k) => profiles.accountIdHexOf(k),
    });
    for (const c of candidates) {
      const id = hexToBytes(c.accountIdHex);
      if (id === null) continue;
      // The scheduled identity check (R.2): a proven rotation re-pins, an
      // attested reset is accepted and re-pinned, silently; an unproven
      // change and "no verdict" leave the account alone. For a member this
      // device never pinned, the roster's key is what it holds.
      try {
        await contacts.checkContactIdentity(id, c.rosterKeyB64);
      } catch {
        continue;
      }
      const standing = lookupStanding({
        rosterKeyB64: c.rosterKeyB64,
        pin: readPin(base64Encode(id)),
        binding: readVerifiedBinding(base64Encode(id)),
      });
      if (standing === "skip") continue;
      if (standing === "reset-identity") {
        // R.7 case 2: the new identity's devices are proposed by an owner or
        // an admin of a group that can be repaired in place. The one-to-one
        // and the owned group continue in a successor instead.
        if (resetCase({ kind, ownerKeyB64, supersededKeyB64: c.rosterKeyB64 }) !== "repair") continue;
        ownRole ??= await ownRoleIn(entry);
        if (!mayAddResetIdentity(ownRole)) continue;
      }
      lookups.push({ personKeyB64: c.rosterKeyB64, accountId: id });
    }
    const accounts: AccountDevices[] = [];
    const wasm = await loadWasm();
    // Which account each listed device belongs to, so an admission records
    // the account it can later re-fetch a KeyPackage from (ADR-0050 2.5a
    // point 4).
    const accountOfDevice = new Map<string, Uint8Array>();
    for (const l of lookups) {
      try {
        const list = await discovery.fetchKeyPackages(l.accountId);
        for (const d of list ?? []) accountOfDevice.set(base64Encode(d.deviceId), l.accountId);
        // ADR-0074 — an own device's KeyPackage bound to another of the
        // account's keys than this device's leaves is not admitted; its row
        // still counts as listed, so its existing leaf is not retired.
        const ownRow = l.personKeyB64 === own;
        accounts.push({
          personKeyB64: l.personKeyB64,
          devices:
            list === null
              ? []
              : list.map((d) => ({
                  deviceKeyB64: base64Encode(d.deviceId),
                  keyPackage: d.keyPackage,
                  ...(ownRow
                    ? { admissible: ownDeviceKeyPackageUsable(wasm, d.keyPackage, own) }
                    : {}),
                })),
        });
      } catch {
        accounts.push({ personKeyB64: l.personKeyB64, devices: null });
      }
      // The lookups are network round trips; `entry` and `secrets` are the
      // session's that started them.
      if (!current()) return;
    }
    // ADR-0074 section 2.3 — an own device is admitted only when the
    // account's DeviceGroup holds it.
    const deviceGroup = ownDeviceRoster(account);
    const plan = planReadmission({
      roster,
      ownPersonKeyB64: own,
      accounts,
      removed: removedDevices(account, entry.groupIdB64),
      ownDeviceGroup:
        deviceGroup === null
          ? null
          : new Set([...deviceGroup.keysHex].map((hex) => base64Encode(hexToBytes(hex) ?? new Uint8Array(0)))),
    });
    if (plan.admit.length === 0 && plan.retire.length === 0) return;
    let proposed = 0;
    for (const d of plan.admit) {
      if (!current()) return;
      try {
        const outcome = await contacts.admitDeviceToGroup(
          secrets,
          entry.group,
          d.keyPackage,
          accountOfDevice.get(d.deviceKeyB64),
        );
        if (!current()) return;
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
      if (!current()) return;
      try {
        const outcome = await contacts.retireDeviceFromGroup(
          secrets,
          entry.group,
          base64Decode(leafKeyB64),
        );
        if (!current()) return;
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
  async function actingCommitterFor(
    entry: ActiveGroup,
    ownRank: number,
    ownSignatureKey: Uint8Array,
  ): Promise<ActingCommitter> {
    const s = auth.state();
    if (s.kind !== "session") return { kind: "defer", toRank: undefined };
    const wasm = await loadWasm();
    return decideActingCommitter({
      accountIdB64: s.account.accountId,
      groupIdB64: entry.groupIdB64,
      ownRank,
      ownSignatureKey,
      rankOf: (senderKeyB64) => {
        try {
          return entry.group.committerRank(base64Decode(senderKeyB64));
        } catch {
          return undefined;
        }
      },
      nowMs: Date.now(),
      freshnessWindowMs: wasm.committerBeaconFreshnessWindowMs(),
      fold: wasmActingCommitterFold(wasm),
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
    const current = binding.capture();
    const start = await startCursor(account, entry, current);
    if (!current()) return null;
    const before = memberSignatureKeys(entry.group);
    // docs/ux/second-device.md section 4 — the people before the sweep's reads.
    const peopleBefore = personKeysOrNull(entry);
    const swept: IncomingChatEvent[] = [];
    let outcome: SweepOutcome | null = null;
    let reached = start;
    try {
      outcome = await contacts.sweepIfCommitter(
        secrets,
        entry.group,
        {
          read: () => (current() ? (coordinator.cursor(entry.groupIdB64) ?? start) : reached),
          advance: (to) => {
            reached = Math.max(reached, to);
            if (current()) coordinator.noteCursor(entry.groupIdB64, to);
          },
        },
        // Named group, not a bare id: this one `ChatClient` polls every chat,
        // so a group-blind mark would suppress another chat's row of the same
        // DS id (`chat/own-published.ts`).
        (id) => chat.markOwnPublishedCommit(entry.groupIdB64, id),
        (rank, ownKey) => actingCommitterFor(entry, rank, ownKey),
        (event) => swept.push(event),
      );
    } finally {
      // Applied and cursor-advanced whether or not our commit won: the only
      // question left is whether they reach the log and the view. A merged
      // own commit joins them as the batch's last commit row, so this device
      // records the same named membership row as every other member
      // (docs/ux/second-device.md section 4).
      const batch = sweepBatchWithOwnCommit(swept, outcome, new Date().toISOString());
      if (batch.length > 0) {
        await settle(account, entry, batch, true, before, { current, reached }, peopleBefore);
      }
    }
    if (!current()) return null;
    if (outcome?.kind === "committed") {
      // ADR-0043 — a removal this sweep committed owes the push-routing
      // rotation, as the chat view's sweep would.
      void rotateIfShrank(account, entry, before);
      if (outcome.addedCount > 0) groupNames.addedHere(entry.groupIdB64);
    }
    return outcome;
  }

  /** Poll the directory until `deviceKey`'s KeyPackage is there, bounded. */
  async function awaitDeviceKeyPackage(
    accountId: Uint8Array,
    deviceKey: Uint8Array,
    current: () => boolean,
  ): Promise<Uint8Array> {
    const deadline = Date.now() + ADMIT_DEVICE_WAIT_MS;
    const wasm = await loadWasm();
    for (;;) {
      const list = await discovery.fetchKeyPackages(accountId);
      // Up to three minutes of polling: the account that asked may have
      // signed out, and the chats a hit would be admitted to are then
      // nobody's, or another account's.
      if (!current()) throw new StaleBindingError();
      const hit = list?.find((d) => bytesEqual(d.deviceId, deviceKey));
      // ADR-0074 — a KeyPackage the device published before the pairing
      // state bundle reached it names another of the account's keys; wait
      // for the one it publishes after it re-minted its binding.
      if (
        hit !== undefined &&
        ownDeviceKeyPackageUsable(wasm, hit.keyPackage, ownPersonKeyB64(chats.identity()))
      ) {
        return hit.keyPackage;
      }
      if (Date.now() >= deadline) {
        throw new Error(
          hit === undefined
            ? "the new device has not published a KeyPackage yet; it will be admitted to your chats later"
            : "the new device's KeyPackage is bound to another of the account's keys; it is admitted once it publishes again",
        );
      }
      await sleepCancellable(ADMIT_DEVICE_POLL_MS, new AbortController().signal);
      if (!current()) throw new StaleBindingError();
    }
  }

  /** Wait, bounded, until the account's DeviceGroup roster names
   *  `deviceKey`; throws when it never does. */
  async function awaitOwnDeviceInRoster(accountIdB64: string, deviceKey: Uint8Array): Promise<void> {
    const deadline = Date.now() + ADMIT_DEVICE_ROSTER_WAIT_MS;
    const keyHex = hexOfKey(deviceKey);
    for (;;) {
      if (ownDeviceAdmissible(ownDeviceRoster(accountIdB64), keyHex)) return;
      if (Date.now() >= deadline) {
        throw new Error(
          "the device is not in the account's device list; it is not added to your chats",
        );
      }
      await sleepCancellable(ADMIT_DEVICE_ROSTER_POLL_MS, new AbortController().signal);
    }
  }

  async function admitDevice(deviceKey: Uint8Array): Promise<DeviceFanOut> {
    const secrets = auth.identitySecrets();
    if (secrets === null) throw new Error("admitDevice: no session");
    const current = binding.capture();
    const keyPackage = await awaitDeviceKeyPackage(secrets.accountId, deviceKey, current);
    // ADR-0074 section 2.3 — only a device the account's DeviceGroup holds
    // is fanned into the chats. The pairing that calls this added it there
    // first; the sync runtime reloads its handle after that change, so the
    // roster is given a moment to show it.
    await awaitOwnDeviceInRoster(base64Encode(secrets.accountId), deviceKey);
    let offered = 0;
    let skipped = 0;
    let failed = 0;
    for (const entry of chats.groups()) {
      // Each admission awaits the network; the list was read under the
      // session that asked, and its secrets are that session's.
      if (!current()) throw new StaleBindingError();
      if (readIsActive(entry.group) === false) {
        skipped += 1;
        continue;
      }
      try {
        const outcome = await contacts.admitDeviceToGroup(
          secrets,
          entry.group,
          keyPackage,
          secrets.accountId,
        );
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
    const current = binding.capture();
    let offered = 0;
    let skipped = 0;
    let failed = 0;
    for (const entry of chats.groups()) {
      // As in `admitDevice`. The directory row below is still deleted: it is
      // this account's row, under this account's token.
      if (!current()) break;
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
    // ADR-0074 section 2.3 — the directory row is deleted FIRST now, with
    // the account statement, by the removal operation that calls this
    // (`devices/removal.ts`), not here.
    return { offered, skipped, failed };
  }

  async function settle(
    account: string,
    entry: ActiveGroup,
    events: readonly IncomingChatEvent[],
    cursorMoved: boolean,
    membersBefore: string[] | null,
    /** The binding the rows were received under, and how far that poll got. */
    scope: { readonly current: () => boolean; readonly reached: number },
    /** The PERSON keys of the roster before the batch applied; `undefined`
     *  from a caller that did not capture them (nothing is suppressed). */
    peopleBefore?: string[] | null,
  ): Promise<void> {
    const groupIdB64 = entry.groupIdB64;
    const superseded = loadSupersededKeys();
    // ADR-0073 R.7 case 1 — a re-sent message whose original this device
    // already holds in an older segment is dropped before it is stored.
    const aliases = loadKeyAliases();
    const resendHeld = await heldResendPredicate(events, {
      receivedGroupId: entry.group.groupId,
      conversationHexOf: (id) => lineageView.conversationHexOf(id),
      segmentIndexOf: (id) => lineageView.segmentIndexOf(id),
      ownPersonKeyB64: ownPersonKeyB64(chats.identity()),
      canonical: (k) => aliases.get(k) ?? k,
      loadLog: async () => {
        const deviceKey = await getDeviceKey(account);
        return deviceKey === null ? null : loadMessageLog(account, deviceKey);
      },
    });
    // ADR-0087 section 9 — the batch that ended this conversation: every row
    // of it is being wiped, and nothing it carried (a pin, a name, a list a
    // member shared) may land in a store behind the wipe.
    if (membershipEnd.isEnding(groupIdB64)) return;
    // ADR-0073 R.8 — in a conversation that cannot be repaired in place for
    // a reset, or that already moved, nothing from the retired key acts:
    // its control and service envelopes are dropped with its rows.
    const inertFacts = superseded.size === 0 ? undefined : continuityFacts(groupIdB64);
    const moved = superseded.size > 0 && laterSegmentExists(lineageView.lineage(), groupIdHex(entry.group.groupId));
    const planned = planBackgroundEvents(events, (k) => superseded.has(k), resendHeld, {
      sender: (k) =>
        // Facts that cannot be read (a torn role-table read) judge nothing
        // repairable: the retired key stays inert.
        supersededSenderInert({
          senderKeyB64: k,
          supersededKeys: superseded,
          kind: inertFacts?.kind ?? "chat",
          ownerKeyB64: inertFacts?.ownerKeyB64,
          moved,
        }),
      senderKeyOfLeaf: (leaf) => personKeyAtLeafB64(entry, leaf),
    });
    // docs/ux/second-device.md section 4 — the people a batch of commits
    // added and removed go on its last commit row, in the log and in the
    // view. A batch that only swapped devices, or a reset person's keys
    // (ADR-0073 R.7 case 2 / UX 16.5, across one batch or two), records
    // nothing, and neither does a one-to-one or this device's own removal.
    let membershipChange: MembershipChange | null = null;
    if (planned.sawCommit) {
      const judged = membershipChangeOfBatch({
        personKeysBefore: peopleBefore ?? null,
        personKeysAfter: personKeysOrNull(entry),
        accountOfKey: (k) => superseded.get(k) ?? accountIdB64OfKey(k),
        isSuperseded: (k) => superseded.has(k),
        awaited: awaitedSwaps(account, groupIdB64),
        nowMs: Date.now(),
        ownPersonKeyB64: ownPersonKeyB64(chats.identity()) ?? null,
      });
      setAwaitedSwaps(account, groupIdB64, judged.awaited);
      if (readIsActive(entry.group) !== false) membershipChange = judged.change;
    }
    const kind = conversationKindOf(entry.group);
    const plan = {
      ...planned,
      timeline: withMembershipChange(planned.timeline, membershipChange, kind),
      toView: withMembershipChange(planned.toView, membershipChange, kind),
    };

    if (!scope.current()) {
      // The session these rows were received for has ended. Everything below
      // the log write addresses the session that is live NOW (ringing a call,
      // filing profiles, delivering to an open view, re-sending names), so
      // none of it runs. The rows themselves are filed for the account they
      // belong to, at the position their own poll reached.
      if (plan.timeline.length > 0 || plan.messageOps.length > 0 || cursorMoved) {
        await fileIntoLog(
          account,
          entry,
          plan.timeline.map((e) => eventToPersistedMessage(e)),
          plan.messageOps,
          scope,
        );
      }
      return;
    }

    // Call signaling first: an offer should start ringing before the log
    // write below spends its round trip.
    for (const row of plan.signaling) callHost.ingestSignaling(groupIdB64, row);

    for (const p of plan.profiles) {
      if (p.profile !== undefined && p.senderKey !== undefined) {
        profiles.ingest(p.senderKey, p.profile, { groupIdB64 });
      }
    }
    // ADR-0060 — scoped-discovery service envelopes, to their handler.
    for (const e of plan.scoped) {
      void intakeScopedEnvelope({
        kind: e.envelope,
        payload: e.payload,
        senderKeyB64: e.senderKey,
        oneToOne: isOneToOne(entry.group),
        scopeHex: journalHex(base64Decode(groupIdB64)),
      });
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
        // ADR-0060 section 4 - the same request is how a device that missed
        // this account's introductions asks for them again.
        noteIntroductionsAsk(groupIdB64, r.senderKey);
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
    // ADR-0087 — the group photo, allowed reactions and handle, the same way.
    for (const g of plan.groupSettings) {
      void groupNames.receiveSetting(entry, g).catch((err) => {
        // eslint-disable-next-line no-console
        console.warn("background receive: group setting receive failed", err);
      });
    }
    // ADR-0073 R.6 — a successor claim, through the claim rule under the
    // per-group lock. Never a row, whatever the verdict.
    for (const c of plan.groupClosed) {
      void groupClosed.receive(entry, c).catch((err) => {
        // eslint-disable-next-line no-console
        console.warn("background receive: successor claim receive failed", err);
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
    const membership = observeMembership(account, groupIdB64, new Set(personKeysOf(entry.group)));
    if (membership.grew) {
      profiles.groupGrew(groupIdB64);
      groupNames.groupGrew(groupIdB64);
      pins.groupGrew(groupIdB64);
      sharedChannels.groupGrew(groupIdB64);
    }
    // ADR-0060 section 3 — a person left: this account's reach keys for the
    // group rotate and the profile carrying the new ones is republished.
    if (membership.shrank) void profiles.groupShrank(groupIdB64);

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
        scope,
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
    scope: { readonly current: () => boolean; readonly reached: number },
  ): Promise<void> {
    try {
      // Before the await: a group freed by a sign-out no longer answers.
      const hex = hexOf(entry);
      const deviceKey = await getDeviceKey(account);
      if (deviceKey === null) return;
      // The coordinator's cursor while the session lasts (a view may have
      // noted a later position); the poll's own once it is over, when the
      // coordinator holds another session's cursors or none.
      const cursor = scope.current()
        ? (coordinator.cursor(entry.groupIdB64) ?? 0)
        : scope.reached;
      // ADR-0052 decision 3 — a message deleted while its chat was closed
      // takes its pin records with it (section 4.1). Which deletes the
      // author gate accepted is read off the fold's result, not off the ops.
      const newlyDeleted: number[] = [];
      // Reactions section 12 — others' reactions to this person's own
      // messages, for the unseen-reactions button.
      const unseen: number[] = [];
      const ownKey = ownPersonKeyB64(chats.identity());
      await updateMessageLog(account, deviceKey, (current) => {
        const groups = current?.groups ?? {};
        const appended = appendToGroupSlot(groups[hex], messages, cursor);
        unseen.length = 0;
        if (ownKey !== null) {
          for (const op of ops) {
            if (op.op !== "reaction" || op.reactionAction === "remove" || op.senderKey === ownKey) continue;
            const target = appended.messages.find((m) => Number(m.messageId) === op.target);
            if (target?.authorKeyB64 === ownKey) unseen.push(op.target);
          }
        }
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
      for (const id of unseen) markUnseenReaction(account, entry.groupIdB64, id);
      // The pin store is the live session's.
      if (!scope.current()) return;
      for (const id of newlyDeleted) {
        void pins.messageDeleted(entry.groupIdB64, id).catch((err) => {
          // eslint-disable-next-line no-console
          console.warn("background receive: dropping pins of a deleted message failed", err);
        });
      }
      // ADR-0065 section 3 — a message its author deleted for everyone
      // leaves Saved here too, as it does with the chat open.
      const unsaved = savedEntriesOf(sync.saved(), hex, newlyDeleted).map((e) => unsaveEdit(e.ref, e.messageId));
      if (unsaved.length > 0) {
        void sync.applyFolderEdits(unsaved).catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("background receive: removing a deleted message from Saved failed", err instanceof Error ? err.name : "");
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
      const identity = wasm.Identity.fromSeed(conversationSeedOf(secrets));
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

  // ADR-0045 amendment (C-88) - a Welcome refused because this device holds
  // its group as active waits for that group's journal to be read to its
  // head. A group whose view is open is long-polled by that view: asked to
  // poll now, and answered `false`, since nothing here can say it got there.
  onCleanup(
    membershipEnd.registerJournalReader(async (groupIdB64) => {
      if (chats.get(groupIdB64) === undefined) return true;
      if (coordinator.isForeground(groupIdB64)) {
        coordinator.requestPoll(groupIdB64);
        return false;
      }
      try {
        for (let i = 0; i < JOURNAL_REPAIR_MAX_POLLS; i++) {
          const r = await pollGroup(groupIdB64, 0, new AbortController().signal);
          // Another path took this batch and may be applying it still: that
          // is not the head, and calling it the head would drop the Welcome.
          if (r.cursorMoved === true) continue;
          if (!r.yieldedRows) return true;
        }
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("journal read for a held Welcome failed", err instanceof Error ? err.name : "");
      }
      return false;
    }),
  );
  // The group is gone from this device, and with it its position in the
  // journal: a person added back starts as a new member does.
  onCleanup(
    membershipEnd.onGroupWiped((groupIdB64) => {
      coordinator.forget(groupIdB64);
      logCursors = null;
      backoff.succeeded(groupIdB64);
    }),
  );

  let unregisterGroupCheck: (() => void) | null = null;

  function start(accountIdB64: string): void {
    if (activeAccount === accountIdB64 && loopAbort !== null) return;
    stop();
    activeAccount = accountIdB64;
    mirrorSupersededKeys();
    // ADR-0073 R.7 case 1 — messages held before a reload are read back.
    void restoreHeldSends(accountIdB64);
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
      // The pause ends early on anything that means something arrived: a push
      // wake, the tab becoming visible, a call starting (all wake the latch).
      sleep: async (ms, signal) => {
        const early = new AbortController();
        const onAbort = (): void => early.abort();
        signal.addEventListener("abort", onAbort, { once: true });
        try {
          await Promise.race([sleepCancellable(ms, early.signal), resume.wait(early.signal)]);
        } finally {
          signal.removeEventListener("abort", onAbort);
          early.abort();
        }
      },
      random: Math.random,
      intervalMs: BACKGROUND_SWEEP_INTERVAL_MS,
      jitterMs: BACKGROUND_SWEEP_JITTER_MS,
      intervalFor: () =>
        backgroundSweepIntervalMs({
          wakeWindowOpen: Date.now() < wakeUntilMs,
          callInProgress: callHost.anyCallInProgress(),
          pushWake: pushWakeReach.read(),
        }),
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
    resetHeldSends();
    // Only when a session was bound: `start` calls `stop` first, and work
    // begun just before the first `start` of a session is that session's.
    if (activeAccount !== null) binding.advance();
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

  // ADR-0057 second security pass, finding 1 - the repair check polls a
  // conversation group to its journal head before deciding. A group the chat
  // view has open is already long-polled to its head by the view, so only a
  // background group is polled here (the same exclusion as `syncGroupNow`).
  const unregisterHeadPoller = journalRepair.registerHeadPoller({
    owns: (groupId) => chats.get(base64Encode(groupId)) !== undefined,
    pollToHead: async (groupId) => {
      const groupIdB64 = base64Encode(groupId);
      if (coordinator.isForeground(groupIdB64)) return;
      for (let i = 0; i < JOURNAL_REPAIR_MAX_POLLS; i++) {
        const r = await pollGroup(groupIdB64, 0, new AbortController().signal);
        if (!r.yieldedRows) return;
      }
    },
  });

  // ADR-0050 section 2.5a item 7 - a Welcome kept owed because no grant
  // reached its addressee's mailbox is retried when a grant or a proposer's
  // re-sent committer envelope arrives, not only at the next poll of its
  // group: `sweepGroup` runs the sweep's owed-Welcome step (or asks an open
  // chat view to poll, which does).
  const unsubscribeGrants = onReceivedGrantsChanged(() => {
    void retryOwedWelcomes(
      chats.groups().map((g) => g.groupIdB64),
      (g) => loadUnacked(g),
      async (groupIdB64) => {
        const entry = chats.get(groupIdB64);
        if (entry !== undefined) await sweepGroup(entry);
      },
    ).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("background receive: owed Welcome retry failed", err);
    });
  });

  onCleanup(() => {
    unsubscribeResetAccepted();
    groupClosed.onPendingClaim(null);
    groupClosed.onAcceptedClaim(null);
    installScopeLineage(null);
    installDeadSessionGuard(null);
    unsubscribeGrants();
    unregisterHeadPoller();
    globalThis.document?.removeEventListener("visibilitychange", onVisibility);
    swContainer?.removeEventListener("message", onSwMessage);
    stop();
  });

  return (
    <BackgroundReceiveContext.Provider
      value={{ coordinator, syncGroupNow, admitDevice, retireDevice, continuityPass }}
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
/** The distinct PERSON keys of a group's roster, or `null` on a torn read. */
function personKeysOrNull(entry: ActiveGroup): string[] | null {
  try {
    entry.group.members();
  } catch {
    return null;
  }
  return personKeysOf(entry.group);
}

function hexOf(entry: ActiveGroup): string {
  // From the registry key, not the wasm handle: a row decrypted just before a
  // sign-out is filed after it, when the handle has been freed.
  let out = "";
  for (const b of base64Decode(entry.groupIdB64)) out += b.toString(16).padStart(2, "0");
  return out;
}

/** The roster member at `leaf`, resolved at receipt (C-68). */
/** Base64 of the PERSON key (ADR-0050) of the member at `leaf`, or
 *  `undefined` on a torn read or when no member holds that leaf. */
function personKeyAtLeafB64(entry: ActiveGroup, leaf: number): string | undefined {
  try {
    const roster = entry.group.members() as unknown as Array<{
      leafIndex: number;
      signaturePublicKey: Uint8Array;
      accountKey?: Uint8Array;
    }>;
    const hit = roster.find((m) => m.leafIndex === leaf);
    return hit === undefined ? undefined : base64Encode(personKeyOf(hit));
  } catch {
    return undefined;
  }
}

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
