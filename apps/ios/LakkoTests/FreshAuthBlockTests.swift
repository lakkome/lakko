// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// UX `docs/ux/identity-recovery.md` section 17.1 — the shared proof block:
/// the pure layout decision, the per-surface lines, the failure mapping and
/// the state machine over fake ports.
@MainActor
final class FreshAuthBlockTests: XCTestCase {

    private func layout(
        _ surface: FreshAuthSurface = .reset,
        live: Bool = false, passkey: Bool = false, usernameKnown: Bool = true,
        usePassword: Bool = false, state: FreshAuthBlockState = .idle, waiting: Bool = false,
        password: String = "", username: String = ""
    ) -> FreshAuthBlockLayout {
        FreshAuthBlockLayout.decide(
            surface: surface, proofLive: live, hasUsablePasskey: passkey, usernameKnown: usernameKnown,
            usePassword: usePassword, state: state, waiting: waiting, password: password, username: username)
    }

    // MARK: - What is drawn

    func testALiveProofDrawsNothingAndLetsTheSurfaceContinue() {
        for surface in [FreshAuthSurface.reset, .deleteAccount, .addDeviceRequest, .addDeviceScan,
                        .addPasskey, .changePassword] {
            let l = layout(surface, live: true, passkey: true)
            XCTAssertFalse(l.showsBlock)
            XCTAssertTrue(l.ready)
            XCTAssertFalse(l.showsPasswordField)
            XCTAssertFalse(l.showsPasskeyButton)
            XCTAssertNil(l.line)
        }
    }

    func testWithoutAProofTheFieldIsShownAndTheButtonWaitsForIt() {
        let empty = layout()
        XCTAssertTrue(empty.showsBlock)
        XCTAssertTrue(empty.showsPasswordField)
        XCTAssertFalse(empty.showsUsernameField)
        XCTAssertFalse(empty.ready, "disabled while the field is empty")
        XCTAssertTrue(layout(password: "pw").ready)
    }

    func testThePasskeyButtonIsAbsentWithoutAUsablePasskey() {
        XCTAssertFalse(layout().showsPasskeyButton, "absent, not disabled")
        XCTAssertTrue(layout(passkey: true).showsPasskeyButton)
        XCTAssertTrue(layout(passkey: true).passkeyButtonEnabled)
    }

    func testAnUnknownUsernameAddsItsFieldAndIsNeeded() {
        let l = layout(usernameKnown: false, password: "pw")
        XCTAssertTrue(l.showsUsernameField)
        XCTAssertFalse(l.ready)
        XCTAssertFalse(layout(usernameKnown: false, password: "pw", username: "   ").ready)
        XCTAssertTrue(layout(usernameKnown: false, password: "pw", username: "dora").ready)
    }

    func testAddADeviceAsksForThePasskeyFirst() {
        for surface in [FreshAuthSurface.addDeviceRequest, .addDeviceScan] {
            let l = layout(surface, passkey: true)
            XCTAssertTrue(l.showsPasskeyNotice)
            XCTAssertFalse(l.showsPasswordField, "no field")
            XCTAssertTrue(l.showsUsePassword)
            XCTAssertTrue(l.mainOpensPasskey)
            XCTAssertTrue(l.ready, "the button opens the sheet")

            let swapped = layout(surface, passkey: true, usePassword: true)
            XCTAssertFalse(swapped.showsPasskeyNotice)
            XCTAssertTrue(swapped.showsPasswordField)
            XCTAssertFalse(swapped.mainOpensPasskey)
            XCTAssertFalse(swapped.ready)

            let noPasskey = layout(surface)
            XCTAssertTrue(noPasskey.showsPasswordField)
            XCTAssertFalse(noPasskey.showsPasskeyNotice)
            XCTAssertFalse(noPasskey.showsPasskeyButton)
        }
    }

    func testTheOtherSurfacesNeverAskForThePasskeyFirst() {
        for surface in [FreshAuthSurface.reset, .deleteAccount, .addPasskey, .changePassword] {
            let l = layout(surface, passkey: true)
            XCTAssertFalse(l.showsPasskeyNotice)
            XCTAssertTrue(l.showsPasswordField)
            XCTAssertTrue(l.showsPasskeyButton)
        }
    }

    // MARK: - The shared states

    func testCheckingDisablesTheFieldAndBothButtons() {
        let l = layout(passkey: true, state: .checking, password: "pw")
        XCTAssertTrue(l.working)
        XCTAssertTrue(l.fieldsDisabled)
        XCTAssertFalse(l.ready)
        XCTAssertFalse(l.passkeyButtonEnabled)
    }

    func testAWrongPasswordSaysWhatDidNotHappenPerSurface() {
        XCTAssertEqual(
            layout(.reset, state: .wrongPassword).line,
            .text(key: "restore_identity.reset.error.wrong_password", isError: true))
        XCTAssertEqual(
            layout(.deleteAccount, state: .wrongPassword).line,
            .text(key: "proof.delete.wrong_password", isError: true))
        XCTAssertEqual(
            layout(.addDeviceRequest, state: .wrongPassword).line,
            .text(key: "proof.add_device.request.wrong_password", isError: true))
        XCTAssertEqual(
            layout(.addDeviceScan, state: .wrongPassword).line,
            .text(key: "proof.add_device.scan.wrong_password", isError: true))
        XCTAssertFalse(layout(.reset, state: .wrongPassword).fieldsDisabled, "the field keeps focus")
    }

    func testATimedRateLimitDisablesThePasswordButNotThePasskey() {
        let l = layout(passkey: true, state: .rateLimited(timed: true), waiting: true, password: "pw")
        XCTAssertEqual(l.line, .countdown)
        XCTAssertTrue(l.fieldsDisabled)
        XCTAssertFalse(l.ready)
        XCTAssertTrue(l.passkeyButtonEnabled, "Use a passkey instead stays usable")
    }

    func testTheRateLimitEndsWithItsCountdown() {
        let l = layout(state: .rateLimited(timed: true), waiting: false, password: "pw")
        XCTAssertNil(l.line)
        XCTAssertFalse(l.fieldsDisabled)
        XCTAssertTrue(l.ready)
    }

    func testARateLimitWithoutATimeUsesTheGenericLine() {
        let l = layout(state: .rateLimited(timed: false), password: "pw")
        XCTAssertEqual(l.line, .text(key: "common.error.too_many_attempts", isError: true))
        XCTAssertFalse(l.fieldsDisabled)
    }

    func testThePasskeySheetShowsTheWaitingLine() {
        let l = layout(passkey: true, state: .passkeySheet)
        XCTAssertEqual(l.line, .text(key: "recovery.passkey.waiting", isError: false))
        XCTAssertTrue(l.working)
        XCTAssertFalse(l.passkeyButtonEnabled)
    }

    func testACancelledGestureIsNotAnErrorAndNamesTheSurface() {
        XCTAssertEqual(
            layout(.reset, state: .cancelled).line, .text(key: "proof.reset.cancelled", isError: false))
        XCTAssertEqual(
            layout(.addDeviceRequest, passkey: true, state: .cancelled).line,
            .text(key: "proof.add_device.request.cancelled", isError: false))
        XCTAssertEqual(
            layout(.addDeviceScan, passkey: true, state: .cancelled).line,
            .text(key: "proof.add_device.scan.cancelled", isError: false))
        XCTAssertEqual(
            layout(.deleteAccount, state: .cancelled).line,
            .text(key: "settings.account.delete.error.cancelled", isError: false))
        XCTAssertEqual(
            layout(.addPasskey, passkey: true, state: .cancelled).line,
            .text(key: "proof.add_passkey.cancelled", isError: false))
        XCTAssertEqual(
            layout(.changePassword, passkey: true, state: .cancelled).line,
            .text(key: "proof.change_password.cancelled", isError: false))
        XCTAssertTrue(layout(.addDeviceRequest, passkey: true, state: .cancelled).ready, "back to idle")
    }

    func testThePasskeyAndOfflineLines() {
        XCTAssertEqual(
            layout(state: .passkeyOtherAccount).line, .text(key: "proof.passkey.other_account", isError: true))
        XCTAssertEqual(layout(state: .passkeyFailed).line, .text(key: "proof.passkey.failed", isError: true))
        XCTAssertEqual(layout(state: .offline).line, .text(key: "common.error.offline", isError: true))
        XCTAssertEqual(layout(state: .usernameRefused).line, .usernameRule)
    }

    func testEveryKeyTheBlockNamesIsInTheCatalog() {
        var keys = [
            "auth.password.label", "auth.username.label", "proof.passkey.use", "proof.password.use",
            "proof.passkey.notice", "proof.passkey.other_account", "proof.passkey.failed",
            "recovery.passkey.waiting", "common.error.offline", "common.error.too_many_attempts",
            "common.error.unavailable", "proof.no_password_link",
            PasswordChangeGateCopy.useWordsKey, "common.checking",
            DeleteAccountDialogGate.deleteLabelKey(busy: true),
            DeleteAccountDialogGate.deleteLabelKey(busy: false),
            "proof.know_password_link", "proof.delete.cannot", "proof.reset.cannot.title",
        ]
        for surface in [FreshAuthSurface.reset, .deleteAccount, .addDeviceRequest, .addDeviceScan,
                        .addPasskey, .changePassword] {
            keys.append(surface.wrongPasswordKey)
            keys.append(contentsOf: [surface.helpKey, surface.cancelledKey].compactMap { $0 })
        }
        // ADR-0067 — the recovery-email and save-phrase surfaces: their own
        // helpers, no "cancelled" line.
        for surface in [FreshAuthSurface.recoveryEmail, .savePhrase] {
            keys.append(surface.wrongPasswordKey)
            keys.append(contentsOf: [surface.helpKey].compactMap { $0 })
            XCTAssertNil(surface.cancelledKey)
        }
        XCTAssertEqual(FreshAuthSurface.recoveryEmail.helpKey, "proof.recovery_email.help")
        XCTAssertEqual(FreshAuthSurface.savePhrase.helpKey, "proof.save_phrase.help")
        for key in keys {
            XCTAssertNotEqual(Localization.text(key), key, "\(key) is missing from the catalog")
        }
        XCTAssertNotEqual(
            Localization.text("recovery.error.rate_limited", texts: ["time": "0:30"]),
            "recovery.error.rate_limited")
    }

    /// The per-surface copy table (UX 17.1): every surface names its own
    /// action in the helper and in the line after a dismissed passkey sheet.
    func testThePerSurfaceCopyTable() {
        let table: [(FreshAuthSurface, help: String, wrong: String, cancelled: String)] = [
            (.reset, "restore_identity.reset.password_help",
             "restore_identity.reset.error.wrong_password", "proof.reset.cancelled"),
            (.deleteAccount, "proof.delete.help",
             "proof.delete.wrong_password", "settings.account.delete.error.cancelled"),
            (.addDeviceRequest, "proof.add_device.help",
             "proof.add_device.request.wrong_password", "proof.add_device.request.cancelled"),
            (.addDeviceScan, "proof.add_device.help",
             "proof.add_device.scan.wrong_password", "proof.add_device.scan.cancelled"),
            (.addPasskey, "proof.add_passkey.help",
             "restore_identity.reset.error.wrong_password", "proof.add_passkey.cancelled"),
            (.changePassword, "proof.change_password.help",
             "restore_identity.reset.error.wrong_password", "proof.change_password.cancelled"),
        ]
        for (surface, help, wrong, cancelled) in table {
            XCTAssertEqual(surface.helpKey, help)
            XCTAssertEqual(surface.wrongPasswordKey, wrong)
            XCTAssertEqual(surface.cancelledKey, cancelled)
            XCTAssertEqual(layout(surface, usePassword: true).helpKey, help, "drawn under the field")
        }
        XCTAssertEqual(PasswordChangeGateCopy.useWordsKey, "proof.change_password.use_words")
    }

    // MARK: - The failure mapping

    func testThePasswordFailures() {
        XCTAssertEqual(
            FreshAuthProofModel.passwordFailure(FreshAuthWrongPassword(), countdown: nil), .wrongPassword)
        XCTAssertEqual(
            FreshAuthProofModel.passwordFailure(FreshAuthUsernameRequired(), countdown: nil), .usernameRefused)
        XCTAssertEqual(
            FreshAuthProofModel.passwordFailure(
                AuthClientError.http(
                    status: 429, endpoint: "/login/start",
                    body: #"{"error":"too-many-attempts","retry_after_secs":30}"#),
                countdown: nil),
            .rateLimited(timed: true))
        XCTAssertEqual(
            FreshAuthProofModel.passwordFailure(
                AuthClientError.http(status: 429, endpoint: "/login/start", body: #"{"error":"too-many-attempts"}"#),
                countdown: nil),
            .rateLimited(timed: false))
        XCTAssertEqual(
            FreshAuthProofModel.passwordFailure(
                AuthClientError.network(endpoint: "/login/start", underlying: URLError(.notConnectedToInternet)),
                countdown: nil),
            .offline)
        XCTAssertEqual(
            FreshAuthProofModel.passwordFailure(
                AuthClientError.http(status: 500, endpoint: "/login/start", body: ""), countdown: nil),
            .failed)
    }

    func testThePasskeyFailures() {
        XCTAssertEqual(
            FreshAuthProofModel.passkeyFailure(WebauthnAssertion.AssertionError.cancelled), .cancelled)
        XCTAssertEqual(
            FreshAuthProofModel.passkeyFailure(FreshAuthPasskeyOtherAccount()), .passkeyOtherAccount)
        XCTAssertEqual(
            FreshAuthProofModel.passkeyFailure(
                WebauthnClientError.network(endpoint: "/webauthn/auth/start", underlying: URLError(.timedOut))),
            .offline)
        XCTAssertEqual(
            FreshAuthProofModel.passkeyFailure(
                WebauthnClientError.http(status: 500, endpoint: "/webauthn/auth/finish", body: "")),
            .passkeyFailed)
    }

    // MARK: - The state machine

    private final class Script {
        var live = false
        var usernameKnown = true
        var hasPasskey: Bool? = false
        var passwordError: Error?
        var passkeyError: Error?
        var passwords: [(password: Data, username: String?)] = []
        var passkeyCalls = 0

        @MainActor var ports: FreshAuthProofModel.Ports {
            .init(
                holdsLive: { self.live },
                usernameKnown: { self.usernameKnown },
                hasPasskey: { self.hasPasskey },
                proveWithPassword: { password, username in
                    self.passwords.append((password, username))
                    if let error = self.passwordError { throw error }
                    self.live = true
                },
                proveWithPasskey: {
                    self.passkeyCalls += 1
                    if let error = self.passkeyError { throw error }
                    self.live = true
                })
        }
    }

    private func model(_ script: Script, _ surface: FreshAuthSurface = .reset) -> FreshAuthProofModel {
        FreshAuthProofModel(surface: surface, ports: script.ports, countdown: RetryCountdown(nowMs: { 0 }))
    }

    func testALiveProofAsksForNothing() async {
        let script = Script()
        script.live = true
        let m = model(script)
        await m.opened()
        XCTAssertFalse(m.layout.showsBlock)
        let ok = await m.ensure()
        XCTAssertTrue(ok)
        XCTAssertTrue(script.passwords.isEmpty)
        XCTAssertEqual(script.passkeyCalls, 0)
    }

    func testTheTypedPasswordProvesAndIsCleared() async {
        let script = Script()
        let m = model(script)
        await m.opened()
        m.password = "hunter2"
        let ok = await m.ensure()
        XCTAssertTrue(ok)
        XCTAssertEqual(script.passwords.map(\.password), [Data("hunter2".utf8)])
        XCTAssertNil(script.passwords.first?.username, "the device knows the name")
        XCTAssertEqual(m.password, "", "never kept")
        XCTAssertFalse(m.layout.showsBlock)
    }

    func testAnEmptyFieldSendsNothing() async {
        let script = Script()
        let m = model(script)
        await m.opened()
        let ok = await m.ensure()
        XCTAssertFalse(ok)
        XCTAssertTrue(script.passwords.isEmpty)
    }

    func testAWrongPasswordStaysOnTheSurfaceAndClearsTheField() async {
        let script = Script()
        script.passwordError = FreshAuthWrongPassword()
        let m = model(script, .deleteAccount)
        await m.opened()
        m.password = "nope"
        let ok = await m.ensure()
        XCTAssertFalse(ok)
        XCTAssertEqual(m.state, .wrongPassword)
        XCTAssertEqual(m.password, "")
        XCTAssertEqual(m.layout.line, .text(key: "proof.delete.wrong_password", isError: true))
    }

    func testOfflineKeepsTheFieldsContents() async {
        let script = Script()
        script.passwordError = AuthClientError.network(
            endpoint: "/login/start", underlying: URLError(.notConnectedToInternet))
        let m = model(script)
        await m.opened()
        m.password = "hunter2"
        _ = await m.ensure()
        XCTAssertEqual(m.state, .offline)
        XCTAssertEqual(m.password, "hunter2")
    }

    func testTheTypedUsernameTravelsOnlyWhenTheDeviceKnowsNone() async {
        let script = Script()
        script.usernameKnown = false
        let m = model(script)
        await m.opened()
        m.password = "pw"
        m.username = "dora"
        _ = await m.ensure()
        XCTAssertEqual(script.passwords.first?.username, "dora")
    }

    func testThePasskeyFirstSurfaceOpensTheSheetFromTheMainButton() async {
        let script = Script()
        script.hasPasskey = true
        let m = model(script, .addDeviceRequest)
        await m.opened()
        XCTAssertTrue(m.layout.mainOpensPasskey)
        let ok = await m.ensure()
        XCTAssertTrue(ok)
        XCTAssertEqual(script.passkeyCalls, 1)
        XCTAssertTrue(script.passwords.isEmpty)
    }

    func testACancelledGestureGoesBackToIdleWithItsLine() async {
        let script = Script()
        script.hasPasskey = true
        script.passkeyError = WebauthnAssertion.AssertionError.cancelled
        let m = model(script, .addDeviceRequest)
        await m.opened()
        let ok = await m.ensure()
        XCTAssertFalse(ok)
        XCTAssertEqual(m.state, .cancelled)
        XCTAssertTrue(m.layout.ready, "the button works again")
        XCTAssertEqual(m.layout.line, .text(key: "proof.add_device.request.cancelled", isError: false))
    }

    func testAnotherAccountsPasskeyInstallsNothing() async {
        let script = Script()
        script.hasPasskey = true
        script.passkeyError = FreshAuthPasskeyOtherAccount()
        let m = model(script)
        await m.opened()
        let ok = await m.usePasskey()
        XCTAssertFalse(ok)
        XCTAssertEqual(m.state, .passkeyOtherAccount)
        XCTAssertFalse(m.proofLive)
    }

    func testUseYourPasswordInsteadSwapsTheNoticeForTheField() async {
        let script = Script()
        script.hasPasskey = true
        let m = model(script, .addDeviceScan)
        await m.opened()
        m.setUsePassword(true)
        XCTAssertTrue(m.layout.showsPasswordField)
        m.password = "pw"
        _ = await m.ensure()
        XCTAssertEqual(script.passkeyCalls, 0)
        XCTAssertEqual(script.passwords.count, 1)
    }

    func testAnExpiredProofShowsTheFieldWithNoErrorStyling() async {
        let script = Script()
        script.live = true
        let m = model(script)
        await m.opened()
        script.live = false
        m.proofExpired()
        XCTAssertTrue(m.layout.showsBlock)
        XCTAssertNil(m.layout.line, "nothing went wrong")
    }

    func testClosingTheSurfaceDiscardsWhatWasTyped() async {
        let script = Script()
        script.passwordError = FreshAuthWrongPassword()
        let m = model(script)
        await m.opened()
        m.password = "typed"
        m.username = "dora"
        m.closed()
        XCTAssertEqual(m.password, "")
        XCTAssertEqual(m.username, "")
        XCTAssertEqual(m.state, .idle)
    }

    func testATimedRateLimitStartsTheCountdown() async {
        let script = Script()
        script.passwordError = AuthClientError.http(
            status: 429, endpoint: "/login/start", body: #"{"error":"too-many-attempts","retry_after_secs":30}"#)
        let m = model(script)
        await m.opened()
        m.password = "pw"
        _ = await m.ensure()
        XCTAssertEqual(m.state, .rateLimited(timed: true))
        XCTAssertTrue(m.countdown.waiting)
        XCTAssertEqual(m.layout.line, .countdown)
        XCTAssertFalse(m.layout.ready)
        m.closed()
    }

    // MARK: - The delete dialog (UX 17.3)

    func testTheDeleteDialogsForms() {
        typealias G = DeleteAccountDialogGate
        XCTAssertEqual(G.form(hasPasskey: true, proofLive: false, choseNoPassword: false), .passkey)
        XCTAssertEqual(
            G.form(hasPasskey: true, proofLive: true, choseNoPassword: false), .passkey,
            "a live proof does not skip the gesture")
        XCTAssertEqual(G.form(hasPasskey: false, proofLive: true, choseNoPassword: false), .plain)
        XCTAssertEqual(G.form(hasPasskey: false, proofLive: false, choseNoPassword: false), .block)
        XCTAssertEqual(G.form(hasPasskey: false, proofLive: false, choseNoPassword: true), .cannot)
        XCTAssertEqual(
            G.form(hasPasskey: false, proofLive: true, choseNoPassword: true), .plain,
            "a proof is a proof")
    }

    func testTheDeleteButton() {
        typealias G = DeleteAccountDialogGate
        XCTAssertTrue(G.canDelete(form: .passkey, blockReady: false, busy: false))
        XCTAssertTrue(G.canDelete(form: .plain, blockReady: false, busy: false))
        XCTAssertFalse(G.canDelete(form: .block, blockReady: false, busy: false), "the field is empty")
        XCTAssertTrue(G.canDelete(form: .block, blockReady: true, busy: false))
        XCTAssertFalse(G.canDelete(form: .cannot, blockReady: true, busy: false))
        XCTAssertFalse(G.canDelete(form: .plain, blockReady: true, busy: true))
        XCTAssertFalse(G.showsDelete(form: .cannot), "replaced, not disabled")
        XCTAssertTrue(G.showsDelete(form: .block))
    }

    func testTheDeleteButtonNamesItsWorkWhileDeleting() {
        typealias G = DeleteAccountDialogGate
        XCTAssertEqual(G.deleteLabelKey(busy: false), "settings.account.delete.confirm")
        XCTAssertEqual(G.deleteLabelKey(busy: true), "settings.account.delete.working")
    }

    // MARK: - The passkey row of "Protect your account"

    func testThePasskeyRowFailures() {
        XCTAssertEqual(
            SignupPasskeyStep.failure(WebauthnRegistration.RegistrationError.cancelled), .stay,
            "a dismissed sheet is not an error")
        XCTAssertEqual(
            SignupPasskeyStep.failure(WebauthnClientError.http(
                status: 403, endpoint: "/webauthn/register/start", body: #"{"error":"fresh-auth-required"}"#)),
            .proofNeeded, "the block appears, not a generic error")
        XCTAssertEqual(SignupPasskeyStep.failure(FreshAuthRequired()), .proofNeeded)
        XCTAssertEqual(
            SignupPasskeyStep.failure(WebauthnClientError.http(
                status: 500, endpoint: "/webauthn/register/start", body: "")),
            .error)
        XCTAssertEqual(SignupPasskeyStep.busyTitleKey(checkingProof: true), "common.checking")
        XCTAssertEqual(SignupPasskeyStep.busyTitleKey(checkingProof: false), "recovery.passkey.waiting")
    }

    // MARK: - The add-passkey block

    /// Without a live proof Settings > Passkeys draws the block: the password
    /// field with the add-passkey helper.
    func testTheAddPasskeyBlockWithoutAProof() {
        let l = layout(.addPasskey)
        XCTAssertTrue(l.showsBlock)
        XCTAssertTrue(l.showsPasswordField)
        XCTAssertEqual(l.helpKey, "proof.add_passkey.help")
        XCTAssertFalse(l.ready, "Add waits for the password")
        XCTAssertTrue(layout(.addPasskey, password: "pw").ready)
        XCTAssertFalse(layout(.addPasskey, live: true).showsBlock)
    }
}

/// The local three-word check in front of a password change: the form's
/// budget and its outcome, over fake ports.
@MainActor
final class PasswordThreeWordsModelTests: XCTestCase {

    private var suite: String!

    override func setUp() {
        super.setUp()
        suite = "password-three-words-\(UUID().uuidString)"
    }

    override func tearDown() {
        UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite)
        super.tearDown()
    }

    private final class Script {
        var matches = false
        var throwsUnavailable = false
        var checks: [(positions: Data, words: [String])] = []
        var draws: [[Int]] = [[1, 2, 3], [4, 5, 6], [7, 8, 9], [10, 11, 12], [1, 5, 9], [2, 6, 10]]
        var nowMs: Int64 = 1_700_000_000_000
    }

    private func model(_ script: Script) -> (PasswordThreeWordsModel, ThreeWordsBudgetStore) {
        let budgets = ThreeWordsBudgetStore(defaults: UserDefaults(suiteName: suite)!)
        let model = PasswordThreeWordsModel(
            ports: .init(
                accountIdB64: { "acct" },
                check: { positions, words in
                    script.checks.append((positions, words))
                    if script.throwsUnavailable { throw ThreeWordsError.unavailable }
                    return script.matches
                },
                drawPositions: { script.draws.isEmpty ? [1, 2, 3] : script.draws.removeFirst() },
                nowMs: { script.nowMs }),
            budgets: budgets)
        return (model, budgets)
    }

    func testIncompleteWordsAreNeverAGuess() {
        let script = Script()
        let (m, budgets) = model(script)
        m.enter()
        XCTAssertEqual(m.positions, [1, 2, 3])
        m.entries = ["abandon", "notaword", "ability"]
        XCTAssertFalse(m.complete)
        XCTAssertFalse(m.submit())
        XCTAssertTrue(script.checks.isEmpty)
        XCTAssertEqual(budgets.load(accountIdB64: "acct"), .fresh)
    }

    func testAMatchOpensTheGateAndClearsTheBudget() {
        let script = Script()
        let (m, budgets) = model(script)
        m.enter()
        m.entries = ["abandon", "ability", "able"]
        XCTAssertFalse(m.submit(), "a miss first")
        XCTAssertTrue(m.noMatch)
        XCTAssertEqual(m.positions, [4, 5, 6], "a miss draws new positions")
        XCTAssertEqual(m.triesLeft, ThreeWords.maxMisses - 1)

        script.matches = true
        m.entries = ["abandon", "ability", "able"]
        XCTAssertTrue(m.submit())
        XCTAssertEqual(script.checks.last?.positions, Data([4, 5, 6]))
        XCTAssertEqual(budgets.load(accountIdB64: "acct"), .fresh)
        XCTAssertNil(m.triesLeft)
    }

    func testFiveMissesPauseTheForm() {
        let script = Script()
        let (m, _) = model(script)
        m.enter()
        for _ in 0..<ThreeWords.maxMisses {
            m.entries = ["abandon", "ability", "able"]
            _ = m.submit()
        }
        XCTAssertTrue(m.pause.waiting)
        script.matches = true
        m.entries = ["abandon", "ability", "able"]
        XCTAssertFalse(m.submit(), "nothing is checked during the pause")
        XCTAssertEqual(script.checks.count, ThreeWords.maxMisses)
        m.pause.stop()
    }

    func testADeviceWithoutThePhraseSaysSo() {
        let script = Script()
        script.throwsUnavailable = true
        let (m, budgets) = model(script)
        m.enter()
        m.entries = ["abandon", "ability", "able"]
        XCTAssertFalse(m.submit())
        XCTAssertTrue(m.unavailable)
        XCTAssertEqual(budgets.load(accountIdB64: "acct"), .fresh, "not a wrong guess")
    }
}
