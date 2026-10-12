// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

// ADR-0066 part 3, Stage A — a KEPT device (pending / adopt / drop /
// superseded without a pending envelope), the proven-predecessor rule, the
// stale-statement retry, the recall-finish retry and the root endorsement of
// the reset / recall.

/// In-memory pending store.
final class InMemoryKeyRotationPendingStore: KeyRotationPendingStoring {
    private(set) var pending: [String: [PendingKeyRotation]] = [:]
    var saveError: Error?
    func load(accountIdB64: String) -> [PendingKeyRotation] { pending[accountIdB64] ?? [] }
    func save(_ pending: [PendingKeyRotation], accountIdB64: String) throws {
        if let saveError { throw saveError }
        self.pending[accountIdB64] = pending
    }
    func clear(accountIdB64: String) { pending[accountIdB64] = nil }
}

/// In-memory predecessor slot.
final class InMemoryOwnPredecessorStore: OwnPredecessorStoring {
    private(set) var seeds: [String: [Data]] = [:]
    var addError: Error?
    func load(accountIdB64: String) -> [Data] { seeds[accountIdB64] ?? [] }
    func add(_ seed: Data, accountIdB64: String) throws {
        if let addError { throw addError }
        guard !(seeds[accountIdB64] ?? []).contains(seed) else { return }
        seeds[accountIdB64, default: []].append(seed)
    }
    func replace(_ seeds: [Data], accountIdB64: String) throws {
        if let addError { throw addError }
        self.seeds[accountIdB64] = seeds
    }
    func clear(accountIdB64: String) { seeds[accountIdB64] = nil }
}

// MARK: - The kept device's verdicts

final class KeptDeviceRuleTests: XCTestCase {

    private func pending(_ byte: UInt8, preparedAtMs: UInt64 = 1_000) -> PendingKeyRotation {
        PendingKeyRotation(
            newSignaturePubkey: Data(repeating: byte, count: 32),
            newEntropy: Data(repeating: byte, count: 16),
            supersessionProof: Data(repeating: byte, count: 145),
            preparedAtMs: preparedAtMs, proofExpiresAtUnixSecs: 99)
    }

    private let never: (UInt64, UInt64) -> Bool = { _, _ in false }
    private let fourteenDaysMs: UInt64 = 14 * 24 * 60 * 60 * 1000

    func testAnEnvelopeFromAnywhereButTheOwnDeviceGroupIsDropped() {
        XCTAssertEqual(
            KeptDeviceRule.receive(
                arrival: .elsewhere, verified: pending(1), held: [], nowMs: 2_000, expired: never),
            .drop(.notOwnDeviceGroup),
            "even one that would verify")
    }

    func testAnEnvelopeThatDoesNotVerifyIsDropped() {
        XCTAssertEqual(
            KeptDeviceRule.receive(
                arrival: .ownDeviceGroup, verified: nil, held: [], nowMs: 2_000, expired: never),
            .drop(.notVerified))
    }

    func testAVerifiedEnvelopeIsStoredAsPendingAndNothingElse() {
        XCTAssertEqual(
            KeptDeviceRule.receive(
                arrival: .ownDeviceGroup, verified: pending(1), held: [], nowMs: 2_000, expired: never),
            .store(pending(1)))
        // A second envelope for another key is held beside it.
        XCTAssertEqual(
            KeptDeviceRule.receive(
                arrival: .ownDeviceGroup, verified: pending(2), held: [pending(1)], nowMs: 2_000,
                expired: never),
            .store(pending(2)))
        // The same key again changes nothing.
        XCTAssertEqual(
            KeptDeviceRule.receive(
                arrival: .ownDeviceGroup, verified: pending(1), held: [pending(1)], nowMs: 2_000,
                expired: never),
            .alreadyHeld)
    }

    func testAPendingEnvelopeIsDroppedAfterFourteenDaysByTheCoresOwnRule() {
        let prepared: UInt64 = 1_000_000
        let old = pending(1, preparedAtMs: prepared)
        XCTAssertEqual(keyRotationPendingMaxAgeMs(), fourteenDaysMs)
        XCTAssertEqual(
            KeptDeviceRule.pruned(
                [old], nowMs: prepared + fourteenDaysMs,
                expired: keyRotationPendingExpired(preparedAtMs:nowMs:)),
            [old], "kept up to the bound")
        XCTAssertEqual(
            KeptDeviceRule.pruned(
                [old, pending(2, preparedAtMs: prepared + 10)], nowMs: prepared + fourteenDaysMs + 1,
                expired: keyRotationPendingExpired(preparedAtMs:nowMs:)),
            [pending(2, preparedAtMs: prepared + 10)], "dropped past it")
        XCTAssertEqual(
            KeptDeviceRule.receive(
                arrival: .ownDeviceGroup, verified: old, held: [], nowMs: prepared + fourteenDaysMs + 1,
                expired: keyRotationPendingExpired(preparedAtMs:nowMs:)),
            .drop(.expired))
    }

    func testAdoptionFollowsTheKeyThePublishedBundleCarries() {
        let held = [pending(1), pending(2)]
        XCTAssertEqual(
            KeptDeviceRule.adoptable(for: Data(repeating: 2, count: 32), among: held), pending(2),
            "never by arrival order")
        XCTAssertNil(KeptDeviceRule.adoptable(for: Data(repeating: 9, count: 32), among: held))
        XCTAssertNil(KeptDeviceRule.adoptable(for: nil, among: held))
    }

    func testSupersededWithNoPendingEnvelopeReadsTheJournalOnceBeforeItIsAReset() {
        XCTAssertEqual(
            KeptDeviceRule.onSuperseded(held: [], publishedSignaturePubkey: nil, journalRead: false),
            .readJournalFirst)
        XCTAssertEqual(
            KeptDeviceRule.onSuperseded(held: [], publishedSignaturePubkey: nil, journalRead: true),
            .treatAsReset, "once, and only then a reset")
    }

    func testSupersededWithPendingEnvelopesTriesThePublishedKeysFirst() {
        let a = pending(1, preparedAtMs: 100)
        let b = pending(2, preparedAtMs: 300)
        let c = pending(3, preparedAtMs: 200)
        XCTAssertEqual(
            KeptDeviceRule.onSuperseded(
                held: [a, b, c], publishedSignaturePubkey: Data(repeating: 1, count: 32), journalRead: false),
            .tryAdopting([a, b, c]), "the published key first, then the newest")
        XCTAssertEqual(
            KeptDeviceRule.onSuperseded(held: [a, b, c], publishedSignaturePubkey: nil, journalRead: false),
            .tryAdopting([b, c, a]))
    }

    func testTheUpgradesAnswersAreToldApart() {
        typealias A = SessionUpgradeAnswer
        XCTAssertEqual(A.of(nil), .upgraded)
        XCTAssertEqual(A.of(status: 401, body: #"{"error":"key-generation-superseded"}"#), .notAccepted)
        XCTAssertEqual(A.of(status: 401, body: #"{"error":"device-revoked"}"#), .deviceRevoked)
        XCTAssertEqual(A.of(status: 401, body: #"{"error":"refresh-invalid"}"#), .refreshInvalid)
        XCTAssertEqual(
            A.of(status: 401, body: #"{"error":"statement-stale","server_time_ms":5}"#), .stale)
        XCTAssertEqual(A.of(status: 503, body: ""), .failed)
        XCTAssertEqual(
            A.of(AuthClientError.network(endpoint: "/x", underlying: URLError(.timedOut))), .offline)
    }

    func testOnlyARefusalByTheServerMovesOnAndOnlyARevocationWipes() {
        XCTAssertEqual(KeptDeviceRule.afterUpgrade(.upgraded), .adopt)
        XCTAssertEqual(KeptDeviceRule.afterUpgrade(.notAccepted), .tryNext)
        XCTAssertEqual(KeptDeviceRule.afterUpgrade(.deviceRevoked), .wipeRevoked)
        XCTAssertEqual(KeptDeviceRule.afterUpgrade(.refreshInvalid), .signInAgain)
        // No verdict is never a wipe.
        XCTAssertEqual(KeptDeviceRule.afterUpgrade(.stale), .retryLater)
        XCTAssertEqual(KeptDeviceRule.afterUpgrade(.offline), .retryLater)
        XCTAssertEqual(KeptDeviceRule.afterUpgrade(.failed), .retryLater)
    }

    func testTheRunEndsAtTheFirstVerdictAndIsAResetOnlyWhenEveryCandidateWasRefused() {
        XCTAssertEqual(KeptDeviceRule.outcome(of: [.notAccepted, .upgraded]), .adopted)
        XCTAssertEqual(KeptDeviceRule.outcome(of: [.notAccepted, .notAccepted]), .treatAsReset)
        XCTAssertEqual(KeptDeviceRule.outcome(of: [.notAccepted, .offline]), .retryLater)
        XCTAssertEqual(KeptDeviceRule.outcome(of: [.deviceRevoked]), .wipeRevoked)
        XCTAssertEqual(KeptDeviceRule.outcome(of: [.refreshInvalid]), .signInAgain)
        XCTAssertEqual(KeptDeviceRule.outcome(of: []), .treatAsReset)
    }

    func testABindingUnderTheAccountsOwnOldKeyIsKept() {
        let current = Data(repeating: 1, count: 32)
        let old = Data(repeating: 2, count: 32)
        let stranger = Data(repeating: 3, count: 32)
        XCTAssertTrue(OwnAccountKeys.bindingIsOwn(
            bindingAccountKey: current, currentAccountKey: current, predecessorKeys: []))
        XCTAssertTrue(OwnAccountKeys.bindingIsOwn(
            bindingAccountKey: old, currentAccountKey: current, predecessorKeys: [old]),
            "after a rotation a device does not re-bind")
        XCTAssertFalse(OwnAccountKeys.bindingIsOwn(
            bindingAccountKey: old, currentAccountKey: current, predecessorKeys: []),
            "after a RESET nothing is kept: the binding is re-minted")
        XCTAssertFalse(OwnAccountKeys.bindingIsOwn(
            bindingAccountKey: stranger, currentAccountKey: current, predecessorKeys: [old]))
        XCTAssertTrue(OwnAccountKeys.isOwn(old, currentAccountKey: current, predecessorKeys: [old]))
        XCTAssertFalse(OwnAccountKeys.isOwn(stranger, currentAccountKey: current, predecessorKeys: [old]))
    }
}

// MARK: - The kept device, end to end over the repository

@MainActor
final class KeptDeviceAdoptionTests: XCTestCase {

    private final class Persistence: SessionPersistence {
        var seed: Data?
        var token: PersistedSessionToken?
        func hasDeviceKey(accountIdB64: String) -> Bool { true }
        func accountIdentitySeed(accountIdB64: String) -> Data? { seed }
        func exportKey(accountIdB64: String) -> Data? { nil }
        func sessionToken(accountIdB64: String) -> PersistedSessionToken? { token }
        func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws { self.token = token }
        func deleteSessionToken(accountIdB64: String) { token = nil }
    }

    private final class Directory: AccountDirectory, DeviceRestoreDirectory {
        func fetchIdentityBundle(accountId: Data, sessionToken: String?) async throws -> IdentityFetchResponse? {
            throw URLError(.notConnectedToInternet)
        }
        func lookupAccount(username: String, accountId: Data, sessionToken: String) async throws -> Data? { nil }
        func registerUsername(accountId: Data, username: String, sessionToken: String) async throws -> RegisterResult {
            RegisterResult(normalisedUsername: username)
        }
        func publishIdentityBundle(
            accountId: Data, signaturePubkey: Data, x25519Pubkey: Data,
            leafAuth: Data?, genesisAttest: Data?, keyResetAttest: Data?, sessionToken: String
        ) async throws {}
    }

    private final class Verifier: KeyResetAttestationVerifying {
        func verify(
            attestation: Data, accountId: Data, newSignaturePubkey: Data, nowMs: Int64, maxAgeMs: Int64?
        ) async throws -> KeyResetClaims {
            KeyResetClaims(keyGeneration: 2, signedAtMs: 1)
        }
    }

    /// The session-upgrade route, scripted.
    private final class Upgrades {
        var answers: [Result<SessionPair, Error>] = []
        private(set) var calls: [(refreshToken: String, accountId: Data, seed: Data)] = []
        func run(_ refreshToken: String, _ accountId: Data, _ seed: Data) throws -> SessionPair {
            calls.append((refreshToken, accountId, seed))
            guard !answers.isEmpty else {
                throw AuthClientError.network(endpoint: "/auth/session/refresh", underlying: URLError(.timedOut))
            }
            return try answers.removeFirst().get()
        }
    }

    private final class Log {
        private(set) var full: [String] = []
        private(set) var staged: [String] = []
        private(set) var journalReads = 0
        private(set) var rotated: [(seed: Data, predecessor: Data)] = []
        func noteFull(_ account: String) { full.append(account) }
        func noteStaged(_ account: String) { staged.append(account) }
        func noteJournalRead() { journalReads += 1 }
        func noteRotated(_ seed: Data, _ predecessor: Data) { rotated.append((seed, predecessor)) }
    }

    private struct Harness {
        let repository: AuthRepository
        let pending: InMemoryKeyRotationPendingStore
        let predecessors: InMemoryOwnPredecessorStore
        let upgrades: Upgrades
        let recalls: InMemoryRecallRecordStore
        let tokens: SessionTokenProvider
        let log: Log
        let notice: KeysRotatedNoticeStore
        let rotations: KeyRotationStore
    }

    private let accountId = Data((0..<16).map { UInt8(0xD0 | $0) })
    private var accountIdB64: String { base64Encode(accountId) }
    private let oldSeed = Data(repeating: 0x5A, count: 64)
    private let nowMs: Int64 = 1_700_000_000_000
    private var suites: [String] = []

    override func tearDown() async throws {
        DeviceKeyStore.deleteDeviceKey(accountIdB64: accountIdB64)
        AccountIdentitySeedStore.deleteAccountIdentitySeed(accountIdB64: accountIdB64)
        DeviceIdentityStore.deleteDeviceIdentitySeed(accountIdB64: accountIdB64)
        try? PhraseStore().deletePhrase(accountIdB64: accountIdB64)
        for suite in suites { UserDefaults().removePersistentDomain(forName: suite) }
        suites = []
        try await super.tearDown()
    }

    private func defaults(_ name: String) -> UserDefaults {
        let suite = "\(name)-\(UUID().uuidString)"
        suites.append(suite)
        return UserDefaults(suiteName: suite)!
    }

    private func device(holdsKeys: Bool = true) -> Harness {
        let store = SessionStore(defaults: defaults("kept-session"))
        store.save(PersistedSession(accountIdB64: accountIdB64, username: "alice"))
        let persistence = Persistence()
        persistence.token = PersistedSessionToken(
            pair: .fixture(access: "tok-old", accessExpiresAtMs: nowMs + 600_000, refresh: "r2.old"))
        persistence.seed = holdsKeys ? oldSeed : nil
        let tokens = SessionTokenProvider()
        let now = nowMs
        tokens.nowMs = { now }
        tokens.refresher = { _ in throw URLError(.notConnectedToInternet) }
        let pending = InMemoryKeyRotationPendingStore()
        let predecessors = InMemoryOwnPredecessorStore()
        let upgrades = Upgrades()
        let recalls = InMemoryRecallRecordStore()
        let log = Log()
        let notice = KeysRotatedNoticeStore(defaults: defaults("kept-notice"))
        let directory = Directory()
        let rotations = KeyRotationStore(defaults: defaults("kept-rotation"))
        let repository = AuthRepository(
            sessionStore: store,
            authClientFactory: { NameRouteStub.client(NameRouteStub.scope { _, _, _ in (503, "") }) },
            persistence: persistence,
            nowMs: { now },
            accountDirectory: directory,
            loginHandleBinds: LoginHandleBindLedger(),
            restoreDirectory: directory,
            markers: RecoveryMarkers(defaults: defaults("kept-markers")),
            tokenProvider: tokens,
            grantNowMs: { now },
            attestationVerifier: Verifier(),
            keyResetHooks: AuthRepository.KeyResetHooks(
                republishOwnKeyPackage: { _ in }, refoundDeviceGroup: { _, _, _, _ in }),
            wipeAfterKeyReset: { [weak store] account in
                log.noteFull(account)
                store?.clear()
            },
            stagedWipeAfterKeyReset: { [weak store] account in
                log.noteStaged(account)
                store?.clear()
            },
            recallRecords: recalls,
            recallOperations: RecallOperations(
                status: { _ in throw URLError(.notConnectedToInternet) },
                takeBack: { _ in throw URLError(.notConnectedToInternet) }),
            ownDeviceId: { _ in nil },
            identityKeyMarks: IdentityKeyRecordMarks(defaults: defaults("kept-marks")),
            logoutOwed: InMemoryLogoutOwedStore(),
            rotationPending: pending,
            ownPredecessors: predecessors,
            rotatedNotice: notice,
            rotationStore: rotations,
            sessionUpgradeOperation: { refreshToken, accountId, seed in
                try upgrades.run(refreshToken, accountId, seed)
            }
        )
        repository.readDeviceGroupJournalToHead = { _ in
            log.noteJournalRead()
            return true
        }
        repository.onAccountKeysRotated = { secrets, predecessor in
            log.noteRotated(secrets.accountIdentitySeed, predecessor)
        }
        return Harness(
            repository: repository, pending: pending, predecessors: predecessors, upgrades: upgrades,
            recalls: recalls, tokens: tokens, log: log, notice: notice, rotations: rotations)
    }

    private func waitUntil(_ condition: @MainActor () -> Bool, timeout: TimeInterval = 5) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 20_000_000)
        }
        return condition()
    }

    /// A real `key-rotation` envelope by the account's old key.
    private func rotation(by signerSeed: Data? = nil) throws -> (envelope: Data, mnemonic: RecoveryMnemonic) {
        let mnemonic = RecoveryMnemonic.generate()
        let nowSecs = nowMs / 1000
        let proof = try signSupersessionProof(
            oldEd25519Seed: try Identity.fromSeed(seed: signerSeed ?? oldSeed).ed25519Seed(),
            newEd25519Seed: try Identity.fromSeed(seed: mnemonic.toSeed()).ed25519Seed(),
            issuedAtUnixSecs: nowSecs, expiresAtUnixSecs: nowSecs + KeyRotationWire.proofWindowSecs)
        return (
            try composeKeyRotationEnvelope(
                preparedAtMs: UInt64(nowMs), newEntropy: mnemonic.toEntropy(), supersessionProof: proof),
            mnemonic)
    }

    private var session: AuthState { .session(SessionAccount(accountIdB64: accountIdB64, username: "alice")) }

    /// The rotating device: while its own rotation's publish is owed the
    /// directory is behind it by design, so the landing rule and the device
    /// removal read "a publish is owed" exactly as after a reset.
    func testARotationWhosePublishIsOwedCountsAsAnOwedPublish() {
        let h = device()
        XCTAssertFalse(h.repository.keyResetPublishOwed(accountIdB64: accountIdB64))
        var record = KeyRotationRecord(targetDeviceIdB64: "ZGV2", startedAtMs: 1)
        record.phase = .announced
        h.rotations.put(record, accountIdB64: accountIdB64)
        XCTAssertFalse(h.repository.keyResetPublishOwed(accountIdB64: accountIdB64), "nothing changed yet")
        record.phase = .committed
        h.rotations.put(record, accountIdB64: accountIdB64)
        XCTAssertTrue(h.repository.keyResetPublishOwed(accountIdB64: accountIdB64))
    }

    /// The rotating device's step 6: the same swap, with everything kept.
    func testTheRotatingDeviceAdoptsItsOwnRotationAndKeepsTheOldKeyAsPredecessor() async throws {
        let h = device()
        let material = try KeyRotationMaterial.generate()
        try h.repository.storeOwnPendingKeyRotation(
            PendingKeyRotation(
                newSignaturePubkey: material.newSignaturePubkey, newEntropy: material.entropy,
                supersessionProof: Data(repeating: 1, count: 145), preparedAtMs: UInt64(nowMs),
                proofExpiresAtUnixSecs: nowMs / 1000 + 60),
            accountIdB64: accountIdB64)
        XCTAssertEqual(h.pending.load(accountIdB64: accountIdB64).count, 1)

        try await h.repository.adoptOwnKeyRotation(
            material: material, session: .fixture(access: "tok-rotated", refresh: "r2.rotated", generation: 2))

        XCTAssertEqual(h.repository.state, session, "the session stays: nothing is wiped")
        XCTAssertEqual(h.repository.identitySecrets?.accountIdentitySeed, material.seed)
        XCTAssertEqual(h.repository.identitySecrets?.session.keyGeneration, 2)
        XCTAssertEqual(h.predecessors.load(accountIdB64: accountIdB64), [oldSeed])
        XCTAssertEqual(DevicePhrase.read(accountIdB64: accountIdB64), material.phrase)
        XCTAssertEqual(h.tokens.current?.sessionToken, "tok-rotated")
        XCTAssertEqual(h.pending.load(accountIdB64: accountIdB64), [], "its own copy has done its work")
        XCTAssertEqual(h.log.rotated.first?.predecessor, oldSeed)
        XCTAssertFalse(h.repository.keysRotatedNotice, "the line is for the OTHER devices")
        XCTAssertEqual(h.log.full + h.log.staged, [])
    }

    // MARK: Pending

    func testAnEnvelopeInTheOwnDeviceGroupIsStoredAsPendingAndChangesNothing() throws {
        let h = device()
        let made = try rotation()

        h.repository.receiveKeyRotationEnvelope(made.envelope, arrival: .ownDeviceGroup)

        let stored = h.pending.load(accountIdB64: accountIdB64)
        XCTAssertEqual(stored.count, 1)
        XCTAssertEqual(
            stored.first?.newSignaturePubkey,
            try Identity.fromSeed(seed: made.mnemonic.toSeed()).signaturePublicKey(),
            "keyed on the new key")
        XCTAssertEqual(stored.first?.newEntropy, made.mnemonic.toEntropy())
        // Nothing else changed.
        XCTAssertEqual(h.repository.state, session)
        XCTAssertEqual(h.repository.identitySecrets?.accountIdentitySeed, oldSeed)
        XCTAssertFalse(h.repository.keysUpdating)
        XCTAssertFalse(h.repository.keysRotatedNotice)
        XCTAssertEqual(h.upgrades.calls.count, 0)
        XCTAssertEqual(h.predecessors.load(accountIdB64: accountIdB64), [])
    }

    func testAnEnvelopeFromAConversationIsDropped() throws {
        let h = device()
        h.repository.receiveKeyRotationEnvelope(try rotation().envelope, arrival: .elsewhere)
        XCTAssertEqual(h.pending.load(accountIdB64: accountIdB64), [])
    }

    func testAnEnvelopeNotByTheAccountKeyThisDeviceHoldsIsDropped() throws {
        let h = device()
        let stranger = Data(repeating: 0x33, count: 64)
        h.repository.receiveKeyRotationEnvelope(try rotation(by: stranger).envelope, arrival: .ownDeviceGroup)
        XCTAssertEqual(h.pending.load(accountIdB64: accountIdB64), [])
    }

    // MARK: Adopt

    func testSupersededWithAPendingEnvelopeAdoptsTheNewKeysAndKeepsEverything() async throws {
        let h = device()
        let made = try rotation()
        let newSeed = made.mnemonic.toSeed()
        h.repository.receiveKeyRotationEnvelope(made.envelope, arrival: .ownDeviceGroup)
        h.upgrades.answers = [
            .success(.fixture(access: "tok-new", accessExpiresAtMs: nowMs + 900_000, refresh: "r2.new", generation: 2))
        ]

        h.repository.keysSuperseded(accountIdB64: accountIdB64, publishedSignaturePubkey: nil)
        XCTAssertTrue(h.repository.keysUpdating, "Updating keys... while it adopts")
        let adopted = await waitUntil { h.repository.keysRotatedNotice }
        XCTAssertTrue(adopted)

        // The session stays; nothing was wiped; no recall record.
        XCTAssertEqual(h.repository.state, session)
        XCTAssertFalse(h.repository.keysUpdating)
        XCTAssertNil(h.repository.keysSignOut)
        XCTAssertNil(h.repository.recall)
        XCTAssertNil(h.recalls.record)
        XCTAssertEqual(h.log.full, [])
        XCTAssertEqual(h.log.staged, [])
        XCTAssertEqual(h.log.journalReads, 0, "an envelope was held: no journal read is needed")
        // The seed and the phrase were swapped; the old key is the predecessor.
        XCTAssertEqual(h.repository.identitySecrets?.accountIdentitySeed, newSeed)
        XCTAssertEqual(h.predecessors.load(accountIdB64: accountIdB64), [oldSeed])
        XCTAssertEqual(
            try AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: accountIdB64), newSeed)
        XCTAssertEqual(DevicePhrase.read(accountIdB64: accountIdB64), made.mnemonic.phrase())
        // The upgrade was asked with the OLD refresh credential and the NEW
        // account key; the pair at the new generation is installed.
        XCTAssertEqual(h.upgrades.calls.count, 1)
        XCTAssertEqual(h.upgrades.calls.first?.refreshToken, "r2.old")
        XCTAssertEqual(h.upgrades.calls.first?.accountId, accountId)
        XCTAssertEqual(
            h.upgrades.calls.first?.seed, try Identity.fromSeed(seed: newSeed).ed25519Seed())
        XCTAssertEqual(h.tokens.current?.sessionToken, "tok-new")
        XCTAssertEqual(h.tokens.current?.keyGeneration, 2)
        XCTAssertFalse(h.tokens.isSuperseded)
        // What derives from the seed elsewhere is told; the pending envelope
        // has done its work; the one line is owed until dismissed.
        XCTAssertEqual(h.log.rotated.count, 1)
        XCTAssertEqual(h.log.rotated.first?.seed, newSeed)
        XCTAssertEqual(h.log.rotated.first?.predecessor, oldSeed)
        XCTAssertEqual(h.pending.load(accountIdB64: accountIdB64), [])
        XCTAssertTrue(h.notice.isOwed(accountIdB64: accountIdB64))
        h.repository.dismissKeysRotatedNotice()
        XCTAssertFalse(h.repository.keysRotatedNotice)
        XCTAssertFalse(h.notice.isOwed(accountIdB64: accountIdB64))
    }

    // MARK: Superseded without a pending envelope

    func testSupersededWithNoPendingEnvelopeReadsTheJournalOnceThenIsAReset() async throws {
        let h = device()

        h.repository.keysSuperseded(accountIdB64: accountIdB64, publishedSignaturePubkey: nil)
        let asked = await waitUntil { h.repository.recall != nil }
        XCTAssertTrue(asked, "the staged wipe and \"Was it you?\" of a reset")

        XCTAssertEqual(h.log.journalReads, 1, "once")
        XCTAssertEqual(h.upgrades.calls.count, 0)
        XCTAssertNotNil(h.recalls.record)
        XCTAssertFalse(h.repository.keysUpdating)
        let wiped = await waitUntil { h.log.staged == [self.accountIdB64] }
        XCTAssertTrue(wiped)
        XCTAssertEqual(h.log.full, [])
    }

    func testAnEnvelopeFoundByTheJournalReadIsAdoptedInsteadOfAReset() async throws {
        let h = device()
        let made = try rotation()
        // The journal read delivers the envelope this device had not seen.
        h.repository.readDeviceGroupJournalToHead = { [weak repository = h.repository] _ in
            h.log.noteJournalRead()
            repository?.receiveKeyRotationEnvelope(made.envelope, arrival: .ownDeviceGroup)
            return true
        }
        h.upgrades.answers = [.success(.fixture(access: "tok-new", refresh: "r2.new", generation: 2))]

        h.repository.keysSuperseded(accountIdB64: accountIdB64, publishedSignaturePubkey: nil)
        let adopted = await waitUntil { h.repository.keysRotatedNotice }

        XCTAssertTrue(adopted)
        XCTAssertEqual(h.log.journalReads, 1)
        XCTAssertNil(h.repository.recall)
        XCTAssertEqual(h.log.staged, [])
        XCTAssertEqual(h.repository.identitySecrets?.accountIdentitySeed, made.mnemonic.toSeed())
    }

    func testAPendingEnvelopeTheServerDoesNotAcceptIsAReset() async throws {
        let h = device()
        h.repository.receiveKeyRotationEnvelope(try rotation().envelope, arrival: .ownDeviceGroup)
        h.upgrades.answers = [
            .failure(AuthClientError.http(
                status: 401, endpoint: "/auth/session/refresh", body: #"{"error":"key-generation-superseded"}"#))
        ]

        h.repository.keysSuperseded(accountIdB64: accountIdB64, publishedSignaturePubkey: nil)
        let asked = await waitUntil { h.repository.recall != nil }

        XCTAssertTrue(asked, "the envelope's key never became the account's: the account was reset")
        XCTAssertEqual(h.repository.identitySecrets == nil || h.repository.recall != nil, true)
        XCTAssertEqual(h.predecessors.load(accountIdB64: accountIdB64), [], "nothing was adopted")
    }

    func testTheDeviceThatWasRemovedWipesEverything() async throws {
        let h = device()
        h.repository.receiveKeyRotationEnvelope(try rotation().envelope, arrival: .ownDeviceGroup)
        h.upgrades.answers = [
            .failure(AuthClientError.http(
                status: 401, endpoint: "/auth/session/refresh", body: #"{"error":"device-revoked"}"#))
        ]

        h.repository.keysSuperseded(accountIdB64: accountIdB64, publishedSignaturePubkey: nil)
        let removed = await waitUntil { h.repository.keysSignOut == .removed }

        XCTAssertTrue(removed)
        XCTAssertEqual(h.repository.keysSignOutCause, .deviceRemoved)
        XCTAssertEqual(h.log.full, [accountIdB64], "the full wipe")
        XCTAssertNil(h.repository.recall, "a removal keeps no recall record")
    }

    func testNoVerdictWipesNothingAndStaysUpdating() async throws {
        let h = device()
        h.repository.receiveKeyRotationEnvelope(try rotation().envelope, arrival: .ownDeviceGroup)
        h.upgrades.answers = [
            .failure(AuthClientError.network(endpoint: "/auth/session/refresh", underlying: URLError(.timedOut)))
        ]

        h.repository.keysSuperseded(accountIdB64: accountIdB64, publishedSignaturePubkey: nil)
        let tried = await waitUntil { h.upgrades.calls.count == 1 }
        XCTAssertTrue(tried)
        try await Task.sleep(nanoseconds: 100_000_000)

        XCTAssertTrue(h.repository.keysUpdating, "the line stays; it is tried again")
        XCTAssertEqual(h.repository.state, session)
        XCTAssertNil(h.repository.recall)
        XCTAssertNil(h.repository.keysSignOut)
        XCTAssertEqual(h.log.full + h.log.staged, [])
        XCTAssertEqual(h.pending.load(accountIdB64: accountIdB64).count, 1, "the envelope is kept")
        XCTAssertEqual(h.repository.identitySecrets?.accountIdentitySeed, oldSeed)
    }

    func testAClockStillRefusedAfterTheRetryDoesNotWipe() async throws {
        let h = device()
        h.repository.receiveKeyRotationEnvelope(try rotation().envelope, arrival: .ownDeviceGroup)
        h.upgrades.answers = [
            .failure(AuthClientError.http(
                status: 401, endpoint: "/auth/session/refresh",
                body: #"{"error":"statement-stale","server_time_ms":1700000000000}"#))
        ]

        h.repository.keysSuperseded(accountIdB64: accountIdB64, publishedSignaturePubkey: nil)
        let tried = await waitUntil { h.upgrades.calls.count == 1 }
        XCTAssertTrue(tried)
        try await Task.sleep(nanoseconds: 100_000_000)

        XCTAssertNil(h.repository.recall, "a wrong clock is never read as a reset")
        XCTAssertEqual(h.log.full + h.log.staged, [])
        XCTAssertTrue(h.repository.keysUpdating)
    }

    func testThePredecessorSlotFailingStopsTheAdoptionBeforeAnythingChanges() async throws {
        let h = device()
        struct KeychainDown: Error {}
        h.predecessors.addError = KeychainDown()
        h.repository.receiveKeyRotationEnvelope(try rotation().envelope, arrival: .ownDeviceGroup)
        h.upgrades.answers = [.success(.fixture(access: "tok-new", refresh: "r2.new", generation: 2))]

        h.repository.keysSuperseded(accountIdB64: accountIdB64, publishedSignaturePubkey: nil)
        let tried = await waitUntil { h.upgrades.calls.count == 1 }
        XCTAssertTrue(tried)
        try await Task.sleep(nanoseconds: 100_000_000)

        XCTAssertEqual(
            h.repository.identitySecrets?.accountIdentitySeed, oldSeed,
            "without the old key kept, the binding would be re-minted: nothing is swapped")
        XCTAssertEqual(h.log.rotated.count, 0)
        XCTAssertNil(h.repository.recall)
    }

    func testADeviceWithoutKeysSignsOutAtOnceAsBefore() async throws {
        let h = device(holdsKeys: false)

        h.repository.keysSuperseded(accountIdB64: accountIdB64, publishedSignaturePubkey: nil)
        let done = await waitUntil { h.repository.keysSignOut == .removed }

        XCTAssertTrue(done)
        XCTAssertEqual(h.log.journalReads, 0)
        XCTAssertEqual(h.upgrades.calls.count, 0)
        XCTAssertEqual(h.log.full, [accountIdB64])
    }
}

// MARK: - The proven-predecessor rule

final class ProvenPredecessorTests: XCTestCase {

    private func freshDefaults() -> UserDefaults {
        let name = "lakko.test.predecessors.\(UUID().uuidString)"
        let d = UserDefaults(suiteName: name)!
        d.removePersistentDomain(forName: name)
        return d
    }

    func testOnlyAVerifiedProofEarnsPredecessorTreatment() {
        XCTAssertEqual(ProvenPredecessorRule.treatment(for: .verifiedProof), .provenPredecessor)
        XCTAssertEqual(ProvenPredecessorRule.treatment(for: .attestationOnly), .none, "a reset is not a rotation")
        XCTAssertEqual(ProvenPredecessorRule.treatment(for: .none), .none)
        XCTAssertFalse(ProvenPredecessorRule.flagsAsReset(.verifiedProof), "a proven rotation is never flagged")
        XCTAssertTrue(ProvenPredecessorRule.flagsAsReset(.attestationOnly))
        XCTAssertTrue(ProvenPredecessorRule.flagsAsReset(.none))
    }

    func testAnUnprovenChangeNeverRecordsAPredecessor() {
        var map = ProvenPredecessors()
        XCTAssertFalse(map.record(oldKeyB64: "old", newKeyB64: "new", evidence: .attestationOnly))
        XCTAssertFalse(map.record(oldKeyB64: "old", newKeyB64: "new", evidence: .none))
        XCTAssertTrue(map.isEmpty)
        XCTAssertFalse(map.samePerson("old", "new"), "two keys with nothing proven between them are two people")
        XCTAssertEqual(map.current(of: "old"), "old")
    }

    func testAcrossAProvenRotationTheOldKeyIsTheSamePerson() {
        var map = ProvenPredecessors()
        XCTAssertTrue(map.record(oldKeyB64: "k1", newKeyB64: "k2", evidence: .verifiedProof))

        XCTAssertTrue(map.samePerson("k1", "k2"))
        XCTAssertTrue(map.samePerson("k2", "k1"))
        XCTAssertEqual(map.current(of: "k1"), "k2")
        XCTAssertEqual(map.current(of: "k2"), "k2")
        XCTAssertEqual(map.allKeys(of: "k1"), ["k2", "k1"])
        XCTAssertEqual(map.predecessors(of: "k2"), ["k1"])
        XCTAssertFalse(map.samePerson("k1", "stranger"))
        XCTAssertFalse(map.record(oldKeyB64: "k1", newKeyB64: "k2", evidence: .verifiedProof), "already recorded")
    }

    func testAChainOfProvenRotationsResolvesToTheNewestKey() {
        var map = ProvenPredecessors()
        map.record(oldKeyB64: "k1", newKeyB64: "k2", evidence: .verifiedProof)
        map.record(oldKeyB64: "k2", newKeyB64: "k3", evidence: .verifiedProof)

        XCTAssertEqual(map.current(of: "k1"), "k3")
        XCTAssertTrue(map.samePerson("k1", "k3"))
        XCTAssertEqual(map.allKeys(of: "k2"), ["k3", "k2", "k1"])
        XCTAssertEqual(map.predecessors(of: "k1"), ["k2", "k1"])
    }

    func testOldKeysDoNotComeBack() {
        var map = ProvenPredecessors()
        map.record(oldKeyB64: "k1", newKeyB64: "k2", evidence: .verifiedProof)
        XCTAssertFalse(
            map.record(oldKeyB64: "k2", newKeyB64: "k1", evidence: .verifiedProof),
            "a proof leading back to a superseded key closes a cycle and is refused")
        XCTAssertFalse(map.record(oldKeyB64: "k1", newKeyB64: "k1", evidence: .verifiedProof))
        XCTAssertFalse(map.record(oldKeyB64: "", newKeyB64: "k1", evidence: .verifiedProof))
        XCTAssertEqual(map.current(of: "k1"), "k2")
    }

    func testALaterResetMakesEveryPredecessorASupersededKey() {
        var map = ProvenPredecessors()
        map.record(oldKeyB64: "k1", newKeyB64: "k2", evidence: .verifiedProof)
        map.record(oldKeyB64: "other1", newKeyB64: "other2", evidence: .verifiedProof)

        XCTAssertTrue(map.forgetPerson(currentKeyB64: "k2"))

        XCTAssertFalse(map.samePerson("k1", "k2"))
        XCTAssertTrue(map.samePerson("other1", "other2"), "another person's predecessors are untouched")
        XCTAssertFalse(map.forgetPerson(currentKeyB64: "k2"))
    }

    func testTheStoreIsPerAccountAndHoldsPublicKeysOnly() {
        let defaults = freshDefaults()
        let alice = ProvenPredecessorStore(ownerAccountIdB64: "alice", defaults: defaults)
        let bob = ProvenPredecessorStore(ownerAccountIdB64: "bob", defaults: defaults)

        XCTAssertTrue(alice.record(oldKeyB64: "k1", newKeyB64: "k2", evidence: .verifiedProof))
        XCTAssertFalse(alice.record(oldKeyB64: "x1", newKeyB64: "x2", evidence: .attestationOnly))

        XCTAssertTrue(alice.load().samePerson("k1", "k2"))
        XCTAssertFalse(alice.load().samePerson("x1", "x2"))
        XCTAssertTrue(bob.load().isEmpty, "another account on the device sees nothing")
        ProvenPredecessorStore.clear(ownerAccountIdB64: "alice", defaults: defaults)
        XCTAssertTrue(alice.load().isEmpty)
    }

    // MARK: The ADR-0016 gate

    private func client(
        pins: PeerPinStore, predecessors: ProvenPredecessorStore, defaults: UserDefaults
    ) -> ContactsClient {
        ContactsClient(
            pinStore: pins,
            transparencyVerify: TransparencyVerify(
                client: nil, operatorKey: Data(repeating: 1, count: 32),
                lastSeenRoot: LastSeenRoot(defaults: defaults), verifyInclusion: { _ in }),
            predecessors: predecessors)
    }

    private func pubB64(_ seed: Data) throws -> String {
        base64Encode(try Identity.fromSeed(seed: seed).signaturePublicKey())
    }

    private func proof(old: Data, new: Data) throws -> Data {
        let now = Int64(Date().timeIntervalSince1970)
        return try signSupersessionProof(
            oldEd25519Seed: try Identity.fromSeed(seed: old).ed25519Seed(),
            newEd25519Seed: try Identity.fromSeed(seed: new).ed25519Seed(),
            issuedAtUnixSecs: now - 10, expiresAtUnixSecs: now + KeyRotationWire.proofWindowSecs)
    }

    /// A contact whose published key changed WITH a valid supersession proof
    /// (the generation moved too, as after a rotation, which also carries an
    /// attestation at the directory): accepted silently, re-pinned, the old
    /// key recorded as the proven predecessor, and NOT reported as a reset.
    func testAProvenRotationIsAcceptedSilentlyRePinnedAndNeverFlaggedAsAReset() async throws {
        let defaults = freshDefaults()
        let pins = PeerPinStore(defaults: defaults, ownerAccountIdB64: "owner")
        let predecessors = ProvenPredecessorStore(ownerAccountIdB64: "owner", defaults: defaults)
        let contacts = client(pins: pins, predecessors: predecessors, defaults: defaults)
        let contactId = Data(repeating: 0xC1, count: 16)
        let oldSeed = Data(repeating: 0x11, count: 32)
        let newSeed = Data(repeating: 0x22, count: 32)
        var reported = 0
        let unsubscribe = ResetEvents.subscribe { _ in reported += 1 }
        defer { unsubscribe() }

        // First sight: TOFU.
        try await contacts.verifyBundleAccountKey(
            accountId: contactId,
            bundle: IdentityFetchResponse(signaturePubkey: try pubB64(oldSeed), x25519Pubkey: ""))
        XCTAssertTrue(predecessors.load().isEmpty)

        // The key changed, by proof, at a higher generation.
        try await contacts.verifyBundleAccountKey(
            accountId: contactId,
            bundle: IdentityFetchResponse(
                signaturePubkey: try pubB64(newSeed), x25519Pubkey: "",
                supersessionProof: base64Encode(try proof(old: oldSeed, new: newSeed)),
                keyGeneration: 2))

        XCTAssertEqual(
            pins.readPin(accountIdB64: base64Encode(contactId))?.pinnedSignaturePubkeyB64,
            try pubB64(newSeed), "re-pinned")
        XCTAssertTrue(predecessors.load().samePerson(try pubB64(oldSeed), try pubB64(newSeed)))
        XCTAssertEqual(predecessors.load().current(of: try pubB64(oldSeed)), try pubB64(newSeed))
        XCTAssertEqual(reported, 0, "a proven rotation is not a reset: no flag, no banner, no closed chat")
    }

    func testAChangeWithoutAProofIsRefusedAndGetsNoPredecessorTreatment() async throws {
        let defaults = freshDefaults()
        let pins = PeerPinStore(defaults: defaults, ownerAccountIdB64: "owner")
        let predecessors = ProvenPredecessorStore(ownerAccountIdB64: "owner", defaults: defaults)
        let contacts = client(pins: pins, predecessors: predecessors, defaults: defaults)
        let contactId = Data(repeating: 0xC2, count: 16)
        let oldSeed = Data(repeating: 0x11, count: 32)
        let newSeed = Data(repeating: 0x22, count: 32)

        try await contacts.verifyBundleAccountKey(
            accountId: contactId,
            bundle: IdentityFetchResponse(signaturePubkey: try pubB64(oldSeed), x25519Pubkey: ""))
        do {
            try await contacts.verifyBundleAccountKey(
                accountId: contactId,
                bundle: IdentityFetchResponse(
                    signaturePubkey: try pubB64(newSeed), x25519Pubkey: "", keyGeneration: 2))
            XCTFail("the gate must refuse an unproven change")
        } catch {
            XCTAssertEqual((error as? SupersessionRefused)?.reason, .proofAbsent)
        }

        XCTAssertTrue(predecessors.load().isEmpty, "an unproven change never gets predecessor treatment")
        XCTAssertEqual(
            pins.readPin(accountIdB64: base64Encode(contactId))?.pinnedSignaturePubkeyB64,
            try pubB64(oldSeed), "not re-pinned")
    }

    func testAProofByAnotherKeyIsRefusedAndGetsNoPredecessorTreatment() async throws {
        let defaults = freshDefaults()
        let pins = PeerPinStore(defaults: defaults, ownerAccountIdB64: "owner")
        let predecessors = ProvenPredecessorStore(ownerAccountIdB64: "owner", defaults: defaults)
        let contacts = client(pins: pins, predecessors: predecessors, defaults: defaults)
        let contactId = Data(repeating: 0xC3, count: 16)
        let oldSeed = Data(repeating: 0x11, count: 32)
        let newSeed = Data(repeating: 0x22, count: 32)
        let stranger = Data(repeating: 0x33, count: 32)

        try await contacts.verifyBundleAccountKey(
            accountId: contactId,
            bundle: IdentityFetchResponse(signaturePubkey: try pubB64(oldSeed), x25519Pubkey: ""))
        do {
            try await contacts.verifyBundleAccountKey(
                accountId: contactId,
                bundle: IdentityFetchResponse(
                    signaturePubkey: try pubB64(newSeed), x25519Pubkey: "",
                    supersessionProof: base64Encode(try proof(old: stranger, new: newSeed)),
                    keyGeneration: 2))
            XCTFail("the gate must refuse a proof the pinned key did not sign")
        } catch {
            XCTAssertEqual((error as? SupersessionRefused)?.reason, .proofInvalid)
        }
        XCTAssertTrue(predecessors.load().isEmpty)
    }
}

// MARK: - Names across a proven rotation

@MainActor
final class ProvenPredecessorNameTests: XCTestCase {

    private final class Persistence: ProfilePersistence {
        var blobs: [String: Data] = [:]
        func load(accountIdB64: String) -> Data? { blobs[accountIdB64] }
        func save(_ data: Data, accountIdB64: String) { blobs[accountIdB64] = data }
        func delete(accountIdB64: String) { blobs[accountIdB64] = nil }
    }

    func testANameKnownUnderTheOldKeyResolvesForTheNewKeyOnlyAcrossAProvenRotation() throws {
        let suite = "predecessor-names-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let directory = ProfileDirectory(
            received: ReceivedProfileStore(
                persistence: Persistence(),
                isNewer: { profileIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1) }),
            verifier: UsernameVerifier(resolve: { _ in .notRegistered }, defaults: defaults),
            ownStore: OwnProfileStore(defaults: defaults))
        directory.verifiesOnResolve = false
        directory.setRenames(from: [])
        directory.activate(accountIdB64: suite, username: nil)
        let oldKey = Data(repeating: 0x41, count: 32)
        let newKey = Data(repeating: 0x42, count: 32)
        let unrelated = Data(repeating: 0x43, count: 32)
        directory.acceptReceived(
            ProfileClaim(displayName: "Dana", username: nil, updatedAtMs: 5),
            signatureKeyB64: base64Encode(oldKey))
        XCTAssertEqual(directory.resolve(signatureKey: oldKey).name, "Dana")
        let before = directory.resolve(signatureKey: newKey).name
        XCTAssertNotEqual(before, "Dana", "nothing is proven yet: the new key is nobody known")

        var map = ProvenPredecessors()
        map.record(oldKeyB64: base64Encode(oldKey), newKeyB64: base64Encode(newKey), evidence: .verifiedProof)
        directory.setProvenPredecessors(map)

        XCTAssertEqual(directory.resolve(signatureKey: newKey).name, "Dana", "the same person, by proof")
        XCTAssertEqual(directory.resolve(signatureKey: oldKey).name, "Dana")
        XCTAssertNotEqual(directory.resolve(signatureKey: unrelated).name, "Dana")

        // An UNPROVEN change gets nothing.
        var unproven = ProvenPredecessors()
        unproven.record(
            oldKeyB64: base64Encode(oldKey), newKeyB64: base64Encode(newKey), evidence: .attestationOnly)
        directory.setProvenPredecessors(unproven)
        XCTAssertEqual(directory.resolve(signatureKey: newKey).name, before)
    }
}

// MARK: - The stale statement and the recall's finish

final class StatementStaleTests: XCTestCase {

    func testTheStaleAnswerCarriesTheServersTime() {
        XCTAssertEqual(
            StatementStale.serverTimeMs(
                status: 401, body: #"{"error":"statement-stale","server_time_ms":1700000000123}"#),
            1_700_000_000_123)
        XCTAssertNil(StatementStale.serverTimeMs(status: 401, body: #"{"error":"recall-refused"}"#))
        XCTAssertNil(StatementStale.serverTimeMs(status: 401, body: #"{"error":"statement-stale"}"#))
        XCTAssertNil(
            StatementStale.serverTimeMs(
                status: 403, body: #"{"error":"statement-stale","server_time_ms":5}"#))
        XCTAssertNil(StatementStale.serverTimeMs(status: 401, body: "not json"))
    }

    private func stale(_ time: Int64 = 777) -> AuthClientError {
        .http(status: 401, endpoint: "/x", body: #"{"error":"statement-stale","server_time_ms":\#(time)}"#)
    }

    func testAStaleStatementIsSignedAgainAtTheServersTimeAndRetriedOnce() async throws {
        var attempts: [Int64?] = []
        let value: String = try await StatementStale.retrying { serverTimeMs in
            attempts.append(serverTimeMs)
            if serverTimeMs == nil { throw self.stale(777) }
            return "ok"
        }
        XCTAssertEqual(value, "ok")
        XCTAssertEqual(attempts, [nil, 777], "first at the device's own time, then at the server's")
    }

    func testASecondStaleAnswerIsThrownAndNotRetriedAgain() async {
        var attempts = 0
        do {
            let _: String = try await StatementStale.retrying { _ in
                attempts += 1
                throw self.stale()
            }
            XCTFail("must throw")
        } catch {
            XCTAssertTrue(StatementStale.isStale(error))
        }
        XCTAssertEqual(attempts, 2, "once, not a loop")
    }

    func testAnyOtherAnswerIsNotRetried() async {
        var attempts = 0
        do {
            let _: String = try await StatementStale.retrying { _ in
                attempts += 1
                throw AuthClientError.http(status: 401, endpoint: "/x", body: #"{"error":"recall-refused"}"#)
            }
            XCTFail("must throw")
        } catch {
            XCTAssertFalse(StatementStale.isStale(error))
        }
        XCTAssertEqual(attempts, 1)
    }

    /// The device must NOT destroy its recall record on a stale statement.
    func testAStaleStatementIsNeverTheRefusalThatEndsARecall() {
        let body = #"{"error":"statement-stale","server_time_ms":5}"#
        XCTAssertEqual(RecallAnswers.refusal(status: 401, body: body), .failed)
        XCTAssertEqual(
            RecallAnswers.status(of: AuthClientError.http(status: 401, endpoint: "/x", body: body)), .unknown,
            "nothing is known: the record stays")
        // The real refusal still ends it.
        XCTAssertEqual(RecallAnswers.refusal(status: 401, body: #"{"error":"recall-refused"}"#), .refused)
        // And the screen keeps the question on a status that is not known.
        let record = RecallRecord(
            accountIdB64: base64Encode(Data(repeating: 1, count: 16)),
            oldSeed: Data(repeating: 2, count: 32), username: "alice", learnedAtMs: 0)
        let transition = RecallMachine.afterStatus(RecallScreenState(record: record), .unknown, nowMs: 1)
        guard case .show = transition else { return XCTFail("the recall must not end") }
    }

    func testTheStatusRouteSignsAgainAtTheServersTimeAndSendsAgain() async throws {
        let accountId = Data((0..<16).map { UInt8(0xB0 | $0) })
        let record = RecallRecord(
            accountIdB64: base64Encode(accountId), oldSeed: Data(repeating: 7, count: 32),
            username: "alice", learnedAtMs: 0)
        var calls = 0
        let host = NameRouteStub.scope { _, path, _ in
            XCTAssertEqual(path, "/account/recall/status")
            calls += 1
            if calls == 1 {
                return (401, #"{"error":"statement-stale","server_time_ms":1700000000000}"#)
            }
            return (200, #"{"anchor_seq":4,"expires_at_ms":9,"changed_at_ms":8,"attempts_remaining":20}"#)
        }

        let window = try await NameRouteStub.client(host).recallStatus(record: record)

        XCTAssertEqual(window.anchorSeq, 4)
        let requests = NameRouteStub.requests(host)
        XCTAssertEqual(requests.count, 2)
        let first = try XCTUnwrap(requests[0].json["statement_b64"] as? String)
        let second = try XCTUnwrap(requests[1].json["statement_b64"] as? String)
        XCTAssertNotEqual(first, second, "a new statement, signed at the server's time")
    }

    // The recall's finish is retried with the SAME bytes on a lost answer.

    private struct Lost: Error {}

    func testALostFinishAnswerIsAskedAgainWithTheIdenticalRequest() async throws {
        let body = Data("the-exact-request".utf8)
        var sent: [Data] = []
        let value: Int = try await RecallFinishRetry.sendIdentical(body: body) { bytes in
            sent.append(bytes)
            if sent.count < 3 {
                throw AuthClientError.network(endpoint: "/account/recall/finish", underlying: URLError(.timedOut))
            }
            return 7
        }
        XCTAssertEqual(value, 7)
        XCTAssertEqual(sent, [body, body, body], "byte-identical every time")
    }

    func testTheFinishIsNotSentMoreOftenThanTheBound() async {
        var sends = 0
        do {
            let _: Int = try await RecallFinishRetry.sendIdentical(body: Data([1])) { _ in
                sends += 1
                throw AuthClientError.network(endpoint: "/x", underlying: URLError(.timedOut))
            }
            XCTFail("must throw")
        } catch {
            XCTAssertTrue(RecallFinishRetry.resends(after: error))
        }
        XCTAssertEqual(sends, RecallFinishRetry.maxSends)
    }

    func testAnAnswerIsAVerdictAndIsNotAskedAgain() async {
        var sends = 0
        do {
            let _: Int = try await RecallFinishRetry.sendIdentical(body: Data([1])) { _ in
                sends += 1
                throw AuthClientError.http(status: 401, endpoint: "/x", body: #"{"error":"wrong-password"}"#)
            }
            XCTFail("must throw")
        } catch {
            XCTAssertFalse(RecallFinishRetry.resends(after: error))
        }
        XCTAssertEqual(sends, 1)
    }
}

// MARK: - The root endorsement of a reset, a recall and a rotation

private final class RecordingDs: DsStatementSending {
    var sent: [DsStatementRequest] = []
    var answers: [DsStatementError?] = []
    func putStatement(_ request: DsStatementRequest) async throws {
        sent.append(request)
        if !answers.isEmpty, let error = answers.removeFirst() { throw error }
    }
}

final class RootEndorsementTests: XCTestCase {

    private let journalId = Data(repeating: 0x77, count: 32)
    private let newAccountSeed = Data(repeating: 0x21, count: 64)
    private let oldAccountSeed = Data(repeating: 0x12, count: 64)

    private func json(_ request: DsStatementRequest) throws -> [String: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: Data(request.bodyJson.utf8)) as? [String: Any])
    }

    private func root(_ seed: Data) throws -> DsStatementSignerProtocol {
        try dsJournalRootSigner(accountIdentity: try Identity.fromSeed(seed: seed), journalId: journalId)
    }

    /// Part A: the attested reset a key reset AND a recall end with carries
    /// the endorsement by the NEW account key over the journal, the
    /// generation and the reset's signer key.
    func testTheAttestedResetSendsTheRootEndorsement() async throws {
        let ds = RecordingDs()
        let newRoot = try root(newAccountSeed)
        let attestation = Data(repeating: 0xA7, count: 177)

        try await JournalOwnership.attestedReset(
            root: newRoot, journalId: journalId, epoch: 1,
            epochKey: try Identity.fromSeed(seed: Data(repeating: 5, count: 32)).signaturePublicKey(),
            attestation: attestation,
            endorse: { rootKey in
                try RootEndorsement.sign(
                    accountIdentitySeed: self.newAccountSeed, journalId: self.journalId,
                    keyGeneration: 3, rootKey: rootKey)
            },
            ds: ds)

        XCTAssertEqual(ds.sent.count, 1)
        XCTAssertTrue(ds.sent[0].path.hasSuffix("/reset"))
        let body = try json(ds.sent[0])
        XCTAssertEqual(body["key_reset_attest_b64"] as? String, attestation.base64EncodedString())
        let endorsement = try XCTUnwrap(
            (body["root_endorsement_b64"] as? String).flatMap { Data(base64Encoded: $0) })
        XCTAssertEqual(endorsement.count, Int(dsRootEndorsementLen()))
        // Ed25519 is deterministic: exactly the core's signature over the
        // reset's SIGNER key (the key that becomes the root).
        XCTAssertEqual(
            endorsement,
            try signDsRootEndorsement(
                newAccountEd25519Seed: try Identity.fromSeed(seed: newAccountSeed).ed25519Seed(),
                journalId: journalId, keyGeneration: 3, rootKey: newRoot.publicKey()))
        XCTAssertNotNil(body["statement_b64"])
    }

    func testTheEndorsementBindsTheGenerationAndTheRootKey() throws {
        let key = try root(newAccountSeed).publicKey()
        let endorsed = try RootEndorsement.sign(
            accountIdentitySeed: newAccountSeed, journalId: journalId, keyGeneration: 3, rootKey: key)
        XCTAssertNotEqual(
            endorsed,
            try RootEndorsement.sign(
                accountIdentitySeed: newAccountSeed, journalId: journalId, keyGeneration: 4, rootKey: key))
        XCTAssertNotEqual(
            endorsed,
            try RootEndorsement.sign(
                accountIdentitySeed: newAccountSeed, journalId: journalId, keyGeneration: 3,
                rootKey: try root(oldAccountSeed).publicKey()))
        XCTAssertNotEqual(
            endorsed,
            try RootEndorsement.sign(
                accountIdentitySeed: oldAccountSeed, journalId: journalId, keyGeneration: 3, rootKey: key),
            "signed by the NEW account key, not the old one")
    }

    /// Part B step 9: the rotation's root transfer is signed by the OLD root,
    /// names the new root, and endorses the NEW root key (the transfer's
    /// `new_root`, not its signer).
    func testTheRotationsRootTransferIsByTheOldRootAndEndorsesTheNewRoot() throws {
        let oldRoot = try root(oldAccountSeed)
        let newRootKey = try root(newAccountSeed).publicKey()
        let attestation = Data(repeating: 0xA7, count: 177)
        var endorsedKey: Data?
        var endorsedGeneration: UInt64?

        let request = try RootRotationRequest.build(
            oldRoot: oldRoot, newRootKey: newRootKey, journalId: journalId,
            attestation: attestation, keyGeneration: 5,
            endorse: { rootKey, generation in
                endorsedKey = rootKey
                endorsedGeneration = generation
                return Data(repeating: 0xEE, count: 64)
            })

        XCTAssertTrue(request.path.hasSuffix("/root"))
        XCTAssertEqual(endorsedKey, newRootKey)
        XCTAssertNotEqual(endorsedKey, oldRoot.publicKey())
        XCTAssertEqual(endorsedGeneration, 5)
        let body = try json(request)
        XCTAssertEqual(body["key_reset_attest_b64"] as? String, attestation.base64EncodedString())
        XCTAssertEqual(
            body["root_endorsement_b64"] as? String, Data(repeating: 0xEE, count: 64).base64EncodedString())
        // The statement itself is the plain transfer by the old root.
        let plain = try oldRoot.rootTransfer(journalId: journalId, newRoot: newRootKey)
        XCTAssertEqual(request.path, plain.path)
    }

    func testWithoutAnAttestationTheTransferGoesPlain() throws {
        let oldRoot = try root(oldAccountSeed)
        let request = try RootRotationRequest.build(
            oldRoot: oldRoot, newRootKey: try root(newAccountSeed).publicKey(), journalId: journalId,
            attestation: nil, keyGeneration: nil,
            endorse: { _, _ in
                XCTFail("nothing to endorse")
                return Data()
            })
        let body = try json(request)
        XCTAssertNil(body["key_reset_attest_b64"])
        XCTAssertNil(body["root_endorsement_b64"])
        XCTAssertNotNil(body["statement_b64"])
    }
}

// MARK: - The predecessor mailbox

final class PredecessorMailboxTests: XCTestCase {

    func testTheOldMailboxIsReadAtMostOnceAMinuteWithACursorOfItsOwn() {
        XCTAssertTrue(PredecessorMailbox.due(lastPollMs: 0, nowMs: 5))
        XCTAssertFalse(PredecessorMailbox.due(lastPollMs: 1_000, nowMs: 1_000 + 59_999))
        XCTAssertTrue(PredecessorMailbox.due(lastPollMs: 1_000, nowMs: 1_000 + 60_000))

        let suite = "predecessor-cursor-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = PredecessorMailbox.CursorStore(defaults: defaults)
        XCTAssertEqual(store.cursor(accountIdB64: "a", keyB64: "k1"), 0)
        store.advance(accountIdB64: "a", keyB64: "k1", to: 7)
        store.advance(accountIdB64: "a", keyB64: "k1", to: 3)
        XCTAssertEqual(store.cursor(accountIdB64: "a", keyB64: "k1"), 7, "it never goes back")
        XCTAssertEqual(store.cursor(accountIdB64: "a", keyB64: "k2"), 0, "per predecessor key")
        XCTAssertEqual(store.cursor(accountIdB64: "b", keyB64: "k1"), 0, "per account")
        store.clear(accountIdB64: "a")
        XCTAssertEqual(store.cursor(accountIdB64: "a", keyB64: "k1"), 0)
    }
}

// MARK: - The channel owner handover

final class ChannelOwnerHandoverTests: XCTestCase {

    private func refused(_ status: Int, _ code: String) -> ChannelsApiError {
        ChannelsApiError(status: status, code: code, detail: "")
    }

    func testTheOldKeysAnswerDecidesWhatFollows() {
        XCTAssertEqual(ChannelOwnerHandover.afterOldKey(nil), .owned)
        XCTAssertEqual(
            ChannelOwnerHandover.afterOldKey(refused(401, "owner-proof-required")), .probeWithNewKey)
        XCTAssertEqual(ChannelOwnerHandover.afterOldKey(refused(409, "owner-changed")), .probeWithNewKey)
        XCTAssertEqual(ChannelOwnerHandover.afterOldKey(refused(503, "unknown")), .retry)
        XCTAssertEqual(ChannelOwnerHandover.afterOldKey(URLError(.timedOut)), .retry)
        XCTAssertEqual(ChannelOwnerHandover.afterNewKey(nil), .owned)
        XCTAssertEqual(ChannelOwnerHandover.afterNewKey(refused(401, "owner-proof-required")), .notOwner)
        XCTAssertEqual(ChannelOwnerHandover.afterNewKey(refused(500, "unknown")), .retry)
    }

    func testTheHandoverIsSignedByTheOldKeyAndNamesTheNewOne() async throws {
        let old = try Identity.fromSeed(seed: Data(repeating: 1, count: 64))
        let new = try Identity.fromSeed(seed: Data(repeating: 2, count: 64))
        var puts: [(signing: Data, naming: Data)] = []

        let outcome = await ChannelOwnerHandover.handOver(old: old, new: new) { signing, naming in
            puts.append((signing.signaturePublicKey(), naming.signaturePublicKey()))
        }

        XCTAssertEqual(outcome, .owned)
        XCTAssertEqual(puts.count, 1)
        XCTAssertEqual(puts[0].signing, old.signaturePublicKey())
        XCTAssertEqual(puts[0].naming, new.signaturePublicKey())
    }

    func testAHandoverThatAlreadyLandedIsConfirmedWithTheNewKey() async throws {
        let old = try Identity.fromSeed(seed: Data(repeating: 1, count: 64))
        let new = try Identity.fromSeed(seed: Data(repeating: 2, count: 64))
        var signers: [Data] = []

        let outcome = await ChannelOwnerHandover.handOver(old: old, new: new) { signing, _ in
            signers.append(signing.signaturePublicKey())
            if signing.signaturePublicKey() == old.signaturePublicKey() {
                throw self.refused(401, "owner-proof-required")
            }
        }

        XCTAssertEqual(outcome, .owned)
        XCTAssertEqual(signers, [old.signaturePublicKey(), new.signaturePublicKey()])
    }

    func testAChannelThisAccountDoesNotOwnIsLeftAlone() async throws {
        let old = try Identity.fromSeed(seed: Data(repeating: 1, count: 64))
        let new = try Identity.fromSeed(seed: Data(repeating: 2, count: 64))
        let outcome = await ChannelOwnerHandover.handOver(old: old, new: new) { _, _ in
            throw self.refused(401, "owner-proof-required")
        }
        XCTAssertEqual(outcome, .notOwner)
        XCTAssertTrue(ChannelOwnerHandover.settled([.owned, .notOwner]))
        XCTAssertFalse(ChannelOwnerHandover.settled([.owned, .retry]))
    }

    func testTheHandoverRequestCarriesTheNewOwnerKeyAndTheOldKeysProof() async throws {
        let channelIdHex = String(repeating: "ab", count: 16)
        let old = try Identity.fromSeed(seed: Data(repeating: 1, count: 64))
        let new = try Identity.fromSeed(seed: Data(repeating: 2, count: 64))
        var captured: URLRequest?
        let client = ChannelsClient(
            baseURL: "http://channels.test", roles: ChannelPublisherRoleCache(),
            transport: { request in
                captured = request
                return (
                    Data(),
                    HTTPURLResponse(url: request.url!, statusCode: 204, httpVersion: nil, headerFields: nil)!)
            })

        try await client.replaceOwner(channelIdHex: channelIdHex, signingIdentity: old, newOwnerIdentity: new)

        let request = try XCTUnwrap(captured)
        XCTAssertEqual(request.httpMethod, "PUT")
        XCTAssertEqual(request.url?.path, "/v1/channels/\(channelIdHex)/owner")
        let newOwnerKey = try ChannelsClient.ownerSigner(accountIdentity: new, channelIdHex: channelIdHex)
            .verifyingKey()
        XCTAssertEqual(request.httpBody, newOwnerKey, "the body is the new owner key, 32 raw bytes")
        XCTAssertEqual(request.httpBody?.count, 32)
        let proof = try XCTUnwrap(request.value(forHTTPHeaderField: channelOwnerProofHeaderName()))
        XCTAssertFalse(proof.isEmpty, "the owner proof of the OLD key")
    }
}

// MARK: - The notification extension's arm

final class NseKeyRotationArmTests: XCTestCase {

    private func isService(_ bytes: Data, isKeyRotation: @escaping (Data) -> Bool, len: Int) -> Bool {
        nseIsServiceEnvelope(
            bytes,
            decodesAsPushRoutingSecret: { _ in false },
            decodesAsCommitterBeacon: { _ in false },
            committerBeaconEnvelopeLen: 27,
            decodesAsProfile: { _ in false },
            profileEnvelopeLens: 29...332,
            decodesAsGroupName: { _ in false },
            groupNameEnvelopeLens: 30...285,
            decodesAsProfileRequest: { _ in false },
            profileRequestEnvelopeLen: 26,
            decodesAsPin: { _ in false },
            pinEnvelopeLen: 31,
            decodesAsRegistrarSeed: { _ in false },
            registrarSeedEnvelopeLen: 205,
            decodesAsScopedDiscovery: { _ in false },
            scopedDiscoveryEnvelopeLens: 1...2,
            decodesAsSharedChannels: { _ in false },
            sharedChannelsEnvelopeLens: 1...2,
            isKeyRotation: isKeyRotation,
            keyRotationEnvelopeLen: len)
    }

    func testAKeyRotationEnvelopeIsSuppressedAndNeverRendered() throws {
        let mnemonic = RecoveryMnemonic.generate()
        let now = Int64(Date().timeIntervalSince1970)
        let proof = try signSupersessionProof(
            oldEd25519Seed: try Identity.fromSeed(seed: Data(repeating: 1, count: 64)).ed25519Seed(),
            newEd25519Seed: try Identity.fromSeed(seed: mnemonic.toSeed()).ed25519Seed(),
            issuedAtUnixSecs: now, expiresAtUnixSecs: now + 60)
        let envelope = try composeKeyRotationEnvelope(
            preparedAtMs: 1, newEntropy: mnemonic.toEntropy(), supersessionProof: proof)
        let len = Int(keyRotationEnvelopeLen())
        XCTAssertEqual(envelope.count, len)
        XCTAssertEqual(len, 192)
        XCTAssertEqual(envelope.first, 0x6C)

        XCTAssertTrue(nseLooksLikeKeyRotation(envelope, envelopeLen: len))
        XCTAssertTrue(isService(envelope, isKeyRotation: { _ in true }, len: len))
        // The peek decides nothing: the verdict is the slim FFI's.
        XCTAssertFalse(isService(envelope, isKeyRotation: { _ in false }, len: len))
        // Another length is never asked.
        var asked = 0
        XCTAssertFalse(
            isService(envelope + Data([0]), isKeyRotation: { _ in asked += 1; return true }, len: len))
        XCTAssertEqual(asked, 0)
        // A chat message of the same length is not suppressed by the peek.
        XCTAssertFalse(nseLooksLikeKeyRotation(Data(repeating: 0x7B, count: len), envelopeLen: len))
        // A build that injected no length suppresses nothing by accident.
        XCTAssertFalse(nseLooksLikeKeyRotation(Data(), envelopeLen: 0))
    }
}
