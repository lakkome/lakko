// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import java.util.Base64

/**
 * ADR-0054 decision 1 — the header that asks the DS to wake a group's
 * recipients once the signed publish it rides on has been appended.
 *
 * Before ADR-0054 the sender's client called `POST /push/notify-group` on
 * push-gw after the publish, under a proof of work. push-gw is anonymous by
 * design, so anyone holding a `push_gid` could wake the group as often as the
 * proof of work allowed. The wake is now a consequence of a publish that has
 * already passed the ADR-0053 proof gate: the DS forwards
 * `(push_gid, message_id, kind)` to push-gw over the internal network, and the
 * client no longer talks to push-gw to wake anyone.
 */
public const val DS_WAKE_HEADER: String = "x-lakko-wake"

/** The two wake classes the DS accepts. A control envelope (beacon, typing,
 *  receipt, routing secret) sends no header at all, so it has no class. */
public enum class DsWakeKind(internal val wire: String) {
    /** An ordinary chat message. */
    MESSAGE("message"),

    /** A call offer or end: push-gw raises the delivery priority and shortens
     *  the TTL, because a ring is worthless once the call is over. */
    CALL("call"),
}

/**
 * One wake request: the group's 32-byte ADR-0042 `push_gid` and the class.
 *
 * The DS answers `400` to a malformed header, which would fail the SEND, so
 * this is the only place the header value is built and the constructor refuses
 * anything but a 32-byte id. Use [forPublish] from a send path: it answers
 * `null` (no header, no wake) instead of throwing.
 */
public class DsWake(pushGid: ByteArray, public val kind: DsWakeKind) {
    private val pushGid: ByteArray = pushGid.copyOf()

    init {
        require(pushGid.size == PUSH_GID_LEN) { "push_gid must be $PUSH_GID_LEN bytes" }
    }

    /** `<push_gid base64url, no padding>;<kind>` — 43 characters, a `;`, and
     *  `message` or `call`. */
    public fun headerValue(): String =
        Base64.getUrlEncoder().withoutPadding().encodeToString(pushGid) + ";" + kind.wire

    public companion object {
        /** ADR-0042 `push_gid` length (keyed-BLAKE3 output). */
        public const val PUSH_GID_LEN: Int = 32

        /**
         * The wake a publish should carry, or `null` for none.
         *
         * `null` when [kind] is `null` (a control envelope stays silent), when
         * [pushGid] is `null` (this device holds no routing secret yet: the
         * ADR-0042 defer; the group id is NEVER used in its place) or when it
         * is not 32 bytes (fail toward silence, never toward a refused send).
         */
        public fun forPublish(pushGid: ByteArray?, kind: DsWakeKind?): DsWake? {
            if (kind == null || pushGid == null || pushGid.size != PUSH_GID_LEN) return null
            return DsWake(pushGid, kind)
        }
    }
}
