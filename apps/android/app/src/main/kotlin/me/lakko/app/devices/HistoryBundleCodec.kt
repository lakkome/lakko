// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import me.lakko.app.chat.toAttachmentRef
import android.content.Context
import android.util.Base64
import android.util.Log
import me.lakko.app.chat.MessageLogPayload
import me.lakko.app.chat.MessageLogStore
import me.lakko.app.chat.PersistedAttachment
import me.lakko.app.chat.PersistedCall
import me.lakko.app.chat.PersistedForward
import me.lakko.app.chat.PersistedGroup
import me.lakko.app.chat.PersistedMessage
import me.lakko.app.chat.PersistedReply
import me.lakko.app.chat.PersistedThumb
import me.lakko.app.chat.bundleFloor
import me.lakko.app.chat.higherFloor
import me.lakko.app.chat.toTranscriptEntry
import uniffi.lakko_ffi_uniffi.CallRecordOutcome
import uniffi.lakko_ffi_uniffi.HistoryAttachment
import uniffi.lakko_ffi_uniffi.HistoryCall
import uniffi.lakko_ffi_uniffi.HistoryForward
import uniffi.lakko_ffi_uniffi.HistoryMessageDirection
import uniffi.lakko_ffi_uniffi.HistoryPlaceholder
import uniffi.lakko_ffi_uniffi.HistoryReply
import uniffi.lakko_ffi_uniffi.HistoryThumb
import uniffi.lakko_ffi_uniffi.StructuredHistoryGroup
import uniffi.lakko_ffi_uniffi.StructuredHistoryMessage
import uniffi.lakko_ffi_uniffi.decodeHistoryPlaceholder
import uniffi.lakko_ffi_uniffi.encodeHistoryPlaceholder
import uniffi.lakko_ffi_uniffi.openHistoryBundleStructured
import uniffi.lakko_ffi_uniffi.sealHistoryBundleStructured

/**
 * History catch-up bundle helpers on Android (Phase 2a3d8-ui).
 *
 * Mirror of:
 *
 *   * `apps/web/src/devices/history-bundle.ts`
 *   * `apps/ios/Lakko/Devices/HistoryBundleCodec.swift`
 *
 * Two pure functions wrap the structured UniFFI surface:
 *
 *   * [buildFromMessageLog] — scanner side. Reads the local
 *     persistent message log, projects rows into
 *     `StructuredHistoryGroup` / `StructuredHistoryMessage`
 *     Records, seals the bundle. Returns `null` when there's
 *     nothing to send.
 *
 *   * [applyToMessageLog] — joining side. Takes the sealed
 *     bytes, opens via the post-join exporter secret, projects
 *     each row back into the persistent message log.
 *     Idempotent on the `messageId` dedup.
 *
 * `payload_bytes` are an opaque JSON projection of the
 * runtime `PersistedMessage` shape — same trade-off as web /
 * iOS; a future sprint replaces the placeholder with true
 * MLS application-payload bytes once the message log starts
 * retaining them.
 */
public object HistoryBundleCodec {

    /** Hard cap on the total `HistoryMessage` count across
     *  every group in a single bundle — must match
     *  `lakko_mls::history_bundle::MAX_BUNDLE_MESSAGES`. */
    public const val MAX_BUNDLE_MESSAGES: Int = 1000

    private const val LOG_TAG = "HistoryBundleCodec"

    /**
     * Build the sealed history-bundle wire bytes from the
     * scanner device's local message log. Returns `null` when
     * there's nothing to send (no log on disk yet or empty
     * groups map).
     */
    public fun buildFromMessageLog(
        context: Context,
        accountIdB64: String,
        secret: ByteArray,
        /** Walk 3, W3-R3 — this account's own person key in a conversation
         *  (base64), by group id hex; `null` falls back to the key a sent
         *  outgoing row was stamped with. */
        ownAuthorKeyB64: (groupIdHex: String) -> String? = { null },
    ): ByteArray? {
        val log = MessageLogStore.getLog(context, accountIdB64) ?: return null
        if (log.groups.isEmpty()) return null

        val groupHexes = log.groups.keys.toList()
        val perGroupCap = maxOf(1, MAX_BUNDLE_MESSAGES / groupHexes.size)

        val groupInputs = mutableListOf<StructuredHistoryGroup>()
        var maxHlc: ULong = 0u

        for (hex in groupHexes) {
            val slot = log.groups[hex] ?: continue
            val groupIdBytes = hexToBytes(hex) ?: continue
            // The MLS bookkeeping rows (a commit, an external-join request)
            // are never carried: they name no people, and a receiver that
            // draws a commit row as "Members changed" showed one per device
            // commit. Same rule on web and iOS.
            val carried = bundledMessages(slot.messages)
            val trimmed = carried.takeLast(perGroupCap)
            val capped = trimmed.size < carried.size
            val messages = mutableListOf<StructuredHistoryMessage>()
            val ownKey = ownAuthorKeyB64(hex)
            for (m in trimmed) {
                // G-6 step 2: encode the canonical cross-platform
                // placeholder via the shared Rust core. Web / iOS
                // call the SAME codec, so the bytes agree on the
                // wire (replacing the Android-only kotlinx JSON
                // shape that did not interoperate with web).
                val placeholder = try {
                    encodePlaceholder(m, ownKey)
                } catch (_: Throwable) {
                    continue
                }
                val createdMs = parseIsoMs(m.createdAt)
                // `messageId` is the HLC-derived Long the chat
                // pipeline assigns; encode it big-endian for the
                // structured-history wire so the joining device
                // sees the same numeric value back.
                val idBytes = java.nio.ByteBuffer.allocate(8)
                    .order(java.nio.ByteOrder.BIG_ENDIAN)
                    .putLong(m.messageId)
                    .array()
                messages.add(
                    StructuredHistoryMessage(
                        payloadBytes = placeholder,
                        senderLeaf = 0u,
                        messageId = idBytes,
                        receivedAtMs = createdMs,
                    ),
                )
                if (m.messageId > 0 && m.messageId.toULong() > maxHlc) {
                    maxHlc = m.messageId.toULong()
                }
            }
            groupInputs.add(
                StructuredHistoryGroup(
                    groupId = groupIdBytes,
                    messages = messages,
                    countCapped = capped,
                ),
            )
        }
        if (groupInputs.isEmpty()) return null

        return sealHistoryBundleStructured(
            secret = secret,
            groups = groupInputs,
            maxHlc = maxHlc,
        )
    }

    /**
     * ADR-0091 phase 2 section 7 — the gap bundle a device that brings
     * another back sends: ONE conversation's rows from [fromMs] on (at most
     * [MAX_BUNDLE_MESSAGES], the newest kept), sealed by [seal] (the gap-key
     * seal `sealGapHistoryStructured`, signed by this device's leaf). `null`
     * when there is nothing in the gap.
     */
    public fun buildGroupSince(
        context: Context,
        accountIdB64: String,
        groupIdHex: String,
        fromMs: Long,
        /** Walk 3, W3-R3 — this account's own person key in the conversation
         *  (base64): the author an outgoing row carries. */
        ownAuthorKeyB64: String? = null,
        seal: (groups: List<StructuredHistoryGroup>, maxHlc: ULong) -> ByteArray,
    ): ByteArray? {
        val log = MessageLogStore.getLog(context, accountIdB64) ?: return null
        val slot = log.groups[groupIdHex] ?: log.groups[groupIdHex.lowercase()] ?: return null
        val groupIdBytes = hexToBytes(groupIdHex) ?: return null
        val carried = bundledMessages(slot.messages)
        val timeOf: (me.lakko.app.chat.PersistedMessage) -> Long? = { m -> parseIsoMs(m.createdAt).toLong().takeIf { it > 0 } }
        val (kept, cut) = me.lakko.app.chat.gapRows(carried, fromMs, MAX_BUNDLE_MESSAGES, timeOf)
        // Not everything of the gap (the receiver keeps its gap line): the
        // cap trimmed the rows, or this device holds no row older than the
        // gap's start, so it may have been away for part of it too. The same
        // rule as web `buildGapHistoryBundle` and iOS `buildForGroup`.
        val capped = me.lakko.app.chat.gapBundleIncomplete(carried, fromMs, cut, timeOf)
        if (kept.isEmpty()) return null
        var maxHlc: ULong = 0u
        val messages = kept.mapNotNull { m ->
            val placeholder = try {
                encodePlaceholder(m, ownAuthorKeyB64)
            } catch (_: Throwable) {
                return@mapNotNull null
            }
            if (m.messageId > 0 && m.messageId.toULong() > maxHlc) maxHlc = m.messageId.toULong()
            StructuredHistoryMessage(
                payloadBytes = placeholder,
                senderLeaf = 0u,
                messageId = java.nio.ByteBuffer.allocate(8)
                    .order(java.nio.ByteOrder.BIG_ENDIAN)
                    .putLong(m.messageId)
                    .array(),
                receivedAtMs = parseIsoMs(m.createdAt),
            )
        }
        if (messages.isEmpty()) return null
        return seal(
            listOf(StructuredHistoryGroup(groupId = groupIdBytes, messages = messages, countCapped = capped)),
            maxHlc,
        )
    }

    /**
     * ADR-0101 section 9 - one page of a conversation's history for a person
     * back after a key reset, newest first: the rows of [sourceSlotHex] (the
     * conversation's earlier group, whose DS ids are unique across journals
     * and older than any of the successor's) with a DS id below
     * [beforeMessageId] (0: the newest), at most [pageSize], sealed by [seal]
     * for [targetGroupIdHex] (the successor the person is in). An EMPTY page
     * (nothing older is held) is sealed too: it tells the requester to stop.
     * `null` when the source slot is not held or the target id is malformed.
     */
    public fun buildHistoryPage(
        context: Context,
        accountIdB64: String,
        sourceSlotHex: String,
        targetGroupIdHex: String,
        beforeMessageId: Long,
        pageSize: Int,
        ownAuthorKeyB64: String? = null,
        seal: (groups: List<StructuredHistoryGroup>, maxHlc: ULong) -> ByteArray,
    ): ByteArray? {
        val log = MessageLogStore.getLog(context, accountIdB64) ?: return null
        val slot = log.groups[sourceSlotHex] ?: log.groups[sourceSlotHex.lowercase()] ?: return null
        val targetBytes = hexToBytes(targetGroupIdHex) ?: return null
        val page = me.lakko.app.chat.historyPageRows(
            bundledMessages(slot.messages),
            { it.messageId },
            beforeMessageId,
            pageSize,
        )
        var maxHlc: ULong = 0u
        val messages = page.mapNotNull { m ->
            val placeholder = try {
                encodePlaceholder(m, ownAuthorKeyB64)
            } catch (_: Throwable) {
                return@mapNotNull null
            }
            if (m.messageId > 0 && m.messageId.toULong() > maxHlc) maxHlc = m.messageId.toULong()
            StructuredHistoryMessage(
                payloadBytes = placeholder,
                senderLeaf = 0u,
                messageId = java.nio.ByteBuffer.allocate(8)
                    .order(java.nio.ByteOrder.BIG_ENDIAN)
                    .putLong(m.messageId)
                    .array(),
                receivedAtMs = parseIsoMs(m.createdAt),
            )
        }
        return seal(
            listOf(StructuredHistoryGroup(groupId = targetBytes, messages = messages, countCapped = false)),
            maxHlc,
        )
    }

    /** ADR-0101 section 9 - the DS ids of the rows an opened page carries
     *  for [groupIdHex] (8-byte big-endian), what the requester's next
     *  `before` is computed from. */
    public fun historyPageMessageIds(
        opened: uniffi.lakko_ffi_uniffi.StructuredHistoryBundle,
        groupIdHex: String,
    ): List<Long> {
        val group = opened.groups.firstOrNull { bytesToHex(it.groupId).equals(groupIdHex, ignoreCase = true) }
            ?: return emptyList()
        return group.messages.mapNotNull { m ->
            m.messageId.takeIf { it.size == 8 }?.let { java.nio.ByteBuffer.wrap(it).order(java.nio.ByteOrder.BIG_ENDIAN).long }
        }
    }

    /** What [applyReentryBundle] took in. */
    public class ReentryBundleApplied(
        /** Rows added to the log. */
        public val applied: Int,
        /** The bundle rows' times (ms), for the gap line. */
        public val rowTimesMs: List<Long>,
        /** The sender cut the bundle to its size cap. */
        public val capped: Boolean,
    )

    /**
     * ADR-0091 phase 2 section 7 — take in an opened gap bundle (its signer
     * already accepted by the joined group) for [groupIdHex] only (any other
     * group in it is ignored). [adapt] makes each row this
     * device's: given the decoded row (the SENDER's direction) and the
     * author the placeholder names (walk 3, W3-R3; base64, `null` when it
     * names none), it sets the author and the direction
     * ([me.lakko.app.chat.reentryBundleRow]), or returns `null` to leave the
     * row out ([me.lakko.app.chat.reentryBundleKeepsRow]).
     */
    public fun applyReentryBundle(
        context: Context,
        accountIdB64: String,
        opened: uniffi.lakko_ffi_uniffi.StructuredHistoryBundle,
        groupIdHex: String,
        adapt: (row: PersistedMessage, authorKeyB64: String?) -> PersistedMessage?,
    ): ReentryBundleApplied {
        val group = opened.groups.firstOrNull { bytesToHex(it.groupId).equals(groupIdHex, ignoreCase = true) }
            ?: return ReentryBundleApplied(0, emptyList(), false)
        val rows = group.messages.mapNotNull { m ->
            try {
                val placeholder = decodeHistoryPlaceholder(m.payloadBytes)
                adapt(persistedOf(placeholder), authorKeyB64Of(placeholder))
            } catch (cause: Throwable) {
                Log.v(LOG_TAG, "decode placeholder failed", cause)
                null
            }
        }
        val merged = writeBundle(context, accountIdB64, mapOf(groupIdHex.lowercase() to rows))
        liveLogSink?.invoke(accountIdB64, merged.opened)
        return ReentryBundleApplied(
            applied = merged.applied,
            rowTimesMs = rows.mapNotNull { parseIsoMs(it.createdAt).toLong().takeIf { t -> t > 0 } },
            capped = group.countCapped,
        )
    }

    /**
     * Open the sealed bytes + project rows into the joining
     * device's persistent message log. Returns the number of
     * rows applied across all groups.
     */
    public fun applyToMessageLog(
        context: Context,
        accountIdB64: String,
        secret: ByteArray,
        sealedBytes: ByteArray,
    ): Int {
        val opened = openHistoryBundleStructured(
            secret = secret,
            sealedBytes = sealedBytes,
        )
        val decoded = opened.groups.associate { g ->
            bytesToHex(g.groupId) to g.messages.mapNotNull { m ->
                // G-6 step 2: decode the canonical cross-platform
                // placeholder via the shared Rust core. Bytes from
                // a web or iOS scanner decode identically here.
                try {
                    decodePlaceholder(m.payloadBytes)
                } catch (cause: Throwable) {
                    Log.v(LOG_TAG, "decode placeholder failed", cause)
                    null
                }
            }
        }
        val merged = writeBundle(context, accountIdB64, decoded)
        // A chat layer already running keeps the whole log in memory and
        // rewrites it from there, so it has to be told what just landed.
        liveLogSink?.invoke(accountIdB64, merged.opened)
        return merged.applied
    }

    /**
     * Write a bundle's [decoded] rows into the account's message log, and the
     * call rows among them into the Calls list at the same moment
     * (`docs/ux/navigation.md` section 6.2: the list is an index written when
     * the call row is). Whether or not a chat layer runs: a bundle that
     * reaches only the disk still files its calls.
     */
    private fun writeBundle(
        context: Context,
        accountIdB64: String,
        decoded: Map<String, List<PersistedMessage>>,
    ): MergedBundle = writeBundleTo(
        existing = MessageLogStore.getLog(context, accountIdB64),
        decoded = decoded,
        putLog = { MessageLogStore.putLog(context = context, accountIdB64 = accountIdB64, payload = it) },
        fileCalls = { me.lakko.app.calls.CallHistoryStore.fileCallRows(context, accountIdB64, it) },
        kindOf = { hex -> conversationKindOf?.invoke(hex) },
    )

    /** [writeBundle] with its two stores passed in. The log is written
     *  first, then the bundle's call rows are filed ([bundleCallEntries]);
     *  the store files each key once, so a bundle written twice files
     *  nothing more. */
    internal fun writeBundleTo(
        existing: MessageLogPayload?,
        decoded: Map<String, List<PersistedMessage>>,
        putLog: (MessageLogPayload) -> Unit,
        fileCalls: (List<Pair<String, me.lakko.app.calls.CallHistoryEntry>>) -> Unit,
        kindOf: (String) -> me.lakko.app.chat.ConversationKind?,
    ): MergedBundle {
        val held = existing ?: MessageLogPayload(version = 1, groups = emptyMap())
        val merged = mergeBundle(held.groups, decoded)
        putLog(MessageLogPayload(version = held.version, groups = merged.groups))
        val calls = bundleCallEntries(merged.opened.mapValues { it.value.messages }, kindOf)
        if (calls.isNotEmpty()) fileCalls(calls)
        return merged
    }

    /**
     * The Calls list entries for the call rows of a bundle, each keyed by its
     * conversation and row id ([me.lakko.app.calls.callRowFiledKey]) so it is
     * filed once. Its time is the row's; its kind the group's when this
     * device holds the group, else a group (the Calls screen reads the kind
     * from the group itself once it is held).
     */
    internal fun bundleCallEntries(
        rowsByGroup: Map<String, List<PersistedMessage>>,
        kindOf: (String) -> me.lakko.app.chat.ConversationKind?,
    ): List<Pair<String, me.lakko.app.calls.CallHistoryEntry>> {
        val out = mutableListOf<Pair<String, me.lakko.app.calls.CallHistoryEntry>>()
        for ((hex, rows) in rowsByGroup) {
            val calls = rows.mapNotNull { row ->
                val call = row.call?.toTranscriptEntry() ?: return@mapNotNull null
                val at = me.lakko.app.ui.chat.parseMessageTimeMs(row.createdAt) ?: return@mapNotNull null
                Triple(row.messageId, call, at)
            }
            if (calls.isEmpty()) continue
            val kind = kindOf(hex) ?: me.lakko.app.chat.ConversationKind.Group
            for ((rowId, call, at) in calls) {
                out += me.lakko.app.calls.callRowFiledKey(hex, rowId) to me.lakko.app.calls.CallHistoryEntry(
                    conversationIdHex = hex.lowercase(),
                    kind = kind,
                    outcome = call.outcome,
                    video = call.video,
                    startedAtMs = at,
                    durationSecs = call.durationSecs,
                )
            }
        }
        return out
    }

    /** The kind of a conversation this device holds (set by the running chat
     *  layer), `null` when none runs or it does not hold the group. */
    @Volatile
    public var conversationKindOf: ((groupIdHex: String) -> me.lakko.app.chat.ConversationKind?)? = null

    /**
     * Where a bundle goes when the chat layer of its account is running
     * (a device pairing while signed in). `null`, or a chat layer on another
     * account: the next bootstrap reads it off the log.
     */
    @Volatile
    public var liveLogSink: ((accountIdB64: String, opened: Map<String, PersistedGroup>) -> Unit)? = null

    /** What [mergeBundle] made of one bundle. */
    internal class MergedBundle(
        /** The whole log after the merge. */
        val groups: Map<String, PersistedGroup>,
        /** How many rows the merge added across all groups. */
        val applied: Int,
        /** Per group, the bundle's own rows and floor: what a running chat
         *  layer takes in ([liveLogSink]). */
        val opened: Map<String, PersistedGroup>,
    )

    /**
     * Merge a bundle's [decoded] rows, group by group, into the log [held] on
     * this device. `copy`, not a new slot: the feed's seen marks (ADR-0063
     * section 5) ride the same slot and must survive the merge. Each group's
     * floor moves up to the highest id the bundle carried for it, and never
     * down ("History is not news", `docs/ux/second-device.md` section 2).
     */
    internal fun mergeBundle(
        held: Map<String, PersistedGroup>,
        decoded: Map<String, List<PersistedMessage>>,
    ): MergedBundle {
        val next = held.toMutableMap()
        val opened = LinkedHashMap<String, PersistedGroup>()
        var applied = 0
        for ((hex, rows) in decoded) {
            val slot = next[hex] ?: PersistedGroup(cursor = 0L, messages = emptyList())
            val (merged, added) = mergeRows(slot.messages, rows)
            applied += added
            val floor = bundleFloor(rows)
            next[hex] = slot.copy(
                messages = merged,
                syncedThroughMessageId = higherFloor(slot.syncedThroughMessageId, floor),
            )
            opened[hex] = PersistedGroup(
                cursor = 0L,
                messages = mergeRows(emptyList(), rows).first,
                syncedThroughMessageId = floor,
            )
        }
        return MergedBundle(next, applied, opened)
    }

    /**
     * The rows of one group after a bundle's [incoming] rows are merged into
     * the [held] ones ([me.lakko.app.chat.placeBundleRows]: server rows by
     * id, call rows by time), and how many were added. A
     * bookkeeping row from a sender that still carries them is dropped (it is
     * nothing the person reads), and a message id already held is kept as
     * held.
     */
    internal fun mergeRows(held: List<PersistedMessage>, incoming: List<PersistedMessage>): Pair<List<PersistedMessage>, Int> {
        val ids = held.map { it.messageId }.toMutableSet()
        val fresh = mutableListOf<PersistedMessage>()
        for (row in incoming) {
            if (isBookkeepingSystemRow(row)) continue
            if (!ids.add(row.messageId)) continue
            fresh += row
        }
        // The same placement as the rows in memory ([me.lakko.app.chat.mergeHistoryRows]):
        // a call row lands at its time, not first by its negative id.
        val merged = me.lakko.app.chat.placeBundleRows(
            held = held,
            fresh = fresh,
            idOf = { it.messageId },
            timeOf = { me.lakko.app.ui.chat.parseMessageTimeMs(it.createdAt) },
            settled = { true },
        )
        return merged to fresh.size
    }

    /** A system row the receive loop files for an MLS commit or an
     *  external-join request (`[mls commit]`, `[mls external join]`, the same
     *  `[mls <kind>]` text on every client). A call row is a system row too
     *  and is not one of these. */
    internal fun isBookkeepingSystemRow(m: PersistedMessage): Boolean =
        m.direction == "system" && m.call == null && m.text.startsWith("[mls ") && m.text.endsWith("]")

    /** The rows of a group a history bundle carries: every row but the
     *  bookkeeping ones. */
    internal fun bundledMessages(messages: List<PersistedMessage>): List<PersistedMessage> =
        messages.filterNot(::isBookkeepingSystemRow)

    // ----------------------------------------------------------------
    // Placeholder codec (G-6 step 2)
    // ----------------------------------------------------------------

    /** Encode a local-store message row into the canonical
     *  cross-platform placeholder bytes via the shared Rust core. */
    private fun encodePlaceholder(m: PersistedMessage, ownAuthorKeyB64: String?): ByteArray =
        encodeHistoryPlaceholder(placeholderOf(m, ownAuthorKeyB64))

    /**
     * Walk 3, W3-R3 — the author a bundled row carries (placeholder version
     * 6): an incoming row its stored author key (the key the live row was
     * attributed to), an outgoing row this account's own key in the
     * conversation ([ownAuthorKeyB64], else the key a sent row was stamped
     * with), a system row none. Anything that is not a 32-byte key is none.
     */
    internal fun bundleAuthorKey(m: PersistedMessage, ownAuthorKeyB64: String?): ByteArray? {
        val b64 = when (m.direction) {
            "incoming" -> m.authorKeyB64
            "outgoing" -> ownAuthorKeyB64 ?: m.authorKeyB64
            else -> null
        } ?: return null
        val bytes = try {
            java.util.Base64.getDecoder().decode(b64)
        } catch (_: IllegalArgumentException) {
            return null
        }
        return bytes.takeIf { it.size == AUTHOR_KEY_LEN }
    }

    /** The author a decoded placeholder names, base64 (the form a live row
     *  stores), or `null`. */
    internal fun authorKeyB64Of(placeholder: HistoryPlaceholder): String? =
        placeholder.authorKey?.takeIf { it.size == AUTHOR_KEY_LEN }
            ?.let { java.util.Base64.getEncoder().encodeToString(it) }

    private const val AUTHOR_KEY_LEN: Int = 32

    /**
     * A local-store message row as the placeholder record. Version 3 also
     * carries the ADR-0087 channel invitation card (without it the new device
     * drew an invitation as its bare `/g/` link, seen live 2026-11-13) and
     * the ADR-0052 reply and forward. A deleted row carries no card.
     */
    internal fun placeholderOf(m: PersistedMessage, ownAuthorKeyB64: String? = null): HistoryPlaceholder {
        // ADR-0047 §5 — placeholder version 2 carries the media metadata, so a
        // voice message, video note or GIF restores as itself on the new
        // device. The core applies the envelope's bounds on encode and decode.
        // The local `played` marker is never transferred.
        val attachments = m.attachments.map { a ->
            HistoryAttachment(
                blobId = Base64.decode(a.blobIdB64, Base64.NO_WRAP),
                key = Base64.decode(a.keyB64, Base64.NO_WRAP),
                nonce = Base64.decode(a.nonceB64, Base64.NO_WRAP),
                size = a.size,
                mime = a.mime,
                name = a.name,
                kind = a.kind,
                durationMs = a.durationMs,
                waveform = a.waveformB64?.let { runCatching { Base64.decode(it, Base64.NO_WRAP) }.getOrNull() },
                width = a.width,
                height = a.height,
                thumb = a.thumb?.let(::historyThumb),
                inlineThumb = a.inlineThumbB64?.let { runCatching { Base64.decode(it, Base64.NO_WRAP) }.getOrNull() }
                    ?.let { me.lakko.app.media.inlineThumbOrNull(it) },
            )
        }
        val card = if (m.deleted) {
            null
        } else {
            m.channelInviteB64?.let { runCatching { java.util.Base64.getDecoder().decode(it) }.getOrNull() }
                ?.takeIf { it.isNotEmpty() }
        }
        val forward = m.forward?.let { f ->
            val origin = f.originGroupIdHex?.let(::hexToBytes)?.takeIf { f.originMessageId != null }
            HistoryForward(
                author = f.author,
                originGroupId = origin,
                originMessageId = if (origin == null) null else f.originMessageId,
            )
        }
        return HistoryPlaceholder(
            messageId = m.messageId.toULong(),
            direction = directionFromString(m.direction),
            createdAt = m.createdAt,
            text = m.text,
            attachments = attachments,
            channelInvite = card,
            reply = m.reply?.let { HistoryReply(target = it.target, author = it.author, excerpt = it.excerpt) },
            forward = forward,
            // ADR-0092 section 3.11 — a content row carries its content and
            // folded state, so the new device draws the card, not a blank.
            content = me.lakko.app.chat.historyContentOf(m),
            authorKey = bundleAuthorKey(m, ownAuthorKeyB64),
            // Placeholder version 7 — a finished call travels as itself, so
            // the new device draws the same call row (and files it in the
            // Calls list). Before this the row reached the new device as its
            // bare text, or not at all.
            call = m.call?.let(::historyCallOf),
        )
    }

    /**
     * A stored call record as the placeholder's call; `null` for an outcome
     * this build does not know (the row then travels as its text alone).
     * A duration outside `UInt` is dropped rather than wrapped; the core
     * drops it too on any outcome but outgoing and incoming.
     */
    internal fun historyCallOf(c: PersistedCall): HistoryCall? {
        val outcome = CallRecordOutcome.entries.firstOrNull { it.name.equals(c.outcome, ignoreCase = true) }
            ?: return null
        val duration = c.durationSecs?.takeIf { it in 0L..UInt.MAX_VALUE.toLong() }?.toUInt()
        return HistoryCall(outcome = outcome, video = c.video, durationSecs = duration)
    }

    /** A placeholder's call as the stored record: the outcome by its name,
     *  as [me.lakko.app.chat.PersistedCall.outcome] stores a
     *  [me.lakko.app.calls.CallOutcome]. */
    internal fun persistedCallOf(c: HistoryCall): PersistedCall = PersistedCall(
        outcome = c.outcome.name.lowercase(java.util.Locale.ROOT),
        video = c.video,
        durationSecs = c.durationSecs?.toLong(),
    )

    /** Decode canonical placeholder bytes (from any platform) back
     *  into a local-store message row. Throws on malformed input;
     *  callers wrap in try/catch. */
    private fun decodePlaceholder(bytes: ByteArray): PersistedMessage = persistedOf(decodeHistoryPlaceholder(bytes))

    /** A decoded placeholder as the local-store row, the invitation card and
     *  the reply and forward included. */
    internal fun persistedOf(placeholder: HistoryPlaceholder): PersistedMessage {
        placeholder.call?.let { return callRowOf(placeholder, it) }
        val attachments = placeholder.attachments.map { a ->
            PersistedAttachment(
                blobIdB64 = Base64.encodeToString(a.blobId, Base64.NO_WRAP),
                keyB64 = Base64.encodeToString(a.key, Base64.NO_WRAP),
                nonceB64 = Base64.encodeToString(a.nonce, Base64.NO_WRAP),
                mime = a.mime,
                size = a.size,
                name = a.name,
                kind = a.kind,
                durationMs = a.durationMs,
                waveformB64 = a.waveform?.takeIf { it.isNotEmpty() }?.let { Base64.encodeToString(it, Base64.NO_WRAP) },
                width = a.width,
                height = a.height,
                thumb = a.thumb?.let { t ->
                    PersistedThumb(
                        blobIdB64 = Base64.encodeToString(t.blobId, Base64.NO_WRAP),
                        keyB64 = Base64.encodeToString(t.key, Base64.NO_WRAP),
                        nonceB64 = Base64.encodeToString(t.nonce, Base64.NO_WRAP),
                        mime = t.mime,
                        size = t.size,
                        width = t.width,
                        height = t.height,
                    )
                },
                inlineThumbB64 = me.lakko.app.media.inlineThumbOrNull(a.inlineThumb)
                    ?.let { Base64.encodeToString(it, Base64.NO_WRAP) },
            )
        }
        return PersistedMessage(
            messageId = placeholder.messageId.toLong(),
            direction = directionToString(placeholder.direction),
            text = placeholder.text,
            attachments = attachments,
            createdAt = placeholder.createdAt,
            content = placeholder.content?.let { c ->
                me.lakko.app.chat.persistedContentOf(c, attachments.mapNotNull { it.toAttachmentRef() })
            },
            reply = placeholder.reply?.let { PersistedReply(target = it.target, author = it.author, excerpt = it.excerpt) },
            forward = placeholder.forward?.let {
                PersistedForward(
                    author = it.author,
                    originGroupIdHex = it.originGroupId?.let(::bytesToHex),
                    originMessageId = it.originMessageId,
                )
            },
            channelInviteB64 = placeholder.channelInvite?.takeIf { it.isNotEmpty() }
                ?.let { java.util.Base64.getEncoder().encodeToString(it) },
            // Walk 3, W3-R3 — an incoming row names its author, so it renders
            // with the author's name and avatar like a live row. (A gap
            // bundle's rows are re-attributed by [applyReentryBundle].)
            authorKeyB64 = if (placeholder.direction == HistoryMessageDirection.INCOMING) {
                authorKeyB64Of(placeholder)
            } else {
                null
            },
        )
    }

    /**
     * A placeholder that carries a call record is a call row, whatever its
     * text and direction say: web sends one with empty text, and another
     * build may say anything. The row is rebuilt from the record alone, as
     * [me.lakko.app.chat.ChatRepository.recordCallRow] writes a live one: a
     * system row naming nobody, its words in this device's interface
     * language, nothing else carried. The message id stays the sender's, so a
     * bundle applied twice adds the row once ([mergeRows]).
     */
    private fun callRowOf(placeholder: HistoryPlaceholder, call: HistoryCall): PersistedMessage {
        val stored = persistedCallOf(call)
        val text = stored.toTranscriptEntry()?.let { me.lakko.app.calls.callRowText(it) } ?: ""
        return PersistedMessage(
            messageId = placeholder.messageId.toLong(),
            direction = "system",
            text = text,
            attachments = emptyList(),
            createdAt = placeholder.createdAt,
            call = stored,
        )
    }

    /** A stored poster frame as the placeholder's thumb; `null` when a byte
     *  field no longer decodes (the attachment itself survives). */
    private fun historyThumb(t: PersistedThumb): HistoryThumb? {
        val blobId = runCatching { Base64.decode(t.blobIdB64, Base64.NO_WRAP) }.getOrNull()
        val key = runCatching { Base64.decode(t.keyB64, Base64.NO_WRAP) }.getOrNull()
        val nonce = runCatching { Base64.decode(t.nonceB64, Base64.NO_WRAP) }.getOrNull()
        if (blobId?.size != 32 || key?.size != 32 || nonce?.size != 12) return null
        return HistoryThumb(
            blobId = blobId,
            key = key,
            nonce = nonce,
            size = t.size,
            mime = t.mime,
            width = t.width,
            height = t.height,
        )
    }

    private fun directionFromString(s: String): HistoryMessageDirection = when (s) {
        "outgoing" -> HistoryMessageDirection.OUTGOING
        "incoming" -> HistoryMessageDirection.INCOMING
        else -> HistoryMessageDirection.SYSTEM
    }

    private fun directionToString(d: HistoryMessageDirection): String = when (d) {
        HistoryMessageDirection.OUTGOING -> "outgoing"
        HistoryMessageDirection.INCOMING -> "incoming"
        HistoryMessageDirection.SYSTEM -> "system"
    }

    // ----------------------------------------------------------------
    // Helpers
    // ----------------------------------------------------------------

    private fun parseIsoMs(iso: String): ULong {
        // ISO-8601 with optional fractional seconds + trailing
        // `Z`. SimpleDateFormat with `Locale.ROOT` is the
        // canonical Android parser; java.time.Instant is API
        // 26+ which our minSdk hits, but we keep the format
        // resilient to both `…Z` and `…+0000`.
        return try {
            java.time.Instant.parse(iso).toEpochMilli().toULong()
        } catch (_: Throwable) {
            0u
        }
    }

    private fun hexToBytes(hex: String): ByteArray? {
        if (hex.length % 2 != 0) return null
        val out = ByteArray(hex.length / 2)
        var i = 0
        while (i < hex.length) {
            val hi = Character.digit(hex[i], 16)
            val lo = Character.digit(hex[i + 1], 16)
            if (hi < 0 || lo < 0) return null
            out[i / 2] = ((hi shl 4) or lo).toByte()
            i += 2
        }
        return out
    }

    private fun bytesToHex(bytes: ByteArray): String {
        val sb = StringBuilder(bytes.size * 2)
        for (b in bytes) {
            val v = b.toInt() and 0xFF
            sb.append(HEX[v ushr 4])
            sb.append(HEX[v and 0x0F])
        }
        return sb.toString()
    }

    private const val HEX = "0123456789abcdef"
}
