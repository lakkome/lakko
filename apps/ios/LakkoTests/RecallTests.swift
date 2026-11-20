// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

// MARK: - Test doubles shared with the other auth suites

/// The recall record's store, in memory: no suite touches the simulator's
/// Keychain for it unless it means to.
final class InMemoryRecallRecordStore: RecallRecordStoring {
    private(set) var record: RecallRecord?
    private(set) var puts = 0
    private(set) var clears: [String] = []
    var putError: Error?

    func load() -> RecallRecord? { record }

    func put(_ record: RecallRecord) throws {
        if let putError { throw putError }
        puts += 1
        self.record = record
    }

    func clear(accountIdB64: String) {
        clears.append(accountIdB64)
        if record?.accountIdB64 == accountIdB64 { record = nil }
    }
}

extension RecallOperations {
    /// A server that cannot be reached: the question stays on screen.
    static let unreachable = RecallOperations(
        status: { _ in
            throw AuthClientError.network(
                endpoint: "/account/recall/status", underlying: URLError(.notConnectedToInternet))
        },
        takeBack: { _ in
            throw AuthClientError.network(
                endpoint: "/account/recall/start", underlying: URLError(.notConnectedToInternet))
        }
    )
}

// MARK: - The pure rules

/// ADR-0074 part 1 (sprint T4): which wipe a sign-out runs, what the recall
/// record holds, every server answer mapped, and the "Was it you?" state
/// machine with its closed / exhausted / offline / expiry states.
@MainActor
final class RecallRulesTests: XCTestCase {

    private let day: Int64 = 24 * 60 * 60 * 1000
    private let record = RecallRecord(
        accountIdB64: base64Encode(Data((0..<16).map { UInt8($0) })),
        oldSeed: Data(repeating: 7, count: 32), username: "alice", learnedAtMs: 1_000)

    private func window(seq: UInt64 = 3, expires: Int64 = 9_000_000, changed: Int64 = 500, left: Int = 20) -> RecallWindow {
        RecallWindow(anchorSeq: seq, expiresAtMs: expires, changedAtMs: changed, attemptsRemaining: left)
    }

    private func http(_ status: Int, _ body: String) -> Error {
        AuthClientError.http(status: status, endpoint: "/account/recall", body: body)
    }

    // The staged / full / no-keys verdict.

    func testASupersededDeviceWithTheKeysRunsTheStagedWipe() {
        XCTAssertEqual(
            SignOutWipe.plan(cause: .keysReset, holdsAccountSeed: true, recallRecordExists: false), .staged)
    }

    func testASupersededDeviceThatNeverHeldTheKeysKeepsNothing() {
        XCTAssertEqual(
            SignOutWipe.plan(cause: .keysReset, holdsAccountSeed: false, recallRecordExists: false), .full)
    }

    func testAnInterruptedStagedWipeRunsAgainAsStaged() {
        // The seed is already deleted, the record is already written.
        XCTAssertEqual(
            SignOutWipe.plan(cause: .keysReset, holdsAccountSeed: false, recallRecordExists: true), .staged)
    }

    func testARemovedDeviceAlwaysRunsTheFullWipe() {
        for holds in [true, false] {
            for exists in [true, false] {
                XCTAssertEqual(
                    SignOutWipe.plan(cause: .deviceRemoved, holdsAccountSeed: holds, recallRecordExists: exists),
                    .full, "device-revoked never leaves a recall record")
            }
        }
    }

    // What the record holds.

    func testTheRecallRecordHoldsExactlyFourFields() throws {
        let json = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(record)) as? [String: Any])
        XCTAssertEqual(Set(json.keys), ["account_id", "old_seed", "username", "learned_at_ms"])
        XCTAssertEqual(try JSONDecoder().decode(RecallRecord.self, from: JSONEncoder().encode(record)), record)
        XCTAssertEqual(record.accountId?.count, 16)
        XCTAssertTrue(record.isWellFormed)
    }

    func testARecordWithAWrongSizedSeedOrAccountIsNotUsable() {
        XCTAssertFalse(RecallRecord(
            accountIdB64: record.accountIdB64, oldSeed: Data(repeating: 7, count: 64), username: "", learnedAtMs: 1)
            .isWellFormed, "the 64-byte phrase seed is never what is kept")
        XCTAssertFalse(RecallRecord(
            accountIdB64: "AAAA", oldSeed: Data(repeating: 7, count: 32), username: "", learnedAtMs: 1)
            .isWellFormed)
    }

    func testTheRecordExpiresThirtyDaysAfterTheDeviceLearned() {
        XCTAssertEqual(record.localExpiryMs, 1_000 + 30 * day)
        XCTAssertFalse(record.locallyExpired(nowMs: 1_000 + 30 * day - 1))
        XCTAssertTrue(record.locallyExpired(nowMs: 1_000 + 30 * day))
    }

    // Every server answer.

    func testEveryAnswerOfTheRecallRoutesIsMapped() {
        typealias R = RecallAnswers
        XCTAssertEqual(R.refusal(of: http(401, #"{"error":"recall-refused"}"#)), .refused)
        XCTAssertEqual(R.refusal(of: http(401, #"{"error":"wrong-password"}"#)), .wrongPassword)
        XCTAssertEqual(R.refusal(of: http(401, "")), .refused, "a bare 401 is never read as a wrong password")
        XCTAssertEqual(
            R.refusal(of: http(429, #"{"error":"too-many-attempts","retry_after_secs":840}"#)),
            .rateLimited(retryAfterSecs: 840))
        XCTAssertEqual(R.refusal(of: http(429, "")), .rateLimited(retryAfterSecs: nil))
        XCTAssertEqual(R.refusal(of: http(400, "")), .failed)
        XCTAssertEqual(R.refusal(of: http(503, #"{"error":"rate-limiter-unavailable"}"#)), .failed)
        XCTAssertEqual(
            R.refusal(of: AuthClientError.network(endpoint: "/x", underlying: URLError(.timedOut))), .offline)
        XCTAssertEqual(R.refusal(of: RecallWrongPassword()), .wrongPassword)
        XCTAssertEqual(R.refusal(of: AuthClientError.argument("x")), .failed)
    }

    func testOnlyARefusalClosesTheStatus() {
        typealias R = RecallAnswers
        XCTAssertEqual(R.status(of: http(401, #"{"error":"recall-refused"}"#)), .refused)
        XCTAssertEqual(R.status(of: http(429, "")), .unknown, "a limit is not proof the recall is over")
        XCTAssertEqual(R.status(of: http(503, "")), .unknown)
        XCTAssertEqual(
            R.status(of: AuthClientError.network(endpoint: "/x", underlying: URLError(.timedOut))), .unknown)
    }

    func testTheDeleteBlockedAnswerCarriesItsDate() {
        XCTAssertEqual(
            RecallWindowOpen.untilMs(status: 409, body: #"{"error":"recall-window-open","until_ms":1762387200000}"#),
            1_762_387_200_000)
        XCTAssertNil(RecallWindowOpen.untilMs(status: 409, body: #"{"error":"something-else","until_ms":5}"#))
        XCTAssertNil(RecallWindowOpen.untilMs(status: 403, body: #"{"error":"recall-window-open","until_ms":5}"#))
        XCTAssertNil(RecallWindowOpen.untilMs(status: 409, body: #"{"error":"recall-window-open"}"#))
        XCTAssertEqual(
            RecallWindowOpen.untilMs(of: WebauthnClientError.http(
                status: 409, endpoint: "/account", body: #"{"error":"recall-window-open","until_ms":77}"#)), 77)
        XCTAssertNil(RecallWindowOpen.untilMs(of: URLError(.timedOut)))
    }

    // The wire.

    func testTheFinishBodyOmitsTheDeviceIdWhenThereIsNone() throws {
        func body(_ deviceId: String?) throws -> [String: Any] {
            try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(RecallFinishRequest(
                sessionId: "s", accountIdB64: "a", anchorSeq: 3, loginFinish: "lf", passwordUpload: "pu",
                recoveryUpload: "ru", newSignaturePubkeyB64: "pk", deviceIdB64: deviceId,
                statementB64: "st", identityProofB64: "ip"))) as? [String: Any])
        }
        let without = try body(nil)
        XCTAssertEqual(
            Set(without.keys),
            ["session_id", "account_id_b64", "anchor_seq", "login_finish", "password_upload", "recovery_upload",
             "new_signature_pubkey_b64", "statement_b64", "identity_proof_b64"])
        XCTAssertEqual(try body("ZGV2")["device_id_b64"] as? String, "ZGV2")
        XCTAssertEqual(without["anchor_seq"] as? Int, 3)
    }

    func testTheStartBodyAndTheAnswersUseTheServersFieldNames() throws {
        let start = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(RecallStartRequest(
            accountIdB64: "a", anchorSeq: 1, statementB64: "st", loginRequest: "l", passwordRequest: "p",
            recoveryRequest: "r"))) as? [String: Any])
        XCTAssertEqual(
            Set(start.keys),
            ["account_id_b64", "anchor_seq", "statement_b64", "login_request", "password_request",
             "recovery_request"])
        let status = try JSONDecoder().decode(RecallStatusResponse.self, from: Data(
            #"{"anchor_seq":2,"expires_at_ms":900,"changed_at_ms":100,"attempts_remaining":4}"#.utf8))
        XCTAssertEqual(status.window, RecallWindow(anchorSeq: 2, expiresAtMs: 900, changedAtMs: 100, attemptsRemaining: 4))
        let started = try JSONDecoder().decode(RecallStartResponse.self, from: Data(
            #"{"session_id":"s","login_response":"l","password_response":"p","recovery_response":"r","server_nonce_b64":"n"}"#.utf8))
        XCTAssertEqual(started.serverNonceB64, "n")
        let finished = try JSONDecoder().decode(RecallFinishResponse.self, from: Data("""
            {"key_reset_attest_b64":"AA","key_generation":5,"session_token":"t","session_expires_at_ms":1,\
            "refresh_token":"r","refresh_expires_at_ms":2,"fresh_grant":"g","fresh_grant_expires_at_ms":3}
            """.utf8))
        XCTAssertEqual(finished.sessionPair.keyGeneration, 5)
        XCTAssertEqual(finished.freshGrant, "g")
    }

    // The screen's state machine.

    private var fresh: RecallScreenState { RecallScreenState(record: record) }

    private func opened(_ state: RecallScreenState? = nil, left: Int = 20) -> RecallScreenState {
        var next = state ?? fresh
        next.check = .open(window(left: left))
        return next
    }

    func testTheQuestionStartsCheckingWithNoDisabled() {
        XCTAssertEqual(fresh.check, .checking)
        XCTAssertEqual(fresh.step, .question)
        XCTAssertFalse(fresh.canTakeBack, "No is disabled until the server confirmed it")
        XCTAssertNil(fresh.untilMs, "no until-date before the server confirmed")
        XCTAssertEqual(fresh.resetAtMs, 1_000, "the device's own date until the server's is known")
        XCTAssertEqual(RecallMachine.tapNo(fresh).step, .question)
    }

    func testAnOpenStatusDrawsTheDatesAndEnablesNo() {
        let shown = RecallMachine.afterStatus(fresh, .open(window()), nowMs: 2_000)
        guard case .show(let state) = shown else { return XCTFail("\(shown)") }
        XCTAssertTrue(state.canTakeBack)
        XCTAssertEqual(state.untilMs, 9_000_000)
        XCTAssertEqual(state.resetAtMs, 500)
        XCTAssertEqual(RecallMachine.tapNo(state).step, .form)
    }

    func testARefusedStatusClosesSilentlyOnTheQuestionAndWithItsLineInTheForm() {
        XCTAssertEqual(RecallMachine.afterStatus(fresh, .refused, nowMs: 2_000), .end(.closed))
        XCTAssertNil(RecallEnd.closed.noticeKey, "nothing says why")
        var inForm = opened()
        inForm.step = .form
        XCTAssertEqual(RecallMachine.afterStatus(inForm, .refused, nowMs: 2_000), .end(.closedInForm))
        XCTAssertEqual(RecallEnd.closedInForm.noticeKey, "devices.recall.error.closed")
        var confirming = opened()
        confirming.step = .confirmYes
        XCTAssertEqual(RecallMachine.afterStatus(confirming, .refused, nowMs: 2_000), .end(.closed))
    }

    func testOfflineKeepsTheQuestionWithNoDisabledAndKeepsWhatWasConfirmed() {
        let first = RecallMachine.afterStatus(fresh, .unknown, nowMs: 2_000)
        guard case .show(let offline) = first else { return XCTFail("\(first)") }
        XCTAssertEqual(offline.check, .offline)
        XCTAssertFalse(offline.canTakeBack)
        // Re-checked when the connection returns.
        guard case .show(let back) = RecallMachine.afterStatus(offline, .open(window()), nowMs: 2_000) else {
            return XCTFail("expected the question")
        }
        XCTAssertTrue(back.canTakeBack)
        // A later failed re-check does not take the confirmation away.
        XCTAssertEqual(RecallMachine.afterStatus(back, .unknown, nowMs: 2_000), .show(back))
    }

    func testTheWindowEndsByTheDevicesClockAndByTheServers() {
        XCTAssertEqual(
            RecallMachine.afterStatus(fresh, .open(window()), nowMs: 1_000 + 30 * day), .end(.expired))
        XCTAssertEqual(RecallMachine.afterStatus(fresh, .unknown, nowMs: 1_000 + 30 * day), .end(.expired))
        XCTAssertTrue(RecallMachine.expired(opened(), nowMs: 9_000_000), "the server's until-date passed")
        XCTAssertFalse(RecallMachine.expired(opened(), nowMs: 8_999_999))
        // A status whose own window is already over.
        XCTAssertEqual(
            RecallMachine.afterStatus(fresh, .open(window(expires: 1_500)), nowMs: 2_000), .end(.closed))
    }

    func testAnOpenStatusWithNoAttemptsLeftIsOver() {
        XCTAssertEqual(RecallMachine.afterStatus(fresh, .open(window(left: 0)), nowMs: 2_000), .end(.closed))
        var inForm = opened()
        inForm.step = .form
        XCTAssertEqual(
            RecallMachine.afterStatus(inForm, .open(window(left: 0)), nowMs: 2_000), .end(.exhausted))
    }

    func testYesAsksOnceInsideTheScreenThenEnds() {
        let confirming = RecallMachine.tapYes(fresh)
        XCTAssertEqual(confirming.step, .confirmYes, "Yes works without the server")
        XCTAssertEqual(RecallMachine.confirmYes(fresh), .show(fresh), "not without the confirmation step")
        XCTAssertEqual(RecallMachine.back(confirming).step, .question)
        XCTAssertEqual(RecallMachine.confirmYes(confirming), .end(.answeredYes))
        XCTAssertNil(RecallEnd.answeredYes.noticeKey)
        XCTAssertNil(RecallEnd.expired.noticeKey)
    }

    func testTheFormSubmitsOnlyWhenOpenAndNotAlreadyWorking() {
        XCTAssertNil(RecallMachine.submit(opened()), "not in the form")
        let form = RecallMachine.tapNo(opened())
        let working = RecallMachine.submit(form)
        XCTAssertEqual(working?.form, .working)
        XCTAssertNil(working.flatMap(RecallMachine.submit), "no second attempt while one runs")
        XCTAssertEqual(working.map(RecallMachine.back)?.step, .form, "Back is disabled while working")
        var offline = form
        offline.check = .offline
        XCTAssertNil(RecallMachine.submit(offline))
    }

    func testAWrongPasswordNamesTheTriesOnlyBelowFive() {
        var working = RecallMachine.tapNo(opened())
        working.form = .working
        guard case .show(let many) = RecallMachine.afterWrongPassword(
            working, status: .open(window(left: 19)), nowMs: 2_000) else { return XCTFail("expected the form") }
        XCTAssertEqual(many.form, .wrongPassword(triesLeft: nil))
        XCTAssertEqual(many.step, .form)
        guard case .show(let few) = RecallMachine.afterWrongPassword(
            working, status: .open(window(left: 4)), nowMs: 2_000) else { return XCTFail("expected the form") }
        XCTAssertEqual(few.form, .wrongPassword(triesLeft: 4))
        guard case .show(let unknown) = RecallMachine.afterWrongPassword(
            working, status: .unknown, nowMs: 2_000) else { return XCTFail("expected the form") }
        XCTAssertEqual(unknown.form, .wrongPassword(triesLeft: nil))
        XCTAssertEqual(RecallCopy.formErrorKey(few.form), "devices.recall.error.wrong_password")
    }

    func testTheLastWrongPasswordExhaustsTheRecall() {
        var working = RecallMachine.tapNo(opened())
        working.form = .working
        XCTAssertEqual(
            RecallMachine.afterWrongPassword(working, status: .refused, nowMs: 2_000), .end(.exhausted))
        XCTAssertEqual(
            RecallMachine.afterWrongPassword(working, status: .open(window(left: 0)), nowMs: 2_000),
            .end(.exhausted))
        XCTAssertEqual(RecallEnd.exhausted.noticeKey, "devices.recall.error.exhausted")
    }

    func testARefusedAttemptMapsToTheFormsStates() {
        var working = RecallMachine.tapNo(opened())
        working.form = .working
        XCTAssertEqual(RecallMachine.afterRefusal(working, .refused), .end(.closedInForm))
        func form(_ refusal: RecallAnswers.Refusal) -> RecallFormStatus? {
            if case .show(let state) = RecallMachine.afterRefusal(working, refusal) { return state.form }
            return nil
        }
        XCTAssertEqual(form(.rateLimited(retryAfterSecs: 60)), .rateLimited(retryAfterSecs: 60))
        XCTAssertEqual(form(.offline), .offline)
        XCTAssertEqual(form(.failed), .failed)
        XCTAssertEqual(form(.wrongPassword), .wrongPassword(triesLeft: nil))
        XCTAssertEqual(RecallCopy.formErrorKey(.offline), "common.error.offline")
        XCTAssertNil(RecallCopy.formErrorKey(.rateLimited(retryAfterSecs: 60)), "the countdown line says it")
        XCTAssertNil(RecallCopy.formErrorKey(.working))
    }

    // The form's gate.

    func testTheFormNeedsThreeFieldsTheSignupRuleAndAMatch() {
        XCTAssertEqual(RecallForm.problem(oldPassword: "", newPassword: "longenough", repeated: "longenough"), .incomplete)
        XCTAssertEqual(RecallForm.problem(oldPassword: "old", newPassword: "short", repeated: "short"), .tooShort)
        XCTAssertEqual(RecallForm.problem(oldPassword: "old", newPassword: "longenough", repeated: "longenougH"), .mismatch)
        XCTAssertNil(RecallForm.problem(oldPassword: "old", newPassword: "longenough", repeated: "longenough"))
        XCTAssertEqual(NewPasswordRule.minLength, SignupViewModel.minPasswordLength, "the sign-up rule")
        func can(_ status: RecallFormStatus, waiting: Bool = false) -> Bool {
            RecallForm.canSubmit(
                oldPassword: "old", newPassword: "longenough", repeated: "longenough", status: status, waiting: waiting)
        }
        XCTAssertTrue(can(.idle))
        XCTAssertTrue(can(.wrongPassword(triesLeft: 3)))
        XCTAssertFalse(can(.working))
        XCTAssertFalse(can(.rateLimited(retryAfterSecs: 60), waiting: true))
        XCTAssertTrue(can(.rateLimited(retryAfterSecs: 60), waiting: false), "once the countdown ended")
        XCTAssertTrue(RecallForm.locked(.working))
        XCTAssertFalse(RecallForm.locked(.rateLimited(retryAfterSecs: 60)), "Back stays usable")
    }

    // The copy.

    func testEveryKeyTheRecallSurfacesUseIsInTheCatalog() {
        for key in RecallCopy.all {
            let text = Localization.text(
                key, texts: ["name": "@alice", "date": "7 Oct", "time": "0:59", "username": "@alice"],
                counts: ["count": 2])
            XCTAssertNotEqual(text, key, "\(key) is missing from the catalog")
        }
        XCTAssertTrue(
            Localization.text(RecallCopy.body, texts: ["name": "@alice", "date": "7 Oct"]).contains("@alice"))
        XCTAssertTrue(Localization.text(RecallCopy.until, texts: ["date": "6 Nov"]).contains("6 Nov"))
        XCTAssertTrue(Localization.text(RecallCopy.deleteBlocked, texts: ["date": "6 Nov"]).contains("6 Nov"))
        XCTAssertTrue(Localization.text(RecallCopy.triesLeft, counts: ["count": 1]).contains("1"))
    }

    func testDatesAreTheMediumDateOfTheInterfaceLanguage() {
        let text = RecallCopy.date(ms: 1_759_795_200_000, locale: Locale(identifier: "en_GB"))
        XCTAssertTrue(text.contains("2025"), text)
        XCTAssertFalse(text.contains(":"), "a date, no time")
    }
}

// MARK: - The flow around the rules

/// The repository around the rules, against a scripted server: the staged
/// wipe and what it keeps, the screen across a restart, each way the recall
/// ends, and the take-back landing on the key reset's own path.
@MainActor
final class RecallFlowTests: XCTestCase {

    private final class Persistence: SessionPersistence {
        var seed: Data?
        var deviceKey = true
        var token: PersistedSessionToken?
        func hasDeviceKey(accountIdB64: String) -> Bool { deviceKey }
        func accountIdentitySeed(accountIdB64: String) -> Data? { seed }
        func exportKey(accountIdB64: String) -> Data? { nil }
        func sessionToken(accountIdB64: String) -> PersistedSessionToken? { token }
        func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws { self.token = token }
        func deleteSessionToken(accountIdB64: String) { token = nil }
    }

    private final class Directory: AccountDirectory, DeviceRestoreDirectory {
        private(set) var published: [(key: Data, attest: Data?)] = []
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
        ) async throws {
            published.append((signaturePubkey, keyResetAttest))
        }
    }

    /// The recall's server, scripted.
    private final class Server: KeyResetAttestationVerifying {
        var statusAnswers: [Result<RecallWindow, Error>] = []
        var statusFallback: Result<RecallWindow, Error> = .failure(
            AuthClientError.network(endpoint: "/account/recall/status", underlying: URLError(.timedOut)))
        private(set) var statusCalls: [RecallRecord] = []
        var takeBack: Result<SessionPair, Error> = .failure(RecallWrongPassword())
        private(set) var attempts: [RecallAttempt] = []
        /// ADR-0075 section 8 — the status read by login handle.
        var byHandle: Result<RecallByHandle, Error> = .failure(
            AuthClientError.http(status: 401, endpoint: "/account/recall/status", body: #"{"error":"recall-refused"}"#))
        private(set) var byHandleCalls: [(handle: Data, seed: Data)] = []
        let attestation = Data(repeating: 0xA7, count: KeyResetResult.attestationLength)
        var grant: FreshAuthGrant? = FreshAuthGrant(grant: "grant-recall", expiresAtMs: 9_000_000)
        private(set) var events: [String] = []

        var operations: RecallOperations {
            RecallOperations(
                status: { [unowned self] record in
                    self.statusCalls.append(record)
                    let answer = self.statusAnswers.isEmpty ? self.statusFallback : self.statusAnswers.removeFirst()
                    return try answer.get()
                },
                takeBack: { [unowned self] attempt in
                    self.attempts.append(attempt)
                    let pair = try self.takeBack.get()
                    return RecallLanded(
                        reset: KeyResetResult(attestation: self.attestation, session: pair),
                        freshGrant: self.grant)
                },
                statusByHandle: { [unowned self] handle, seed in
                    self.byHandleCalls.append((handle, seed))
                    return try self.byHandle.get()
                }
            )
        }

        func verify(
            attestation: Data, accountId: Data, newSignaturePubkey: Data, nowMs: Int64, maxAgeMs: Int64?
        ) async throws -> KeyResetClaims {
            events.append("verify")
            return KeyResetClaims(keyGeneration: 3, signedAtMs: 1_000)
        }

        var hooks: AuthRepository.KeyResetHooks {
            AuthRepository.KeyResetHooks(
                republishOwnKeyPackage: { [unowned self] _ in self.events.append("keypackage") },
                refoundDeviceGroup: { [unowned self] _, _, _ in self.events.append("devicegroup") }
            )
        }
    }

    private final class WipeLog {
        private(set) var full: [String] = []
        private(set) var staged: [String] = []
        func noteFull(_ account: String) { full.append(account) }
        func noteStaged(_ account: String) { staged.append(account) }
    }

    private final class Clock {
        var nowMs: Int64 = 1_000_000
    }

    private struct Harness {
        let repository: AuthRepository
        let store: SessionStore
        let persistence: Persistence
        let directory: Directory
        let server: Server
        let recalls: InMemoryRecallRecordStore
        let wiped: WipeLog
        let tokens: SessionTokenProvider
        let clock: Clock
        let markers: RecoveryMarkers
    }

    private let accountId = Data((0..<16).map { UInt8(0xC0 | $0) })
    private var accountIdB64: String { base64Encode(accountId) }
    private let phraseSeed = Data(repeating: 0x5A, count: 64)
    private let day: Int64 = 24 * 60 * 60 * 1000
    private var suites: [String] = []

    override func tearDown() async throws {
        // A landed recall persists the new keys the way a key reset does.
        DeviceKeyStore.deleteDeviceKey(accountIdB64: accountIdB64)
        AccountIdentitySeedStore.deleteAccountIdentitySeed(accountIdB64: accountIdB64)
        DeviceIdentityStore.deleteDeviceIdentitySeed(accountIdB64: accountIdB64)
        try? PhraseStore().deletePhrase(accountIdB64: accountIdB64)
        AccountProtectionStore().clear(accountIdB64: accountIdB64)
        for suite in suites { UserDefaults().removePersistentDomain(forName: suite) }
        suites = []
        try await super.tearDown()
    }

    private func defaults(_ name: String) -> UserDefaults {
        let suite = "\(name)-\(UUID().uuidString)"
        suites.append(suite)
        return UserDefaults(suiteName: suite)!
    }

    private var openWindow: RecallWindow {
        RecallWindow(anchorSeq: 4, expiresAtMs: 1_000_000 + 30 * day, changedAtMs: 900_000, attemptsRemaining: 20)
    }

    /// A device. `signedIn` restores a session with the keys; otherwise it
    /// launches signed out (with whatever `recalls` already holds).
    private func device(
        signedIn: Bool = true,
        holdsKeys: Bool = true,
        phraseProtected: Bool = true,
        recalls: InMemoryRecallRecordStore = InMemoryRecallRecordStore(),
        server: Server = Server(),
        clock: Clock = Clock()
    ) -> Harness {
        let store = SessionStore(defaults: defaults("recall-session"))
        let persistence = Persistence()
        // ADR-0075: a phrase-protected account by default (after it is taken
        // back, its new words are shown).
        AccountProtectionStore().setPhraseSaved(phraseProtected && signedIn, accountIdB64: accountIdB64)
        if signedIn {
            store.save(PersistedSession(accountIdB64: accountIdB64, username: "alice"))
            persistence.token = PersistedSessionToken(pair: .fixture(access: "tok-old", accessExpiresAtMs: 2_000_000))
            persistence.seed = holdsKeys ? phraseSeed : nil
        }
        let tokens = SessionTokenProvider()
        tokens.nowMs = { clock.nowMs }
        // The launch refresh is not what these tests are about.
        tokens.refresher = { _ in throw URLError(.notConnectedToInternet) }
        let wiped = WipeLog()
        let directory = Directory()
        let markers = RecoveryMarkers(defaults: defaults("recall-markers"))
        let repository = AuthRepository(
            sessionStore: store,
            authClientFactory: { NameRouteStub.client(NameRouteStub.scope { _, _, _ in (503, "") }) },
            persistence: persistence,
            nowMs: { clock.nowMs },
            accountDirectory: directory,
            loginHandleBinds: LoginHandleBindLedger(),
            restoreDirectory: directory,
            markers: markers,
            tokenProvider: tokens,
            grantNowMs: { clock.nowMs },
            attestationVerifier: server,
            keyResetHooks: server.hooks,
            wipeAfterKeyReset: { [weak store] account in
                wiped.noteFull(account)
                store?.clear()
            },
            stagedWipeAfterKeyReset: { [weak store] account in
                wiped.noteStaged(account)
                store?.clear()
            },
            recallRecords: recalls,
            recallOperations: server.operations,
            ownDeviceId: { _ in nil },
            identityKeyMarks: IdentityKeyRecordMarks(defaults: defaults("recall-marks")),
            logoutOwed: InMemoryLogoutOwedStore(),
            earlierKeysInputs: skipEarlierKeysGate
        )
        return Harness(
            repository: repository, store: store, persistence: persistence, directory: directory,
            server: server, recalls: recalls, wiped: wiped, tokens: tokens, clock: clock, markers: markers)
    }

    private func waitUntil(_ condition: @MainActor () -> Bool, timeout: TimeInterval = 5) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 20_000_000)
        }
        return condition()
    }

    /// A device on "Was it you?" with the server having confirmed the window.
    private func asking(server: Server = Server()) async -> Harness {
        server.statusAnswers = [.success(openWindow)]
        let h = device(server: server)
        h.repository.handleKeysSuperseded(accountIdB64: accountIdB64)
        let open = await waitUntil { h.repository.recall?.canTakeBack == true }
        XCTAssertTrue(open)
        return h
    }

    // MARK: ADR-0075 section 8 — the phrase as the proof

    func testAnOldDeviceTakesTheAccountBackWithThePhraseInsteadOfThePassword() async throws {
        let h = await asking()
        h.repository.recallTapNo()
        h.server.takeBack = .success(.fixture(access: "tok-recalled", refresh: "r1.recalled", generation: 3))
        let old = RecoveryMnemonic.generate()

        await h.repository.recallTakeBack(proof: .phrase(" \(old.phrase()) "), newPassword: Data("new-password".utf8))

        // The proof is the seed of the words typed; no password is sent.
        let attempt = try XCTUnwrap(h.server.attempts.first)
        XCTAssertEqual(attempt.oldPhraseSeed, old.toSeed())
        XCTAssertEqual(attempt.oldPassword, Data())
        XCTAssertEqual(attempt.newPassword, Data("new-password".utf8))
        // The outcome is the recall's: a NEW identity, its words shown last.
        XCTAssertNil(h.repository.recall)
        XCTAssertEqual(
            h.repository.state, .saveFreshPhrase(SessionAccount(accountIdB64: accountIdB64, username: "alice")))
    }

    func testAnAccountWithoutASavedPhraseIsTakenBackWithNoWordsOnAnyScreen() async throws {
        let server = Server()
        server.statusAnswers = [.success(openWindow)]
        let h = device(phraseProtected: false, server: server)
        h.repository.handleKeysSuperseded(accountIdB64: accountIdB64)
        let open = await waitUntil { h.repository.recall?.canTakeBack == true }
        XCTAssertTrue(open)
        // The record kept the flag across the staged wipe.
        XCTAssertEqual(h.recalls.record?.phraseSaved, false)
        h.repository.recallTapNo()
        server.takeBack = .success(.fixture(access: "tok-recalled", refresh: "r1.recalled", generation: 3))

        await h.repository.recallTakeBack(oldPassword: Data("old-password".utf8), newPassword: Data("new-password".utf8))

        XCTAssertEqual(
            h.repository.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "alice")),
            "no phrase screen")
        XCTAssertNil(h.repository.freshPhrase())
        XCTAssertFalse(h.markers.isPhraseOwed(accountIdB64: accountIdB64))
        XCTAssertNil(h.repository.devicePhrase())
        XCTAssertTrue(h.repository.accountRecalledThisSession)
    }

    func testWordsThatAreNoPhraseAreAWrongProofWithoutARequest() async {
        let h = await asking()
        h.repository.recallTapNo()
        await h.repository.recallTakeBack(
            proof: .phrase("these are not twelve words"), newPassword: Data("new-password".utf8))
        XCTAssertTrue(h.server.attempts.isEmpty)
        XCTAssertEqual(h.repository.recall?.form, .wrongPassword(triesLeft: nil))
        XCTAssertEqual(h.repository.recall?.step, .form)
    }

    func testAFreshDeviceAsksByHandleWhetherTheWordsWereTheAccountsPhrase() async throws {
        let server = Server()
        let h = device(signedIn: false, server: server)
        let old = RecoveryMnemonic.generate()
        let phraseWindow = RecallWindow(
            anchorSeq: 6, expiresAtMs: 1_000_000 + 10 * day, changedAtMs: 900_000, attemptsRemaining: 20,
            phraseArm: true)

        // Every refusal reads as "not this account's phrase": no offer.
        let none = await h.repository.phraseRecallOffer(username: "@Alice", mnemonicPhrase: old.phrase())
        XCTAssertNil(none)
        // The account is named by the login handle of the typed name, the
        // statement signed by the key derived from the typed words.
        XCTAssertEqual(server.byHandleCalls.first?.handle, try loginHandle(username: "alice"))
        XCTAssertEqual(
            server.byHandleCalls.first?.seed, try Identity.fromSeed(seed: old.toSeed()).ed25519Seed())

        // An account that had an email vault: the phrase proves nothing.
        server.byHandle = .success(RecallByHandle(window: openWindow, accountId: accountId))
        let noArm = await h.repository.phraseRecallOffer(username: "alice", mnemonicPhrase: old.phrase())
        XCTAssertNil(noArm)

        server.byHandle = .success(RecallByHandle(window: phraseWindow, accountId: accountId))
        let offered = await h.repository.phraseRecallOffer(username: "alice", mnemonicPhrase: old.phrase())
        let offer = try XCTUnwrap(offered)
        XCTAssertEqual(offer.accountId, accountId)
        XCTAssertEqual(offer.username, "alice")
        XCTAssertEqual(offer.window.changedAtMs, 900_000, "the date the prompt names")

        // Words that are no phrase ask nothing.
        let before = server.byHandleCalls.count
        let bad = await h.repository.phraseRecallOffer(username: "alice", mnemonicPhrase: "not a phrase")
        XCTAssertNil(bad)
        XCTAssertEqual(server.byHandleCalls.count, before)
    }

    func testAFreshDeviceTakesTheAccountBackWithThePhrase() async throws {
        let server = Server()
        let h = device(signedIn: false, server: server)
        let old = RecoveryMnemonic.generate()
        let offer = PhraseRecallOffer(
            accountId: accountId, username: "alice",
            window: RecallWindow(
                anchorSeq: 6, expiresAtMs: 1_000_000 + 10 * day, changedAtMs: 900_000, attemptsRemaining: 20,
                phraseArm: true))

        // The words do not open the anchor: nothing changed, nothing landed.
        do {
            try await h.repository.takeBackWithPhrase(
                offer: offer, mnemonicPhrase: old.phrase(), newPassword: Data("new-password".utf8))
            XCTFail("a wrong proof must not land")
        } catch {
            XCTAssertTrue(error is RecallWrongPassword)
        }
        XCTAssertNil(h.tokens.current)
        XCTAssertFalse(h.repository.accountRecalledThisSession)

        server.takeBack = .success(.fixture(access: "tok-recalled", refresh: "r1.recalled", generation: 3))
        try await h.repository.takeBackWithPhrase(
            offer: offer, mnemonicPhrase: old.phrase(), newPassword: Data("new-password".utf8))

        let attempt = try XCTUnwrap(server.attempts.last)
        XCTAssertEqual(attempt.anchorSeq, 6)
        XCTAssertEqual(attempt.oldPhraseSeed, old.toSeed())
        XCTAssertEqual(attempt.oldPassword, Data())
        // The statements are signed by the key derived from the typed words.
        XCTAssertEqual(attempt.record.oldSeed, try Identity.fromSeed(seed: old.toSeed()).ed25519Seed())
        XCTAssertEqual(attempt.record.accountIdB64, accountIdB64)
        XCTAssertNotEqual(attempt.newSeed, old.toSeed(), "the old identity does not return")
        // The recall's outcome: the session, the attested upload, the new words last.
        XCTAssertEqual(h.tokens.current?.sessionToken, "tok-recalled")
        XCTAssertEqual(
            h.repository.state, .saveFreshPhrase(SessionAccount(accountIdB64: accountIdB64, username: "alice")))
        XCTAssertEqual(h.directory.published.first?.attest, server.attestation)
        XCTAssertTrue(h.repository.accountRecalledThisSession)
        let phrase = try XCTUnwrap(h.repository.freshPhrase())
        XCTAssertEqual(try RecoveryMnemonic.fromPhrase(phrase: phrase).toSeed(), attempt.newSeed)
    }

    private func refused() -> Error {
        AuthClientError.http(status: 401, endpoint: "/account/recall/status", body: #"{"error":"recall-refused"}"#)
    }

    // MARK: The staged wipe

    func testASupersededDeviceWithKeysKeepsOnlyTheRecallRecordAndAsks() async throws {
        let server = Server()
        server.statusAnswers = [.success(openWindow)]
        let h = device(server: server)
        XCTAssertEqual(h.repository.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "alice")))

        h.repository.handleKeysSuperseded(accountIdB64: accountIdB64)

        // The record is written before the wipe starts.
        let record = try XCTUnwrap(h.recalls.record)
        XCTAssertEqual(record.accountIdB64, accountIdB64)
        XCTAssertEqual(record.username, "alice")
        XCTAssertEqual(record.learnedAtMs, 1_000_000)
        XCTAssertEqual(
            record.oldSeed, try Identity.fromSeed(seed: phraseSeed).ed25519Seed(),
            "the 32-byte Ed25519 seed of the account identity, not the phrase seed")
        XCTAssertEqual(record.oldSeed.count, 32)
        XCTAssertNil(h.tokens.current, "the superseded pair is gone at once")
        XCTAssertNotNil(h.repository.recall)
        XCTAssertNil(h.repository.keysSignOut, "not the signed-out screen")

        let open = await waitUntil { h.repository.recall?.canTakeBack == true }
        XCTAssertTrue(open)
        XCTAssertEqual(h.wiped.staged, [accountIdB64], "the staged wipe, once")
        XCTAssertEqual(h.wiped.full, [], "never the full wipe while the recall is open")
        XCTAssertEqual(h.server.statusCalls, [record], "the status is asked with the kept record")
        XCTAssertEqual(h.repository.recall?.untilMs, openWindow.expiresAtMs)
        XCTAssertEqual(h.repository.recall?.resetAtMs, 900_000)

        // A second signal changes nothing.
        h.repository.handleKeysSuperseded(accountIdB64: accountIdB64)
        XCTAssertEqual(h.recalls.puts, 1)
    }

    func testADeviceThatNeverHeldTheKeysKeepsNothingAndShowsTheSignedOutScreen() async {
        let h = device(holdsKeys: false)
        h.repository.handleKeysSuperseded(accountIdB64: accountIdB64)
        XCTAssertNil(h.repository.recall)
        XCTAssertNil(h.recalls.record)
        XCTAssertEqual(h.repository.keysSignOutCause, .keysReset)
        let done = await waitUntil { h.repository.keysSignOut == .removed }
        XCTAssertTrue(done)
        XCTAssertEqual(h.wiped.full, [accountIdB64])
        XCTAssertEqual(h.wiped.staged, [])
        XCTAssertTrue(h.server.statusCalls.isEmpty, "nothing to ask the server with")
    }

    func testARemovedDeviceWithKeysRunsTheFullWipeAndKeepsNoRecord() async {
        let h = device()
        h.repository.handleDeviceRevoked(accountIdB64: accountIdB64)
        XCTAssertNil(h.repository.recall)
        XCTAssertNil(h.recalls.record, "device-revoked never leaves a recall record")
        XCTAssertEqual(h.repository.keysSignOutCause, .deviceRemoved)
        let done = await waitUntil { h.repository.keysSignOut == .removed }
        XCTAssertTrue(done)
        XCTAssertEqual(h.wiped.full, [accountIdB64])
        XCTAssertEqual(h.wiped.staged, [])
    }

    func testWhenTheRecordCannotBeStoredTheDeviceKeepsNothing() async {
        let recalls = InMemoryRecallRecordStore()
        recalls.putError = RecallRecordStoreError.keychain(-25300)
        let h = device(recalls: recalls)
        h.repository.handleKeysSuperseded(accountIdB64: accountIdB64)
        XCTAssertNil(h.repository.recall)
        let done = await waitUntil { h.repository.keysSignOut == .removed }
        XCTAssertTrue(done)
        XCTAssertEqual(h.wiped.full, [accountIdB64])
    }

    // MARK: The status

    func testARefusedStatusDestroysTheRecordAndFinishesTheWipe() async {
        let server = Server()
        server.statusAnswers = [.failure(refused())]
        let h = device(server: server)
        h.repository.handleKeysSuperseded(accountIdB64: accountIdB64)
        let done = await waitUntil { h.repository.keysSignOut == .removed }
        XCTAssertTrue(done, "the closed state is the signed-out screen: Sign in")
        XCTAssertNil(h.repository.recall)
        XCTAssertNil(h.recalls.record, "the record is destroyed")
        XCTAssertEqual(h.wiped.staged, [accountIdB64])
        XCTAssertEqual(h.wiped.full, [accountIdB64], "the rest of the wipe ran")
        XCTAssertNil(h.repository.keysSignOutNoticeKey, "nothing says why")
        XCTAssertEqual(h.repository.keysSignOutUsername, "alice")
        h.repository.dismissKeysSignOut()
        XCTAssertNil(h.repository.keysSignOut)
    }

    func testOfflineShowsTheQuestionAndIsReCheckedWhenTheConnectionReturns() async {
        let server = Server()
        let h = device(server: server)
        h.repository.handleKeysSuperseded(accountIdB64: accountIdB64)
        let offline = await waitUntil { h.repository.recall?.check == .offline }
        XCTAssertTrue(offline)
        XCTAssertEqual(h.repository.recall?.canTakeBack, false)
        XCTAssertNotNil(h.recalls.record, "an unreachable server ends nothing")
        // Yes still works offline; No does not.
        h.repository.recallTapNo()
        XCTAssertEqual(h.repository.recall?.step, .question)

        server.statusAnswers = [.success(openWindow)]
        await h.repository.refreshRecallStatus()
        XCTAssertEqual(h.repository.recall?.check, .open(openWindow))
        XCTAssertEqual(h.wiped.full, [])
    }

    func testALimitOrAServerFaultOnTheStatusEndsNothing() async {
        let server = Server()
        server.statusAnswers = [.failure(AuthClientError.http(
            status: 429, endpoint: "/account/recall/status", body: #"{"error":"too-many-attempts"}"#))]
        let h = device(server: server)
        h.repository.handleKeysSuperseded(accountIdB64: accountIdB64)
        let offline = await waitUntil { h.repository.recall?.check == .offline }
        XCTAssertTrue(offline)
        XCTAssertNotNil(h.recalls.record)
    }

    // MARK: Yes, it was me

    func testYesIsLocalDestroysTheRecordAndFinishesTheWipe() async {
        let h = await asking()
        let asked = h.server.statusCalls.count
        h.repository.recallTapYes()
        XCTAssertEqual(h.repository.recall?.step, .confirmYes)
        XCTAssertNotNil(h.recalls.record, "nothing is deleted before the confirmation")
        h.repository.recallBack()
        XCTAssertEqual(h.repository.recall?.step, .question)
        h.repository.recallTapYes()
        h.repository.recallConfirmYes()
        XCTAssertNil(h.repository.recall)
        XCTAssertNil(h.recalls.record)
        XCTAssertEqual(h.repository.keysSignOut, .removing)
        let done = await waitUntil { h.repository.keysSignOut == .removed }
        XCTAssertTrue(done)
        XCTAssertEqual(h.wiped.full, [accountIdB64])
        XCTAssertNil(h.repository.keysSignOutNoticeKey)
        XCTAssertEqual(h.server.statusCalls.count, asked, "Yes sends nothing")
        XCTAssertTrue(h.server.attempts.isEmpty)
    }

    // MARK: The restart

    func testTheScreenSurvivesARestart() async throws {
        let first = await asking()
        let record = try XCTUnwrap(first.recalls.record)

        // The app is closed and opened again: signed out, the record there.
        let server = Server()
        server.statusAnswers = [.success(openWindow)]
        let relaunched = device(signedIn: false, recalls: first.recalls, server: server)
        XCTAssertNotNil(relaunched.repository.recall, "driven by the record, not by memory")
        XCTAssertEqual(relaunched.repository.recall?.username, "alice")
        XCTAssertEqual(relaunched.repository.recall?.learnedAtMs, record.learnedAtMs)
        XCTAssertNil(relaunched.repository.keysSignOut)
        let open = await waitUntil { relaunched.repository.recall?.canTakeBack == true }
        XCTAssertTrue(open, "the window is confirmed again at launch")
        XCTAssertEqual(relaunched.wiped.full, [])
        XCTAssertEqual(relaunched.wiped.staged, [], "nothing is left to wipe twice")
    }

    func testAnExpiredRecordEndsTheRecallAtLaunch() async {
        let recalls = InMemoryRecallRecordStore()
        try? recalls.put(RecallRecord(
            accountIdB64: accountIdB64, oldSeed: Data(repeating: 7, count: 32), username: "alice", learnedAtMs: 1_000))
        let clock = Clock()
        clock.nowMs = 1_000 + 30 * day
        let h = device(signedIn: false, recalls: recalls, clock: clock)
        XCTAssertNil(h.repository.recall)
        XCTAssertNil(h.recalls.record, "the record is destroyed")
        let done = await waitUntil { h.repository.keysSignOut == .removed }
        XCTAssertTrue(done)
        XCTAssertEqual(h.wiped.full, [accountIdB64], "the wipe is finished")
        XCTAssertTrue(h.server.statusCalls.isEmpty, "the device's own clock is enough")
    }

    func testTheWindowPassingWhileTheScreenIsUpEndsIt() async {
        let h = await asking()
        h.clock.nowMs = openWindow.expiresAtMs
        await h.repository.refreshRecallStatus()
        XCTAssertNil(h.repository.recall)
        XCTAssertNil(h.recalls.record)
        let done = await waitUntil { h.repository.keysSignOut == .removed }
        XCTAssertTrue(done)
    }

    func testARecordWhoseRecallAlreadyLandedIsDestroyedAtLaunch() throws {
        // The app closed after `finish` answered and before the record went:
        // the restored session holds OTHER keys than the record's.
        let recalls = InMemoryRecallRecordStore()
        try recalls.put(RecallRecord(
            accountIdB64: accountIdB64, oldSeed: Data(repeating: 7, count: 32), username: "alice", learnedAtMs: 1_000))
        let h = device(recalls: recalls)
        XCTAssertNil(h.repository.recall)
        XCTAssertNil(h.recalls.record)
        XCTAssertEqual(h.repository.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "alice")))
    }

    func testAnInterruptedStagedWipeKeepsItsRecordAndRunsAgain() async throws {
        // The app closed mid-wipe: the session is still restorable and the
        // record names the SAME keys.
        let recalls = InMemoryRecallRecordStore()
        let old = try Identity.fromSeed(seed: phraseSeed).ed25519Seed()
        let record = RecallRecord(accountIdB64: accountIdB64, oldSeed: old, username: "alice", learnedAtMs: 500_000)
        try recalls.put(record)
        let server = Server()
        server.statusAnswers = [.success(openWindow)]
        let h = device(recalls: recalls, server: server)
        XCTAssertNil(h.repository.recall, "the session restored; the refresh will say superseded again")
        XCTAssertEqual(h.recalls.record, record)

        h.repository.handleKeysSuperseded(accountIdB64: accountIdB64)
        XCTAssertEqual(h.recalls.record, record, "the first record is kept: the date it learned does not move")
        XCTAssertEqual(h.recalls.puts, 1)
        XCTAssertEqual(h.repository.recall?.learnedAtMs, 500_000)
        let staged = await waitUntil { h.wiped.staged == [self.accountIdB64] }
        XCTAssertTrue(staged)
    }

    // MARK: Taking the account back

    func testNothingIsSentBeforeTheServerConfirmedTheWindow() async {
        let h = device()
        h.repository.handleKeysSuperseded(accountIdB64: accountIdB64)
        _ = await waitUntil { h.repository.recall?.check == .offline }
        await h.repository.recallTakeBack(oldPassword: Data("old".utf8), newPassword: Data("new-password".utf8))
        XCTAssertTrue(h.server.attempts.isEmpty)
    }

    func testAWrongOldPasswordKeepsTheFormAndNamesTheTriesBelowFive() async {
        let h = await asking()
        h.repository.recallTapNo()
        XCTAssertEqual(h.repository.recall?.step, .form)
        h.server.takeBack = .failure(RecallWrongPassword())
        h.server.statusAnswers = [.success(RecallWindow(
            anchorSeq: 4, expiresAtMs: openWindow.expiresAtMs, changedAtMs: 900_000, attemptsRemaining: 3))]

        await h.repository.recallTakeBack(oldPassword: Data("wrong".utf8), newPassword: Data("new-password".utf8))

        XCTAssertEqual(h.repository.recall?.form, .wrongPassword(triesLeft: 3))
        XCTAssertEqual(h.repository.recall?.step, .form)
        XCTAssertNotNil(h.recalls.record, "a wrong password ends nothing")
        XCTAssertEqual(h.server.attempts.count, 1)
        XCTAssertEqual(h.server.attempts.first?.anchorSeq, 4)
        XCTAssertEqual(h.server.attempts.first?.oldPassword, Data("wrong".utf8))
        XCTAssertNil(h.repository.keysSignOut)
        XCTAssertNil(h.tokens.current, "nothing landed")
    }

    func testTheLastWrongPasswordEndsTheRecallWithItsLine() async {
        let h = await asking()
        h.repository.recallTapNo()
        h.server.takeBack = .failure(RecallWrongPassword())
        h.server.statusAnswers = [.failure(refused())]
        await h.repository.recallTakeBack(oldPassword: Data("wrong".utf8), newPassword: Data("new-password".utf8))
        XCTAssertNil(h.repository.recall)
        XCTAssertNil(h.recalls.record)
        XCTAssertEqual(h.repository.keysSignOutNoticeKey, "devices.recall.error.exhausted")
        let done = await waitUntil { h.repository.keysSignOut == .removed }
        XCTAssertTrue(done)
        XCTAssertEqual(h.wiped.full, [accountIdB64])
        // The line goes with the screen.
        h.repository.dismissKeysSignOut()
        XCTAssertNil(h.repository.keysSignOutNoticeKey)
    }

    func testARefusalWhileTheFormIsOpenEndsTheRecallAsClosed() async {
        let h = await asking()
        h.repository.recallTapNo()
        h.server.takeBack = .failure(AuthClientError.http(
            status: 401, endpoint: "/account/recall/start", body: #"{"error":"recall-refused"}"#))
        await h.repository.recallTakeBack(oldPassword: Data("old".utf8), newPassword: Data("new-password".utf8))
        XCTAssertNil(h.repository.recall)
        XCTAssertNil(h.recalls.record)
        XCTAssertEqual(h.repository.keysSignOutNoticeKey, "devices.recall.error.closed")
        let done = await waitUntil { h.repository.keysSignOut == .removed }
        XCTAssertTrue(done)
    }

    func testALimitOrNoConnectionKeepsTheFormAndTheRecord() async {
        let h = await asking()
        h.repository.recallTapNo()
        h.server.takeBack = .failure(AuthClientError.http(
            status: 429, endpoint: "/account/recall/start",
            body: #"{"error":"too-many-attempts","retry_after_secs":600}"#))
        await h.repository.recallTakeBack(oldPassword: Data("old".utf8), newPassword: Data("new-password".utf8))
        XCTAssertEqual(h.repository.recall?.form, .rateLimited(retryAfterSecs: 600))
        // Back stays usable while rate limited.
        h.repository.recallBack()
        XCTAssertEqual(h.repository.recall?.step, .question)
        h.repository.recallTapNo()

        h.server.takeBack = .failure(AuthClientError.network(
            endpoint: "/account/recall/finish", underlying: URLError(.notConnectedToInternet)))
        await h.repository.recallTakeBack(oldPassword: Data("old".utf8), newPassword: Data("new-password".utf8))
        XCTAssertEqual(h.repository.recall?.form, .offline)
        XCTAssertNotNil(h.recalls.record)
        XCTAssertEqual(h.wiped.full, [])
    }

    func testTakingTheAccountBackLandsOnTheKeyResetsPath() async throws {
        let h = await asking()
        let record = try XCTUnwrap(h.recalls.record)
        h.repository.recallTapNo()
        let pair = SessionPair.fixture(access: "tok-recalled", refresh: "r1.recalled", generation: 3)
        h.server.takeBack = .success(pair)

        await h.repository.recallTakeBack(oldPassword: Data("old-password".utf8), newPassword: Data("new-password".utf8))

        // What was sent: the record's key, the anchor, a NEW identity.
        let attempt = try XCTUnwrap(h.server.attempts.first)
        XCTAssertEqual(attempt.record, record)
        XCTAssertEqual(attempt.anchorSeq, 4)
        XCTAssertEqual(attempt.newPassword, Data("new-password".utf8))
        XCTAssertEqual(attempt.newSeed.count, 64)
        XCTAssertEqual(attempt.newSignaturePubkey, try Identity.fromSeed(seed: attempt.newSeed).signaturePublicKey())
        XCTAssertNotEqual(
            attempt.newSignaturePubkey, try Identity.fromSeed(seed: phraseSeed).signaturePublicKey(),
            "the old identity does not return")
        XCTAssertNil(attempt.deviceId, "the session is bound after the landing")

        // The record is destroyed once finish answered.
        XCTAssertNil(h.recalls.record)
        XCTAssertNil(h.repository.recall)
        XCTAssertNil(h.repository.keysSignOut)
        XCTAssertEqual(h.wiped.full, [], "taking the account back wipes nothing more")

        // From here it is the key reset's client order.
        XCTAssertEqual(h.tokens.current?.sessionToken, "tok-recalled")
        XCTAssertEqual(h.persistence.token?.sessionToken, "tok-recalled")
        XCTAssertEqual(
            h.repository.state, .saveFreshPhrase(SessionAccount(accountIdB64: accountIdB64, username: "alice")))
        XCTAssertEqual(h.store.load(), PersistedSession(accountIdB64: accountIdB64, username: "alice"),
                       "a relaunch on the phrase screen restores from the session record")
        XCTAssertTrue(h.markers.isPhraseOwed(accountIdB64: accountIdB64))
        XCTAssertEqual(h.directory.published.count, 1, "the attested directory upload")
        XCTAssertEqual(h.directory.published.first?.key, attempt.newSignaturePubkey)
        XCTAssertEqual(h.directory.published.first?.attest, h.server.attestation)
        XCTAssertEqual(h.server.events, ["verify", "keypackage", "devicegroup"])
        XCTAssertFalse(h.markers.isPublishOwed(accountIdB64: accountIdB64), "every step landed")
        XCTAssertFalse(h.repository.freshPublishPending)

        // The new 12 words are shown last, and are the new identity's.
        let phrase = try XCTUnwrap(h.repository.freshPhrase())
        XCTAssertEqual(phrase.split(separator: " ").count, 12)
        XCTAssertEqual(try RecoveryMnemonic.fromPhrase(phrase: phrase).toSeed(), attempt.newSeed)

        // Confirming the phrase lands the session, with the done line.
        try h.repository.confirmFreshPhraseSaved()
        XCTAssertEqual(h.repository.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "alice")))
        XCTAssertEqual(h.repository.identitySecrets?.accountIdentitySeed, attempt.newSeed)
        XCTAssertTrue(h.repository.accountRecalledThisSession)
        XCTAssertEqual(h.repository.liveFreshGrant(), "grant-recall", "the fresh-auth grant finish answered")
        h.repository.dismissRecalledNotice()
        XCTAssertFalse(h.repository.accountRecalledThisSession)
    }

    func testTheNewWordsAreWithheldWhileThePublishIsOwed() async throws {
        let server = Server()
        let h = await asking(server: server)
        h.repository.recallTapNo()
        server.takeBack = .success(.fixture(access: "tok-recalled", refresh: "r1.recalled", generation: 3))
        // The DeviceGroup step cannot land.
        struct Down: Error {}
        let failing = AuthRepository.KeyResetHooks(
            republishOwnKeyPackage: { _ in },
            refoundDeviceGroup: { _, _, _ in throw Down() })
        h.repository.keyResetHooks = failing

        await h.repository.recallTakeBack(oldPassword: Data("old-password".utf8), newPassword: Data("new-password".utf8))

        XCTAssertNil(h.recalls.record, "the server took the account back: the record is destroyed regardless")
        XCTAssertTrue(h.repository.freshPublishPending)
        XCTAssertTrue(h.markers.isPublishOwed(accountIdB64: accountIdB64), "the owed-publish record")
        XCTAssertThrowsError(try h.repository.confirmFreshPhraseSaved()) { error in
            XCTAssertTrue(error is FreshPublishPending)
        }
    }
}

// MARK: - Account deletion while a window is open

@MainActor
final class RecallDeleteBlockedTests: XCTestCase {

    func testTheBlockedLineNamesTheDate() {
        let until: Int64 = 1_762_387_200_000
        let text = PasskeysViewModel.deleteBlocked(untilMs: until).text
        let date = RecallCopy.date(ms: until, locale: Localization.currentLocale)
        XCTAssertFalse(date.isEmpty)
        XCTAssertTrue(text.contains(date), text)
        XCTAssertNotEqual(text, RecallCopy.deleteBlocked)
    }
}
