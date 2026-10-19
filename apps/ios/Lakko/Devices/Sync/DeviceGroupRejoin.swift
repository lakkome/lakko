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
///    key) and post it to the DeviceGroup's knock (ADR-0053, one `ds-knock`
///    token) as the ADR-0056 pairing payload: login handle, KeyPackage and a
///    fresh rendezvous id. The joiner's provider and the rendezvous are kept
///    (``DeviceGroupSnapshotStore/PendingKind/rejoin``) so a restart keeps
///    waiting for the same Welcome.
/// 3. Poll the rendezvous for the Welcome; join from it. Knock again with a
///    fresh KeyPackage after ``reknockAfterMs`` without one.
///
/// ## A sibling's side
///
/// A device holding the DeviceGroup reads the knocks once its journal is read
/// to the head and admits a knock only when ``admissionVerdict(_:)`` says so:
/// the payload names this account's login handle; the KeyPackage's leaf key
/// is listed in the account's directory with a device binding that verifies
/// under the account key (a deleted device is tombstoned there, ADR-0074, and
/// not listed); the knock is newer than every addition of that key this
/// device has seen and newer than when it started watching; this device is
/// the designated admitter (the lowest leaf other than the knocking key) or
/// the knock has waited ``fallbackAdmitAfterMs``. To admit it replaces the
/// key's previous leaf, if still in the roster, by the KeyPackage in ONE
/// commit (`replaceDeviceAt`), or adds the KeyPackage when no previous leaf
/// is there — one ordinary DeviceGroup commit through the outbox
/// (``DeviceGroupCommits``), so the DS arbitrates two siblings acting at once
/// — and the Welcome goes to the knock's rendezvous.
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

    /// How long a knock waits for the designated admitter before any sibling
    /// admits it. Chosen, not measured.
    public static let fallbackAdmitAfterMs: Int64 = 10 * 60_000

    /// How often a sibling reads the knocks while its journal is at the head.
    /// Chosen, not measured.
    public static let knockCheckIntervalMs: Int64 = 30_000

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
        /// Waiting for the designated admitter.
        case notMineYet
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

    /// Everything ``admissionVerdict(_:)`` reads.
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
        let others = input.roster.filter { $0.key != input.key }
        let lowest = others.min { $0.leaf < $1.leaf }
        let designated = lowest?.key == input.ownKey
        if !designated, input.nowMs - input.knockAtMs < fallbackAdmitAfterMs { return .notMineYet }
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

    public init(rendezvousB64: String, joinerSnapshotB64: String, knockedAtMs: Int64) {
        self.v = 1
        self.rendezvousB64 = rendezvousB64
        self.joinerSnapshotB64 = joinerSnapshotB64
        self.knockedAtMs = knockedAtMs
    }

    public func encoded() throws -> Data { try JSONEncoder().encode(self) }

    /// `nil` for bytes this build did not write.
    public static func decode(_ bytes: Data) -> DeviceGroupRejoinState? {
        guard let state = try? JSONDecoder().decode(DeviceGroupRejoinState.self, from: bytes),
              state.v == 1 else { return nil }
        return state
    }

    /// Whether a rejoin knocked long enough ago to knock again (or the clock
    /// went back).
    public static func shouldKnockAgain(_ state: DeviceGroupRejoinState?, nowMs: Int64) -> Bool {
        guard let state else { return true }
        return nowMs - state.knockedAtMs >= DeviceGroupRejoin.reknockAfterMs || nowMs < state.knockedAtMs
    }
}

/// What one rejoin attempt needs.
@MainActor
public struct DeviceGroupRejoinPorts {
    /// Delete the local DeviceGroup state (snapshot and pending commit).
    public var dropLocalState: () -> Void
    /// Make sure this device's directory KeyPackage is published.
    public var publishDirectoryKeyPackage: () async throws -> Void
    public var loadState: () -> DeviceGroupRejoinState?
    public var saveState: (DeviceGroupRejoinState) throws -> Void
    /// Build a fresh joiner (DeviceGroup KeyPackage from the device identity)
    /// and knock with it; returns the state to keep (already saved before
    /// the knock went out).
    public var knock: (_ nowMs: Int64) async throws -> DeviceGroupRejoinState
    /// Wait for the Welcome at the state's rendezvous until `deadlineMs`;
    /// `nil` on a timeout.
    public var awaitWelcome: (DeviceGroupRejoinState, _ deadlineMs: Int64) async throws -> Data?
    /// Join from the Welcome with the joiner kept in the state and persist
    /// the joined DeviceGroup (fail-closed).
    public var join: (DeviceGroupRejoinState, Data) async throws -> Void
    public var clearState: () -> Void
    public var now: () -> Int64

    public init(
        dropLocalState: @escaping () -> Void,
        publishDirectoryKeyPackage: @escaping () async throws -> Void,
        loadState: @escaping () -> DeviceGroupRejoinState?,
        saveState: @escaping (DeviceGroupRejoinState) throws -> Void,
        knock: @escaping (Int64) async throws -> DeviceGroupRejoinState,
        awaitWelcome: @escaping (DeviceGroupRejoinState, Int64) async throws -> Data?,
        join: @escaping (DeviceGroupRejoinState, Data) async throws -> Void,
        clearState: @escaping () -> Void,
        now: @escaping () -> Int64
    ) {
        self.dropLocalState = dropLocalState
        self.publishDirectoryKeyPackage = publishDirectoryKeyPackage
        self.loadState = loadState
        self.saveState = saveState
        self.knock = knock
        self.awaitWelcome = awaitWelcome
        self.join = join
        self.clearState = clearState
        self.now = now
    }
}

extension DeviceGroupRejoin {
    /// What one pass of the device's side came to.
    public enum PassOutcome: Equatable {
        /// The device is a member again (its DeviceGroup is on disk).
        case joined
        /// The caller should call again.
        case waiting
    }

    /// One pass of the device side: drop the state (first pass only), make
    /// sure a knock is out, wait for its Welcome until the re-knock time.
    @MainActor
    public static func rejoinOnce(ports: DeviceGroupRejoinPorts, firstPass: Bool) async throws -> PassOutcome {
        if firstPass {
            ports.dropLocalState()
            try await ports.publishDirectoryKeyPackage()
        }
        var state = ports.loadState()
        if DeviceGroupRejoinState.shouldKnockAgain(state, nowMs: ports.now()) {
            let knocked = try await ports.knock(ports.now())
            try ports.saveState(knocked)
            state = knocked
        }
        guard let current = state else { return .waiting }
        guard let welcome = try await ports.awaitWelcome(current, current.knockedAtMs + reknockAfterMs) else {
            return .waiting
        }
        try await ports.join(current, welcome)
        ports.clearState()
        return .joined
    }
}
