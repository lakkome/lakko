// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The pure decisions of the signed translation update (ADR-0061 section 5).
/// The IO lives in ``TranslationUpdater``; these are total functions over
/// plain values so the cadence and the launch rule test without a network,
/// a file system or the FFI.

/// What to do with the bundle stored from an earlier run.
enum StoredTranslationVerdict: Equatable {
    /// It still verifies as the same version: use it.
    case use
    /// It no longer verifies (an app update raised the built-in version or
    /// the release, the pinned key changed, the file is damaged), or it
    /// verifies as a different version than recorded: delete it.
    case discard
}

/// The launch rule. `reverifiedVersion` is the version `l10nAcceptBundle`
/// returned for the stored bytes with no last-accepted floor, `nil` when it
/// refused them.
func storedTranslationVerdict(storedVersion: UInt64, reverifiedVersion: UInt64?) -> StoredTranslationVerdict {
    guard let reverifiedVersion, reverifiedVersion == storedVersion else { return .discard }
    return .use
}

/// The fetch cadence: the release notice's (ADR-0038), at most once per
/// `throttleMillis` after the last check that got an answer. A clock that
/// moved backwards makes the check due rather than silencing it.
func translationCheckDue(lastCheckMillis: Int64?, nowMillis: Int64, throttleMillis: Int64) -> Bool {
    guard let lastCheckMillis else { return true }
    let elapsed = nowMillis - lastCheckMillis
    return elapsed < 0 || elapsed >= throttleMillis
}

/// Whether a response counts as a check for the throttle. A 2xx answer does
/// (accepted or not: the server served what it has, and an unchanged bundle
/// is refused as not newer every time, so refetching it at every launch
/// would only cost traffic). Anything else (404 while nothing is
/// published, a 5xx, no response at all) retries at the next launch.
func translationResponseCountsAsCheck(httpStatus: Int?) -> Bool {
    guard let httpStatus else { return false }
    return (200..<300).contains(httpStatus)
}
