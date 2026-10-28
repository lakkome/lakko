// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import uniffi.lakko_ffi_uniffi.FfiPushWakeKind
import uniffi.lakko_ffi_uniffi.pushWakeHeaderValue

/**
 * ADR-0054 decision 1 / ADR-0078 section 3 — the header that asks the DS to
 * wake a group's recipients once the signed publish it rides on has been
 * appended.
 *
 * Since ADR-0078 the value is a wake **sealed to push-gw's X25519 key**: the
 * group's `push_gid`, the kind, a nonce, a timestamp, the group's unlock key
 * and a signature by the group's wake key are all inside, as base64url
 * without padding ([DsWake.HEADER_VALUE_LEN] characters). The DS checks the
 * shape and forwards it unread, so it no longer learns which group is woken
 * or whether the wake is a call.
 */
public const val DS_WAKE_HEADER: String = "x-lakko-wake"

/** The two wake classes, sealed inside the header. A control envelope
 *  (beacon, typing, receipt, routing secret) sends no header at all. */
public enum class DsWakeKind {
    /** An ordinary chat message. */
    MESSAGE,

    /** A call offer or end: push-gw raises the delivery priority and shortens
     *  the TTL, because a ring is worthless once the call is over. */
    CALL,
}

/** Seals one wake; the default is the core's `pushWakeHeaderValue`. A
 *  parameter only so a host JVM test (no native core) can supply its own. */
public fun interface PushWakeSealer {
    /** The header value for [secret]'s group, sealed to [gwKey]. */
    public fun seal(gwKey: ByteArray, secret: ByteArray, kind: DsWakeKind, timestampMs: Long): String
}

/**
 * One wake request: the sealed header value.
 *
 * The DS answers `400` to a malformed header, which would fail the SEND, so
 * the constructor refuses anything but [HEADER_VALUE_LEN] base64url
 * characters. Use [seal] from a send path: it answers `null` (no header, no
 * wake) instead of throwing.
 */
public class DsWake(private val value: String) {

    init {
        require(isWellFormed(value)) { "a wake header is $HEADER_VALUE_LEN base64url characters" }
    }

    /** The header value. */
    public fun headerValue(): String = value

    public companion object {
        /** The header value length (mirror of `PUSH_WAKE_HEADER_LEN`). */
        public const val HEADER_VALUE_LEN: Int = 312

        /** The core's sealer. */
        public val CORE_SEALER: PushWakeSealer = PushWakeSealer { gwKey, secret, kind, timestampMs ->
            pushWakeHeaderValue(
                gwKey,
                secret,
                if (kind == DsWakeKind.CALL) FfiPushWakeKind.CALL else FfiPushWakeKind.MESSAGE,
                timestampMs.coerceAtLeast(0).toULong(),
            )
        }

        /** Whether [value] is the shape the DS accepts. */
        public fun isWellFormed(value: String): Boolean =
            value.length == HEADER_VALUE_LEN &&
                value.all { it in 'A'..'Z' || it in 'a'..'z' || it in '0'..'9' || it == '-' || it == '_' }

        /**
         * The wake a publish should carry, or `null` for none: a sealing
         * failure or a malformed result fails toward silence, never toward a
         * refused send.
         */
        public fun seal(
            gwKey: ByteArray,
            secret: ByteArray,
            kind: DsWakeKind,
            timestampMs: Long,
            sealer: PushWakeSealer = CORE_SEALER,
        ): DsWake? {
            val value = try {
                sealer.seal(gwKey, secret, kind, timestampMs)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (_: Throwable) {
                return null
            }
            return if (isWellFormed(value)) DsWake(value) else null
        }
    }
}
