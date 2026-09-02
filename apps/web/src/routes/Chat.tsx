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
  onMount,
  onCleanup,
  Show,
  type JSX,
} from "solid-js";
import { A, useNavigate, useParams } from "@solidjs/router";
import { useAuth } from "../auth/AuthContext";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { groupIdHex as hexOf } from "../devices/sync/state";
import {
  CallSession,
  mintCallId,
  type WebCallState,
  type WebCallEndReason,
  type WebConnectionStats,
  type VideoPublishState,
} from "../calls/call-session";
import {
  callActions,
  callState as callUi,
  qualityColour,
  type Participant,
  type QualityTier,
} from "../state/call-state";
import { createSfuCallUi, type SfuCallUiHandle } from "../calls/sfu-call-ui";
import { sfuBaseUrl } from "../calls/sfu-config";
import { CallJwtClient } from "../calls/call-jwt-client";
import Call from "./Call";
import { loadWasm as loadWasmForCalls } from "../wasm";
// ADR-0045 UX §6 — the core's own answer to *will the identical call work
// later*. Imported as a named export rather than through `loadWasm` because
// `formatError` is synchronous; `src/wasm.ts` imports the same module, so
// this is the same instance and not a second one.
import { membershipRefusalRetryMaySucceed as wasmMembershipRefusalRetryMaySucceed } from "../../wasm/lakko_ffi_wasm";
import {
  deleteGroupFromMessageLog,
  loadMessageLog,
  MESSAGE_LOG_VERSION,
  saveMessageLog,
  type MessageLogPayload,
  type PersistedAttachment,
  type PersistedGroup,
  type PersistedMessage,
} from "../chat/message-log-store";
import { getDeviceKey } from "../auth/key-store";
import { base64Decode, base64Encode, base64UrlDecodeNoPad } from "../auth/account-id";
import { ChatClient } from "../chat/client";
import { makeCallSignalingPublisher } from "../chat/call-signaling-publish";
import { ContactsClient } from "../contacts/client";
import {
  loadAccepted,
  peerContactByMemberPubkeys,
} from "../contacts/local-store";
import type { IncomingChatEvent } from "../chat/client";
import {
  composeEnvelope,
  composeEditEnvelope,
  composeDeleteEnvelope,
  composeReactionEnvelope,
  composeTypingEnvelope,
  composeReadReceiptEnvelope,
  downloadAndDecryptRef,
  encryptAndUploadFile,
  formatAttachmentSize,
  isInlinePreviewable,
  saveDecryptedBytes,
  type AttachmentRefBytes,
  AttachmentSizeError,
  AttachmentMissingError,
  MAX_ATTACHMENT_BYTES,
} from "../chat/attachments";
import { useChats, type ActiveGroup } from "../chat/ChatContext";
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
  JOIN_REQUESTS_NOT_SAVED,
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
  clearArmedRetries,
  armedApproveJoinRetry,
  armedRetryFor,
  claimArmedRetry,
  retryKey,
  retryScopeKey,
  retryVerdict,
  type GroupSnapshot,
  type RetryableRequest,
  type RosterMemberView,
} from "../chat/membership-retry";
// ADR-0045 S7 residual 1 — the request that left the pending set without
// landing. Same split, same reason as above.
import {
  clearUnappliedForGroup,
  dismissUnapplied,
  loadUnapplied,
  recordUnapplied,
  unappliedAddRequests,
  unappliedHeading,
  unappliedLine,
  UNAPPLIED_EXPLANATION,
  type UnappliedAdd,
} from "../chat/unapplied-requests";
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
  clearLeaveRequested,
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
import { leaveGroupConfirmation } from "../chat/leave-group-confirm";
import { namedAction, requestConfirm } from "../ui/confirm-dialog";
import {
  applyOp,
  editDeleteAuthorized,
  OWN_REACTION_SENTINEL,
  REACTION_EMOJI,
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
  foldReadWatermark,
  isReadByEveryOther,
  shouldSendTyping,
  shouldSendReadReceipt,
  TYPING_EXPIRY_MS,
  type TypingMap,
  type ReadWatermarkMap,
} from "../chat/presence-ops";
import { paths } from "./paths";

/** A timeline entry — verified incoming events PLUS optimistic
 *  outbound bubbles that haven't been confirmed yet.
 *
 *  Phase 1.19c2: outgoing bubbles now carry an optional
 *  `attachments` field so the optimistic UI can show the chips
 *  immediately after Send (the bytes are already uploaded to
 *  storage by the time the bubble renders). */
type TimelineRow =
  | { kind: "incoming"; event: IncomingChatEvent }
  | {
      kind: "outgoing";
      /** Stable client-side id for `<For>` keying — distinct
       *  from any DS-assigned id. */
      tempId: number;
      plaintext: string;
      attachments: AttachmentRefBytes[];
      ack: "pending" | "sent";
      /** Filled when ack flips to "sent". */
      messageId?: number;
      createdAt?: string;
      /** ADR-0023 — the identity that authored this row: this
       *  device's own signature key at send time, or the key the
       *  message log persisted for a row restored on reload.
       *
       *  Stamped at send rather than read from `ownLeaf()` at gate
       *  time because a leaf is a position, not a person: any leaf,
       *  ours included, can be reassigned to a member added later.
       *  `undefined` (unresolvable) fails the gate closed. */
      authorKey?: MemberIdentity | undefined;
    };

/** Read the author IDENTITY a displayed row should be gated on:
 *  incoming application messages carry the author's signature key,
 *  resolved from the MLS sender leaf at receipt; outgoing rows carry
 *  the key stamped when we sent (or restored from the log).
 *  `undefined` ⇒ no author is known for this row (a control frame, a
 *  row stored before the identity was persisted) — its edit/delete
 *  gate fails closed.
 *
 *  Never a leaf index: OpenMLS hands a vacated leaf to the next
 *  member added, so a leaf read at gate time can name somebody who
 *  was not there when the message was written. */
function rowAuthorIdentity(row: TimelineRow): MemberIdentity | undefined {
  if (row.kind === "outgoing") return row.authorKey;
  if (row.event.kind === "application") return row.event.senderKey;
  return undefined;
}

/** The DS `messageId` of a displayed row, or `undefined` when
 *  it doesn't have one yet (an un-acked outgoing bubble). Every
 *  edit/delete/react affordance is gated on this being present
 *  (the op references its target by id). */
function rowMessageId(row: TimelineRow): number | undefined {
  if (row.kind === "outgoing") {
    return row.ack === "sent" ? row.messageId : undefined;
  }
  if (row.event.kind === "application") return row.event.messageId;
  return undefined;
}

export default function Chat(): JSX.Element {
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

  const entry = (): ActiveGroup | undefined => {
    const id = groupIdB64();
    if (id === null) return undefined;
    return chats.get(id);
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
    <article>
      <Show
        when={auth.identitySecrets() !== null}
        fallback={<NeedsReauth />}
      >
        <Show
          when={entry() !== undefined}
          fallback={<GroupNotFound />}
        >
          <ChatView
            entry={entry() as ActiveGroup}
            requireIdentity={requireIdentity}
            onLeave={() => navigate(paths.contacts)}
          />
        </Show>
      </Show>
    </article>
  );
}

// ---------------------------------------------------------------
// G3-3-conn-quality — in-call connection-quality display helpers
// ---------------------------------------------------------------

/**
 * Bucket a measured round-trip time into the shared
 * [`QualityTier`]. These are UX display bands (NOT measured
 * SLOs) matching the iOS / Android clients exactly: a `null`
 * RTT (pre-measurement) reads as `good` so a freshly-connected
 * call doesn't flash a red dot before the first stats sample.
 */
function qualityTierForRtt(rttMs: number | null): QualityTier {
  if (rttMs === null) return "good";
  if (rttMs < 100) return "excellent";
  if (rttMs < 200) return "good";
  if (rttMs < 350) return "fair";
  if (rttMs < 600) return "poor";
  return "bad";
}

/** Format a bits-per-second rate for the in-call readout.
 *  Mirrors the `formatBps` in the Call.tsx design route. */
function formatBps(bps: number | null): string {
  if (bps === null) return "—";
  if (bps >= 1_000_000) return `${(bps / 1_000_000).toFixed(1)} Mbps`;
  if (bps >= 1_000) return `${(bps / 1_000).toFixed(0)} kbps`;
  return `${bps} bps`;
}

/** Map a [`QualityTier`] to a concrete CSS colour for the dot. */
function qualityDotColour(tier: QualityTier): string {
  switch (qualityColour(tier)) {
    case "green":
      return "#22c55e";
    case "yellow":
      return "#eab308";
    case "red":
      return "#ef4444";
  }
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
  // Phase 2a3e2b-web — cross-device sync for per-thread state
  // (drafts now; flags / order / read cursors follow). Used
  // below to seed the composer from sibling-device drafts
  // and to broadcast composer edits back out.
  const sync = useDeviceGroupSync();
  // Raw bytes of the conversation group_id — fed straight to
  // the sync API and into the LWW resolver keying. `Group.groupId`
  // is a wasm-bindgen getter (no parens), distinct from
  // `DeviceGroup.groupId()` which is a method (the surfaces
  // diverged in the Rust source). `createMemo` so the
  // closure-capturing effects below see a stable reference.
  const groupIdBytes = createMemo(() => props.entry.group.groupId);
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
  const contacts = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    onPersist: async () => {
      const s = auth.state();
      if (s.kind !== "session") return;
      await chats.persistSnapshot(s.account.accountId);
    },
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
  });

  const [rows, setRows] = createSignal<TimelineRow[]>([]);
  const [cursor, setCursor] = createSignal(0);

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
  // The messageId whose reaction picker is open (or `null`).
  const [reactPickerId, setReactPickerId] = createSignal<number | null>(null);

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
  // their slot. Session state (multi-device convergence is Phase-2
  // future work). The own-message bubble reads it to flip "Sent" →
  // "Read"; a lower value never lowers a higher one (re-sync
  // idempotent).
  const [readWatermarks, setReadWatermarks] = createSignal<ReadWatermarkMap>(
    new Map(),
  );

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
  const [error, setError] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);

  // Phase 3a8 — 1×1 call surface. Only one CallSession can
  // ride a given chat thread at a time; the field tracks
  // the active session (if any) + its observable state.
  // Audio is bound to a hidden `<audio>` element via the
  // ref below; the call overlay shows the duration / hang
  // up button.
  const [callState, setCallState] = createSignal<WebCallState>("idle");
  const [callEndReason, setCallEndReason] = createSignal<WebCallEndReason | null>(null);
  // G3-3-conn-quality — live connection-quality sample from the
  // session's getStats poll loop. null until the first tick lands.
  const [callConnStats, setCallConnStats] =
    createSignal<WebConnectionStats | null>(null);
  let activeCall: CallSession | null = null;
  const [remoteAudioStream, setRemoteAudioStream] = createSignal<MediaStream | null>(
    null,
  );
  const [audioRef, setAudioRef] = createSignal<HTMLAudioElement | null>(null);
  // 1×1 video-call state. `videoCall` is set when the call is dialed
  // with a camera; `localCallStream` is the local capture (mic + camera)
  // for the self-preview tile; `remoteHasVideo` tracks whether the remote
  // peer is sending video (set per `ontrack`).
  const [videoCall, setVideoCall] = createSignal(false);
  const [localCallStream, setLocalCallStream] = createSignal<MediaStream | null>(
    null,
  );
  const [remoteHasVideo, setRemoteHasVideo] = createSignal(false);
  // True while a FRESH incoming 1×1 offer advertised video (`parsed.mediaVideo`).
  // Gates the "Accept with video" affordance on the ringing banner — an
  // audio-only offer keeps a single "Accept". Set when the answerer session is
  // spun up for a first-seen offer; cleared when the call ends.
  const [remoteWantsVideo, setRemoteWantsVideo] = createSignal(false);
  // B2a — mid-call camera-on lifecycle for the 1×1 path (off / starting /
  // live / failed), fed by the CallSession's `onVideoPublishState`. The SFU
  // path exposes the same via `sfuHandle().videoPublishState()`; the effect
  // below mirrors whichever is active into the shared store.
  const [oneToOneVideoState, setOneToOneVideoState] =
    createSignal<VideoPublishState>("off");
  // SFU multi-party (group) call. Distinct from the 1×1 `activeCall`
  // above — a group chat (>2 members) routes through the Rust SFU with
  // SFrame, while a 1×1 chat stays P2P DTLS-SRTP. `sfuHandle` is held in
  // a signal so the remote-stream accessor reads reactively in JSX;
  // `sfuActive` gates the in-call overlay (the SFU path leaves the 1×1
  // `callState()` at `idle`).
  const [sfuHandle, setSfuHandle] = createSignal<SfuCallUiHandle | null>(null);
  // G-18 / ADR-0029 — the active SFU call's per-call id, retained so a mid-call
  // epoch advance can re-derive the call seed at the NEW epoch. Set when a group
  // call starts; read only while `sfuHandle()` is live (a stale value after the
  // call ends is harmless — the rekey effect guards on the live handle).
  let activeCallId: Uint8Array | null = null;
  const [sfuActive, setSfuActive] = createSignal(false);
  createEffect(() => {
    const el = audioRef();
    const stream = remoteAudioStream();
    if (el !== null && stream !== null) {
      el.srcObject = stream;
      void el.play().catch(() => {
        // Autoplay might be blocked until user gesture —
        // the Accept button click on the receiver counts.
      });
    }
  });

  // -----------------------------------------------------------------
  // G3-3-web-session-bind — drive the full Call.tsx in-call UI (the
  // shared `callActions` store) from the live `CallSession`. The web
  // analog of iOS/Android driveFrom + call-present: the rich UI is
  // presented as a full-screen overlay over the chat for live call
  // states, while `ringing` keeps the lightweight inline banner
  // (Accept / Decline).
  // -----------------------------------------------------------------

  /** True while the full in-call overlay should cover the chat.
   *  `ringing` is excluded — an incoming call shows the inline
   *  banner until accepted. */
  const callOverlayVisible = (): boolean => {
    if (sfuActive()) return true;
    const s = callState();
    return (
      s === "outgoing" ||
      s === "connecting" ||
      s === "connected" ||
      s === "ending"
    );
  };

  /** Synthesised local participant. `camOn` seeds from whether this is a
   *  video call (`videoCall()`); `micOn` seeds true. The live mic + camera
   *  state is thereafter driven through `callActions.setMic` / `setCamera`
   *  ↔ `setMicrophoneEnabled` / `setCameraEnabled` (see the effects below).
   *  Quality dot tracks the live RTT sample. */
  const localParticipant = (): Participant => ({
    id: "local",
    displayName: "You",
    isLocal: true,
    micOn: true,
    // Initial camera state = whether this is a video call. The live
    // toggle thereafter is the store's `camOn` (driven by the camera
    // button + mirrored onto the session below) — this seed is read
    // once at connect, so it does not fight the toggle.
    camOn: videoCall(),
    handRaised: false,
    quality: qualityTierForRtt(callConnStats()?.rttMs ?? null),
    speaking: false,
    screenSharing: false,
  });

  /** Synthesised sole remote participant for the 1×1 call. The web
   *  P2P path carries no roster, so the single remote IS this chat's
   *  contact. ADR-0021 follow-up: resolve the linked contact's
   *  user-chosen displayName (matched by this chat's group id), so a
   *  rename shows in the in-call roster; fall back to the chat label,
   *  then "Peer". Read at connect time, so a pre-call rename is
   *  reflected. */
  const remoteParticipant = (): Participant => ({
    id: "peer",
    displayName: resolvePeerName(),
    isLocal: false,
    micOn: true,
    // Lights up the remote video tile once the peer's video track
    // arrives (the effect below keeps it in sync after connect).
    // The remote tile shows video whenever the peer has a live video track —
    // NOT gated on whether WE started a video call, so a peer that turns their
    // camera on mid-call (piece B2b) renders even in a call we started
    // audio-only. In a pure-audio call `remoteHasVideo()` is false anyway.
    camOn: remoteHasVideo(),
    handRaised: false,
    quality: "good",
    speaking: false,
    screenSharing: false,
  });

  /** Best contact name for this 1×1 chat: the linked contact's
   *  displayName (ADR-0021), else the chat label, else "Peer". */
  const resolvePeerName = (): string => {
    const accepted = loadAccepted();
    // Fast path: a chat created from an accepted request carries a stored
    // groupIdB64 link to its contact.
    const byId = accepted.find((c) => c.groupIdB64 === props.entry.groupIdB64);
    if (byId !== undefined && byId.displayName.length > 0) {
      return byId.displayName;
    }
    // Universal: match the 1×1 peer by the group's non-self member pubkey, so
    // an outbound or Welcome-joined chat (no stored link) still resolves.
    const own = chats.identity();
    const ownB64 = own === null ? null : base64Encode(own.signaturePublicKey);
    let memberB64s: string[] = [];
    try {
      memberB64s = (
        props.entry.group.members() as Array<{ signaturePublicKey: Uint8Array }>
      ).map((m) => base64Encode(m.signaturePublicKey));
    } catch {
      // members() can throw on a torn group; fall through to the label.
    }
    const byMember = peerContactByMemberPubkeys(memberB64s, ownB64, accepted);
    if (byMember !== undefined && byMember.displayName.length > 0) {
      return byMember.displayName;
    }
    return props.entry.label.length > 0 ? props.entry.label : "Peer";
  };

  /** Map the session's `WebCallState` onto the shared call-UI
   *  store (phase + 1×1 roster). Called from `onStateChange`. */
  const syncCallUiFromState = (s: WebCallState): void => {
    switch (s) {
      case "outgoing":
      case "connecting":
        callActions.setPhase({ kind: "connecting" });
        callActions.setParticipants([localParticipant()]);
        // 1×1 web media is P2P DTLS-SRTP, not SFrame — keep the
        // E2EE indicator inactive (and the text pill is hidden in
        // the embedded view regardless).
        callActions.setE2ee({ active: false, shortFingerprint: null });
        break;
      case "connected":
        callActions.setPhase({ kind: "connected" });
        callActions.setParticipants([localParticipant(), remoteParticipant()]);
        break;
      case "ending":
        // Transient — hold the connected visuals until `done`.
        break;
      case "idle":
      case "ringing":
      case "done":
        // `ringing` uses the inline banner (no overlay); `done` /
        // `idle` tear the overlay down.
        callActions.reset();
        break;
    }
  };

  // When the user hits "End call" inside the embedded Call.tsx, it
  // sets the store phase to `ended`; mirror that onto the real
  // session by hanging up (idempotent — a remote-initiated end
  // already drove the session to `done`).
  createEffect(() => {
    if (callUi.phase().kind !== "ended") return;
    if (activeCall !== null) {
      void activeCall.hangUp("hungUp");
    }
    const h = sfuHandle();
    if (h !== null) {
      h.hangUp();
      setSfuHandle(null);
      setSfuActive(false);
    }
  });

  // G3-3-web-call-controls — drive the real microphone from the
  // store's local-participant `micOn` flag. The Call.tsx mic button
  // toggles `callActions.setMic`; this mirrors it onto the live
  // session (idempotent — `track.enabled = on` is cheap, so the
  // redundant calls on each quality patch are harmless). Drives whichever
  // call kind is live — the 1×1 `activeCall` or the SFU group `sfuHandle`.
  createEffect(() => {
    const local = callUi.participants().find((p) => p.isLocal);
    if (local === undefined) return;
    if (activeCall !== null) {
      activeCall.setMicrophoneEnabled(local.micOn);
    }
    const h = sfuHandle();
    if (h !== null) {
      h.setMicrophoneEnabled(local.micOn);
    }
  });

  // Apply an in-call device-picker change to the live session
  // (`replaceTrack` on the live sender — no renegotiation; on the SFU path the
  // sender keeps its SFrame transform so media stays encrypted). The outgoing
  // dial / SFU publish already honour the selection at capture time; this
  // covers a mid-call switch (and an answerer that captured the OS default
  // before the user's pick). `lastAudio/VideoInput` debounce so an unrelated
  // reactive tick doesn't re-capture. Drives whichever call kind is live —
  // 1×1 `activeCall` or the SFU group `sfuHandle`.
  let lastAudioInput: string | null = null;
  let lastVideoInput: string | null = null;
  createEffect(() => {
    const audioId = callUi.settings().selectedAudioInput;
    const videoId = callUi.settings().selectedVideoInput;
    const sfu = sfuHandle();
    if (activeCall === null && sfu === null) {
      lastAudioInput = null;
      lastVideoInput = null;
      return;
    }
    if (audioId !== null && audioId !== lastAudioInput) {
      lastAudioInput = audioId;
      void activeCall?.switchAudioInput(audioId);
      void sfu?.switchAudioInput(audioId);
    }
    if (videoId !== null && videoId !== lastVideoInput) {
      lastVideoInput = videoId;
      void activeCall?.switchVideoInput(videoId);
      void sfu?.switchVideoInput(videoId);
    }
  });

  // In-call camera control. The Call.tsx camera button toggles
  // `callActions.setCamera`; this mirrors the store's local `camOn`
  // onto the live session (local-only `track.enabled` gate, no
  // renegotiation). Drives whichever call kind is live — the 1×1
  // `activeCall` or the SFU group `sfuHandle` (a no-op on the group path
  // while the call is audio-only; a group video call publishes the camera
  // at connect, so the toggle gates the live video track).
  createEffect(() => {
    const local = callUi.participants().find((p) => p.isLocal);
    if (local === undefined) return;
    if (activeCall !== null) {
      if (local.camOn) {
        // 1×1 mid-call camera-on (piece B2b): addLocalVideo acquires + adds a
        // video track + renegotiates the first time (a call started
        // audio-only), and re-enables an existing sender after. Idempotent +
        // in-flight-guarded, so a redundant reactive tick is a cheap no-op. On
        // a capture failure revert the toggle + surface a message — fail-safe,
        // the call stays up audio-only.
        void activeCall.addLocalVideo().catch((err: unknown) => {
          callActions.setCamera(false);
          setError(
            `Could not start the camera: ${
              err instanceof Error ? err.message : String(err)
            }`,
          );
        });
      } else {
        activeCall.setCameraEnabled(false);
      }
    }
    const h = sfuHandle();
    if (h !== null) {
      if (local.camOn) {
        // Group mid-call camera-on (piece B): publishVideo acquires the camera
        // + renegotiates the first time (a call started audio-only), and
        // re-enables an existing sender after. Idempotent + in-flight-guarded,
        // so a redundant reactive tick is a cheap no-op. On a capture failure
        // (permission denied / no camera) revert the toggle + surface a
        // message — fail-safe, the call stays up audio-only (invariant #4).
        void h.publishVideo().catch((err: unknown) => {
          callActions.setCamera(false);
          setError(
            `Could not start the camera: ${
              err instanceof Error ? err.message : String(err)
            }`,
          );
        });
      } else {
        h.setCameraEnabled(false);
      }
    }
  });

  // B2a — mirror the ACTIVE call's mid-call camera-on lifecycle into the
  // shared store, so one videoPublishState surface drives the camera-button
  // spinner + the self-tile "Starting camera…" hint regardless of call kind.
  // Reads the SFU group handle's state when a group call is live, else the
  // 1×1 CallSession's; both re-establish reactively when `sfuHandle()` flips.
  createEffect(() => {
    const h = sfuHandle();
    const state: VideoPublishState =
      h !== null ? h.videoPublishState() : oneToOneVideoState();
    callActions.setVideoPublishState(state);
  });

  // Keep the remote tile's camera state in sync after connect: the
  // 1×1 roster is set on the state transition, but the peer's video
  // track can arrive a beat later (a separate `ontrack`). When
  // `remoteHasVideo` flips, patch just the remote participant.
  createEffect(() => {
    if (callState() !== "connected") return;
    callActions.setParticipantCamera("peer", remoteHasVideo());
  });

  /** Build the outbound-signaling callback that puts raw bytes onto the chat
   *  group's DS journal as an MLS application message.
   *
   *  This used to seal + publish here, and it was the only
   *  `createApplicationMessage` call site outside `chat/client.ts` — no MLS
   *  lock, no durable persist. It now routes through `broadcastControl`, the
   *  same locked, fail-closed-persisting seam every other application message
   *  takes; see `chat/call-signaling-publish.ts` for the two hazards that
   *  bypass reopened, and the natives' matching route. */
  function publishSignalingFor(group: ActiveGroup): (bytes: Uint8Array) => void {
    return makeCallSignalingPublisher({
      chat,
      identity: () => props.requireIdentity(),
      group: () => group.group,
    });
  }

  function buildCallSession(callId: Uint8Array): CallSession {
    return new CallSession(callId, {
      onRemoteStream: (s) => {
        setRemoteAudioStream(s);
        // `ontrack` fires per track (audio, then video); re-check each
        // time so the remote video tile lights up when its track arrives.
        setRemoteHasVideo(s.getVideoTracks().length > 0);
      },
      onLocalStream: (s) => setLocalCallStream(s),
      // B2a — mid-call camera-on lifecycle. The effect below mirrors this
      // into the shared store (camera-button spinner + self-tile hint).
      onVideoPublishState: (s) => setOneToOneVideoState(s),
      onOutboundSignaling: publishSignalingFor(props.entry),
      onStateChange: (s, reason) => {
        setCallState(s);
        if (reason !== undefined) setCallEndReason(reason);
        // G3-3-web-session-bind — drive the full Call.tsx UI store.
        syncCallUiFromState(s);
        if (s === "done") {
          activeCall = null;
          setRemoteAudioStream(null);
          setLocalCallStream(null);
          setVideoCall(false);
          setRemoteHasVideo(false);
          setRemoteWantsVideo(false);
          setOneToOneVideoState("off");
          // G3-3-conn-quality — drop the stale sample so the
          // next call doesn't briefly flash the previous one.
          setCallConnStats(null);
        }
      },
      // G3-3-conn-quality — live RTT / bitrate / relay sample,
      // pushed every 2 s while the call is connected. Also feeds
      // the full Call.tsx connection-detail popover + quality dot.
      onConnectionStats: (stats) => {
        setCallConnStats(stats);
        callActions.setConnectionDetail({
          rttMs: stats.rttMs,
          downBps: stats.downBps,
          upBps: stats.upBps,
          connectionType: stats.relayed
            ? "relay"
            : stats.rttMs !== null
              ? "direct"
              : "unknown",
        });
        callActions.setQuality("local", qualityTierForRtt(stats.rttMs));
      },
    });
  }

  async function onCallClicked(): Promise<void> {
    await startOneToOneCall(false);
  }

  async function onVideoCallClicked(): Promise<void> {
    await startOneToOneCall(true);
  }

  async function startOneToOneCall(withVideo: boolean): Promise<void> {
    if (activeCall !== null) return;
    setVideoCall(withVideo);
    const session = buildCallSession(mintCallId());
    activeCall = session;
    try {
      await session.dial({
        audio: true,
        video: withVideo,
        // Honour the in-call device picker's current selection at capture
        // time; `null` falls back to the OS default inside CallSession.
        audioDeviceId: callUi.settings().selectedAudioInput,
        videoDeviceId: callUi.settings().selectedVideoInput,
      });
    } catch (err) {
      setError(
        `Could not start call: ${err instanceof Error ? err.message : String(err)}`,
      );
      activeCall = null;
      setVideoCall(false);
    }
  }

  // SFU multi-party (group) call. Routed for group chats (>2 members);
  // 1×1 stays on `onCallClicked` (P2P DTLS-SRTP). Derives the per-call
  // SFrame seed + leaf from the MLS group, mints the call-admission JWT
  // bound to the live session token, and drives the shared call-UI
  // store through `createSfuCallUi`. Fail-closed throughout: any missing
  // prerequisite (SFU URL / session / seed) aborts before media.
  async function startGroupCall(withVideo: boolean): Promise<void> {
    if (sfuHandle() !== null || activeCall !== null) return;
    const base = sfuBaseUrl();
    if (base === null) {
      setError(
        "Group calls need an SFU endpoint — set VITE_LAKKO_SFU_URL and rebuild.",
      );
      return;
    }
    const id = chats.identity();
    const secrets = auth.identitySecrets();
    if (id === null || secrets === null) {
      setError("Re-authenticate to start a group call (no live session token).");
      return;
    }
    try {
      const wasm = await loadWasmForCalls();
      const callIdBytes = mintCallId();
      const callIdHex = hexOf(callIdBytes);
      // Per-call SFrame seed + this client's leaf. Deterministic per MLS
      // epoch (audit-pack C-1) so every participant derives identical
      // bytes. `ownLeafIndex` is the KID this client stamps into frames.
      const seed = props.entry.group.exportSecret(
        id,
        "lakko/call/v1",
        callIdBytes,
        32,
      );
      const localLeaf = props.entry.group.ownLeafIndex;
      // Canonical call safety number — the same string every participant
      // derives from the shared seed, shown in the E2EE pill for an
      // out-of-band man-in-the-middle check. Display-only + non-critical:
      // a derivation failure must never abort the call.
      let safetyNumber: string | undefined;
      try {
        safetyNumber = wasm.callSafetyNumber(seed);
      } catch {
        safetyNumber = undefined;
      }
      // Call-admission JWT (JWS Compact, EdDSA) bound to the session.
      const jwt = new CallJwtClient(wasm);
      jwt.setSessionToken(secrets.sessionToken);
      const minted = await jwt.mint(callIdHex, "participant");
      // ADR-0021 — precompute the per-call peer_tag -> contact name map
      // once, so the SFU roster resolves to real contact names instead of
      // a pseudonym. The tag is derived locally (the SFU never sends a
      // name); a peer with no matching contact keeps the pseudonym slice.
      const tagToName = new Map<string, string>();
      // ADR-0021: on a (vanishingly rare) tag collision between two
      // contacts, do NOT show one of their names — fall back to the
      // pseudonym, so a collision never surfaces a wrong contact name.
      const collidedTags = new Set<string>();
      for (const c of loadAccepted()) {
        if (c.accountIdHex !== undefined) {
          const tag = wasm.sfuPeerTag(callIdHex, c.accountIdHex);
          if (tagToName.has(tag)) {
            collidedTags.add(tag);
          } else {
            tagToName.set(tag, c.displayName);
          }
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
      const handle = createSfuCallUi(
        {
          sfuBaseUrl: base,
          jwt: minted.jwt,
          seed,
          localLeaf,
          media: {
            audio: true,
            // A group video call publishes the camera at connect (the session
            // SFrame-seals the video sender like the audio one); an audio call
            // leaves it off. The in-call camera toggle gates the live track.
            video: withVideo,
            // Honour the in-call device picker's current selections at publish
            // time (an `{ exact }` deviceId constraint, else the OS default).
            audioDeviceId: callUi.settings().selectedAudioInput,
            videoDeviceId: callUi.settings().selectedVideoInput,
          },
        },
        { localDisplayName: "You", safetyNumber, resolveDisplayName },
      );
      activeCallId = callIdBytes;
      setSfuHandle(handle);
      setSfuActive(true);
      await handle.connect();
    } catch (err) {
      setError(
        `Could not start group call: ${err instanceof Error ? err.message : String(err)}`,
      );
      sfuHandle()?.hangUp();
      setSfuHandle(null);
      setSfuActive(false);
    }
  }

  async function onHangUp(): Promise<void> {
    if (activeCall === null) return;
    await activeCall.hangUp("hungUp");
  }

  /** Exit the in-call overlay — tears down whichever session is live
   *  (SFU group call or 1×1 P2P). Wired to `Call`'s `onExit`. */
  function onCallExit(): void {
    const h = sfuHandle();
    if (h !== null) {
      h.hangUp();
      setSfuHandle(null);
      setSfuActive(false);
      return;
    }
    void onHangUp();
  }

  async function onAcceptCall(): Promise<void> {
    if (activeCall === null) return;
    await activeCall.accept();
  }

  /**
   * Accept an incoming 1×1 call joining with the camera SENDING (the symmetric /
   * FaceTime-style affordance). The answerer captures audio-only at accept time
   * (see `applyIncomingSignaling`); opting into video is `accept()` + camOn=true,
   * which drives the SAME shipped `addLocalVideo` mid-call camera-on path the
   * caller uses — no duplicated capture / renegotiation logic. `setVideoCall`
   * BEFORE accept seeds the synthesised local participant's `camOn` so the
   * connecting-transition roster carries it; `setCamera(true)` after reinforces
   * once the store has a live local participant. The camera-sync effect then
   * fires `addLocalVideo` (fail-safe: a capture failure reverts the toggle and
   * surfaces a message — the call stays up audio-only).
   */
  async function onAcceptWithVideo(): Promise<void> {
    if (activeCall === null) return;
    setVideoCall(true);
    await activeCall.accept();
    callActions.setCamera(true);
  }

  async function onDeclineCall(): Promise<void> {
    if (activeCall === null) return;
    await activeCall.decline();
  }

  // Route every inbound callSignaling row to the active
  // session (or spin a fresh answerer session up for a
  // first-seen offer).
  createEffect(() => {
    const list = rows();
    for (const row of list) {
      if (row.kind !== "incoming") continue;
      const ev = row.event;
      if (ev.kind !== "callSignaling") continue;
      const bytes = ev.payloadBytes;
      void (async (): Promise<void> => {
        try {
          const wasm = await loadWasmForCalls();
          const parsed = wasm.parseCallSignaling(bytes);
          if (activeCall === null) {
            // Fresh inbound offer — spin up an answerer
            // session bound to the offer's call_id.
            if (parsed.kind !== "offer") return;
            const session = buildCallSession(parsed.callId);
            activeCall = session;
            // Surface whether the offerer advertised video, so the ringing
            // banner can offer "Accept with video" (else audio-only "Accept").
            setRemoteWantsVideo(parsed.mediaVideo);
          }
          // Ignore stale signaling targeting a different
          // call_id.
          if (
            activeCall.callId.length === parsed.callId.length &&
            !uint8ArrayEquals(activeCall.callId, parsed.callId)
          ) {
            return;
          }
          await activeCall.applyIncomingSignaling(parsed);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("inbound call signaling failed", err);
        }
      })();
    }
  });

  onCleanup(() => {
    if (activeCall !== null) {
      activeCall.dispose();
      activeCall = null;
    }
    const h = sfuHandle();
    if (h !== null) {
      h.hangUp();
      setSfuHandle(null);
      setSfuActive(false);
    }
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
    const draft = sync.drafts()[hexOf(groupIdBytes())];
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
        .setDraft(groupIdBytes(), text)
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
      }
    | null
  >(null);
  // Cached per-blob_id plaintext blobs for inline preview +
  // re-download. Keyed by hex(blob_id). Re-decrypt is cheap (no
  // network round-trip after the first hit) but caching keeps
  // a clicked image from re-fetching on every render.
  const [decryptedCache, setDecryptedCache] = createSignal<
    Map<string, { bytes: Uint8Array; objectUrl: string | null }>
  >(new Map());
  // 1.18g — Add-member panel state. Inline form expands below
  // the chat header when toggled; collapses after a successful
  // add or explicit Cancel.
  const [addMemberOpen, setAddMemberOpen] = createSignal(false);
  const [addMemberInput, setAddMemberInput] = createSignal("");
  // ADR-0028 (G-3) — the minted group invite-link (the owner/admin
  // "create invite link" affordance), or `null` before it is generated.
  const [inviteLink, setInviteLink] = createSignal<string | null>(null);
  // `memberCount` mirrors `props.entry.group.memberCount` into
  // a reactive signal so the header re-renders after
  // `addMember`. Without this the WASM-side property read in
  // JSX wouldn't re-evaluate on a render driven by a different
  // signal — Solid's fine-grained reactivity only tracks signal
  // accesses, not arbitrary object property reads.
  const [memberCount, setMemberCount] = createSignal(
    props.entry.group.memberCount,
  );
  // 1.18h — members panel state. `membersVersion` is a
  // monotonic counter bumped on every epoch advance (refresh
  // success, addMember success, removeMember success); the
  // `members` createMemo re-reads `group.members()` whenever
  // it changes. Same trick as `memberCount` — Solid only
  // tracks signal reads, not WASM-side property accesses.
  const [membersOpen, setMembersOpen] = createSignal(false);
  const [membersVersion, setMembersVersion] = createSignal(0);
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
  const acceptedByPubkey = createMemo<Map<string, string>>(() => {
    membersVersion();
    const map = new Map<string, string>();
    for (const c of loadAccepted()) {
      map.set(c.signaturePubkeyB64, c.displayName);
    }
    return map;
  });
  const members = createMemo<readonly { leafIndex: number; sigPubkey: Uint8Array }[]>(
    () => {
      // Subscribe to the version signal so the memo re-runs.
      membersVersion();
      const arr = props.entry.group.members() as Array<{
        leafIndex: number;
        signaturePublicKey: Uint8Array;
      }>;
      return arr.map((m) => ({
        leafIndex: m.leafIndex,
        sigPubkey: m.signaturePublicKey,
      }));
    },
  );

  // G-18 / ADR-0029 — mid-call call-key rotation. When the group's MLS epoch
  // advances during a live SFU call (`membersVersion` bumps after the receive
  // loop / add / remove / external-join applies a commit), re-derive the
  // per-call seed at the NEW epoch and rekey every attached SFrame transform.
  // The worker keeps the previous key for the overlap window so in-flight
  // frames sealed under the old epoch still open, then drops it (forward
  // secrecy). Fail-closed: a derivation failure leaves the call on the current
  // seed — never a downgrade.
  createEffect(() => {
    membersVersion(); // reactive dep — fires on every epoch advance
    const handle = sfuHandle();
    const id = chats.identity();
    if (handle === null || activeCallId === null || id === null) return;
    try {
      const newSeed = props.entry.group.exportSecret(
        id,
        "lakko/call/v1",
        activeCallId,
        32,
      );
      handle.rekey(newSeed);
    } catch {
      // Best-effort — keep the current seed on a re-derivation failure.
    }
  });

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
      const identity = wasm.Identity.fromSeed(secrets.accountIdentitySeed);
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
  async function actingCommitterNow(ownRank: number): Promise<ActingCommitter> {
    const s = auth.state();
    if (s.kind !== "session") return { kind: "defer", toRank: undefined };
    const wasm = await loadWasmForCalls();
    return decideActingCommitter({
      accountIdB64: s.account.accountId,
      groupIdB64: base64Encode(props.entry.group.groupId),
      ownRank,
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
      fold: (rank, observed, windowMs) => {
        const verdict = wasm.actingCommitterVerdict(
          rank,
          // wasm-bindgen CONSUMES each observation, which is why they are
          // built fresh here on every evaluation rather than cached — and
          // which is also what ranking-at-verdict-time requires anyway.
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
          // The verdict is a wasm-owned object; this runs on every poll of
          // every open group, so leaking one per tick is not academic.
          verdict.free();
        }
      },
    });
  }

  /**
   * ADR-0045 S4b — emit this device's own liveness beacon.
   *
   * **The foreground gate is the load-bearing part and it is a caller
   * obligation the core cannot check.** A beacon means *able to sweep*, and on
   * this client a sweep runs only at the tail of a poll driven by this
   * component while the tab is visible — the ADR-0041 background decrypt is
   * read-only-throwaway and structurally cannot commit (C-59). A beacon from a
   * hidden tab would therefore be a lie that costs the group a takeover: every
   * other candidate would defer to a device that is not going to sweep, and
   * would keep deferring for as long as the hidden tab kept beaconing.
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
      const secrets = props.requireIdentity();
      const identity = wasm.Identity.fromSeed(secrets.accountIdentitySeed);
      try {
        ownRank = props.entry.group.ownCommitterRank(identity);
      } finally {
        identity.free();
      }
    } catch {
      // No identity / torn group — this device cannot establish that it is a
      // candidate, so it must not claim to be one.
      return;
    }
    const verdict = beaconEmitVerdict({
      ownRank,
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
        (id) => chat.markOwnPublishedCommit(id),
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

    setMemberCount(props.entry.group.memberCount);
    setMembersVersion(membersVersion() + 1);

    const s = auth.state();
    if (s.kind === "session") {
      const after = memberSignatureKeys(props.entry.group);
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
        leaveGroupConfirmation({ groupLabel: props.entry.label }),
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
      setNotice(
        "Leave requested. It applies when an owner or an admin applies the change.",
      );
      void refresh();
    } catch (err) {
      setError(formatError(err));
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
  function noteOwnRequest(
    key: string,
    transferToB64?: string,
    joinRequest?: boolean,
  ): void {
    const s = auth.state();
    if (s.kind !== "session") return;
    recordObservation(
      s.account.accountId,
      base64Encode(props.entry.group.groupId),
      key,
      Date.now(),
      true,
      transferToB64,
      joinRequest,
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
    let roster: RosterMemberView[] | null;
    try {
      roster = (
        props.entry.group.members() as Array<{
          leafIndex: number;
          signaturePublicKey: Uint8Array;
        }>
      ).map((m) => ({
        leafIndex: m.leafIndex,
        signatureKeyB64: base64Encode(m.signaturePublicKey),
      }));
    } catch {
      roster = null;
    }
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
    const identity = props.requireIdentity();
    const group = props.entry.group;
    switch (request.kind) {
      case "add": {
        const out = await contacts.addMemberToGroup(
          identity,
          group,
          base64Decode(request.subjectAccountIdB64),
          base64Decode(request.subjectKeyB64),
        );
        chat.markOwnPublishedCommit(
          out.kind === "committed" ? out.commitMessageId : out.proposalMessageId,
        );
        if (out.kind === "proposed") noteOwnRequest(`add:${request.subjectKeyB64}`);
        return;
      }
      case "remove":
      case "leave": {
        const leaf = rosterViews().find(
          (m) => m.signatureKeyB64 === request.subjectKeyB64,
        )?.leafIndex;
        // The verdict already dropped a request whose subject has left, so an
        // unresolvable leaf here is a torn read between the verdict and now.
        // Refusing to guess is the only safe answer: any fallback leaf names
        // somebody, and for a removal that somebody is a real person.
        if (leaf === undefined) return;
        const out = await contacts.removeMemberFromGroup(identity, group, leaf);
        chat.markOwnPublishedCommit(
          out.kind === "committed" ? out.commitMessageId : out.proposalMessageId,
        );
        if (out.kind === "proposed") {
          noteOwnRequest(`${request.kind === "leave" ? "leave" : "remove"}:${leaf}`);
        }
        return;
      }
      case "approve-join": {
        // The joiner's signature key IS the `proposal_ref` handle, so the
        // descriptor reconstructs the call whole. The KeyPackage it needs is
        // the one the core retained — which is also why the verdict refuses
        // to fire once that request has left the quarantine.
        const out = await contacts.approveExternalJoin(
          identity,
          group,
          base64Decode(request.subjectKeyB64),
        );
        chat.markOwnPublishedCommit(out.proposalMessageId);
        // Identical bookkeeping to the manual `onApproveJoin`, third argument
        // included: only this device can know the add came from a JOIN
        // request, and that flag is what later says `Ask them to request to
        // join again` instead of offering a retry nobody can perform.
        noteOwnRequest(`add:${request.subjectKeyB64}`, undefined, true);
        // `unkeptJoinRequest` is deliberately NOT cleared here. The manual
        // handler clears it as feedback for something the user just did; a
        // background re-send is not that, and the notice's action item — ask
        // that person to send their request again — stays true regardless,
        // because a request the quarantine never kept is not recoverable by
        // approving a different one.
        return;
      }
      case "set-admin": {
        const out = await contacts.setMemberAdmin(
          identity,
          group,
          base64Decode(request.subjectKeyB64),
          request.makeAdmin,
        );
        chat.markOwnPublishedCommit(
          out.kind === "committed" ? out.commitMessageId : out.proposalMessageId,
        );
        if (out.kind === "proposed") noteOwnRequest("role-change");
        return;
      }
      default: {
        const out = await contacts.transferGroupOwnership(
          identity,
          group,
          base64Decode(request.subjectKeyB64),
          request.outgoing,
        );
        chat.markOwnPublishedCommit(
          out.kind === "committed" ? out.commitMessageId : out.proposalMessageId,
        );
        if (out.kind === "proposed") {
          noteOwnRequest("role-change", request.subjectKeyB64);
        }
      }
    }
  }

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
          setError(formatError(err));
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
   * **What it removes:** this group's entry in the per-account message log
   * (a read-modify-write on a store that already exists — the log is keyed
   * `{ groupIdHex -> PersistedGroup }` internally, so no new store is needed),
   * its pending-request observations, its durable leave record, and its row in
   * the in-memory group registry.
   *
   * **What it does NOT remove, which is why the confirmation body is exact
   * rather than sweeping:** the inactive MLS group stays in the provider
   * snapshot as inert bytes. `Group` exposes no delete, so a `Group::delete`
   * is a real (small) core follow-up rather than something this rides free.
   * The remnant is inert in the strong sense — it cannot send, decrypt, export
   * or process anything — but it is not nothing.
   *
   * The push subscription is already gone by this point: the §6.2.8 teardown
   * retires it at the moment of the eviction, not at the moment of the delete,
   * so a user who never presses this button still stops being woken.
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
        deleteChatConfirmation({ chatLabel: props.entry.label }),
      ))
    ) {
      return;
    }
    const accountIdB64 = s.account.accountId;
    const groupIdB64 = base64Encode(props.entry.group.groupId);
    setError(null);
    setBusy(true);
    try {
      const deviceKey = await getDeviceKey(accountIdB64);
      if (deviceKey !== null) {
        await deleteGroupFromMessageLog(
          accountIdB64,
          deviceKey,
          groupIdHexForLog(),
        );
      }
      // Local bookkeeping for a group that no longer exists here. Both are
      // best-effort by construction and neither can fail the delete.
      pruneObservations(accountIdB64, groupIdB64, []);
      clearUnappliedForGroup(accountIdB64, groupIdB64);
      clearLeaveRequested(accountIdB64, groupIdB64);
      // ADR-0045 UX §7 — an armed re-send names a group that is gone from
      // here. The verdict would drop it anyway on `membershipEnded`, so this
      // is tidiness rather than a gate; it is here because leaving a record
      // behind for a deleted chat is the kind of residue that later reads as
      // a bug.
      clearArmedRetries(retryScopeKey(accountIdB64, groupIdB64));
      // Drop the row from the registry LAST, so a failure above leaves the
      // chat reachable rather than orphaning a transcript behind a row the
      // user can no longer open.
      chats.remove(groupIdB64);
      props.onLeave();
    } catch (err) {
      setError(formatError(err));
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
    // ADR-0045 S7 residual 1 — an own `add` that has left the pending set
    // without reaching the roster was not carried by whatever commit emptied
    // the set, and nothing will ever carry it. Detected HERE, in the same pass
    // that prunes, because the observation about to be dropped is the only
    // record that the request existed at all. The roster is read in this same
    // pass for the same reason: the merge that empties the pending set is the
    // merge that moves the roster.
    let rosterKeysB64: string[] | null;
    try {
      rosterKeysB64 = (
        props.entry.group.members() as Array<{ signaturePublicKey: Uint8Array }>
      ).map((m) => base64Encode(m.signaturePublicKey));
    } catch {
      rosterKeysB64 = null;
    }
    recordUnapplied(
      s.account.accountId,
      groupIdB64,
      unappliedAddRequests(observations, liveKeys, rosterKeysB64),
    );
    pruneObservations(s.account.accountId, groupIdB64, liveKeys);
    return visibleRequests(requests, canManageMembers());
  });

  /** ADR-0045 S7 residual 1 — the rows the user has not dismissed. Read through
   *  the same `pendingVersion` the detection writes under, so a newly detected
   *  row appears without a second trigger. */
  const unappliedAdds = createMemo<UnappliedAdd[]>(() => {
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
  function onDismissUnapplied(addSignatureKeyB64: string): void {
    const s = auth.state();
    if (s.kind !== "session") return;
    dismissUnapplied(
      s.account.accountId,
      base64Encode(props.entry.group.groupId),
      addSignatureKeyB64,
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
    if (keyB64 === undefined) return "(unknown)";
    const name = acceptedByPubkey().get(keyB64);
    if (name !== undefined && name.length > 0) return name;
    try {
      return shortFingerprint(base64Decode(keyB64));
    } catch {
      return "(unknown)";
    }
  }

  // ADR-0028 OQ2 — resolve which member's `/g/` link each pending request
  // arrived on. Per `membersVersion`, read the journal's join-attribution
  // envelopes and build a `joinerKeyB64 -> { issuerFingerprint, issuerInRoster }`
  // map; `issuerInRoster` checks the attribution's issuer pubkey against the live
  // roster. Advisory metadata for the admin — the role-gated approval gate is the
  // boundary, so a missing / failed lookup never blocks Approve.
  const [joinAttributions, setJoinAttributions] = createSignal<
    Map<string, { issuerFingerprint: string; issuerInRoster: boolean }>
  >(new Map());
  createEffect(() => {
    membersVersion();
    if (!canManageMembers()) return;
    const gid = props.entry.group.groupId;
    const rosterB64 = new Set(members().map((m) => base64Encode(m.sigPubkey)));
    void (async () => {
      try {
        const attrs = await contacts.fetchJoinAttributions(gid);
        const map = new Map<string, { issuerFingerprint: string; issuerInRoster: boolean }>();
        for (const a of attrs) {
          map.set(base64Encode(a.joinerSignatureKey), {
            issuerFingerprint: a.issuerFingerprint,
            issuerInRoster: rosterB64.has(base64Encode(a.issuerSignaturePubkey)),
          });
        }
        setJoinAttributions(map);
      } catch {
        // best-effort advisory metadata — leave the map as-is
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
    const name = acceptedByPubkey().get(identity);
    if (name !== undefined && name.length > 0) return name;
    const m = members().find((mm) => base64Encode(mm.sigPubkey) === identity);
    return m === undefined ? "A member" : shortFingerprint(m.sigPubkey);
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
      const name = acceptedByPubkey().get(keyB64);
      return {
        leafIndex: m.leafIndex,
        signatureKeyB64: keyB64,
        label:
          name !== undefined && name.length > 0
            ? name
            : shortFingerprint(m.sigPubkey),
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
      setNotice(
        transferOutcomeNotice(
          landed ? "committed" : "proposed",
          successor.label,
          outgoing,
        ),
      );
      void refresh();
    } catch (err) {
      setError(formatError(err));
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
    // header already shows who it is).
    const oneOnOne = memberCount() <= 2;
    if (typers.length === 1) {
      const who = typers[0] as MemberIdentity;
      return oneOnOne ? "typing…" : `${labelForIdentity(who)} is typing…`;
    }
    return "Several people are typing…";
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
    // in ChatClient via ownPublishedIds, so duplicates here
    // are rare).
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
    let watermarks = readWatermarks();
    let watermarksChanged = false;
    const nowMs = Date.now();
    for (const e of events) {
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
          if (e.senderKey !== undefined) {
            const next = foldReadWatermark(watermarks, e.senderKey, e.target);
            if (next !== watermarks) {
              watermarks = next;
              watermarksChanged = true;
            }
          }
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
    if (watermarksChanged) {
      setReadWatermarks(watermarks);
    }
    // ADR-0045 — a proposal in this batch changed what the pending-request
    // panel should show. Bumped here rather than at either call site, so both
    // receive paths refresh it identically.
    if (sawProposal) setPendingVersion(pendingVersion() + 1);
  }

  async function refresh(): Promise<void> {
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
    // the Welcome intake) and there is no single interval to clear.
    //
    // The cursor is deliberately left where the last applied row put it
    // (§6.2.13): it costs nothing, and whether a rejoin wants it reset is the
    // rejoin path's question, not this one's.
    if (membershipEnded()) return;
    setError(null);
    setBusy(true);
    try {
      // ADR-0043 — baseline for the membership diff, captured BEFORE the poll
      // applies any commit. A Commit carries no membership delta out of the
      // poll loop, so a removal committed by SOMEONE ELSE is inferred from
      // this snapshot against the post-poll one. This device's OWN removals
      // never show up here (they were applied locally before this poll, so
      // the baseline is already post-removal); those rotate directly in
      // `onRemoveMember`.
      const membersBeforePoll = memberSignatureKeys(props.entry.group);
      const result = await chat.pollMessages(
        props.requireIdentity(),
        props.entry.group,
        cursor(),
        0,
        // G-38 — the poll's apply half runs under the same per-group MLS lock
        // the commit paths take, so the cursor read-modify-write has to run in
        // there with it. Handing the accessors in is what lets `ChatClient`
        // re-read the position under the lock (was this batch consumed while we
        // were on the network?) and advance it in the same region as the apply.
        // Writing `result.cursor` back out here would land a microtask AFTER
        // the lock released, which is exactly the window a queued poll used to
        // re-read and re-feed already-consumed rows through.
        { read: () => cursor(), advance: (to) => setCursor(to) },
      );
      // The batch was stale (another poll consumed past our `after` while we
      // were parked on the fetch). Nothing was applied and the cursor was not
      // touched — drop it. The poll that DID apply owns rendering those rows.
      if (result.outcome === "cursor-moved") return;
      // ADR-0045 — the shared consumer; see `consumeEvents`.
      consumeEvents(result.events);
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
      setMemberCount(props.entry.group.memberCount);
      // 1.18h — bump membersVersion so the members panel's
      // createMemo re-reads the post-poll leaf set.
      setMembersVersion(membersVersion() + 1);
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
    } catch (err) {
      setError(formatError(err));
    } finally {
      setBusy(false);
    }
  }

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
      },
    ]);
    setInput("");
    setPendingAttachments([]);

    try {
      // 1.19c2: compose the JSON envelope from (text, refs).
      // Empty refs ⇒ pure-text envelope; non-empty ⇒ multi-
      // attachment envelope via the wasm EnvelopeBuilder.
      const envelopeBytes = await composeEnvelope(text, attached);
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
      // Roll back the optimistic bubble + restore the
      // attachments to the composer so the user can retry
      // without re-picking the same files.
      setRows(
        rows().filter((r) => !(r.kind === "outgoing" && r.tempId === tempId)),
      );
      setPendingAttachments(attached);
      setError(formatError(err));
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
      const envelopeBytes = await composeEditEnvelope(messageId, newText);
      await chat.sendMessage(
        props.requireIdentity(),
        props.entry.group,
        envelopeBytes,
      );
      // Apply locally (no echo). Author == us, so the gate the
      // receive loop runs would accept it; we mirror that here.
      setMsgOps(applyOp(msgOps(), { op: "edit", target: messageId, text: newText }));
    } catch (err) {
      setError(formatError(err));
    }
  }

  /** Delete our own message — publish a delete op + tombstone it
   *  locally. */
  async function onDeleteMessage(messageId: number): Promise<void> {
    setError(null);
    try {
      const envelopeBytes = await composeDeleteEnvelope(messageId);
      await chat.sendMessage(
        props.requireIdentity(),
        props.entry.group,
        envelopeBytes,
      );
      setMsgOps(applyOp(msgOps(), { op: "delete", target: messageId }));
    } catch (err) {
      setError(formatError(err));
    }
  }

  /** React to any message with `emoji` — publish a reaction op +
   *  toggle it locally under our own leaf. Re-reacting with the
   *  same emoji toggles it off (the op + the local apply are both
   *  toggles, so they stay in sync). */
  async function onReact(messageId: number, emoji: string): Promise<void> {
    setReactPickerId(null);
    setError(null);
    try {
      const envelopeBytes = await composeReactionEnvelope(messageId, emoji);
      await chat.sendMessage(
        props.requireIdentity(),
        props.entry.group,
        envelopeBytes,
      );
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
      setError(formatError(err));
    }
  }

  // ----- ADR-0024 typing + read-receipt send path ----------------
  //
  // Typing + read receipts ride the same application send path as
  // chat / edit / etc. (`chat.sendMessage`), so the DS echo is
  // dropped via `ownPublishedIds` exactly like the ADR-0023 ops.
  // They are best-effort fire-and-forget — a transient failure must
  // never wedge the composer or surface a user-facing error (the
  // signals are non-essential status). They render no optimistic
  // bubble.

  /** Publish an already-composed ephemeral envelope (typing /
   *  read-receipt) without an optimistic bubble. Swallows failures
   *  (these signals are non-essential; the next one supersedes a
   *  dropped one). */
  async function publishEphemeral(envelopeBytes: Uint8Array): Promise<void> {
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
  // Driven by its own timer rather than by the poll, because the poll on this
  // client is event-driven (mount, a user action, the Refresh button) while a
  // beacon's whole meaning is a CADENCE: "still here". The tick is cheap and
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
          if (secret !== null) await retireOldPushGid(secret);
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
          const ref = await encryptAndUploadFile(
            file,
            undefined,
            (stage) => {
              setUploadProgress({
                fileIndex: i,
                total: list.length,
                fileName: file.name,
                stage,
              });
            },
          );
          setPendingAttachments([...pendingAttachments(), ref]);
        } catch (err) {
          if (err instanceof AttachmentSizeError) {
            setError(
              `"${file.name}" is ${formatAttachmentSize(err.got)} — over the ${formatAttachmentSize(err.max)} limit.`,
            );
          } else {
            setError(`upload of "${file.name}" failed: ${formatError(err)}`);
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
        setError(
          "The attachment is no longer available on the server.",
        );
      } else {
        setError(`download failed: ${formatError(err)}`);
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
    const key = blobIdHex(ref.blobId);
    const cached = decryptedCache().get(key);
    if (cached !== undefined && cached.objectUrl !== null) {
      // Already cached — the bubble already renders inline.
      // Nothing to do; the Hide affordance on the chip clears
      // the URL.
      return;
    }
    setError(null);
    try {
      const bytes = await downloadAndDecryptRef(ref);
      const blob = new Blob([bytes.slice()], {
        type: ref.mime ?? "application/octet-stream",
      });
      const url = URL.createObjectURL(blob);
      const next = new Map(decryptedCache());
      next.set(key, { bytes, objectUrl: url });
      setDecryptedCache(next);
      // No window.open — the JSX picks up the cached URL via
      // the `getCachedUrl` getter passed to AttachmentChips.
    } catch (err) {
      if (err instanceof AttachmentMissingError) {
        setError("The attachment is no longer available on the server.");
      } else {
        setError(`preview failed: ${formatError(err)}`);
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
    const key = blobIdHex(ref.blobId);
    const cached = decryptedCache().get(key);
    if (cached === undefined || cached.objectUrl === null) return;
    URL.revokeObjectURL(cached.objectUrl);
    const next = new Map(decryptedCache());
    next.set(key, { bytes: cached.bytes, objectUrl: null });
    setDecryptedCache(next);
  }

  /** Cache accessor passed to [`AttachmentChips`] — returns the
   *  cached object URL when the attachment has been previewed
   *  in this session, `null` otherwise. The chip reads this on
   *  every render so the inline image appears reactively once
   *  the cache lands. */
  function getCachedAttachmentUrl(ref: AttachmentRefBytes): string | null {
    return decryptedCache().get(blobIdHex(ref.blobId))?.objectUrl ?? null;
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
  async function onAddMember(): Promise<void> {
    const username = addMemberInput().trim();
    if (username.length === 0) return;
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
      // Suppress the echo so the sender's next pollMessages
      // doesn't try to processIncoming on its own artifact at
      // an epoch we've already advanced past. ADR-0045: the artifact is a
      // PROPOSAL in a role-table group and a commit only in a legacy one.
      chat.markOwnPublishedCommit(
        result.outcome.kind === "committed"
          ? result.outcome.commitMessageId
          : result.outcome.proposalMessageId,
      );
      // ADR-0045 — record when the request landed, so the quiet window is
      // measured against the DS row rather than a local clock, and so this
      // device knows the request is its own (the UX §10 visibility exception).
      if (result.outcome.kind === "proposed") {
        noteOwnRequest(
          `add:${base64Encode(result.recipientSignaturePubkey)}`,
        );
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
      setMemberCount(props.entry.group.memberCount);
      // 1.18h — bump membersVersion so the members panel
      // (when open) re-reads the updated leaf set.
      setMembersVersion(membersVersion() + 1);
      setNotice(
        result.outcome.kind === "committed"
          ? `@${result.recipientUsername} added (commit #${result.outcome.commitMessageId}).`
          : `@${result.recipientUsername} requested. They join when an owner or an admin applies the change.`,
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
      setError(formatError(err));
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
        confirmLabel: namedAction(decided.confirmLabel, memberLabel),
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
          proposed.kind === "committed"
            ? proposed.commitMessageId
            : proposed.proposalMessageId,
        );
        if (proposed.kind === "proposed") noteOwnRequest(`remove:${leafIndex}`);
        const swept = await sweepNow();
        setMemberCount(props.entry.group.memberCount);
        setMembersVersion(membersVersion() + 1);
        setNotice(
          swept?.kind === "committed"
            ? `Member at leaf #${leafIndex} removed (commit #${swept.commitMessageId}).`
            : "Removal requested. It applies when an owner or an admin applies the change.",
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
            chat.markOwnPublishedCommit(removed.commitMessageId);
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
      setMemberCount(props.entry.group.memberCount);
      setMembersVersion(membersVersion() + 1);
      setNotice(
        result.kind === "committed"
          ? `Member at leaf #${leafIndex} removed (commit #${result.commitMessageId}).`
          : "Removal requested.",
      );
      void refresh();
    } catch (err) {
      setError(formatError(err));
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
        setNotice(makeAdmin ? "Promoted to admin." : "Removed as admin.");
      } else {
        setNotice(
          "Change requested. It applies when an owner or an admin applies the change.",
        );
      }
      void refresh();
    } catch (err) {
      setError(formatError(err));
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
      chat.markOwnPublishedCommit(result.proposalMessageId);
      // ADR-0045 — the same bookkeeping every other membership request gets:
      // the quiet window is measured against the DS row, and this device knows
      // the request is its own.
      // The third argument is ADR-0045 S7 residual 1: this add came from a JOIN
      // request, and only this device can know that (on the wire an approved
      // add is indistinguishable from an admin adding a contact). If the sweep
      // never carries it, that flag is what makes the notice say `Ask them to
      // request to join again` rather than offering a retry this side cannot
      // perform — a successful approve consumed the retained request.
      noteOwnRequest(`add:${base64Encode(proposalRef)}`, undefined, true);
      // If this device is the acting committer the sweep runs now, so the
      // common single-admin case still reads as "tap, done".
      const swept = await sweepNow();
      setMemberCount(props.entry.group.memberCount);
      setMembersVersion(membersVersion() + 1);
      if (swept?.kind === "committed") {
        setNotice("Join approved. They have been added to the group.");
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
        const queue = depth === null ? null : waitingQueueLine(depth);
        setNotice(
          "Join approved. They join when an owner or an admin applies the change." +
            (queue === null ? "" : ` ${queue}`),
        );
      }
      // Same reason as in `onRejectJoin`: the queue moved, so the notice
      // about a request it turned away is no longer current.
      setUnkeptJoinRequest(false);
      void refresh();
    } catch (err) {
      setError(formatError(err));
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
      await contacts.rejectExternalJoin(props.entry.group, proposalRef);
      setMembersVersion(membersVersion() + 1);
      // Acting on the queue is what may have made room for the request the
      // quarantine turned away, so the notice about it stops being current.
      setUnkeptJoinRequest(false);
      setNotice("Join request rejected.");
    } catch (err) {
      setError(formatError(err));
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
      const link = await contacts.createGroupInviteLink(
        props.requireIdentity(),
        props.entry.group,
        0n,
        window.location.origin,
      );
      setInviteLink(link);
    } catch (err) {
      setError(formatError(err));
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
      setNotice(
        revoked
          ? "Invite link revoked."
          : "Link cleared (an inline link has nothing to revoke).",
      );
    } catch (err) {
      setError(formatError(err));
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

  onMount(() => {
    void (async (): Promise<void> => {
      await hydrateFromLog();
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
            (id) => chat.markOwnPublishedCommit(id),
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
      // ADR-0045 S4b — announce this device immediately rather than waiting a
      // tick. The core's own rule is that the FIRST beacon of a session is
      // immediate, precisely because a candidate that has just opened the app
      // is when a stale view is likeliest to hand the duty elsewhere.
      void emitBeaconIfDue();
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
      const groupId = groupIdBytes();
      // Encode the messageId as 8 big-endian bytes so the
      // wire shape stays stable regardless of how JS strings
      // serialise BigInt. Receivers compare the raw bytes
      // against their own per-group highest-id.
      const idBytes = new Uint8Array(8);
      let v = target;
      for (let i = 7; i >= 0; i -= 1) {
        idBytes[i] = Number(v & 0xffn);
        v >>= 8n;
      }
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
    }
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
      const slot = log.groups[groupIdHexForLog()];
      if (slot === undefined) return;
      const seeded = slot.messages.map(persistedToTimelineRow);
      // Only seed when the in-memory list is empty — a refresh
      // running concurrently with hydration shouldn't get
      // stomped by the disk load.
      if (rows().length === 0) {
        setRows(seeded);
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
      // Read current log (other groups may have rows that
      // belong to the same account — we don't want to wipe
      // them when writing back). Per-tab serialisation is the
      // common case (one Chat route mounted at a time), so a
      // read-modify-write race against another tab is rare
      // enough to ignore for MVP.
      const existing =
        (await loadMessageLog(accountIdB64, deviceKey)) ??
        ({ version: MESSAGE_LOG_VERSION, groups: {} } as MessageLogPayload);
      const next: MessageLogPayload = {
        // Bumped on write: this device now writes `authorKeyB64`.
        version: MESSAGE_LOG_VERSION,
        groups: {
          ...existing.groups,
          [groupIdHexForLog()]: {
            cursor: String(cursor()),
            messages: rows()
              .map(timelineRowToPersisted)
              .filter((m): m is PersistedMessage => m !== null),
          } satisfies PersistedGroup,
        },
      };
      await saveMessageLog(accountIdB64, deviceKey, next);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("persistToLogBestEffort failed", err);
    }
  }

  return (
    <div
      onDragEnter={onDragEnter}
      onDragOver={onDragOver}
      onDragLeave={onDragLeave}
      onDrop={(e) => {
        void onDrop(e);
      }}
      style={{
        display: "flex",
        "flex-direction": "column",
        gap: "1rem",
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
            Drop files to attach
          </span>
        </div>
      </Show>
      <header
        style={{
          display: "flex",
          "align-items": "center",
          "justify-content": "space-between",
          gap: "0.75rem",
        }}
      >
        <div>
          {/* ADR-0021 follow-up: show the linked contact's user-chosen
              name in the chat header (falls back to the chat label). */}
          <h1 style={{ margin: 0 }}>{resolvePeerName()}</h1>
          <div
            style={{
              color: "var(--surface-muted)",
              "font-size": "0.9rem",
              "margin-top": "0.25rem",
            }}
          >
            {memberCount()} member
            {memberCount() === 1 ? "" : "s"} · seat #
            {props.entry.group.ownLeafIndex}
          </div>
        </div>
        <div style={{ display: "flex", gap: "0.5rem", "flex-wrap": "wrap" }}>
          <button
            type="button"
            onClick={() => {
              setMembersOpen(!membersOpen());
              setNotice(null);
              setError(null);
              // Bump version so the panel reads fresh data
              // when it opens (covers the case where new
              // commits arrived between the last refresh and
              // this click).
              setMembersVersion(membersVersion() + 1);
            }}
            disabled={busy()}
          >
            {membersOpen() ? "Hide members" : "Members"}
          </button>
          {/* ADR-0022 — only owner/admin (or an open/legacy group) sees the
              add affordance; a plain member's add would be refused by peers. */}
          <Show when={canManageMembers()}>
            <button
              type="button"
              onClick={() => {
                setAddMemberOpen(!addMemberOpen());
                setNotice(null);
                setError(null);
              }}
              disabled={busy()}
            >
              {addMemberOpen() ? "Cancel add" : "Add member"}
            </button>
          </Show>
          <button
            type="button"
            onClick={() => {
              void refresh();
            }}
            disabled={busy()}
          >
            {busy() ? "Refreshing…" : "Refresh"}
          </button>
          {/* Phase 3a8 — call button. Visible whenever no
              call is active in this thread.

              ADR-0045 UX §6.2.8 item 4 — and GONE once this device's
              membership has ended. The per-call SFrame seed comes from
              `exportSecret("lakko/call/v1", …)`, which is gated on
              `is_active()` and now raises, so the button fails closed — which
              is correct, and is not enough. A button that always errors is the
              shape §6.1.1 rejects, and §6.2.11 forbids any live indicator on a
              dead group for a stronger reason: a call control asserts a live
              E2EE session, and there is not one. */}
          <Show
            when={
              (callState() === "idle" || callState() === "done") &&
              !sfuActive() &&
              !membershipEnded()
            }
          >
            <button
              type="button"
              onClick={() => {
                // Group chat (>2 members) → SFU + SFrame; 1×1 → P2P.
                if (memberCount() > 2) {
                  void startGroupCall(false);
                } else {
                  void onCallClicked();
                }
              }}
              disabled={busy()}
            >
              📞 Call
            </button>
            {/* Video call. Group (>2) routes through the SFU with the camera
                SFrame-sealed like the audio track; 1×1 is P2P DTLS-SRTP. Same
                memberCount branch as the audio button above. */}
            <button
              type="button"
              onClick={() => {
                if (memberCount() > 2) {
                  void startGroupCall(true);
                } else {
                  void onVideoCallClicked();
                }
              }}
              disabled={busy()}
            >
              📹 Video
            </button>
          </Show>
          <button type="button" onClick={props.onLeave}>
            Back to Contacts
          </button>
        </div>
      </header>

      {/* G3-3-web-session-bind — full in-call UI presented over the
          chat for live call states (the web analog of iOS/Android
          call-present). `ringing` keeps the lightweight inline banner
          below (Accept / Decline) until the call is accepted. */}
      <Show when={callOverlayVisible()}>
        <Call
          embedded={true}
          onExit={(): void => {
            onCallExit();
          }}
          remoteStreamFor={(participantId): MediaStream | undefined => {
            // 1×1 P2P: the sole remote tile id is `peer` → the live P2P
            // remote stream (carries the peer's video track when present).
            if (participantId === "peer") {
              return remoteAudioStream() ?? undefined;
            }
            // SFU group: roster id is `peer-<peerId>`; the SFU sets the
            // downstream MSID stream id to that same peer id, so strip the
            // prefix to look the stream up.
            const peerId = participantId.replace(/^peer-/, "");
            return sfuHandle()?.remoteStreams().get(peerId);
          }}
          localStream={(): MediaStream | undefined =>
            // Self-preview: the SFU group capture when a group call is live,
            // else the 1×1 P2P capture. Only one kind is live at a time.
            sfuHandle()?.localStream() ?? localCallStream() ?? undefined
          }
        />
      </Show>

      {/* Hidden audio sink for the remote stream. Rendered for the
          whole call lifetime (ringing → ending) in a stable slot so
          it does not unmount when the inline banner gives way to the
          full overlay — otherwise remote audio would cut out. */}
      <Show when={callState() !== "idle" && callState() !== "done"}>
        <audio
          ref={(el) => setAudioRef(el)}
          autoplay
          style={{ display: "none" }}
        />
      </Show>

      {/* SFU group-call remote audio — one hidden sink per remote
          MediaStream, keyed by the stream object identity (a stable
          stream keeps its element, so audio does not cut on roster
          churn). Remote VIDEO renders per-tile from the store roster:
          the `remoteStreamFor` prop above resolves a tile's stream via
          the SFU MSID-stream-id-to-peer mapping (`peer-<peerId>`). */}
      <Show when={sfuActive()}>
        <For
          each={[
            ...(
              sfuHandle()?.remoteStreams() ?? new Map<string, MediaStream>()
            ).values(),
          ]}
        >
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
      </Show>

      {/* Phase 3a8 — inline incoming-call banner. Now shown ONLY
          while `ringing` (Accept / Decline); live states use the
          full overlay above. */}
      <Show when={callState() === "ringing"}>
        <div
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
            "box-shadow": "0 2px 8px rgba(0, 0, 0, 0.15)",
          }}
        >
          <div>
            <strong>
              {(() => {
                switch (callState()) {
                  case "outgoing":
                    return "📞 Calling…";
                  case "ringing":
                    return "📞 Incoming call";
                  case "connecting":
                    return "📞 Connecting…";
                  case "connected":
                    return "📞 In call";
                  case "ending":
                    return "📞 Ending…";
                  default:
                    return "📞";
                }
              })()}
            </strong>
            <div style={{ "font-size": "0.85rem", "margin-top": "0.2rem" }}>
              Audio + video · end-to-end encrypted P2P (DTLS-SRTP).
            </div>
            {/* G3-3-conn-quality — live RTT / bitrate / path readout
                from the session's getStats poll. Shown only while
                connected (the sample is meaningless before ICE
                settles). */}
            <Show when={callState() === "connected"}>
              <div
                style={{
                  "font-size": "0.8rem",
                  "margin-top": "0.3rem",
                  display: "flex",
                  "align-items": "center",
                  gap: "0.4rem",
                }}
              >
                <span
                  aria-hidden="true"
                  style={{
                    width: "0.55rem",
                    height: "0.55rem",
                    "border-radius": "50%",
                    "background-color": qualityDotColour(
                      qualityTierForRtt(callConnStats()?.rttMs ?? null),
                    ),
                    display: "inline-block",
                    "flex-shrink": 0,
                  }}
                />
                <span>
                  {(() => {
                    const s = callConnStats();
                    const rtt =
                      s?.rttMs != null ? `${s.rttMs} ms` : "measuring…";
                    const path =
                      s?.relayed === true ? "relay" : s != null ? "direct" : "—";
                    return `${rtt} · ${path} · ↓ ${formatBps(
                      s?.downBps ?? null,
                    )} · ↑ ${formatBps(s?.upBps ?? null)}`;
                  })()}
                </span>
              </div>
            </Show>
          </div>
          <div style={{ display: "flex", gap: "0.4rem" }}>
            <Show when={callState() === "ringing"}>
              <button
                type="button"
                onClick={() => {
                  void onAcceptCall();
                }}
                style={{
                  background: "var(--surface)",
                  color: "var(--surface-fg)",
                  border: "none",
                  padding: "0.5rem 0.9rem",
                  "border-radius": "var(--radius-sm)",
                  "font-weight": "600",
                  cursor: "pointer",
                }}
              >
                Accept
              </button>
              {/* Symmetric / FaceTime-style: a video offer also offers joining
                  with the camera sending. Audio-only offers keep just "Accept". */}
              <Show when={remoteWantsVideo()}>
                <button
                  type="button"
                  onClick={() => {
                    void onAcceptWithVideo();
                  }}
                  style={{
                    background: "var(--surface)",
                    color: "var(--surface-fg)",
                    border: "none",
                    padding: "0.5rem 0.9rem",
                    "border-radius": "var(--radius-sm)",
                    "font-weight": "600",
                    cursor: "pointer",
                  }}
                >
                  Accept with video
                </button>
              </Show>
              <button
                type="button"
                onClick={() => {
                  void onDeclineCall();
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
                Decline
              </button>
            </Show>
            <Show when={callState() !== "ringing"}>
              <button
                type="button"
                onClick={() => {
                  void onHangUp();
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
                Hang up
              </button>
            </Show>
          </div>
        </div>
      </Show>

      <Show when={callState() === "done" && callEndReason() !== null}>
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
          Last call ended ({callEndReason()}).
        </div>
      </Show>

      {/* 1.18g — inline "Add member" form. Toggled via the
         button above. Submit drives
         ContactsClient.addMemberToGroupByUsername, which fetches
         the recipient's KeyPackage, runs Group.addMember,
         broadcasts the commit to existing members via DS, and
         ships the Welcome to the new member's stranger inbox. */}
      <Show when={addMemberOpen()}>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void onAddMember();
          }}
          style={{
            display: "flex",
            gap: "0.5rem",
            padding: "0.75rem",
            border: "1px solid var(--surface-border)",
            "border-radius": "var(--radius-md)",
            background: "var(--surface-elevated)",
          }}
        >
          <input
            type="text"
            placeholder="@username"
            autocomplete="off"
            value={addMemberInput()}
            onInput={(e) => setAddMemberInput(e.currentTarget.value)}
            style={{ flex: 1 }}
            disabled={busy()}
          />
          <button
            class="primary"
            type="submit"
            disabled={busy() || addMemberInput().trim().length === 0}
          >
            {busy() ? "Adding…" : "Add"}
          </button>
        </form>
      </Show>

      {/* 1.18h — members panel. Lists every leaf at the
          current epoch with a fingerprint-short label + Remove
          button. The own-leaf row gets a "(you)" badge instead
          of a Remove button (can't kick yourself). */}
      <Show when={membersOpen()}>
        <section
          aria-label="Members"
          style={{
            padding: "0.5rem 0.75rem",
            border: "1px solid var(--surface-border)",
            "border-radius": "var(--radius-md)",
            background: "var(--surface-elevated)",
          }}
        >
          <h2
            style={{
              "font-size": "0.85rem",
              "text-transform": "uppercase",
              "letter-spacing": "0.05em",
              color: "var(--surface-muted)",
              margin: "0 0 0.5rem 0",
            }}
          >
            Members ({members().length})
          </h2>
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
                {NEW_OWNER_DISMISS}
              </button>
            </div>
          </Show>
          {/* ADR-0028 (G-3) — owner/admin "create invite link" affordance.
              Mints a /g/<slug> link conveying this group's id; a non-member
              opens it to request to join, approved via the pending list above.
              Same role gate as add/remove. */}
          <Show when={canManageMembers()}>
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
                Create invite link
              </button>
              <Show when={inviteLink() !== null}>
                <div
                  style={{
                    display: "flex",
                    gap: "0.3rem",
                    "align-items": "center",
                  }}
                >
                  <input
                    type="text"
                    readonly
                    value={inviteLink() ?? ""}
                    style={{
                      flex: "1",
                      "font-size": "0.75rem",
                      "font-family": "monospace",
                      padding: "0.25rem 0.4rem",
                      "border-radius": "var(--radius-sm)",
                      border: "1px solid var(--surface-border)",
                      background: "var(--surface)",
                      color: "var(--surface-fg)",
                      "min-width": "0",
                    }}
                  />
                  <button
                    type="button"
                    onClick={() => {
                      const l = inviteLink();
                      if (l) void navigator.clipboard?.writeText(l);
                    }}
                    style={{
                      "font-size": "0.75rem",
                      padding: "0.25rem 0.5rem",
                      "border-radius": "var(--radius-sm)",
                      border: "1px solid var(--accent)",
                      background: "var(--accent)",
                      color: "var(--accent-fg)",
                      cursor: "pointer",
                    }}
                  >
                    Copy
                  </button>
                  <button
                    type="button"
                    disabled={busy()}
                    onClick={() => void onRevokeInviteLink()}
                    style={{
                      "font-size": "0.75rem",
                      padding: "0.25rem 0.5rem",
                      "border-radius": "var(--radius-sm)",
                      border: "1px solid var(--surface-border)",
                      background: "transparent",
                      color: "var(--danger, #c0392b)",
                      cursor: "pointer",
                    }}
                  >
                    Revoke
                  </button>
                </div>
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
                Join requests ({pendingJoins().length})
              </h3>
              {/* ADR-0045 S7 residual 2 — the quarantine's one statable
                  property. `Group::pending_external_joins` is in-memory: it is
                  not in the provider snapshot, and the poll cursor has already
                  advanced past the proposals that populated it, so a restart
                  loses the list and no re-fetch brings it back. Ageing rows out
                  of the panel instead would be strictly worse — the request
                  stays live for the joiner, who goes on polling. */}
              <p
                style={{
                  "font-size": "0.7rem",
                  color: "var(--surface-muted)",
                  margin: "0 0 0.4rem 0",
                }}
              >
                {JOIN_REQUESTS_NOT_SAVED}
              </p>
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
                  {JOIN_REQUEST_NOT_KEPT}
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
                        <span
                          title={hexOf(ref)}
                          style={{
                            "font-family": "monospace",
                            "font-size": "0.78rem",
                            color: "var(--surface-fg)",
                            overflow: "hidden",
                            "text-overflow": "ellipsis",
                            "white-space": "nowrap",
                          }}
                        >
                          {hexOf(ref).slice(0, 16)}…
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
                                invite source unknown
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
                              via {attr.issuerFingerprint.slice(0, 11)}
                              {attr.issuerInRoster
                                ? " · member"
                                : " · not a current member"}
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
                          Approve
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
                          Reject
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
                Waiting to be added ({pendingAdds().length})
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
                      <span
                        style={{
                          "font-family": "monospace",
                          "font-size": "0.78rem",
                          color: "var(--surface-fg)",
                        }}
                      >
                        {labelForAddRequest(req)}
                      </span>
                      <span
                        style={{
                          "font-size": "0.7rem",
                          color: "var(--surface-muted)",
                        }}
                      >
                        Waiting to be added.
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
                {UNAPPLIED_EXPLANATION}
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
                        <span
                          style={{
                            "font-family": "monospace",
                            "font-size": "0.78rem",
                            color: "var(--surface-fg)",
                          }}
                        >
                          {labelForSignatureKey(item.addSignatureKeyB64)}
                        </span>
                        <span
                          style={{ "font-size": "0.7rem", color: "var(--surface-muted)" }}
                        >
                          {unappliedLine(item)}
                        </span>
                      </span>
                      <button
                        type="button"
                        onClick={() => onDismissUnapplied(item.addSignatureKeyB64)}
                        style={{ "font-size": "0.7rem" }}
                      >
                        Dismiss
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
              {FROZEN_ROSTER_HEADER}
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
                const displayName = acceptedByPubkey().get(
                  base64Encode(m.sigPubkey),
                );
                // ADR-0045 — NO optimistic update. A member with a pending
                // removal stays in the roster, annotated, because they are
                // still cryptographically in the group and still receiving
                // every message until the commit lands. Dropping the row would
                // be the membership equivalent of a green E2EE pill over an
                // unencrypted call.
                const annotation = (): string | null =>
                  rowAnnotation(pendingRequests(), m.leafIndex, ownLeaf());
                const label = (): string =>
                  displayName !== undefined
                    ? displayName
                    : shortFingerprint(m.sigPubkey);
                return (
                  <li
                    // The annotation is part of the row's ACCESSIBLE LABEL, not
                    // a separate focus stop — a screen-reader user hears it as
                    // one thing, and hears WHY the Remove button below is
                    // disabled rather than losing the control.
                    aria-label={
                      annotation() === null
                        ? label()
                        : `${label()}, ${annotation()?.replace(/\.$/, "")}`
                    }
                    style={{
                      display: "flex",
                      "align-items": "center",
                      gap: "0.5rem",
                      padding: "0.35rem 0.5rem",
                      "border-radius": "var(--radius-sm)",
                      background: "var(--surface-bg)",
                    }}
                  >
                    <span
                      style={{
                        display: "flex",
                        "flex-direction": "column",
                        gap: "0.1rem",
                      }}
                    >
                      <code style={{ "font-size": "0.85rem" }}>
                        #{m.leafIndex} · {label()}
                      </code>
                      <Show when={annotation() !== null}>
                        <span
                          aria-hidden="true"
                          style={{
                            "font-size": "0.7rem",
                            color: "var(--surface-muted)",
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
                          (you)
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
                          disabled={busy()}
                          style={{ "font-size": "0.8rem" }}
                        >
                          {adminPubkeys().has(hexOf(m.sigPubkey))
                            ? "Remove admin"
                            : "Make admin"}
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
                          disabled={busy() || annotation() !== null}
                          style={{
                            "font-size": "0.8rem",
                            color: "var(--error)",
                          }}
                        >
                          Remove
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
                  {DELETE_CHAT_CONTROL}
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
                      Transfer ownership
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
                      Leave group
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
                      Leave group
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
              aria-label="Transfer ownership"
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
                  {TRANSFER_IRREVERSIBLE_HEADLINE}
                </strong>
                <span style={{ "font-size": "0.75rem", color: "var(--surface-muted)" }}>
                  {TRANSFER_IRREVERSIBLE_BODY}
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
                  New owner
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
                        aria-label={row.isAdmin ? `${row.label}, admin` : row.label}
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
                          Admin
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
                  Your role afterwards
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
                {TRANSFER_UNLOCKS_LEAVE}
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
                  Cancel
                </button>
              </div>
            </div>
          </Show>
        </section>
      </Show>

      {error() !== null && <div class="form-error">{error()}</div>}
      {notice() !== null && <div class="form-notice">{notice()}</div>}

      <Show
        when={rows().length > 0}
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
              No messages yet. Send the first one below.
            </Show>
          </p>
        }
      >
        <ol
          style={{
            "list-style": "none",
            padding: 0,
            margin: 0,
            display: "flex",
            "flex-direction": "column",
            gap: "0.4rem",
          }}
        >
          <For each={rows()}>
            {(row) => (
              <TimelineBubble
                row={row}
                onDownload={onDownloadAttachment}
                onPreview={onPreviewAttachment}
                onHidePreview={onHideAttachmentPreview}
                getCachedUrl={getCachedAttachmentUrl}
                ownIdentity={ownSignatureKeyB64()}
                opState={(id) => msgOps().get(id)}
                readByOthers={(id) =>
                  isReadByEveryOther(readWatermarks(), id, ownSignatureKeyB64())
                }
                editingId={editingId()}
                editText={editText()}
                setEditText={setEditText}
                onStartEdit={(id, current) => {
                  setEditText(current);
                  setEditingId(id);
                  setReactPickerId(null);
                }}
                onCancelEdit={() => setEditingId(null)}
                onSaveEdit={onSaveEdit}
                onDeleteMessage={onDeleteMessage}
                reactPickerId={reactPickerId()}
                onOpenReactPicker={(id) => {
                  setReactPickerId(id);
                  setEditingId(null);
                }}
                onCloseReactPicker={() => setReactPickerId(null)}
                onReact={onReact}
                actionsAvailable={messageActionsAvailable()}
              />
            )}
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
         the filename + size with a × to drop it. The chips
         carry no preview because the local bytes have been
         GC'd after encrypt — re-decrypt would round-trip
         through storage, which is overkill for a composer
         scratch row. */}
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
                <span>{ref.name ?? "(unnamed)"}</span>
                <span style={{ color: "var(--surface-muted)" }}>
                  · {formatAttachmentSize(ref.size)}
                </span>
                <button
                  type="button"
                  onClick={() => removePendingAttachment(i())}
                  disabled={busy()}
                  aria-label="Remove attachment"
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
            <span
              style={{
                color: "var(--surface-muted)",
                "font-size": "0.8rem",
                "align-self": "center",
              }}
            >
              {(() => {
                const p = uploadProgress();
                if (p === null) return "Encrypting + uploading…";
                const verb = p.stage === "encrypting"
                  ? "Encrypting"
                  : "Uploading";
                // 1-based index in the user-facing copy — humans
                // count from 1. Total = 1 collapses the "(1/1)"
                // ornament since it's noise.
                const tail =
                  p.total > 1 ? ` (${p.fileIndex + 1}/${p.total})` : "";
                return `${verb} "${p.fileName}"${tail}…`;
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
      <Show when={typingIndicator() !== null && !membershipEnded()}>
        <div
          style={{
            "font-size": "0.8rem",
            "font-style": "italic",
            color: "var(--surface-muted)",
            padding: "0.15rem 0.25rem",
            "min-height": "1.1rem",
          }}
          aria-live="polite"
        >
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

      <Show when={!membershipEnded()}>
      <form
        onSubmit={onSend}
        style={{
          display: "flex",
          gap: "0.5rem",
          "padding-top": "0.5rem",
          "border-top": "1px solid var(--surface-border)",
        }}
      >
        {/* 1.19c2 — Attach button. The hidden `<input
            type="file" multiple>` is keyed by uploading() so
            the same file can be re-picked after a failed
            upload, and the label drives the click for a tidy
            chrome-less control. */}
        <label
          style={{
            display: "inline-flex",
            "align-items": "center",
            "justify-content": "center",
            padding: "0 0.6rem",
            border: "1px solid var(--surface-border)",
            "border-radius": "var(--radius-md)",
            background: "var(--surface-elevated)",
            cursor: uploading() ? "wait" : "pointer",
            opacity: uploading() ? 0.6 : 1,
          }}
          aria-label="Attach files"
          title={`Attach files (up to ${formatAttachmentSize(MAX_ATTACHMENT_BYTES)} each)`}
        >
          📎
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
        <input
          type="text"
          placeholder="Message"
          autocomplete="off"
          value={input()}
          onInput={(e) => setInput(e.currentTarget.value)}
          // ADR-0024 — leaving the composer is a typing-stop
          // transition; emit one `typing:false` (best-effort).
          onBlur={() => sendTypingStop()}
          style={{ flex: 1 }}
        />
        <button
          class="primary"
          type="submit"
          disabled={
            uploading() ||
            (input().length === 0 && pendingAttachments().length === 0)
          }
        >
          Send
        </button>
      </form>
      </Show>
    </div>
  );
}

// ---------------------------------------------------------------
// Subcomponents
// ---------------------------------------------------------------

function TimelineBubble(props: {
  row: TimelineRow;
  onDownload: (ref: AttachmentRefBytes) => void | Promise<void>;
  onPreview: (ref: AttachmentRefBytes) => void | Promise<void>;
  onHidePreview: (ref: AttachmentRefBytes) => void;
  getCachedUrl: (ref: AttachmentRefBytes) => string | null;
  /** ADR-0023 — our own signature key (for the own-reaction
   *  highlight). Not our leaf: a leaf we compare against a stored
   *  reactor would start matching a stranger after a renumber. */
  ownIdentity: MemberIdentity | undefined;
  /** Reactive overlay accessor for a given target messageId. */
  opState: (messageId: number) => MessageOpState | undefined;
  /** ADR-0024 — reactive read-state accessor: has this messageId
   *  been read by EVERY other member? Drives the own-message
   *  Sent → Read indicator. */
  readByOthers: (messageId: number) => boolean;
  editingId: number | null;
  editText: string;
  setEditText: (v: string) => void;
  onStartEdit: (messageId: number, current: string) => void;
  onCancelEdit: () => void;
  onSaveEdit: (messageId: number) => void | Promise<void>;
  onDeleteMessage: (messageId: number) => void | Promise<void>;
  reactPickerId: number | null;
  onOpenReactPicker: (messageId: number) => void;
  onCloseReactPicker: () => void;
  onReact: (messageId: number, emoji: string) => void | Promise<void>;
  /** ADR-0045 UX §6.2.5 — whether Edit / Delete / React are offered at all.
   *  `false` once this device's membership has ended: all three compose an
   *  application message, so all three are refused by the same core gate that
   *  refuses Send. Threaded as a prop rather than read here so the two bubble
   *  components stay pure renderers of what `membership-ended.ts` decided. */
  actionsAvailable: boolean;
}): JSX.Element {
  // System rows + call signaling never carry an op overlay.
  if (props.row.kind === "incoming") {
    const ev = props.row.event;
    if (ev.kind === "system") {
      return (
        <li
          style={{
            "font-size": "0.8rem",
            color: "var(--surface-muted)",
            "font-style": "italic",
            "text-align": "center",
          }}
        >
          · {ev.systemKind} #{ev.messageId} ·
        </li>
      );
    }
    if (ev.kind === "callSignaling") {
      // Phase 3a8 — call signaling rides the same MLS group
      // as chat messages, but it isn't part of the visible
      // timeline. The CallContext upstream already routed
      // it into the state machine; nothing renders here.
      return null;
    }
    if (ev.kind === "op") {
      // ADR-0023 — ops are folded into the target message's
      // overlay by the receive loop, never rendered as a
      // standalone bubble. (Defensive: ops don't enter `rows()`,
      // but keep the timeline render total.)
      return null;
    }
    if (ev.kind === "externalJoinRequest") {
      // ADR-0027 (G-3) — surfaced in the members panel, never a timeline
      // bubble (the receive loop skips it before `rows()`). Defensive.
      return null;
    }
    if (ev.kind === "pushRoutingSecret") {
      // ADR-0042 — the receive loop consumes it (persist + subscribe) and
      // never pushes it into `rows()`. Defensive.
      return null;
    }
    if (ev.kind === "committerBeacon") {
      // ADR-0045 — the receive loop consumes it and never pushes it into
      // `rows()`. Defensive.
      return null;
    }
    if (ev.kind === "membershipProposal") {
      // ADR-0045 UX §10 — a request is not something that HAPPENED to the
      // group; only outcomes belong in the timeline. The receive loop routes
      // it to the members panel and never pushes it into `rows()`. Defensive.
      return null;
    }
    // Incoming application message — never our own → Edit/Delete
    // are not offered (the author-only gate would reject our op
    // anyway); React is.
    return (
      <ChatBubbleBody
        messageId={ev.messageId}
        isOwn={false}
        ack="sent"
        originalText={ev.text}
        attachments={ev.attachments}
        meta={`peer · #${ev.messageId} · ${ev.createdAt}`}
        tone="incoming"
        {...props}
      />
    );
  }
  // Outgoing — our own message. Edit/Delete offered once acked
  // (a pending bubble has no messageId yet).
  const r = props.row;
  return (
    <ChatBubbleBody
      messageId={r.ack === "sent" ? r.messageId : undefined}
      isOwn={true}
      ack={r.ack}
      originalText={r.plaintext}
      attachments={r.attachments}
      meta={
        r.ack === "pending"
          ? "you · sending…"
          : `you · sent · #${r.messageId ?? "?"} · ${r.createdAt ?? ""}`
      }
      tone="outgoing"
      {...props}
    />
  );
}

/**
 * ADR-0023 — the shared bubble body for both incoming + outgoing
 * messages, carrying the op overlay (edited text / deleted
 * tombstone / reaction chips) and the per-message action menu.
 *
 * Reactivity: `opState(messageId)` is read inside the render so a
 * later op fold (or a local own-op apply) re-renders just this
 * bubble. The action affordances are gated on `messageId !==
 * undefined` (an un-acked outgoing bubble has no DS id to
 * reference yet).
 */
function ChatBubbleBody(props: {
  messageId: number | undefined;
  isOwn: boolean;
  ack: "pending" | "sent";
  originalText: string;
  attachments: AttachmentRefBytes[];
  meta: string;
  tone: "incoming" | "outgoing";
  onDownload: (ref: AttachmentRefBytes) => void | Promise<void>;
  onPreview: (ref: AttachmentRefBytes) => void | Promise<void>;
  onHidePreview: (ref: AttachmentRefBytes) => void;
  getCachedUrl: (ref: AttachmentRefBytes) => string | null;
  ownIdentity: MemberIdentity | undefined;
  opState: (messageId: number) => MessageOpState | undefined;
  readByOthers: (messageId: number) => boolean;
  editingId: number | null;
  editText: string;
  setEditText: (v: string) => void;
  onStartEdit: (messageId: number, current: string) => void;
  onCancelEdit: () => void;
  onSaveEdit: (messageId: number) => void | Promise<void>;
  onDeleteMessage: (messageId: number) => void | Promise<void>;
  reactPickerId: number | null;
  onOpenReactPicker: (messageId: number) => void;
  onCloseReactPicker: () => void;
  onReact: (messageId: number, emoji: string) => void | Promise<void>;
  /** ADR-0045 UX §6.2.5 — whether Edit / Delete / React are offered at all.
   *  `false` once this device's membership has ended: all three compose an
   *  application message, so all three are refused by the same core gate that
   *  refuses Send. Threaded as a prop rather than read here so the two bubble
   *  components stay pure renderers of what `membership-ended.ts` decided. */
  actionsAvailable: boolean;
}): JSX.Element {
  const isIncoming = props.tone === "incoming";
  // The live overlay for this message (reactive). `undefined`
  // until an op lands.
  const state = (): MessageOpState | undefined =>
    props.messageId === undefined ? undefined : props.opState(props.messageId);
  const isDeleted = (): boolean => state()?.deleted === true;
  const editedText = (): string | undefined => state()?.editedText;
  const displayText = (): string => editedText() ?? props.originalText;
  const isEdited = (): boolean => editedText() !== undefined;
  const isEditing = (): boolean =>
    props.messageId !== undefined && props.editingId === props.messageId;
  const pickerOpen = (): boolean =>
    props.messageId !== undefined && props.reactPickerId === props.messageId;
  // Reactions sorted for a stable chip order.
  const reactionEntries = (): Array<{
    emoji: string;
    count: number;
    mine: boolean;
  }> => {
    const m = state()?.reactions;
    if (m === undefined) return [];
    const own = props.ownIdentity ?? OWN_REACTION_SENTINEL;
    return [...m.entries()]
      .map(([emoji, reactors]) => ({
        emoji,
        count: reactors.size,
        mine: reactors.has(own),
      }))
      .filter((e) => e.count > 0)
      .sort((a, b) => a.emoji.localeCompare(b.emoji));
  };

  const bubbleStyle = isIncoming
    ? {
        padding: "0.5rem 0.75rem",
        background: "var(--surface-elevated)",
        border: "1px solid var(--surface-border)",
        "border-radius": "var(--radius-md)",
        "align-self": "flex-start",
        "max-width": "75%",
      }
    : {
        padding: "0.5rem 0.75rem",
        background: "var(--accent)",
        color: "var(--accent-fg)",
        "border-radius": "var(--radius-md)",
        "align-self": "flex-end",
        "max-width": "75%",
        opacity: props.ack === "pending" ? 0.7 : 1,
      };
  const metaColour = isIncoming ? "var(--surface-muted)" : undefined;

  return (
    <li style={{ ...bubbleStyle, position: "relative" }}>
      <Show
        when={!isDeleted()}
        fallback={
          <div
            style={{
              "font-style": "italic",
              opacity: 0.7,
              "white-space": "pre-wrap",
            }}
          >
            message deleted
          </div>
        }
      >
        <Show
          when={isEditing()}
          fallback={
            <>
              <Show when={displayText().length > 0}>
                <div style={{ "white-space": "pre-wrap" }}>
                  {displayText()}
                  <Show when={isEdited()}>
                    <span
                      style={{
                        "font-size": "0.7rem",
                        opacity: 0.6,
                        "margin-left": "0.35rem",
                      }}
                    >
                      (edited)
                    </span>
                  </Show>
                </div>
              </Show>
              <Show when={props.attachments.length > 0}>
                <AttachmentChips
                  attachments={props.attachments}
                  onDownload={props.onDownload}
                  onPreview={props.onPreview}
                  onHidePreview={props.onHidePreview}
                  getCachedUrl={props.getCachedUrl}
                  tone={props.tone}
                />
              </Show>
            </>
          }
        >
          {/* Inline edit composer — pre-filled with the current
              text; Save publishes the edit op, Cancel discards. */}
          <div style={{ display: "flex", "flex-direction": "column", gap: "0.35rem" }}>
            <textarea
              value={props.editText}
              onInput={(e) => props.setEditText(e.currentTarget.value)}
              rows={2}
              style={{
                width: "100%",
                "box-sizing": "border-box",
                "font-family": "inherit",
                "font-size": "inherit",
                padding: "0.3rem 0.4rem",
                "border-radius": "var(--radius-sm)",
                border: "1px solid var(--surface-border)",
                color: "var(--surface-fg)",
                background: "var(--surface-bg)",
                resize: "vertical",
              }}
            />
            <div style={{ display: "flex", gap: "0.4rem" }}>
              <button
                type="button"
                onClick={() => {
                  if (props.messageId !== undefined) {
                    void props.onSaveEdit(props.messageId);
                  }
                }}
                style={bubbleActionBtnStyle}
              >
                Save
              </button>
              <button
                type="button"
                onClick={() => props.onCancelEdit()}
                style={bubbleActionBtnStyle}
              >
                Cancel
              </button>
            </div>
          </div>
        </Show>
      </Show>

      {/* Reaction chips — rendered under the body (incl. on a
          deleted message: a reaction can outlive the text). */}
      <Show when={reactionEntries().length > 0}>
        <div
          style={{
            display: "flex",
            "flex-wrap": "wrap",
            gap: "0.3rem",
            "margin-top": "0.35rem",
          }}
        >
          <For each={reactionEntries()}>
            {(re) => {
              // The chip carries BOTH a fact and an affordance, and §6.2.5
              // separates them: the count is transcript — part of the history
              // §6.2.6 keeps whole — while tapping it is the React op, which
              // dies with Send. So once the membership has ended the chip
              // renders as static text rather than as a button. It is not
              // rendered disabled, for the same reason the composer is not:
              // a greyed control on a group you will never be in again
              // promises a future it does not have.
              const chipStyle = {
                display: "inline-flex",
                "align-items": "center",
                gap: "0.2rem",
                padding: "0.05rem 0.4rem",
                "font-size": "0.8rem",
                "border-radius": "999px",
                border: re.mine
                  ? "1px solid var(--accent)"
                  : "1px solid var(--surface-border)",
                background: re.mine
                  ? "var(--accent-soft, rgba(99,102,241,0.18))"
                  : isIncoming
                    ? "var(--surface-bg)"
                    : "rgba(255,255,255,0.15)",
                color: "inherit",
              } as const;
              // A FUNCTION, not a stored element: Solid's JSX creates the
              // DOM node where it is evaluated, so a single shared node placed
              // in both branches would be MOVED between them rather than
              // duplicated. Each branch builds its own.
              const chipBody = (): JSX.Element => (
                <>
                  <span>{re.emoji}</span>
                  <span style={{ "font-size": "0.72rem", opacity: 0.85 }}>
                    {re.count}
                  </span>
                </>
              );
              return (
                <Show
                  when={props.actionsAvailable}
                  fallback={<span style={chipStyle}>{chipBody()}</span>}
                >
                  <button
                    type="button"
                    title={re.mine ? "Remove your reaction" : "React"}
                    onClick={() => {
                      if (props.messageId !== undefined) {
                        void props.onReact(props.messageId, re.emoji);
                      }
                    }}
                    style={{ ...chipStyle, cursor: "pointer" }}
                  >
                    {chipBody()}
                  </button>
                </Show>
              );
            }}
          </For>
        </div>
      </Show>

      <div
        style={{
          color: metaColour,
          "font-size": "0.75rem",
          "margin-top": "0.2rem",
          display: "flex",
          "align-items": "center",
          gap: "0.35rem",
        }}
      >
        <span>{props.meta}</span>
        {/* ADR-0024 — read state on our OWN acked messages. A single
            check once published ("Sent"); a filled double check once
            EVERY other member's read watermark covers this message
            ("Read"). Gated on a present messageId. */}
        <Show
          when={
            props.isOwn &&
            props.ack === "sent" &&
            props.messageId !== undefined
          }
        >
          {(() => {
            const read = (): boolean =>
              props.messageId !== undefined &&
              props.readByOthers(props.messageId);
            return (
              <span
                title={read() ? "Read" : "Sent"}
                aria-label={read() ? "Read" : "Sent"}
                style={{
                  "font-size": "0.72rem",
                  opacity: read() ? 1 : 0.8,
                  "font-weight": read() ? 600 : 400,
                }}
              >
                {read() ? "✓✓ Read" : "✓ Sent"}
              </span>
            );
          })()}
        </Show>
      </div>

      {/* Per-message action menu — gated on the message having a
          DS messageId (the op references its target by id) and on
          the membership still holding (ADR-0045 UX §6.2.5: Edit /
          Delete / React each compose an application message, so
          all three die with Send — the menu is WITHDRAWN, not
          disabled). The hover-revealed strip carries React on any
          message, plus Edit + Delete on our own. Inline-edit mode
          hides it. */}
      <Show
        when={
          props.actionsAvailable &&
          props.messageId !== undefined &&
          !isEditing() &&
          !isDeleted()
        }
      >
        <div
          class="lakko-msg-actions"
          style={{
            position: "absolute",
            top: "-0.6rem",
            [isIncoming ? "right" : "left"]: "0.4rem",
            display: "flex",
            gap: "0.2rem",
            padding: "0.1rem",
            "border-radius": "var(--radius-sm)",
            background: "var(--surface-bg)",
            border: "1px solid var(--surface-border)",
            "box-shadow": "0 1px 3px rgba(0,0,0,0.18)",
          }}
        >
          <button
            type="button"
            title="React"
            onClick={() =>
              pickerOpen()
                ? props.onCloseReactPicker()
                : props.onOpenReactPicker(props.messageId as number)
            }
            style={bubbleMiniBtnStyle}
          >
            🙂
          </button>
          <Show when={props.isOwn}>
            <button
              type="button"
              title="Edit"
              onClick={() =>
                props.onStartEdit(props.messageId as number, displayText())
              }
              style={bubbleMiniBtnStyle}
            >
              ✎
            </button>
            <button
              type="button"
              title="Delete"
              onClick={() => {
                if (props.messageId !== undefined) {
                  void props.onDeleteMessage(props.messageId);
                }
              }}
              style={bubbleMiniBtnStyle}
            >
              🗑
            </button>
          </Show>
        </div>
      </Show>

      {/* Emoji picker popover. Gated on the same predicate as the strip that
          opens it, so a picker left open when the eviction commit lands
          mid-poll closes with it rather than floating over a dead group. */}
      <Show when={props.actionsAvailable && pickerOpen()}>
        <div
          style={{
            position: "absolute",
            top: "1.2rem",
            [isIncoming ? "right" : "left"]: "0.4rem",
            "z-index": 5,
            display: "flex",
            gap: "0.15rem",
            padding: "0.25rem",
            "border-radius": "var(--radius-md)",
            background: "var(--surface-bg)",
            border: "1px solid var(--surface-border)",
            "box-shadow": "0 2px 6px rgba(0,0,0,0.22)",
          }}
        >
          <For each={REACTION_EMOJI}>
            {(emoji) => (
              <button
                type="button"
                onClick={() => {
                  if (props.messageId !== undefined) {
                    void props.onReact(props.messageId, emoji);
                  }
                }}
                style={{
                  border: "none",
                  background: "transparent",
                  "font-size": "1.1rem",
                  cursor: "pointer",
                  padding: "0.1rem 0.2rem",
                  "line-height": 1,
                }}
              >
                {emoji}
              </button>
            )}
          </For>
        </div>
      </Show>
    </li>
  );
}

/** Shared style for the Save / Cancel buttons in the inline
 *  edit composer. */
const bubbleActionBtnStyle = {
  padding: "0.2rem 0.6rem",
  "font-size": "0.78rem",
  cursor: "pointer",
  "border-radius": "var(--radius-sm)",
  border: "1px solid var(--surface-border)",
  background: "var(--surface-bg)",
  color: "var(--surface-fg)",
} as const;

/** Shared style for the tiny hover-strip action buttons. */
const bubbleMiniBtnStyle = {
  border: "none",
  background: "transparent",
  "font-size": "0.85rem",
  cursor: "pointer",
  padding: "0.1rem 0.25rem",
  "line-height": 1,
} as const;

/**
 * Render the attachment-chip strip below a bubble's text body.
 * Each chip shows `name · size` plus action buttons:
 *
 *   * **Open** — inline-preview when the MIME is a known image
 *     format (`isInlinePreviewable`); fetches + decrypts the
 *     bytes lazily, caches the object URL.
 *   * **Save** — download + decrypt + trigger a save dialog.
 *
 * `tone` shifts the chip colour so they stay legible inside
 * both the incoming-grey and outgoing-accent bubble bodies.
 */
function AttachmentChips(props: {
  attachments: AttachmentRefBytes[];
  onDownload: (ref: AttachmentRefBytes) => void | Promise<void>;
  onPreview: (ref: AttachmentRefBytes) => void | Promise<void>;
  onHidePreview: (ref: AttachmentRefBytes) => void;
  /** Returns the cached object URL for an attachment, or `null`
   *  if it hasn't been decrypted in this session. The bubble
   *  renders an inline `<img>` whenever this is non-null. */
  getCachedUrl: (ref: AttachmentRefBytes) => string | null;
  tone: "incoming" | "outgoing";
}): JSX.Element {
  const chipBg = props.tone === "incoming" ? "var(--surface-bg)" : "rgba(255,255,255,0.15)";
  const chipText = props.tone === "incoming" ? undefined : "inherit";
  return (
    <div
      style={{
        display: "flex",
        "flex-direction": "column",
        gap: "0.35rem",
        "margin-top": "0.4rem",
      }}
    >
      <For each={props.attachments}>
        {(ref) => {
          const cachedUrl = (): string | null => props.getCachedUrl(ref);
          const previewable = isInlinePreviewable(ref);
          return (
            <div style={{ display: "flex", "flex-direction": "column", gap: "0.25rem" }}>
              <Show when={previewable && cachedUrl() !== null}>
                {/* Inline image preview — renders the decrypted
                    blob URL directly in the bubble. Polish 1.x:
                    matches the Signal / Telegram pattern of
                    showing the picture next to its filename,
                    rather than blowing open a new tab. */}
                <img
                  src={cachedUrl() ?? ""}
                  alt={ref.name ?? "image attachment"}
                  style={{
                    display: "block",
                    "max-width": "100%",
                    "max-height": "320px",
                    width: "auto",
                    height: "auto",
                    "border-radius": "var(--radius-sm)",
                    "object-fit": "contain",
                  }}
                />
              </Show>
              <span
                style={{
                  display: "inline-flex",
                  "align-items": "center",
                  gap: "0.35rem",
                  padding: "0.25rem 0.5rem",
                  background: chipBg,
                  color: chipText,
                  "border-radius": "var(--radius-sm)",
                  "font-size": "0.8rem",
                  "max-width": "100%",
                }}
              >
                <span
                  style={{
                    "white-space": "nowrap",
                    overflow: "hidden",
                    "text-overflow": "ellipsis",
                    "max-width": "20ch",
                  }}
                  title={ref.name ?? "(unnamed)"}
                >
                  {ref.name ?? "(unnamed)"}
                </span>
                <span style={{ opacity: 0.7 }}>
                  · {formatAttachmentSize(ref.size)}
                </span>
                <Show when={previewable}>
                  <Show
                    when={cachedUrl() !== null}
                    fallback={
                      <button
                        type="button"
                        onClick={() => {
                          void props.onPreview(ref);
                        }}
                        style={{
                          border: "none",
                          background: "transparent",
                          color: "inherit",
                          cursor: "pointer",
                          "text-decoration": "underline",
                          padding: 0,
                          "font-size": "0.8rem",
                        }}
                      >
                        Open
                      </button>
                    }
                  >
                    <button
                      type="button"
                      onClick={() => props.onHidePreview(ref)}
                      style={{
                        border: "none",
                        background: "transparent",
                        color: "inherit",
                        cursor: "pointer",
                        "text-decoration": "underline",
                        padding: 0,
                        "font-size": "0.8rem",
                      }}
                    >
                      Hide
                    </button>
                  </Show>
                </Show>
                <button
                  type="button"
                  onClick={() => {
                    void props.onDownload(ref);
                  }}
                  style={{
                    border: "none",
                    background: "transparent",
                    color: "inherit",
                    cursor: "pointer",
                    "text-decoration": "underline",
                    padding: 0,
                    "font-size": "0.8rem",
                  }}
                >
                  Save
                </button>
              </span>
            </div>
          );
        }}
      </For>
    </div>
  );
}

function GroupNotFound(): JSX.Element {
  return (
    <section style={panelStyle()}>
      <h1 style={{ "margin-top": 0 }}>Group not found</h1>
      <p>
        Either the URL is wrong, or this group's MLS state was dropped
        on the most recent page reload — Phase-1 doesn't persist
        in-memory MLS groups (tracked as ROADMAP 1.16e6).
      </p>
      <p>
        <A href={paths.contacts}>Back to Contacts</A>
      </p>
    </section>
  );
}

function NeedsReauth(): JSX.Element {
  return (
    <section style={panelStyle()}>
      <h1 style={{ "margin-top": 0 }}>Re-enter your password</h1>
      <p>
        This page reload restored your session from the local device
        key, but the secrets needed to decrypt and sign messages
        aren't in memory. Please re-enter your password.
      </p>
      <p>
        <A href={paths.login}>Re-enter password</A>
      </p>
    </section>
  );
}

// ---------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------

function panelStyle(): JSX.CSSProperties {
  return {
    background: "var(--surface-elevated)",
    border: "1px solid var(--surface-border)",
    "border-radius": "var(--radius-md)",
    padding: "1rem",
  };
}

/** 12-hex-char prefix of an Ed25519 verifying key — enough to
 *  visually tell members apart in the leaf list without crowding
 *  the row. Mirror of `Invite.tsx`'s helper; if a third call
 *  site appears, fold both into `contacts/local-store.ts`'s
 *  existing `fingerprintShort` (which works in base64 — JS
 *  string typing wants the hex variant here). */
function shortFingerprint(pubkey: Uint8Array): string {
  let hex = "";
  for (let i = 0; i < pubkey.length && hex.length < 12; i++) {
    hex += (pubkey[i] ?? 0).toString(16).padStart(2, "0");
  }
  return hex.slice(0, 12);
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
 * ADR-0045 UX §6 / §12 item 6 — the single funnel every membership handler
 * already went through, now refusal-aware.
 *
 * Before this it was `err.message`, so a member who tapped `Add` was told
 * `add proposal refused: proposer is not owner or admin` — the core's
 * sentence for a developer reading a log. The reason now rides beside that
 * sentence and selects the copy; the message is never inspected.
 *
 * A non-refusal keeps its own message unchanged. Collapsing an upload failure
 * into "This change could not be requested." would be a worse lie than the
 * raw text, and §6 row 7 is about not leaking a *refusal's* internal reason,
 * not about hiding every error in the app.
 */
function formatError(err: unknown): string {
  const refusal = membershipRefusalOf(err, membershipRetryMaySucceed);
  if (refusal !== null) {
    if (!refusal.recognised) {
      // The only case the fail-closed row shows for a refusal nobody decided
      // about: a core newer than this build. Worth a console line, because it
      // is a shipping gap rather than a user error.
      console.warn("membership refusal with no copy in this build", refusal.reason, err);
    }
    return refusal.message;
  }
  if (err instanceof Error) return err.message;
  return String(err);
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
function timelineRowToPersisted(row: TimelineRow): PersistedMessage | null {
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
      attachments: row.attachments.map(persistedFromAttachmentRef),
      createdAt: row.createdAt,
      // ADR-0023 — persisted so edit / delete keep working after a
      // reload. The key we stamped at send, read back verbatim: the
      // store is the record of who wrote this, not a place to
      // re-derive it from whoever is holding the device now.
      authorKeyB64: authorIdentityToPersisted(row.authorKey),
    };
  }
  // Incoming: split application vs system.
  if (row.event.kind === "application") {
    return {
      messageId: String(row.event.messageId),
      direction: "incoming",
      text: row.event.text,
      attachments: row.event.attachments.map(persistedFromAttachmentRef),
      createdAt: row.event.createdAt,
      // ADR-0023 — the author's signature key, resolved back at
      // receipt in the epoch that named it. Storing the leaf here
      // was never an option: by the time this row is read back the
      // leaf may belong to whoever was added next.
      authorKeyB64: authorIdentityToPersisted(row.event.senderKey),
    };
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
    text: `[mls ${row.event.systemKind}]`,
    attachments: [],
    createdAt: row.event.createdAt,
  };
}

function persistedFromAttachmentRef(a: AttachmentRefBytes): PersistedAttachment {
  return {
    blobIdB64: b64UrlEncode(a.blobId),
    keyB64: b64UrlEncode(a.key),
    nonceB64: b64UrlEncode(a.nonce),
    mime: a.mime ?? null,
    sizeStr: String(a.size),
    name: a.name ?? null,
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
function persistedToTimelineRow(m: PersistedMessage): TimelineRow {
  const attachments = m.attachments.map(attachmentRefFromPersisted);
  if (m.direction === "outgoing") {
    return {
      kind: "outgoing",
      tempId: -1,
      plaintext: m.text,
      attachments,
      ack: "sent",
      messageId: Number(m.messageId),
      createdAt: m.createdAt,
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
      },
    };
  }
  // system
  return {
    kind: "incoming",
    event: {
      kind: "system",
      messageId: Number(m.messageId),
      systemKind: "commit",
      createdAt: m.createdAt,
    },
  };
}

function attachmentRefFromPersisted(a: PersistedAttachment): AttachmentRefBytes {
  return {
    blobId: b64UrlDecode(a.blobIdB64),
    key: b64UrlDecode(a.keyB64),
    nonce: b64UrlDecode(a.nonceB64),
    mime: a.mime ?? undefined,
    size: Number(a.sizeStr),
    name: a.name ?? undefined,
  };
}

function b64UrlEncode(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i += 1) {
    binary += String.fromCharCode(bytes[i] as number);
  }
  return btoa(binary)
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

function uint8ArrayEquals(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

function b64UrlDecode(s: string): Uint8Array {
  const padded = s + "===".slice((s.length + 3) % 4);
  const std = padded.replace(/-/g, "+").replace(/_/g, "/");
  const binary = atob(std);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    out[i] = binary.charCodeAt(i);
  }
  return out;
}
