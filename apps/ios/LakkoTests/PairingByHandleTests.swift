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

    func testAnUnsupportedVersionShowsTheUpdateCopyAndAnythingElseItsDescription() {
        XCTAssertEqual(
            PairingScanFailure.userFacing(FfiError.Unsupported(message: "pairing QR version 1")),
            PairingScanFailure.unsupportedCopy)
        XCTAssertFalse(PairingScanFailure.isWrongAccount(FfiError.Group(message: "something else")))
        XCTAssertEqual(
            PairingScanFailure.userFacing(AuthClientError.argument("boom")),
            AuthClientError.argument("boom").errorDescription)
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
        XCTAssertEqual(model.error, "Enter your username first.")
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
                phraseSeed: randomBytes(64), sessionToken: "tok", sessionExpiresAtMs: 42, accountId: nil))
            XCTFail("no account id from the redeem, no session")
        } catch {}
        XCTAssertTrue(NameRouteStub.requests(host).isEmpty)
    }
}
