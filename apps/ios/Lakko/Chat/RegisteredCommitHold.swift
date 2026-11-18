// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import os

// ADR-0091 phase 1 — a device does not skip the commit that moves its
// conversation.
//
// When a conversation row fails to apply, the receive loop classifies it before
// skipping it. A row framed at this device's epoch, that is a commit (the epoch
// after it is one higher), in a journal whose registered head the DS reports
// above this device's epoch, is the registered commit for the next epoch: since
// ADR-0081 an epoch is registered only together with its commit row, and a
// losing commit is never appended. That row is HELD: the batch stops at it, the
// cursor stays before it, and the apply is retried with a bounded backoff.
// Anything else is skipped as before, so a row that can never apply still never
// wedges the poll.
//
// The verdict, the tracker and the log lines are the same on web, iOS and
// Android (contract in the ADR); the tracker lives in memory and is
// process-wide, keyed by the conversation's journal id.

/// What a failed conversation row is.
enum RegisteredCommitVerdict: String, Equatable {
    /// The registered commit for this device's next epoch: do not skip it.
    case hold
    /// Anything else: skipped as before.
    case skip

    /// The verdict for a row that failed to apply.
    ///
    /// - `isOwnEcho`: the failure was this device's own row coming back.
    /// - `sealedEpoch` / `epochAfter`: `epochOfMessage` / `epochAfterMessage`
    ///   of the row's bytes (`nil` when not an MLS message).
    /// - `localEpoch`: the group's epoch read just before processing the row.
    /// - `latestEpoch`: the DS read's `latest_epoch` of the same fetch.
    static func of(
        isOwnEcho: Bool,
        sealedEpoch: UInt64?,
        epochAfter: UInt64?,
        localEpoch: UInt64?,
        latestEpoch: UInt64?
    ) -> Self {
        guard !isOwnEcho,
              let sealedEpoch, let epochAfter, let localEpoch, let latestEpoch,
              localEpoch < UInt64.max else { return .skip }
        let next = localEpoch + 1
        return sealedEpoch == localEpoch && epochAfter == next && latestEpoch >= next ? .hold : .skip
    }
}

/// The fixed set of failure kinds a log line may name. Never a text that came
/// from a peer: the kind is one of these words, or `refused:` and the core's
/// typed refusal code.
enum FailedApplyKind {
    static let wrongEpoch = "wrong-epoch"
    static let ownMessage = "own-message"
    static let missingProposal = "missing-proposal"
    static let decrypt = "decrypt"
    static let other = "other"
    static let refusedPrefix = "refused:"

    /// Classify from the typed refusal first, then from the error text.
    static func of(_ error: Error) -> String {
        if let reason = membershipRefusalReason(of: error) {
            return refusedPrefix + code(reason)
        }
        let text = String(describing: error)
        if text.contains("WrongEpoch") { return wrongEpoch }
        if text.contains("CannotDecryptOwnMessage") { return ownMessage }
        if text.contains("MissingProposal") { return missingProposal }
        if text.contains("Decrypt") || text.contains("Aead") || text.contains("AEAD") { return decrypt }
        return other
    }

    /// The core's stable refusal code (`lakko_mls::RefusalReason::code`).
    /// Exhaustive, so a new reason fails the build here.
    static func code(_ reason: FfiRefusalReason) -> String {
        switch reason {
        case .notAMember: return "notAMember"
        case .addNotOwnerOrAdmin: return "addNotOwnerOrAdmin"
        case .removeNotOwnerOrAdmin: return "removeNotOwnerOrAdmin"
        case .ownerUnremovable: return "ownerUnremovable"
        case .ownerCannotLeave: return "ownerCannotLeave"
        case .notOwner: return "notOwner"
        case .leafUpdateRefused: return "leafUpdateRefused"
        case .unsupportedProposal: return "unsupportedProposal"
        case .noRoleTable: return "noRoleTable"
        case .ownerNotAMember: return "ownerNotAMember"
        case .roleTableWouldVanish: return "roleTableWouldVanish"
        case .notACommitter: return "notACommitter"
        case .roleChangePending: return "roleChangePending"
        case .proposalsPending: return "proposalsPending"
        case .pendingSetFull: return "pendingSetFull"
        case .noSuchJoinRequest: return "noSuchJoinRequest"
        case .joinerAlreadyAMember: return "joinerAlreadyAMember"
        case .sweepAlreadyStaged: return "sweepAlreadyStaged"
        case .deviceBindingInvalid: return "deviceBindingInvalid"
        case .deviceLimitReached: return "deviceLimitReached"
        case .oneToOneMembershipFixed: return "oneToOneMembershipFixed"
        case .oneToOneHasNoRoles: return "oneToOneHasNoRoles"
        case .groupKindChanged: return "groupKindChanged"
        case .groupStillActive: return "groupStillActive"
        }
    }
}

/// The log lines of phase 1. Plain text, no identifier and no content: never a
/// group id, row id, key, account, name or text — only the kind, epochs, the
/// verdict and the attempt number.
enum RegisteredCommitLog {
    enum Level { case notice, error }

    /// Where a line goes. Replaceable by tests; production writes the unified
    /// log (the lines carry no identifier, so they are logged as public).
    nonisolated(unsafe) static var sink: (_ level: Level, _ line: String) -> Void = { level, line in
        switch level {
        case .notice: RegisteredCommitLog.logger.notice("\(line, privacy: .public)")
        case .error: RegisteredCommitLog.logger.error("\(line, privacy: .public)")
        }
    }

    private static let logger = Logger(subsystem: "me.lakko", category: "chat-receive")

    private static func number(_ value: UInt64?) -> String { value.map { String($0) } ?? "-" }

    /// One failed apply in a conversation poll (or the sweep verdict reader,
    /// or a DeviceGroup / PublisherGroup loop, with `verdict = skip`).
    static func failedRow(
        kind: String,
        sealed: UInt64?,
        after: UInt64?,
        local: UInt64?,
        latest: UInt64?,
        verdict: RegisteredCommitVerdict,
        attempt: Int?
    ) -> String {
        "chat row not applied: kind=\(kind) sealed=\(number(sealed)) after=\(number(after)) "
            + "local=\(number(local)) latest=\(number(latest)) verdict=\(verdict.rawValue) "
            + "attempt=\(attempt.map { String($0) } ?? "-")/\(RegisteredCommitHold.maxAttempts)"
    }

    /// The attempts are spent: this device is now behind in a conversation.
    static func exhausted(local: UInt64?, latest: UInt64?) -> String {
        "chat registered commit skipped after \(RegisteredCommitHold.maxAttempts) attempts: "
            + "this device is now behind in a conversation (local=\(number(local)) latest=\(number(latest)))"
    }

    /// This device refuses, by its own rules, a commit the DS registered.
    static func divergence(kind: String) -> String {
        "chat registered commit refused by this device's rules (\(kind)): "
            + "the other devices applied it; a bug or version skew"
    }

    /// The journal repair guard.
    static let repairBlocked = "a registered commit is known above this epoch; not repaired"

    static func emit(_ level: Level, _ line: String) { sink(level, line) }

    /// Classify and log one failed row with `verdict = skip` (the paths that
    /// never hold: the sweep verdict reader, the DeviceGroup and PublisherGroup
    /// loops). Logging only.
    static func skipped(error: Error, payload: Data, localEpoch: UInt64?, latestEpoch: UInt64?) {
        emit(.notice, failedRow(
            kind: FailedApplyKind.of(error),
            sealed: epochOfMessage(bytes: payload),
            after: epochAfterMessage(bytes: payload),
            local: localEpoch,
            latest: latestEpoch,
            verdict: .skip,
            attempt: nil))
    }
}

/// The hold tracker: in memory, process-wide, keyed by the conversation's
/// journal id (hex). Thread-safe; the clock is injectable.
final class RegisteredCommitHold {

    /// The tracker every conversation poll shares.
    static let shared = RegisteredCommitHold()

    /// Attempts before a held row is skipped. Chosen, not measured.
    static let maxAttempts = 8

    /// The wait after failure `k` (k = 1..7). Chosen, not measured.
    static let retryDelaysMs: [Int64] = [2, 5, 15, 30, 60, 120, 300].map { $0 * 1000 }

    /// One held row.
    struct Hold: Equatable {
        let rowId: Int64
        let sealedEpoch: UInt64
        var attempts: Int
        let firstFailedAtMs: Int64
        var nextAttemptAtMs: Int64
    }

    /// This device fell behind in the conversation (phase 2 reads it).
    struct BehindMark: Equatable {
        let epoch: UInt64
        let atMs: Int64
    }

    /// What one failure of a held row came to.
    enum FailureOutcome: Equatable {
        /// Held: stop the batch at this row; retry no sooner than the time.
        case held(attempt: Int, nextAttemptAtMs: Int64)
        /// The attempts are spent: the row is skipped and the behind mark set.
        case exhausted(attempt: Int)
    }

    /// What a poll does before fetching.
    enum PollGate: Equatable {
        case fetch
        /// Do not fetch; wait this long (cancellable) and return.
        case wait(ms: Int64)
    }

    private let lock = NSLock()
    private var holds: [String: Hold] = [:]
    private var behindMarks: [String: BehindMark] = [:]
    private let nowMs: () -> Int64

    init(nowMs: @escaping () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }) {
        self.nowMs = nowMs
    }

    func now() -> Int64 { nowMs() }

    func hold(groupIdHex: String) -> Hold? {
        lock.lock(); defer { lock.unlock() }
        return holds[groupIdHex]
    }

    func behindMark(groupIdHex: String) -> BehindMark? {
        lock.lock(); defer { lock.unlock() }
        return behindMarks[groupIdHex]
    }

    /// A HOLD row failed once more.
    func recordFailure(groupIdHex: String, rowId: Int64, sealedEpoch: UInt64, localEpoch: UInt64) -> FailureOutcome {
        lock.lock(); defer { lock.unlock() }
        let now = nowMs()
        var hold: Hold
        if let existing = holds[groupIdHex], existing.rowId == rowId {
            hold = existing
            hold.attempts += 1
        } else {
            hold = Hold(rowId: rowId, sealedEpoch: sealedEpoch, attempts: 1,
                        firstFailedAtMs: now, nextAttemptAtMs: now)
        }
        if hold.attempts >= Self.maxAttempts {
            holds[groupIdHex] = nil
            behindMarks[groupIdHex] = BehindMark(epoch: localEpoch, atMs: now)
            return .exhausted(attempt: hold.attempts)
        }
        hold.nextAttemptAtMs = now + Self.retryDelaysMs[hold.attempts - 1]
        holds[groupIdHex] = hold
        return .held(attempt: hold.attempts, nextAttemptAtMs: hold.nextAttemptAtMs)
    }

    /// A row applied: the hold goes when it was the held row.
    func recordApplied(groupIdHex: String, rowId: Int64) {
        lock.lock(); defer { lock.unlock() }
        if holds[groupIdHex]?.rowId == rowId { holds[groupIdHex] = nil }
    }

    /// The held row was read again and is no longer a HOLD row (the group's
    /// epoch moved past it by another path): the hold goes.
    func recordSkipped(groupIdHex: String, rowId: Int64) {
        lock.lock(); defer { lock.unlock() }
        if holds[groupIdHex]?.rowId == rowId { holds[groupIdHex] = nil }
    }

    /// The group's epoch now. A hold whose row is framed below it is answered;
    /// a behind mark below it is lifted.
    func observeEpoch(groupIdHex: String, epoch: UInt64) {
        lock.lock(); defer { lock.unlock() }
        if let hold = holds[groupIdHex], epoch > hold.sealedEpoch { holds[groupIdHex] = nil }
        if let mark = behindMarks[groupIdHex], epoch > mark.epoch { behindMarks[groupIdHex] = nil }
    }

    /// No hold, or its next attempt is due.
    func isDue(groupIdHex: String) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard let hold = holds[groupIdHex] else { return true }
        return nowMs() >= hold.nextAttemptAtMs
    }

    /// Before fetching a conversation journal: while a hold waits for its next
    /// attempt, do not fetch; wait out the long-poll time or the attempt time,
    /// whichever comes first.
    func pollGate(groupIdHex: String, waitSecs: Int) -> PollGate {
        lock.lock(); defer { lock.unlock() }
        guard let hold = holds[groupIdHex] else { return .fetch }
        let remaining = hold.nextAttemptAtMs - nowMs()
        guard remaining > 0 else { return .fetch }
        return .wait(ms: min(Int64(max(0, waitSecs)) * 1000, remaining))
    }

    /// Journal repair must not Replace at `notedEpoch`: a registered commit is
    /// held here, or this device fell behind at or above that epoch.
    func blocksRepair(groupIdHex: String, notedEpoch: UInt64) -> Bool {
        lock.lock(); defer { lock.unlock() }
        if holds[groupIdHex] != nil { return true }
        if let mark = behindMarks[groupIdHex], mark.epoch >= notedEpoch { return true }
        return false
    }

    /// Forget everything (a session closed).
    func reset() {
        lock.lock(); defer { lock.unlock() }
        holds.removeAll()
        behindMarks.removeAll()
    }
}
