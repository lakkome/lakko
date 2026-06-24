// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.util.Collections
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsClientArgError
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsNetworkError
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ProcessedMessage
import uniffi.lakko_ffi_uniffi.parseBodyEnvelope

/**
 * Application-message orchestrator on top of MLS groups.
 *
 * Mirror of `apps/web/src/chat/client.ts`, simplified for the
 * Android side:
 *
 *   * No `IdentitySecrets` indirection — the caller hands a
 *     live [Identity] handle directly (caller-managed lifetime;
 *     ChatRepository in 1.15f4 will thread the canonical
 *     Identity through).
 *   * No `onPersist` hook on this layer — MLS state persistence
 *     (mirror of the wasm `Identity.snapshotStorage` flow) lands
 *     with 1.15f3 once `Identity.snapshotStorage` is exposed on
 *     UniFFI.
 *
 * Two operations:
 *
 *   * [sendMessage] — `Group.createApplicationMessage(identity,
 *     plaintext)` → POST to `/ds/group/{group_id_hex}/messages`
 *     via [DsClient.publishGroup]. Returns the DS message id
 *     (also the cursor value).
 *
 *   * [pollMessages] — `DsClient.fetchGroup` for the group's
 *     journal → for each row, `group.processIncoming(identity,
 *     payload)` → dispatch to [IncomingChatEvent]. Application
 *     messages carry plaintext + a `kind = Application`
 *     discriminator; commits / proposals advance MLS state but
 *     surface as `System` rows the UI can render as a thin
 *     separator (or skip).
 *
 * ## Self-message echo
 *
 * `Group.createApplicationMessage` produces wire bytes that look
 * like a normal MLS application message — the DS does not
 * deduplicate, and a local poll WILL pick our own messages back
 * up. OpenMLS can't decrypt the sender's own application
 * messages at the same epoch, so it would surface as a
 * `processIncoming` failure: we'd see a "can't decrypt own
 * message" exception, swallow it, and the UI would never see
 * the row.
 *
 * Instead we **track the DS ids of our own publishes** during
 * the session and drop matches on receive. The set lives on the
 * [ChatClient] instance — swapping clients (e.g. logout) loses
 * the list; pending echoes after a swap surface as the
 * processIncoming-failed path above, which is harmless but
 * leaves a single noisy log entry per orphaned echo.
 *
 * The 1.18g flow extends this to commits as well: when
 * `ContactsClient.addMemberToGroup` broadcasts a commit, the
 * caller registers the resulting DS id via
 * [markOwnPublishedCommit] so the next poll suppresses the
 * echo. Cross-client because `ContactsClient` owns the
 * addMember orchestration but `ChatClient` owns the
 * `ownPublishedIds` registry; a thin accessor keeps the
 * surfaces independent.
 */
public class ChatClient(
    private val ds: DsClient = DsClient(),
) : AutoCloseable {

    /**
     * Track DS message ids we ourselves just published, so the
     * next poll can drop the echo. `Collections.synchronizedSet`
     * because [sendMessage] and [pollMessages] may run on
     * different coroutines on different dispatchers; the set is
     * mutated from both.
     */
    private val ownPublishedIds: MutableSet<Long> =
        Collections.synchronizedSet(mutableSetOf())

    override fun close() {
        ds.close()
    }

    /**
     * Encrypt [plaintext] for the current group epoch and post
     * the wire bytes to the DS group journal. Returns the DS
     * receipt fields.
     *
     * Caller keeps [group] alive — the [Group] handle carries
     * the MLS ratchet state; dropping it would lose every
     * subsequent message. 1.15f3 wires snapshot persistence so
     * a restart can rehydrate the state from disk.
     *
     * @throws ChatClientArgError on empty plaintext.
     * @throws ChatError wrapping any underlying MLS / DS / FFI
     *         failure.
     */
    public suspend fun sendMessage(
        identity: Identity,
        group: Group,
        plaintext: ByteArray,
    ): SendMessageResult {
        if (plaintext.isEmpty()) {
            throw ChatClientArgError("plaintext must be non-empty")
        }

        val wireBytes = try {
            group.createApplicationMessage(identity, plaintext)
        } catch (e: FfiException) {
            throw ChatError("createApplicationMessage failed", e)
        }

        val groupId = try {
            group.groupId()
        } catch (e: FfiException) {
            throw ChatError("group.groupId unavailable", e)
        }

        val publish = try {
            ds.publishGroup(groupId, wireBytes)
        } catch (e: DsClientArgError) {
            throw ChatError("DS publish failed", e)
        } catch (e: DsHttpError) {
            throw ChatError("DS publish failed", e)
        } catch (e: DsNetworkError) {
            throw ChatError("DS publish failed", e)
        }
        ownPublishedIds.add(publish.id)

        return SendMessageResult(messageId = publish.id, createdAt = publish.createdAt)
    }

    /**
     * Register a DS message id as one we published — typically
     * a commit returned by `ContactsClient.addMemberToGroup`'s
     * broadcast step (1.18g equivalent). The next
     * [pollMessages] will see the id and drop it via the
     * [ownPublishedIds] filter, same path
     * [sendMessage] uses for application messages.
     *
     * Without registration the sender's own [pollMessages]
     * would hit [Group.processIncoming] on its own outgoing
     * commit at the already-advanced epoch, fail, and emit a
     * phantom System row in the timeline.
     */
    public fun markOwnPublishedCommit(messageId: Long) {
        ownPublishedIds.add(messageId)
    }

    /**
     * Pull all rows newer than [after] for the group's journal,
     * decrypt application messages, return a list of
     * discriminated [IncomingChatEvent]s + a fresh cursor.
     *
     * Each [Group.processIncoming] mutates MLS provider storage
     * (application messages drain keys, commits replace state,
     * proposals queue). 1.15f3 wires a persist hook here so the
     * snapshot stays in sync.
     *
     * Bad frames / wrong epochs / malformed wire are silently
     * dropped — the cursor still advances so we don't
     * re-process. Same posture as the contacts inbox poll.
     */
    public suspend fun pollMessages(
        identity: Identity,
        group: Group,
        after: Long = 0,
        waitSecs: Int = 0,
    ): PollMessagesResult {
        val groupId = try {
            group.groupId()
        } catch (e: FfiException) {
            throw ChatError("group.groupId unavailable", e)
        }

        val raw = try {
            ds.fetchGroup(groupId, after = after, waitSecs = waitSecs)
        } catch (e: DsClientArgError) {
            throw ChatError("DS fetch failed", e)
        } catch (e: DsHttpError) {
            throw ChatError("DS fetch failed", e)
        } catch (e: DsNetworkError) {
            throw ChatError("DS fetch failed", e)
        }

        val events = mutableListOf<IncomingChatEvent>()
        var cursor = after
        for (message in raw.messages) {
            cursor = message.id

            // Drop our own echoes — the DS doesn't dedupe and
            // OpenMLS can't decrypt them on the sender side at
            // the same epoch.
            if (ownPublishedIds.remove(message.id)) {
                continue
            }

            val processed = try {
                // ADR-0022 — the CHAT receive loop enforces the group-admin
                // role policy: an add / remove / role-change commit whose
                // committer's role does not authorise it is REFUSED (throws)
                // and skipped, never merged (fail-closed). A group with no role
                // table (legacy) behaves like the open `processIncoming`. This
                // is the chat group, not the SFU call group (the call path
                // reuses the chat exporter, not a separate Group), so the
                // call-path caveat does not apply here.
                group.processIncomingVerifyingRoles(identity, message.payload)
            } catch (_: FfiException) {
                // Bad frame, wrong epoch, malformed wire, OR a role-unauthorized
                // commit refused by the gate. Drop + advance the cursor.
                continue
            }
            events.add(toEvent(message.id, message.createdAt, processed))
        }

        return PollMessagesResult(events = events, cursor = cursor, hasMore = raw.hasMore)
    }
}

/** Phase 3a10 — cheap byte-sniff for the
 *  `CallSignalingPayload` proto. Returns `true` when the
 *  first byte matches one of the oneof body tags
 *  (0x52/0x5A/0x62/0x6A for fields 10..13). Chat envelopes
 *  start with `{` (0x7B); sets don't overlap. */
private fun looksLikeCallSignaling(bytes: ByteArray): Boolean {
    if (bytes.isEmpty()) return false
    val first = bytes[0].toInt() and 0xFF
    return first == 0x52 || first == 0x5A || first == 0x62 || first == 0x6A
}

private fun toEvent(
    messageId: Long,
    createdAt: String,
    processed: ProcessedMessage,
): IncomingChatEvent =
    when (processed) {
        is ProcessedMessage.Application -> {
            // Phase 3a10 — sniff for call-signaling proto
            // bytes before running the chat-envelope
            // parser. Cheap unambiguous discriminator:
            // CallSignalingPayload's first proto wire tag
            // is one of 0x52 / 0x5A / 0x62 / 0x6A
            // (oneof fields 10..13); chat envelopes start
            // with `{` (0x7B). No overlap.
            if (looksLikeCallSignaling(processed.data)) {
                IncomingChatEvent.CallSignaling(
                    messageId = messageId,
                    payloadBytes = processed.data,
                    createdAt = createdAt,
                )
            } else {
                // ADR-0023 — feed the application bytes through
                // the structured body parser, which surfaces the
                // chat / edit / delete / reaction discriminator.
                // The plaintext was already authenticated by the
                // MLS Application message's MAC, so the worst case
                // here is a malformed envelope from a buggy peer —
                // never a tampered payload. A malformed / ambiguous
                // envelope degrades to CHAT with the raw text;
                // legacy pre-1.19 plain-UTF-8 bytes collapse to
                // `(text = <utf-8 lossy>, attachments = [])`.
                //
                // `senderLeaf` is the ORIGINAL author's MLS leaf for
                // this Application message (`null` for an external
                // sender — such a message can never be a valid
                // edit/delete target). It feeds the author-only
                // edit/delete gate + reaction attribution.
                val body = parseBodyEnvelope(processed.data)
                val senderLeaf = processed.senderLeaf
                when (body.kind) {
                    MessageBodyKind.CHAT -> IncomingChatEvent.Application(
                        messageId = messageId,
                        text = body.text,
                        attachments = body.attachments,
                        plaintext = body.text,
                        senderLeaf = senderLeaf,
                        createdAt = createdAt,
                    )
                    MessageBodyKind.EDIT -> IncomingChatEvent.Op(
                        messageId = messageId,
                        op = MessageOp.Edit,
                        target = body.target ?: 0L,
                        text = body.text,
                        emoji = null,
                        senderLeaf = senderLeaf,
                        createdAt = createdAt,
                    )
                    MessageBodyKind.DELETE -> IncomingChatEvent.Op(
                        messageId = messageId,
                        op = MessageOp.Delete,
                        target = body.target ?: 0L,
                        text = "",
                        emoji = null,
                        senderLeaf = senderLeaf,
                        createdAt = createdAt,
                    )
                    MessageBodyKind.REACTION -> IncomingChatEvent.Op(
                        messageId = messageId,
                        op = MessageOp.Reaction,
                        target = body.target ?: 0L,
                        text = "",
                        emoji = body.emoji,
                        senderLeaf = senderLeaf,
                        createdAt = createdAt,
                    )
                    // ADR-0024 — an ephemeral typing signal. Never a
                    // bubble; the repository folds it into the per-leaf
                    // typing state (with a stale-`createdAt` re-sync
                    // guard + auto-expiry). `senderLeaf` attributes it
                    // (an unattributable signal is dropped on apply).
                    MessageBodyKind.TYPING -> IncomingChatEvent.Typing(
                        messageId = messageId,
                        isTyping = body.isTyping ?: false,
                        senderLeaf = senderLeaf,
                        createdAt = createdAt,
                    )
                    // ADR-0024 — a read receipt: the sender has read up
                    // to `target` (the read-up-to messageId watermark).
                    // The repository folds it into the monotone
                    // per-leaf `readUpTo` map.
                    MessageBodyKind.READ_RECEIPT -> IncomingChatEvent.ReadReceipt(
                        messageId = messageId,
                        upTo = body.target ?: 0L,
                        senderLeaf = senderLeaf,
                        createdAt = createdAt,
                    )
                }
            }
        }
        is ProcessedMessage.Commit -> IncomingChatEvent.System(
            messageId = messageId,
            systemKind = SystemKind.Commit,
            createdAt = createdAt,
        )
        is ProcessedMessage.Proposal -> IncomingChatEvent.System(
            messageId = messageId,
            systemKind = SystemKind.Proposal,
            createdAt = createdAt,
        )
        is ProcessedMessage.ExternalJoinProposal -> IncomingChatEvent.ExternalJoinRequest(
            // ADR-0027 (G-3) — surface the joiner's signature key the members
            // sheet needs to approve / reject, not a content-less system row.
            messageId = messageId,
            joinerSignatureKey = processed.joinerSignatureKey,
            createdAt = createdAt,
        )
    }

// =============================================================================
// Result types
// =============================================================================

/** Result of [ChatClient.sendMessage]. */
public data class SendMessageResult(
    /** DS-assigned monotonic id (also the cursor value for the
     *  next [ChatClient.pollMessages] call). */
    val messageId: Long,
    /** ISO-8601 timestamp from the DS. */
    val createdAt: String,
)

/** Discriminated chat event surfaced by [ChatClient.pollMessages]. */
public sealed interface IncomingChatEvent {
    /** Application-data message (user-visible chat row).
     *
     *  Phase 1.19c4: now carries `(text, attachments)` from the
     *  1.19a JSON envelope (parsed via the UniFFI
     *  `parseEnvelope(bytes:)` surface from 1.19b). Legacy
     *  pre-1.19 plain-UTF-8 bytes collapse cleanly to
     *  `(text = <utf-8 lossy>, attachments = [])` via the
     *  parser's documented fallback paths — existing bubbles
     *  render unchanged. `plaintext` is preserved as an alias
     *  of `text` for transitional call-site compatibility. */
    public data class Application(
        val messageId: Long,
        /** Plain user-visible text from the envelope (empty
         *  string is valid — attachment-only message). */
        val text: String,
        /** Zero or more attachment refs from the envelope. */
        val attachments: List<AttachmentRef>,
        /** Alias of [text] — kept for the few call sites that
         *  still read `plaintext`. New code should read
         *  [text]. */
        val plaintext: String,
        /** ADR-0023 — the original author's MLS leaf index. The
         *  timeline records it so a later edit/delete op can be
         *  gated against it (author-only). `null` for an
         *  external sender — such a message can never be a valid
         *  edit/delete target. */
        val senderLeaf: UInt?,
        val createdAt: String,
    ) : IncomingChatEvent

    /** ADR-0023 — an edit / delete / reaction op carried in the
     *  same JSON envelope as a chat message. The repository
     *  folds it into the *target* message (keyed on [target] ==
     *  the target's DS [Application.messageId]) rather than
     *  rendering it as a standalone bubble. The op itself still
     *  consumes a DS [messageId] + surfaces here so the
     *  incremental poll applies it exactly once. */
    public data class Op(
        /** This op message's own DS-assigned id. */
        val messageId: Long,
        /** Which op this is. */
        val op: MessageOp,
        /** The DS [Application.messageId] of the target message. */
        val target: Long,
        /** Edit text (empty for delete / reaction). */
        val text: String,
        /** Reaction emoji (`null` outside [MessageOp.Reaction]). */
        val emoji: String?,
        /** The op sender's MLS leaf index. The author-only gate
         *  (edit / delete) checks it against the target's stored
         *  author leaf; a reaction attributes its toggle to it.
         *  `null` ⇒ the op is dropped (fail-closed). */
        val senderLeaf: UInt?,
        val createdAt: String,
    ) : IncomingChatEvent

    /** ADR-0024 — an ephemeral typing signal carried in the same
     *  JSON envelope. Never rendered as a bubble: the repository
     *  folds it into the per-`(group, senderLeaf)` "is typing"
     *  state with a stale-`createdAt` re-sync guard + auto-expiry.
     *  Consumes a DS [messageId] like every other op so the
     *  incremental poll applies it exactly once. */
    public data class Typing(
        /** This signal's own DS-assigned id. */
        val messageId: Long,
        /** `true` ⇒ the sender is typing now; `false` ⇒ an explicit
         *  stop (blur / on-send). */
        val isTyping: Boolean,
        /** The signal sender's MLS leaf index. `null` ⇒ dropped
         *  (no attribution possible). */
        val senderLeaf: UInt?,
        /** DS row timestamp — the receiver ignores any typing
         *  signal older than the expiry window (re-sync replay
         *  guard). */
        val createdAt: String,
    ) : IncomingChatEvent

    /** ADR-0024 — a read receipt: the sender has read up to [upTo]
     *  (the highest read messageId) in this group. The repository
     *  folds it into a monotone per-`(group, senderLeaf)` watermark
     *  (a lower value never overwrites a higher one, so re-sync
     *  replay is idempotent). The message author renders read-state
     *  on its OWN messages from the other members' watermarks. */
    public data class ReadReceipt(
        /** This receipt's own DS-assigned id. */
        val messageId: Long,
        /** The read-up-to messageId watermark the sender asserts. */
        val upTo: Long,
        /** The receipt sender's MLS leaf index. `null` ⇒ dropped. */
        val senderLeaf: UInt?,
        val createdAt: String,
    ) : IncomingChatEvent

    /** Commit / proposal / external-join-proposal frame. MLS
     *  state advanced, no user-visible bytes. */
    public data class System(
        val messageId: Long,
        val systemKind: SystemKind,
        val createdAt: String,
    ) : IncomingChatEvent

    /** Phase 3a10 — call-signaling event lifted out of an
     *  MLS application message. The chat repository
     *  dispatches it into the [me.lakko.app.calls.CallSession]
     *  state machine instead of the chat timeline. */
    public data class CallSignaling(
        val messageId: Long,
        val payloadBytes: ByteArray,
        val createdAt: String,
    ) : IncomingChatEvent {
        // Override equals / hashCode for the payload
        // ByteArray — default reference equality would
        // surprise downstream consumers.
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is CallSignaling) return false
            return messageId == other.messageId &&
                payloadBytes.contentEquals(other.payloadBytes) &&
                createdAt == other.createdAt
        }

        override fun hashCode(): Int {
            var result = messageId.hashCode()
            result = 31 * result + payloadBytes.contentHashCode()
            result = 31 * result + createdAt.hashCode()
            return result
        }
    }

    /** ADR-0027 (G-3) — an external-join *request*: a non-member posted an
     *  external Add proposal to this group's journal and the MLS core retained
     *  it pending an owner/admin approval. Carries the joiner's leaf signature
     *  key (the `proposalRef` the approve / reject repository methods take). Not
     *  a timeline bubble — the view model folds it into a pending list rendered
     *  in the members sheet (the retained set, `group.pendingExternalJoins()`,
     *  is the source of truth; this event is the nudge a new request landed). */
    public data class ExternalJoinRequest(
        val messageId: Long,
        val joinerSignatureKey: ByteArray,
        val createdAt: String,
    ) : IncomingChatEvent {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is ExternalJoinRequest) return false
            return messageId == other.messageId &&
                joinerSignatureKey.contentEquals(other.joinerSignatureKey) &&
                createdAt == other.createdAt
        }

        override fun hashCode(): Int {
            var result = messageId.hashCode()
            result = 31 * result + joinerSignatureKey.contentHashCode()
            result = 31 * result + createdAt.hashCode()
            return result
        }
    }
}

/** Kind discriminator for [IncomingChatEvent.System]. */
public enum class SystemKind {
    Commit,
    Proposal,
    ExternalJoinProposal,
}

/** ADR-0023 — op discriminator for [IncomingChatEvent.Op]. */
public enum class MessageOp {
    Edit,
    Delete,
    Reaction,
}

/** Result of [ChatClient.pollMessages]. */
public data class PollMessagesResult(
    val events: List<IncomingChatEvent>,
    /** Fresh cursor — pass back as [ChatClient.pollMessages]'s
     *  `after` argument. */
    val cursor: Long,
    /** `true` if the DS reported more rows past the last
     *  returned id; caller should poll again immediately with
     *  the new cursor. */
    val hasMore: Boolean,
)

// =============================================================================
// Errors
// =============================================================================

/** Programmer error caught before any network call. */
public class ChatClientArgError(message: String) : IllegalArgumentException(message)

/** Wraps any underlying failure (DS, MLS process, FFI). The
 *  cause carries the original exception for diagnostics. */
public class ChatError(message: String, cause: Throwable) :
    RuntimeException(message, cause)
