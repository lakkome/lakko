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

    /// Shown: the composer is replaced by the waiting note (UX 2.2).
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
/// (the waiting note survives a restart, UX 4), per account, behind the store
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
    /// The newest `latest_epoch` a read of this journal named.
    public var latestSeen: UInt64?
    /// The requests sent, oldest first, at most ``reentryRequestsKept()``.
    public var requests: [ReentrySentRequest] = []
    /// When the last request went out (section 3's 30-minute rule).
    public var lastSentMs: UInt64?
    /// The route the last request took (for the second line of UX 2.2).
    public var lastRoute: String?

    public init() {}

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

    /// The groups with a record (the launch pass and the waiting notes).
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
    }

    private let lock = NSLock()
    private var entries: [String: Entry] = [:]
    private let nowMs: () -> UInt64

    public init(nowMs: @escaping () -> UInt64 = { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }) {
        self.nowMs = nowMs
    }

    /// A row applied: the failures before it are no longer "since the last
    /// applied row".
    public func noteApplied(groupIdHex: String) {
        lock.lock(); defer { lock.unlock() }
        entries[groupIdHex, default: Entry()].failedRowAboveLocal = false
        entries[groupIdHex]?.firstFailedRowAtMs = nil
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
    /// How long an owed history waits for its epoch. Chosen, not measured:
    /// a replacement is normally swept within minutes.
    public static let lifetimeMs: UInt64 = 24 * 60 * 60 * 1000

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

// MARK: - The rules

public enum ConversationReentryRules {

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
        if next.gapFromMs == nil, next.behind != .inStep, let first = evidence.firstFailedRowAtMs {
            next.gapFromMs = first
        }
        let cleared = record.behind != .inStep && next.behind == .inStep
        if cleared {
            next.evidence = false
            next.gapFromMs = nil
            next.latestSeen = nil
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
    public static func sendAllowed(behind: ReentryBehind, isPersonContent: Bool) -> Bool {
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

    // MARK: The waiting note (UX 2.2)

    /// Who can bring this device back, for the second line.
    public enum Waiter: Equatable {
        case ownDevice
        case person(name: String)
        case ownerOrAdmin
    }

    /// The second line appears only after this long (UX 2.2). Chosen, not
    /// measured.
    public static let secondLineAfterMs: UInt64 = 30_000

    /// The note's lines, or `nil` when the chat is not waiting.
    static func note(
        behind: ReentryBehind, waiter: Waiter?, nowMs: UInt64,
        text: (String, [String: String]) -> String = { Localization.text($0, texts: $1) }
    ) -> [String]? {
        guard case .confirmed(let since) = behind else { return nil }
        var lines = [text("chat.reentry.waiting", [:])]
        if let waiter, nowMs >= since &+ secondLineAfterMs {
            switch waiter {
            case .ownDevice: lines.append(text("chat.reentry.second.own_device", [:]))
            case .person(let name): lines.append(text("chat.reentry.second.person", ["name": name]))
            case .ownerOrAdmin: lines.append(text("chat.reentry.second.group", [:]))
            }
        }
        return lines
    }

    // MARK: What a waiting chat offers (UX 2.2)

    /// The message menu of a waiting chat: Copy, Forward, Save and Pin (for
    /// me; the sheet offers no shared pin, ``ChatRepository/maySetSharedPin(groupIdHex:)``).
    /// Everything that would change the chat or send is absent.
    static let waitingMenuActions: Set<MessageMenuAction> = [
        .copy, .copyCoordinates, .forward, .save, .unsave, .pin, .unpin,
    ]

    /// `actions` with only what a waiting chat offers, in their order.
    static func waitingMenu(_ actions: [MessageMenuAction]) -> [MessageMenuAction] {
        actions.filter { waitingMenuActions.contains($0) }
    }

    /// The info blocks a waiting chat hides: every control that would change
    /// the chat (UX 2.2). The chat shows what this device last knew.
    static let waitingHiddenInfoSections: Set<InfoSection> = [
        .call, .renameGroup, .addMember, .inviteLink, .roles, .transferOwnership, .leaveGroup,
        .groupType, .deleteGroup, .invitePeople, .reactions,
    ]

    /// `sections` with the changing blocks left out, in their order.
    static func waitingInfoSections(_ sections: [InfoSection]) -> [InfoSection] {
        sections.filter { !waitingHiddenInfoSections.contains($0) }
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

    /// The sentence of a gap row: the date once when both times fall on the
    /// same day.
    static func gapSentence(
        fromMs: UInt64, toMs: UInt64, calendar: Calendar = .current, locale: Locale = .current,
        text: (String, [String: String]) -> String = { Localization.text($0, texts: $1) }
    ) -> String {
        let from = Date(timeIntervalSince1970: TimeInterval(fromMs) / 1000)
        let to = Date(timeIntervalSince1970: TimeInterval(toMs) / 1000)
        func formatter(_ template: String) -> DateFormatter {
            let f = DateFormatter()
            f.locale = locale
            f.calendar = calendar
            f.timeZone = calendar.timeZone
            f.setLocalizedDateFormatFromTemplate(template)
            return f
        }
        let day = formatter("d MMM")
        let time = formatter("HH:mm")
        // "11 Nov, 13:10" (UX 2.4); on the same day the date is shown once.
        func full(_ date: Date) -> String { "\(day.string(from: date)), \(time.string(from: date))" }
        let sameDay = calendar.isDate(from, inSameDayAs: to)
        return text("chat.reentry.gap", [
            "from": full(from),
            "to": sameDay ? time.string(from: to) : full(to),
        ])
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
