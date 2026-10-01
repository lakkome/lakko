// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// One item to draw: a message and the replies folded under it.
struct RankedFeedItem: Equatable {
    let ref: FeedCandidateRef
    let replies: [FeedCandidateRef]

    var key: String { ref.key }
}

/// The one ranking, reached through the UniFFI core (ADR-0063). Mirror of web
/// `feed/feed-ranking.ts`.
///
/// The order lives in `lakko-feed` so web, iOS and Android draw the same
/// messages in the same order; this only moves ``FeedInput``'s lists across
/// the boundary and maps the returned indices back to rows. It re-derives
/// nothing: a filter or a tie-break written here would be the drift the core
/// exists to prevent.
enum FeedRanking {

    static func conversations(_ sources: [FeedSource]) -> [FeedConversation] {
        sources.map { s in
            FeedConversation(
                id: Data(feedHex: s.idHex) ?? Data(s.idHex.utf8),
                kind: s.kind == .channel ? .channel : .group,
                muted: s.muted,
                audience: UInt32(clamping: max(0, s.audience)))
        }
    }

    /// Rank `input` under `rule` at `nowMs`.
    static func rank(rule: FeedRule, input: FeedInput, nowMs: Int64) -> [RankedFeedItem] {
        let entries = rankFeed(
            rule: rule,
            conversations: conversations(input.sources),
            candidates: input.candidates,
            nowMs: nowMs)
        var out: [RankedFeedItem] = []
        out.reserveCapacity(entries.count)
        for e in entries {
            let i = Int(e.index)
            guard i < input.refs.count else { continue }
            let replies = e.replies.compactMap { r -> FeedCandidateRef? in
                let j = Int(r)
                return j < input.refs.count ? input.refs[j] : nil
            }
            out.append(RankedFeedItem(ref: input.refs[i], replies: replies))
        }
        return out
    }
}

extension Data {
    /// Lowercase or uppercase hex to bytes; `nil` on an odd length or a
    /// non-hex character.
    init?(feedHex hexString: String) {
        let bytes = Array(hexString.utf8)
        guard bytes.count % 2 == 0 else { return nil }
        var out = Data(capacity: bytes.count / 2)
        var i = 0
        func nibble(_ c: UInt8) -> UInt8? {
            switch c {
            case 0x30...0x39: return c - 0x30
            case 0x61...0x66: return c - 0x61 + 10
            case 0x41...0x46: return c - 0x41 + 10
            default: return nil
            }
        }
        while i < bytes.count {
            guard let hi = nibble(bytes[i]), let lo = nibble(bytes[i + 1]) else { return nil }
            out.append(hi << 4 | lo)
            i += 2
        }
        self = out
    }

    /// Lowercase hex.
    var feedHex: String {
        map { String(format: "%02x", $0) }.joined()
    }
}
