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
    }

    override func tearDown() {
        URLProtocol.unregisterClass(StatusStub.self)
        UserDefaults(suiteName: suiteName)?.removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    func testTheWipeRunsForTheDeletedAccountAfterTheServerDeletion() async {
        StatusStub.status = 204
        var wiped: [String] = []
        let model = makeModel { wiped.append($0) }

        await model.deleteAccount()

        XCTAssertEqual(wiped, [accountIdB64])
        XCTAssertNil(model.message)
    }

    func testNoWipeWhenTheServerRefuses() async {
        StatusStub.status = 500
        var wiped: [String] = []
        let model = makeModel { wiped.append($0) }

        await model.deleteAccount()

        XCTAssertEqual(wiped, [], "the account still exists, so its data stays")
        XCTAssertNotNil(model.message)
    }

    // MARK: - Fixtures

    private func makeModel(wipe: @escaping (String) -> Void) -> PasskeysViewModel {
        let sessionStore = SessionStore(defaults: UserDefaults(suiteName: suiteName)!)
        sessionStore.save(PersistedSession(accountIdB64: accountIdB64, username: "dora"))
        let auth = AuthRepository(
            sessionStore: sessionStore,
            authClientFactory: { AuthClient(baseURL: "http://auth.test") },
            persistence: RestoringPersistence(),
            nowMs: { 1_000_000 },
            accountDirectory: NoDirectory(),
            pushBinding: PushAccountBindingStore(defaults: UserDefaults(suiteName: suiteName)!),
            removeDeliveredNotifications: {}
        )
        XCTAssertNotNil(auth.identitySecrets, "the fixture restores a session")
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StatusStub.self]
        return PasskeysViewModel(
            authRepository: auth,
            wipeAfterDeletion: wipe,
            client: WebauthnClient(baseURL: "http://auth.test", session: URLSession(configuration: configuration))
        )
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

        override class func canInit(with request: URLRequest) -> Bool { true }
        override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

        override func startLoading() {
            let response = HTTPURLResponse(
                url: request.url ?? URL(string: "http://stub.test")!,
                statusCode: Self.status, httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": "application/json"])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: Data("{}".utf8))
            client?.urlProtocolDidFinishLoading(self)
        }

        override func stopLoading() {}
    }
}
