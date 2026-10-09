// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// Delete account runs the same local wipe as Forget device, after the server
/// deletion and only after it. Before 2026-09-30 it called the auth half of
/// the wipe directly, so every chat store stayed on the device.
///
/// The network is a stub: every request in the process gets the configured
/// status, so neither the auth nor the discovery call leaves the simulator.
@MainActor
final class DeleteAccountWipeTests: XCTestCase {

    private let accountIdB64 = "D1D1D1D1D1D1D1D1D1D1Dw=="
    private var suiteName: String!

    override func setUp() {
        super.setUp()
        suiteName = "delete-account-wipe-\(UUID().uuidString)"
        URLProtocol.registerClass(StatusStub.self)
        StatusStub.body = "{}"
        StatusStub.requests = 0
        StatusStub.lastFreshGrant = nil
    }

    override func tearDown() {
        URLProtocol.unregisterClass(StatusStub.self)
        UserDefaults(suiteName: suiteName)?.removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    func testTheWipeRunsForTheDeletedAccountAfterTheServerDeletion() async throws {
        StatusStub.status = 204
        var wiped: [String] = []
        let (model, auth) = makeModel { wiped.append($0) }
        try await auth.proveWithPassword(password: Data("pw".utf8))

        let deleted = await model.deleteAccount(proof: model.makeProof(.deleteAccount), hasPasskey: false)

        XCTAssertTrue(deleted)
        XCTAssertEqual(wiped, [accountIdB64])
        XCTAssertNil(model.message)
        XCTAssertEqual(StatusStub.lastFreshGrant, "grant-1", "the grant rides in the header")
    }

    func testNoWipeWhenTheServerRefuses() async throws {
        StatusStub.status = 500
        var wiped: [String] = []
        let (model, auth) = makeModel { wiped.append($0) }
        try await auth.proveWithPassword(password: Data("pw".utf8))

        let deleted = await model.deleteAccount(proof: model.makeProof(.deleteAccount), hasPasskey: false)

        XCTAssertFalse(deleted)
        XCTAssertEqual(wiped, [], "the account still exists, so its data stays")
        XCTAssertNotNil(model.message)
    }

    /// UX 17.3 — without a proof (and with nothing typed) nothing is sent and
    /// nothing is wiped.
    func testWithoutAProofNothingIsSentAndNothingIsWiped() async {
        StatusStub.status = 204
        StatusStub.requests = 0
        var wiped: [String] = []
        let (model, _) = makeModel { wiped.append($0) }

        let deleted = await model.deleteAccount(proof: model.makeProof(.deleteAccount), hasPasskey: false)

        XCTAssertFalse(deleted)
        XCTAssertEqual(wiped, [])
        XCTAssertEqual(StatusStub.requests, 0, "no request before the proof")
    }

    /// The proof ran out on the way: the block appears (no error line), the
    /// proof is dropped, nothing is wiped.
    func testA403FreshAuthRequiredReopensTheBlock() async throws {
        StatusStub.status = 403
        StatusStub.body = #"{"error":"fresh-auth-required"}"#
        var wiped: [String] = []
        let (model, auth) = makeModel { wiped.append($0) }
        try await auth.proveWithPassword(password: Data("pw".utf8))
        let proof = model.makeProof(.deleteAccount)

        let deleted = await model.deleteAccount(proof: proof, hasPasskey: false)

        XCTAssertFalse(deleted)
        XCTAssertEqual(wiped, [])
        XCTAssertNil(model.message, "nothing went wrong: no error styling")
        XCTAssertFalse(auth.holdsLiveFreshAuth())
        XCTAssertFalse(proof.proofLive)
        XCTAssertTrue(proof.layout.showsBlock)
    }

    /// ADR-0066 section 1.3 — while the account can still be taken back
    /// after a key reset it cannot be deleted: `409 recall-window-open` shows
    /// the blocked line with the date, and nothing is wiped.
    func testA409RecallWindowOpenShowsTheBlockedLineAndWipesNothing() async throws {
        StatusStub.status = 409
        StatusStub.body = #"{"error":"recall-window-open","until_ms":1762387200000}"#
        var wiped: [String] = []
        let (model, auth) = makeModel { wiped.append($0) }
        try await auth.proveWithPassword(password: Data("pw".utf8))

        let deleted = await model.deleteAccount(proof: model.makeProof(.deleteAccount), hasPasskey: false)

        XCTAssertFalse(deleted)
        XCTAssertEqual(wiped, [], "the account still exists, so its data stays")
        guard case .error(let line)? = model.message else {
            return XCTFail("expected the blocked line")
        }
        let date = RecallCopy.date(ms: 1_762_387_200_000, locale: Localization.currentLocale)
        XCTAssertEqual(
            line.text, Localization.text("settings.account.delete.blocked", texts: ["date": date]))
        XCTAssertTrue(line.text.contains(date))
        XCTAssertTrue(auth.holdsLiveFreshAuth(), "the proof is not what was refused")
    }

    // MARK: - An account with a passkey: one gesture (UX 17.3)

    /// The gesture bound to the deletion is the proof: one passkey sheet, no
    /// sign-in first, no fresh-auth grant obtained or sent.
    func testAPasskeyAccountMakesOneGestureAndNeedsNoGrant() async {
        StatusStub.status = 204
        var wiped: [String] = []
        var gestures: [String] = []
        var passkeySignIns = 0
        let (model, auth) = makeModel(gesture: { gestures.append($0) }) { wiped.append($0) }
        let proof = FreshAuthProofModel(surface: .deleteAccount, ports: .init(
            holdsLive: { false }, usernameKnown: { true }, hasPasskey: { true },
            proveWithPassword: { _, _ in XCTFail("no password is asked") },
            proveWithPasskey: { passkeySignIns += 1 }))

        let deleted = await model.deleteAccount(proof: proof, hasPasskey: true)

        XCTAssertTrue(deleted)
        XCTAssertEqual(gestures.count, 1, "one system passkey sheet")
        XCTAssertEqual(
            gestures.first, actionHashForDeleteAccount(accountId: Data(base64Encoded: accountIdB64)!),
            "bound to deleting this account")
        XCTAssertEqual(passkeySignIns, 0, "never a passkey sign-in first")
        XCTAssertFalse(auth.holdsLiveFreshAuth())
        XCTAssertNil(StatusStub.lastFreshGrant, "the header is not sent without a live proof")
        XCTAssertEqual(wiped, [accountIdB64])
    }

    /// A proof that happens to be live rides along; the gesture is still made.
    func testAPasskeyAccountSendsALiveGrantAndStillMakesTheGesture() async throws {
        StatusStub.status = 204
        var gestures = 0
        let (model, auth) = makeModel(gesture: { _ in gestures += 1 }) { _ in }
        try await auth.proveWithPassword(password: Data("pw".utf8))

        let deleted = await model.deleteAccount(proof: model.makeProof(.deleteAccount), hasPasskey: true)

        XCTAssertTrue(deleted)
        XCTAssertEqual(gestures, 1)
        XCTAssertEqual(StatusStub.lastFreshGrant, "grant-1")
    }

    /// Dismissing the one sheet deletes nothing and sends nothing.
    func testADismissedGestureDeletesNothing() async {
        StatusStub.status = 204
        var wiped: [String] = []
        let (model, _) = makeModel(
            gesture: { _ in throw WebauthnAssertion.AssertionError.cancelled }) { wiped.append($0) }

        let deleted = await model.deleteAccount(proof: model.makeProof(.deleteAccount), hasPasskey: true)

        XCTAssertFalse(deleted)
        XCTAssertEqual(wiped, [])
        XCTAssertEqual(StatusStub.requests, 0, "no DELETE without the gesture")
        guard case .error(let line) = model.message else {
            return XCTFail("the dialog says the deletion was cancelled")
        }
        XCTAssertEqual(line.text, Localization.text("settings.account.delete.error.cancelled"))
    }

    /// The device thought the account had no passkey; the server says the
    /// action grant is missing: one gesture, one retry.
    func testA403ActionAuthorizationMakesTheGestureAndRetriesOnce() async throws {
        StatusStub.status = 403
        StatusStub.body = "action-authorization-required"
        var gestures = 0
        var wiped: [String] = []
        let (model, auth) = makeModel(gesture: { _ in
            gestures += 1
            StatusStub.status = 204
        }) { wiped.append($0) }
        try await auth.proveWithPassword(password: Data("pw".utf8))

        let deleted = await model.deleteAccount(proof: model.makeProof(.deleteAccount), hasPasskey: false)

        XCTAssertTrue(deleted)
        XCTAssertEqual(gestures, 1)
        XCTAssertEqual(wiped, [accountIdB64])
    }

    // MARK: - Fixtures

    private func makeModel(
        gesture: (@MainActor (String) async throws -> Void)? = nil,
        wipe: @escaping (String) -> Void
    ) -> (PasskeysViewModel, AuthRepository) {
        let accountId = Data(base64Encoded: accountIdB64)!
        let sessionStore = SessionStore(defaults: UserDefaults(suiteName: suiteName)!)
        sessionStore.save(PersistedSession(accountIdB64: accountIdB64, username: "dora"))
        let auth = AuthRepository(
            sessionStore: sessionStore,
            authClientFactory: { AuthClient(baseURL: "http://auth.test") },
            persistence: RestoringPersistence(),
            nowMs: { 1_000_000 },
            accountDirectory: NoDirectory(),
            pushBinding: PushAccountBindingStore(defaults: UserDefaults(suiteName: suiteName)!),
            removeDeliveredNotifications: {},
            proofLoginOperation: { _, _ in
                LoginResult(
                    accountId: accountId, sessionKey: Data(), exportKey: Data(repeating: 0xE7, count: 32),
                    session: .fixture(access: "tok-proof", refresh: "r1.proof"),
                    freshGrant: FreshAuthGrant(grant: "grant-1", expiresAtMs: 1_600_000))
            },
            passkeyPresence: { _ in false },
            grantNowMs: { 1_000_000 }
        )
        XCTAssertNotNil(auth.identitySecrets, "the fixture restores a session")
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StatusStub.self]
        let model = PasskeysViewModel(
            authRepository: auth,
            wipeAfterDeletion: wipe,
            client: WebauthnClient(baseURL: "http://auth.test", session: URLSession(configuration: configuration)),
            assertDeleteAction: gesture ?? { _ in XCTFail("no passkey gesture is expected") }
        )
        return (model, auth)
    }

    private final class RestoringPersistence: SessionPersistence {
        func hasDeviceKey(accountIdB64: String) -> Bool { true }
        func accountIdentitySeed(accountIdB64: String) -> Data? { Data(repeating: 0x5A, count: 64) }
        func exportKey(accountIdB64: String) -> Data? { Data(repeating: 0xE7, count: 32) }
        func sessionToken(accountIdB64: String) -> PersistedSessionToken? {
            PersistedSessionToken(sessionToken: "tok-live", sessionExpiresAtMs: 2_000_000)
        }
        func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws {}
        func deleteSessionToken(accountIdB64: String) {}
    }

    private final class NoDirectory: AccountDirectory {
        func registerUsername(accountId: Data, username: String, sessionToken: String) async throws -> RegisterResult {
            throw AuthClientError.argument("discovery must not be reached")
        }
        func publishIdentityBundle(
            accountId: Data, signaturePubkey: Data, x25519Pubkey: Data,
            leafAuth: Data?, genesisAttest: Data?, keyResetAttest: Data?, sessionToken: String
        ) async throws {
            throw AuthClientError.argument("discovery must not be reached")
        }
    }

    /// Answers every request with ``status`` and an empty JSON body.
    final class StatusStub: URLProtocol {
        nonisolated(unsafe) static var status = 204
        nonisolated(unsafe) static var body = "{}"
        nonisolated(unsafe) static var requests = 0
        nonisolated(unsafe) static var lastFreshGrant: String?

        override class func canInit(with request: URLRequest) -> Bool { true }
        override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

        /// ADR-0066: every session landing names the device and records the
        /// account key at auth, in the background. Those two are not what
        /// these tests count.
        private static let landingPaths: Set<String> = ["/auth/session/bind-device", "/auth/genesis/attest"]

        override func startLoading() {
            if !Self.landingPaths.contains(request.url?.path ?? "") {
                Self.requests += 1
            }
            if let grant = request.value(forHTTPHeaderField: WebauthnClient.freshGrantHeader) {
                Self.lastFreshGrant = grant
            }
            let response = HTTPURLResponse(
                url: request.url ?? URL(string: "http://stub.test")!,
                statusCode: Self.status, httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": "application/json"])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: Data(Self.body.utf8))
            client?.urlProtocolDidFinishLoading(self)
        }

        override func stopLoading() {}
    }
}
