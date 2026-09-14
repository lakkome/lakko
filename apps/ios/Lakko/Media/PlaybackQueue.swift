// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// UX spec §2 timeline bubble — "Playback continues to the next consecutive
/// unplayed voice message from the same sender." Mirror of
/// `apps/web/src/media/playback-queue.ts`.
///
/// "Consecutive" is read literally: from the message that just finished, walk
/// forward while each row is a voice message by the same sender. The first
/// unplayed one in that run plays next. Any other row — text, a voice message
/// from someone else, a video note — ends the run. An unknown sender never
/// matches, including another unknown one: two unattributed rows are not known
/// to share an author.
public enum PlaybackQueue {

    public struct Item: Equatable {
        /// Stable key the player addresses the item by.
        public let key: String
        public let isVoice: Bool
        /// Author identity (signature key), `nil` when unresolved.
        public let senderKey: String?
        public let played: Bool

        public init(key: String, isVoice: Bool, senderKey: String?, played: Bool) {
            self.key = key
            self.isVoice = isVoice
            self.senderKey = senderKey
            self.played = played
        }
    }

    public static func nextVoiceToPlay(_ items: [Item], finishedKey: String) -> String? {
        guard let index = items.firstIndex(where: { $0.key == finishedKey }),
              let sender = items[index].senderKey else {
            return nil
        }
        for item in items.dropFirst(index + 1) {
            guard item.isVoice, item.senderKey == sender else { return nil }
            if !item.played { return item.key }
        }
        return nil
    }

    /// §2 — the speed toggle cycles 1x -> 1.5x -> 2x -> 1x.
    public static let rates: [Float] = [1, 1.5, 2]

    public static func nextRate(after current: Float) -> Float {
        guard let i = rates.firstIndex(of: current) else { return 1 }
        return rates[(i + 1) % rates.count]
    }

    public static func rateLabel(_ rate: Float) -> String {
        rate == rate.rounded() ? "\(Int(rate))x" : "\(rate)x"
    }
}
