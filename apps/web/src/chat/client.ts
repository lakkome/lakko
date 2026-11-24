// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `ChatClient` — Application-message orchestrator on top of
 * MLS groups (Phase 1.16e4).
 *
 * Two operations:
 *
 *   - `sendMessage(identity, group, plaintextBytes)` —
 *     `Group.createApplicationMessage` → POST to
 *     `/ds/group/{group_id_hex}/messages`. Returns the DS
 *     message id (also serves as the cursor for the next poll).
 *
 *   - `pollMessages(identity, group, after, waitSecs?)` —
 *     `DsClient.fetchGroup` → for each row,
 *     `group.processIncoming(identity, payload)` → if it's
 *     an application message, append the plaintext to the
 *     output. Commits / proposals advance group state but
 *     surface as `null` plaintext (the UI can render them as
 *     "system" rows or skip).
 *
 * The Group instance is held by the **caller** (typically by
 * the global `<ChatProvider>` registry from 1.16e5); this class
 * never owns it.
 *
 * ## Self-message echo
 *
 * `Group.createApplicationMessage` produces wire bytes that
 * look like a normal MLS application message — the DS does
 * not deduplicate, and the local poll WILL pick our own
 * messages back up. We drop the echo on receive by
 * remembering the DS ids of our own publishes during this
 * session. Cross-device (the same user sending from another
 * tab) is out of scope for 1.16e4 — the echo would surface
 * as a `Cannot decrypt own message` error inside OpenMLS,
 * because the sender's own application messages aren't
 * decryptable by the sender at the same epoch. We surface
 * those as warnings + skip.
 *
 * ## Canonical Identity (1.16e6c2)
 *
 * Every MLS operation must run against the **same**
 * `WasmIdentity` instance — the one whose OpenMLS provider
 * holds the live state for every Group on this device. The
 * `identityProvider` constructor hook is how the chat context
 * hands that canonical Identity in. When it returns `null`
 * (session restored from IDB without a fresh password) the
 * methods throw `ChatClientArgError` — the route gates re-auth
 * before reaching here.
 *
 * Pre-1.16e6c2 fallback: if `identityProvider` is unset (tests,
 * direct callers without a ChatProvider), each call rebuilds an
 * `Identity.fromSeed(secrets.accountIdentitySeed)` (ADR-0017 — the
 * phrase seed, was the export key) and discards it. That
 * still works for stateless ops but means provider-storage
 * mutations vanish at the end of each call. Production paths
 * MUST wire the canonical accessor.
 *
 * ## Persistence (1.16e6c2)
 *
 * After every state-mutating call we fire the optional `onPersist` callback so
 * the on-disk MLS snapshot stays in sync with the canonical Identity's
 * provider.
 *
 * That write is **fail-closed on both paths**, for two different reasons:
 *
 *   - on SEND, a failed persist ABORTS the publish, so a gen-N ciphertext never
 *     reaches the DS without its N+1 ratchet state on disk (AEAD nonce reuse);
 *   - on RECEIVE, a failed persist HOLDS the journal cursor, so the cursor can
 *     never end up ahead of the durable MLS state. It used to be best-effort
 *     here, and since ADR-0045 that was enough to brick a device permanently —
 *     see `chat/durable-cursor.ts` for the mechanism and why holding (rather
 *     than aborting) is the right shape on the receive side.
 */
import { runBeforeSend } from "./before-send";
import { DsClient, DsHttpError } from "../ds/client";
import type { FetchResult } from "../ds/client";
import { base64Decode, base64Encode } from "../auth/account-id";
import { withGroupMlsLock } from "../mls/group-lock";

import { readKnockCursor, takeKnockReadSlot, writeKnockCursor } from "./knock-cursor";
import { boundContactsAccount } from "../contacts/local-store";
import { DS_WAKE_HEADER_LEN, type DsWake } from "../ds/wake";
import { loadWasm, wasmIfLoaded } from "../wasm";
import {
  epochAfterMessage as wasmEpochAfterMessage,
  epochOfMessage as wasmEpochOfMessage,
  isKnockRowLen as wasmIsKnockRowLen,
  knockRowVersion as wasmKnockRowVersion,
  Role,
  type Group as WasmGroup,
  type Identity as WasmIdentity,
  type ContentState,
  type MessageContent,
} from "../../wasm/lakko_ffi_wasm";
import { attachReceiveHook } from "../attach/receive-hook";
import type { IdentitySecrets } from "../auth/AuthContext";
import type { MembershipChange } from "./membership-rows";
import { personKeysOrNull } from "./person-key";
import {
  parseBodyEnvelope,
  type AttachmentRefBytes,
  type ContentOp,
  type ForwardOrigin,
  type ForwardRef,
  type ReplyRef,
} from "./attachments";
import { tryDecodeSecretEnvelope } from "../push/group-routing";
import { looksLikeCallSignaling as looksLikeCallSignalingBytes } from "../calls/call-signaling-wake";
import { DurableCursorGate } from "./durable-cursor";
import { conversationKindOf } from "./conversation-kind";
import { dropsAtReceipt } from "../moderation/block-rules";
import {
  attestOutgoing,
  evidenceAtReceipt,
  unwrapReceived,
  type MessageEvidence,
} from "../moderation/attestation";
import { loadUnacked } from "./commit-outbox";
import { registeredCommitHolds } from "./registered-commit-hold";
import { conversationBehind, rowTimeMs } from "./conversation-behind";
import { clearReentryReadFrom, noteLeavesSeen, reentryReadFrom } from "./reentry-store";
import { deliverReentryRequest, noteLeafWrote } from "./reentry-admit";
import { consumeKnockKeyRow } from "./knock-key";
import { readCursorEpoch, recordCursorEpoch, rewindLine, snapshotRewindFrom } from "./cursor-epoch";
import { mlsSnapshotWriter } from "./mls-store";
import { errorLogTag } from "./log-tag";
import {
  consumeOwnPublished,
  isOwnPublished,
  markOwnPublished,
} from "./own-published";
import { sniffProfile, type ReceivedProfile } from "../profile/profile-envelope";
import { sniffScopedEnvelope, type ScopedEnvelopeKind } from "../reach/envelope-sniff";
import { sniffProfileRequest } from "../profile/profile-request";
import { sniffGroupClosed, type GroupClosedNotice } from "./group-closed-envelope";
import { sniffGroupName } from "./group-name-envelope";
import type { StoredGroupName } from "./group-name-store";
import { sniffPin } from "./pin-envelope";
import { sniffSharedChannels, type SharedList } from "../discover/shared-channels";
import { isRegistrarSeedEnvelope } from "./registrar-seed-envelope";
import { isGroupContactRequestEnvelope } from "./group-contact-request-envelope";
import { deliverGroupContactRequest } from "../contacts/group-contact-request";
import { deliverRegistrarSeed } from "../ds/registrar-seeds";
import { deliverAccountDeleted, isAccountDeletedBytes } from "./account-deleted-envelope";
import { deliverKnockSeed, groupIsPublic, knockSeedsForRow, rememberKnockName } from "../ds/knock-seeds";
import { rememberKnockOrigin, rowStampMs } from "./knock-origins";
import { joinReplyBindingVerifies, rememberJoinReply } from "../reach/join-replies";
import { reachSession } from "../reach/reach-service";
import { sniffGroupSetting, type GroupSetting } from "./group-settings-envelope";
import { noteOwnerTransition, readRoleTableOwner } from "../ds/root-handover";
import type { IncomingPin } from "./pin-store";
import {
  resolveLeafSignatureKey,
  resolveMemberIdentity,
  type MemberIdentity,
  type RosterMember,
} from "./author-identity";
import { publishJoinRequestRead, splitPendingJoinKeys } from "./join-request-reads";

/** Result of `sendMessage`. */
export interface SendMessageResult {
  /** DS-assigned monotonic id (also the cursor value). */
  messageId: number;
  /** ISO-8601 timestamp from the DS. */
  createdAt: string;
}

/** A decoded application message returned by `pollMessages`.
 *  `Commit` and `Proposal` rows advance group state but don't
 *  carry user-visible bytes; we surface them as
 *  `kind: "system"` so the UI can render a thin separator if
 *  it wants.
 *
 *  Phase 1.19c2: the application variant now carries
 *  `(text, attachments)` from the 1.19a JSON envelope (parsed
 *  via the wasm `parseEnvelope` surface from 1.19b). Legacy
 *  pre-1.19 plain-UTF-8 bytes collapse to
 *  `{ text: <utf-8 lossy>, attachments: [] }` via the same
 *  envelope parser's documented fallback paths — existing
 *  bubbles keep rendering unchanged. `plaintext` is preserved
 *  as an alias of `text` for transitional call-site
 *  compatibility. */
export type IncomingChatEvent =
  | {
      kind: "application";
      messageId: number;
      /** Plain user-visible text from the envelope (empty
       *  string is valid — attachment-only message). */
      text: string;
      /** Zero or more attachment refs from the envelope. */
      attachments: AttachmentRefBytes[];
      /** Alias of `text` — kept for the few call sites that
       *  still read `plaintext`. New code should read `text`. */
      plaintext: string;
      /** ADR-0023 — the original author's MLS leaf index. Kept for
       *  diagnostics and for the roster lookups that legitimately
       *  want a position; it is NOT what authorship is compared on
       *  (see `senderKey`). */
      senderLeaf: number | undefined;
      /** ADR-0023 — the original author's IDENTITY: their signature
       *  public key, base64, resolved from `senderLeaf` against the
       *  roster of the epoch this message decrypted at. This is what
       *  the author-only edit/delete gate compares, and what the
       *  timeline persists.
       *
       *  A leaf index is a position in the ratchet tree, not a
       *  person: OpenMLS hands a vacated leaf to the next member
       *  added, so a leaf recorded now can name somebody else after
       *  the next commit. Resolving here — at receipt, in the epoch
       *  the roster is authoritative for — is the only moment the
       *  mapping is valid.
       *
       *  `undefined` for a frame whose sender the client could not
       *  resolve (a control message, an external sender, a torn
       *  roster read) — such a message can never be a valid
       *  edit/delete target. */
      senderKey: MemberIdentity | undefined;
      createdAt: string;
      /** ADR-0052 — set when this message replies to another: the target's
       *  DS `messageId` plus the quoted author / excerpt snapshot as the
       *  replier wrote them. Absent on a plain message. */
      reply?: ReplyRef;
      /** ADR-0052 decision 2 — set when this message was forwarded: the
       *  original author's name and / or the origin reference, as the
       *  forwarder wrote them. Absent on a plain message. */
      forward?: ForwardRef;
      /** ADR-0073 R.7 case 1 — set on a message RE-SENT into a successor
       *  one-to-one: the original group id and message id. Never drawn; a
       *  device that holds the original drops this row
       *  (`resend-marker.ts`). */
      resendOrigin?: ForwardOrigin;
      /** ADR-0061 section 6 — set when the sender's newer Lakko wrote this
       *  message in a wire version this build cannot read: the envelope's
       *  version. `text` is empty and there are no attachments; every screen
       *  draws `chat.message.unsupported` in its place. */
      unsupportedVersion?: number;
      /** ADR-0099 section 1 - what a report needs of this message, captured
       *  at receipt: the attestation verdict, the raw attested bytes, the
       *  sender leaf key and its device binding. Absent on an own message,
       *  on a frame with no resolvable sender leaf, and on a row restored
       *  from a log written before the field existed. */
      evidence?: MessageEvidence;
      /** ADR-0087 section 14 — the channel invitation card the envelope
       *  carried beside its `/g/` link text. Kept with the message so the
       *  bubble and the Chats row open it offline. */
      channelInvite?: Uint8Array;
      /** ADR-0092 — a poll, checklist, location, contact card or article;
       *  `text` is empty and `attachments` are an article's images. */
      content?: MessageContent;
      /** ADR-0092 section 3.11 — a restored poll's or checklist's state. */
      contentState?: ContentState;
    }
  | {
      /** ADR-0092 — an op on a poll or checklist (a vote, a close, a tick, an
       *  added item, an edit). Filed by the receive loop in the attachment
       *  store (`attach/receive.ts`); never a row. */
      kind: "contentOp";
      messageId: number;
      op: ContentOp;
      senderLeaf: number | undefined;
      senderKey: MemberIdentity | undefined;
      createdAt: string;
    }
  | {
      /** ADR-0092 section 3.9 — an open-items hand-over or old-ballot rows;
       *  consumed by the receive loop, never a row. */
      kind: "openItems";
      messageId: number;
      envelope: Uint8Array;
      ballots: boolean;
      senderKey: MemberIdentity | undefined;
      createdAt: string;
    }
  | {
      /** ADR-0023 / ADR-0024 — an edit / delete / reaction op OR a
       *  typing / read-receipt status signal, carried in the same
       *  JSON envelope as a chat message. None of them render a
       *  standalone bubble: the UI folds an edit/delete/reaction
       *  into the *target* message (by `target` === the target's DS
       *  `messageId`), a `typing` into the ephemeral per-leaf typing
       *  registry, and a `read-receipt` into the monotone per-leaf
       *  read watermark. The op itself still consumes a DS
       *  `messageId` and surfaces here so the receive loop can apply
       *  it once (the poll is incremental). */
      kind: "op";
      /** This op message's own DS-assigned id. */
      messageId: number;
      /** `"edit" | "delete" | "reaction" | "typing" | "read-receipt"`. */
      op: "edit" | "delete" | "reaction" | "typing" | "read-receipt";
      /** The DS `messageId` the op acts on. For edit/delete/reaction
       *  + read-receipt this is the target message; for `typing` it
       *  is unused (`0`). */
      target: number;
      /** Edit text (empty for delete / reaction / typing /
       *  read-receipt). */
      text: string;
      /** Reaction emoji (only for `op === "reaction"`). */
      emoji: string | undefined;
      /** Add or take off (only for `op === "reaction"`): explicit, never a
       *  toggle (UX reactions-and-emoji section 17). */
      reactionAction?: "add" | "remove";
      /** The reacted-to message's author account key as the reactor saw it
       *  (only for `op === "reaction"`, when sent). A claim: the notification
       *  path reads it, no attribution does. */
      reactionTargetAuthor?: Uint8Array;
      /** ADR-0024 — the typing state (only for `op === "typing"`):
       *  `true` = started typing, `false` = stopped. `undefined`
       *  for every other op. */
      isTyping: boolean | undefined;
      /** The op sender's MLS leaf index. Diagnostic only — every
       *  attribution decision reads `senderKey`. */
      senderLeaf: number | undefined;
      /** The op sender's IDENTITY: their signature public key,
       *  base64, resolved from `senderLeaf` against the roster of
       *  the epoch this op decrypted at. The author-only gate
       *  (edit/delete) checks it against the target's stored author
       *  identity, reactions attribute the toggle to it, and typing
       *  / read receipts are keyed on it — none of which may key on
       *  a leaf, which the next member added inherits.
       *  `undefined` ⇒ the op is dropped (fail-closed). */
      senderKey: MemberIdentity | undefined;
      createdAt: string;
    }
  | {
      kind: "system";
      messageId: number;
      /** The MLS frame kind: "commit" / "external-join-proposal". Lower-case
       *  for parity with `ProcessedMessageKind` strings on the Rust side.
       *  ADR-0045 removed `"proposal"` from this set — a membership proposal
       *  is now its own `membershipProposal` event, because it is not something
       *  that happened to the group. */
      systemKind: "commit" | "external-join-proposal";
      createdAt: string;
      /** `docs/ux/second-device.md` section 4 — on a commit row that changed
       *  the people of a group, the person keys that joined and left
       *  (`chat/membership-rows.ts`). Absent on every other row; the receive
       *  paths add it after the batch is applied. */
      membership?: MembershipChange;
      /** `docs/ux/second-device.md` section 4 — for a received commit, the
       *  people of the group (base64 person keys) read right after it was
       *  applied; `null` on a torn read. Absent where the producer read none
       *  (this device's own commit, older paths). */
      personKeysAfter?: readonly string[] | null;
    }
  | {
      /** ADR-0045 — a membership PROPOSAL: a member asked for an add, a
       *  removal, a departure or a role change, and one elected committer will
       *  sweep it into a commit.
       *
       *  **Never a timeline row.** A request is not something that happened to
       *  the group; it is an intent the system may discard a second later.
       *  Writing "Alice asked to remove Bob" into a permanent, mutually visible
       *  transcript would both leak a pending change to members who cannot act
       *  on it and leave a durable record of an intent that never took effect.
       *  Only OUTCOMES belong in the timeline. So this drives the members
       *  panel, and `createdAt` is the DS row timestamp the panel's quiet
       *  window is measured against — never a local clock, because every member
       *  reads the identical value for the identical row. */
      kind: "membershipProposal";
      messageId: number;
      /** `"add" | "remove" | "leave" | "update" | "role-change" |
       *  "unsupported"` — the wasm `ProposalRecord.kind` verbatim. */
      proposalKind: string;
      /** `false` when the receipt gate REFUSED it: a proposal advances no
       *  state, so dropping it unstored IS the refusal, and the core reports it
       *  as data rather than throwing. A dropped proposal is not a pending
       *  request and must not be surfaced as one. */
      stored: boolean;
      /** The prospective member's 32-byte signature key. `add` only. */
      addSignatureKey: Uint8Array | undefined;
      /** The leaf the proposal removes. `remove` only. */
      removeTargetLeaf: number | undefined;
      /** The proposing member's leaf, or `undefined` for an unresolvable
       *  sender (always unauthorised — fail-closed). */
      senderLeaf: number | undefined;
      createdAt: string;
    }
  | {
      /** ADR-0027 (G-3) — an external-join *request*: a non-member
       *  posted a `Sender::NewMemberProposal` external Add proposal
       *  to this group's journal, and the MLS core retained it
       *  pending an owner/admin approval. This event surfaces the
       *  joiner's leaf signature key (the `proposal_ref` handle the
       *  approve / reject FFI takes) so the members UI can render a
       *  pending-request row. The retained set is the source of
       *  truth (`group.pendingExternalJoins()`); this event is the
       *  reactive nudge that a new request landed. */
      kind: "externalJoinRequest";
      messageId: number;
      /** The joiner's leaf signature public key — the rendezvous
       *  tag + the `proposal_ref` for `approveExternalJoin` /
       *  `rejectExternalJoin`. */
      joinerSignatureKey: Uint8Array;
      /** ADR-0027 — whether the core actually KEPT the request.
       *
       *  Carried rather than dropped, and the distinction is not
       *  cosmetic: the core fires this event even when the
       *  quarantine (`MAX_PENDING_EXTERNAL_JOINS`) was full and the
       *  request was not retained. Such a request can be approved
       *  by nobody — `approveExternalJoin` has nothing to look up
       *  and refuses with `noSuchJoinRequest` — so offering Approve
       *  for it is an affordance that cannot work. Route it through
       *  `externalJoinAffordance` rather than reading this
       *  directly. */
      retained: boolean;
      createdAt: string;
    }
  | {
      /** Phase 3a8 — call-signaling event lifted from an
       *  MLS application message. Driven into the
       *  CallSession state machine by the chat-route
       *  dispatcher. */
      kind: "callSignaling";
      messageId: number;
      /** Plaintext application-payload bytes — caller
       *  feeds them to `parseCallSignaling` (wasm) inside
       *  the call surface. We don't ship the parsed
       *  result on the event itself because the wasm
       *  module isn't loaded eagerly on every poll
       *  iteration; the call surface loads it lazily on
       *  its own mount. */
      payloadBytes: Uint8Array;
      /** MLS-authenticated sender leaf of the frame, when it named
       *  one. The group-call record refuses a `groupStart` /
       *  `groupJoin` / `groupLeave` whose claimed leaf differs. */
      senderLeaf?: number | undefined;
      /** The sender leaf's own (device) signature key, resolved at receipt
       *  (C-68) — the key the group-call roster files a member under. */
      senderMemberKey?: string | undefined;
      /** The sender's person key, resolved at receipt (C-68). */
      senderKey?: MemberIdentity | undefined;
      createdAt: string;
    }
  | {
      /** ADR-0042 — an in-group push-routing-secret distribution
       *  envelope (the group OWNER broadcast the group's
       *  `push_routing_secret`). Never a timeline bubble: the receive
       *  loop persists the secret + subscribes under the derived opaque
       *  `push_gid` (invariant #5). Classified by its domain-separated
       *  envelope prefix via the wasm fail-closed codec. */
      kind: "pushRoutingSecret";
      messageId: number;
      /** The decoded 32-byte `push_routing_secret`. */
      secret: Uint8Array;
      /** ADR-0044 — the MLS epoch the MINTER stamped this secret with (NOT
       *  the epoch this envelope was sealed at: a re-broadcast replays the
       *  original stamp on a freshly sealed message, which is exactly what
       *  lets the gate tell a retry from a stale replay). The receive gate
       *  orders envelopes by `(mintEpoch, secretBytes)` and refuses anything
       *  not strictly newer than what this device holds. */
      mintEpoch: bigint;
      /** The MLS leaf that authored this envelope (ADR-0023 precedent).
       *  The receive-side gate resolves it to the sender's signature
       *  public key via `group.members()` and refuses a CHANGED secret
       *  from anyone but the ADR-0022 role-table OWNER — otherwise any
       *  single member could unilaterally rotate the group's routing id
       *  and strand an orphan push-gw subscription per churn.
       *  `undefined` for a control frame / external sender, which the
       *  gate treats as unverifiable (fail-closed). */
      senderLeaf: number | undefined;
      createdAt: string;
    }
  | {
      /** ADR-0045 decision 2 — a committer-liveness beacon: an owner /
       *  admin announcing it is foregrounded and able to sweep the pending
       *  proposal set. Never a timeline bubble.
       *
       *  The envelope carries NO fields at all (a fixed 27 bytes, identical
       *  on every call and for every sender), deliberately: naming the
       *  sender would add an unauthenticated identity claim next to the
       *  MLS-authenticated one, and a self-reported timestamp would let a
       *  candidate hold the committer duty by asserting its own freshness.
       *  So everything the takeover fold needs comes from the two fields
       *  below — WHO, from the MLS-authenticated sender leaf, and HOW OLD,
       *  from the DS row — which is exactly why this is surfaced as an
       *  event rather than dropped in the parser. The fold itself
       *  (`actingCommitter`) is not built yet; this event is the seam it
       *  will consume. */
      kind: "committerBeacon";
      messageId: number;
      /** The MLS leaf that authored the beacon. The fold resolves it to a
       *  signature public key via `group.members()` and then to an
       *  ADR-0022 committer rank. `undefined` for a control frame /
       *  external sender, which the fold must treat as unusable
       *  (fail-closed — an unattributable beacon proves no candidate is
       *  live). */
      senderLeaf: number | undefined;
      /** The DS row's timestamp — the beacon's age, which is the half of
       *  the freshness question the bytes deliberately do not carry. */
      createdAt: string;
    }
  | {
      /** ADR-0048 §2 trigger 4 — a member asking the group to introduce
       *  itself (`crates/lakko-contacts/src/profile_request.rs`). Never a
       *  timeline bubble: every OTHER member answers by scheduling its own
       *  profile publish for this group through the throttled growth path.
       *
       *  Field-less like the beacon, and for the same reasons: the asker is
       *  already MLS-authenticated through the sender leaf, and the answer
       *  (the ordinary profile envelope) needs nothing from the request. */
      kind: "profileRequest";
      messageId: number;
      /** Diagnostic only. */
      senderLeaf: number | undefined;
      /** The sender's person key resolved at receipt (C-68) — what the
       *  self-check reads, so a device never answers its own echo.
       *  `undefined` ⇒ unattributable, dropped. */
      senderKey: MemberIdentity | undefined;
      createdAt: string;
    }
  | {
      /** ADR-0048 — a member's profile (display name + claimed username).
       *  Never a timeline bubble: the receive loop files it in the profile
       *  store under `senderKey` and nothing else.
       *
       *  `profile` is `undefined` when the domain matched but the envelope
       *  failed its bounds — the core refuses a bad profile as a whole, so the
       *  row is dropped rather than stored or rendered. */
      kind: "profile";
      messageId: number;
      profile: ReceivedProfile | undefined;
      /** Diagnostic only. */
      senderLeaf: number | undefined;
      /** The sender's signature key resolved at receipt (C-68) — the ONLY key
       *  a profile may be filed under. `undefined` ⇒ the profile is dropped. */
      senderKey: MemberIdentity | undefined;
      createdAt: string;
    }
  | {
      /** ADR-0051 — the group's shared name (or a clear, `name === ""`).
       *  Never a timeline bubble: the receive loop runs it through the
       *  role-gated verdict (`chat/group-name-store.ts`) under the per-group
       *  lock and re-titles the chat in place.
       *
       *  `groupName` is `undefined` when the domain matched but the envelope
       *  failed its bounds — the core refuses it as a whole, so the row is
       *  dropped rather than stored or rendered. */
      kind: "group-name";
      messageId: number;
      groupName: StoredGroupName | undefined;
      /** Diagnostic only. */
      senderLeaf: number | undefined;
      /** The sender's ACCOUNT key resolved at receipt (C-68), the key the
       *  ADR-0022 role table names. `undefined` ⇒ unverifiable, dropped. */
      senderKey: MemberIdentity | undefined;
      createdAt: string;
    }
  | {
      /** ADR-0073 R.6 — a successor CLAIM: a device founding the group this
       *  conversation continues in names it here. Never a timeline bubble:
       *  the receive loop runs it through the claim rule
       *  (`chat/successor-claim.ts`) under the per-group lock.
       *
       *  `groupClosed` is `undefined` when the domain matched but the
       *  envelope failed its bounds — dropped, never stored or rendered. */
      kind: "group-closed";
      messageId: number;
      groupClosed: GroupClosedNotice | undefined;
      /** Diagnostic only. */
      senderLeaf: number | undefined;
      /** The sender's ACCOUNT key resolved at receipt (C-68), the key the
       *  ADR-0022 role table names. `undefined` ⇒ unverifiable, dropped. */
      senderKey: MemberIdentity | undefined;
      createdAt: string;
    }
  | {
      /** ADR-0052 decision 3 — a shared pin or unpin of ONE message. Never a
       *  timeline bubble: the receive loop runs it through the role-gated
       *  verdict (`chat/pin-store.ts`) under the per-group lock and the
       *  pinned bar re-derives.
       *
       *  `pin` is `undefined` when the domain matched but the envelope
       *  failed its bounds — the core refuses it as a whole, so the row is
       *  dropped rather than stored or rendered. */
      kind: "pin";
      messageId: number;
      pin: IncomingPin | undefined;
      /** Diagnostic only. */
      senderLeaf: number | undefined;
      /** The sender's ACCOUNT key resolved at receipt (C-68), the key the
       *  ADR-0022 role table names. `undefined` ⇒ unverifiable in a group
       *  with a role table, dropped. */
      senderKey: MemberIdentity | undefined;
      createdAt: string;
    }
  | {
      /** ADR-0059 section 6 — the listed channels one member shares with
       *  this group. Never a timeline bubble: the receive loop files it
       *  under `senderKey` in the shared-channels store (strictly newer
       *  stamp per member) under the per-group lock. `list` is `undefined`
       *  when the domain matched but the envelope failed its bounds: dropped. */
      kind: "shared-channels";
      messageId: number;
      list: SharedList | undefined;
      /** Diagnostic only. */
      senderLeaf: number | undefined;
      /** The sender's ACCOUNT key resolved at receipt (C-68) — the only key
       *  a list may be filed under. `undefined` ⇒ dropped. */
      senderKey: MemberIdentity | undefined;
      createdAt: string;
    }
  | {
      /** ADR-0060 — a scoped-discovery service envelope (an introduction offer
       *  or bundle, a committer delegation, and the DeviceGroup-only
       *  Visibility / ReachState / ADR-0057 RegistrarSeed kinds should one
       *  arrive in a conversation). Dispatched to its handler, never a
       *  timeline row. `payload` is the plaintext, for the handler to decode. */
      kind: "scopedDiscovery";
      messageId: number;
      envelope: ScopedEnvelopeKind;
      payload: Uint8Array;
      senderLeaf: number | undefined;
      /** The sender's ACCOUNT key resolved at receipt (C-68). */
      senderKey: MemberIdentity | undefined;
      createdAt: string;
    }
  | {
      /** ADR-0087 — a group setting that travels beside the group name: the
       *  photo, the allowed reactions, the conversation handle, or (consumed
       *  at receipt, never handed on) a knock seed the owner sent an admin.
       *  Never a timeline bubble. `setting` is `undefined` when the domain
       *  matched but the body failed its bounds: dropped. */
      kind: "group-setting";
      messageId: number;
      setting: GroupSetting | undefined;
      /** Diagnostic only. */
      senderLeaf: number | undefined;
      /** The sender's ACCOUNT key resolved at receipt (C-68). */
      senderKey: MemberIdentity | undefined;
      createdAt: string;
    }
  | {
      /** ADR-0057 — a registrar-seed envelope the group's owner sent one
       *  member. Never a timeline bubble and never handed to a view: the
       *  receive path runs it through the gate in `ds/registrar-seeds.ts`
       *  AT RECEIPT (the live role table's owner is read there) and drops it
       *  from the batch. Listed in the union only so a caller that forgets
       *  to consume it cannot render it. */
      kind: "registrarSeed";
      messageId: number;
      /** The whole envelope, for decode and open. */
      plaintext: Uint8Array;
      /** The sender's ACCOUNT key resolved at receipt (C-68). */
      senderKey: MemberIdentity | undefined;
      createdAt: string;
    }
  | {
      /** `docs/ux/who-can-find-me.md` section 6.4 - a contact request sealed
       *  to one member of this group. Never a timeline bubble, never a
       *  notification body, never handed to a view: consumed at receipt
       *  (`contacts/group-contact-request.ts`), where only the device it is
       *  sealed to opens it. Listed in the union, like `registrarSeed`, only
       *  so a caller that forgets to consume it cannot render it. */
      kind: "groupContactRequest";
      messageId: number;
      /** The whole envelope. */
      plaintext: Uint8Array;
      /** The sender's ACCOUNT key resolved at receipt (C-68). */
      senderKey: MemberIdentity | undefined;
      createdAt: string;
    }
  | {
      /** ADR-0098 — an account-deleted statement. Never a timeline bubble,
       *  never a notification: consumed at receipt
       *  (`chat/account-deleted-envelope.ts`), where it is verified against
       *  this conversation's id and the sender's account key. Listed in the
       *  union, like `registrarSeed`, only so a caller that forgets to
       *  consume it cannot render it. */
      kind: "accountDeleted";
      messageId: number;
      /** The whole envelope. */
      plaintext: Uint8Array;
      /** The sender's ACCOUNT key resolved at receipt (C-68). */
      senderKey: MemberIdentity | undefined;
      createdAt: string;
    }
  | {
      /** A `lakko/…` service envelope none of the sniffers above claimed: a
       *  newer version of a known kind or a kind this build does not know
       *  (the core's `MessageBody::ServiceEnvelope`). Never rendered and never
       *  persisted: the poll and the sweep drop it at receipt, and every view
       *  carries a defensive arm. Listed in the union, like `registrarSeed`,
       *  only so a caller that forgets to drop it cannot render it. */
      kind: "serviceEnvelope";
      messageId: number;
      createdAt: string;
    };

/**
 * The caller's journal cursor, as a read/write pair rather than a plain number.
 *
 * `pollMessages` must re-read it and advance it INSIDE the per-group MLS lock
 * (see the method docs), which it cannot do through a return value — by the time
 * the caller writes `result.cursor` the lock is already released and a queued
 * poll has re-read the stale position. Handing the accessors in is the only
 * shape that keeps the ratchet and the journal position moving together.
 */
export interface PollCursorRef {
  /** The group's current cursor. Called once under the lock, and compared with
   *  the `after` the fetch actually used. */
  read(): number;
  /** Advance the cursor. Called under the lock, only on the applied path. */
  advance(to: number): void;
  /** Called under the lock once the batch has passed the stale check, before
   *  its first row is applied. The place to snapshot state the caller diffs
   *  against the post-apply state (the ADR-0043 membership baseline): a
   *  snapshot taken before the fetch would, under a long poll, predate
   *  whatever another path applied while this one was parked on the network,
   *  and the diff would report that change a second time. */
  beforeApply?(): void;
}

/** What one `pollMessages` attempt did. */
export type PollOutcome =
  /** The batch was fetched and applied under the lock. */
  | "applied"
  /** The cursor moved between the fetch and the lock, so the batch in hand was
   *  stale. Dropped WITHOUT applying and WITHOUT touching the cursor; the next
   *  poll re-fetches from the current position. Only ever returned when a
   *  `PollCursorRef` was supplied. */
  | "cursor-moved"
  /** The batch WAS applied in memory, but the MLS snapshot did not reach disk,
   *  so the cursor was HELD at its old value and these rows will be re-fetched.
   *  The events are still returned — they are real, already decrypted, and the
   *  UI should render them. See `chat/durable-cursor.ts` for why holding is the
   *  only safe direction. */
  | "persist-held";

/** Result of `pollMessages`. */
export interface PollMessagesResult {
  events: IncomingChatEvent[];
  cursor: number;
  hasMore: boolean;
  outcome: PollOutcome;
}

/** Programmer error caught before any network call. */
export class ChatClientArgError extends Error {
  override name = "ChatClientArgError";
}

/**
 * ADR-0073 R.7 case 1 — nothing is sent into a session only removed devices
 * can read. Thrown BEFORE anything is sealed by {@link ChatClient.sendMessage}
 * and {@link ChatClient.broadcastControl} when the installed guard says the
 * group is a one-to-one whose other person's reset this device accepted (the
 * group can never be repaired; the conversation continues in a successor).
 * A caller with something worth keeping holds it for the successor
 * (`chat/held-send.ts`); everything else simply does not go out.
 */
export class HeldForSuccessor extends Error {
  override name = "HeldForSuccessor";
  constructor() {
    super("this session is superseded; the message waits for the conversation's successor");
  }
}

/**
 * ADR-0091 phase 2 section 2 (C-93) - nothing is sealed into a conversation
 * this device is behind in: whatever it sealed would be at an epoch nobody
 * else holds. Thrown BEFORE anything is sealed by
 * {@link ChatClient.broadcastControl} while the device is suspected or
 * confirmed behind, and by {@link ChatClient.sendMessage} once confirmed
 * (while only suspected a send is attempted: its refusal is evidence).
 */
export class ConversationBehindError extends Error {
  override name = "ConversationBehindError";
  constructor() {
    super("this device is behind in this conversation; nothing is sent until it is back");
  }
}

/**
 * Whether `err` is (or wraps, through `cause` or a `ContactsError`'s
 * `variant.cause`) a {@link ConversationBehindError}. Such a refusal is the
 * ADR-0091 stop list working, not a failure: callers that log failures stay
 * quiet on it (live walk 2026-11-19).
 */
export function isConversationBehind(err: unknown): boolean {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && current !== undefined && current !== null; depth += 1) {
    if (current instanceof ConversationBehindError) return true;
    if (typeof current !== "object") return false;
    const next =
      (current as { cause?: unknown }).cause ??
      ((current as { variant?: { cause?: unknown } }).variant?.cause);
    current = next;
  }
  return false;
}

/** Told after every applied poll batch of a group (ADR-0091: the re-entry
 *  runtime sends what it owes once the group reaches an epoch). */
export type PollObserver = (group: WasmGroup) => void;

let pollObserver: PollObserver | null = null;

/** Install (or, with `null`, remove) the one poll observer. */
export function installPollObserver(observer: PollObserver | null): void {
  pollObserver = observer;
}

function notifyPollObserver(group: WasmGroup): void {
  const observer = pollObserver;
  if (observer === null) return;
  try {
    observer(group);
  } catch {
    /* an observer never fails a poll */
  }
}

/** The group's id in standard base64, or `null` when it cannot be read. */
function groupKeyOf(group: WasmGroup): string | null {
  try {
    return base64Encode(group.groupId);
  } catch {
    return null;
  }
}

/** A `401` that is not this device's clock: evidence (ADR-0091 section 1). */
function refusedNotSkewed(err: unknown): DsHttpError | null {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && current !== undefined && current !== null; depth += 1) {
    if (current instanceof DsHttpError) return current.status === 401 && !current.clockSkewed ? current : null;
    if (typeof current !== "object") return null;
    current = (current as { cause?: unknown }).cause;
  }
  return null;
}

/** Whether a publish into `group` is refused as into a dead session. */
export type DeadSessionGuard = (group: WasmGroup) => boolean;

let deadSessionGuard: DeadSessionGuard | null = null;

/**
 * Install (or, with `null`, remove) the one guard every `ChatClient` in the
 * tab consults. Module-level on purpose: a dozen providers each build their
 * own client, and a guard on one of them would leave the profile, pin, name,
 * typing, receipt and call-signaling publishers free to write into the dead
 * session. A guard that throws reads as "not dead" (it must never block a
 * live conversation).
 */
export function installDeadSessionGuard(guard: DeadSessionGuard | null): void {
  deadSessionGuard = guard;
}

function refuseDeadSession(group: WasmGroup): void {
  const guard = deadSessionGuard;
  if (guard === null) return;
  let dead = false;
  try {
    dead = guard(group) === true;
  } catch {
    dead = false;
  }
  if (dead) throw new HeldForSuccessor();
}

/** Wrap any underlying failure (DS, MLS process, etc). */
export class ChatError extends Error {
  override name = "ChatError";
  constructor(message: string, public readonly cause?: unknown) {
    super(message);
  }
}

/** Optional wires the route layer hands to ChatClient. */
export interface ChatClientOptions {
  /** Returns the **canonical** `WasmIdentity` for this session.
   *  See `<ChatProvider>` (1.16e6c1). When unset, the client
   *  falls back to building a throwaway `Identity.fromSeed` per
   *  call — only acceptable for tests; production paths MUST
   *  wire this so MLS state stays coherent across calls. */
  identityProvider?: () => WasmIdentity | null;
  /** Fires after every successful MLS state-mutating call so
   *  the route layer can persist the canonical Identity's
   *  provider snapshot to disk. Must resolve only when the snapshot
   *  was written and throw otherwise (`chat/persist-hooks.ts`): a send
   *  publishes only after it resolved (`persistDurableOrThrow`). */
  onPersist?: () => Promise<void>;
  /** ADR-0054 / ADR-0078 — build the sealed `x-lakko-wake` header value for
   *  a publish to a group's DS `group_id` (`push/group-routing.ts`
   *  `sealedWakeForGroup`): a wake sealed to push-gw, derived from the group's
   *  `push_routing_secret`. Returns `null` when this device does not (yet)
   *  hold the secret or push-gw's key is unavailable — the publish then
   *  carries NO wake header (never the `group_id`, invariant #5). When UNSET
   *  the client wakes nobody at all. Every production client that sends a
   *  waking frame (a chat message, a call offer or end) MUST wire this. */
  wakeSealer?: (groupId: Uint8Array, kind: "message" | "call") => Promise<string | null>;
}

/**
 * Whether a published application message fans a push wake out, and of what
 * class. Since ADR-0054 the wake is the `x-lakko-wake` header of the signed
 * publish itself (see `ds/wake.ts`); the DS forwards it to push-gw.
 *
 *   * `"message"` — the ordinary message wake `sendMessage` has always fired.
 *   * `"call"` — a ring. Same header and same best-effort discipline, but
 *     push-gw is asked for its highest priority and a short TTL, because a
 *     late ring is worse than no ring (`docs/ux/calls.md` §7).
 *   * `null` — no wake at all: the ADR-0042 rule for control envelopes, and
 *     still the default for every one of them but call signaling.
 */
type OutboundWake = "message" | "call" | null;

/** Options for {@link ChatClient.broadcastControl}. */
export interface BroadcastControlOptions {
  /**
   * Fan a wake out for this control frame. Omitted means silence — the
   * ADR-0042 default every existing caller relies on. The call-signaling
   * publisher is the one caller that sets it, and only for the two frames
   * `calls/call-signaling-wake.ts` names.
   */
  readonly wake?: Exclude<OutboundWake, null | "message">;
  /**
   * ADR-0073 R.6 — the ONE frame that is posted into a superseded
   * one-to-one on purpose: the successor claim, which the person's other
   * devices read there. Everything else is refused ({@link HeldForSuccessor}).
   */
  readonly intoSupersededSession?: boolean;
  /**
   * ADR-0099 section 1 - the frame is an edit, a delete or a reaction (a
   * chat envelope sent without a wake), so it is attested like a message.
   * Never set for a typing signal, a read receipt or a service envelope; a
   * service envelope is not wrapped even when it is.
   */
  readonly attest?: boolean;
  /**
   * ADR-0091 live walk 3 — an action the person took (an edit, a deletion, a
   * reaction, a poll or checklist op, a shared pin, a rename, a group photo or
   * reactions setting, a handle, a contact request inside a group). Held
   * only while the device is suspected or confirmed behind, never for a
   * refused read alone: then it is attempted, and its refusal is evidence
   * the person sees. Omitted: an automatic send, held also after a refused
   * read until a read is answered.
   */
  readonly personAction?: boolean;
}

export class ChatClient {
  /** The receive path's cursor-may-lag-never-lead gate. One per client, which
   *  is the right granularity because `snapshotStorage()` serialises the whole
   *  provider — see `chat/durable-cursor.ts`. */
  private readonly cursorGate = new DurableCursorGate();
  private readonly identityProvider: (() => WasmIdentity | null) | undefined;
  private readonly onPersist: (() => Promise<void>) | undefined;
  private readonly wakeSealer:
    | ((groupId: Uint8Array, kind: "message" | "call") => Promise<string | null>)
    | undefined;

  constructor(
    private readonly ds: DsClient = new DsClient(),
    options: ChatClientOptions = {},
  ) {
    this.identityProvider = options.identityProvider;
    this.onPersist = options.onPersist;
    this.wakeSealer = options.wakeSealer;
  }

  /**
   * Resolve the `WasmIdentity` to drive an MLS operation. When
   * a canonical accessor is wired (production), we hand back
   * its instance — same provider for every call, mutations
   * accumulate, snapshots capture the full state. When unset,
   * we fall back to deterministic `Identity.fromSeed` so tests
   * and standalone callers keep working.
   *
   * Throws `ChatClientArgError` if neither path produces an
   * Identity (i.e. canonical is wired but the user has no
   * fresh export key in scope) — the caller should surface a
   * "please re-enter your password" UI.
   */
  private async resolveIdentity(
    secrets: IdentitySecrets | null,
  ): Promise<WasmIdentity> {
    if (this.identityProvider !== undefined) {
      const canonical = this.identityProvider();
      if (canonical !== null) return canonical;
      throw new ChatClientArgError(
        "no canonical Identity in session — re-enter your password",
      );
    }
    if (secrets === null) {
      throw new ChatClientArgError("session has no Identity in memory");
    }
    const wasm = await loadWasm();
    // ADR-0017: the canonical AccountIdentity is seeded from the
    // BIP39 phrase seed, not the OPAQUE export key.
    return wasm.Identity.fromSeed(secrets.accountIdentitySeed);
  }

  /** Persist the canonical Identity's provider snapshot durably,
   *  AWAIT the IDB write, and RE-THROW on failure.
   *
   *  Used by BOTH paths that write MLS state, for two different reasons that
   *  happen to want the same discipline:
   *
   *   - `sendMessage` / `broadcastControl` call it BEFORE publishing, so the
   *     gen-N ciphertext never reaches the DS without its N+1 ratchet state on
   *     disk (the AEAD nonce-reuse-on-crash window, below);
   *   - `applyPolledBatch` calls it BEFORE advancing the journal cursor, so the
   *     cursor can never lead the durable state (see `chat/durable-cursor.ts`).
   *     The receive path used to swallow this failure, which since ADR-0045
   *     made a lost proposal a permanent epoch strand.
   *
   *  A swallowed persist failure on the send path would
   *  re-open the AEAD nonce-reuse-on-crash window: if the
   *  post-`createApplicationMessage` (generation N+1) ratchet
   *  state is not durably on disk, the gen-N ciphertext must not
   *  be published, so the caller can safely abort and re-consume
   *  generation N after a reload.
   *
   *  When `onPersist` is unset (tests / standalone callers with
   *  no reload-from-IDB path) this is a no-op that resolves: such
   *  callers persist nothing, so a reload restores no group and
   *  cannot re-consume a generation — they are outside this bug's
   *  blast radius. Production paths (`<ChatProvider>`) always wire
   *  `onPersist`, so the fail-closed gate is live there. */
  private async persistDurableOrThrow(): Promise<void> {
    if (this.onPersist === undefined) return;
    await this.onPersist();
  }

  /**
   * Encrypt `plaintext` for the current group epoch and post
   * the wire bytes to the DS. Returns the DS-assigned id.
   *
   * Caller is responsible for keeping `group` alive — the
   * Group instance carries the MLS ratchet state. With
   * `identityProvider` wired (1.16e6c2), the underlying provider
   * storage is persisted to IDB **before** the publish, and a
   * failed persist ABORTS the send (fail-closed) so the gen-N
   * ciphertext never reaches the DS without its N+1 ratchet state
   * durably on disk — see the persist-before-publish comment
   * below for the nonce-reuse-on-crash rationale.
   *
   * ## The seal and the publish are SERIALIZED against epoch changes
   *
   * Runs under the same per-group `withGroupMlsLock` the four
   * `ContactsClient` create-commit regions, the poll's apply half and the
   * ADR-0045 sweep take. The hazard is NOT two `createApplicationMessage`
   * calls interleaving — that wasm call is synchronous, so they cannot — nor
   * two same-epoch messages arriving out of order, which MLS resolves by
   * generation. It is that a message can be SEALED at one epoch and PUBLISHED
   * on the far side of a commit that changes it. `lakko-mls` never sets
   * `max_past_epochs`, so it is openmls's default of **0**: a recipient keeps
   * no past-epoch secrets, and a straddling message is not late but
   * permanently undecryptable for everyone, dropped silently while the
   * sender's own optimistic echo stays on screen.
   *
   * Both directions were reachable while this path held no lock:
   *
   *   - a commit region eager-merges N -> N+1 *inside* its create and then
   *     parks in persist + publish, so a send in that window seals at N+1 and
   *     can reach the DS first — a future epoch to peers still at N;
   *   - an ADR-0045 sweep STAGES (the group stays at N), publishes the commit,
   *     then spends a second round-trip on the journal verdict.
   *     `MlsGroup::create_message` gates on `is_active()`, not
   *     `is_operational()`, so a pending commit does not block a send: it
   *     seals at N with the commit already on the wire, which makes the loss a
   *     certainty rather than a race for that whole window.
   *
   * The lock closes the LOCAL half only, and that bound is real: a poll
   * applying somebody ELSE's commit is a race against the DS, not against
   * local state, so a message sealed here and published after a remote commit
   * that was already ordered ahead of it is still lost. Closing that needs
   * `max_past_epochs > 0` in `lakko-mls` (invariant #3 — an ADR and property
   * tests), not a client-side lock.
   *
   * The DS publish is INSIDE the region, deliberately: the whole hazard is
   * seal-then-publish-late, so leaving the publish out would fix nothing. That
   * is not the ADR-0044 wedge shape — `withGroupMlsLock` releases on the
   * task's rejection as well as its resolution, so a failed publish frees the
   * queue. The cost is a stall, and it is bounded: no caller of this lock long
   * polls (the sweep's own verdict read is `waitSecs: 0`). Sends on one group
   * now serialize, which is a latency cost on back-to-back sends and is also
   * what makes the concurrent-persist overlap below unreachable.
   */
  async sendMessage(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    plaintext: Uint8Array,
  ): Promise<SendMessageResult> {
    if (plaintext.length === 0) {
      throw new ChatClientArgError("plaintext must be non-empty");
    }
    // ADR-0073 R.7 case 1 — before anything is sealed.
    refuseDeadSession(group);
    // ADR-0091 section 2 — once confirmed behind, nothing is sent.
    {
      const key = groupKeyOf(group);
      if (key !== null && conversationBehind.isConfirmed(key)) throw new ConversationBehindError();
    }
    const myIdentity = await this.resolveIdentity(identity);

    // Read BEFORE the lock is keyed and before anything is sealed — a
    // synchronous getter that touches no mutable state.
    let groupId: Uint8Array;
    try {
      groupId = group.groupId;
    } catch (err) {
      throw new ChatError("group.groupId unavailable", err);
    }

    // ADR-0048 §2 — a profile this device owes the group goes out first, so
    // nobody reads this message before this device's name. Before the lock:
    // the profile is published under the same lock.
    await runBeforeSend(base64Encode(groupId));

    // ADR-0099 section 1 - every envelope sent through here is a chat
    // envelope (a message, a reply, a forward, a reaction that notifies), so
    // it is attested; a service envelope is never wrapped whatever the flag.
    return withGroupMlsLock(base64Encode(groupId), async () =>
      this.sealAndPublish(myIdentity, group, groupId, plaintext, "message", true),
    );
  }

  /**
   * The serialized body shared by {@link sendMessage} and
   * {@link broadcastControl}: seal at the live epoch, persist fail-closed,
   * publish. Runs under `withGroupMlsLock`; never call it directly.
   *
   * `wake` is the ONLY difference between the two callers. `sendMessage`
   * always passes `"message"`. `broadcastControl` passes `null` for the
   * control envelopes ADR-0042 rules must wake nobody — a routing-secret
   * handshake, a liveness beacon, a profile, a group name — and `"call"` for
   * the two signaling frames that must ring a closed tab (see
   * `calls/call-signaling-wake.ts` for which two, and why only those).
   */
  private async sealAndPublish(
    myIdentity: WasmIdentity,
    group: WasmGroup,
    groupId: Uint8Array,
    plaintext: Uint8Array,
    wake: OutboundWake,
    attest: boolean,
  ): Promise<SendMessageResult> {
    // ADR-0099 section 1 - the attested wrapper, BEFORE the MLS encrypt and
    // before anything else in this region: a refusal of the core aborts the
    // send with nothing sealed.
    let sealedPlaintext: Uint8Array = plaintext;
    // The core that seals the message signs it; a page without one (only a
    // unit test with no wasm) has nothing to sign with.
    const core = attest ? wasmIfLoaded() : null;
    if (core !== null) {
      try {
        sealedPlaintext = attestOutgoing(core, myIdentity, groupId, plaintext, attest);
      } catch (err) {
        throw new ChatError("attestMessage failed", err);
      }
    }
    // ADR-0054 — the wake rides the signed publish as a header, so the
    // `push_gid` is resolved BEFORE anything is sealed: nothing awaits between
    // the ratchet advance and the persist below except the persist itself.
    const dsWake = wake === null ? null : await this.resolveDsWake(groupId, wake);

    // `createApplicationMessage` ADVANCES the MLS sender ratchet:
    // it consumes the current generation N (deriving the AEAD
    // key+nonce from N) and leaves the in-memory OpenMLS provider
    // at generation N+1. Because the provider is in-memory
    // (MemoryStorage), that N+1 state lives ONLY in the snapshot
    // until it is persisted.
    let wireBytes: Uint8Array;
    try {
      wireBytes = group.createApplicationMessage(myIdentity, sealedPlaintext);
    } catch (err) {
      throw new ChatError("createApplicationMessage failed", err);
    }

    // SECURITY (nonce-reuse-on-crash) — PERSIST BEFORE PUBLISH,
    // FAIL-CLOSED. The N+1 ratchet state produced just above MUST
    // be durably on disk before the gen-N ciphertext reaches the
    // DS. Otherwise a crash (or a silently-failed persist) between
    // the publish and the persist would reload the ratchet at N,
    // and the next send would re-consume generation N — an
    // identical (key, nonce) on a different plaintext
    // (C1 XOR C2 = P1 XOR P2 plaintext leak to the zero-trust
    // server + Poly1305 tag forgery). So: snapshot the
    // post-advance state and AWAIT it; if it does not durably
    // land, ABORT — do NOT call `publishGroup` — so the gen-N
    // ciphertext never leaves this device. Only one gen-N
    // ciphertext can ever exist on the wire, so gen N stays safe
    // to re-consume after a reload. A crash in the NEW window
    // (after this persist, before the publish) merely SKIPS
    // generation N; RFC 9420 receivers tolerate sender-ratchet
    // gaps, so there is no reuse either way.
    try {
      await this.persistDurableOrThrow();
    } catch (err) {
      throw new ChatError(
        `MLS snapshot persist failed — ${
          wake === "message" ? "send" : "control broadcast"
        } aborted to prevent nonce reuse`,
        err,
      );
    }

    let publish;
    try {
      publish = await this.ds.publishGroup(groupId, wireBytes, { wake: dsWake });
    } catch (err) {
      // ADR-0091 section 1 — a publish refused 401, clock not skewed, is
      // evidence that this device is behind.
      if (refusedNotSkewed(err) !== null) conversationBehind.notePublishRefused(base64Encode(groupId));
      throw new ChatError("DS publish failed", err);
    }
    // Keyed by GROUP, and shared with every other `ChatClient` on this page, so
    // a service envelope published outside the chat route (a profile right
    // after a Welcome) is still recognised as an echo there.
    markOwnPublished(base64Encode(groupId), publish.id);
    // Live walk 2 — the newest row this device knows in the conversation
    // starts a later gap (ADR-0091 section 2), known before any request.
    conversationBehind.noteOwnRowAccepted(base64Encode(groupId), rowTimeMs(publish.createdAt));

    return { messageId: publish.id, createdAt: publish.createdAt };
  }

  /**
   * ADR-0054 decision 1 — the wake a publish carries. The DS forwards it to
   * push-gw once the ADR-0053 proof gate passed and the row was appended; this
   * client never calls push-gw to wake anybody.
   *
   * The wake names the opaque ADR-0042 `push_gid`, NOT the DS `group_id`. No
   * resolver wired, a resolver that yields `null` (this device holds no routing
   * secret for the group yet) and a resolver that fails all mean the same
   * thing: the publish goes out with NO wake header. Never the `group_id` in
   * its place (invariant #5). Recipients still receive the row on their next
   * foreground poll; the wake resumes once the secret propagates.
   *
   * A failure here never fails the send — it was best-effort when the wake was
   * a separate push-gw request, and it stays so now that it is a header.
   *
   * `wake` names the class: `"message"` for an ordinary message, `"call"` for
   * the two signaling frames that must ring a closed tab (push-gw raises the
   * transport priority and shortens the TTL for it, `docs/ux/calls.md` §7).
   */
  private async resolveDsWake(
    groupId: Uint8Array,
    wake: Exclude<OutboundWake, null>,
  ): Promise<DsWake | null> {
    if (this.wakeSealer === undefined) return null;
    let headerValue: string | null;
    try {
      headerValue = await this.wakeSealer(groupId, wake);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("wake sealing failed; publishing without a wake", err);
      return null;
    }
    if (headerValue === null) return null;
    if (headerValue.length !== DS_WAKE_HEADER_LEN) {
      // A header the DS would answer with 400 would fail the publish itself,
      // after the ratchet has already advanced; drop the wake instead.
      // eslint-disable-next-line no-console
      console.warn(`sealed wake has ${headerValue.length} characters; publishing without a wake`);
      return null;
    }
    return { headerValue };
  }

  /**
   * Register a DS message id as one **we** published into `groupIdB64` (Phase
   * 1.18g) — typically a commit or proposal returned by
   * `ContactsClient.addMemberToGroup`'s broadcast step.
   *
   * The next `pollMessages` of THAT group will see this id and drop it — the
   * same suppression path `sendMessage` uses for application messages. Without
   * registration the sender's own pollMessages would hit `processIncoming` on
   * its own outgoing commit at the already-advanced epoch, fail, log a noisy
   * console warning, and render a phantom system row in the timeline.
   *
   * ## The group is not optional
   *
   * A DS message id is per-group monotonic, so it identifies a row only
   * together with its journal. One `ChatClient` serves every group here — the
   * background receive loop polls them all through a single instance — so a
   * registry that only held ids would make "row N is my own echo" true for
   * every group at once: marking a sweep commit at row N in chat A would
   * SUPPRESS an unrelated row N in chat B, whose cursor has already moved past
   * it, losing that message permanently. Hence the group-keyed registry in
   * `chat/own-published.ts`, and hence this parameter.
   *
   * Crosses the client boundary because `ContactsClient` owns the addMember
   * flow (discovery resolution + Welcome shipping) but the echo registry is the
   * receive path's; a thin accessor keeps the surfaces independent.
   */
  markOwnPublishedCommit(groupIdB64: string, messageId: number): void {
    markOwnPublished(groupIdB64, messageId);
  }

  /**
   * ADR-0042 — broadcast a CONTROL application message to the group
   * journal (currently: the push-routing-secret distribution envelope).
   *
   * Mirrors `sendMessage`'s persist-before-publish fail-closed discipline
   * — `createApplicationMessage` advances the sender ratchet, so the
   * post-advance state MUST be durably persisted before the ciphertext
   * reaches the DS (the AEAD nonce-reuse-on-crash class). Unlike
   * `sendMessage` it fires NO user-facing push wake BY DEFAULT: a
   * routing-secret handshake should not wake devices (a member without the
   * secret cannot be woken under `push_gid` yet anyway — they catch up on the
   * next foreground poll).
   *
   * ## The opt-in wake
   *
   * `options.wake` is how a caller says this particular control frame IS
   * worth a wake. It exists for one caller — the call-signaling publisher,
   * for a call offer and a call end (`calls/call-signaling-wake.ts`) — because
   * a ring that only arrives on the next foreground poll is not a ring at
   * all (`docs/ux/calls.md` §2.1). It is opt-IN rather than opt-out on
   * purpose: every existing caller keeps its silence without being edited,
   * and a new control envelope is silent until someone deliberately decides
   * otherwise, which is the ADR-0042 default stated as a default.
   *
   * The wake is fanned out AFTER the DS publish, unawaited, with every
   * failure swallowed, exactly as a message wake is — so opting in cannot
   * make a publish slower or a call fail.
   *
   * Records the DS id in this group's echo registry so the broadcaster's own
   * next poll drops the echo (the owner already holds the secret it minted).
   *
   * Serialized under the same per-group `withGroupMlsLock` as `sendMessage`,
   * for the epoch-straddle reason documented there — it shares that method's
   * body verbatim (`sealAndPublish`), differing only in firing no push wake.
   * Both are in scope because both reach the identical seal-then-publish
   * window; the payloads differ only in what a loss costs. A lost beacon is
   * absorbed by ADR-0045's freshness window (sized for exactly one), a lost
   * routing secret self-heals on the next re-broadcast, and a lost chat
   * message does not come back at all. This is also the timer-driven caller
   * set — the ADR-0045 beacon on its 15 s tick, and via `sendMessage` the
   * ADR-0024 typing throttle and read-receipt debounce, which have been
   * firing off timers since long before the beacon existed.
   */
  async broadcastControl(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    controlBytes: Uint8Array,
    options: BroadcastControlOptions = {},
  ): Promise<{ messageId: number }> {
    if (controlBytes.length === 0) {
      throw new ChatClientArgError("controlBytes must be non-empty");
    }
    if (options.intoSupersededSession !== true) refuseDeadSession(group);
    // ADR-0091 section 2 (C-93) — no repeat envelope, typing signal, read
    // receipt or call signal while this device is behind: each would be
    // sealed at an epoch nobody can open. Live walk 3: nor after a refused
    // read until a read is answered again.
    {
      const key = groupKeyOf(group);
      const held =
        key !== null &&
        (options.personAction === true ? conversationBehind.stopsActing(key) : conversationBehind.holdsAutomaticSends(key));
      if (held) throw new ConversationBehindError();
    }
    const myIdentity = await this.resolveIdentity(identity);

    let groupId: Uint8Array;
    try {
      groupId = group.groupId;
    } catch (err) {
      throw new ChatError("group.groupId unavailable", err);
    }

    const result = await withGroupMlsLock(base64Encode(groupId), async () =>
      this.sealAndPublish(
        myIdentity,
        group,
        groupId,
        controlBytes,
        options.wake ?? null,
        options.attest === true,
      ),
    );
    return { messageId: result.messageId };
  }

  /**
   * Pull all rows newer than `after` for the group's journal,
   * decrypt application messages, return discriminated events
   * + a fresh cursor. Persists the MLS snapshot once at the
   * end if any rows were processed (each `processIncoming`
   * mutates provider storage — application messages drain
   * keys, commits replace state, proposals queue).
   *
   * ## G-38 residual 1 (local sub-case) — the apply is SERIALIZED
   *
   * `processIncomingVerifyingRoles` mutates the same in-memory provider the
   * four `ContactsClient` create-commit regions eagerly merge into, so a poll
   * applying a batch while a local commit is parked in its publish is the
   * ADR-0040 interleave with the ADR-0040 outcome. The apply therefore runs
   * under the SAME per-group `withGroupMlsLock` those regions take — one lock,
   * one resource. Android closed this first; `chat/SplitPoll.kt`'s
   * `runSplitPoll` documents the identical split step by step.
   *
   * The FETCH stays OUTSIDE the lock. It touches no MLS state, and parking a DS
   * round-trip behind a commit publish is G-37's own defect at a per-group blast
   * radius. It matters on web too: the chat route's automatic receive loop
   * long-polls (`waitSecs` = `LONG_POLL_WAIT_SECS`, see `ds/long-poll-loop.ts`),
   * and a lock that spanned that fetch would stall every send on the group
   * for the full parked duration.
   *
   * ## The cursor moves INSIDE the lock, or not at all
   *
   * The DS journal position and the MLS ratchet advance together or they
   * desynchronise, so `cursor.advance` is called in the same region as the
   * apply. Leaving it to the caller (which is what `setCursor(result.cursor)`
   * in `routes/Chat.tsx` used to do) is not equivalent and was in fact broken:
   * the caller's write lands a microtask AFTER the lock is released, so a second
   * poll queued behind the first re-reads the OLD cursor, decides its batch is
   * fresh, and re-feeds already-consumed rows to OpenMLS.
   *
   * `cursor.read` is therefore called TWICE — once to produce `after` (by the
   * caller, before the fetch) and again under the lock. If the two disagree the
   * batch in hand starts at a row someone else has already consumed, so it is
   * DROPPED whole: no apply, no cursor write, `outcome: "cursor-moved"`. Applying
   * it anyway would re-feed consumed rows and, worse, let the stale batch's end
   * id wind the cursor BACKWARD. The next poll re-fetches from the current
   * position.
   *
   * `cursor` is optional so a caller that owns the position itself (tests,
   * one-shot callers) keeps the previous contract: no re-read gate, no advance,
   * and `result.cursor` to apply by hand.
   */
  async pollMessages(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    after: number,
    waitSecs = 0,
    cursorRef?: PollCursorRef,
    signal?: AbortSignal,
  ): Promise<PollMessagesResult> {
    const myIdentity = await this.resolveIdentity(identity);

    const groupId: Uint8Array = group.groupId;
    // ADR-0091 phase 1 - a registered commit of this group failed and waits
    // for its next attempt: do not fetch (the read would stop at the same row
    // at once and spin). Wait out the long poll or the attempt, whichever is
    // first, and report nothing new.
    const holdWaitMs = registeredCommitHolds.msUntilDue(base64Encode(groupId));
    if (holdWaitMs > 0) {
      await sleepUnlessAborted(Math.min(waitSecs * 1000, holdWaitMs), signal);
      const current = cursorRef?.read() ?? after;
      return { events: [], cursor: current, hasMore: false, outcome: "applied" };
    }
    // The FETCH — deliberately outside the lock. `signal` aborts only this
    // half: once a batch is in hand it is applied (or dropped as stale) under
    // the lock like any other, because abandoning a fetched batch mid-apply is
    // exactly the torn state the lock exists to prevent.
    let raw;
    try {
      raw = await this.ds.fetchGroup(groupId, {
        after,
        waitSecs,
        ...(signal !== undefined ? { signal } : {}),
      });
    } catch (err) {
      // ADR-0091 section 1 — consecutive refused reads (the device's key left
      // the read tier) make it suspect it is behind; a skewed clock does not.
      const refused = err instanceof DsHttpError && err.status === 401 ? err : null;
      if (refused !== null) {
        conversationBehind.noteRefusedRead(base64Encode(groupId), {
          clockSkewed: refused.clockSkewed,
          localEpoch: readGroupEpoch(group),
          ownCommitOwed: readHasStagedSweep(group),
        });
      }
      throw new ChatError("DS fetch failed", err);
    }
    // ADR-0091 phase 1b - a snapshot rolled back under an advanced cursor:
    // the group was loaded below the epoch recorded with its cursor, so the
    // commit leaving the loaded epoch lies behind the cursor. Read again from
    // just before it (`chat/cursor-epoch.ts`). The cursor itself never moves
    // backward; the rows between are re-applied and re-filed by DS id.
    let readFrom = after;
    {
      const loaded = readGroupEpoch(group);
      const recorded = readCursorEpoch(base64Encode(groupId));
      const rewind = snapshotRewindFrom({
        after,
        loaded,
        recorded,
        registeredCommits: raw.registeredCommits,
      });
      if (rewind !== undefined && loaded !== undefined && recorded !== undefined) {
        // eslint-disable-next-line no-console
        console.warn(rewindLine(loaded, recorded));
        try {
          raw = await this.fetchRewound(groupId, rewind, after, signal);
        } catch (err) {
          throw new ChatError("DS fetch failed", err);
        }
        readFrom = rewind;
      }
    }

    // ADR-0091 section 5 — the first poll after a re-entry reads again from
    // the commit that brought this device back: rows sent after it that were
    // skipped before the Welcome arrived are applied now.
    const reentryMark = readFrom === after ? reentryReadFrom(base64Encode(groupId)) : undefined;
    if (reentryMark !== undefined && reentryMark < after) {
      try {
        raw = await this.fetchRewound(groupId, reentryMark, after, signal);
      } catch (err) {
        throw new ChatError("DS fetch failed", err);
      }
      readFrom = reentryMark;
    }

    // ADR-0053 contract point 7 — an owner or admin also reads the group's
    // KNOCK queue, where a `/g/` joiner's external-join proposal and its
    // attribution now arrive (a non-member can no longer write the journal).
    // Read after the journal returns, never long-polled: a knock costs one
    // poll period of latency, which the approval it waits on dwarfs. A failed
    // read is logged and retried next poll; it never fails the journal poll.
    const groupIdB64 = base64Encode(groupId);
    let knocks: { after: number; rows: FetchResult; owner: string } | null = null;
    // The knock cursor is the signed-in account's (`knock-cursor.ts`), pinned
    // here so the read and the write of one batch name the same account. With
    // no account bound there is nowhere to record progress, so the queue is
    // not read at all rather than re-fed on every poll.
    const knockOwner = boundContactsAccount();
    if (
      knockOwner !== null &&
      readsKnocks(group, myIdentity) &&
      takeKnockReadSlot(groupIdB64, Date.now())
    ) {
      const knockAfter = readKnockCursor(groupIdB64, knockOwner);
      try {
        knocks = {
          after: knockAfter,
          rows: await this.ds.fetchKnocks(groupId, { after: knockAfter, waitSecs: 0 }),
          owner: knockOwner,
        };
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn(`ChatClient: knock read failed; retried next poll: ${errorLogTag(err)}`);
      }
    }

    // The APPLY + the cursor advance — inside the lock, atomic together.
    return withGroupMlsLock(groupIdB64, async () => {
      const result = await this.applyPolledBatch(myIdentity, group, after, raw, cursorRef, readFrom);
      if (result.outcome === "applied") {
        if (reentryMark !== undefined) clearReentryReadFrom(groupIdB64);
        await this.observeBehind(group, raw, result);
        notifyPollObserver(group);
      }
      if (result.outcome !== "applied") return result;
      if (knocks === null) {
        // Section 8.1 — no knock read this time: the notices still follow the
        // role and the pending set (a demoted admin's are dropped).
        if (knockOwner !== null) announceJoinRequestRead(group, myIdentity, knockOwner, null);
        return result;
      }
      const knockEvents = await this.applyKnocks(
        myIdentity,
        group,
        knocks.after,
        knocks.rows,
        knocks.owner,
      );
      announceJoinRequestRead(group, myIdentity, knocks.owner, knockEvents);
      return knockEvents.length === 0
        ? result
        : { ...result, events: [...result.events, ...knockEvents] };
    });
  }

  /**
   * ADR-0091 section 1 — fold one applied read into the conversation's
   * behind state. Drained: the read returned no row and nothing more, and
   * nothing was held. The ADR-0040 outbox is read only when the read could
   * suspect (it is an IndexedDB read), and a staged sweep always counts.
   */
  private async observeBehind(group: WasmGroup, raw: FetchResult, result: PollMessagesResult): Promise<void> {
    const key = groupKeyOf(group);
    const local = readGroupEpoch(group);
    if (key === null || local === undefined) return;
    const drained = raw.messages.length === 0 && !raw.hasMore && !result.hasMore && registeredCommitHolds.heldRowId(key) === undefined;
    const latest = raw.latestEpoch === undefined ? undefined : BigInt(raw.latestEpoch);
    let ownCommitOwed = readHasStagedSweep(group);
    const mightSuspect = (drained && latest !== undefined && latest > local) || conversationBehind.state(key).kind !== "in-step";
    if (!ownCommitOwed && mightSuspect) {
      try {
        ownCommitOwed = (await loadUnacked(key)).length > 0;
      } catch {
        // Unknown: counted as owed, so nothing suspects on a guess.
        ownCommitOwed = true;
      }
    }
    conversationBehind.observeRead(key, {
      ownCommitOwed,
      drained,
      latestEpoch: latest,
      localEpoch: local,
      registeredCommits: raw.registeredCommits,
    });
  }

  /**
   * ADR-0053 contract point 7 — feed knock rows to the SAME receive path the
   * journal rows use, under the lock the caller holds. Only an external-join
   * request can come out of a knock (a non-member cannot encrypt anything
   * else); an attribution is not an MLS message and is skipped here, and read
   * by `ContactsClient.fetchJoinAttributions` instead. Anything else a knock
   * row decodes to is dropped rather than rendered: the queue is written by
   * strangers.
   */
  private async applyKnocks(
    myIdentity: WasmIdentity,
    group: WasmGroup,
    after: number,
    raw: FetchResult,
    owner: string,
  ): Promise<IncomingChatEvent[]> {
    const groupIdB64 = base64Encode(group.groupId);
    // A concurrent poll consumed this batch already.
    if (readKnockCursor(groupIdB64, owner) !== after) return [];
    const events: IncomingChatEvent[] = [];
    let last = after;
    let queueFull = false;
    for (const row of raw.messages) {
      // ADR-0087 section 5 — a sealed KeyPackage row: opened with the knock
      // seed of its own version, retained with `retainJoinRequest`, and
      // approved later from the retained package. The cursor stops while the
      // queue is full, so the backlog stays on the DS instead of being lost.
      const sealed = await this.applySealedKnockRow(myIdentity, group, row);
      if (sealed !== "not-sealed") {
        if (sealed === "queue-full") {
          queueFull = true;
          break;
        }
        last = row.id;
        if (typeof sealed === "object") events.push(sealed);
        continue;
      }
      last = row.id;
      let processed;
      try {
        // `processKnock`, never `processIncomingVerifyingRoles`: a knock row is
        // an unauthenticated write, so a MEMBER can post a real commit or an
        // application message there. The core refuses anything that is not a
        // stranger's external-join proposal for this group BEFORE processing
        // it; applying such a row here would move only the admins who read
        // knocks and fork them from everyone else.
        processed = group.processKnock(myIdentity, row.payload);
      } catch (err) {
        // A refused row (a member's commit or message, a proposal for another
        // group), an attribution (not MLS), a stale-epoch proposal, garbage.
        // Dropped; the knock cursor still moves past it.
        // eslint-disable-next-line no-console
        console.warn(`ChatClient: knock row dropped: ${errorLogTag(err)}`);
        continue;
      }
      if (processed.kind !== "ExternalJoinProposal") continue;
      const event = await toEvent(
        row.id,
        row.createdAt,
        processed,
        (leaf) => resolveMemberIdentity(rosterOf(group), leaf),
        (leaf) => resolveLeafSignatureKey(rosterOf(group), leaf),
        group,
      );
      if (event.kind === "externalJoinRequest") events.push(event);
    }
    if (queueFull) {
      // eslint-disable-next-line no-console
      console.info("ChatClient: join requests queue full; knock cursor held");
    }
    if (last === after) return events;
    // The retained request lives in the provider (ADR-0027), so it must be
    // durable before the cursor moves past the knock that carried it.
    try {
      await this.persistDurableOrThrow();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`ChatClient: knock batch not persisted; cursor held: ${errorLogTag(err)}`);
      return events;
    }
    writeKnockCursor(groupIdB64, last, owner);
    return events;
  }

  /**
   * One knock row in the ADR-0087 sealed form, or `"not-sealed"` for any other
   * row (the earlier external-join proposal form is read by `processKnock`).
   * Returns the request event, `"queue-full"` (stop the cursor before this
   * row), or `"dropped"` (a row nobody here can read, another group's, a
   * private group's row without a capability of this group, an inadmissible
   * package). Never throws.
   */
  private async applySealedKnockRow(
    myIdentity: WasmIdentity,
    group: WasmGroup,
    row: { readonly id: number; readonly createdAt: string; readonly payload: Uint8Array },
  ): Promise<IncomingChatEvent | "queue-full" | "dropped" | "not-sealed"> {
    // The shape test runs on the already-loaded module (the chat view loads it
    // before any poll); a module that is not loaded reads every row as the
    // earlier form, which `processKnock` then judges.
    let version: number | undefined;
    try {
      if (!wasmIsKnockRowLen(row.payload.length)) return "not-sealed";
      version = wasmKnockRowVersion(row.payload);
    } catch {
      return "not-sealed";
    }
    if (version === undefined) return "not-sealed";
    const wasm = await loadWasm();
    const groupId = group.groupId;
    const groupIdB64 = base64Encode(groupId);
    // Every seed this device holds for the group, the row's own version
    // first: a request found by search names the initial version whatever
    // key it was sealed to (the public record carries the key alone).
    const seeds = await knockSeedsForRow(groupId, groupIdB64, version);
    let request: ReturnType<typeof wasm.openKnockRow> | undefined;
    for (const seed of seeds) {
      try {
        request = wasm.openKnockRow(seed, row.payload);
        break;
      } catch {
        /* another version's key */
      }
    }
    if (request === undefined) {
      // ADR-0091 section 3, route 3 — a member's request to be brought back,
      // sealed to the same knock key under its own label. Handed to the
      // re-admitter; the knock cursor moves past it either way.
      for (const seed of seeds) {
        let reentry: ReturnType<typeof wasm.openReentryKnockRow> | undefined;
        try {
          reentry = wasm.openReentryKnockRow(seed, row.payload);
        } catch {
          continue;
        }
        try {
          // A row in this group's queue speaks for this group only.
          if (base64Encode(reentry.groupId) !== groupIdB64) break;
          deliverReentryRequest({
            route: "knock",
            groupId: reentry.groupId,
            epochLeft: reentry.epochLeft,
            gapFromMs: reentry.gapFromMs,
            gapKey: reentry.gapKey,
            keyPackage: reentry.keyPackage,
          });
        } finally {
          reentry.free();
        }
        break;
      }
      return "dropped";
    }
    try {
      // The link the request came through, when it came through one (its
      // issuer is the "Invited by" line, UX section 8). A private group takes
      // a row only with a capability of this group.
      let issuerKeyB64: string | null = null;
      const cap = request.capability;
      if (cap !== undefined && cap.length > 0) {
        try {
          const c = wasm.GroupInviteCapability.decodeAndVerify(cap);
          const same = base64Encode(c.groupId) === groupIdB64;
          if (same) issuerKeyB64 = base64Encode(c.issuerSignaturePubkey);
          c.free();
        } catch {
          issuerKeyB64 = null;
        }
      }
      if (!groupIsPublic(groupIdB64) && issuerKeyB64 === null) return "dropped";
      let joined;
      try {
        joined = group.retainJoinRequest(myIdentity, request.keyPackage);
      } catch {
        return "dropped";
      }
      const outcome = joined.outcome;
      const joinerKey = joined.joinerSignatureKey;
      joined.free();
      if (outcome === "queue-full") return "queue-full";
      if (outcome !== "retained" && outcome !== "already-pending") return "dropped";
      // The Welcome goes to the joiner's mailbox under the reply grant (C-69),
      // checked against the account the package is bound to.
      const session = reachSession();
      const reply = request.replyGrant;
      try {
        let accountKey: Uint8Array | undefined;
        try {
          accountKey = group.pendingExternalJoinAccountKey(joinerKey);
        } catch {
          accountKey = undefined;
        }
        if (session !== null && joinReplyBindingVerifies(reply, accountKey)) {
          // Durable before the knock cursor moves past this row: a sealed
          // knock is read once (`reach/join-replies.ts`).
          await rememberJoinReply(group.groupId, joinerKey, session.encodeGrant(reply));
        }
      } finally {
        reply.free();
      }
      const profile = request.profile;
      if (profile !== undefined) {
        const name = profile.displayName ?? (profile.username === undefined ? "" : `@${profile.username}`);
        rememberKnockName(base64Encode(joinerKey), name);
        profile.free();
      }
      await rememberKnockOrigin(
        groupIdB64,
        base64Encode(joinerKey),
        { askedAtMs: rowStampMs(row.createdAt), issuerKeyB64 },
        // Newly kept: an older record for this joiner belongs to a request
        // that stopped waiting (UX section 8, the line's time is this one's).
        outcome === "retained",
      );
      return {
        kind: "externalJoinRequest",
        messageId: row.id,
        joinerSignatureKey: joinerKey,
        retained: true,
        createdAt: row.createdAt,
      };
    } finally {
      request.free();
    }
  }

  /** The serialized half of {@link pollMessages}: everything that touches MLS
   *  state or the journal cursor. Runs under `withGroupMlsLock`; never call it
   *  directly. */
  private async applyPolledBatch(
    myIdentity: WasmIdentity,
    group: WasmGroup,
    after: number,
    raw: FetchResult,
    cursorRef: PollCursorRef | undefined,
    readFrom: number = after,
  ): Promise<PollMessagesResult> {
    // Re-read under the lock: did someone else consume past `after` while we
    // were parked on the network? Then this batch is stale — drop it whole.
    if (cursorRef !== undefined) {
      const current = cursorRef.read();
      if (current !== after) {
        // `cursor` reports the CURRENT position, never `after`, so even a caller
        // that ignores `outcome` and blindly writes it back cannot wind
        // backward.
        return { events: [], cursor: current, hasMore: false, outcome: "cursor-moved" };
      }
      cursorRef.beforeApply?.();
    }

    return this.applyPolledRows(myIdentity, group, after, raw, cursorRef, readFrom);
  }

  /** The row loop and the cursor gate of {@link applyPolledBatch}. */
  private async applyPolledRows(
    myIdentity: WasmIdentity,
    group: WasmGroup,
    after: number,
    raw: FetchResult,
    cursorRef: PollCursorRef | undefined,
    readFrom: number = after,
  ): Promise<PollMessagesResult> {
    const events: IncomingChatEvent[] = [];
    // `readFrom` is below `after` only on a phase-1b rewind; the cursor
    // reported and advanced is never below `after` (see the end).
    let cursor = readFrom;
    // Echo ids matched in this batch, CONSUMED only once the cursor actually
    // advances past them. Deleting on match would be wrong on the held path:
    // the batch gets re-fetched, our own row would then be handed to OpenMLS,
    // and every subsequent poll would log a `CannotDecryptOwnMessage` for it.
    const matchedEchoes: number[] = [];
    const groupIdB64 = base64Encode(group.groupId);
    // Who-can-find-me 6.4 - set when an opened contact request could not be
    // written yet; the cursor is then held (see the commit below).
    let groupRequestUnwritten = false;
    // ADR-0057 root handover - the role table's owner before each applied
    // row, so a commit that moves ownership away from this account is seen
    // at the apply that carried it.
    let ownerSeen = readRoleTableOwner(group);
    // ADR-0091 phase 1 - the DS's newest registered epoch, read with this
    // batch, judges a row that fails to apply.
    const latestEpoch = raw.latestEpoch === undefined ? undefined : BigInt(raw.latestEpoch);
    let holding = false;
    for (const message of raw.messages) {
      const cursorBefore = cursor;
      cursor = message.id;
      // Drop our own echoes — the DS doesn't dedupe and
      // OpenMLS can't decrypt them on the sender side at the
      // same epoch. Matched against THIS group's registry only: a DS id is
      // unique within one journal, so a group-blind match would suppress
      // another chat's row of the same id (see `markOwnPublishedCommit`).
      if (isOwnPublished(groupIdB64, message.id)) {
        matchedEchoes.push(message.id);
        // Live walk 2 — the gap starts no later than this device's newest
        // row in the conversation, its own included.
        conversationBehind.noteOwnRowAccepted(groupIdB64, rowTimeMs(message.createdAt));
        continue;
      }
      // ADR-0092 section 3.6 — an anonymous ballot is NOT an MLS message: a
      // journal row sealed under its poll's secret. Never fed to OpenMLS.
      if (attachReceiveHook().isBallotRow(message.payload)) {
        await attachReceiveHook().ballotRow(group, groupIdB64, message.id, message.createdAt, message.payload);
        continue;
      }
      // Marked BEFORE the call, not after. A throw is not proof that nothing
      // durable changed — `process_message` decrypts and ratchets before the
      // ADR-0022 role gate can refuse a commit or the ADR-0045 receipt gate can
      // drop a proposal. See `chat/durable-cursor.ts`.
      this.cursorGate.markTouched();
      const localEpoch = readGroupEpoch(group);
      let processed;
      try {
        // ADR-0022 — the CHAT receive loop enforces the group-admin role
        // policy: an add / remove / role-change commit whose committer's role
        // does not authorise it is REFUSED (throws here) and skipped, never
        // merged (fail-closed). A group with no role table (legacy) behaves
        // like the open `processIncoming`. This is the chat group, not the
        // SFU call group (the call path reuses the chat exporter, not a
        // separate Group), so the call-path caveat does not apply here.
        processed = group.processIncomingVerifyingRoles(myIdentity, message.payload);
      } catch (err) {
        // Bad frame, wrong epoch, malformed wire, OR a role-unauthorized
        // commit refused by the gate. ADR-0091 phase 1: the registered commit
        // for the next epoch is HELD - the batch stops here, the cursor stays
        // before it, and it is retried on a later poll - so one failure no
        // longer strands the device. Every other row is skipped as before: a
        // row that can never apply must not wedge the poll. The line logged
        // carries no identifier.
        const outcome = registeredCommitHolds.recordFailure(groupIdB64, {
          id: message.id,
          isOwnEcho: false,
          sealed: framedEpoch(message.payload),
          after: framedEpochAfter(message.payload),
          local: localEpoch,
          latest: latestEpoch,
          registeredCommits: raw.registeredCommits,
          error: err,
        });
        if (outcome.kind === "held") {
          cursor = cursorBefore;
          holding = true;
          break;
        }
        // ADR-0091 section 2 — the gap bookkeeping of a skipped row.
        conversationBehind.noteSkippedRow(groupIdB64, {
          sealed: framedEpoch(message.payload),
          after: framedEpochAfter(message.payload),
          local: localEpoch,
          atMs: rowTimeMs(message.createdAt),
        });
        continue;
      }
      registeredCommitHolds.recordApplied(groupIdB64, message.id);
      conversationBehind.noteAppliedRow(groupIdB64, rowTimeMs(message.createdAt));
      // ADR-0091 section 4 — a leaf that writes at the current epoch is not
      // behind: a re-entry for it within the hour is refused.
      if (processed.kind === "Application" && processed.senderLeaf !== undefined) {
        const leafKey = resolveLeafSignatureKey(rosterOf(group), processed.senderLeaf);
        const epochNow = readGroupEpoch(group);
        if (leafKey !== undefined && epochNow !== undefined) noteLeafWrote(groupIdB64, leafKey, epochNow, Date.now());
      }
      // ADR-0091 section 3 — the group's knock key from the owner or an
      // admin, consumed at receipt and never shown.
      if (await consumeKnockKeyRow(group, processed, (leaf) => resolveMemberIdentity(rosterOf(group), leaf))) continue;
      {
        const ownerNow = readRoleTableOwner(group);
        noteOwnerTransition(group.groupId, ownerSeen, ownerNow);
        ownerSeen = ownerNow;
      }
      // 1.19c2: application messages run through the wasm
      // envelope parser (which is async because it has to
      // resolve the cached wasm module promise on first call,
      // then synchronous thereafter). System rows skip it.
      const event = await toEvent(
        message.id,
        message.createdAt,
        processed,
        (leaf) => resolveMemberIdentity(rosterOf(group), leaf),
        (leaf) => resolveLeafSignatureKey(rosterOf(group), leaf),
        group,
      );
      // ADR-0057 — consumed here, at receipt, and never handed on.
      if (event.kind === "registrarSeed") {
        await consumeRegistrarSeed(group, event);
        continue;
      }
      // ADR-0098 — the same: verified and recorded at receipt.
      if (event.kind === "accountDeleted") {
        await consumeAccountDeleted(group, event);
        continue;
      }
      // Section 6.4 - a contact request sealed to one member, the same way.
      if (event.kind === "groupContactRequest") {
        if (!(await consumeGroupContactRequest(group, event))) groupRequestUnwritten = true;
        continue;
      }
      // ADR-0087 section 5 — a knock seed, the same way.
      if (event.kind === "group-setting" && event.setting?.kind === "knockSeed") {
        await consumeKnockSeed(group, event.setting.plaintext, event.senderKey);
        continue;
      }
      // Never shown, never persisted, never previewed.
      if (event.kind === "serviceEnvelope") continue;
      // ADR-0099 section 6 - a blocked person's message, typing, receipt or
      // profile in the one-to-one with them: consumed, never stored, never
      // shown, never notified (the shared block rule decides).
      if (blockDropsEvent(wasmIfLoaded(), group, event)) continue;
      // ADR-0092 — polls and checklists: their ops, the hand-over and the
      // electorate's growth are filed here, at receipt (C-68), for an open
      // chat and a closed one alike.
      if (event.kind === "contentOp") {
        await attachReceiveHook().contentOp(group, groupIdB64, event);
        continue;
      }
      if (event.kind === "openItems") {
        for (const adopted of await attachReceiveHook().openItems(group, groupIdB64, event)) {
          events.push(adopted);
        }
        continue;
      }
      if (event.kind === "application" && event.content !== undefined) {
        await attachReceiveHook().content(group, groupIdB64, event);
      }
      if (processed.kind === "Commit" && event.kind === "system") {
        // docs/ux/second-device.md section 4 — the people right after THIS
        // commit, read before anything else can move the roster, so each
        // commit of a batch names the people it changed on its own row (a
        // removal and a re-add in one batch must not cancel out).
        const personKeysAfter = personKeysOrNull(group);
        await attachReceiveHook().commit(group, groupIdB64);
        events.push({ ...event, personKeysAfter });
        continue;
      }
      if (processed.kind === "Commit") {
        await attachReceiveHook().commit(group, groupIdB64);
      }
      events.push(event);
    }

    // Still inside the lock: the ratchet and the journal position move
    // together, and in THIS order. The persist is fail-closed (it re-throws
    // inside the gate, which reports `held`) rather than the best-effort write
    // it used to be — a swallowed failure here left the durable MLS state
    // behind a cursor that had already moved on, which since ADR-0045 strands
    // the device permanently. See `chat/durable-cursor.ts`.
    // ADR-0091 phase 1 - a hold the group is already past, or a behind mark
    // it moved above, is over.
    registeredCommitHolds.observeEpoch(groupIdB64, readGroupEpoch(group));
    // docs/ux/one-tab.md backstop (option 4) — the group in memory is past
    // the last snapshot that reached disk (an own commit merged whose
    // persist did not land): this batch persists before the cursor moves,
    // even when every row in it was our own echo.
    {
      const inMemory = readGroupEpoch(group);
      const onDisk = mlsSnapshotWriter.durableEpoch(groupIdB64);
      if (inMemory !== undefined && onDisk !== undefined && onDisk < inMemory) {
        this.cursorGate.markTouched();
      }
    }
    let persistedThisBatch = false;
    const verdict = await this.cursorGate.commit(
      async () => {
        // Who-can-find-me 6.4: an opened contact request that could not be
        // written yet holds the cursor (and the snapshot), so a reload reads
        // its row again instead of losing it.
        if (groupRequestUnwritten) throw new Error("a contact request from this group is not written yet");
        await this.persistDurableOrThrow();
        persistedThisBatch = true;
      },
      () => {
        cursorRef?.advance(Math.max(cursor, after));
        // ADR-0091 phase 1b - the epoch the group is at ON DISK, with the
        // cursor it was written for: the epoch of the last write that
        // landed, never the in-memory epoch (which leads it after a failed
        // persist and made a rewind point past the snapshot). A hook that is
        // not the store's writer (tests) leaves no durable epoch: the
        // in-memory one stands for it only when this batch persisted.
        const durableEpoch =
          mlsSnapshotWriter.durableEpoch(groupIdB64) ??
          (persistedThisBatch ? readGroupEpoch(group) : undefined);
        if (durableEpoch !== undefined) recordCursorEpoch(groupIdB64, durableEpoch);
        // Consume the echo registry entries only now: on the held path they
        // stay armed so the re-fetched batch still suppresses our own rows.
        for (const id of matchedEchoes) consumeOwnPublished(groupIdB64, id);
      },
    );
    // ADR-0057 obligation 6 — no heal on apply. An epoch this batch reached
    // is registered by its committer before the commit was published; a
    // device that is neither root nor a registrar never registers one, and a
    // write refused for a missing key waits for that registration and retries
    // on the next poll.
    if (verdict === "held") {
      // Report `after`, never the un-committed `cursor`: a caller that ignores
      // `outcome` and writes `result.cursor` back must still not lead.
      return { events, cursor: after, hasMore: false, outcome: "persist-held" };
    }
    return {
      events,
      cursor: Math.max(cursor, after),
      hasMore: holding ? false : raw.hasMore,
      outcome: "applied",
    };
  }

  /**
   * ADR-0091 phase 1b - the rows from `from` on, read page by page until a
   * page reaches past `after` or the journal ends (at most
   * {@link REWIND_MAX_PAGES} pages), as one result. The last page's
   * `hasMore`, `latestEpoch` and `registeredCommits` stand for the whole.
   */
  private async fetchRewound(
    groupId: Uint8Array,
    from: number,
    after: number,
    signal: AbortSignal | undefined,
  ): Promise<FetchResult> {
    const opts = signal !== undefined ? { signal } : {};
    let page = await this.ds.fetchGroup(groupId, { after: from, waitSecs: 0, ...opts });
    const messages = [...page.messages];
    for (let pages = 1; pages < REWIND_MAX_PAGES && page.hasMore; pages += 1) {
      const last = messages.at(-1)?.id ?? from;
      if (last >= after) break;
      page = await this.ds.fetchGroup(groupId, { after: last, waitSecs: 0, ...opts });
      messages.push(...page.messages);
    }
    return { ...page, messages };
  }
}

/** Pages a phase-1b rewind reads at most before it applies. Chosen, not
 *  measured: a rewind normally re-reads the rows of one missed commit. */
const REWIND_MAX_PAGES = 20;

/** Whether a staged sweep awaits its verdict (an own commit owed). */
function readHasStagedSweep(group: WasmGroup): boolean {
  try {
    return group.hasStagedSweep();
  } catch {
    return false;
  }
}

/** The group's epoch, `undefined` when it cannot be read. */
function readGroupEpoch(group: WasmGroup): bigint | undefined {
  try {
    return group.epoch;
  } catch {
    return undefined;
  }
}

/** A row's framed epoch without decrypting; `undefined` when not MLS. */
function framedEpoch(payload: Uint8Array): bigint | undefined {
  try {
    return wasmEpochOfMessage(payload);
  } catch {
    return undefined;
  }
}

/** The epoch after a row (one above the framed epoch for a commit). */
function framedEpochAfter(payload: Uint8Array): bigint | undefined {
  try {
    return wasmEpochAfterMessage(payload);
  } catch {
    return undefined;
  }
}

/** Sleep `ms`, returning early when `signal` aborts. */
function sleepUnlessAborted(ms: number, signal: AbortSignal | undefined): Promise<void> {
  if (ms <= 0 || signal?.aborted === true) return Promise.resolve();
  return new Promise((resolve) => {
    const timer = setTimeout(done, ms);
    function done(): void {
      clearTimeout(timer);
      signal?.removeEventListener("abort", done);
      resolve();
    }
    signal?.addEventListener("abort", done, { once: true });
  });
}

/**
 * `docs/ux/group-and-channel-privacy.md` section 8.1 - hand one applied poll
 * of a group to whoever announces join requests (`join-request-reads.ts`):
 * whether this device is owner or admin (the knock-read gate) and still a
 * member, the pending set, and, when the knock queue was read in this poll,
 * the requests in it. Only requests the core kept count; a request the full
 * queue turned away is not one an admin can answer. Local reads only, no DS
 * call. Never throws.
 */
function announceJoinRequestRead(
  group: WasmGroup,
  identity: WasmIdentity,
  owner: string,
  knockEvents: readonly IncomingChatEvent[] | null,
): void {
  let pending: string[];
  let active: boolean;
  try {
    pending = splitPendingJoinKeys(group.pendingExternalJoins(), base64Encode);
    active = group.isActive;
  } catch {
    return;
  }
  const arrived: string[] = [];
  for (const e of knockEvents ?? []) {
    if (e.kind !== "externalJoinRequest" || !e.retained) continue;
    const k = base64Encode(e.joinerSignatureKey);
    if (!arrived.includes(k)) arrived.push(k);
  }
  publishJoinRequestRead({
    accountIdB64: owner,
    groupIdB64: base64Encode(group.groupId),
    arrivedKeysB64: arrived,
    pendingKeysB64: pending,
    manages: readsKnocks(group, identity),
    active,
    knocksRead: knockEvents !== null,
  });
}

/**
 * ADR-0053 contract point 7 — does this device read the group's knock queue?
 * An owner or admin: the set ADR-0027 lets approve an external join. A plain
 * member never acts on a join request, so reading strangers' rows would only
 * cost it bandwidth. A torn read, or a group with no role table, is `false`.
 *
 * Gated on the ROLE TABLE, never on `ownCommitterRank`: since ADR-0050
 * section 2.5a a plain member holds a committer rank while its pending store
 * holds a device admission it could sweep, so "has a rank" no longer means
 * "may approve a join".
 */
export function readsKnocks(group: WasmGroup, identity: WasmIdentity): boolean {
  try {
    const role = group.ownRole(identity);
    return role === Role.Owner || role === Role.Admin;
  } catch {
    return false;
  }
}

/**
 * The group's roster as the identity resolver reads it.
 *
 * A torn / unreadable roster yields `[]`, which resolves every leaf to
 * `undefined` — the fail-closed direction: an unattributable message can never
 * be edited or deleted by anyone, which is strictly what an unknown author
 * already meant.
 */
export function rosterOf(group: WasmGroup): RosterMember[] {
  let roster: RosterMember[];
  try {
    roster = group.members() as unknown as RosterMember[];
  } catch {
    return [];
  }
  // ADR-0091 section 7 (security review 2026-11-21): the first time this
  // device sees each leaf in the roster clamps where a gap's history it may
  // send for that leaf starts. Only the first observation is kept.
  try {
    noteLeavesSeen(
      base64Encode(group.groupId),
      roster.map((m) => base64Encode(m.signaturePublicKey)),
      Date.now(),
    );
  } catch {
    /* a missed sighting only narrows a later gap history */
  }
  return roster;
}

/**
 * ADR-0057 obligation 4 — hand one registrar-seed envelope to the gate, with
 * the owner read off the LIVE role table now, at receipt (C-68). Never
 * throws: every refusal is a silent drop. INTERNAL, shared with the ADR-0045
 * sweep's verdict apply, which consumes journal rows the poll never sees.
 */
export async function consumeRegistrarSeed(
  group: WasmGroup,
  event: { readonly plaintext: Uint8Array; readonly senderKey: MemberIdentity | undefined },
): Promise<void> {
  let ownerAccountKey: Uint8Array | undefined;
  let groupId: Uint8Array;
  try {
    ownerAccountKey = group.roleTableOwner();
    groupId = group.groupId;
  } catch {
    return;
  }
  let senderAccountKey: Uint8Array | undefined;
  try {
    senderAccountKey = event.senderKey === undefined ? undefined : base64Decode(event.senderKey);
  } catch {
    senderAccountKey = undefined;
  }
  await deliverRegistrarSeed({
    groupId,
    groupIdB64: base64Encode(groupId),
    plaintext: event.plaintext,
    senderAccountKey,
    ownerAccountKey,
  });
}

/**
 * ADR-0098 — hand one account-deleted statement on, with this conversation's
 * group id and the sender's account key resolved at receipt (C-68). Never
 * throws; the sink verifies it and drops anything that does not verify.
 */
export async function consumeAccountDeleted(
  group: WasmGroup,
  event: { readonly plaintext: Uint8Array; readonly senderKey: MemberIdentity | undefined },
): Promise<void> {
  let groupId: Uint8Array;
  try {
    groupId = group.groupId;
  } catch {
    return;
  }
  let senderAccountKey: Uint8Array | undefined;
  try {
    senderAccountKey = event.senderKey === undefined ? undefined : base64Decode(event.senderKey);
  } catch {
    senderAccountKey = undefined;
  }
  await deliverAccountDeleted({
    groupId,
    groupIdB64: base64Encode(groupId),
    plaintext: event.plaintext,
    senderAccountKey,
  });
}

/**
 * `docs/ux/who-can-find-me.md` section 6.4 - hand one contact request sealed
 * inside this group to the request list's handler, with the sender's account
 * key as resolved at receipt (C-68). Never throws. `false` when the request
 * is not written yet: the caller holds its journal cursor.
 */
export async function consumeGroupContactRequest(
  group: WasmGroup,
  event: {
    readonly messageId: number;
    readonly createdAt: string;
    readonly plaintext: Uint8Array;
    readonly senderKey: MemberIdentity | undefined;
  },
): Promise<boolean> {
  let groupId: Uint8Array;
  try {
    groupId = group.groupId;
  } catch {
    return true;
  }
  let senderAccountKey: Uint8Array | undefined;
  try {
    senderAccountKey = event.senderKey === undefined ? undefined : base64Decode(event.senderKey);
  } catch {
    senderAccountKey = undefined;
  }
  return deliverGroupContactRequest({
    groupId,
    groupIdB64: base64Encode(groupId),
    journalMessageId: event.messageId,
    createdAt: event.createdAt,
    plaintext: event.plaintext,
    senderAccountKey,
  });
}

/**
 * ADR-0087 section 5 — hand one knock-seed envelope to its handler with the
 * owner read off the LIVE role table now, at receipt (C-68). Never throws.
 */
export async function consumeKnockSeed(
  group: WasmGroup,
  plaintext: Uint8Array,
  senderKey: MemberIdentity | undefined,
): Promise<void> {
  let ownerAccountKey: Uint8Array | undefined;
  let groupId: Uint8Array;
  try {
    ownerAccountKey = group.roleTableOwner();
    groupId = group.groupId;
  } catch {
    return;
  }
  let senderAccountKey: Uint8Array | undefined;
  try {
    senderAccountKey = senderKey === undefined ? undefined : base64Decode(senderKey);
  } catch {
    senderAccountKey = undefined;
  }
  await deliverKnockSeed({ groupId, groupIdB64: base64Encode(groupId), plaintext, senderAccountKey, ownerAccountKey });
}

/**
 * Turn one processed MLS message into the timeline event it represents.
 *
 * INTERNAL API, exported for exactly one other caller: the ADR-0045 committer
 * sweep's verdict apply (`chat/sweep.ts`, adapted in `contacts/client.ts`).
 * That path consumes journal rows the poll will never fetch again — it advances
 * the same cursor over them — so it must produce timeline events for them, and
 * it must produce them through THIS function rather than a second dispatch of
 * its own. The ladder below routes call signaling, the ADR-0042/0044
 * push-routing secret, the ADR-0045 committer beacon, the ADR-0048 profile
 * request and profile, the ADR-0051 group name, external-join proposals,
 * membership proposals and the ADR-0023/0024 chat / edit / delete / reaction /
 * typing / read-receipt bodies; two copies of that would be two things to keep
 * in step, and the one that fell behind would silently render a service
 * envelope as a chat bubble.
 *
 * Not part of the public `ChatClient` surface — nothing outside those two
 * receive paths should be constructing timeline events.
 *
 * `resolveSender` maps an MLS leaf index to the member's signature key against
 * the group's roster AS IT STANDS NOW. Both callers invoke this function
 * immediately after `processIncomingVerifyingRoles` returned, which is the one
 * moment "now" is the epoch the message was sealed at — an Application frame
 * only decrypts at the current epoch, and a Commit that renumbers leaves is a
 * separate journal row applied before or after, never during. Resolving later,
 * from a stored leaf, is what lets a member added into a departed member's
 * vacated leaf inherit that member's authorship. See `author-identity.ts`.
 */
export async function toEvent(
  messageId: number,
  createdAt: string,
  processed: ReturnType<WasmGroup["processIncoming"]>,
  resolveSender: (leafIndex: number) => MemberIdentity | undefined,
  resolveSenderDevice?: (leafIndex: number) => string | undefined,
  /** ADR-0099 section 1 - the group the frame decrypted in, read for the
   *  report evidence at receipt (its id and the sender leaf's device
   *  binding). Absent: no evidence is kept. */
  evidenceGroup?: EvidenceGroup,
): Promise<IncomingChatEvent> {
  // The WASM `ProcessedMessage.kind` is a PascalCase string
  // discriminator: "Application" | "Commit" | "Proposal" |
  // "ExternalJoinProposal" (see lakko-ffi-wasm/src/mls.rs).
  if (processed.kind === "Application" && processed.applicationData !== undefined) {
    // ADR-0099 section 1 - the raw plaintext may be an attested chat
    // envelope; every classifier below reads the envelope inside it, exactly
    // as before the wrapper existed. A malformed wrapper stays whole and
    // classifies as a service envelope, which is dropped.
    const raw = processed.applicationData;
    const core = wasmIfLoaded();
    const bytes = unwrapReceived(core, raw);
    // ADR-0023 — the original author's MLS leaf for this
    // Application message. `undefined` for a control frame /
    // external sender (which can never be a valid edit/delete
    // target nor a reaction attribution).
    const senderLeaf = processed.senderLeaf;
    // ...and the identity that leaf names, resolved HERE because
    // here is where the roster is the one the message was sealed
    // under. Every downstream attribution reads this, not the leaf.
    const senderKey =
      senderLeaf === undefined ? undefined : resolveSender(senderLeaf);
    // Phase 3a8 — sniff for call-signaling bytes before
    // running the chat-envelope parser. Call signaling is a
    // prost-encoded `CallSignalingPayload` proto; chat
    // envelopes are JSON (the first byte is `{` / 0x7B). The
    // discriminator is cheap + unambiguous because the
    // proto's first wire tag is field 10 (offer) ... field
    // 16 (group_leave) — see `looksLikeCallSignaling` for the
    // table — none of which is 0x7B.
    if (looksLikeCallSignaling(bytes)) {
      return {
        kind: "callSignaling",
        messageId,
        payloadBytes: bytes,
        // The MLS-authenticated sender, so a group-control body that
        // names a leaf other than its own sender can be refused.
        senderLeaf,
        // Resolved HERE, at the epoch the body was sealed at: the group-call
        // roster keys members on these, never on the leaf (C-68).
        senderMemberKey:
          senderLeaf === undefined || resolveSenderDevice === undefined
            ? undefined
            : resolveSenderDevice(senderLeaf),
        senderKey,
        createdAt,
      };
    }
    // ADR-0057 — a registrar-seed envelope, before the chat parser for the
    // same reason as every service envelope below. Classified by the core
    // (`dispatchIncomingEnvelope`, kind `RegistrarSeed`), gated by its one
    // fixed length so a chat message never pays for the call.
    if (isRegistrarSeedEnvelope(await loadWasm(), bytes)) {
      return { kind: "registrarSeed", messageId, plaintext: bytes, senderKey, createdAt };
    }
    // ADR-0098 — an account-deleted statement, before the chat parser for
    // the same reason. Fixed length, so a chat message never pays the call.
    if (isAccountDeletedBytes(await loadWasm(), bytes)) {
      return { kind: "accountDeleted", messageId, plaintext: bytes, senderKey, createdAt };
    }
    // Section 6.4 - a contact request sealed to one member, the same way.
    if (isGroupContactRequestEnvelope(await loadWasm(), bytes)) {
      return { kind: "groupContactRequest", messageId, plaintext: bytes, senderKey, createdAt };
    }
    // ADR-0042 — sniff for a push-routing-secret distribution envelope
    // before the chat parser. The wasm codec is fail-closed (domain +
    // version + exact-length gate), so a non-envelope plaintext returns
    // null and falls through to the normal chat parse. ADR-0044 made that
    // gate a hard flag day: a retired v1 envelope (62 bytes, no mint epoch)
    // is refused here too and simply never surfaces as a routing secret.
    const routingSecret = await tryDecodeSecretEnvelope(bytes);
    if (routingSecret !== null) {
      return {
        kind: "pushRoutingSecret",
        messageId,
        secret: routingSecret.secret,
        // ADR-0044 — the minter's recency stamp, carried so the receive gate
        // can order this envelope against what the device already holds.
        mintEpoch: routingSecret.mintEpoch,
        // ADR-0042 — carried so the receive-side owner gate can resolve
        // the author's signature key against the role-table owner.
        senderLeaf,
        createdAt,
      };
    }
    // ADR-0045 — sniff for a committer-liveness beacon before the chat
    // parser. Without this arm the beacon reaches `parseBodyEnvelope`,
    // whose lossy-UTF-8 fallback degrades it (correctly, per its own
    // contract) into a chat bubble reading `lakko/committer-beacon/v1`
    // plus two control bytes. The wasm codec behind the peek is
    // fail-closed on domain + version + exact length.
    if (await looksLikeCommitterBeacon(bytes)) {
      return {
        kind: "committerBeacon",
        messageId,
        senderLeaf,
        createdAt,
      };
    }
    // ADR-0048 §2 trigger 4 — a profile request, before the chat parser for
    // the same reason as the beacon: it is field-less and the lossy-UTF-8
    // fallback would render it as a bubble reading
    // `lakko/profile-request/v1` plus a control byte. The sniff is backed
    // by the fail-closed wasm codec (domain + exact length + version).
    if (sniffProfileRequest(await loadWasm(), bytes)) {
      return {
        kind: "profileRequest",
        messageId,
        senderLeaf,
        senderKey,
        createdAt,
      };
    }
    // ADR-0048 — a profile envelope, before the chat parser for the same
    // reason as the two arms above: the lossy-UTF-8 fallback would render it
    // as a bubble carrying the sender's name. A domain match that fails its
    // bounds is dropped here too (`profile: undefined`), never parsed as chat.
    {
      const sniff = sniffProfile(await loadWasm(), bytes);
      if (sniff.kind !== "none") {
        return {
          kind: "profile",
          messageId,
          profile: sniff.kind === "profile" ? sniff.profile : undefined,
          senderLeaf,
          senderKey,
          createdAt,
        };
      }
    }
    // ADR-0051 — the shared group name, before the chat parser for the same
    // reason: the lossy-UTF-8 fallback would render it as a bubble carrying
    // the domain string and the name. A domain match that fails its bounds
    // is dropped here too (`groupName: undefined`), never parsed as chat.
    {
      const sniff = sniffGroupName(await loadWasm(), bytes);
      if (sniff.kind !== "none") {
        return {
          kind: "group-name",
          messageId,
          groupName: sniff.kind === "name" ? sniff.value : undefined,
          senderLeaf,
          senderKey,
          createdAt,
        };
      }
    }
    // ADR-0073 R.6 — the group-closed envelope (a successor claim), the group name's twin:
    // same reason, same gate shape. A domain match that fails its bounds is
    // dropped here too (`groupClosed: undefined`), never parsed as chat.
    {
      const sniff = sniffGroupClosed(await loadWasm(), bytes);
      if (sniff.kind !== "none") {
        return {
          kind: "group-closed",
          messageId,
          groupClosed: sniff.kind === "closed" ? sniff.value : undefined,
          senderLeaf,
          senderKey,
          createdAt,
        };
      }
    }
    // ADR-0052 decision 3 — a shared pin, before the chat parser for the
    // same reason: the lossy-UTF-8 fallback would render it as a bubble
    // reading the domain string plus binary. A domain match that fails its
    // bounds is dropped here too (`pin: undefined`), never parsed as chat.
    {
      const sniff = sniffPin(await loadWasm(), bytes);
      if (sniff.kind !== "none") {
        return {
          kind: "pin",
          messageId,
          pin: sniff.kind === "pin" ? sniff.value : undefined,
          senderLeaf,
          senderKey,
          createdAt,
        };
      }
    }
    // ADR-0059 section 6 — a shared-channels list, before the chat parser
    // for the same reason as the pin: the lossy-UTF-8 fallback would render
    // it as a bubble carrying the domain string and channel names. A domain
    // match that fails its bounds is dropped (`list: undefined`).
    {
      const sniff = sniffSharedChannels(await loadWasm(), bytes);
      if (sniff.kind !== "none") {
        return {
          kind: "shared-channels",
          messageId,
          list: sniff.kind === "list" ? sniff.value : undefined,
          senderLeaf,
          senderKey,
          createdAt,
        };
      }
    }
    // ADR-0087 — the group photo, allowed reactions, conversation handle and
    // knock seed, before the chat parser for the same reason as the arms
    // above. A domain match that fails its bounds is dropped.
    {
      const sniff = sniffGroupSetting(await loadWasm(), bytes);
      if (sniff.kind !== "none") {
        return {
          kind: "group-setting",
          messageId,
          setting: sniff.kind === "setting" ? sniff.setting : undefined,
          senderLeaf,
          senderKey,
          createdAt,
        };
      }
    }
    // ADR-0060 — the scoped-discovery service envelopes, before the chat
    // parser for the same reason as the arms above: rendered through the
    // lossy-UTF-8 fallback they would put domain bytes (and, for a
    // delegation or an introduction, secrets) in a bubble.
    {
      const kind = sniffScopedEnvelope(await loadWasm(), bytes);
      if (kind !== null) {
        return {
          kind: "scopedDiscovery",
          messageId,
          envelope: kind,
          payload: bytes,
          senderLeaf,
          senderKey,
          createdAt,
        };
      }
    }
    // ADR-0023 — feed the application bytes through the
    // structured body parser, which surfaces the chat / edit /
    // delete / reaction discriminator. A plain chat message
    // (or any legacy / malformed envelope, which the parser
    // degrades to `chat` with the raw text) returns an
    // `application` event; an op returns the `op` event the UI
    // folds into the target message.
    // ADR-0092 section 3.9 — the open-items hand-over and the old-ballot rows
    // are service envelopes the receive loop consumes.
    {
      let items = false;
      let ballots = false;
      try {
        const w = await loadWasm();
        items = w.isOpenItemsEnvelope(bytes);
        ballots = !items && w.isPollBallotsEnvelope(bytes);
      } catch {
        // A core without the recognisers: the envelope reads as a service
        // envelope below and is dropped.
      }
      if (items || ballots) {
        return { kind: "openItems", messageId, envelope: bytes, ballots, senderKey, createdAt };
      }
    }
    const body = await parseBodyEnvelope(bytes);
    // A service envelope the sniffers above did not recognise (a v3 profile
    // from a newer build reached every client as a chat bubble reading
    // `lakko/profile/v1` plus binary before the core classified it).
    if (body.kind === "service-envelope") {
      return { kind: "serviceEnvelope", messageId, createdAt };
    }
    // ADR-0061 section 6 — a chat message from a newer Lakko. It keeps its
    // place in the timeline as a row of its own; the words are the catalog's.
    if (body.kind === "unsupported") {
      return {
        kind: "application",
        messageId,
        text: "",
        attachments: [],
        plaintext: "",
        senderLeaf,
        senderKey,
        createdAt,
        unsupportedVersion: body.version,
      };
    }
    if (body.kind === "content-op") {
      return { kind: "contentOp", messageId, op: body.op, senderLeaf, senderKey, createdAt };
    }
    if (body.kind === "content") {
      return {
        kind: "application",
        messageId,
        text: "",
        attachments: body.attachments,
        plaintext: "",
        senderLeaf,
        senderKey,
        createdAt,
        content: body.content,
        ...(body.reply === undefined ? {} : { reply: body.reply }),
        ...(body.forward === undefined ? {} : { forward: body.forward }),
      };
    }
    if (body.kind === "chat") {
      const evidence = receiptEvidence(core, raw, senderLeaf, resolveSenderDevice, evidenceGroup);
      return {
        kind: "application",
        messageId,
        ...(evidence === undefined ? {} : { evidence }),
        text: body.text,
        attachments: body.attachments,
        // Alias kept for transitional call-site compatibility;
        // identical to `text`.
        plaintext: body.text,
        senderLeaf,
        senderKey,
        createdAt,
        // ADR-0052 — the reply reference rides through unchanged; the
        // snapshot is the replier's and is never re-read from the target.
        ...(body.reply === undefined ? {} : { reply: body.reply }),
        // ADR-0052 decision 2 — the forward attribution rides through
        // unchanged; the name is the forwarder's claim and is never resolved.
        ...(body.forward === undefined ? {} : { forward: body.forward }),
        // ADR-0073 R.7 case 1 — the re-send marker, for the receive rule.
        ...(body.resendOrigin === undefined ? {} : { resendOrigin: body.resendOrigin }),
        // ADR-0087 section 14 — the invitation card, kept with the message.
        ...(body.channelInvite === undefined ? {} : { channelInvite: body.channelInvite }),
      };
    }
    return {
      kind: "op",
      messageId,
      op: body.kind,
      // ADR-0024 — `typing` carries no target; `read-receipt`'s
      // `target` is the read-up-to messageId.
      target: body.kind === "typing" ? 0 : body.target,
      text: body.kind === "edit" ? body.text : "",
      emoji: body.kind === "reaction" ? body.emoji : undefined,
      ...(body.kind === "reaction" ? { reactionAction: body.action } : {}),
      ...(body.kind === "reaction" && body.targetAuthor !== undefined
        ? { reactionTargetAuthor: body.targetAuthor }
        : {}),
      isTyping: body.kind === "typing" ? body.isTyping : undefined,
      senderLeaf,
      senderKey,
      createdAt,
    };
  }
  // ADR-0027 (G-3) — an external-join proposal carries the joiner's
  // signature key the members UI needs to approve / reject it. Surface
  // it as a dedicated event rather than a content-less system row.
  if (processed.kind === "ExternalJoinProposal" && processed.externalJoinSignatureKey) {
    return {
      kind: "externalJoinRequest",
      messageId,
      joinerSignatureKey: processed.externalJoinSignatureKey,
      retained: processed.externalJoinRetained,
      createdAt,
    };
  }
  // ADR-0045 — a membership proposal surfaces its full record, not a
  // content-less system row: the panel needs WHAT was proposed, by WHOM, and
  // whether the receipt gate stored it, and the DS row's `createdAt` is the
  // only place the quiet window's clock can come from.
  const proposal = processed.kind === "Proposal" ? processed.proposal : undefined;
  if (proposal !== undefined) {
    return {
      kind: "membershipProposal",
      messageId,
      proposalKind: proposal.kind,
      stored: proposal.stored,
      addSignatureKey: proposal.addSignatureKey,
      removeTargetLeaf: proposal.removeTargetLeaf,
      senderLeaf: proposal.senderLeaf,
      createdAt,
    };
  }
  const sk: "commit" | "external-join-proposal" =
    processed.kind === "Commit" ? "commit" : "external-join-proposal";
  return { kind: "system", messageId, systemKind: sk, createdAt };
}

/** ADR-0099 section 6 - whether `event` from a blocked sender is dropped at
 *  receipt in `group` (the one-to-one with them). Never throws. */
export function blockDropsEvent(
  wasm: Awaited<ReturnType<typeof loadWasm>> | null,
  group: { groupKind(): number | undefined },
  event: IncomingChatEvent,
): boolean {
  const senderKey = "senderKey" in event ? event.senderKey : undefined;
  if (senderKey === undefined) return false;
  const conversation = conversationKindOf(group) === "chat" ? "one-to-one" : "group";
  return dropsAtReceipt(wasm, conversation, {
    kind: event.kind,
    ...(event.kind === "op" ? { op: event.op } : {}),
    senderKey,
  });
}

/** ADR-0099 section 1 - what {@link toEvent} reads of the group for the
 *  report evidence. Structural, so a test can hand in a plain object. */
export interface EvidenceGroup {
  readonly groupId: Uint8Array;
  deviceBindingAt(leaf: number): Uint8Array | undefined;
}

/**
 * The report evidence of one received chat message, read NOW (C-68: the
 * leaf the binding is read at may hold somebody else after the next
 * commit). `undefined` when the sender leaf or its key is unknown, or when no
 * group was handed in. Never throws.
 */
function receiptEvidence(
  wasm: Awaited<ReturnType<typeof loadWasm>> | null,
  raw: Uint8Array,
  senderLeaf: number | undefined,
  resolveSenderDevice: ((leafIndex: number) => string | undefined) | undefined,
  group: EvidenceGroup | undefined,
): MessageEvidence | undefined {
  if (group === undefined || senderLeaf === undefined || resolveSenderDevice === undefined) return undefined;
  let leafKey: Uint8Array | undefined;
  let binding: Uint8Array | undefined;
  let groupId: Uint8Array;
  try {
    const keyB64 = resolveSenderDevice(senderLeaf);
    leafKey = keyB64 === undefined ? undefined : base64Decode(keyB64);
    groupId = group.groupId;
    binding = group.deviceBindingAt(senderLeaf) ?? undefined;
  } catch {
    return undefined;
  }
  return evidenceAtReceipt(wasm, raw, groupId, leafKey, binding);
}

/**
 * Cheap discriminator: are these bytes a call-signaling
 * proto? Returns `true` when the leading bytes are one of
 * the `CallSignalingPayload.body` oneof field tags. Cheap
 * enough to run on every `Application` row in the poll batch.
 *
 * Wire layout of the leading tag of an embedded
 * `CallSignalingPayload` (wire type 2, `(field << 3) | 2`):
 *
 *   field 10 (offer)        → 0x52
 *   field 11 (answer)       → 0x5A
 *   field 12 (candidate)    → 0x62
 *   field 13 (end)          → 0x6A
 *   field 14 (group_start)  → 0x72
 *   field 15 (group_join)   → 0x7A
 *   field 16 (group_leave)  → 0x82 0x01  (the tag is 130, a two-byte varint)
 *
 * The three group-control arms were missing, so a group-call
 * start / join / leave fell through to `parseBodyEnvelope`,
 * whose lossy-UTF-8 fallback rendered it as a garbage chat
 * bubble. The background mirror in `public/sw-push-decrypt.js`
 * carries the same table and must stay in step.
 *
 * Chat envelopes are JSON and start with `{` (0x7B); the
 * `lakko/…` service envelopes start with `l` (0x6C). Neither
 * overlaps. A false positive on some future non-JSON shape is
 * harmless — the call dispatcher's `parseCallSignaling`
 * rejects it and the row is dropped.
 *
 * The tag table itself moved to `calls/call-signaling-wake.ts`
 * when the SEND path started needing the same reading (which of
 * these frames is worth a push wake). Two readers of one table
 * is one definition; the third, `public/sw-push-decrypt.js`,
 * cannot import from `src/` and keeps its documented copy.
 */
const looksLikeCallSignaling = looksLikeCallSignalingBytes;

/**
 * First byte of the ADR-0045 committer-beacon domain separator (`'l'` of
 * `"lakko/committer-beacon/v1\0"`). The cheap half of the pre-filter; the
 * wasm `decodeCommitterBeacon` behind it is the authoritative fail-closed
 * check. The same `'l'` every `lakko/…` domain starts with, so on its own it
 * is not a decision — it only skips the wasm call for a chat envelope (which
 * starts `{`).
 */
const COMMITTER_BEACON_FIRST_BYTE = 0x6c;

/**
 * ADR-0045 — is this application plaintext a committer-liveness beacon?
 *
 * Unlike every other envelope filter in this file, the length-plus-prefix peek
 * here is a COMPLETE test rather than merely a fast path: the beacon carries no
 * fields, so every beacon on the wire is byte-identical, and `length ===
 * committerBeaconEnvelopeLen()` plus the domain prefix already pins all but the
 * version byte. The wasm decode behind it is confirmation, and it is what makes
 * the arm fail-closed on an unknown schema version — a future v2 beacon this
 * build does not understand is NOT silently swallowed here; it falls through to
 * the chat parser exactly as any other unrecognised plaintext does.
 *
 * The expected length comes from `committerBeaconEnvelopeLen()` on the FFI, not
 * from a local `27`. The constant exists precisely so the hosts do not each
 * carry their own copy of a number that lives in
 * `crates/lakko-contacts/src/committer_beacon.rs`; the foreground links the
 * full FFI, so there is no reason to compromise.
 */
async function looksLikeCommitterBeacon(bytes: Uint8Array): Promise<boolean> {
  if (bytes.length === 0 || bytes[0] !== COMMITTER_BEACON_FIRST_BYTE) {
    return false;
  }
  const wasm = await loadWasm();
  if (bytes.length !== wasm.committerBeaconEnvelopeLen()) return false;
  try {
    wasm.decodeCommitterBeacon(bytes);
    return true;
  } catch {
    // Not a beacon (wrong domain / unknown version) — fall through so the
    // caller runs the normal chat parse.
    return false;
  }
}
