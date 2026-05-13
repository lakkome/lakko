// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.util.Collections
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsClientArgError
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsNetworkError
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.ProcessedMessage

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
                group.processIncoming(identity, message.payload)
            } catch (_: FfiException) {
                // Bad frame, wrong epoch, malformed wire. Drop +
                // advance the cursor.
                continue
            }
            events.add(toEvent(message.id, message.createdAt, processed))
        }

        return PollMessagesResult(events = events, cursor = cursor, hasMore = raw.hasMore)
    }
}

private fun toEvent(
    messageId: Long,
    createdAt: String,
    processed: ProcessedMessage,
): IncomingChatEvent =
    when (processed) {
        is ProcessedMessage.Application -> IncomingChatEvent.Application(
            messageId = messageId,
            // UTF-8 decode with replacement-on-error so malformed
            // bytes still surface as the best-effort string we
            // can show. The plaintext was already authenticated
            // by the MLS Application message's MAC.
            plaintext = String(processed.data, Charsets.UTF_8),
            createdAt = createdAt,
        )
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
        is ProcessedMessage.ExternalJoinProposal -> IncomingChatEvent.System(
            messageId = messageId,
            systemKind = SystemKind.ExternalJoinProposal,
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
    /** Application-data message (user-visible chat row). */
    public data class Application(
        val messageId: Long,
        /** UTF-8-decoded plaintext (with replacement on malformed
         *  bytes — the MLS layer has already authenticated the
         *  ciphertext, so the worst-case here is malformed UTF-8
         *  from a buggy peer, not a tampered payload). */
        val plaintext: String,
        val createdAt: String,
    ) : IncomingChatEvent

    /** Commit / proposal / external-join-proposal frame. MLS
     *  state advanced, no user-visible bytes. */
    public data class System(
        val messageId: Long,
        val systemKind: SystemKind,
        val createdAt: String,
    ) : IncomingChatEvent
}

/** Kind discriminator for [IncomingChatEvent.System]. */
public enum class SystemKind {
    Commit,
    Proposal,
    ExternalJoinProposal,
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
