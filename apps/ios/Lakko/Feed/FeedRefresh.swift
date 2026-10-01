// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// "Couldn't check for new messages." (`docs/ux/feed.md`, Feed state table,
/// row "refresh failed (network)"): the rule, kept pure and tested. Mirror of
/// web `feed/feed-refresh.ts`.
///
/// The feed is fed by two checks, the group poll and the channel post sync.
/// Each reports, per run, how many sources it tried and how many answered.
/// The banner shows when no source of either kind answered: the latest group
/// check and the latest channel check both reached nothing (a kind with
/// nothing to try does not count against). A check where some source answered
/// is not a failure, and the banner goes at the next check that reaches
/// anything.
struct FeedCheckTally: Equatable {
    var tried = 0
    var answered = 0
}

struct FeedRefreshState: Equatable {
    /// What one run of one check found. `nil`: nothing was tried, so it says
    /// nothing (no sources, or every one skipped for now).
    enum Outcome: Equatable {
        case reached
        case nothing
    }

    enum Kind {
        case groups
        case channels
    }

    var groups: Outcome?
    var channels: Outcome?

    static func outcome(of tally: FeedCheckTally) -> Outcome? {
        if tally.answered > 0 { return .reached }
        if tally.tried > 0 { return .nothing }
        return nil
    }

    /// Fold one run in. A run that tried nothing leaves the kind as it was,
    /// so it neither raises nor clears the banner.
    func recording(_ kind: Kind, _ tally: FeedCheckTally) -> FeedRefreshState {
        guard let outcome = Self.outcome(of: tally) else { return self }
        var next = self
        switch kind {
        case .groups: next.groups = outcome
        case .channels: next.channels = outcome
        }
        return next
    }

    /// Banner on: something was tried, and nothing of any kind answered.
    var failed: Bool {
        if groups == .reached || channels == .reached { return false }
        return groups == .nothing || channels == .nothing
    }
}
