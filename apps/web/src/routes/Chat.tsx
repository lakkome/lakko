// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/chat/:id` route — full timeline + composer for a 1×1 MLS
 * group (Phase 1.16e5).
 *
 * The `:id` URL segment is a **base64url-without-padding**
 * encoding of the MLS `group_id` bytes (matches the
 * `paths.chat(...)` helper). On mount we decode the segment,
 * look up the live `<ChatProvider>` registry, and render the
 * timeline. If the group isn't registered (typical case: the
 * page was opened directly via URL with no in-memory state —
 * Phase-1 doesn't persist MLS groups across reloads), we
 * surface a "group not found, return to /contacts" panel.
 *
 * Composer + timeline are largely unchanged from the inline
 * `ChatPane` that 1.16e4 shipped in `Contacts.tsx`. Two
 * additions:
 *
 *   * Full-page layout (the `AppShell` <main> takes care of
 *     scrolling). Timeline grows with the viewport rather than
 *     being clamped at 16rem.
 *   * **Ack indicators** on outbound bubbles — `pending` while
 *     the POST to the DS is in flight, `sent` once the DS has
 *     assigned an id (the publish call returns). Cross-peer
 *     "delivered" / "seen" markers need a Read-Ack ladder
 *     primitive — tracked as a follow-up.
 *
 * `EditChain` LWW for per-message drafts (the lakko-sync
 * surface) is also a follow-up — current composer is a plain
 * `<input>`, no edit history.
 */
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  on,
  onMount,
  onCleanup,
  Show,
  untrack,
  type JSX,
} from "solid-js";
import { A, useNavigate, useParams, useSearchParams } from "@solidjs/router";
import { conversationSeedOf, useAuth } from "../auth/AuthContext";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { groupIdHex as hexOf } from "../devices/sync/state";
import { useCallHost } from "../calls/CallHostContext";
import { QuietIncomingCallPanel } from "../calls/QuietIncomingCallPanel";
import { quietCallOfferFor, type QuietCallOffer } from "../calls/quiet-call-offer";
import { callOwnsCaptureNow } from "../calls/capture-rule";
import { useBackgroundReceive } from "../chat/BackgroundReceiveContext";
import {
  appendToGroupSlot,
  eventToPersistedMessage,
  mergeForegroundSlot,
  systemRowText,
} from "../chat/background-log";
import {
  forwardJumpRow,
  forwardJumpTarget,
  forwardPayloadOf,
  forwardRefFromPersisted,
  persistedFromForwardRef,
} from "../chat/forward";
import { ForwardPicker } from "../chat/ForwardPicker";
import type { ForwardPickerRow } from "../chat/forward-picker";
import {
  bulkDeleteConfirmation,
  deleteDecision,
  enterSelection,
  forwardEnabled,
  leaveSelection,
  oldestFirst,
  progressLabel,
  selectionCountLabel,
  toggleSelected,
  withoutDone,
  type Selection,
} from "../chat/selection";
import {
  persistedFromReplyRef,
  replyRefFromPersisted,
  replySnapshotFor,
  type ReplySnapshot,
} from "../chat/reply-snapshot";
import { opsFromPersisted, persistedWithOps } from "../chat/message-ops-store";
import { sendFailureNotice, withSendAck } from "../chat/send-failure";
import { memberKeysOf, peerName } from "../chat/peer-name";
import {
  ownPersonKeyB64,
  peopleCountOf,
  personKeyOf,
  personKeysOf,
  type RosterLeaf as RosterLeafView,
} from "../chat/person-key";
import { groupTitle, isUnnamedGroupLabel } from "../chat/group-title";
import { UNNAMED_MEMBER, memberListLabelOf, memberNameOf } from "../profile/member-label";
import { pendingJoinLabel } from "../chat/pending-join-label";
import { JoinAttributionGate } from "../chat/join-attribution-gate";
import { groupCallsFor } from "../calls/group-call-store";
import { chatCallAffordance, type ChatCallAffordance } from "../calls/group-call-host";
import {
  NO_GROUP_CALLS,
  freshParticipantCount,
  selectLiveGroupCall,
  type GroupCallViewer,
  type GroupCallRecord,
  type GroupCallRecords,
} from "../calls/group-call-record";
import { loadWasm as loadWasmForCalls } from "../wasm";
// ADR-0045 UX §6 — the core's own answer to *will the identical call work
// later*. Imported as a named export rather than through `loadWasm` because
// `formatError` is synchronous; `src/wasm.ts` imports the same module, so
// this is the same instance and not a second one.
import { membershipRefusalRetryMaySucceed as wasmMembershipRefusalRetryMaySucceed } from "../../wasm/lakko_ffi_wasm";
import {
  loadMessageLog,
  MESSAGE_LOG_VERSION,
  updateMessageLog,
  type PersistedMessage,
} from "../chat/message-log-store";
import { getDeviceKey } from "../auth/key-store";
import {
  base64Decode,
  base64Encode,
  base64UrlDecodeNoPad,
  base64UrlEncodeNoPad,
} from "../auth/account-id";
import { ChatClient, HeldForSuccessor, type PollMessagesResult } from "../chat/client";
import { isDsNetworkFailure } from "../ds/client";
import { dsAccess, dsClockSkewNotice } from "../ds/access";
import {
  createWakeLatch,
  LONG_POLL_WAIT_SECS,
  NETWORK_BACKOFF_MS,
  runLongPollLoop,
  sleepCancellable,
} from "../ds/long-poll-loop";
import { ContactsClient, ContactsError, SupersessionRefused } from "../contacts/client";
import { supersessionRefusedMessage } from "../contacts/supersession-message";
import { DiscoveryClient } from "../discovery/client";
import {
  accountIdB64OfKey,
  loadAccepted,
  loadKeyAliases,
  loadSupersededKeys,
  subscribeAcceptedChanged,
} from "../contacts/local-store";
import { useGroupClosed } from "../chat/GroupClosedContext";
import { useLineage } from "../chat/use-lineage";
import { awaitedSwaps, setAwaitedSwaps } from "../chat/continuity-store";
import { subscribeMessageLogChanged } from "../chat/message-log-events";
import {
  HELD_SEND_GIVE_UP_MS,
  REPLY_NO_JUMP_TARGET,
  flushedRowsToShow,
  heldVerdict,
  identityCheckDue,
  opRoute,
  replyForSend,
  sendRoute,
  type OpRoute,
} from "../chat/held-send";
import {
  heldSends,
  persistHeldSends,
  restoreHeldSends,
  subscribeHeldFlushed,
  type HeldMessage,
} from "../chat/held-send-queue";
import { heldResendPredicate } from "../chat/resend-receive";
import { unrepairableBy } from "../chat/successor-claim";
import { READMISSION_INTERVAL_MS } from "../chat/device-readmission";
import {
  encodeReadPosition,
  historyRowId,
  historyRowOrigin,
  isHistoryRowId,
  laterSegmentExists,
} from "../chat/lineage";
import { dropsFromSupersededSender, type InertSender } from "../chat/background-events";
import {
  deviceSwapAcrossCommits,
  dropsSupersededSender,
  supersededSenderInert,
  repairPlan,
  resetCase,
} from "../chat/reset-repair";
import {
  findPersonRecord,
  groupInfoSections,
  infoHeading,
  infoOpenLabel,
  infoOpening,
  infoTargetForKind,
} from "../chat/conversation-info";
import { uiRules } from "../chat/ui-rules";
import type { IncomingChatEvent } from "../chat/client";
import {
  attachmentRefFromPersisted,
  b64UrlDecode,
  persistedFromAttachmentRef,
} from "../chat/persisted-attachment";
// ADR-0047 — voice messages, video notes, the emoji picker and GIFs. Every
// rule lives in a plain module under `media/` (tested there); this file wires
// them into the timeline and the composer.
import { EmojiPicker } from "../media/EmojiPicker";
import type { MediaUpload } from "../media/MediaBubbles";
import {
  RecordButton,
  RecorderNotices,
  RecordingBar,
  VideoNoteOverlay,
  recorderBusy,
} from "../media/MediaComposer";
import { createMediaHub, mediaKey, type MediaHub } from "../media/media-hub";
import { describeImage, describeVideo, type VisualPreview } from "../media/visual-thumb";
import { createRecorder } from "../media/recording-controller";
import type { CaptureResult } from "../media/media-capture";
import { isSingleEmojiGrapheme } from "../media/emoji/emoji-index";
import { MAX_GIF_FULL_BYTES, fetchGifMedia, type GifItem } from "../media/gif-client";
import {
  composeEnvelope,
  composeForwardEnvelope,
  composeReplyEnvelope,
  encryptAndUploadBytes,
  reencryptAttachmentRef,
  composeEditEnvelope,
  composeDeleteEnvelope,
  composeReactionEnvelope,
  composeTypingEnvelope,
  composeReadReceiptEnvelope,
  downloadAndDecryptRef,
  encryptAndUploadFile,
  formatAttachmentSize,
  type ForwardOrigin,
  type ForwardRef,
  type ReplyRef,
  saveDecryptedBytes,
  type AttachmentMediaMeta,
  type AttachmentRefBytes,
  type ThumbRefBytes,
  AttachmentSizeError,
  AttachmentMissingError,
  MAX_ATTACHMENT_BYTES,
} from "../chat/attachments";
import { useChats, type ActiveGroup } from "../chat/ChatContext";
import { recordRemovedDevices } from "../chat/device-readmission";
import { isOneToOne, useProfiles } from "../profile/ProfileContext";
import { intakeScopedEnvelope } from "../reach/scoped-intake";
import { useGroupNames } from "../chat/GroupNameContext";
import { usePins } from "../chat/PinContext";
import { useSharedChannels } from "../discover/SharedChannelsContext";
import { ShareChannelsSwitch } from "../discover/ShareChannelsSwitch";
import {
  clampPinIndex,
  nextPinIndex,
  historyRowOffersUnpin,
  pinBarItems,
  pinRoute,
  pinsAcrossSegments,
  pinTargetOfRow,
  type PinTarget,
  type HeldForPin,
} from "../chat/pin-bar";
import { pinRowDecision, type PinOption, type PinState } from "../chat/pin-sheet";
import { PinList, PinnedBar, type PinBarEntry } from "../chat/PinnedBar";
import { PinSheet } from "../chat/PinSheet";
import { useReadWatermarks } from "../chat/ReadWatermarkContext";
import { useCallLog } from "../calls/CallLogContext";
import { callBackKind, type CallLogEntry } from "../calls/call-log";
import { planCallStart } from "../calls/call-start-flow";
import { mergeConversationEntries } from "../chat/call-timeline";
import {
  GROUP_NAME_LABEL,
  GROUP_NAME_ONLY_ADMINS,
  GROUP_NAME_SAVE,
  groupNameErrorCode,
  groupNameErrorText,
} from "../chat/group-name-store";
import { observeMembership } from "../profile/membership-baseline";
import { isOwnKeyB64 } from "../profile/own-key";
import { createUsernameCheck } from "../profile/username-field";
import { mayLookUp, usernameReason } from "../profile/rules";
import {
  ensureGroupPushRouting,
  memberSignatureKeys,
  publishRemoveThenRotate,
  pushGidForGroup,
  readPushRotationRole,
  receivePushRoutingSecretGated,
  redistributeSecretToGroup,
  retireOldPushGid,
  rotatePushRoutingSecret,
  rotatePushRoutingSecretIfMembershipShrank,
  type PushRotationOutcome,
} from "../push/group-routing";
import { getGroupRoutingSecretForAccount } from "../push/group-routing-store";
import { createPushWakeReach } from "../push/wake-reach";
import { mayParkReceive } from "../chat/receive-activity";
// ADR-0045 (G-38 residual 1) — proposal-based membership. The committer sweep
// lives in `chat/sweep.ts` and the panel's state rules in
// `chat/pending-requests.ts`, both plain modules, because `vitest.config.ts`
// collects `src/**\/*.test.ts` in a `node` environment with no jsdom and no
// Solid transform — a rule that lived in this `.tsx` file could not be tested
// at all.
import type { SweepOutcome } from "../chat/sweep";
import {
  foldPendingRequests,
  loadObservations,
  pendingNotice,
  proposalKey,
  pruneObservations,
  recordObservation,
  rowAnnotation,
  visibleRequests,
  waitingQueueLine,
  waitingToBeAdded,
  type PendingRequest,
  type ProposalRecordView,
} from "../chat/pending-requests";
// ADR-0045 UX §6 / §12 item 6 — the typed membership-refusal table. The
// reason travels on the thrown error as `refusalReason`; this module turns it
// into a sentence and asks the CORE whether waiting helps. Nothing here reads
// a message, which is the whole point of the table existing.
import {
  JOIN_REQUEST_NOT_KEPT,
  externalJoinAffordance,
  membershipRefusalOf,
} from "../chat/membership-refusal";
// ADR-0045 UX §7 — the automatic re-send of a request a full queue refused.
// The decision (whether to arm, when to fire, when to stop) lives in the
// plain module and is tested there; this file supplies the group state it
// reads and re-issues what it decides to fire.
import {
  armRetry,
  armedRetries,
  armedApproveJoinRetry,
  armedRetryFor,
  claimArmedRetry,
  retryKey,
  retryScopeKey,
  retryVerdict,
  runSupersededRerequests,
  type GroupSnapshot,
  type RetryableRequest,
  type RosterMemberView,
} from "../chat/membership-retry";
// ADR-0045 S7 residual 1 — the request that left the pending set without
// landing. Same split, same reason as above.
import {
  dismissUnapplied,
  loadUnapplied,
  unappliedHeading,
  unappliedLine,
  UNAPPLIED_EXPLANATION,
  type UnappliedRequest,
} from "../chat/unapplied-requests";
// ADR-0050 section 2.5a point 4 — re-requesting a superseded own request.
import {
  readIntentSnapshot,
  readRosterViews,
  reissueOwnRequest,
} from "../chat/rerequest-membership";
// ADR-0045 UX §6.1 — ownership transfer, the owner's way out. Same split and
// the same reason: every rule and every string lives in the plain module, this
// file only renders what it decides.
import {
  NEW_OWNER_DISMISS,
  OUTGOING_OWNER_OPTIONS,
  PRESELECTED_OUTGOING_OWNER,
  TRANSFER_IRREVERSIBLE_BODY,
  TRANSFER_IRREVERSIBLE_HEADLINE,
  TRANSFER_UNLOCKS_LEAVE,
  exitBlock,
  loadLastObservedOwner,
  newOwnerNotice,
  rememberObservedOwner,
  transferActionLabel,
  transferCandidates,
  transferOutcomeNotice,
  type ExitBlock,
  type NewOwnerNotice,
  type OutgoingOwnerChoice,
  type TransferCandidate,
} from "../chat/ownership-transfer";
// ADR-0045 S4b — the committer liveness beacon. Same split as above: every
// rule (the emit gate, the rank-at-verdict-time projection, the observation
// store) lives in `chat/committer-beacon.ts` where it is host-testable, and
// this file only drives it.
import {
  BEACON_TICK_MS,
  beaconEmitVerdict,
  decideActingCommitter,
  deviceCommitterRank,
  wasmActingCommitterFold,
  recordBeaconObservation,
  type ActingCommitter,
} from "../chat/committer-beacon";
// ADR-0045 UX §6.2 — the end of your own membership. Same split, same reason:
// the state derivation, every string, the composer's replacement, the durable
// `leaveRequestedAt` record and the one-shot teardown list all live in the
// plain module where they are executable; this file renders and drives them.
import {
  // The delete-chat TITLE and BODY are no longer read here: they reach the
  // confirmation through `chat/delete-chat-confirm.ts`, which is also where
  // the action label that names the chat is decided. Only the CONTROL label
  // (the button in the exit block) is rendered by this file.
  DELETE_CHAT_CONTROL,
  FROZEN_ROSTER_HEADER,
  composerState,
  loadLeaveRequestedAt,
  membershipEnded as membershipHasEnded,
  claimTeardownSteps,
  membershipState,
  messageActionsState,
  readIsActive,
  recordLeaveRequested,
  releaseTeardownStep,
  terminalRow,
  type EndedTeardownStep,
  type MembershipState,
} from "../chat/membership-ended";
import { removeMemberConfirmation } from "../chat/remove-member-confirm";
import { deleteChatConfirmation } from "../chat/delete-chat-confirm";
import { deleteConversation } from "../chat/delete-chat";
import { leaveGroupConfirmation } from "../chat/leave-group-confirm";
import { namedActionKey, pendingConfirm, requestConfirm } from "../ui/confirm-dialog";
import { currentLanguage, joinList, t } from "../l10n";
import { KeyedError, RenderedError, keyedErrorText } from "../l10n/keyed-error";
import { createLiveText } from "../l10n/live-text";
import {
  applyOp,
  editDeleteAuthorized,
  OWN_REACTION_SENTINEL,
  type MessageOpState,
} from "../chat/message-ops";
import {
  authorIdentityToPersisted,
  persistedAuthorIdentity,
  type MemberIdentity,
} from "../chat/author-identity";
import {
  foldTyping,
  activeTypers,
  pruneTyping,
  isReadByEveryOther,
  shouldSendTyping,
  shouldSendReadReceipt,
  TYPING_EXPIRY_MS,
  type TypingMap,
  type ReadWatermarkMap,
} from "../chat/presence-ops";
import { paths } from "./paths";
import { Avatar } from "../ui/Avatar";
import {
  accountIdBytesFromHex,
  takePendingGroupSetup,
  type PendingGroupSetup,
} from "../chat/pending-group-setup";
import { Icon } from "../ui/icons";
import { ModalLayer } from "../ui/ModalLayer";
// docs/ux/client-redesign.md section 5.2 — the row type and the bubble
// renderer live under `chat/`; this file computes the layout (date
// separators, sender runs) through `timeline-layout.ts` and renders the list.
import {
  rowAuthorIdentity,
  rowCreatedAt,
  rowIdentity,
  asHistoryRow,
  rowMessageId,
  dedupByMessageId,
  type TimelineRow,
} from "../chat/timeline-row";
import { TimelineBubble } from "../chat/TimelineBubble";
import { CallTimelineRow } from "../chat/CallTimelineRow";
import {
  layoutTimeline,
  type TimelineItem,
  type TimelineLayout,
} from "../chat/timeline-layout";
import { conversationKindOf, type ConversationKind } from "../chat/conversation-kind";
import { MembersCanFindMeSwitch } from "../settings/MembersCanFindMeSwitch";

/**
 * `/chat/:id` (from the Chats list) and `/groups/:id` (from the Groups list)
 * both render this; `backTo` is the list the conversation was opened from, so
 * Back returns there (docs/ux/client-redesign.md section 3).
 */
/** One attribution gate for the whole page, so the per-group 10 s floor holds
 *  across remounts of the chat view (ADR-0053, `chat/join-attribution-gate.ts`). */
const joinAttributionGate = new JoinAttributionGate();

/** When this page last ran the identity check of a one-to-one's peer, by
 *  conversation id (R.2 "Triggers": opening a one-to-one, and a send whose
 *  last check is older than the re-admission interval). */
const lastPeerIdentityCheckAtMs = new Map<string, number>();
/** How long a send waits for that check before it goes ahead. Chosen: long
 *  enough for one directory fetch, short enough to be unnoticed. A check that
 *  answers later still takes effect (the re-send covers what slipped by). */
const SEND_IDENTITY_CHECK_WAIT_MS = 3000;

export default function Chat(props: { backTo?: string }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const params = useParams<{ id: string }>();
  const navigate = useNavigate();

  // Decode the URL slug into a groupId base64-standard (the
  // ChatProvider's registry key).
  //
  // 1.18e: the inline base64url-no-pad → standard-base64
  // conversion that lived here since 1.16e5 is now the shared
  // `base64UrlDecodeNoPad` helper (added in 1.18c next to its
  // encode-side sibling). Folding the duplicate keeps both
  // call sites — `/chat/:id` and `/i/:slug` — converging on
  // one tested decoder.
  const groupIdB64 = (): string | null => {
    try {
      const bytes = base64UrlDecodeNoPad(params.id);
      return base64Encode(bytes);
    } catch {
      return null;
    }
  };

  // ADR-0073 R.5 — the route names a CONVERSATION: the id of the lineage's
  // root, or of any of its groups (a link from before the move). What opens
  // is the conversation's current group; the view is keyed on that group, so
  // a conversation that moves to a successor remounts onto it in place.
  const lineageView = useLineage();
  const entry = (): ActiveGroup | undefined => {
    const id = groupIdB64();
    if (id === null) return undefined;
    return lineageView.resolve(id)?.head ?? chats.get(id);
  };

  // Identity gate — the chat needs a fresh export key in
  // memory to derive Identity for processIncoming /
  // createApplicationMessage.
  const requireIdentity = (): NonNullable<
    ReturnType<typeof auth.identitySecrets>
  > => {
    const s = auth.identitySecrets();
    if (s === null) throw new Error("identity unavailable");
    return s;
  };

  return (
    <article class="lk-chat">
      <Show
        when={auth.identitySecrets() !== null}
        fallback={<NeedsReauth />}
      >
        {/* Keyed on the group id: navigating from one chat straight to another
            (the call banner, a notification) re-uses this route component, and
            an unkeyed view would carry one chat's timeline, cursor and receive
            loop into the next. The id, not the entry object, is the key — the
            registry replaces the entry on an ADR-0040 persist-abort reset, which
            must NOT remount the view. */}
        <Show
          when={entry()?.groupIdB64 ?? null}
          keyed
          fallback={<GroupNotFound />}
        >
          {(_id) => (
            <ChatView
              entry={entry() as ActiveGroup}
              requireIdentity={requireIdentity}
              onLeave={() => navigate(props.backTo ?? paths.home)}
            />
          )}
        </Show>
      </Show>
    </article>
  );
}

// ---------------------------------------------------------------
// Main view
// ---------------------------------------------------------------

function ChatView(props: {
  entry: ActiveGroup;
  requireIdentity: () => NonNullable<
    ReturnType<ReturnType<typeof useAuth>["identitySecrets"]>
  >;
  onLeave: () => void;
}): JSX.Element {
  // 1.16e6c2: wire the canonical Identity + post-mutation
  // snapshot persist into ChatClient so `sendMessage` /
  // `pollMessages` flush MLS state to IDB on every successful
  // op. Closures read the auth + chat contexts lazily so they
  // always see the current session.
  const auth = useAuth();
  const chats = useChats();
  // The header's way to a contact's own page (`openInfo`): a one-to-one's
  // info is the contact screen on the Contacts route, not a layer here.
  const navigate = useNavigate();
  // ADR-0048 — profile names: the one display resolver, the received-profile
  // store the receive loop feeds, and the publish triggers.
  const profiles = useProfiles();
  const groupNames = useGroupNames();
  const pins = usePins();
  const sharedChannels = useSharedChannels();
  // ADR-0024 — the durable read watermarks both receive paths feed.
  const readWatermarkStore = useReadWatermarks();
  const callLog = useCallLog();
  // The app-level 1x1 call slot and signaling queue (`calls/CallHostContext`):
  // calls ring and survive on every screen, not just inside this view.
  const callHost = useCallHost();
  // The hand-off with the background receive for the chats that are not open
  // (`chat/receive-coordinator.ts`): one shared journal cursor per group, who
  // polls it, and the events it applied while this view was not mounted.
  const receive = useBackgroundReceive();
  // Claimed synchronously, before the first effect: from here on the
  // background sweep leaves this group to this view's own long poll.
  const releaseForeground = receive.coordinator.claimForeground(props.entry.groupIdB64);
  onCleanup(releaseForeground);
  // Phase 2a3e2b-web — cross-device sync for per-thread state
  // (drafts now; flags / order / read cursors follow). Used
  // below to seed the composer from sibling-device drafts
  // and to broadcast composer edits back out.
  const sync = useDeviceGroupSync();
  // ADR-0073 R.5 — the CONVERSATION this group belongs to. Everything a
  // person attaches to a conversation (the draft, the read position, call
  // history) is keyed on its id and never rewritten when the conversation
  // continues in a successor group; everything protocol-level keeps the
  // group id above.
  const lineageView = useLineage();
  const conversationIdBytes = createMemo(() =>
    lineageView.conversationIdOf(props.entry.group.groupId),
  );
  /** The older groups of this conversation this device still holds, oldest
   *  first: read-only history above the live timeline. */
  const historySegments = createMemo(
    (): readonly ActiveGroup[] => lineageView.resolve(props.entry.groupIdB64)?.history ?? [],
  );
  const chat = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: async () => {
      const s = auth.state();
      if (s.kind !== "session") return;
      await chats.persistSnapshot(s.account.accountId);
    },
    // ADR-0042 — route the notify wake under the opaque `push_gid` instead
    // of the leaky DS `group_id`. Returns null when no secret is held yet
    // (→ the wake is skipped; recipients catch up on foreground poll).
    pushGidResolver: async (groupId) => {
      const s = auth.state();
      if (s.kind !== "session") return null;
      return pushGidForGroup(s.account.accountId, base64Encode(groupId));
    },
  });
  // 1.18g — ContactsClient is the home of the addMemberToGroup
  // flow (resolves @username via discovery + drives the MLS
  // addMember + ships Welcome). Same canonical-Identity +
  // persist plumbing as ChatClient — both end up calling
  // `chats.persistSnapshot` after every mutation.
  // The discovery client reads the session token at request time: the group
  // invite upload and revoke sit behind discovery's `require_session`, and a
  // tokenless client made every upload a 401 that fell back to the inline link.
  const contacts = new ContactsClient(
    new DiscoveryClient(undefined, {
      sessionToken: () => auth.identitySecrets()?.sessionToken ?? null,
    }),
    undefined,
    {
      identityProvider: () => chats.identity(),
      onPersist: async () => {
        const s = auth.state();
        if (s.kind !== "session") return;
        await chats.persistSnapshot(s.account.accountId);
      },
      // ADR-0048 §3 — an add-by-username lookup proves the binding a username
      // verification would check.
      onUsernameResolved: (key, username) => profiles.learnUsername(key, username),
      // ADR-0040 (G-35) — the FAIL-CLOSED persist the four commit paths run BEFORE
      // publishing: the outbox entry + the N+1 snapshot in one atomic write. A
      // throw here (no session, no device key, IDB failure) aborts the commit so
      // the group is never advanced on the wire without a re-publishable record.
      onPersistWithCommit: async (entry) => {
        const s = auth.state();
        if (s.kind !== "session") {
          throw new Error("no active session — commit aborted (ADR-0040)");
        }
        await chats.persistSnapshotWithCommit(s.account.accountId, entry);
      },
      // ADR-0040 F1 (lakko-security) — on a FAIL-CLOSED commit persist-abort, reset
      // the in-memory group back to the last durable snapshot (epoch N), dropping
      // the un-published eager merge. Reloads the group from IDB + replaces the
      // cached registry reference so the next `props.entry.group` read (reactive
      // via `chats.get`) sees the epoch-N group — a subsequent commit builds on N,
      // not the poisoned N+1 that would strand the whole group.
      onCommitPersistAbort: async (groupIdB64) => {
        const s = auth.state();
        if (s.kind !== "session") {
          throw new Error("no active session — cannot reset group (ADR-0040 F1)");
        }
        await chats.resetGroupToDurableSnapshot(s.account.accountId, groupIdB64);
      },
    },
  );

  const [rows, setRows] = createSignal<TimelineRow[]>([]);
  /** docs/ux/calls.md section 6 — the calls this device was in, in this
   *  conversation. Local to this device and never on the wire; the store is
   *  `calls/call-log-store.ts`, so they survive a reload the way the
   *  timeline's own rows do. */
  const callRows = (): readonly CallLogEntry[] => [
    // ADR-0073 R.5 — call history is the conversation's: the calls of its
    // earlier groups stay in the timeline.
    ...historySegments().flatMap((g) => callLog.of(g.groupIdB64)),
    ...callLog.of(props.entry.groupIdB64),
  ];
  /** ADR-0073 R.5 — the stored rows of the conversation's OLDER groups,
   *  under their on-screen history ids (`lineage.ts`). Display-only: never
   *  in `rows`, so never persisted into this group's log, never counted by
   *  the read position, and never the target of an op. */
  const [historyRows, setHistoryRows] = createSignal<TimelineRow[]>([]);
  /** The edit / delete / reaction overlay of those rows, keyed like them. */
  const [historyOps, setHistoryOps] = createSignal<Map<number, MessageOpState>>(new Map());
  /** The conversation as the reader sees it: the journal's rows and this
   *  device's call rows, in time order (`chat/call-timeline.ts`). Merged
   *  BEFORE the layout, so a call on a new day carries that day's separator
   *  and a call between two of someone's messages breaks their run. */
  // `rows` itself, not `rows()`: an entry's `row` is a LIVE read, so a row that
  // is replaced (an ack flip, an upload landing) updates the bubble it belongs
  // to instead of replacing it — `chat/call-timeline.ts` states why that is
  // load-bearing and what it cost before.
  // One DS id, one bubble (`dedupByMessageId`): a row can reach this list by
  // more than one route, and the list is where every route meets.
  const uniqueRows = createMemo(() => dedupByMessageId([...historyRows(), ...rows()]));
  const timelineEntries = createMemo(() =>
    mergeConversationEntries(uniqueRows, callRows(), rowCreatedAt, rowIdentity),
  );
  // docs/ux section 5.2 — date separators + sender runs, one layout record
  // per entry (`chat/timeline-layout.ts`). An outgoing row and an incoming row
  // from this person's other device (ADR-0050) are both "self", so they run
  // together on the right; every other incoming row runs by its sender key.
  const timelineLayout = createMemo<TimelineLayout[]>(() => {
    const own = ownPersonKeyB64(chats.identity()) ?? "self";
    return layoutTimeline(
      timelineEntries().map((entry): TimelineItem => {
        if (entry.kind === "call") {
          // Nobody said it, so it joins no run and is centred.
          return {
            createdAt: new Date(entry.call.startedAtMs).toISOString(),
            senderKey: undefined,
            side: "system",
          };
        }
        const row = entry.row;
        if (row.kind === "outgoing") {
          return { createdAt: row.createdAt, senderKey: own, side: "outgoing" };
        }
        const ev = row.event;
        if (ev.kind === "application") {
          const fromSelf = ev.senderKey !== undefined && ev.senderKey === own;
          return {
            createdAt: ev.createdAt,
            senderKey: fromSelf ? own : ev.senderKey,
            side: fromSelf ? "outgoing" : "incoming",
          };
        }
        return { createdAt: rowCreatedAt(row), senderKey: undefined, side: "system" };
      }),
      new Date(),
      currentLanguage(),
    );
  });
  // The journal cursor. The authority is the coordinator's per-group value,
  // shared with the background receive: a poll that ran for this group while
  // the view was not mounted has already moved it, and a view that started from
  // its own older copy (the debounced log write may not have landed) would
  // re-fetch consumed rows that can never decrypt again. The local signal only
  // makes the value reactive for the persist effect.
  const [cursorSignal, setCursorSignal] = createSignal(0);
  const cursor = (): number =>
    Math.max(cursorSignal(), receive.coordinator.cursor(props.entry.groupIdB64) ?? 0);
  const setCursor = (to: number): void => {
    const next = receive.coordinator.noteCursor(props.entry.groupIdB64, to);
    setCursorSignal(Math.max(cursorSignal(), next));
  };

  // ADR-0023 — per-message op overlay (edit / delete / reaction).
  // Keyed on the target's DS `messageId`. In-memory + session-
  // scoped: multi-device convergence (the persisted-log replay
  // of ops) is deferred to Phase 2, so a reload starts the
  // overlay empty. The render path reads it by messageId and
  // folds it onto the bubble; the receive loop folds incoming
  // ops into it; the send handlers apply the own-op locally
  // (the sender never receives its own application message
  // back). A fresh Map replaces the signal on every mutation so
  // SolidJS re-renders the affected bubbles.
  const [msgOps, setMsgOps] = createSignal<Map<number, MessageOpState>>(
    new Map(),
  );
  // Inline-edit composer state — the messageId currently being
  // edited (or `null`), plus the working text.
  const [editingId, setEditingId] = createSignal<number | null>(null);
  const [editText, setEditText] = createSignal("");
  // ADR-0052 / message-actions.md section 3.1 — the reply the composer is
  // armed with: the target's DS id and the snapshot taken when Reply was
  // chosen. Session-scoped by construction: this view is keyed on the group
  // id, so switching conversation remounts it and the strip goes with it (a
  // reply is not a draft that follows the person). Replying and editing are
  // exclusive - each start clears the other - and a send, Escape and the
  // strip's close control all clear it.
  const [replyingTo, setReplyingTo] = createSignal<(ReplySnapshot & { target: number }) | null>(
    null,
  );
  /** Reply was chosen on `messageId`: snapshot the message AS IT IS NOW (the
   *  edited text of an edited message, the kind of a media message), arm the
   *  strip, end any open edit, focus the composer. */
  function onStartReply(messageId: number): void {
    const row = rows().find((r) => rowMessageId(r) === messageId);
    if (row === undefined) return;
    const original = row.kind === "outgoing" ? row.plaintext : row.event.kind === "application" ? row.event.text : "";
    const attachments = row.kind === "outgoing" ? row.attachments : row.event.kind === "application" ? row.event.attachments : [];
    const text = msgOps().get(messageId)?.editedText ?? original;
    // The name is whatever the timeline already calls that person: "You" for
    // an own message (this device's or this person's other device, ADR-0050),
    // else the one display resolver, else the same fallback the bubble draws.
    const snapshot = replySnapshotFor({ text, attachments }, authorLabelOf(row), (a) => hub.decision(a).bubble);
    setEditingId(null);
    setReplyingTo({ target: messageId, ...snapshot });
    composerInput?.focus();
  }
  /** Take the armed reply for a send and clear the strip in the same step, so
   *  two sends can never carry one reply. */
  function takeArmedReply(): ReplyRef | undefined {
    const armed = replyingTo();
    if (armed === null) return undefined;
    setReplyingTo(null);
    return { target: armed.target, author: armed.author, excerpt: armed.excerpt };
  }
  // ADR-0052 decision 2 / message-actions.md section 5.1 — the message the
  // "Forward to" picker is open for, by DS id; `null` when the picker is
  // closed. Session-scoped like the reply strip: this view is keyed on the
  // group id, so switching conversation drops it.
  const [forwardingId, setForwardingId] = createSignal<number | null>(null);
  // message-actions.md section 8 — selection mode. The mode and the set are
  // two signals: deselecting the last row leaves the person IN the mode with
  // "0 selected" and both buttons disabled, which is what the spec's "enabled
  // when the selection is not empty" describes. Session-scoped like the reply
  // strip and the forward id: this view is keyed on the group id, so
  // navigating to another conversation remounts it and the selection goes
  // with it ("Leaving": nothing about a selection is persisted). Every rule
  // is `chat/selection.ts`; this is the wiring.
  const [selecting, setSelecting] = createSignal(false);
  const [selection, setSelection] = createSignal<Selection>(leaveSelection());
  /** Section 8 "Forward" — the picker is open for the whole selection (as
   *  against `forwardingId`, which opens it for one message). */
  const [bulkForwardOpen, setBulkForwardOpen] = createSignal(false);
  function enterSelectionMode(messageId: number): void {
    setSelection(enterSelection(messageId));
    setSelecting(true);
  }
  function leaveSelectionMode(): void {
    setSelecting(false);
    setSelection(leaveSelection());
    setBulkForwardOpen(false);
  }
  const toggleSelectedRow = (messageId: number): void => {
    setSelection(toggleSelected(selection(), messageId));
  };
  /** The selected messages, oldest first, with the one fact Delete needs:
   *  whether each is this person's own — the same answer the card gives
   *  Edit / Delete (an outgoing row; a row that is no longer here is not
   *  ours to delete). */
  const selectedItems = (): Array<{ id: number; own: boolean }> =>
    oldestFirst(selection()).map((id) => ({
      id,
      own: rows().find((r) => rowMessageId(r) === id)?.kind === "outgoing",
    }));
  const deleteState = createMemo(() => deleteDecision(selectedItems()));
  // Section 8 "Leaving" — Escape on web. On the capture phase, so it is
  // decided before whatever is stacked on top of the mode (the confirmation,
  // the picker) handles the same key: while one of those is open, Escape is
  // theirs and the selection stays as it was.
  createEffect(() => {
    if (!selecting()) return;
    const onKey = (e: KeyboardEvent): void => {
      if (e.key !== "Escape") return;
      if (pendingConfirm() !== null || bulkForwardOpen()) return;
      e.preventDefault();
      leaveSelectionMode();
    };
    document.addEventListener("keydown", onKey, true);
    onCleanup(() => document.removeEventListener("keydown", onKey, true));
  });
  // Both bulk actions compose application messages, so both die with Send
  // (ADR-0045 UX section 6.2.5); a mode whose two buttons can never act again
  // ends with the membership.
  createEffect(() => {
    if (selecting() && membershipEnded()) leaveSelectionMode();
  });
  /** The timeline's name for a row's author: "You" for an own message (this
   *  device's or this person's other device, ADR-0050), else the one display
   *  resolver, else the same fallback the bubble draws. Shared by the reply
   *  snapshot and the forward attribution so the two never name one person
   *  two ways. */
  function authorLabelOf(row: TimelineRow): string {
    const author = authorOf(row);
    return author.kind === "self"
      ? t("chat.author.you")
      : author.kind === "named"
        ? author.name
        : t("chat.author.unknown");
  }
  /** Who wrote a row: this person, a named member, or nobody known. The
   *  forward attribution branches on this rather than on the words, which
   *  are in the interface language. */
  function authorOf(
    row: TimelineRow,
  ): { kind: "self" } | { kind: "named"; name: string } | { kind: "unknown" } {
    const own = ownPersonKeyB64(chats.identity());
    const senderKey =
      row.kind === "incoming" && row.event.kind === "application" ? row.event.senderKey : undefined;
    if (row.kind === "outgoing" || (senderKey !== undefined && senderKey === own)) {
      return { kind: "self" };
    }
    return senderKey !== undefined
      ? { kind: "named", name: profiles.resolve(senderKey).name }
      : { kind: "unknown" };
  }
  /** The name a FORWARD carries for a row's author (spec section 5): the
   *  recipient reads it, so an own message names this account - the profile
   *  name, else the username - never "You", which is the timeline's word for
   *  the viewer and a lie on every other screen (seen live 2026-09-26 as
   *  "Forwarded from You" on the recipient's phone). An author the timeline
   *  cannot name yields no name at all, so the bubble reads "Forwarded". */
  function forwardAuthorLabelOf(row: TimelineRow): string | undefined {
    const author = authorOf(row);
    if (author.kind === "self") {
      const own = profiles.ownProfile().displayName?.trim();
      if (own) return own;
      const s = auth.state();
      return s.kind === "session" && s.account.username.trim() !== ""
        ? s.account.username
        : undefined;
    }
    return author.kind === "named" ? author.name : undefined;
  }
  /** Section 5.1 — the href of another conversation, routed by ITS kind the
   *  way the Chats list routes a row (a one-to-one under `/chat/`, a group
   *  under `/groups/`). */
  function hrefOfConversation(target: ActiveGroup): string {
    const kind = conversationKindOf(target.group);
    const slug = base64UrlEncodeNoPad(target.group.groupId);
    return kind === "chat" ? paths.chat(slug) : paths.groupChat(slug);
  }
  /**
   * Section 5.1 "What is sent" — a row of the picker was chosen for the
   * message `forwardingId` names. Compose the payload (the pure rule in
   * `chat/forward.ts`), re-encrypt every attachment under fresh keys so the
   * two conversations never share a blob (decision 2, OQ1), publish through
   * the TARGET group's own send path — `chat.sendMessage` takes that group's
   * MLS lock, like every send — record the sent row where the target's
   * timeline will find it, and go there. A throw keeps the picker open with
   * the error shown; nothing was sent.
   */
  async function forwardTo(target: ForwardPickerRow): Promise<void> {
    const messageId = forwardingId();
    if (messageId === null) return;
    const entry = chats.get(target.id);
    if (entry === undefined) throw new KeyedError("chat.error.conversation_gone");
    await forwardOne(messageId, entry);
    setForwardingId(null);
    // Into this very conversation the person is already looking at it.
    if (entry.groupIdB64 !== props.entry.groupIdB64) navigate(hrefOfConversation(entry));
  }
  /**
   * Section 8 "Forward" — the picker was chosen for the whole selection. One
   * forwarded message per selected message, oldest first, each composed
   * exactly as {@link forwardOne} composes a single forward. On a failure it
   * stops: the picker closes, the unsent messages stay selected, and the
   * notice says how far it got. On full success it leaves the mode and goes
   * to the target the way the single forward does.
   */
  async function forwardSelectionTo(target: ForwardPickerRow): Promise<void> {
    const entry = chats.get(target.id);
    if (entry === undefined) throw new KeyedError("chat.error.conversation_gone");
    const ids = oldestFirst(selection());
    const done: number[] = [];
    setError(null);
    for (const id of ids) {
      try {
        await forwardOne(id, entry);
      } catch {
        setSelection(withoutDone(selection(), done));
        setBulkForwardOpen(false);
        const forwarded = done.length;
        setError(() => progressLabel("forwarded", forwarded, ids.length));
        return;
      }
      done.push(id);
    }
    leaveSelectionMode();
    if (entry.groupIdB64 !== props.entry.groupIdB64) navigate(hrefOfConversation(entry));
  }
  /**
   * ONE forward of ONE message into `entry`: compose, re-encrypt, publish,
   * record. The single forward and the bulk one both run this, so the two
   * cannot compose a forwarded message two ways (section 8: what selection
   * does at the end is the one-message action repeated). Navigation and the
   * picker's state are the callers'. A throw means nothing was sent for THIS
   * message.
   */
  async function forwardOne(messageId: number, entry: ActiveGroup): Promise<void> {
    const row = rows().find((r) => rowMessageId(r) === messageId);
    if (row === undefined) throw new KeyedError("chat.error.message_gone");
    const original =
      row.kind === "outgoing" ? row.plaintext : row.event.kind === "application" ? row.event.text : "";
    const attachments =
      row.kind === "outgoing"
        ? row.attachments
        : row.event.kind === "application"
          ? row.event.attachments
          : [];
    const carried =
      row.kind === "outgoing" ? row.forward : row.event.kind === "application" ? row.event.forward : undefined;
    const payload = forwardPayloadOf(
      {
        text: original,
        editedText: msgOps().get(messageId)?.editedText,
        attachments,
        messageId,
        forward: carried,
      },
      forwardAuthorLabelOf(row),
      props.entry.groupIdB64,
      // ADR-0053 contract point 10 — a forward out of a one-to-one carries no
      // origin.
      conversationKind() === "chat",
    );
    // Sequential, not `Promise.all`: one failure must not leave the others
    // uploaded and orphaned, and a forward is rarely more than a few files.
    const fresh: AttachmentRefBytes[] = [];
    for (const a of payload.attachments) fresh.push(await reencryptAttachmentRef(a));
    const bytes = await composeForwardEnvelope(payload.text, fresh, payload.forward);
    let result: Awaited<ReturnType<typeof chat.sendMessage>>;
    try {
      result = await chat.sendMessage(props.requireIdentity(), entry.group, bytes);
    } catch (err) {
      // ADR-0073 R.7 case 1 — a forward INTO a superseded one-to-one is held
      // for that conversation's successor, with its re-encrypted references
      // and its attribution; the picker reports it sent, as a typed message
      // there shows the ordinary sending state.
      if (err instanceof HeldForSuccessor) {
        heldSends.hold(
          lineageView.conversationHexOf(entry.group.groupId),
          { text: payload.text, attached: fresh, reply: undefined, forward: payload.forward },
          Date.now(),
        );
        void persistHeldSends(accountIdB64ForLog());
        if (entry.groupIdB64 === props.entry.groupIdB64) resumeHeldSends();
        return;
      }
      throw new RenderedError(
        () =>
          dsClockSkewNotice(err) ??
          sendFailureNotice(membershipRefusalOf(err, membershipRetryMaySucceed)),
      );
    }
    // This device's signature key: one key for the device across every group
    // it is a leaf of (ADR-0050), so the key read off this roster names this
    // device in the target's roster too.
    const authorKey = ownSignatureKeyB64();
    if (entry.groupIdB64 === props.entry.groupIdB64) {
      // Into this very conversation: the row is the timeline's, acked at once
      // (the sender never receives its own message back), persisted by the
      // ordinary sweep.
      setRows([
        ...rows(),
        {
          kind: "outgoing",
          tempId: nextTempId++,
          plaintext: payload.text,
          attachments: fresh,
          ack: "sent",
          messageId: result.messageId,
          createdAt: result.createdAt,
          authorKey,
          forward: payload.forward,
        },
      ]);
      return;
    }
    // Into another conversation: the sent row goes straight into that
    // conversation's slot of the persisted log — the only record of an own
    // message, since the DS never echoes it back — so the view opened next
    // seeds from it. Awaited BEFORE navigating: the destination hydrates from
    // the log on mount.
    const s = auth.state();
    if (s.kind === "session") {
      const deviceKey = await getDeviceKey(s.account.accountId);
      if (deviceKey !== null) {
        const hex = hexOf(entry.group.groupId);
        const sent: PersistedMessage = {
          messageId: String(result.messageId),
          direction: "outgoing",
          text: payload.text,
          attachments: fresh.map((a) => persistedFromAttachmentRef(a)),
          createdAt: result.createdAt,
          authorKeyB64: authorIdentityToPersisted(authorKey),
          forward: persistedFromForwardRef(payload.forward),
        };
        await updateMessageLog(s.account.accountId, deviceKey, (existing) => {
          const groups = existing?.groups ?? {};
          return {
            version: MESSAGE_LOG_VERSION,
            groups: { ...groups, [hex]: appendToGroupSlot(groups[hex], [sent], 0) },
          };
        });
      }
    }
  }
  /**
   * Section 5.1 "Tap to jump" — the "Forwarded from" line was pressed. When
   * this device holds the origin conversation: this one, so scroll to the
   * original if the timeline holds it; another, so open it at that message
   * (`?at=`, honoured on mount below). Otherwise nothing happens and nothing
   * is said — an explanation would confirm the conversation exists.
   */
  function onOpenForwardOrigin(origin: ForwardOrigin | undefined): void {
    if (origin === undefined) return;
    const target = forwardJumpTarget({ origin }, (id) => chats.get(id) !== undefined);
    if (target === null) return;
    // ADR-0073 R.5 — the origin group may be an OLDER segment of a
    // conversation that has since continued in a successor: the jump goes to
    // the conversation (its current group), at the row the older segment's
    // message is drawn under.
    const view = lineageView.resolve(target.groupIdB64);
    const head = view?.head ?? chats.get(target.groupIdB64);
    if (head === undefined) return;
    const rowId = forwardJumpRow({
      originGroupIdB64: target.groupIdB64,
      messageId: target.messageId,
      headGroupIdB64: head.groupIdB64,
      heldHistory: (view?.history ?? []).map((g) => g.groupIdB64),
      rowId: historyRowId,
    });
    if (rowId === null) return;
    if (head.groupIdB64 === props.entry.groupIdB64) {
      jumpToMessage(rowId);
      return;
    }
    navigate(`${hrefOfConversation(head)}?at=${String(rowId)}`);
  }
  // The quick reaction row moved into the per-message action menu, which owns
  // its own open state (`TimelineBubble.tsx` / `MessageMenu.tsx`): one gesture
  // opens the reactions and the actions together, so the view no longer tracks
  // "whose reaction bar is open" separately from "whose menu is open".

  // ADR-0024 — ephemeral typing registry, keyed on the sender's MLS
  // leaf. Never persisted; a reload starts empty (a stale `true` row
  // re-downloaded on the next poll is dropped by the stale-ignore
  // guard in `foldTyping`). The receive loop folds incoming typing
  // signals in; an interval prunes expired entries; the indicator
  // above the composer reads `activeTypers`.
  const [typingMap, setTypingMap] = createSignal<TypingMap>(new Map());
  // ADR-0024 — monotone per-member read watermark: the sender's
  // signature key → the highest messageId that member has read.
  // Keyed on the identity, not the leaf: the watermark is monotone
  // and never reset on a membership change, so a leaf key handed a
  // departed member's whole read history to whoever was added into
  // their slot. Held in the session-wide durable store
  // (`chat/read-watermark-store.ts`), which the background receive
  // also feeds, so "Read" survives a reload and a chat switch. The
  // own-message bubble reads it to flip "Sent" → "Read"; a lower
  // value never lowers a higher one (re-sync idempotent).
  const readWatermarks = (): ReadWatermarkMap =>
    readWatermarkStore.of(props.entry.groupIdB64);

  // Phase 2a3f1-web — persistent message log. The plaintext
  // timeline survives reloads under the device root key in
  // IDB; without it, every refresh emptied the visible
  // history (MLS forward secrecy wipes the chain keys, so
  // there's no way to re-derive past plaintexts from the wire
  // bytes). The two stores (MLS snapshot ⊥ message log) ride
  // side-by-side keyed on the same `accountIdB64`.
  const accountIdB64ForLog = (): string | null => {
    const s = auth.state();
    if (s.kind !== "session") return null;
    return s.account.accountId;
  };
  const groupIdHexForLog = (): string => hexOf(props.entry.group.groupId);
  let didHydrateFromLog = false;
  let pendingPersistTimer: ReturnType<typeof setTimeout> | null = null;
  const PERSIST_DEBOUNCE_MS = 500;
  const [input, setInput] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  // ADR-0061 — held as how to render them, so a line already on screen
  // follows a language switch.
  const [error, setError] = createLiveText();
  const [notice, setNotice] = createLiveText();

  // Phase 3a8 — the 1x1 call surface moved to the app-level call host
  // (`calls/CallHostContext.tsx`): it rang only while this view was mounted
  // and ended when the user left it. The group (SFU) call followed it there.
  // Owned by this view, it was left on unmount — navigating to Home, Contacts
  // or another chat hung it up — and its mid-call rekey (ADR-0029) was an
  // effect over this view's `membersVersion`, so it stopped with the view. This
  // view now only asks the host for a call and reads the host's slot; a return
  // to the call's chat finds the host's overlay, never a second call.

  // Ongoing group calls this chat has heard announced (see
  // `calls/group-call-record.ts` for the contract shared with iOS/Android).
  const groupCalls = (): GroupCallRecords => {
    const s = auth.state();
    if (s.kind !== "session") return NO_GROUP_CALLS;
    // `calls/group-call-store.ts` — fed by the call host from BOTH receive
    // paths, so a start row consumed in the background is already here.
    return groupCallsFor(s.account.accountId, props.entry.groupIdB64);
  };
  // Liveness is time-based (a call with nobody in it stops being live 60 s
  // after it started), so the selector needs a clock that ticks.
  const [groupCallClock, setGroupCallClock] = createSignal(Date.now());
  const groupCallClockTimer = setInterval(() => setGroupCallClock(Date.now()), 10_000);
  onCleanup(() => clearInterval(groupCallClockTimer));
  /** This device's own entry counts only while it is in that call, so a
   *  session that died before a reload never reads as someone in the call. */
  const groupCallViewer = (): GroupCallViewer | undefined => {
    const id = chats.identity();
    if (id === null) return undefined;
    return {
      ownMemberKey: base64Encode(id.signaturePublicKey),
      inCallIdHex: callHost.groupCall()?.callIdHex ?? null,
    };
  };
  const liveGroupCall = createMemo<GroupCallRecord | null>(() =>
    selectLiveGroupCall(groupCalls(), groupCallClock(), groupCallViewer()),
  );

  /**
   * A call from THIS conversation, arriving while it is muted
   * (`docs/ux/calls.md` section 2.1.1).
   *
   * The host holds one quiet offer for the whole session, so the conversation
   * has to ask whether it is the one the call arrived in — and the rule that
   * answers that, along with the window that bounds it, lives in
   * `calls/quiet-call-offer.ts` rather than in this condition. The
   * conversation list asks the same question about each of its rows; two
   * components deciding it separately is how they come to disagree, and
   * neither of them is reachable from a test.
   */
  const quietCall = (): QuietCallOffer | null =>
    quietCallOfferFor(callHost.quietIncomingCall(), props.entry.groupIdB64, Date.now());

  /** Best name for this chat (ADR-0048 §5) — `chat/peer-name.ts`, shared with
   *  the call host's ringing banner. */
  /** The other member's signature key in a 1x1 chat, else `null`. */
  const peerKeyB64 = (): string | null => {
    if (memberCount() !== 2) return null;
    const ownB64 = ownPersonKeyB64(chats.identity());
    const peers = memberKeysOf(props.entry.group).filter((k) => k !== ownB64);
    return peers.length === 1 ? (peers[0] ?? null) : null;
  };
  /** Under the name (docs/ux section 5.2): "typing..." while a typing
   *  indicator is live, else the person's "@username" in a 1x1, else the size
   *  of the group in people. Which of the two follows the conversation kind, so
   *  a two-person group says "2 people" like the list it is filed under. */
  const headerSub = (): string => {
    const typing = typingIndicator();
    if (typing !== null && !membershipEnded()) return typing;
    const peer = isGroupChat() ? null : peerKeyB64();
    if (peer !== null) return profiles.resolve(peer).username ?? "";
    return t("chats.row.people", { count: memberCount() });
  };
  /**
   * The name at the top opens this conversation's info
   * (`docs/ux/conversation-info.md` section 3).
   *
   * What opens is decided by what the conversation is - a one-to-one opens
   * the person, a group opens the group - so the group's name, invite link,
   * roles, ownership and `Leave group` can no longer surface in a chat
   * between two people, where none of them mean anything.
   *
   * A group's info is a LAYER over the conversation, which stays where it is
   * behind the dim. A person's info IS the contact screen, and there is one
   * of it: the header goes to the contact's own page (`paths.contact`)
   * exactly the way the contact list opens it, Message included - from here
   * Message is just the way back. Until 2026-09-26 the chat rendered a
   * second copy of that card as a layer, with Message withheld, and the same
   * person had two cards. A one-to-one whose contact record cannot be found
   * (a roster that cannot be read) opens nothing at all (`infoOpening`):
   * every one-to-one peer is a contact by construction
   * (`contacts/adopt-one-to-one.ts`), so there is no "not a contact" card
   * left to draw.
   */
  function openInfo(): void {
    setHeaderMenuOpen(false);
    const opening = infoOpening(infoTarget(), personRecord());
    if (opening === "nothing") return;
    if (opening === "contact-page") {
      const record = personRecord();
      if (record === null) return;
      navigate(paths.contact(base64UrlEncodeNoPad(base64Decode(record.signaturePubkeyB64))));
      return;
    }
    setInfoOpen(true);
    setNotice(null);
    setError(null);
    // Bump version so the layer reads fresh data when it opens (covers the
    // case where new commits arrived between the last poll and this click).
    setMembersVersion(membersVersion() + 1);
  }

  /** One obvious way out, shared by the backdrop, `Esc` and the close
   *  button. The add-member form is part of the group info, so it closes
   *  with it rather than being left armed behind the conversation. */
  function closeInfo(): void {
    setInfoOpen(false);
    setAddMemberOpen(false);
  }

  const resolvePeerName = (): string => {
    const ownKeyB64 = ownPersonKeyB64(chats.identity());
    const memberKeysB64 = memberKeysOf(props.entry.group);
    // An unnamed group is titled from its members, never from an id.
    const others = memberKeysB64.filter((k) => k !== ownKeyB64);
    return peerName({
      ownKeyB64,
      memberKeysB64,
      resolveName: (k) => profiles.resolve(k).name,
      linkedContactName: loadAccepted().find((c) => c.groupIdB64 === props.entry.groupIdB64)
        ?.displayName,
      // ADR-0051 §5 — the shared name first, then the member-derived title.
      label: groupTitle(
        groupNames.nameOf(props.entry.groupIdB64) ?? props.entry.label,
        others.map((k) => memberNameOf(profiles.resolve(k))),
      ),
      // The conversation kind (ADR-0062) decides, not the size: a two-person group is
      // titled as a group, the same way the list files it.
      isGroup: isGroupChat(),
    });
  };

  /** What this chat's call buttons (and the in-chat "call in progress"
   *  banner) offer — `calls/group-call-host.ts`. */
  const callAffordance = (): ChatCallAffordance =>
    chatCallAffordance({
      membershipEnded: membershipEnded(),
      callInProgress: callHost.anyCallInProgress(),
      liveCallInThisChat: liveGroupCall() !== null,
      isGroup: isGroupChat(),
    });

  /** Place a 1x1 call. The session lives in the call host, so it survives
   *  leaving this chat. */
  async function startOneToOneCall(withVideo: boolean): Promise<void> {
    const busy = callHost.callBusyMessage("start");
    if (busy !== null) {
      setError(busy);
      return;
    }
    try {
      await callHost.dial(props.entry.groupIdB64, withVideo);
    } catch (err) {
      console.warn("[chat] could not start call", err);
      setError(() => t("chat.call.error.start"));
    }
  }

  /**
   * Start (`joinCallIdHex === null`) or join a group call in this chat. The
   * session belongs to the call host; this view only reports the outcome. A
   * join polls this chat first (`refresh`), so the seed is derived at the epoch
   * the call is at and a `CallEnd` already on the journal is seen.
   */
  async function requestGroupCall(joinCallIdHex: string | null, withVideo: boolean): Promise<void> {
    setGroupCallClock(Date.now());
    const outcome = await callHost.startGroupCall({
      groupIdB64: props.entry.groupIdB64,
      withVideo,
      joinCallIdHex,
      syncGroup: refresh,
    });
    switch (outcome.kind) {
      case "connected":
        return;
      case "busy":
        setError(outcome.message);
        return;
      case "ended":
        setNotice(() => t("chat.call.group_ended"));
        return;
      case "failed":
        setError(outcome.message);
        return;
    }
  }

  /**
   * The camera opened for docs/ux/calls.md section 3.2's camera check, or
   * `null` when no check is up. Holding the stream here rather than a boolean
   * is what lets the check open on a live picture instead of on a placeholder.
   */

  /** Place a call of the given media, routed to whichever kind of call this
   *  chat is. */
  function placeCall(withVideo: boolean): void {
    switch (callAffordance()) {
      case "join-group": {
        const live = liveGroupCall();
        if (live !== null) void requestGroupCall(live.callIdHex, withVideo);
        return;
      }
      case "start-group":
        void requestGroupCall(null, withVideo);
        return;
      case "start-one-to-one":
        void startOneToOneCall(withVideo);
        return;
      case "hidden":
        return;
    }
  }

  /**
   * The header's two call buttons (docs/ux/calls.md section 3).
   *
   * Both dial immediately — no preparation screen for either kind. Section 3.1
   * always said so for the phone button; section 3.2 said the same for the
   * video button from 2026-09-21, when its camera check was removed. The
   * self-view that check existed for is on the calling screen, so the caller
   * still sees themselves, a moment later and without a second press.
   */
  function onCallButton(withVideo: boolean): void {
    if (callAffordance() === "hidden") return;
    if (!withVideo) {
      placeCall(false);
      return;
    }
    void placeVideoCall();
  }

  /**
   * Place a video call, or the voice call this device can actually place.
   *
   * The camera is probed first and released again. That looks wasteful next to
   * dialling straight away, and it is deliberate: the 1x1 session asks for the
   * camera and the microphone in ONE `getUserMedia` and has no per-track
   * fallback of its own (unlike the SFU path's `capture-fallback.ts`), so on a
   * device with no camera the whole request fails and no call is placed at
   * all. Section 3.2 owes that person a voice call and one line saying why,
   * and the probe is what lets `planCallStart` say it. The probe's own stream
   * is stopped immediately; the session acquires its own.
   */
  async function placeVideoCall(): Promise<void> {
    let stream: MediaStream | null = null;
    try {
      stream = await navigator.mediaDevices.getUserMedia({ video: true });
    } catch {
      // No camera, or the permission was refused. Either way the call is still
      // placeable, just not as a video call.
      stream = null;
    }
    if (stream !== null) for (const track of stream.getTracks()) track.stop();
    const plan = planCallStart("video", stream !== null);
    if (plan.notice !== null) setNotice(plan.notice);
    placeCall(plan.media.video);
  }

  // A contact detail's Call / Video buttons open this chat with `?call=audio`
  // or `?call=video` (docs/ux/client-redesign.md section 5.3): the same header
  // buttons, pressed once the chat can offer a call. The parameter is dropped
  // from the URL first, so a reload or Back does not place a second call.
  const [searchParams, setSearchParams] = useSearchParams<{ call?: string; at?: string; reply?: string }>();
  // ADR-0052 decision 2 — `?at=<messageId>`: a forwarded message's "Forwarded
  // from" line in another conversation opened this one at its original. The
  // jump runs once the row is in the loaded timeline (the log hydrates
  // asynchronously), then the parameter is dropped so a reload or Back does
  // not jump again. A message this device does not hold is never found, and
  // then nothing happens and nothing is said (section 5.1).
  let atHandled = false;
  createEffect(() => {
    const at = searchParams.at;
    // The older segments' rows too: `?at=` may name one (ADR-0073 R.5).
    const held = uniqueRows();
    if (atHandled || at === undefined) return;
    const id = Number(at);
    if (!Number.isFinite(id)) {
      atHandled = true;
      setSearchParams({ at: undefined }, { replace: true });
      return;
    }
    if (!held.some((r) => rowMessageId(r) === id)) return;
    atHandled = true;
    setSearchParams({ at: undefined }, { replace: true });
    // The row exists in the list; give it a frame to exist in the DOM.
    requestAnimationFrame(() => jumpToMessage(id));
  });
  // docs/ux/feed.md section 5 — `?reply=<messageId>`: Reply chosen on a feed
  // item opens this conversation with the reply strip already set
  // (message-actions.md 3.1). Armed once the row is in the loaded timeline,
  // then the parameter is dropped so a reload or Back does not arm it again;
  // a message this device does not hold arms nothing.
  let replyHandled = false;
  createEffect(() => {
    const wanted = searchParams.reply;
    const held = rows();
    if (replyHandled || wanted === undefined) return;
    const id = Number(wanted);
    if (!Number.isFinite(id)) {
      replyHandled = true;
      setSearchParams({ reply: undefined }, { replace: true });
      return;
    }
    if (!held.some((r) => rowMessageId(r) === id)) return;
    replyHandled = true;
    setSearchParams({ reply: undefined }, { replace: true });
    onStartReply(id);
    requestAnimationFrame(() => jumpToMessage(id));
  });
  let autoCallHandled = false;
  createEffect(() => {
    const wanted = searchParams.call;
    if (autoCallHandled || (wanted !== "audio" && wanted !== "video")) return;
    if (callAffordance() === "hidden") return;
    autoCallHandled = true;
    setSearchParams({ call: undefined }, { replace: true });
    onCallButton(wanted === "video");
  });

  // Phase 2a3e2b-web: cross-device draft synchronisation.
  //
  // Seed-side effect — on first mount + every time a fresh
  // Draft payload arrives from a sibling device, replace the
  // composer's text with the remote one IF the local composer
  // is currently empty. Choosing empty-only avoids stomping a
  // user mid-typing on this device when a stale sibling-device
  // update lands; the typing-side then broadcasts its own HLC
  // and wins by LWW.
  let seededDraft = false;
  createEffect(() => {
    const draft = sync.drafts()[hexOf(conversationIdBytes())];
    if (draft === undefined) return;
    if (seededDraft) return;
    if (input() !== "") {
      // User is already typing on this device — don't stomp.
      // Mark seeded so we don't keep re-checking, and let the
      // local broadcast win the LWW race.
      seededDraft = true;
      return;
    }
    setInput(draft.text);
    seededDraft = true;
  });

  // Outbound broadcast — debounced. Every keystroke fires a
  // signal change; we batch ≤ 1 payload / DRAFT_DEBOUNCE_MS to
  // avoid drowning the DS journal in single-character commits.
  let draftBroadcastTimer: ReturnType<typeof setTimeout> | null = null;
  let lastBroadcastDraft: string | null = null;
  const DRAFT_DEBOUNCE_MS = 500;
  createEffect(() => {
    const text = input();
    if (text === lastBroadcastDraft) return;
    if (draftBroadcastTimer !== null) clearTimeout(draftBroadcastTimer);
    draftBroadcastTimer = setTimeout(() => {
      draftBroadcastTimer = null;
      lastBroadcastDraft = text;
      // Best-effort — a transient DS failure shouldn't crash
      // the composer; the next keystroke will re-broadcast.
      void sync
        .setDraft(conversationIdBytes(), text)
        .catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("setDraft failed", err);
        });
    }, DRAFT_DEBOUNCE_MS);
  });
  onCleanup(() => {
    if (draftBroadcastTimer !== null) {
      clearTimeout(draftBroadcastTimer);
      draftBroadcastTimer = null;
    }
  });
  // 1.19c2 — attachments staged in the composer. The user picks
  // file(s); each gets encrypt+upload immediately so the ref is
  // ready by the time they hit Send. Cleared on Send, on
  // explicit per-chip remove, and on navigation away.
  const [pendingAttachments, setPendingAttachments] = createSignal<
    AttachmentRefBytes[]
  >([]);
  const [uploading, setUploading] = createSignal(false);
  /** Per-file upload-progress snapshot — drives the
   *  "Uploading hello.png (2/5)…" indicator in the composer
   *  tray. `null` when no upload is in flight. The granularity
   *  is one entry per file × two stages (encrypt → upload);
   *  byte-level progress within a single PUT would need a
   *  fetch-stream rewrite (`fetch` doesn't expose upload
   *  progress; XHR does, but the trade-off in code complexity
   *  beats the marginal UX win for sub-MAX_ATTACHMENT_BYTES
   *  files). */
  const [uploadProgress, setUploadProgress] = createSignal<
    | {
        fileIndex: number;
        total: number;
        fileName: string;
        stage: "encrypting" | "uploading";
        /** UX spec section 10 — the local thumb shown while a photo or
         *  video uploads, filed in the media hub under this key. */
        previewKey?: string;
      }
    | null
  >(null);
  // ADR-0047 — the media runtime every bubble shares: decrypted media as
  // object URLs (revoked when this view unmounts — before this, every
  // previewed image stayed in memory for the life of the tab), the
  // one-at-a-time player, auto-continue, and the local-only "played" set.
  const hub: MediaHub = createMediaHub({
    voiceQueue: () =>
      rows().flatMap((row) => {
        const attachments =
          row.kind === "outgoing"
            ? row.attachments
            : row.event.kind === "application"
              ? row.event.attachments
              : [];
        const voice = attachments.find((a) => hub.decision(a).bubble === "voice");
        const key = voice !== undefined && voice.blobId.length > 0 ? mediaKey(voice.blobId) : null;
        return [
          {
            key: key ?? `row:${rowMessageId(row) ?? "pending"}`,
            isVoice: key !== null,
            senderKey: rowAuthorIdentity(row),
            // Our own recordings never show as unplayed and are never
            // auto-continued into.
            played: row.kind === "outgoing",
          },
        ];
      }),
  });
  onCleanup(() => hub.dispose());
  // 1.18g — Add-member panel state. Inline form expands below
  // the chat header when toggled; collapses after a successful
  // add or explicit Cancel.
  const [addMemberOpen, setAddMemberOpen] = createSignal(false);
  const [addMemberInput, setAddMemberInput] = createSignal("");
  // ADR-0051 — the `Group name` row of the group info (UX spec §3.1.2 / §5).
  const [groupNameInput, setGroupNameInput] = createSignal("");
  const [groupNameBusy, setGroupNameBusy] = createSignal(false);
  const [groupNameError, setGroupNameError] = createLiveText();
  // The field follows the shared name (a rename that lands while the panel
  // is open replaces what is typed — the spec's "re-title in place").
  createEffect(
    on(
      () => groupNames.nameOf(props.entry.groupIdB64),
      (shared) => {
        setGroupNameInput(shared ?? "");
        setGroupNameError(null);
      },
    ),
  );
  /** Save the `Group name` row. The affordance is gated on the `ownRole`
   *  signal, which can lag the group by one poll, so the role is re-read
   *  off the LIVE group here and a stale control answers with the spec's
   *  copy. Send first, then store: the title changes only once the send
   *  succeeded. */
  async function onSaveGroupName(): Promise<void> {
    if (groupNameBusy()) return;
    setGroupNameError(null);
    setGroupNameBusy(true);
    try {
      const role = await groupNames.liveRole(props.entry.group);
      if (role === "member") {
        setGroupNameError(GROUP_NAME_ONLY_ADMINS);
        return;
      }
      await groupNames.rename(props.entry, groupNameInput());
      setNotice(null);
    } catch (err) {
      const code = groupNameErrorCode(err);
      setGroupNameError(code !== undefined ? () => groupNameErrorText(code) : errorText(err, "chat.group_name.error.save"));
    } finally {
      setGroupNameBusy(false);
    }
  }
  /** Whether the row is editable: owner / admin, or a table-less group,
   *  and the membership still standing. A plain member sees it read-only. */
  const canRenameGroup = (): boolean => ownRole() !== "member" && !membershipEnded();
  const groupNameUnchanged = (): boolean =>
    groupNameInput().trim() === (groupNames.nameOf(props.entry.groupIdB64) ?? "");
  // ADR-0048 §4 — no lookup leaves the device for a name that fails the rule.
  const addMemberCheck = createUsernameCheck(addMemberInput);
  // ADR-0028 (G-3) — the minted group invite-link (the owner/admin
  // "create invite link" affordance), or `null` before it is generated.
  const [inviteLink, setInviteLink] = createSignal<string | null>(null);
  // `memberCount` mirrors `peopleCountOf(props.entry.group)` into
  // a reactive signal so the header re-renders after
  // `addMember`. Without this the WASM-side property read in
  // JSX wouldn't re-evaluate on a render driven by a different
  // signal — Solid's fine-grained reactivity only tracks signal
  // accesses, not arbitrary object property reads.
  const [memberCount, setMemberCount] = createSignal(
    peopleCountOf(props.entry.group),
  );
  // docs/ux/client-redesign.md section 3 — the conversation kind, read from
  // the group (ADR-0062): set by the creator, the same on every member's
  // device, never changed by a commit. A two-person group is a group; a
  // one-to-one never becomes one. The member count above is display only.
  const conversationKind = createMemo<ConversationKind>(() => conversationKindOf(props.entry.group));
  const isGroupChat = (): boolean => conversationKind() === "group";
  // 1.18h — members panel state. `membersVersion` is a
  // monotonic counter bumped on every epoch advance (refresh
  // success, addMember success, removeMember success); the
  // `members` createMemo re-reads `group.members()` whenever
  // it changes. Same trick as `memberCount` — Solid only
  // tracks signal reads, not WASM-side property accesses.
  const [infoOpen, setInfoOpen] = createSignal(false);
  const [membersVersion, setMembersVersion] = createSignal(0);
  // The header's overflow menu. Info is NOT in it: the name at the top is the
  // only way in (`docs/ux/conversation-info.md` section 1), because two routes
  // to one screen means two things to keep in step and the one in the menu is
  // the one that drifts.
  const [headerMenuOpen, setHeaderMenuOpen] = createSignal(false);
  /**
   * Which info this conversation's header opens - the person or the group.
   *
   * `null` until the shared rules have loaded. Every reader below waits for
   * it rather than guessing a destination, which is the same discipline the
   * conversation list follows: a fallback here would be a second copy of the
   * rule, and a second copy is what this whole move exists to end.
   */
  const infoTarget = createMemo(() => infoTargetForKind(uiRules(), conversationKind()));
  /** The header button's accessible name, once the destination is known.
   *  `undefined` before that, so the button falls back to its own text - the
   *  conversation's name - rather than to an invented label. */
  const infoTargetLabel = (): string | undefined => {
    const target = infoTarget();
    return target === null ? undefined : infoOpenLabel(target);
  };
  /** The heading of the opened layer. The empty string is never rendered:
   *  the layer stays CLOSED while the destination is unknown, so there is no
   *  heading to invent and no risk of showing the wrong panel. */
  const infoTargetHeading = (): string => {
    const target = infoTarget();
    return target === null ? "" : infoHeading(target);
  };
  /**
   * Which blocks the group's info holds (`chat/conversation-info.ts`), read
   * from the same capabilities the panel already computed. Defined as a
   * function rather than a memo so the forward references to `canRenameGroup`
   * and `membershipEnded` resolve at CALL time, which is a render.
   */
  const groupSections = (): ReturnType<typeof groupInfoSections> =>
    groupInfoSections(uiRules(), {
      canManageMembers: canManageMembers(),
      canRename: canRenameGroup(),
      canMintInvite: canManageMembers(),
      isOwner: isOwner(),
      // The same two conditions `exitBlockState` hides the whole exit block
      // on: a group with no ADR-0022 role table has no ADR-0045 proposal
      // path, so a member has nothing to leave WITH, and a torn own-leaf read
      // is not a licence to offer an exit. Passing it means the rule now
      // agrees with that machine instead of being overridden by it.
      canLeave: usesProposalPath() && ownLeaf() !== undefined,
      membershipEnded: membershipEnded(),
    });
  /** The version of the accepted-contacts list the person info was built
   *  from; bumped by a rename or a remove made inside the layer. */
  const [acceptedVersion, setAcceptedVersion] = createSignal(0);
  // Also bumped by a write from outside the layer: the one-to-one adoption
  // rule (`contacts/OneToOneContactAdoption.tsx`) files the peer of this very
  // conversation as a contact, and the info sheet must read the row.
  onMount(() => {
    const unsubscribe = subscribeAcceptedChanged(() => setAcceptedVersion((v) => v + 1));
    onCleanup(unsubscribe);
  });
  // ADR-0027 / §7.1 — an external-join request arrived while the quarantine
  // was full, so the core did not keep it. It is in no list and can be
  // approved by nobody; the only honest thing left is to say it happened.
  //
  // Session-scoped and cleared by the admin acting on the queue (see
  // `onApproveJoin` / `onRejectJoin`), because acting is precisely what may
  // have made room. There is no count: `MAX_PENDING_EXTERNAL_JOINS` is
  // module-private in the core and a client-side literal is the stale-drift
  // class §7.1 flags.
  const [unkeptJoinRequest, setUnkeptJoinRequest] = createSignal(false);
  // 1.18i — accepted-contacts directory keyed by
  // base64(signature_pubkey). Lookup at member-render time
  // turns a hex fingerprint into a real displayName when the
  // member is somebody we've already accepted via the
  // ContactRequest handshake (1.16d3d). Re-read at the same
  // cadence as `members` so a fresh accept-flow happening in a
  // sibling tab on `/contacts` surfaces here without a hard
  // reload. The cost is one localStorage read per
  // membersVersion bump — sub-millisecond.
  // ADR-0048 §5 — every member label in this view goes through the one
  // display resolver (local rename → profile name → verified username →
  // fingerprint without `@`). `name` for one-string surfaces (typing, authors,
  // reactions), `listLabel` (`Name · @username`) for roster rows.
  // A person the device cannot name is "Member", never a key fingerprint
  // (docs/ux/client-redesign.md section 6).
  const memberName = (keyB64: string): string => memberNameOf(profiles.resolve(keyB64));
  const memberListLabel = (keyB64: string): string =>
    memberListLabelOf(profiles.resolve(keyB64));
  // ADR-0050 — one row per PERSON: `sigPubkey` is the member's account key
  // (what roles, names and authorship key on), `leafIndex` its first leaf,
  // `deviceCount` how many leaves the person holds. Removing the row removes
  // every leaf of the account (`removeMemberFromGroup` fans out).
  const members = createMemo<
    readonly { leafIndex: number; sigPubkey: Uint8Array; deviceCount: number }[]
  >(() => {
    // Subscribe to the version signal so the memo re-runs.
    membersVersion();
    const arr = props.entry.group.members() as Array<{
      leafIndex: number;
      signaturePublicKey: Uint8Array;
      accountKey?: Uint8Array;
    }>;
    const out: { leafIndex: number; sigPubkey: Uint8Array; deviceCount: number }[] = [];
    for (const m of arr) {
      const person = personKeyOf(m);
      const personB64 = base64Encode(person);
      const row = out.find((r) => base64Encode(r.sigPubkey) === personB64);
      if (row !== undefined) {
        row.deviceCount += 1;
        continue;
      }
      out.push({ leafIndex: m.leafIndex, sigPubkey: person, deviceCount: 1 });
    }
    return out;
  });

  // ADR-0048 §2 trigger 3 — when this group gains a member, republish this
  // device's profile (throttled + jittered per group by the scheduler), so the
  // newcomer — who cannot read any earlier epoch — learns the existing names.
  // The first-ever roster read is the baseline, not growth. Every path that
  // can add a member (a polled commit, a sweep, an own direct add) bumps
  // `membersVersion`, which is what re-runs `members()`.
  //
  // The baseline is PERSISTED per (account, group) (`membership-baseline.ts`)
  // and shared with the background receive, not kept in a variable of this
  // view: a variable lived as long as the tab, so a join that landed while
  // the tab was closed read as a baseline to the next session and the
  // newcomer never got this device's name.
  createEffect(() => {
    const current = new Set(members().map((m) => base64Encode(m.sigPubkey)));
    const s = auth.state();
    if (s.kind !== "session") return;
    const change = untrack(() =>
      observeMembership(s.account.accountId, props.entry.groupIdB64, current),
    );
    // ADR-0060 section 3 — a person left: rotate this account's reach keys
    // for the group and republish the profile carrying them.
    if (change.shrank) void profiles.groupShrank(props.entry.groupIdB64);
    const grew = change.grew;
    if (grew) {
      profiles.groupGrew(props.entry.groupIdB64);
      // ADR-0051 §3 — an owner / admin holding a name re-sends it (original
      // stamp) so the newcomer learns it; same throttle + jitter.
      groupNames.groupGrew(props.entry.groupIdB64);
      // ADR-0052 decision 3 — and the room's shared pins, original stamps.
      pins.groupGrew(props.entry.groupIdB64);
      sharedChannels.groupGrew(props.entry.groupIdB64);
    }
  });

  // G-18 / ADR-0029 — the mid-call call-key rotation is the call host's: it
  // runs whenever this group's MLS lock queue drains (`onGroupMlsIdle`), which
  // every poll and commit of this view does, and so does the background
  // receive once this view is closed.

  // ADR-0022 — the local member's role in this group ("owner" / "admin" /
  // "member"), or `null` for an open / legacy group with no role table. Drives
  // the add / remove affordance gating below. UX only — the receive-loop gate
  // (`processIncomingVerifyingRoles`) is the real boundary; this just hides
  // affordances a plain member's commit would be refused for anyway.
  const [ownRole, setOwnRole] = createSignal<"owner" | "admin" | "member" | null>(null);

  /**
   * Read this device's ADR-0022 role off the LIVE group, right now.
   *
   * The `ownRole` signal is refreshed asynchronously by the effect below, so
   * at any given instant it can lag the group state by one poll. That is fine
   * for hiding affordances, and NOT fine for the ADR-0043 rotation call sites:
   * a poll batch carrying a demotion AND a removal would have a just-demoted
   * admin rotate off the stale "admin" value, every peer refuse the envelope
   * (`reject-not-authorized`), and the rotator strand itself on a secret
   * nobody else holds. iOS and Android read the role off the live group at
   * rotation time; this is the web equivalent.
   *
   * Never throws — a torn group reads as `null` (which does not rotate).
   */
  async function readOwnRoleNow(): Promise<
    "owner" | "admin" | "member" | null
  > {
    try {
      const secrets = props.requireIdentity();
      const wasm = await loadWasmForCalls();
      const identity = wasm.Identity.fromSeed(conversationSeedOf(secrets));
      try {
        return await readPushRotationRole(props.entry.group, identity);
      } finally {
        identity.free();
      }
    } catch {
      return null;
    }
  }

  createEffect(() => {
    membersVersion(); // recompute when the roster / epoch changes
    props.requireIdentity(); // track the identity dependency
    void (async () => {
      setOwnRole(await readOwnRoleNow());
    })();
  });
  // A plain member may not add / remove; everyone else (owner, admin, or an
  // open/legacy group where `ownRole` is `null`) may.
  //
  // ADR-0045 UX §6.2.7 — and NOBODY may once their own membership has ended.
  // Gating here rather than at each of the six render sites is deliberate:
  // `Add member`, the invite block, every `Remove` and every admin toggle are
  // all refused by the core for an inactive group, and one gate cannot be
  // partly applied. Note the difference from role-gating, which hides what
  // SOMEONE ELSE may do — this hides what nobody in this group may do any
  // more.
  //
  // Defined as a function reading a memo, so the forward reference to
  // `membershipEnded` is resolved at CALL time. Every caller is either a memo
  // body or a render, both of which run after this component's setup.
  const canManageMembers = (): boolean =>
    ownRole() !== "member" && !membershipEnded();
  /** Whether the header's overflow menu has anything in it at all. */
  const headerMenuHasItems = (): boolean => canManageMembers();
  // ADR-0022 — only the owner may promote / demote admins.
  const isOwner = (): boolean => ownRole() === "owner";

  /** Bumped whenever the pending set or its observations may have moved, so the
   *  panel memos below re-read. Separate from `membersVersion` because a
   *  proposal changes nothing about the roster. */
  const [pendingVersion, setPendingVersion] = createSignal(0);
  /** Ticks so a `pending` request matures into `waiting` without traffic. */
  const [pendingClock, setPendingClock] = createSignal(Date.now());

  /**
   * ADR-0045 UX §6.2 — is this device still a member of this group?
   *
   * The one signal there is. `processIncomingVerifyingRoles` returns the same
   * content-less `Commit` for a commit that removes our own leaf as for any
   * other, so nothing in the receive path announces it; `Group.isActive` is
   * what OpenMLS itself consults before refusing to encrypt, export or
   * decrypt, and reading it is how the UI's gate becomes the same predicate as
   * the core's refusal rather than a re-derivation of it.
   *
   * Invalidated on `membersVersion` (bumped after every poll and every sweep,
   * which is every path a removing commit can arrive on) and on
   * `pendingVersion` (bumped by `noteOwnRequest`, which is what writes the
   * durable leave record the copy is narrowed by).
   *
   * The state is TERMINAL and survives a restart, so this answers the same at
   * launch as it did in the batch that carried the removal — there is no
   * transient window to debounce and nothing to re-check later.
   */
  const membership = createMemo<MembershipState>(() => {
    membersVersion();
    pendingVersion();
    const s = auth.state();
    return membershipState({
      isActive: readIsActive(props.entry.group),
      leaveRequestedAtMs:
        s.kind === "session"
          ? loadLeaveRequestedAt(
              s.account.accountId,
              base64Encode(props.entry.group.groupId),
            )
          : undefined,
    });
  });

  /** The standing gate the four background disciplines consult (§6.2.8). Read
   *  as a function rather than through the memo directly so a call site reads
   *  as the question it is asking. */
  const membershipEnded = (): boolean => membershipHasEnded(membership());

  /** §6.2.5 — the bar that REPLACES the composer, or `null` while the
   *  membership holds. Narrowed to the one variant the template renders, the
   *  same split `ownerExit` / `memberExit` use, so the JSX can take `<Show>`'s
   *  accessor form rather than casting a union inside the template. */
  const composerWithdrawn = createMemo(() => {
    const state = composerState(membership(), input());
    return state.kind === "withdrawn" ? state : null;
  });

  /** §6.2.5 — whether a message still carries Edit / Delete / React. The same
   *  question the composer asks, answered by the same predicate: all three ops
   *  compose an application message, so all three die with Send. */
  const messageActionsAvailable = createMemo(
    () => messageActionsState(membership()) === "available",
  );

  /**
   * The same question asked SYNCHRONOUSLY, off the live handle, with no memo
   * in between.
   *
   * `membersVersion` is bumped by an effect, so the memo above can still hold
   * its pre-batch value in the microtask that immediately follows a poll —
   * which is exactly where the ADR-0043 rotation is fired. §6.2.8 item 3 says
   * the eviction verdict must be evaluated on the SAME commit as the shrink it
   * causes, and this is what makes that literal rather than approximate. An
   * unreadable handle answers `false`, matching {@link membershipState}'s
   * fail-toward-active direction.
   */
  const evictedNow = (): boolean => readIsActive(props.entry.group) === false;

  // The current admin set as hex sig-pubkey strings (re-read per
  // `membersVersion`) — drives the per-member "Make/Remove admin" label.
  const adminPubkeys = createMemo<Set<string>>(() => {
    membersVersion();
    const set = new Set<string>();
    try {
      const flat = props.entry.group.roleTableAdmins() as Uint8Array;
      for (let i = 0; i + 32 <= flat.length; i += 32) {
        set.add(hexOf(flat.slice(i, i + 32)));
      }
    } catch {
      // open / legacy group with no role table — no admins.
    }
    return set;
  });
  // Owner sig-pubkey (hex), re-read per `membersVersion` — the owner row never
  // shows a promote/demote toggle (the owner is fixed).
  const ownerPubkeyHex = createMemo<string | null>(() => {
    membersVersion();
    try {
      const o = props.entry.group.roleTableOwner() as Uint8Array | undefined;
      return o ? hexOf(o) : null;
    } catch {
      return null;
    }
  });

  // ADR-0027 (G-3) — pending external-join requests, re-read per
  // `membersVersion` (bumped after every poll, so a freshly-retained
  // request appears). Each entry is the 32-byte joiner leaf signature key —
  // the `proposal_ref` handle the approve / reject helpers take. The MLS
  // core's retained set is the source of truth; the `externalJoinRequest`
  // event is only the nudge that drives the membersVersion bump.
  const pendingJoins = createMemo<Uint8Array[]>(() => {
    membersVersion();
    // ADR-0045 UX §6.2.7 — a queue this reader can no longer act on. Approving
    // needs `add_member`, which the core refuses for an inactive group, so the
    // section would be a list of buttons that all fail.
    if (membershipEnded()) return [];
    try {
      const flat = props.entry.group.pendingExternalJoins();
      const out: Uint8Array[] = [];
      for (let i = 0; i + 32 <= flat.length; i += 32) {
        out.push(flat.slice(i, i + 32));
      }
      return out;
    } catch {
      return [];
    }
  });

  // -------------------------------------------------------------------------
  // ADR-0045 — proposal-based membership
  // -------------------------------------------------------------------------

  /** Does this group route membership through proposals? A group with NO role
   *  table has no owner, hence no rank 0, hence no committer — and every peer's
   *  receipt gate drops proposals in it — so it keeps direct commits. A torn
   *  read is treated as "no role table", which keeps the direct path working. */
  const usesProposalPath = (): boolean => {
    try {
      return props.entry.group.roleTableOwner() !== undefined;
    } catch {
      return false;
    }
  };

  /**
   * Run one committer sweep for this group, then reconcile the two things a
   * merged sweep changes locally.
   *
   * **The push-routing rotation is here, not at propose time, and that is
   * C-61.** A sweep is the moment a removal actually lands on this device, so
   * it is the moment the evicted member must stop being able to derive the
   * group's `push_gid`. Detecting it as a membership SHRINK — the same
   * before/after member-set diff the poll loop runs for someone else's removal
   * — keeps the rotation strictly after the commit by construction: the new
   * secret is stamped with the post-merge `group.epoch`, so it both out-ranks
   * the secret it replaces (ADR-0044 recency) and is sealed at an epoch the
   * evicted leaf can no longer read (RFC 9420 forward secrecy).
   *
   * Fire-and-forget on the rotation half, as everywhere else: push is
   * best-effort and a failed rotation must never unwind a landed membership
   * change.
   */
  /**
   * The web's reading of "foregrounded" — see {@link emitBeaconIfDue} for why
   * it is load-bearing rather than cosmetic.
   *
   * `visibilityState` covers the tab being hidden, backgrounded or in a
   * background window; it does NOT cover an unmounted route, which the
   * emitter's own lifetime already covers (this component owns the interval,
   * and `onCleanup` stops it). An environment with no `document` reads as NOT
   * foreground — the fail-closed direction, since a beacon nobody can back up
   * with a sweep is worse than no beacon.
   */
  function isForeground(): boolean {
    return globalThis.document?.visibilityState === "visible";
  }

  /**
   * ADR-0045 S4b — resolve a beacon's MLS sender leaf to its signature key.
 *
 * Same rule as `author-identity.ts`'s `resolveMemberIdentity`, in bytes
 * rather than base64 because the beacon store encodes it itself.
   *
   * The leaf is resolved HERE, at receipt, because that is the only moment the
   * roster is authenticated in the epoch the beacon was sealed at. The KEY is
   * what gets persisted, never the leaf: OpenMLS reuses a vacated leaf for the
   * next member added, so a stored leaf silently comes to name somebody else.
   */
  /** Base64 of the PERSON key (ADR-0050) at `leaf`; `undefined` on a torn
   *  read or when no member holds it. */
  function personKeyOfLeafB64(leaf: number): string | undefined {
    try {
      const roster = props.entry.group.members() as unknown as Array<{
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

  function signatureKeyOfLeaf(leaf: number): Uint8Array | undefined {
    try {
      const roster = props.entry.group.members() as Array<{
        leafIndex: number;
        signaturePublicKey: Uint8Array;
      }>;
      return roster.find((m) => m.leafIndex === leaf)?.signaturePublicKey;
    } catch {
      // A torn roster read: the sender is unattributable, so the beacon is no
      // evidence about anyone and is simply not recorded.
      return undefined;
    }
  }

  /**
   * ADR-0045 S4b — the acting-committer verdict, taken fresh on every sweep
   * attempt.
   *
   * **Every rank in the projection is derived HERE and now**, by
   * `Group.committerRank` against the group's live `0xFF02` role table — never
   * carried over from when the beacon arrived. A rank is a position in that
   * table's canonical order, so promoting, demoting or removing any admin
   * renumbers every admin sorting after it; a rank captured under one ordering
   * and compared against this device's own current rank is not stale, it is
   * meaningless.
   *
   * Fails closed to `defer` on anything it cannot read — no session, no wasm.
   * Deferring costs a sweep attempt; acting on a view of who else is running
   * that this device could not actually read is the hazard.
   */
  async function actingCommitterNow(
    ownRank: number,
    ownSignatureKey: Uint8Array,
  ): Promise<ActingCommitter> {
    const s = auth.state();
    if (s.kind !== "session") return { kind: "defer", toRank: undefined };
    const wasm = await loadWasmForCalls();
    return decideActingCommitter({
      accountIdB64: s.account.accountId,
      groupIdB64: base64Encode(props.entry.group.groupId),
      ownRank,
      ownSignatureKey,
      rankOf: (senderKeyB64) => {
        try {
          return props.entry.group.committerRank(base64Decode(senderKeyB64));
        } catch {
          return undefined;
        }
      },
      nowMs: Date.now(),
      // Read off the FFI, never hand-copied — the interval and the window are
      // two halves of one budget and a local literal is the drift class
      // ADR-0044 exists about.
      freshnessWindowMs: wasm.committerBeaconFreshnessWindowMs(),
      fold: wasmActingCommitterFold(wasm),
    });
  }

  /**
   * ADR-0045 S4b — emit this device's own liveness beacon.
   *
   * **The foreground gate is the load-bearing part and it is a caller
   * obligation the core cannot check.** A beacon means *able to sweep*, and on
   * this client a sweep runs only at the tail of a poll driven by this
   * component — and that poll pauses while the tab is hidden, except during a
   * call — and the ADR-0041 background decrypt is read-only-throwaway and
   * structurally cannot commit (C-59). A beacon from a hidden tab would
   * therefore be a lie that costs the group a takeover: every
   * other candidate would defer to a device that is not going to sweep, and
   * would keep deferring for as long as the hidden tab kept beaconing.
   *
   * The same goes for a device with no root and no registrar seed for the
   * group (ADR-0050 section 2.5a item 8): it could not register the epoch a
   * sweep moves to, so the sweep never stages, and its beacon would make
   * others defer to it for nothing.
   *
   * Everything else — only-candidates-emit, first-of-session-immediate, one
   * per interval — is the core's `shouldEmitBeacon`, called through the gate.
   *
   * Best-effort and silent: nobody is watching a beacon, and a failed emit
   * only means this device does not claim the duty on this tick.
   */
  let lastBeaconAtMs: number | undefined;
  async function emitBeaconIfDue(): Promise<void> {
    // Short-circuited FIRST so a hidden tab does no work at all — the same
    // answer `beaconEmitVerdict` gives below, reached before the Ed25519 key
    // derivation the rank read costs. The gate there stays the authority.
    if (!isForeground()) return;
    // ADR-0045 UX §6.2.8 item 2 — an evicted device must never beacon. A
    // beacon means *able to sweep*, and this one can never sweep again: the
    // envelope is an ordinary MLS application message and
    // `createApplicationMessage` is refused for an inactive group, so the emit
    // would throw on its own ticker every interval anyway.
    //
    // Note this is the SAME rule that keeps a backgrounded tab from emitting,
    // reached from the other direction: there the device could sweep and is
    // not running; here it is running and never can. Placed beside that check
    // for exactly that reason.
    //
    // The ticker is cleared too (`endedTeardown`), so this is belt and braces
    // — but it is the half that is a GATE, and the gate is what makes a
    // surviving timer harmless rather than merely unlikely.
    if (membershipEnded()) return;
    if (!usesProposalPath()) return;
    const wasm = await loadWasmForCalls();
    let ownRank: number | undefined;
    try {
      // ADR-0050 section 2.5a item 8 — the rank of the identity the GROUP runs
      // on (this device's leaf, the one sweeps use), never one rebuilt from
      // the account seed: on a bound device the account key holds no leaf, so
      // the account identity has no rank and this device never beaconed. The
      // context owns the instance; it is not freed here.
      ownRank = deviceCommitterRank(props.entry.group, chats.identity());
    } catch {
      // No identity / torn group — this device cannot establish that it is a
      // candidate, so it must not claim to be one.
      return;
    }
    const verdict = beaconEmitVerdict({
      ownRank,
      // ADR-0050 section 2.5a item 8 — the sweep's own pre-stage check: a
      // device with no root and no registrar seed cannot complete a sweep, so
      // it must not announce that it can.
      mayRegister: ownRank !== undefined && dsAccess.mayRegister(props.entry.group.groupId),
      foreground: isForeground(),
      lastEmittedAtMs: lastBeaconAtMs,
      nowMs: Date.now(),
      throttle: (rank, last, now) => wasm.shouldEmitBeacon(rank, last, now),
    });
    if (verdict !== "emit") return;
    // Stamped BEFORE the await, so a publish that is slow or that fails
    // cannot let the next tick fire a second beacon a few seconds later. It is
    // also this device's re-entrancy guard: a publish outlasting the tick
    // interval reads as throttled on the next pass rather than doubling up.
    lastBeaconAtMs = Date.now();
    try {
      await broadcastToGroup(wasm.composeCommitterBeacon());
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ADR-0045 committer beacon not emitted", err);
    }
  }

  async function sweepNow(): Promise<SweepOutcome | null> {
    const before = memberSignatureKeys(props.entry.group);
    const personsBefore = personKeysOrNull(props.entry.group);
    // ADR-0045 — the verdict apply consumes journal rows BELOW our own commit
    // and advances the shared cursor over them, so the poll will never fetch
    // them again. Collected in journal order and fed to the SAME consumer the
    // poll uses, once the sweep has settled: they are strictly newer than
    // anything the poll just rendered, so appending them is journal order.
    const swept: IncomingChatEvent[] = [];
    let outcome: SweepOutcome | null = null;
    try {
      outcome = await contacts.sweepIfCommitter(
        props.requireIdentity(),
        props.entry.group,
        { read: () => cursor(), advance: (to) => setCursor(to) },
        (id) => chat.markOwnPublishedCommit(props.entry.groupIdB64, id),
        actingCommitterNow,
        (event) => swept.push(event),
      );
    } catch (err) {
      // A verdict this device could not read leaves the sweep staged for the
      // next attempt (which runs at the tail of every poll). Never surfaced as
      // a user-facing error: nothing they did failed, and the request is still
      // live.
      // eslint-disable-next-line no-console
      console.warn("ADR-0045 sweep did not complete", err);
    } finally {
      // On EVERY exit, including a LOST verdict and a throw. Whether our own
      // commit won the order says nothing about the rows below it: they were
      // applied, the ratchet moved and the cursor advanced past them either
      // way, so the only question left is whether the user gets to read them.
      if (swept.length > 0) consumeEvents(swept);
    }
    if (outcome === null) return null;
    if (outcome.kind !== "committed") return outcome;

    setMemberCount(peopleCountOf(props.entry.group));
    setMembersVersion(membersVersion() + 1);

    const s = auth.state();
    if (s.kind === "session") {
      const after = memberSignatureKeys(props.entry.group);
      // ADR-0050 — a leaf this sweep removed is never re-admitted by this
      // client's scheduled check.
      recordRemovedDevices(s.account.accountId, props.entry.groupIdB64, before, after);
      // ADR-0073 R.7 case 2 — this sweep only swapped the devices of people
      // who stay: no `Members changed` row for it.
      suppressDeviceSwapRow(
        personsBefore,
        personKeysOrNull(props.entry.group),
        outcome.commitMessageId,
      );
      // ADR-0045 UX §6.2.8 item 3 — captured synchronously alongside the
      // post-sweep roster, so the eviction and the shrink it caused are judged
      // on the same commit. A sweep can carry our own removal: an admin can
      // sweep a `Leave` it proposed for itself.
      const selfEvicted = evictedNow();
      void (async () => {
        await rotatePushRoutingSecretIfMembershipShrank({
          accountId: s.account.accountId,
          group: props.entry.group,
          // Live role read, never the `ownRole()` signal: that is refreshed by
          // an async effect and can still hold a pre-sweep value.
          ownRole: await readOwnRoleNow(),
          previousMemberKeys: before,
          currentMemberKeys: after,
          broadcast: broadcastToGroup,
          selfEvicted,
        });
      })().catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("push-routing rotation failed", err);
      });
      // ADR-0042 — a swept ADD needs the current routing secret, exactly as the
      // direct add path re-broadcasts it. No-op when this device holds none.
      if (outcome.addedCount > 0) {
        void redistributeSecretToGroup(
          s.account.accountId,
          props.entry.group,
          broadcastToGroup,
        ).catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("push-routing secret redistribute failed", err);
        });
      }
    }
    return outcome;
  }

  /** The observation key for a received proposal — the same synthetic key
   *  `pending-requests.ts` derives from the core's record, so an observation
   *  recorded when the row landed re-joins the live pending set on a later
   *  render. Kept in step with `proposalKey` by shape, not by import, because
   *  the event carries the wasm record's fields flat. */
  function proposalKeyOfEvent(e: {
    proposalKind: string;
    addSignatureKey: Uint8Array | undefined;
    removeTargetLeaf: number | undefined;
    senderLeaf: number | undefined;
    stored: boolean;
  }): string {
    return proposalKey({
      kind: e.proposalKind,
      addSignatureKey: e.addSignatureKey,
      removeTargetLeaf: e.removeTargetLeaf,
      senderLeaf: e.senderLeaf,
      stored: e.stored,
    });
  }

  /**
   * ADR-0045 — leave the group.
   *
   * Reachable for the first time because of this ADR: naming your OWN leaf in a
   * Remove proposal needs no role, and ADR-0022's gate deliberately dropped the
   * self-remove branch as unreachable when a member could not commit their own
   * departure.
   *
   * The core still refuses the OWNER's departure fail-closed — an ownerless
   * group could never appoint another admin, since a role change is owner-only —
   * but that is a PRECONDITION rather than a dead end, and as of UX §6.1 the
   * route out of it exists: transfer ownership first, then leave as an ordinary
   * member. So the owner's control is no longer HIDDEN but rendered disabled
   * with its reason, directly below the `Transfer ownership` that discharges it
   * (`exitBlock`). This handler is unreachable for the owner either way, and
   * that gate is the role table rather than the local record of having asked:
   * an owner is still the owner while their transfer is pending, and a
   * `Leave group` that went live on publication would offer an action the core
   * refuses and the cryptography does not support.
   */
  async function onLeaveGroup(): Promise<void> {
    const leaf = ownLeaf();
    if (leaf === undefined) return;
    // The confirmation is the last exit — a published request cannot be
    // withdrawn — and the body carries the one fact the user needs.
    //
    // ADR-0045 UX §14 — an in-page dialog rather than `window.confirm`: the
    // body is §6's irreversibility copy and has to read as the app's, not
    // under a browser-supplied title, and the action names the group. The
    // copy lives in `leave-group-confirm.ts` so it is testable at all —
    // `vitest` cannot load this file.
    if (
      !(await requestConfirm(
        leaveGroupConfirmation({ groupLabel: resolvePeerName() }),
      ))
    ) {
      return;
    }
    setError(null);
    setNotice(null);
    setBusy(true);
    // ADR-0045 UX §7 — keyed on our own SIGNATURE key, never on the leaf:
    // OpenMLS hands a vacated leaf to the next member added, so a retry armed
    // against a leaf could come to name somebody else entirely. `undefined`
    // (a torn roster read) simply means no retry is armed for this attempt.
    const ownKeyB64 = ownSignatureKeyB64();
    const leaveRequest: RetryableRequest | null =
      ownKeyB64 === undefined ? null : { kind: "leave", subjectKeyB64: ownKeyB64 };
    try {
      const out =
        leaveRequest === null
          ? await contacts.removeMemberFromGroup(
              props.requireIdentity(),
              props.entry.group,
              leaf,
            )
          : await withMembershipRetry(leaveRequest, () =>
              contacts.removeMemberFromGroup(
                props.requireIdentity(),
                props.entry.group,
                leaf,
              ),
            );
      chat.markOwnPublishedCommit(
        props.entry.groupIdB64,
        out.kind === "committed" ? out.commitMessageId : out.proposalMessageId,
      );
      if (out.kind === "proposed") noteOwnRequest(`remove:${leaf}`);
      // ADR-0045 UX §6.2.3 — the ONE thing that later tells `You left this
      // group.` from `You are no longer a member of this group.`, and it has
      // to be its own durable field rather than a read of the observation
      // `noteOwnRequest` just wrote. That observation lives under
      // `remove:{ownLeaf}` and is pruned the moment the core stops holding the
      // request — which is exactly what the sweep that removes us does. The
      // poll batch carrying the eviction therefore destroys the only evidence
      // that we asked for it, and the order of those two steps is an
      // implementation detail no test pins.
      //
      // Written on BOTH arms, `committed` included: a legacy group with no
      // role table takes the direct path and evicts us in the same call.
      // Fail-safe when absent — the fallback string is true either way.
      noteOwnLeaveRequested();
      setNotice(() => t("chat.leave.requested"));
      void refresh();
    } catch (err) {
      setError(errorText(err, "chat.error.leave"));
    } finally {
      setBusy(false);
    }
  }

  /** Record that a request this device just published landed, so the quiet
   *  window can be measured from now and so the panel knows it is ours (the
   *  UX §10 own-request visibility exception).
   *
   *  `transferToB64` is supplied only by the ownership-transfer path (UX
   *  §6.1.10): a role change carries no payload, so the successor's key on our
   *  own record is the only thing that later tells a transfer from a promote,
   *  and it is what names the person in `Waiting to make {name} the owner.` */
  function noteOwnRequest(key: string, transferToB64?: string): void {
    const s = auth.state();
    if (s.kind !== "session") return;
    recordObservation(
      s.account.accountId,
      base64Encode(props.entry.group.groupId),
      key,
      Date.now(),
      true,
      transferToB64,
    );
    setPendingVersion(pendingVersion() + 1);
  }

  // -------------------------------------------------------------------------
  // ADR-0045 UX §7 — the automatic re-send of a queue-full refusal
  // -------------------------------------------------------------------------

  /** The `(account, group)` scope every armed retry is filed under. `null`
   *  outside a session, where there is nothing to file. */
  function retryScope(): string | null {
    const s = auth.state();
    if (s.kind !== "session") return null;
    return retryScopeKey(
      s.account.accountId,
      base64Encode(props.entry.group.groupId),
    );
  }

  /** `pendingProposals().length` — the exact number the core's cap gates on.
   *  A torn read answers `0`, which is the fail-safe direction: it can only
   *  make a later drain test *fail* to fire, never fire spuriously. */
  function pendingCountNow(): number {
    try {
      return (props.entry.group.pendingProposals() as unknown[]).length;
    } catch {
      return 0;
    }
  }

  /**
   * ADR-0027 — the joiner key of every request the core still holds in its
   * retained external-join quarantine, or `null` on a torn read.
   *
   * Deliberately NOT the `pendingJoins` memo: that one folds a failed read
   * into an empty list (correct for a panel, which then renders nothing) and
   * gates on `membershipEnded`. Here an empty list and a failed read are
   * different answers — the first says the request is gone, the second says
   * nothing — and conflating them would drop a live retry.
   */
  function retainedJoinKeysNow(): string[] | null {
    try {
      const flat = props.entry.group.pendingExternalJoins();
      const out: string[] = [];
      for (let i = 0; i + 32 <= flat.length; i += 32) {
        out.push(base64Encode(flat.slice(i, i + 32)));
      }
      return out;
    } catch {
      return null;
    }
  }

  /** The group as the retry verdict reads it, captured in ONE pass — the
   *  roster and the pending set must come from the same moment. */
  function retrySnapshot(): GroupSnapshot {
    // With each leaf's ADR-0050 account key: an add or a removal names the
    // PERSON, so the roster test must match the account key too.
    const roster: RosterMemberView[] | null = readRosterViews(props.entry.group);
    let pendingKeys: string[] = [];
    let pendingCount = 0;
    try {
      const records = props.entry.group.pendingProposals() as ProposalRecordView[];
      pendingCount = records.length;
      pendingKeys = records.filter((r) => r.stored).map(proposalKey);
    } catch {
      /* a torn read leaves the set empty, which only ever suppresses a fire */
    }
    const owner = ownerKeyB64();
    return {
      roster,
      pendingKeys,
      pendingCount,
      roleTable:
        owner === undefined
          ? null
          : { ownerKeyB64: owner, adminKeysB64: [...adminKeysB64()] },
      membershipEnded: membershipEnded(),
      retainedJoinKeys: retainedJoinKeysNow(),
    };
  }

  /**
   * Claim the slot for `request` before a MANUAL attempt starts.
   *
   * This is half of the exactly-once property and the half that is easy to
   * miss: the re-issue goes through `withGroupMlsLock`, which is
   * non-reentrant, so the driver cannot hold that lock across its own last
   * check. A user tapping again therefore takes the record away rather than
   * racing an in-flight retry for it.
   */
  function disarmRetry(request: RetryableRequest): void {
    const scope = retryScope();
    if (scope !== null) claimArmedRetry(scope, retryKey(request));
  }

  /**
   * Arm an automatic re-send if — and only if — the CORE says this refusal
   * resolves on its own.
   *
   * Nothing here lists a reason. `armedRetryFor` asks
   * `membershipRetryMaySucceed`, which is the wasm export, so a `DoNotRetry`
   * refusal, a `NotPermitted` one, an unrecognised reason from a newer core
   * and an ordinary error all take the same path: no record, nothing retried.
   */
  function armRetryIfQueueFull(
    request: RetryableRequest,
    err: unknown,
    pendingCountAtRefusal: number,
  ): void {
    const scope = retryScope();
    if (scope === null) return;
    const armed = armedRetryFor(
      err,
      request,
      pendingCountAtRefusal,
      membershipRetryMaySucceed,
    );
    if (armed !== null) armRetry(scope, armed);
  }

  /**
   * Run one membership request, arming a re-send if a full queue refused it.
   *
   * Wrapping rather than editing each handler's body keeps the arm and the
   * disarm adjacent to each other and to the call, which is what stops a new
   * handler from arming without disarming.
   */
  async function withMembershipRetry<T>(
    request: RetryableRequest,
    run: () => Promise<T>,
  ): Promise<T> {
    disarmRetry(request);
    const before = pendingCountNow();
    try {
      return await run();
    } catch (err) {
      armRetryIfQueueFull(request, err, before);
      throw err;
    }
  }

  /**
   * The approve-an-external-join twin of {@link withMembershipRetry}.
   *
   * Separate because the arming gate is: `armedApproveJoinRetry` additionally
   * asks `externalJoinAffordance` whether the request is one an approve could
   * act on at all, and the `retained` answer is read LIVE off the core's
   * quarantine rather than assumed. A torn read of that set answers `false`,
   * which is the fail-safe direction — it can only decline to arm, never arm
   * a retry the core has nothing to look up.
   */
  async function withApproveJoinRetry<T>(
    proposalRef: Uint8Array,
    run: () => Promise<T>,
  ): Promise<T> {
    const subjectKeyB64 = base64Encode(proposalRef);
    disarmRetry({ kind: "approve-join", subjectKeyB64 });
    const before = pendingCountNow();
    try {
      return await run();
    } catch (err) {
      const scope = retryScope();
      if (scope !== null) {
        const armed = armedApproveJoinRetry(
          err,
          subjectKeyB64,
          before,
          retainedJoinKeysNow()?.includes(subjectKeyB64) ?? false,
          membershipRetryMaySucceed,
        );
        if (armed !== null) armRetry(scope, armed);
      }
      throw err;
    }
  }

  /**
   * Re-issue one armed request, reconstructed from its descriptor.
   *
   * **Reconstructed, never a stored closure.** The descriptor is exactly what
   * the request is; anything a closure would additionally capture (a roster
   * snapshot, a leaf index, a KeyPackage) is stale by the time the queue
   * drains. In particular a removal's LEAF is resolved from the live roster
   * here, because OpenMLS hands a vacated leaf to the next member added and a
   * stored one would eventually remove the wrong person.
   */
  async function reissueMembershipRequest(request: RetryableRequest): Promise<void> {
    await reissueOwnRequest(reissueHost(), request);
  }

  /** The chat view as a re-issue host: the user path's `contacts` and
   *  identity, echo suppression on this view's `chat`, and the quiet-window
   *  observation so a re-sent request shows as ours in the panel. Shared with
   *  the background receive loop through `chat/rerequest-membership.ts`, so
   *  the two re-derive a request identically. */
  function reissueHost() {
    return {
      contacts,
      identity: props.requireIdentity(),
      group: props.entry.group,
      markOwnPublished: (id: number) => chat.markOwnPublishedCommit(props.entry.groupIdB64, id),
      noteOwnProposal: (key: string, transferToB64?: string) => noteOwnRequest(key, transferToB64),
    };
  }

  /**
   * ADR-0050 section 2.5a point 4 — re-request an own membership request a
   * merged commit dropped (`chat/membership-retry.ts`).
   *
   * **Driven by an observed merged commit, never by a timer.** It reads the
   * same version signals the queue-full driver above reads, which move on
   * every poll tail and every sweep; the pass itself decides nothing unless the
   * group's epoch has moved past a record's, which only a merged commit does.
   * So a quiet group costs one store read per bump and re-requests nothing.
   *
   * Silent: a re-request in flight shows nowhere. A request that stops for a
   * reason other than "landed" / "subject left" lands in `Not applied`.
   */
  createEffect(() => {
    membersVersion();
    pendingVersion();
    const s = auth.state();
    if (s.kind !== "session") return;
    // Stand down while a manual membership operation is in flight, for the
    // same reason the queue-full driver does.
    if (busy()) return;
    const ended = membershipEnded();
    let host: ReturnType<typeof reissueHost>;
    try {
      host = reissueHost();
    } catch {
      return;
    }
    void runSupersededRerequests({
      accountIdB64: s.account.accountId,
      groupIdB64: base64Encode(props.entry.group.groupId),
      snapshot: () => readIntentSnapshot(props.entry.group, ended),
      rerequest: (request) => reissueOwnRequest(host, request),
      retryMaySucceed: membershipRetryMaySucceed,
    })
      .then(async (summary) => {
        if (summary.stopped > 0) setPendingVersion(pendingVersion() + 1);
        if (summary.rerequested > 0) {
          setMembersVersion(membersVersion() + 1);
          await sweepNow();
          void refresh();
        }
      })
      .catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("superseded re-request pass failed", err);
      });
  });

  /**
   * The driver: evaluate every armed request against the group as it is now.
   *
   * **Driven by the fold, never by a timer.** It reads `membersVersion` and
   * `pendingVersion`, which are bumped by the poll tail and by every sweep —
   * i.e. by exactly the events that can drain a queue. A quiet group produces
   * no evaluations that decide anything and no attempts at all.
   *
   * Synchronous and free when nothing is armed, which is the overwhelmingly
   * common case: `armedRetries` returns an empty array and the effect ends.
   */
  createEffect(() => {
    membersVersion();
    pendingVersion();
    const scope = retryScope();
    if (scope === null) return;
    // Stand down while a MANUAL membership operation is in flight.
    //
    // The disarm-on-tap covers every handler that knows its descriptor up
    // front, but `onAddMember` cannot: the request only becomes nameable once
    // the typed handle has resolved, so between the tap and the resolution
    // there is a window in which a retry for the same person could fire behind
    // the user's own attempt. Both would be published, and the sweep would
    // exclude one of them as a duplicate signature key — self-correcting, but
    // a wasted slot in exactly the queue that was full. Standing down costs
    // one refresh cycle.
    if (busy()) return;
    const armed = armedRetries(scope);
    if (armed.length === 0) return;
    const view = retrySnapshot();
    for (const entry of armed) {
      const verdict = retryVerdict(entry, view);
      if (verdict.kind === "wait") continue;
      // Claim first, decide second — the claim is the exactly-once step, and a
      // `drop` claims for the same reason a `fire` does: leaving a dropped
      // record in the store would re-evaluate it on every future fold.
      const claimed = claimArmedRetry(scope, retryKey(entry.request));
      if (claimed === undefined) continue;
      if (verdict.kind === "drop") continue;
      void (async () => {
        try {
          await reissueMembershipRequest(claimed.request);
          setMembersVersion(membersVersion() + 1);
          await sweepNow();
          void refresh();
        } catch (err) {
          // A second queue-full refusal re-arms against the NEW count and waits
          // for the next drain — silently, because nothing has changed for the
          // user since the refusal they were already shown.
          //
          // An approve re-arms through its OWN helper, so the affordance gate
          // runs on this path too. It is defence in depth rather than a hole
          // being closed — `approve_external_join` looks the request up BEFORE
          // it proposes, so a queue-full refusal proves the request was still
          // retained — but the re-arm is part of the retry path, and a second
          // way to arm one that skipped a gate the first way applies is the
          // shape that eventually diverges.
          const rearmed =
            claimed.request.kind === "approve-join"
              ? armedApproveJoinRetry(
                  err,
                  claimed.request.subjectKeyB64,
                  pendingCountNow(),
                  retainedJoinKeysNow()?.includes(claimed.request.subjectKeyB64) ?? false,
                  membershipRetryMaySucceed,
                )
              : armedRetryFor(
                  err,
                  claimed.request,
                  pendingCountNow(),
                  membershipRetryMaySucceed,
                );
          if (rearmed !== null) {
            armRetry(scope, rearmed);
            return;
          }
          // Anything else is a real failure of a request the user made and
          // was not present for, so it replaces the stale message rather than
          // being logged away.
          setError(errorText(err, "chat.error.membership_request"));
        }
      })();
    }
  });

  /**
   * ADR-0045 UX §6.2.4 — `Delete this chat`.
   *
   * Reachable only once the membership has ended (the exit block's `ended`
   * arm), so this never removes a live conversation.
   *
   * What is removed, what is deliberately NOT (the inert MLS remnant in the
   * provider snapshot), and why the push subscription is already gone by
   * this point are all stated once, on `chat/delete-chat.ts`. This function
   * owns only the confirmation and the busy / error state around it.
   */
  async function onDeleteChat(): Promise<void> {
    const s = auth.state();
    if (s.kind !== "session") return;
    // The confirmation is the only protection there is, so it states exactly
    // what is destroyed rather than gesturing at it.
    //
    // ADR-0045 UX §14 — an in-page dialog rather than `window.confirm`. This
    // is the one control in the design that destroys user data on request,
    // and §6.2.4's body is the sentence a user reads before losing a
    // transcript that exists nowhere else; putting it under a chrome-supplied
    // title, in the browser's voice, was the worst place in the app to do it.
    // The action names the chat.
    if (
      !(await requestConfirm(
        deleteChatConfirmation({ chatLabel: resolvePeerName() }),
      ))
    ) {
      return;
    }
    setError(null);
    setBusy(true);
    try {
      // The removal itself — which stores, in which order — lives in
      // `chat/delete-chat.ts`, shared with the contact card, which is where
      // an ended ONE-TO-ONE is closed from (this exit block is the group
      // layer's). The registry row goes last there, so a failure leaves the
      // chat reachable.
      // ADR-0073 R.5 — a conversation is every group of its lineage this
      // device holds; the open group (the head) goes last.
      const held = lineageView.resolve(props.entry.groupIdB64)?.held ?? [props.entry];
      await deleteConversation({
        accountIdB64: s.account.accountId,
        segments: held.map((g) => ({
          groupIdB64: g.groupIdB64,
          groupIdHex: hexOf(g.group.groupId),
        })),
        headGroupIdB64: props.entry.groupIdB64,
        chats,
        // ADR-0052 decision 3 — the pins are wiped with the conversation.
        afterSegment: (segment) => pins.forgetConversation(segment.groupIdB64),
      });
      // Nothing held for a conversation that is gone is ever sent.
      if (heldSends.take(conversationHex()).length > 0) void persistHeldSends(s.account.accountId);
      props.onLeave();
    } catch (err) {
      setError(errorText(err, "chat.error.delete_chat"));
    } finally {
      setBusy(false);
    }
  }

  /** ADR-0045 UX §6.2.3 — the durable `leaveRequestedAt` record. Deliberately
   *  NOT folded into {@link noteOwnRequest}: that store is pruned against the
   *  core's live pending set on every fold, and this must outlive the sweep
   *  that empties it. */
  function noteOwnLeaveRequested(): void {
    const s = auth.state();
    if (s.kind !== "session") return;
    recordLeaveRequested(
      s.account.accountId,
      base64Encode(props.entry.group.groupId),
      Date.now(),
    );
    setPendingVersion(pendingVersion() + 1);
  }



  /** ADR-0045 — the live pending requests, joined from the core's retained set
   *  (the truth about WHAT is pending) and the local observation store (WHEN it
   *  landed, from the DS row). Prunes observations for requests the core no
   *  longer holds in the same pass. */
  const pendingRequests = createMemo<PendingRequest[]>(() => {
    membersVersion();
    pendingVersion();
    const now = pendingClock();
    const s = auth.state();
    if (s.kind !== "session") return [];
    // ADR-0045 UX §6.2.7 — the pending notice describes changes that will
    // apply to a group that no longer contains us, and the annotations
    // describe a roster that will never move again. Emptied at the SOURCE so
    // the notice, the row annotations, `Waiting to be added` and the exit
    // block's `waitingRoleChange` all fall silent together rather than one
    // render site at a time.
    if (membershipEnded()) return [];
    const groupIdB64 = base64Encode(props.entry.group.groupId);
    let records: ProposalRecordView[];
    try {
      records = props.entry.group.pendingProposals() as ProposalRecordView[];
    } catch {
      return [];
    }
    const observations = loadObservations(s.account.accountId, groupIdB64);
    const { requests, liveKeys } = foldPendingRequests(records, observations, now);
    // An own request that left the pending set without landing is no longer
    // detected here: since ADR-0050 2.5a it is RE-REQUESTED from its durable
    // intent record (`runSupersededRerequests`, above), and listed in `Not
    // applied` only if that re-request stops. The observation is pruned as
    // before — it carries only the quiet window, never the request.
    pruneObservations(s.account.accountId, groupIdB64, liveKeys);
    return visibleRequests(requests, canManageMembers());
  });

  /** ADR-0045 S7 residual 1, widened by ADR-0050 2.5a — the `Not applied`
   *  rows the user has not dismissed. A row is written when a re-request
   *  stops; the pass bumps `pendingVersion` when it writes one. */
  const unappliedAdds = createMemo<UnappliedRequest[]>(() => {
    // Reading `pendingRequests()` FIRST is the dependency that orders these two
    // memos: that one is where the detection runs and the row is written, and a
    // Solid memo has no guaranteed evaluation order relative to a sibling. Read
    // the other way round, a freshly detected row would not surface until the
    // next unrelated bump.
    pendingRequests();
    membersVersion();
    pendingVersion();
    const s = auth.state();
    if (s.kind !== "session") return [];
    // A group we are no longer in freezes: the pending surface is emptied at
    // the source (above) and this goes with it, for the same reason — nothing
    // here can be acted on any more.
    if (membershipEnded()) return [];
    return [
      ...loadUnapplied(
        s.account.accountId,
        base64Encode(props.entry.group.groupId),
      ).values(),
    ];
  });

  /** Dismiss one `Not applied` row. It stays dismissed. */
  function onDismissUnapplied(key: string): void {
    const s = auth.state();
    if (s.kind !== "session") return;
    dismissUnapplied(
      s.account.accountId,
      base64Encode(props.entry.group.groupId),
      key,
    );
    setPendingVersion(pendingVersion() + 1);
  }

  const pendingRequestNotice = createMemo(() => pendingNotice(pendingRequests()));
  const pendingAdds = createMemo(() => waitingToBeAdded(pendingRequests()));


  /** The identity treatment for a pending add — an accepted contact's display
   *  name when we have one, else the short fingerprint, matching what the
   *  roster and the join-request rows already do. */
  function labelForAddRequest(req: PendingRequest): string {
    return labelForSignatureKey(req.addSignatureKeyB64);
  }

  /** The same treatment, reachable from a bare signature key — the `Not applied`
   *  rows have left the pending set, so they no longer have a
   *  {@link PendingRequest} to carry it. */
  function labelForSignatureKey(keyB64: string | undefined): string {
    if (keyB64 === undefined) return UNNAMED_MEMBER();
    return memberListLabel(keyB64);
  }

  // ADR-0028 OQ2 — resolve which member's `/g/` link each pending request
  // arrived on. Read the join-attribution envelopes (from the knock queue since
  // ADR-0053) and build a `joinerKeyB64 -> { issuerKeyB64, issuerInRoster }`
  // map; `issuerInRoster` checks the attribution's issuer pubkey against the live
  // roster. Advisory metadata for the admin — the role-gated approval gate is the
  // boundary, so a missing / failed lookup never blocks Approve.
  //
  // ADR-0053 — the attributions live in the knock queue and the read pages it
  // from the start, so it is taken only when the pending joiner set changes,
  // at most once per 10 s per group (`chat/join-attribution-gate.ts`); an
  // empty pending set clears the map with no read. The roster check is done
  // at read time below (not fetch time), so a roster change needs no re-read.
  const [joinAttributionKeys, setJoinAttributionKeys] = createSignal<Map<string, string>>(
    new Map(),
  );
  const joinAttributions = createMemo(() => {
    const rosterB64 = new Set(members().map((m) => base64Encode(m.sigPubkey)));
    const map = new Map<string, { issuerKeyB64: string; issuerInRoster: boolean }>();
    for (const [joiner, issuerKeyB64] of joinAttributionKeys()) {
      // ADR-0048 — the issuer is named through the resolver.
      map.set(joiner, { issuerKeyB64, issuerInRoster: rosterB64.has(issuerKeyB64) });
    }
    return map;
  });
  let attributionRetry: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => {
    if (attributionRetry !== undefined) clearTimeout(attributionRetry);
  });
  const [attributionTick, setAttributionTick] = createSignal(0);
  // This view starts with an empty map, so whatever set the gate remembers
  // from an earlier mount no longer describes what is on screen.
  joinAttributionGate.forget(props.entry.groupIdB64);
  createEffect(() => {
    attributionTick();
    if (!canManageMembers()) return;
    const pending = pendingJoins().map((k) => base64Encode(k));
    const gid = props.entry.group.groupId;
    const groupIdB64 = props.entry.groupIdB64;
    const decision = joinAttributionGate.decide(groupIdB64, pending, Date.now());
    if (decision.kind === "clear") {
      setJoinAttributionKeys(new Map());
      return;
    }
    if (decision.kind === "keep") return;
    if (decision.kind === "wait") {
      if (attributionRetry !== undefined) clearTimeout(attributionRetry);
      attributionRetry = setTimeout(() => {
        attributionRetry = undefined;
        setAttributionTick((n) => n + 1);
      }, decision.retryInMs);
      return;
    }
    void (async () => {
      try {
        const attrs = await contacts.fetchJoinAttributions(gid);
        const map = new Map<string, string>();
        for (const a of attrs) {
          map.set(base64Encode(a.joinerSignatureKey), base64Encode(a.issuerSignaturePubkey));
        }
        setJoinAttributionKeys(map);
        joinAttributionGate.fetched(groupIdB64, pending);
      } catch {
        // best-effort advisory metadata — leave the map as-is; the set stays
        // "changed" and is read again once the floor allows.
      }
    })();
  });

  let nextTempId = 1;

  // Our own MLS leaf in this group. A POSITION, not an identity:
  // it is the input `ownSignatureKeyB64` resolves through, and the
  // handle the MLS/SFU surfaces take. Nothing that attributes
  // authorship may compare it (ADR-0023 did, and that is what let a
  // member added into a vacated leaf inherit its predecessor's
  // history). Re-read on every membersVersion bump (a Remove/Add
  // can renumber leaves — which is the whole point).
  const ownLeaf = createMemo<number | undefined>(() => {
    membersVersion();
    try {
      return props.entry.group.ownLeafIndex;
    } catch {
      return undefined;
    }
  });

  // ADR-0024 — `currentTime` ticks every 2 s so the typing label
  // re-evaluates `activeTypers(nowMs)` even with no traffic (an
  // entry whose `true` aged out drops without waiting for the next
  // poll). Driven by the same interval that prunes the map.
  const [currentTime, setCurrentTime] = createSignal(Date.now());

  /** ADR-0024 — resolve a member's display label from its signature
   *  key, reusing the roster's pubkey → accepted-contact-displayName
   *  resolution. Falls back to a short fingerprint when the member
   *  isn't a known accepted contact (matching the roster's posture).
   *
   *  Keyed on the identity rather than the leaf so a member who left
   *  cannot be relabelled with the name of whoever took their slot. */
  const labelForIdentity = (identity: MemberIdentity): string => {
    if (identity === OWN_REACTION_SENTINEL) return t("chat.author.a_member");
    return memberName(identity);
  };

  // -------------------------------------------------------------------------
  // ADR-0045 UX §6.1 — ownership transfer
  //
  // Every rule and every string lives in `chat/ownership-transfer.ts`; what is
  // here is the wiring that feeds it live group state and renders its verdicts.
  // -------------------------------------------------------------------------

  /** The roster's OWN identity treatment, in the roster's own order, as the
   *  picker reads it. Built here rather than in the module so the picker and
   *  the members list cannot render the same person differently — that is the
   *  one thing the user cross-checks. */
  const rosterViews = createMemo(() =>
    members().map((m) => {
      const keyB64 = base64Encode(m.sigPubkey);
      return {
        leafIndex: m.leafIndex,
        signatureKeyB64: keyB64,
        label: memberListLabel(keyB64),
      };
    }),
  );

  /** The role table's owner key, base64. `undefined` for an open / legacy
   *  group. */
  const ownerKeyB64 = createMemo<string | undefined>(() => {
    membersVersion();
    try {
      const o = props.entry.group.roleTableOwner() as Uint8Array | undefined;
      return o === undefined ? undefined : base64Encode(o);
    } catch {
      return undefined;
    }
  });

  /** The admin set as base64 keys. The panel's existing `adminPubkeys()` is
   *  hex; the transfer surface keys on base64 throughout because that is what
   *  the observation store and the accepted-contact map already use. */
  const adminKeysB64 = createMemo<Set<string>>(() => {
    membersVersion();
    const set = new Set<string>();
    try {
      const flat = props.entry.group.roleTableAdmins() as Uint8Array;
      for (let i = 0; i + 32 <= flat.length; i += 32) {
        set.add(base64Encode(flat.slice(i, i + 32)));
      }
    } catch {
      // open / legacy group with no role table — no admins.
    }
    return set;
  });

  // -------------------------------------------------------------------------
  // ADR-0073 "Phase 2 revised" — a member reset their account keys
  // -------------------------------------------------------------------------
  //
  // Nothing here has a surface (R.0): no flag, no line, no row. The identity
  // gate accepted the reset and re-pinned (`contacts/reset-verdict.ts`); this
  // block does the one thing the open chat owes it in a group this account
  // administers: propose, silently, the removal of the leaves still bound to
  // a superseded key (R.7 case 2). The decisions are `chat/reset-repair.ts`;
  // the one-to-one and the owned group continue in a successor founded by
  // the background runtime (`chat/BackgroundReceiveContext.tsx`).

  const groupClosed = useGroupClosed();

  /** Every key an accepted reset retired -> its account id. */
  const supersededKeys = createMemo<Map<string, string>>(() => {
    acceptedVersion();
    return loadSupersededKeys();
  });

  /** The roster as leaves, `undefined` on a torn read (never "nobody"). */
  const rosterLeaves = (): readonly RosterLeafView[] | undefined => {
    membersVersion();
    try {
      return props.entry.group.members() as RosterLeafView[];
    } catch {
      return undefined;
    }
  };

  /** In-flight removals, keyed on the superseded key, so a re-run of the
   *  driver (every roster or store change) cannot propose twice. */
  const evictionInFlight = new Set<string>();

  /**
   * R.7 case 2 — propose the removal of one leaf bound to a superseded key.
   * Rides the same path as a `Remove` the admin would tap (the membership
   * retry, the own-request record, the sweep) minus the confirmation: there
   * is no decision, the old devices are not the person's any more. Silent
   * on failure; the next roster change tries again.
   */
  async function proposeResetEviction(supersededKeyB64: string, leafIndex: number): Promise<void> {
    if (evictionInFlight.has(supersededKeyB64)) return;
    evictionInFlight.add(supersededKeyB64);
    try {
      const removeRun = () =>
        contacts.removeMemberFromGroup(props.requireIdentity(), props.entry.group, leafIndex);
      const proposed = await withMembershipRetry(
        { kind: "remove", subjectKeyB64: supersededKeyB64 },
        removeRun,
      );
      chat.markOwnPublishedCommit(
        props.entry.groupIdB64,
        proposed.kind === "committed" ? proposed.commitMessageId : proposed.proposalMessageId,
      );
      if (proposed.kind === "proposed") noteOwnRequest(`remove:${leafIndex}`);
      await sweepNow();
      setMemberCount(peopleCountOf(props.entry.group));
      setMembersVersion(membersVersion() + 1);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[reset-repair] removal of a superseded leaf not proposed", err);
    } finally {
      evictionInFlight.delete(supersededKeyB64);
    }
  }

  createEffect(() => {
    const keys = supersededKeys();
    if (keys.size === 0) return;
    const roster = rosterLeaves();
    const pending = pendingRequests();
    const ended = membershipEnded();
    const owner = ownerKeyB64();
    const role = ownRole();
    const kind = isGroupChat() ? "group" : "chat";
    untrack(() => {
      const leaves = roster?.map((l) => ({
        leafIndex: l.leafIndex,
        personKeyB64: base64Encode(personKeyOf(l)),
      }));
      for (const supersededKeyB64 of keys.keys()) {
        if (resetCase({ kind, ownerKeyB64: owner, supersededKeyB64 }) !== "repair") continue;
        const plan = repairPlan({
          ownRole: role,
          roster: leaves,
          supersededKeyB64,
          pendingRemoveLeaves: pending
            .filter((r) => r.kind === "remove" && r.targetLeaf !== undefined)
            .map((r) => r.targetLeaf as number),
          removeInFlight: evictionInFlight.has(supersededKeyB64),
          membershipEnded: ended,
        });
        if (plan.removeLeaf !== null) void proposeResetEviction(supersededKeyB64, plan.removeLeaf);
      }
    });
  });

  /**
   * R.7 case 2 / UX 16.5 — a commit that only swapped the devices of people
   * who stay leaves no `Members changed` row. Called at both membership-diff
   * sites (the poll and this device's own sweep) with the PERSON keys either
   * side; people are compared by the account this device knows each key
   * under, so a superseded key and its successor are one person. A torn read
   * suppresses nothing.
   */
  function suppressDeviceSwapRow(
    personsBefore: readonly string[] | null,
    personsAfter: readonly string[] | null,
    commitMessageId: number | undefined,
  ): void {
    const s = auth.state();
    if (s.kind !== "session") return;
    const superseded = supersededKeys();
    const swap = deviceSwapAcrossCommits({
      personKeysBefore: personsBefore,
      personKeysAfter: personsAfter,
      accountOfKey: (k) => superseded.get(k) ?? accountIdB64OfKey(k),
      isSuperseded: (k) => superseded.has(k),
      awaited: awaitedSwaps(s.account.accountId, props.entry.groupIdB64),
      nowMs: Date.now(),
    });
    setAwaitedSwaps(s.account.accountId, props.entry.groupIdB64, swap.awaited);
    if (!swap.suppress) return;
    const messageId =
      commitMessageId ??
      rows().reduce((max, r) => (r.kind === "incoming" ? Math.max(max, r.event.messageId) : max), 0);
    setRows(
      rows().filter(
        (r) =>
          !(
            r.kind === "incoming" &&
            r.event.kind === "system" &&
            r.event.systemKind === "commit" &&
            r.event.messageId === messageId
          ),
      ),
    );
  }

  /** This device's own signature key, base64 — resolved through the roster
   *  rather than stored, so a leaf renumber cannot make it name someone else.
   *
   *  This is the value every authorship decision on our side uses: the
   *  outgoing row's author stamp, our own reaction attribution, and the
   *  "read by everyone else" exclusion. */
  const ownSignatureKeyB64 = createMemo<string | undefined>(() => {
    const leaf = ownLeaf();
    if (leaf === undefined) return undefined;
    return rosterViews().find((m) => m.leafIndex === leaf)?.signatureKeyB64;
  });

  /** §6.1.3 — every member except the current owner, marked (never filtered)
   *  for admin, in the roster's order. */
  const transferPickerRows = createMemo<TransferCandidate[]>(() =>
    transferCandidates(rosterViews(), ownerKeyB64(), adminKeysB64()),
  );

  /**
   * §6.1.6 / §6.1.8 — the role change already waiting to apply, if any.
   *
   * `waiting` and not merely `pending` is the whole point: below the quiet
   * window the control stays ENABLED and a second tap is refused by the
   * client's C-65 pre-check. A request that is not yet rendered must not
   * silently disable a control with nothing to explain it.
   *
   * The successor label is resolved through the LIVE roster HERE, at render
   * time, never from a stored label: a successor who has since left degrades to
   * the unnamed variant rather than to a name the roster no longer carries.
   */
  const waitingRoleChange = createMemo(() => {
    const hit = pendingRequests().find(
      (r) => r.kind === "role-change" && r.state === "waiting",
    );
    if (hit === undefined) return null;
    const successor =
      hit.transferToB64 === undefined
        ? undefined
        : rosterViews().find((m) => m.signatureKeyB64 === hit.transferToB64);
    return {
      // Our own PROMOTE is not our own transfer — see the module's doc on this
      // field. Only a record carrying a successor key is a transfer.
      ownTransfer: hit.own && hit.transferToB64 !== undefined,
      successorLabel: successor?.label ?? null,
    };
  });

  /** §6.1.1 — the exit block: which of the two role-disjoint controls this
   *  reader gets, and why the other one is disabled. */
  const exitBlockState = createMemo<ExitBlock>(() =>
    exitBlock({
      usesProposalPath: usesProposalPath(),
      isOwner: isOwner(),
      ownLeafKnown: ownLeaf() !== undefined,
      candidateCount: transferPickerRows().length,
      waitingRoleChange: waitingRoleChange(),
      ownRemovalPending:
        rowAnnotation(pendingRequests(), ownLeaf() ?? -1, ownLeaf()) !== null,
      // §6.2.7 — beats every other arm. Distinct from `ownRemovalPending`
      // above in the way that matters: that is a request in flight over a
      // group we are still fully in, this is the terminal state after it
      // landed.
      membershipEnded: membershipEnded(),
    }),
  );

  /** The owner's arm of the exit block, or `null` for every other reader.
   *  Split into two narrowed memos so the JSX can use `<Show>`'s accessor form
   *  rather than casting a union inside the template. */
  const ownerExit = createMemo(() => {
    const block = exitBlockState();
    return block.kind === "owner" ? block : null;
  });
  /** The non-owner's arm. Carries no `transfer` field at all — a member's
   *  transfer is refused by the core and by every peer, and nothing they could
   *  do would make it work, which is the case where "a control that always
   *  refuses is worse than no control" still holds. */
  const memberExit = createMemo(() => {
    const block = exitBlockState();
    return block.kind === "member" ? block : null;
  });

  // The transfer step's own state. Opening it is what §6.1.5 calls the
  // confirmation — there is no second dialog behind it.
  const [transferOpen, setTransferOpen] = createSignal(false);
  /** The picked successor's signature key, base64 — never a leaf index, because
   *  OpenMLS hands a vacated leaf to the next Add. */
  const [transferPick, setTransferPick] = createSignal<string | null>(null);
  const [transferOutgoing, setTransferOutgoing] = createSignal<OutgoingOwnerChoice>(
    PRESELECTED_OUTGOING_OWNER,
  );

  const transferSelected = createMemo<TransferCandidate | undefined>(() => {
    const pick = transferPick();
    return pick === null
      ? undefined
      : transferPickerRows().find((r) => r.signatureKeyB64 === pick);
  });
  const transferAction = createMemo(() => transferActionLabel(transferSelected()));

  function openTransferStep(): void {
    setError(null);
    setNotice(null);
    setTransferPick(null);
    setTransferOutgoing(PRESELECTED_OUTGOING_OWNER);
    setTransferOpen(true);
  }

  /**
   * §6.1 — publish the ownership transfer, then sweep it (ADR-0045).
   *
   * The shape is `onToggleAdmin`'s, deliberately and for its stated reason: a
   * role change is owner-only and the owner is rank 0, so the sweep that
   * follows is this same device and in practice the change is immediate. A
   * transfer IS a role change — not a new commit type, just the owner-only
   * role-table rewrite with the owner slot moved — so anything else here would
   * be a second, differently-shaped route into membership.
   *
   * Two things beyond the toggle's shape. The observation record carries the
   * SUCCESSOR'S KEY (§6.1.10), which is the only thing that later distinguishes
   * *our pending role change is a transfer* from *our pending role change is a
   * promote*. And the outcome copy names the transferor's own new role only
   * when the sweep actually committed: half the panel changes under them in one
   * tap, and a request that has not landed must not claim it has.
   */
  async function onTransferOwnership(): Promise<void> {
    const successor = transferSelected();
    if (successor === undefined) return;
    const outgoing = transferOutgoing();
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      const result = await withMembershipRetry(
        {
          kind: "transfer-ownership",
          subjectKeyB64: successor.signatureKeyB64,
          outgoing,
        },
        () =>
          contacts.transferGroupOwnership(
            props.requireIdentity(),
            props.entry.group,
            base64Decode(successor.signatureKeyB64),
            outgoing,
          ),
      );
      chat.markOwnPublishedCommit(
        props.entry.groupIdB64,
        result.kind === "committed"
          ? result.commitMessageId
          : result.proposalMessageId,
      );
      if (result.kind === "proposed") {
        noteOwnRequest("role-change", successor.signatureKeyB64);
      }
      setTransferOpen(false);
      const swept = await sweepNow();
      setMembersVersion(membersVersion() + 1);
      const landed =
        result.kind === "committed" || swept?.kind === "committed";
      setNotice(() =>
        transferOutcomeNotice(
          landed ? "committed" : "proposed",
          successor.label,
          outgoing,
        ),
      );
      void refresh();
    } catch (err) {
      setError(errorText(err, "chat.error.transfer"));
    } finally {
      setBusy(false);
    }
  }

  /**
   * §6.1.9 — tell the new owner, best-effort and fail-open.
   *
   * Nothing on the wire announces a transfer: the role table simply changes and
   * every other device learns of it by re-reading. So this is a client-side
   * INFERENCE from an observed change, and it is structurally unable to fire on
   * a device that first observes the table already naming it owner — a fresh
   * device, a restore, a first sync. Hence fail-open everywhere, and hence the
   * notice must never be the only way a user learns their role; the panel's
   * affordances stay the primary signal, exactly as they are for admin today.
   *
   * The notice is decided BEFORE the new owner is remembered, or the comparison
   * would always be against itself. It then lives in a signal until dismissed,
   * rather than being recomputed: the very next `membersVersion` bump would see
   * the remembered owner equal the current one and correctly return `null`, so
   * a recomputed notice would vanish on the next poll.
   */
  const [newOwnerBanner, setNewOwnerBanner] = createSignal<NewOwnerNotice | null>(
    null,
  );
  createEffect(() => {
    membersVersion();
    const s = auth.state();
    if (s.kind !== "session") return;
    const current = ownerKeyB64();
    if (current === undefined) return;
    const groupIdB64 = base64Encode(props.entry.group.groupId);
    const previous = loadLastObservedOwner(s.account.accountId, groupIdB64);
    // Resolved through the live roster; `null` when the previous owner has left
    // in a later commit (transfer and leave are necessarily separate ones).
    const previousLabel =
      previous === undefined
        ? null
        : (rosterViews().find((m) => m.signatureKeyB64 === previous)?.label ??
          null);
    const notice = newOwnerNotice(
      previous,
      current,
      ownSignatureKeyB64(),
      previousLabel,
    );
    if (notice !== null) setNewOwnerBanner(notice);
    rememberObservedOwner(s.account.accountId, groupIdB64, current);
  });

  /** ADR-0024 — the typing-indicator string, or `null` when nobody
   *  (other than us) is currently typing. For a 1×1 chat a bare
   *  "typing…" reads cleaner than a name; for a group we name the
   *  single typer, else "Several people are typing…". */
  const typingIndicator = createMemo<string | null>(() => {
    const now = currentTime();
    const own = ownSignatureKeyB64();
    const typers = activeTypers(typingMap(), now).filter((m) => m !== own);
    if (typers.length === 0) return null;
    // A 1×1 chat has exactly one peer — skip the name (the chat
    // header already shows who it is). ADR-0062: the kind, not the count.
    const oneOnOne = !isGroupChat();
    if (typers.length === 1) {
      const who = typers[0] as MemberIdentity;
      return oneOnOne
        ? t("chat.typing.one_to_one")
        : t("chat.typing.named", { name: labelForIdentity(who) });
    }
    return t("chat.typing.several");
  });

  /** Look up the author IDENTITY of the displayed message with this
   *  DS `messageId`. `undefined` ⇒ the target is unknown or has no
   *  known author, so the edit/delete gate fails closed. `extra`
   *  lets the receive loop also scan rows arriving in the same
   *  batch (which aren't in `rows()` yet). */
  function authorIdentityForMessageId(
    messageId: number,
    extra: readonly TimelineRow[] = [],
  ): MemberIdentity | undefined {
    for (const row of rows()) {
      if (rowMessageId(row) !== messageId) continue;
      return rowAuthorIdentity(row);
    }
    for (const row of extra) {
      if (rowMessageId(row) !== messageId) continue;
      return rowAuthorIdentity(row);
    }
    return undefined;
  }

  /** Whether the target message is known at all (in the live
   *  timeline or this batch). An op against an unknown target is
   *  dropped (fail-closed; the ADR — we never materialise a
   *  bubble for a target we've never displayed). */
  function targetKnown(
    messageId: number,
    extra: readonly TimelineRow[],
  ): boolean {
    return (
      rows().some((r) => rowMessageId(r) === messageId) ||
      extra.some((r) => rowMessageId(r) === messageId)
    );
  }

  /** ADR-0023 — fold one incoming op event into the overlay map,
   *  enforcing the author-only gate for edit/delete. Returns the
   *  (possibly unchanged) map; `===` to the input when the op was
   *  dropped, so the caller can skip a needless signal write.
   *
   *  Gate (fail-closed, invariant #4):
   *   - target unknown                 → DROP
   *   - op.senderKey undefined          → DROP (unattributable)
   *   - edit/delete + senderKey ≠ author → DROP
   *   - reaction                         → accept from any member
   *
   *  Everything here keys on the sender's signature KEY, resolved
   *  from its MLS leaf back in the receive loop. Keying on the leaf
   *  is what let a member added into a departed member's vacated
   *  leaf edit and delete that member's history and toggle off their
   *  reactions. */
  function applyIncomingOp(
    e: Extract<IncomingChatEvent, { kind: "op" }>,
    extra: readonly TimelineRow[],
    prev: Map<number, MessageOpState>,
  ): Map<number, MessageOpState> {
    if (!targetKnown(e.target, extra)) return prev;
    if (e.senderKey === undefined) return prev;
    if (e.op === "reaction") {
      return applyOp(prev, {
        op: "reaction",
        target: e.target,
        emoji: e.emoji ?? "",
        reactor: e.senderKey,
      });
    }
    // edit / delete — author-only (fail-closed, invariant #4).
    const author = authorIdentityForMessageId(e.target, extra);
    if (!editDeleteAuthorized(author, e.senderKey)) return prev;
    if (e.op === "edit") {
      return applyOp(prev, { op: "edit", target: e.target, text: e.text });
    }
    // Section 4.1 — an author-gated delete takes the message's pin records
    // with it (shared and personal).
    void pins.messageDeleted(props.entry.groupIdB64, e.target);
    return applyOp(prev, { op: "delete", target: e.target });
  }

  /**
   * Consume a batch of receive-path events — the ONE place a decoded event
   * becomes UI state.
   *
   * Two callers, and that is the point. The poll is the obvious one. The other
   * is the ADR-0045 committer sweep: its verdict applies every journal row the
   * DS ordered below its own commit and then advances the SAME cursor over
   * them, so those rows are gone from the poll's future forever. A chat message
   * another member sent in the window between the sweep's stand-down re-read
   * and its publish lands in exactly that set, and before this it was decrypted
   * and then thrown away. Feeding both paths through this function is what
   * keeps the two from drifting — every envelope kind is folded here once.
   *
   * Events must arrive in journal order; the ops fold and the `fresh` scan both
   * assume it.
   */
  function consumeEvents(events: readonly IncomingChatEvent[]): void {
    // De-dup: the same DS message id can come back across
    // refreshes after a reset; outgoing rows we sent in
    // this session also surface here on a second refresh
    // through a different path (we already filter the echo
    // in ChatClient via the per-group echo registry, so
    // duplicates here are rare).
    const incomingIds = new Set(
      rows().flatMap((r) =>
        r.kind === "incoming" ? [r.event.messageId] : [],
      ),
    );
    // ADR-0023 — split the batch: chat / system / callSignaling
    // events become timeline rows as before; edit / delete /
    // reaction OPS fold into the per-message overlay instead of
    // rendering as standalone bubbles. Each op is applied once
    // (the poll is incremental, so a given op id arrives once).
    const fresh: TimelineRow[] = [];
    /** ADR-0045 — did this batch carry a membership proposal? Drives the
     *  panel refresh and the tail sweep below. */
    let sawProposal = false;
    let ops = msgOps();
    let opsChanged = false;
    // ADR-0024 — accumulate typing / read-receipt folds across the
    // batch, then write each signal once at the end.
    let typing = typingMap();
    let typingChanged = false;
    const nowMs = Date.now();
    const retired = supersededKeys();
    const senderSuperseded = (k: string): boolean => dropsSupersededSender(k, retired);
    const moved =
      retired.size > 0 &&
      laterSegmentExists(lineageView.lineage(), hexOf(props.entry.group.groupId));
    const inertSender: InertSender = {
      sender: (k) =>
        supersededSenderInert({
          senderKeyB64: k,
          supersededKeys: retired,
          kind: conversationKind(),
          ownerKeyB64: ownerKeyB64(),
          moved,
        }),
      senderKeyOfLeaf: (leaf) => personKeyOfLeafB64(leaf),
    };
    for (const e of events) {
      // ADR-0073 R.5 / R.8 — a row from a leaf bound to a key an accepted
      // reset retired (a removed device still holding the old keys) is
      // dropped before it is stored or shown.
      // The same rule as the background settle (`background-events.ts`): a
      // call signal from such a sender never rings, and in a conversation
      // that cannot be repaired in place for that reset, or that already
      // moved, its control and service envelopes are dropped too.
      if (dropsFromSupersededSender(e, senderSuperseded, inertSender)) continue;
      if (e.kind === "op") {
        // ADR-0024 — typing / read-receipt are ephemeral status,
        // not message overlays: fold them into their own state.
        if (e.op === "typing") {
          if (e.senderKey !== undefined) {
            const createdAtMs = Date.parse(e.createdAt);
            // A malformed timestamp parses to NaN — treat it as
            // stale (drop the `true`) rather than resurrecting it.
            const stamp = Number.isNaN(createdAtMs)
              ? nowMs - TYPING_EXPIRY_MS
              : createdAtMs;
            const next = foldTyping(
              typing,
              e.senderKey,
              e.isTyping ?? false,
              stamp,
              nowMs,
            );
            if (next !== typing) {
              typing = next;
              typingChanged = true;
            }
          }
          continue;
        }
        if (e.op === "read-receipt") {
          readWatermarkStore.ingest(props.entry.groupIdB64, e.senderKey, e.target);
          continue;
        }
        // Fold the edit/delete/reaction op into the overlay first
        // so a chat row + an op that targets it that arrive in the
        // same batch both land (the chat row appears in `fresh`
        // below; the author-leaf lookup also scans `fresh`).
        const next = applyIncomingOp(e, fresh, ops);
        if (next !== ops) {
          ops = next;
          opsChanged = true;
        }
        continue;
      }
      // ADR-0027 (G-3) — an external-join request is not a timeline row; it
      // surfaces in the members panel via the `pendingJoins` memo. The
      // membersVersion bump after this loop refreshes that memo, so just
      // skip it here (don't materialise a chat bubble for it).
      //
      // Except for one thing the memo cannot see. A request the core did NOT
      // retain is absent from `pendingExternalJoins()`, so it renders as
      // nothing at all — which is correct about the Approve button and silent
      // about the joiner, who is now waiting on an admin nobody told. §7.1
      // recorded that state as unsurfaceable; `retained` is what made it
      // statable, so it is stated.
      if (e.kind === "externalJoinRequest") {
        if (externalJoinAffordance({ retained: e.retained }).kind === "informational") {
          setUnkeptJoinRequest(true);
        }
        continue;
      }
      // ADR-0042 — a push-routing-secret distribution envelope: persist the
      // secret (idempotent) + subscribe under the derived opaque `push_gid`.
      // Never a timeline bubble. Fire-and-forget (best-effort).
      //
      // GATED on the ADR-0022 role table, ranked owner > admin > member:
      // the OWNER may rotate whatever is stored; an ADMIN may only seed a
      // device that holds none (so an admin-performed add-member still
      // delivers routing to the new member); a plain member may never
      // change it. An identical re-broadcast is always idempotent-accepted
      // from anyone, so the post-add-member redistribute keeps working.
      // Without this, any single member could unilaterally rotate the
      // group's routing id and strand an orphan push-gw subscription on
      // every churn.
      if (e.kind === "pushRoutingSecret") {
        const s = auth.state();
        if (s.kind === "session") {
          const groupIdB64 = base64Encode(props.entry.group.groupId);
          void receivePushRoutingSecretGated(
            s.account.accountId,
            props.entry.group,
            groupIdB64,
            e.senderLeaf,
            e.secret,
            // ADR-0044 — the minter's stamp. The gate refuses anything
            // ahead of our own epoch (`reject-future`) and anything not
            // strictly newer than what we hold (`reject-stale`), so a
            // stale re-broadcast can no longer retire the live routing id.
            e.mintEpoch,
          ).catch((err) => {
            // eslint-disable-next-line no-console
            console.warn("push-routing secret receive failed", err);
          });
        }
        continue;
      }
      // ADR-0045 S4b — a committer-liveness beacon. Not a timeline row: it
      // is an observation about WHO can sweep the pending proposal set,
      // recorded here from the only two things the field-less envelope
      // deliberately does not carry — WHO, from the MLS-authenticated sender
      // leaf, and HOW OLD, from the DS row's server-assigned timestamp
      // (never a local clock, and never anything the sender asserted).
      //
      // The leaf is resolved to a signature KEY here and the key is what is
      // stored; the rank is derived later, at verdict time, against the role
      // table as it stands then.
      if (e.kind === "committerBeacon") {
        const s = auth.state();
        if (s.kind === "session" && e.senderLeaf !== undefined) {
          const senderKey = signatureKeyOfLeaf(e.senderLeaf);
          if (senderKey !== undefined) {
            recordBeaconObservation(
              s.account.accountId,
              base64Encode(props.entry.group.groupId),
              base64Encode(senderKey),
              Date.parse(e.createdAt),
            );
          }
        }
        continue;
      }
      // ADR-0045 — a membership proposal is a PANEL signal, never a timeline
      // row (UX §10). Record when it landed, using the DS row's timestamp so
      // the quiet window means the same thing on every device, then let the
      // panel read the core's retained set for what is actually pending.
      //
      // A record the receipt gate REFUSED (`stored === false`) is recorded
      // nowhere and rendered nowhere: no sweep will ever commit it, so it is
      // not a pending request. The drop is invisible on the receiving side by
      // design — it is not that peer's business.
      if (e.kind === "membershipProposal") {
        if (e.stored) {
          const s = auth.state();
          if (s.kind === "session") {
            recordObservation(
              s.account.accountId,
              base64Encode(props.entry.group.groupId),
              proposalKeyOfEvent(e),
              Date.parse(e.createdAt),
              false,
            );
            sawProposal = true;
          }
        }
        continue;
      }
      // Call signaling is handed to the call dispatcher — once per journal
      // row, in order — and never becomes a timeline row. Dispatching it from
      // the rows is what re-applied every offer on every poll.
      if (e.kind === "callSignaling") {
        callHost.ingestSignaling(props.entry.groupIdB64, {
          messageId: e.messageId,
          createdAt: e.createdAt,
          payloadBytes: e.payloadBytes,
          ...(e.senderLeaf === undefined ? {} : { senderLeaf: e.senderLeaf }),
          ...(e.senderMemberKey === undefined ? {} : { senderMemberKey: e.senderMemberKey }),
          ...(e.senderKey === undefined ? {} : { senderPersonKey: e.senderKey }),
        });
        continue;
      }
      // ADR-0051 — the shared group name. Through the role-gated verdict
      // under the per-group lock; on accept the store re-titles the header,
      // the list row and the group info in place. Never a timeline row.
      if (e.kind === "group-name") {
        void groupNames.receive(props.entry, e).catch((err) => {
          // eslint-disable-next-line no-console
          console.warn("group name receive failed", err);
        });
        continue;
      }
      // ADR-0073 R.6 — a successor claim. Judged by the claim rule under the
      // per-group lock; never a timeline row, whatever the verdict.
      if (e.kind === "group-closed") {
        void groupClosed.receive(props.entry, e).catch((err) => {
          // eslint-disable-next-line no-console
          console.warn("successor claim receive failed", err);
        });
        continue;
      }
      // ADR-0052 decision 3 — a shared pin or unpin of one message. Through
      // the role-gated verdict under the per-group lock; on accept the store
      // changes and the pinned bar re-derives. Never a timeline row.
      if (e.kind === "pin") {
        void pins.receive(props.entry, e).catch((err) => {
          // eslint-disable-next-line no-console
          console.warn("pin receive failed", err);
        });
        continue;
      }
      // ADR-0059 section 6 — a member's shared channels. Filed per member
      // (account key resolved at receipt) under the per-group lock; never a
      // timeline row.
      if (e.kind === "shared-channels") {
        void sharedChannels.receive(props.entry, e).catch((err) => {
          // eslint-disable-next-line no-console
          console.warn("shared channels receive failed", err);
        });
        continue;
      }
      // ADR-0048 §2 trigger 4 — another member asked this group to introduce
      // itself. Answered by the profile provider through the throttled growth
      // path; never a timeline row. This device's own request (re-fetched
      // after a reload, when the own-echo registry is empty) is not a question
      // for it, and an unattributable sender is dropped.
      if (e.kind === "profileRequest") {
        const own = chats.identity();
        if (e.senderKey !== undefined && !(own !== null && isOwnKeyB64(own, e.senderKey))) {
          profiles.requested(props.entry.groupIdB64);
        }
        continue;
      }
      // ADR-0048 — a member's profile. Filed in the profile store under the
      // sender's signature key resolved AT RECEIPT (C-68) and nowhere else;
      // never a timeline row. A malformed envelope (`profile` undefined) or an
      // unattributable sender is dropped.
      if (e.kind === "profile") {
        if (e.profile !== undefined && e.senderKey !== undefined) {
          profiles.ingest(e.senderKey, e.profile, { groupIdB64: props.entry.groupIdB64 });
        }
        continue;
      }
      // ADR-0060 — a scoped-discovery service envelope: to its handler
      // (a committer delegation, a contact's introduction offer or bundle),
      // never a timeline row.
      if (e.kind === "scopedDiscovery") {
        void intakeScopedEnvelope({
          kind: e.envelope,
          payload: e.payload,
          senderKeyB64: e.senderKey,
          oneToOne: isOneToOne(props.entry.group),
        });
        continue;
      }
      // ADR-0057 — consumed at receipt by the chat client; a stray one is
      // dropped here, never shown.
      if (e.kind === "registrarSeed") continue;
      // An unrecognised `lakko/…` service envelope: never a row.
      if (e.kind === "serviceEnvelope") continue;
      if (incomingIds.has(e.messageId)) continue;
      fresh.push({ kind: "incoming", event: e });
    }
    if (fresh.length > 0) {
      setRows([...rows(), ...fresh]);
    }
    if (opsChanged) {
      setMsgOps(ops);
    }
    if (typingChanged) {
      setTypingMap(typing);
    }
    // ADR-0045 — a proposal in this batch changed what the pending-request
    // panel should show. Bumped here rather than at either call site, so both
    // receive paths refresh it identically.
    if (sawProposal) setPendingVersion(pendingVersion() + 1);
  }

  /**
   * The user-initiated poll: the Refresh button, mount, and every membership
   * action. Shows `busy` and surfaces a failure in the error slot. The body is
   * `pollOnce`, shared with the automatic receive loop below, which runs the
   * same poll silently.
   */
  async function refresh(): Promise<void> {
    if (membershipEnded()) return;
    setError(null);
    setBusy(true);
    try {
      await pollOnce(0, undefined, false);
    } catch (err) {
      setError(errorText(err, "chat.error.refresh"));
    } finally {
      setBusy(false);
    }
  }

  /** The group's epoch, or `undefined` when the handle cannot be read. */
  function readEpoch(): bigint | undefined {
    try {
      return props.entry.group.epoch;
    } catch {
      return undefined;
    }
  }

  /**
   * One poll of this group's journal and everything that hangs off it. Throws
   * on failure; touches neither `busy` nor the error slot — the callers own
   * that. Returns the outcome so the receive loop can pace itself.
   */
  async function pollOnce(
    waitSecs: number,
    signal: AbortSignal | undefined,
    background: boolean,
  ): Promise<PollMessagesResult["outcome"] | "membership-ended"> {
    // ADR-0045 UX §6.2.8 item 1 — the poll must STOP for a group this device
    // is no longer in, and stopping is not an optimisation. `unprotect_message`
    // refuses on `!is_active()` before it looks at the message at all, and the
    // refusal is not epoch-scoped, so every fetched row would raise
    // `UseAfterEviction`, be swallowed by the per-message catch in
    // `ChatClient.applyPolledBatch`, and advance the cursor — one warning per
    // row, forever, for a group that will never produce a readable byte again.
    // A loop that logs a refusal per row per poll is indistinguishable from a
    // bug.
    //
    // A GATE rather than a disposal, because `refresh` is re-entered from a
    // dozen call sites (mount, the Refresh button, every membership action,
    // the Welcome intake). The automatic receive loop has an interval-shaped
    // lifetime and is also stopped outright (`shouldContinue`), but this gate
    // stays the authority for every caller.
    //
    // The cursor is deliberately left where the last applied row put it
    // (§6.2.13): it costs nothing, and whether a rejoin wants it reset is the
    // rejoin path's question, not this one's.
    if (membershipEnded()) return "membership-ended";
    {
      // ADR-0043 — baseline for the membership diff, captured BEFORE the poll
      // applies any commit. A Commit carries no membership delta out of the
      // poll loop, so a removal committed by SOMEONE ELSE is inferred from
      // this snapshot against the post-poll one. This device's OWN removals
      // never show up here (they were applied locally before this poll, so
      // the baseline is already post-removal); those rotate directly in
      // `onRemoveMember`.
      //
      // Re-taken under the MLS lock right before the apply (`beforeApply`):
      // a long poll parks for up to `LONG_POLL_WAIT_SECS`, and a baseline from
      // before the park would include whatever another path applied in the
      // meantime — an own removal, or a manual refresh's batch — and rotate
      // for it a second time. The pre-fetch values stand only for the
      // `cursor-moved` path, which returns before reading them.
      let membersBeforePoll = memberSignatureKeys(props.entry.group);
      // ADR-0073 R.7 case 2 — the PERSON keys, for the device-swap row.
      let personsBeforePoll: string[] | null = personKeysOrNull(props.entry.group);
      let epochBeforePoll = readEpoch();
      const result = await chat.pollMessages(
        props.requireIdentity(),
        props.entry.group,
        cursor(),
        waitSecs,
        // G-38 — the poll's apply half runs under the same per-group MLS lock
        // the commit paths take, so the cursor read-modify-write has to run in
        // there with it. Handing the accessors in is what lets `ChatClient`
        // re-read the position under the lock (was this batch consumed while we
        // were on the network?) and advance it in the same region as the apply.
        // Writing `result.cursor` back out here would land a microtask AFTER
        // the lock released, which is exactly the window a queued poll used to
        // re-read and re-feed already-consumed rows through.
        {
          read: () => cursor(),
          advance: (to) => setCursor(to),
          beforeApply: () => {
            membersBeforePoll = memberSignatureKeys(props.entry.group);
            personsBeforePoll = personKeysOrNull(props.entry.group);
            epochBeforePoll = readEpoch();
          },
        },
        signal,
      );
      // The batch was stale (another poll consumed past our `after` while we
      // were parked on the fetch). Nothing was applied and the cursor was not
      // touched — drop it. The poll that DID apply owns rendering those rows.
      // With the receive loop parked on a long poll, this is the COMMON outcome
      // of a user-initiated refresh racing it, not a corner case.
      if (result.outcome === "cursor-moved") return result.outcome;
      // ADR-0073 R.7 case 1 — a re-sent message whose original this device
      // already holds in an older segment is not a row here (the same rule
      // the background receive applies, `resend-receive.ts`).
      const resendAliases = loadKeyAliases();
      const resendHeld = await heldResendPredicate(result.events, {
        receivedGroupId: props.entry.group.groupId,
        conversationHexOf: (id) => lineageView.conversationHexOf(id),
        segmentIndexOf: (id) => lineageView.segmentIndexOf(id),
        ownPersonKeyB64: ownPersonKeyB64(chats.identity()),
        canonical: (k) => resendAliases.get(k) ?? k,
        loadLog: async () => {
          const account = accountIdB64ForLog();
          if (account === null) return null;
          const deviceKey = await getDeviceKey(account);
          return deviceKey === null ? null : loadMessageLog(account, deviceKey);
        },
      });
      // ADR-0045 — the shared consumer; see `consumeEvents`.
      consumeEvents(
        result.events.filter((e) => !(e.kind === "application" && resendHeld(e))),
      );
      // The component was torn down while this batch was being applied (a
      // route change during a long poll). The rows are decrypted and the
      // cursor has moved, but the debounced timeline persist died with the
      // component, and MLS forward secrecy means they can never be decrypted
      // again — so write them now rather than lose them. The rest of this
      // function still runs: an ADR-0043 rotation owed for a removal in this
      // batch must not be skipped because the view closed.
      if (signal?.aborted === true) void persistToLogBestEffort();
      // NB: no `setCursor` here — `pollMessages` advanced it under the lock, or
      // deliberately HELD it (`outcome: "persist-held"`) because the MLS
      // snapshot did not reach disk. The events are still consumed either way:
      // they were decrypted and the in-memory state moved. A held batch is
      // re-fetched on the next poll, which is the benign direction — see
      // `chat/durable-cursor.ts`.
      // 1.18g — mirror the post-poll member count so the
      // header re-renders after an incoming commit advances
      // the local group state (someone else added / removed a
      // member; we processed their commit in pollMessages).
      // No-op when no commits came in: setSignal short-circuits
      // on equal values.
      setMemberCount(peopleCountOf(props.entry.group));
      // 1.18h — bump membersVersion so the members panel's
      // createMemo re-reads the post-poll leaf set.
      //
      // The automatic receive loop bumps it only when this batch could have
      // changed what those readers see: the epoch moved, or it carried a
      // commit / external-join request. Every bump re-runs the members-panel
      // effects, and one of them (`fetchJoinAttributions`, for owners and
      // admins) refetches the whole journal — once per incoming chat message,
      // and once per idle long-poll timeout, is load nothing asked for. A
      // user-initiated refresh still re-reads everything.
      if (
        !background ||
        readEpoch() !== epochBeforePoll ||
        result.events.some(
          (e) => e.kind === "system" || e.kind === "externalJoinRequest",
        )
      ) {
        setMembersVersion(membersVersion() + 1);
      }
      // ADR-0043 — someone else's Remove commit just landed? Then the
      // evicted member still holds this group's push_routing_secret and
      // push-gw is still waking it. Re-key so nobody ever notifies the id it
      // can derive again. Driven off an actual before/after member-set
      // comparison, NOT off `membersVersion` — that counter is bumped
      // unconditionally after every poll, so it is a "a poll happened" tick,
      // not a membership signal.
      //
      // Owner-or-admin only, no-op on growth, and a no-op on this device's
      // own removals (their baseline is already post-removal). Fire-and-
      // forget: a rotation failure must never break the poll loop.
      //
      // The role is read off the LIVE group (`readOwnRoleNow`), never from the
      // `ownRole()` signal: that signal is refreshed by an async effect and at
      // this point still holds the PRE-poll role, so a batch carrying a
      // demotion AND a removal would have a just-demoted admin rotate into a
      // secret every peer refuses.
      {
        const s = auth.state();
        if (s.kind === "session") {
          // Snapshot the post-poll roster synchronously, before the awaited
          // role read lets another poll advance the group.
          const currentMemberKeys = memberSignatureKeys(props.entry.group);
          recordRemovedDevices(
            s.account.accountId,
            props.entry.groupIdB64,
            membersBeforePoll,
            currentMemberKeys,
          );
          // ADR-0073 R.7 case 2 — a batch that only swapped the devices of
          // people who stay leaves no `Members changed` row.
          {
            const commit = [...result.events]
              .reverse()
              .find((e) => e.kind === "system" && e.systemKind === "commit");
            suppressDeviceSwapRow(
              personsBeforePoll,
              personKeysOrNull(props.entry.group),
              commit?.messageId,
            );
          }
          // ADR-0045 UX §6.2.8 item 3 — and this is the site the finding was
          // about. Snapshotted here, in the same synchronous region as the
          // roster, because `membershipRotationDecision` would otherwise see a
          // member depart (us), read `ownRole` from the UNTOUCHED role table —
          // a `Remove` rewrites no `GroupContext` — and return "rotate", after
          // which the broadcast throws. Read off the live handle rather than
          // the memo, which an effect has not refreshed yet at this point.
          const selfEvicted = evictedNow();
          void (async () => {
            await rotatePushRoutingSecretIfMembershipShrank({
              accountId: s.account.accountId,
              group: props.entry.group,
              ownRole: await readOwnRoleNow(),
              previousMemberKeys: membersBeforePoll,
              currentMemberKeys,
              broadcast: broadcastToGroup,
              selfEvicted,
            });
          })().catch((err) => {
            // eslint-disable-next-line no-console
            console.warn("push-routing rotation failed", err);
          });
        }
      }
      // ADR-0045 — the sweep trigger. This is the ONLY thing that makes a
      // `waiting` request ever end, so it runs at the tail of every poll rather
      // than only when a proposal was seen: a verdict owed from a previous run
      // (or from a page this device already closed) has to be finished too, and
      // nothing in the batch announces one.
      //
      // Cheap when there is nothing to do: `sweepIfCommitter` short-circuits
      // synchronously — before any network — when this device is not rank 0, or
      // when nothing is pending and no verdict is owed.
      //
      // Deliberately AFTER the cursor has settled, and deliberately not awaited
      // by the poll's own error path: a sweep failure is not a poll failure.
      void sweepNow().catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("ADR-0045 tail sweep failed", err);
      });
      // ADR-0073 R.6 / R.7 — the conversation's continuity pass, at the tail
      // of the open chat's own poll (the background sweep leaves this group
      // to this view). Silent; a no-op unless a reset was accepted.
      receive.continuityPass(props.entry.groupIdB64);
      return result.outcome;
    }
  }

  /**
   * The automatic receive loop — without it nothing arrived until the user
   * pressed Refresh: messages, membership commits, ADR-0045 proposals and
   * beacons, and the 1x1 call signaling (an incoming call rang only when
   * somebody clicked).
   *
   * It long-polls this group's journal (`LONG_POLL_WAIT_SECS`) through the same
   * `pollOnce` as the Refresh button, silently. Overlap with a user-initiated
   * refresh is already safe: the fetch is outside the per-group MLS lock, the
   * apply and the cursor move are inside it, and a batch whose cursor moved
   * while it was on the network is dropped (`cursor-moved`).
   *
   * Lifetime: started once the mount sequence (log hydrate -> outbox sweep ->
   * first refresh) has finished, so the first long poll starts from the cursor
   * that sequence left behind; aborted on unmount; ended for good when this
   * device's membership ends.
   *
   * Paused while the tab is hidden AND a push wake is known to reach it —
   * push then covers the backgrounded tab, as on the native clients, and the
   * ADR-0045 beacon is foreground-gated for the same reason. A tab push cannot
   * reach (notifications denied, no VAPID key in the build, no Push API) never
   * parks: the in-app ring is the only path an incoming call has there, and
   * this loop is what fetches the offer for THIS chat — the background receive
   * deliberately skips a group whose view is open. The rule and the observed
   * defect are in `chat/receive-activity.ts`.
   *
   * A call in progress or ringing overrides the pause in any case: 1x1 call
   * signaling rides this journal, and a live SFU call's mid-call rekey
   * (ADR-0029) is driven by the epoch advances this poll applies.
   */
  const receiveLoopAbort = new AbortController();
  const receiveLoopResume = createWakeLatch();
  const pushWakeReach = createPushWakeReach();
  const callInProgress = (): boolean =>
    callHost.callLaneGroupIdB64() === props.entry.groupIdB64;
  /**
   * Whether a call is HOLDING the microphone and camera, so the composer's
   * recorder must stand aside (`docs/ux/media-messages.md`).
   *
   * Deliberately not `callInProgress` itself, which answers a different
   * question and has a second reader above: a ringing offer keeps the receive
   * loop awake precisely BECAUSE the call signaling rides this journal, while
   * the same offer holds no microphone. The rule that tells the two apart is
   * `crates/lakko-ui-rules/src/capture.rs`, reached through
   * `calls/capture-rule.ts`; both facts are the client's existing reads.
   *
   * It is also not scoped to THIS chat, where `callInProgress` is: since
   * `docs/ux/calls.md` section 4.5 a live call collapses into a bar and
   * survives every navigation, so a call placed from another conversation is
   * holding this device's one microphone while the person stands here.
   */
  const callOwnsCapture = (): boolean => callOwnsCaptureNow(uiRules(), callHost);
  function receiveLoopPaused(): boolean {
    return mayParkReceive({
      documentHidden: globalThis.document?.visibilityState === "hidden",
      callInProgress: callInProgress(),
      pushWake: pushWakeReach.read(),
    });
  }
  function startReceiveLoop(): void {
    void runLongPollLoop(receiveLoopAbort.signal, {
      shouldContinue: () => !membershipEnded(),
      isPaused: receiveLoopPaused,
      waitUntilResumed: (signal) => receiveLoopResume.wait(signal),
      tick: async (signal) => {
        const outcome = await pollOnce(LONG_POLL_WAIT_SECS, signal, true);
        // A held persist leaves the cursor where it was, so the next long poll
        // would hand back the same rows at once. Give storage a moment rather
        // than re-applying in a tight loop.
        return outcome === "persist-held" ? NETWORK_BACKOFF_MS : 0;
      },
      classifyFailure: (err) => (isDsNetworkFailure(err) ? "network" : "other"),
      sleep: sleepCancellable,
      now: () => Date.now(),
      onFailure: (err, backoffMs) => {
        // eslint-disable-next-line no-console
        console.warn(`chat receive loop poll failed; retrying in ${backoffMs} ms`, err);
      },
    });
  }
  const onReceiveLoopVisibility = (): void => receiveLoopResume.wake();
  globalThis.document?.addEventListener("visibilitychange", onReceiveLoopVisibility);
  // A call starting in a hidden tab must un-pause the loop at once.
  createEffect(
    on(callInProgress, (inCall) => {
      if (inCall) receiveLoopResume.wake();
    }),
  );
  onCleanup(() => {
    globalThis.document?.removeEventListener(
      "visibilitychange",
      onReceiveLoopVisibility,
    );
    receiveLoopAbort.abort();
  });

  // No leave guard: a 1x1 call and a group call both belong to the call host
  // and continue on whatever screen the user moves to, and the host prompts on
  // a reload or tab close.

  async function onSend(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const text = input();
    const attached = pendingAttachments();
    // 1.19c2: attachment-only messages are valid (empty text +
    // ≥1 attachment); pre-1.19 text-only messages still valid
    // (≥1 char text + zero attachments). The only invalid
    // input is the all-empty composer.
    if (text.length === 0 && attached.length === 0) return;
    setError(null);

    // ADR-0024 — sending a message is a typing-stop transition;
    // emit one `typing:false` (best-effort) so peers clear our
    // indicator immediately rather than waiting for the ~6 s expiry.
    sendTypingStop();

    // ADR-0052 — the reply this message is sent as, if the strip is armed.
    // Taken here so the optimistic bubble draws the quote at once and the
    // strip clears with the send.
    const reply = takeArmedReply();
    // Optimistic outbound bubble.
    const tempId = nextTempId++;
    setRows([
      ...rows(),
      {
        kind: "outgoing",
        tempId,
        plaintext: text,
        attachments: attached,
        ack: "pending",
        // Stamped at SEND time, from the roster we are a member of
        // right now — the same "resolve once, carry the key" shape
        // the receive path uses for everyone else.
        authorKey: ownSignatureKeyB64(),
        ...(reply === undefined ? {} : { reply }),
      },
    ]);
    setInput("");
    setPendingAttachments([]);
    await publishOutgoing(tempId, text, attached, reply);
  }

  /**
   * Publish one outgoing row's envelope and settle its ack.
   *
   * Shared by the first attempt and by Retry, so the two cannot drift on
   * what a send is. A failure leaves the bubble in the timeline reading
   * "Not sent - Retry" (`docs/ux/media-messages.md` section 3), which is
   * the whole point: it used to DELETE the row, and since `setInput("")`
   * had already run and nothing put the words back, a message typed on a
   * bad connection simply vanished. The attachments were handed back to
   * the composer; the text was not handed back anywhere.
   */
  async function publishOutgoing(
    tempId: number,
    text: string,
    attached: AttachmentRefBytes[],
    /** ADR-0052 — the reply reference the row carries, re-sent verbatim on
     *  Retry. */
    reply: ReplyRef | undefined,
    /** When a held message was first held, so the give-up counts from then. */
    heldAtMs: number = Date.now(),
    /** ADR-0052 — the forward attribution of a held forward into this
     *  conversation; `undefined` for everything typed here. */
    forward: ForwardRef | undefined = undefined,
  ): Promise<void> {
    setRows([...withSendAck(rows(), tempId, "pending")]);
    // ADR-0073 R.2 / R.7 case 1 — a one-to-one: the peer's identity is
    // checked when the last check is old (an accepted reset re-pins and the
    // route below then holds), and a message is NEVER sent into a session
    // only removed devices can read. It is held, in the ordinary sending
    // state, until the conversation's successor exists on this device.
    if (!isGroupChat()) {
      await Promise.race([
        checkPeerIdentity(false),
        new Promise<void>((resolve) => setTimeout(resolve, SEND_IDENTITY_CHECK_WAIT_MS)),
      ]);
      const route = sendRoute({ kind: "chat", unrepairableKeyB64: unrepairableKeyNow() });
      const verdict = heldVerdict({ route, heldAtMs, nowMs: Date.now() });
      if (verdict === "give-up") {
        setRows([...withSendAck(rows(), tempId, "failed")]);
        setError(() => sendFailureNotice(null));
        return;
      }
      if (verdict === "wait") {
        holdOutgoing(
          tempId,
          { text, attached, reply, replyGroupIdB64: props.entry.groupIdB64, forward },
          heldAtMs,
        );
        return;
      }
    }
    try {
      // 1.19c2: compose the JSON envelope from (text, refs).
      // Empty refs ⇒ pure-text envelope; non-empty ⇒ multi-
      // attachment envelope via the wasm EnvelopeBuilder. A reply is the
      // same envelope plus the reference (ADR-0052).
      const envelopeBytes =
        forward !== undefined
          ? await composeForwardEnvelope(text, attached, forward)
          : reply === undefined
            ? await composeEnvelope(text, attached)
            : await composeReplyEnvelope(text, attached, reply);
      const result = await chat.sendMessage(
        props.requireIdentity(),
        props.entry.group,
        envelopeBytes,
      );
      // Flip ack → sent + record DS id.
      setRows(
        rows().map((r) =>
          r.kind === "outgoing" && r.tempId === tempId
            ? {
                ...r,
                ack: "sent",
                messageId: result.messageId,
                createdAt: result.createdAt,
              }
            : r,
        ),
      );
    } catch (err) {
      // The publish itself refused the superseded session (a reset accepted
      // between the route above and the send): held, exactly as if the
      // route had said so.
      if (err instanceof HeldForSuccessor) {
        holdOutgoing(
          tempId,
          { text, attached, reply, replyGroupIdB64: props.entry.groupIdB64, forward },
          heldAtMs,
        );
        return;
      }
      setRows([...withSendAck(rows(), tempId, "failed")]);
      // ADR-0053 contract point 5 — a proof refused because this device's
      // clock is wrong says so; anything else keeps the ordinary copy.
      setError(
        () =>
          dsClockSkewNotice(err) ??
          sendFailureNotice(membershipRefusalOf(err, membershipRetryMaySucceed)),
      );
    }
  }

  // ----- ADR-0023 edit / delete / reaction send handlers -------
  //
  // The sender never receives its own application message back
  // (the echo is filtered in ChatClient), so each handler BOTH
  // publishes the op AND applies it locally. The op carries a DS
  // `messageId` target; the local apply uses the same overlay
  // map the receive loop folds into. Failures roll the local
  // apply back (or never apply, for delete/react where we apply
  // only after the publish resolves).

  /** Save an inline edit of our own message. Author-gated by the
   *  UI affordance (offered on own messages only) + by the
   *  receive-side gate every peer runs. */
  async function onSaveEdit(messageId: number): Promise<void> {
    const newText = editText();
    // An empty edit is a no-op rather than a silent delete — the
    // user must use Delete explicitly.
    if (newText.length === 0) {
      setEditingId(null);
      return;
    }
    setEditingId(null);
    setError(null);
    try {
      // ADR-0073 R.7 case 1 — in a superseded one-to-one an op is applied
      // here and never sent: its target exists only in the old session.
      if (opRouteNow() === "send") {
        const envelopeBytes = await composeEditEnvelope(messageId, newText);
        await chat.sendMessage(props.requireIdentity(), props.entry.group, envelopeBytes);
      }
      // Apply locally (no echo). Author == us, so the gate the
      // receive loop runs would accept it; we mirror that here.
      setMsgOps(applyOp(msgOps(), { op: "edit", target: messageId, text: newText }));
    } catch (err) {
      setError(errorText(err, "chat.error.edit"));
    }
  }

  /** ONE delete of ONE own message: publish the delete op, tombstone it
   *  locally, drop its pin records. The single delete and the bulk one both
   *  run this (section 8). A throw means this message was not deleted. */
  async function deleteOne(messageId: number): Promise<void> {
    if (opRouteNow() === "send") {
      const envelopeBytes = await composeDeleteEnvelope(messageId);
      await chat.sendMessage(props.requireIdentity(), props.entry.group, envelopeBytes);
    }
    setMsgOps(applyOp(msgOps(), { op: "delete", target: messageId }));
    // Section 4.1 — a deleted message leaves the bar and its pin records
    // go with it.
    void pins.messageDeleted(props.entry.groupIdB64, messageId);
  }

  /** Delete our own message — publish a delete op + tombstone it
   *  locally. */
  async function onDeleteMessage(messageId: number): Promise<void> {
    setError(null);
    try {
      await deleteOne(messageId);
    } catch (err) {
      setError(errorText(err, "chat.error.delete_message"));
    }
  }

  /**
   * Section 8 "Delete" — the bar's Delete. Confirms ("Delete N messages?"),
   * then one delete per message, oldest first, each applied locally as the
   * single delete does. On a failure it stops, keeps the unsent messages
   * selected and says how far it got; full success leaves the mode.
   */
  async function deleteSelection(): Promise<void> {
    if (!deleteState().enabled) return;
    const ids = oldestFirst(selection());
    if (!(await requestConfirm(bulkDeleteConfirmation(ids.length)))) return;
    setError(null);
    const done: number[] = [];
    for (const id of ids) {
      try {
        await deleteOne(id);
      } catch {
        setSelection(withoutDone(selection(), done));
        const deleted = done.length;
        setError(() => progressLabel("deleted", deleted, ids.length));
        return;
      }
      done.push(id);
    }
    leaveSelectionMode();
  }

  /** React to any message with `emoji` — publish a reaction op +
   *  toggle it locally under our own leaf. Re-reacting with the
   *  same emoji toggles it off (the op + the local apply are both
   *  toggles, so they stay in sync). */
  async function onReact(messageId: number, emoji: string): Promise<void> {
    // The action menu closes itself after an action; only the full picker,
    // which the view owns, has to be dismissed from here.
    setFullReactPickerId(null);
    setError(null);
    // ADR-0047 §3 — a reaction is exactly one emoji grapheme. The wire accepts
    // any string up to 64 bytes; this is the check that keeps the six-emoji
    // bar and the picker one model.
    if (!isSingleEmojiGrapheme(emoji)) {
      setError(() => t("chat.reaction.single_emoji"));
      return;
    }
    try {
      if (opRouteNow() === "send") {
        const envelopeBytes = await composeReactionEnvelope(messageId, emoji);
        await chat.sendMessage(props.requireIdentity(), props.entry.group, envelopeBytes);
      }
      // Our own identity, not our own leaf: the chip's "mine" test
      // and the peers' fold must agree on one value that survives a
      // renumber.
      const reactor = ownSignatureKeyB64() ?? OWN_REACTION_SENTINEL;
      setMsgOps(
        applyOp(msgOps(), {
          op: "reaction",
          target: messageId,
          emoji,
          reactor,
        }),
      );
    } catch (err) {
      setError(errorText(err, "chat.error.react"));
    }
  }

  // ----- ADR-0024 typing + read-receipt send path ----------------
  //
  // Typing + read receipts ride the same application send path as
  // chat / edit / etc. (`chat.sendMessage`), so the DS echo is
  // dropped via the per-group echo registry exactly like the
  // ADR-0023 ops. They are best-effort fire-and-forget — a
  // transient failure must never wedge the composer or surface a
  // user-facing error (the
  // signals are non-essential status). They render no optimistic
  // bubble.

  /** Publish an already-composed ephemeral envelope (typing /
   *  read-receipt) without an optimistic bubble. Swallows failures
   *  (these signals are non-essential; the next one supersedes a
   *  dropped one). */
  async function publishEphemeral(envelopeBytes: Uint8Array): Promise<void> {
    // ADR-0073 R.7 case 1 — a typing signal or a read receipt is not sent
    // into a superseded one-to-one either (the publish would refuse it).
    if (opRouteNow() !== "send") return;
    try {
      await chat.sendMessage(
        props.requireIdentity(),
        props.entry.group,
        envelopeBytes,
      );
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("publishEphemeral failed", err);
    }
  }

  // Outbound typing — throttled to ≤ 1 `true` / TYPING_SEND_THROTTLE_MS
  // while the user is actively editing. `typingActiveSent` tracks
  // whether we've already emitted a `true` since the last stop, so
  // the stop transition fires exactly one `false`.
  let lastSentTypingTrueAtMs: number | null = null;
  let typingActiveSent = false;
  createEffect(() => {
    const text = input();
    const isEditing = text.length > 0;
    if (!isEditing) return; // an empty composer is handled by the stop path
    if (shouldSendTyping(isEditing, lastSentTypingTrueAtMs, Date.now())) {
      lastSentTypingTrueAtMs = Date.now();
      typingActiveSent = true;
      void (async (): Promise<void> => {
        void publishEphemeral(await composeTypingEnvelope(true));
      })();
    }
  });

  /** Emit a single `typing:false` on the stop transition (blur OR
   *  on sending a message). No-op if we never sent a `true` since
   *  the last stop — ADR-0024 says do NOT spam `false`. */
  function sendTypingStop(): void {
    if (!typingActiveSent) return;
    typingActiveSent = false;
    lastSentTypingTrueAtMs = null;
    void (async (): Promise<void> => {
      void publishEphemeral(await composeTypingEnvelope(false));
    })();
  }

  // Outbound read receipt — when the chat view is open (this route
  // is mounted) and the highest received messageId advances past our
  // own last-published watermark, broadcast it (throttled by the
  // debounce + the monotone `shouldSendReadReceipt` guard). Mirrors
  // the existing read-cursor effect's shape but rides the chat group
  // (a peer-visible receipt) rather than the DeviceGroup.
  let lastPublishedReadId: number | null = null;
  let readReceiptTimer: ReturnType<typeof setTimeout> | null = null;
  const READ_RECEIPT_DEBOUNCE_MS = 1200;
  createEffect(() => {
    const list = rows();
    if (list.length === 0) return;
    let highest: number | null = null;
    for (const r of list) {
      const id = rowMessageId(r);
      if (id !== undefined && (highest === null || id > highest)) highest = id;
    }
    const target = shouldSendReadReceipt(highest, lastPublishedReadId);
    if (target === null) return;
    if (readReceiptTimer !== null) clearTimeout(readReceiptTimer);
    readReceiptTimer = setTimeout(() => {
      readReceiptTimer = null;
      lastPublishedReadId = target;
      void (async (): Promise<void> => {
        void publishEphemeral(await composeReadReceiptEnvelope(target));
      })();
    }, READ_RECEIPT_DEBOUNCE_MS);
  });
  onCleanup(() => {
    if (readReceiptTimer !== null) {
      clearTimeout(readReceiptTimer);
      readReceiptTimer = null;
    }
  });

  // ADR-0024 — typing auto-expire sweep. An entry whose last fresh
  // `true` aged out is dropped from the indicator even with no
  // further traffic. Cheap (one pass over a tiny map); the interval
  // is cleared on unmount.
  const typingSweep = setInterval(() => {
    const now = Date.now();
    setCurrentTime(now);
    // ADR-0045 — the same tick matures a `pending` request into `waiting` with
    // no traffic at all, which matters precisely in the case the notice exists
    // for: a group where nobody is doing anything and nobody has swept.
    setPendingClock(now);
    const cur = typingMap();
    const pruned = pruneTyping(cur, now);
    if (pruned !== cur) setTypingMap(pruned);
  }, 2_000);
  onCleanup(() => clearInterval(typingSweep));

  // ADR-0045 S4b — the beacon emitter.
  //
  // Driven by its own timer rather than by the poll, because the poll's pace is
  // set by traffic (a long poll returns as soon as a row arrives, and pauses
  // in a hidden tab) while a beacon's whole meaning is a CADENCE: "still
  // here". The tick is cheap and
  // deliberately much shorter than the beacon interval — `shouldEmitBeacon` is
  // the throttle, so the tick only has to be fine-grained enough not to add
  // meaningful latency to the first beacon after the tab becomes visible.
  //
  // It lives on this component, so it exists exactly while this group's chat
  // is open — which is exactly when this device can sweep it.
  const beaconTick = setInterval(() => {
    void emitBeaconIfDue();
  }, BEACON_TICK_MS);
  onCleanup(() => clearInterval(beaconTick));

  /**
   * ADR-0045 UX §6.2.8 — the ONE-SHOT half of the teardown.
   *
   * Three of §6.2.8's five disciplines are standing GATES with nothing to
   * dispose — the poll (`refresh`), the shrink rotation
   * (`membershipRotationDecision`'s `selfEvicted`) and the exporter-backed
   * call buttons — and each consults `membershipEnded()` where it runs. These
   * two have an effect to fire, and firing an effect more than once is what
   * the latch prevents: this runs inside a `createEffect`, which re-runs on
   * every roster change for reasons that have nothing to do with the
   * eviction.
   *
   * `claimTeardownSteps` is what decides; this only performs. It CLAIMS what
   * it hands out, synchronously, rather than marking each step done when it
   * finishes — because the push retire is async, and marking it after the
   * await leaves a window in which this effect re-runs, reads the step as
   * still owed, and fires a second concurrent unsubscribe. A step that FAILS
   * is released so the next re-run retries it.
   */
  const teardownDone = new Set<EndedTeardownStep>();
  createEffect(() => {
    const owed = claimTeardownSteps(membership(), teardownDone);
    if (owed.length === 0) return;
    for (const step of owed) {
      if (step === "stop-beacon-ticker") {
        // Belt and braces beside the gate inside `emitBeaconIfDue`: the gate
        // is what makes a surviving timer harmless, this is what stops the
        // wasted wake. Local and infallible, so it never needs releasing.
        clearInterval(beaconTick);
        continue;
      }
      // `retire-push-subscription` — the half ADR-0043 recorded as unclosable
      // ("nobody else can delete it and no client has 'you were removed'
      // handling"). This IS that handling. `retireOldPushGid` already does
      // exactly the right three things — derive the gid, unsubscribe this
      // device, drop the reverse-index row — bounded and never throwing.
      //
      // The honest bound: it needs this device to be online and running, so it
      // COMPLEMENTS the group's own rotation and replaces nothing. A phone
      // that is never opened again keeps its row, exactly as before.
      const s = auth.state();
      if (s.kind !== "session") {
        // Nothing to unsubscribe on behalf of. Release the claim so a later
        // re-run, once a session exists, picks it up.
        releaseTeardownStep(teardownDone, step);
        continue;
      }
      const accountId = s.account.accountId;
      const groupIdB64 = base64Encode(props.entry.group.groupId);
      void (async () => {
        try {
          const secret = await getGroupRoutingSecretForAccount(
            accountId,
            groupIdB64,
          );
          // No secret means nothing was ever subscribed under a `push_gid` for
          // this group, so there is nothing to retire — done, not failed, so
          // the claim stands.
          if (secret !== null) await retireOldPushGid(accountId, secret);
        } catch (err) {
          // Released so the next re-run retries it.
          releaseTeardownStep(teardownDone, "retire-push-subscription");
          // eslint-disable-next-line no-console
          console.warn("ADR-0045 push subscription teardown failed", err);
        }
      })();
    }
  });

  /**
   * Encrypt + upload an iterable of files sequentially (so a
   * slow user-stream doesn't fan out N parallel storage PUTs),
   * appending the resulting refs to the pending list. Failed
   * uploads surface a per-file error and stop the loop.
   *
   * Shared between the `<input type="file">` Change handler and
   * the drag-drop handler — both surface a `FileList`-ish
   * iterable; this helper is the one place we own the upload
   * loop.
   */
  async function processFiles(files: ArrayLike<File>): Promise<void> {
    if (files.length === 0) return;
    setError(null);
    setUploading(true);
    const list = Array.from(files);
    try {
      for (let i = 0; i < list.length; i += 1) {
        const file = list[i];
        if (file === undefined) continue;
        try {
          // Polish 1.x: drive `uploadProgress` so the composer
          // tray can render "Uploading <file> (i/n)…" with a
          // stage indicator. The encrypt-vs-upload split lets
          // the user see the slow phase (network) separately
          // from the fast one (in-memory AEAD).
          // UX spec section 10 — a photo or video carries its pixel size
          // and a JPEG thumb so the receiver can lay the bubble out before
          // downloading. Produced before the upload so the composer shows
          // the thumb meanwhile; any failure sends the file without them.
          const inline = hub.inlineType(file.type.length > 0 ? file.type : undefined);
          let preview: VisualPreview | null = null;
          let previewKey: string | undefined;
          if (inline === "image" || inline === "video") {
            preview = inline === "image" ? await describeImage(file) : await describeVideo(file);
            if (preview.thumb !== null) {
              previewKey = `compose-preview:${nextTempId++}`;
              hub.fileLocal(previewKey, preview.thumb.blob, "image/jpeg");
            }
          }
          let ref: AttachmentRefBytes;
          try {
            ref = await encryptAndUploadFile(
              file,
              undefined,
              (stage) => {
                setUploadProgress({
                  fileIndex: i,
                  total: list.length,
                  fileName: file.name,
                  stage,
                  ...(previewKey === undefined ? {} : { previewKey }),
                });
              },
            );
            if (preview !== null) ref = await withVisualPreview(ref, file, preview);
          } finally {
            if (previewKey !== undefined) hub.revoke(previewKey);
          }
          setPendingAttachments([...pendingAttachments(), ref]);
        } catch (err) {
          if (err instanceof AttachmentSizeError) {
            const { got, max } = err;
            const name = file.name;
            setError(() =>
              t("chat.attachment.error.too_large_named", {
                name,
                size: formatAttachmentSize(got),
                limit: formatAttachmentSize(max),
              }),
            );
          } else {
            const name = file.name;
            console.warn("[chat] attachment upload failed", err);
            setError(() => t("chat.attachment.error.upload", { name }));
          }
          // Stop the loop on the first failure — a network
          // outage isn't going to be fixed by trying the next
          // file. The user picks again after surfacing.
          break;
        }
      }
    } finally {
      setUploading(false);
      setUploadProgress(null);
    }
  }

  /**
   * UX spec section 10 — attach the pixel size and upload the thumb of a
   * photo or video, then file the local bytes in the media hub so this
   * device's own bubble shows them without downloading its own upload. A
   * failed thumb upload sends the attachment without a thumb.
   */
  async function withVisualPreview(
    ref: AttachmentRefBytes,
    file: File,
    preview: VisualPreview,
  ): Promise<AttachmentRefBytes> {
    let thumb: ThumbRefBytes | undefined;
    if (preview.thumb !== null) {
      try {
        const bytes = new Uint8Array(await preview.thumb.blob.arrayBuffer());
        const t = await encryptAndUploadBytes(bytes, { mime: "image/jpeg", name: undefined });
        thumb = {
          blobId: t.blobId,
          key: t.key,
          nonce: t.nonce,
          mime: "image/jpeg",
          size: bytes.length,
          width: preview.thumb.width,
          height: preview.thumb.height,
        };
        hub.fileLocal(`thumb:${mediaKey(t.blobId)}`, preview.thumb.blob, "image/jpeg");
      } catch (err) {
        console.warn("attachment thumb upload failed; sending without a thumb", err);
      }
    }
    hub.fileLocal(mediaKey(ref.blobId), file, ref.mime);
    return {
      ...ref,
      ...(preview.size === null ? {} : { width: preview.size.width, height: preview.size.height }),
      ...(thumb === undefined ? {} : { thumb }),
    };
  }

  /**
   * Handle the composer's `<input type="file">` change event.
   * Delegates to [`processFiles`] then resets the input so the
   * same file can be re-picked (browsers suppress the change
   * event when the value is identical).
   */
  async function onFilesPicked(e: Event): Promise<void> {
    const target = e.currentTarget as HTMLInputElement;
    const files = target.files;
    if (files === null) return;
    try {
      await processFiles(files);
    } finally {
      target.value = "";
    }
  }

  // Drag-drop polish (1.x): a drop anywhere on the chat surface
  // queues the dropped files into the pending-attachments list
  // — same flow as clicking the 📎 button. Tracks `isDragOver`
  // so the surface can render a "drop here" hint while a drag
  // hovers; `dragCounter` handles the dragenter/dragleave race
  // (children fire leave events when the drag crosses them).
  const [isDragOver, setIsDragOver] = createSignal(false);
  // eslint-disable-next-line solid/reactivity
  let dragCounter = 0;

  function onDragEnter(e: DragEvent): void {
    if (!hasFilesType(e.dataTransfer)) return;
    e.preventDefault();
    dragCounter += 1;
    setIsDragOver(true);
  }

  function onDragOver(e: DragEvent): void {
    if (!hasFilesType(e.dataTransfer)) return;
    e.preventDefault();
    // `move` shows the cursor that matches "this drop will
    // produce something" instead of the default "no" cursor on
    // top of a non-droppable surface.
    if (e.dataTransfer !== null) {
      e.dataTransfer.dropEffect = "copy";
    }
  }

  function onDragLeave(e: DragEvent): void {
    if (!hasFilesType(e.dataTransfer)) return;
    e.preventDefault();
    dragCounter = Math.max(0, dragCounter - 1);
    if (dragCounter === 0) {
      setIsDragOver(false);
    }
  }

  async function onDrop(e: DragEvent): Promise<void> {
    if (!hasFilesType(e.dataTransfer)) return;
    e.preventDefault();
    dragCounter = 0;
    setIsDragOver(false);
    const files = e.dataTransfer?.files;
    if (files === undefined || files.length === 0) return;
    await processFiles(files);
  }

  function removePendingAttachment(index: number): void {
    setPendingAttachments(
      pendingAttachments().filter((_, i) => i !== index),
    );
  }

  // ----- ADR-0047 media messages ----------------------------------------
  //
  // A voice message, video note or GIF is sent as its OWN message the moment
  // it is finished (recorded and released / sent from preview / tapped in the
  // GIF grid): the bubble enters the timeline at once with a determinate
  // upload ring, and a failure leaves it there as `Not sent · Retry` rather
  // than rolling it back. The job keeps whatever it already achieved (the
  // bytes, the uploaded thumb, the uploaded ref) so a retry resumes rather
  // than re-uploading.

  interface MediaSendJob {
    readonly tempId: number;
    readonly localKey: string;
    name: string;
    readonly meta: AttachmentMediaMeta;
    /** Placeholder mime shown until the bytes are known (a GIF's type comes
     *  from the proxy response). */
    mime: string;
    loadBytes: () => Promise<{ bytes: Uint8Array; mime: string }>;
    bytes?: Uint8Array;
    poster?: { blob: Blob; mime: string; width: number; height: number };
    thumb?: ThumbRefBytes;
    posterTried?: boolean;
    uploaded?: AttachmentRefBytes;
    /** ADR-0052 — the take was sent as a reply: the reference the row and
     *  the envelope carry. */
    reply?: ReplyRef;
  }
  const mediaJobs = new Map<number, MediaSendJob>();
  const [mediaUploads, setMediaUploads] = createSignal<ReadonlyMap<number, number | "failed">>(new Map());
  function setMediaUpload(tempId: number, value: number | "failed" | null): void {
    const next = new Map(mediaUploads());
    if (value === null) next.delete(tempId);
    else next.set(tempId, value);
    setMediaUploads(next);
  }
  function mediaUploadFor(row: TimelineRow): MediaUpload {
    if (row.kind !== "outgoing" || row.ack === "sent") return undefined;
    const u = mediaUploads().get(row.tempId);
    return u ?? (row.ack === "failed" ? "failed" : 0);
  }
  onCleanup(() => mediaJobs.clear());

  function placeholderRef(job: MediaSendJob): AttachmentRefBytes {
    return {
      blobId: new Uint8Array(0),
      key: new Uint8Array(0),
      nonce: new Uint8Array(0),
      mime: job.mime,
      size: job.bytes?.length ?? 0,
      name: job.name,
      ...job.meta,
    };
  }

  function startMediaSend(job: MediaSendJob): void {
    setError(null);
    mediaJobs.set(job.tempId, job);
    setRows([
      ...rows(),
      {
        kind: "outgoing",
        tempId: job.tempId,
        plaintext: "",
        attachments: [placeholderRef(job)],
        ack: "pending",
        mediaLocalKey: job.localKey,
        authorKey: ownSignatureKeyB64(),
        ...(job.reply === undefined ? {} : { reply: job.reply }),
      },
    ]);
    void runMediaJob(job);
  }

  function setMediaRowAck(tempId: number, ack: "pending" | "failed"): void {
    setRows(
      rows().map((r) =>
        r.kind === "outgoing" && r.tempId === tempId && r.ack !== ack ? { ...r, ack } : r,
      ),
    );
  }

  async function runMediaJob(job: MediaSendJob): Promise<void> {
    setMediaUpload(job.tempId, 0);
    setMediaRowAck(job.tempId, "pending");
    try {
      if (job.uploaded === undefined) {
        if (job.bytes === undefined) {
          const loaded = await job.loadBytes();
          job.bytes = loaded.bytes;
          job.mime = loaded.mime;
          // A GIF's container is only known once the proxy answered.
          if (job.meta.kind === "gif" && loaded.mime === "image/webp") {
            job.name = job.name.replace(/\.gif$/, ".webp");
          }
          hub.fileLocal(job.localKey, new Blob([loaded.bytes.slice()]), loaded.mime);
        }
        if (job.poster !== undefined && job.thumb === undefined && !job.posterTried) {
          job.posterTried = true;
          try {
            const posterBytes = new Uint8Array(await job.poster.blob.arrayBuffer());
            const t = await encryptAndUploadBytes(posterBytes, { mime: job.poster.mime, name: undefined });
            job.thumb = {
              blobId: t.blobId,
              key: t.key,
              nonce: t.nonce,
              mime: job.poster.mime,
              size: posterBytes.length,
              width: job.poster.width,
              height: job.poster.height,
            };
            // The poster is ours already: file it so the sent bubble shows it
            // without downloading its own upload.
            hub.fileLocal(`thumb:${mediaKey(t.blobId)}`, job.poster.blob, job.poster.mime);
          } catch (err) {
            // ADR-0047 open question, current answer: a failed poster upload
            // sends the note without one; the bubble shows a neutral circle.
            console.warn("video note poster upload failed; sending without a poster", err);
          }
        }
        job.uploaded = await encryptAndUploadBytes(
          job.bytes,
          { mime: job.mime, name: job.name },
          { ...job.meta, ...(job.thumb === undefined ? {} : { thumb: job.thumb }) },
          undefined,
          (fraction) => setMediaUpload(job.tempId, fraction),
        );
      }
      const uploaded = job.uploaded;
      const envelopeBytes =
        job.reply === undefined
          ? await composeEnvelope("", [uploaded])
          : await composeReplyEnvelope("", [uploaded], job.reply);
      const result = await chat.sendMessage(props.requireIdentity(), props.entry.group, envelopeBytes);
      // The take's own bytes are already in the hub under the local key; MOVE
      // them to the real blob id before the row flips, so the bubble finds them
      // there without downloading its own upload. A move rather than a re-file
      // plus a revoke: the bubble may be PLAYING these bytes right now (a take
      // that failed to send can be played and then retried), and re-filing them
      // made a second copy under a second URL while revoking the first from
      // under the element that was reading it.
      const sentKey = mediaKey(uploaded.blobId);
      const bytes = job.bytes;
      if (!hub.adopt(job.localKey, sentKey) && bytes !== undefined) {
        hub.fileLocal(sentKey, new Blob([bytes.slice()]), uploaded.mime);
      }
      setRows(
        rows().map((r) =>
          r.kind === "outgoing" && r.tempId === job.tempId
            ? {
                kind: "outgoing" as const,
                tempId: r.tempId,
                plaintext: "",
                attachments: [uploaded],
                ack: "sent" as const,
                messageId: result.messageId,
                createdAt: result.createdAt,
                authorKey: r.authorKey,
                ...(job.reply === undefined ? {} : { reply: job.reply }),
              }
            : r,
        ),
      );
      mediaJobs.delete(job.tempId);
      setMediaUpload(job.tempId, null);
    } catch (err) {
      // ADR-0073 R.7 case 1 — the bytes are uploaded and the session is
      // superseded: the message is held for the successor like a typed one,
      // as its uploaded reference (the row stays in the sending state).
      if (err instanceof HeldForSuccessor && job.uploaded !== undefined) {
        const uploaded = job.uploaded;
        mediaJobs.delete(job.tempId);
        setMediaUpload(job.tempId, null);
        setRows(
          rows().map((r) =>
            r.kind === "outgoing" && r.tempId === job.tempId
              ? {
                  kind: "outgoing" as const,
                  tempId: r.tempId,
                  plaintext: "",
                  attachments: [uploaded],
                  ack: "pending" as const,
                  authorKey: r.authorKey,
                  ...(job.reply === undefined ? {} : { reply: job.reply }),
                }
              : r,
          ),
        );
        holdOutgoing(job.tempId, { text: "", attached: [uploaded], reply: job.reply }, Date.now());
        return;
      }
      setMediaUpload(job.tempId, "failed");
      setMediaRowAck(job.tempId, "failed");
      if (err instanceof AttachmentSizeError) {
        const { got, max } = err;
        setError(() =>
          t("chat.attachment.error.too_large", {
            size: formatAttachmentSize(got),
            limit: formatAttachmentSize(max),
          }),
        );
      }
      console.warn("media message not sent", err);
    }
  }

  /**
   * Send that row again.
   *
   * One entry point for both kinds of outgoing row, because from the bubble
   * they are the same gesture and the user cannot tell them apart. A media
   * take has an upload job to re-run; a row from the composer has only its
   * envelope, which is re-published from the row itself. Before this, the
   * "Not sent - Retry" the bubble already drew for a composer row with an
   * attachment called into the media path, found no job for that tempId and
   * returned silently - a button that did nothing, every time.
   *
   * **It can deliver the message twice.** A publish that reached the server
   * and lost its answer is indistinguishable here from one that never
   * arrived: the send is not idempotent, there is no key to de-duplicate on,
   * and our own message is one the MLS ratchet will not decrypt a second
   * time, so the journal cannot be consulted either. The same is true of a
   * person re-typing a message by hand, which is what they did before this
   * existed; a button makes it cheaper to reach, not newly possible. Losing
   * the message is the worse failure.
   */
  function retrySend(tempId: number): void {
    const job = mediaJobs.get(tempId);
    if (job !== undefined) {
      void runMediaJob(job);
      return;
    }
    const row = rows().find((r) => r.kind === "outgoing" && r.tempId === tempId);
    if (row === undefined || row.kind !== "outgoing") return;
    setError(null);
    void publishOutgoing(tempId, row.plaintext, row.attachments, row.reply);
  }

  /** A finished recording the user chose to send. */
  function sendCapturedTake(take: CaptureResult): void {
    const tempId = nextTempId++;
    const stamp = new Date().toISOString().replace(/[:.]/g, "-");
    const isVoice = take.kind === "voice";
    // ADR-0052 — a take recorded with a reply armed is sent as the reply, and
    // the strip clears the same way a text send clears it.
    const reply = takeArmedReply();
    startMediaSend({
      tempId,
      localKey: `local:${tempId}`,
      ...(reply === undefined ? {} : { reply }),
      name: isVoice ? `voice-${stamp}.m4a` : `video-${stamp}.mp4`,
      mime: take.mime,
      meta: {
        kind: take.kind,
        durationMs: take.durationMs,
        ...(take.waveform === undefined ? {} : { waveform: take.waveform }),
        ...(take.width === undefined ? {} : { width: take.width }),
        ...(take.height === undefined ? {} : { height: take.height }),
      },
      ...(take.poster === undefined ? {} : { poster: take.poster }),
      loadBytes: async () => ({ bytes: new Uint8Array(await take.blob.arrayBuffer()), mime: take.mime }),
    });
  }

  /** UX spec §5 — a tapped GIF is sent immediately: its full rendition is
   *  fetched through the proxy, encrypted and uploaded like any file, so a
   *  recipient only ever fetches the encrypted blob from Lakko storage. */
  function sendGif(item: GifItem): void {
    const tempId = nextTempId++;
    // ADR-0052 — a GIF is a sent message like any other: it carries the armed
    // reply and clears the strip.
    const reply = takeArmedReply();
    startMediaSend({
      tempId,
      localKey: `local:${tempId}`,
      ...(reply === undefined ? {} : { reply }),
      name: `gif-${item.id}.gif`,
      mime: "image/gif",
      meta: { kind: "gif", width: item.width, height: item.height },
      loadBytes: async () => {
        const token = auth.identitySecrets()?.sessionToken ?? null;
        if (token === null) throw new Error("no session for the GIF proxy");
        const r = await fetchGifMedia(item.fullUrl, token, MAX_GIF_FULL_BYTES);
        if (r.kind !== "ok") throw new Error(`GIF download failed: ${r.kind}`);
        return { bytes: r.value.bytes, mime: r.value.mime };
      },
    });
  }

  const recorder = createRecorder({
    onSend: sendCapturedTake,
    inCall: () => callOwnsCapture(),
    createUrl: (blob) => hub.registry.set(`take:${Date.now()}:${Math.random()}`, blob),
    revokeUrl: (url) => {
      for (const [k, u] of hub.registry.entries()) if (u === url) hub.registry.revoke(k);
    },
  });
  onCleanup(() => recorder.dispose());
  // UX spec §6 — the call owns the microphone and camera; a take in progress
  // when a call starts is dropped, as is one when the membership ends. Read
  // through the same `callOwnsCapture` the recorder is gated on, so a take is
  // dropped exactly when recording stops being allowed: an offer merely being
  // rung takes nothing, and the take is dropped the moment it is answered.
  createEffect(
    on(
      () => callOwnsCapture() || membershipEnded(),
      (blocked) => {
        if (blocked && recorder.state().phase !== "idle") recorder.cancel();
      },
    ),
  );

  // ADR-0047 — the composer's emoji / GIF picker, and the reaction picker.
  const [composerPickerOpen, setComposerPickerOpen] = createSignal(false);
  const [fullReactPickerId, setFullReactPickerId] = createSignal<number | null>(null);
  let composerInput: HTMLTextAreaElement | undefined;
  let composerCaret: { start: number; end: number } | null = null;
  /** docs/ux section 5.2 — the field grows with the draft up to five lines
   *  (the CSS `max-height` is the cap; past it the field scrolls). Reset to
   *  one line when the draft is cleared. */
  function growComposer(): void {
    const el = composerInput;
    if (el === undefined) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }
  createEffect(on(input, () => growComposer()));
  function rememberCaret(): void {
    if (composerInput === undefined) return;
    composerCaret = {
      start: composerInput.selectionStart ?? composerInput.value.length,
      end: composerInput.selectionEnd ?? composerInput.value.length,
    };
  }
  /** UX spec §4 — insert at the caret, keep the picker open, keep focus. */
  function insertEmoji(emoji: string): void {
    const current = input();
    const fromInput =
      composerInput !== undefined && globalThis.document?.activeElement === composerInput
        ? { start: composerInput.selectionStart ?? current.length, end: composerInput.selectionEnd ?? current.length }
        : null;
    const caret = fromInput ?? composerCaret ?? { start: current.length, end: current.length };
    const start = Math.min(caret.start, current.length);
    const end = Math.min(Math.max(caret.end, start), current.length);
    const next = current.slice(0, start) + emoji + current.slice(end);
    setInput(next);
    const pos = start + emoji.length;
    composerCaret = { start: pos, end: pos };
    // The picker's search box is an `<input>`; the composer is a textarea, so
    // a focused input is never the composer.
    const searchFocused = globalThis.document?.activeElement instanceof HTMLInputElement;
    if (composerInput !== undefined && !searchFocused) {
      composerInput.value = next;
      composerInput.focus();
      composerInput.setSelectionRange(pos, pos);
    }
  }

  /**
   * Download + decrypt a received attachment + offer a Save
   * dialog. Used by the attachment-chip Save button on incoming
   * bubbles.
   */
  async function onDownloadAttachment(
    ref: AttachmentRefBytes,
  ): Promise<void> {
    setError(null);
    try {
      const bytes = await downloadAndDecryptRef(ref);
      saveDecryptedBytes(
        bytes,
        ref.name ?? `attachment-${blobIdHex(ref.blobId).slice(0, 8)}`,
        ref.mime,
      );
    } catch (err) {
      if (err instanceof AttachmentMissingError) {
        setError(() => t("chat.attachment.error.gone"));
      } else {
        console.warn("[chat] attachment download failed", err);
        setError(() => t("chat.attachment.error.download"));
      }
    }
  }

  /**
   * Inline-preview an image attachment. Fetches once + caches
   * the resulting object URL in `decryptedCache`; the
   * [`AttachmentChips`] component reads the cache reactively and
   * renders the `<img>` inside the bubble once the URL is
   * available. Re-click is a no-op (the cache already has the
   * entry); the user toggles the inline preview off via the
   * Hide button on the chip.
   *
   * Polish 1.x — inline image previews live in the bubble
   * itself rather than blowing open a new tab, matching iOS /
   * Android attachment UX and Signal / Telegram patterns. The
   * SVG exclusion in [`isInlinePreviewable`] keeps the trust
   * boundary tight: no `<img>` src from an unverified SVG, only
   * raster formats decoded by the browser's image pipeline.
   */
  async function onPreviewAttachment(
    ref: AttachmentRefBytes,
  ): Promise<void> {
    // ADR-0047 — inline only for a type the core's allowlist accepts.
    if (hub.inlineType(ref.mime) !== "image") return;
    if (hub.urlFor(mediaKey(ref.blobId)) !== null) {
      // Already filed — the bubble already renders inline.
      return;
    }
    setError(null);
    try {
      // Filed in the hub's registry, so the URL is revoked on Hide or
      // when this view unmounts.
      await hub.load(ref);
    } catch (err) {
      if (err instanceof AttachmentMissingError) {
        setError(() => t("chat.attachment.error.gone"));
      } else {
        console.warn("[chat] attachment preview failed", err);
        setError(() => t("chat.attachment.error.preview"));
      }
    }
  }

  /**
   * Drop a cached inline preview without re-decrypting on the
   * next Open. Revokes the object URL so the browser releases
   * the underlying Blob; the `bytes` ref is kept in the cache
   * for re-download but the URL is cleared so the bubble
   * re-renders without the inline image.
   */
  function onHideAttachmentPreview(ref: AttachmentRefBytes): void {
    hub.revoke(mediaKey(ref.blobId));
  }

  /** Cache accessor passed to [`AttachmentChips`] — returns the
   *  cached object URL when the attachment has been previewed
   *  in this session, `null` otherwise. The chip reads this on
   *  every render so the inline image appears reactively once
   *  the cache lands. */
  function getCachedAttachmentUrl(ref: AttachmentRefBytes): string | null {
    return hub.urlFor(mediaKey(ref.blobId));
  }

  // 1.18g — "Add member" submission handler. Resolves the
  // typed @username via discovery, runs the full MLS add
  // (fetch KeyPackage → group.addMember → publish commit to DS
  // → ship Welcome to stranger inbox), then registers the
  // commit's DS id with `ChatClient.markOwnPublishedCommit` so
  // the very next pollMessages drops our own echo and doesn't
  // hit `processIncoming` on a commit we just applied locally.
  // Final step is a `refresh()` — pulls the (now-suppressed)
  // commit off the wire + re-renders the header (memberCount
  // signal updates from `group.memberCount` post-add).
  /** Everything an add owes once the artifact is on the wire, shared by the
   *  typed add and the New-group setup. */
  function afterMemberAdded(
    outcome: { kind: "committed"; commitMessageId: number } | { kind: "proposed"; proposalMessageId: number },
    recipientKeyB64: string,
  ): void {
    // Suppress the echo so the sender's next pollMessages
    // doesn't try to processIncoming on its own artifact at
    // an epoch we've already advanced past. ADR-0045: the artifact is a
    // PROPOSAL in a role-table group and a commit only in a legacy one.
    chat.markOwnPublishedCommit(
      props.entry.groupIdB64,
      outcome.kind === "committed" ? outcome.commitMessageId : outcome.proposalMessageId,
    );
    // ADR-0045 — record when the request landed, so the quiet window is
    // measured against the DS row rather than a local clock, and so this
    // device knows the request is its own (the UX §10 visibility exception).
    if (outcome.kind === "proposed") {
      noteOwnRequest(`add:${recipientKeyB64}`);
    }
    // ADR-0042 — re-broadcast the CURRENT push-routing secret so the
    // newly-added member learns it on their next poll (no-op if this
    // device holds no secret; idempotent for existing members). Runs
    // AFTER the add commit so the new member is already in the group.
    // Fire-and-forget; failures retry on the next add / launch sweep.
    {
      const s = auth.state();
      if (s.kind === "session") {
        void redistributeSecretToGroup(
          s.account.accountId,
          props.entry.group,
          broadcastToGroup,
        ).catch((err) => {
          // eslint-disable-next-line no-console
          console.warn("push-routing secret redistribute failed", err);
        });
      }
    }
    // Mirror the post-add member count into the reactive
    // signal so the header re-renders.
    setMemberCount(peopleCountOf(props.entry.group));
    // 1.18h — bump membersVersion so the members panel
    // (when open) re-reads the updated leaf set.
    setMembersVersion(membersVersion() + 1);
  }

  /** The New-group screen's remaining picks (`docs/ux/new-group.md` §2 step
   *  3): added one after another through the same path as a typed add, then
   *  swept once. A failed add is reported and skipped; the rest still land. */
  async function runPendingGroupSetup(setup: PendingGroupSetup): Promise<void> {
    setBusy(true);
    setError(null);
    const failed: string[] = [];
    for (const m of setup.members) {
      try {
        const outcome = await contacts.addMemberToGroup(
          props.requireIdentity(),
          props.entry.group,
          accountIdBytesFromHex(m.accountIdHex),
          base64Decode(m.signaturePubkeyB64),
        );
        afterMemberAdded(outcome, m.signaturePubkeyB64);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("new group: add failed", m.label, err);
        failed.push(m.label);
      }
    }
    try {
      await sweepNow();
    } finally {
      setBusy(false);
    }
    void refresh();
    if (failed.length > 0) {
      setError(() => t("chat.members.add.error.some_failed", { names: joinList(failed) }));
    }
  }
  onMount(() => {
    const setup = takePendingGroupSetup(props.entry.groupIdB64);
    if (setup !== null) void runPendingGroupSetup(setup);
  });

  async function onAddMember(): Promise<void> {
    const username = addMemberInput().trim();
    if (username.length === 0) return;
    if (!mayLookUp(addMemberCheck())) return;
    setError(null);
    setNotice(null);
    setBusy(true);
    // ADR-0045 UX §7 — the add descriptor is only knowable once the typed
    // handle has resolved, which happens inside the call, so it is captured
    // through the resolution hook rather than reconstructed from a throw. The
    // pending count is read at the SAME moment: it is the baseline the drain
    // is measured against, and reading it before the discovery round-trip
    // would compare against a set that may since have moved.
    let addRequest: RetryableRequest | null = null;
    let addPendingBefore = 0;
    try {
      const result = await contacts.addMemberToGroupByUsername(
        props.requireIdentity(),
        props.entry.group,
        username,
        (recipient) => {
          addRequest = {
            kind: "add",
            subjectKeyB64: base64Encode(recipient.signaturePubkey),
            subjectAccountIdB64: base64Encode(recipient.accountId),
          };
          // A manual attempt owns the slot: disarm any retry already armed for
          // this same person before starting, so an in-flight one cannot
          // publish a second copy behind this.
          disarmRetry(addRequest);
          addPendingBefore = pendingCountNow();
        },
      );
      afterMemberAdded(result.outcome, base64Encode(result.recipientSignaturePubkey));
      const addedKeyB64 = base64Encode(result.recipientSignaturePubkey);
      const addCommitted = result.outcome.kind === "committed";
      setNotice(() =>
        addCommitted
          ? t("chat.members.add.done", { name: memberName(addedKeyB64) })
          : t("chat.members.add.requested", { name: memberName(addedKeyB64) }),
      );
      setAddMemberInput("");
      setAddMemberOpen(false);
      // ADR-0045 — if this device is the committer (the owner, rank 0) sweep
      // the request it just published straight away, so the common case is
      // still "tap, done". Awaited, not fired-and-forgotten, so the panel
      // refresh below sees the post-sweep roster.
      await sweepNow();
      // Pull anything that arrived during the add (no-op when
      // there's nothing new but cheap, and keeps the cursor
      // moving past the suppressed commit row).
      void refresh();
    } catch (err) {
      // ADR-0045 UX §7 — `addRequest` is `null` only when the failure came
      // BEFORE the handle resolved (an unknown username, a discovery outage),
      // where there is no request to re-send and nothing to arm.
      if (addRequest !== null) {
        armRetryIfQueueFull(addRequest, err, addPendingBefore);
      }
      setError(errorText(err, "chat.error.add_member"));
    } finally {
      setBusy(false);
    }
  }

  // 1.18h — Remove-member handler. Symmetric to onAddMember
  // minus the discovery/Welcome dance: caller picks the leaf
  // index from the members list, we drive the MLS remove,
  // broadcast the commit, suppress the echo. The removed
  // member's leaf is gone from `group.members()` immediately;
  // their own client lands the commit on next poll and learns
  // they're out.
  async function onRemoveMember(leafIndex: number): Promise<void> {
    // ADR-0045 UX §6 — the confirmation is the LAST EXIT, because a published
    // request cannot be withdrawn: it is in every member's proposal store and
    // dies only when some commit lands, and there is no wire withdraw. The body
    // line is not a nicety — it is the one fact a user removing someone for a
    // safety reason must have, and the fact the old immediate-looking UI
    // concealed.
    //
    // ADR-0045 UX §14 — an in-page dialog rather than `window.confirm`. §6's
    // decision is unchanged and still lives in `remove-member-confirm.ts`;
    // what the widget adds is §6.1's rule that the action button NAMES the
    // person, which a browser OK/Cancel pair cannot do. The roster label is
    // read from `rosterViews()` so the button and the row the user just
    // pressed cannot name the same member differently — the one thing they
    // cross-check. An absent label falls back to the bare `Remove`.
    const decided = removeMemberConfirmation({
      usesProposalPath: usesProposalPath(),
    });
    const memberLabel = rosterViews().find(
      (r) => r.leafIndex === leafIndex,
    )?.label;
    if (
      !(await requestConfirm({
        ...decided,
        confirmLabel: namedActionKey("common.remove", "chat.members.remove.confirm_named", memberLabel),
        tone: "danger",
      }))
    ) {
      return;
    }
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      // C-61 — rotate STRICTLY AFTER the Remove commit is published. Both
      // guarantees the ordering buys depend on it:
      //
      //   - forward secrecy (ADR-0043): the distribution envelope is an MLS
      //     application message, so sealed BEFORE the commit it is readable
      //     by the member being evicted (still in that epoch) and the
      //     rotation buys nothing;
      //   - recency (ADR-0044): the new secret is stamped with `group.epoch`
      //     at mint time, so after the commit it strictly out-ranks the
      //     secret it replaces. Before it, it would TIE with a stale secret
      //     minted at the same epoch and win or lose on the byte tiebreak.
      //
      // The sequencing lives in `publishRemoveThenRotate` rather than in two
      // adjacent statements here, so it is pinned by a test instead of by
      // convention.
      //
      // Owner-or-admin (ADR-0043 ratified option (b)) — the gate lives inside
      // `rotatePushRoutingSecret`, which also no-ops when this device holds no
      // secret, so rotation never becomes a second mint path.
      //
      // Fire-and-forget: push is best-effort and a failed rotation must not
      // unwind the removal. A failed BROADCAST leaves the row pending, and
      // the existing `ensureGroupPushRouting` re-broadcast branch re-sends it
      // on the next chat-open / launch.
      const session = auth.state();

      // ADR-0045 — on the proposal path the removal does NOT happen here: this
      // publishes a request, and the member leaves the group only when a
      // committer sweeps it. That moves C-61 with it.
      //
      // C-61 requires the rotation to follow the Remove COMMIT, because the
      // distribution envelope is an MLS application message: sealed before the
      // commit, it is readable by the leaf being evicted and the rotation buys
      // nothing. Rotating after publishing a *proposal* would do exactly that —
      // the proposal advances no epoch, so the evicted member is still in the
      // one the new secret would be sealed at.
      //
      // So the rotation moves to where the removal actually lands: the sweep,
      // via the same membership-SHRINK detection the poll loop already runs for
      // someone else's removal. `sweepNow` captures the roster either side and
      // rotates when it shrinks, which keeps the ordering correct by
      // construction rather than by an argument about two adjacent statements.
      if (usesProposalPath()) {
        // ADR-0045 UX §7 — armed against the member's signature key, resolved
        // now and re-resolved to a leaf at fire time. A torn roster read means
        // no retry for this attempt rather than a retry against a guess.
        const targetKeyB64 = rosterViews().find(
          (m) => m.leafIndex === leafIndex,
        )?.signatureKeyB64;
        const removeRun = () =>
          contacts.removeMemberFromGroup(
            props.requireIdentity(),
            props.entry.group,
            leafIndex,
          );
        const proposed =
          targetKeyB64 === undefined
            ? await removeRun()
            : await withMembershipRetry(
                { kind: "remove", subjectKeyB64: targetKeyB64 },
                removeRun,
              );
        chat.markOwnPublishedCommit(
          props.entry.groupIdB64,
          proposed.kind === "committed"
            ? proposed.commitMessageId
            : proposed.proposalMessageId,
        );
        if (proposed.kind === "proposed") noteOwnRequest(`remove:${leafIndex}`);
        const swept = await sweepNow();
        setMemberCount(peopleCountOf(props.entry.group));
        setMembersVersion(membersVersion() + 1);
        const removeLanded = swept?.kind === "committed";
        setNotice(() =>
          removeLanded ? t("chat.members.remove.done") : t("chat.members.remove.requested"),
        );
        void refresh();
        return;
      }

      const result = await publishRemoveThenRotate({
        publishRemove: async () => {
          const removed = await contacts.removeMemberFromGroup(
            props.requireIdentity(),
            props.entry.group,
            leafIndex,
          );
          if (removed.kind === "committed") {
            chat.markOwnPublishedCommit(props.entry.groupIdB64, removed.commitMessageId);
          }
          return removed;
        },
        rotate:
          session.kind === "session"
            ? async (): Promise<PushRotationOutcome> =>
                rotatePushRoutingSecret(
                  session.account.accountId,
                  props.entry.group,
                  // Live role read, same reason as the poll path: the
                  // `ownRole()` signal can still hold a pre-commit value.
                  await readOwnRoleNow(),
                  broadcastToGroup,
                )
            : undefined,
        onRotationError: (err) => {
          // eslint-disable-next-line no-console
          console.warn("push-routing rotation failed", err);
        },
      });
      setMemberCount(peopleCountOf(props.entry.group));
      setMembersVersion(membersVersion() + 1);
      const removeCommitted = result.kind === "committed";
      setNotice(() =>
        removeCommitted ? t("chat.members.remove.done") : t("chat.members.remove.requested_short"),
      );
      void refresh();
    } catch (err) {
      setError(errorText(err, "chat.error.remove_member"));
    } finally {
      setBusy(false);
    }
  }

  // ADR-0022 — promote / demote a member as admin (owner only). Publishes the
  // setGroupRoles commit + suppresses the echo, same shape as add / remove.
  async function onToggleAdmin(
    member: { leafIndex: number; sigPubkey: Uint8Array },
    makeAdmin: boolean,
  ): Promise<void> {
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      const result = await withMembershipRetry(
        {
          kind: "set-admin",
          subjectKeyB64: base64Encode(member.sigPubkey),
          makeAdmin,
        },
        () =>
          contacts.setMemberAdmin(
            props.requireIdentity(),
            props.entry.group,
            member.sigPubkey,
            makeAdmin,
          ),
      );
      chat.markOwnPublishedCommit(
        props.entry.groupIdB64,
        result.kind === "committed"
          ? result.commitMessageId
          : result.proposalMessageId,
      );
      if (result.kind === "proposed") noteOwnRequest("role-change");
      // ADR-0045 — a role change is owner-only and the owner is rank 0, so the
      // sweep that follows is the same device: in practice this is still
      // immediate.
      const swept = await sweepNow();
      setMembersVersion(membersVersion() + 1);
      if (result.kind === "committed" || swept?.kind === "committed") {
        setNotice(() =>
          makeAdmin ? t("chat.members.admin.promoted") : t("chat.members.admin.demoted"),
        );
      } else {
        setNotice(() => t("chat.members.admin.requested"));
      }
      void refresh();
    } catch (err) {
      setError(errorText(err, "chat.error.change_admin"));
    } finally {
      setBusy(false);
    }
  }

  // ADR-0027 (G-3), on the ADR-0045 proposal path since S7 — approve a pending
  // external-join request (owner / admin only). The approve publishes ONE
  // artifact: an ordinary admin-authored `Add` proposal. No commit, no epoch
  // advance, and no Welcome here — the joiner's Welcome (and the
  // `WelcomeRendezvous` it opens) is produced by whichever device sweeps the
  // proposal, which may be another admin and has no deadline.
  async function onApproveJoin(proposalRef: Uint8Array): Promise<void> {
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      // ADR-0045 UX §7 — an approve refused by a full pending set now carries
      // its reason (the core stopped flattening it into prose in `21f6c8c`),
      // so it re-sends itself when a committer drains the queue. The wrapper
      // also disarms first, so tapping Approve again takes the record rather
      // than racing an in-flight re-send for it.
      const result = await withApproveJoinRetry(proposalRef, () =>
        contacts.approveExternalJoin(
          props.requireIdentity(),
          props.entry.group,
          proposalRef,
        ),
      );
      chat.markOwnPublishedCommit(props.entry.groupIdB64, result.proposalMessageId);
      // ADR-0045 — the same bookkeeping every other membership request gets:
      // the quiet window is measured against the DS row, and this device knows
      // the request is its own.
      // The third argument is ADR-0045 S7 residual 1: this add came from a JOIN
      // request, and only this device can know that (on the wire an approved
      // add is indistinguishable from an admin adding a contact). If the sweep
      // never carries it, that flag is what makes the notice say `Ask them to
      // request to join again` rather than offering a retry this side cannot
      // perform — a successful approve consumed the retained request.
      noteOwnRequest(`add:${base64Encode(proposalRef)}`);
      // If this device is the acting committer the sweep runs now, so the
      // common single-admin case still reads as "tap, done".
      const swept = await sweepNow();
      setMemberCount(peopleCountOf(props.entry.group));
      setMembersVersion(membersVersion() + 1);
      if (swept?.kind === "committed") {
        setNotice(() => t("chat.members.join.approved_added"));
      } else {
        // ADR-0045 S7 residual 2 — an approve that did not apply has grown a
        // queue only a sweep drains, and the core's own feedback (its cap
        // refusal) lands on whoever proposes NEXT. Naming the depth here is
        // what puts it in front of the person who caused it. Read from the
        // core rather than from the memo, which has not re-run yet.
        let depth: number | null = null;
        try {
          depth = (props.entry.group.pendingProposals() as unknown[]).length;
        } catch {
          depth = null;
        }
        const queueDepth = depth;
        setNotice(() => {
          const approved = t("chat.members.join.approved_requested");
          const queueLine = queueDepth === null ? null : waitingQueueLine(queueDepth);
          return queueLine === null
            ? approved
            : t("common.sentence_pair", { first: approved, second: queueLine });
        });
      }
      // Same reason as in `onRejectJoin`: the queue moved, so the notice
      // about a request it turned away is no longer current.
      setUnkeptJoinRequest(false);
      void refresh();
    } catch (err) {
      setError(errorText(err, "chat.error.approve_join"));
    } finally {
      setBusy(false);
    }
  }

  // ADR-0027 (G-3) — reject a pending external-join request (owner / admin
  // only). Local-only: drops the retained proposal so it leaves the pending
  // list. No commit, no journal post — the joiner never receives a Welcome.
  async function onRejectJoin(proposalRef: Uint8Array): Promise<void> {
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      // ADR-0045 UX §7 — a reject is the user saying no, so any armed re-send
      // of an approve for the same request goes with it. The verdict would
      // drop it anyway once it saw the emptied quarantine
      // (`join-request-gone`), but leaving a record that contradicts a
      // decision the user just made is not a state to keep for a poll cycle.
      disarmRetry({
        kind: "approve-join",
        subjectKeyB64: base64Encode(proposalRef),
      });
      await contacts.rejectExternalJoin(
        props.requireIdentity(),
        props.entry.group,
        proposalRef,
      );
      setMembersVersion(membersVersion() + 1);
      // Acting on the queue is what may have made room for the request the
      // quarantine turned away, so the notice about it stops being current.
      setUnkeptJoinRequest(false);
      setNotice(() => t("chat.members.join.rejected"));
    } catch (err) {
      setError(errorText(err, "chat.error.reject_join"));
    } finally {
      setBusy(false);
    }
  }

  // ADR-0028 (G-3) — mint a group invite-link (owner/admin only). Signs a
  // capability over this group's id with the local identity key and shows the
  // `/g/<slug>` link for the owner to copy/share. A non-member opens it,
  // requests to join, and an admin approves (ADR-0027). `0n` = never expires.
  async function onCreateInviteLink(): Promise<void> {
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      // ADR-0028 v2 — the preview the link carries: the shared group name
      // (ADR-0051) or the user-given label, never a stand-in; this device's
      // own profile name; the people in the roster now. The link key rides
      // the URL fragment (docs/ux/group-invite-page.md).
      const ownLabel = props.entry.label;
      const link = await contacts.createGroupInviteLink(
        props.requireIdentity(),
        props.entry.group,
        0n,
        window.location.origin,
        {
          groupName:
            groupNames.nameOf(props.entry.groupIdB64) ??
            (isUnnamedGroupLabel(ownLabel) ? undefined : ownLabel.trim()),
          inviterName: profiles.ownProfile().displayName?.trim() || undefined,
          memberCount: memberCount(),
        },
      );
      setInviteLink(link);
    } catch (err) {
      setError(errorText(err, "chat.error.create_link"));
    } finally {
      setBusy(false);
    }
  }

  // ADR-0028 revoke — kill the currently-shown group invite link. A pointer
  // link carries a ledger blob the server deletes (issuer-signed); an inline
  // link has no ledger row, so we just clear the field.
  async function onRevokeInviteLink(): Promise<void> {
    const link = inviteLink();
    if (!link) return;
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      const revoked = await contacts.revokeGroupInviteLink(props.requireIdentity(), link);
      setInviteLink(null);
      setNotice(() =>
        revoked ? t("chat.invite_link.revoked") : t("chat.invite_link.cleared"),
      );
    } catch (err) {
      setError(errorText(err, "chat.error.revoke_link"));
    } finally {
      setBusy(false);
    }
  }

  // Pull the current backlog on mount so a fresh `/chat/:id`
  // visit shows whatever the peer has sent so far.
  //
  // Phase 2a3f1-web — also seed `rows` from the persistent
  // message log BEFORE the first refresh. This way reloads
  // show prior history immediately + the DS poll just
  // catches up any rows that arrived while the tab was
  // closed (the persisted `cursor` lets the next refresh
  // start from the right slot).
  // ADR-0042 — post the given control bytes (a push-routing-secret
  // distribution envelope) to the group journal as an E2EE MLS
  // application message via the fail-closed persist-before-publish path.
  async function broadcastToGroup(bytes: Uint8Array): Promise<void> {
    await chat.broadcastControl(
      props.requireIdentity(),
      props.entry.group,
      bytes,
    );
  }

  // ADR-0042 — the owner-mint + subscribe funnel for THIS group. Reads the
  // session lazily so it always sees the current account.
  async function provisionPushRouting(): Promise<void> {
    const s = auth.state();
    if (s.kind !== "session") return;
    const id = chats.identity();
    if (id === null) return;
    try {
      await ensureGroupPushRouting(
        s.account.accountId,
        props.entry.group,
        id,
        broadcastToGroup,
      );
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("push-routing provisioning failed", err);
    }
  }

  /** Events the background receive applied for this group — held while the
   *  view was not mounted, and delivered live after that. Through the same
   *  consumer as a poll batch, so every kind is folded exactly as if this view
   *  had polled it. */
  function onBackgroundEvents(events: readonly IncomingChatEvent[]): void {
    consumeEvents(events);
    if (events.some((e) => e.kind === "system" || e.kind === "externalJoinRequest")) {
      setMemberCount(peopleCountOf(props.entry.group));
      setMembersVersion(membersVersion() + 1);
    }
  }
  let detachFromBackground: (() => void) | null = null;
  let detachPollRequest: (() => void) | null = null;
  onCleanup(() => {
    detachFromBackground?.();
    detachFromBackground = null;
    detachPollRequest?.();
    detachPollRequest = null;
  });

  onMount(() => {
    void (async (): Promise<void> => {
      await hydrateFromLog();
      // ADR-0073 R.7 case 1 — messages held for this conversation go out if
      // this view opened its successor; R.2 — opening a one-to-one checks the
      // peer's published identity.
      // After a reload they are read back from the message log first.
      const heldAccount = accountIdB64ForLog();
      if (heldAccount !== null) await restoreHeldSends(heldAccount);
      // What the background flush sent while this view was mounting.
      showFlushedHeldSends();
      resumeHeldSends();
      void checkPeerIdentity(false);
      // Attached only once the timeline is seeded from the log: the consumer
      // de-duplicates against the rows it holds, and events drained into an
      // empty timeline would stop the seed from running.
      if (!receiveLoopAbort.signal.aborted) {
        detachFromBackground = receive.coordinator.attachView(
          props.entry.groupIdB64,
          onBackgroundEvents,
        );
        // ADR-0050 — the background receive publishes a device fan-out
        // proposal into this (foreground) group and cannot sweep it itself;
        // it asks this view to poll, whose tail sweeps. `refresh` is the
        // same entry the Refresh button uses, so it runs while the tab is
        // hidden too.
        detachPollRequest = receive.coordinator.onPollRequested(
          props.entry.groupIdB64,
          () => {
            void refresh();
          },
        );
      }
      // ADR-0040 (G-35) — re-publish any pending commit that crashed mid-window
      // on a previous run BEFORE the first poll, re-arming the own-echo filter
      // for the re-published commit so `refresh()` (below) drops it instead of
      // hitting CannotDecryptOwnMessage. Best-effort — a re-publish failure
      // leaves the entry un-acked for the next mount's sweep.
      try {
        // ADR-0045 obligation 1 — the SAME restart seam also finishes a verdict
        // this device owes. A staged sweep is durable and freezes this group's
        // membership operations across restarts, so a page that closed between
        // publishing a sweep commit and reading the journal order must resolve
        // it before anything else touches the group. Handing the cursor + the
        // echo hook in is what lets it re-publish idempotently and re-adjudicate
        // exactly as if nothing had happened.
        // ADR-0045 — an owed verdict applies journal rows exactly as a fresh
        // sweep does and advances the same cursor over them, so the restart
        // path has to surface them too. `hydrateFromLog()` has already run, so
        // the timeline is ready to receive them, and `refresh()` below starts
        // from the cursor this leaves behind.
        const sweptOnRestart: IncomingChatEvent[] = [];
        try {
          await contacts.sweepPendingCommits(
            props.entry.group,
            (id) => chat.markOwnPublishedCommit(props.entry.groupIdB64, id),
            props.requireIdentity(),
            { read: () => cursor(), advance: (to) => setCursor(to) },
            (event) => sweptOnRestart.push(event),
          );
        } finally {
          if (sweptOnRestart.length > 0) consumeEvents(sweptOnRestart);
        }
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("commit-outbox sweep failed", err);
      }
      await refresh();
      // Automatic receive from here on — see `startReceiveLoop`. Not started
      // earlier so the first long poll cannot race the outbox sweep for the
      // cursor it hands to `refresh`.
      if (!receiveLoopAbort.signal.aborted) startReceiveLoop();
      // ADR-0045 S4b — announce this device immediately rather than waiting a
      // tick. The core's own rule is that the FIRST beacon of a session is
      // immediate, precisely because a candidate that has just opened the app
      // is when a stale view is likeliest to hand the duty elsewhere.
      void emitBeaconIfDue();
      // ADR-0048 §2 trigger 4 — opening a group some of whose people this
      // device still cannot name asks them to introduce themselves, at most
      // once a day per group (the core's `shouldRequestProfiles`). After the
      // first poll, so the names that were already on the journal have been
      // filed and are not asked for again. The roster's PEOPLE (account
      // keys), which is what the resolver names.
      void profiles
        .requestProfilesOnOpen(
          props.entry.groupIdB64,
          untrack(() => members()).map((m) => base64Encode(m.sigPubkey)),
        )
        .catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("ADR-0048 profile request not sent", err);
        });
      // ADR-0042 — ensure this group's opaque push routing is provisioned.
      // Owner + no secret yet → mint + persist + broadcast the distribution
      // envelope (E2EE journal); then subscribe under `push_gid` (or defer
      // if this device holds no secret). Idempotent + race-safe (a shared
      // per-group in-flight lock coalesces this with the rehydrate sweep).
      // Best-effort — never blocks the chat.
      void provisionPushRouting();
    })();
  });

  // Phase 2a3e2c-web — read cursor sync. Every time the
  // timeline grows past the previously-broadcast value, mint
  // a fresh `ReadCursor` payload pointing at the newest
  // visible message. The sibling-device chat list folds the
  // cursor into its unread-count tracker on its next mount.
  //
  // Debounced to 1500 ms so a noisy poll batch doesn't fan
  // out one DeviceGroup write per row; the user's "thread
  // open + scrolling" intent is captured at the end of the
  // burst.
  let lastBroadcastReadId: bigint | null = null;
  let readCursorTimer: ReturnType<typeof setTimeout> | null = null;
  const READ_CURSOR_DEBOUNCE_MS = 1500;
  createEffect(() => {
    const list = rows();
    if (list.length === 0) return;
    // Find the highest server-assigned messageId — outgoing
    // rows with `ack === "pending"` carry no messageId yet so
    // they're skipped.
    let lastId: bigint | null = null;
    for (const r of list) {
      let id: number | undefined;
      if (r.kind === "outgoing") {
        if (r.ack === "sent" && r.messageId !== undefined) id = r.messageId;
      } else {
        id = r.event.messageId;
      }
      if (id !== undefined) {
        const b = BigInt(id);
        if (lastId === null || b > lastId) lastId = b;
      }
    }
    if (lastId === null) return;
    if (lastBroadcastReadId !== null && lastBroadcastReadId >= lastId) return;
    const target = lastId;
    if (readCursorTimer !== null) clearTimeout(readCursorTimer);
    readCursorTimer = setTimeout(() => {
      readCursorTimer = null;
      lastBroadcastReadId = target;
      // ADR-0073 R.5 — the read position is the CONVERSATION's: keyed on its
      // id, and (past the first group) carrying which of its groups the
      // message id belongs to. In the first group this is the 8 big-endian
      // bytes every build has always written.
      const groupId = conversationIdBytes();
      const idBytes = encodeReadPosition({
        segment: lineageView.segmentIndexOf(props.entry.group.groupId),
        messageId: Number(target),
      });
      void sync
        .setReadCursor(groupId, idBytes, BigInt(Date.now()))
        .catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("setReadCursor failed", err);
        });
    }, READ_CURSOR_DEBOUNCE_MS);
  });

  onCleanup(() => {
    if (readCursorTimer !== null) {
      clearTimeout(readCursorTimer);
      readCursorTimer = null;
    }
  });

  // Debounced persist whenever the timeline mutates. We
  // re-read `rows()` inside the effect so every transitive
  // signal change schedules a write; the 500 ms debounce
  // collapses bursts (poll batch, send + ack flip) into one
  // write.
  createEffect(() => {
    // touch the signals
    void rows();
    void cursor();
    // ADR-0047 — the local "played" marker is persisted with the row.
    void hub.playedKeys();
    if (!didHydrateFromLog) return;
    if (pendingPersistTimer !== null) clearTimeout(pendingPersistTimer);
    pendingPersistTimer = setTimeout(() => {
      pendingPersistTimer = null;
      void persistToLogBestEffort();
    }, PERSIST_DEBOUNCE_MS);
  });

  onCleanup(() => {
    if (pendingPersistTimer !== null) {
      clearTimeout(pendingPersistTimer);
      pendingPersistTimer = null;
      // Flush rather than drop: rows applied in the last debounce window have
      // already moved the shared cursor, so the background receive that takes
      // this group over will not fetch them again — this write is the only
      // place they reach the log. The merge keeps anything it files meanwhile.
      void persistToLogBestEffort();
    }
  });

  /** ADR-0073 R.5 — fill `historyRows` from the logs of the conversation's
   *  older groups. Pure display: nothing here writes anywhere. */
  function seedHistoryRows(log: NonNullable<Awaited<ReturnType<typeof loadMessageLog>>>): void {
    const segments = historySegments();
    const history: TimelineRow[] = [];
    const ops = new Map<number, MessageOpState>();
    segments.forEach((segment, index) => {
      const stored = log.groups[hexOf(segment.group.groupId)];
      if (stored === undefined) return;
      const distance = segments.length - index;
      for (const m of stored.messages) {
        history.push(asHistoryRow(persistedToTimelineRow(m), distance));
      }
      for (const [id, op] of opsFromPersisted(stored.messages)) {
        ops.set(historyRowId(id, distance), op);
      }
    });
    setHistoryRows(history);
    setHistoryOps(ops);
  }

  function reloadHistoryRows(): void {
    void (async () => {
      const accountIdB64 = accountIdB64ForLog();
      if (accountIdB64 === null) return;
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey === null) return;
      const log = await loadMessageLog(accountIdB64, deviceKey);
      if (log !== null) seedHistoryRows(log);
    })().catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("conversation history load failed", err);
    });
  }

  // The lineage can arrive after this view mounted (the synced state loads
  // on its own schedule, and a claim can land while the chat is open): the
  // history follows the set of older groups this device holds.
  createEffect(
    on(
      () => historySegments().map((g) => g.groupIdB64).join(","),
      () => reloadHistoryRows(),
      { defer: true },
    ),
  );
  // R.7 case 3 — members who have not moved yet still write in the old
  // group; the background receive files their rows into its log, and they
  // appear here, above, as they arrive.
  onMount(() => {
    const unsubscribe = subscribeMessageLogChanged((account) => {
      if (historySegments().length > 0 && account === accountIdB64ForLog()) reloadHistoryRows();
    });
    onCleanup(unsubscribe);
  });

  // -------------------------------------------------------------------------
  // ADR-0073 R.2 / R.7 case 1 — the held send and the peer's identity check
  // -------------------------------------------------------------------------

  const conversationHex = (): string => hexOf(conversationIdBytes());

  /** The superseded account that makes THIS group unrepairable, read off the
   *  live group and store (never a memo: the send path asks right after a
   *  check may have re-pinned). `null` once the view is on the successor. */
  function unrepairableKeyNow(): string | null {
    let owner: string | undefined;
    try {
      const o = props.entry.group.roleTableOwner() as Uint8Array | undefined;
      owner = o === undefined ? undefined : base64Encode(o);
    } catch {
      owner = undefined;
    }
    return unrepairableBy({
      kind: isGroupChat() ? "group" : "chat",
      rosterPersonKeysB64: personKeysOf(props.entry.group),
      ownPersonKeyB64: ownPersonKeyB64(chats.identity()),
      ownerKeyB64: owner,
      supersededKeys: loadSupersededKeys(),
    });
  }

  /** What an edit, a delete, a reaction or an ephemeral signal does now
   *  (`held-send.ts` `opRoute`). */
  function opRouteNow(): OpRoute {
    return opRoute({
      kind: isGroupChat() ? "group" : "chat",
      unrepairableKeyB64: isGroupChat() ? null : unrepairableKeyNow(),
    });
  }

  /**
   * R.2 "Triggers" — run the fail-closed identity check of the one-to-one's
   * peer: at open, and before a send when the last check is older than the
   * re-admission arm's interval. Silent whatever it answers: an accepted
   * reset re-pins and starts the conversation's move; a refusal and "no
   * verdict" change nothing here (the refusal copy belongs to actions that
   * need the new key, and this is not one).
   */
  async function checkPeerIdentity(force: boolean): Promise<void> {
    if (isGroupChat()) return;
    const key = conversationHex();
    const now = Date.now();
    if (!force && !identityCheckDue(lastPeerIdentityCheckAtMs.get(key), now, READMISSION_INTERVAL_MS)) {
      return;
    }
    const peer = peerKeyB64();
    if (peer === null) return;
    const accountIdB64 = accountIdB64OfKey(peer);
    if (accountIdB64 === null) return;
    lastPeerIdentityCheckAtMs.set(key, now);
    try {
      await contacts.checkContactIdentity(base64Decode(accountIdB64), peer);
    } catch {
      // Refused, inconclusive or offline: nothing changes, nothing is shown.
    }
  }

  /** Give-up timers of the messages this view holds, by row. */
  const heldGiveUpTimers = new Map<number, ReturnType<typeof setTimeout>>();

  /** Hold one outgoing row until the successor exists (the row stays in the
   *  ordinary sending state). The give-up puts it in the ordinary failed
   *  state, whose Retry enters the same path again. */
  function holdOutgoing(tempId: number, payload: HeldMessage, heldAtMs: number): void {
    const key = conversationHex();
    heldSends.hold(key, payload, heldAtMs);
    // Mirrored into the message log, so a reload does not lose it.
    void persistHeldSends(accountIdB64ForLog());
    const wait = Math.max(0, heldAtMs + HELD_SEND_GIVE_UP_MS - Date.now());
    const previous = heldGiveUpTimers.get(tempId);
    if (previous !== undefined) clearTimeout(previous);
    heldGiveUpTimers.set(
      tempId,
      setTimeout(() => {
        heldGiveUpTimers.delete(tempId);
        // Still held: take it back out of the store, leave the rest.
        const all = heldSends.take(key);
        let found = false;
        for (const h of all) {
          if (!found && h.payload === payload) found = true;
          else heldSends.hold(key, h.payload, h.heldAtMs);
        }
        if (!found) return;
        void persistHeldSends(accountIdB64ForLog());
        setRows([...withSendAck(rows(), tempId, "failed")]);
        setError(() => sendFailureNotice(null));
      }, wait),
    );
  }

  /** Bring the messages held for this conversation into this view: each is
   *  an outgoing row in the sending state again, and goes out now if the
   *  group this view opened can take it (the successor), else stays held. */
  function resumeHeldSends(): void {
    // Not mirrored to the log at the take: each message is either held again
    // at once (the mirror is rewritten there) or sent (rewritten below), so a
    // reload in between still finds it.
    const taken = heldSends.take(conversationHex());
    for (const held of taken) {
      const tempId = nextTempId++;
      // R.7 case 1 — a reply composed in another segment keeps its quote and
      // loses its jump target: the id belongs to that segment's journal.
      const reply = replyForSend(
        held.payload.reply,
        held.payload.replyGroupIdB64,
        props.entry.groupIdB64,
      );
      setRows([
        ...rows(),
        {
          kind: "outgoing",
          tempId,
          plaintext: held.payload.text,
          attachments: held.payload.attached,
          ack: "pending",
          authorKey: ownSignatureKeyB64(),
          ...(reply === undefined ? {} : { reply }),
          ...(held.payload.forward === undefined ? {} : { forward: held.payload.forward }),
        },
      ]);
      void publishOutgoing(
        tempId,
        held.payload.text,
        held.payload.attached,
        reply,
        held.heldAtMs,
        held.payload.forward,
      ).finally(() => persistHeldSends(accountIdB64ForLog()));
    }
  }

  /** What the background flush sent into THIS group while this view was
   *  mounting (it flushes only while no view is attached): kept until the
   *  rows are on screen. */
  let flushedForThisGroup: PersistedMessage[] = [];
  /** Show the flushed messages this view does not hold yet
   *  (`held-send.ts` `flushedRowsToShow`). Run when a flush reports and
   *  again once the log is read, whichever order they came in. */
  function showFlushedHeldSends(): void {
    if (flushedForThisGroup.length === 0) return;
    const shown = new Set<number>();
    for (const r of rows()) {
      const id = rowMessageId(r);
      if (id !== undefined) shown.add(id);
    }
    const missing = flushedRowsToShow(shown, flushedForThisGroup);
    if (missing.length > 0) setRows([...rows(), ...missing.map(persistedToTimelineRow)]);
  }
  const unsubscribeHeldFlushed = subscribeHeldFlushed((report) => {
    if (report.groupIdB64 !== props.entry.groupIdB64) return;
    flushedForThisGroup = [...flushedForThisGroup, ...report.messages];
    // Before the log is read the timeline is empty and the seed only fills
    // an EMPTY timeline: the rows are added after it instead.
    if (didHydrateFromLog) showFlushedHeldSends();
  });
  onCleanup(unsubscribeHeldFlushed);

  onCleanup(() => {
    // The messages stay in the store (the next mount of the conversation
    // resumes them); only this view's timers go.
    for (const timer of heldGiveUpTimers.values()) clearTimeout(timer);
    heldGiveUpTimers.clear();
  });

  async function hydrateFromLog(): Promise<void> {
    try {
      const accountIdB64 = accountIdB64ForLog();
      if (accountIdB64 === null) {
        didHydrateFromLog = true;
        return;
      }
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey === null) {
        didHydrateFromLog = true;
        return;
      }
      const log = await loadMessageLog(accountIdB64, deviceKey);
      didHydrateFromLog = true;
      if (log === null) return;
      // ADR-0073 R.5 — the conversation's older groups, read-only, above.
      seedHistoryRows(log);
      const slot = log.groups[groupIdHexForLog()];
      if (slot === undefined) return;
      const seeded = slot.messages.map(persistedToTimelineRow);
      // ADR-0047 — restore which voice messages / video notes this device
      // has already played (local-only, UX spec §8).
      hub.seedPlayed(
        slot.messages.flatMap((m) =>
          m.attachments
            .filter((a) => a.played === true)
            .map((a) => mediaKey(b64UrlDecode(a.blobIdB64))),
        ),
      );
      // Only seed when the in-memory list is empty — a refresh
      // running concurrently with hydration shouldn't get
      // stomped by the disk load.
      if (rows().length === 0) {
        setRows(seeded);
      }
      // ADR-0023 — restore the edit / delete / reaction overlay. Guarded on
      // the overlay rather than on `rows`, because it is keyed on DS ids and
      // lands correctly on whichever rows are present; and not seeded over a
      // non-empty overlay, which can only mean the poll has already applied
      // ops this session that the stored rows predate. This is also what
      // stops the next persist writing the restored rows back WITHOUT their
      // ops: that write reads the overlay, not the row.
      if (msgOps().size === 0) {
        const storedOps = opsFromPersisted(slot.messages);
        if (storedOps.size > 0) setMsgOps(storedOps);
      }
      const cur = Number(slot.cursor);
      if (Number.isFinite(cur) && cur > cursor()) {
        setCursor(cur);
      }
    } catch (err) {
      // Best-effort — corrupt blob or transient IDB failure
      // shouldn't block the chat surface. The DS poll backs
      // up the timeline anyway.
      // eslint-disable-next-line no-console
      console.warn("hydrateFromLog failed", err);
      didHydrateFromLog = true;
    }
  }

  async function persistToLogBestEffort(): Promise<void> {
    try {
      const accountIdB64 = accountIdB64ForLog();
      if (accountIdB64 === null) return;
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey === null) return;
      // ADR-0023 — fold the mutation overlay in on the way out. Without this
      // the row written here is the message as first received, so a reload
      // brings back text the author replaced and messages they deleted - and
      // brings them back for good, because the op rows are long behind the
      // cursor and MLS forward secrecy will not decrypt them a second time.
      const ops = msgOps();
      const messages = dedupByMessageId(rows())
        .map((r) => timelineRowToPersisted(r, hub.playedKeys()))
        .filter((m): m is PersistedMessage => m !== null)
        .map((m) => persistedWithOps(m, ops.get(Number(m.messageId))));
      const hex = groupIdHexForLog();
      const at = cursor();
      // Serialized with every other writer of this account's log, and MERGED
      // rather than replacing this group's slot: the background receive may
      // have filed newer rows for this chat after this view last looked (the
      // hand-off at mount, or this final write landing after the view closed).
      // See `chat/background-log.ts`.
      await updateMessageLog(accountIdB64, deviceKey, (existing) => {
        const groups = existing?.groups ?? {};
        return {
          // Bumped on write: this device now writes `authorKeyB64`.
          version: MESSAGE_LOG_VERSION,
          groups: {
            ...groups,
            [hex]: mergeForegroundSlot(groups[hex], { messages, cursor: at }),
          },
        };
      });
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("persistToLogBestEffort failed", err);
    }
  }

  /**
   * The other person in a one-to-one conversation, as the contact list knows
   * them. `null` only while the roster cannot be read: every one-to-one peer
   * is a contact by construction (`contacts/adopt-one-to-one.ts`), so a
   * missing record is not a state with a screen of its own - the header then
   * opens nothing (`chat/conversation-info.ts`).
   */
  const personRecord = createMemo(() => {
    acceptedVersion();
    membersVersion();
    return findPersonRecord(loadAccepted(), {
      groupIdB64: props.entry.groupIdB64,
      peerKeyB64: peerKeyB64(),
    });
  });

  // The timeline scrolls in its own region. Follow new rows while the reader
  // is at (or near) the bottom; leave the position alone once they have
  // scrolled up to read.
  let scrollEl: HTMLDivElement | undefined;
  let stickToBottom = true;
  /** ADR-0052 / section 3.1 "Tap to jump" — scroll the timeline to the row
   *  whose DS id is `messageId` and highlight it for about a second and a
   *  half. The row is looked up in what this device holds; when it is not
   *  there (older than what is loaded, never received here) nothing happens
   *  and nothing is said - a quote is not a promise that the original
   *  exists. */
  let highlightTimer: ReturnType<typeof setTimeout> | null = null;
  function jumpToMessage(messageId: number): void {
    // A reply whose original belongs to another segment carries no jump
    // target (`held-send.ts` `replyForSend`): the quote is all there is.
    if (messageId === REPLY_NO_JUMP_TARGET) return;
    const root = scrollEl;
    if (root === undefined) return;
    const target = root.querySelector<HTMLElement>(
      `[data-message-id="${String(messageId)}"]`,
    );
    if (target === null) return;
    // The reader is now looking at something older; new rows must not drag
    // them back down while the highlight is still up.
    stickToBottom = false;
    target.scrollIntoView({ block: "center", behavior: "smooth" });
    for (const el of root.querySelectorAll(".lk-msg-highlight")) {
      el.classList.remove("lk-msg-highlight");
    }
    if (highlightTimer !== null) clearTimeout(highlightTimer);
    target.classList.add("lk-msg-highlight");
    highlightTimer = setTimeout(() => {
      target.classList.remove("lk-msg-highlight");
      highlightTimer = null;
    }, 1500);
  }
  onCleanup(() => {
    if (highlightTimer !== null) clearTimeout(highlightTimer);
  });

  // ADR-0052 decision 3 / message-actions.md section 4 — pins. The store is
  // the session-wide `PinContext`; what this view adds is the LOCAL half of
  // the authority rule (which rows the sheet offers), the bar derived from
  // the store and the rows this device holds, and the tap.
  const pinsOneToOne = (): boolean => conversationKind() === "chat";
  /** Whether this device may set or clear a shared pin here: either
   *  participant of a one-to-one, else the ADR-0022 owner / admin, else any
   *  member of a table-less group (`ownRole() === null`); nobody once the
   *  membership has ended. UX only — the receive-side gate on every peer is
   *  the boundary, so a stale answer here costs a refused envelope, never a
   *  pin nobody agreed to. */
  const maySetSharedPin = (): boolean =>
    !membershipEnded() && (pinsOneToOne() || ownRole() !== "member");
  /** The stored pin an on-screen row id stands for: the row itself in this
   *  group, or `(older group, original id)` for a row of an older segment
   *  (ADR-0073 R.5, `pin-bar.ts` `pinTargetOfRow`). */
  const pinTargetOf = (rowId: number): PinTarget | null =>
    pinTargetOfRow({
      rowId,
      headGroupIdB64: props.entry.groupIdB64,
      olderGroupIdsB64: historySegments().map((g) => g.groupIdB64),
      origin: historyRowOrigin,
    });
  const pinStateOf = (rowId: number): PinState => {
    const target = pinTargetOf(rowId);
    return {
      pinnedShared:
        target !== null && pins.shared(target.groupIdB64).get(target.messageId)?.pinned === true,
      pinnedPersonal: target !== null && pins.personal(target.groupIdB64).has(target.messageId),
      // An older segment's shared pin is cleared on this device alone, which
      // every holder may do; a new shared pin there is refused below.
      maySetShared: target !== null && target.older ? true : maySetSharedPin(),
      oneToOne: pinsOneToOne(),
    };
  };
  /** Section 4.1 "The row" — `Pin` or `Unpin`, for the action card. */
  const pinLabelOf = (messageId: number): "Pin" | "Unpin" =>
    pinRowDecision(pinStateOf(messageId)).row;
  /** The sheet, when open: the message it is about and the rows it offers. */
  const [pinSheet, setPinSheet] = createSignal<{
    messageId: number;
    title: string;
    options: readonly PinOption[];
  } | null>(null);
  async function runPinOption(messageId: number, option: PinOption): Promise<void> {
    setError(null);
    try {
      const target = pinTargetOf(messageId);
      if (target === null) return;
      const route = pinRoute({ older: target.older, pinned: option.pinned, scope: option.scope });
      if (route === "refused") return;
      if (route === "local-only") {
        await pins.clearSharedLocally(target.groupIdB64, target.messageId);
      } else if (option.scope === "shared") {
        await pins.setShared(props.entry, target.messageId, option.pinned);
      } else {
        await pins.setPersonal(target.groupIdB64, target.messageId, option.pinned);
      }
    } catch (err) {
      setError(errorText(err, "chat.error.pin"));
    }
  }
  /** The `Pin` / `Unpin` row was chosen: act at once, or open the sheet. */
  function onPinRow(messageId: number): void {
    const decision = pinRowDecision(pinStateOf(messageId));
    if (decision.action.kind === "immediate") {
      void runPinOption(messageId, decision.action.option);
      return;
    }
    setPinSheet({ messageId, title: decision.action.title, options: decision.action.options });
  }
  /** What the bar needs about a held message, or `null` when this device
   *  does not hold it (not yet downloaded, older than what is loaded). */
  function heldForPin(messageId: number): HeldForPin | null {
    const row = uniqueRows().find((r) => rowMessageId(r) === messageId);
    if (row === undefined) return null;
    const op = msgOps().get(messageId) ?? historyOps().get(messageId);
    return { deleted: op?.deleted === true };
  }
  /** Section 4 — the bar's entries, in its order, with the author and the
   *  excerpt resolved the way the reply quote resolves them (the edited
   *  text of an edited message, the one-word stand-in for a media message). */
  const pinEntries = createMemo((): PinBarEntry[] => {
    // ADR-0073 R.5 — the pins of the conversation's older groups stay in
    // the bar, each under the id its row is drawn with.
    const older = historySegments();
    const spanned = pinsAcrossSegments(
      [
        ...older.map((segment, index) => ({
          distanceFromHead: older.length - index,
          shared: pins.shared(segment.groupIdB64),
          personal: pins.personal(segment.groupIdB64),
        })),
        {
          distanceFromHead: 0,
          shared: pins.shared(props.entry.groupIdB64),
          personal: pins.personal(props.entry.groupIdB64),
        },
      ],
      historyRowId,
    );
    const items = pinBarItems({
      shared: spanned.shared,
      personal: spanned.personal,
      held: heldForPin,
    });
    const out: PinBarEntry[] = [];
    for (const item of items) {
      const row = uniqueRows().find((r) => rowMessageId(r) === item.messageId);
      if (row === undefined) continue;
      const original =
        row.kind === "outgoing" ? row.plaintext : row.event.kind === "application" ? row.event.text : "";
      const attachments =
        row.kind === "outgoing"
          ? row.attachments
          : row.event.kind === "application"
            ? row.event.attachments
            : [];
      const text =
        (msgOps().get(item.messageId) ?? historyOps().get(item.messageId))?.editedText ?? original;
      const snapshot = replySnapshotFor(
        { text, attachments },
        authorLabelOf(row),
        (a) => hub.decision(a).bubble,
      );
      out.push({ ...item, author: snapshot.author, excerpt: snapshot.excerpt });
    }
    return out;
  });
  /** Which pin the bar shows; newest first, moved by the tap. */
  const [pinIndex, setPinIndex] = createSignal(0);
  createEffect(() => {
    const n = pinEntries().length;
    setPinIndex((i) => clampPinIndex(i, n));
  });
  const [pinListOpen, setPinListOpen] = createSignal(false);
  /** Section 4 — the tap: scroll to the shown pin with the reply highlight,
   *  then advance to the next older one, wrapping past the oldest. */
  function onPinBarTap(): void {
    const entries = pinEntries();
    const entry = entries[pinIndex()];
    if (entry === undefined) return;
    jumpToMessage(entry.messageId);
    setPinIndex(nextPinIndex(pinIndex(), entries.length));
  }
  createEffect(() => {
    rows().length;
    const el = scrollEl;
    if (el === undefined || !stickToBottom) return;
    requestAnimationFrame(() => {
      el.scrollTop = el.scrollHeight;
    });
  });

  return (
    <div
      class="lk-chat-body"
      onDragEnter={onDragEnter}
      onDragOver={onDragOver}
      onDragLeave={onDragLeave}
      onDrop={(e) => {
        void onDrop(e);
      }}
      style={{
        position: "relative",
        // Subtle outline + accent tint while a file drag hovers
        // anywhere over the chat body. The inner content stays
        // interactive; the overlay below the header is purely
        // a visual cue.
        outline: isDragOver()
          ? "2px dashed var(--accent)"
          : "2px dashed transparent",
        "outline-offset": "-4px",
        "border-radius": "var(--radius-md)",
        transition: "outline-color 0.1s",
      }}
    >
      <Show when={isDragOver()}>
        <div
          style={{
            position: "absolute",
            inset: "0",
            background: "rgba(0, 122, 255, 0.08)",
            "border-radius": "var(--radius-md)",
            display: "flex",
            "align-items": "center",
            "justify-content": "center",
            "pointer-events": "none",
            "z-index": 10,
          }}
        >
          <span
            style={{
              padding: "0.5rem 1rem",
              background: "var(--surface-elevated)",
              border: "1px solid var(--accent)",
              "border-radius": "var(--radius-md)",
              "font-weight": "600",
            }}
          >
            {t("chat.composer.drop_files")}
          </span>
        </div>
      </Show>
      {/* message-actions.md section 8 "Entering" — while selecting, the
          selection bar replaces the header: a leading Cancel, the count in the
          middle, nothing trailing (the trailing cell is an empty twin of the
          leading one, which is what keeps the count centred). */}
      <Show when={selecting()}>
        <header class="lk-chat-head lk-select-bar">
          <button type="button" class="ghost lk-select-cancel" onClick={leaveSelectionMode}>
            {t("common.cancel")}
          </button>
          <span class="lk-select-count" role="status" aria-live="polite">
            {selectionCountLabel(selection().size)}
          </span>
          <span class="lk-select-bar-trail" aria-hidden="true" />
        </header>
      </Show>
      <Show when={!selecting()}>
      <header class="lk-chat-head">
        <button
          type="button"
          class="lk-icon-button lk-narrow-only"
          aria-label={t("chat.header.back")}
          title={t("chat.header.back")}
          onClick={props.onLeave}
        >
          <Icon name="back" size={20} />
        </button>
        {/* `docs/ux/conversation-info.md` section 1 — the avatar and the name
            open this conversation's info, and are the only way in. A real
            <button> rather than a div with `role="button"`: Enter, Space and
            the accessible role come with the element instead of being
            re-implemented, and the label says what it opens. */}
        <button
          type="button"
          class="lk-chat-title"
          title={infoTargetLabel()}
          aria-label={infoTargetLabel()}
          // A dialog only where one opens: a contact's info is their own
          // page, and the button is then a plain link-like control.
          aria-haspopup={
            infoOpening(infoTarget(), personRecord()) === "layer" ? "dialog" : undefined
          }
          aria-expanded={infoOpen()}
          onClick={openInfo}
        >
          <Avatar label={resolvePeerName()} id={props.entry.groupIdB64} size="sm" />
          <div class="lk-chat-title-text">
            {/* ADR-0021 follow-up: show the linked contact's user-chosen
                name in the chat header (falls back to the chat label). */}
            <h1>{resolvePeerName()}</h1>
            <div class="lk-chat-sub" classList={{ typing: typingIndicator() !== null && !membershipEnded() }}>
              {headerSub()}
            </div>
          </div>
        </button>
        <div class="lk-chat-actions">
          {/* Phase 3a8 — call buttons. Visible whenever no call is active in
              this thread.

              ADR-0045 UX §6.2.8 item 4 — and GONE once this device's
              membership has ended. The per-call SFrame seed comes from
              `exportSecret("lakko/call/v1", …)`, which is gated on
              `is_active()` and now raises, so the button fails closed — which
              is correct, and is not enough. A button that always errors is the
              shape §6.1.1 rejects, and §6.2.11 forbids any live indicator on a
              dead group for a stronger reason: a call control asserts a live
              E2EE session, and there is not one. */}
          <Show when={callAffordance() !== "hidden"}>
            {/* A group call already in progress is JOINED, never shadowed by a
                second one. Otherwise: group chat (>2 members) -> SFU + SFrame,
                camera sealed like the audio track; 1×1 -> P2P DTLS-SRTP. */}
            <button
              type="button"
              class="lk-icon-button"
              title={callAffordance() === "join-group" ? t("chat.call.join") : t("chat.call.voice")}
              aria-label={callAffordance() === "join-group" ? t("chat.call.join") : t("chat.call.voice")}
              onClick={() => onCallButton(false)}
              disabled={busy()}
            >
              <Icon name="phone" size={20} />
            </button>
            <button
              type="button"
              class="lk-icon-button"
              title={callAffordance() === "join-group" ? t("chat.call.join_video") : t("chat.call.video")}
              aria-label={callAffordance() === "join-group" ? t("chat.call.join_video") : t("chat.call.video")}
              onClick={() => onCallButton(true)}
              disabled={busy()}
            >
              <Icon name="video" size={20} />
            </button>
          </Show>
          {/* Everything that is not a call lives in the overflow menu
              (docs/ux section 5.2). Receiving is automatic, so there is no
              Refresh: the receive loop below polls while the chat is open. */}
          <div class="lk-menu-anchor">
            {/* Shown only when it has something in it: with `Members` gone
                to the header, a plain member's menu would otherwise open
                empty. */}
            <Show when={headerMenuHasItems()}>
            <button
              type="button"
              class="lk-icon-button"
              title={t("common.more_options")}
              aria-label={t("common.more_options")}
              aria-haspopup="menu"
              aria-expanded={headerMenuOpen()}
              onClick={() => setHeaderMenuOpen(!headerMenuOpen())}
              disabled={busy()}
            >
              <Icon name="more" size={20} />
            </button>
            </Show>
            <Show when={headerMenuOpen()}>
              <div class="lk-menu-backdrop" onClick={() => setHeaderMenuOpen(false)} />
              <div class="lk-menu" role="menu">
                {/* `Members` used to live here as a second route to the same
                    screen. It is gone: the name at the top is the one way in
                    (`docs/ux/conversation-info.md` section 1).

                    Only in a GROUP. A one-to-one does not turn into a group
                    by growing: the maintainer's call on 2026-09-23, after
                    both natives lost the affordance and only web kept it.
                    Adding a third person starts a new group, which is the one
                    rule on all three clients rather than a capability one of
                    them happens to have (`conversation-info.md` section 2.1).

                    ADR-0022 — within a group, only owner/admin (or an
                    open/legacy group) sees it; a plain member's add would be
                    refused by peers. Adding is part of the group's info, so
                    this opens the info with the form ready rather than a form
                    of its own floating under the header. */}
                <Show when={isGroupChat() && canManageMembers()}>
                  <button
                    type="button"
                    role="menuitem"
                    onClick={() => {
                      setAddMemberOpen(true);
                      openInfo();
                    }}
                  >
                    <Icon name="plus" size={18} />
                    {t("chat.members.add.action")}
                  </button>
                </Show>
              </div>
            </Show>
          </div>
        </div>
      </header>
      </Show>

      {/* ADR-0052 decision 3 / message-actions.md section 4 — the pinned bar,
          under the header, one pin at a time. No pins, no bar: the header
          does not move. */}
      <Show when={pinEntries().length > 0}>
        <PinnedBar
          entries={pinEntries()}
          index={pinIndex()}
          onTap={onPinBarTap}
          onOpenList={() => setPinListOpen(true)}
        />
      </Show>
      <PinList
        open={pinListOpen()}
        entries={pinEntries()}
        onPick={(id) => {
          setPinListOpen(false);
          jumpToMessage(id);
        }}
        onClose={() => setPinListOpen(false)}
      />
      <PinSheet
        open={pinSheet() !== null}
        title={pinSheet()?.title ?? ""}
        options={pinSheet()?.options ?? []}
        onPick={(option) => {
          const sheet = pinSheet();
          if (sheet !== null) void runPinOption(sheet.messageId, option);
        }}
        onClose={() => setPinSheet(null)}
      />

      {/* `docs/ux/calls.md` section 2.1.1 — a call from a MUTED conversation is
          offered here, quietly, instead of taking the screen over. It is
          between the header and the timeline on purpose: in the column's flow,
          so nothing the person was reading is covered, which is what separates
          it from the group-call strip further down (that one is sticky over the
          timeline) and from the full-screen incoming call (a modal that covers
          everything and takes focus).

          An unmuted call never appears here — `quietIncomingCall` is null for
          one — so this is not a second incoming-call surface competing with
          §2.2's, it is the surface a muted call gets instead of it. */}
      <Show when={quietCall()}>
        {(offer) => (
          <QuietIncomingCallPanel
            callerName={resolvePeerName()}
            callerId={props.entry.groupIdB64}
            video={offer().video}
            onAccept={(withVideo) => void callHost.acceptIncomingCall(withVideo)}
            onDecline={() => void callHost.declineIncomingCall()}
          />
        )}
      </Show>

      <div
        class="lk-chat-scroll"
        ref={scrollEl}
        onScroll={() => {
          const el = scrollEl;
          if (el === undefined) return;
          stickToBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
        }}
      >
      {/* The in-call UI and the remote audio sinks of both call kinds are the
          call host's (`calls/CallHostContext.tsx`), rendered on every screen. */}

      {/* Group call in progress that this device is not in. Announced by
          the initiator's GroupCallStart and kept current by every
          participant's Join heartbeat / Leave (see calls/group-call-record.ts). */}
      <Show when={callAffordance() === "join-group"}>
        <div
          role="status"
          style={{
            position: "sticky",
            top: "0",
            "z-index": 10,
            margin: "0.5rem 0",
            padding: "0.75rem 1rem",
            background: "var(--accent)",
            color: "var(--accent-fg)",
            "border-radius": "var(--radius-md)",
            display: "flex",
            "align-items": "center",
            "justify-content": "space-between",
            gap: "0.75rem",
            "flex-wrap": "wrap",
          }}
        >
          <div>
            <strong>{t("chat.call.group_in_progress")}</strong>
            <div style={{ "font-size": "0.85rem", "margin-top": "0.2rem" }}>
              {(() => {
                const live = liveGroupCall();
                const n = live === null ? 0 : freshParticipantCount(live, groupCallClock(), groupCallViewer());
                const status =
                  n === 0 ? t("chat.call.group_starting") : t("chat.call.group_people", { count: n });
                return t("chat.call.group_status_e2ee", { status });
              })()}
            </div>
          </div>
          <div style={{ display: "flex", gap: "0.4rem" }}>
            <button
              type="button"
              disabled={busy()}
              onClick={() => {
                const live = liveGroupCall();
                if (live !== null) void requestGroupCall(live.callIdHex, false);
              }}
              style={{
                background: "var(--surface-elevated)",
                color: "var(--surface-fg)",
                border: "none",
                padding: "0.5rem 0.9rem",
                "border-radius": "var(--radius-sm)",
                "font-weight": "600",
                cursor: "pointer",
              }}
            >
              {t("chat.call.join_short")}
            </button>
            <button
              type="button"
              disabled={busy()}
              onClick={() => {
                const live = liveGroupCall();
                if (live !== null) void requestGroupCall(live.callIdHex, true);
              }}
              style={{
                background: "var(--surface-elevated)",
                color: "var(--surface-fg)",
                border: "none",
                padding: "0.5rem 0.9rem",
                "border-radius": "var(--radius-sm)",
                "font-weight": "600",
                cursor: "pointer",
              }}
            >
              {t("chat.call.join_video")}
            </button>
          </div>
        </div>
      </Show>

      {/* The ringing banner and the 1x1 in-call UI are the call host's
          (`calls/CallHostContext.tsx`), shown on every screen. What stays
          here is the one line about THIS chat's most recent 1x1 call. */}
      <Show
        when={
          !callHost.inCall() &&
          callHost.lastEnded()?.groupIdB64 === props.entry.groupIdB64
        }
      >
        <div
          style={{
            margin: "0.5rem 0",
            padding: "0.5rem 0.75rem",
            background: "var(--surface-elevated)",
            "border-radius": "var(--radius-sm)",
            "font-size": "0.85rem",
            color: "var(--surface-muted)",
          }}
        >
          {lastEndedLine(callHost.lastEnded()?.reason)}
        </div>
      </Show>


      {/* `docs/ux/conversation-info.md` — the info about THIS conversation,
          opened by the name at the top of it.

          Two things changed here. It is a LAYER over the conversation, which
          stays where it is behind the dim, rather than a section that pushed
          the messages sideways — a panel that shoves the timeline aside to
          show two rows reads as a glitch, which is why the click used to feel
          like nothing happened (section 3). And what is inside is decided by
          what the conversation IS (section 1): everything below is a GROUP's
          info, and a conversation between two people opens the person instead
          — so `Group name`, `Create invite link`, `Make admin`, `Transfer
          ownership` and `Leave group` can no longer appear where there is no
          group to name, no owner to transfer and nothing to leave that is not
          simply the other person. */}
      <ModalLayer
        open={infoOpen() && infoTarget() === "group"}
        title={infoTargetHeading()}
        onClose={closeInfo}
      >
        {/* The layer is the GROUP's idiom and nothing else's: a person's info
            is their own page (`openInfo` navigates there), so this never has
            to choose a panel for any other destination. A one-to-one showing
            the group panel is precisely the defect the routing was changed to
            fix, and a fallback here would reopen it for the first render. */}
        <Show when={infoTarget() === "group"}>
          {/* `docs/ux/conversation-info.md` section 4 — an action refused
              says so IN PLACE and the layer stays open. These two used to sit
              under the panel, which is now behind the dim and unreadable. */}
          <Show when={error()}>
            {(message) => (
              <div class="form-error">
                {message()}
              </div>
            )}
          </Show>
          <Show when={notice()}>{(message) => <div class="form-notice">{message()}</div>}</Show>
          {/* ADR-0059 section 6 — share the listed channels this person
              follows with this group. Not a block of the shared info rule
              (`lakko-ui-rules`): it is shown in every group this device is
              still a member of. */}
          <Show when={!membershipEnded()}>
            <ShareChannelsSwitch entry={props.entry} />
          </Show>
          {/* 1.18g — the add-member form, reached from `Add member` in the
              header's overflow menu, which opens this info with the form
              ready. Submit drives
              ContactsClient.addMemberToGroupByUsername, which fetches the
              recipient's KeyPackage, runs Group.addMember, broadcasts the
              commit to existing members via DS, and ships the Welcome to
              the new member's stranger inbox. */}
          <Show when={addMemberOpen() && groupSections().includes("add-member")}>
            <form
              onSubmit={(e) => {
                e.preventDefault();
                void onAddMember();
              }}
              class="lk-card"
              style={{ "flex-direction": "row", "align-items": "center", gap: "8px" }}
            >
              <input
                type="text"
                placeholder={t("chat.members.add.placeholder")}
                autocomplete="off"
                value={addMemberInput()}
                onInput={(e) => setAddMemberInput(e.currentTarget.value)}
                style={{ flex: 1 }}
                disabled={busy()}
                aria-invalid={addMemberCheck().kind === "invalid"}
              />
              <button
                class="primary"
                type="submit"
                disabled={busy() || !mayLookUp(addMemberCheck())}
              >
                {busy() ? t("common.adding") : t("chat.members.add.submit")}
              </button>
              <Show when={usernameReason(addMemberCheck())}>
                {(reason) => (
                  <span class="form-error" role="status" style={{ "align-self": "center" }}>
                    {reason()}
                  </span>
                )}
              </Show>
            </form>
          </Show>
          {/* ADR-0051 — the `Group name` row (UX spec §3.1.2 / §5). Visible to
              everyone in a group; editable by the owner and admins (read from
              the live group at Save), read-only for a plain member. An empty
              name clears it and the title falls back to the members' names. */}
          <section aria-label={GROUP_NAME_LABEL()} class="lk-card" style={{ gap: "8px" }}>
            <h2>{GROUP_NAME_LABEL()}</h2>
            <Show
              when={groupSections().includes("rename-group")}
              fallback={<span style={{ "font-size": "0.9rem" }}>{resolvePeerName()}</span>}
            >
              <form
                onSubmit={(e) => {
                  e.preventDefault();
                  void onSaveGroupName();
                }}
                style={{ display: "flex", "flex-direction": "row", "align-items": "center", gap: "8px" }}
              >
                <input
                  type="text"
                  aria-label={GROUP_NAME_LABEL()}
                  placeholder={resolvePeerName()}
                  autocomplete="off"
                  maxLength={64}
                  value={groupNameInput()}
                  onInput={(e) => setGroupNameInput(e.currentTarget.value)}
                  style={{ flex: 1 }}
                  disabled={groupNameBusy()}
                  aria-invalid={groupNameError() !== null}
                />
                <button
                  class="primary"
                  type="submit"
                  disabled={groupNameBusy() || groupNameUnchanged()}
                >
                  {groupNameBusy() ? t("common.saving") : GROUP_NAME_SAVE()}
                </button>
              </form>
            </Show>
            <Show when={groupNameError()}>
              {(msg) => (
                <span class="form-error" role="alert">
                  {msg()}
                </span>
              )}
            </Show>
          </section>
          <MembersCanFindMeSwitch kind="group" groupIdB64={props.entry.groupIdB64} />
          <section aria-label={t("chat.members.title")} class="lk-card" style={{ gap: "8px" }}>
            <h2>{t("chat.members.heading", { count: members().length })}</h2>
            {/* ADR-0045 UX §5.1 — the pending notice. TOP of the panel, above
                everything, and rendered only once at least one request is past
                the quiet window: the owner is rank 0 and sweeps its own request
                immediately, so in the common case nothing ever renders here. A
                spinner that appears and vanishes in 300 ms teaches the user that
                membership is slow when it is not.

                Announced POLITELY (`aria-live="polite"`): nothing about it is
                urgent and it can change while the roster is being read. It is
                carried by TEXT, never by hue or a lone icon. */}
            <Show when={pendingRequestNotice() !== null}>
              <div
                role="status"
                aria-live="polite"
                style={{
                  margin: "0 0 0.6rem 0",
                  padding: "0.4rem 0.5rem",
                  border: "1px solid var(--surface-border)",
                  "border-radius": "var(--radius-sm)",
                  background: "var(--surface-bg)",
                  display: "flex",
                  "flex-direction": "column",
                  gap: "0.2rem",
                }}
              >
                <span style={{ "font-size": "0.8rem", color: "var(--surface-fg)" }}>
                  {pendingRequestNotice()?.count}
                </span>
                <span style={{ "font-size": "0.75rem", color: "var(--surface-muted)" }}>
                  {pendingRequestNotice()?.liveness}
                </span>
              </div>
            </Show>
            {/* ADR-0045 UX §6.1.9 — the new-owner notice. Rendered once,
                dismissible, at panel level, announced POLITELY.

                It is a client-side INFERENCE from an observed role-table change,
                not a message: nothing on the wire announces a transfer. So it is
                best-effort and fails open — a device that first observes the table
                already naming it owner says nothing — and it must never be the
                only way a user learns their role. The panel's affordances stay the
                primary signal, exactly as they are for admin today.

                Placed directly BELOW the pending notice so §5's "pending notice,
                top of the panel, above everything" ordering stays literally
                intact; the two are almost never present at the same moment. */}
            <Show when={newOwnerBanner() !== null}>
              <div
                role="status"
                aria-live="polite"
                style={{
                  margin: "0 0 0.6rem 0",
                  padding: "0.4rem 0.5rem",
                  border: "1px solid var(--surface-border)",
                  "border-radius": "var(--radius-sm)",
                  background: "var(--surface-bg)",
                  display: "flex",
                  "flex-direction": "column",
                  gap: "0.2rem",
                }}
              >
                <span style={{ "font-size": "0.8rem", color: "var(--surface-fg)" }}>
                  {newOwnerBanner()?.headline}
                </span>
                {/* The second line names the exit deliberately: it is the one
                    sentence that stops the new owner from arriving where the old
                    one was. */}
                <span style={{ "font-size": "0.75rem", color: "var(--surface-muted)" }}>
                  {newOwnerBanner()?.body}
                </span>
                <button
                  type="button"
                  onClick={() => setNewOwnerBanner(null)}
                  style={{
                    "font-size": "0.75rem",
                    padding: "0.2rem 0.5rem",
                    "border-radius": "var(--radius-sm)",
                    border: "1px solid var(--surface-border)",
                    background: "transparent",
                    color: "var(--surface-fg)",
                    cursor: "pointer",
                    "align-self": "flex-start",
                  }}
                >
                  {NEW_OWNER_DISMISS()}
                </button>
              </div>
            </Show>
            {/* ADR-0028 (G-3) — owner/admin "create invite link" affordance.
                Mints a /g/<slug> link conveying this group's id; a non-member
                opens it to request to join, approved via the pending list above.
                Same role gate as add/remove. */}
            <Show when={groupSections().includes("invite-link")}>
              <div
                style={{
                  margin: "0 0 0.6rem 0",
                  display: "flex",
                  "flex-direction": "column",
                  gap: "0.4rem",
                }}
              >
                <button
                  type="button"
                  disabled={busy()}
                  onClick={() => void onCreateInviteLink()}
                  style={{
                    "font-size": "0.8rem",
                    padding: "0.3rem 0.6rem",
                    "border-radius": "var(--radius-sm)",
                    border: "1px solid var(--surface-border)",
                    background: "var(--surface-bg)",
                    color: "var(--surface-fg)",
                    cursor: "pointer",
                    "align-self": "flex-start",
                  }}
                >
                  {t("chat.invite_link.create")}
                </button>
                <Show when={inviteLink()}>
                  {(l) => (
                    <ShareLinkRow
                      url={l()}
                      onRevoke={() => void onRevokeInviteLink()}
                      busy={busy()}
                    />
                  )}
                </Show>
              </div>
            </Show>
            {/* ADR-0027 (G-3) — pending external-join requests. Owner / admin
                only (same gate as add / remove). Each row offers Approve (ADR-0045
                S7: publishes ONE `Add` proposal; the joiner is admitted when a
                committer sweeps it) or Reject (drops the retained request
                locally). A request whose joiner has since landed disappears on
                its own — the core filters it out of `pendingExternalJoins()` on
                every read — so this list retires its own rows with no message
                saying so. Hidden when empty. */}
            <Show
              when={canManageMembers() && (pendingJoins().length > 0 || unkeptJoinRequest())}
            >
              <div
                style={{
                  margin: "0 0 0.6rem 0",
                  padding: "0.4rem 0.5rem",
                  border: "1px dashed var(--surface-border)",
                  "border-radius": "var(--radius-sm)",
                  background: "var(--surface-bg)",
                }}
              >
                <h3
                  style={{
                    "font-size": "0.78rem",
                    "text-transform": "uppercase",
                    "letter-spacing": "0.04em",
                    color: "var(--surface-muted)",
                    margin: "0 0 0.2rem 0",
                  }}
                >
                  {t("chat.members.join.heading", { count: pendingJoins().length })}
                </h3>
                {/* ADR-0027 / §7.1 — a request the quarantine turned away. It
                    is in no list below, because the core never kept it, and it
                    gets no Approve for the same reason: there is nothing to
                    approve. Informational only, which is the most
                    `externalJoinAffordance` allows for an unretained
                    request. */}
                <Show when={unkeptJoinRequest()}>
                  <p
                    style={{
                      "font-size": "0.7rem",
                      color: "var(--surface-muted)",
                      margin: "0 0 0.4rem 0",
                    }}
                  >
                    {JOIN_REQUEST_NOT_KEPT()}
                  </p>
                </Show>
                <ul
                  style={{
                    "list-style": "none",
                    padding: 0,
                    margin: 0,
                    display: "flex",
                    "flex-direction": "column",
                    gap: "0.3rem",
                  }}
                >
                  <For each={pendingJoins()}>
                    {(ref) => (
                      <li
                        style={{
                          display: "flex",
                          "align-items": "center",
                          "justify-content": "space-between",
                          gap: "0.5rem",
                        }}
                      >
                        <div
                          style={{
                            display: "flex",
                            "flex-direction": "column",
                            gap: "0.1rem",
                            overflow: "hidden",
                          }}
                        >
                          <span class="lk-ellipsis" style={{ "font-size": "0.85rem" }}>
                            {/* Resolved through the joiner's ACCOUNT key: the
                                request is keyed on its device leaf, contacts on
                                the account (ADR-0050). */}
                            {pendingJoinLabel(props.entry.group, ref, base64Encode, (k) =>
                              profiles.resolve(k),
                            )}
                          </span>
                          {(() => {
                            const attr = joinAttributions().get(base64Encode(ref));
                            if (!attr) {
                              return (
                                <span
                                  style={{
                                    "font-size": "0.7rem",
                                    color: "var(--surface-muted)",
                                  }}
                                >
                                  {t("chat.members.join.link_unknown")}
                                </span>
                              );
                            }
                            return (
                              <span
                                style={{
                                  "font-size": "0.7rem",
                                  color: attr.issuerInRoster
                                    ? "var(--surface-muted)"
                                    : "var(--danger, #c0392b)",
                                }}
                              >
                                {attr.issuerInRoster
                                  ? t("chat.members.join.via_member", { name: memberName(attr.issuerKeyB64) })
                                  : t("chat.members.join.via_former", { name: memberName(attr.issuerKeyB64) })}
                              </span>
                            );
                          })()}
                        </div>
                        <span style={{ display: "flex", gap: "0.3rem" }}>
                          <button
                            type="button"
                            disabled={busy()}
                            onClick={() => void onApproveJoin(ref)}
                            style={{
                              "font-size": "0.78rem",
                              padding: "0.2rem 0.5rem",
                              "border-radius": "var(--radius-sm)",
                              border: "1px solid var(--accent)",
                              background: "var(--accent)",
                              color: "var(--accent-fg)",
                              cursor: "pointer",
                            }}
                          >
                            {t("chat.members.join.approve")}
                          </button>
                          <button
                            type="button"
                            disabled={busy()}
                            onClick={() => void onRejectJoin(ref)}
                            style={{
                              "font-size": "0.78rem",
                              padding: "0.2rem 0.5rem",
                              "border-radius": "var(--radius-sm)",
                              border: "1px solid var(--surface-border)",
                              background: "transparent",
                              color: "var(--surface-fg)",
                              cursor: "pointer",
                            }}
                          >
                            {t("chat.members.join.reject")}
                          </button>
                        </span>
                      </li>
                    )}
                  </For>
                </ul>
              </div>
            </Show>
            {/* ADR-0045 UX §5 — `Waiting to be added ({n})`. A separate section,
                not a roster row, and that is the load-bearing half of the
                no-optimistic-update rule in reverse: a person who has been
                REQUESTED is not a member, so putting them in the members list
                would assert a membership that does not exist. Same identity
                treatment the join-request rows use (short fingerprint). */}
            <Show when={pendingAdds().length > 0}>
              <div
                style={{
                  margin: "0 0 0.6rem 0",
                  padding: "0.4rem 0.5rem",
                  border: "1px dashed var(--surface-border)",
                  "border-radius": "var(--radius-sm)",
                  background: "var(--surface-bg)",
                }}
              >
                <h3
                  style={{
                    "font-size": "0.78rem",
                    "text-transform": "uppercase",
                    "letter-spacing": "0.04em",
                    color: "var(--surface-muted)",
                    margin: "0 0 0.4rem 0",
                  }}
                >
                  {t("chat.members.pending.adds_heading", { count: pendingAdds().length })}
                </h3>
                <ul
                  style={{
                    "list-style": "none",
                    padding: 0,
                    margin: 0,
                    display: "flex",
                    "flex-direction": "column",
                    gap: "0.3rem",
                  }}
                >
                  <For each={pendingAdds()}>
                    {(req) => (
                      <li
                        style={{
                          display: "flex",
                          "flex-direction": "column",
                          gap: "0.1rem",
                        }}
                      >
                        <span style={{ "font-size": "0.85rem" }}>{labelForAddRequest(req)}</span>
                        <span
                          style={{
                            "font-size": "0.7rem",
                            color: "var(--surface-muted)",
                          }}
                        >
                          {t("chat.members.pending.waiting_added")}
                        </span>
                      </li>
                    )}
                  </For>
                </ul>
              </div>
            </Show>
            {/* ADR-0045 S7 residual 1 — `Not applied ({n})`.

                A sweep does not necessarily carry everything it finds, and an
                excluded request used to vanish without a trace: OpenMLS empties
                the proposal store on every merged commit whether or not the
                commit carried the proposal, so the pending set that was the
                request's only representation is cleared either way. For an
                approved join it is worse, because a successful approve CONSUMED
                the retained request — the admin cannot even re-approve.

                This section says the one thing that is knowable, and no more:
                the request did not apply. The core reports no reason for an
                exclusion (`CreateCommitError` names no proposal), so neither
                does the copy. Sits directly under `Waiting to be added` because
                it is where those rows go when they stop waiting. */}
            <Show when={unappliedAdds().length > 0}>
              <div
                style={{
                  margin: "0 0 0.6rem 0",
                  padding: "0.4rem 0.5rem",
                  border: "1px dashed var(--surface-border)",
                  "border-radius": "var(--radius-sm)",
                  background: "var(--surface-bg)",
                }}
              >
                <h3
                  style={{
                    "font-size": "0.78rem",
                    "text-transform": "uppercase",
                    "letter-spacing": "0.04em",
                    color: "var(--surface-muted)",
                    margin: "0 0 0.2rem 0",
                  }}
                >
                  {unappliedHeading(unappliedAdds().length)}
                </h3>
                <p
                  style={{
                    "font-size": "0.7rem",
                    color: "var(--surface-muted)",
                    margin: "0 0 0.4rem 0",
                  }}
                >
                  {UNAPPLIED_EXPLANATION()}
                </p>
                <ul
                  style={{
                    "list-style": "none",
                    padding: 0,
                    margin: 0,
                    display: "flex",
                    "flex-direction": "column",
                    gap: "0.3rem",
                  }}
                >
                  <For each={unappliedAdds()}>
                    {(item) => (
                      <li
                        style={{
                          display: "flex",
                          "align-items": "flex-start",
                          "justify-content": "space-between",
                          gap: "0.5rem",
                        }}
                      >
                        <span
                          style={{ display: "flex", "flex-direction": "column", gap: "0.1rem" }}
                        >
                          <span style={{ "font-size": "0.85rem" }}>
                            {labelForSignatureKey(item.subjectKeyB64)}
                          </span>
                          <span
                            style={{ "font-size": "0.7rem", color: "var(--surface-muted)" }}
                          >
                            {unappliedLine(item)}
                          </span>
                        </span>
                        <button
                          type="button"
                          onClick={() => onDismissUnapplied(item.key)}
                          style={{ "font-size": "0.7rem" }}
                        >
                          {t("common.dismiss")}
                        </button>
                      </li>
                    )}
                  </For>
                </ul>
              </div>
            </Show>
            {/* ADR-0045 UX §6.2.7 — the roster stays, READ-ONLY, and carries one
                line saying so.

                It stays because it is the last thing you knew about who you were
                in a group with, it is genuinely informative next to a frozen
                transcript, and it is still readable: `members()` is a plain read
                of the public group with no state gate.

                The line is a statement about the LIST, not about any row in it,
                and that is load-bearing rather than stylistic. A `Remove`
                rewrites no `GroupContext`, so the role table is untouched and a
                removed admin's frozen roster still names them an admin — nothing
                here may present that as a live role. "Stopped updating" is the
                literal truth (the local view is frozen at the removing epoch and
                never moves again) rather than a euphemism for it, and it is
                deliberately neutral between having left and having been
                removed. */}
            <Show when={membershipEnded()}>
              <p
                style={{
                  margin: "0 0 0.4rem 0",
                  "font-size": "0.75rem",
                  color: "var(--surface-muted)",
                }}
              >
                {FROZEN_ROSTER_HEADER()}
              </p>
            </Show>
            <ul
              style={{
                "list-style": "none",
                padding: 0,
                margin: 0,
                display: "flex",
                "flex-direction": "column",
                gap: "0.25rem",
              }}
            >
              <For each={members()}>
                {(m) => {
                  const isSelf = m.leafIndex === props.entry.group.ownLeafIndex;
                  // 1.18i — render the accepted-contact's
                  // displayName when this leaf's pubkey is in our
                  // local cache. Falls back to a hex fingerprint
                  // (a recipient we've not formally accepted yet,
                  // or somebody else who joined a group we're in).
                  // Lookup key matches `local-store.ts`'s
                  // `signaturePubkeyB64` shape (standard base64
                  // of the 32-byte Ed25519 verifying key).
                  const memberKeyB64 = base64Encode(m.sigPubkey);
                  // ADR-0045 — NO optimistic update. A member with a pending
                  // removal stays in the roster, annotated, because they are
                  // still cryptographically in the group and still receiving
                  // every message until the commit lands. Dropping the row would
                  // be the membership equivalent of a green E2EE pill over an
                  // unencrypted call.
                  const annotation = (): string | null =>
                    rowAnnotation(pendingRequests(), m.leafIndex, ownLeaf());
                  const removePending = (): boolean =>
                    rowAnnotation(pendingRequests(), m.leafIndex, ownLeaf()) !== null;
                  const label = (): string => memberListLabel(memberKeyB64);
                  return (
                    <li
                      // The annotation is part of the row's ACCESSIBLE LABEL, not
                      // a separate focus stop — a screen-reader user hears it as
                      // one thing, and hears WHY the Remove button below is
                      // disabled rather than losing the control.
                      aria-label={
                        annotation() === null
                          ? label()
                          : t("chat.members.row_label", { name: label(), state: annotation() ?? "" })
                      }
                      class="lk-row"
                      style={{ padding: "8px 10px", gap: "10px" }}
                    >
                      <Avatar label={label()} id={memberKeyB64} size="sm" />
                      <span
                        style={{
                          display: "flex",
                          "flex-direction": "column",
                          gap: "0.1rem",
                        }}
                      >
                        <span class="lk-row-title">{label()}</span>
                        <Show when={m.deviceCount > 1}>
                          <span class="lk-row-sub">{t("chat.members.devices", { count: m.deviceCount })}</span>
                        </Show>
                        <Show when={annotation() !== null}>
                          <span
                            aria-hidden="true"
                            style={{
                              "font-size": "0.7rem",
                              color: "var(--surface-muted)",
                              display: "inline-flex",
                              "align-items": "center",
                              gap: "0.25rem",
                            }}
                          >
                            {annotation()}
                          </span>
                        </Show>
                      </span>
                      <span style={{ flex: 1 }} />
                      <Show
                        when={!isSelf}
                        fallback={
                          <span
                            style={{
                              "font-size": "0.8rem",
                              color: "var(--surface-muted)",
                            }}
                          >
                            {t("chat.author.you")}
                          </span>
                        }
                      >
                        {/* ADR-0022 — only the owner promotes / demotes admins;
                            never the owner's own row. */}
                        <Show when={isOwner() && ownerPubkeyHex() !== hexOf(m.sigPubkey)}>
                          <button
                            type="button"
                            onClick={() => {
                              void onToggleAdmin(
                                m,
                                !adminPubkeys().has(hexOf(m.sigPubkey)),
                              );
                            }}
                            class="ghost small"
                            disabled={busy()}
                          >
                            {adminPubkeys().has(hexOf(m.sigPubkey))
                              ? t("chat.members.admin.remove")
                              : t("chat.members.admin.make")}
                          </button>
                        </Show>
                        {/* ADR-0022 — only owner/admin (or an open/legacy
                            group) sees Remove; a member's removal is refused.
                            ADR-0045 UX §5.3 — while a removal is pending the
                            button stays VISIBLE but disabled: hiding it would
                            read as the permission having vanished, and the row
                            annotation is what explains the disabled state. */}
                        <Show when={canManageMembers()}>
                          <button
                            type="button"
                            onClick={() => {
                              void onRemoveMember(m.leafIndex);
                            }}
                            class="ghost small"
                            disabled={busy() || removePending()}
                            style={{ color: "var(--danger)" }}
                          >
                            {t("common.remove")}
                          </button>
                        </Show>
                      </Show>
                    </li>
                  );
                }}
              </For>
            </ul>
            {/* ADR-0045 UX §5 item 6 / §6.1.1 — THE EXIT BLOCK, bottom of the
                panel. Exactly one of its two controls is ever live for a given
                reader, because they are role-disjoint: the owner sees
                `Transfer ownership` above a DISABLED `Leave group` carrying its
                precondition, everyone else sees `Leave group` alone.

                The owner's `Leave group` used to be HIDDEN, and this reverses
                that — not because the old argument (a control that always refuses
                is worse than no control) was wrong, but because it was right only
                while transfer was unreachable. A disabled control whose reason
                names the action directly above it is a signposted route, and it
                is the same visible-but-disabled pattern §5.3 already uses for a
                pending row's `Remove`. Hiding it now would leave an owner looking
                for the exit finding nothing at all about leaving.

                Only on the proposal path: a legacy group's core has no
                self-remove route and no owner to transfer from. */}
            <Show when={exitBlockState().kind !== "hidden"}>
              <div
                style={{
                  "margin-top": "0.6rem",
                  display: "flex",
                  "flex-direction": "column",
                  gap: "0.35rem",
                }}
              >
                {/* ADR-0045 UX §6.2.4 / §6.2.7 — once the membership has ended,
                    both of the other controls are gone (one names a group the
                    reader is not in, the other is owner-only and they are not)
                    and the block holds `Delete this chat` instead.

                    This slot is the right home by §6.1.1's own argument: the
                    block's meaning is *the exit*, and once you are out the only
                    exit-shaped action left is closing the door behind you. A
                    permanently dead row with no action available is its own
                    defect — everything else about this state is "you cannot",
                    and this is the one thing they still can.

                    It is also the ONE control in this whole design that destroys
                    user data on request. Everything else in §6.2 removes a claim
                    the UI should not have been making; this removes a transcript
                    that exists nowhere else, on one confirmation, with no undo
                    and no export. The confirmation says so in as many words, and
                    that is the only protection there is. */}
                <Show when={exitBlockState().kind === "ended"}>
                  <button
                    type="button"
                    disabled={busy()}
                    onClick={() => void onDeleteChat()}
                    style={{
                      "font-size": "0.8rem",
                      padding: "0.3rem 0.6rem",
                      "border-radius": "var(--radius-sm)",
                      border: "1px solid var(--surface-border)",
                      background: "transparent",
                      color: "var(--danger, #c0392b)",
                      cursor: "pointer",
                      "align-self": "flex-start",
                    }}
                  >
                    {DELETE_CHAT_CONTROL()}
                  </button>
                </Show>
                <Show when={ownerExit()}>
                  {(owner) => (
                    <>
                      {/* NOT destructive-styled at rest. Red would file it with
                        `Remove` and `Leave group`, and an owner scanning for a
                        way out should not have to read a warning colour to
                        find one. Transfer is not a destruction; it is a
                        handover that happens to be final, and finality is a
                        property of the confirmation below, not of the button
                        that opens it. */}
                      <button
                        type="button"
                        disabled={busy() || !owner().transfer.enabled}
                        title={owner().transfer.disabledReason ?? undefined}
                        onClick={openTransferStep}
                        style={{
                          "font-size": "0.8rem",
                          padding: "0.3rem 0.6rem",
                          "border-radius": "var(--radius-sm)",
                          border: "1px solid var(--surface-border)",
                          background: "var(--surface-bg)",
                          color: "var(--surface-fg)",
                          cursor: "pointer",
                          "align-self": "flex-start",
                        }}
                      >
                        {t("chat.transfer.action")}
                      </button>
                      <Show when={owner().transfer.disabledReason !== null}>
                        <span
                          style={{
                            "font-size": "0.7rem",
                            color: "var(--surface-muted)",
                          }}
                        >
                          {owner().transfer.disabledReason}
                        </span>
                      </Show>
                      <button
                        type="button"
                        disabled
                        style={{
                          "font-size": "0.8rem",
                          padding: "0.3rem 0.6rem",
                          "border-radius": "var(--radius-sm)",
                          border: "1px solid var(--surface-border)",
                          background: "transparent",
                          color: "var(--surface-muted)",
                          "align-self": "flex-start",
                        }}
                      >
                        {t("chat.leave.action")}
                      </button>
                      {/* The reason is part of the control's ACCESSIBLE
                          description, not a decoration: a reader who cannot see
                          the greyed button must still hear why. */}
                      <span
                        style={{
                          "font-size": "0.7rem",
                          color: "var(--surface-muted)",
                        }}
                      >
                        {owner().leave.disabledReason}
                      </span>
                    </>
                  )}
                </Show>
                <Show when={memberExit()}>
                  {(member) => (
                    <>
                      <button
                        type="button"
                        disabled={busy() || !member().leave.enabled}
                        onClick={() => void onLeaveGroup()}
                        style={{
                          "font-size": "0.8rem",
                          padding: "0.3rem 0.6rem",
                          "border-radius": "var(--radius-sm)",
                          border: "1px solid var(--surface-border)",
                          background: "transparent",
                          color: "var(--danger, #c0392b)",
                          cursor: "pointer",
                          "align-self": "flex-start",
                        }}
                      >
                        {t("chat.leave.action")}
                      </button>
                      <Show when={member().leave.disabledReason !== null}>
                        <span
                          style={{
                            "font-size": "0.7rem",
                            color: "var(--surface-muted)",
                          }}
                        >
                          {member().leave.disabledReason}
                        </span>
                      </Show>
                    </>
                  )}
                </Show>
              </div>
            </Show>

            {/* ADR-0045 UX §6.1.4 / §6.1.5 — the transfer step.

                There is NO second dialog behind this, and that is the design
                rather than a shortcut: this step IS the confirmation, which
                happens to require two choices to be well-formed. Stacking an
                `Are you sure?` on a modal that already asked two questions buys
                friction of the kind that gets dismissed reflexively. What
                replaces it is friction that cannot be discharged without reading
                — the action names the person, is disabled until one is picked,
                and uses the picked row's label verbatim.

                Ordering inside the step is load-bearing: the irreversibility
                statement sits ABOVE the role choice. Below it, the pre-selected
                option would read as the safe one and the warning would read as
                being about the other one. */}
            <Show when={transferOpen()}>
              <div
                role="group"
                aria-label={t("chat.transfer.action")}
                style={{
                  "margin-top": "0.6rem",
                  padding: "0.5rem 0.6rem",
                  border: "1px solid var(--surface-border)",
                  "border-radius": "var(--radius-sm)",
                  background: "var(--surface-bg)",
                  display: "flex",
                  "flex-direction": "column",
                  gap: "0.5rem",
                }}
              >
                <div style={{ display: "flex", "flex-direction": "column", gap: "0.2rem" }}>
                  <strong style={{ "font-size": "0.82rem", color: "var(--surface-fg)" }}>
                    {TRANSFER_IRREVERSIBLE_HEADLINE()}
                  </strong>
                  <span style={{ "font-size": "0.75rem", color: "var(--surface-muted)" }}>
                    {TRANSFER_IRREVERSIBLE_BODY()}
                  </span>
                </div>

                {/* §6.1.3 — every member except the owner, in the roster's own
                    order, with a muted `Admin` MARKER rather than a filter. An
                    admins-only picker is EMPTY in the commonest small group
                    (owner plus one member, no admins ever appointed) — the exact
                    dead end this feature exists to close, reappearing as a UI
                    rule. The selection value is the 32-byte signature key. */}
                <fieldset
                  style={{
                    border: "none",
                    margin: 0,
                    padding: 0,
                    display: "flex",
                    "flex-direction": "column",
                    gap: "0.2rem",
                  }}
                >
                  <legend
                    style={{
                      "font-size": "0.7rem",
                      "text-transform": "uppercase",
                      "letter-spacing": "0.04em",
                      color: "var(--surface-muted)",
                      padding: 0,
                    }}
                  >
                    {t("chat.transfer.new_owner_legend")}
                  </legend>
                  <For each={transferPickerRows()}>
                    {(row) => (
                      <label
                        style={{
                          display: "flex",
                          "align-items": "center",
                          gap: "0.4rem",
                          "font-size": "0.8rem",
                        }}
                      >
                        {/* UX §13 — a picker row announces the roster label plus
                            `, admin` where the marker shows. The explicit
                            `aria-label` is what keeps that a comma rather than the
                            two spans running together. */}
                        <input
                          type="radio"
                          name="lakko-transfer-successor"
                          aria-label={row.isAdmin ? t("chat.transfer.candidate_admin", { name: row.label }) : row.label}
                          value={row.signatureKeyB64}
                          checked={transferPick() === row.signatureKeyB64}
                          onChange={() => setTransferPick(row.signatureKeyB64)}
                        />
                        <span aria-hidden="true">{row.label}</span>
                        {/* MARK, do not filter and do not sort. Who is already an
                            admin is information the owner assigned and is worth
                            having when choosing a successor; it is not a reason to
                            hide anyone, and hoisting admins to the top would be a
                            soft version of the filter §6.1.3 rejects. */}
                        <Show when={row.isAdmin}>
                          <span
                            aria-hidden="true"
                            style={{
                              "font-size": "0.7rem",
                              color: "var(--surface-muted)",
                            }}
                          >
                            {t("chat.members.admin.marker")}
                          </span>
                        </Show>
                      </label>
                    )}
                  </For>
                </fieldset>

                {/* §6.1.4 — a real decision, pre-selected but never defaulted. A
                    single-select control, NEVER a checkbox: a checkbox has an
                    unstated default state and this has none. The core refuses to
                    default it because either silent choice is wrong in one
                    direction, and the UI must not undo with a layout choice what
                    the core protected with a type. */}
                <fieldset
                  style={{
                    border: "none",
                    margin: 0,
                    padding: 0,
                    display: "flex",
                    "flex-direction": "column",
                    gap: "0.3rem",
                  }}
                >
                  <legend
                    style={{
                      "font-size": "0.7rem",
                      "text-transform": "uppercase",
                      "letter-spacing": "0.04em",
                      color: "var(--surface-muted)",
                      padding: 0,
                    }}
                  >
                    {t("chat.transfer.role_legend")}
                  </legend>
                  <For each={OUTGOING_OWNER_OPTIONS}>
                    {(opt) => (
                      <label
                        style={{
                          display: "flex",
                          "align-items": "flex-start",
                          gap: "0.4rem",
                          "font-size": "0.8rem",
                        }}
                      >
                        <input
                          type="radio"
                          name="lakko-transfer-outgoing"
                          value={opt.value}
                          checked={transferOutgoing() === opt.value}
                          onChange={() => setTransferOutgoing(opt.value)}
                        />
                        <span
                          style={{
                            display: "flex",
                            "flex-direction": "column",
                            gap: "0.1rem",
                          }}
                        >
                          <span>{opt.label}</span>
                          <span
                            style={{
                              "font-size": "0.7rem",
                              color: "var(--surface-muted)",
                            }}
                          >
                            {opt.consequence}
                          </span>
                        </span>
                      </label>
                    )}
                  </For>
                </fieldset>

                {/* Note APPLIED, not NOW: the transfer is a proposal, and
                    asserting the exit is open before the commit lands would be
                    the optimistic update §3 forbids. */}
                <span style={{ "font-size": "0.72rem", color: "var(--surface-muted)" }}>
                  {TRANSFER_UNLOCKS_LEAVE()}
                </span>

                <div style={{ display: "flex", gap: "0.4rem" }}>
                  <button
                    type="button"
                    disabled={busy() || !transferAction().enabled}
                    onClick={() => void onTransferOwnership()}
                    style={{
                      "font-size": "0.8rem",
                      padding: "0.3rem 0.6rem",
                      "border-radius": "var(--radius-sm)",
                      border: "1px solid var(--danger, #c0392b)",
                      background: "transparent",
                      color: "var(--danger, #c0392b)",
                      cursor: "pointer",
                    }}
                  >
                    {transferAction().label}
                  </button>
                  {/* `Cancel` is a PRE-PUBLISH exit, and it is the last one.
                      Nothing here is a withdraw: once the proposal is published it
                      is in every member's store and dies only when some commit
                      lands, so no client may offer a cancel afterwards —
                      including, and especially, for a transfer. */}
                  <button
                    type="button"
                    disabled={busy()}
                    onClick={() => setTransferOpen(false)}
                    style={{
                      "font-size": "0.8rem",
                      padding: "0.3rem 0.6rem",
                      "border-radius": "var(--radius-sm)",
                      border: "1px solid var(--surface-border)",
                      background: "transparent",
                      color: "var(--surface-fg)",
                      cursor: "pointer",
                    }}
                  >
                    {t("common.cancel")}
                  </button>
                </div>
              </div>
            </Show>
          </section>
        </Show>
      </ModalLayer>

      {/* ADR-0052 decision 2 / section 5.1 — the "Forward to" picker, open
          while a message id is armed. Picking sends at once and navigates;
          Back, Esc and the backdrop send nothing. */}
      <ForwardPicker
        open={forwardingId() !== null || bulkForwardOpen()}
        onClose={() => {
          setForwardingId(null);
          // Section 8: Cancel in the picker "returns to the selection as it
          // was" - the mode and the set are untouched.
          setBulkForwardOpen(false);
        }}
        onPick={(row) => (bulkForwardOpen() ? forwardSelectionTo(row) : forwardTo(row))}
      />

      {/* The layer carries its own copy of both while it is open, so an
          action refused there is readable rather than dimmed out behind it. */}
      <Show when={!infoOpen()}>
        {error() !== null && (
          <div class="form-error">
            {error()}
          </div>
        )}
        {notice() !== null && <div class="form-notice">{notice()}</div>}
      </Show>

      <Show
        when={timelineEntries().length > 0}
        fallback={
          <p
            style={{
              color: "var(--surface-muted)",
              padding: "1rem 0",
            }}
          >
            {/* ADR-0045 UX §6.2 — the empty-timeline copy invites a send, and
                once the membership has ended that is an invitation the
                composer below can no longer accept. The terminal row that
                follows carries the whole of what there is to say. */}
            <Show when={!membershipEnded()}>
              {t("chat.timeline.empty")}
            </Show>
          </p>
        }
      >
        <ol class="lk-timeline" classList={{ selecting: selecting() }}>
          <For each={timelineEntries()}>
            {(entry, i) => {
              const place = (): TimelineLayout =>
                timelineLayout()[i()] ?? { dayLabel: null, startsRun: true, endsRun: true };
              return (
              <>
              <Show when={place().dayLabel}>
                {(label) => (
                  <li class="lk-day-sep" aria-label={label()}>
                    <span>{label()}</span>
                  </li>
                )}
              </Show>
              <Show when={entry.kind === "call" ? entry.call : undefined}>
                {(call) => (
                  <CallTimelineRow
                    entry={call()}
                    canCallBack={callAffordance() !== "hidden" && !callHost.anyCallInProgress()}
                    onCallBack={(e) => onCallButton(callBackKind(e) === "video")}
                  />
                )}
              </Show>
              <Show when={entry.kind === "message" ? entry.row : undefined}>
                {(row) => (
              <TimelineBubble
                row={row()}
                isGroup={isGroupChat()}
                startsRun={place().startsRun}
                endsRun={place().endsRun}
                onDownload={onDownloadAttachment}
                onPreview={onPreviewAttachment}
                onHidePreview={onHideAttachmentPreview}
                getCachedUrl={getCachedAttachmentUrl}
                ownIdentity={ownSignatureKeyB64()}
                opState={(id) => msgOps().get(id) ?? historyOps().get(id)}
                readByOthers={(id) =>
                  isReadByEveryOther(readWatermarks(), id, ownSignatureKeyB64())
                }
                readersOf={(id) => {
                  const own = ownSignatureKeyB64();
                  const out: MemberIdentity[] = [];
                  for (const [member, upTo] of readWatermarks()) {
                    if (member !== own && upTo >= id) out.push(member);
                  }
                  return out;
                }}
                editingId={editingId()}
                editText={editText()}
                setEditText={setEditText}
                onStartEdit={(id, current) => {
                  // ADR-0052 — replying and editing are exclusive: starting
                  // an edit drops an armed reply.
                  setReplyingTo(null);
                  setEditText(current);
                  setEditingId(id);
                }}
                onCancelEdit={() => setEditingId(null)}
                onReply={onStartReply}
                onJumpTo={jumpToMessage}
                onForward={(id) => setForwardingId(id)}
                pinLabelOf={pinLabelOf}
                onPin={onPinRow}
                onOpenForwardOrigin={onOpenForwardOrigin}
                onSaveEdit={onSaveEdit}
                onDeleteMessage={onDeleteMessage}
                onReact={onReact}
                // ADR-0073 R.5 — a row of an older group of the conversation
                // is read-only: no op can name it in this group's journal.
                actionsAvailable={messageActionsAvailable() && !isHistoryRowId(rowMessageId(row()))}
                // ...except that a pin of an older segment can be removed.
                unpinOnly={((): boolean => {
                  const id = rowMessageId(row());
                  return (
                    id !== undefined &&
                    historyRowOffersUnpin({ isHistoryRow: isHistoryRowId(id), pinRow: pinLabelOf(id) })
                  );
                })()}
                hub={hub}
                mediaUpload={mediaUploadFor(row())}
                onRetrySend={() => {
                  const r = row();
                  if (r.kind === "outgoing") retrySend(r.tempId);
                }}
                fullReactPickerId={fullReactPickerId()}
                onOpenFullReactPicker={(id) => {
                  setFullReactPickerId(id);
                }}
                onCloseFullReactPicker={() => setFullReactPickerId(null)}
                selecting={selecting()}
                isSelected={(id) => selection().has(id)}
                onToggleSelect={toggleSelectedRow}
                onSelect={enterSelectionMode}
              />
                )}
              </Show>
              </>
              );
            }}
          </For>
        </ol>
      </Show>

      {/* ADR-0045 UX §6.2.5 — the terminal row.

          It goes in the TIMELINE, not in a banner, and §10 already says why:
          "only outcomes belong in the timeline" — and this is an outcome, one
          that happened at a definite point in the transcript. A row marks
          WHERE the history stops, survives scrollback, needs no dismissal
          state, and reuses the centred muted treatment system rows already
          have.

          It renders after the list rather than inside it because it is not a
          `TimelineRow`: nothing produced it on the wire (the removing commit
          returns the same content-less `Commit` as any other), and the state
          is terminal, so "after everything" is the only place it can be.

          Announced POLITELY, once. It arrives unbidden, possibly mid-sentence,
          and interrupting someone to tell them they have been removed from a
          group is the wrong register for a fact they cannot act on. Carried by
          TEXT, never by hue or an icon. */}
      <Show when={terminalRow(membership())}>
        {(row) => (
          <div
            role="status"
            aria-live="polite"
            style={{
              "font-size": "0.8rem",
              color: "var(--surface-muted)",
              "text-align": "center",
              padding: "0.75rem 0.5rem 0.25rem 0.5rem",
              display: "flex",
              "flex-direction": "column",
              gap: "0.2rem",
            }}
          >
            <span>{row().headline}</span>
            <span>{row().consequence}</span>
          </div>
        )}
      </Show>

      {/* 1.19c2 — pending-attachments tray. Renders above the
         text composer when ≥1 file is staged; each chip shows
         the filename + size with a × to drop it. A photo or
         video chip shows its thumb (UX spec section 10), filed
         in the media hub from the local bytes before upload. */}
      <Show when={pendingAttachments().length > 0 || uploading()}>
        <div
          style={{
            display: "flex",
            "flex-wrap": "wrap",
            gap: "0.4rem",
            padding: "0.5rem",
            border: "1px solid var(--surface-border)",
            "border-radius": "var(--radius-md)",
            background: "var(--surface-elevated)",
          }}
        >
          <For each={pendingAttachments()}>
            {(ref, i) => (
              <span
                style={{
                  display: "inline-flex",
                  "align-items": "center",
                  gap: "0.35rem",
                  padding: "0.2rem 0.5rem",
                  background: "var(--surface-bg)",
                  border: "1px solid var(--surface-border)",
                  "border-radius": "var(--radius-sm)",
                  "font-size": "0.8rem",
                }}
              >
                <Show
                  when={
                    ref.thumb !== undefined
                      ? hub.urlFor(`thumb:${mediaKey(ref.thumb.blobId)}`)
                      : null
                  }
                >
                  {(url) => (
                    <img
                      src={url()}
                      alt=""
                      style={{
                        width: "28px",
                        height: "28px",
                        "object-fit": "cover",
                        "border-radius": "4px",
                      }}
                    />
                  )}
                </Show>
                <span>{ref.name ?? t("chat.attachment.unnamed")}</span>
                <span style={{ color: "var(--surface-muted)" }}>
                  · {formatAttachmentSize(ref.size)}
                </span>
                <button
                  type="button"
                  onClick={() => removePendingAttachment(i())}
                  disabled={busy()}
                  aria-label={t("chat.composer.remove_attachment")}
                  style={{
                    border: "none",
                    background: "transparent",
                    cursor: "pointer",
                    "font-size": "1rem",
                    "line-height": 1,
                    padding: 0,
                  }}
                >
                  ×
                </button>
              </span>
            )}
          </For>
          <Show when={uploading()}>
            <Show
              when={(() => {
                const key = uploadProgress()?.previewKey;
                return key === undefined ? null : hub.urlFor(key);
              })()}
            >
              {(url) => (
                <img
                  src={url()}
                  alt=""
                  style={{
                    width: "36px",
                    height: "36px",
                    "object-fit": "cover",
                    "border-radius": "var(--radius-sm)",
                    "align-self": "center",
                  }}
                />
              )}
            </Show>
            <span
              style={{
                color: "var(--surface-muted)",
                "font-size": "0.8rem",
                "align-self": "center",
              }}
            >
              {(() => {
                const p = uploadProgress();
                if (p === null) return t("chat.upload.working");
                const name = p.fileName;
                if (p.total > 1) {
                  const args = { name, index: p.fileIndex + 1, total: p.total };
                  return p.stage === "encrypting"
                    ? t("chat.upload.encrypting_of", args)
                    : t("chat.upload.uploading_of", args);
                }
                return p.stage === "encrypting"
                  ? t("chat.upload.encrypting", { name })
                  : t("chat.upload.uploading", { name });
              })()}
            </span>
          </Show>
        </div>
      </Show>

      {/* ADR-0024 — typing indicator. Sits just above the compose
          bar; auto-hides when nobody is typing (the memo returns
          null) and after the ~6 s expiry sweep.

          ADR-0045 UX §6.2.11 — and never on a dead group. No new typing
          signal can arrive (nothing decrypts any more), so what would render
          is whatever was in flight at the moment of the eviction: a live
          indicator over a conversation that has stopped. */}
      {/* The typing indicator is the header's second line (docs/ux 5.2); the
          screen-reader announcement stays here so it is still read out. */}
      <Show when={typingIndicator() !== null && !membershipEnded()}>
        <div class="lakko-visually-hidden" aria-live="polite">
          {typingIndicator()}
        </div>
      </Show>

      {/* ADR-0045 UX §6.2.5 — the composer is REPLACED, not disabled.

          §5.3 argues a pending row's `Remove` must stay visible-but-disabled,
          because hiding it would read as the permission having vanished when
          it is merely suspended. The permission here has vanished, permanently
          and by design, so the rule inverts: disable while the state is
          TRANSIENT, remove when it is TERMINAL. A disabled Send button on a
          group you will never be in again is a control promising a future it
          does not have.

          The bar sits in the composer's own slot so the layout does not jump,
          and it is STATIC TEXT inside that region rather than a live region or
          a focus stop — so a screen-reader user reaching the place they type
          hears why they cannot, rather than discovering an absence. */}
      <Show when={composerWithdrawn()}>
        {(bar) => (
          <div
            style={{
              display: "flex",
              "flex-direction": "column",
              gap: "0.4rem",
              "padding-top": "0.5rem",
              "border-top": "1px solid var(--surface-border)",
            }}
          >
            {/* The unsent draft, preserved because the commit can land
                mid-sentence and silently discarding what the user typed is a
                small data loss with no upside.

                `readonly`, never `disabled`: this is the web expression of the
                cross-platform shape §6.2.5 specifies — SELECTABLE,
                non-editable text. A disabled control that contains the user's
                own words is a control that shows them and withholds them, and
                on iOS and Android a disabled field is not selectable at all,
                which is why the spec names a shape rather than a widget. It
                must stay reachable by keyboard and by assistive technology,
                which `readonly` is and `disabled` is not. */}
            <Show when={bar().draft !== null}>
              <label
                style={{
                  display: "flex",
                  "flex-direction": "column",
                  gap: "0.2rem",
                }}
              >
                <span
                  style={{
                    "font-size": "0.7rem",
                    "text-transform": "uppercase",
                    "letter-spacing": "0.04em",
                    color: "var(--surface-muted)",
                  }}
                >
                  {bar().draftLabel}
                </span>
                <input
                  type="text"
                  readonly
                  value={bar().draft ?? ""}
                  style={{
                    flex: 1,
                    background: "transparent",
                    color: "var(--surface-fg)",
                  }}
                />
              </label>
            </Show>
            <span
              style={{ "font-size": "0.8rem", color: "var(--surface-muted)" }}
            >
              {bar().notice}
            </span>
          </div>
        )}
      </Show>
      </div>

      {/* message-actions.md section 8 — while selecting, the action bar
          replaces the composer: Forward and Delete, each with its icon and
          label, and under the bar the one line that explains a disabled
          Delete. Sits in the composer's slot so the layout does not jump. */}
      <Show when={selecting()}>
        <div class="lk-select-actions" role="toolbar" aria-label={t("chat.select.toolbar")}>
          <div class="lk-select-actions-row">
            <button
              type="button"
              class="lk-select-action"
              disabled={!forwardEnabled(selection())}
              onClick={() => setBulkForwardOpen(true)}
            >
              <Icon name="forward" size={18} />
              <span>{t("chat.message.forward")}</span>
            </button>
            <button
              type="button"
              class="lk-select-action danger"
              disabled={!deleteState().enabled}
              onClick={() => void deleteSelection()}
            >
              <Icon name="trash" size={18} />
              <span>{t("common.delete")}</span>
            </button>
          </div>
          <Show when={deleteState().helper}>
            {(helper) => <div class="lk-select-helper">{helper()}</div>}
          </Show>
        </div>
      </Show>
      <Show when={!selecting()}>
      {/* ADR-0047 UX §6 — the permission-denied card and the short hints. */}
      <RecorderNotices recorder={recorder} />
      <VideoNoteOverlay recorder={recorder} />
      {/* ADR-0052 / section 3.1 — the reply strip: one line above the
          composer, a rule in the accent colour, the quoted author, the
          excerpt, a close control at the trailing end. It shows exactly the
          snapshot the recipient will see. */}
      <Show when={replyingTo()}>
        {(armed) => (
          <div class="lk-reply-strip" role="status" aria-label={t("chat.reply.strip_label", { name: armed().author })}>
            <span class="lk-reply-strip-rule" aria-hidden="true" />
            <span class="lk-reply-strip-body">
              <span class="lk-reply-strip-name">{armed().author}</span>
              <span class="lk-reply-strip-excerpt">{armed().excerpt}</span>
            </span>
            <button
              type="button"
              class="lk-icon-button lk-reply-strip-close"
              aria-label={t("chat.reply.cancel")}
              title={t("chat.reply.cancel")}
              onClick={() => {
                setReplyingTo(null);
                composerInput?.focus();
              }}
            >
              <Icon name="close" size={18} />
            </button>
          </div>
        )}
      </Show>
      <form onSubmit={onSend} class="lk-composer">
        <Show when={composerPickerOpen() && !recorderBusy(recorder)}>
          <EmojiPicker
            mode="compose"
            onEmoji={insertEmoji}
            onGif={sendGif}
            onClose={() => setComposerPickerOpen(false)}
            sessionToken={() => auth.identitySecrets()?.sessionToken ?? null}
          />
        </Show>
        <Show
          when={!recorderBusy(recorder)}
          fallback={<RecordingBar recorder={recorder} />}
        >
        {/* 1.19c2 — Attach button. The hidden `<input
            type="file" multiple>` is keyed by uploading() so
            the same file can be re-picked after a failed
            upload, and the label drives the click for a tidy
            chrome-less control. */}
        <label
          class="lk-icon-button"
          style={{
            cursor: uploading() ? "wait" : "pointer",
            opacity: uploading() ? 0.6 : 1,
          }}
          aria-label={t("chat.composer.attach")}
          title={t("chat.composer.attach_limit", { limit: formatAttachmentSize(MAX_ATTACHMENT_BYTES) })}
        >
          <Icon name="clip" size={20} />
          <input
            type="file"
            multiple
            disabled={uploading()}
            onChange={(e) => {
              void onFilesPicked(e);
            }}
            style={{ display: "none" }}
          />
        </label>
        {/* docs/ux section 5.2 — attach, the field, the emoji / GIF button,
            then the trailing microphone-or-send. The field is a textarea that
            grows to five lines; Enter sends, Shift+Enter breaks the line. */}
        <textarea
          ref={composerInput}
          class="lk-composer-input"
          rows={1}
          placeholder={t("chat.composer.placeholder")}
          autocomplete="off"
          value={input()}
          onInput={(e) => setInput(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
              e.preventDefault();
              e.currentTarget.form?.requestSubmit();
            }
            // ADR-0052 — Escape in the composer drops an armed reply.
            if (e.key === "Escape" && replyingTo() !== null) {
              e.preventDefault();
              setReplyingTo(null);
            }
          }}
          // ADR-0024 — leaving the composer is a typing-stop
          // transition; emit one `typing:false` (best-effort).
          onBlur={() => {
            rememberCaret();
            sendTypingStop();
          }}
        />
        {/* ADR-0047 UX section 4 — the emoji / GIF button beside the field. */}
        <button
          type="button"
          class="lk-icon-button"
          data-picker-toggle
          aria-label={t("chat.composer.emoji_gifs")}
          aria-expanded={composerPickerOpen()}
          aria-pressed={composerPickerOpen()}
          title={t("chat.composer.emoji_gifs")}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => {
            rememberCaret();
            setComposerPickerOpen(!composerPickerOpen());
          }}
        >
          <Icon name="smile" size={20} />
        </button>
        </Show>
        {/* UX spec §1 — Send while the field or tray has content, otherwise
            the record button (mic or camera). The record button stays mounted
            through a held take: it holds the pointer capture the gesture runs
            on. */}
        <Show
          when={
            (input().length > 0 || pendingAttachments().length > 0 || uploading()) &&
            !recorderBusy(recorder)
          }
          fallback={<RecordButton recorder={recorder} />}
        >
        <button
          class="lk-icon-button accent lk-send"
          type="submit"
          aria-label={t("chat.composer.send")}
          title={t("chat.composer.send")}
          disabled={
            uploading() ||
            (input().length === 0 && pendingAttachments().length === 0)
          }
        >
          <Icon name="send" size={18} />
        </button>
        </Show>
      </form>
      </Show>

    </div>
  );
}

/**
 * A shareable link as one row: the URL, Copy, Share where the platform has a
 * share sheet, and Revoke (docs/ux section 5.3 — never a raw textarea).
 */
function ShareLinkRow(props: {
  url: string;
  busy: boolean;
  onRevoke: () => void;
}): JSX.Element {
  const [copied, setCopied] = createSignal(false);
  const canShare = (): boolean =>
    typeof navigator !== "undefined" && typeof navigator.share === "function";
  async function copy(): Promise<void> {
    try {
      await navigator.clipboard.writeText(props.url);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch {
      /* the button is the only feedback surface; a failed copy leaves it as is */
    }
  }
  return (
    <div class="lk-link-row" role="group" aria-label={t("chat.invite_link.label")}>
      <span class="lk-link-text" title={props.url}>
        {props.url.replace(/^https?:\/\//, "")}
      </span>
      <button
        type="button"
        class="lk-icon-button"
        title={copied() ? t("common.copied") : t("chat.invite_link.copy")}
        aria-label={copied() ? t("common.copied") : t("chat.invite_link.copy")}
        onClick={() => void copy()}
      >
        <Icon name={copied() ? "check" : "copy"} size={16} />
      </button>
      <Show when={canShare()}>
        <button
          type="button"
          class="lk-icon-button"
          title={t("chat.invite_link.share")}
          aria-label={t("chat.invite_link.share")}
          onClick={() => void navigator.share({ url: props.url }).catch(() => undefined)}
        >
          <Icon name="share" size={16} />
        </button>
      </Show>
      <button
        type="button"
        class="ghost small"
        style={{ color: "var(--danger)" }}
        disabled={props.busy}
        onClick={props.onRevoke}
      >
        {t("chat.invite_link.revoke")}
      </button>
    </div>
  );
}

function GroupNotFound(): JSX.Element {
  return (
    <section style={panelStyle()}>
      <h1 style={{ "margin-top": 0 }}>{t("chat.not_found.title")}</h1>
      <p>{t("chat.not_found.body")}</p>
      <p>
        <A href={paths.home}>{t("chat.not_found.back")}</A>
      </p>
    </section>
  );
}

function NeedsReauth(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();
  // `/login` is an anonymous-only route: linking to it while signed in
  // bounces straight back home. Sign out first, then go there — the same
  // sequence the sidebar's Log out uses.
  function signInAgain(): void {
    auth.logout();
    navigate(paths.login, { replace: true });
  }
  return (
    <section style={panelStyle()}>
      <h1 style={{ "margin-top": 0 }}>{t("chat.reauth.title")}</h1>
      <p>{t("chat.reauth.body")}</p>
      <p>
        <button class="primary" type="button" onClick={signInAgain}>
          {t("chat.reauth.action")}
        </button>
      </p>
    </section>
  );
}

// ---------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------

/** The one line under the timeline about this chat's last one-to-one call. */
function lastEndedLine(reason: string | undefined): string {
  switch (reason) {
    case "hungUp":
      return t("chat.call.last_ended.hung_up");
    case "busy":
      return t("chat.call.last_ended.busy");
    case "declined":
      return t("chat.call.last_ended.declined");
    case "timedOut":
      return t("chat.call.last_ended.timed_out");
    default:
      return t("chat.call.last_ended.error");
  }
}

function panelStyle(): JSX.CSSProperties {
  return {
    background: "var(--surface-elevated)",
    border: "1px solid var(--surface-border)",
    "border-radius": "var(--radius-md)",
    padding: "1rem",
  };
}


/**
 * ADR-0045 UX §6 — ask the core whether a plain retry may succeed.
 *
 * Wrapped rather than passed raw because `formatError` is synchronous and
 * this is a wasm export: on any path that just caught a refusal the module is
 * initialised (the refusal came out of it), but a throw here would replace a
 * refusal message with a crash. Failing closed means no invitation to retry,
 * never a wrong one.
 */
function membershipRetryMaySucceed(reason: string): boolean {
  try {
    return wasmMembershipRefusalRetryMaySucceed(reason);
  } catch {
    return false;
  }
}

/**
 * ADR-0045 UX §6 / §12 item 6 and ADR-0061 section 6 — the single funnel
 * every handler's failure goes through, returned as how to render it so a
 * message already on screen follows a language switch.
 *
 * A membership refusal renders from its reason (the core's sentence is for a
 * developer reading a log). A sentence written for a screen (a
 * {@link KeyedError} or {@link RenderedError}, possibly wrapped) keeps its
 * own words, and a few well-known contact and network failures have theirs.
 * Anything else is logged and shown as the action's own `fallbackKey`: core,
 * server and browser prose never reaches the person.
 */
/** ADR-0073 R.7 case 2 — the distinct PERSON keys of a roster (base64 account
 *  keys), or `null` on a torn read, which decides nothing. */
function personKeysOrNull(group: ActiveGroup["group"]): string[] | null {
  try {
    group.members();
  } catch {
    return null;
  }
  return personKeysOf(group);
}

function errorText(err: unknown, fallbackKey: string): () => string {
  const refusal = membershipRefusalOf(err, membershipRetryMaySucceed);
  if (refusal !== null) {
    if (!refusal.recognised) {
      // The only case the fail-closed row shows for a refusal nobody decided
      // about: a core newer than this build. Worth a console line, because it
      // is a shipping gap rather than a user error.
      console.warn("membership refusal with no copy in this build", refusal.reason, err);
    }
    return () => membershipRefusalOf(err, membershipRetryMaySucceed)?.message ?? refusal.message;
  }
  const shown = shownErrorText(err);
  if (shown !== null) return shown;
  if (dsClockSkewNotice(err) !== null) return () => dsClockSkewNotice(err) ?? "";
  if (err instanceof SupersessionRefused) {
    const loggedReset = err.loggedReset;
    return () => supersessionRefusedMessage(loggedReset);
  }
  if (err instanceof ContactsError) {
    const variant = err.variant;
    switch (variant.kind) {
      case "no-identity-secrets":
        return () => t("common.error.session_ended");
      case "username-unknown":
        return () => t("chat.members.add.error.unknown_username", { name: `@${variant.username}` });
      case "no-identity-bundle":
        return () => t("chat.members.add.error.no_bundle");
      default:
        break;
    }
  }
  if (isDsNetworkFailure(err)) return () => t("common.error.offline");
  console.warn("[chat] action failed", err);
  return () => t(fallbackKey);
}

/** The screen text of a {@link KeyedError} or {@link RenderedError} anywhere
 *  in the wrap chain (`cause`, or `variant.cause` for the contacts and chat
 *  clients' own wrappers), or `null`. */
function shownErrorText(err: unknown): (() => string) | null {
  let current: unknown = err;
  for (let depth = 0; depth < 6 && current !== null && current !== undefined; depth += 1) {
    const found = current;
    if (keyedErrorText(found) !== null) return () => keyedErrorText(found) ?? "";
    if (typeof current !== "object") return null;
    const wrapped = current as { cause?: unknown; variant?: { cause?: unknown } };
    current = wrapped.variant?.cause ?? wrapped.cause;
  }
  return null;
}

/** Lowercase hex of a small byte array, for short
 *  filename-fallback when an attachment ref carries no name. */
function blobIdHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) {
    out += b.toString(16).padStart(2, "0");
  }
  return out;
}

/** `true` when the drag event carries an actual file payload
 *  (not just selected text or a link) — the
 *  [`DataTransfer.types`] list contains `"Files"` for OS-side
 *  file drags from Finder / Explorer / a download manager.
 *
 *  Guard exists so a stray text-selection drag doesn't trigger
 *  the drop-zone overlay; the file-only filter matches how the
 *  attach-button path treats the user input (a `<input
 *  type="file">` only fires for actual files). */
function hasFilesType(dt: DataTransfer | null): boolean {
  if (dt === null) return false;
  // `DataTransfer.types` is a DOMStringList — `includes` is
  // available on modern browsers; the `Array.from` fallback
  // keeps the check working for older list-only implementations.
  const types = Array.from(dt.types);
  return types.includes("Files");
}

// ---------------------------------------------------------------
// Phase 2a3f1-web — TimelineRow ↔ PersistedMessage conversion
// ---------------------------------------------------------------

/** Convert a runtime [`TimelineRow`] into the on-disk
 *  [`PersistedMessage`] shape. Returns `null` for outgoing
 *  rows that haven't been acked yet — those carry no
 *  server-assigned `messageId` and would collide on reload
 *  with whatever the DS eventually hands back. The next ack
 *  flips the row to `kind: "outgoing"` with a populated
 *  `messageId` and the next persist sweep includes it. */
function timelineRowToPersisted(
  row: TimelineRow,
  /** ADR-0047 — media keys this device has played; local-only state. */
  played: ReadonlySet<string> = new Set(),
): PersistedMessage | null {
  // Phase 3a8 — call signaling events never persist into
  // the chat log; they're an out-of-band side channel that
  // the CallContext consumes.
  if (row.kind === "incoming" && row.event.kind === "callSignaling") {
    return null;
  }
  if (row.kind === "outgoing") {
    if (
      row.ack !== "sent" ||
      row.messageId === undefined ||
      row.createdAt === undefined
    ) {
      return null;
    }
    return {
      messageId: String(row.messageId),
      direction: "outgoing",
      text: row.plaintext,
      attachments: row.attachments.map((a) =>
        persistedFromAttachmentRef(a, played.has(mediaKey(a.blobId))),
      ),
      createdAt: row.createdAt,
      // ADR-0023 — persisted so edit / delete keep working after a
      // reload. The key we stamped at send, read back verbatim: the
      // store is the record of who wrote this, not a place to
      // re-derive it from whoever is holding the device now.
      authorKeyB64: authorIdentityToPersisted(row.authorKey),
      // ADR-0052 — the reply reference, so the quote survives a reload.
      ...(row.reply === undefined ? {} : { reply: persistedFromReplyRef(row.reply) }),
      // ADR-0052 decision 2 — the forward attribution, verbatim.
      ...(row.forward === undefined ? {} : { forward: persistedFromForwardRef(row.forward) }),
    };
  }
  // Incoming: split application vs system.
  if (row.event.kind === "application") {
    // The same shape the background receive files for a chat that is not
    // open (`chat/background-log.ts`), so the two read back identically.
    // ADR-0023 — the author is the signature key resolved at receipt; storing
    // the leaf was never an option, it may belong to whoever was added next.
    const anyPlayed = row.event.attachments.some((a) => played.has(mediaKey(a.blobId)));
    const base = eventToPersistedMessage(row.event);
    return anyPlayed
      ? {
          ...base,
          attachments: row.event.attachments.map((a) =>
            persistedFromAttachmentRef(a, played.has(mediaKey(a.blobId))),
          ),
        }
      : base;
  }
  if (row.event.kind === "callSignaling") {
    // Already filtered out at the top of this function.
    return null;
  }
  if (row.event.kind === "op") {
    // ADR-0023 — op events are folded into the target message's
    // overlay by the receive loop, never materialised as timeline
    // rows, so this branch is unreachable. Never persist them.
    return null;
  }
  if (row.event.kind === "externalJoinRequest") {
    // ADR-0027 (G-3) — external-join requests never enter `rows()` (the
    // receive loop skips them), so this branch is unreachable. Never persist.
    return null;
  }
  if (row.event.kind === "pushRoutingSecret") {
    // ADR-0042 — consumed by the receive loop (persist + subscribe), never
    // materialised as a timeline row, so this branch is unreachable.
    return null;
  }
  if (row.event.kind === "committerBeacon") {
    // ADR-0045 — consumed by the receive loop, never materialised as a
    // timeline row, so this branch is unreachable. Never persist it.
    return null;
  }
  if (row.event.kind === "profile") {
    // ADR-0048 — consumed by the receive loop (filed in the profile store),
    // never a timeline row. Never persist it: a name belongs in the encrypted
    // profile store, not in the transcript.
    return null;
  }
  if (row.event.kind === "profileRequest") {
    // ADR-0048 §2 trigger 4 — answered by the profile provider, never a
    // timeline row. Never persist it.
    return null;
  }
  if (row.event.kind === "group-name") {
    // ADR-0051 — consumed by the group-name gate, never a timeline row (no
    // "renamed the group" transcript row in this version). Never persist it.
    return null;
  }
  if (row.event.kind === "group-closed") {
    // ADR-0073 R.6 — a successor claim, consumed by the claim rule; never in
    // the transcript.
    return null;
  }
  if (row.event.kind === "pin") {
    // ADR-0052 decision 3 — consumed by the pin gate, never a timeline row
    // (no "pinned a message" transcript row in this version). Never persist
    // it.
    return null;
  }
  if (row.event.kind === "shared-channels") {
    // ADR-0059 section 6 — consumed by the shared-channels store, never a
    // timeline row. Never persist it: which channels a member follows is
    // not part of the transcript.
    return null;
  }
  if (row.event.kind === "scopedDiscovery") {
    // ADR-0060 — a service envelope; its payload can hold secrets (a
    // delegation, an introduction record). Never a row, never persisted.
    return null;
  }
  if (row.event.kind === "registrarSeed") {
    // ADR-0057 — consumed at receipt by the chat client and never a row.
    // Never persist it: it carries a sealed registrar seed.
    return null;
  }
  if (row.event.kind === "serviceEnvelope") {
    // An unrecognised `lakko/…` service envelope: never a row, never
    // persisted, so it can never become the chat-list preview line.
    return null;
  }
  if (row.event.kind === "membershipProposal") {
    // ADR-0045 UX §10 — a pending request must NEVER reach the persisted
    // message log: it is an intent the system may discard a second later, and
    // writing it to a durable, mutually visible transcript would both leak a
    // pending change to members who cannot act on it and outlive the intent.
    return null;
  }
  // system
  return {
    messageId: String(row.event.messageId),
    direction: "system",
    text: systemRowText(row.event.systemKind),
    attachments: [],
    createdAt: row.event.createdAt,
  };
}

/** Inverse — reconstruct a [`TimelineRow`] from the on-disk
 *  shape. Outgoing rows come back as acked
 *  (`ack: "sent"`, no `tempId`); the UI's `<For>` key uses
 *  `messageId` for outgoing+sent so no key conflict with the
 *  fresh-send tempIds.
 *
 *  Caveat: `system` rows survive the round trip with their
 *  text already baked in; the systemKind discriminator isn't
 *  recoverable but the rendered string is the same. */
/**
 * Client-side ids for rows restored from the log. Every restored outgoing row
 * used to be `tempId: -1`, so they all shared the identity `out:-1` that the
 * timeline's entry bookkeeping (`chat/call-timeline.ts`) keys on - and its
 * live read answers with the LAST row under an identity, so a chat with N
 * restored own messages drew the newest one N times and the others not at
 * all. Seen live on 2026-09-26 with messages forwarded into a chat. Negative
 * and counting down, so no restored id can meet a live `nextTempId`.
 */
let restoredTempId = -1;

function persistedToTimelineRow(m: PersistedMessage): TimelineRow {
  const attachments = m.attachments.map((a) => attachmentRefFromPersisted(a).ref);
  // ADR-0052 — absent on a plain message and on a row written before v4.
  const reply = replyRefFromPersisted(m.reply);
  // ADR-0052 decision 2 — absent unless forwarded, and on a row before v5.
  const forward = forwardRefFromPersisted(m.forward);
  if (m.direction === "outgoing") {
    restoredTempId -= 1;
    return {
      kind: "outgoing",
      tempId: restoredTempId,
      plaintext: m.text,
      attachments,
      ack: "sent",
      messageId: Number(m.messageId),
      createdAt: m.createdAt,
      ...(reply === undefined ? {} : { reply }),
      ...(forward === undefined ? {} : { forward }),
      // Absent on a row written before this field existed, and
      // `null` when it could not be resolved — both restore as
      // `undefined`, which fails the edit/delete gate closed rather
      // than falling back to our CURRENT key (which a self-update
      // may since have rotated).
      authorKey: persistedAuthorIdentity(m.authorKeyB64),
    };
  }
  if (m.direction === "incoming") {
    return {
      kind: "incoming",
      event: {
        kind: "application",
        messageId: Number(m.messageId),
        text: m.text,
        plaintext: m.text,
        attachments,
        // The leaf is NOT restored, and never will be: a leaf index
        // is a position in the ratchet tree, so a value stored under
        // one epoch names a different member under the next. Only
        // the identity round-trips.
        senderLeaf: undefined,
        // ADR-0023 — the author's signature key, restored so a peer
        // editing or deleting their own message across a reload now
        // applies (before this it always failed closed). Absent on a
        // pre-existing row and `null` when unresolved — both become
        // `undefined`, which is exactly that old fail-closed
        // behaviour, never a misattribution.
        senderKey: persistedAuthorIdentity(m.authorKeyB64),
        createdAt: m.createdAt,
        ...(reply === undefined ? {} : { reply }),
        ...(forward === undefined ? {} : { forward }),
        // ADR-0061 section 6 — a message from a newer Lakko stays one.
        ...(typeof m.unsupportedVersion === "number" ? { unsupportedVersion: m.unsupportedVersion } : {}),
      },
    };
  }
  // system
  return {
    kind: "incoming",
    event: {
      kind: "system",
      messageId: Number(m.messageId),
      systemKind: "commit" as const,
      createdAt: m.createdAt,
    },
  };
}


