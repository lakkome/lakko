// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0057, the second security pass (finding 1 + LOW 7): repair a journal a
/// lower rank pushed ahead.
///
/// Every commit registers its next epoch in APPEND mode. A member-rank
/// registrar may append at most one epoch past the newest admin or root
/// registration, but that is still enough to put a key at `N+1` that no
/// commit on the journal ever reaches. Every device at `N` then finds its own
/// Append refused (`409`), and once the fake epoch pushes `N` out of the write
/// tier its signed requests are refused (`401`).
///
/// The owner or an admin repairs it from above:
///
///   * **Triggers.** (a) this device's Append got `409`; (b) a signed request
///     at its current epoch got `401` after every retry. Either only NOTES the
///     journal; a plain member never repairs (its note is dropped).
///   * **The check, 30 s later** (``delaySeconds``, chosen, not measured: long
///     enough for a real committer's publish to follow its registration). Poll
///     the group to the journal head; if the group's epoch is still the `N`
///     noted (no commit reached `N+1`), send `replaceEpoch(journal, N, keyAtN)`.
///     `204`: the fake epoch is gone. `403`: an equal or higher rank
///     registered above, a real epoch this device reaches by polling; stop.
///     A head poll that fails for any reason but `401` (offline, 5xx, the
///     clock) sends nothing: no Replace on a head this device could not read.
///
/// Never part of a commit path: the commit that met `409` was abandoned before
/// anything was noted, and the Replace re-registers only the epoch this device
/// is already at. Only conversation groups have a head poller here; the
/// DeviceGroup and a PublisherGroup are never repaired (every registrar there
/// is the root). Every step is logged at info level; no key material is.
///
/// Mirror of `apps/web/src/ds/journal-repair.ts`.
@MainActor
public final class JournalRepair {

    /// How long after a trigger the check runs. Chosen, not measured.
    public static let delaySeconds: TimeInterval = 30

    /// How many polls the head poll may take (each applies one page).
    public static let maxPolls = 20

    /// Why a journal was noted.
    public enum Reason: String {
        case appendConflict = "append-conflict"
        case refusedAtHead = "refused-at-head"
    }

    /// What one check came to.
    public enum Outcome: Equatable {
        /// 204: the epochs above `N` were dropped.
        case replaced
        /// 403: an equal or higher rank registered above; stop.
        case outranked
        /// The group's epoch moved away from `N`: nothing to repair.
        case moved
        /// This device holds no admin or root authority (any more).
        case notPermitted
        /// The head poll failed for a reason other than `401`.
        case pollFailed
        /// The group is not held here, or its key could not be derived.
        case noKey
        /// The Replace failed for any other reason.
        case failed
        /// ADR-0091 phase 1 — this device holds a registered commit for the
        /// group, or fell behind at or above the noted epoch: the epochs above
        /// are real, so nothing is replaced.
        case registeredCommitKnown
    }

    /// What the check needs from the session.
    public struct Ports {
        /// This device's authority over the journal now.
        public var authority: (_ groupIdHex: String) -> JournalAuthority?
        /// Poll the group to the journal head, applying everything. Throws the
        /// DS refusal or the network failure.
        public var pollToHead: (_ groupIdHex: String) async throws -> Void
        /// The group's current epoch and the key registered for it, or `nil`
        /// when the group is not held (or cannot derive: evicted).
        public var currentEpochKey: (_ groupIdHex: String) -> (epoch: UInt64, key: Data)?
        /// The journal id of a group.
        public var journalId: (_ groupIdHex: String) -> Data?
        public var ds: DsStatementSending
        /// Run `work` after `seconds`; returns a canceller. Injectable for
        /// tests.
        public var schedule: (_ seconds: TimeInterval, _ work: @escaping @MainActor () -> Void) -> () -> Void
        /// ADR-0091 phase 1 — whether a registered commit is known above
        /// `epoch` for the group (a hold, or a behind mark at or above it).
        public var registeredCommitKnown: (_ groupIdHex: String, _ epoch: UInt64) -> Bool

        public init(
            authority: @escaping (String) -> JournalAuthority?,
            pollToHead: @escaping (String) async throws -> Void,
            currentEpochKey: @escaping (String) -> (epoch: UInt64, key: Data)?,
            journalId: @escaping (String) -> Data?,
            ds: DsStatementSending,
            schedule: @escaping (TimeInterval, @escaping @MainActor () -> Void) -> () -> Void =
                JournalRepair.defaultSchedule,
            registeredCommitKnown: @escaping (String, UInt64) -> Bool = { _, _ in false }
        ) {
            self.authority = authority
            self.pollToHead = pollToHead
            self.currentEpochKey = currentEpochKey
            self.journalId = journalId
            self.ds = ds
            self.schedule = schedule
            self.registeredCommitKnown = registeredCommitKnown
        }
    }

    public nonisolated static func defaultSchedule(
        _ seconds: TimeInterval, _ work: @escaping @MainActor () -> Void
    ) -> () -> Void {
        let task = Task { @MainActor in
            try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
            if !Task.isCancelled { work() }
        }
        return { task.cancel() }
    }

    private let ports: Ports
    private var pending: [String: () -> Void] = [:]

    public init(ports: Ports) {
        self.ports = ports
    }

    /// Cancel every scheduled check (sign-out).
    public func reset() {
        for cancel in pending.values { cancel() }
        pending.removeAll()
    }

    /// `true` while a check is scheduled or running for the group.
    public func isPending(groupIdHex: String) -> Bool { pending[groupIdHex] != nil }

    /// Note a trigger at `epoch` (the epoch this device is at). Schedules one
    /// check ``delaySeconds`` later when this device is the root or an admin
    /// and no check is pending for the group. Returns whether one was
    /// scheduled. Never throws.
    @discardableResult
    public func note(groupIdHex: String, epoch: UInt64, reason: Reason) -> Bool {
        guard pending[groupIdHex] == nil else { return false }
        guard let authority = ports.authority(groupIdHex), authority.isRootOrAdmin else { return false }
        NSLog("ADR-0057 repair: %@ at epoch %llu; checking in %d s",
              reason.rawValue, epoch, Int(Self.delaySeconds))
        let cancel = ports.schedule(Self.delaySeconds) { [weak self] in
            guard let self else { return }
            Task { @MainActor in
                _ = await self.check(groupIdHex: groupIdHex, epoch: epoch)
                self.pending[groupIdHex] = nil
            }
        }
        pending[groupIdHex] = cancel
        return true
    }

    /// The check itself: poll to the head, and Replace at `epoch` only when
    /// the group is still there. Exposed for tests. Never throws.
    public func check(groupIdHex: String, epoch: UInt64) async -> Outcome {
        do {
            try await ports.pollToHead(groupIdHex)
            NSLog("ADR-0057 repair: polled to the journal head")
        } catch {
            guard Self.isUnauthorized(error) else {
                NSLog("ADR-0057 repair: head poll failed; not repaired")
                return .pollFailed
            }
            NSLog("ADR-0057 repair: head poll refused (401)")
        }
        guard let current = ports.currentEpochKey(groupIdHex),
              let journalId = ports.journalId(groupIdHex) else { return .noKey }
        guard current.epoch == epoch else {
            NSLog("ADR-0057 repair: the group moved from epoch %llu to %llu; nothing to repair",
                  epoch, current.epoch)
            return .moved
        }
        guard let authority = ports.authority(groupIdHex), authority.isRootOrAdmin else {
            return .notPermitted
        }
        // ADR-0091 phase 1 — the head poll did not reach a registered commit
        // this device holds (or it fell behind one): the epochs above are not
        // fake, and a Replace would drop the group's real next epoch.
        if ports.registeredCommitKnown(groupIdHex, epoch) {
            NSLog("ADR-0057 repair: %@", RegisteredCommitLog.repairBlocked)
            return .registeredCommitKnown
        }
        NSLog("ADR-0057 repair: replacing at epoch %llu", epoch)
        do {
            try await JournalOwnership.replaceEpoch(
                authority: authority, journalId: journalId, epoch: epoch,
                epochKey: current.key, ds: ports.ds)
            NSLog("ADR-0057 repair: replaced at epoch %llu; the epochs above are gone", epoch)
            return .replaced
        } catch DsStatementError.rankShortfall {
            NSLog("ADR-0057 repair: replace refused (403): an equal or higher rank registered above")
            return .outranked
        } catch {
            NSLog("ADR-0057 repair: replace failed")
            return .failed
        }
    }

    /// `true` for the DS's uniform `401` (a signed request refused), whether
    /// it came from a journal read or a statement.
    static func isUnauthorized(_ error: Error) -> Bool {
        if case DsClientError.http(let status, _, _) = error { return status == 401 }
        if case DsStatementError.unauthorized = error { return true }
        if case ChatError.wrapped(_, let underlying) = error { return isUnauthorized(underlying) }
        return false
    }
}
