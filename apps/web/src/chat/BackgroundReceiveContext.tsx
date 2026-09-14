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
 * the tab is visible — or while any call is ringing, starting or live, because
 * the call rides a group journal. That call's group gets a dedicated long poll
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
import {
  memberSignatureKeys,
  readPushRotationRole,
  receivePushRoutingSecretGated,
  rotatePushRoutingSecretIfMembershipShrank,
} from "../push/group-routing";
import { base64Encode } from "../auth/account-id";
import { loadWasm } from "../wasm";
import {
  appendToGroupSlot,
  eventToPersistedMessage,
} from "./background-log";
import { planBackgroundEvents } from "./background-events";
import {
  BACKGROUND_CONCURRENCY,
  BACKGROUND_SWEEP_INTERVAL_MS,
  BACKGROUND_SWEEP_JITTER_MS,
  GroupBackoff,
  MAX_IMMEDIATE_REPOLLS,
  runBackgroundReceiveLoop,
  type GroupPollResult,
} from "./background-receive-scheduler";
import { useChats, type ActiveGroup } from "./ChatContext";
import { ChatClient, type IncomingChatEvent } from "./client";
import { recordBeaconObservation } from "./committer-beacon";
import { readIsActive } from "./membership-ended";
import {
  loadMessageLog,
  MESSAGE_LOG_VERSION,
  updateMessageLog,
} from "./message-log-store";
import { proposalKey, recordObservation } from "./pending-requests";
import { ReceiveCoordinator } from "./receive-coordinator";

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
}

const BackgroundReceiveContext = createContext<BackgroundReceiveApi>();

export function BackgroundReceiveProvider(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
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

  function isActive(): boolean {
    return activeAccount !== null && (!isHidden() || callHost.anyCallInProgress());
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
    return { yieldedRows: result.outcome === "applied" && end > start };
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
      void rotateIfShrank(account, entry, membersBefore);
    }

    // The chat view gets the rest, now or when it next mounts.
    coordinator.deliver(groupIdB64, plan.toView);

    if (plan.timeline.length > 0 || cursorMoved) {
      await fileIntoLog(account, entry, plan.timeline.map((e) => eventToPersistedMessage(e)));
    }
  }

  async function fileIntoLog(
    account: string,
    entry: ActiveGroup,
    messages: ReturnType<typeof eventToPersistedMessage>[],
  ): Promise<void> {
    try {
      const deviceKey = await getDeviceKey(account);
      if (deviceKey === null) return;
      const hex = hexOf(entry);
      const cursor = coordinator.cursor(entry.groupIdB64) ?? 0;
      await updateMessageLog(account, deviceKey, (current) => {
        const groups = current?.groups ?? {};
        return {
          version: MESSAGE_LOG_VERSION,
          groups: { ...groups, [hex]: appendToGroupSlot(groups[hex], messages, cursor) },
        };
      });
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

  function start(accountIdB64: string): void {
    if (activeAccount === accountIdB64 && loopAbort !== null) return;
    stop();
    activeAccount = accountIdB64;
    const controller = new AbortController();
    loopAbort = controller;
    void runBackgroundReceiveLoop(controller.signal, {
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
    stopCallLane();
    activeAccount = null;
    logCursors = null;
    backoff.clear();
    coordinator.reset();
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
  onCleanup(() => {
    globalThis.document?.removeEventListener("visibilitychange", onVisibility);
    stop();
  });

  return (
    <BackgroundReceiveContext.Provider value={{ coordinator, syncGroupNow }}>
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
