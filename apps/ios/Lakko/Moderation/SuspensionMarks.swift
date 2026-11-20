// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import os

/// ADR-0089 step 4 / UX `reporting-and-moderation.md` section 3 — whether
/// another person's account is suspended, as the moderation log says.
///
/// The service publishes a daily snapshot of active suspensions in 65,536
/// prefix buckets; this asks for the one bucket an account falls in
/// (`GET /moderation/snapshot/latest/bucket/{prefix}`, anonymous) and checks
/// the answer with `moderationAccountStanding`, which verifies the log's
/// signed and witnessed head, the snapshot's inclusion in it, the bucket's
/// inclusion in the snapshot, and its freshness, against the pinned log key
/// (`Info.plist` `LakkoModerationLogKey`) and the transparency witness pins.
///
/// A suspension the device cannot confirm is not shown: anything that is
/// not a verified `suspended` (no pin, no network, an answer that does not
/// verify) reads as ``Standing/unknown``, which every mark site treats as
/// not suspended. The mark is a warning, not access control.
///
/// A verified `suspended` is not enough on its own (security fix F2): the
/// snapshot is built by the moderation service, so a `suspended` is acted on
/// only once the moderator's signed statement behind the entry is proven
/// too: the log entry at the bucket's `logIndex` (`GET
/// /moderation/log/entry/{logIndex}?size={root.log_size}`), the moderator
/// list (`GET /moderation/moderators`, admitted under the pinned root key
/// and the device's moderator-list floor as the report sheet admits it,
/// held in memory, re-admitted at each use; an older list is not used) and
/// `moderationProveAccountStanding` / `moderationProveChannelStanding`.
/// A `suspended` that is not proven (no root key pinned, no `logIndex`, a
/// fetch or the proof failed) reads as ``Standing/unknown``. The proof is
/// held with the answer, until the snapshot's `reuseUntilMs`.
///
/// Answers are held in memory only, per prefix, until the snapshot's
/// `reuseUntilMs`; one request per prefix is in flight at a time, and a
/// prefix whose fetch failed is not asked again for ten minutes. Nothing is
/// written to disk, so when a suspension ends nothing about it is kept.
/// This device's own account is never asked about (its own standing is the
/// step-3 suspended screen). Logs carry no key, prefix or identifier.
@MainActor
public final class SuspensionMarks: ObservableObject {

    public static let shared = SuspensionMarks(ports: .live)

    /// The Info.plist key carrying the pinned moderation log operator key
    /// (base64url, 32 bytes).
    public nonisolated static let logKeyInfoPlistKey = "LakkoModerationLogKey"

    /// How long a prefix whose fetch failed is left alone (chosen).
    public nonisolated static let failureBackoffMs: Int64 = 600_000

    /// How long a contact request's arrival waits for a verdict (chosen).
    public nonisolated static let requestCheckBoundMs: Int64 = 3_000

    /// What the log says about one account.
    public enum Standing: Equatable {
        case suspended(untilMs: Int64?)
        case notSuspended
        /// Could not be confirmed either way. Treated as not suspended.
        case unknown

        public var isSuspended: Bool {
            if case .suspended = self { return true }
            return false
        }
    }

    /// The seams of the proof behind a `suspended` (F2), injectable for
    /// tests. ``unavailable`` proves nothing, so every `suspended` reads as
    /// unknown.
    public struct ProofPorts {
        /// The pinned moderation root key, or `nil` (then nothing is proven).
        public var rootKey: () -> Data?
        /// `GET /moderation/moderators`: the raw list, or `nil`.
        public var moderatorList: () async -> Data?
        /// Whether `list` verifies under `rootKey` and is not older than this
        /// device's moderator-list floor (the one the report sheet keeps,
        /// ``ModeratorListGate``), raising the floor on a newer one. Asked
        /// every time a list is used, held or fetched, so a server cannot
        /// hand an old list that still names a since-revoked moderator.
        public var admitList: (_ list: Data, _ rootKey: Data) -> Bool
        /// `GET /moderation/log/entry/{logIndex}?size={logSize}`, or `nil`.
        public var fetchEntry: (_ logIndex: UInt64, _ logSize: UInt64) async -> String?
        public var proveAccount: (
            _ answer: String, _ entry: String, _ list: Data, _ rootKey: Data,
            _ accountKey: Data, _ nowMs: Int64
        ) throws -> Void
        public var proveChannel: (
            _ answer: String, _ entry: String, _ list: Data, _ rootKey: Data,
            _ channelId: Data, _ nowMs: Int64
        ) throws -> Void

        public init(
            rootKey: @escaping () -> Data?,
            moderatorList: @escaping () async -> Data?,
            admitList: @escaping (Data, Data) -> Bool,
            fetchEntry: @escaping (UInt64, UInt64) async -> String?,
            proveAccount: @escaping (String, String, Data, Data, Data, Int64) throws -> Void,
            proveChannel: @escaping (String, String, Data, Data, Data, Int64) throws -> Void
        ) {
            self.rootKey = rootKey
            self.moderatorList = moderatorList
            self.admitList = admitList
            self.fetchEntry = fetchEntry
            self.proveAccount = proveAccount
            self.proveChannel = proveChannel
        }

        /// Proves nothing: every `suspended` reads as unknown.
        public static var unavailable: ProofPorts {
            ProofPorts(
                rootKey: { nil },
                moderatorList: { nil },
                admitList: { _, _ in false },
                fetchEntry: { _, _ in nil },
                proveAccount: { _, _, _, _, _, _ in throw ProofUnavailable() },
                proveChannel: { _, _, _, _, _, _ in throw ProofUnavailable() })
        }

        public static var live: ProofPorts {
            ProofPorts(
                rootKey: { ModerationClient.pinnedRootKey },
                moderatorList: { await SuspensionMarks.fetchModeratorList() },
                admitList: { list, root in ModeratorListGate.admit(list, rootKey: root) != nil },
                fetchEntry: { await SuspensionMarks.fetchLogEntry(logIndex: $0, logSize: $1) },
                proveAccount: { answer, entry, list, root, key, now in
                    try moderationProveAccountStanding(
                        answerJson: answer, entryJson: entry, moderatorList: list,
                        moderationRootKey: root, accountKey: key, nowMs: now)
                },
                proveChannel: { answer, entry, list, root, channel, now in
                    try moderationProveChannelStanding(
                        answerJson: answer, entryJson: entry, moderatorList: list,
                        moderationRootKey: root, channelId: channel, nowMs: now)
                })
        }
    }

    public struct ProofUnavailable: Error {}

    /// The seams, injectable for tests.
    public struct Ports {
        /// The pinned log operator key, or `nil` (then nothing is asked).
        public var logKey: () -> Data?
        public var witnessKeys: () -> [Data]
        public var witnessThreshold: () -> UInt32
        public var prefix: (_ accountKey: Data) throws -> String
        /// The raw answer for a prefix, or `nil` on any failure.
        public var fetch: (_ prefix: String) async -> String?
        public var verify: (
            _ answer: String, _ accountKey: Data, _ logKey: Data,
            _ witnessKeys: [Data], _ threshold: UInt32, _ nowMs: Int64
        ) throws -> ModerationStanding
        public var nowMs: () -> Int64
        /// The current key of the person a key belongs to (ADR-0074).
        public var currentKey: @MainActor (Data) -> Data
        /// Whether a key is this device's own account (any of its keys).
        public var isOwn: @MainActor (Data) -> Bool
        public var sleepMs: (Int64) async -> Void
        /// ADR-0089 step 5 — the bucket prefix of a 16-byte channel id.
        public var channelPrefix: (_ channelId: Data) throws -> String
        /// ADR-0089 step 5 — check an answer for a channel (`suspended` means
        /// removed).
        public var verifyChannel: (
            _ answer: String, _ channelId: Data, _ logKey: Data,
            _ witnessKeys: [Data], _ threshold: UInt32, _ nowMs: Int64
        ) throws -> ModerationStanding
        /// F2 — the proof behind a `suspended`.
        public var proof: ProofPorts

        public init(
            logKey: @escaping () -> Data?,
            witnessKeys: @escaping () -> [Data],
            witnessThreshold: @escaping () -> UInt32,
            prefix: @escaping (Data) throws -> String,
            fetch: @escaping (String) async -> String?,
            verify: @escaping (String, Data, Data, [Data], UInt32, Int64) throws -> ModerationStanding,
            nowMs: @escaping () -> Int64,
            currentKey: @escaping @MainActor (Data) -> Data,
            isOwn: @escaping @MainActor (Data) -> Bool,
            sleepMs: @escaping (Int64) async -> Void,
            channelPrefix: @escaping (Data) throws -> String = { try moderationChannelBucketPrefix(channelId: $0) },
            verifyChannel: @escaping (String, Data, Data, [Data], UInt32, Int64) throws -> ModerationStanding = {
                answer, channel, logKey, witnesses, threshold, now in
                try moderationChannelStanding(
                    answerJson: answer, channelId: channel, logOperatorKey: logKey,
                    witnessKeys: witnesses, witnessThreshold: threshold, nowMs: now)
            },
            proof: ProofPorts = .unavailable
        ) {
            self.logKey = logKey
            self.witnessKeys = witnessKeys
            self.witnessThreshold = witnessThreshold
            self.prefix = prefix
            self.fetch = fetch
            self.verify = verify
            self.nowMs = nowMs
            self.currentKey = currentKey
            self.isOwn = isOwn
            self.sleepMs = sleepMs
            self.channelPrefix = channelPrefix
            self.verifyChannel = verifyChannel
            self.proof = proof
        }

        public static var live: Ports {
            Ports(
                logKey: { SuspensionMarks.pinnedLogKey },
                witnessKeys: { TransparencyConfig.pinnedWitnessKeys },
                witnessThreshold: { UInt32(clamping: TransparencyConfig.witnessThreshold) },
                prefix: { try moderationBucketPrefix(accountKey: $0) },
                fetch: { await SuspensionMarks.fetchBucket(prefix: $0) },
                verify: { answer, key, logKey, witnesses, threshold, now in
                    try moderationAccountStanding(
                        answerJson: answer, accountKey: key, logOperatorKey: logKey,
                        witnessKeys: witnesses, witnessThreshold: threshold, nowMs: now)
                },
                nowMs: { Int64(Date().timeIntervalSince1970 * 1000) },
                currentKey: { ReachCenter.shared.keyResolver.current(of: $0) },
                isOwn: { key in
                    if let own = ReachCenter.shared.ownAccountKey, own == key { return true }
                    return ReachCenter.shared.keyResolver.isOwn(key)
                },
                sleepMs: { ms in try? await Task.sleep(nanoseconds: UInt64(max(0, ms)) * 1_000_000) },
                proof: .live
            )
        }
    }

    /// The pinned log operator key, or `nil` when the build carries none or
    /// it is malformed (then no mark is shown anywhere and nothing is asked).
    public nonisolated static var pinnedLogKey: Data? {
        ModerationClient.rootKey(
            fromInfoValue: Bundle.main.object(forInfoDictionaryKey: logKeyInfoPlistKey) as? String)
    }

    /// The accounts known suspended, by current key, with the end of the
    /// suspension (`nil` for no end). Screens read ``isSuspended(_:)``.
    @Published public private(set) var marks: [Data: Int64?] = [:]

    private let ports: Ports
    private var answers: [String: (json: String, reuseUntilMs: Int64)] = [:]
    private var failedUntil: [String: Int64] = [:]
    private var inFlight: [String: Task<String?, Never>] = [:]
    private var results: [Data: (standing: Standing, validUntilMs: Int64)] = [:]
    /// ADR-0089 step 5 — channel answers, by 16-byte channel id.
    private var channelResults: [Data: (standing: Standing, validUntilMs: Int64)] = [:]
    /// F2 — the moderator list admitted under the root key, held in memory.
    private var heldModeratorList: Data?
    /// F2 — one proof per target in flight (account keys are 32 bytes,
    /// channel ids 16, so the two never collide).
    private var proofsInFlight: [Data: Task<Bool, Never>] = [:]
    /// Moved on by ``reset()``: an answer that arrives for an earlier
    /// generation writes nothing.
    private var generation: UInt64 = 0
    private static let log = Logger(subsystem: "me.lakko.app", category: "suspension-marks")

    public init(ports: Ports) {
        self.ports = ports
    }

    /// Whether the account behind `key` is known suspended now. Never asks
    /// the network; ``check(_:)`` does.
    public func isSuspended(_ key: Data) -> Bool {
        guard key.count == 32, ports.logKey() != nil else { return false }
        let account = ports.currentKey(key)
        guard !ports.isOwn(key), !ports.isOwn(account), let entry = marks[account] else { return false }
        guard let until = entry else { return true }
        return until > ports.nowMs()
    }

    /// Ask about each key in the background; the marks follow.
    public func check(_ keys: [Data]) {
        guard ports.logKey() != nil else { return }
        for key in Set(keys) {
            Task { _ = await self.standing(key) }
        }
    }

    /// Whether a contact request from `key` is dropped: only a verified
    /// `suspended`, waited for at most `boundMs`. On timeout or anything
    /// else the request is shown as before.
    public func verifiedSuspended(_ key: Data, boundMs: Int64 = SuspensionMarks.requestCheckBoundMs) async -> Bool {
        await verifiedSuspended([key], boundMs: boundMs).contains(key)
    }

    /// Of `keys`, the ones verified suspended within ONE bound of `boundMs`
    /// for all of them together: every key is asked about at once, and when
    /// the bound ends the answers in so far are what counts. A key whose
    /// answer is late is not in the set (its request is shown as before); the
    /// late answer still marks it.
    public func verifiedSuspended(_ keys: [Data], boundMs: Int64 = SuspensionMarks.requestCheckBoundMs) async -> Set<Data> {
        let distinct = Array(Set(keys.filter { $0.count == 32 }))
        guard ports.logKey() != nil, !distinct.isEmpty else { return [] }
        return await withCheckedContinuation { (continuation: CheckedContinuation<Set<Data>, Never>) in
            let gather = Gather(continuation, expecting: distinct.count)
            for key in distinct {
                Task { @MainActor in
                    let standing = await self.standing(key)
                    gather.answer(key, suspended: standing.isSuspended)
                }
            }
            Task { @MainActor in
                await self.ports.sleepMs(boundMs)
                gather.finish()
            }
        }
    }

    /// The standing of the account behind `key` (32 bytes).
    public func standing(_ key: Data) async -> Standing {
        guard let logKey = ports.logKey(), key.count == 32 else { return .unknown }
        let account = ports.currentKey(key)
        guard account.count == 32, !ports.isOwn(key), !ports.isOwn(account) else { return .unknown }
        let now = ports.nowMs()
        let started = generation
        if let held = results[account], now < held.validUntilMs { return held.standing }
        guard let prefix = try? ports.prefix(account) else { return settle(account, .unknown) }

        guard let got = await answer(for: prefix, now: now, started: started) else {
            guard generation == started else { return .unknown }
            return settle(account, .unknown)
        }
        let (answer, fresh) = got

        let checkedAt = ports.nowMs()
        do {
            let verified = try ports.verify(
                answer, account, logKey, ports.witnessKeys(), ports.witnessThreshold(), checkedAt)
            if answers[prefix]?.json != answer {
                answers[prefix] = (answer, verified.reuseUntilMs)
                failedUntil[prefix] = nil
            }
            var standing: Standing = verified.suspended ? .suspended(untilMs: verified.untilMs) : .notSuspended
            var validUntil = verified.reuseUntilMs
            if verified.suspended {
                let proven = await proveSuspended(
                    answer: answer, logIndex: verified.logIndex, target: account, isChannel: false)
                guard generation == started else { return .unknown }
                if !proven {
                    standing = .unknown
                    validUntil = min(validUntil, ports.nowMs() + Self.failureBackoffMs)
                }
            }
            results[account] = (standing, validUntil)
            return settle(account, standing)
        } catch {
            if fresh { noteUnverified(prefix) }
            return settle(account, .unknown)
        }
    }

    /// ADR-0089 step 5 — what the log says about a removed channel, and when
    /// to ask again: the snapshot's `reuseUntilMs` when the answer verified,
    /// else ``failureBackoffMs`` from now. `.suspended` means verified
    /// removed; anything not verified is `.unknown` (C-92: a removal the log
    /// did not prove is never claimed).
    public func channelCheck(_ channelId: Data) async -> (standing: Standing, recheckAtMs: Int64) {
        let backoff = { self.ports.nowMs() + Self.failureBackoffMs }
        guard let logKey = ports.logKey(), channelId.count == 16 else { return (.unknown, backoff()) }
        let now = ports.nowMs()
        let started = generation
        if let held = channelResults[channelId], now < held.validUntilMs {
            return (held.standing, held.validUntilMs)
        }
        guard let prefix = try? ports.channelPrefix(channelId),
              let got = await answer(for: prefix, now: now, started: started),
              generation == started
        else { return (.unknown, backoff()) }
        let (answer, fresh) = got
        do {
            let verified = try ports.verifyChannel(
                answer, channelId, logKey, ports.witnessKeys(), ports.witnessThreshold(), ports.nowMs())
            if answers[prefix]?.json != answer {
                answers[prefix] = (answer, verified.reuseUntilMs)
                failedUntil[prefix] = nil
            }
            var standing: Standing = verified.suspended ? .suspended(untilMs: nil) : .notSuspended
            var validUntil = verified.reuseUntilMs
            if verified.suspended {
                let proven = await proveSuspended(
                    answer: answer, logIndex: verified.logIndex, target: channelId, isChannel: true)
                guard generation == started else { return (.unknown, backoff()) }
                if !proven {
                    standing = .unknown
                    validUntil = min(validUntil, backoff())
                }
            }
            channelResults[channelId] = (standing, validUntil)
            return (standing, validUntil)
        } catch {
            if fresh { noteUnverified(prefix) }
            return (.unknown, backoff())
        }
    }

    /// F2 — whether the moderator's statement behind a verified `suspended`
    /// for `target` (an account key, or a channel id when `isChannel`) is
    /// proven. Anything short of a proof is `false`: no root key pinned, no
    /// `logIndex`, no `root.log_size` in the answer, a fetch that failed, or
    /// a proof that did not hold. One proof per target in flight.
    private func proveSuspended(answer: String, logIndex: UInt64?, target: Data, isChannel: Bool) async -> Bool {
        if let running = proofsInFlight[target] { return await running.value }
        let task = Task { @MainActor [weak self] () -> Bool in
            guard let self else { return false }
            return await self.runProof(answer: answer, logIndex: logIndex, target: target, isChannel: isChannel)
        }
        proofsInFlight[target] = task
        let result = await task.value
        if proofsInFlight[target] == task { proofsInFlight[target] = nil }
        return result
    }

    private func runProof(answer: String, logIndex: UInt64?, target: Data, isChannel: Bool) async -> Bool {
        let started = generation
        guard let rootKey = ports.proof.rootKey(), rootKey.count == 32,
              let logIndex, let logSize = Self.answerLogSize(answer) else { return false }
        guard let entry = await ports.proof.fetchEntry(logIndex, logSize), generation == started else {
            if generation == started { Self.log.info("moderation log entry fetch failed") }
            return false
        }
        // The list held in memory is used only while it is still admitted
        // (a newer one may have raised the floor since); else it is fetched
        // again, and a fetched list older than the floor is not used.
        let list: Data
        if let held = heldModeratorList, ports.proof.admitList(held, rootKey) {
            list = held
        } else {
            heldModeratorList = nil
            guard let fetched = await ports.proof.moderatorList(), generation == started else {
                if generation == started { Self.log.info("moderator list unavailable") }
                return false
            }
            guard ports.proof.admitList(fetched, rootKey) else {
                Self.log.info("moderator list not admitted")
                return false
            }
            heldModeratorList = fetched
            list = fetched
        }
        do {
            if isChannel {
                try ports.proof.proveChannel(answer, entry, list, rootKey, target, ports.nowMs())
            } else {
                try ports.proof.proveAccount(answer, entry, list, rootKey, target, ports.nowMs())
            }
            return true
        } catch {
            // A list held from before may be the reason (a moderator added
            // since): the next proof fetches it again.
            heldModeratorList = nil
            Self.log.info("moderation standing not proven")
            return false
        }
    }

    /// The `root.log_size` of a bucket answer, or `nil`.
    nonisolated static func answerLogSize(_ answer: String) -> UInt64? {
        struct Answer: Decodable {
            struct Root: Decodable {
                let logSize: UInt64
                enum CodingKeys: String, CodingKey { case logSize = "log_size" }
            }
            let root: Root
        }
        return (try? JSONDecoder().decode(Answer.self, from: Data(answer.utf8)))?.root.logSize
    }

    /// A channel answered normally again (a restore): its held verdict goes.
    public func forgetChannel(_ channelId: Data) {
        channelResults[channelId] = nil
    }

    /// The answer for `prefix`: the one held while reusable, else fetched
    /// (one request per prefix in flight). `nil` on a failure, inside a
    /// failure backoff, or when ``reset()`` ran meanwhile. `fresh` says this
    /// call made the request.
    private func answer(for prefix: String, now: Int64, started: UInt64) async -> (String, Bool)? {
        if let cached = answers[prefix], now < cached.reuseUntilMs { return (cached.json, false) }
        if let until = failedUntil[prefix], now < until { return nil }
        var fresh = false
        let task: Task<String?, Never>
        if let running = inFlight[prefix] {
            task = running
        } else {
            let fetch = ports.fetch
            task = Task { await fetch(prefix) }
            inFlight[prefix] = task
            fresh = true
        }
        let fetched = await task.value
        guard generation == started else { return nil }
        if fresh { inFlight[prefix] = nil }
        guard let fetched else {
            if fresh {
                failedUntil[prefix] = ports.nowMs() + Self.failureBackoffMs
                answers[prefix] = nil
                Self.log.info("moderation snapshot fetch failed")
            }
            return nil
        }
        return (fetched, fresh)
    }

    /// An answer that does not verify is a failed fetch.
    private func noteUnverified(_ prefix: String) {
        answers[prefix] = nil
        failedUntil[prefix] = ports.nowMs() + Self.failureBackoffMs
        Self.log.info("moderation snapshot did not verify")
    }

    /// Drop everything held (sign-out, account switch).
    public func reset() {
        generation &+= 1
        for task in inFlight.values { task.cancel() }
        inFlight = [:]
        answers = [:]
        failedUntil = [:]
        results = [:]
        channelResults = [:]
        for task in proofsInFlight.values { task.cancel() }
        proofsInFlight = [:]
        heldModeratorList = nil
        if !marks.isEmpty { marks = [:] }
    }

    @discardableResult
    private func settle(_ account: Data, _ standing: Standing) -> Standing {
        switch standing {
        case .suspended(let until):
            if marks[account] != .some(until) { marks[account] = .some(until) }
        case .notSuspended, .unknown:
            if marks[account] != nil { marks[account] = nil }
        }
        return standing
    }

    /// `GET {moderation base}/moderation/snapshot/latest/bucket/{prefix}`,
    /// anonymous, through the anti-censorship transport when armed.
    nonisolated static func fetchBucket(
        prefix: String,
        baseURL: String = ApiEndpoints.baseURL(for: .moderation)
    ) async -> String? {
        guard isPrefix(prefix) else { return nil }
        var base = baseURL
        while base.hasSuffix("/") { base.removeLast() }
        guard let url = URL(string: base + "/moderation/snapshot/latest/bucket/" + prefix) else { return nil }
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        guard let (data, response) = try? await AntiCensorshipTransport.perform(
            request, session: AnonymousHTTP.session, config: .current),
              let http = response as? HTTPURLResponse, http.statusCode == 200 else { return nil }
        return String(data: data, encoding: .utf8)
    }

    /// F2 — `GET {moderation base}/moderation/log/entry/{logIndex}?size={logSize}`,
    /// anonymous, through the anti-censorship transport when armed.
    nonisolated static func fetchLogEntry(
        logIndex: UInt64, logSize: UInt64,
        baseURL: String = ApiEndpoints.baseURL(for: .moderation)
    ) async -> String? {
        var base = baseURL
        while base.hasSuffix("/") { base.removeLast() }
        guard let url = URL(string: base + "/moderation/log/entry/\(logIndex)?size=\(logSize)") else { return nil }
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        guard let (data, response) = try? await AntiCensorshipTransport.perform(
            request, session: AnonymousHTTP.session, config: .current),
              let http = response as? HTTPURLResponse, http.statusCode == 200 else { return nil }
        return String(data: data, encoding: .utf8)
    }

    /// F2 — the moderator list as the report sheet fetches it, or `nil`.
    /// Admission (``ModeratorListGate``) is ``ProofPorts/admitList``.
    nonisolated static func fetchModeratorList() async -> Data? {
        guard case .list(let list) = await ModerationClient().fetchModeratorList() else { return nil }
        return list
    }

    /// A bucket prefix is exactly four lower-case hex characters.
    nonisolated static func isPrefix(_ prefix: String) -> Bool {
        prefix.count == 4 && prefix.unicodeScalars.allSatisfy { ("0"..."9").contains($0) || ("a"..."f").contains($0) }
    }

    /// Collects answers until all are in or the bound ends, then resumes its
    /// continuation once. Every caller runs on the main actor.
    @MainActor
    private final class Gather {
        private var continuation: CheckedContinuation<Set<Data>, Never>?
        private var remaining: Int
        private var suspended: Set<Data> = []

        init(_ continuation: CheckedContinuation<Set<Data>, Never>, expecting count: Int) {
            self.continuation = continuation
            self.remaining = count
        }

        func answer(_ key: Data, suspended isSuspended: Bool) {
            guard continuation != nil else { return }
            if isSuspended { suspended.insert(key) }
            remaining -= 1
            if remaining == 0 { finish() }
        }

        func finish() {
            continuation?.resume(returning: suspended)
            continuation = nil
        }
    }
}

/// ADR-0089 step 4 — what each mark site shows, decided here so the tests
/// can run it (UX `reporting-and-moderation.md` section 3).
public enum SuspensionMarkRules {

    /// What sits under a person's name on their card.
    public enum CardLine: Equatable { case none, blocked, suspended }

    /// The one-to-one's top banner: a suspended peer, unless blocked (the
    /// block banner then stands alone).
    public static func showsOneToOneBanner(blocked: Bool, suspended: Bool) -> Bool {
        suspended && !blocked
    }

    /// The card's muted line: blocked wins over suspended.
    public static func cardLine(blocked: Bool, suspended: Bool) -> CardLine {
        if blocked { return .blocked }
        return suspended ? .suspended : .none
    }

    /// The member list's muted marker: never on this device's own row.
    public static func showsMemberMarker(isOwnRow: Bool, suspended: Bool) -> Bool {
        suspended && !isOwnRow
    }

    /// Search lists nobody known suspended; the rest keep their order.
    public static func searchVisible<T>(_ people: [T], key: (T) -> Data, isSuspended: (Data) -> Bool) -> [T] {
        people.filter { !isSuspended(key($0)) }
    }

    /// An invite link's page offers Add only when its inviter is not known
    /// suspended.
    public static func inviteOffersAdd(inviterSuspended: Bool) -> Bool {
        !inviterSuspended
    }

    /// Held requests are not shown while their sender is known suspended
    /// (kept, so they come back when the suspension ends).
    public static func visibleRequests(
        _ requests: [IncomingContactRequest], isSuspended: (Data) -> Bool
    ) -> [IncomingContactRequest] {
        requests.filter { !isSuspended($0.fromPubkey) }
    }

    /// What a tap on Add does: nothing is sent while the inviter's standing
    /// says suspended (`nil`); otherwise `send` runs. The standing is waited
    /// for at most its bound, so a tap before it answered cannot send to a
    /// suspended account.
    public static func addThroughInvite<Result>(
        inviter: Data,
        inviterSuspended: (Data) async -> Bool,
        send: () async -> Result
    ) async -> Result? {
        if await inviterSuspended(inviter) { return nil }
        return await send()
    }
}
