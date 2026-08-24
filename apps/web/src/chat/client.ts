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
 * After every successful state-mutating call we fire the
 * optional `onPersist` callback so the on-disk MLS snapshot
 * stays in sync with the canonical Identity's provider. Failures
 * log-and-continue — the in-memory state is still good and the
 * next mutation will retry the persist naturally.
 */
import { DsClient } from "../ds/client";
import type { FetchResult } from "../ds/client";
import { base64Encode } from "../auth/account-id";
import { withGroupMlsLock } from "../mls/group-lock";
import { PushClient } from "../push/client";
import { loadWasm } from "../wasm";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import type { IdentitySecrets } from "../auth/AuthContext";
import {
  parseBodyEnvelope,
  type AttachmentRefBytes,
} from "./attachments";
import { tryDecodeSecretEnvelope } from "../push/group-routing";

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
      /** ADR-0023 — the original author's MLS leaf index, used
       *  by the UI to remember authorship for the author-only
       *  edit/delete gate + reaction attribution. `undefined`
       *  for a frame whose sender leaf the FFI couldn't surface
       *  (a control message, an external sender) — such a
       *  message can never be a valid edit/delete target. */
      senderLeaf: number | undefined;
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
      /** ADR-0024 — the typing state (only for `op === "typing"`):
       *  `true` = started typing, `false` = stopped. `undefined`
       *  for every other op. */
      isTyping: boolean | undefined;
      /** The op sender's MLS leaf index — the author-only gate
       *  (edit/delete) checks it against the target's stored
       *  author leaf, reactions attribute the toggle to it, and
       *  typing / read receipts are keyed on it.
       *  `undefined` ⇒ the op is dropped (fail-closed). */
      senderLeaf: number | undefined;
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
}

/** What one `pollMessages` attempt did. */
export type PollOutcome =
  /** The batch was fetched and applied under the lock. */
  | "applied"
  /** The cursor moved between the fetch and the lock, so the batch in hand was
   *  stale. Dropped WITHOUT applying and WITHOUT touching the cursor; the next
   *  poll re-fetches from the current position. Only ever returned when a
   *  `PollCursorRef` was supplied. */
  | "cursor-moved";

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
  /** ADR-0042 — resolve the opaque `push_gid` for a group's DS
   *  `group_id`, so the notify wake routes under the routing id instead
   *  of the leaky DS journal key. Returns `null` when this device does
   *  not (yet) hold the group's `push_routing_secret` — in which case the
   *  wake is SKIPPED (never notified under `group_id`, invariant #5). When
   *  UNSET (tests / standalone callers with no push-routing store) the
   *  notify falls back to the raw `group_id`, mirroring the
   *  `identityProvider` fallback convention — production paths
   *  (`<ChatProvider>`) MUST wire this. */
  pushGidResolver?: (groupId: Uint8Array) => Promise<Uint8Array | null>;
}

export class ChatClient {
  /** Track DS message ids we ourselves just published, so the
   *  next poll can drop the echo. Cleared per `ChatClient`
   *  instance; if the UI loses this list (e.g. swap clients)
   *  the echo surfaces as a `processIncoming` failure that
   *  we log-and-skip. */
  private readonly ownPublishedIds = new Set<number>();
  private readonly identityProvider: (() => WasmIdentity | null) | undefined;
  private readonly onPersist: (() => Promise<void>) | undefined;
  private readonly pushGidResolver:
    | ((groupId: Uint8Array) => Promise<Uint8Array | null>)
    | undefined;

  constructor(
    private readonly ds: DsClient = new DsClient(),
    options: ChatClientOptions = {},
    /** 1.20e — push-gw client for the fan-out wake. Side-channel:
     *  sendMessage fires `notifyGroup` after the DS publish to
     *  wake every subscribed device. Default to a fresh client
     *  so the chat surface needs no extra wiring. */
    private readonly pushClient: PushClient = new PushClient(),
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

  /** Run the persist hook, swallow + log failures. The
   *  in-memory MLS state is still good even if the snapshot
   *  write missed; the next mutation will retry.
   *
   *  Only safe on the RECEIVE / non-critical paths (`pollMessages`,
   *  contact ops). It MUST NOT be used on the send path — see
   *  `persistDurableOrThrow`. */
  private async persistBestEffort(): Promise<void> {
    if (this.onPersist === undefined) return;
    try {
      await this.onPersist();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ChatClient: MLS snapshot persist failed", err);
    }
  }

  /** Persist the canonical Identity's provider snapshot durably,
   *  AWAIT the IDB write, and RE-THROW on failure — the
   *  fail-closed counterpart of `persistBestEffort`.
   *
   *  This is the security-critical variant used by `sendMessage`
   *  BEFORE it publishes. A swallowed persist failure here would
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
   */
  async sendMessage(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    plaintext: Uint8Array,
  ): Promise<SendMessageResult> {
    if (plaintext.length === 0) {
      throw new ChatClientArgError("plaintext must be non-empty");
    }
    const myIdentity = await this.resolveIdentity(identity);

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

    let groupId: Uint8Array;
    try {
      groupId = group.groupId;
    } catch (err) {
      throw new ChatError("group.groupId unavailable", err);
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
        "MLS snapshot persist failed — send aborted to prevent nonce reuse",
        err,
      );
    }

    let publish;
    try {
      publish = await this.ds.publishGroup(groupId, wireBytes);
    } catch (err) {
      throw new ChatError("DS publish failed", err);
    }
    this.ownPublishedIds.add(publish.id);

    // 1.20e — fan a wake out to every device subscribed to
    // this group. Fire-and-forget — failures here would not
    // help the sender (the message is already on the DS,
    // recipients will see it on their next poll regardless).
    // No await; the function returns the DS receipt
    // immediately while the wake fans out in the background.
    this.notifyPushGroupBestEffort(groupId, publish.id);

    return { messageId: publish.id, createdAt: publish.createdAt };
  }

  /**
   * 1.20e — fire `pushClient.notifyGroup(pushGid, messageId)`
   * on the side. Best-effort: if push-gw is down, the message
   * has still been published to DS + recipients will pick it
   * up on their next poll. We don't gate `sendMessage` on this.
   *
   * ADR-0042 — the notify routes under the opaque `push_gid`, NOT the DS
   * `group_id`. When a `pushGidResolver` is wired (production) and it
   * yields `null` (this device holds no secret for the group yet), the
   * wake is SKIPPED rather than leaking the `group_id` to push-gw
   * (invariant #5). A publisher is a member, so it normally holds the
   * secret. When no resolver is wired (tests) the raw `group_id` is used,
   * matching the `identityProvider`/`resolveIdentity` fallback convention.
   */
  private notifyPushGroupBestEffort(
    groupId: Uint8Array,
    messageId: number,
  ): void {
    // The notifyGroup call drives a PoW solve, which takes
    // 100-500 ms; running it inline in sendMessage would
    // double the user-visible send latency. Off the critical
    // path:
    void (async (): Promise<void> => {
      let target = groupId;
      if (this.pushGidResolver !== undefined) {
        const pushGid = await this.pushGidResolver(groupId);
        if (pushGid === null) {
          // ADR-0042 — no secret held yet: do NOT wake under the DS key.
          // Recipients still receive the message on their next foreground
          // poll; the wake resumes once the secret propagates.
          return;
        }
        target = pushGid;
      }
      await this.pushClient.notifyGroup(target, messageId);
    })().catch((err) => {
      // eslint-disable-next-line no-console
      console.warn("push notify-group failed", err);
    });
  }

  /**
   * Register a DS message id as one **we** published (Phase
   * 1.18g) — typically a commit returned by
   * `ContactsClient.addMemberToGroup`'s broadcast step.
   *
   * The next `pollMessages` will see this id and drop it via
   * the `ownPublishedIds` filter — same suppression path
   * `sendMessage` uses for application messages. Without
   * registration the sender's own pollMessages would hit
   * `processIncoming` on its own outgoing commit at the
   * already-advanced epoch, fail, log a noisy console warning,
   * and render a phantom system row in the timeline.
   *
   * Crosses the client boundary because `ContactsClient` owns
   * the addMember flow (discovery resolution + Welcome shipping)
   * but `ChatClient` owns the ownPublishedIds registry.
   * Plumbing the registry through ContactsClient would couple
   * the two; a thin `markOwnPublishedCommit` accessor keeps the
   * surfaces independent.
   */
  markOwnPublishedCommit(messageId: number): void {
    this.ownPublishedIds.add(messageId);
  }

  /**
   * ADR-0042 — broadcast a CONTROL application message to the group
   * journal (currently: the push-routing-secret distribution envelope).
   *
   * Mirrors `sendMessage`'s persist-before-publish fail-closed discipline
   * — `createApplicationMessage` advances the sender ratchet, so the
   * post-advance state MUST be durably persisted before the ciphertext
   * reaches the DS (the AEAD nonce-reuse-on-crash class). Unlike
   * `sendMessage` it fires NO user-facing push wake: a routing-secret
   * handshake should not wake devices (a member without the secret cannot
   * be woken under `push_gid` yet anyway — they catch up on the next
   * foreground poll).
   *
   * Records the DS id in `ownPublishedIds` so the broadcaster's own next
   * poll drops the echo (the owner already holds the secret it minted).
   */
  async broadcastControl(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    controlBytes: Uint8Array,
  ): Promise<{ messageId: number }> {
    if (controlBytes.length === 0) {
      throw new ChatClientArgError("controlBytes must be non-empty");
    }
    const myIdentity = await this.resolveIdentity(identity);

    let wireBytes: Uint8Array;
    try {
      wireBytes = group.createApplicationMessage(myIdentity, controlBytes);
    } catch (err) {
      throw new ChatError("createApplicationMessage failed", err);
    }

    let groupId: Uint8Array;
    try {
      groupId = group.groupId;
    } catch (err) {
      throw new ChatError("group.groupId unavailable", err);
    }

    // PERSIST BEFORE PUBLISH, FAIL-CLOSED — same rationale as sendMessage.
    try {
      await this.persistDurableOrThrow();
    } catch (err) {
      throw new ChatError(
        "MLS snapshot persist failed — control broadcast aborted to prevent nonce reuse",
        err,
      );
    }

    let publish;
    try {
      publish = await this.ds.publishGroup(groupId, wireBytes);
    } catch (err) {
      throw new ChatError("DS publish failed", err);
    }
    this.ownPublishedIds.add(publish.id);
    return { messageId: publish.id };
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
   * radius. Web's poll uses `waitSecs = 0` today, so there is no long poll to
   * protect — the split is kept anyway, for the same reason Android keeps it:
   * the moment a long poll is turned on, a lock that spanned the fetch would
   * stall every send on the group for its full duration.
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
  ): Promise<PollMessagesResult> {
    const myIdentity = await this.resolveIdentity(identity);

    const groupId: Uint8Array = group.groupId;
    // The FETCH — deliberately outside the lock.
    let raw;
    try {
      raw = await this.ds.fetchGroup(groupId, { after, waitSecs });
    } catch (err) {
      throw new ChatError("DS fetch failed", err);
    }

    // The APPLY + the cursor advance — inside the lock, atomic together.
    return withGroupMlsLock(base64Encode(groupId), async () =>
      this.applyPolledBatch(myIdentity, group, after, raw, cursorRef),
    );
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
    }

    const events: IncomingChatEvent[] = [];
    let cursor = after;
    let mutated = false;
    for (const message of raw.messages) {
      cursor = message.id;
      // Drop our own echoes — the DS doesn't dedupe and
      // OpenMLS can't decrypt them on the sender side at the
      // same epoch.
      if (this.ownPublishedIds.has(message.id)) {
        this.ownPublishedIds.delete(message.id);
        continue;
      }
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
        // commit refused by the gate — log and skip. Cursor still advances.
        // eslint-disable-next-line no-console
        console.warn("processIncomingVerifyingRoles failed", message.id, err);
        continue;
      }
      // processIncoming touched provider storage even when the
      // result is a no-op for the UI (e.g. an external commit
      // that we rejected). Track the mutation so the persist
      // hook still fires.
      mutated = true;
      // 1.19c2: application messages run through the wasm
      // envelope parser (which is async because it has to
      // resolve the cached wasm module promise on first call,
      // then synchronous thereafter). System rows skip it.
      events.push(await toEvent(message.id, message.createdAt, processed));
    }

    if (mutated) {
      await this.persistBestEffort();
    }

    // Still inside the lock: the ratchet and the journal position move together.
    cursorRef?.advance(cursor);
    return { events, cursor, hasMore: raw.hasMore, outcome: "applied" };
  }
}

async function toEvent(
  messageId: number,
  createdAt: string,
  processed: ReturnType<WasmGroup["processIncoming"]>,
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
    // Phase 3a8 — sniff for call-signaling bytes before
    // running the chat-envelope parser. Call signaling is a
    // prost-encoded `CallSignalingPayload` proto; chat
    // envelopes are JSON (the first byte is `{` / 0x7B). The
    // discriminator is cheap + unambiguous because the
    // proto's first wire tag is field 10 (offer) ... field
    // 13 (end) — first byte ∈ {0x52, 0x5A, 0x62, 0x6A},
    // none of which is 0x7B.
    if (looksLikeCallSignaling(bytes)) {
      return {
        kind: "callSignaling",
        messageId,
        payloadBytes: bytes,
        createdAt,
      };
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
    // ADR-0023 — feed the application bytes through the
    // structured body parser, which surfaces the chat / edit /
    // delete / reaction discriminator. A plain chat message
    // (or any legacy / malformed envelope, which the parser
    // degrades to `chat` with the raw text) returns an
    // `application` event; an op returns the `op` event the UI
    // folds into the target message.
    const body = await parseBodyEnvelope(bytes);
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
        createdAt,
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
 * proto? Returns `true` when the first byte matches one of
 * the `CallSignalingPayload.body` oneof field tags
 * (0x52 / 0x5A / 0x62 / 0x6A) AND the bytes are NOT JSON
 * (which would start with `{` / 0x7B). Cheap enough to run
 * on every `Application` row in the poll batch.
 *
 * Wire layout for the first byte of an embedded
 * `CallSignalingPayload`:
 *
 *   field 10 (offer)        → tag = (10 << 3) | 2 = 0x52
 *   field 11 (answer)       → tag = (11 << 3) | 2 = 0x5A
 *   field 12 (candidate)    → tag = (12 << 3) | 2 = 0x62
 *   field 13 (end)          → tag = (13 << 3) | 2 = 0x6A
 *
 * Chat envelopes are JSON and start with `{` (0x7B). The
 * sets don't overlap. A false positive on a non-JSON,
 * non-signaling future-envelope shape is still harmless —
 * the caller's downstream `parseCallSignaling` rejects with
 * a proto-decode error and the caller swallows.
 */
function looksLikeCallSignaling(bytes: Uint8Array): boolean {
  if (bytes.length === 0) return false;
  const first = bytes[0] as number;
  return first === 0x52 || first === 0x5a || first === 0x62 || first === 0x6a;
}

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
