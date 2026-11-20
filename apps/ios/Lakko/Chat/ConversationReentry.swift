// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation
import LakkoCore
import os

// ADR-0091 phase 2 — a device that fell behind a conversation notices it and
// comes back (`docs/ux/conversation-rejoin.md`).
//
// This file holds what can be asserted without a running repository: the
// durable per-conversation record, the in-memory evidence a poll collects,
// the re-admitter's memory, the knock key members keep, the owed gap history,
// and the pure rules over them. The pipeline itself is in
// `ChatRepository+Reentry.swift`. Every decision the ADR writes once in Rust
// (`conversationBehindVerdict`, `reentryRequestDue`, `behindStateStopsActing`,
// `reentryRoute`, `reentryAdmissionVerdict`) is called, never restated.

// MARK: - The behind state, durable

/// ``FfiBehindState`` as this device stores it.
public enum ReentryBehind: Codable, Equatable, Sendable {
    case inStep
    case suspected(sinceMs: UInt64)
    case confirmed(sinceMs: UInt64)

    public init(_ ffi: FfiBehindState) {
        switch ffi {
        case .inStep: self = .inStep
        case .suspected(let since): self = .suspected(sinceMs: since)
        case .confirmed(let since): self = .confirmed(sinceMs: since)
        }
    }

    public var ffi: FfiBehindState {
        switch self {
        case .inStep: return .inStep
        case .suspected(let since): return .suspected(sinceMs: since)
        case .confirmed(let since): return .confirmed(sinceMs: since)
        }
    }

    /// C-93: no commit, sweep, beacon, proposal, registration or repeat
    /// envelope in the conversation while this holds (the core's rule).
    public var stopsActing: Bool { behindStateStopsActing(state: ffi) }

    /// Confirmed: a message sent meanwhile is shown not sent (UX 2.2).
    public var isConfirmed: Bool {
        if case .confirmed = self { return true }
        return false
    }

    /// When the suspicion began, `nil` in step.
    public var sinceMs: UInt64? {
        switch self {
        case .inStep: return nil
        case .suspected(let since), .confirmed(let since): return since
        }
    }
}

/// One request this device sent to be brought back (section 3).
public struct ReentrySentRequest: Codable, Equatable, Sendable {
    /// The re-entry KeyPackage wire. Not secret (its private keys stay in the
    /// MLS provider); `Group.joinReentry` takes it back.
    public let keyPackage: Data
    public let epochLeft: UInt64
    public let sentAtMs: UInt64
    /// `own-device`, `mailbox` or `knock`.
    public let route: String
    /// The conversation's journal cursor when the request went out. Every
    /// row after it was skipped (the device could read none), so a device
    /// that comes back reads the journal again from here (section 5).
    public let cursorAtSend: Int64

    public init(keyPackage: Data, epochLeft: UInt64, sentAtMs: UInt64, route: String, cursorAtSend: Int64) {
        self.keyPackage = keyPackage
        self.epochLeft = epochLeft
        self.sentAtMs = sentAtMs
        self.route = route
        self.cursorAtSend = cursorAtSend
    }
}

/// What this device keeps per conversation while it is behind there. Durable
/// (the behind state survives a restart, UX 4), per account, behind the store
/// gate, dropped with the group and on Forget device.
public struct ConversationReentryRecord: Codable, Equatable, Sendable {
    public var behind: ReentryBehind = .inStep
    /// The time of the first row this device could not read (UX 2.4), the DS
    /// server stamp of that row.
    public var gapFromMs: UInt64?
    /// Evidence of real progress above this device's epoch (section 1): a row
    /// framed above it failed, a publish was refused, or reads were refused.
    /// Journal repair is held while it is set (section 2).
    public var evidence: Bool = false
    /// The newest `latest_epoch` a read of this journal named. Diagnostic
    /// only: a re-entry request never names it (walk-3 rule W3-R1, every
    /// request probes the journal right before it is minted).
    public var latestSeen: UInt64?
    /// The requests sent, oldest first, at most ``reentryRequestsKept()``.
    public var requests: [ReentrySentRequest] = []
    /// When the last request went out (section 3's 30-minute rule).
    public var lastSentMs: UInt64?
    /// The route the last request took (for the second line of UX 2.2).
    public var lastRoute: String?
    /// ADR-0091 live walk 5 (2026-11-20): a request of the current behind
    /// episode went out by route `own-device` (it was sent, not only tried)
    /// and the device is still behind. From then on the core's `reentryRoute`
    /// sends every request of the episode by the conversation's own route.
    /// Cleared with the episode (back in step). Optional so a record written
    /// before it decodes; `nil` reads as `false`.
    public var ownDeviceAsked: Bool?

    public init() {}

    /// `ownDeviceUnanswered` for the core's `reentryRoute`.
    public var ownDeviceUnanswered: Bool { ownDeviceAsked == true }

    /// `lastRouteWasOwnDevice` for the core's `reentryRequestDue`: the last
    /// request went to another own device and was sent (a failed send leaves
    /// ``ownDeviceAsked`` unset and is retried on the 30-minute rule's
    /// one-minute offset).
    public var lastRouteWasOwnDevice: Bool {
        ownDeviceAsked == true && lastRoute == ConversationReentryRules.routeName(.ownDevice)
    }

    /// The KeyPackages `Group.joinReentry` may accept.
    public var recordedKeyPackages: [Data] { requests.map(\.keyPackage) }

    /// Where a device that came back reads the journal again from: the
    /// earliest cursor any recorded request went out at.
    public var readFromAfterReentry: Int64? { requests.map(\.cursorAtSend).min() }

    /// Nothing worth keeping: in step and no request out.
    public var isEmpty: Bool { behind == .inStep && requests.isEmpty }
}

/// The per-account store of ``ConversationReentryRecord``.
public struct ConversationReentryStore {
    let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public static func key(_ accountIdB64: String) -> String {
        "lakko.conversation-reentry.v1.\(accountIdB64)"
    }

    func all(accountIdB64: String) -> [String: ConversationReentryRecord] {
        defaults.data(forKey: Self.key(accountIdB64))
            .flatMap { try? JSONDecoder().decode([String: ConversationReentryRecord].self, from: $0) } ?? [:]
    }

    public func load(accountIdB64: String, groupIdHex: String) -> ConversationReentryRecord {
        all(accountIdB64: accountIdB64)[groupIdHex] ?? ConversationReentryRecord()
    }

    /// Write `record` (an empty record removes the entry). `false` when the
    /// account's stores are sealed (``AccountStoreGate``) or encoding failed:
    /// the caller then sends nothing.
    @discardableResult
    public func save(_ record: ConversationReentryRecord, accountIdB64: String, groupIdHex: String) -> Bool {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return false }
        defer { gate.leave() }
        var map = all(accountIdB64: accountIdB64)
        map[groupIdHex] = record.isEmpty ? nil : record
        guard let data = try? JSONEncoder().encode(map) else { return false }
        defaults.set(data, forKey: Self.key(accountIdB64))
        return true
    }

    public func remove(accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var map = all(accountIdB64: accountIdB64)
        guard map.removeValue(forKey: groupIdHex) != nil else { return }
        defaults.set(try? JSONEncoder().encode(map), forKey: Self.key(accountIdB64))
    }

    /// The groups with a record (the launch pass).
    public func groups(accountIdB64: String) -> [String] {
        Array(all(accountIdB64: accountIdB64).keys).sorted()
    }
}

// MARK: - What a poll observes (in memory)

/// The evidence the receive loop collects between reads, per conversation
/// journal (hex id). In memory and process-wide like
/// ``RegisteredCommitHold``: a restart starts it over, which only means the
/// next evidence has to be seen again. Thread-safe; the clock is injectable.
public final class ReentryObservations {
    public static let shared = ReentryObservations()

    struct Entry {
        var refusedReads: UInt32 = 0
        var failedRowAboveLocal = false
        var publishRefused = false
        /// The DS stamp of the first row that failed since the last applied
        /// row, for the gap line.
        var firstFailedRowAtMs: UInt64?
        /// Leaf index -> (epoch, when) of the last application message this
        /// device decrypted from it (the re-admitter's "leaf is writing").
        var leafActivity: [UInt32: (epoch: UInt64, atMs: UInt64)] = [:]
        /// The largest DS stamp of a row applied since the last
        /// ``takeLastAppliedRowAtMs(groupIdHex:)``: what the durable
        /// ``ReentryRowClockStore`` is raised to after the read.
        var lastAppliedRowAtMs: UInt64?
    }

    private let lock = NSLock()
    private var entries: [String: Entry] = [:]
    private let nowMs: () -> UInt64

    public init(nowMs: @escaping () -> UInt64 = { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }) {
        self.nowMs = nowMs
    }

    /// A row applied: the failures before it are no longer "since the last
    /// applied row".
    /// `createdAtMs` is the row's DS stamp (the walk-2 rule R1: the gap
    /// starts no later than the newest row this device applied).
    public func noteApplied(groupIdHex: String, createdAtMs: UInt64? = nil) {
        lock.lock(); defer { lock.unlock() }
        entries[groupIdHex, default: Entry()].failedRowAboveLocal = false
        entries[groupIdHex]?.firstFailedRowAtMs = nil
        if let createdAtMs {
            let held = entries[groupIdHex]?.lastAppliedRowAtMs ?? 0
            entries[groupIdHex]?.lastAppliedRowAtMs = max(held, createdAtMs)
        }
    }

    /// The largest stamp of a row applied since the last call, then
    /// forgotten (the caller writes it to the durable clock).
    public func takeLastAppliedRowAtMs(groupIdHex: String) -> UInt64? {
        lock.lock(); defer { lock.unlock() }
        let applied = entries[groupIdHex]?.lastAppliedRowAtMs
        entries[groupIdHex]?.lastAppliedRowAtMs = nil
        return applied
    }

    /// A row failed to apply. `sealed` is its framed epoch, `local` the
    /// group's epoch before it, `createdAtMs` its DS stamp.
    public func noteFailedRow(groupIdHex: String, sealed: UInt64?, local: UInt64?, createdAtMs: UInt64?) {
        lock.lock(); defer { lock.unlock() }
        var entry = entries[groupIdHex] ?? Entry()
        if let sealed, let local, sealed > local { entry.failedRowAboveLocal = true }
        if entry.firstFailedRowAtMs == nil { entry.firstFailedRowAtMs = createdAtMs ?? nowMs() }
        entries[groupIdHex] = entry
    }

    /// A signed read came back `401` with the clock not skewed.
    public func noteReadRefused(groupIdHex: String) {
        lock.lock(); defer { lock.unlock() }
        entries[groupIdHex, default: Entry()].refusedReads &+= 1
    }

    /// A signed read succeeded.
    public func noteReadAnswered(groupIdHex: String) {
        lock.lock(); defer { lock.unlock() }
        entries[groupIdHex, default: Entry()].refusedReads = 0
    }

    /// A publish of this device to the journal was refused `401`, clock not
    /// skewed.
    public func notePublishRefused(groupIdHex: String) {
        lock.lock(); defer { lock.unlock() }
        entries[groupIdHex, default: Entry()].publishRefused = true
    }

    /// An application message from `leaf` decrypted at `epoch`.
    public func noteLeafActivity(groupIdHex: String, leaf: UInt32, epoch: UInt64) {
        lock.lock(); defer { lock.unlock() }
        entries[groupIdHex, default: Entry()].leafActivity[leaf] = (epoch, nowMs())
    }

    /// When this device last decrypted an application message from `leaf`
    /// sealed at `epoch`, if ever.
    public func lastActivity(groupIdHex: String, leaf: UInt32, epoch: UInt64) -> UInt64? {
        lock.lock(); defer { lock.unlock() }
        guard let seen = entries[groupIdHex]?.leafActivity[leaf], seen.epoch == epoch else { return nil }
        return seen.atMs
    }

    struct Snapshot: Equatable {
        let refusedReads: UInt32
        let failedRowAboveLocal: Bool
        let publishRefused: Bool
        let firstFailedRowAtMs: UInt64?
    }

    func snapshot(groupIdHex: String) -> Snapshot {
        lock.lock(); defer { lock.unlock() }
        let entry = entries[groupIdHex] ?? Entry()
        return Snapshot(
            refusedReads: entry.refusedReads, failedRowAboveLocal: entry.failedRowAboveLocal,
            publishRefused: entry.publishRefused, firstFailedRowAtMs: entry.firstFailedRowAtMs)
    }

    /// The device is back in step: the evidence is spent.
    public func clear(groupIdHex: String) {
        lock.lock(); defer { lock.unlock() }
        let activity = entries[groupIdHex]?.leafActivity ?? [:]
        entries[groupIdHex] = Entry(leafActivity: activity)
    }

    /// Forget everything (a session closed).
    public func reset() {
        lock.lock(); defer { lock.unlock() }
        entries.removeAll()
    }
}

// MARK: - When this device last knew the conversation (walk-2 rule R1)

/// Per account and conversation, durable: the newest DS stamp of a row this
/// device applied there (or of an own publish the DS accepted), and the local
/// time of the last read of the journal that was not refused. Not gap
/// bookkeeping: both survive "back in step", and go with the group and on
/// Forget device. The gap a request names starts no later than either
/// (``ConversationReentryRules/gapStart(firstSkippedMs:lastKnownRowMs:lastReadOkMs:suspicionSinceMs:nowMs:)``),
/// so a device that fell behind through refused reads (no row skipped)
/// still names when it last knew the conversation.
public struct ReentryRowClock: Codable, Equatable, Sendable {
    public var lastKnownRowMs: UInt64?
    public var lastReadOkMs: UInt64?

    public init(lastKnownRowMs: UInt64? = nil, lastReadOkMs: UInt64? = nil) {
        self.lastKnownRowMs = lastKnownRowMs
        self.lastReadOkMs = lastReadOkMs
    }
}

public struct ReentryRowClockStore {
    /// The last good read is written at most once a minute; a slightly
    /// earlier value is the safe side (the gap starts earlier). Chosen, not
    /// measured.
    public static let readOkWriteIntervalMs: UInt64 = 60_000

    let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public static func key(_ accountIdB64: String) -> String {
        "lakko.conversation-reentry.row-clock.v1.\(accountIdB64)"
    }

    private func all(_ accountIdB64: String) -> [String: ReentryRowClock] {
        defaults.data(forKey: Self.key(accountIdB64))
            .flatMap { try? JSONDecoder().decode([String: ReentryRowClock].self, from: $0) } ?? [:]
    }

    private func update(
        _ accountIdB64: String, _ groupIdHex: String, _ change: (inout ReentryRowClock) -> Bool
    ) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var map = all(accountIdB64)
        var clock = map[groupIdHex] ?? ReentryRowClock()
        guard change(&clock) else { return }
        map[groupIdHex] = clock
        defaults.set(try? JSONEncoder().encode(map), forKey: Self.key(accountIdB64))
    }

    public func load(accountIdB64: String, groupIdHex: String) -> ReentryRowClock {
        all(accountIdB64)[groupIdHex] ?? ReentryRowClock()
    }

    /// A row stamped `atMs` applied here (or an own publish was accepted):
    /// the clock only moves forward.
    public func noteRowKnown(accountIdB64: String, groupIdHex: String, atMs: UInt64) {
        update(accountIdB64, groupIdHex) { clock in
            guard atMs > (clock.lastKnownRowMs ?? 0) else { return false }
            clock.lastKnownRowMs = atMs
            return true
        }
    }

    /// A read of the journal was not refused at `nowMs`.
    public func noteReadOk(accountIdB64: String, groupIdHex: String, nowMs: UInt64) {
        update(accountIdB64, groupIdHex) { clock in
            if let last = clock.lastReadOkMs, nowMs >= last, nowMs < last &+ Self.readOkWriteIntervalMs {
                return false
            }
            clock.lastReadOkMs = nowMs
            return true
        }
    }

    /// The device came back by a re-entry Welcome at `nowMs`: it has applied
    /// no row of the new group yet.
    public func noteReentryJoined(accountIdB64: String, groupIdHex: String, nowMs: UInt64) {
        update(accountIdB64, groupIdHex) { clock in
            clock = ReentryRowClock(lastKnownRowMs: nil, lastReadOkMs: nowMs)
            return true
        }
    }

    public func remove(accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var map = all(accountIdB64)
        guard map.removeValue(forKey: groupIdHex) != nil else { return }
        defaults.set(try? JSONEncoder().encode(map), forKey: Self.key(accountIdB64))
    }
}

// MARK: - The knock key a member was last sent (walk-2 rule R4)

/// What this device last DELIVERED of a group's knock key to the members:
/// the key version and the member set (leaf signature keys, base64) at that
/// successful broadcast. Durable per account and group; set only after the
/// broadcast went out, so a send that was skipped, refused while behind or
/// failed leaves the send due.
public struct ReentryKnockKeySentMark: Codable, Equatable, Sendable {
    public let version: UInt16
    public let members: [String]

    public init(version: UInt16, members: [String]) {
        self.version = version
        self.members = members.sorted()
    }
}

public struct ReentryKnockKeySentStore {
    let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public static func key(_ accountIdB64: String) -> String {
        "lakko.conversation-reentry.knock-key-sent.v1.\(accountIdB64)"
    }

    private func all(_ accountIdB64: String) -> [String: ReentryKnockKeySentMark] {
        defaults.data(forKey: Self.key(accountIdB64))
            .flatMap { try? JSONDecoder().decode([String: ReentryKnockKeySentMark].self, from: $0) } ?? [:]
    }

    public func load(accountIdB64: String, groupIdHex: String) -> ReentryKnockKeySentMark? {
        all(accountIdB64)[groupIdHex]
    }

    public func save(_ mark: ReentryKnockKeySentMark, accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var map = all(accountIdB64)
        map[groupIdHex] = mark
        defaults.set(try? JSONEncoder().encode(map), forKey: Self.key(accountIdB64))
    }

    public func remove(accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var map = all(accountIdB64)
        guard map.removeValue(forKey: groupIdHex) != nil else { return }
        defaults.set(try? JSONEncoder().encode(map), forKey: Self.key(accountIdB64))
    }
}

// MARK: - The re-admitter's memory

/// Section 4's two per-leaf limits, per account: a re-entry for a leaf is
/// handled once per epoch, and accepted at most once an hour. Keyed on the
/// leaf's 32-byte signature key (never its index, C-68).
public struct ReentryAdmissionMemory {
    struct Entry: Codable, Equatable {
        var handledEpoch: UInt64?
        var acceptedAtMs: UInt64?
    }

    let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public static func key(_ accountIdB64: String) -> String {
        "lakko.conversation-reentry.admissions.v1.\(accountIdB64)"
    }

    static func entryKey(groupIdHex: String, leafKey: Data) -> String {
        "\(groupIdHex)|\(leafKey.base64EncodedString())"
    }

    private func all(_ accountIdB64: String) -> [String: Entry] {
        defaults.data(forKey: Self.key(accountIdB64))
            .flatMap { try? JSONDecoder().decode([String: Entry].self, from: $0) } ?? [:]
    }

    private func write(_ map: [String: Entry], _ accountIdB64: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(try? JSONEncoder().encode(map), forKey: Self.key(accountIdB64))
    }

    public func handledAt(accountIdB64: String, groupIdHex: String, leafKey: Data, epoch: UInt64) -> Bool {
        all(accountIdB64)[Self.entryKey(groupIdHex: groupIdHex, leafKey: leafKey)]?.handledEpoch == epoch
    }

    public func lastAcceptedMs(accountIdB64: String, groupIdHex: String, leafKey: Data) -> UInt64? {
        all(accountIdB64)[Self.entryKey(groupIdHex: groupIdHex, leafKey: leafKey)]?.acceptedAtMs
    }

    public func markHandled(accountIdB64: String, groupIdHex: String, leafKey: Data, epoch: UInt64) {
        var map = all(accountIdB64)
        map[Self.entryKey(groupIdHex: groupIdHex, leafKey: leafKey), default: Entry()].handledEpoch = epoch
        write(map, accountIdB64)
    }

    public func markAccepted(accountIdB64: String, groupIdHex: String, leafKey: Data, atMs: UInt64) {
        var map = all(accountIdB64)
        map[Self.entryKey(groupIdHex: groupIdHex, leafKey: leafKey), default: Entry()].acceptedAtMs = atMs
        write(map, accountIdB64)
    }

    public func removeGroup(accountIdB64: String, groupIdHex: String) {
        let map = all(accountIdB64).filter { !$0.key.hasPrefix(groupIdHex + "|") }
        write(map, accountIdB64)
    }
}

// MARK: - The knock key a member keeps (route 3)

/// The group's knock public key as the owner sent it in-group (section 3,
/// route 3), per account and group. A plain member holds no knock seed, so
/// this is how it seals a re-entry request to the knock queue.
public struct ReentryKnockKeyStore {
    public struct Held: Codable, Equatable {
        public let version: UInt16
        public let publicKey: Data
    }

    let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public static func key(_ accountIdB64: String) -> String {
        "lakko.conversation-reentry.knock-keys.v1.\(accountIdB64)"
    }

    private func all(_ accountIdB64: String) -> [String: Held] {
        defaults.data(forKey: Self.key(accountIdB64))
            .flatMap { try? JSONDecoder().decode([String: Held].self, from: $0) } ?? [:]
    }

    public func load(accountIdB64: String, groupIdHex: String) -> Held? {
        all(accountIdB64)[groupIdHex]
    }

    /// Keep `held` when it is strictly newer than what is stored
    /// (`knockKeyIsNewer`). `true` when it was kept.
    @discardableResult
    public func keepIfNewer(_ held: Held, accountIdB64: String, groupIdHex: String) -> Bool {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return false }
        defer { gate.leave() }
        var map = all(accountIdB64)
        guard knockKeyIsNewer(incomingVersion: held.version, storedVersion: map[groupIdHex]?.version) else {
            return false
        }
        map[groupIdHex] = held
        defaults.set(try? JSONEncoder().encode(map), forKey: Self.key(accountIdB64))
        return true
    }

    public func remove(accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var map = all(accountIdB64)
        guard map.removeValue(forKey: groupIdHex) != nil else { return }
        defaults.set(try? JSONEncoder().encode(map), forKey: Self.key(accountIdB64))
    }
}

// MARK: - The gap history a re-admitter owes (section 7)

/// One gap history a re-admitter owes the device it brought back: sent once
/// this device holds the group at `targetEpoch` (the epoch the replacement
/// creates), at most ``ReentryHistoryOwed/lifetimeMs`` after it was owed.
public struct ReentryOwedHistory: Codable, Equatable, Sendable {
    public let groupIdHex: String
    /// The re-entry KeyPackage (the owed row's identity).
    public let keyPackage: Data
    /// The returning leaf's 32-byte signature key: the history secret's
    /// context (both sides know it, whichever recorded request was used).
    public let leafKey: Data
    public let targetEpoch: UInt64
    public let gapFromMs: UInt64
    public let owedAtMs: UInt64

    public init(
        groupIdHex: String, keyPackage: Data, leafKey: Data, targetEpoch: UInt64, gapFromMs: UInt64, owedAtMs: UInt64
    ) {
        self.groupIdHex = groupIdHex
        self.keyPackage = keyPackage
        self.leafKey = leafKey
        self.targetEpoch = targetEpoch
        self.gapFromMs = gapFromMs
        self.owedAtMs = owedAtMs
    }
}

public struct ReentryHistoryOwed {
    /// How long an owed history waits for its epoch (`GAP_OWED_TTL_MS`).
    /// Chosen, not measured: the returning device looks for the bundle for
    /// ``ConversationReentryRules/gapHistoryLookRetentionMs``, so the
    /// re-admitter owes it as long.
    public static let lifetimeMs: UInt64 = 30 * 24 * 60 * 60 * 1000

    let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public static func key(_ accountIdB64: String) -> String {
        "lakko.conversation-reentry.history-owed.v1.\(accountIdB64)"
    }

    public func load(accountIdB64: String) -> [ReentryOwedHistory] {
        defaults.data(forKey: Self.key(accountIdB64))
            .flatMap { try? JSONDecoder().decode([ReentryOwedHistory].self, from: $0) } ?? []
    }

    public func save(_ rows: [ReentryOwedHistory], accountIdB64: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(try? JSONEncoder().encode(rows), forKey: Self.key(accountIdB64))
    }

    public func add(_ row: ReentryOwedHistory, accountIdB64: String) {
        var rows = load(accountIdB64: accountIdB64).filter { $0.keyPackage != row.keyPackage }
        rows.append(row)
        save(rows, accountIdB64: accountIdB64)
    }
}

// MARK: - The gap history a returning device looks for (section 7)

/// One conversation whose gap history this device, back after a re-entry,
/// still looks for: the history secret (exported at the epoch the
/// replacement created; sealed at rest), the gap's two ends, and when it
/// last looked.
public struct ReentryGapLook: Equatable, Sendable {
    public let groupIdHex: String
    /// The 32-byte history secret. Never written in the clear.
    public let secret: Data
    public let gapFromMs: UInt64
    /// The moment the device came back: the gap line's end.
    public let gapToMs: UInt64
    public var lastLookMs: UInt64?
    /// The route of the request that brought this device back (W3-R3: a
    /// bundle row without an author is placed by it).
    public let route: String?

    public init(
        groupIdHex: String, secret: Data, gapFromMs: UInt64, gapToMs: UInt64,
        lastLookMs: UInt64? = nil, route: String? = nil
    ) {
        self.groupIdHex = groupIdHex
        self.secret = secret
        self.gapFromMs = gapFromMs
        self.gapToMs = gapToMs
        self.lastLookMs = lastLookMs
        self.route = route
    }
}

/// The durable gap looks, per account and group, in `UserDefaults`. The
/// secret is sealed with AES-GCM under a per-account wrap key kept in the
/// Keychain (the ``PushRoutingStore`` pattern); the other fields are public.
public struct ReentryGapLookStore {
    /// The stored form of one look.
    struct Stored: Codable, Equatable {
        let sealedSecret: Data
        let gapFromMs: UInt64
        let gapToMs: UInt64
        var lastLookMs: UInt64?
        var route: String?
    }

    let defaults: UserDefaults
    /// The wrap key of an account: created on a write, read on a read.
    let wrapKey: (_ accountIdB64: String, _ create: Bool) -> Data?

    /// Keychain service of the wrap key; the Keychain account is the
    /// account id, so Forget device removes the key with the rows.
    public static let wrapKeyService = "me.lakko.reentry-gap-look-wrap.v1"

    public init(
        defaults: UserDefaults = .standard,
        wrapKey: @escaping (_ accountIdB64: String, _ create: Bool) -> Data? = { account, create in
            create
                ? KeychainWrapKey.loadOrCreate(service: ReentryGapLookStore.wrapKeyService, account: account)
                : KeychainWrapKey.read(service: ReentryGapLookStore.wrapKeyService, account: account)
        }
    ) {
        self.defaults = defaults
        self.wrapKey = wrapKey
    }

    public static func key(_ accountIdB64: String) -> String {
        "lakko.conversation-reentry.gap-look.v1.\(accountIdB64)"
    }

    /// Forget device: the rows and the account's wrap key.
    public static func wipe(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: key(accountIdB64))
        KeychainWrapKey.delete(service: wrapKeyService, account: accountIdB64)
    }

    func storedRows(_ accountIdB64: String) -> [String: Stored] {
        defaults.data(forKey: Self.key(accountIdB64))
            .flatMap { try? JSONDecoder().decode([String: Stored].self, from: $0) } ?? [:]
    }

    private func write(_ map: [String: Stored], _ accountIdB64: String) {
        if map.isEmpty {
            defaults.removeObject(forKey: Self.key(accountIdB64))
        } else {
            defaults.set(try? JSONEncoder().encode(map), forKey: Self.key(accountIdB64))
        }
    }

    /// The groups this account has a look for.
    public func groupIds(accountIdB64: String) -> [String] {
        Array(storedRows(accountIdB64).keys)
    }

    /// The look for `groupIdHex`, or `nil` when none is stored or its secret
    /// does not open (a lost wrap key: the look is then useless).
    public func load(accountIdB64: String, groupIdHex: String) -> ReentryGapLook? {
        guard let stored = storedRows(accountIdB64)[groupIdHex],
              let keyBytes = wrapKey(accountIdB64, false),
              let box = try? AES.GCM.SealedBox(combined: stored.sealedSecret),
              let secret = try? AES.GCM.open(box, using: SymmetricKey(data: keyBytes)) else { return nil }
        return ReentryGapLook(
            groupIdHex: groupIdHex, secret: secret, gapFromMs: stored.gapFromMs,
            gapToMs: stored.gapToMs, lastLookMs: stored.lastLookMs, route: stored.route)
    }

    /// Store `look` (replacing any for its group). `false` when the account
    /// is closing or the secret could not be sealed; nothing is written then.
    @discardableResult
    public func save(_ look: ReentryGapLook, accountIdB64: String) -> Bool {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return false }
        defer { gate.leave() }
        guard let keyBytes = wrapKey(accountIdB64, true),
              let sealed = try? AES.GCM.seal(look.secret, using: SymmetricKey(data: keyBytes)).combined
        else { return false }
        var map = storedRows(accountIdB64)
        map[look.groupIdHex] = Stored(
            sealedSecret: sealed, gapFromMs: look.gapFromMs, gapToMs: look.gapToMs, lastLookMs: look.lastLookMs,
            route: look.route)
        write(map, accountIdB64)
        return true
    }

    /// Record that a look for `groupIdHex` started at `atMs`.
    public func markLooked(accountIdB64: String, groupIdHex: String, atMs: UInt64) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var map = storedRows(accountIdB64)
        guard map[groupIdHex] != nil else { return }
        map[groupIdHex]?.lastLookMs = atMs
        write(map, accountIdB64)
    }

    public func remove(accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var map = storedRows(accountIdB64)
        guard map.removeValue(forKey: groupIdHex) != nil else { return }
        write(map, accountIdB64)
    }
}

/// What a gap look does now.
public enum ReentryGapLookVerdict: Equatable, Sendable {
    /// Read the rendezvous once.
    case look
    /// Looked less than ``ConversationReentryRules/gapHistoryLookIntervalMs`` ago.
    case wait
    /// ``ConversationReentryRules/gapHistoryLookRetentionMs`` has passed since
    /// the gap's end: drop the look; the gap line stays.
    case drop
}

/// What a received gap history does to one stored gap line.
public enum ReentryGapNarrowing: Equatable, Sendable {
    case keep
    case remove
    /// The line now ends at the earliest row received.
    case replace(String)
}

// MARK: - The rules

public enum ConversationReentryRules {

    /// The `ownDeviceInConversation` the core's `reentryRoute` is asked
    /// with: another own device holds a leaf in the conversation AND this
    /// device can reach it (a DeviceGroup sender is registered). Without a
    /// sender route 1 cannot send, so the request goes by route 2 or 3 at
    /// once instead of waiting on it for ever (live walk 5, 2026-11-20).
    public static func ownDeviceReachable(inConversation: Bool, senderRegistered: Bool) -> Bool {
        inConversation && senderRegistered
    }

    /// The route as stored.
    public static func routeName(_ route: FfiReentryRoute) -> String {
        switch route {
        case .ownDevice: return "own-device"
        case .mailbox: return "mailbox"
        case .knock: return "knock"
        }
    }

    /// Section 1's observation from one poll of the conversation journal.
    ///
    /// `drained`: the read returned no row and no "more", and the batch was
    /// not held (a held batch stops early and is read again).
    static func observation(
        ownCommitOwed: Bool,
        rowsReturned: Int,
        hasMore: Bool,
        held: Bool,
        latestEpoch: UInt64?,
        localEpoch: UInt64,
        evidence: ReentryObservations.Snapshot
    ) -> FfiBehindObservation {
        FfiBehindObservation(
            ownCommitOwed: ownCommitOwed,
            drained: rowsReturned == 0 && !hasMore && !held,
            latestEpoch: latestEpoch,
            localEpoch: localEpoch,
            refusedReads: evidence.refusedReads,
            failedRowAboveLocal: evidence.failedRowAboveLocal,
            publishRefused: evidence.publishRefused)
    }

    /// Fold one observation into `record` (the core's verdict decides the
    /// state). Returns the next record; `cleared` when this observation moved
    /// a behind device back in step.
    static func fold(
        _ record: ConversationReentryRecord,
        observation: FfiBehindObservation,
        evidence: ReentryObservations.Snapshot,
        nowMs: UInt64
    ) -> (record: ConversationReentryRecord, cleared: Bool) {
        var next = record
        let state = conversationBehindVerdict(previous: record.behind.ffi, observation: observation, nowMs: nowMs)
        next.behind = ReentryBehind(state)
        if let latest = observation.latestEpoch {
            next.latestSeen = max(next.latestSeen ?? 0, latest)
        }
        if evidence.failedRowAboveLocal || evidence.publishRefused
            || evidence.refusedReads >= reentryRefusedReadsToSuspect() {
            next.evidence = true
        }
        if next.behind != .inStep, let first = evidence.firstFailedRowAtMs {
            next.gapFromMs = min(next.gapFromMs ?? first, first)
        }
        let cleared = record.behind != .inStep && next.behind == .inStep
        if cleared {
            next.evidence = false
            next.gapFromMs = nil
            next.latestSeen = nil
            // Live walk 5: the episode ends; the next one asks its own device
            // first again.
            next.ownDeviceAsked = nil
        }
        return (next, cleared)
    }

    /// Add a request, keeping the newest ``reentryRequestsKept()``. Returns the
    /// next list and the KeyPackages dropped (whose private keys the caller
    /// forgets with `Identity.forgetKeyPackage`).
    public static func keeping(
        _ requests: [ReentrySentRequest], adding request: ReentrySentRequest,
        kept: Int = Int(reentryRequestsKept())
    ) -> (requests: [ReentrySentRequest], dropped: [Data]) {
        let all = requests + [request]
        let keep = max(1, kept)
        guard all.count > keep else { return (all, []) }
        return (Array(all.suffix(keep)), all.prefix(all.count - keep).map(\.keyPackage))
    }

    /// Whether a send while behind goes out. Once confirmed nothing is sent
    /// (UX 2.2); while only suspected the person's own content still is (its
    /// `401`, if it comes, is evidence), but no repeat envelope, typing or
    /// read receipt: each would be sealed at a stale epoch nobody can open
    /// (section 2). `isPersonContent` is a publish that carries a wake.
    ///
    /// Walk-3 rule W3-R2: `readRefused` (a read of this journal was refused
    /// with an unskewed `401` and no read has been answered since) holds every
    /// automatic send even while still in step, since the refusal may be the
    /// first sign of a commit this device missed. The person's own content is
    /// unchanged.
    ///
    /// Walk-3 rule W3-R5: `personAction` (the person's own act that is not a
    /// message: an edit, a deletion, a reaction, a content op, a shared pin, a
    /// group setting, a contact request inside the group) is held ONLY while
    /// the conversation is suspected or confirmed behind, never on a refused
    /// read alone: it is attempted, and its `401` is evidence.
    public static func sendAllowed(
        behind: ReentryBehind, isPersonContent: Bool, readRefused: Bool = false, personAction: Bool = false
    ) -> Bool {
        if personAction { return !behind.stopsActing }
        if readRefused, !isPersonContent { return false }
        switch behind {
        case .inStep: return true
        case .suspected: return isPersonContent
        case .confirmed: return false
        }
    }

    /// Journal repair (ADR-0057) must not Replace while there is evidence the
    /// epochs above are real (section 2).
    public static func blocksRepair(_ record: ConversationReentryRecord) -> Bool {
        record.behind != .inStep && record.evidence
    }

    // MARK: The gap line (UX 2.4)

    /// The local row the gap line is stored as. The `[mls ` prefix keeps it
    /// out of every history bundle (``HistoryBundleCodec/isBookkeepingSystemRow(_:)``):
    /// the line is this device's alone.
    public static func gapMarker(fromMs: UInt64, toMs: UInt64) -> String {
        "[mls reentry-gap from=\(fromMs) to=\(toMs)]"
    }

    /// The two times a stored gap row records, or `nil` when it is not one.
    public static func parseGap(_ text: String) -> (fromMs: UInt64, toMs: UInt64)? {
        let prefix = "[mls reentry-gap from="
        guard text.hasPrefix(prefix), text.hasSuffix("]") else { return nil }
        let body = text.dropFirst(prefix.count).dropLast()
        let parts = body.split(separator: " ")
        guard parts.count == 2, parts[1].hasPrefix("to="),
              let from = UInt64(parts[0]), let to = UInt64(parts[1].dropFirst(3)), from <= to
        else { return nil }
        return (from, to)
    }

    /// The sentence of a gap row (UX 2.4): a plain fact, no times, no
    /// mention of this device (maintainer, 2026-11-20).
    static func gapSentence(
        text: (String, [String: String]) -> String = { Localization.text($0, texts: $1) }
    ) -> String {
        text("chat.reentry.gap", [:])
    }

    /// Walk-2 rule R1 — where the gap starts: the first row this device
    /// skipped, but never later than when it last knew the conversation (the
    /// newest row it applied, else its last good read, else when it first
    /// suspected, else now). Never in the future. The request carries this
    /// value as `gap_from_ms`, and the gap line starts at it.
    public static func gapStart(
        firstSkippedMs: UInt64?, lastKnownRowMs: UInt64?, lastReadOkMs: UInt64?,
        suspicionSinceMs: UInt64?, nowMs: UInt64
    ) -> UInt64 {
        let known = lastKnownRowMs ?? lastReadOkMs ?? suspicionSinceMs ?? nowMs
        return min(firstSkippedMs.map { min($0, known) } ?? known, nowMs)
    }

    /// Walk-2 rule R2 — where a gap line starting at `fromMs` goes among the
    /// conversation's rows: after the last row at or before its start, i.e.
    /// before the first row stamped later than `fromMs`; at the end when
    /// there is none. A row with no readable stamp does not decide.
    static func gapLineIndex(_ messages: [ChatMessage], fromMs: UInt64) -> Int {
        messages.firstIndex { message in
            guard let ms = ChatRepository.parseDsTimestampMs(message.createdAt), ms >= 0 else { return false }
            return UInt64(ms) > fromMs
        } ?? messages.count
    }

    /// Walk-2 rule R4 — whether the knock key is due to the members: never
    /// delivered, delivered at another version, or a current member was not
    /// in the set at the last delivery.
    public static func knockKeySendDue(
        mark: ReentryKnockKeySentMark?, version: UInt16, members: [String]
    ) -> Bool {
        guard let mark, mark.version == version else { return true }
        let delivered = Set(mark.members)
        return members.contains { !delivered.contains($0) }
    }

    // MARK: The gap history (section 7)

    /// The exporter label of the gap history secret. A client convention:
    /// the core defines no carrier for this bundle.
    public static let historyExporterLabel = "lakko/conversation-reentry/history/v1"

    /// The exporter context: the returning leaf's signature key. A device
    /// that re-requested at one epoch holds two KeyPackages for it and cannot
    /// know which one the re-admitter used; the leaf key is the same for both.
    public static func historyContext(leafKey: Data) -> Data { leafKey }

    /// The exporter length of the history secret.
    public static let historySecretLength: UInt32 = 32

    /// The rendezvous mailbox the bundle is posted to, derived from the
    /// secret so only members of the new epoch can name it.
    public static func historyRendezvous(secret: Data) -> Data {
        var input = Data("lakko/conversation-reentry/history-rendezvous/v1".utf8)
        input.append(secret)
        return Data(SHA256.hash(data: input))
    }

    /// The rows of a gap history: the conversation's own rows from
    /// `gapFromMs` on, without bookkeeping rows, the newest at most
    /// ``HistoryBundleCodec/maxBundleMessages``.
    static func gapRows(_ rows: [PersistedMessage], gapFromMs: UInt64) -> [PersistedMessage] {
        let carried = HistoryBundleCodec.bundledMessages(rows).filter { row in
            guard row.messageId > 0 else { return false }
            guard let ms = ChatRepository.parseDsTimestampMs(row.createdAt) else { return false }
            return ms >= Int64(clamping: gapFromMs)
        }
        return Array(carried.suffix(HistoryBundleCodec.maxBundleMessages))
    }

    /// Whether an owed history is still owed at `nowMs`.
    public static func historyStillOwed(_ owed: ReentryOwedHistory, nowMs: UInt64) -> Bool {
        nowMs < owed.owedAtMs &+ ReentryHistoryOwed.lifetimeMs
    }

    /// Whether the sender held a row older than the gap's start: without one
    /// it cannot vouch that it saw the start of the gap, and the bundle is
    /// marked `countCapped` (the web rule).
    static func gapHeldBefore(_ rows: [PersistedMessage], gapFromMs: UInt64) -> Bool {
        HistoryBundleCodec.bundledMessages(rows).contains { row in
            guard let ms = ChatRepository.parseDsTimestampMs(row.createdAt) else { return false }
            return ms < Int64(clamping: gapFromMs)
        }
    }

    // MARK: The gap look of a returning device

    /// `GAP_HISTORY_LOOK_INTERVAL_MS`: at most one look per group per
    /// minute. Chosen, not measured.
    public static let gapHistoryLookIntervalMs: UInt64 = 60_000

    /// `GAP_HISTORY_LOOK_RETENTION_MS`: a look is kept 30 days from the gap's
    /// end, the Delivery Service's retention. Chosen, not measured.
    public static let gapHistoryLookRetentionMs: UInt64 = 30 * 24 * 60 * 60 * 1000

    /// What a look for `look` does at `nowMs`.
    public static func gapLookVerdict(_ look: ReentryGapLook, nowMs: UInt64) -> ReentryGapLookVerdict {
        if nowMs >= look.gapToMs &+ gapHistoryLookRetentionMs { return .drop }
        if let last = look.lastLookMs, nowMs < last &+ gapHistoryLookIntervalMs { return .wait }
        return .look
    }

    /// What a received gap history does to a stored row's `text` (the web
    /// `narrowGapLine` rule): only the gap line ending at `gapToMs` moves; a
    /// complete bundle removes it; otherwise it ends at the earliest row the
    /// bundle carried, and goes when that is at or before its start.
    public static func narrowGap(
        text: String, gapToMs: UInt64, complete: Bool, earliestMs: Int64?
    ) -> ReentryGapNarrowing {
        guard let gap = parseGap(text), gap.toMs == gapToMs else { return .keep }
        if complete { return .remove }
        guard let earliestMs, earliestMs >= 0, UInt64(earliestMs) < gap.toMs else { return .keep }
        let earliest = UInt64(earliestMs)
        if earliest <= gap.fromMs { return .remove }
        return .replace(gapMarker(fromMs: gap.fromMs, toMs: earliest))
    }

    // MARK: The group state re-sent after a replacement

    /// Whether the re-admitter also re-sends the push routing secret after a
    /// replacement: only an owner or an admin (rule 3 of the walk fix). A
    /// plain member, a route-1 sibling that is a plain member, and the other
    /// person of a one-to-one never send it.
    public static func resendsPushRoutingSecret(ownRole: Role?) -> Bool {
        ownRole == .owner || ownRole == .admin
    }

    /// A gap bundle row with no author: its direction on this device, read
    /// from the sender's and the route of the request (the Android
    /// `reentryBundleDirection`). The same account keeps it, a one-to-one's
    /// other person swaps it, a group's re-admitter reads as incoming. A
    /// system row stays one.
    public static func bundleDirection(route: String?, senderDirection: String) -> String {
        if senderDirection == "system" { return "system" }
        switch route {
        case "own-device": return senderDirection
        case "mailbox": return senderDirection == "outgoing" ? "incoming" : "outgoing"
        default: return "incoming"
        }
    }

    // MARK: Quiet refusals

    /// Whether `error` is the refusal of a send this device must not make
    /// while behind (section 2), possibly wrapped in ``ChatError/wrapped(_:underlying:)``.
    /// Such a refusal is the stop list working, not a failure to log.
    public static func isBehindRefusal(_ error: Error) -> Bool {
        var current: Error = error
        for _ in 0..<8 {
            if current is ChatRepository.BehindInConversation { return true }
            guard case ChatError.wrapped(_, let underlying) = current else { return false }
            current = underlying
        }
        return false
    }
}

// MARK: - Log lines

/// The log lines of phase 2. Plain text, no identifier and no content.
public enum ReentryLog {
    private static let logger = Logger(subsystem: "me.lakko", category: "chat-reentry")

    /// Replaceable by tests.
    nonisolated(unsafe) public static var sink: (String) -> Void = { line in
        ReentryLog.logger.notice("\(line, privacy: .public)")
    }

    public static func emit(_ line: String) { sink(line) }

    static func state(_ behind: ReentryBehind) -> String {
        switch behind {
        case .inStep: return "in-step"
        case .suspected: return "suspected"
        case .confirmed: return "confirmed"
        }
    }
}
