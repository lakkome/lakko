// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// Where a relaunch lands, for every combination of on-device material.
///
/// The defect: a relaunch saw a session marker plus a device key and flipped
/// to `.session` with no `IdentitySecrets`, so chat and contacts were dead
/// until the user logged out and in again. A relaunch must now land either a
/// FULL session (state and secrets together) or the logged-out state — and a
/// missing, undecodable or expired input must fail closed to logged out.
@MainActor
final class SessionRestoreTests: XCTestCase {

    // MARK: - Fixtures

    private final class FakePersistence: SessionPersistence {
        var deviceKey = true
        var seed: Data? = Data(repeating: 0x5A, count: 64)
        var export: Data? = Data(repeating: 0xE7, count: 32)
        var token: PersistedSessionToken? = PersistedSessionToken(
            sessionToken: "tok-live",
            sessionExpiresAtMs: 2_000_000
        )
        private(set) var putTokens: [(PersistedSessionToken, String)] = []
        private(set) var deletedTokens: [String] = []

        func hasDeviceKey(accountIdB64: String) -> Bool { deviceKey }
        func accountIdentitySeed(accountIdB64: String) -> Data? { seed }
        func exportKey(accountIdB64: String) -> Data? { export }
        func sessionToken(accountIdB64: String) -> PersistedSessionToken? { token }
        func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws {
            putTokens.append((token, accountIdB64))
        }
        func deleteSessionToken(accountIdB64: String) {
            deletedTokens.append(accountIdB64)
        }
    }

    private let accountId = Data((0..<16).map { UInt8($0) })
    private var accountIdB64: String { base64Encode(accountId) }
    private var marker: PersistedSession {
        PersistedSession(accountIdB64: accountIdB64, username: "alice")
    }
    private let now: Int64 = 1_000_000

    private func decide(
        _ persistence: FakePersistence,
        marker: PersistedSession?? = .none
    ) -> SessionRestoreDecision {
        let resolvedMarker: PersistedSession?
        switch marker {
        case .none: resolvedMarker = self.marker
        case .some(let value): resolvedMarker = value
        }
        return SessionRestore.decide(marker: resolvedMarker, persistence: persistence, nowMs: now)
    }

    // MARK: - Decision table

    func testNoMarkerIsLoggedOutWithNoCachedUsername() {
        XCTAssertEqual(decide(FakePersistence(), marker: .some(nil)), .anon(cachedUsername: nil))
    }

    func testEverythingPresentRestoresTheFullSession() {
        let persistence = FakePersistence()
        let expected = IdentitySecrets(
            accountId: accountId,
            exportKey: Data(repeating: 0xE7, count: 32),
            accountIdentitySeed: Data(repeating: 0x5A, count: 64),
            sessionToken: "tok-live",
            sessionExpiresAtMs: 2_000_000
        )
        XCTAssertEqual(
            decide(persistence),
            .session(
                account: SessionAccount(accountIdB64: accountIdB64, username: "alice"),
                secrets: expected
            )
        )
    }

    func testMissingExportKeyStillRestoresWithAnEmptyExportKey() {
        // The no-password paths (TOTP / device restore) already run sessions
        // with an empty export key, so it is not a required input.
        let persistence = FakePersistence()
        persistence.export = nil
        guard case .session(_, let secrets) = decide(persistence) else {
            return XCTFail("a missing export key must not block the restore")
        }
        XCTAssertEqual(secrets.exportKey, Data())
    }

    func testMalformedAccountIdIsLoggedOut() {
        let bad = PersistedSession(accountIdB64: "not base64 !!", username: "alice")
        XCTAssertEqual(decide(FakePersistence(), marker: .some(bad)), .anon(cachedUsername: "alice"))
        let short = PersistedSession(accountIdB64: base64Encode(Data([1, 2, 3])), username: "alice")
        XCTAssertEqual(decide(FakePersistence(), marker: .some(short)), .anon(cachedUsername: "alice"))
    }

    func testMissingDeviceKeyIsLoggedOut() {
        let persistence = FakePersistence()
        persistence.deviceKey = false
        XCTAssertEqual(decide(persistence), .anon(cachedUsername: "alice"))
    }

    func testMissingSeedIsLoggedOutNotAHalfSession() {
        // The pre-fix outcome for exactly this state was `.session` with nil
        // secrets. The seed is the identity; without it there is no session.
        let persistence = FakePersistence()
        persistence.seed = nil
        XCTAssertEqual(decide(persistence), .anon(cachedUsername: "alice"))
    }

    func testWrongLengthSeedIsLoggedOut() {
        let persistence = FakePersistence()
        persistence.seed = Data(repeating: 0x5A, count: 32)
        XCTAssertEqual(decide(persistence), .anon(cachedUsername: "alice"))
    }

    func testMissingTokenIsLoggedOut() {
        // Also the post-logout state: logout deletes the token, so a relaunch
        // must not resurrect the session.
        let persistence = FakePersistence()
        persistence.token = nil
        XCTAssertEqual(decide(persistence), .anon(cachedUsername: "alice"))
    }

    func testExpiredTokenIsLoggedOutAndDiscarded() {
        let persistence = FakePersistence()
        persistence.token = PersistedSessionToken(sessionToken: "tok-old", sessionExpiresAtMs: now - 1)
        XCTAssertEqual(decide(persistence), .anonDiscardingToken(cachedUsername: "alice"))
    }

    func testTokenExpiringExactlyNowIsExpired() {
        let persistence = FakePersistence()
        persistence.token = PersistedSessionToken(sessionToken: "tok-edge", sessionExpiresAtMs: now)
        XCTAssertEqual(decide(persistence), .anonDiscardingToken(cachedUsername: "alice"))
    }

    func testEmptyTokenIsLoggedOutAndDiscarded() {
        let persistence = FakePersistence()
        persistence.token = PersistedSessionToken(sessionToken: "", sessionExpiresAtMs: now + 10_000)
        XCTAssertEqual(decide(persistence), .anonDiscardingToken(cachedUsername: "alice"))
    }

    // MARK: - Applied by AuthRepository at launch

    private func makeSessionStore(with marker: PersistedSession?) -> SessionStore {
        let defaults = UserDefaults(suiteName: "session-restore-\(UUID().uuidString)")!
        let store = SessionStore(defaults: defaults)
        if let marker { store.save(marker) }
        return store
    }

    func testLaunchWithFullMaterialLandsStateAndSecretsTogether() {
        let persistence = FakePersistence()
        let repo = AuthRepository(
            sessionStore: makeSessionStore(with: marker),
            persistence: persistence,
            nowMs: { self.now }
        )
        XCTAssertEqual(repo.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "alice")))
        XCTAssertEqual(repo.identitySecrets?.accountIdentitySeed, Data(repeating: 0x5A, count: 64))
        XCTAssertEqual(repo.identitySecrets?.sessionToken, "tok-live")
        XCTAssertEqual(repo.identitySecrets?.accountId, accountId)
        XCTAssertTrue(persistence.deletedTokens.isEmpty)
    }

    func testLaunchWithMarkerAndDeviceKeyButNoTokenIsLoggedOut() {
        // The exact on-device state that used to produce the dead session.
        let persistence = FakePersistence()
        persistence.token = nil
        let repo = AuthRepository(
            sessionStore: makeSessionStore(with: marker),
            persistence: persistence,
            nowMs: { self.now }
        )
        XCTAssertEqual(repo.state, .anon(cachedUsername: "alice"))
        XCTAssertNil(repo.identitySecrets)
    }

    func testLaunchWithExpiredTokenIsLoggedOutAndDeletesIt() {
        let persistence = FakePersistence()
        persistence.token = PersistedSessionToken(sessionToken: "tok-old", sessionExpiresAtMs: now - 1)
        let repo = AuthRepository(
            sessionStore: makeSessionStore(with: marker),
            persistence: persistence,
            nowMs: { self.now }
        )
        XCTAssertEqual(repo.state, .anon(cachedUsername: "alice"))
        XCTAssertNil(repo.identitySecrets)
        XCTAssertEqual(persistence.deletedTokens, [accountIdB64])
    }

    func testPersistedTokenRoundTripsThroughItsWireShape() throws {
        let token = PersistedSessionToken(sessionToken: "v1.abc.def", sessionExpiresAtMs: 1_766_000_000_000)
        let data = try JSONEncoder().encode(token)
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(Set(json.keys), ["token", "expires_at_ms"])
        XCTAssertEqual(try JSONDecoder().decode(PersistedSessionToken.self, from: data), token)
    }
}
