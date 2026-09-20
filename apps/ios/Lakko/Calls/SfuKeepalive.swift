// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The application-layer keepalive of the SFU signaling socket.
///
/// `SfuSignalingClient` has had `sendPing` and `onPong` since the signaling
/// arc, and nothing ever called the first or subscribed to the second — so a
/// phone whose link to the call server died kept a call on screen with the
/// timer counting, on a socket nobody was answering. The rule here is what the
/// session evaluates on every tick; it is pure so its arithmetic can be pinned
/// without a socket. The same numbers and the same rule run on Android and web.
///
/// "Inbound" is the last time the server proved it was there: the Welcome
/// frame, or a Pong. The deadline wins over a due ping — a link past the
/// deadline is declared dead rather than pinged once more.
enum SfuKeepalive {
    /// Chosen, not measured: three missed pings before the link is declared
    /// dead.
    static let pingIntervalMs: Int64 = 15_000
    static let deadlineMs: Int64 = 45_000
    /// How often the session re-evaluates; coarse on purpose.
    static let tickMs: Int64 = 5_000

    enum Action: Equatable {
        case ping, wait, dead
    }

    /// - Parameters:
    ///   - nowMs: the instant of the tick.
    ///   - lastInboundMs: when the server last proved it was there.
    ///   - lastPingMs: when this device last asked.
    /// When a call being recorded actually ended.
    ///
    /// `lastInboundMs` is the last time the server proved it was there. A
    /// device that notices the end only after the deadline has passed since
    /// that proof was asleep, or its link died and the tick ran late; either
    /// way the call ended when the link did, not when the device woke up. Two
    /// devices left in a call overnight each wrote an `8:20:53` row the next
    /// morning at the moment the laptop woke. Shared word-for-word with web
    /// and Android. A `lastInboundMs` of 0 means the link never came up.
    static func callEndedAtMs(nowMs: Int64, lastInboundMs: Int64) -> Int64 {
        if lastInboundMs <= 0 { return nowMs }
        return nowMs - lastInboundMs > deadlineMs ? lastInboundMs : nowMs
    }

    static func action(nowMs: Int64, lastInboundMs: Int64, lastPingMs: Int64) -> Action {
        if nowMs &- lastInboundMs >= deadlineMs { return .dead }
        if nowMs &- lastPingMs >= pingIntervalMs { return .ping }
        return .wait
    }
}
