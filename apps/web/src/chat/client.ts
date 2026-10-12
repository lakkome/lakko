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
import { DsClient } from "../ds/client";
import type { FetchResult } from "../ds/client";
import { base64Decode, base64Encode } from "../auth/account-id";
import { withGroupMlsLock } from "../mls/group-lock";

import { readKnockCursor, takeKnockReadSlot, writeKnockCursor } from "./knock-cursor";
import { boundContactsAccount } from "../contacts/local-store";
import { PUSH_GID_LEN, type DsWake } from "../ds/wake";
import { loadWasm } from "../wasm";
import {
  Role,
  type Group as WasmGroup,
  type Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import type { IdentitySecrets } from "../auth/AuthContext";
import {
  parseBodyEnvelope,
  type AttachmentRefBytes,
  type ForwardOrigin,
  type ForwardRef,
  type ReplyRef,
} from "./attachments";
import { tryDecodeSecretEnvelope } from "../push/group-routing";
import { looksLikeCallSignaling as looksLikeCallSignalingBytes } from "../calls/call-signaling-wake";
import { DurableCursorGate } from "./durable-cursor";
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
import { deliverRegistrarSeed } from "../ds/registrar-seeds";
import { noteOwnerTransition, readRoleTableOwner } from "../ds/root-handover";
import type { IncomingPin } from "./pin-store";
import {
  resolveLeafSignatureKey,
  resolveMemberIdentity,
  type MemberIdentity,
  type RosterMember,
} from "./author-identity";

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
      /** ADR-0065 R.7 case 1 — set on a message RE-SENT into a successor
       *  one-to-one: the original group id and message id. Never drawn; a
       *  device that holds the original drops this row
       *  (`resend-marker.ts`). */
      resendOrigin?: ForwardOrigin;
      /** ADR-0061 section 6 — set when the sender's newer Lakko wrote this
       *  message in a wire version this build cannot read: the envelope's
       *  version. `text` is empty and there are no attachments; every screen
       *  draws `chat.message.unsupported` in its place. */
      unsupportedVersion?: number;
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
      /** ADR-0065 R.6 — a successor CLAIM: a device founding the group this
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
 * ADR-0065 R.7 case 1 — nothing is sent into a session only removed devices
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
   *  provider snapshot to disk. Best-effort — exceptions are
   *  logged and swallowed; the in-memory state stays good. */
  onPersist?: () => Promise<void>;
  /** ADR-0042 / ADR-0054 — resolve the opaque `push_gid` for a group's DS
   *  `group_id`, so the `x-lakko-wake` header of a publish names the routing
   *  id instead of the leaky DS journal key. Returns `null` when this device
   *  does not (yet) hold the group's `push_routing_secret` — the publish then
   *  carries NO wake header (never the `group_id`, invariant #5). When UNSET
   *  the client wakes nobody at all: there is no fallback to the `group_id`.
   *  Every production client that sends a waking frame (a chat message, a
   *  call offer or end) MUST wire this. */
  pushGidResolver?: (groupId: Uint8Array) => Promise<Uint8Array | null>;
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
   * ADR-0065 R.6 — the ONE frame that is posted into a superseded
   * one-to-one on purpose: the successor claim, which the person's other
   * devices read there. Everything else is refused ({@link HeldForSuccessor}).
   */
  readonly intoSupersededSession?: boolean;
}

export class ChatClient {
  /** The receive path's cursor-may-lag-never-lead gate. One per client, which
   *  is the right granularity because `snapshotStorage()` serialises the whole
   *  provider — see `chat/durable-cursor.ts`. */
  private readonly cursorGate = new DurableCursorGate();
  private readonly identityProvider: (() => WasmIdentity | null) | undefined;
  private readonly onPersist: (() => Promise<void>) | undefined;
  private readonly pushGidResolver:
    | ((groupId: Uint8Array) => Promise<Uint8Array | null>)
    | undefined;

  constructor(
    private readonly ds: DsClient = new DsClient(),
    options: ChatClientOptions = {},
  ) {
    this.identityProvider = options.identityProvider;
    this.onPersist = options.onPersist;
    this.pushGidResolver = options.pushGidResolver;
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
    // ADR-0065 R.7 case 1 — before anything is sealed.
    refuseDeadSession(group);
    const myIdentity = await this.resolveIdentity(identity);

    // Read BEFORE the lock is keyed and before anything is sealed — a
    // synchronous getter that touches no mutable state.
    let groupId: Uint8Array;
    try {
      groupId = group.groupId;
    } catch (err) {
      throw new ChatError("group.groupId unavailable", err);
    }

    return withGroupMlsLock(base64Encode(groupId), async () =>
      this.sealAndPublish(myIdentity, group, groupId, plaintext, "message"),
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
  ): Promise<SendMessageResult> {
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
      wireBytes = group.createApplicationMessage(myIdentity, plaintext);
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
      throw new ChatError("DS publish failed", err);
    }
    // Keyed by GROUP, and shared with every other `ChatClient` on this page, so
    // a service envelope published outside the chat route (a profile right
    // after a Welcome) is still recognised as an echo there.
    markOwnPublished(base64Encode(groupId), publish.id);

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
    if (this.pushGidResolver === undefined) return null;
    let pushGid: Uint8Array | null;
    try {
      pushGid = await this.pushGidResolver(groupId);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("push_gid resolution failed; publishing without a wake", err);
      return null;
    }
    if (pushGid === null) return null;
    if (pushGid.length !== PUSH_GID_LEN) {
      // A header the DS would answer with 400 would fail the publish itself,
      // after the ratchet has already advanced; drop the wake instead.
      // eslint-disable-next-line no-console
      console.warn(`push_gid has ${pushGid.length} bytes; publishing without a wake`);
      return null;
    }
    return { pushGid, kind: wake };
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
    const myIdentity = await this.resolveIdentity(identity);

    let groupId: Uint8Array;
    try {
      groupId = group.groupId;
    } catch (err) {
      throw new ChatError("group.groupId unavailable", err);
    }

    const result = await withGroupMlsLock(base64Encode(groupId), async () =>
      this.sealAndPublish(myIdentity, group, groupId, controlBytes, options.wake ?? null),
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
      throw new ChatError("DS fetch failed", err);
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
        console.warn("ChatClient: knock read failed; retried next poll", err);
      }
    }

    // The APPLY + the cursor advance — inside the lock, atomic together.
    return withGroupMlsLock(groupIdB64, async () => {
      const result = await this.applyPolledBatch(myIdentity, group, after, raw, cursorRef);
      if (knocks === null || result.outcome !== "applied") return result;
      const knockEvents = await this.applyKnocks(
        myIdentity,
        group,
        knocks.after,
        knocks.rows,
        knocks.owner,
      );
      return knockEvents.length === 0
        ? result
        : { ...result, events: [...result.events, ...knockEvents] };
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
    for (const row of raw.messages) {
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
        console.warn("ChatClient: knock row dropped", row.id, err);
        continue;
      }
      if (processed.kind !== "ExternalJoinProposal") continue;
      const event = await toEvent(
        row.id,
        row.createdAt,
        processed,
        (leaf) => resolveMemberIdentity(rosterOf(group), leaf),
        (leaf) => resolveLeafSignatureKey(rosterOf(group), leaf),
      );
      if (event.kind === "externalJoinRequest") events.push(event);
    }
    if (last === after) return events;
    // The retained request lives in the provider (ADR-0027), so it must be
    // durable before the cursor moves past the knock that carried it.
    try {
      await this.persistDurableOrThrow();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ChatClient: knock batch not persisted; cursor held", err);
      return events;
    }
    writeKnockCursor(groupIdB64, last, owner);
    return events;
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

    return this.applyPolledRows(myIdentity, group, after, raw, cursorRef);
  }

  /** The row loop and the cursor gate of {@link applyPolledBatch}. */
  private async applyPolledRows(
    myIdentity: WasmIdentity,
    group: WasmGroup,
    after: number,
    raw: FetchResult,
    cursorRef: PollCursorRef | undefined,
  ): Promise<PollMessagesResult> {
    const events: IncomingChatEvent[] = [];
    let cursor = after;
    // Echo ids matched in this batch, CONSUMED only once the cursor actually
    // advances past them. Deleting on match would be wrong on the held path:
    // the batch gets re-fetched, our own row would then be handed to OpenMLS,
    // and every subsequent poll would log a `CannotDecryptOwnMessage` for it.
    const matchedEchoes: number[] = [];
    const groupIdB64 = base64Encode(group.groupId);
    // ADR-0057 root handover - the role table's owner before each applied
    // row, so a commit that moves ownership away from this account is seen
    // at the apply that carried it.
    let ownerSeen = readRoleTableOwner(group);
    for (const message of raw.messages) {
      cursor = message.id;
      // Drop our own echoes — the DS doesn't dedupe and
      // OpenMLS can't decrypt them on the sender side at the
      // same epoch. Matched against THIS group's registry only: a DS id is
      // unique within one journal, so a group-blind match would suppress
      // another chat's row of the same id (see `markOwnPublishedCommit`).
      if (isOwnPublished(groupIdB64, message.id)) {
        matchedEchoes.push(message.id);
        continue;
      }
      // Marked BEFORE the call, not after. A throw is not proof that nothing
      // durable changed — `process_message` decrypts and ratchets before the
      // ADR-0022 role gate can refuse a commit or the ADR-0045 receipt gate can
      // drop a proposal. See `chat/durable-cursor.ts`.
      this.cursorGate.markTouched();
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
        // commit refused by the gate — log and skip. The cursor still advances
        // past it, deliberately: a row that can never apply must not be able to
        // wedge the poll, and only a PERSIST failure holds the cursor. The two
        // are distinguishable because they are different operations at
        // different points in the batch.
        // eslint-disable-next-line no-console
        console.warn("processIncomingVerifyingRoles failed", message.id, err);
        continue;
      }
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
      );
      // ADR-0057 — consumed here, at receipt, and never handed on.
      if (event.kind === "registrarSeed") {
        await consumeRegistrarSeed(group, event);
        continue;
      }
      // Never shown, never persisted, never previewed.
      if (event.kind === "serviceEnvelope") continue;
      events.push(event);
    }

    // Still inside the lock: the ratchet and the journal position move
    // together, and in THIS order. The persist is fail-closed (it re-throws
    // inside the gate, which reports `held`) rather than the best-effort write
    // it used to be — a swallowed failure here left the durable MLS state
    // behind a cursor that had already moved on, which since ADR-0045 strands
    // the device permanently. See `chat/durable-cursor.ts`.
    const verdict = await this.cursorGate.commit(
      () => this.persistDurableOrThrow(),
      () => {
        cursorRef?.advance(cursor);
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
    return { events, cursor, hasMore: raw.hasMore, outcome: "applied" };
  }
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
  try {
    return group.members() as unknown as RosterMember[];
  } catch {
    return [];
  }
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
): Promise<IncomingChatEvent> {
  // The WASM `ProcessedMessage.kind` is a PascalCase string
  // discriminator: "Application" | "Commit" | "Proposal" |
  // "ExternalJoinProposal" (see lakko-ffi-wasm/src/mls.rs).
  if (processed.kind === "Application" && processed.applicationData !== undefined) {
    const bytes = processed.applicationData;
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
    // ADR-0065 R.6 — the group-closed envelope (a successor claim), the group name's twin:
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
    if (body.kind === "chat") {
      return {
        kind: "application",
        messageId,
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
        // ADR-0065 R.7 case 1 — the re-send marker, for the receive rule.
        ...(body.resendOrigin === undefined ? {} : { resendOrigin: body.resendOrigin }),
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
