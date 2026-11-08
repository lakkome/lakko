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
  onMount,
  Show,
  untrack,
  useContext,
  type Accessor,
  type JSX,
} from "solid-js";
import { A } from "@solidjs/router";

import { captureGate } from "../platform/capture-gate";
import { cameraStartFailure } from "./capture-fallback";
import { composeAndUploadCallInvite } from "./composer";
import { rememberMintedLink } from "../contacts/minted-links";
import {
  CALL_INVITE_LABEL,
  callInviteMaxUses,
  CALL_INVITE_MEDIA_VIDEO,
  callInviteExpiryMs,
} from "./call-invite-values";
import {
  INVITE_IDLE,
  copyCallLink,
  mintedInviteFate,
  mintedInviteUrl,
  inviteBusy as inviteIsBusy,
  inviteStatusHoldMs,
  inviteStatusLine,
  nextInvitePress,
  type InCallInvite,
} from "./in-call-invite";
import { DiscoveryClient } from "../discovery/client";
import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { sendTextToConversation } from "../chat/send-text";
import { KeyedError } from "../l10n/keyed-error";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { useLineage } from "../chat/use-lineage";
import {
  CallUnavailableInSession,
  oneToOneCallEndsOnMove,
  oneToOneCallTarget,
} from "./call-continuity";
import { unrepairableBy } from "../chat/successor-claim";
import { loadSupersededKeys, subscribeAcceptedChanged } from "../contacts/local-store";
import {
  offerAgeMs,
  offerExpiresAtMs,
  ringRulesOf,
  ringVerdictFor,
  ringWindowOf,
} from "./ring-rule";
import {
  quietCallOffer,
  quietCallOfferIsLive,
  type QuietCallOffer,
} from "./quiet-call-offer";
import { ChatClient } from "../chat/client";
import { sealedWakeForGroup } from "../push/group-routing";
import {
  makeCallSignalingPublisher,
  makeGroupCallControlPublisher,
} from "../chat/call-signaling-publish";
import type { CallSignalingWake } from "./call-signaling-wake";
import { readIsActive } from "../chat/membership-ended";
import { memberKeysOf, peerName } from "../chat/peer-name";
import { ownPersonKeyB64, personKeysOf } from "../chat/person-key";
import { ContactsClient } from "../contacts/client";
import { conversationWithContact } from "../contacts/contact-conversation";
import { inviteContactToCall, type InviteOutcome } from "./invite-route";
import { sharedGroupsFor } from "../contacts/shared-groups";
import { heldGrantReaches } from "../search/wiring";
import { loadSentRequests, recordSentRequest } from "../contacts/sent-requests";
import { foundOneToOneWith } from "../contacts/found-one-to-one";
import { InviteContactsPicker } from "./InviteContactsPicker";
import { base64Decode, base64Encode, base64UrlEncodeNoPad } from "../auth/account-id";
import { loadAccepted } from "../contacts/local-store";
import { onGroupMlsIdle } from "../mls/group-lock";
import { callTileName, personNameOrNull } from "./tile-name";
import {
  ENDED_FOR_ALL,
  endedForEveryoneLine,
  endsTheCallForMe,
} from "./end-for-everyone";
import { useProfiles } from "../profile/ProfileContext";
import { memberNameOf } from "../profile/member-label";
import { useGroupNames } from "../chat/GroupNameContext";
import { conversationKindOf } from "../chat/conversation-kind";
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
import { RemoteAudioSinkController } from "./remote-audio-output";
import { classifyCallOutcome, type CallLogEntry } from "./call-log";
import { useCallLog } from "./CallLogContext";
import { useMembershipEnd } from "../chat/MembershipEndContext";
import { CallJwtClient, turnProviderForCall } from "./call-jwt-client";
import { groupPeerAnnouncer } from "./call-relay";
import {
  GROUP_CALL_ALONE_END_MS,
  decideGroupCallEpochChange,
  groupCallAloneAction,
  groupCallStartAbandoned,
  runGroupCallStart,
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
import { callBusyMessage, callSlotOccupant, type CallKind, type CallText } from "./call-slot";
import {
  decideLinkCallStart,
  linkCallNotice,
  linkCallOccupiesSlot,
  linkCallOverlayShown,
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
import {
  SFU_CALL_STATE_PROBE_MS,
  parseSfuCallState,
  sfuRoomStateUrl,
} from "./sfu-call-state";
import { callEndedAtMs } from "./sfu-keepalive";
import { createSfuCallUi, type SfuCallUiHandle } from "./sfu-call-ui";
import { OneToOneScreenShare } from "./one-to-one-screen-share";
import { currentShareStream, screenShareActions } from "./screen-share-store";
import { SCREEN_SHARE_NOTICE_MS } from "./screen-share-capability";
import { groupCallTileSources } from "./group-call-tile-sources";
import { sfuBaseUrl } from "./sfu-config";
import { useBackgroundReceive } from "../chat/BackgroundReceiveContext";
import { uiRules } from "../chat/ui-rules";
import { CallBar } from "./CallBar";
import { callBarWouldShow } from "./call-bar-model";
import { t } from "../l10n";
import { callFailureReason, callFailureText } from "./call-errors";
import { durableSnapshotHook, sessionAccountOf } from "../chat/persist-hooks";
import { OWN_AVATAR_KEY } from "../profile/avatar-runtime";

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
  /** The ringing call was offered with video. */
  readonly ringingOfferedVideo: Accessor<boolean>;
  /**
   * The muted conversation's call, offered quietly
   * (`docs/ux/calls.md` section 2.1.1), or `null`.
   *
   * Non-null exactly while a MUTED 1x1 call is ringing and still inside the
   * ring window. An unmuted call is `null` here — it takes the screen instead
   * (section 2.2), and a quiet copy of it beside the full screen would be the
   * interruption plus clutter.
   *
   * Two surfaces read it, each for its own conversation: the panel at the top
   * of that chat and the Accept / Decline on that chat's row in the
   * conversation list. Both go through `quietCallOfferFor`, so neither decides
   * for itself whether the call is theirs or whether it is still live.
   */
  readonly quietIncomingCall: Accessor<QuietCallOffer | null>;
  /**
   * Answer the ringing 1x1 call. No-op when nothing is ringing.
   *
   * THE accept path on this client. The full-screen incoming call, the
   * notification's Accept action and both of the muted conversation's quiet
   * surfaces all arrive here; nothing reaches the session directly. Three
   * surfaces each holding their own copy of "accept, and turn the camera on if
   * they asked for video" is how they come to disagree.
   */
  acceptIncomingCall(withVideo: boolean): Promise<void>;
  /** Refuse the ringing 1x1 call. No-op when nothing is ringing. THE decline
   *  path, for the same reason as {@link acceptIncomingCall}. */
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
  /** This account started the group call this device is in, so `End` offers
   *  to end it for everyone (`docs/ux/calls.md` section 4.1.2). */
  startedGroupCall(): boolean;
  /** End the group call this device is in for every person in it. Only acts
   *  for the call's starter. */
  endGroupCallForEveryone(): Promise<void>;
  /** End the link call for every person in it. Only acts for the link's
   *  owner. */
  endLinkCallForEveryone(): void;
  /** One line saying the call this device was in was ended for everyone, for
   *  a few seconds after it happened; `null` otherwise. */
  readonly endedForEveryoneNotice: Accessor<string | null>;
  /** The call occupying the one call slot, or `null` when it is free. */
  callOccupant(): CallKind | null;
  /** Why starting / joining a call now would be refused, or `null` when the
   *  slot is free. */
  callBusyMessage(verb: "start" | "join"): CallText | null;
  /** End whatever call occupies the slot (declines a ringing 1x1 call). */
  endCurrentCall(): Promise<void>;
  /** The call-link call in the host's link slot — occupying the slot, or kept
   *  after it ended so its outcome can be shown — or `null`. */
  readonly linkCall: Accessor<LinkCallSlot | null>;
  /** Found (owner) or join a call from a `/c/` link. Never rejects. */
  startLinkCall(opts: StartLinkCallOptions): Promise<LinkCallStartOutcome>;
  /** Leave the link call (or stop waiting to be admitted). */
  leaveLinkCall(): void;
  /**
   * `Copy link` for the link call in the slot
   * (`docs/ux/call-links.md` section 3).
   *
   * The first use in a given call mints the capability — a network round
   * trip — and later uses reuse the link. Never rejects, and never ends the
   * call: a failure becomes a sentence in {@link CallHostApi.linkCallInvite}.
   */
  copyLinkCallInvite(): Promise<void>;
  /** `Invite a contact`: whether the picker of conversations is open. */
  readonly linkCallInvitePickerOpen: Accessor<boolean>;
  openLinkCallInvitePicker(): void;
  closeLinkCallInvitePicker(): void;
  /** Send the invitation into the picked conversation. Rejects when it was
   *  not sent; the call is untouched either way. */
  sendLinkCallInvite(target: { readonly keyB64: string; readonly name: string }): Promise<InviteOutcome>;
  /** What `Invite` is doing for the call in the link slot. */
  readonly linkCallInvite: Accessor<InCallInvite>;
  /** The one line the call screen shows about it, or `null` once it has been
   *  up long enough to read. */
  readonly linkCallInviteStatus: Accessor<string | null>;
  /** Forget a link call's outcome once it has been shown. */
  dismissLinkCallOutcome(): void;
  /** The `/c/` route is showing the link call; the host does not repeat its
   *  status elsewhere meanwhile. Returns the release. */
  claimLinkCallView(): () => void;
  /**
   * The call in the slot has been put away into the bar across the top of the
   * app (`docs/ux/calls.md` section 4.5).
   *
   * A property of the CALL, not of a screen: it lives beside the session here
   * rather than in any route, so a navigation cannot touch it and the call
   * stays collapsed across every screen until the person brings it back. It
   * says nothing about whether the bar is on screen — that is the shared rule's
   * answer to this plus the phase (`calls/call-bar-model.ts`).
   */
  readonly callCollapsed: Accessor<boolean>;
  /** Put the call away without ending it. No-op when there is no call. */
  collapseCall(): void;
  /** Bring the call screen back. */
  expandCall(): void;
}

/** The call-link call in the host's link slot. */
export interface LinkCallSlot {
  /** Lowercase hex of the capability's 16-byte `call_id`. */
  readonly callIdHex: string;
  /** The `/c/<slug>` route the call was started from, to return to it. */
  readonly routePath: string;
  readonly role: "owner" | "joiner";
  readonly phase: LinkCallPhase;
  /** The reason, for `phase === "error"`, or `null` when there is no more
   *  specific one than "the call failed". */
  readonly errorMessage: CallText | null;
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
  /** The name typed on the link page (a guest): this person's name for the
   *  call (`docs/ux/calls.md` section 4.1), never written to the profile
   *  here. Absent: none typed. */
  readonly callName?: string;
}

export type LinkCallStartOutcome =
  | { readonly kind: "started" }
  | { readonly kind: "already-in-this-call" }
  | { readonly kind: "busy"; readonly message: CallText }
  | { readonly kind: "failed"; readonly message: CallText };

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
  | { readonly kind: "busy"; readonly message: CallText }
  /** The call to join is no longer live. */
  | { readonly kind: "ended" }
  /** The person left while the call was still connecting. Nothing failed
   *  and nothing is said. */
  | { readonly kind: "left" }
  | { readonly kind: "failed"; readonly message: CallText };

const CallHostContext = createContext<CallHostApi>();

const LIVE_STATES: readonly WebCallState[] = ["ringing", "outgoing", "connecting", "connected", "ending"];

/**
 * How often the muted conversation's quiet offer re-checks whether the ring
 * window has run out.
 *
 * A second is fine because nothing is waiting on it: the surfaces vanish on
 * their own when the call is answered, declined or gives up, and this only
 * covers the one case where the window simply expires. It is NOT a second copy
 * of the window — that number is `lakko_ui_rules::ring`'s and is never
 * restated here.
 */
const QUIET_OFFER_SWEEP_MS = 1_000;
/** How long "{name} ended the call for everyone" stays on screen. */
const ENDED_FOR_EVERYONE_NOTICE_MS = 6_000;

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
  const ownTileName = (): string =>
    profiles.ownProfile().displayName?.trim() || t("call.participant.you");
  /** The own profile name, `""` when none: the placeholder of the field
   *  where a name for the call is typed (section 4.1). */
  const ownProfileName = (): string => profiles.ownProfile().displayName?.trim() ?? "";
  const callLog = useCallLog();
  const membershipEnd = useMembershipEnd();
  const groupNames = useGroupNames();

  // Seals and publishes signaling through the locked, fail-closed-persisting
  // seam (`call-signaling-publish.ts`), like every other application message.
  const chat = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
    // ADR-0054 — a call offer and a call end ring a closed tab through the
    // `x-lakko-wake` header of their DS publish, which names the group's
    // opaque `push_gid` (ADR-0042). Without a resolver there is no wake at
    // all: the DS `group_id` is never put in the header in its place.
    wakeSealer: async (groupId, kind) => {
      const s = auth.state();
      if (s.kind !== "session") return null;
      return sealedWakeForGroup(s.account.accountId, base64Encode(groupId), kind);
    },
  });

  const [state, setState] = createSignal<WebCallState>("idle");
  const [callGroup, setCallGroup] = createSignal<string | null>(null);
  const [lastEnded, setLastEnded] = createSignal<EndedCallNotice | null>(null);
  // "{name} ended the call for everyone", shown for a few seconds to a person
  // whose call was ended by its starter (`docs/ux/calls.md` section 4.1.2).
  const [endedForEveryoneNotice, setEndedForEveryoneNotice] = createSignal<string | null>(null);
  let endedForEveryoneTimer: ReturnType<typeof setTimeout> | null = null;
  function showEndedForEveryone(starterName: string | null): void {
    showCallEndedLine(endedForEveryoneLine(starterName));
  }
  function showCallEndedLine(line: string): void {
    setEndedForEveryoneNotice(line);
    if (endedForEveryoneTimer !== null) clearTimeout(endedForEveryoneTimer);
    endedForEveryoneTimer = setTimeout(() => {
      endedForEveryoneTimer = null;
      setEndedForEveryoneNotice(null);
    }, ENDED_FOR_EVERYONE_NOTICE_MS);
  }
  onCleanup(() => {
    if (endedForEveryoneTimer !== null) clearTimeout(endedForEveryoneTimer);
  });
  /** The group call whose terminating `CallEnd` is already on the journal
   *  (the starter ended it for everyone), so leaving it publishes no second
   *  one. */
  let groupCallEndedForEveryone: string | null = null;
  const [remoteStream, setRemoteStream] = createSignal<MediaStream | null>(null);
  const [localStream, setLocalStream] = createSignal<MediaStream | null>(null);
  const [videoCall, setVideoCall] = createSignal(false);
  const [remoteHasVideo, setRemoteHasVideo] = createSignal(false);
  /** What the other person of the 1x1 call last said about its microphone
   *  and camera (the `mediaState` call signal, 2026-11-07); unmuted and on
   *  until the first one arrives. */
  const [remoteMicOn, setRemoteMicOn] = createSignal(true);
  const [remoteCameraOn, setRemoteCameraOn] = createSignal(true);
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
  /** The link call's raw 16-byte `call_id`. Kept beside the slot (which holds
   *  only the hex form, for comparison) because a link minted mid-call has to
   *  carry the exact bytes the call is running under — `docs/ux/call-links.md`
   *  section 3: "the same `call_id` the people already in the call are using,
   *  so the person who follows it arrives in THIS call and not a new one". */
  const [linkCallId, setLinkCallId] = createSignal<Uint8Array | null>(null);
  /** What `Invite` is doing for the call in the link slot. Reset per call:
   *  a link belongs to one `call_id` and must never outlive it. */
  const [linkInvite, setLinkInvite] = createSignal<InCallInvite>(INVITE_IDLE);
  /**
   * Whether the call screen is currently showing a line about it.
   *
   * The line and the link are two different lifetimes, and conflating them is
   * what would make a second `Invite` pay for a second round trip: the line
   * goes quiet after its hold, while the minted link stays in `linkInvite`
   * until the call ends.
   */
  const [inviteLineShown, setInviteLineShown] = createSignal(false);
  /** The link call's orchestrator, while it holds a session. */
  const [linkHandle, setLinkHandle] = createSignal<MultiJoinerCall | null>(null);
  /** How many `/c/` route views are showing the link call. */
  const [linkViewClaims, setLinkViewClaims] = createSignal(0);

  /**
   * The call has been put away into the bar (`docs/ux/calls.md` section 4.5).
   *
   * One flag for the one call slot, whichever kind occupies it: a 1x1 call, a
   * group call and a call-link call are never live at the same time
   * (`call-slot.ts`), so three flags would be three ways to describe one call's
   * presentation and would have to be kept in step.
   *
   * Reset by the slot-change effect below rather than by each start path, so
   * a call is never handed to someone who put the previous one away.
   */
  const [collapsed, setCollapsed] = createSignal(false);

  /**
   * `docs/ux/calls.md` section 2.1 — "a muted chat never rings".
   *
   * The mute flag is per conversation, travels between a person's devices on
   * the DeviceGroup journal, and is the same one the conversation list draws
   * (`devices/sync/state.ts` `FlagsState`). Reading it here is the whole of
   * what was missing: it was stored, synced and drawn, and no ring path
   * consulted it.
   */
  const sync = useDeviceGroupSync();
  // ADR-0073 R.5 — mute is the CONVERSATION's, whichever of its groups a
  // call arrives in.
  const lineageView = useLineage();

  /**
   * The ringing 1x1 call is muted: it happens and it is recorded, but it does
   * not interrupt (no ringtone, no vibration, no screen that takes over the
   * tab). Read by {@link CallHostView}.
   */
  const [silentRing, setSilentRing] = createSignal(false);

  /**
   * That same muted call, as the two quiet surfaces of `docs/ux/calls.md`
   * section 2.1.1 need to see it: the panel inside the conversation and the
   * Accept / Decline on its row in the conversation list.
   *
   * Deliberately a SECOND signal rather than something {@link silentRing} is
   * derived from, and the coupling that would create is the reason. This one
   * is bounded by the ring window and goes empty when the window runs out;
   * `silentRing` must stay true for as long as the call is ringing, because it
   * is what withholds the ringtone. Folding the two together would mean that
   * the instant the quiet offer expired, a muted call still in its session's
   * own ring timer would start ringing out loud and throw the full screen up —
   * the exact interruption mute exists to withhold, arriving at the end of the
   * window instead of at the start.
   */
  const [quietOffer, setQuietOffer] = createSignal<QuietCallOffer | null>(null);

  /**
   * The quiet offer takes itself down when the ring window runs out, without
   * anyone touching anything (section 2.1.1: "Both disappear when the ring
   * window runs out").
   *
   * A repeating check rather than one timer set for the deadline, for two
   * reasons that both end in a dead Accept button left on screen: a background
   * tab's timers are throttled hard, so a single timeout armed tens of seconds
   * out can land well after its moment; and a system clock that jumps forward
   * moves the deadline into the past without moving the timeout. A check that
   * re-reads the clock answers both, and it runs only while a muted call is
   * actually being offered.
   *
   * It is not the only guard: {@link CallHostApi.quietIncomingCall} compares
   * the deadline again when it is read, so a tick that has not landed yet
   * cannot make a surface offer a call the caller has given up on.
   */
  createEffect(() => {
    const offer = quietOffer();
    if (offer === null) return;
    const tick = setInterval(() => {
      if (!quietCallOfferIsLive(offer, Date.now())) setQuietOffer(null);
    }, QUIET_OFFER_SWEEP_MS);
    onCleanup(() => clearInterval(tick));
  });

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
    /** This device pressed Accept: a call that then never connected failed,
     *  it was not missed. */
    accepted: boolean;
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
      outcome: classifyCallOutcome({ incoming: j.incoming, connected, accepted: j.accepted, reason }),
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
      accepted: false,
      recordedWithReason: false,
    };
  }

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  /**
   * Is this conversation muted?
   *
   * Reads the synced per-conversation flag the chat list draws. Unknown — no
   * flags row for the group yet, a group id that does not decode — is NOT
   * muted, and that direction is deliberate: failing the other way would
   * silence a call because a local store had not filled in yet, which is a
   * far worse defect than an unwanted ring.
   */
  function isMuted(groupIdB64: string): boolean {
    try {
      return sync.getFlags(lineageView.conversationIdOf(base64Decode(groupIdB64)))?.muted === true;
    } catch {
      return false;
    }
  }

  const inCall = (): boolean => LIVE_STATES.includes(state());

  function groupEntry(groupIdB64: string) {
    return chats.get(groupIdB64);
  }

  function nameFor(groupIdB64: string | null): string {
    if (groupIdB64 === null) return t("chat.peer.fallback_name");
    const entry = groupEntry(groupIdB64);
    if (entry === undefined) return t("chat.peer.fallback_name");
    // The person key, as the chat header uses: member keys name people, and
    // the device key never matched one, so the call title named this person
    // beside the other one ("A, B").
    const ownKeyB64 = ownPersonKeyB64(chats.identity());
    const memberKeysB64 = memberKeysOf(entry.group);
    // The same title the chat header and the list show (ADR-0051 §5): the
    // kind the conversation carries (ADR-0062) decides group-vs-1x1, the
    // shared name comes first.
    const isGroup = conversationKindOf(entry.group) === "group";
    return peerName({
      ownKeyB64,
      memberKeysB64,
      resolveName: (k) => profiles.resolve(k).name,
      linkedContactName: loadAccepted().find((c) => c.groupIdB64 === groupIdB64)?.displayName,
      label: groupTitle(
        groupNames.nameOf(groupIdB64) ?? entry.label,
        memberKeysB64
          .filter((k) => k !== ownKeyB64)
          .map((k) => memberNameOf(profiles.resolve(k), "the call roster")),
      ),
      isGroup,
    });
  }

  /** ADR-0084 - the person a one-to-one call is with (their profile key),
   *  whose photo the incoming-call screen shows; `null` for a group. */
  function personKeyFor(groupIdB64: string | null): string | null {
    if (groupIdB64 === null) return null;
    const entry = groupEntry(groupIdB64);
    if (entry === undefined || conversationKindOf(entry.group) === "group") return null;
    const ownKeyB64 = ownPersonKeyB64(chats.identity());
    return memberKeysOf(entry.group).find((k) => k !== ownKeyB64) ?? null;
  }

  function publisherFor(
    groupIdB64: string,
  ): (bytes: Uint8Array, wake: CallSignalingWake) => Promise<void> {
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
    displayName: t("call.participant.you"),
    isLocal: true,
    // A call placed from the camera check can start muted (docs/ux/calls.md
    // section 3.2); every one of these constructors used to hardcode `true`,
    // so the mute survived exactly until the session reported its first state.
    micOn: callUi.pendingMicOn(),
    camOn: videoCall(),
    quality: qualityTierForRtt(connStats()?.rttMs ?? null),
    speaking: false,
    personKeyB64: OWN_AVATAR_KEY,
  });

  const remoteParticipant = (): Participant => ({
    id: "peer",
    displayName: nameFor(callGroup()),
    isLocal: false,
    micOn: remoteMicOn(),
    camOn: remoteHasVideo() && remoteCameraOn(),
    quality: "good",
    speaking: false,
    personKeyB64: personKeyFor(callGroup()) ?? undefined,
  });

  /** The safety number of the live 1x1 call, derived once when its key is
   *  armed. Display-only: a derivation failure never affects the call. */
  let oneToOneSafety: string | null = null;

  /**
   * The screen share of the live 1x1 call (ADR-0083 section 4), built once
   * the call connects, with the session it belongs to. A share stream that
   * arrives first is held until it is built.
   */
  let oneToOneShare: { readonly session: CallSession; readonly share: OneToOneScreenShare } | null =
    null;
  let heldOneToOneScreen: {
    readonly session: CallSession;
    readonly stream: MediaStream;
    readonly track: MediaStreamTrack;
  } | null = null;

  function startOneToOneShare(session: CallSession, groupIdB64: string, callId: Uint8Array): void {
    if (oneToOneShare?.session === session) {
      oneToOneShare.share.onConnected();
      return;
    }
    void loadWasm()
      .then((wasm) => {
        // Ended, or replaced, while the module resolved.
        if (activeCall !== session || slotSession !== session) return;
        if (oneToOneShare?.session === session) return;
        const identity = chats.identity();
        if (identity === null) return;
        let share: OneToOneScreenShare;
        try {
          share = new OneToOneScreenShare({
            wasm,
            identity,
            callId,
            group: () => groupEntry(groupIdB64)?.group ?? null,
            session: {
              attachScreenShare: (capture) => session.attachScreenShare(capture),
              detachScreenShare: () => session.detachScreenShare(),
            },
            publish: (signal) => {
              // Screen-share state belongs to a connected call: never a wake.
              void publisherFor(groupIdB64)(signal, "none");
            },
            nameOf: (personKeyB64) => callNameOfPerson(personKeyB64) ?? "",
            notice: (text) => callActions.showNotice(text, SCREEN_SHARE_NOTICE_MS),
            store: screenShareActions,
          });
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("1x1 screen share unavailable", err);
          return;
        }
        oneToOneShare = { session, share };
        const held = heldOneToOneScreen;
        heldOneToOneScreen = null;
        if (held !== null && held.session === session) share.onRemoteTrack(held.track);
        if (session.getState() === "connected") share.onConnected();
      })
      .catch(() => {
        // Without the core nothing is shared; the call goes on.
      });
  }

  function endOneToOneShare(session: CallSession): void {
    if (heldOneToOneScreen?.session === session) heldOneToOneScreen = null;
    if (oneToOneShare?.session !== session) return;
    oneToOneShare.share.dispose();
    oneToOneShare = null;
    screenShareActions.reset();
  }

  /**
   * Say whether this 1x1 call's media is sealed, and show its safety number
   * when it is.
   *
   * This used to publish `active: false` unconditionally with a comment that
   * the 1x1 web path was P2P DTLS-SRTP and had no safety number. That was
   * true until the path gained SFrame; leaving it would now have the chip
   * denying an encryption that is actually running, which is the same class of
   * lie as claiming one that is not.
   */
  function publishOneToOneE2ee(): void {
    const armed = activeCall?.hasCallKey() === true;
    callActions.setE2ee({
      active: armed,
      shortFingerprint: armed ? oneToOneSafety : null,
    });
  }

  function syncCallUiFromState(s: WebCallState): void {
    switch (s) {
      // `docs/ux/calls.md` §4's state table separates these two, and the store
      // used to collapse them into one: a call ringing on the other person's
      // phone said `Connecting...`, which described the network instead of the
      // call. `outgoing` is the caller waiting to be answered — `Calling...`.
      case "outgoing":
        callActions.setPhase({ kind: "calling" });
        callActions.setParticipants([localParticipant()]);
        publishOneToOneE2ee();
        break;
      case "connecting":
        callActions.setPhase({ kind: "connecting" });
        callActions.setParticipants([localParticipant()]);
        publishOneToOneE2ee();
        break;
      case "connected":
        callActions.setPhase({ kind: "connected" });
        callActions.setParticipants([localParticipant(), remoteParticipant()]);
        publishOneToOneE2ee();
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

  /**
   * The per-call SFrame key for a 1x1 call, or `null` when this device cannot
   * derive one (no live identity, or the chat is not a group this device is
   * in). A `null` leaves the session unarmed, which makes it refuse before it
   * opens the microphone rather than send media unsealed.
   *
   * The same exporter label, call id and leaf the natives use, so both ends of
   * a 1x1 call arrive at identical bytes (audit-pack C-1).
   */
  function oneToOneCallKey(
    groupIdB64: string,
    callId: Uint8Array,
  ): { seed: Uint8Array; localLeaf: number } | null {
    const id = chats.identity();
    const entry = groupEntry(groupIdB64);
    if (id === null || entry === undefined) return null;
    try {
      return {
        seed: entry.group.exportSecret(id, "lakko/call/v1", callId, 32),
        localLeaf: entry.group.ownLeafIndex,
      };
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("1x1 call key derivation failed; the call will refuse", err);
      return null;
    }
  }

  /**
   * Build the session for one 1x1 call.
   *
   * `ringWindowMs` is the shared rule's window (`ring-rule.ts`), read from
   * wasm by the caller — which has already awaited it to parse the frame or
   * to dial. It is passed rather than defaulted because it is the one timing
   * number the OTHER device also has to be using: if the two sides disagree,
   * one files a missed call while the other is still ringing.
   */
  function buildCallSession(
    groupIdB64: string,
    callId: Uint8Array,
    ringWindowMs: number,
  ): CallSession {
    const session: CallSession = new CallSession(callId, {
      // Each of these, like `onStateChange` below, is dropped unless it comes
      // from the session that holds (or last held) the slot: a session that
      // was replaced can still finish an await and report.
      onRemoteStream: (s) => {
        if (slotSession !== session) return;
        setRemoteStream(s);
        watchRemoteVideo(s, setRemoteHasVideo);
      },
      onLocalStream: (s) => {
        if (slotSession !== session) return;
        setLocalStream(s);
      },
      onRemoteScreenStream: (stream, track) => {
        if (slotSession !== session) return;
        if (oneToOneShare?.session === session) oneToOneShare.share.onRemoteTrack(track);
        else heldOneToOneScreen = { session, stream, track };
      },
      onVideoPublishState: (s) => {
        if (slotSession !== session) return;
        setVideoPublishState(s);
      },
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
        if (s === "connected") startOneToOneShare(session, groupIdB64, callId);
        if (s === "done") {
          endOneToOneShare(session);
          if (callJournal?.session === session && !session.neverPlaced()) recordOneToOneCall(reason);
          endedCallIds.add(callIdHex(callId));
          activeCall = null;
          setSilentRing(false);
          setQuietOffer(null);
          if (reason !== undefined && !session.neverPlaced()) setLastEnded({ groupIdB64, reason });
          setRemoteStream(null);
          setLocalStream(null);
          setVideoCall(false);
          setRemoteHasVideo(false);
          setRemoteMicOn(true);
          setRemoteCameraOn(true);
          setRemoteWantsVideo(false);
          setVideoPublishState("off");
          setConnStats(null);
        }
      },
      onConnectionStats: (stats) => {
        // Only for the live call: a sample read by a session that has ended
        // would be written onto whatever call is current.
        if (activeCall !== session) return;
        setConnStats(stats);
        callActions.setConnectionDetail({
          rttMs: stats.rttMs,
          downBps: stats.downBps,
          upBps: stats.upBps,
          connectionType: stats.relayed ? "relay" : stats.rttMs !== null ? "direct" : "unknown",
        });
        callActions.setQuality("local", qualityTierForRtt(stats.rttMs));
      },
    }, { ringTimeoutMs: ringWindowMs });
    // Armed before anything opens a device: an unarmed session refuses rather
    // than putting unsealed media on the wire.
    const key = oneToOneCallKey(groupIdB64, callId);
    oneToOneSafety = null;
    if (key !== null) {
      session.setCallKey(key.seed, key.localLeaf);
      // Display-only, and deliberately not awaited: the call must not wait on
      // a string, and a failure here leaves the chip without digits rather
      // than affecting the media.
      void loadWasm()
        .then((wasm) => {
          oneToOneSafety = wasm.callSafetyNumber(key.seed);
          if (activeCall === session) publishOneToOneE2ee();
        })
        .catch(() => {
          oneToOneSafety = null;
        });
    }
    // ADR-0078 section 2: the TURN credential comes from the same anonymous
    // exchange as an SFU call token (one `call-join` token, the room of this
    // call id). Without it a 1x1 call resolves STUN-only ICE, and two peers
    // that cannot reach each other directly never connect.
    session.setTurnProvider(
      turnProviderForCall(
        {
          mintForCall: async (id, role) => new CallJwtClient(await loadWasm()).mintForCall(id, role),
        },
        callId,
      ),
    );
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
      // One reading of the clock, because two questions are asked about this
      // row — may it interrupt, and how long is it still offered for — and two
      // readings taken a few milliseconds apart could answer them
      // inconsistently about one offer. The age itself is derived twice, here
      // and inside `ringVerdictFor`, which is safe precisely because
      // `offerAgeMs` is pure and both derivations are given the same two
      // values: the alternative was a second copy of its unreadable-timestamp
      // rule living here.
      const nowMs = Date.now();
      const ageMs = offerAgeMs(item.row.createdAt, nowMs);
      const decision = decideInboundSignaling({
        route: routeSignalingKind(parsed.kind),
        kind: parsed.kind,
        callIdHex: hex,
        groupIdB64: item.groupIdB64,
        // One question, one answer, in Rust: is this offer still live, and
        // may it interrupt? Read for every row, used only for an offer.
        ring: ringVerdictFor(ringRulesOf(wasm), {
          muted: isMuted(item.groupIdB64),
          rowCreatedAt: item.row.createdAt,
          nowMs,
        }),
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
        // Read before the record is closed: who started this call.
        const starter = groupCallsFor(account, item.groupIdB64).get(hex)?.initiatorPersonKey ?? null;
        applyGroupCallEventFor(account, item.groupIdB64, { kind: "end", callIdHex: hex }, Date.now());
        // The starter ended the call for everyone: a device that is in it
        // leaves. The same body from anyone else closes the record and takes
        // nobody out of a call (`end-for-everyone.ts`).
        const g = activeGroupCall;
        if (
          g !== null &&
          g.callIdHex === hex &&
          g.groupIdB64 === item.groupIdB64 &&
          endsTheCallForMe({
            endReason: parsed.endReason,
            senderPersonKey: item.row.senderPersonKey ?? item.row.senderMemberKey,
            starterPersonKey: starter,
          })
        ) {
          groupCallEndedForEveryone = hex;
          showEndedForEveryone(starter === null ? null : callNameOfPerson(starter));
          // The ended-phase effect leaves the call.
          callActions.endCall();
        }
      }

      // The other person's screen-share state (ADR-0083 section 4): for the
      // call in the slot only, verified against the MLS sender's own leaf.
      if (parsed.kind === "screenShare") {
        const live = oneToOneShare;
        if (decision.oneToOne === "apply" && live !== null && activeCall === live.session) {
          let senderKey: Uint8Array | null = null;
          try {
            senderKey =
              item.row.senderMemberKey === undefined ? null : base64Decode(item.row.senderMemberKey);
          } catch {
            senderKey = null;
          }
          live.share.receive(parsed.screenShareState, senderKey);
        }
        return;
      }

      // The other person's camera and microphone state (2026-11-07): for the
      // call in the slot only. It is the MLS sender's word about its own
      // devices, so it only ever names the remote tile.
      if (parsed.kind === "mediaState") {
        if (decision.oneToOne === "apply" && activeCall !== null) {
          setRemoteMicOn(parsed.microphoneOn);
          setRemoteCameraOn(parsed.cameraOn);
        }
        return;
      }

      switch (decision.oneToOne) {
        case "drop":
          return;
        case "busy":
          endedCallIds.add(hex);
          // An End for a call this device never took: it stops the ring the
          // caller started, so it keeps its wake like any unanswered call's End.
          await publisherFor(item.groupIdB64)(
            wasm.composeCallEndSignaling(parsed.callId, "busy"),
            "call",
          );
          return;
        case "ring":
        case "silent": {
          // The two arms differ in exactly one thing: whether the call is
          // allowed to interrupt. Everything else — the session, the slot,
          // the journal that writes the call-log row when it ends — is the
          // same, because a muted call is still a call.
          const session = buildCallSession(
            item.groupIdB64,
            parsed.callId,
            Number(wasm.callRingWindowMs()),
          );
          openCallJournal(session, item.groupIdB64, hex, true, parsed.mediaVideo);
          occupy(item.groupIdB64, session);
          setSilentRing(decision.oneToOne === "silent");
          // The two quiet surfaces of section 2.1.1. The deadline comes from
          // what is LEFT of the shared window rather than from the whole of
          // it: a `silent` offer can already be most of the window old, and a
          // surface counting the full window from arrival would go on offering
          // an answer after the caller had given up.
          setQuietOffer(
            decision.oneToOne === "silent" && ageMs !== null
              ? {
                  groupIdB64: item.groupIdB64,
                  callIdHex: hex,
                  video: parsed.mediaVideo,
                  expiresAtMs: offerExpiresAtMs(ringWindowOf(wasm), nowMs, ageMs),
                }
              : null,
          );
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
            // The MLS sender resolved at receipt, never the leaf the body
            // names (C-68): this is who may end the call for everyone.
            initiatorPersonKey: row.senderPersonKey ?? row.senderMemberKey ?? null,
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

  /** `successor-claim.ts` `unrepairableBy` over a group this device holds:
   *  the superseded account that makes it a dead session, else `null` (also
   *  for a group whose roster cannot be read). */
  function unrepairableKeyOfGroup(groupIdB64: string): string | null {
    const entry = chats.get(groupIdB64);
    if (entry === undefined) return null;
    try {
      const owner = entry.group.roleTableOwner() as Uint8Array | undefined;
      return unrepairableBy({
        kind: conversationKindOf(entry.group),
        rosterPersonKeysB64: personKeysOf(entry.group),
        ownPersonKeyB64: ownPersonKeyB64(chats.identity()),
        ownerKeyB64: owner === undefined ? undefined : base64Encode(owner),
        supersededKeys: loadSupersededKeys(),
      });
    } catch {
      return null;
    }
  }

  async function dial(askedGroupIdB64: string, askedVideo: boolean): Promise<void> {
    if (currentOccupant() !== null) return;
    if (groupEntry(askedGroupIdB64) === undefined) return;
    // The refusal screen of the desktop; the gate can sit there for as long
    // as the person takes, so the slot is read again after it.
    const permits = await captureGate({ audio: true, video: askedVideo });
    if (!permits.microphone || currentOccupant() !== null) return;
    const withVideo = askedVideo && permits.camera;
    // ADR-0073 R.12 — never signal into a dead session: the call is placed
    // in the conversation's successor when this device holds one, and
    // otherwise does not start (the caller shows its ordinary
    // could-not-start state; no copy of its own).
    const headGroupIdB64 = lineageView.resolve(askedGroupIdB64)?.head.groupIdB64 ?? null;
    const target = oneToOneCallTarget({
      groupIdB64: askedGroupIdB64,
      unrepairableKeyB64: unrepairableKeyOfGroup(askedGroupIdB64),
      headGroupIdB64,
      headUnrepairableKeyB64:
        headGroupIdB64 === null || headGroupIdB64 === askedGroupIdB64
          ? null
          : unrepairableKeyOfGroup(headGroupIdB64),
    });
    if (target.kind === "unavailable") throw new CallUnavailableInSession();
    const groupIdB64 = target.groupIdB64;
    if (groupEntry(groupIdB64) === undefined) return;
    setVideoCall(withVideo);
    // The window the callee will ring for, so both sides give up together.
    const wasm = await loadWasm();
    const callId = mintCallId();
    const session = buildCallSession(groupIdB64, callId, Number(wasm.callRingWindowMs()));
    // A call this device placed is never silent: mute is about being
    // interrupted, and nobody is interrupting anybody here. So there is
    // nothing to offer quietly either.
    setSilentRing(false);
    setQuietOffer(null);
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
      // A later call has the slot: this one was already over, and neither its
      // failure nor its video flag belongs to the call that replaced it.
      if (slotSession !== session) return;
      // A call that never rang anywhere leaves no trace: no row in the
      // conversation and no "last call ended" line beside the one sentence
      // the caller shows for this failure.
      if (callJournal?.session === session) callJournal = null;
      setLastEnded(null);
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

  async function accept(askedVideo: boolean): Promise<void> {
    const call = activeCall;
    if (call === null) return;
    const permits = await captureGate({ audio: true, video: askedVideo });
    // Refused with the ring still on: the person can still decline, or come
    // back to Accept. It may also have stopped ringing while the screen was up.
    if (!permits.microphone || activeCall !== call || call.getState() !== "ringing") return;
    const withVideo = askedVideo && permits.camera;
    if (withVideo) setVideoCall(true);
    if (callJournal?.session === call) callJournal.accepted = true;
    await call.accept();
    // Opting into video on accept drives the same mid-call camera-on path the
    // caller uses (see the camera effect below). Only for the call that was
    // accepted: it may have ended, or been replaced, while the answer was made.
    if (withVideo && activeCall === call) callActions.setCamera(true);
  }

  async function decline(): Promise<void> {
    await activeCall?.decline();
  }

  async function hangUp(): Promise<void> {
    await activeCall?.hangUp("hungUp");
  }

  // ADR-0073 R.12 / UX 16.4 — a live one-to-one call ends when its
  // conversation moves: once the other person's reset is accepted the media
  // is going to a removed device. It ends as a call ends when the other side
  // drops; nothing is said (R.0). The decision is `call-continuity.ts`.
  const [contactsVersion, setContactsVersion] = createSignal(0);
  const unsubscribeContacts = subscribeAcceptedChanged(() => setContactsVersion((v) => v + 1));
  onCleanup(unsubscribeContacts);
  createEffect(() => {
    contactsVersion();
    const groupIdB64 = inCall() ? callGroup() : null;
    if (groupIdB64 === null) return;
    if (chats.get(groupIdB64) === undefined) return;
    const ends = oneToOneCallEndsOnMove({
      callGroupIdB64: groupIdB64,
      unrepairableKeyB64: unrepairableKeyOfGroup(groupIdB64),
      headGroupIdB64: lineageView.resolve(groupIdB64)?.head.groupIdB64 ?? null,
    });
    if (ends) untrack(() => void activeCall?.hangUp("hungUp"));
  });

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
  // toggle AND the call's own "this is a video call" flag, and says one line.
  // Reverting the toggle alone does not hold: `localParticipant()` reads
  // `camOn` from `videoCall()`, so the next roster rebuild put it straight
  // back and the failed capture was retried forever (see `cameraStartFailure`,
  // which owns that rule and the copy).
  createEffect(() => {
    const local = callUi.participants().find((p) => p.isLocal);
    if (local === undefined || activeCall === null) return;
    if (local.camOn) {
      const call = activeCall;
      void call.addLocalVideo().catch((err: unknown) => {
        // The call ended, or another took its place, while the camera was
        // starting: the failure is not the current call's to show.
        if (activeCall !== call) return;
        const outcome = cameraStartFailure(err);
        setVideoCall(outcome.videoCall);
        callActions.setCamera(outcome.camOn);
        callActions.showNotice(outcome.notice);
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
    // A picture only while the track is there AND the person has not said the
    // camera is off: a camera turned off is `track.enabled = false`, which
    // the track itself never reports.
    callActions.setParticipantCamera("peer", remoteHasVideo() && remoteCameraOn());
  });

  // The other person's mute mark (group-call-grid 7.2), from what they said.
  createEffect(() => {
    if (state() !== "connected") return;
    callActions.setParticipantMic("peer", remoteMicOn());
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
  /** The start the group slot belongs to while it has no session yet: a
   *  joiner's group sync, the JWT mint and the Start publish all run before
   *  there is anything to hang up. Leaving clears it (`releaseGroupSlot`),
   *  and a start that finds it changed after an await builds nothing
   *  (`runGroupCallStart`). The same shape as `linkAttempt`. */
  let groupAttempt: object | null = null;

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
        // No group-call frame wakes anyone, an End included: a group call
        // never rings a closed device, so there is no ring to stop.
        await makeGroupCallControlPublisher({
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
    // Before the latch below: a group call goes on whatever the answer is,
    // since a person with no microphone can still listen and watch
    // (`capture-fallback.ts`).
    await captureGate({ audio: true, video: opts.withVideo });
    {
      // The slot is the latch: it is written below with no await in between,
      // so a double tap finds it taken.
      const occupant = currentOccupant();
      if (occupant !== null) {
        return { kind: "busy", message: callBusyMessage(occupant, opts.joinCallIdHex === null ? "start" : "join") };
      }
    }
    const initiator = opts.joinCallIdHex === null;
    const couldNot: CallText = () =>
      t(initiator ? "call.error.start_group" : "call.error.join_group");
    let callId: Uint8Array;
    if (opts.joinCallIdHex === null) {
      callId = mintCallId();
    } else {
      const parsed = hexToBytes(opts.joinCallIdHex);
      if (parsed === null || parsed.length !== 16) {
        return { kind: "failed", message: couldNot };
      }
      callId = parsed;
    }
    const groupIdB64 = opts.groupIdB64;
    const hex = callIdHex(callId);
    const attempt = {};
    groupAttempt = attempt;
    // One call at a time, app-wide: from here on a 1x1 offer from any chat is
    // answered busy, and the receive paths keep this group polled.
    setGroupSlot({ groupIdB64, callIdHex: hex, starting: true });
    // The session this start builds, once it exists: what the outcome is
    // read from when the start does not end connected.
    let started: SfuCallUiHandle | null = null;
    /** What the awaits before the session produce. */
    interface StartMaterial {
      readonly base: string;
      readonly id: NonNullable<ReturnType<typeof chats.identity>>;
      readonly wasm: Awaited<ReturnType<typeof loadWasm>>;
      readonly seed: Uint8Array;
      readonly localLeaf: number;
      readonly keyedEpoch: bigint | null;
      readonly safetyNumber: string | undefined;
      readonly jwt: string;
      /** The TURN credential minted beside the token (ADR-0078 section 2). */
      readonly turn: RTCIceServer | null;
      /** When this device asked for the call. */
      readonly startedAtMs: number;
    }
    const callOf = (m: StartMaterial): ActiveGroupCall => ({
      groupIdB64,
      callId,
      callIdHex: hex,
      ownLeaf: m.localLeaf,
      ownMemberKey: base64Encode(m.id.signaturePublicKey),
      ownPersonKey: ownPersonKeyB64(m.id) ?? base64Encode(m.id.signaturePublicKey),
      initiator,
      joinPublished: false,
      heartbeat: null,
      keyedEpoch: m.keyedEpoch,
      video: opts.withVideo,
      startedAtMs: m.startedAtMs,
      connectedAtMs: null,
      sawRemote: false,
      aloneTimer: null,
    });
    const sessionEnded = (): GroupCallStartOutcome => ({
      kind: "failed",
      message: () => t("common.error.session_ended"),
    });
    try {
      const run = await runGroupCallStart<StartMaterial, GroupCallStartOutcome>({
        initiator,
        isCurrent: () => groupAttempt === attempt,
        sync: async () => {
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
        },
        callStillLive: () => {
          const account = accountId();
          const still = account === null ? undefined : groupCallsFor(account, groupIdB64).get(hex);
          // Not in this call yet: this device's own stale entry (a session that
          // died before a reload) must not make a finished call look joinable.
          const ownId = chats.identity();
          const viewer: GroupCallViewer | undefined =
            ownId === null
              ? undefined
              : { ownMemberKey: base64Encode(ownId.signaturePublicKey), inCallIdHex: null };
          return still !== undefined && isGroupCallLive(still, Date.now(), viewer);
        },
        prepare: async () => {
          const base = sfuBaseUrl();
          if (base === null) {
            return {
              ok: false,
              // No SFU endpoint in this build (`VITE_LAKKO_SFU_URL`).
              reason: { kind: "failed", message: () => t("chat.call.unavailable") },
            };
          }
          const id = chats.identity();
          const secrets = auth.identitySecrets();
          const entry = groupEntry(groupIdB64);
          if (id === null || secrets === null) return { ok: false, reason: sessionEnded() };
          if (entry === undefined || readIsActive(entry.group) === false) {
            return {
              ok: false,
              reason: { kind: "failed", message: () => t("call.error.not_member") },
            };
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
          // Call-admission JWT (JWS Compact, EdDSA), paid for by one anonymous
          // `call-join` token (ADR-0078 section 2): it names the room and a
          // random peer id, never the account.
          const minted = await new CallJwtClient(wasm).mintForCall(callId, "participant");
          return {
            ok: true,
            material: {
              base,
              id,
              wasm,
              seed,
              localLeaf,
              keyedEpoch,
              safetyNumber,
              jwt: minted.jwt,
              turn: minted.turn,
              startedAtMs: Date.now(),
            },
          };
        },
        // Announce before connecting, so a member who opens the chat while the
        // SFU handshake is still running already sees a call to join.
        announce: (m) =>
          publishGroupControl(groupIdB64, (w) =>
            w.composeGroupCallStartSignaling(callId, m.localLeaf, BigInt(m.startedAtMs), true, opts.withVideo),
          ),
        // The Start is on the journal and no call follows it: tell the group
        // what leaving tells it (the End an initiator owes), through the same
        // ordered queue, so the End lands behind the Start.
        takeBackAnnouncement: (m, why) => {
          void announceGroupLeave(callOf(m), why === "left" ? "hungUp" : "error", false, Date.now());
        },
        open: (m) => {
          // Set before anything else, so a failure from here on reaches
          // `leaveGroupCall` — which is what publishes the End an initiator owes.
          activeGroupCall = callOf(m);
          if (initiator) {
            // Our own publish never comes back to us (the poll drops the echo), so
            // the record is updated here as it would be on every other member.
            applyGroupCallEvent(groupIdB64, {
              kind: "start",
              callIdHex: hex,
              initiatorLeaf: m.localLeaf,
              initiatorPersonKey: ownPersonKeyB64(m.id) ?? base64Encode(m.id.signaturePublicKey),
              createdAtMs: m.startedAtMs,
              mediaAudio: true,
              mediaVideo: opts.withVideo,
              observedAtMs: m.startedAtMs,
            });
          }
          // ADR-0078 section 2 — the SFU names nobody. Each peer announces
          // its own SFU peer id, signed with its MLS leaf key and sealed
          // under the call seed; the announcement is verified against the
          // chat group's roster at the current epoch, and the tile names the
          // person the leaf belongs to through the one resolver.
          const announcer = groupPeerAnnouncer(
            m.wasm,
            m.id,
            callId,
            () => groupEntry(groupIdB64)?.group ?? null,
          );
          const handle = createSfuCallUi(
            {
              sfuBaseUrl: m.base,
              jwt: m.jwt,
              seed: m.seed,
              localLeaf: m.localLeaf,
              peerAnnouncer: announcer,
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
              callIdHex: hex,
              profileName: ownProfileName(),
              safetyNumber: m.safetyNumber,
              resolveDisplayName: callNameOfPerson,
              onSessionStateChange: (state) => onGroupSessionState(hex, state),
            },
          );
          // The SFU has a public address, so most clients reach it on a
          // server-reflexive candidate - but one behind a symmetric NAT has none,
          // and for it the relay is the only way into the room.
          handle.session.setTurnProvider(async () => m.turn);
          started = handle;
          setGroupHandle(handle);
          setGroupSlot({ groupIdB64, callIdHex: hex, starting: false });
          // The awaits above (JWT mint, the Start publish — which itself runs under
          // the group lock) can straddle an epoch advance whose drain found no
          // session to rekey. Catch up now, before any media is sealed.
          onGroupMlsSettled(groupIdB64);
        },
        connect: async () => {
          await (started as SfuCallUiHandle | null)?.connect();
        },
      });
      switch (run.kind) {
        case "connected":
        case "left":
        case "ended":
          // `left`: the person left (or a newer call took the slot) while this
          // start was in flight. Nothing failed, the screens are told nothing,
          // and the slot is not this attempt's to touch.
          return { kind: run.kind };
        case "refused":
          return run.reason;
        case "failed":
          break;
      }
      const session = started as SfuCallUiHandle | null;
      // The person left while the session was connecting (`leaveGroupCall`
      // took it out of the slot and hung it up). An attempt that is no longer
      // current never gets here; this is the same answer read off the session.
      if (
        groupCallStartAbandoned({
          sessionState: session?.session.getState() ?? null,
          stillInSlot: session !== null && groupHandle() === session,
        })
      ) {
        return { kind: "left" };
      }
      // The SFU says the call ended before this device arrived
      // (`sfu-call-state.ts`): close the chat's record for everyone rather
      // than report a failure, since nothing failed on this side.
      const endedByServer = session?.session.callEndedByServer() === true;
      await leaveGroupCall("error");
      if (endedByServer) {
        await endGroupCallRecord(groupIdB64, hex, callId);
        return { kind: "ended" };
      }
      // eslint-disable-next-line no-console
      console.warn("group call could not start", run.error);
      return {
        kind: "failed",
        message: callFailureText(run.error, couldNot),
      };
    } finally {
      // Only the attempt the slot still belongs to frees it: one the person
      // left has had it taken back already, and it may be a newer call's now.
      if (groupAttempt === attempt) {
        groupAttempt = null;
        if (groupHandle() === null && activeGroupCall === null) setGroupSlot(null);
      }
    }
  }

  /** A person's name on a call tile: the one resolver every screen names
   *  people through, then the username they claim as plain text while it is
   *  not verified yet (`tile-name.ts`). */
  function callNameOfPerson(personKeyB64: string): string | null {
    return callTileName({
      resolved: personNameOrNull(profiles.resolve(personKeyB64)),
      announcedName: null,
      claimedUsername: profiles.claimedUsernameOf(personKeyB64),
    });
  }

  // A profile, a rename or an accepted contact landing while a group call is
  // up re-names the tiles already on screen: the adapter replays the person
  // behind each peer (from its verified announcement) through the resolver.
  createEffect(() => {
    profiles.version();
    const h = groupHandle();
    if (h !== null) untrack(() => h.refreshNames());
    // A link call too: a member's username verified, a contact renamed.
    const link = linkHandle();
    if (link !== null) untrack(() => link.refreshNames());
    const oneToOne = oneToOneShare;
    if (oneToOne !== null) untrack(() => oneToOne.share.refreshNames());
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
  /**
   * Close a group call's record on this device and for the chat: the SFU
   * said the call ended (`sfu-call-state.ts`). Any member may say so — the
   * End signal carries only the call id, and a record that is over on the
   * server is over for everyone.
   */
  async function endGroupCallRecord(
    groupIdB64: string,
    callIdHex: string,
    callId: Uint8Array,
  ): Promise<void> {
    applyGroupCallEvent(groupIdB64, { kind: "end", callIdHex });
    dismissGroupCall(callIdHex);
    try {
      await publishGroupControl(groupIdB64, (w) => w.composeCallEndSignaling(callId, "hungUp"));
    } catch (err) {
      console.warn("group call: could not publish the End the SFU reported", err);
    }
  }

  /**
   * Ask the SFU whether a call this device is offering to join is still
   * there, and close its record when it is not (`sfu-call-state.ts`). Never
   * throws; a probe that cannot be answered changes nothing.
   */
  async function probeGroupCallState(groupIdB64: string, callIdHex: string): Promise<void> {
    const base = sfuBaseUrl();
    const callId = hexToBytes(callIdHex);
    if (base === null || callId === null) return;
    try {
      // ADR-0078 section 2: the probe names the room (a one-way function of
      // the call id) and carries no token, so no admission token is minted
      // to ask and the issuer never sees who probes.
      const wasm = await loadWasm();
      const res = await fetch(sfuRoomStateUrl(base, wasm.sfuRoomForCall(callId)), {
        credentials: "omit",
        referrerPolicy: "no-referrer",
        cache: "no-store",
      });
      if (!res.ok) return;
      const state = parseSfuCallState((await res.json()) as unknown);
      if (state !== "ended") return;
      // Still shown as live, and this device is still not in it?
      if (activeGroupCall?.callIdHex === callIdHex) return;
      await endGroupCallRecord(groupIdB64, callIdHex, callId);
    } catch {
      // Unreachable SFU: the record stays as it is.
    }
  }

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
      // `finally`: clearing the slot takes the call screen down, and nothing
      // the screen does on its way out may leave the session open (camera and
      // microphone on, senders sending, the SFU socket up).
      try {
        setGroupHandle(null);
      } finally {
        h.hangUp();
      }
    }
    // A start still in its awaits is no longer the current attempt: it
    // builds no session, and the slot is free now, not when it returns.
    groupAttempt = null;
    setGroupSlot(null);
    return { call: g, failed };
  }

  /**
   * Leave the group call this device is in: tear the SFU session down and tell
   * the group, as `planGroupCallLeave` decides. Idempotent.
   */
  /** This account started the group call this device is in. Account-level:
   *  any device of the starter that is in the call. */
  function startedGroupCall(): boolean {
    const g = activeGroupCall;
    const account = accountId();
    if (g === null || account === null) return false;
    const starter = groupCallsFor(account, g.groupIdB64).get(g.callIdHex)?.initiatorPersonKey;
    return starter !== undefined && starter !== null && starter === g.ownPersonKey;
  }

  /**
   * End the group call for every person in it (`docs/ux/calls.md` section
   * 4.1.2). The end is a `CallEnd` with the reason `endedForAll`, published
   * into the group like any call signal; every device in the call checks
   * that it comes from the call's starter before it leaves. Published before
   * this device leaves, so it is on its way while the session goes down.
   */
  async function endGroupCallForEveryone(): Promise<void> {
    const g = activeGroupCall;
    if (g === null || !startedGroupCall()) return;
    groupCallEndedForEveryone = g.callIdHex;
    applyGroupCallEvent(g.groupIdB64, { kind: "end", callIdHex: g.callIdHex });
    const published = publishGroupControl(g.groupIdB64, (w) =>
      w.composeCallEndSignaling(g.callId, ENDED_FOR_ALL),
    );
    // The ended-phase effect leaves the call (and publishes this device's
    // Leave behind the End, through the same queue).
    callActions.endCall();
    try {
      await published;
    } catch (err) {
      // The starter has left either way; the others stay until they hang up
      // or the call empties.
      // eslint-disable-next-line no-console
      console.warn("group call: the end for everyone could not be published", err);
    }
  }

  /**
   * Write the group call's row (`docs/ux/calls.md` §6).
   *
   * Direction here is not who rang whom — nobody rings in a group call — but
   * whose call it was from this device's side: one it started is `outgoing`,
   * one it joined is `incoming`. There is deliberately no `missed` row for a
   * group call this device never joined: nothing rang, so nothing was missed,
   * and the group-call banner is what announces a live one.
   */
  function recordGroupCall(g: ActiveGroupCall, endedAtMs: number): void {
    const connected = g.connectedAtMs !== null;
    callLog.record(g.groupIdB64, {
      id: g.callIdHex,
      // Nobody rings in a group call, so "which way it went" is whose call it
      // was from this device's side: one it started is `outgoing`, one it
      // joined is `incoming`. Never `missed` — nothing rang, so nothing was
      // missed, and the group-call banner is what announces a live one.
      outcome: g.initiator ? "outgoing" : "incoming",
      video: g.video,
      durationMs: connected ? Math.max(0, endedAtMs - (g.connectedAtMs as number)) : null,
      startedAtMs: g.startedAtMs,
    });
  }

  async function leaveGroupCall(reason: GroupLeaveReason): Promise<void> {
    // Read before the slot is released: the call ended when the link was last
    // known alive, not when this device noticed (`callEndedAtMs`).
    const lastInbound = groupHandle()?.session.lastInboundAtMs() ?? 0;
    const { call: g, failed } = releaseGroupSlot();
    if (g === null) return;
    await announceGroupLeave(g, reason, failed, callEndedAtMs(Date.now(), lastInbound));
  }

  /**
   * Write the call's row and tell the group this device is out of the call
   * `g`, as `planGroupCallLeave` decides. `g` is already out of the slot, or
   * never reached it: a start the person left after its `GroupCallStart` was
   * published takes the announcement back through here.
   */
  async function announceGroupLeave(
    g: ActiveGroupCall,
    reason: GroupLeaveReason,
    failed: boolean,
    endedAtMs: number,
  ): Promise<void> {
    recordGroupCall(g, endedAtMs);
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
    const endAlreadyPublished = groupCallEndedForEveryone === g.callIdHex;
    if (endAlreadyPublished) groupCallEndedForEveryone = null;
    if (plan.publishEnd && !endAlreadyPublished) {
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
        // ADR-0087 section 9 - the person is out of the group, not only this
        // device: the same batch is ending the conversation.
        if (membershipEnd.isEnding(groupIdB64)) showCallEndedLine(t("call.ended.no_longer_in_group"));
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
  // ADR-0087 section 9 - an end heard from the account's other devices: this
  // group's handle may still be active, so no lock drain ends the call.
  onCleanup(
    membershipEnd.registerCallStop((groupIdB64) => {
      if (groupSlot()?.groupIdB64 !== groupIdB64) return;
      releaseGroupSlot();
      showCallEndedLine(t("call.ended.no_longer_in_group"));
    }),
  );
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
        // Left, or replaced by another call, while the camera was starting.
        if (untrack(groupHandle) !== h) return;
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

  /**
   * A new call is never handed to someone who put the last one away.
   *
   * Collapsing is a property of THE CALL (`docs/ux/calls.md` section 4.5), so
   * it has to end with that call rather than persist as a preference: a person
   * who collapsed a call an hour ago and now presses Accept expects a call
   * screen, not a bar. One effect on the slot rather than a reset in each of the
   * four start paths — dial, accept, start-group, start-link — because a fifth
   * one added later would otherwise inherit the last call's presentation.
   *
   * Keyed on the occupant changing, which covers both directions: the slot going
   * free, and the improbable case of it being handed straight from one kind to
   * another without a reactive gap in between.
   */
  let lastOccupant: CallKind | null = null;
  createEffect(() => {
    const occupant = currentOccupant();
    if (occupant === lastOccupant) return;
    lastOccupant = occupant;
    setCollapsed(false);
  });

  /** The attempt the link slot belongs to. A report from an older attempt —
   *  one the user already left — never touches the slot. */
  let linkAttempt: object | null = null;

  function applyLinkEvent(
    attempt: object,
    event: LinkCallEvent,
    errorMessage?: CallText | null,
  ): void {
    if (linkAttempt !== attempt) return;
    const slot = linkSlot();
    if (slot === null) return;
    const phase = reduceLinkCallPhase(slot.phase, event);
    if (phase === slot.phase) return;
    // Over on its own: drop the session (idempotent — it also resets the
    // shared in-call store) and keep the outcome for the user to see.
    // `finally`, as in `releaseGroupSlot`: both writes below take the call
    // screen down, and the session is closed whatever the screen does on its
    // way out.
    const over = !linkCallOccupiesSlot(phase);
    const h = over ? linkHandle() : null;
    try {
      setLinkSlot({
        ...slot,
        phase,
        errorMessage: phase === "error" ? (errorMessage ?? slot.errorMessage) : slot.errorMessage,
      });
      if (over) setLinkHandle(null);
    } finally {
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
      return { kind: "failed", message: () => t("common.error.session_ended") };
    }
    const base = sfuBaseUrl();
    if (base === null) {
      return {
        kind: "failed",
        // No SFU endpoint in this build (`VITE_LAKKO_SFU_URL`).
        message: () => t("call.error.unavailable"),
      };
    }

    const attempt = {};
    linkAttempt = attempt;
    setLinkHandle(null);
    setLinkCallId(opts.callId);
    setLinkInvite(INVITE_IDLE);
    setInviteLineShown(false);
    if (opts.isOwner) {
      // `docs/ux/call-links.md` section 5: pressing the button shows "the call
      // screen, connecting" — never a form, and not a strip at the bottom of
      // whatever screen the person was on. The founder's overlay is mounted
      // from `starting`, so the store needs a phase it can draw. A joiner is
      // left alone: it is waiting to be admitted, which its own notice says
      // better than a connecting screen would.
      callActions.setPhase({ kind: "calling" });
    }
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
      media: { audio: true, video: opts.mediaVideo },
      localDisplayName: ownTileName(),
      profileName: ownProfileName(),
      ...(opts.callName !== undefined ? { initialCallName: opts.callName } : {}),
      ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
      // Tiles are named by the one resolver: a member's verified peer
      // announcement names the person (ADR-0078 section 2), and a member's
      // sealed profile is filed like any received profile.
      onMemberProfile: (personKeyB64, profile) => profiles.ingest(personKeyB64, profile),
      // The link's owner ended the call for everyone: say who, then end it
      // here the way a call that ended on its own ends.
      onEndedForEveryone: (peerId) => {
        const name =
          callUi.participants().find((p) => p.id === `peer-${peerId}`)?.displayName ?? "";
        showEndedForEveryone(name.length > 0 ? name : null);
        applyLinkEvent(attempt, { kind: "ended" });
      },
      resolvePersonName: callNameOfPerson,
      onJoined: () => applyLinkEvent(attempt, { kind: "joined" }),
      onEnded: () => applyLinkEvent(attempt, { kind: "ended" }),
      onNoCommitter: () => applyLinkEvent(attempt, { kind: "no-committer" }),
      onError: (err: unknown) => {
        // An attempt the person already left has nothing to report.
        if (linkAttempt !== attempt) return;
        // eslint-disable-next-line no-console
        console.warn("link call failed", err);
        applyLinkEvent(attempt, { kind: "error" }, callFailureReason(err));
      },
      // The shared call store belongs to the attempt in the slot; an older
      // one that finishes later writes nothing to it and is never started.
      isCurrent: () => linkAttempt === attempt,
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
      // Left, or replaced, while the start was in flight: whatever the
      // attempt ran into afterwards is not shown as a call that could not
      // start. The same neutral outcome as the branch above.
      if (linkAttempt !== attempt) return { kind: "started" };
      // eslint-disable-next-line no-console
      console.warn("link call could not start", err);
      const reason = callFailureReason(err);
      applyLinkEvent(attempt, { kind: "error" }, reason);
      return { kind: "failed", message: reason ?? (() => t("call.link.error.generic")) };
    }
  }

  /** End the link call for every person in it. Only the link's owner. The
   *  ended-phase effect then leaves, as after `End`. */
  function endLinkCallForEveryone(): void {
    const h = linkHandle();
    if (h === null || linkSlot()?.role !== "owner") return;
    h.endForEveryone();
    callActions.endCall();
  }

  /** Leave the link call: no outcome to show, the user chose it. */
  function leaveLinkCall(): void {
    // Read before the slot is cleared: whether a link call holds the shared
    // call store at all (this also runs at sign-out with none).
    const slot = untrack(linkSlot);
    const h = untrack(linkHandle);
    const held = h !== null || (slot !== null && linkCallOccupiesSlot(slot.phase));
    linkAttempt = null;
    // `finally`, as in `releaseGroupSlot`: the writes below take the call
    // screen down, and the session is closed whatever it does on its way out.
    try {
      setLinkHandle(null);
      setLinkSlot(null);
      setLinkCallId(null);
      setLinkInvite(INVITE_IDLE);
      setInviteLineShown(false);
    } finally {
      h?.hangUp();
      // The attempt is no longer current, so its `hangUp` leaves the store
      // alone (`MultiJoinerCallOptions.isCurrent`); the store is put back
      // here, now, by the call that held it. A session still being built
      // when the person left is hung up when it arrives and by then writes
      // nothing: the store may be a newer call's.
      if (held) callActions.reset();
    }
  }

  function dismissLinkCallOutcome(): void {
    const slot = linkSlot();
    if (slot !== null && !linkCallOccupiesSlot(slot.phase)) setLinkSlot(null);
  }

  // ----- Invite: a link to the call already running --------------------------

  let inviteStatusTimer: ReturnType<typeof setTimeout> | null = null;
  function showInvite(state: InCallInvite): void {
    if (inviteStatusTimer !== null) clearTimeout(inviteStatusTimer);
    inviteStatusTimer = null;
    setLinkInvite(state);
    setInviteLineShown(inviteStatusLine(state) !== null);
    const hold = inviteStatusHoldMs(state);
    if (hold === null) return;
    inviteStatusTimer = setTimeout(() => {
      inviteStatusTimer = null;
      setInviteLineShown(false);
    }, hold);
  }
  onCleanup(() => {
    if (inviteStatusTimer !== null) clearTimeout(inviteStatusTimer);
  });

  /** What the call screen shows under its status row, or `null`. */
  const linkInviteStatus = (): string | null =>
    inviteLineShown() ? inviteStatusLine(linkInvite()) : null;

  /**
   * The link to the call in the slot, minted on first use.
   *
   * `null` when there is none to give: no call, no session, or the mint
   * failed — and in each case the status line already says so.
   */
  async function linkForCall(): Promise<string | null> {
    const state = untrack(linkInvite);
    const press = nextInvitePress(state);
    if (press === "ignore") return null;
    const have = mintedInviteUrl(state);
    if (press === "reoffer" && have !== null) return have;
    const callId = untrack(linkCallId);
    const slot = untrack(linkSlot);
    if (callId === null || slot === null || !linkCallOccupiesSlot(slot.phase)) return null;
    // The canonical identity, not a throwaway: the capability's owner key is
    // derived from this seed, and `MultiJoinerCall` refuses a group whose
    // roster does not contain the owner named by the link.
    const identity = chats.identity();
    const secrets = auth.identitySecrets();
    if (identity === null || secrets === null) {
      showInvite({ kind: "failed", failure: "signed-out" });
      return null;
    }
    showInvite({ kind: "minting" });
    let url: string;
    try {
      const discovery = new DiscoveryClient();
      discovery.setSessionToken(secrets.sessionToken);
      const expiryMs = callInviteExpiryMs(Date.now());
      const maxUses = callInviteMaxUses();
      const minted = await composeAndUploadCallInvite({
        ownerSigningSeed: identity.ed25519Seed,
        ownerAccountId: secrets.accountId,
        identity,
        discoveryClient: discovery,
        callId,
        mediaVideo: CALL_INVITE_MEDIA_VIDEO,
        expiryMs,
        maxUses,
        label: CALL_INVITE_LABEL,
      });
      url = minted.url;
      // ADR-0074 section 3.5a rule 5: the ledger id and the link id, kept
      // under the device key, are what revokes this link if the account's
      // keys are rotated later. Best effort; the link works either way.
      void rememberMintedLink(
        base64Encode(secrets.accountId),
        "call",
        base64UrlEncodeNoPad(minted.blobId),
        {
          linkSecret: minted.linkSecret,
          linkId: minted.linkId,
          // ADR-0078 section 10: the only key that revokes the ledger row.
          revocationSeed: minted.revocationSeed,
          facts: {
            label: CALL_INVITE_LABEL,
            expiryMs: Number(expiryMs),
            maxUses,
            bootstrapReferral: false,
          },
        },
      );
    } catch (err) {
      // The call runs over the SFU and owes the ledger nothing, so a failed
      // mint is a sentence and an unchanged call.
      // eslint-disable-next-line no-console
      console.warn("call host: could not prepare a link for this call", err);
      showInvite({ kind: "failed", failure: "unavailable" });
      return null;
    }
    // The call may have ended while the mint was in flight, or ended and
    // been followed by another: the link is the first call's and is never
    // shown on the second.
    const after = untrack(linkSlot);
    switch (
      mintedInviteFate({
        mintedForCallIdHex: slot.callIdHex,
        slotCallIdHex:
          after !== null && linkCallOccupiesSlot(after.phase) ? after.callIdHex : null,
      })
    ) {
      case "show":
        return url;
      case "call-over":
        showInvite(INVITE_IDLE);
        return null;
      case "another-call":
        // The line and the state are the new call's: left as they are.
        return null;
    }
  }

  /** `Copy link`: the link goes to the clipboard, and the line says so. */
  async function copyLinkCallInvite(): Promise<void> {
    const url = await linkForCall();
    if (url === null) return;
    const clipboard = globalThis.navigator?.clipboard;
    const delivery = await copyCallLink(
      url,
      clipboard !== undefined && typeof clipboard.writeText === "function"
        ? async (text: string): Promise<void> => {
            await clipboard.writeText(text);
          }
        : undefined,
    );
    showInvite({ kind: "ready", url, delivery });
  }

  // `Invite contacts`: a picker of this person's contacts; the invitation is
  // sent into the one-to-one with the contact they choose.
  const [invitePickerOpen, setInvitePickerOpen] = createSignal(false);

  // Founds the one-to-one with a contact who has never been written to from
  // this device. The same construction as the mailbox's client: groups are
  // this device's, the account mailbox is the person's (ADR-0050).
  const contactsClient = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    accountIdentityProvider: () => chats.accountIdentity(),
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
    ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
    onUsernameResolved: (key, username) => profiles.learnUsername(key, username),
  });

  /**
   * Send the invitation to a contact.
   *
   * It goes into the one-to-one with that contact
   * (`contacts/contact-conversation.ts`); when this device holds none, one is
   * founded first, the way `Message` on the contact's card does. The message
   * is the call's link and nothing else, which is what lets every client draw
   * it as an invitation with a `Join` button (`calls/call-invite-link.ts`).
   * Rejects when it could not be sent, so the picker shows the reason and the
   * row stays pressable; the call is untouched either way.
   */
  async function sendLinkCallInvite(target: {
    readonly keyB64: string;
    readonly name: string;
  }): Promise<InviteOutcome> {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    const contact = loadAccepted().find((c) => c.signaturePubkeyB64 === target.keyB64);
    if (s.kind !== "session" || secrets === null || contact === undefined) {
      throw new KeyedError("call.invite.send_failed");
    }
    const url = await linkForCall();
    if (url === null) throw new KeyedError("call.invite.send_failed");
    const forCallIdHex = untrack(linkSlot)?.callIdHex ?? null;
    // Whether the call the link was made for still holds the slot; asked
    // after the send, before its outcome is written to the call's status.
    const sameCall = (): boolean => {
      const now = untrack(linkSlot);
      return now !== null && linkCallOccupiesSlot(now.phase) && now.callIdHex === forCallIdHex;
    };
    const accountIdHex = contact.accountIdHex;
    const accountIdOf = (hex: string | undefined): Uint8Array => {
      const bytes = hex === undefined ? null : hexToBytes(hex);
      if (bytes === null) throw new Error("contact has no account id");
      return bytes;
    };
    let outcome: InviteOutcome;
    try {
      outcome = await inviteContactToCall({
        existing: () =>
          conversationWithContact({
            contact,
            conversations: chats.groups(),
            ownKeyB64: ownPersonKeyB64(chats.identity()),
            peopleOf: (g) => personKeysOf(g.group),
            isOneToOne: (g) => conversationKindOf(g.group) === "chat",
          }),
        accountKnown: accountIdHex !== undefined,
        grantReaches: () => heldGrantReaches(base64Decode(contact.signaturePubkeyB64)),
        requestWaiting: () =>
          loadSentRequests(s.account.accountId).some((r) => r.recipientAccountIdHex === accountIdHex),
        sharedGroups: () =>
          sharedGroupsFor({
            groups: chats.groups(),
            accountIdB64: s.account.accountId,
            ownKeyB64: ownPersonKeyB64(chats.identity()),
            personKeysB64: [contact.signaturePubkeyB64, ...(contact.previousSignaturePubkeysB64 ?? [])],
          }),
        found: async () =>
          (
            await foundOneToOneWith({
              client: contactsClient,
              identity: secrets,
              chats,
              profiles,
              contact,
              peerAccountId: accountIdOf(accountIdHex),
              peerPubkey: base64Decode(contact.signaturePubkeyB64),
            })
          ).conversation,
        sendLink: (entry) => sendTextToConversation({ chat, accountId: s.account.accountId, entry, text: url }),
        // Who-can-find-me 6.4 - no inbox reaches them: the contact request
        // goes inside a group both are in; the call link waits until they
        // accept.
        sendInGroup: async (groupIdB64) => {
          const shared = chats.get(groupIdB64);
          if (shared === undefined || accountIdHex === undefined) throw new Error("the shared group is gone");
          await contactsClient.sendContactRequestInGroup(
            secrets,
            accountIdOf(accountIdHex),
            base64Decode(contact.signaturePubkeyB64),
            shared.group,
          );
          const name = profiles.resolve(contact.signaturePubkeyB64);
          recordSentRequest(s.account.accountId, {
            recipientUsername: name.username ?? "",
            recipientName: memberNameOf(name, "a call invitation's sent request"),
            recipientAccountIdHex: accountIdHex,
            sentAtMs: Date.now(),
            recipientKeyB64: contact.signaturePubkeyB64,
            throughGroupIdB64: groupIdB64,
          });
        },
      });
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("call host: could not send the call invitation", err);
      if (sameCall()) showInvite({ kind: "failed", failure: "not-sent", url });
      throw new KeyedError("call.invite.send_failed");
    }
    // The picker stays open: several contacts can be invited one after
    // another, and it is closed by the person.
    if (outcome === "invited" && sameCall()) {
      showInvite({ kind: "ready", url, delivery: { kind: "sent", name: target.name } });
    }
    return outcome;
  }

  // The embedded in-call UI's End button moves the store to `ended`. A session
  // that is still up means the user is ending it; one already closed ended on
  // its own, and `onEnded` keeps that outcome for the user to see.
  createEffect(() => {
    if (callUi.phase().kind !== "ended") return;
    const h = linkHandle();
    if (h === null) {
      // No session yet: End was pressed on a founder's screen while the call
      // was still coming up. Leaving drops the attempt, so the session the
      // start is still building is hung up the moment it arrives.
      const slot = untrack(linkSlot);
      if (slot !== null && linkCallOccupiesSlot(slot.phase)) untrack(() => leaveLinkCall());
      return;
    }
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
        // Left, or replaced by another call, while the camera was starting.
        if (untrack(linkHandle) !== h) return;
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
      // The shared call screen follows the session's state, and a session
      // ended here may never report `done` (its End cannot be published
      // without a session), so the screen would stay on `Calling...` and keep
      // looping the outgoing tone on the sign-in page (seen live 2026-10-29,
      // stopped only by a reload). Nothing of a call survives a sign-out.
      callActions.reset();
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
    ringingOfferedVideo: remoteWantsVideo,
    // The clock is read here rather than kept in a signal: the sweep above is
    // what makes the surfaces disappear, and this is the guard that stops a
    // tick that has not landed yet from leaving a dead Accept on screen.
    quietIncomingCall: () =>
      quietCallOffer({ ringing: state() === "ringing", taken: quietOffer(), nowMs: Date.now() }),
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
    startedGroupCall,
    endGroupCallForEveryone,
    endLinkCallForEveryone,
    endedForEveryoneNotice,
    callOccupant: currentOccupant,
    callBusyMessage: (verb) => {
      const occupant = currentOccupant();
      return occupant === null ? null : callBusyMessage(occupant, verb);
    },
    endCurrentCall,
    linkCall: linkSlot,
    startLinkCall,
    leaveLinkCall,
    copyLinkCallInvite,
    linkCallInvitePickerOpen: invitePickerOpen,
    openLinkCallInvitePicker: () => setInvitePickerOpen(true),
    closeLinkCallInvitePicker: () => setInvitePickerOpen(false),
    sendLinkCallInvite,
    linkCallInvite: linkInvite,
    linkCallInviteStatus: linkInviteStatus,
    dismissLinkCallOutcome,
    callCollapsed: collapsed,
    collapseCall: () => setCollapsed(true),
    expandCall: () => setCollapsed(false),
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
    silentRing,
    connStats,
    nameFor,
    personKeyFor,
    groupNameFor: (groupIdB64: string | null): string | null =>
      groupIdB64 === null ? null : groupNames.nameOf(groupIdB64),
    hangUp,
    groupHandle,
    dismissedGroupCalls,
    dismissGroupCall,
    anyCallInProgress,
    probeGroupCallState,
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
  /** Ask the SFU whether a call offered as joinable is still there; close
   *  its record when the SFU says it ended (`sfu-call-state.ts`). */
  probeGroupCallState(groupIdB64: string, callIdHex: string): Promise<void>;
  readonly remoteStream: Accessor<MediaStream | null>;
  readonly localStream: Accessor<MediaStream | null>;
  readonly remoteWantsVideo: Accessor<boolean>;
  /** The ringing 1x1 call is muted (`docs/ux/calls.md` section 2.1): take the
   *  call, withhold the interruption. */
  readonly silentRing: Accessor<boolean>;
  readonly connStats: Accessor<WebConnectionStats | null>;
  nameFor(groupIdB64: string | null): string;
  /** The other person of a one-to-one call, `null` for a group. */
  personKeyFor(groupIdB64: string | null): string | null;
  /** The group's shared name (ADR-0051), or `null` when it has none. */
  groupNameFor(groupIdB64: string | null): string | null;
  /**
   * End the live 1x1 call.
   *
   * Accept and Decline are deliberately NOT here: they are on the public
   * {@link CallHostApi}, because the muted conversation's quiet surfaces
   * (`docs/ux/calls.md` section 2.1.1) live outside this view and have to
   * reach the same two entry points the full screen reaches. A second pair
   * here is how the two would come to differ.
   */
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
  const auth = useAuth();
  const internals = useContext(CallHostInternalsContext);
  if (internals === undefined) throw new Error("CallHostView outside <CallHostProvider>");
  const [audioEl, setAudioEl] = createSignal<HTMLAudioElement | null>(null);
  /** What the group call's tiles ask of it, safe to ask after it is over. */
  const groupTiles = groupCallTileSources(() => internals.groupHandle());

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
  /**
   * `docs/ux/calls.md` section 2.1 — "a muted chat never rings", decided by
   * the shared rule (`ring-rule.ts`) when the offer arrived.
   *
   * This is the whole of what `silent` withholds: the ringtone, the vibration,
   * and the screen that takes over the tab. The call itself is untouched — it
   * holds the slot, it runs its own ring timer, and it writes the same
   * call-log row an unmuted call writes when it ends.
   *
   * It is no longer the whole of what `silent` DOES. This comment used to end
   * by conceding that, since the withheld screen was also the only Accept this
   * client had, a muted call simply went unanswered. Section 2.1.1 rejects
   * that: muting takes away the interruption, not the call. The call is
   * offered on two quiet surfaces instead — a panel inside the conversation
   * and Accept / Decline on its row in the conversation list — both driven by
   * {@link CallHostApi.quietIncomingCall} and both reaching the same accept
   * and decline this screen reaches.
   */
  const ringingAudibly = (): boolean => host.state() === "ringing" && !internals.silentRing();

  createEffect(() => {
    if (!ringingAudibly()) {
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

  // The 1x1 sink: the same binding, retry and console line as a group
  // call's sinks (`RemoteAudio` below).
  let oneToOneSink: RemoteAudioSinkController | null = null;
  createEffect(() => {
    const el = audioEl();
    const stream = internals.remoteStream();
    if (el === null) return;
    if (oneToOneSink === null) oneToOneSink = new RemoteAudioSinkController(el, "one-to-one");
    oneToOneSink.setStream(stream);
  });
  onCleanup(() => {
    oneToOneSink?.dispose();
    oneToOneSink = null;
  });

  // The in-call Speaker control (docs/ux/calls.md section 4.1) moves the sound
  // to the chosen output. It writes `selectedAudioOutput`; this is what makes
  // that a real thing rather than a stored preference — nothing applied it
  // before, so the old settings panel's speaker picker changed nothing.
  createEffect(() => {
    applyAudioOutput(audioEl(), callUi.settings().selectedAudioOutput);
  });

  const oneToOneOverlayWanted = (): boolean => {
    const s = host.state();
    return s === "outgoing" || s === "connecting" || s === "connected" || s === "ending";
  };

  /**
   * The call screen would be up if the call had not been put away
   * (`docs/ux/calls.md` section 4.5).
   *
   * One read over all three kinds of call, because the bar is one bar and the
   * slot holds one call: whichever screen `collapsed` is hiding, the bar takes
   * its place.
   */
  const callScreenWanted = (): boolean =>
    oneToOneOverlayWanted() ||
    internals.groupHandle() !== null ||
    linkCallOverlayShown(host.linkCall()?.phase ?? null, host.linkCall()?.role ?? null);

  /**
   * The call screen is actually on screen.
   *
   * Fed to the shared visibility rule as its `callScreenUp`, which is what
   * keeps "the bar is never shown over the call screen" a single decision
   * rather than a condition repeated at both surfaces.
   */
  const callScreenUp = (): boolean => callScreenWanted() && !host.callCollapsed();

  /** Put the call away, or `undefined` when there is nothing the bar could
   *  show in its place — an ended call, whose overlay still owns the screen
   *  until it is dismissed. So the chevron and `Esc` are absent exactly when
   *  collapsing would leave the person with no call surface at all, and that
   *  is the shared rule's answer rather than a second list of phases here. */
  const collapsible = (): boolean => {
    const rules = uiRules();
    return rules !== null && callBarWouldShow(rules, callUi.phase().kind, host.state() === "ringing");
  };
  const collapse = (): (() => void) | undefined =>
    collapsible() ? (): void => host.collapseCall() : undefined;

  /**
   * One surface, always: never neither.
   *
   * A collapsed call whose phase moves somewhere the bar does not go would be
   * invisible with no way back — the overlay is hidden because it was put away,
   * and the bar refuses the phase. The reachable case is a call ENDING under the
   * bar: the store reaches `ended` while the group handle or the link slot is
   * still held for the moment it takes to leave, and section 4.5 gives the ended
   * overlay (and its End button) to the call screen rather than to the bar.
   *
   * So the screen comes back instead. Both halves of the condition come from the
   * shared rule, so this enforces its own consequence rather than adding a
   * second opinion about which phases collapse.
   */
  createEffect(() => {
    if (host.callCollapsed() && callScreenWanted() && !collapsible()) host.expandCall();
  });

  return (
    <>
      {/* Stable slot for the whole call, so remote audio does not cut out when
          the banner gives way to the overlay. */}
      <Show when={host.inCall()}>
        <audio ref={(el) => setAudioEl(el)} autoplay style={{ display: "none" }} />
      </Show>

      {/* `docs/ux/calls.md` §2.2 — a ringing call takes the whole screen, not
          a strip at the top of whatever the user was reading. §2.1 exempts a
          muted conversation: taking over the tab IS the interruption. Such a
          call is offered by the two quiet surfaces of §2.1.1 instead, which
          live in the conversation and on its row rather than here. */}
      <Show when={ringingAudibly()}>
        <IncomingCallScreen
          callerName={internals.nameFor(host.callGroupIdB64())}
          callerId={host.callGroupIdB64()}
          callerKey={internals.personKeyFor(host.callGroupIdB64())}
          video={internals.remoteWantsVideo()}
          onAccept={(withVideo) => void host.acceptIncomingCall(withVideo)}
          onDecline={() => void host.declineIncomingCall()}
        />
      </Show>

      <Show when={oneToOneOverlayWanted() && !host.callCollapsed()}>
        <Call
          embedded={true}
          onExit={() => void internals.hangUp()}
          onCollapse={collapse()}
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
      <Show when={internals.groupHandle() !== null}>
        {/* The tiles read the call through `groupTiles`, never through the
            accessor this `<Show>` could hand over: they are read while the
            screen is taken down, after the handle was cleared
            (`calls/group-call-tile-sources.ts`). */}
        {/* The overlay is what `collapsed` hides; the audio sinks below
            are not, so they stay outside this branch and a collapsed call
            keeps its sound. */}
        <Show when={!host.callCollapsed()}>
          <Call
            embedded={true}
            group={true}
            groupName={internals.groupNameFor(host.groupCall()?.groupIdB64 ?? null)}
            onExit={() => void host.leaveGroupCall()}
            onEndForEveryone={
              host.startedGroupCall() ? () => void host.endGroupCallForEveryone() : undefined
            }
            onCollapse={collapse()}
            remoteStreamFor={groupTiles.remoteStreamFor}
            remoteLightStreamFor={groupTiles.remoteLightStreamFor}
            remoteMiddleStreamFor={groupTiles.remoteMiddleStreamFor}
            remoteVideoGivenFor={groupTiles.remoteVideoGivenFor}
            reportTileVideoQuality={groupTiles.reportTileVideoQuality}
            localStream={groupTiles.localStream}
            remoteAudioReceiverFor={groupTiles.remoteAudioReceiverFor}
          />
        </Show>
        {/* One hidden sink per remote stream, keyed by stream identity, so
            audio does not cut on roster churn. Remote video renders per tile
            through `remoteStreamFor`. */}
        <For each={groupTiles.remoteStreamList()}>
          {(stream) => <RemoteAudio stream={stream} />}
        </For>
      </Show>

      {/* The call-link call's in-call UI, once this device is in the call's
          group (owner: from founding; joiner: from its Welcome). */}
      {/* Live for either role; and from the first second for the founder, who
          pressed a button that promised a call screen (`docs/ux/call-links.md`
          section 5). A joiner keeps its notice until it is admitted: it has
          not published anything and is not in a call yet. */}
      <Show
        when={
          linkCallOverlayShown(
            host.linkCall()?.phase ?? null,
            host.linkCall()?.role ?? null,
          ) && !host.callCollapsed()
        }
      >
        <Call
          embedded={true}
          group={true}
          onExit={() => host.leaveLinkCall()}
          onEndForEveryone={
            host.linkCall()?.role === "owner" ? () => host.endLinkCallForEveryone() : undefined
          }
          onCollapse={collapse()}
          /* `docs/ux/call-links.md` section 3: the one control the call screen
             gains. Only this kind of call has it — a link names a `call_id`,
             and a call placed inside a conversation is reached through that
             conversation instead. */
          onCopyInviteLink={() => void host.copyLinkCallInvite()}
          /* Always there for an account, with or without contacts: an item
             that comes and goes reads as a missing feature (found live
             2026-10-02), so the list itself says when it is empty. Only a
             guest (`auth/guest-account.ts`), who has no contacts, gets the
             link alone. */
          onInviteContact={
            auth.isGuest() ? undefined : () => host.openLinkCallInvitePicker()
          }
          inviteBusy={inviteIsBusy(host.linkCallInvite())}
          inviteStatus={host.linkCallInviteStatus()}
          remoteStreamFor={(participantId): MediaStream | undefined =>
            internals.linkHandle()?.remoteStreamFor(participantId)
          }
          remoteLightStreamFor={(participantId): MediaStream | undefined =>
            internals.linkHandle()?.remoteLightStreamFor(participantId)
          }
          remoteMiddleStreamFor={(participantId): MediaStream | undefined =>
            internals.linkHandle()?.remoteMiddleStreamFor(participantId)
          }
          remoteVideoGivenFor={(participantId) =>
            internals.linkHandle()?.remoteVideoGivenFor(participantId)
          }
          reportTileVideoQuality={(tile, participantId, quality): void => {
            internals
              .linkHandle()
              ?.reportTileVideoQuality(tile, participantId.replace(/^peer-/, ""), quality);
          }}
          localStream={(): MediaStream | undefined =>
            internals.linkHandle()?.localStream() ?? undefined
          }
          remoteAudioReceiverFor={(participantId) =>
            internals.linkHandle()?.remoteAudioReceiverFor(participantId)
          }
        />
      </Show>
      <For each={[...(internals.linkHandle()?.remoteStreams().values() ?? [])]}>
        {(stream) => <RemoteAudio stream={stream} />}
      </For>
      {/* The sound of the share on screen (ADR-0083: it plays with the call,
          with no control of its own). The share's picture is drawn muted by
          the call screen, so this is its one sink. */}
      <Show when={host.inCall() ? currentShareStream() : undefined} keyed>
        {(stream) => <RemoteAudio stream={stream} />}
      </Show>

      {/* `Invite a contact` (`docs/ux/call-links.md` section 3): the same list
          of conversations a forward offers, under its own title. */}
      <InviteContactsPicker
        open={host.linkCallInvitePickerOpen()}
        onClose={() => host.closeLinkCallInvitePicker()}
        onInvite={(row) => host.sendLinkCallInvite({ keyB64: row.keyB64, name: row.name })}
      />

      {/* The call, put away (`docs/ux/calls.md` section 4.5). Last in the
          fragment so it paints over the app and under nothing that matters:
          the call screen it replaces is not up while it is, and the ringing
          screen and the group-call banner sit above it by z-index. It renders
          nothing at all without a collapsed call. */}
      <CallBar
        peerName={
          host.callOccupant() === "one-to-one"
            ? internals.nameFor(host.callGroupIdB64())
            : null
        }
        groupCall={host.callOccupant() !== null && host.callOccupant() !== "one-to-one"}
        groupName={
          host.callOccupant() === "group"
            ? internals.groupNameFor(host.groupCall()?.groupIdB64 ?? null)
            : null
        }
        ringing={host.state() === "ringing"}
        callScreenUp={callScreenUp()}
        onExpand={() => host.expandCall()}
      />

      <LinkCallNoticeView />
      {/* "{name} ended the call for everyone", for a few seconds after the
          starter of the call this device was in ended it
          (`docs/ux/calls.md` section 4.1.2). */}
      <Show when={host.endedForEveryoneNotice()}>
        {(text) => (
          <div class="lk-call-incoming bottom" role="status">
            <span class="lk-call-incoming-icon" aria-hidden="true">
              <Icon name="phoneEnd" size={20} />
            </span>
            <div class="lk-call-incoming-text">
              <span>{text()}</span>
            </div>
          </div>
        )}
      </Show>
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

/**
 * One hidden sink for one remote stream, following the Speaker control.
 *
 * Bound once it is in the document, with `play()` called and retried, and
 * played through Web Audio where `calls/remote-audio.ts` says so (the desktop
 * app on WebKit, where the element path was silent on 2026-11-07). Each sink
 * writes a `[call] remote audio` line saying whether it plays.
 */
function RemoteAudio(props: { stream: MediaStream }): JSX.Element {
  const [el, setEl] = createSignal<HTMLAudioElement | null>(null);
  createEffect(() => {
    applyAudioOutput(el(), callUiSettings().selectedAudioOutput);
  });
  onMount(() => {
    const node = el();
    if (node === null) return;
    const sink = new RemoteAudioSinkController(node, "group");
    createEffect(() => {
      sink.setStream(props.stream);
    });
    onCleanup(() => sink.dispose());
  });
  return <audio autoplay style={{ display: "none" }} ref={(node) => setEl(node)} />;
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
    return linkCallNotice(
      slot?.phase ?? null,
      slot?.role ?? null,
      internals.linkViewShown(),
      slot?.errorMessage ?? null,
    );
  });

  return (
    <Show when={notice()}>
      {(n) => (
        <div class="lk-call-incoming bottom" role="status" aria-label={t("call.link.notice.a11y")}>
          <span class="lk-call-incoming-icon" aria-hidden="true">
            <Icon name="link" size={20} />
          </span>
          <div class="lk-call-incoming-text">
            <span>{n().text}</span>
          </div>
          <div class="lk-call-incoming-actions">
            <Show when={n().actions.includes("open")}>
              <A href={host.linkCall()?.routePath ?? "/"} class="lk-button-link">
                {t("common.open")}
              </A>
            </Show>
            <Show when={n().actions.includes("cancel")}>
              <button type="button" class="ghost" onClick={() => host.leaveLinkCall()}>
                {t("common.cancel")}
              </button>
            </Show>
            <Show when={n().actions.includes("dismiss")}>
              <button type="button" class="ghost" onClick={() => host.dismissLinkCallOutcome()}>
                {t("common.dismiss")}
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
  const [failure, setFailure] = createSignal<{ callIdHex: string; message: CallText } | null>(
    null,
  );
  const [joining, setJoining] = createSignal(false);

  /** The calls the chat records say are in progress, one per group. */
  const liveCalls = createMemo((): { groupIdB64: string; callIdHex: string }[] => {
    const s = auth.state();
    if (s.kind !== "session") return [];
    const groups = chats.groups();
    const ownId = chats.identity();
    const viewer: GroupCallViewer | undefined =
      ownId === null
        ? undefined
        : {
            ownMemberKey: base64Encode(ownId.signaturePublicKey),
            inCallIdHex: host.groupCall()?.callIdHex ?? null,
          };
    return liveGroupCallsFor(
      s.account.accountId,
      groups.map((g) => g.groupIdB64),
      clock(),
      viewer,
    ).map((l) => ({ groupIdB64: l.groupIdB64, callIdHex: l.call.callIdHex }));
  });

  // While a call is offered as joinable, ask the SFU every
  // `SFU_CALL_STATE_PROBE_MS` whether it is still there, and close the record
  // when it is not (`sfu-call-state.ts`). The call this device is in needs
  // no probe: its own socket is the proof.
  createEffect(() => {
    const targets = liveCalls().filter((l) => l.callIdHex !== host.groupCall()?.callIdHex);
    if (targets.length === 0) return;
    const probe = (): void => {
      for (const t of targets) void internals.probeGroupCallState(t.groupIdB64, t.callIdHex);
    };
    probe();
    const timer = setInterval(probe, SFU_CALL_STATE_PROBE_MS);
    onCleanup(() => clearInterval(timer));
  });

  const banner = createMemo(() => {
    const s = auth.state();
    if (s.kind !== "session") return null;
    foregroundVersion();
    const live = liveCalls();
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
        <div class="lk-call-incoming" role="status" aria-label={t("chat.call.group_in_progress")}>
          <span class="lk-call-incoming-icon" aria-hidden="true">
            <Icon name="groups" size={20} />
          </span>
          <div class="lk-call-incoming-text">
            <strong>{t("call.group_banner.title", { name: internals.nameFor(ref().groupIdB64) })}</strong>
            <span>{t("call.e2ee")}</span>
            <Show when={failure()?.callIdHex === ref().callIdHex ? failure()?.message : undefined}>
              {(message) => (
                <span class="lk-call-incoming-error" role="alert">
                  {message()()}
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
              {t("common.dismiss")}
            </button>
            <A
              href={paths.chat(base64UrlEncodeNoPad(base64Decode(ref().groupIdB64)))}
              class="lk-button-link"
            >
              {t("call.group_banner.open_chat")}
            </A>
            <button type="button" disabled={joining()} onClick={() => void join(true)}>
              <Icon name="video" size={18} />
              {t("call.group_banner.join_video")}
            </button>
            <button
              type="button"
              class="primary"
              disabled={joining()}
              onClick={() => void join(false)}
            >
              <Icon name="phone" size={18} />
              {t("chat.call.join_short")}
            </button>
          </div>
        </div>
      )}
    </Show>
  );
}
