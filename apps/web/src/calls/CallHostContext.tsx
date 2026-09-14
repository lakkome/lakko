// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The app-level home of the 1x1 call.
 *
 * ## The defect
 *
 * The 1x1 `CallSession` lived inside `routes/Chat.tsx` and heard signaling only
 * from that chat's own receive loop. So an incoming call rang only while the
 * callee happened to have the caller's chat open — on Home, Contacts or any
 * other chat nothing rang at all — and leaving the chat hung the call up (a
 * confirmation prompt was added to soften that, which only asked the user to
 * choose between staying on one screen and losing the call).
 *
 * ## What lives here now
 *
 *   - **One call slot** for the whole signed-in session, with its session,
 *     media streams and state. Navigating between routes does not touch it.
 *   - **One exactly-once signaling queue**, keyed on `(group, DS message id)`,
 *     fed by BOTH receive paths: the open chat view's long poll and the
 *     background receive for every other joined group
 *     (`chat/BackgroundReceiveContext.tsx`). Where each row goes is decided by
 *     `call-host-routing.ts`: C-11 (group-control bodies never reach the 1x1
 *     session), C-12 widened to the group (a body names the active call only if
 *     it arrives on that call's journal), the stale-offer rule, and one call at
 *     a time (a second offer is answered `End{busy}`).
 *   - **Group-call announcements** (`GroupCallStart` / `Join` / `Leave`) for
 *     every chat, folded into `group-call-store.ts`, so a chat opened after its
 *     start row was consumed in the background still knows the call is live.
 *   - **The call UI** ({@link CallHostView}, mounted in the authenticated
 *     layout): the ringing banner with Accept / Accept with video / Decline on
 *     any screen, the full in-call overlay, and the remote audio sink.
 *
 * ## The group (SFU) call lives here too
 *
 * It used to be created and owned by `routes/Chat.tsx`, so leaving the chat
 * left the call, and its mid-call SFrame rekey (G-18 / ADR-0029) — an effect
 * over a counter only the mounted chat view bumps — stopped when the view did.
 * Now:
 *
 *   - **One group-call slot**, beside the 1x1 slot and mutually exclusive with
 *     it: the SFU session, its UI adapter (`sfu-call-ui.ts`), the group-control
 *     publish chain, and the leave path. A chat view asks for a call through
 *     {@link CallHostApi.startGroupCall} and reads the slot to show the live call
 *     instead of offering a second one.
 *   - **The rekey** runs when the call's group's MLS lock queue drains
 *     (`onGroupMlsIdle`). Every region that moves an epoch — the open chat's
 *     long poll, the background receive, a commit, the sweep, the F1 reset —
 *     runs under that lock, whatever screen is showing. While the chat is
 *     closed the background receive's call lane long-polls the group, so a
 *     commit is applied (and the key rotated) within one poll.
 *   - **The overlay, the remote audio sinks and the hang-up** are rendered by
 *     {@link CallHostView} on every authenticated screen.
 *   - **A call started in a chat that is not open** is announced by a banner in
 *     {@link CallHostView}, from the group-call records both receive paths feed.
 *
 * The decisions are in `group-call-host.ts` (pure, tested).
 */
import {
  createContext,
  createEffect,
  createMemo,
  createSignal,
  For,
  onCleanup,
  Show,
  untrack,
  useContext,
  type Accessor,
  type JSX,
} from "solid-js";
import { A } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { ChatClient } from "../chat/client";
import { makeCallSignalingPublisher } from "../chat/call-signaling-publish";
import { readIsActive } from "../chat/membership-ended";
import { memberKeysOf, peerName } from "../chat/peer-name";
import { base64Decode, base64Encode, base64UrlEncodeNoPad } from "../auth/account-id";
import { loadAccepted } from "../contacts/local-store";
import { onGroupMlsIdle } from "../mls/group-lock";
import { useProfiles } from "../profile/ProfileContext";
import { paths } from "../routes/paths";
import {
  callActions,
  callState as callUi,
  qualityColour,
  type Participant,
  type QualityTier,
} from "../state/call-state";
import { loadWasm } from "../wasm";
import Call from "../routes/Call";
import type { ParsedCallSignaling } from "../../wasm/lakko_ffi_wasm";
import {
  CallSession,
  mintCallId,
  type VideoPublishState,
  type WebCallEndReason,
  type WebCallState,
  type WebConnectionStats,
} from "./call-session";
import {
  callIdHex,
  routeSignalingKind,
  SerialOnceQueue,
  type InboundSignalingRow,
} from "./call-signaling-dispatch";
import { decideInboundSignaling } from "./call-host-routing";
import { CallJwtClient } from "./call-jwt-client";
import {
  decideGroupCallEpochChange,
  planGroupCallLeave,
  selectGroupCallBanner,
  type GroupLeaveReason,
} from "./group-call-host";
import { hexToBytes, isGroupCallLive, type GroupCallEvent } from "./group-call-record";
import {
  applyGroupCallEventFor,
  clearGroupCallStore,
  groupCallsFor,
  liveGroupCallsFor,
} from "./group-call-store";
import type { SfuCallState } from "./sfu-call-session";
import { createSfuCallUi, type SfuCallUiHandle } from "./sfu-call-ui";
import { sfuBaseUrl } from "./sfu-config";
import { useBackgroundReceive } from "../chat/BackgroundReceiveContext";

/** A 1x1 call that ended, for the chat view's "Last call ended" line. */
export interface EndedCallNotice {
  readonly groupIdB64: string;
  readonly reason: WebCallEndReason;
}

export interface CallHostApi {
  /** State of the 1x1 call in the slot (`idle` when none has run). */
  readonly state: Accessor<WebCallState>;
  /** The group of the call in the slot, or `null`. Kept after `done` until the
   *  next call, so `lastEnded` can name it. */
  readonly callGroupIdB64: Accessor<string | null>;
  /** The most recent 1x1 call that ended on this device, or `null`. */
  readonly lastEnded: Accessor<EndedCallNotice | null>;
  /** A 1x1 call is ringing, dialing, connecting, live or ending. */
  inCall(): boolean;
  /** The group of the ringing / dialing / live 1x1 call, or `null`. */
  activeGroupIdB64(): string | null;
  /** Hand over one decrypted call-signaling row from any receive path. Rows
   *  are delivered exactly once per `(group, messageId)`, in order. */
  ingestSignaling(groupIdB64: string, row: InboundSignalingRow): void;
  /** Resolves once every row handed over so far has been applied. */
  whenSignalingIdle(): Promise<void>;
  /** Place a 1x1 call to the chat `groupIdB64`. Refused while any call runs. */
  dial(groupIdB64: string, withVideo: boolean): Promise<void>;
  /** The group (SFU) call that is starting or live on this device, or `null`. */
  readonly groupCall: Accessor<GroupCallSlot | null>;
  /** Whether a group call is starting or live anywhere. */
  groupCallBusy(): boolean;
  /** The group whose journal carries the call in progress — the ringing or
   *  live 1x1 call's, or the starting or live group call's — or `null`. The
   *  receive paths keep it polled even while its chat is closed. */
  callLaneGroupIdB64(): string | null;
  /** Any call — 1x1 or group — is ringing, starting or live. */
  anyCallInProgress(): boolean;
  /** Start (`joinCallIdHex === null`) or join a group call in `groupIdB64`.
   *  Never rejects; a failure is reported in the outcome. */
  startGroupCall(opts: StartGroupCallOptions): Promise<GroupCallStartOutcome>;
  /** Leave the group call this device is in (publishes Leave / End as owed). */
  leaveGroupCall(): Promise<void>;
}

/** The group call in the host's group slot. */
export interface GroupCallSlot {
  readonly groupIdB64: string;
  readonly callIdHex: string;
  /** `true` until the SFU session exists. */
  readonly starting: boolean;
}

export interface StartGroupCallOptions {
  readonly groupIdB64: string;
  readonly withVideo: boolean;
  /** The call to join, or `null` to start a new one. */
  readonly joinCallIdHex: string | null;
  /**
   * Bring the group up to date before joining. The seed has to be derived at
   * the epoch the call is at (every participant exports from the same MLS epoch
   * — audit-pack C-1), and a `CallEnd` already on the journal must be seen
   * before joining a call that is over. Should not throw; a failure is logged
   * and the join proceeds on the state in hand (the rekey catches up).
   */
  readonly syncGroup?: () => Promise<void>;
}

export type GroupCallStartOutcome =
  | { readonly kind: "connected" }
  /** Another call is starting or live; nothing was done. */
  | { readonly kind: "busy" }
  /** The call to join is no longer live. */
  | { readonly kind: "ended" }
  | { readonly kind: "failed"; readonly message: string };

const CallHostContext = createContext<CallHostApi>();

const LIVE_STATES: readonly WebCallState[] = ["ringing", "outgoing", "connecting", "connected", "ending"];

/** Display bands for the connection RTT — the same as the native clients. */
export function qualityTierForRtt(rttMs: number | null): QualityTier {
  if (rttMs === null) return "good";
  if (rttMs < 100) return "excellent";
  if (rttMs < 200) return "good";
  if (rttMs < 350) return "fair";
  if (rttMs < 600) return "poor";
  return "bad";
}

export function CallHostProvider(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();

  // Seals and publishes signaling through the locked, fail-closed-persisting
  // seam (`call-signaling-publish.ts`), like every other application message.
  const chat = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: async () => {
      const s = auth.state();
      if (s.kind !== "session") return;
      await chats.persistSnapshot(s.account.accountId);
    },
  });

  const [state, setState] = createSignal<WebCallState>("idle");
  const [callGroup, setCallGroup] = createSignal<string | null>(null);
  const [lastEnded, setLastEnded] = createSignal<EndedCallNotice | null>(null);
  const [remoteStream, setRemoteStream] = createSignal<MediaStream | null>(null);
  const [localStream, setLocalStream] = createSignal<MediaStream | null>(null);
  const [videoCall, setVideoCall] = createSignal(false);
  const [remoteHasVideo, setRemoteHasVideo] = createSignal(false);
  const [remoteWantsVideo, setRemoteWantsVideo] = createSignal(false);
  const [videoPublishState, setVideoPublishState] = createSignal<VideoPublishState>("off");
  const [connStats, setConnStats] = createSignal<WebConnectionStats | null>(null);
  /** The group-call slot, set from the first step of a start / join. */
  const [groupSlot, setGroupSlot] = createSignal<GroupCallSlot | null>(null);
  /** The live group call's session adapter, once it exists. */
  const [groupHandle, setGroupHandle] = createSignal<SfuCallUiHandle | null>(null);
  /** Calls the user dismissed or left this session: not announced again. */
  const [dismissedGroupCalls, setDismissedGroupCalls] = createSignal<ReadonlySet<string>>(
    new Set(),
  );

  let activeCall: CallSession | null = null;
  /** The session that last occupied the slot. Unlike `activeCall` it is NOT
   *  cleared on `done`: `CallSession` reports `done` twice when it ends — once
   *  without a reason from `dispose()`, then again with the reason — and the
   *  second report is the only one that says why the call ended. */
  let slotSession: CallSession | null = null;
  /** Call ids of 1x1 calls that ended here this session: never ring again. */
  const endedCallIds = new Set<string>();

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  const inCall = (): boolean => LIVE_STATES.includes(state());

  function groupEntry(groupIdB64: string) {
    return chats.get(groupIdB64);
  }

  function nameFor(groupIdB64: string | null): string {
    if (groupIdB64 === null) return "Peer";
    const entry = groupEntry(groupIdB64);
    if (entry === undefined) return "Peer";
    const own = chats.identity();
    return peerName({
      ownKeyB64: own === null ? null : base64Encode(own.signaturePublicKey),
      memberKeysB64: memberKeysOf(entry.group),
      resolveName: (k) => profiles.resolve(k).name,
      linkedContactName: loadAccepted().find((c) => c.groupIdB64 === groupIdB64)?.displayName,
      label: entry.label,
    });
  }

  function publisherFor(groupIdB64: string): (bytes: Uint8Array) => Promise<void> {
    return makeCallSignalingPublisher({
      chat,
      identity: () => auth.identitySecrets(),
      group: () => {
        const entry = groupEntry(groupIdB64);
        if (entry === undefined) throw new Error("the call's chat is no longer joined");
        return entry.group;
      },
    });
  }

  // ----- the shared in-call UI store --------------------------------------

  const localParticipant = (): Participant => ({
    id: "local",
    displayName: "You",
    isLocal: true,
    micOn: true,
    camOn: videoCall(),
    handRaised: false,
    quality: qualityTierForRtt(connStats()?.rttMs ?? null),
    speaking: false,
    screenSharing: false,
  });

  const remoteParticipant = (): Participant => ({
    id: "peer",
    displayName: nameFor(callGroup()),
    isLocal: false,
    micOn: true,
    camOn: remoteHasVideo(),
    handRaised: false,
    quality: "good",
    speaking: false,
    screenSharing: false,
  });

  function syncCallUiFromState(s: WebCallState): void {
    switch (s) {
      case "outgoing":
      case "connecting":
        callActions.setPhase({ kind: "connecting" });
        callActions.setParticipants([localParticipant()]);
        // 1x1 web media is P2P DTLS-SRTP, not SFrame: no safety number, so no
        // pill (see `safety-pill.ts`).
        callActions.setE2ee({ active: false, shortFingerprint: null });
        break;
      case "connected":
        callActions.setPhase({ kind: "connected" });
        callActions.setParticipants([localParticipant(), remoteParticipant()]);
        break;
      case "ending":
        break;
      case "idle":
      case "ringing":
      case "done":
        callActions.reset();
        break;
    }
  }

  function buildCallSession(groupIdB64: string, callId: Uint8Array): CallSession {
    const session: CallSession = new CallSession(callId, {
      onRemoteStream: (s) => {
        setRemoteStream(s);
        setRemoteHasVideo(s.getVideoTracks().length > 0);
      },
      onLocalStream: (s) => setLocalStream(s),
      onVideoPublishState: (s) => setVideoPublishState(s),
      onOutboundSignaling: publisherFor(groupIdB64),
      onStateChange: (s, reason) => {
        // Only the session that holds (or last held) the slot drives the UI; a
        // late report from an older one must not touch the one that replaced it.
        if (slotSession !== session) return;
        setState(s);
        syncCallUiFromState(s);
        if (s === "done") {
          endedCallIds.add(callIdHex(callId));
          activeCall = null;
          if (reason !== undefined) setLastEnded({ groupIdB64, reason });
          setRemoteStream(null);
          setLocalStream(null);
          setVideoCall(false);
          setRemoteHasVideo(false);
          setRemoteWantsVideo(false);
          setVideoPublishState("off");
          setConnStats(null);
        }
      },
      onConnectionStats: (stats) => {
        setConnStats(stats);
        callActions.setConnectionDetail({
          rttMs: stats.rttMs,
          downBps: stats.downBps,
          upBps: stats.upBps,
          connectionType: stats.relayed ? "relay" : stats.rttMs !== null ? "direct" : "unknown",
        });
        callActions.setQuality("local", qualityTierForRtt(stats.rttMs));
      },
    });
    return session;
  }

  function occupy(groupIdB64: string, session: CallSession): void {
    activeCall = session;
    slotSession = session;
    setCallGroup(groupIdB64);
    setLastEnded(null);
  }

  // ----- inbound signaling --------------------------------------------------

  interface QueuedRow {
    readonly groupIdB64: string;
    readonly row: InboundSignalingRow;
  }

  function newQueue(): SerialOnceQueue<QueuedRow> {
    return new SerialOnceQueue<QueuedRow>(
      handleRow,
      (err) => {
        // eslint-disable-next-line no-console
        console.warn("call signaling dispatch failed", err);
      },
      // A DS message id is unique only within one group's journal.
      (item) => `${item.groupIdB64}:${item.row.messageId}`,
    );
  }
  let queue = newQueue();

  async function handleRow(item: QueuedRow): Promise<void> {
    const account = accountId();
    if (account === null) return;
    const wasm = await loadWasm();
    let parsed: ParsedCallSignaling;
    try {
      parsed = wasm.parseCallSignaling(item.row.payloadBytes);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("inbound call signaling did not parse; dropped", err);
      return;
    }
    try {
      const hex = callIdHex(parsed.callId);
      const decision = decideInboundSignaling({
        route: routeSignalingKind(parsed.kind),
        kind: parsed.kind,
        callIdHex: hex,
        groupIdB64: item.groupIdB64,
        rowCreatedAt: item.row.createdAt,
        nowMs: Date.now(),
        active:
          activeCall === null
            ? null
            : { callIdHex: callIdHex(activeCall.callId), groupIdB64: callGroup() ?? "" },
        endedCallIds,
        groupCallBusy: groupSlot() !== null,
        groupRecordKnowsCall: groupCallsFor(account, item.groupIdB64).has(hex),
      });

      if (decision.groupRecord === "apply-control") {
        applyGroupControlBody(account, item.groupIdB64, parsed, hex, item.row);
      } else if (decision.groupRecord === "end") {
        applyGroupCallEventFor(account, item.groupIdB64, { kind: "end", callIdHex: hex }, Date.now());
      }

      switch (decision.oneToOne) {
        case "drop":
          return;
        case "busy":
          endedCallIds.add(hex);
          await publisherFor(item.groupIdB64)(wasm.composeCallEndSignaling(parsed.callId, "busy"));
          return;
        case "ring": {
          const session = buildCallSession(item.groupIdB64, parsed.callId);
          occupy(item.groupIdB64, session);
          setRemoteWantsVideo(parsed.mediaVideo);
          await session.applyIncomingSignaling(parsed);
          return;
        }
        case "apply":
          if (activeCall !== null) await activeCall.applyIncomingSignaling(parsed);
          return;
      }
    } finally {
      parsed.free();
    }
  }

  function applyGroupControlBody(
    account: string,
    groupIdB64: string,
    parsed: ParsedCallSignaling,
    hex: string,
    row: InboundSignalingRow,
  ): void {
    const created = Date.parse(row.createdAt);
    const observedAtMs = Number.isNaN(created) ? Date.now() : created;
    // A body may only speak for its own sender: the leaf it names is an
    // unauthenticated field, the MLS sender leaf is not.
    const claimed = parsed.kind === "groupStart" ? parsed.initiatorLeaf : parsed.leafIndex;
    if (row.senderLeaf === undefined || row.senderLeaf !== claimed) return;
    const now = Date.now();
    switch (parsed.kind) {
      case "groupStart":
        applyGroupCallEventFor(
          account,
          groupIdB64,
          {
            kind: "start",
            callIdHex: hex,
            initiatorLeaf: parsed.initiatorLeaf,
            createdAtMs: Number(parsed.createdAtMs),
            mediaAudio: parsed.mediaAudio,
            mediaVideo: parsed.mediaVideo,
            observedAtMs,
          },
          now,
        );
        return;
      case "groupJoin":
      case "groupLeave":
        applyGroupCallEventFor(
          account,
          groupIdB64,
          {
            kind: parsed.kind === "groupJoin" ? "join" : "leave",
            callIdHex: hex,
            leafIndex: parsed.leafIndex,
            observedAtMs,
          },
          now,
        );
        return;
      default:
        return;
    }
  }

  // ----- controls ---------------------------------------------------------

  async function dial(groupIdB64: string, withVideo: boolean): Promise<void> {
    if (activeCall !== null || groupSlot() !== null) return;
    if (groupEntry(groupIdB64) === undefined) return;
    setVideoCall(withVideo);
    const session = buildCallSession(groupIdB64, mintCallId());
    occupy(groupIdB64, session);
    try {
      await session.dial({
        audio: true,
        video: withVideo,
        audioDeviceId: callUi.settings().selectedAudioInput,
        videoDeviceId: callUi.settings().selectedVideoInput,
      });
    } catch (err) {
      // `dial` drives the session to `done` on a capture / setup failure; make
      // sure the slot is free even if it threw before reporting a state.
      if (activeCall === session) {
        activeCall = null;
        setState("done");
        syncCallUiFromState("done");
      }
      setVideoCall(false);
      throw err;
    }
  }

  async function accept(withVideo: boolean): Promise<void> {
    const call = activeCall;
    if (call === null) return;
    if (withVideo) setVideoCall(true);
    await call.accept();
    // Opting into video on accept drives the same mid-call camera-on path the
    // caller uses (see the camera effect below).
    if (withVideo) callActions.setCamera(true);
  }

  async function decline(): Promise<void> {
    await activeCall?.decline();
  }

  async function hangUp(): Promise<void> {
    await activeCall?.hangUp("hungUp");
  }

  // The embedded in-call UI's End button moves the store to `ended`.
  createEffect(() => {
    if (callUi.phase().kind !== "ended") return;
    if (activeCall !== null) void activeCall.hangUp("hungUp");
  });

  // Mic toggle -> the live session (local `track.enabled` gate).
  createEffect(() => {
    const local = callUi.participants().find((p) => p.isLocal);
    if (local === undefined || activeCall === null) return;
    activeCall.setMicrophoneEnabled(local.micOn);
  });

  // In-call device picker -> `replaceTrack` on the live sender.
  let lastAudioInput: string | null = null;
  let lastVideoInput: string | null = null;
  createEffect(() => {
    const audioId = callUi.settings().selectedAudioInput;
    const videoId = callUi.settings().selectedVideoInput;
    state();
    if (activeCall === null) {
      lastAudioInput = null;
      lastVideoInput = null;
      return;
    }
    if (audioId !== null && audioId !== lastAudioInput) {
      lastAudioInput = audioId;
      void activeCall.switchAudioInput(audioId);
    }
    if (videoId !== null && videoId !== lastVideoInput) {
      lastVideoInput = videoId;
      void activeCall.switchVideoInput(videoId);
    }
  });

  // Camera toggle -> mid-call camera-on / off. A capture failure reverts the
  // toggle; the call stays up audio-only.
  createEffect(() => {
    const local = callUi.participants().find((p) => p.isLocal);
    if (local === undefined || activeCall === null) return;
    if (local.camOn) {
      void activeCall.addLocalVideo().catch((err: unknown) => {
        callActions.setCamera(false);
        // eslint-disable-next-line no-console
        console.warn("could not start the camera", err);
      });
    } else {
      activeCall.setCameraEnabled(false);
    }
  });

  // The 1x1 camera-on lifecycle into the shared store (only while this host
  // owns the call — the SFU path mirrors its own).
  createEffect(() => {
    const s = videoPublishState();
    if (!inCall()) return;
    callActions.setVideoPublishState(s);
  });

  // The remote tile's camera flag once the peer's video track arrives.
  createEffect(() => {
    if (state() !== "connected") return;
    callActions.setParticipantCamera("peer", remoteHasVideo());
  });

  // ----- the group (SFU) call -----------------------------------------------

  /** The group call this device is in. `callId` is retained so a mid-call
   *  epoch advance can re-derive the seed at the NEW epoch (G-18 / ADR-0029);
   *  the flags record what this device announced, which decides what leaving
   *  has to publish. */
  interface ActiveGroupCall {
    readonly groupIdB64: string;
    readonly callId: Uint8Array;
    readonly callIdHex: string;
    readonly ownLeaf: number;
    readonly initiator: boolean;
    joinPublished: boolean;
    /** The MLS epoch the session's current seed was derived at. */
    keyedEpoch: bigint | null;
  }
  let activeGroupCall: ActiveGroupCall | null = null;
  /** Synchronous latch so a double tap cannot open two SFU sessions. */
  let groupCallStarting = false;

  function readEpoch(group: { readonly epoch: bigint }): bigint | undefined {
    try {
      return group.epoch;
    } catch {
      return undefined;
    }
  }

  function applyGroupCallEvent(groupIdB64: string, event: GroupCallEvent): void {
    const account = accountId();
    if (account === null) return;
    applyGroupCallEventFor(account, groupIdB64, event, Date.now());
  }

  function dismissGroupCall(callIdHex: string): void {
    const next = new Set(dismissedGroupCalls());
    next.add(callIdHex);
    setDismissedGroupCalls(next);
  }

  /**
   * Publish a group-control body (`GroupCallStart` / `Join` / `Leave`, or the
   * `CallEnd` that terminates a group call) through the locked signaling seam,
   * strictly in the order requested.
   *
   * Chained rather than fired independently because order is part of the
   * meaning: a Leave that overtook its own Join would leave the other members
   * counting a participant who has already gone. Resolves `false` when the body
   * did not reach the journal; never rejects.
   */
  let groupControlTail: Promise<unknown> = Promise.resolve();
  function publishGroupControl(
    groupIdB64: string,
    compose: (wasm: Awaited<ReturnType<typeof loadWasm>>) => Uint8Array,
  ): Promise<boolean> {
    const run = groupControlTail.then(async (): Promise<boolean> => {
      let ok = true;
      try {
        const wasm = await loadWasm();
        const bytes = compose(wasm);
        await makeCallSignalingPublisher({
          chat,
          identity: () => auth.identitySecrets(),
          group: () => {
            const entry = groupEntry(groupIdB64);
            if (entry === undefined) throw new Error("the call's chat is no longer joined");
            return entry.group;
          },
          onError: (err) => {
            ok = false;
            // eslint-disable-next-line no-console
            console.warn("group call signaling publish failed", err);
          },
        })(bytes);
      } catch (err) {
        ok = false;
        // eslint-disable-next-line no-console
        console.warn("group call signaling compose failed", err);
      }
      return ok;
    });
    groupControlTail = run;
    return run;
  }

  /**
   * Start (initiator) or join a group call over the SFU.
   *
   * Derives the per-call SFrame seed + leaf from the MLS group, mints the
   * call-admission JWT bound to the live session token, and drives the shared
   * call-UI store through `createSfuCallUi`. Fail-closed throughout: any missing
   * prerequisite (SFU URL / session / seed) aborts before media, and the session
   * refuses to publish at all without `RTCRtpScriptTransform` (invariant #4).
   *
   * The initiator publishes `GroupCallStart` after the JWT mint; every
   * participant publishes `GroupCallJoin` once its SFU session is connected
   * (see `onGroupSessionState`); leaving is `leaveGroupCall`.
   */
  async function startGroupCall(opts: StartGroupCallOptions): Promise<GroupCallStartOutcome> {
    if (groupCallStarting || groupSlot() !== null || activeCall !== null) return { kind: "busy" };
    const initiator = opts.joinCallIdHex === null;
    const verb = initiator ? "start" : "join";
    let callId: Uint8Array;
    if (opts.joinCallIdHex === null) {
      callId = mintCallId();
    } else {
      const parsed = hexToBytes(opts.joinCallIdHex);
      if (parsed === null || parsed.length !== 16) {
        return { kind: "failed", message: `Could not ${verb} group call: malformed call id.` };
      }
      callId = parsed;
    }
    const groupIdB64 = opts.groupIdB64;
    const hex = callIdHex(callId);
    groupCallStarting = true;
    // One call at a time, app-wide: from here on a 1x1 offer from any chat is
    // answered busy, and the receive paths keep this group polled.
    setGroupSlot({ groupIdB64, callIdHex: hex, starting: true });
    try {
      const base = sfuBaseUrl();
      if (base === null) {
        return {
          kind: "failed",
          message: "Group calls need an SFU endpoint — set VITE_LAKKO_SFU_URL and rebuild.",
        };
      }
      if (chats.identity() === null || auth.identitySecrets() === null) {
        return {
          kind: "failed",
          message: `Re-authenticate to ${verb} a group call (no live session token).`,
        };
      }
      if (!initiator) {
        if (opts.syncGroup !== undefined) {
          try {
            await opts.syncGroup();
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("group call join: syncing the group failed; joining on the state in hand", err);
          }
        }
        // The sync enqueues any signaling it decrypted; wait for it to apply.
        await queue.whenIdle();
        const account = accountId();
        const still = account === null ? undefined : groupCallsFor(account, groupIdB64).get(hex);
        if (still === undefined || !isGroupCallLive(still, Date.now())) {
          return { kind: "ended" };
        }
      }
      const id = chats.identity();
      const secrets = auth.identitySecrets();
      const entry = groupEntry(groupIdB64);
      if (id === null || secrets === null) {
        return {
          kind: "failed",
          message: `Re-authenticate to ${verb} a group call (no live session token).`,
        };
      }
      if (entry === undefined || readIsActive(entry.group) === false) {
        return { kind: "failed", message: `Could not ${verb} group call: not a member of this chat.` };
      }
      const wasm = await loadWasm();
      // Per-call SFrame seed + this client's leaf. Deterministic per MLS epoch
      // (audit-pack C-1) so every participant derives identical bytes.
      // `ownLeafIndex` is the leaf this client stamps into the frame KID.
      const keyedEpoch = readEpoch(entry.group) ?? null;
      const seed = entry.group.exportSecret(id, "lakko/call/v1", callId, 32);
      const localLeaf = entry.group.ownLeafIndex;
      // Canonical call safety number — display-only; a derivation failure must
      // never abort the call.
      let safetyNumber: string | undefined;
      try {
        safetyNumber = wasm.callSafetyNumber(seed);
      } catch {
        safetyNumber = undefined;
      }
      // Call-admission JWT (JWS Compact, EdDSA) bound to the session.
      const jwt = new CallJwtClient(wasm);
      jwt.setSessionToken(secrets.sessionToken);
      const minted = await jwt.mint(hex, "participant");
      // ADR-0021 — the per-call peer_tag -> contact name map, derived locally
      // (the SFU never sends a name). On a tag collision between two contacts
      // neither name is shown.
      const tagToName = new Map<string, string>();
      const collidedTags = new Set<string>();
      for (const c of loadAccepted()) {
        if (c.accountIdHex !== undefined) {
          const tag = wasm.sfuPeerTag(hex, c.accountIdHex);
          if (tagToName.has(tag)) collidedTags.add(tag);
          else tagToName.set(tag, profiles.resolve(c.signaturePubkeyB64).name);
        }
      }
      const resolveDisplayName = (
        _peerId: string,
        pseudonym: string,
        peerTag: string | undefined,
      ): string => {
        if (peerTag !== undefined && !collidedTags.has(peerTag)) {
          const name = tagToName.get(peerTag);
          if (name !== undefined) return name;
        }
        return pseudonym.length > 0 ? pseudonym.slice(0, 8) : "Peer";
      };
      if (initiator) {
        // Announce before connecting, so a member who opens the chat while the
        // SFU handshake is still running already sees a call to join.
        const now = Date.now();
        const announced = await publishGroupControl(groupIdB64, (w) =>
          w.composeGroupCallStartSignaling(callId, localLeaf, BigInt(now), true, opts.withVideo),
        );
        if (!announced) throw new Error("the call could not be announced to the group");
        // Our own publish never comes back to us (the poll drops the echo), so
        // the record is updated here as it would be on every other member.
        applyGroupCallEvent(groupIdB64, {
          kind: "start",
          callIdHex: hex,
          initiatorLeaf: localLeaf,
          createdAtMs: now,
          mediaAudio: true,
          mediaVideo: opts.withVideo,
          observedAtMs: now,
        });
      }
      // Set before any further await, so a failure from here on reaches
      // `leaveGroupCall` — which is what publishes the End an initiator owes.
      activeGroupCall = {
        groupIdB64,
        callId,
        callIdHex: hex,
        ownLeaf: localLeaf,
        initiator,
        joinPublished: false,
        keyedEpoch,
      };
      const handle = createSfuCallUi(
        {
          sfuBaseUrl: base,
          jwt: minted.jwt,
          seed,
          localLeaf,
          media: {
            audio: true,
            // A group video call publishes the camera at connect (SFrame-sealed
            // like the audio sender); the in-call toggle gates the live track.
            video: opts.withVideo,
            audioDeviceId: callUi.settings().selectedAudioInput,
            videoDeviceId: callUi.settings().selectedVideoInput,
          },
        },
        {
          localDisplayName: "You",
          safetyNumber,
          resolveDisplayName,
          onSessionStateChange: (state) => onGroupSessionState(hex, state),
        },
      );
      setGroupHandle(handle);
      setGroupSlot({ groupIdB64, callIdHex: hex, starting: false });
      // The awaits above (JWT mint, the Start publish — which itself runs under
      // the group lock) can straddle an epoch advance whose drain found no
      // session to rekey. Catch up now, before any media is sealed.
      onGroupMlsSettled(groupIdB64);
      await handle.connect();
      return { kind: "connected" };
    } catch (err) {
      await leaveGroupCall("error");
      return {
        kind: "failed",
        message: `Could not ${verb} group call: ${err instanceof Error ? err.message : String(err)}`,
      };
    } finally {
      groupCallStarting = false;
      if (groupHandle() === null && activeGroupCall === null) setGroupSlot(null);
    }
  }

  /** SFU session lifecycle -> the group journal. */
  function onGroupSessionState(hex: string, state: SfuCallState): void {
    const g = activeGroupCall;
    if (g === null || g.callIdHex !== hex) return;
    if (state === "connected" && !g.joinPublished) {
      g.joinPublished = true;
      applyGroupCallEvent(g.groupIdB64, {
        kind: "join",
        callIdHex: hex,
        leafIndex: g.ownLeaf,
        observedAtMs: Date.now(),
      });
      void publishGroupControl(g.groupIdB64, (w) =>
        w.composeGroupCallJoinSignaling(g.callId, g.ownLeaf, BigInt(Date.now())),
      );
    } else if (state === "failed") {
      // Normally already handled: the adapter moves the store to `ended` first,
      // and the ended-phase effect leaves. This covers the case where it did not.
      void leaveGroupCall("error");
    }
  }

  /** Take the group call out of the slot and tear the SFU session down.
   *  Synchronous, so the several paths that can end a call act once. */
  function releaseGroupSlot(): { readonly call: ActiveGroupCall | null; readonly failed: boolean } {
    const g = activeGroupCall;
    activeGroupCall = null;
    const h = groupHandle();
    const failed = h?.session.getState() === "failed";
    if (h !== null) {
      // Clear the slot BEFORE `hangUp`: the adapter's `closed` transition moves
      // the store to `ended`, whose effect must find no handle left to tear down
      // a second time.
      setGroupHandle(null);
      h.hangUp();
    }
    if (!groupCallStarting) setGroupSlot(null);
    return { call: g, failed };
  }

  /**
   * Leave the group call this device is in: tear the SFU session down and tell
   * the group, as `planGroupCallLeave` decides. Idempotent.
   */
  async function leaveGroupCall(reason: GroupLeaveReason): Promise<void> {
    const { call: g, failed } = releaseGroupSlot();
    if (g === null) return;
    const account = accountId();
    const rec = account === null ? undefined : groupCallsFor(account, g.groupIdB64).get(g.callIdHex);
    const plan = planGroupCallLeave({
      reason,
      sessionFailed: failed,
      joinPublished: g.joinPublished,
      initiator: g.initiator,
      ownLeaf: g.ownLeaf,
      recordJoinedLeaves: rec === undefined ? null : rec.joinedLeaves,
    });
    // Hanging up on a call is not a reason to be told about it again on the
    // next screen. A call whose session failed stays announced, so the user can
    // retry from wherever they are.
    if (plan.leaveReason === "hungUp") dismissGroupCall(g.callIdHex);
    if (plan.publishLeave) {
      applyGroupCallEvent(g.groupIdB64, {
        kind: "leave",
        callIdHex: g.callIdHex,
        leafIndex: g.ownLeaf,
        observedAtMs: Date.now(),
      });
      void publishGroupControl(g.groupIdB64, (w) =>
        w.composeGroupCallLeaveSignaling(g.callId, g.ownLeaf, plan.leaveReason),
      );
    }
    if (plan.publishEnd) {
      applyGroupCallEvent(g.groupIdB64, { kind: "end", callIdHex: g.callIdHex });
      await publishGroupControl(g.groupIdB64, (w) =>
        w.composeCallEndSignaling(g.callId, plan.leaveReason),
      );
    }
  }

  /**
   * The group's MLS lock queue drained: the epoch may have moved. Rekey the
   * live group call if it did (G-18 / ADR-0029) — the worker keeps the previous
   * key for the overlap window so in-flight frames still open, then drops it.
   * Fail-closed: a re-derivation failure keeps the current seed, never a
   * downgrade; a group this device is no longer in ends the call locally.
   */
  function onGroupMlsSettled(groupIdB64: string): void {
    const g = activeGroupCall;
    const h = groupHandle();
    const entry = groupEntry(groupIdB64);
    const currentEpoch = entry === undefined ? undefined : readEpoch(entry.group);
    const action = decideGroupCallEpochChange({
      callGroupIdB64: g !== null && h !== null ? g.groupIdB64 : null,
      settledGroupIdB64: groupIdB64,
      groupJoined: entry !== undefined,
      isActive: entry === undefined ? undefined : readIsActive(entry.group),
      keyedEpoch: g?.keyedEpoch ?? null,
      currentEpoch,
    });
    switch (action) {
      case "none":
        return;
      case "end-locally":
        // Nothing can be published to a group this device has left.
        releaseGroupSlot();
        return;
      case "rekey": {
        const id = chats.identity();
        if (g === null || h === null || entry === undefined || id === null) return;
        try {
          const newSeed = entry.group.exportSecret(id, "lakko/call/v1", g.callId, 32);
          h.rekey(newSeed);
          g.keyedEpoch = currentEpoch ?? null;
        } catch {
          // Keep the current seed on a re-derivation failure.
        }
        return;
      }
    }
  }
  onCleanup(onGroupMlsIdle(onGroupMlsSettled));

  // A chat deleted from the registry while its group call runs: the group's
  // lock may never drain again, so watch the registry itself.
  createEffect(() => {
    const slot = groupSlot();
    if (slot === null || slot.starting) return;
    if (chats.get(slot.groupIdB64) === undefined) untrack(() => releaseGroupSlot());
  });

  // The embedded in-call UI's End button moves the store to `ended`; leave the
  // group call through `leaveGroupCall`, so the Leave (and, if nobody else is
  // left, the End) reaches the group.
  createEffect(() => {
    if (callUi.phase().kind !== "ended") return;
    if (groupHandle() !== null) void untrack(() => leaveGroupCall("hungUp"));
  });

  // Mic toggle -> the live SFU session.
  createEffect(() => {
    const local = callUi.participants().find((p) => p.isLocal);
    const h = groupHandle();
    if (local === undefined || h === null) return;
    h.setMicrophoneEnabled(local.micOn);
  });

  // In-call device picker -> `replaceTrack` on the live SFU sender (it keeps its
  // SFrame transform, so media stays encrypted).
  let lastGroupAudioInput: string | null = null;
  let lastGroupVideoInput: string | null = null;
  createEffect(() => {
    const audioId = callUi.settings().selectedAudioInput;
    const videoId = callUi.settings().selectedVideoInput;
    const h = groupHandle();
    if (h === null) {
      lastGroupAudioInput = null;
      lastGroupVideoInput = null;
      return;
    }
    if (audioId !== null && audioId !== lastGroupAudioInput) {
      lastGroupAudioInput = audioId;
      void h.switchAudioInput(audioId);
    }
    if (videoId !== null && videoId !== lastGroupVideoInput) {
      lastGroupVideoInput = videoId;
      void h.switchVideoInput(videoId);
    }
  });

  // Camera toggle -> group mid-call camera-on. A capture failure reverts the
  // toggle and the call stays up audio-only (invariant #4).
  createEffect(() => {
    const local = callUi.participants().find((p) => p.isLocal);
    const h = groupHandle();
    if (local === undefined || h === null) return;
    if (local.camOn) {
      void h.publishVideo().catch((err: unknown) => {
        callActions.setCamera(false);
        // eslint-disable-next-line no-console
        console.warn("could not start the camera", err);
      });
    } else {
      h.setCameraEnabled(false);
    }
  });

  // The group call's camera-on lifecycle into the shared store.
  createEffect(() => {
    const h = groupHandle();
    if (h === null) return;
    callActions.setVideoPublishState(h.videoPublishState());
  });

  // Sign-out ends the call and forgets the session's call bookkeeping.
  createEffect(() => {
    const signedIn = auth.state().kind === "session";
    untrack(() => {
      if (signedIn) return;
      const call = activeCall;
      if (call !== null) {
        if (call.getState() === "ringing") void call.decline();
        else void call.hangUp("hungUp");
      }
      // The session is gone, so nothing can be published: tear down locally.
      releaseGroupSlot();
      setDismissedGroupCalls(new Set<string>());
      queue.close();
      queue = newQueue();
      endedCallIds.clear();
      clearGroupCallStore();
    });
  });

  const anyCallInProgress = (): boolean => inCall() || groupSlot() !== null;

  // A reload or tab close cannot publish an End or a Leave reliably; the
  // browser's own prompt is the most that can be done.
  const onBeforeUnload = (e: BeforeUnloadEvent): void => {
    if (!anyCallInProgress()) return;
    e.preventDefault();
    e.returnValue = "";
  };
  globalThis.window?.addEventListener("beforeunload", onBeforeUnload);
  onCleanup(() => {
    globalThis.window?.removeEventListener("beforeunload", onBeforeUnload);
    queue.close();
    const call = activeCall;
    if (call !== null) void call.hangUp("hungUp");
    if (activeGroupCall !== null || groupHandle() !== null) void leaveGroupCall("hungUp");
  });

  const api: CallHostApi = {
    state,
    callGroupIdB64: callGroup,
    lastEnded,
    inCall,
    activeGroupIdB64: () => (inCall() ? callGroup() : null),
    ingestSignaling(groupIdB64, row) {
      queue.enqueue({ groupIdB64, row });
    },
    whenSignalingIdle: () => queue.whenIdle(),
    dial,
    groupCall: groupSlot,
    groupCallBusy: () => groupSlot() !== null,
    callLaneGroupIdB64: () => (inCall() ? callGroup() : (groupSlot()?.groupIdB64 ?? null)),
    anyCallInProgress,
    startGroupCall,
    leaveGroupCall: () => leaveGroupCall("hungUp"),
  };

  const internals: CallHostInternals = {
    remoteStream,
    localStream,
    remoteWantsVideo,
    connStats,
    nameFor,
    accept,
    decline,
    hangUp,
    groupHandle,
    dismissedGroupCalls,
    dismissGroupCall,
    anyCallInProgress,
  };

  return (
    <CallHostContext.Provider value={api}>
      <CallHostInternalsContext.Provider value={internals}>
        {props.children}
      </CallHostInternalsContext.Provider>
    </CallHostContext.Provider>
  );
}

/** What only the host's own view needs. */
interface CallHostInternals {
  readonly remoteStream: Accessor<MediaStream | null>;
  readonly localStream: Accessor<MediaStream | null>;
  readonly remoteWantsVideo: Accessor<boolean>;
  readonly connStats: Accessor<WebConnectionStats | null>;
  nameFor(groupIdB64: string | null): string;
  accept(withVideo: boolean): Promise<void>;
  decline(): Promise<void>;
  hangUp(): Promise<void>;
  /** The live group call's session adapter, or `null`. */
  readonly groupHandle: Accessor<SfuCallUiHandle | null>;
  readonly dismissedGroupCalls: Accessor<ReadonlySet<string>>;
  dismissGroupCall(callIdHex: string): void;
  anyCallInProgress(): boolean;
}

const CallHostInternalsContext = createContext<CallHostInternals>();

export function useCallHost(): CallHostApi {
  const ctx = useContext(CallHostContext);
  if (ctx === undefined) throw new Error("useCallHost() called outside <CallHostProvider>");
  return ctx;
}

/**
 * The call UI, on every authenticated screen: the 1x1 ringing banner, the full
 * in-call overlay and remote audio of both call kinds, and the banner for a
 * group call in a chat that is not open. Mounted once, in the app shell, so a
 * route change neither hides a ringing call nor unmounts a live one.
 *
 * Rendered inside `<BackgroundReceiveProvider>` (the router is), which the
 * group-call banner reads for which chat is open and to sync a group before
 * joining its call.
 */
export function CallHostView(): JSX.Element {
  const host = useCallHost();
  const internals = useContext(CallHostInternalsContext);
  if (internals === undefined) throw new Error("CallHostView outside <CallHostProvider>");
  const [audioEl, setAudioEl] = createSignal<HTMLAudioElement | null>(null);

  createEffect(() => {
    const el = audioEl();
    const stream = internals.remoteStream();
    if (el === null) return;
    el.srcObject = stream;
    if (stream !== null) {
      void el.play().catch(() => {
        // Autoplay can be blocked until a gesture; Accept counts as one.
      });
    }
  });

  const overlayVisible = (): boolean => {
    const s = host.state();
    return s === "outgoing" || s === "connecting" || s === "connected" || s === "ending";
  };

  const buttonStyle = {
    background: "var(--surface-elevated)",
    color: "var(--surface-fg)",
    border: "none",
    padding: "0.5rem 0.9rem",
    "border-radius": "var(--radius-sm)",
    "font-weight": "600",
    cursor: "pointer",
  } as const;

  return (
    <>
      {/* Stable slot for the whole call, so remote audio does not cut out when
          the banner gives way to the overlay. */}
      <Show when={host.inCall()}>
        <audio ref={(el) => setAudioEl(el)} autoplay style={{ display: "none" }} />
      </Show>

      <Show when={host.state() === "ringing"}>
        <div
          role="alertdialog"
          aria-label="Incoming call"
          style={{
            position: "fixed",
            top: "1rem",
            left: "50%",
            transform: "translateX(-50%)",
            "z-index": 1100,
            "max-width": "calc(100vw - 2rem)",
            padding: "0.75rem 1rem",
            background: "var(--accent)",
            color: "var(--accent-fg)",
            "border-radius": "var(--radius-md)",
            "box-shadow": "0 4px 16px rgba(0, 0, 0, 0.25)",
            display: "flex",
            "align-items": "center",
            gap: "1rem",
            "flex-wrap": "wrap",
          }}
        >
          <div>
            <strong>Incoming call from {internals.nameFor(host.callGroupIdB64())}</strong>
            <div style={{ "font-size": "0.85rem", "margin-top": "0.2rem" }}>
              {internals.remoteWantsVideo() ? "Video call" : "Audio call"} · end-to-end encrypted
              P2P (DTLS-SRTP)
            </div>
          </div>
          <div style={{ display: "flex", gap: "0.4rem" }}>
            <button type="button" style={buttonStyle} onClick={() => void internals.accept(false)}>
              Accept
            </button>
            <Show when={internals.remoteWantsVideo()}>
              <button type="button" style={buttonStyle} onClick={() => void internals.accept(true)}>
                Accept with video
              </button>
            </Show>
            <button
              type="button"
              onClick={() => void internals.decline()}
              style={{
                background: "transparent",
                color: "var(--accent-fg)",
                border: "1px solid var(--accent-fg)",
                padding: "0.5rem 0.9rem",
                "border-radius": "var(--radius-sm)",
                cursor: "pointer",
              }}
            >
              Decline
            </button>
          </div>
        </div>
      </Show>

      <Show when={overlayVisible()}>
        <Call
          embedded={true}
          onExit={() => void internals.hangUp()}
          remoteStreamFor={(participantId) =>
            participantId === "peer" ? (internals.remoteStream() ?? undefined) : undefined
          }
          localStream={() => internals.localStream() ?? undefined}
        />
      </Show>

      {/* The group (SFU) call's in-call UI. The session is the host's, so this
          stays up on every screen and a return to the call's chat shows it
          rather than offering a second call. */}
      <Show when={internals.groupHandle()}>
        {(handle) => (
          <>
            <Call
              embedded={true}
              onExit={() => void host.leaveGroupCall()}
              remoteStreamFor={(participantId): MediaStream | undefined =>
                // The roster id is `peer-<peerId>`; the SFU sets the downstream
                // MSID stream id to that same peer id.
                handle().remoteStreams().get(participantId.replace(/^peer-/, ""))
              }
              localStream={(): MediaStream | undefined => handle().localStream() ?? undefined}
            />
            {/* One hidden sink per remote stream, keyed by stream identity, so
                audio does not cut on roster churn. Remote video renders per tile
                through `remoteStreamFor`. */}
            <For each={[...handle().remoteStreams().values()]}>
              {(stream) => (
                <audio
                  autoplay
                  style={{ display: "none" }}
                  ref={(el) => {
                    el.srcObject = stream;
                  }}
                />
              )}
            </For>
          </>
        )}
      </Show>

      <GroupCallBanner />
    </>
  );
}

/**
 * A group call in progress in a chat that is not open. The chat view has its
 * own banner; this one covers every other screen, which is where a call started
 * by another member used to go unnoticed.
 */
function GroupCallBanner(): JSX.Element {
  const host = useCallHost();
  const maybeInternals = useContext(CallHostInternalsContext);
  if (maybeInternals === undefined) throw new Error("GroupCallBanner outside <CallHostProvider>");
  const internals: CallHostInternals = maybeInternals;
  const auth = useAuth();
  const chats = useChats();
  const receive = useBackgroundReceive();

  const [foregroundVersion, setForegroundVersion] = createSignal(0);
  onCleanup(receive.coordinator.onForegroundChange(() => setForegroundVersion((v) => v + 1)));
  // Liveness is time-based (an empty call stops being live 60 s after it
  // started), so the selection needs a clock that ticks.
  const [clock, setClock] = createSignal(Date.now());
  const clockTimer = setInterval(() => setClock(Date.now()), 10_000);
  onCleanup(() => clearInterval(clockTimer));
  /** A failed join, for the call it was about only. */
  const [failure, setFailure] = createSignal<{ callIdHex: string; message: string } | null>(null);
  const [joining, setJoining] = createSignal(false);

  const banner = createMemo(() => {
    const s = auth.state();
    if (s.kind !== "session") return null;
    foregroundVersion();
    const groups = chats.groups();
    const live = liveGroupCallsFor(
      s.account.accountId,
      groups.map((g) => g.groupIdB64),
      clock(),
    ).map((l) => ({ groupIdB64: l.groupIdB64, callIdHex: l.call.callIdHex }));
    return selectGroupCallBanner({
      live,
      isChatOpen: (g) => receive.coordinator.isForeground(g),
      isMember: (g) => {
        const entry = chats.get(g);
        return entry !== undefined && readIsActive(entry.group) !== false;
      },
      inAnyCall: internals.anyCallInProgress(),
      dismissedCallIds: internals.dismissedGroupCalls(),
    });
  });

  async function join(withVideo: boolean): Promise<void> {
    const ref = banner();
    if (ref === null || joining()) return;
    setFailure(null);
    setJoining(true);
    try {
      const outcome = await host.startGroupCall({
        groupIdB64: ref.groupIdB64,
        withVideo,
        joinCallIdHex: ref.callIdHex,
        syncGroup: () => receive.syncGroupNow(ref.groupIdB64),
      });
      if (outcome.kind === "ended") internals.dismissGroupCall(ref.callIdHex);
      else if (outcome.kind === "failed") {
        setFailure({ callIdHex: ref.callIdHex, message: outcome.message });
      }
    } finally {
      setJoining(false);
    }
  }

  const buttonStyle = {
    background: "var(--surface-elevated)",
    color: "var(--surface-fg)",
    border: "none",
    padding: "0.5rem 0.9rem",
    "border-radius": "var(--radius-sm)",
    "font-weight": "600",
    cursor: "pointer",
  } as const;

  return (
    <Show when={banner()}>
      {(ref) => (
        <div
          role="status"
          aria-label="Group call in progress"
          style={{
            position: "fixed",
            top: "1rem",
            left: "50%",
            transform: "translateX(-50%)",
            "z-index": 1100,
            "max-width": "calc(100vw - 2rem)",
            padding: "0.75rem 1rem",
            background: "var(--accent)",
            color: "var(--accent-fg)",
            "border-radius": "var(--radius-md)",
            "box-shadow": "0 4px 16px rgba(0, 0, 0, 0.25)",
            display: "flex",
            "align-items": "center",
            gap: "1rem",
            "flex-wrap": "wrap",
          }}
        >
          <div>
            <strong>Group call in {internals.nameFor(ref().groupIdB64)}</strong>
            <div style={{ "font-size": "0.85rem", "margin-top": "0.2rem" }}>
              End-to-end encrypted
            </div>
            <Show when={failure()?.callIdHex === ref().callIdHex ? failure()?.message : undefined}>
              {(message) => (
                <div role="alert" style={{ "font-size": "0.85rem", "margin-top": "0.2rem" }}>
                  {message()}
                </div>
              )}
            </Show>
          </div>
          <div style={{ display: "flex", gap: "0.4rem", "flex-wrap": "wrap" }}>
            <button
              type="button"
              style={buttonStyle}
              disabled={joining()}
              onClick={() => void join(false)}
            >
              Join
            </button>
            <button
              type="button"
              style={buttonStyle}
              disabled={joining()}
              onClick={() => void join(true)}
            >
              Join with video
            </button>
            <A
              href={paths.chat(base64UrlEncodeNoPad(base64Decode(ref().groupIdB64)))}
              style={{ ...buttonStyle, "text-decoration": "none" }}
            >
              Open chat
            </A>
            <button
              type="button"
              onClick={() => {
                setFailure(null);
                internals.dismissGroupCall(ref().callIdHex);
              }}
              style={{
                background: "transparent",
                color: "var(--accent-fg)",
                border: "1px solid var(--accent-fg)",
                padding: "0.5rem 0.9rem",
                "border-radius": "var(--radius-sm)",
                cursor: "pointer",
              }}
            >
              Dismiss
            </button>
          </div>
        </div>
      )}
    </Show>
  );
}

/** Dot colour for a quality tier, for compact in-chat readouts. */
export function qualityDotColour(tier: QualityTier): string {
  switch (qualityColour(tier)) {
    case "green":
      return "#22c55e";
    case "yellow":
      return "#eab308";
    case "red":
      return "#ef4444";
  }
}
