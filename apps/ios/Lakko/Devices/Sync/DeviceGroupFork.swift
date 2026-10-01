// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Whether this device's DeviceGroup has forked from the one on the DS.
///
/// Live walk 2026-09-30: a device that removed another device from the
/// DeviceGroup held a sync handle an epoch behind, met its own removal commit
/// on the journal (MLS cannot apply a device's own commit), and persisted the
/// old epoch with the cursor past the commit. It never recovers on its own:
/// it has read the whole journal, its snapshot is at epoch N, and the DS chain
/// is at N+1 registered by the account root, which nothing replaces. The
/// removal path no longer produces this state (it persists and reloads before
/// publishing, ``DeviceRemoval``), but a device already in it needs repair.
///
/// The signal: the journal is drained (this poll returned no rows and nothing
/// more) while the DS says a newer epoch is registered than the one this
/// device holds. That is also true, briefly, while another device has
/// registered N+1 (ADR-0053: register before publish) and its commit has not
/// reached the journal yet, so the verdict only turns `forked` after the
/// condition has held for ``graceMs``.
///
/// Mirror of the web and Android rule of the same name; pure, so the
/// thresholds are testable without a DS.
public enum DeviceGroupFork {

    /// How long the drained-and-behind condition must hold before the
    /// DeviceGroup is treated as forked. Chosen, not measured: two minutes
    /// is far longer than a commit takes to follow its registration on a
    /// working connection, and short enough that a forked device repairs
    /// within the session in which it is noticed.
    public static let graceMs: Int64 = 120_000

    /// The verdict of one check.
    public enum Verdict: Equatable {
        /// Nothing suggests a fork. Any suspicion timer is cleared.
        case inSync
        /// Drained and behind since `since` (Unix milliseconds), not yet for
        /// longer than the grace.
        case suspect(since: Int64)
        /// Drained and behind for longer than the grace.
        case forked
    }

    /// - Parameters:
    ///   - localEpoch: the sync handle's DeviceGroup epoch at the time of the check.
    ///   - drained: this poll's fetch after the cursor returned zero rows and
    ///     `has_more` was false. A fetch that returned rows is never drained,
    ///     even when none of them applied.
    ///   - latestEpoch: the DS's newest registered epoch for the journal, `nil`
    ///     when the DS did not say (an unclaimed journal).
    ///   - suspectSince: the time the current suspicion started, `nil` when
    ///     there is none.
    ///   - now: the current time, Unix milliseconds.
    public static func verdict(
        localEpoch: UInt64,
        drained: Bool,
        latestEpoch: UInt64?,
        suspectSince: Int64?,
        now: Int64
    ) -> Verdict {
        guard drained, let latest = latestEpoch, latest > localEpoch else {
            return .inSync
        }
        guard let since = suspectSince else {
            return .suspect(since: now)
        }
        if now - since > graceMs {
            return .forked
        }
        return .suspect(since: since)
    }
}
