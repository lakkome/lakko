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
    static func action(nowMs: Int64, lastInboundMs: Int64, lastPingMs: Int64) -> Action {
        if nowMs &- lastInboundMs >= deadlineMs { return .dead }
        if nowMs &- lastPingMs >= pingIntervalMs { return .ping }
        return .wait
    }
}
