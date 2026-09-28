// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

/**
 * Cross-device sync state types (Phase 2a3e2b-Android).
 *
 * Mirror of:
 *
 *   * `apps/web/src/devices/sync/state.ts`
 *   * `apps/ios/Lakko/Devices/Sync/DeviceGroupSyncState.swift`
 *
 * Four record shapes — one per typed body variant in
 * `schemas/mls-app/devicegroup_v1.proto` — plus the per-record
 * `updatedAtHlc`. Since ADR-0058 these are read-only views the
 * repository derives from the Rust `SyncedState` fold; nothing on this
 * side decides which write wins any more.
 */

/** Per-group composer draft. Empty `text` clears the
 *  receivers' local draft for the same group. */
public data class DraftState(
    val text: String,
    val replyToId: ByteArray,
    val updatedAtHlc: ULong,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is DraftState) return false
        return text == other.text &&
            replyToId.contentEquals(other.replyToId) &&
            updatedAtHlc == other.updatedAtHlc
    }

    override fun hashCode(): Int {
        var result = text.hashCode()
        result = 31 * result + replyToId.contentHashCode()
        result = 31 * result + updatedAtHlc.hashCode()
        return result
    }
}

/** Per-group UI flags. Whole-snapshot LWW. */
public data class FlagsState(
    val pinned: Boolean,
    val muted: Boolean,
    val archived: Boolean,
    val unreadOverride: Boolean,
    val updatedAtHlc: ULong,
)

/** Per-group read cursor. */
public data class ReadCursorState(
    val upToMessageId: ByteArray,
    val readAtMs: ULong,
    val updatedAtHlc: ULong,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is ReadCursorState) return false
        return upToMessageId.contentEquals(other.upToMessageId) &&
            readAtMs == other.readAtMs &&
            updatedAtHlc == other.updatedAtHlc
    }

    override fun hashCode(): Int {
        var result = upToMessageId.contentHashCode()
        result = 31 * result + readAtMs.hashCode()
        result = 31 * result + updatedAtHlc.hashCode()
        return result
    }
}

/** Whole-account explicit conversation ordering. */
public data class OrderState(
    val groupIds: List<ByteArray>,
    val updatedAtHlc: ULong,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is OrderState) return false
        if (groupIds.size != other.groupIds.size) return false
        for (i in groupIds.indices) {
            if (!groupIds[i].contentEquals(other.groupIds[i])) return false
        }
        return updatedAtHlc == other.updatedAtHlc
    }

    override fun hashCode(): Int {
        var result = groupIds.fold(0) { acc, b -> acc * 31 + b.contentHashCode() }
        result = 31 * result + updatedAtHlc.hashCode()
        return result
    }
}

/** Hex-encode a `group_id` byte slice for map key use.
 *  Lowercase, no `0x` prefix. */
public fun groupIdHexOf(bytes: ByteArray): String {
    val sb = StringBuilder(bytes.size * 2)
    for (b in bytes) {
        val v = b.toInt() and 0xFF
        sb.append(HEX_CHARS[v ushr 4])
        sb.append(HEX_CHARS[v and 0x0F])
    }
    return sb.toString()
}

private const val HEX_CHARS = "0123456789abcdef"
