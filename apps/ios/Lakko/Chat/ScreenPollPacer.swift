// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// When the chat screen's receive loop runs its next long poll.
///
/// The loop is `while !Task.isCancelled { await pollOnceLongPoll() }`, and a
/// long poll normally parks on the Delivery Service for up to 30 s. A poll
/// that fails at once does not park: with no network a request fails in a few
/// milliseconds, and the loop went straight into the next one. Seen on the iOS
/// simulator 2026-11-24 after the Mac's network changed: with a one-to-one
/// open, about 26 000 requests to that conversation's journal in one minute,
/// every one refused with "The Internet connection appears to be offline".
///
/// The rule is the web client's (`apps/web/src/ds/long-poll-loop.ts`), so the
/// two loops behave alike: after a failure, wait a backoff that starts at 2 s
/// for a network failure and 5 s for any other, doubles per further failure in
/// a row and stops at 30 s; after a success, the next poll starts no sooner
/// than 500 ms after the previous one started (a poll that returns at once
/// without failing, a gate or a proxy that strips `wait`, does not spin
/// either). A success resets the backoff, so a poll that works again resumes
/// at once. The numbers are chosen, not measured.
///
/// A backoff wait ends early when the network comes back
/// (``NetworkReturnSignal``, `docs/ux/connection-state.md` section 3), and the
/// backoff starts again from the beginning: the retry is the next poll.
struct ScreenPollPacer {
    /// First wait after a failed request that never reached the server.
    static let networkBackoffMs: UInt64 = 2_000
    /// First wait after any other failure (an HTTP error, a decode error).
    static let otherBackoffMs: UInt64 = 5_000
    /// Ceiling of the doubling wait: during a long outage, one request per
    /// half minute.
    static let maxBackoffMs: UInt64 = 30_000
    /// Shortest spacing between the starts of two successful polls.
    static let minTickSpacingMs: UInt64 = 500

    /// What one poll came to.
    enum Outcome: Equatable {
        case succeeded
        case failed(network: Bool)
    }

    private(set) var consecutiveFailures = 0

    /// The wait before the next poll, given how this one ended and how long
    /// it took.
    mutating func delayMs(after outcome: Outcome, elapsedMs: UInt64) -> UInt64 {
        switch outcome {
        case .succeeded:
            consecutiveFailures = 0
            return elapsedMs < Self.minTickSpacingMs ? Self.minTickSpacingMs - elapsedMs : 0
        case .failed(let network):
            consecutiveFailures += 1
            return Self.backoffMs(network: network, consecutiveFailures: consecutiveFailures)
        }
    }

    /// The network came back (`docs/ux/connection-state.md` section 3): the
    /// next failure starts the backoff from the beginning again.
    mutating func noteNetworkReturned() {
        consecutiveFailures = 0
    }

    /// The wait after the `consecutiveFailures`-th failure in a row (below 1
    /// counts as 1).
    static func backoffMs(network: Bool, consecutiveFailures: Int) -> UInt64 {
        let base = network ? networkBackoffMs : otherBackoffMs
        let doublings = min(max(consecutiveFailures, 1) - 1, 16)
        return min(maxBackoffMs, base << UInt64(doublings))
    }

    /// Whether `error` is a request that never got an answer from the server
    /// (no network, no route, a timeout, a dropped connection), looking
    /// through the wrappers the chat layer puts around DS errors.
    static func isNetworkFailure(_ error: Error) -> Bool {
        var current: Error = error
        for _ in 0..<4 {
            if current is URLError { return true }
            if case DsClientError.network = current { return true }
            guard case ChatError.wrapped(_, let underlying) = current else { return false }
            current = underlying
        }
        return false
    }
}
