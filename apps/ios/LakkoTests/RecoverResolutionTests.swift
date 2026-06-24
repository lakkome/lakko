// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// Locks the cross-device recover account-resolution behaviour (G-29).
///
/// Mirror of `apps/web/src/auth/recover-resolution.test.ts` — the six
/// cases assert the same branch logic the inlined block in
/// ``AuthRepository/recover(mnemonicPhrase:newPassword:username:)`` used
/// to carry: a local cache hit wins on an empty or matching username, a
/// fresh-device lookup binds the typed username, an unregistered username
/// collapses into the bad-phrase error (no probing), and neither-cache-
/// nor-username raises the fresh-device prompt.
final class RecoverResolutionTests: XCTestCase {

    /// Deterministic fake encoder so the test can assert the looked-up
    /// bytes were passed through it.
    private let fakeEncode: (Data) -> String = { data in
        "enc:" + data.map { String($0) }.joined(separator: ",")
    }

    /// Closure-based fake lookup that records whether it was invoked +
    /// the username it received.
    private final class FakeLookup {
        private(set) var wasCalled = false
        private(set) var receivedUsername: String?
        private let result: RecoverAccountLookup

        init(result: RecoverAccountLookup) {
            self.result = result
        }

        func lookup(_ username: String) async throws -> RecoverAccountLookup {
            wasCalled = true
            receivedUsername = username
            return result
        }
    }

    // (a) cache hit + empty username → cached, lookup NOT called.
    func testUsesCachedAccountWhenTypedUsernameEmptyNoLookup() async throws {
        let fake = FakeLookup(
            result: RecoverAccountLookup(account: Data(), normalisedUsername: "alice")
        )
        let resolved = try await resolveRecoverAccount(
            cached: CachedAccount(accountIdB64: "CACHED", username: "alice"),
            username: nil,
            lookup: fake.lookup,
            encodeAccountId: fakeEncode
        )
        XCTAssertEqual(
            resolved,
            ResolvedRecoverAccount(accountIdB64: "CACHED", resolvedUsername: "alice")
        )
        XCTAssertFalse(fake.wasCalled)
    }

    // (b) cache hit + matching whitespace-padded username → cached,
    //     lookup NOT called.
    func testUsesCachedAccountWhenTypedUsernameMatchesNoLookup() async throws {
        let fake = FakeLookup(
            result: RecoverAccountLookup(account: Data(), normalisedUsername: "alice")
        )
        let resolved = try await resolveRecoverAccount(
            cached: CachedAccount(accountIdB64: "CACHED", username: "alice"),
            username: "  alice  ", // trimmed before compare
            lookup: fake.lookup,
            encodeAccountId: fakeEncode
        )
        XCTAssertEqual(
            resolved,
            ResolvedRecoverAccount(accountIdB64: "CACHED", resolvedUsername: "alice")
        )
        XCTAssertFalse(fake.wasCalled)
    }

    // (c) cached nil + username → lookup called, returns encoded
    //     looked-up bytes + normalised username.
    func testLooksUpOnFreshDeviceGivenUsername() async throws {
        let bytes = Data([1, 2, 3, 4])
        let fake = FakeLookup(
            result: RecoverAccountLookup(account: bytes, normalisedUsername: "alice")
        )
        let resolved = try await resolveRecoverAccount(
            cached: nil,
            username: "alice",
            lookup: fake.lookup,
            encodeAccountId: fakeEncode
        )
        XCTAssertTrue(fake.wasCalled)
        XCTAssertEqual(fake.receivedUsername, "alice")
        XCTAssertEqual(
            resolved,
            ResolvedRecoverAccount(accountIdB64: "enc:1,2,3,4", resolvedUsername: "alice")
        )
    }

    // (d) cached present but username differs → lookup called not cache.
    func testLooksUpWhenTypedUsernameDiffersFromCached() async throws {
        let bytes = Data([9, 9])
        let fake = FakeLookup(
            result: RecoverAccountLookup(account: bytes, normalisedUsername: "bob")
        )
        let resolved = try await resolveRecoverAccount(
            cached: CachedAccount(accountIdB64: "CACHED", username: "alice"),
            username: "bob",
            lookup: fake.lookup,
            encodeAccountId: fakeEncode
        )
        XCTAssertTrue(fake.wasCalled)
        XCTAssertEqual(fake.receivedUsername, "bob")
        XCTAssertEqual(
            resolved,
            ResolvedRecoverAccount(accountIdB64: "enc:9,9", resolvedUsername: "bob")
        )
    }

    // (e) lookup returns account nil → throws "invalid recovery".
    func testThrowsBadPhraseCollapsedErrorWhenUsernameUnregistered() async throws {
        let fake = FakeLookup(
            result: RecoverAccountLookup(account: nil, normalisedUsername: "ghost")
        )
        do {
            _ = try await resolveRecoverAccount(
                cached: nil,
                username: "ghost",
                lookup: fake.lookup,
                encodeAccountId: fakeEncode
            )
            XCTFail("expected resolveRecoverAccount to throw")
        } catch let AuthClientError.argument(message) {
            XCTAssertEqual(message, "invalid recovery")
        }
        XCTAssertTrue(fake.wasCalled)
    }

    // (f) cached nil + whitespace-only username → throws + lookup
    //     NOT called.
    func testThrowsFreshDevicePromptWhenNeitherCacheNorUsername() async throws {
        let fake = FakeLookup(
            result: RecoverAccountLookup(account: Data(), normalisedUsername: "")
        )
        do {
            _ = try await resolveRecoverAccount(
                cached: nil,
                username: "   ", // whitespace-only → empty after trim
                lookup: fake.lookup,
                encodeAccountId: fakeEncode
            )
            XCTFail("expected resolveRecoverAccount to throw")
        } catch let AuthClientError.argument(message) {
            XCTAssertTrue(
                message.contains("no account on this device"),
                "unexpected message: \(message)"
            )
        }
        XCTAssertFalse(fake.wasCalled)
    }
}
