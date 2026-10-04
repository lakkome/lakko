// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0065, amended 2026-10-04 — the password-fresh reset grant: the
/// holder, the wire fields, the two refusals and the warning screen's
/// decision (UX 15.1a). The flow around them is in `RecoveryFlowTests`.
final class ResetGrantTests: XCTestCase {

    private let grant = ResetGrant(grant: "g1", expiresAtMs: 1_000)

    // MARK: - The holder

    func testAnEmptyHolderHoldsNothing() {
        var holder = ResetGrantHolder()
        XCTAssertNil(holder.live(accountIdB64: "a", nowMs: 0))
        XCTAssertFalse(holder.holdsLive(accountIdB64: "a", nowMs: 0))
    }

    func testAGrantIsLiveForItsAccountUntilItsExpiry() {
        var holder = ResetGrantHolder()
        holder.put(grant, accountIdB64: "a")
        XCTAssertTrue(holder.holdsLive(accountIdB64: "a", nowMs: 999))
        XCTAssertEqual(holder.live(accountIdB64: "a", nowMs: 999), "g1")
        XCTAssertEqual(holder.live(accountIdB64: "a", nowMs: 999), "g1", "one grant serves both calls of a reset")
    }

    func testAGrantIsNotLiveAtOrPastItsExpiryAndIsDroppedByTheRead() {
        var holder = ResetGrantHolder()
        holder.put(grant, accountIdB64: "a")
        XCTAssertFalse(holder.holdsLive(accountIdB64: "a", nowMs: 1_000))
        XCTAssertNil(holder.live(accountIdB64: "a", nowMs: 1_000))
        XCTAssertNil(holder.live(accountIdB64: "a", nowMs: 0), "dropped, not merely hidden")
    }

    func testAGrantIsBoundToItsAccount() {
        var holder = ResetGrantHolder()
        holder.put(grant, accountIdB64: "a")
        XCTAssertNil(holder.live(accountIdB64: "b", nowMs: 0))
        XCTAssertFalse(holder.holdsLive(accountIdB64: "b", nowMs: 0))
        XCTAssertEqual(holder.live(accountIdB64: "a", nowMs: 0), "g1", "the other account's read drops nothing")
    }

    func testClearSpendsTheGrant() {
        var holder = ResetGrantHolder()
        holder.put(grant, accountIdB64: "a")
        holder.clear()
        XCTAssertNil(holder.live(accountIdB64: "a", nowMs: 0))
    }

    func testANewGrantReplacesTheHeldOne() {
        var holder = ResetGrantHolder()
        holder.put(grant, accountIdB64: "a")
        holder.put(ResetGrant(grant: "g2", expiresAtMs: 5_000), accountIdB64: "b")
        XCTAssertNil(holder.live(accountIdB64: "a", nowMs: 0))
        XCTAssertEqual(holder.live(accountIdB64: "b", nowMs: 0), "g2")
    }

    func testASignInWithoutAGrantKeepsTheSameAccountsGrantAndDropsAnotherAccounts() {
        var holder = ResetGrantHolder()
        holder.put(grant, accountIdB64: "a")
        holder.put(nil, accountIdB64: "a")
        XCTAssertEqual(holder.live(accountIdB64: "a", nowMs: 0), "g1")
        holder.put(nil, accountIdB64: "b")
        XCTAssertNil(holder.live(accountIdB64: "a", nowMs: 0))
    }

    // MARK: - The wire

    func testThePasswordLoginFinishCarriesTheGrant() throws {
        let json = """
        {"ok":true,"account_id":"AAAA","session_token":"t","session_expires_at_ms":5,"refresh_token":"r","refresh_expires_at_ms":6,"key_generation":1,"reset_grant":"opaque.grant","reset_grant_expires_at_ms":777}
        """
        let resp = try JSONDecoder().decode(LoginFinishResponse.self, from: Data(json.utf8))
        XCTAssertEqual(
            ResetGrant.from(grant: resp.resetGrant, expiresAtMs: resp.resetGrantExpiresAtMs),
            ResetGrant(grant: "opaque.grant", expiresAtMs: 777))
    }

    func testAFinishWithoutTheFieldsDecodesAndCarriesNoGrant() throws {
        let json = """
        {"ok":true,"account_id":"AAAA","session_token":"t","session_expires_at_ms":5,"refresh_token":"r","refresh_expires_at_ms":6,"key_generation":1}
        """
        let resp = try JSONDecoder().decode(LoginFinishResponse.self, from: Data(json.utf8))
        XCTAssertNil(ResetGrant.from(grant: resp.resetGrant, expiresAtMs: resp.resetGrantExpiresAtMs))
    }

    func testHalfAGrantIsNoGrant() {
        XCTAssertNil(ResetGrant.from(grant: "g", expiresAtMs: nil))
        XCTAssertNil(ResetGrant.from(grant: nil, expiresAtMs: 5))
        XCTAssertNil(ResetGrant.from(grant: "", expiresAtMs: 5))
    }

    func testBothResetCallsSendTheGrantInTheBody() throws {
        let start = try JSONSerialization.jsonObject(
            with: JSONEncoder().encode(KeyResetStartRequest(resetGrant: "g1", recoveryRequest: "req"))
        ) as? [String: String]
        XCTAssertEqual(start, ["reset_grant": "g1", "recovery_request": "req"])
        let finish = try JSONSerialization.jsonObject(
            with: JSONEncoder().encode(
                KeyResetFinishRequest(resetGrant: "g1", recoveryUpload: "up", newSignaturePubkeyB64: "pk"))
        ) as? [String: String]
        XCTAssertEqual(finish, ["reset_grant": "g1", "recovery_upload": "up", "new_signature_pubkey_b64": "pk"])
    }

    // MARK: - The refusals

    func testA403PasswordRequiredIsRead() {
        XCTAssertTrue(KeyResetRefusal.isPasswordRequired(AuthClientError.http(
            status: 403, endpoint: "/account/keys/reset/start", body: #"{"error":"password-required"}"#)))
        XCTAssertTrue(KeyResetRefusal.isPasswordRequired(AuthClientError.http(
            status: 403, endpoint: "/account/keys/reset/finish", body: #"{"error":"password-required"}"#)))
    }

    func testOtherAnswersAreNotPasswordRequired() {
        XCTAssertFalse(KeyResetRefusal.isPasswordRequired(AuthClientError.http(
            status: 403, endpoint: "/account/keys/reset/start", body: #"{"error":"something-else"}"#)))
        XCTAssertFalse(KeyResetRefusal.isPasswordRequired(AuthClientError.http(
            status: 403, endpoint: "/account/keys/reset/start", body: "")))
        XCTAssertFalse(KeyResetRefusal.isPasswordRequired(AuthClientError.http(
            status: 401, endpoint: "/account/keys/reset/start", body: #"{"error":"password-required"}"#)))
        XCTAssertFalse(KeyResetRefusal.isPasswordRequired(AuthClientError.network(
            endpoint: "/account/keys/reset/start", underlying: URLError(.timedOut))))
        XCTAssertFalse(KeyResetRefusal.isPasswordRequired(KeyResetWrongPassword()))
    }

    func testTheLogins401IsAWrongPassword() {
        XCTAssertTrue(KeyResetRefusal.isWrongPassword(
            AuthClientError.http(status: 401, endpoint: "/login/finish", body: "")))
        XCTAssertFalse(KeyResetRefusal.isWrongPassword(AuthClientError.http(
            status: 429, endpoint: "/login/start", body: #"{"error":"too-many-attempts","retry_after_secs":9}"#)))
        XCTAssertFalse(KeyResetRefusal.isWrongPassword(
            AuthClientError.network(endpoint: "/login/start", underlying: URLError(.timedOut))))
    }

    // MARK: - The warning screen (UX 15.1a)

    func testALiveGrantShowsNoPasswordField() {
        XCTAssertFalse(KeyResetPasswordGate.showsPasswordField(holdsLiveGrant: true, revealed: false))
    }

    func testNoLiveGrantShowsThePasswordField() {
        XCTAssertTrue(KeyResetPasswordGate.showsPasswordField(holdsLiveGrant: false, revealed: false))
    }

    func testARefusedResetRevealsTheField() {
        XCTAssertTrue(KeyResetPasswordGate.showsPasswordField(holdsLiveGrant: true, revealed: true))
    }

    func testWithoutTheFieldTheWordAloneEnablesTheButton() {
        XCTAssertTrue(KeyResetPasswordGate.canSubmit(showsPasswordField: false, password: "", confirmation: "RESET"))
        XCTAssertFalse(KeyResetPasswordGate.canSubmit(showsPasswordField: false, password: "", confirmation: "reset"))
        XCTAssertFalse(KeyResetPasswordGate.canSubmit(showsPasswordField: false, password: "pw", confirmation: ""))
    }

    func testWithTheFieldBothThePasswordAndTheWordAreNeeded() {
        XCTAssertFalse(KeyResetPasswordGate.canSubmit(showsPasswordField: true, password: "", confirmation: "RESET"))
        XCTAssertFalse(KeyResetPasswordGate.canSubmit(showsPasswordField: true, password: "pw", confirmation: ""))
        XCTAssertTrue(KeyResetPasswordGate.canSubmit(showsPasswordField: true, password: "pw", confirmation: "RESET"))
    }

    func testTheUsernameFieldIsShownOnlyWithThePasswordFieldAndOnlyWhenTheNameIsUnknown() {
        XCTAssertTrue(KeyResetPasswordGate.showsUsernameField(showsPasswordField: true, usernameKnown: false))
        XCTAssertFalse(KeyResetPasswordGate.showsUsernameField(showsPasswordField: true, usernameKnown: true))
        XCTAssertFalse(
            KeyResetPasswordGate.showsUsernameField(showsPasswordField: false, usernameKnown: false),
            "a live grant needs no sign-in, so no name")
        XCTAssertFalse(KeyResetPasswordGate.showsUsernameField(showsPasswordField: false, usernameKnown: true))
    }

    func testWithTheUsernameFieldTheButtonNeedsTheNameToo() {
        func can(_ username: String, password: String = "pw", word: String = "RESET") -> Bool {
            KeyResetPasswordGate.canSubmit(
                showsPasswordField: true, showsUsernameField: true,
                password: password, username: username, confirmation: word)
        }
        XCTAssertFalse(can(""))
        XCTAssertFalse(can("   "), "whitespace is not a name")
        XCTAssertTrue(can("alice"))
        XCTAssertFalse(can("alice", password: ""))
        XCTAssertFalse(can("alice", word: ""))
    }

    func testWithoutTheUsernameFieldATypedNameIsNotNeeded() {
        XCTAssertTrue(KeyResetPasswordGate.canSubmit(
            showsPasswordField: true, showsUsernameField: false,
            password: "pw", username: "", confirmation: "RESET"))
    }
}
