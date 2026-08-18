// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.util.Collections
import kotlin.coroutines.cancellation.CancellationException
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsClientArgError
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsNetworkError
import me.lakko.app.ds.FetchResult
import me.lakko.app.push.GroupPushRouting
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.FfiPushRoutingSecret
import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ProcessedMessage
import uniffi.lakko_ffi_uniffi.decodePushRoutingSecret
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
     * subsequent message.
     *
     * ## SECURITY: persist BEFORE publish, fail-closed
     *
     * `createApplicationMessage` ADVANCES the MLS sender
     * ratchet: it consumes the current generation N (deriving
     * the AEAD key+nonce from N) and leaves the in-memory
     * OpenMLS provider at generation N+1. Because that provider
     * is in-memory, the N+1 state lives ONLY in the snapshot
     * until it is persisted to disk.
     *
     * [persistRatchet] is invoked AFTER the ratchet has advanced
     * to N+1 and BEFORE the gen-N ciphertext is published to the
     * DS. It MUST durably persist that N+1 state and THROW on any
     * failure. If it throws, this method propagates the failure
     * and does NOT publish — so the gen-N ciphertext never leaves
     * the device. Only one gen-N ciphertext can ever exist on the
     * wire, so generation N stays safe to re-consume after a
     * reload.
     *
     * Otherwise a crash (or a silently-failed persist) between
     * the publish and the persist would reload the ratchet at N,
     * and the next send would re-consume generation N — an
     * identical AEAD (key, nonce) on a different plaintext
     * (C1 XOR C2 = P1 XOR P2 plaintext leak to the zero-trust
     * server + Poly1305 tag forgery). A crash in the NEW window
     * (after [persistRatchet], before the publish) merely SKIPS
     * generation N; RFC 9420 receivers tolerate sender-ratchet
     * gaps, so there is no reuse either way. Mirror of the web
     * reference `apps/web/src/chat/client.ts`
     * (`persistDurableOrThrow` before `publishGroup`).
     *
     * @param persistRatchet durable, fail-closed persist of the
     *        post-advance (N+1) MLS provider snapshot. Required —
     *        there is deliberately no default, so a new caller
     *        cannot silently skip it and re-open the nonce-reuse
     *        window. A [CancellationException] it raises is
     *        re-thrown verbatim (structured concurrency); any
     *        other failure aborts the send.
     * @throws ChatClientArgError on empty plaintext.
     * @throws ChatError wrapping any underlying MLS / persist /
     *         DS / FFI failure (a persist failure aborts the send
     *         BEFORE the publish).
     */
    public suspend fun sendMessage(
        identity: Identity,
        group: Group,
        plaintext: ByteArray,
        persistRatchet: suspend () -> Unit,
    ): SendMessageResult =
        publishPreparedSend(prepareSend(identity, group, plaintext, persistRatchet))

    /**
     * G-37 — the PROVIDER half of [sendMessage], split out so the caller can
     * release the repository's global provider lock before the DS publish.
     *
     * Everything here touches the shared OpenMLS provider: the ratchet advance
     * (`createApplicationMessage`, N -> N+1) and the durable snapshot of the
     * resulting state. The network half is [publishPreparedSend].
     *
     * The persist-before-publish ordering is UNCHANGED and still fail-closed —
     * it is simply expressed by the split itself: [persistRatchet] runs here, and
     * the publish cannot run until this function has returned normally. A throw
     * (empty plaintext, MLS failure, persist failure) means no [PreparedSend]
     * exists, so there is nothing to publish and the gen-N ciphertext never
     * leaves the device.
     *
     * DELIBERATELY the only place the ratchet advances: a caller that wants the
     * publish outside the provider lock MUST come through here first.
     */
    public suspend fun prepareSend(
        identity: Identity,
        group: Group,
        plaintext: ByteArray,
        persistRatchet: suspend () -> Unit,
    ): PreparedSend {
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

        // SECURITY (nonce-reuse-on-crash) — PERSIST BEFORE
        // PUBLISH, FAIL-CLOSED. The N+1 ratchet state produced by
        // `createApplicationMessage` above must be durably on disk
        // before the gen-N ciphertext reaches the DS. If the
        // persist fails, ABORT — do NOT publish — so gen N stays
        // safe to re-consume after a reload. Re-throw a
        // CancellationException verbatim so a cancelled send is
        // not misreported as a persist failure (structured
        // concurrency); convert any genuine persist failure into
        // a send-aborting ChatError.
        try {
            persistRatchet()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Throwable) {
            throw ChatError(
                "MLS snapshot persist failed — send aborted to prevent nonce reuse",
                e,
            )
        }

        return PreparedSend(groupId = groupId, wireBytes = wireBytes)
    }

    /**
     * G-37 — the NETWORK half of [sendMessage]: publish an already-prepared,
     * already-persisted ciphertext.
     *
     * Touches no MLS state at all — [PreparedSend] carries plain bytes, so this
     * runs OUTSIDE the repository's global provider lock and a 15 s DS publish no
     * longer blocks every other group's MLS work.
     */
    public suspend fun publishPreparedSend(prepared: PreparedSend): SendMessageResult {
        val publish = try {
            ds.publishGroup(prepared.groupId, prepared.wireBytes)
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
        val raw = fetchGroupJournal(groupId, after = after, waitSecs = waitSecs)
        return applyFetchedMessages(identity, group, raw, after = after)
    }

    /**
     * G-37 — the NETWORK half of [pollMessages], split out so the caller can run
     * it OUTSIDE the repository's locks.
     *
     * This touches no MLS state whatsoever: it is a plain `GET` against the DS
     * journal, and the `groupId` is a value the caller already holds (the
     * `GroupRecord` caches it), so not even the `Group` handle is read here.
     *
     * That matters because [waitSecs] is a DELIBERATE long poll — 30 s on the
     * live chat screen — and the previous shape awaited it while holding the one
     * global lock, so every send / edit / reaction / membership commit on EVERY
     * group queued behind an open chat screen's idle connection.
     *
     * The MLS half is [applyFetchedMessages], which the caller runs under the
     * lock. See `ChatRepository.pollGroup` for the composition + the guards that
     * keep the two halves consistent.
     */
    public suspend fun fetchGroupJournal(
        groupId: ByteArray,
        after: Long = 0,
        waitSecs: Int = 0,
    ): FetchResult {
        return try {
            ds.fetchGroup(groupId, after = after, waitSecs = waitSecs)
        } catch (e: DsClientArgError) {
            throw ChatError("DS fetch failed", e)
        } catch (e: DsHttpError) {
            throw ChatError("DS fetch failed", e)
        } catch (e: DsNetworkError) {
            throw ChatError("DS fetch failed", e)
        }
    }

    /**
     * G-37 — the MLS half of [pollMessages]: apply an already-fetched batch.
     *
     * DELIBERATELY NOT `suspend`. Every statement below mutates OpenMLS provider
     * state through the shared `Group` handle and must run under the caller's
     * provider lock; making it non-suspending is a structural guarantee that no
     * network wait can ever be reintroduced into this critical section (a
     * suspending call simply would not compile here).
     *
     * [after] is the cursor the batch was fetched with, and seeds the returned
     * cursor so an empty batch does not rewind it.
     */
    public fun applyFetchedMessages(
        identity: Identity,
        group: Group,
        raw: FetchResult,
        after: Long = 0,
    ): PollMessagesResult {
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
            } catch (_: Throwable) {
                // Bad frame, wrong epoch, malformed wire, a role-unauthorized commit
                // refused by the gate, OR a re-fetched own commit
                // (`CannotDecryptOwnMessage`, ADR-0040 §5 benign drop). Drop + advance
                // the cursor. ADR-0040 F3 (lakko-security) — this INNER per-message
                // catch is `Throwable`, not `FfiException`: `CannotDecryptOwnMessage`
                // surfaces as an `FfiException` today, but were a binding ever to
                // surface an own-commit re-fetch as a different `Throwable`, a narrow
                // catch would let it escape to the OUTER poll-loop catch (which does
                // NOT commit the batch cursor) and re-fetch the orphan row forever.
                // Mirrors the blanket iOS / web per-message drop posture. (Only the
                // per-message drop is widened — the outer poll-loop catch is unchanged.)
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
 *  start with `{` (0x7B); sets don't overlap.
 *
 *  `internal` rather than `private` ONLY so the background
 *  push-preview path can reuse this exact sniff instead of
 *  duplicating the tag set — see
 *  `me.lakko.app.push.isServiceEnvelope`. The proto oneof tags
 *  must have exactly one definition; a second copy would drift
 *  silently the next time a field number moves. Behaviour here is
 *  unchanged. */
internal fun looksLikeCallSignaling(bytes: ByteArray): Boolean {
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
            // ADR-0042 — sniff for a push-routing-secret distribution
            // envelope (a cheap length + first-byte pre-filter, then the
            // authoritative fail-closed FFI decode). A non-envelope
            // plaintext returns `null` and falls through to the normal
            // chat parse. First byte `'l'` (0x6c) is disjoint from both
            // the chat `{` (0x7B) and the call-signaling tags below.
            // ADR-0044 — the v2 decode yields the 32-byte secret AND the
            // `mint_epoch` the MINTER stamped it with. A retired v1
            // envelope (no stamp, hence no position in the recency order)
            // is refused by the FFI with `UnknownVersion` and lands in
            // the `null` branch — a fail-closed drop, never a
            // downgrade.
            val routingSecret: FfiPushRoutingSecret? =
                if (GroupPushRouting.looksLikeSecretEnvelope(processed.data)) {
                    try {
                        decodePushRoutingSecret(processed.data)
                    } catch (_: Throwable) {
                        null
                    }
                } else {
                    null
                }
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
            } else if (routingSecret != null) {
                // ADR-0042 — never a timeline bubble: the repository
                // persists the secret + subscribes under the derived
                // opaque `push_gid`.
                //
                // `senderLeaf` is the MLS-authenticated author leaf of
                // this Application message (`null` for an external
                // sender). It is the ONLY trustworthy sender identity
                // available here — the envelope BODY is attacker-chosen,
                // the leaf is not — so the repository's receive gate
                // resolves it to the sender's signature key and refuses
                // a CHANGED secret from a non-owner. Without it any one
                // member could unilaterally rotate the group's push_gid,
                // forcing every member to re-subscribe and stranding an
                // orphan push-gw subscription per churn. Same threading
                // precedent as the ADR-0023 author-only edit/delete gate
                // below.
                IncomingChatEvent.PushRoutingSecret(
                    messageId = messageId,
                    secret = routingSecret.secret,
                    // ADR-0044 — the MINTER's stamp, carried through so the
                    // receive gate can order it against what is held. NOT
                    // the receiver's own epoch.
                    mintEpoch = routingSecret.mintEpoch,
                    senderLeaf = processed.senderLeaf,
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

/**
 * G-37 — a sealed, already-persisted application message awaiting only its DS
 * publish. The hand-off between [ChatClient.prepareSend] (provider work, runs
 * under the repository's global provider lock) and
 * [ChatClient.publishPreparedSend] (network, runs outside it).
 *
 * Holds plain bytes only — no `Group`, no `Identity`, no provider handle — which
 * is exactly what makes it safe to carry across the lock release.
 */
public class PreparedSend internal constructor(
    /** Raw MLS group id the ciphertext is addressed to. */
    internal val groupId: ByteArray,
    /** The gen-N application-message wire bytes, already sealed. */
    internal val wireBytes: ByteArray,
)

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

    /** ADR-0042 — a push-routing-secret distribution envelope: an
     *  in-group MLS application message carrying the 32-byte
     *  `push_routing_secret` the group OWNER minted. Never a timeline
     *  bubble — the repository persists it + subscribes under the
     *  derived opaque `push_gid` (closing the invariant-#5 push
     *  social-graph leak). Consumes a DS [messageId] like any other
     *  application row so the poll cursor advances past it.
     *
     *  [senderLeaf] is the MLS-authenticated author leaf (`null` for an
     *  external sender). The repository resolves it against the live
     *  roster + the ADR-0022 role table and REFUSES a changed secret
     *  from a non-owner — only the owner may rotate a group's
     *  `push_gid`. It is carried on the event because the classifier is
     *  the only place the `ProcessedMessage.Application.senderLeaf`
     *  binding is still in hand.
     *
     *  ADR-0044 — [PushRoutingSecret.mintEpoch] is the `Group.epoch()`
     *  value the MINTER stamped this secret with, carried verbatim off
     *  the v2 envelope. The receive gate orders envelopes by
     *  `(mintEpoch, secretBytes)` and clamps the stamp against the
     *  receiver's OWN epoch, which is what refuses a stale re-broadcast
     *  of a superseded secret (`lakko-security` F-A, G-36 residual 9). */
    public data class PushRoutingSecret(
        val messageId: Long,
        val secret: ByteArray,
        val mintEpoch: ULong,
        val senderLeaf: UInt?,
        val createdAt: String,
    ) : IncomingChatEvent {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is PushRoutingSecret) return false
            return messageId == other.messageId &&
                secret.contentEquals(other.secret) &&
                mintEpoch == other.mintEpoch &&
                senderLeaf == other.senderLeaf &&
                createdAt == other.createdAt
        }

        override fun hashCode(): Int {
            var result = messageId.hashCode()
            result = 31 * result + secret.contentHashCode()
            result = 31 * result + mintEpoch.hashCode()
            result = 31 * result + (senderLeaf?.hashCode() ?: 0)
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
