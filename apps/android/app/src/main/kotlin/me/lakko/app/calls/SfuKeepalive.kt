// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/*
 * Application-layer keepalive for the SFU signaling socket.
 *
 * `SfuSignalingClient` has always been able to send a Ping and receive a Pong,
 * but nothing drove it, so a dead SFU link was undetectable from the phone:
 * the call timer kept counting on a socket nobody answered. The session now
 * pings on a fixed cadence and declares the link dead once the server has
 * been silent for three of them. The rule is kept pure so it is unit-tested
 * and shared word-for-word with the iOS and web clients; the names and the
 * numbers are the same on all three.
 */

/** Chosen, not measured: three missed pings before the link is declared dead. */
public const val SFU_KEEPALIVE_PING_INTERVAL_MS: Long = 15_000L
public const val SFU_KEEPALIVE_DEADLINE_MS: Long = 45_000L

/** How often the session re-evaluates; coarse on purpose. */
public const val SFU_KEEPALIVE_TICK_MS: Long = 5_000L

public enum class SfuKeepaliveAction { PING, WAIT, DEAD }

/**
 * `lastInboundMs` is the last time the server proved it is there (the Welcome
 * frame or a Pong); `lastPingMs` the last time we asked. DEAD wins over PING.
 */
public fun sfuKeepaliveAction(nowMs: Long, lastInboundMs: Long, lastPingMs: Long): SfuKeepaliveAction =
    when {
        nowMs - lastInboundMs >= SFU_KEEPALIVE_DEADLINE_MS -> SfuKeepaliveAction.DEAD
        nowMs - lastPingMs >= SFU_KEEPALIVE_PING_INTERVAL_MS -> SfuKeepaliveAction.PING
        else -> SfuKeepaliveAction.WAIT
    }

/**
 * When a call being recorded actually ended.
 *
 * [lastInboundMs] is the last time the server proved it was there. A device
 * that notices the end only after the deadline has passed since that proof was
 * asleep, or its link died and the tick ran late; either way the call ended
 * when the link did, not when the device woke up. Two devices left in a call
 * overnight each wrote an `8:20:53` row the next morning at the moment the
 * laptop woke. Shared word-for-word with web and iOS. A [lastInboundMs] of 0
 * means the link never came up.
 */
public fun callEndedAtMs(nowMs: Long, lastInboundMs: Long): Long {
    if (lastInboundMs <= 0L) return nowMs
    return if (nowMs - lastInboundMs > SFU_KEEPALIVE_DEADLINE_MS) lastInboundMs else nowMs
}
