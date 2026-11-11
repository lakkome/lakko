// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0081 decision 4 — a device that lost or fell behind its DeviceGroup
/// rejoins through a sibling instead of re-founding.
///
/// Mirror of `apps/web/src/devices/device-group-rejoin.ts` and Android's
/// `DeviceGroupRejoin.kt`.
///
/// ## When a device rejoins
///
/// - **Lost:** it has no usable DeviceGroup state — no snapshot, or one that
///   does not decrypt under its wrap key or does not restore — while the
///   account's directory lists another own device
///   (``DeviceGroupLifecycle/NotJoined`` at bootstrap, then the directory
///   check in ``DeviceGroupSyncRepository``). With no other device listed it
///   founds instead: nobody can be cut off.
/// - **Behind:** a signed read of its DeviceGroup journal after its cursor
///   returned no rows and nothing more, the DS names a newer epoch than its
///   handle's, and it has no commit of its own in flight (``isBehind``). Since
///   ADR-0081 decision 1 an epoch is registered only together with the commit
///   row that reaches it, so a drained journal that is still ahead means the
///   device cannot apply a row it has already passed (its own commit read
///   back, a row it could not decrypt, a journal pruned past it): nothing it
///   will ever read moves it. No grace period is needed for a commit "still on
///   its way" any more; the one exception is this device's own outbox entry,
///   which is finished first.
///
/// ## The device's side
///
/// 1. Delete the local DeviceGroup state (snapshot, pending commit) and make
///    sure its own directory KeyPackage is published (ADR-0050; it carries
///    the signed device binding, published under the device-bound session).
/// 2. Build a DeviceGroup KeyPackage from the same device identity (same leaf
///    key) as the ADR-0056 pairing payload: login handle, KeyPackage and a
///    fresh rendezvous id. The joiner's provider and the rendezvous are kept
///    (``DeviceGroupSnapshotStore/PendingKind/rejoin``) so a restart keeps
///    waiting for the same Welcome.
/// 3. (Third amendment, 2026-11-11.) Open the long poll on the rendezvous,
///    then post the knock (one `ds-knock` token) and measure that request's
///    round trip `rtt`. Wait for a presence note or a Welcome for
///    ``answerWindowMs(rttMs:)``. A presence note means a device of the
///    account is running: wait for the Welcome with no deadline, knocking
///    again with a fresh KeyPackage after ``reknockAfterMs``. Nothing in the
///    window means every listed device is offline: FOUND the DeviceGroup
///    (``PassOutcome/found``); the caller founds and starts at once.
///
/// ## A sibling's side
///
/// A device holding the DeviceGroup keeps a long poll on its knocks while it
/// is in the foreground. On a knock naming this account's login handle it
/// first posts a presence note to the knock's rendezvous
/// (``shouldAnswer(knockAtMs:watchingSinceMs:listeningSinceMs:handled:knockId:isOwnKey:nowMs:)``),
/// before any directory lookup, then admits it when ``admissionVerdict(_:)``
/// says so: the KeyPackage's leaf key is listed in the account's directory
/// with a device binding that verifies under the account key (a deleted
/// device is tombstoned there, ADR-0074, and not listed); the knock is newer
/// than every addition of that key this device has seen and newer than when
/// it started watching. Every running device that passes the checks admits at
/// once (no designated admitter): it replaces the key's previous leaf, if
/// still in the roster, by the KeyPackage in ONE commit (`replaceDeviceAt`),
/// or adds the KeyPackage when no previous leaf is there — one ordinary
/// DeviceGroup commit through the outbox (``DeviceGroupCommits``), so the DS
/// lets exactly one commit take the epoch and the others drop theirs on
/// `409` — and the Welcome goes to the knock's rendezvous.
///
/// Under the ADR-0015 policy a removal needs a passkey attestation OR
/// (ADR-0081 amendment) the device's own self-removal proof: the rejoining
/// device signs, with its old leaf key, that its leaf may be replaced by the
/// KeyPackage it knocks with, and the proof rides in the commit's
/// removal-authorisation slot. A knock without a proof the sibling verified,
/// whose previous leaf is present, is left waiting under the policy (verdict
/// ``AdmissionVerdict/needsAttestation``): a device that no longer holds its
/// old key is a new device, its KeyPackage key matches no leaf, and it is
/// simply added.
public enum DeviceGroupRejoin {

    /// How long a rejoining device waits for a Welcome before knocking again
    /// with a fresh KeyPackage. Chosen, not measured.
    public static let reknockAfterMs: Int64 = 30 * 60_000

    /// The server-side wait of one long poll on the knocks (the DS wakes it
    /// when a knock is posted). Chosen, not measured.
    public static let knockLongPollSecs = 25

    /// The shortest answer window (third amendment). Chosen, not measured.
    public static let minAnswerWindowMs: Int64 = 1_000

    /// How long a rejoining device waits for a presence note or a Welcome
    /// after its knock: `max(4 x rtt, 1 s)`, `rtt` the knock request's own
    /// round trip. The answer is a DS wake, one read and one write on a
    /// sibling, and this device's read.
    public static func answerWindowMs(rttMs: Int64) -> Int64 {
        let rtt = max(0, rttMs)
        let scaled = rtt > Int64.max / 4 ? Int64.max : rtt * 4
        return max(scaled, minAnswerWindowMs)
    }

    /// How old a knock already waiting when this device starts listening may
    /// be and still be answered: older, its knocker has either founded on
    /// silence or will knock again. Keeps a device that comes back to the
    /// foreground from answering a week of old knocks. Chosen, not measured.
    public static let waitingKnockMaxAgeMs: Int64 = 30_000

    /// Whether a sibling answers a knock naming this account (a presence note
    /// before any directory lookup, then the admission checks): not its own
    /// key, not handled, not from before it started watching (it would not
    /// admit those), not waiting for more than ``waitingKnockMaxAgeMs`` when
    /// this listen began, and not so old that the knocker has knocked again.
    public static func shouldAnswer(
        knockAtMs: Int64, watchingSinceMs: Int64, listeningSinceMs: Int64, handled: Set<Int64>,
        knockId: Int64, isOwnKey: Bool, nowMs: Int64
    ) -> Bool {
        if isOwnKey || handled.contains(knockId) { return false }
        if knockAtMs < watchingSinceMs { return false }
        if knockAtMs < listeningSinceMs - waitingKnockMaxAgeMs { return false }
        return nowMs - knockAtMs < reknockAfterMs
    }

    /// What a row on the rejoin rendezvous is: a presence note, or a Welcome
    /// (anything else; a malformed one fails the join and the device knocks
    /// again).
    public enum RendezvousRow: Equatable {
        case presence
        case welcome(Data)
    }

    public static func classify(row: Data, isPresence: (Data) -> Bool) -> RendezvousRow {
        isPresence(row) ? .presence : .welcome(row)
    }

    /// At most this many handled knock ids are kept (knocks live seven days).
    public static let maxHandledKnocks = 256

    /// Is this device behind its DeviceGroup for good? `drained`: this poll's
    /// fetch after the cursor returned zero rows and `hasMore` was false.
    /// `latestEpoch`: the DS's `latest_epoch` (`nil` when it named none).
    /// `commitPending`: this device has a DeviceGroup commit in flight (its
    /// entry is finished first; until then the DS may hold this device's own
    /// next epoch).
    public static func isBehind(
        localEpoch: UInt64, drained: Bool, latestEpoch: UInt64?, commitPending: Bool
    ) -> Bool {
        guard !commitPending, drained, let latestEpoch else { return false }
        return latestEpoch > localEpoch
    }

    /// Signed reads of the DeviceGroup journal refused in a row (not for the
    /// clock: that is its own error) after which the device takes the
    /// journal as reset under it (third amendment, point 5). Three, so a
    /// refusal racing a commit's registration is not enough (the same number
    /// on web and Android). Chosen, not measured.
    public static let lostAfterRefusedReads = 3

    /// Is this device lost because its journal was reset under it? Its
    /// signed read was refused `refusedReads` times in a row while it did not
    /// found the journal itself (a founding whose claim has not landed is
    /// refused too, and is claimed again instead), or a drained read names a
    /// newest epoch below the one this device holds (a chain it does not
    /// hold) with no commit of its own in flight.
    public static func isLost(
        refusedReads: Int, foundingClaimPending: Bool,
        localEpoch: UInt64?, drained: Bool, latestEpoch: UInt64?, commitPending: Bool
    ) -> Bool {
        if !foundingClaimPending, refusedReads >= lostAfterRefusedReads { return true }
        guard !commitPending, drained, let localEpoch, let latestEpoch else { return false }
        return latestEpoch < localEpoch
    }

    /// An error as a log label: its type and case (and an HTTP status),
    /// never its text, which may carry a journal or rendezvous id.
    public static func errorLabel(_ error: Error) -> String {
        if let ds = error as? DsClientError {
            switch ds {
            case .http(let status, _, _): return "DsClientError.http(\(status))"
            case .network: return "DsClientError.network"
            case .clockSkew: return "DsClientError.clockSkew"
            case .argument: return "DsClientError.argument"
            }
        }
        if let statement = error as? DsStatementError, case .http(let status, _) = statement {
            return "DsStatementError.http(\(status))"
        }
        let mirror = Mirror(reflecting: error)
        if mirror.displayStyle == .enum {
            if let label = mirror.children.first?.label { return "\(type(of: error)).\(label)" }
            return "\(type(of: error)).\(String(describing: error))"
        }
        return String(describing: type(of: error))
    }

    /// What a sibling decides about one knock.
    public enum AdmissionVerdict: Equatable {
        case admit
        /// Already handled by this device.
        case handled
        /// From before this device started watching the journal: it cannot
        /// know whether another device admitted it; the knocker knocks again.
        case beforeWatching
        /// The key was added after the knock: someone already admitted it.
        case alreadyAdmitted
        /// The knocking key is this device's own.
        case ownKey
        /// The key is not listed in the account's directory with a verified
        /// binding (unknown, deleted, tombstoned).
        case notListed
        /// The previous leaf is present, the policy wants its removal
        /// authorised, and the knock carries no self-removal proof that
        /// covers it; a passkey attestation this device cannot obtain.
        case needsAttestation
    }

    /// One device of the roster: its leaf index and leaf key.
    public struct RosterMember: Equatable {
        public let leaf: UInt32
        public let key: Data

        public init(leaf: UInt32, key: Data) {
            self.leaf = leaf
            self.key = key
        }
    }

    /// Everything ``admissionVerdict(_:)`` reads. `roster` and `nowMs` are
    /// kept for the callers' convenience; since the third amendment no rule
    /// waits on the roster's order or on time.
    public struct AdmissionInput: Equatable {
        public var knockId: Int64
        public var knockAtMs: Int64
        public var key: Data
        public var ownKey: Data
        public var roster: [RosterMember]
        public var handled: Set<Int64>
        /// When this device started watching the journal (unix ms).
        public var watchingSinceMs: Int64
        /// When this device last saw `key` added to the roster, if ever.
        public var lastAddedAtMs: Int64?
        /// The directory lists the key with a binding under the account key.
        public var listed: Bool
        public var policyRequiresAttestation: Bool
        /// The knock carries a self-removal proof that covers replacing the
        /// previous leaf by its KeyPackage (checked with
        /// `deviceGroupSelfRemovalCovers`).
        public var selfRemovalProofCovers: Bool
        /// A leaf of the DeviceGroup already carries the knock's KeyPackage
        /// leaf (`containsLeafOfKeyPackage`): another sibling answered it. A
        /// replacement keeps the key (often the leaf index too), so the roster
        /// of keys cannot tell.
        public var keyPackageAlreadyIn: Bool
        public var nowMs: Int64

        public init(
            knockId: Int64, knockAtMs: Int64, key: Data, ownKey: Data, roster: [RosterMember],
            handled: Set<Int64>, watchingSinceMs: Int64, lastAddedAtMs: Int64?, listed: Bool,
            policyRequiresAttestation: Bool, selfRemovalProofCovers: Bool = false,
            keyPackageAlreadyIn: Bool = false, nowMs: Int64
        ) {
            self.knockId = knockId
            self.knockAtMs = knockAtMs
            self.key = key
            self.ownKey = ownKey
            self.roster = roster
            self.handled = handled
            self.watchingSinceMs = watchingSinceMs
            self.lastAddedAtMs = lastAddedAtMs
            self.listed = listed
            self.policyRequiresAttestation = policyRequiresAttestation
            self.selfRemovalProofCovers = selfRemovalProofCovers
            self.keyPackageAlreadyIn = keyPackageAlreadyIn
            self.nowMs = nowMs
        }
    }

    /// The pure admission rule (see the type doc).
    public static func admissionVerdict(_ input: AdmissionInput) -> AdmissionVerdict {
        if input.handled.contains(input.knockId) { return .handled }
        if input.key == input.ownKey { return .ownKey }
        if input.knockAtMs < input.watchingSinceMs { return .beforeWatching }
        if input.keyPackageAlreadyIn { return .alreadyAdmitted }
        if let added = input.lastAddedAtMs, added >= input.knockAtMs { return .alreadyAdmitted }
        if !input.listed { return .notListed }
        let present = input.roster.contains { $0.key == input.key }
        if present, input.policyRequiresAttestation, !input.selfRemovalProofCovers { return .needsAttestation }
        return .admit
    }
}

/// What a sibling keeps across sessions
/// (``DeviceGroupSnapshotStore/PendingKind/admissions``): when it started
/// watching, when it last saw each key added, and the knocks it handled.
/// Cleared with the DeviceGroup state.
public struct DeviceGroupAdmissionRecord: Codable, Equatable {
    public var v: Int
    public var watchingSinceMs: Int64
    /// Leaf key (lowercase hex) -> when this device last saw it added.
    public var lastAddedAtMs: [String: Int64]
    public var handled: [Int64]

    public init(watchingSinceMs: Int64) {
        self.v = 1
        self.watchingSinceMs = watchingSinceMs
        self.lastAddedAtMs = [:]
        self.handled = []
    }

    /// The record with `keyHex` seen added at `atMs` (the later time wins).
    public func noteAdded(keyHex: String, atMs: Int64) -> DeviceGroupAdmissionRecord {
        if let prior = lastAddedAtMs[keyHex], prior >= atMs { return self }
        var next = self
        next.lastAddedAtMs[keyHex] = atMs
        return next
    }

    /// The record with `knockId` handled (bounded, newest kept).
    public func noteHandled(_ knockId: Int64) -> DeviceGroupAdmissionRecord {
        if handled.contains(knockId) { return self }
        var next = self
        next.handled = Array((handled + [knockId]).sorted().suffix(DeviceGroupRejoin.maxHandledKnocks))
        return next
    }

    public func encoded() throws -> Data { try JSONEncoder().encode(self) }

    /// `nil` for bytes this build did not write.
    public static func decode(_ bytes: Data) -> DeviceGroupAdmissionRecord? {
        guard let record = try? JSONDecoder().decode(DeviceGroupAdmissionRecord.self, from: bytes),
              record.v == 1 else { return nil }
        return record
    }
}

/// A rejoin in progress (``DeviceGroupSnapshotStore/PendingKind/rejoin``).
public struct DeviceGroupRejoinState: Codable, Equatable {
    public var v: Int
    /// The rendezvous the Welcome will arrive at, standard base64.
    public var rendezvousB64: String
    /// The joiner's DeviceIdentity provider snapshot (it holds the
    /// KeyPackage's private keys), standard base64.
    public var joinerSnapshotB64: String
    public var knockedAtMs: Int64
    /// A presence note answered this knock: a device of the account is
    /// running, so the Welcome is waited for until the re-knock time. `nil`
    /// or `false` (the answer window had not passed, a crash) knocks again.
    public var answered: Bool?

    public init(rendezvousB64: String, joinerSnapshotB64: String, knockedAtMs: Int64, answered: Bool? = nil) {
        self.v = 1
        self.rendezvousB64 = rendezvousB64
        self.joinerSnapshotB64 = joinerSnapshotB64
        self.knockedAtMs = knockedAtMs
        self.answered = answered
    }

    public func encoded() throws -> Data { try JSONEncoder().encode(self) }

    /// `nil` for bytes this build did not write.
    public static func decode(_ bytes: Data) -> DeviceGroupRejoinState? {
        guard let state = try? JSONDecoder().decode(DeviceGroupRejoinState.self, from: bytes),
              state.v == 1 else { return nil }
        return state
    }

    /// Whether to knock (again): no knock out, a knock no presence note
    /// answered (its answer window is over, or a restart cut it short), a
    /// knock long enough ago, or the clock went back.
    public static func shouldKnockAgain(_ state: DeviceGroupRejoinState?, nowMs: Int64) -> Bool {
        guard let state else { return true }
        if state.answered != true { return true }
        return nowMs - state.knockedAtMs >= DeviceGroupRejoin.reknockAfterMs || nowMs < state.knockedAtMs
    }
}

/// A knock built and kept, not yet posted.
public struct DeviceGroupPreparedKnock {
    public let state: DeviceGroupRejoinState
    public let payload: Data

    public init(state: DeviceGroupRejoinState, payload: Data) {
        self.state = state
        self.payload = payload
    }
}

/// A long poll on one rejoin rendezvous, open from its creation.
@MainActor
public protocol DeviceGroupRendezvousWatch: AnyObject {
    /// The next row, or `nil` when `untilMs` (unix ms) passed first.
    func next(untilMs: Int64) async -> DeviceGroupRejoin.RendezvousRow?
    func close()
}

/// What one rejoin attempt needs.
@MainActor
public struct DeviceGroupRejoinPorts {
    /// Delete the local DeviceGroup state (snapshot and pending commit).
    public var dropLocalState: () -> Void
    /// Make sure this device's directory KeyPackage is published.
    public var publishDirectoryKeyPackage: () async throws -> Void
    /// Whether a knock can name the account (its login handle is known). A
    /// knock that names nothing no sibling can admit, so the device founds.
    public var canKnock: () -> Bool
    public var loadState: () -> DeviceGroupRejoinState?
    public var saveState: (DeviceGroupRejoinState) throws -> Void
    /// Build a fresh joiner (DeviceGroup KeyPackage from the device identity)
    /// and the knock payload; the returned state is already saved.
    public var prepareKnock: (_ nowMs: Int64) async throws -> DeviceGroupPreparedKnock
    /// Open the long poll on the state's rendezvous (it starts at once).
    public var openWatch: (DeviceGroupRejoinState) -> DeviceGroupRendezvousWatch
    /// Post the knock.
    public var postKnock: (DeviceGroupPreparedKnock) async throws -> Void
    /// Join from the Welcome with the joiner kept in the state and persist
    /// the joined DeviceGroup (fail-closed).
    public var join: (DeviceGroupRejoinState, Data) async throws -> Void
    public var clearState: () -> Void
    public var now: () -> Int64
    /// A device of the account answered this device's knock (a presence
    /// note or a Welcome within the answer window, or an answered knock kept
    /// from before): section 7.2 of `docs/ux/identity-recovery.md` then shows
    /// the ordinary list, not the card. Called before the device waits for
    /// its Welcome.
    public var noteAnswered: () -> Void
    /// One line, numbers only.
    public var log: (String) -> Void

    public init(
        dropLocalState: @escaping () -> Void,
        publishDirectoryKeyPackage: @escaping () async throws -> Void,
        canKnock: @escaping () -> Bool,
        loadState: @escaping () -> DeviceGroupRejoinState?,
        saveState: @escaping (DeviceGroupRejoinState) throws -> Void,
        prepareKnock: @escaping (Int64) async throws -> DeviceGroupPreparedKnock,
        openWatch: @escaping (DeviceGroupRejoinState) -> DeviceGroupRendezvousWatch,
        postKnock: @escaping (DeviceGroupPreparedKnock) async throws -> Void,
        join: @escaping (DeviceGroupRejoinState, Data) async throws -> Void,
        clearState: @escaping () -> Void,
        now: @escaping () -> Int64,
        noteAnswered: @escaping () -> Void = {},
        log: @escaping (String) -> Void = { NSLog("%@", $0) }
    ) {
        self.dropLocalState = dropLocalState
        self.publishDirectoryKeyPackage = publishDirectoryKeyPackage
        self.canKnock = canKnock
        self.loadState = loadState
        self.saveState = saveState
        self.prepareKnock = prepareKnock
        self.openWatch = openWatch
        self.postKnock = postKnock
        self.join = join
        self.clearState = clearState
        self.now = now
        self.noteAnswered = noteAnswered
        self.log = log
    }
}

extension DeviceGroupRejoin {
    /// What one pass of the device's side came to.
    public enum PassOutcome: Equatable {
        /// The device is a member again (its DeviceGroup is on disk).
        case joined
        /// The caller should call again.
        case waiting
        /// No device of the account answered: the caller founds the
        /// DeviceGroup (third amendment, point 4).
        case found
    }

    /// One pass of the device side: drop the state (first pass only); knock
    /// when no answered knock is out, with the rendezvous long poll open
    /// first, and wait ``answerWindowMs(rttMs:)`` for an answer; with a
    /// presence note, or an answered knock kept from before, wait for the
    /// Welcome until the re-knock time.
    @MainActor
    public static func rejoinOnce(ports: DeviceGroupRejoinPorts, firstPass: Bool) async throws -> PassOutcome {
        if firstPass {
            ports.dropLocalState()
            try await ports.publishDirectoryKeyPackage()
        }
        guard ports.canKnock() else {
            ports.log("DeviceGroupSync rejoin: no login handle to knock with; founding")
            return .found
        }
        let kept = ports.loadState()
        if !DeviceGroupRejoinState.shouldKnockAgain(kept, nowMs: ports.now()), let kept {
            if kept.answered == true { ports.noteAnswered() }
            let watch = ports.openWatch(kept)
            defer { watch.close() }
            return try await waitForWelcome(
                watch: watch, state: kept, untilMs: kept.knockedAtMs + reknockAfterMs, ports: ports)
        }
        let prepared = try await ports.prepareKnock(ports.now())
        let watch = ports.openWatch(prepared.state)
        defer { watch.close() }
        let postedAt = ports.now()
        try await ports.postKnock(prepared)
        let rtt = max(0, ports.now() - postedAt)
        let window = answerWindowMs(rttMs: rtt)
        let first = await watch.next(untilMs: ports.now() + window)
        guard let first else {
            ports.log("DeviceGroupSync rejoin: no answer within \(window) ms (knock rtt \(rtt) ms); founding")
            return .found
        }
        ports.log("DeviceGroupSync rejoin: answered after \(max(0, ports.now() - postedAt)) ms (knock rtt \(rtt) ms)")
        ports.noteAnswered()
        switch first {
        case .welcome(let welcome):
            try await join(prepared.state, welcome, ports: ports)
            return .joined
        case .presence:
            var answered = prepared.state
            answered.answered = true
            try ports.saveState(answered)
            return try await waitForWelcome(
                watch: watch, state: answered, untilMs: answered.knockedAtMs + reknockAfterMs, ports: ports)
        }
    }

    /// Join, or (a row that is no Welcome for this joiner) mark the knock
    /// unanswered so the next pass knocks again instead of reading the same
    /// row.
    @MainActor
    private static func join(
        _ state: DeviceGroupRejoinState, _ welcome: Data, ports: DeviceGroupRejoinPorts
    ) async throws {
        do {
            try await ports.join(state, welcome)
        } catch {
            var unanswered = state
            unanswered.answered = false
            try? ports.saveState(unanswered)
            throw error
        }
        ports.clearState()
    }

    @MainActor
    private static func waitForWelcome(
        watch: DeviceGroupRendezvousWatch, state: DeviceGroupRejoinState, untilMs: Int64,
        ports: DeviceGroupRejoinPorts
    ) async throws -> PassOutcome {
        while let row = await watch.next(untilMs: untilMs) {
            // More presence notes: more than one device is running.
            guard case .welcome(let welcome) = row else { continue }
            try await join(state, welcome, ports: ports)
            return .joined
        }
        return .waiting
    }
}

/// The rendezvous long poll of a rejoin: one request held open on the DS
/// (which wakes it when a row is posted), started when the watch is made, so
/// it is open before the knock goes out. Rows are kept in order until read.
/// A failed read is retried after a short pause; nothing is thrown out.
@MainActor
public final class DeviceGroupRendezvousLongPoll: DeviceGroupRendezvousWatch {
    /// The server-side wait of one read. Chosen, not measured.
    static let waitSecs = 15

    public typealias Fetch = (_ after: Int64, _ waitSecs: Int) async throws -> FetchResult

    private let fetch: Fetch
    private let isPresence: (Data) -> Bool
    private var rows: [Data] = []
    private var waiter: CheckedContinuation<Void, Never>?
    private var pollTask: Task<Void, Never>?

    public init(fetch: @escaping Fetch, isPresence: @escaping (Data) -> Bool) {
        self.fetch = fetch
        self.isPresence = isPresence
        pollTask = Task { [weak self] in await self?.run() }
    }

    private func run() async {
        var cursor: Int64 = 0
        while !Task.isCancelled {
            do {
                let result = try await fetch(cursor, Self.waitSecs)
                guard !Task.isCancelled else { return }
                for message in result.messages where message.id > cursor {
                    cursor = message.id
                    rows.append(message.payload)
                }
                if !result.messages.isEmpty { wake() }
            } catch {
                guard !Task.isCancelled else { return }
                // A rendezvous nobody wrote yet, the network, the DS: wait a
                // little and read again (never a tight loop).
                try? await Task.sleep(nanoseconds: 1_000_000_000)
            }
        }
    }

    private func wake() {
        let w = waiter
        waiter = nil
        w?.resume()
    }

    public func next(untilMs: Int64) async -> DeviceGroupRejoin.RendezvousRow? {
        while true {
            if !rows.isEmpty {
                return DeviceGroupRejoin.classify(row: rows.removeFirst(), isPresence: isPresence)
            }
            let remainingMs = untilMs - Int64(Date().timeIntervalSince1970 * 1000)
            if remainingMs <= 0 || Task.isCancelled || pollTask == nil { return nil }
            let timer = Task { [weak self] in
                try? await Task.sleep(nanoseconds: UInt64(remainingMs) * 1_000_000)
                self?.wake()
            }
            await withTaskCancellationHandler {
                await withCheckedContinuation { (c: CheckedContinuation<Void, Never>) in
                    if Task.isCancelled || !rows.isEmpty || pollTask == nil {
                        c.resume()
                        return
                    }
                    wake()
                    waiter = c
                }
            } onCancel: {
                Task { @MainActor [weak self] in self?.wake() }
            }
            timer.cancel()
        }
    }

    public func close() {
        pollTask?.cancel()
        pollTask = nil
        wake()
    }
}
