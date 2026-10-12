// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// ADR-0066 sprint T2 — removing a device really removes it: what a landing
/// sends, the session-names-a-device rule, the recorded account key, Log out
/// at the server, the removal state machine with its owed record, the
/// device-revoked verdict (against logout and a superseded generation), and
/// the DeviceGroup filter on own-account admission.
@MainActor
final class DeviceRevocationTests: XCTestCase {

    private let deviceA = Data(repeating: 0xA1, count: 32)
    private let deviceB = Data(repeating: 0xB2, count: 32)
    private var suites: [String] = []

    override func tearDown() async throws {
        for suite in suites { UserDefaults().removePersistentDomain(forName: suite) }
        try await super.tearDown()
    }

    private func defaults(_ name: String) -> UserDefaults {
        let suite = "\(name)-\(UUID().uuidString)"
        suites.append(suite)
        return UserDefaults(suiteName: suite)!
    }

    // MARK: - What a landing sends

    func testADeviceIdIsUsableOnlyWhenItIs32NonZeroBytes() {
        XCTAssertEqual(LandingDevice.usable(deviceA), deviceA)
        XCTAssertNil(LandingDevice.usable(nil))
        XCTAssertNil(LandingDevice.usable(Data(repeating: 0, count: 32)), "the all-zero id is the server's no-device")
        XCTAssertNil(LandingDevice.usable(Data(repeating: 1, count: 31)))
        XCTAssertNil(LandingDevice.usable(Data(repeating: 1, count: 33)))
    }

    func testALandingAssertsTheDeviceOnlyWhenTheAccountIsKnownBeforeTheFinish() {
        XCTAssertEqual(LandingDevice.asserted(accountKnownBeforeFinish: true, ownDeviceId: deviceA), deviceA)
        XCTAssertNil(LandingDevice.asserted(accountKnownBeforeFinish: false, ownDeviceId: deviceA))
        XCTAssertNil(LandingDevice.asserted(accountKnownBeforeFinish: true, ownDeviceId: nil))
        XCTAssertNil(LandingDevice.asserted(
            accountKnownBeforeFinish: true, ownDeviceId: Data(repeating: 0, count: 32)))
    }

    func testTheFinishBodiesCarryTheDeviceIdOnlyWhenGiven() throws {
        let with = try JSONSerialization.jsonObject(with: JSONEncoder().encode(SignupFinishRequest(
            accountId: "a", passwordUpload: "p", deviceIdB64: "ZGV2"))) as? [String: Any]
        XCTAssertEqual(with?["device_id_b64"] as? String, "ZGV2")
        let without = try JSONSerialization.jsonObject(with: JSONEncoder().encode(SignupFinishRequest(
            accountId: "a", passwordUpload: "p"))) as? [String: Any]
        XCTAssertNil(without?["device_id_b64"], "absent, never null: an unbound session")
        let login = try JSONSerialization.jsonObject(with: JSONEncoder().encode(LoginFinishRequest(
            sessionId: "s", finalize: "f", deviceIdB64: "ZGV2"))) as? [String: Any]
        XCTAssertEqual(login?["device_id_b64"] as? String, "ZGV2")
        let bare = try JSONSerialization.jsonObject(with: JSONEncoder().encode(LoginFinishRequest(
            sessionId: "s", finalize: "f"))) as? [String: Any]
        XCTAssertNil(bare?["device_id_b64"])
    }

    func testTheStoredPairKeepsItsDeviceNoteAndAnOldRowReadsAsUnbound() throws {
        let pair = SessionPair.fixture().naming(deviceId: deviceA)
        let data = try JSONEncoder().encode(PersistedSessionToken(pair: pair))
        let back = try JSONDecoder().decode(PersistedSessionToken.self, from: data)
        XCTAssertEqual(back.pair.deviceIdB64, deviceA.base64EncodedString())
        XCTAssertEqual(back.pair, pair)
        let old = try JSONEncoder().encode(PersistedSessionToken(pair: .fixture()))
        XCTAssertNil(try JSONDecoder().decode(PersistedSessionToken.self, from: old).pair.deviceIdB64)
    }

    // MARK: - Is the session bound

    func testTheBindingDecision() {
        let unbound = SessionPair.fixture()
        XCTAssertEqual(SessionDeviceBinding.decide(pair: unbound, ownDeviceId: deviceA), .bind(deviceA))
        XCTAssertEqual(
            SessionDeviceBinding.decide(pair: unbound.naming(deviceId: deviceA), ownDeviceId: deviceA), .bound)
        XCTAssertEqual(
            SessionDeviceBinding.decide(pair: unbound.naming(deviceId: deviceB), ownDeviceId: deviceA),
            .boundToAnother)
        XCTAssertEqual(SessionDeviceBinding.decide(pair: unbound, ownDeviceId: nil), .noDeviceIdentity)
        XCTAssertEqual(
            SessionDeviceBinding.decide(pair: unbound, ownDeviceId: Data(repeating: 0, count: 32)),
            .noDeviceIdentity)
    }

    func testTheBindRefusalsAreToldApart() {
        typealias B = SessionDeviceBinding
        XCTAssertEqual(B.classify(status: 401, body: #"{"error":"device-revoked"}"#), .deviceRevoked)
        XCTAssertEqual(B.classify(status: 401, body: #"{"error":"key-generation-superseded"}"#), .superseded)
        XCTAssertEqual(B.classify(status: 401, body: #"{"error":"refresh-invalid"}"#), .refreshInvalid)
        XCTAssertEqual(B.classify(status: 409, body: #"{"error":"device-already-bound"}"#), .alreadyBound)
        XCTAssertEqual(B.classify(status: 400, body: ""), .badDeviceId)
        XCTAssertEqual(B.classify(status: 401, body: ""), .other, "a plain 401 is not a removal")
        XCTAssertEqual(B.classify(status: 503, body: ""), .other)
        XCTAssertEqual(B.refusal(of: URLError(.notConnectedToInternet)), .other)
    }

    // MARK: - The recorded account key

    func testTheGenesisMintRunsUntilItSucceededForTheKeyThisDeviceHolds() {
        let key = Data(repeating: 7, count: 32)
        let other = Data(repeating: 8, count: 32)
        XCTAssertTrue(IdentityKeyRecord.needsMint(recordedPubkeyB64: nil, currentPubkey: key))
        XCTAssertFalse(IdentityKeyRecord.needsMint(
            recordedPubkeyB64: key.base64EncodedString(), currentPubkey: key))
        XCTAssertTrue(
            IdentityKeyRecord.needsMint(recordedPubkeyB64: key.base64EncodedString(), currentPubkey: other),
            "after a key reset the key changed: mint again")
    }

    func testTheGenesisRefusals() {
        XCTAssertEqual(
            IdentityKeyRecord.classify(status: 409, body: #"{"error":"identity-key-mismatch"}"#),
            .identityKeyMismatch)
        XCTAssertEqual(IdentityKeyRecord.classify(status: 400, body: ""), .unusableKey)
        XCTAssertEqual(IdentityKeyRecord.classify(status: 401, body: ""), .retryLater)
        XCTAssertEqual(IdentityKeyRecord.classify(status: 503, body: ""), .retryLater)
    }

    func testTheRecordedMarkIsPerAccountAndCleared() {
        let marks = IdentityKeyRecordMarks(defaults: defaults("key-marks"))
        let key = Data(repeating: 7, count: 32)
        XCTAssertNil(marks.recorded(accountIdB64: "a"))
        marks.markRecorded(pubkey: key, accountIdB64: "a")
        XCTAssertEqual(marks.recorded(accountIdB64: "a"), key.base64EncodedString())
        XCTAssertNil(marks.recorded(accountIdB64: "b"))
        marks.clear(accountIdB64: "a")
        XCTAssertNil(marks.recorded(accountIdB64: "a"))
    }

    // MARK: - Log out at the server

    func testOnlyAnUnreachableFirstAttemptLeavesTheLogoutOwed() {
        XCTAssertEqual(LogoutOwed.attempt(of: nil), .done)
        XCTAssertEqual(
            LogoutOwed.attempt(of: AuthClientError.http(status: 401, endpoint: "/auth/session/logout", body: "")),
            .refused)
        XCTAssertEqual(LogoutOwed.attempt(of: URLError(.notConnectedToInternet)), .unreachable)
        XCTAssertFalse(LogoutOwed.keepsRecord(after: .done, isLaunchRetry: false))
        XCTAssertFalse(LogoutOwed.keepsRecord(after: .refused, isLaunchRetry: false))
        XCTAssertTrue(LogoutOwed.keepsRecord(after: .unreachable, isLaunchRetry: false))
        XCTAssertFalse(LogoutOwed.keepsRecord(after: .unreachable, isLaunchRetry: true), "one retry, no more")
    }

    func testTheLaunchRetryUsesTheAccessTokenWhileItLivesThenTheRefresh() {
        let pair = SessionPair.fixture(
            access: "acc", accessExpiresAtMs: 1_000, refresh: "ref", refreshExpiresAtMs: 5_000)
        XCTAssertEqual(LogoutOwed.retryPlan(pair: pair, nowMs: 500), .logout(sessionToken: "acc"))
        XCTAssertEqual(LogoutOwed.retryPlan(pair: pair, nowMs: 2_000), .refreshThenLogout(refreshToken: "ref"))
        XCTAssertEqual(LogoutOwed.retryPlan(pair: pair, nowMs: 6_000), .drop)
    }

    // MARK: - device-revoked against logout and a superseded generation

    func testA401IsReadByItsBody() {
        typealias R = SessionTokenRule
        XCTAssertEqual(R.classify401(body: Data(#"{"error":"device-revoked"}"#.utf8)), .deviceRevoked)
        XCTAssertEqual(
            R.classify401(body: Data(#"{"error":"key-generation-superseded"}"#.utf8)), .keyGenerationSuperseded)
        XCTAssertEqual(R.classify401(body: Data(#"{"error":"refresh-invalid"}"#.utf8)), .refreshInvalid)
        XCTAssertEqual(R.classify401(body: Data()), .other, "a logged-out session: a plain 401, never a wipe")
    }

    func testARevokedDeviceIsNeverRetriedAndALogoutIsNeverAWipe() {
        typealias R = SessionTokenRule
        XCTAssertEqual(R.afterBearer401(meaning: .deviceRevoked, alreadyRetried: false), .signOutRevoked)
        XCTAssertEqual(R.afterBearer401(meaning: .deviceRevoked, alreadyRetried: true), .signOutRevoked)
        XCTAssertEqual(R.afterBearer401(meaning: .keyGenerationSuperseded, alreadyRetried: false), .signOutSuperseded)
        XCTAssertEqual(R.afterBearer401(meaning: .other, alreadyRetried: false), .refreshAndRetry)
        XCTAssertEqual(R.afterBearer401(meaning: .refreshInvalid, alreadyRetried: true), .report)
    }

    func testOnlyAuthsAnswerIsProofOfARemoval() {
        typealias R = SessionTokenRule
        let auth = "https://lakko.test/native/auth"
        XCTAssertTrue(R.revocationIsProof(
            requestURL: URL(string: "https://lakko.test/native/auth/totp/enroll/start"), authBaseURL: auth))
        XCTAssertFalse(R.revocationIsProof(
            requestURL: URL(string: "https://lakko.test/native/discovery/keypackage"), authBaseURL: auth))
        XCTAssertFalse(R.revocationIsProof(
            requestURL: URL(string: "https://lakko.test/native/authority/x"), authBaseURL: auth))
        XCTAssertFalse(R.revocationIsProof(requestURL: nil, authBaseURL: auth))
        XCTAssertFalse(R.revocationIsProof(requestURL: URL(string: "https://x/y"), authBaseURL: ""))
    }

    func testARefreshAnsweredDeviceRevokedMarksTheSessionOnceAndIsNotASupersession() async {
        let provider = SessionTokenProvider()
        provider.nowMs = { 1_000_000 }
        let fired = Counter()
        provider.onDeviceRevoked = { _ in fired.bump("revoked") }
        provider.onSuperseded = { _ in fired.bump("superseded") }
        provider.onRefreshInvalid = { _ in fired.bump("invalid") }
        provider.refresher = { _ in
            throw AuthClientError.http(
                status: 401, endpoint: "/auth/session/refresh", body: #"{"error":"device-revoked"}"#)
        }
        provider.install(.fixture(), accountIdB64: "acct")
        let first = await provider.refresh()
        let second = await provider.refresh()
        XCTAssertEqual(first, .revoked)
        XCTAssertEqual(second, .revoked)
        XCTAssertTrue(provider.isDeviceRevoked)
        XCTAssertFalse(provider.isSuperseded)
        XCTAssertEqual(fired.names, ["revoked"], "once, and nothing else")
        let access = await provider.accessToken()
        XCTAssertEqual(access, .revoked, "nothing more is sent under a revoked session")
    }

    func testARefreshInvalidAfterALogoutIsNotARemoval() async {
        let provider = SessionTokenProvider()
        provider.nowMs = { 1_000_000 }
        let fired = Counter()
        provider.onDeviceRevoked = { _ in fired.bump("revoked") }
        provider.onRefreshInvalid = { _ in fired.bump("invalid") }
        provider.refresher = { _ in
            throw AuthClientError.http(
                status: 401, endpoint: "/auth/session/refresh", body: #"{"error":"refresh-invalid"}"#)
        }
        provider.install(.fixture(), accountIdB64: "acct")
        let outcome = await provider.refresh()
        XCTAssertEqual(outcome, .invalid)
        XCTAssertFalse(provider.isDeviceRevoked)
        XCTAssertEqual(fired.names, ["invalid"])
    }

    func testARefreshKeepsTheDeviceTheSessionNames() async {
        let provider = SessionTokenProvider()
        provider.nowMs = { 1_000_000 }
        provider.refresher = { _ in .fixture(access: "tok-new") }
        provider.install(SessionPair.fixture().naming(deviceId: deviceA), accountIdB64: "acct")
        let outcome = await provider.refresh()
        XCTAssertEqual(outcome, .refreshed(SessionPair.fixture(access: "tok-new").naming(deviceId: deviceA)))
        XCTAssertEqual(provider.current?.deviceIdB64, deviceA.base64EncodedString())
    }

    func testDeviceRevokedFromAnotherServiceAsksAuthInsteadOfWiping() async throws {
        let provider = SessionTokenProvider()
        provider.nowMs = { 1_000_000 }
        provider.authBaseURL = { "https://lakko.test/auth" }
        let fired = Counter()
        provider.onDeviceRevoked = { _ in fired.bump("revoked") }
        // Auth says the session is fine.
        provider.refresher = { _ in fired.bump("asked-auth"); return .fixture(access: "tok-new") }
        provider.install(.fixture(access: "tok"), accountIdB64: "acct")
        var request = URLRequest(url: URL(string: "https://lakko.test/storage/backup")!)
        request.setValue("Bearer tok", forHTTPHeaderField: "Authorization")
        _ = try await provider.perform(request) { sent in
            (Data(#"{"error":"device-revoked"}"#.utf8),
             HTTPURLResponse(url: sent.url!, statusCode: 401, httpVersion: nil, headerFields: nil)!)
        }
        XCTAssertEqual(fired.names, ["asked-auth"])
        XCTAssertFalse(provider.isDeviceRevoked)

        // The same body from auth itself is proof.
        var authRequest = URLRequest(url: URL(string: "https://lakko.test/auth/totp/enroll/start")!)
        authRequest.setValue("Bearer tok-new", forHTTPHeaderField: "Authorization")
        _ = try await provider.perform(authRequest) { sent in
            (Data(#"{"error":"device-revoked"}"#.utf8),
             HTTPURLResponse(url: sent.url!, statusCode: 401, httpVersion: nil, headerFields: nil)!)
        }
        XCTAssertTrue(provider.isDeviceRevoked)
        XCTAssertEqual(fired.names, ["asked-auth", "revoked"])
    }

    func testAKeyPackagePublishRefusalIsReadByItsCode() {
        typealias K = KeyPackagePublishRefusal
        XCTAssertEqual(K.classify(status: 403, body: #"{"error":"device-mismatch"}"#), .deviceMismatch)
        XCTAssertEqual(K.classify(status: 403, body: #"{"error":"device-revoked"}"#), .deviceRevoked)
        XCTAssertEqual(K.classify(status: 403, body: ""), .other)
        XCTAssertEqual(K.classify(status: 401, body: #"{"error":"device-revoked"}"#), .other)
    }

    // MARK: - The removal state machine

    func testTheOwedStepsRunInOrderAndAreMarkedOneByOne() {
        var record = DeviceRemovalRecord(deviceIdB64: "d", revokedAtMs: 1)
        XCTAssertEqual(DeviceRemovalPlan.remaining(record), [.directory, .deviceGroup, .chats])
        XCTAssertFalse(DeviceRemovalPlan.isComplete(record))
        record = DeviceRemovalPlan.marking(record, done: .deviceGroup)
        XCTAssertEqual(DeviceRemovalPlan.remaining(record), [.directory, .chats])
        record = DeviceRemovalPlan.marking(record, done: .directory)
        record = DeviceRemovalPlan.marking(record, done: .chats)
        XCTAssertTrue(DeviceRemovalPlan.isComplete(record))
        XCTAssertEqual(DeviceRemovalPlan.result(after: record), .removed)
        XCTAssertEqual(
            DeviceRemovalPlan.result(after: DeviceRemovalRecord(deviceIdB64: "d", revokedAtMs: 1)),
            .removedPending)
    }

    func testARemovalDoesNotStartWithoutKeysOnTheOwnDeviceOrWhileAPublishIsOwed() {
        typealias P = DeviceRemovalPlan
        XCTAssertEqual(P.canStart(publishOwed: false, holdsAccountKeys: true, targetIsOwnDevice: false), .proceed)
        XCTAssertEqual(
            P.canStart(publishOwed: true, holdsAccountKeys: true, targetIsOwnDevice: false), .blockedPublishOwed)
        XCTAssertEqual(
            P.canStart(publishOwed: false, holdsAccountKeys: false, targetIsOwnDevice: false), .noAccountKeys)
        XCTAssertEqual(P.canStart(publishOwed: false, holdsAccountKeys: true, targetIsOwnDevice: true), .ownDevice)
    }

    func testTheRevokeAnswers() {
        typealias P = DeviceRemovalPlan
        XCTAssertEqual(P.revokeOutcome(of: nil), .revoked)
        XCTAssertEqual(P.revokeOutcome(status: 403, body: #"{"error":"fresh-auth-required"}"#), .freshAuthRequired)
        XCTAssertEqual(P.revokeOutcome(status: 403, body: #"{"error":"account-key-required"}"#), .accountKeyRequired)
        XCTAssertEqual(P.revokeOutcome(status: 429, body: #"{"error":"too-many-attempts"}"#), .rateLimited)
        XCTAssertEqual(P.revokeOutcome(status: 401, body: #"{"error":"device-revoked"}"#), .sessionEnded)
        XCTAssertEqual(P.revokeOutcome(status: 400, body: ""), .failed)
        XCTAssertEqual(P.revokeOutcome(status: 403, body: ""), .failed)
        XCTAssertEqual(
            P.revokeOutcome(of: AuthClientError.network(
                endpoint: "/account/devices/revoke", underlying: URLError(.notConnectedToInternet))),
            .offline)
    }

    func testTheDirectoryAnswers() {
        typealias P = DeviceRemovalPlan
        XCTAssertEqual(P.directoryOutcome(status: 204), .done)
        XCTAssertEqual(P.directoryOutcome(status: 404), .done, "the row is gone, which is the goal")
        XCTAssertEqual(P.directoryOutcome(status: 403), .retry, "account-key-required: owed, not done")
        XCTAssertEqual(P.directoryOutcome(status: 401), .sessionEnded)
        XCTAssertEqual(P.directoryOutcome(status: 503), .retry)
        XCTAssertEqual(P.directoryOutcome(of: URLError(.timedOut)), .retry)
    }

    func testTheOwedStoreKeepsWhatIsOwedAndDropsWhatIsDone() {
        let store = DeviceRemovalStore(defaults: defaults("removal-store"))
        var first = DeviceRemovalRecord(deviceIdB64: "one", revokedAtMs: 1)
        store.put(first, accountIdB64: "a")
        store.put(DeviceRemovalRecord(deviceIdB64: "two", revokedAtMs: 2), accountIdB64: "a")
        XCTAssertEqual(store.load(accountIdB64: "a").map(\.deviceIdB64).sorted(), ["one", "two"])
        XCTAssertTrue(store.load(accountIdB64: "b").isEmpty)
        first = DeviceRemovalPlan.marking(first, done: .directory)
        store.put(first, accountIdB64: "a")
        XCTAssertEqual(store.load(accountIdB64: "a").first { $0.deviceIdB64 == "one" }?.directoryDone, true)
        for step in DeviceRemovalPlan.Step.allCases { first = DeviceRemovalPlan.marking(first, done: step) }
        store.put(first, accountIdB64: "a")
        XCTAssertEqual(store.load(accountIdB64: "a").map(\.deviceIdB64), ["two"])
        store.clear(accountIdB64: "a")
        XCTAssertTrue(store.load(accountIdB64: "a").isEmpty)
    }

    // MARK: - The removal flow

    /// A scripted world for ``DeviceRemovalRunner``.
    private final class World {
        var calls: [String] = []
        var statements: [Data] = []
        var signed = 0
        var grant: String? = "grant"
        var publishOwed = false
        var seed = Data(repeating: 0x5A, count: 64)
        var revokeError: Error?
        var directoryError: Error?
        var deviceGroup: Result<DeviceRemovalPlan.DeviceGroupOutcome, Error> = .success(.done)
        var chatsAnswer = true
        var keyRecordAsked = 0
        var attestations: [Data?] = []
        var gesture: RemoveDeviceAttestation.Outcome = .notNeeded
        var gestureAskedFor: [Data] = []
    }

    private func runner(_ world: World, store: DeviceRemovalStore) -> DeviceRemovalRunner {
        let own = deviceA
        return DeviceRemovalRunner(
            ports: .init(
                session: {
                    DeviceRemovalRunner.Session(
                        accountId: Data(repeating: 1, count: 16), accountIdB64: "acct",
                        accountIdentitySeed: world.seed, ownDeviceId: own)
                },
                sessionToken: { "tok" },
                publishOwed: { _ in world.publishOwed },
                freshGrant: { world.grant },
                sign: { _, deviceId in
                    world.signed += 1
                    return Data([UInt8(world.signed)]) + deviceId
                },
                revoke: { _, grant, _, statement in
                    world.calls.append("revoke:\(grant)")
                    world.statements.append(statement)
                    if let error = world.revokeError { throw error }
                },
                deleteDirectoryRow: { _, _, statement in
                    world.calls.append("directory")
                    world.statements.append(statement)
                    if let error = world.directoryError { throw error }
                },
                removeFromDeviceGroup: { _, _, attestation in
                    world.calls.append("device-group")
                    world.attestations.append(attestation)
                    return try world.deviceGroup.get()
                },
                retireFromChats: { _ in
                    world.calls.append("chats")
                    return world.chatsAnswer
                },
                recordAccountKey: { world.keyRecordAsked += 1 },
                nowMs: { 42 },
                attestation: { deviceId in
                    world.gestureAskedFor.append(deviceId)
                    return world.gesture
                }),
            store: store)
    }

    func testARemovalRevokesFirstThenTheDirectoryThenTheDeviceGroupThenTheChats() async {
        let world = World()
        let store = DeviceRemovalStore(defaults: defaults("removal-order"))
        let removal = runner(world, store: store)
        let result = await removal.remove(deviceId: deviceB, attestation: Data([9]))
        XCTAssertEqual(result, .removed)
        XCTAssertEqual(world.calls, ["revoke:grant", "directory", "device-group", "chats"])
        XCTAssertEqual(world.attestations, [Data([9])])
        XCTAssertEqual(world.statements.count, 2)
        XCTAssertNotEqual(world.statements[0], world.statements[1], "signed right before each request")
        XCTAssertTrue(store.load(accountIdB64: "acct").isEmpty, "nothing is owed")
        XCTAssertTrue(removal.pendingDeviceIds.isEmpty)
    }

    func testWhenTheRevokeFailsNothingElseHappensAndNothingIsOwed() async {
        let world = World()
        world.revokeError = AuthClientError.http(status: 503, endpoint: "/account/devices/revoke", body: "")
        let store = DeviceRemovalStore(defaults: defaults("removal-not-revoked"))
        let result = await runner(world, store: store).remove(deviceId: deviceB, attestation: nil)
        XCTAssertEqual(result, .notRevoked(.failed))
        XCTAssertEqual(world.calls, ["revoke:grant"])
        XCTAssertTrue(store.load(accountIdB64: "acct").isEmpty)
    }

    func testAnAccountKeyRequiredAnswerAsksForTheGenesisMintAndRemovesNothing() async {
        let world = World()
        world.revokeError = AuthClientError.http(
            status: 403, endpoint: "/account/devices/revoke", body: #"{"error":"account-key-required"}"#)
        let store = DeviceRemovalStore(defaults: defaults("removal-no-key"))
        let result = await runner(world, store: store).remove(deviceId: deviceB, attestation: nil)
        XCTAssertEqual(result, .notRevoked(.accountKeyRequired))
        XCTAssertEqual(world.keyRecordAsked, 1)
        XCTAssertEqual(world.calls, ["revoke:grant"])
    }

    func testWithoutALiveProofNothingIsSent() async {
        let world = World()
        world.grant = nil
        let result = await runner(world, store: DeviceRemovalStore(defaults: defaults("removal-no-proof")))
            .remove(deviceId: deviceB, attestation: nil)
        XCTAssertEqual(result, .notRevoked(.freshAuthRequired))
        XCTAssertTrue(world.calls.isEmpty)
    }

    func testARemovalDoesNotRunWhileAKeyResetPublishIsOwedOnTheOwnDeviceOrWithoutKeys() async {
        let world = World()
        let removal = runner(world, store: DeviceRemovalStore(defaults: defaults("removal-blocked")))
        world.publishOwed = true
        let owed = await removal.remove(deviceId: deviceB, attestation: nil)
        XCTAssertEqual(owed, .notStarted(.blockedPublishOwed))
        world.publishOwed = false
        let own = await removal.remove(deviceId: deviceA, attestation: nil)
        XCTAssertEqual(own, .notStarted(.ownDevice))
        world.seed = Data()
        let keyless = await removal.remove(deviceId: deviceB, attestation: nil)
        XCTAssertEqual(keyless, .notStarted(.noAccountKeys))
        XCTAssertTrue(world.calls.isEmpty, "nothing was sent in any of the three")
    }

    func testAnOfflineStepAfterTheRevokeIsOwedAndRetriedWithANewStatementAndNoProof() async {
        let world = World()
        world.directoryError = DiscoveryClientError.network(
            endpoint: "/discovery/keypackage/device", underlying: URLError(.notConnectedToInternet))
        world.chatsAnswer = false
        let store = DeviceRemovalStore(defaults: defaults("removal-owed"))
        let removal = runner(world, store: store)
        let result = await removal.remove(deviceId: deviceB, attestation: Data([9]))
        XCTAssertEqual(result, .removedPending, "signed out at the server; the rest finishes by itself")
        // A later step is not held back by an earlier one that did not land.
        XCTAssertEqual(world.calls, ["revoke:grant", "directory", "device-group", "chats"])
        let owed = store.load(accountIdB64: "acct")
        XCTAssertEqual(owed.count, 1)
        XCTAssertEqual(owed.first.map(DeviceRemovalPlan.remaining), [.directory, .chats])
        XCTAssertEqual(removal.pendingDeviceIds, [deviceB.base64EncodedString()])
        let firstDirectoryStatement = world.statements[1]

        // The next landing: a fresh runner (a relaunch), no proof in hand.
        world.calls = []
        world.grant = nil
        world.directoryError = nil
        world.chatsAnswer = true
        let relaunched = runner(world, store: store)
        await relaunched.resumeOwed()
        XCTAssertEqual(world.calls, ["directory", "chats"], "never a second revoke, never the done step")
        XCTAssertNotEqual(world.statements.last, firstDirectoryStatement, "a statement is never stored")
        XCTAssertTrue(store.load(accountIdB64: "acct").isEmpty)
        XCTAssertTrue(relaunched.pendingDeviceIds.isEmpty)
    }

    func testRemovingADeviceAlreadySignedOutRunsOnlyWhatIsOwed() async {
        let world = World()
        world.deviceGroup = .success(.needsAttestation)
        let store = DeviceRemovalStore(defaults: defaults("removal-attest"))
        let removal = runner(world, store: store)
        let first = await removal.remove(deviceId: deviceB, attestation: nil)
        XCTAssertEqual(first, .removedPending, "the attested removal waits for the passkey gesture")
        XCTAssertEqual(store.load(accountIdB64: "acct").first.map(DeviceRemovalPlan.remaining), [.deviceGroup])

        world.calls = []
        world.deviceGroup = .success(.done)
        let second = await removal.remove(deviceId: deviceB, attestation: Data([7]))
        XCTAssertEqual(second, .removed)
        XCTAssertEqual(world.calls, ["device-group"], "no second revoke")
        XCTAssertEqual(world.attestations.last, Data([7]))
    }

    func testAnOwedRemovalIsNotRetriedWhileAKeyResetPublishIsOwed() async {
        let world = World()
        let store = DeviceRemovalStore(defaults: defaults("removal-resume-blocked"))
        store.put(DeviceRemovalRecord(deviceIdB64: deviceB.base64EncodedString(), revokedAtMs: 1), accountIdB64: "acct")
        world.publishOwed = true
        await runner(world, store: store).resumeOwed()
        XCTAssertTrue(world.calls.isEmpty)
        XCTAssertEqual(store.load(accountIdB64: "acct").count, 1)
    }

    // MARK: - The DeviceGroup filter

    private func device(_ key: String) -> DeviceReadmission.DirectoryDevice {
        DeviceReadmission.DirectoryDevice(deviceKeyB64: key, keyPackage: Data(key.utf8))
    }

    func testAnOwnDeviceIsAdmittedOnlyWhenItIsInTheDeviceGroup() {
        let roster = [DeviceReadmission.RosterLeafKeys(leafKeyB64: "own-1", personKeyB64: "me")]
        let accounts = [DeviceReadmission.AccountDevices(
            personKeyB64: "me", devices: [device("own-1"), device("own-2"), device("own-removed")])]
        let plan = DeviceReadmission.plan(
            roster: roster, ownPersonKeyB64: "me", accounts: accounts, removed: [],
            ownDeviceGroupRoster: ["own-1", "own-2"])
        XCTAssertEqual(plan.admit.map(\.deviceKeyB64), ["own-2"], "a device that re-published itself stays out")
    }

    func testAnUnreadableDeviceGroupAdmitsNoOwnDevice() {
        let accounts = [DeviceReadmission.AccountDevices(personKeyB64: "me", devices: [device("own-2")])]
        let plan = DeviceReadmission.plan(
            roster: [], ownPersonKeyB64: "me", accounts: accounts, removed: [], ownDeviceGroupRoster: nil)
        XCTAssertTrue(plan.admit.isEmpty)
        XCTAssertFalse(DeviceReadmission.ownDeviceAdmissible(deviceKeyB64: "x", ownDeviceGroupRoster: nil))
        XCTAssertFalse(DeviceReadmission.ownDeviceAdmissible(deviceKeyB64: "x", ownDeviceGroupRoster: []))
        XCTAssertTrue(DeviceReadmission.ownDeviceAdmissible(deviceKeyB64: "x", ownDeviceGroupRoster: ["x"]))
    }

    func testTheFilterIsForTheOwnAccountOnlyAndTheRemovedMemoryStillApplies() {
        let roster = [
            DeviceReadmission.RosterLeafKeys(leafKeyB64: "own-1", personKeyB64: "me"),
            DeviceReadmission.RosterLeafKeys(leafKeyB64: "bob-1", personKeyB64: "bob"),
        ]
        let accounts = [
            DeviceReadmission.AccountDevices(personKeyB64: "me", devices: [device("own-1"), device("own-2")]),
            DeviceReadmission.AccountDevices(personKeyB64: "bob", devices: [device("bob-1"), device("bob-2")]),
        ]
        let plan = DeviceReadmission.plan(
            roster: roster, ownPersonKeyB64: "me", accounts: accounts, removed: [],
            ownDeviceGroupRoster: ["own-1"])
        XCTAssertEqual(plan.admit.map(\.deviceKeyB64), ["bob-2"], "a contact's device is not judged by our DeviceGroup")
        // Defence in depth: a device this client saw removed stays out even
        // when the DeviceGroup lists it.
        let remembered = DeviceReadmission.plan(
            roster: roster, ownPersonKeyB64: "me", accounts: accounts, removed: ["own-2", "bob-2"],
            ownDeviceGroupRoster: ["own-1", "own-2"])
        XCTAssertTrue(remembered.admit.isEmpty)
    }

    func testTheRetireHalfIsUntouchedByTheFilter() {
        let roster = [
            DeviceReadmission.RosterLeafKeys(leafKeyB64: "own-1", personKeyB64: "me"),
            DeviceReadmission.RosterLeafKeys(leafKeyB64: "own-gone", personKeyB64: "me"),
        ]
        let accounts = [DeviceReadmission.AccountDevices(personKeyB64: "me", devices: [device("own-1")])]
        let plan = DeviceReadmission.plan(
            roster: roster, ownPersonKeyB64: "me", accounts: accounts, removed: [], ownDeviceGroupRoster: nil)
        XCTAssertEqual(plan.retire, ["own-gone"])
    }

    // MARK: - The surfaces

    func testTheRemoveSheetSaysWhatDidNotHappen() {
        typealias S = RemoveDeviceSheetState
        XCTAssertTrue(S.closes(on: .removed))
        XCTAssertTrue(S.closes(on: .removedPending), "signed out: the row takes over")
        XCTAssertFalse(S.closes(on: .notRevoked(.failed)))
        XCTAssertNil(S.line(for: .removed))
        XCTAssertNil(S.line(for: .notRevoked(.freshAuthRequired)), "the proof block says it")
        XCTAssertEqual(S.line(for: .notRevoked(.failed)), RemoveDeviceCopy.notRevoked)
        XCTAssertEqual(S.line(for: .notRevoked(.accountKeyRequired)), RemoveDeviceCopy.tryLater)
        XCTAssertEqual(S.line(for: .notRevoked(.offline)), RemoveDeviceCopy.offline)
        XCTAssertEqual(S.line(for: .notRevoked(.rateLimited)), RemoveDeviceCopy.rateLimited)
        XCTAssertEqual(S.line(for: .notStarted(.blockedPublishOwed)), RemoveDeviceCopy.publishOwed)
    }

    func testEveryKeyTheRemoveSurfacesUseIsInTheCatalog() {
        let keys = RemoveDeviceCopy.all + [
            FreshAuthSurface.removeDevice.helpKey,
            FreshAuthSurface.removeDevice.wrongPasswordKey, FreshAuthSurface.removeDevice.cancelledKey,
            "devices.signed_out.removing", "devices.signed_out.sign_in",
        ].compactMap { $0 }
        for key in keys {
            XCTAssertNotEqual(Localization.text(key, texts: ["device": "x"]), key, "\(key) is missing from the catalog")
        }
    }

    /// The T2 stand-ins are gone: each surface reads the key UX 18.6 names.
    func testTheRemoveSurfacesReadTheirOwnKeys() {
        XCTAssertEqual(RemoveDeviceCopy.confirm, "settings.devices.remove.confirm")
        XCTAssertEqual(RemoveDeviceCopy.working, "settings.devices.remove.working")
        XCTAssertEqual(RemoveDeviceCopy.pending, "devices.remove.pending")
        XCTAssertEqual(RemoveDeviceCopy.finish, "devices.remove.finish")
        XCTAssertEqual(RemoveDeviceCopy.done, "devices.remove.done")
        XCTAssertEqual(RemoveDeviceCopy.notRevoked, "devices.remove.error.not_revoked")
        XCTAssertEqual(RemoveDeviceCopy.tryLater, "devices.remove.error.try_later")
        XCTAssertEqual(RemoveDeviceCopy.passkeyCancelled, "proof.remove_device.cancelled")
        XCTAssertEqual(FreshAuthSurface.removeDevice.helpKey, "proof.remove_device.help")
        XCTAssertEqual(FreshAuthSurface.removeDevice.wrongPasswordKey, "proof.remove_device.wrong_password")
        XCTAssertEqual(FreshAuthSurface.removeDevice.cancelledKey, "proof.remove_device.cancelled")
        XCTAssertTrue(
            Localization.text(RemoveDeviceCopy.title, texts: ["device": "Device 2"]).contains("Device 2"),
            "the title keeps its {device} argument")
    }

    func testADeviceRowHasNoRemoveControlWhileItsRemovalIsFinishing() {
        XCTAssertEqual(DeviceRowPresentation.control(isSelf: true, removalPending: false), .none)
        XCTAssertEqual(DeviceRowPresentation.control(isSelf: true, removalPending: true), .none)
        XCTAssertEqual(DeviceRowPresentation.control(isSelf: false, removalPending: false), .remove)
        XCTAssertEqual(DeviceRowPresentation.control(isSelf: false, removalPending: true), .pending)
    }

    /// `Finish removing` is drawn only when the owed step waits on the
    /// person, never on a row that finishes by itself and never without an
    /// owed removal.
    func testADeviceRowOffersFinishOnlyWhenTheOwedStepWaitsOnThePerson() {
        XCTAssertEqual(
            DeviceRowPresentation.control(isSelf: false, removalPending: true, waitingOnPerson: true),
            .pendingFinish)
        XCTAssertEqual(
            DeviceRowPresentation.control(isSelf: false, removalPending: true, waitingOnPerson: false), .pending)
        XCTAssertEqual(
            DeviceRowPresentation.control(isSelf: false, removalPending: false, waitingOnPerson: true), .remove)
        XCTAssertEqual(
            DeviceRowPresentation.control(isSelf: true, removalPending: true, waitingOnPerson: true), .none)
        var record = DeviceRemovalRecord(deviceIdB64: "d", revokedAtMs: 1)
        XCTAssertTrue(DeviceRemovalPlan.waitsOnPerson(record, deviceGroupNeedsAttestation: true))
        XCTAssertFalse(DeviceRemovalPlan.waitsOnPerson(record, deviceGroupNeedsAttestation: false))
        record = DeviceRemovalPlan.marking(record, done: .deviceGroup)
        XCTAssertFalse(DeviceRemovalPlan.waitsOnPerson(record, deviceGroupNeedsAttestation: true))
    }

    func testTheLineUnderTheListAfterARemovalStep() {
        XCTAssertEqual(DeviceRemovalNote.afterRemove(.removed)?.key, RemoveDeviceCopy.done)
        XCTAssertNil(DeviceRemovalNote.afterRemove(.removedPending), "the row says Removing...")
        XCTAssertEqual(DeviceRemovalNote.afterFinish(.removed)?.key, RemoveDeviceCopy.done)
        XCTAssertNil(DeviceRemovalNote.afterFinish(.pending))
        XCTAssertNil(DeviceRemovalNote.afterFinish(.cancelled), "the button stays; nothing claims a result")
        XCTAssertNil(DeviceRemovalNote.afterFinish(.nothingOwed))
    }

    /// ADR-0015 attested removal after a relaunch: the background retry
    /// cannot make the gesture, so the removal waits on the person, and
    /// `Finish removing` runs the gesture and completes the owed steps.
    func testAnOwedRemovalThatNeedsThePasskeyWaitsOnThePersonAndFinishCompletesIt() async {
        let world = World()
        world.deviceGroup = .success(.needsAttestation)
        let store = DeviceRemovalStore(defaults: defaults("removal-finish"))
        let removal = runner(world, store: store)
        let first = await removal.remove(deviceId: deviceB, attestation: nil)
        XCTAssertEqual(first, .removedPending)
        let id = deviceB.base64EncodedString()
        XCTAssertEqual(removal.pendingDeviceIds, [id])
        XCTAssertEqual(removal.waitingOnPersonDeviceIds, [id], "the row carries Finish removing")

        // A relaunch: a new runner over the same owed record. The background
        // retry finds it waiting again.
        let relaunched = runner(world, store: store)
        relaunched.reload()
        XCTAssertTrue(relaunched.waitingOnPersonDeviceIds.isEmpty, "not known before a run looked")
        await relaunched.resumeOwed()
        XCTAssertEqual(relaunched.waitingOnPersonDeviceIds, [id])
        XCTAssertEqual(world.attestations.last ?? Data([1]), nil, "the background retry has no attestation")

        // The passkey sheet is dismissed: nothing changes, the button stays.
        world.gesture = .cancelled
        world.calls = []
        let cancelled = await relaunched.finish(deviceId: deviceB)
        XCTAssertEqual(cancelled, .cancelled)
        XCTAssertTrue(world.calls.isEmpty, "no step runs without the gesture")
        XCTAssertEqual(relaunched.waitingOnPersonDeviceIds, [id])

        // The gesture: the owed DeviceGroup step runs with the attestation.
        world.gesture = .obtained(Data([7, 7]))
        world.deviceGroup = .success(.done)
        let finished = await relaunched.finish(deviceId: deviceB)
        XCTAssertEqual(finished, .removed)
        XCTAssertEqual(world.calls, ["device-group"], "only what was owed; no second revoke")
        XCTAssertEqual(world.attestations.last, Data([7, 7]))
        XCTAssertEqual(world.gestureAskedFor, [deviceB, deviceB])
        XCTAssertTrue(relaunched.pendingDeviceIds.isEmpty)
        XCTAssertTrue(relaunched.waitingOnPersonDeviceIds.isEmpty)
        XCTAssertTrue(store.load(accountIdB64: "acct").isEmpty)
        let again = await relaunched.finish(deviceId: deviceB)
        XCTAssertEqual(again, .nothingOwed)
    }

    /// A removal that is merely offline does not wait on the person.
    func testAnOfflineOwedRemovalDoesNotOfferFinish() async {
        let world = World()
        world.deviceGroup = .failure(URLError(.notConnectedToInternet))
        let store = DeviceRemovalStore(defaults: defaults("removal-offline-row"))
        let removal = runner(world, store: store)
        _ = await removal.remove(deviceId: deviceB, attestation: nil)
        XCTAssertEqual(removal.pendingDeviceIds, [deviceB.base64EncodedString()])
        XCTAssertTrue(removal.waitingOnPersonDeviceIds.isEmpty)
    }

    func testTheSignedOutScreenFollowsTheCause() {
        XCTAssertEqual(RemovedDevice.variant(cause: .deviceRemoved, state: .removing), .removed(.removing))
        XCTAssertEqual(RemovedDevice.variant(cause: .deviceRemoved, state: .removed), .removed(.removed))
        XCTAssertEqual(RemovedDevice.variant(cause: .keysReset, state: .removing), .keysReset(.removing))
    }
}

/// Records callback names from any thread.
final class Counter: @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [String] = []
    func bump(_ name: String) { lock.withLock { recorded.append(name) } }
    var names: [String] { lock.withLock { recorded } }
}

/// ADR-0066 sprint T2 — the repository around the rules: a landing names the
/// device, the account key is recorded, Log out reaches the server, and a
/// removed device wipes by itself.
@MainActor
final class DeviceRevocationRepositoryTests: XCTestCase {

    private final class Persistence: SessionPersistence {
        var seed: Data? = Data(repeating: 0x5A, count: 64)
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

    private final class WipeLog {
        private(set) var accounts: [String] = []
        func note(_ account: String) { accounts.append(account) }
    }

    private let accountId = Data((0..<16).map { UInt8($0) })
    private var accountIdB64: String { base64Encode(accountId) }
    private let ownDevice = Data(repeating: 0xD1, count: 32)
    private var suites: [String] = []

    override func tearDown() async throws {
        for suite in suites { UserDefaults().removePersistentDomain(forName: suite) }
        try await super.tearDown()
    }

    private func defaults(_ name: String) -> UserDefaults {
        let suite = "\(name)-\(UUID().uuidString)"
        suites.append(suite)
        return UserDefaults(suiteName: suite)!
    }

    private struct Harness {
        let repository: AuthRepository
        let tokens: SessionTokenProvider
        let persistence: Persistence
        let wiped: WipeLog
        let host: String
        let marks: IdentityKeyRecordMarks
        let logoutOwed: InMemoryLogoutOwedStore
        let recalls: InMemoryRecallRecordStore
    }

    private static let boundPairJSON = """
        {"session_token":"tok-bound","session_expires_at_ms":2000000,"refresh_token":"r2.bound",\
        "refresh_expires_at_ms":9000000000,"key_generation":1}
        """

    /// A relaunch restore of a signed-in account against a scripted auth.
    private func restored(
        pair: SessionPair,
        owedLogout: SessionPair? = nil,
        signedIn: Bool = true,
        auth: @escaping NameRouteStub.Handler
    ) -> Harness {
        let store = SessionStore(defaults: defaults("revocation-session"))
        store.save(PersistedSession(accountIdB64: accountIdB64, username: "alice"))
        let persistence = Persistence()
        if signedIn { persistence.token = PersistedSessionToken(pair: pair) }
        let tokens = SessionTokenProvider()
        tokens.nowMs = { 1_000_000 }
        let wiped = WipeLog()
        let host = NameRouteStub.scope(auth)
        let marks = IdentityKeyRecordMarks(defaults: defaults("revocation-marks"))
        let owed = InMemoryLogoutOwedStore()
        if let owedLogout { owed.store(owedLogout) }
        let directory = Directory()
        let device = ownDevice
        let recalls = InMemoryRecallRecordStore()
        let repository = AuthRepository(
            sessionStore: store,
            authClientFactory: { NameRouteStub.client(host) },
            persistence: persistence,
            nowMs: { 1_000_000 },
            accountDirectory: directory,
            loginHandleBinds: LoginHandleBindLedger(),
            restoreDirectory: directory,
            markers: RecoveryMarkers(defaults: defaults("revocation-markers")),
            tokenProvider: tokens,
            grantNowMs: { 1_000_000 },
            wipeAfterKeyReset: { account in wiped.note(account) },
            recallRecords: recalls,
            recallOperations: .unreachable,
            ownDeviceId: { _ in device },
            identityKeyMarks: marks,
            logoutOwed: owed
        )
        // The launch refresh is not what these tests are about.
        tokens.refresher = { _ in throw URLError(.notConnectedToInternet) }
        return Harness(
            repository: repository, tokens: tokens, persistence: persistence, wiped: wiped,
            host: host, marks: marks, logoutOwed: owed, recalls: recalls)
    }

    private func waitUntil(_ condition: @MainActor () -> Bool, timeout: TimeInterval = 5) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 20_000_000)
        }
        return condition()
    }

    private func paths(_ host: String) -> [String] {
        NameRouteStub.requests(host).map { "\($0.method) \($0.path)" }
    }

    private static let genesisOK = (200, #"{"genesis_attest_b64":"AAAA"}"#)

    func testALandingWithAnUnboundPairBindsTheDeviceAndDropsTheUnboundPair() async {
        let harness = restored(pair: .fixture(access: "tok-unbound", refresh: "r2.unbound")) { _, path, _ in
            switch path {
            case "/auth/session/bind-device": return (200, Self.boundPairJSON)
            case "/auth/genesis/attest": return Self.genesisOK
            default: return (503, "")
            }
        }
        let bound = await waitUntil { harness.tokens.current?.sessionToken == "tok-bound" }
        XCTAssertTrue(bound)
        let request = NameRouteStub.requests(harness.host).first { $0.path == "/auth/session/bind-device" }
        XCTAssertEqual(request?.json["device_id_b64"] as? String, base64UrlEncodeNoPad(ownDevice))
        XCTAssertEqual(request?.json["refresh_token"] as? String, "r2.unbound")
        XCTAssertEqual(request?.authorization, "Bearer tok-unbound")
        XCTAssertEqual(harness.tokens.current?.deviceIdB64, ownDevice.base64EncodedString())
        XCTAssertEqual(harness.repository.identitySecrets?.sessionToken, "tok-bound")
        XCTAssertEqual(harness.persistence.token?.sessionToken, "tok-bound", "the unbound pair is not kept")
        XCTAssertEqual(harness.persistence.token?.deviceIdB64, ownDevice.base64EncodedString())
    }

    func testALandingWhosePairAlreadyNamesThisDeviceDoesNotBindAgain() async {
        let harness = restored(pair: SessionPair.fixture().naming(deviceId: ownDevice)) { _, path, _ in
            path == "/auth/genesis/attest" ? Self.genesisOK : (503, "")
        }
        let minted = await waitUntil { harness.marks.recorded(accountIdB64: self.accountIdB64) != nil }
        XCTAssertTrue(minted)
        XCTAssertFalse(paths(harness.host).contains("POST /auth/session/bind-device"))
    }

    func testTheAccountKeyIsRecordedOnALandingUntilItSucceededOnce() async throws {
        let seed = Data(repeating: 0x5A, count: 64)
        let pubkey = try Identity.fromSeed(seed: seed).signaturePublicKey()
        let answers = Counter()
        let harness = restored(pair: SessionPair.fixture().naming(deviceId: ownDevice)) { _, path, _ in
            guard path == "/auth/genesis/attest" else { return (503, "") }
            answers.bump("attest")
            // The first landing cannot reach a working auth.
            return answers.names.count == 1 ? (503, "") : Self.genesisOK
        }
        let asked = await waitUntil { answers.names.count == 1 }
        XCTAssertTrue(asked)
        try? await Task.sleep(nanoseconds: 100_000_000)
        XCTAssertNil(harness.marks.recorded(accountIdB64: accountIdB64), "a failed mint records nothing")
        let first = NameRouteStub.requests(harness.host).first { $0.path == "/auth/genesis/attest" }
        XCTAssertEqual(first?.json["pubkey_b64"] as? String, base64UrlEncodeNoPad(pubkey))

        // The next landing tries again, and then never again.
        await harness.repository.recordIdentityKeyIfNeeded(accountIdBytes: accountId, accountIdentitySeed: seed)
        XCTAssertEqual(harness.marks.recorded(accountIdB64: accountIdB64), pubkey.base64EncodedString())
        await harness.repository.recordIdentityKeyIfNeeded(accountIdBytes: accountId, accountIdentitySeed: seed)
        XCTAssertEqual(answers.names.count, 2, "recorded: no further mint")

        // After a key reset the key is another one: the mint runs again.
        let newSeed = Data(repeating: 0x6B, count: 64)
        await harness.repository.recordIdentityKeyIfNeeded(accountIdBytes: accountId, accountIdentitySeed: newSeed)
        XCTAssertEqual(answers.names.count, 3)
        XCTAssertEqual(
            harness.marks.recorded(accountIdB64: accountIdB64),
            try Identity.fromSeed(seed: newSeed).signaturePublicKey().base64EncodedString())
    }

    func testAKeyMismatchAtTheMintRecordsNothing() async {
        let harness = restored(pair: SessionPair.fixture().naming(deviceId: ownDevice)) { _, path, _ in
            path == "/auth/genesis/attest" ? (409, #"{"error":"identity-key-mismatch"}"#) : (503, "")
        }
        let asked = await waitUntil { self.paths(harness.host).contains("POST /auth/genesis/attest") }
        XCTAssertTrue(asked)
        try? await Task.sleep(nanoseconds: 100_000_000)
        XCTAssertNil(harness.marks.recorded(accountIdB64: accountIdB64))
        XCTAssertNil(harness.repository.keysSignOut, "a mismatch at the mint wipes nothing")
    }

    func testLogOutSignsTheSessionOutAtTheServerAndWipesNothing() async {
        let harness = restored(pair: SessionPair.fixture(access: "tok-live").naming(deviceId: ownDevice)) { _, path, _ in
            switch path {
            case "/auth/session/logout": return (204, "")
            case "/auth/genesis/attest": return Self.genesisOK
            default: return (503, "")
            }
        }
        harness.repository.logout()
        XCTAssertEqual(harness.repository.state, .anon(cachedUsername: "alice"), "the local sign-out does not wait")
        let sent = await waitUntil { self.paths(harness.host).contains("POST /auth/session/logout") }
        XCTAssertTrue(sent)
        let request = NameRouteStub.requests(harness.host).first { $0.path == "/auth/session/logout" }
        XCTAssertEqual(request?.authorization, "Bearer tok-live")
        let cleared = await waitUntil { harness.logoutOwed.load() == nil }
        XCTAssertTrue(cleared, "answered: nothing is owed")
        XCTAssertTrue(harness.wiped.accounts.isEmpty)
        XCTAssertNil(harness.repository.keysSignOut)
    }

    func testAnOfflineLogOutIsOwedAndRetriedOnceAtTheNextLaunch() async {
        // The launch: an owed logout, nobody signed in.
        let owed = SessionPair.fixture(access: "tok-owed", accessExpiresAtMs: 2_000_000)
        let harness = restored(pair: .fixture(), owedLogout: owed, signedIn: false) { _, path, _ in
            path == "/auth/session/logout" ? (204, "") : (503, "")
        }
        XCTAssertEqual(harness.repository.state, .anon(cachedUsername: "alice"))
        let sent = await waitUntil { self.paths(harness.host).contains("POST /auth/session/logout") }
        XCTAssertTrue(sent)
        XCTAssertEqual(
            NameRouteStub.requests(harness.host).first { $0.path == "/auth/session/logout" }?.authorization,
            "Bearer tok-owed")
        let cleared = await waitUntil { harness.logoutOwed.load() == nil }
        XCTAssertTrue(cleared)
    }

    func testAnOwedLogoutIsDroppedWhenASessionWasRestored() async {
        let harness = restored(
            pair: SessionPair.fixture().naming(deviceId: ownDevice),
            owedLogout: .fixture(access: "tok-owed")
        ) { _, path, _ in
            path == "/auth/genesis/attest" ? Self.genesisOK : (503, "")
        }
        XCTAssertNil(harness.logoutOwed.load(), "sending it would end the session that is live now")
        try? await Task.sleep(nanoseconds: 150_000_000)
        XCTAssertFalse(paths(harness.host).contains("POST /auth/session/logout"))
    }

    func testARemovedDeviceShowsTheRemovedScreenAndWipesByItself() async {
        let harness = restored(pair: SessionPair.fixture().naming(deviceId: ownDevice)) { _, path, _ in
            path == "/auth/genesis/attest" ? Self.genesisOK : (503, "")
        }
        harness.tokens.refresher = { _ in
            throw AuthClientError.http(
                status: 401, endpoint: "/auth/session/refresh", body: #"{"error":"device-revoked"}"#)
        }
        _ = await harness.tokens.refresh()
        let done = await waitUntil { harness.repository.keysSignOut == .removed }
        XCTAssertTrue(done)
        XCTAssertEqual(harness.repository.keysSignOutCause, .deviceRemoved)
        XCTAssertEqual(harness.repository.keysSignOutUsername, "alice")
        XCTAssertEqual(harness.wiped.accounts, [accountIdB64], "the wipe ran by itself, once")
        XCTAssertNil(harness.tokens.current, "nothing more is sent with the revoked pair")
        XCTAssertFalse(paths(harness.host).contains("POST /auth/session/logout"), "a removal is not a logout")
    }

    func testTheDevicesOwnDeviceGroupTurningInactiveIsProofOfRemoval() async {
        let harness = restored(pair: SessionPair.fixture().naming(deviceId: ownDevice)) { _, path, _ in
            path == "/auth/genesis/attest" ? Self.genesisOK : (503, "")
        }
        harness.repository.deviceGroupReportedRemoval()
        XCTAssertEqual(harness.repository.keysSignOut, .removing)
        XCTAssertEqual(harness.repository.keysSignOutCause, .deviceRemoved)
        let done = await waitUntil { harness.repository.keysSignOut == .removed }
        XCTAssertTrue(done)
        XCTAssertEqual(harness.wiped.accounts, [accountIdB64])
    }

    /// ADR-0066 T3 item 1: an older generation is always told
    /// `key-generation-superseded`. On a device that holds the keys that is
    /// the staged wipe and "Was it you?"; a removal signal arriving while it
    /// is up changes nothing and must not destroy the recall record.
    func testAResetElsewhereKeepsItsOwnCause() async {
        let harness = restored(pair: SessionPair.fixture().naming(deviceId: ownDevice)) { _, path, _ in
            path == "/auth/genesis/attest" ? Self.genesisOK : (503, "")
        }
        harness.repository.handleKeysSuperseded(accountIdB64: accountIdB64)
        XCTAssertNotNil(harness.repository.recall, "a device with the keys asks: Was it you?")
        XCTAssertNil(harness.repository.keysSignOut)
        XCTAssertNotNil(harness.recalls.record)
        // A removal signal while the recall is up changes nothing.
        harness.repository.handleDeviceRevoked(accountIdB64: accountIdB64)
        XCTAssertNil(harness.repository.keysSignOut)
        XCTAssertNotNil(harness.repository.recall)
        XCTAssertNotNil(harness.recalls.record, "the recall record survives a late removal signal")
        let done = await waitUntil { harness.wiped.accounts == [self.accountIdB64] }
        XCTAssertTrue(done, "one wipe")
    }

    /// `device-revoked` wipes everything and never leaves a recall record,
    /// not even one a previous reset left behind.
    func testARemovedDeviceLeavesNoRecallRecord() async throws {
        let harness = restored(pair: SessionPair.fixture().naming(deviceId: ownDevice)) { _, path, _ in
            path == "/auth/genesis/attest" ? Self.genesisOK : (503, "")
        }
        try harness.recalls.put(RecallRecord(
            accountIdB64: accountIdB64, oldSeed: Data(repeating: 1, count: 32), username: "alice", learnedAtMs: 1))
        harness.repository.handleDeviceRevoked(accountIdB64: accountIdB64)
        XCTAssertEqual(harness.repository.keysSignOutCause, .deviceRemoved)
        XCTAssertNil(harness.repository.recall)
        XCTAssertNil(harness.recalls.record, "a removal changes no keys: nothing to take back")
        let done = await waitUntil { harness.repository.keysSignOut == .removed }
        XCTAssertTrue(done)
        XCTAssertEqual(harness.wiped.accounts, [accountIdB64])
    }

    func testALoggedOutSessionIsSignedOutWithoutAWipe() async {
        let harness = restored(pair: SessionPair.fixture().naming(deviceId: ownDevice)) { _, path, _ in
            path == "/auth/genesis/attest" ? Self.genesisOK : (503, "")
        }
        harness.tokens.refresher = { _ in
            throw AuthClientError.http(
                status: 401, endpoint: "/auth/session/refresh", body: #"{"error":"refresh-invalid"}"#)
        }
        _ = await harness.tokens.refresh()
        let signedOut = await waitUntil { harness.repository.state == .anon(cachedUsername: "alice") }
        XCTAssertTrue(signedOut)
        XCTAssertNil(harness.repository.keysSignOut)
        XCTAssertTrue(harness.wiped.accounts.isEmpty, "a logout is never a wipe")
    }
}
