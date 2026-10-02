// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

// ADR-0056 - a fresh device names its account by the login handle of the
// username typed, never by account id. The pairing payload carries the
// handle; the confirming device checks it against its own handle; nothing
// before a session looks the account up; the grant redeem carries the grant
// only and the account id comes from its response.

private func handle(_ name: String) -> Data {
    (try? loginHandle(username: name)) ?? Data()
}

private func randomBytes(_ count: Int) -> Data {
    Data((0..<count).map { _ in UInt8.random(in: 0...255) })
}

/// Records every discovery request made through `URLSession.shared` (or any
/// session using the default protocol classes) while registered. Only
/// discovery paths are kept: the host app may make unrelated requests.
private final class AnyRequestRecorder: URLProtocol {
    private static let lock = NSLock()
    private static var urls: [URL] = []

    static func reset() { lock.withLock { urls = [] } }
    static var recorded: [URL] { lock.withLock { urls } }

    override class func canInit(with request: URLRequest) -> Bool {
        if let url = request.url, url.absoluteString.contains("discovery") {
            lock.withLock { urls.append(url) }
        }
        return false
    }
}

// MARK: - The payload

final class PairingPayloadByHandleTests: XCTestCase {

    private func payload(for name: String) throws -> Data {
        try PairingCodec.buildPairingQrPayload(
            loginHandle: handle(name),
            keyPackage: try Identity.fromSeed(seed: randomBytes(32)).createKeyPackage(),
            rendezvousPseudonym: PairingCodec.generateRendezvousPseudonym()
        )
    }

    func testTheConfirmingDeviceOpensAPayloadNamingItsOwnHandle() throws {
        let rendezvous = PairingCodec.generateRendezvousPseudonym()
        let keyPackage = try Identity.fromSeed(seed: randomBytes(32)).createKeyPackage()
        let wire = try PairingCodec.buildPairingQrPayload(
            loginHandle: handle("alice"),
            keyPackage: keyPackage,
            rendezvousPseudonym: rendezvous
        )
        let decoded = try PairingCodec.parsePairingQrPayload(
            wireBytes: wire,
            expectedLoginHandle: handle("alice")
        )
        XCTAssertEqual(decoded.keyPackage, keyPackage)
        XCTAssertEqual(decoded.rendezvousPseudonym, rendezvous)
    }

    func testAPayloadNamingAnotherAccountIsRefusedWithTheWrongAccountCopy() throws {
        let wire = try payload(for: "bob")
        do {
            _ = try PairingCodec.parsePairingQrPayload(wireBytes: wire, expectedLoginHandle: handle("alice"))
            XCTFail("a payload for bob must not open on alice's device")
        } catch {
            XCTAssertTrue(PairingScanFailure.isWrongAccount(error), "\(error)")
            XCTAssertEqual(PairingScanFailure.userFacing(error), PairingScanFailure.wrongAccountCopy)
        }
    }

    func testAnUnsupportedVersionShowsTheUpdateCopyAndAnythingElseTheGenericCopy() {
        XCTAssertEqual(
            PairingScanFailure.userFacing(FfiError.Unsupported(message: "pairing QR version 1")),
            PairingScanFailure.unsupportedCopy)
        XCTAssertFalse(PairingScanFailure.isWrongAccount(FfiError.Group(message: "something else")))
        // ADR-0061: anything else renders the generic sentence, never the
        // error's own text.
        XCTAssertEqual(
            PairingScanFailure.userFacing(AuthClientError.argument("boom")),
            Localization.text("device_link.request.error.generic"))
    }

    func testTheScanFlowChecksThePayloadAgainstTheConfirmingDevicesOwnHandle() async throws {
        let wire = try payload(for: "bob")
        do {
            _ = try await PairingOrchestrator.runScanFlow(
                scannedBytes: wire,
                ownLoginHandle: handle("alice"),
                accountIdBytes: randomBytes(16),
                accountIdB64: "unused-\(UUID().uuidString)"
            )
            XCTFail("refused before any DeviceGroup work")
        } catch {
            XCTAssertTrue(PairingScanFailure.isWrongAccount(error), "\(error)")
        }
    }

    func testAFreshDevicesQrCarriesTheHandleOfTheTypedName() throws {
        let session = try PairingOrchestrator.prepareRestoreByNameFlow(
            loginHandle: handle("alice"),
            redeem: { _ in throw AuthClientError.argument("not reached") }
        )
        XCTAssertNoThrow(try PairingCodec.parsePairingQrPayload(
            wireBytes: session.qrPayload, expectedLoginHandle: handle("alice")))
        XCTAssertThrowsError(try PairingCodec.parsePairingQrPayload(
            wireBytes: session.qrPayload, expectedLoginHandle: handle("bob")))
    }

    func testOwnHandleIsTheCoresForANamedAccountAndNoneForAGuest() {
        XCTAssertEqual(ownLoginHandle(username: "alice"), handle("alice"))
        XCTAssertEqual(ownLoginHandle(username: " @Alice "), handle("alice"))
        XCTAssertNil(ownLoginHandle(username: ""))
        XCTAssertNil(ownLoginHandle(username: "  "))
        XCTAssertNil(ownLoginHandle(username: "a"), "a name that breaks the rule names nobody")
    }
}

// MARK: - The join by name

/// The fresh device joins before it knows its account: a plain group join,
/// then the DeviceGroup check once the redeem names the account.
final class JoinByNameTests: XCTestCase {

    func testTheDeferredCheckAdoptsTheAccountsDeviceGroupAndRefusesAnyOther() throws {
        let accountId = randomBytes(16)
        let founderIdentity = try Identity.fromSeed(seed: randomBytes(32))
        let founder = try DeviceGroup.createFounder(identity: founderIdentity, accountId: accountId)

        let joiner = try Identity.fromSeed(seed: randomBytes(32))
        let addition = try founder.addDeviceFromWire(
            identity: founderIdentity,
            keyPackage: try joiner.createKeyPackage()
        )

        let plain = try PairingOrchestrator.joinWelcomeByName(
            welcomeBytes: addition.welcome,
            identity: joiner
        )
        // The recovery row is sealed under the DeviceGroup exporter; the
        // plain group the fresh device holds derives the same key.
        let context = randomBytes(32)
        XCTAssertEqual(
            try plain.exportSecret(
                identity: joiner, label: RecoveryPayload.recoveryExporterLabel,
                context: context, length: 32),
            try founder.exportSecret(
                identity: founderIdentity, label: RecoveryPayload.recoveryExporterLabel,
                context: context, length: 32))

        XCTAssertThrowsError(
            try PairingOrchestrator.adoptJoinedDeviceGroup(identity: joiner, accountIdBytes: randomBytes(16))
        ) { error in
            XCTAssertEqual(error as? PairingOrchestrator.JoinByNameError, .wrongAccount)
        }
        let adopted = try PairingOrchestrator.adoptJoinedDeviceGroup(identity: joiner, accountIdBytes: accountId)
        XCTAssertEqual(try adopted.groupId(), try founder.groupId())
    }
}

// MARK: - The redeem

final class DeviceGrantRedeemTests: XCTestCase {

    func testTheRedeemSendsTheGrantOnlyAndTheAccountIdComesFromTheResponse() async throws {
        let accountId = randomBytes(16)
        let grant = base64UrlEncodeNoPad(randomBytes(32))
        let host = NameRouteStub.scope { _, _, _ in
            (200, """
            {"account_id_b64":"\(base64UrlEncodeNoPad(accountId))","session_token":"tok","session_expires_at_ms":42}
            """)
        }
        let result = try await NameRouteStub.client(host).deviceAuthorizeRedeem(grantTokenB64: grant)
        XCTAssertEqual(result.accountId, accountId)
        XCTAssertEqual(result.sessionToken, "tok")
        XCTAssertEqual(result.sessionExpiresAtMs, 42)

        let requests = NameRouteStub.requests(host)
        XCTAssertEqual(requests.count, 1)
        XCTAssertEqual(requests.first?.path, "/device/authorize/redeem")
        XCTAssertEqual(requests.first?.json.keys.sorted(), ["grant_token_b64"])
        XCTAssertEqual(requests.first?.json["grant_token_b64"] as? String, grant)
        XCTAssertNil(requests.first?.authorization, "anonymous")
    }

    func testAMalformedAccountIdInTheResponseIsRefused() async {
        let host = NameRouteStub.scope { _, _, _ in
            (200, #"{"account_id_b64":"AAAA","session_token":"tok","session_expires_at_ms":42}"#)
        }
        do {
            _ = try await NameRouteStub.client(host).deviceAuthorizeRedeem(grantTokenB64: "grant")
            XCTFail("a 3-byte account id is not an account id")
        } catch {}
    }
}

// MARK: - The restore screen

@MainActor
final class RestoreViaDeviceByNameTests: XCTestCase {

    private func viewModel(_ host: String) -> RecoverViewModel {
        let store = SessionStore(defaults: UserDefaults(suiteName: "pairing-by-handle-\(UUID().uuidString)")!)
        let repository = AuthRepository(
            sessionStore: store,
            authClientFactory: { NameRouteStub.client(host) },
            nowMs: { 1_000_000 }
        )
        return RecoverViewModel(repository: repository, cachedUsername: nil)
    }

    func testTheTypedNameBecomesTheHandleWithNoLookup() {
        let host = NameRouteStub.scope { _, _, _ in (500, "") }
        URLProtocol.registerClass(AnyRequestRecorder.self)
        defer { URLProtocol.unregisterClass(AnyRequestRecorder.self) }
        AnyRequestRecorder.reset()

        let model = viewModel(host)
        model.mode = .anotherDevice
        model.username = " @Alice "
        model.prepareDeviceRestore()

        XCTAssertNil(model.error)
        XCTAssertEqual(model.deviceRestoreTarget?.username, "alice")
        XCTAssertEqual(model.deviceRestoreTarget?.loginHandle, handle("alice"))
        XCTAssertTrue(NameRouteStub.requests(host).isEmpty, "no auth request")
        XCTAssertTrue(AnyRequestRecorder.recorded.isEmpty, "no discovery request: \(AnyRequestRecorder.recorded)")
    }

    func testANameThatBreaksTheRuleOrNoNameGetsItsCopy() {
        let host = NameRouteStub.scope { _, _, _ in (500, "") }
        let model = viewModel(host)
        model.mode = .anotherDevice

        model.username = "   "
        model.prepareDeviceRestore()
        XCTAssertEqual(model.error, "Enter the username of the account.")
        XCTAssertNil(model.deviceRestoreTarget)

        model.username = "a"
        model.prepareDeviceRestore()
        XCTAssertEqual(model.error, UsernameRuleViolation(reason: .tooShort).errorDescription)
        XCTAssertNil(model.deviceRestoreTarget)
    }

    func testARestoreWithoutARedeemedAccountIsRefused() async {
        let host = NameRouteStub.scope { _, _, _ in (500, "") }
        let model = viewModel(host)
        model.mode = .anotherDevice
        model.username = "alice"
        model.prepareDeviceRestore()
        do {
            try await model.completeFromDeviceRestore(outcome: .init(
                phraseEntropy: randomBytes(16), sessionToken: "tok", sessionExpiresAtMs: 42, accountId: nil))
            XCTFail("no account id from the redeem, no session")
        } catch {}
        XCTAssertTrue(NameRouteStub.requests(host).isEmpty)
    }
}

// MARK: - Security pass: nothing lands before both checks

/// Discovery for the restore gates, scripted. Records the order of the reads.
private final class ScriptedRestoreDirectory: DeviceRestoreDirectory {
    var publishedKey: Data?
    var lookup: Result<Data?, Error> = .success(nil)
    private(set) var calls: [String] = []
    private(set) var lookupSessionToken: String?

    func fetchIdentityBundle(accountId: Data, sessionToken: String?) async throws -> IdentityFetchResponse? {
        calls.append("identity")
        guard let publishedKey else { return nil }
        return IdentityFetchResponse(signaturePubkey: base64Encode(publishedKey), x25519Pubkey: "")
    }

    func lookupAccount(username: String, accountId: Data, sessionToken: String) async throws -> Data? {
        calls.append("lookup:\(username)")
        lookupSessionToken = sessionToken
        return try lookup.get()
    }
}

private final class NoAccountDirectory: AccountDirectory {
    func registerUsername(accountId: Data, username: String, sessionToken: String) async throws -> RegisterResult {
        throw AuthClientError.argument("not reached")
    }
    func publishIdentityBundle(
        accountId: Data, signaturePubkey: Data, x25519Pubkey: Data,
        leafAuth: Data?, genesisAttest: Data?, sessionToken: String
    ) async throws {}
}

private final class MemorySessionPersistence: SessionPersistence {
    private(set) var tokens: [String: PersistedSessionToken] = [:]
    func hasDeviceKey(accountIdB64: String) -> Bool { false }
    func accountIdentitySeed(accountIdB64: String) -> Data? { nil }
    func exportKey(accountIdB64: String) -> Data? { nil }
    func sessionToken(accountIdB64: String) -> PersistedSessionToken? { tokens[accountIdB64] }
    func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws { tokens[accountIdB64] = token }
    func deleteSessionToken(accountIdB64: String) { tokens[accountIdB64] = nil }
}

/// ADR-0056 security pass: a join-by-name restore verifies the transferred
/// seed against the published identity, then checks at discovery that the
/// typed name resolves to the redeemed account, and only then writes
/// anything - the pairing's held state (`persistJoin`) and its own.
@MainActor
final class DeviceRestoreSecurityPassTests: XCTestCase {

    private var accountIdsToClean: [String] = []

    override func tearDown() async throws {
        for b64 in accountIdsToClean {
            DeviceKeyStore.deleteDeviceKey(accountIdB64: b64)
            AccountIdentitySeedStore.deleteAccountIdentitySeed(accountIdB64: b64)
            DeviceIdentityStore.deleteDeviceIdentitySeed(accountIdB64: b64)
            DeviceGroupSnapshotStore().deleteSnapshot(accountIdB64: b64)
            try? PhraseStore().deletePhrase(accountIdB64: b64)
        }
        accountIdsToClean = []
        try await super.tearDown()
    }

    private struct Harness {
        let model: RecoverViewModel
        let repository: AuthRepository
        let store: SessionStore
        let persistence: MemorySessionPersistence
        let directory: ScriptedRestoreDirectory
    }

    private func harness() -> Harness {
        let host = NameRouteStub.scope { _, _, _ in (503, "") }
        let store = SessionStore(defaults: UserDefaults(suiteName: "restore-pass-\(UUID().uuidString)")!)
        let persistence = MemorySessionPersistence()
        let directory = ScriptedRestoreDirectory()
        let repository = AuthRepository(
            sessionStore: store,
            authClientFactory: { NameRouteStub.client(host) },
            persistence: persistence,
            nowMs: { 1_000_000 },
            accountDirectory: NoAccountDirectory(),
            loginHandleBinds: LoginHandleBindLedger(),
            restoreDirectory: directory
        )
        let model = RecoverViewModel(repository: repository, cachedUsername: nil)
        model.mode = .anotherDevice
        model.username = "alice"
        model.prepareDeviceRestore()
        return Harness(model: model, repository: repository, store: store, persistence: persistence, directory: directory)
    }

    /// A redeemed outcome whose `persistJoin` records that it ran.
    private func outcome(entropy: Data, accountId: Data, ran: @escaping () -> Void) -> PairingOrchestrator.JoinRecoveryOutcome {
        accountIdsToClean.append(base64Encode(accountId))
        return PairingOrchestrator.JoinRecoveryOutcome(
            phraseEntropy: entropy,
            sessionToken: "redeemed-token",
            sessionExpiresAtMs: 9_000_000,
            accountId: accountId,
            persistJoin: ran
        )
    }

    /// The identity key the phrase of `entropy` derives (recovery payload
    /// v2: the entropy is what travels; the seed is rebuilt from it).
    private func publishedKey(of entropy: Data) throws -> Data {
        try Identity.fromSeed(seed: seed(of: entropy)).signaturePublicKey()
    }

    private func seed(of entropy: Data) throws -> Data {
        try RecoveryMnemonic.fromEntropy(entropy: entropy).toSeed()
    }

    private func assertNothingLanded(_ h: Harness, accountId: Data, file: StaticString = #filePath, line: UInt = #line) {
        let b64 = base64Encode(accountId)
        XCTAssertNil(h.store.load(), "no account marker", file: file, line: line)
        XCTAssertNil(h.repository.identitySecrets, "no session installed", file: file, line: line)
        XCTAssertTrue(h.persistence.tokens.isEmpty, "no session token persisted", file: file, line: line)
        XCTAssertNil(try? DeviceKeyStore.getDeviceKey(accountIdB64: b64), file: file, line: line)
        XCTAssertNil(try? AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: b64), file: file, line: line)
        if case .session = h.repository.state { XCTFail("landed a session", file: file, line: line) }
    }

    func testASeedThatFailsVerificationPersistsNothingAndAsksDiscoveryNothingMore() async throws {
        let h = harness()
        let entropy = randomBytes(16)
        let accountId = randomBytes(16)
        h.directory.publishedKey = try publishedKey(of: randomBytes(16))
        var ran = false
        let o = outcome(entropy: entropy, accountId: accountId) { ran = true }
        do {
            try await h.model.completeFromDeviceRestore(outcome: o)
            XCTFail("a seed that does not reproduce the published key must be refused")
        } catch {
            XCTAssertTrue(error is PhraseMismatch, "\(error)")
        }
        XCTAssertFalse(ran, "the pairing's held state was not written")
        XCTAssertEqual(h.directory.calls, ["identity"], "the name check never ran")
        XCTAssertTrue(o.phraseEntropy.allSatisfy { $0 == 0 }, "the received entropy is zeroed")
        assertNothingLanded(h, accountId: accountId)
    }

    func testATypedNameThatResolvesToAnotherAccountIsRefusedWithTheCopyAndNothingLands() async throws {
        let h = harness()
        let entropy = randomBytes(16)
        let accountId = randomBytes(16)
        h.directory.publishedKey = try publishedKey(of: entropy)
        h.directory.lookup = .success(randomBytes(16))
        var ran = false
        let o = outcome(entropy: entropy, accountId: accountId) { ran = true }
        do {
            try await h.model.completeFromDeviceRestore(outcome: o)
            XCTFail("the typed name belongs to another account")
        } catch {
            XCTAssertEqual(error as? TypedNameCheckRefusal, .otherAccount)
            XCTAssertEqual(
                (error as? LocalizedError)?.errorDescription,
                "The name you typed belongs to a different account. Nothing was changed on this device.")
        }
        XCTAssertEqual(h.directory.calls, ["identity", "lookup:alice"])
        XCTAssertEqual(h.directory.lookupSessionToken, "redeemed-token", "the lookup draws with the new session")
        XCTAssertFalse(ran)
        XCTAssertTrue(o.phraseEntropy.allSatisfy { $0 == 0 })
        assertNothingLanded(h, accountId: accountId)
    }

    func testANameDiscoveryDoesNotKnowIsRefusedTheSameWay() async throws {
        let h = harness()
        let entropy = randomBytes(16)
        let accountId = randomBytes(16)
        h.directory.publishedKey = try publishedKey(of: entropy)
        h.directory.lookup = .success(nil)
        var ran = false
        let o = outcome(entropy: entropy, accountId: accountId) { ran = true }
        do {
            try await h.model.completeFromDeviceRestore(outcome: o)
            XCTFail("an unknown name confirms nothing")
        } catch {
            XCTAssertEqual(error as? TypedNameCheckRefusal, .otherAccount)
        }
        XCTAssertFalse(ran)
        assertNothingLanded(h, accountId: accountId)
    }

    func testALookupThatCannotRunFailsClosed() async throws {
        for failure: Error in [
            AnonTokenUnavailable.disabled,
            AnonTokenUnavailable.quotaExhausted(retryAfterSecs: 60),
            URLError(.notConnectedToInternet),
        ] {
            let h = harness()
            let entropy = randomBytes(16)
            let accountId = randomBytes(16)
            h.directory.publishedKey = try publishedKey(of: entropy)
            h.directory.lookup = .failure(failure)
            var ran = false
            let o = outcome(entropy: entropy, accountId: accountId) { ran = true }
            do {
                try await h.model.completeFromDeviceRestore(outcome: o)
                XCTFail("a lookup that could not run must refuse: \(failure)")
            } catch {
                XCTAssertEqual(error as? TypedNameCheckRefusal, .unavailable, "\(failure)")
                XCTAssertEqual((error as? LocalizedError)?.errorDescription, TypedNameCheck.unavailableCopy)
            }
            XCTAssertFalse(ran)
            XCTAssertTrue(o.phraseEntropy.allSatisfy { $0 == 0 })
            assertNothingLanded(h, accountId: accountId)
        }
    }

    func testTheHappyPathPersistsOnlyAfterBothChecksThenLands() async throws {
        let h = harness()
        let entropy = randomBytes(16)
        let accountId = randomBytes(16)
        h.directory.publishedKey = try publishedKey(of: entropy)
        h.directory.lookup = .success(accountId)
        var callsWhenPersisted: [String] = []
        var landedWhenPersisted = true
        let o = outcome(entropy: entropy, accountId: accountId) { [directory = h.directory, store = h.store] in
            callsWhenPersisted = directory.calls
            landedWhenPersisted = store.load() != nil
        }
        try await h.model.completeFromDeviceRestore(outcome: o)

        XCTAssertEqual(callsWhenPersisted, ["identity", "lookup:alice"], "the held state lands after both checks")
        XCTAssertFalse(landedWhenPersisted, "and before the session lands")
        let b64 = base64Encode(accountId)
        XCTAssertEqual(h.store.load()?.accountIdB64, b64)
        XCTAssertEqual(h.store.load()?.username, "alice")
        XCTAssertEqual(h.repository.identitySecrets?.accountId, accountId)
        XCTAssertEqual(h.persistence.tokens[b64]?.sessionToken, "redeemed-token")
        XCTAssertEqual(
            try AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: b64), try seed(of: entropy))
        // A device restored this way holds the phrase like the one that
        // confirmed it.
        XCTAssertEqual(
            DevicePhrase.read(accountIdB64: b64),
            try RecoveryMnemonic.fromEntropy(entropy: entropy).phrase())
        XCTAssertTrue(o.phraseEntropy.allSatisfy { $0 == 0 }, "the received entropy is zeroed")
        XCTAssertTrue(h.repository.newPasswordOwed, "reached through Forgot password: the new-password step follows")
    }
}

/// ADR-0056 security pass: the join by name holds what it received and
/// writes it only when told to.
final class PendingJoinByNameTests: XCTestCase {

    func testNothingIsWrittenUntilPersistAndPersistWritesTheSeedAndTheSnapshot() throws {
        let accountId = randomBytes(16)
        let b64 = base64Encode(accountId)
        defer {
            DeviceIdentityStore.deleteDeviceIdentitySeed(accountIdB64: b64)
            DeviceGroupSnapshotStore().deleteSnapshot(accountIdB64: b64)
        }
        let founderIdentity = try Identity.fromSeed(seed: randomBytes(32))
        let founder = try DeviceGroup.createFounder(identity: founderIdentity, accountId: accountId)
        let deviceSeed = randomBytes(32)
        let joiner = try Identity.fromSeed(seed: deviceSeed)
        let addition = try founder.addDeviceFromWire(
            identity: founderIdentity, keyPackage: try joiner.createKeyPackage())
        _ = try PairingOrchestrator.joinWelcomeByName(welcomeBytes: addition.welcome, identity: joiner)
        let joined = try PairingOrchestrator.adoptJoinedDeviceGroup(identity: joiner, accountIdBytes: accountId)

        let pending = PairingOrchestrator.PendingJoinByName(
            identity: joiner, joined: joined, accountIdBytes: accountId, deviceSeed: deviceSeed)
        XCTAssertNil(try DeviceIdentityStore.getDeviceIdentitySeed(accountIdB64: b64), "held, not written")
        XCTAssertNil(DeviceGroupSnapshotStore().getSnapshot(accountIdB64: b64), "held, not written")

        pending.persist()
        XCTAssertEqual(try DeviceIdentityStore.getDeviceIdentitySeed(accountIdB64: b64), deviceSeed)
        XCTAssertNotNil(DeviceGroupSnapshotStore().getSnapshot(accountIdB64: b64))
        XCTAssertTrue(pending.persisted)
    }
}

/// ADR-0056 security pass: zeroing reaches the bytes that were received,
/// not a copy-on-write copy of them.
final class JoinRecoveryOutcomeWipeTests: XCTestCase {

    func testWipeZeroesTheEntropyInPlace() {
        let o = PairingOrchestrator.JoinRecoveryOutcome(
            phraseEntropy: Data(repeating: 0xAB, count: 16),
            sessionToken: nil,
            sessionExpiresAtMs: nil
        )
        let before = o.phraseEntropy.withUnsafeBytes { UInt(bitPattern: $0.baseAddress) }
        o.wipeEntropy()
        let after = o.phraseEntropy.withUnsafeBytes { UInt(bitPattern: $0.baseAddress) }
        XCTAssertEqual(o.phraseEntropy.count, 16)
        XCTAssertTrue(o.phraseEntropy.allSatisfy { $0 == 0 })
        XCTAssertEqual(before, after, "the same buffer, not a fresh copy")
    }
}
