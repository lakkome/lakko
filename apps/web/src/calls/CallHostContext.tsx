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
 *   - **The call UI** ({@link CallHostView}, mounted in the router's root
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
 *     {@link CallHostView} on every screen.
 *   - **A call started in a chat that is not open** is announced by a banner in
 *     {@link CallHostView}, from the group-call records both receive paths feed.
 *
 * The decisions are in `group-call-host.ts` (pure, tested).
 *
 * ## And the call-link (`/c/<slug>`, ADR-0032) call
 *
 * `routes/JoinCall.tsx` owned its `MultiJoinerCall`, so leaving the route hung
 * it up, and nothing kept a link call from starting while a 1x1 or group call
 * was live — both then drove the one shared in-call UI store. The host now
 * holds a third slot for it, with the orchestrator, the overlay, the remote
 * audio sinks, the controls and the hang-up; the route decodes the link, runs
 * the guest sign-up and asks {@link CallHostApi.startLinkCall}. Before the call
 * is live, and after it ends, {@link CallHostView} carries its status on other
 * screens. The protocol itself (owner-founds, committer election, admission
 * dedup, per-admission rekey, the owner-membership check, the admission
 * timeout) is `MultiJoinerCall`'s and unchanged.
 *
 * The three slots are one: `call-slot.ts` decides which call occupies it, and
 * every entry point — dial, group start / join, link start / join, and a 1x1
 * offer (answered busy) — refuses while it is taken. Link-call decisions are in
 * `link-call-host.ts`.
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
import { ownPersonKeyB64 } from "../chat/person-key";
import { base64Decode, base64Encode, base64UrlEncodeNoPad } from "../auth/account-id";
import { loadAccepted } from "../contacts/local-store";
import { onGroupMlsIdle } from "../mls/group-lock";
import { buildPeerTagMap, personForPeerTag, type PeerTagBinding, type PeerTagMap } from "./peer-tag-map";
import { useProfiles } from "../profile/ProfileContext";
import { memberNameOf } from "../profile/member-label";
import { useGroupNames } from "../chat/GroupNameContext";
import { decideConversationKind } from "../chat/conversation-kind";
import { persistedConversationKind } from "../chat/conversation-kind-store";
import { groupTitle } from "../chat/group-title";
import { paths } from "../routes/paths";
import {
  callActions,
  callState as callUi,
  type Participant,
  type QualityTier,
} from "../state/call-state";
import { loadWasm } from "../wasm";
import Call from "../routes/Call";
import { Icon } from "../ui/icons";
import { IncomingCallScreen } from "./IncomingCallScreen";
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
import { CallSoundController } from "./CallSoundController";
import { classifyCallOutcome, type CallLogEntry } from "./call-log";
import { useCallLog } from "./CallLogContext";
import { CallJwtClient } from "./call-jwt-client";
import {
  GROUP_CALL_ALONE_END_MS,
  decideGroupCallEpochChange,
  groupCallAloneAction,
  planGroupCallLeave,
  selectGroupCallBanner,
  type GroupLeaveReason,
} from "./group-call-host";
import {
  freshMemberKeys,
  GROUP_CALL_HEARTBEAT_MS,
  hexToBytes,
  isGroupCallLive,
  type GroupCallEvent,
  type GroupCallViewer,
} from "./group-call-record";
import { callBusyMessage, callSlotOccupant, type CallKind } from "./call-slot";
import {
  decideLinkCallStart,
  linkCallNotice,
  linkCallOccupiesSlot,
  reduceLinkCallPhase,
  type LinkCallEvent,
  type LinkCallPhase,
} from "./link-call-host";
import { MultiJoinerCall, type MultiJoinerCallOptions } from "./multi-joiner-call";
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
  /**
   * Lowercase hex of the call id of the 1x1 call RINGING on this device, or
   * `null` when nothing is ringing.
   *
   * Exists so a press of Accept or Decline on a background notification can
   * be matched to the call it names before it is acted on — see
   * `call-notification-intent.ts`, where that match is the whole security
   * boundary around an attacker-influenceable input.
   */
  readonly ringingCallIdHex: Accessor<string | null>;
  /** Answer the ringing 1x1 call. No-op when nothing is ringing. */
  acceptIncomingCall(withVideo: boolean): Promise<void>;
  /** Refuse the ringing 1x1 call. No-op when nothing is ringing. */
  declineIncomingCall(): Promise<void>;
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
  /** The call occupying the one call slot, or `null` when it is free. */
  callOccupant(): CallKind | null;
  /** Why starting / joining a call now would be refused, or `null` when the
   *  slot is free. */
  callBusyMessage(verb: "start" | "join"): string | null;
  /** End whatever call occupies the slot (declines a ringing 1x1 call). */
  endCurrentCall(): Promise<void>;
  /** The call-link call in the host's link slot — occupying the slot, or kept
   *  after it ended so its outcome can be shown — or `null`. */
  readonly linkCall: Accessor<LinkCallSlot | null>;
  /** Found (owner) or join a call from a `/c/` link. Never rejects. */
  startLinkCall(opts: StartLinkCallOptions): Promise<LinkCallStartOutcome>;
  /** Leave the link call (or stop waiting to be admitted). */
  leaveLinkCall(): void;
  /** Forget a link call's outcome once it has been shown. */
  dismissLinkCallOutcome(): void;
  /** The `/c/` route is showing the link call; the host does not repeat its
   *  status elsewhere meanwhile. Returns the release. */
  claimLinkCallView(): () => void;
}

/** The call-link call in the host's link slot. */
export interface LinkCallSlot {
  /** Lowercase hex of the capability's 16-byte `call_id`. */
  readonly callIdHex: string;
  /** The `/c/<slug>` route the call was started from, to return to it. */
  readonly routePath: string;
  readonly role: "owner" | "joiner";
  readonly phase: LinkCallPhase;
  /** The reason, for `phase === "error"`. */
  readonly errorMessage: string | null;
}

/** A verified `CallInviteCapability`, as the link route decoded it. */
export interface StartLinkCallOptions {
  readonly callId: Uint8Array;
  readonly ownerAccountId: Uint8Array;
  readonly ownerSignatureKey: Uint8Array;
  readonly mediaVideo: boolean;
  /** The local identity IS the capability owner: found + host. */
  readonly isOwner: boolean;
  readonly routePath: string;
}

export type LinkCallStartOutcome =
  | { readonly kind: "started" }
  | { readonly kind: "already-in-this-call" }
  | { readonly kind: "busy"; readonly message: string }
  | { readonly kind: "failed"; readonly message: string };

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
  | { readonly kind: "busy"; readonly message: string }
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
  /** The name on the user's own tile (§4.4): the profile name, `You` when
   *  none is set. Read at call start; a rename mid-call lands next call. */
  const ownTileName = (): string => profiles.ownProfile().displayName?.trim() || "You";
  const callLog = useCallLog();
  const groupNames = useGroupNames();

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
  /** The call-link slot. */
  const [linkSlot, setLinkSlot] = createSignal<LinkCallSlot | null>(null);
  /** The link call's orchestrator, while it holds a session. */
  const [linkHandle, setLinkHandle] = createSignal<MultiJoinerCall | null>(null);
  /** How many `/c/` route views are showing the link call. */
  const [linkViewClaims, setLinkViewClaims] = createSignal(0);

  let activeCall: CallSession | null = null;
  /** The session that last occupied the slot. Unlike `activeCall` it is NOT
   *  cleared on `done`: `CallSession` reports `done` twice when it ends — once
   *  without a reason from `dispose()`, then again with the reason — and the
   *  second report is the only one that says why the call ended. */
  let slotSession: CallSession | null = null;
  /** Call ids of 1x1 calls that ended here this session: never ring again. */
  const endedCallIds = new Set<string>();

  /**
   * What the call log needs to know about the call in the 1x1 slot
   * (`docs/ux/calls.md` §6). Not a signal: nothing renders from it, and it
   * must be readable from the terminal report without a reactive read.
   */
  interface CallJournal {
    readonly session: CallSession;
    readonly groupIdB64: string;
    readonly callIdHex: string;
    /** This device was being called. */
    readonly incoming: boolean;
    /** Whether the call was PLACED as a video call — the caller's choice, or
     *  what the offer said for an incoming one. A camera turned on mid-call
     *  does not retroactively make a voice call a video call. */
    readonly video: boolean;
    readonly startedAtMs: number;
    /** When media first flowed, or `null` if it never did. */
    connectedAtMs: number | null;
    /** The reason the recorded row was written from, so a second terminal
     *  report that carries no reason cannot overwrite one that did. */
    recordedWithReason: boolean;
  }
  let callJournal: CallJournal | null = null;

  /**
   * Write the 1x1 call's row.
   *
   * `CallSession` reports `done` twice — once with no reason from `dispose()`,
   * then again with the reason — and the order is not guaranteed, so a report
   * carrying no reason must never replace a row written from one that did.
   * Recording is otherwise idempotent on the call id (`call-log-store.ts`), so
   * being called more than once costs nothing.
   */
  function recordOneToOneCall(reason: WebCallEndReason | undefined): void {
    const j = callJournal;
    if (j === null) return;
    if (j.recordedWithReason && reason === undefined) return;
    const connected = j.connectedAtMs !== null;
    const entry: CallLogEntry = {
      id: j.callIdHex,
      outcome: classifyCallOutcome({ incoming: j.incoming, connected, reason }),
      video: j.video,
      // `null`, not zero: a call that never connected has no duration, and
      // `Outgoing call, 0:00` would report a conversation that did not happen.
      durationMs: connected ? Math.max(0, Date.now() - (j.connectedAtMs as number)) : null,
      startedAtMs: j.startedAtMs,
    };
    callLog.record(j.groupIdB64, entry);
    if (reason !== undefined) j.recordedWithReason = true;
  }

  /** Start the journal for a call this device is about to be in. */
  function openCallJournal(
    session: CallSession,
    groupIdB64: string,
    hex: string,
    incoming: boolean,
    video: boolean,
  ): void {
    callJournal = {
      session,
      groupIdB64,
      callIdHex: hex,
      incoming,
      video,
      startedAtMs: Date.now(),
      connectedAtMs: null,
      recordedWithReason: false,
    };
  }

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
    const ownKeyB64 = own === null ? null : base64Encode(own.signaturePublicKey);
    const memberKeysB64 = memberKeysOf(entry.group);
    const account = accountId();
    // The same title the chat header and the list show (ADR-0051 §5): the
    // persisted kind decides group-vs-1x1, the shared name comes first.
    const isGroup =
      decideConversationKind(
        account === null ? null : persistedConversationKind(account, groupIdB64),
        memberKeysB64.length,
      ) === "group";
    return peerName({
      ownKeyB64,
      memberKeysB64,
      resolveName: (k) => profiles.resolve(k).name,
      linkedContactName: loadAccepted().find((c) => c.groupIdB64 === groupIdB64)?.displayName,
      label: groupTitle(
        groupNames.nameOf(groupIdB64) ?? entry.label,
        memberKeysB64
          .filter((k) => k !== ownKeyB64)
          .map((k) => memberNameOf(profiles.resolve(k))),
      ),
      isGroup,
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
    // A call placed from the camera check can start muted (docs/ux/calls.md
    // section 3.2); every one of these constructors used to hardcode `true`,
    // so the mute survived exactly until the session reported its first state.
    micOn: callUi.pendingMicOn(),
    camOn: videoCall(),
    quality: qualityTierForRtt(connStats()?.rttMs ?? null),
    speaking: false,
  });

  const remoteParticipant = (): Participant => ({
    id: "peer",
    displayName: nameFor(callGroup()),
    isLocal: false,
    micOn: true,
    camOn: remoteHasVideo(),
    quality: "good",
    speaking: false,
  });

  function syncCallUiFromState(s: WebCallState): void {
    switch (s) {
      // `docs/ux/calls.md` §4's state table separates these two, and the store
      // used to collapse them into one: a call ringing on the other person's
      // phone said `Connecting...`, which described the network instead of the
      // call. `outgoing` is the caller waiting to be answered — `Calling...`.
      case "outgoing":
        callActions.setPhase({ kind: "calling" });
        callActions.setParticipants([localParticipant()]);
        callActions.setE2ee({ active: false, shortFingerprint: null });
        break;
      case "connecting":
        callActions.setPhase({ kind: "connecting" });
        callActions.setParticipants([localParticipant()]);
        // 1x1 web media is P2P DTLS-SRTP, not SFrame: no safety number, so no
        // chip (see `encryption-chip.ts`).
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
        watchRemoteVideo(s, setRemoteHasVideo);
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
        // The one moment media is known to flow: what separates an answered
        // call from a missed one in the conversation's history.
        if (s === "connected" && callJournal?.session === session) {
          callJournal.connectedAtMs ??= Date.now();
        }
        if (s === "done") {
          if (callJournal?.session === session) recordOneToOneCall(reason);
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
      // A DS message id is unique only within one group's journal.
      (item) => `${item.groupIdB64}:${item.row.messageId}`,
      (err) => {
        // eslint-disable-next-line no-console
        console.warn("call signaling dispatch failed", err);
      },
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
        // A group or link call makes this device busy for a 1x1 offer too.
        groupCallBusy: groupSlot() !== null || linkOccupies(),
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
          openCallJournal(session, item.groupIdB64, hex, true, parsed.mediaVideo);
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
      case "groupLeave": {
        // The roster keys members on the sender's signature key resolved at
        // receipt, never on the leaf (C-68). Unresolvable sender: dropped.
        const memberKey = row.senderMemberKey;
        if (memberKey === undefined || memberKey.length === 0) return;
        applyGroupCallEventFor(
          account,
          groupIdB64,
          parsed.kind === "groupJoin"
            ? {
                kind: "join",
                callIdHex: hex,
                memberKey,
                personKey: row.senderPersonKey ?? memberKey,
                observedAtMs,
              }
            : { kind: "leave", callIdHex: hex, memberKey, observedAtMs },
          now,
        );
        return;
      }
      default:
        return;
    }
  }

  // ----- controls ---------------------------------------------------------

  async function dial(groupIdB64: string, withVideo: boolean): Promise<void> {
    if (currentOccupant() !== null) return;
    if (groupEntry(groupIdB64) === undefined) return;
    setVideoCall(withVideo);
    const callId = mintCallId();
    const session = buildCallSession(groupIdB64, callId);
    openCallJournal(session, groupIdB64, callIdHex(callId), false, withVideo);
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
    /** This device's signature key and person key (base64) — how the
     *  group-call record files this device. */
    readonly ownMemberKey: string;
    readonly ownPersonKey: string;
    readonly initiator: boolean;
    joinPublished: boolean;
    /** Re-publishes the Join every `GROUP_CALL_HEARTBEAT_MS` once it was
     *  published; cleared when the call leaves the slot. */
    heartbeat: ReturnType<typeof setInterval> | null;
    /** The MLS epoch the session's current seed was derived at. */
    keyedEpoch: bigint | null;
    /** Whether this device asked for video when it started or joined. */
    readonly video: boolean;
    /** When this device entered the call (`docs/ux/calls.md` §6). */
    readonly startedAtMs: number;
    /** When this device's own media first flowed, or `null` if it never did. */
    connectedAtMs: number | null;
    /** This call has had at least one other participant on the roster
     *  (`groupCallAloneAction`): an initiator still waiting for the first
     *  joiner never ends by itself. */
    sawRemote: boolean;
    /** The alone-end timer (`GROUP_CALL_ALONE_END_MS`), running while everyone
     *  else has left; cleared when someone comes back or the call leaves the
     *  slot. */
    aloneTimer: ReturnType<typeof setTimeout> | null;
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
    {
      const occupant = groupCallStarting ? "group" : currentOccupant();
      if (occupant !== null) {
        return { kind: "busy", message: callBusyMessage(occupant, opts.joinCallIdHex === null ? "start" : "join") };
      }
    }
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
        // Not in this call yet: this device's own stale entry (a session that
        // died before a reload) must not make a finished call look joinable.
        const ownId = chats.identity();
        const viewer: GroupCallViewer | undefined =
          ownId === null
            ? undefined
            : { ownMemberKey: base64Encode(ownId.signaturePublicKey), inCallIdHex: null };
        if (still === undefined || !isGroupCallLive(still, Date.now(), viewer)) {
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
      // ADR-0021 — the per-call peer_tag -> person map, derived locally (the
      // SFU never sends a name) over every account id this device holds: the
      // accepted contacts' AND the ones members claimed in their profiles
      // (ADR-0048 v2). Not frozen at call start — a newcomer's profile is
      // answered within the first poll or so, and the roster event that
      // named its tile may have run before it landed. The map is rebuilt
      // whenever the profile store changes (`profiles.version` also bumps on
      // an accepted-contacts change), and `refreshNames` below re-runs the
      // roster through it.
      let tagMap: { version: number; map: PeerTagMap } | null = null;
      const currentTagMap = (): PeerTagMap => {
        const v = profiles.version();
        if (tagMap !== null && tagMap.version === v) return tagMap.map;
        const bindings: PeerTagBinding[] = [];
        for (const c of loadAccepted()) {
          if (c.accountIdHex !== undefined) {
            bindings.push({ personKeyB64: c.signaturePubkeyB64, accountIdHex: c.accountIdHex });
          }
        }
        for (const b of profiles.accountIdBindings()) bindings.push(b);
        const map = buildPeerTagMap(bindings, (accountIdHex) => wasm.sfuPeerTag(hex, accountIdHex));
        tagMap = { version: v, map };
        return map;
      };
      const resolveDisplayName = (
        _peerId: string,
        pseudonym: string,
        peerTag: string | undefined,
      ): string => {
        // The one resolver every screen names people through; a call tile
        // shows the name only.
        const person = personForPeerTag(currentTagMap(), peerTag);
        if (person !== null) return profiles.resolve(person).name;
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
        ownMemberKey: base64Encode(id.signaturePublicKey),
        ownPersonKey: ownPersonKeyB64(id) ?? base64Encode(id.signaturePublicKey),
        initiator,
        joinPublished: false,
        heartbeat: null,
        keyedEpoch,
        video: opts.withVideo,
        startedAtMs: Date.now(),
        connectedAtMs: null,
        sawRemote: false,
        aloneTimer: null,
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
          localDisplayName: ownTileName(),
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

  // A profile, a rename or an accepted contact landing while a group call is
  // up re-names the tiles already on screen: the adapter replays each peer's
  // (pseudonym, peer_tag) through `resolveDisplayName`, whose map above is
  // keyed on the same version this effect tracks.
  createEffect(() => {
    profiles.version();
    const h = groupHandle();
    if (h !== null) untrack(() => h.refreshNames());
  });

  /** SFU session lifecycle -> the group journal. */
  function onGroupSessionState(hex: string, state: SfuCallState): void {
    const g = activeGroupCall;
    if (g === null || g.callIdHex !== hex) return;
    if (state === "connected") g.connectedAtMs ??= Date.now();
    if (state === "connected" && !g.joinPublished) {
      g.joinPublished = true;
      publishGroupJoin(g);
      // Heartbeat: the same Join, re-published while this device is in the
      // call, so a participant whose app dies without a Leave stops counting
      // on every other device once its Join goes stale.
      g.heartbeat = setInterval(() => {
        if (activeGroupCall !== g) {
          stopGroupHeartbeat(g);
          return;
        }
        publishGroupJoin(g);
      }, GROUP_CALL_HEARTBEAT_MS);
    } else if (state === "failed") {
      // Normally already handled: the adapter moves the store to `ended` first,
      // and the ended-phase effect leaves. This covers the case where it did not.
      void leaveGroupCall("error");
    }
  }

  /** Publish this device's `GroupCallJoin` (first Join or heartbeat) and apply
   *  it locally: our own publish never comes back through the poll. */
  function publishGroupJoin(g: ActiveGroupCall): void {
    // The leaf the receivers check the body against is the sender leaf of
    // the frame, so name the leaf this device holds NOW.
    let leaf = g.ownLeaf;
    try {
      leaf = groupEntry(g.groupIdB64)?.group.ownLeafIndex ?? g.ownLeaf;
    } catch {
      leaf = g.ownLeaf;
    }
    applyGroupCallEvent(g.groupIdB64, {
      kind: "join",
      callIdHex: g.callIdHex,
      memberKey: g.ownMemberKey,
      personKey: g.ownPersonKey,
      observedAtMs: Date.now(),
    });
    void publishGroupControl(g.groupIdB64, (w) =>
      w.composeGroupCallJoinSignaling(g.callId, leaf, BigInt(Date.now())),
    );
  }

  function stopGroupHeartbeat(g: ActiveGroupCall): void {
    if (g.heartbeat !== null) {
      clearInterval(g.heartbeat);
      g.heartbeat = null;
    }
  }

  /** Take the group call out of the slot and tear the SFU session down.
   *  Synchronous, so the several paths that can end a call act once. */
  function releaseGroupSlot(): { readonly call: ActiveGroupCall | null; readonly failed: boolean } {
    const g = activeGroupCall;
    activeGroupCall = null;
    if (g !== null) {
      stopGroupHeartbeat(g);
      if (g.aloneTimer !== null) {
        clearTimeout(g.aloneTimer);
        g.aloneTimer = null;
      }
    }
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
  /**
   * Write the group call's row (`docs/ux/calls.md` §6).
   *
   * Direction here is not who rang whom — nobody rings in a group call — but
   * whose call it was from this device's side: one it started is `outgoing`,
   * one it joined is `incoming`. There is deliberately no `missed` row for a
   * group call this device never joined: nothing rang, so nothing was missed,
   * and the group-call banner is what announces a live one.
   */
  function recordGroupCall(g: ActiveGroupCall): void {
    const connected = g.connectedAtMs !== null;
    callLog.record(g.groupIdB64, {
      id: g.callIdHex,
      // Nobody rings in a group call, so "which way it went" is whose call it
      // was from this device's side: one it started is `outgoing`, one it
      // joined is `incoming`. Never `missed` — nothing rang, so nothing was
      // missed, and the group-call banner is what announces a live one.
      outcome: g.initiator ? "outgoing" : "incoming",
      video: g.video,
      durationMs: connected ? Math.max(0, Date.now() - (g.connectedAtMs as number)) : null,
      startedAtMs: g.startedAtMs,
    });
  }

  async function leaveGroupCall(reason: GroupLeaveReason): Promise<void> {
    const { call: g, failed } = releaseGroupSlot();
    if (g === null) return;
    recordGroupCall(g);
    const account = accountId();
    const rec = account === null ? undefined : groupCallsFor(account, g.groupIdB64).get(g.callIdHex);
    const plan = planGroupCallLeave({
      reason,
      sessionFailed: failed,
      joinPublished: g.joinPublished,
      initiator: g.initiator,
      ownMemberKey: g.ownMemberKey,
      recordFreshMemberKeys: rec === undefined ? null : freshMemberKeys(rec, Date.now()),
    });
    // Hanging up on a call is not a reason to be told about it again on the
    // next screen. A call whose session failed stays announced, so the user can
    // retry from wherever they are.
    if (plan.leaveReason === "hungUp") dismissGroupCall(g.callIdHex);
    if (plan.publishLeave) {
      applyGroupCallEvent(g.groupIdB64, {
        kind: "leave",
        callIdHex: g.callIdHex,
        memberKey: g.ownMemberKey,
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
  onCleanup(() => {
    if (activeGroupCall !== null) stopGroupHeartbeat(activeGroupCall);
  });

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

  // Everyone else left -> the call ends by itself as a hang-up after
  // `GROUP_CALL_ALONE_END_MS`, unless someone comes back first
  // (`docs/ux/calls.md` §4.3, decided by `groupCallAloneAction`).
  createEffect(() => {
    const remotes = callUi.participants().filter((p) => !p.isLocal).length;
    untrack(() => {
      const g = activeGroupCall;
      if (g === null || groupHandle() === null) return;
      if (remotes > 0) g.sawRemote = true;
      switch (groupCallAloneAction(remotes, g.sawRemote, g.aloneTimer !== null)) {
        case "arm-end":
          console.info(
            "group call: everyone else left; ending in " +
              GROUP_CALL_ALONE_END_MS / 1000 +
              " s",
          );
          g.aloneTimer = setTimeout(() => {
            g.aloneTimer = null;
            if (activeGroupCall !== g) return;
            if (callUi.participants().some((p) => !p.isLocal)) return;
            console.info("group call: ended because everyone else left");
            void leaveGroupCall("hungUp");
          }, GROUP_CALL_ALONE_END_MS);
          break;
        case "disarm":
          if (g.aloneTimer !== null) clearTimeout(g.aloneTimer);
          g.aloneTimer = null;
          break;
        default:
          break;
      }
    });
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

  // ----- the call-link (ADR-0032) call ---------------------------------------

  function linkOccupies(): boolean {
    const slot = linkSlot();
    return slot !== null && linkCallOccupiesSlot(slot.phase);
  }

  function currentOccupant(): CallKind | null {
    return callSlotOccupant({
      oneToOneInCall: inCall(),
      groupCallActive: groupSlot() !== null,
      linkCallActive: linkOccupies(),
    });
  }

  /** The attempt the link slot belongs to. A report from an older attempt —
   *  one the user already left — never touches the slot. */
  let linkAttempt: object | null = null;

  function applyLinkEvent(attempt: object, event: LinkCallEvent, errorMessage?: string): void {
    if (linkAttempt !== attempt) return;
    const slot = linkSlot();
    if (slot === null) return;
    const phase = reduceLinkCallPhase(slot.phase, event);
    if (phase === slot.phase) return;
    setLinkSlot({
      ...slot,
      phase,
      errorMessage: phase === "error" ? (errorMessage ?? slot.errorMessage) : slot.errorMessage,
    });
    if (!linkCallOccupiesSlot(phase)) {
      // Over on its own: drop the session (idempotent — it also resets the
      // shared in-call store) and keep the outcome for the user to see.
      const h = linkHandle();
      setLinkHandle(null);
      h?.hangUp();
    }
  }

  async function startLinkCall(opts: StartLinkCallOptions): Promise<LinkCallStartOutcome> {
    const hex = callIdHex(opts.callId);
    const verb = opts.isOwner ? "start" : "join";
    const active = linkSlot();
    // No await between this decision and the slot write below, so a double tap
    // finds the slot taken (`already-in-this-call`) rather than starting twice.
    const decision = decideLinkCallStart({
      occupant: currentOccupant(),
      activeLinkCallIdHex:
        active !== null && linkCallOccupiesSlot(active.phase) ? active.callIdHex : null,
      requestedCallIdHex: hex,
      verb,
    });
    if (decision.kind !== "start") return decision;
    const identity = chats.identity();
    const secrets = auth.identitySecrets();
    if (identity === null || secrets === null) {
      return { kind: "failed", message: "Re-authenticate to join this call (no live session)." };
    }
    const base = sfuBaseUrl();
    if (base === null) {
      return {
        kind: "failed",
        message: "This client has no SFU endpoint — set VITE_LAKKO_SFU_URL and rebuild.",
      };
    }

    const attempt = {};
    linkAttempt = attempt;
    setLinkHandle(null);
    setLinkSlot({
      callIdHex: hex,
      routePath: opts.routePath,
      role: opts.isOwner ? "owner" : "joiner",
      phase: "starting",
      errorMessage: null,
    });

    const callOpts: MultiJoinerCallOptions = {
      identity,
      callId: opts.callId,
      ownerAccountId: opts.ownerAccountId,
      ownerSignatureKey: opts.ownerSignatureKey,
      sfuBaseUrl: base,
      sessionToken: secrets.sessionToken,
      media: { audio: true, video: opts.mediaVideo },
      localDisplayName: ownTileName(),
      onJoined: () => applyLinkEvent(attempt, { kind: "joined" }),
      onEnded: () => applyLinkEvent(attempt, { kind: "ended" }),
      onNoCommitter: () => applyLinkEvent(attempt, { kind: "no-committer" }),
      onError: (err: unknown) =>
        applyLinkEvent(attempt, { kind: "error" }, err instanceof Error ? err.message : String(err)),
    };

    try {
      // Owner founds + hosts (`onJoined` fires inside `host()`); a joiner
      // connects signaling-only, relays its KeyPackage and waits for a Welcome.
      const call = opts.isOwner
        ? await MultiJoinerCall.host(callOpts)
        : await MultiJoinerCall.join(callOpts);
      const slot = linkSlot();
      if (linkAttempt !== attempt || slot === null || !linkCallOccupiesSlot(slot.phase)) {
        // Left, replaced, or already over while the start was in flight.
        call.hangUp();
        return { kind: "started" };
      }
      setLinkHandle(call);
      applyLinkEvent(attempt, { kind: "signaling-open", role: opts.isOwner ? "owner" : "joiner" });
      return { kind: "started" };
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      applyLinkEvent(attempt, { kind: "error" }, message);
      return { kind: "failed", message };
    }
  }

  /** Leave the link call: no outcome to show, the user chose it. */
  function leaveLinkCall(): void {
    linkAttempt = null;
    const h = linkHandle();
    setLinkHandle(null);
    setLinkSlot(null);
    h?.hangUp();
  }

  function dismissLinkCallOutcome(): void {
    const slot = linkSlot();
    if (slot !== null && !linkCallOccupiesSlot(slot.phase)) setLinkSlot(null);
  }

  // The embedded in-call UI's End button moves the store to `ended`. A session
  // that is still up means the user is ending it; one already closed ended on
  // its own, and `onEnded` keeps that outcome for the user to see.
  createEffect(() => {
    if (callUi.phase().kind !== "ended") return;
    const h = linkHandle();
    if (h === null) return;
    const s = h.sessionState();
    if (s !== "closed" && s !== "failed") untrack(() => leaveLinkCall());
  });

  // Link-call controls: mic, camera, device picker, camera lifecycle — the same
  // bindings the group call has. Only once the call is live: a joiner still
  // waiting for its Welcome has published nothing (invariant #4), and must not
  // start announcing camera state to a room it has not been admitted to.
  const liveLinkHandle = (): MultiJoinerCall | null =>
    linkSlot()?.phase === "live" ? linkHandle() : null;
  createEffect(() => {
    const local = callUi.participants().find((p) => p.isLocal);
    const h = liveLinkHandle();
    if (local === undefined || h === null) return;
    h.setMicrophoneEnabled(local.micOn);
  });
  createEffect(() => {
    const local = callUi.participants().find((p) => p.isLocal);
    const h = liveLinkHandle();
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
  let lastLinkAudioInput: string | null = null;
  let lastLinkVideoInput: string | null = null;
  createEffect(() => {
    const audioId = callUi.settings().selectedAudioInput;
    const videoId = callUi.settings().selectedVideoInput;
    const h = liveLinkHandle();
    if (h === null) {
      lastLinkAudioInput = null;
      lastLinkVideoInput = null;
      return;
    }
    if (audioId !== null && audioId !== lastLinkAudioInput) {
      lastLinkAudioInput = audioId;
      void h.switchAudioInput(audioId);
    }
    if (videoId !== null && videoId !== lastLinkVideoInput) {
      lastLinkVideoInput = videoId;
      void h.switchVideoInput(videoId);
    }
  });
  createEffect(() => {
    const h = liveLinkHandle();
    if (h === null) return;
    callActions.setVideoPublishState(h.videoPublishState());
  });

  async function endCurrentCall(): Promise<void> {
    switch (currentOccupant()) {
      case "one-to-one": {
        const call = activeCall;
        if (call === null) return;
        if (call.getState() === "ringing") await call.decline();
        else await call.hangUp("hungUp");
        return;
      }
      case "group":
        await leaveGroupCall("hungUp");
        return;
      case "link":
        leaveLinkCall();
        return;
      case null:
        return;
    }
  }

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
      leaveLinkCall();
      setDismissedGroupCalls(new Set<string>());
      queue.close();
      queue = newQueue();
      endedCallIds.clear();
      clearGroupCallStore();
    });
  });

  const anyCallInProgress = (): boolean => currentOccupant() !== null;

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
    leaveLinkCall();
  });

  const api: CallHostApi = {
    state,
    callGroupIdB64: callGroup,
    lastEnded,
    inCall,
    activeGroupIdB64: () => (inCall() ? callGroup() : null),
    // Keyed on `state()` so it re-reads whenever the slot moves. `activeCall`
    // is assigned by `occupy` BEFORE the session reports `ringing`, so by the
    // time this reads `"ringing"` the call is there.
    ringingCallIdHex: () =>
      state() === "ringing" && activeCall !== null ? callIdHex(activeCall.callId) : null,
    acceptIncomingCall: (withVideo) =>
      activeCall !== null && activeCall.getState() === "ringing"
        ? accept(withVideo)
        : Promise.resolve(),
    declineIncomingCall: () =>
      activeCall !== null && activeCall.getState() === "ringing"
        ? decline()
        : Promise.resolve(),
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
    callOccupant: currentOccupant,
    callBusyMessage: (verb) => {
      const occupant = currentOccupant();
      return occupant === null ? null : callBusyMessage(occupant, verb);
    },
    endCurrentCall,
    linkCall: linkSlot,
    startLinkCall,
    leaveLinkCall,
    dismissLinkCallOutcome,
    claimLinkCallView() {
      setLinkViewClaims((n) => n + 1);
      let released = false;
      return () => {
        if (released) return;
        released = true;
        setLinkViewClaims((n) => Math.max(0, n - 1));
      };
    },
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
    linkHandle,
    linkViewShown: () => linkViewClaims() > 0,
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
  /** The link call's orchestrator, while it holds a session. */
  readonly linkHandle: Accessor<MultiJoinerCall | null>;
  /** A `/c/` route view is showing the link call. */
  linkViewShown(): boolean;
}

const CallHostInternalsContext = createContext<CallHostInternals>();

export function useCallHost(): CallHostApi {
  const ctx = useContext(CallHostContext);
  if (ctx === undefined) throw new Error("useCallHost() called outside <CallHostProvider>");
  return ctx;
}

/**
 * The call UI, on every screen: the 1x1 ringing banner, the full in-call
 * overlay and remote audio of every call kind, the banner for a group call in a
 * chat that is not open, and a call-link call's status away from its route.
 * Mounted once, in the router's root layout (`App.tsx`) — not the authenticated
 * app shell, because a call-link call runs on the public `/c/` route — so a
 * route change neither hides a ringing call nor unmounts a live one. Renders
 * nothing while no call exists.
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

  /**
   * `docs/ux/calls.md` §2.1 — a ringing call is audible, not only visible.
   *
   * The `ringtone-incoming` cue has shipped in `public/sounds/` since the
   * sound controller was written and had no caller anywhere: `routes/Call.tsx`
   * maps only the OUTGOING phases (ringback, join, leave), and the incoming
   * screen is rendered from here, not from there. So a call arriving while the
   * user was reading another chat put a screen up in silence — which on a
   * large window is easy to miss entirely, and is half of what made the
   * 2026-09-18 iOS-to-web call look like nothing had happened.
   *
   * Best effort, and honest about it: a browser's autoplay policy can refuse
   * `play()` until the page has seen a gesture, and `navigator.vibrate` is
   * absent on desktop and ignored by browsers with no engagement. Both failure
   * modes are silent by design (the controller logs a warning); neither
   * affects the call itself.
   */
  const sounds = new CallSoundController();
  let vibrateTimer: ReturnType<typeof setInterval> | null = null;
  const stopVibration = (): void => {
    if (vibrateTimer !== null) {
      clearInterval(vibrateTimer);
      vibrateTimer = null;
    }
    globalThis.navigator?.vibrate?.(0);
  };
  createEffect(() => {
    if (host.state() !== "ringing") {
      sounds.stopLoop("ringtone-incoming");
      stopVibration();
      return;
    }
    sounds.startLoop("ringtone-incoming");
    if (vibrateTimer === null && typeof globalThis.navigator?.vibrate === "function") {
      const pulse = (): void => {
        globalThis.navigator?.vibrate?.([400, 200, 400, 1_000]);
      };
      pulse();
      vibrateTimer = setInterval(pulse, 2_000);
    }
  });
  onCleanup(() => {
    stopVibration();
    sounds.dispose();
  });

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

  // The in-call Speaker control (docs/ux/calls.md section 4.1) moves the sound
  // to the chosen output. It writes `selectedAudioOutput`; this is what makes
  // that a real thing rather than a stored preference — nothing applied it
  // before, so the old settings panel's speaker picker changed nothing.
  createEffect(() => {
    applyAudioOutput(audioEl(), callUi.settings().selectedAudioOutput);
  });

  const overlayVisible = (): boolean => {
    const s = host.state();
    return s === "outgoing" || s === "connecting" || s === "connected" || s === "ending";
  };

  return (
    <>
      {/* Stable slot for the whole call, so remote audio does not cut out when
          the banner gives way to the overlay. */}
      <Show when={host.inCall()}>
        <audio ref={(el) => setAudioEl(el)} autoplay style={{ display: "none" }} />
      </Show>

      {/* `docs/ux/calls.md` §2.2 — a ringing call takes the whole screen, not
          a strip at the top of whatever the user was reading. */}
      <Show when={host.state() === "ringing"}>
        <IncomingCallScreen
          callerName={internals.nameFor(host.callGroupIdB64())}
          callerId={host.callGroupIdB64()}
          video={internals.remoteWantsVideo()}
          onAccept={(withVideo) => void internals.accept(withVideo)}
          onDecline={() => void internals.decline()}
        />
      </Show>

      <Show when={overlayVisible()}>
        <Call
          embedded={true}
          onExit={() => void internals.hangUp()}
          /* The encryption sheet's one sentence names the other person
             (docs/ux/calls.md §5), and on a 1x1 call this device knows who
             that is. */
          peerName={internals.nameFor(host.callGroupIdB64())}
          /* One person, one colour: the portrait's tint is hashed from the
             same id the chat list and the contact row hash. */
          peerId={host.callGroupIdB64()}
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
              group={true}
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
              {(stream) => <RemoteAudio stream={stream} />}
            </For>
          </>
        )}
      </Show>

      {/* The call-link call's in-call UI, once this device is in the call's
          group (owner: from founding; joiner: from its Welcome). */}
      <Show when={host.linkCall()?.phase === "live"}>
        <Call
          embedded={true}
          group={true}
          onExit={() => host.leaveLinkCall()}
          remoteStreamFor={(participantId): MediaStream | undefined =>
            internals.linkHandle()?.remoteStreamFor(participantId)
          }
          localStream={(): MediaStream | undefined =>
            internals.linkHandle()?.localStream() ?? undefined
          }
        />
      </Show>
      <Show when={internals.linkHandle()}>
        {(handle) => (
          <For each={[...handle().remoteStreams().values()]}>
            {(stream) => <RemoteAudio stream={stream} />}
          </For>
        )}
      </Show>

      <LinkCallNoticeView />
      <GroupCallBanner />
    </>
  );
}

/** Remote video tracks already being watched, so a repeated `ontrack` for the
 *  same stream does not stack listeners. */
const watchedRemoteTracks = new WeakSet<MediaStreamTrack>();

/**
 * Keep "the other person is sending a picture" true, in both directions.
 *
 * `docs/ux/calls.md` §4.1 requires the receiver's screen to return to the
 * portrait when the sender stops their camera, and the naive read of the stream
 * cannot do it: stopping the camera on the 1x1 path flips `track.enabled` on the
 * sender rather than removing the track, so the receiver's
 * `getVideoTracks().length` stays 1 forever and the screen would sit on a frozen
 * or black fill — the empty box §4 exists to remove.
 *
 * The signal that does move is the receiving track's own `muted` flag: the
 * browser sets it when media stops arriving and fires `mute` / `unmute`. Using
 * it needs no new wire message — the 1x1 signalling protocol carries offer,
 * answer, ICE and end, and nothing else. **Honest limit:** the flag follows the
 * media, so it lands a beat after the press rather than with it, and how long a
 * beat is depends on the engine. A deterministic version is the `media_state`
 * envelope the SFU path already uses (`lakko_contacts::media_state`), which
 * would need a variant on the 1x1 `CallSignalingPayload` — a Rust wire change.
 *
 * The camera-ON direction needs none of this: adding a track renegotiates, so
 * `ontrack` fires on the receiver and this runs again with a live track.
 */
function watchRemoteVideo(stream: MediaStream, report: (hasVideo: boolean) => void): void {
  const publish = (): void => {
    report(stream.getVideoTracks().some((t) => t.readyState === "live" && !t.muted));
  };
  for (const track of stream.getVideoTracks()) {
    if (watchedRemoteTracks.has(track)) continue;
    watchedRemoteTracks.add(track);
    track.addEventListener("mute", publish);
    track.addEventListener("unmute", publish);
    track.addEventListener("ended", publish);
  }
  publish();
}

/**
 * Move a sink to the output the Speaker control picked.
 *
 * `setSinkId` is absent in Firefox and in Safari on iOS, which is why
 * `docs/ux/calls.md` §4.1 says to omit the Speaker control there rather than
 * show a dead button — `routes/Call.tsx` feature-detects the same method. A
 * rejected promise means the id names a device that is gone (a headset
 * unplugged between the pick and the apply); the sound keeps playing through
 * whatever it was on, which is the right outcome and not worth an error.
 */
function applyAudioOutput(el: HTMLAudioElement | null, deviceId: string | null): void {
  if (el === null || deviceId === null) return;
  if (typeof el.setSinkId !== "function") return;
  void el.setSinkId(deviceId).catch((err: unknown) => {
    // eslint-disable-next-line no-console
    console.warn("call host: could not move audio to the selected output", err);
  });
}

/** One hidden sink for one remote stream, following the Speaker control. */
function RemoteAudio(props: { stream: MediaStream }): JSX.Element {
  const [el, setEl] = createSignal<HTMLAudioElement | null>(null);
  createEffect(() => {
    applyAudioOutput(el(), callUiSettings().selectedAudioOutput);
  });
  return (
    <audio
      autoplay
      style={{ display: "none" }}
      ref={(node) => {
        node.srcObject = props.stream;
        setEl(node);
      }}
    />
  );
}

/** The call store's device settings. A module-level reader so `RemoteAudio`
 *  does not have to be nested inside the provider to see them. */
function callUiSettings(): ReturnType<typeof callUi.settings> {
  return callUi.settings();
}

/** A call-link call's status on a screen other than its `/c/` route. */
function LinkCallNoticeView(): JSX.Element {
  const host = useCallHost();
  const maybeInternals = useContext(CallHostInternalsContext);
  if (maybeInternals === undefined) throw new Error("LinkCallNoticeView outside <CallHostProvider>");
  const internals: CallHostInternals = maybeInternals;

  const notice = createMemo(() => {
    const slot = host.linkCall();
    return linkCallNotice(slot?.phase ?? null, internals.linkViewShown(), slot?.errorMessage ?? null);
  });

  return (
    <Show when={notice()}>
      {(n) => (
        <div class="lk-call-incoming bottom" role="status" aria-label="Call from a link">
          <span class="lk-call-incoming-icon" aria-hidden="true">
            <Icon name="link" size={20} />
          </span>
          <div class="lk-call-incoming-text">
            <span>{n().text}</span>
          </div>
          <div class="lk-call-incoming-actions">
            <Show when={n().actions.includes("open")}>
              <A href={host.linkCall()?.routePath ?? "/"} class="lk-button-link">
                Open
              </A>
            </Show>
            <Show when={n().actions.includes("cancel")}>
              <button type="button" class="ghost" onClick={() => host.leaveLinkCall()}>
                Cancel
              </button>
            </Show>
            <Show when={n().actions.includes("dismiss")}>
              <button type="button" class="ghost" onClick={() => host.dismissLinkCallOutcome()}>
                Dismiss
              </button>
            </Show>
          </div>
        </div>
      )}
    </Show>
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
    const ownId = chats.identity();
    const viewer: GroupCallViewer | undefined =
      ownId === null
        ? undefined
        : {
            ownMemberKey: base64Encode(ownId.signaturePublicKey),
            inCallIdHex: host.groupCall()?.callIdHex ?? null,
          };
    const live = liveGroupCallsFor(
      s.account.accountId,
      groups.map((g) => g.groupIdB64),
      clock(),
      viewer,
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
      else if (outcome.kind === "failed" || outcome.kind === "busy") {
        setFailure({ callIdHex: ref.callIdHex, message: outcome.message });
      }
    } finally {
      setJoining(false);
    }
  }

  return (
    <Show when={banner()}>
      {(ref) => (
        <div class="lk-call-incoming" role="status" aria-label="Group call in progress">
          <span class="lk-call-incoming-icon" aria-hidden="true">
            <Icon name="groups" size={20} />
          </span>
          <div class="lk-call-incoming-text">
            <strong>Group call in {internals.nameFor(ref().groupIdB64)}</strong>
            <span>End-to-end encrypted</span>
            <Show when={failure()?.callIdHex === ref().callIdHex ? failure()?.message : undefined}>
              {(message) => (
                <span class="lk-call-incoming-error" role="alert">
                  {message()}
                </span>
              )}
            </Show>
          </div>
          <div class="lk-call-incoming-actions">
            <button
              type="button"
              class="ghost"
              onClick={() => {
                setFailure(null);
                internals.dismissGroupCall(ref().callIdHex);
              }}
            >
              Dismiss
            </button>
            <A
              href={paths.chat(base64UrlEncodeNoPad(base64Decode(ref().groupIdB64)))}
              class="lk-button-link"
            >
              Open chat
            </A>
            <button type="button" disabled={joining()} onClick={() => void join(true)}>
              <Icon name="video" size={18} />
              Video
            </button>
            <button
              type="button"
              class="primary"
              disabled={joining()}
              onClick={() => void join(false)}
            >
              <Icon name="phone" size={18} />
              Join
            </button>
          </div>
        </div>
      )}
    </Show>
  );
}
