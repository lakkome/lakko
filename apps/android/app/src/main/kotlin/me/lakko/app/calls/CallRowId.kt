// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/*
 * The message id of a call row (`docs/ux/calls.md` section 6).
 *
 * A call row is local: no server assigns its id. It used to take the next
 * small negative counter value (-1, -2, ...), which another device of the
 * account had already used for something else: a history bundle carrying the
 * row could be dropped as already held, and the same call recorded twice
 * became two rows. The id is now derived from the call itself, the same
 * scheme the web client uses, so every device that records or receives the
 * row names it alike:
 *
 *     message_id = 0x8000_0000_0000_0000 | first 8 bytes of the call id (big-endian)
 *
 * which is negative as a signed 64-bit value and never a server id.
 */

/** The top bit every call row id carries. */
private const val CALL_ROW_ID_BIT: Long = Long.MIN_VALUE

/**
 * Local rows that count down from zero (pending sends, local system rows)
 * stay above this floor; a call row id derived from a call id lies below it
 * except with probability 2^-31. [me.lakko.app.chat.nextLocalRowId] counts
 * only rows above it, so a call row's huge negative id never drags the
 * counter down (or past [Long.MIN_VALUE]).
 */
public const val LOCAL_COUNTER_FLOOR: Long = -(1L shl 32)

/** The call row id of the call [callIdHex] (lowercase or uppercase hex of at
 *  least 8 bytes), or `null` when it is not one. */
public fun callRowMessageId(callIdHex: String?): Long? {
    if (callIdHex == null || callIdHex.length < 16) return null
    var v = 0L
    for (i in 0 until 16) {
        val d = Character.digit(callIdHex[i], 16)
        if (d < 0) return null
        v = (v shl 4) or d.toLong()
    }
    return v or CALL_ROW_ID_BIT
}

/**
 * The call row id when no call id is known: a 63-bit hash of the conversation
 * and the call's start (else the row's time) with the top bit set, so it is
 * stable for one call and never a small counter value.
 */
public fun fallbackCallRowMessageId(groupIdHex: String, stableTime: String): Long {
    val digest = java.security.MessageDigest.getInstance("SHA-256")
        .digest("lakko/android/call-row/v1\u0000${groupIdHex.lowercase()}\u0000$stableTime".toByteArray())
    var v = 0L
    for (i in 0 until 8) v = (v shl 8) or (digest[i].toLong() and 0xff)
    return v or CALL_ROW_ID_BIT
}
