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
