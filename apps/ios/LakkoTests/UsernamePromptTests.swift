// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// The pure rules of the username prompt (a sign-in that named no username)
/// and the DeviceGroup rejoin picking up a username learned meanwhile. The
/// repository flows are in `RecoveryFlowTests` (its scripted auth service).
@MainActor
final class UsernamePromptTests: XCTestCase {

    func testOnlyAnEmptyUsernameOfANonGuestIsLearned() {
        XCTAssertTrue(UsernamePrompt.shouldLearn(username: "", isGuest: false))
        XCTAssertTrue(UsernamePrompt.shouldLearn(username: "  ", isGuest: false))
        XCTAssertFalse(UsernamePrompt.shouldLearn(username: "", isGuest: true), "a guest has no name by design")
        XCTAssertFalse(UsernamePrompt.shouldLearn(username: "alice", isGuest: false))
    }

    func testBackupCandidatesTakeTheProfileFirstAndDropEmptyAndRepeatedNames() {
        XCTAssertEqual(UsernamePrompt.backupCandidates(profileUsername: "bob", register: "alice"), ["bob", "alice"])
        XCTAssertEqual(UsernamePrompt.backupCandidates(profileUsername: "alice", register: "alice"), ["alice"])
        XCTAssertEqual(UsernamePrompt.backupCandidates(profileUsername: nil, register: "alice"), ["alice"])
        XCTAssertEqual(UsernamePrompt.backupCandidates(profileUsername: " ", register: nil), [])
        XCTAssertEqual(UsernamePrompt.backupCandidates(profileUsername: nil, register: nil), [])
    }

    func testAuthsAnswerAndErrorsMapToTheOutcomes() {
        XCTAssertEqual(UsernamePrompt.outcome(of: .same, canonical: "alice"), .adopted("alice"))
        XCTAssertEqual(UsernamePrompt.outcome(of: .other, canonical: "alice"), .notThisAccount)
        XCTAssertEqual(UsernamePrompt.outcome(of: .none, canonical: "alice"), .accountHasNone)
        XCTAssertEqual(
            UsernamePrompt.outcome(of: AuthClientError.http(status: 429, endpoint: "/auth/login-handle/check", body: "")),
            .tooMany)
        XCTAssertEqual(
            UsernamePrompt.outcome(of: AuthClientError.network(
                endpoint: "/auth/login-handle/check", underlying: URLError(.notConnectedToInternet))),
            .network)
        XCTAssertEqual(
            UsernamePrompt.outcome(of: AuthClientError.http(status: 503, endpoint: "/auth/login-handle/check", body: "")),
            .network, "any other refusal reads as 'could not check'")
    }

    func testWhichOutcomesCloseThePromptAndWhatEachShows() {
        XCTAssertTrue(UsernamePrompt.closesPrompt(.adopted("alice")))
        XCTAssertTrue(UsernamePrompt.closesPrompt(.accountHasNone))
        for kept: UsernamePrompt.Outcome in [.notThisAccount, .tooMany, .network, .rule(.tooShort), .empty] {
            XCTAssertFalse(UsernamePrompt.closesPrompt(kept), "\(kept)")
        }
        XCTAssertEqual(UsernamePrompt.errorKey(.notThisAccount), "auth.username_prompt.error.not_this")
        XCTAssertEqual(UsernamePrompt.errorKey(.tooMany), "auth.username_prompt.error.too_many")
        XCTAssertEqual(UsernamePrompt.errorKey(.network), "auth.username_prompt.error.network")
        XCTAssertNil(UsernamePrompt.errorKey(.rule(.invalidCharacters)), "the username rule's own copy")
        XCTAssertNil(UsernamePrompt.errorKey(.adopted("alice")))
        XCTAssertNil(UsernamePrompt.errorKey(.accountHasNone))
    }

    func testThePromptCopyIsInTheCatalog() {
        for key in [
            "auth.username_prompt.title", "auth.username_prompt.lede", "auth.username_prompt.field",
            "auth.username_prompt.continue", "auth.username_prompt.forgot", "auth.username_prompt.forgot_note",
            "auth.username_prompt.error.not_this", "auth.username_prompt.error.too_many",
            "auth.username_prompt.error.network",
        ] {
            XCTAssertNotEqual(Localization.text(key), key, "\(key) is missing")
        }
    }

    // MARK: - The rejoin

    /// The rejoin reads the login handle afresh on every pass: a session
    /// record that learned its username gives the handle at the next pass.
    func testTheRejoinPicksUpAUsernameTheSessionRecordLearned() throws {
        let accountIdB64 = base64Encode(Data((0..<16).map { _ in UInt8.random(in: 0...255) }))
        let repo = DeviceGroupSyncRepository()
        var record = PersistedSession(accountIdB64: accountIdB64, username: "")
        repo.loginHandleProvider = { nil }
        repo.storedSessionRecord = { record }
        XCTAssertNil(repo.currentLoginHandle(accountIdB64: accountIdB64), "no name: no handle, no knock")

        record = PersistedSession(accountIdB64: accountIdB64, username: "alice")
        XCTAssertEqual(repo.currentLoginHandle(accountIdB64: accountIdB64), try loginHandle(username: "alice"))
    }

    /// A rejoin waiting for a login handle wakes as soon as the username is
    /// learned instead of sleeping out its 30 s.
    func testALearnedUsernameWakesTheRejoinWait() async {
        let repo = DeviceGroupSyncRepository()
        let started = Date()
        let wait = Task { await repo.sleepUntilLoginHandleOrRetry() }
        try? await Task.sleep(nanoseconds: 200_000_000)
        repo.loginHandleArrived()
        await wait.value
        XCTAssertLessThan(Date().timeIntervalSince(started), 5, "woken, not the 30 s pass")
    }
}
