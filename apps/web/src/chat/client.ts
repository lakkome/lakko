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
      /** The MLS frame kind: "commit" / "proposal" /
       *  "external-join-proposal". Lower-case for parity with
       *  `ProcessedMessageKind` strings on the Rust side. */
      systemKind: "commit" | "proposal" | "external-join-proposal";
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
    };

/** Result of `pollMessages`. */
export interface PollMessagesResult {
  events: IncomingChatEvent[];
  cursor: number;
  hasMore: boolean;
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
   *  write missed; the next mutation will retry. */
  private async persistBestEffort(): Promise<void> {
    if (this.onPersist === undefined) return;
    try {
      await this.onPersist();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ChatClient: MLS snapshot persist failed", err);
    }
  }

  /**
   * Encrypt `plaintext` for the current group epoch and post
   * the wire bytes to the DS. Returns the DS-assigned id.
   *
   * Caller is responsible for keeping `group` alive — the
   * Group instance carries the MLS ratchet state. With
   * `identityProvider` wired (1.16e6c2), the underlying provider
   * storage is persisted to IDB after each successful publish.
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

    let publish;
    try {
      publish = await this.ds.publishGroup(groupId, wireBytes);
    } catch (err) {
      throw new ChatError("DS publish failed", err);
    }
    this.ownPublishedIds.add(publish.id);

    // 1.16e6c2: createApplicationMessage advanced the message
    // secret tree on disk (well, in the canonical provider's
    // storage). Snapshot it.
    await this.persistBestEffort();

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
   * 1.20e — fire `pushClient.notifyGroup(groupId, messageId)`
   * on the side. Best-effort: if push-gw is down, the message
   * has still been published to DS + recipients will pick it
   * up on their next poll. We don't gate `sendMessage` on this.
   */
  private notifyPushGroupBestEffort(
    groupId: Uint8Array,
    messageId: number,
  ): void {
    // The notifyGroup call drives a PoW solve, which takes
    // 100-500 ms; running it inline in sendMessage would
    // double the user-visible send latency. Off the critical
    // path:
    void this.pushClient
      .notifyGroup(groupId, messageId)
      .catch((err) => {
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
   * Pull all rows newer than `after` for the group's journal,
   * decrypt application messages, return discriminated events
   * + a fresh cursor. Persists the MLS snapshot once at the
   * end if any rows were processed (each `processIncoming`
   * mutates provider storage — application messages drain
   * keys, commits replace state, proposals queue).
   */
  async pollMessages(
    identity: IdentitySecrets | null,
    group: WasmGroup,
    after: number,
    waitSecs = 0,
  ): Promise<PollMessagesResult> {
    const myIdentity = await this.resolveIdentity(identity);

    const groupId: Uint8Array = group.groupId;
    let raw;
    try {
      raw = await this.ds.fetchGroup(groupId, { after, waitSecs });
    } catch (err) {
      throw new ChatError("DS fetch failed", err);
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

    return { events, cursor, hasMore: raw.hasMore };
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
  const sk: "commit" | "proposal" | "external-join-proposal" =
    processed.kind === "Commit"
      ? "commit"
      : processed.kind === "Proposal"
        ? "proposal"
        : "external-join-proposal";
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
