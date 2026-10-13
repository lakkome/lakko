// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0073, amended 2026-10-06 — the fresh-auth proof: the holder, the wire
/// fields, the refusals, the reset warning screen's decision (UX 17.2) and
/// the password-change gate. The flows around them are in
/// `RecoveryFlowTests`; the shared block is in `FreshAuthBlockTests`.
final class FreshAuthTests: XCTestCase {

    private let grant = FreshAuthGrant(grant: "g1", expiresAtMs: 1_000)

    // MARK: - The holder

    func testAnEmptyHolderHoldsNothing() {
        var holder = FreshAuthHolder()
        XCTAssertNil(holder.live(accountIdB64: "a", nowMs: 0))
        XCTAssertFalse(holder.holdsLive(accountIdB64: "a", nowMs: 0))
    }

    func testAGrantIsLiveForItsAccountUntilItsExpiry() {
        var holder = FreshAuthHolder()
        holder.put(grant, accountIdB64: "a")
        XCTAssertTrue(holder.holdsLive(accountIdB64: "a", nowMs: 999))
        XCTAssertEqual(holder.live(accountIdB64: "a", nowMs: 999), "g1")
        XCTAssertEqual(holder.live(accountIdB64: "a", nowMs: 999), "g1", "one grant serves both calls of a reset")
    }

    func testAGrantIsNotLiveAtOrPastItsExpiryAndIsDroppedByTheRead() {
        var holder = FreshAuthHolder()
        holder.put(grant, accountIdB64: "a")
        XCTAssertFalse(holder.holdsLive(accountIdB64: "a", nowMs: 1_000))
        XCTAssertNil(holder.live(accountIdB64: "a", nowMs: 1_000))
        XCTAssertNil(holder.live(accountIdB64: "a", nowMs: 0), "dropped, not merely hidden")
    }

    func testAGrantIsBoundToItsAccount() {
        var holder = FreshAuthHolder()
        holder.put(grant, accountIdB64: "a")
        XCTAssertNil(holder.live(accountIdB64: "b", nowMs: 0))
        XCTAssertFalse(holder.holdsLive(accountIdB64: "b", nowMs: 0))
        XCTAssertEqual(holder.live(accountIdB64: "a", nowMs: 0), "g1", "the other account's read drops nothing")
    }

    func testClearSpendsTheGrant() {
        var holder = FreshAuthHolder()
        holder.put(grant, accountIdB64: "a")
        holder.clear()
        XCTAssertNil(holder.live(accountIdB64: "a", nowMs: 0))
    }

    func testANewGrantReplacesTheHeldOne() {
        var holder = FreshAuthHolder()
        holder.put(grant, accountIdB64: "a")
        holder.put(FreshAuthGrant(grant: "g2", expiresAtMs: 5_000), accountIdB64: "b")
        XCTAssertNil(holder.live(accountIdB64: "a", nowMs: 0))
        XCTAssertEqual(holder.live(accountIdB64: "b", nowMs: 0), "g2")
    }

    func testASignInWithoutAGrantKeepsTheSameAccountsGrantAndDropsAnotherAccounts() {
        var holder = FreshAuthHolder()
        holder.put(grant, accountIdB64: "a")
        holder.put(nil, accountIdB64: "a")
        XCTAssertEqual(holder.live(accountIdB64: "a", nowMs: 0), "g1")
        holder.put(nil, accountIdB64: "b")
        XCTAssertNil(holder.live(accountIdB64: "a", nowMs: 0))
    }

    // MARK: - The wire

    func testThePasswordLoginFinishCarriesTheGrant() throws {
        let json = """
        {"ok":true,"account_id":"AAAA","session_token":"t","session_expires_at_ms":5,"refresh_token":"r","refresh_expires_at_ms":6,"key_generation":1,"fresh_grant":"opaque.grant","fresh_grant_expires_at_ms":777}
        """
        let resp = try JSONDecoder().decode(LoginFinishResponse.self, from: Data(json.utf8))
        XCTAssertEqual(
            FreshAuthGrant.from(grant: resp.freshGrant, expiresAtMs: resp.freshGrantExpiresAtMs),
            FreshAuthGrant(grant: "opaque.grant", expiresAtMs: 777))
    }

    func testAFinishWithoutTheFieldsDecodesAndCarriesNoGrant() throws {
        let json = """
        {"ok":true,"account_id":"AAAA","session_token":"t","session_expires_at_ms":5,"refresh_token":"r","refresh_expires_at_ms":6,"key_generation":1}
        """
        let resp = try JSONDecoder().decode(LoginFinishResponse.self, from: Data(json.utf8))
        XCTAssertNil(FreshAuthGrant.from(grant: resp.freshGrant, expiresAtMs: resp.freshGrantExpiresAtMs))
    }

    func testHalfAGrantIsNoGrant() {
        XCTAssertNil(FreshAuthGrant.from(grant: "g", expiresAtMs: nil))
        XCTAssertNil(FreshAuthGrant.from(grant: nil, expiresAtMs: 5))
        XCTAssertNil(FreshAuthGrant.from(grant: "", expiresAtMs: 5))
    }

    func testBothResetCallsSendTheGrantInTheBody() throws {
        let start = try JSONSerialization.jsonObject(
            with: JSONEncoder().encode(KeyResetStartRequest(freshGrant: "g1", recoveryRequest: "req"))
        ) as? [String: String]
        XCTAssertEqual(start, ["fresh_grant": "g1", "recovery_request": "req"])
        let finish = try JSONSerialization.jsonObject(
            with: JSONEncoder().encode(
                KeyResetFinishRequest(freshGrant: "g1", recoveryUpload: "up", newSignaturePubkeyB64: "pk"))
        ) as? [String: String]
        XCTAssertEqual(finish, ["fresh_grant": "g1", "recovery_upload": "up", "new_signature_pubkey_b64": "pk"])
    }

    // MARK: - The refusals

    func testA403PasswordRequiredIsRead() {
        XCTAssertTrue(FreshAuthRefusal.isRequired(AuthClientError.http(
            status: 403, endpoint: "/account/keys/reset/start", body: #"{"error":"fresh-auth-required"}"#)))
        XCTAssertTrue(FreshAuthRefusal.isRequired(AuthClientError.http(
            status: 403, endpoint: "/account/keys/reset/finish", body: #"{"error":"fresh-auth-required"}"#)))
    }

    func testOtherAnswersAreNotPasswordRequired() {
        XCTAssertFalse(FreshAuthRefusal.isRequired(AuthClientError.http(
            status: 403, endpoint: "/account/keys/reset/start", body: #"{"error":"something-else"}"#)))
        XCTAssertFalse(FreshAuthRefusal.isRequired(AuthClientError.http(
            status: 403, endpoint: "/account/keys/reset/start", body: "")))
        XCTAssertFalse(FreshAuthRefusal.isRequired(AuthClientError.http(
            status: 401, endpoint: "/account/keys/reset/start", body: #"{"error":"fresh-auth-required"}"#)))
        XCTAssertFalse(FreshAuthRefusal.isRequired(AuthClientError.network(
            endpoint: "/account/keys/reset/start", underlying: URLError(.timedOut))))
        XCTAssertFalse(FreshAuthRefusal.isRequired(FreshAuthWrongPassword()))
    }

    func testTheLogins401IsAWrongPassword() {
        XCTAssertTrue(FreshAuthRefusal.isWrongPassword(
            AuthClientError.http(status: 401, endpoint: "/login/finish", body: "")))
        XCTAssertFalse(FreshAuthRefusal.isWrongPassword(AuthClientError.http(
            status: 429, endpoint: "/login/start", body: #"{"error":"too-many-attempts","retry_after_secs":9}"#)))
        XCTAssertFalse(FreshAuthRefusal.isWrongPassword(
            AuthClientError.network(endpoint: "/login/start", underlying: URLError(.timedOut))))
    }

    func testTheRefusalIsReadFromThePasskeyClientToo() {
        XCTAssertTrue(FreshAuthRefusal.isRequired(WebauthnClientError.http(
            status: 403, endpoint: "/account", body: #"{"error":"fresh-auth-required"}"#)))
        XCTAssertFalse(FreshAuthRefusal.isRequired(WebauthnClientError.http(
            status: 403, endpoint: "/account", body: #"{"error":"action-grant-required"}"#)))
        XCTAssertTrue(FreshAuthRefusal.isRequired(FreshAuthRequired()))
    }

    func testOfflineIsReadFromBothClients() {
        XCTAssertTrue(FreshAuthRefusal.isOffline(
            AuthClientError.network(endpoint: "/login/start", underlying: URLError(.notConnectedToInternet))))
        XCTAssertTrue(FreshAuthRefusal.isOffline(
            WebauthnClientError.network(endpoint: "/account", underlying: URLError(.notConnectedToInternet))))
        XCTAssertFalse(FreshAuthRefusal.isOffline(AuthClientError.http(status: 500, endpoint: "/x", body: "")))
    }

    // MARK: - The other wire shapes

    func testTheSignupFinishCarriesTheGrant() throws {
        let json = """
        {"ok":true,"session_token":"t","session_expires_at_ms":5,"refresh_token":"r","refresh_expires_at_ms":6,"key_generation":1,"fresh_grant":"g.signup","fresh_grant_expires_at_ms":900}
        """
        let resp = try JSONDecoder().decode(SignupFinishResponse.self, from: Data(json.utf8))
        XCTAssertEqual(
            FreshAuthGrant.from(grant: resp.freshGrant, expiresAtMs: resp.freshGrantExpiresAtMs),
            FreshAuthGrant(grant: "g.signup", expiresAtMs: 900))
    }

    func testThePasskeySignInFinishCarriesTheGrant() throws {
        let json = """
        {"account_id_b64":"AAAA","session_token":"t","session_expires_at_ms":5,"refresh_token":"r","refresh_expires_at_ms":6,"key_generation":1,"fresh_grant":"g.passkey","fresh_grant_expires_at_ms":901}
        """
        let resp = try JSONDecoder().decode(AuthFinishResponse.self, from: Data(json.utf8))
        XCTAssertEqual(resp.fresh_grant, "g.passkey")
        XCTAssertEqual(resp.fresh_grant_expires_at_ms, 901)
        let bare = """
        {"account_id_b64":"AAAA","session_token":"t","session_expires_at_ms":5,"refresh_token":"r","refresh_expires_at_ms":6,"key_generation":1}
        """
        XCTAssertNil(try JSONDecoder().decode(AuthFinishResponse.self, from: Data(bare.utf8)).fresh_grant)
    }

    func testTheGrantMintAndBothRegisterCallsSendTheGrantInTheBody() throws {
        let start = try JSONSerialization.jsonObject(
            with: JSONEncoder().encode(DeviceAuthorizeStartRequest(freshGrant: "g1"))) as? [String: String]
        XCTAssertEqual(start, ["fresh_grant": "g1"])
        let register = try JSONSerialization.jsonObject(
            with: JSONEncoder().encode(RegisterStartRequest(user_name: "u", nickname: "n", fresh_grant: "g1"))
        ) as? [String: String]
        XCTAssertEqual(register, ["user_name": "u", "nickname": "n", "fresh_grant": "g1"])
        XCTAssertEqual(WebauthnClient.freshGrantHeader, "x-lakko-fresh-grant")
    }

    // MARK: - The reset warning screen (UX 17.2)

    private func form(
        live: Bool = false, landing: SessionLanding? = .password, passkey: Bool = false,
        knows: Bool = false, noPassword: Bool = false
    ) -> KeyResetProofGate.Form {
        KeyResetProofGate.form(
            holdsLiveProof: live, landing: landing, hasUsablePasskey: passkey,
            choseKnowsPassword: knows, choseNoPassword: noPassword)
    }

    func testALiveProofShowsNoBlockHoweverTheSessionWasLanded() {
        for landing in [SessionLanding.password, .passkey, .authenticator, .other] {
            XCTAssertEqual(form(live: true, landing: landing), .noBlock)
        }
        XCTAssertEqual(form(live: true, landing: nil), .noBlock)
    }

    func testWithoutAProofTheBlockIsShown() {
        XCTAssertEqual(form(landing: .password), .block)
        XCTAssertEqual(form(landing: .passkey), .block)
        XCTAssertEqual(form(landing: .other), .block)
        XCTAssertEqual(form(landing: nil), .block, "a relaunch restore knows no landing")
    }

    func testAnAuthenticatorSignInWithoutAPasskeyOpensOnThePanel() {
        XCTAssertEqual(form(landing: .authenticator), .cannotReset)
        XCTAssertEqual(form(landing: .authenticator, knows: true), .block, "I know my password")
    }

    func testAUsablePasskeyAlwaysGetsTheBlock() {
        XCTAssertEqual(form(landing: .authenticator, passkey: true), .block)
        XCTAssertEqual(form(landing: .other, passkey: true, noPassword: true), .block)
    }

    func testIDoNotKnowMyPasswordSwitchesToThePanel() {
        XCTAssertEqual(form(landing: .password, noPassword: true), .cannotReset)
        XCTAssertEqual(form(landing: .other, noPassword: true), .cannotReset)
    }

    func testTheNoPasswordLinkIsOnlyUnderABlockWithoutAPasskey() {
        XCTAssertTrue(KeyResetProofGate.showsNoPasswordLink(form: .block, hasUsablePasskey: false))
        XCTAssertFalse(KeyResetProofGate.showsNoPasswordLink(form: .block, hasUsablePasskey: true))
        XCTAssertFalse(KeyResetProofGate.showsNoPasswordLink(form: .noBlock, hasUsablePasskey: false))
        XCTAssertFalse(KeyResetProofGate.showsNoPasswordLink(form: .cannotReset, hasUsablePasskey: false))
    }

    func testResetNeedsTheWordAndWhatTheBlockNeeds() {
        XCTAssertTrue(KeyResetProofGate.canSubmit(form: .noBlock, blockReady: false, confirmation: "RESET"))
        XCTAssertFalse(KeyResetProofGate.canSubmit(form: .noBlock, blockReady: true, confirmation: "reset"))
        XCTAssertFalse(KeyResetProofGate.canSubmit(form: .block, blockReady: false, confirmation: "RESET"))
        XCTAssertTrue(KeyResetProofGate.canSubmit(form: .block, blockReady: true, confirmation: "RESET"))
        XCTAssertFalse(KeyResetProofGate.canSubmit(form: .block, blockReady: true, confirmation: ""))
        XCTAssertFalse(
            KeyResetProofGate.canSubmit(form: .cannotReset, blockReady: true, confirmation: "RESET"),
            "there is no reset button in the panel")
    }

    // MARK: - The password-change gate

    func testAPasswordChangeNeedsAProofOrTheThreeWords() {
        XCTAssertFalse(PasswordChangeGate.allowed(holdsLiveProof: false, threeWordsPassedAtMs: nil, nowMs: 1_000))
        XCTAssertTrue(PasswordChangeGate.allowed(holdsLiveProof: true, threeWordsPassedAtMs: nil, nowMs: 1_000))
        XCTAssertTrue(PasswordChangeGate.allowed(holdsLiveProof: false, threeWordsPassedAtMs: 500, nowMs: 1_000))
    }

    func testTheThreeWordsOpenTheGateForTenMinutes() {
        let passed: Int64 = 1_000
        XCTAssertTrue(PasswordChangeGate.threeWordsLive(passedAtMs: passed, nowMs: passed))
        XCTAssertTrue(PasswordChangeGate.threeWordsLive(passedAtMs: passed, nowMs: passed + 599_999))
        XCTAssertFalse(PasswordChangeGate.threeWordsLive(passedAtMs: passed, nowMs: passed + 600_000))
        XCTAssertFalse(PasswordChangeGate.threeWordsLive(passedAtMs: passed, nowMs: passed - 1), "a clock set back")
        XCTAssertFalse(PasswordChangeGate.threeWordsLive(passedAtMs: nil, nowMs: passed))
    }
}
