// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation

/// The app-wide holder of the Feed's "Couldn't check for new messages."
/// state (`docs/ux/feed.md`, Feed state table, row "refresh failed
/// (network)"). Mirror of web `feed/feed-refresh-store.ts`.
///
/// Every check reports here: the foreground group receive's sweep
/// (``ForegroundReceiveScheduler``), every channel post sync
/// (``ChannelPostSync``) and the Feed's pull to refresh / Retry
/// (``FeedViewModel/refresh()``). The rule itself is the pure
/// ``FeedRefreshState``; this only keeps the latest group and channel result.
///
/// In memory and per account: ``bind(accountIdB64:)`` forgets what another
/// account's checks found, and ``reset()`` runs on sign-out and on the
/// account wipe. A tally that arrives while no account is bound is dropped,
/// so a check finishing after sign-out cannot raise a banner for the next
/// session.
@MainActor
final class FeedRefreshCenter: ObservableObject {

    /// `true` while the banner should show.
    @Published private(set) var failed = false

    private(set) var state = FeedRefreshState()
    private(set) var accountIdB64: String?

    /// Start reporting for `accountIdB64`. Another account's results are
    /// forgotten; the same account keeps what it has.
    func bind(accountIdB64: String) {
        guard self.accountIdB64 != accountIdB64 else { return }
        self.accountIdB64 = accountIdB64
        apply(FeedRefreshState())
    }

    /// Forget everything (sign-out, account wipe).
    func reset() {
        accountIdB64 = nil
        apply(FeedRefreshState())
    }

    /// One run of one check finished.
    func record(_ kind: FeedRefreshState.Kind, _ tally: FeedCheckTally) {
        guard accountIdB64 != nil else { return }
        apply(state.recording(kind, tally))
    }

    private func apply(_ next: FeedRefreshState) {
        state = next
        if failed != next.failed { failed = next.failed }
    }
}
