// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// A call with nothing arriving on it is over. The Swift twin of
/// `apps/web/src/calls/media-watchdog.ts`.
///
/// ## The defect this closes
///
/// A 1x1 call ran for **45 minutes** in the browser after the other side had
/// left it, counting time the whole way (2026-09-21). The transport-level
/// defences did not fire and could not have: the ICE state never left
/// `connected`, because both ends were on one machine and the candidate pair
/// stayed reachable after the peer stopped taking part.
///
/// That is the gap, and it is wider here than on the web. A peer connection
/// answers "can these two addresses still reach each other", which is not
/// "is there still a call here"; nothing in the session asked the second
/// question. On iOS nothing asked the first one either for a CONNECTED call:
/// `.disconnected` was ignored with a comment pointing at an upper-layer
/// watchdog, and that watchdog — the host's establish timer — does nothing
/// once the state is `.connected`.
///
/// ## The rule
///
/// Inbound RTP only ever climbs on a call that is happening: a muted
/// microphone still sends RTP, a camera turned off still leaves the audio
/// flowing, and a 1x1 call always carries an audio section both ways. A count
/// that has not moved for the whole window means nothing is arriving, and the
/// call ends the way a dead transport ends it.
///
/// ## Why NOT the candidate pair
///
/// The obvious meter is the selected pair's `bytesReceived`, and it is the
/// wrong one: it counts ICE consent checks, which a peer's ICE agent answers
/// every few seconds for as long as its process is alive, call or no call. A
/// watchdog metered on the pair would have watched the 45-minute call's
/// counter climb the whole way and called it healthy — the same blindness as
/// the ICE state, wearing a different hat.
///
/// ## What it does not cover
///
/// It only runs on a CONNECTED call. A call still being set up has not
/// received anything YET, which is a different thing from having stopped, and
/// is bounded by the host's establish timer instead: a peer who takes a minute
/// to answer is nowhere near this rule.
public enum MediaWatchdog {

    /// What the watchdog carries between samples.
    public struct InboundActivity: Equatable, Sendable {
        /// The last inbound RTP packet count, summed across every receiver.
        public let packets: Int64
        /// When that count last CHANGED, on the caller's monotonic clock.
        public let lastChangeAtSec: TimeInterval

        public init(packets: Int64, lastChangeAtSec: TimeInterval) {
            self.packets = packets
            self.lastChangeAtSec = lastChangeAtSec
        }
    }

    /// Fold one stats sample into the activity.
    ///
    /// A count that went BACKWARDS counts as activity rather than silence: it
    /// means the receivers were replaced (a renegotiation, an ICE restart),
    /// and reading a reset as silence would end a call that is working.
    public static func note(
        _ prev: InboundActivity?, packets: Int64, atSec: TimeInterval
    ) -> InboundActivity {
        guard let prev, packets == prev.packets else {
            return InboundActivity(packets: packets, lastChangeAtSec: atSec)
        }
        return prev
    }

    /// Whether the call has been silent long enough to be over.
    ///
    /// A `nil` activity is never silent — nothing has been measured yet, and a
    /// call that has not produced its first sample has not failed to produce a
    /// second. A timeout of zero or less disables the watchdog rather than
    /// ending every call instantly, so a caller can turn it off without a
    /// second flag.
    public static func hasGoneSilent(
        _ activity: InboundActivity?, atSec: TimeInterval, timeoutSec: TimeInterval
    ) -> Bool {
        guard let activity, timeoutSec > 0 else { return false }
        return atSec - activity.lastChangeAtSec >= timeoutSec
    }
}
