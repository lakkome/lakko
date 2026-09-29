// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.Context
import android.util.Base64
import android.util.Log
import me.lakko.app.chat.MessageLogPayload
import me.lakko.app.chat.MessageLogStore
import me.lakko.app.chat.PersistedAttachment
import me.lakko.app.chat.PersistedGroup
import me.lakko.app.chat.PersistedMessage
import me.lakko.app.chat.PersistedThumb
import uniffi.lakko_ffi_uniffi.HistoryAttachment
import uniffi.lakko_ffi_uniffi.HistoryMessageDirection
import uniffi.lakko_ffi_uniffi.HistoryPlaceholder
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
            val trimmed = slot.messages.takeLast(perGroupCap)
            val capped = trimmed.size < slot.messages.size
            val messages = mutableListOf<StructuredHistoryMessage>()
            for (m in trimmed) {
                // G-6 step 2: encode the canonical cross-platform
                // placeholder via the shared Rust core. Web / iOS
                // call the SAME codec, so the bytes agree on the
                // wire (replacing the Android-only kotlinx JSON
                // shape that did not interoperate with web).
                val placeholder = try {
                    encodePlaceholder(m)
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
        val existing = MessageLogStore.getLog(context, accountIdB64)
            ?: MessageLogPayload(version = 1, groups = emptyMap())

        val nextGroups = existing.groups.toMutableMap()
        var applied = 0

        for (g in opened.groups) {
            val hex = bytesToHex(g.groupId)
            val slot = nextGroups[hex]
                ?: PersistedGroup(cursor = 0L, messages = emptyList())
            val existingIds = slot.messages.map { it.messageId }.toMutableSet()
            val merged = slot.messages.toMutableList()
            for (m in g.messages) {
                // G-6 step 2: decode the canonical cross-platform
                // placeholder via the shared Rust core. Bytes from
                // a web or iOS scanner decode identically here.
                val decoded = try {
                    decodePlaceholder(m.payloadBytes)
                } catch (cause: Throwable) {
                    Log.v(LOG_TAG, "decode placeholder failed", cause)
                    continue
                }
                if (decoded.messageId in existingIds) continue
                merged.add(decoded)
                existingIds.add(decoded.messageId)
                applied += 1
            }
            // Sort by HLC-derived numeric messageId so the
            // timeline lands in order.
            merged.sortWith(
                Comparator { a, b ->
                    a.messageId.compareTo(b.messageId).also { _ ->
                        // Body intentionally trivial — kept as a
                        // Comparator { } block for symmetry with
                        // the sibling iOS implementation that
                        // exposes the same shape.
                    }
                },
            )
            // `copy`, not a new slot: the feed's seen marks (ADR-0058
            // section 5) ride the same slot and must survive the merge.
            nextGroups[hex] = slot.copy(messages = merged)
        }

        MessageLogStore.putLog(
            context = context,
            accountIdB64 = accountIdB64,
            payload = MessageLogPayload(
                version = existing.version,
                groups = nextGroups,
            ),
        )
        return applied
    }

    // ----------------------------------------------------------------
    // Placeholder codec (G-6 step 2)
    // ----------------------------------------------------------------

    /** Encode a local-store message row into the canonical
     *  cross-platform placeholder bytes via the shared Rust core. */
    private fun encodePlaceholder(m: PersistedMessage): ByteArray {
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
            )
        }
        val placeholder = HistoryPlaceholder(
            messageId = m.messageId.toULong(),
            direction = directionFromString(m.direction),
            createdAt = m.createdAt,
            text = m.text,
            attachments = attachments,
        )
        return encodeHistoryPlaceholder(placeholder)
    }

    /** Decode canonical placeholder bytes (from any platform) back
     *  into a local-store message row. Throws on malformed input;
     *  callers wrap in try/catch. */
    private fun decodePlaceholder(bytes: ByteArray): PersistedMessage {
        val placeholder = decodeHistoryPlaceholder(bytes)
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
            )
        }
        return PersistedMessage(
            messageId = placeholder.messageId.toLong(),
            direction = directionToString(placeholder.direction),
            text = placeholder.text,
            attachments = attachments,
            createdAt = placeholder.createdAt,
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
