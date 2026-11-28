// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// UX `identity-recovery.md` 19.1a to 19.1d — the required protect step, the
/// landing rule, the last second way in, the authenticator-only note, the
/// "chats depend on your password" notice, and the authenticator setup
/// (19.1b). Pure rules and one model over scripted ports: nothing here
/// reaches a server.
@MainActor
final class ProtectStepTests: XCTestCase {

    private func factors(_ email: Bool, _ authenticator: Bool, _ passkey: Bool) -> AccountFactors {
        AccountFactors(email: email, authenticator: authenticator, passkey: passkey)
    }

    private let none = AccountSeals.passwordOnly
    private let phrase = AccountSeals(phraseSaved: true, passkeyTagsB64: [])
    private let passkeySeal = AccountSeals(phraseSaved: false, passkeyTagsB64: ["dGFn"])

    // MARK: - 19.1a: the step

    func testTheRowsAreEmailAuthenticatorPasskeyAndThePhraseIsApart() {
        let step = ProtectStep.decide(
            offersEmail: true, passkeysSupported: true, done: [], keepsChats: false, leavable: true)
        XCTAssertEqual(step.factors.map(\.row), [.email, .authenticator, .passkey])
        XCTAssertEqual(step.phrase.row, .phrase)
        XCTAssertEqual(step.titleKey, "protect.title")
        XCTAssertNil(step.warning)
        XCTAssertFalse(step.showsPasswordOnlyNotice, "nothing is done yet")
        // The authenticator row is always there; the others only where they
        // can be used.
        let bare = ProtectStep.decide(
            offersEmail: false, passkeysSupported: false, done: [], keepsChats: false, leavable: true)
        XCTAssertEqual(bare.factors.map(\.row), [.authenticator])
        // A row that is done is shown even where it could not be added now.
        let kept = ProtectStep.decide(
            offersEmail: false, passkeysSupported: false, done: [.email, .passkey], keepsChats: false,
            leavable: true)
        XCTAssertEqual(kept.factors.map(\.row), [.email, .authenticator, .passkey])
        XCTAssertEqual(kept.factors.map(\.done), [true, false, true])
    }

    func testContinueIsTheOnlyWayOffAndNeedsOneOfTheThree() {
        func footer(_ done: Set<ProtectStep.Row>) -> ProtectStep.Footer {
            ProtectStep.decide(
                offersEmail: true, passkeysSupported: true, done: done, keepsChats: false, leavable: true).footer
        }
        XCTAssertEqual(footer([]), .continueOn(enabled: false))
        // A saved phrase alone does not enable it.
        XCTAssertEqual(footer([.phrase]), .continueOn(enabled: false))
        for row in [ProtectStep.Row.email, .authenticator, .passkey] {
            XCTAssertEqual(footer([row]), .continueOn(enabled: true), "\(row)")
        }
        // The Settings entry has no footer of its own.
        XCTAssertEqual(
            ProtectStep.decide(
                offersEmail: true, passkeysSupported: true, done: [], keepsChats: false, leavable: false).footer,
            ProtectStep.Footer.none)
        // The landing rule's heading.
        XCTAssertEqual(
            ProtectStep.decide(
                offersEmail: true, passkeysSupported: true, done: [], keepsChats: false, leavable: true,
                required: true).titleKey,
            "protect.title_required")
    }

    func testTheAuthenticatorAloneIsAllowedWithAWarning() {
        func warning(_ done: Set<ProtectStep.Row>) -> ProtectStep.Warning? {
            ProtectStep.decide(
                offersEmail: true, passkeysSupported: true, done: done, keepsChats: false, leavable: true).warning
        }
        XCTAssertEqual(warning([.authenticator]), .authenticatorOnly)
        XCTAssertEqual(warning([.authenticator, .phrase]), .authenticatorOnlyWithPhrase)
        XCTAssertEqual(ProtectStep.Warning.authenticatorOnly.key, "protect.totp_only.warning")
        XCTAssertEqual(ProtectStep.Warning.authenticatorOnlyWithPhrase.key, "protect.totp_only.warning_phrase")
        // It goes as soon as an email or a passkey is added.
        XCTAssertNil(warning([.authenticator, .email]))
        XCTAssertNil(warning([.authenticator, .passkey]))
        XCTAssertNil(warning([]))
        XCTAssertNil(warning([.email]))
    }

    func testTheHonestNoticeStandsOnceAFactorIsDoneAndNothingKeepsTheChats() {
        func notice(_ done: Set<ProtectStep.Row>, keeps: Bool = false) -> Bool {
            ProtectStep.decide(
                offersEmail: true, passkeysSupported: true, done: done, keepsChats: keeps, leavable: true)
                .showsPasswordOnlyNotice
        }
        XCTAssertFalse(notice([]))
        XCTAssertTrue(notice([.email]))
        XCTAssertTrue(notice([.passkey]), "a passkey that gave no secret keeps nothing")
        XCTAssertFalse(notice([.passkey], keeps: true), "a passkey that keeps the chats")
        XCTAssertFalse(notice([.email, .phrase]), "a saved phrase")
        XCTAssertFalse(notice([.phrase]), "no second way in yet")
    }

    func testTheDoneRowsFollowTheAccount() {
        XCTAssertEqual(ProtectStep.done(factors: factors(true, false, true), phraseSaved: true), [.email, .passkey, .phrase])
        XCTAssertEqual(ProtectStep.done(factors: nil, phraseSaved: false), [])
        XCTAssertTrue(ProtectStep.offersEmail(available: true, hasUsername: true))
        XCTAssertFalse(ProtectStep.offersEmail(available: false, hasUsername: true))
        XCTAssertFalse(ProtectStep.offersEmail(available: true, hasUsername: false))
    }

    // MARK: - 19.1c: every account keeps a second way in

    func testTheLandingRule() {
        XCTAssertTrue(FactorLanding.needsStep(hasUsername: true, factors: factors(false, false, false)))
        for some in [factors(true, false, false), factors(false, true, false), factors(false, false, true)] {
            XCTAssertFalse(FactorLanding.needsStep(hasUsername: true, factors: some))
        }
        XCTAssertFalse(FactorLanding.needsStep(hasUsername: false, factors: factors(false, false, false)), "a guest")
        XCTAssertFalse(FactorLanding.needsStep(hasUsername: true, factors: nil), "unknown decides nothing")
    }

    func testTheLastOfTheThreeCannotBeRemoved() {
        XCTAssertTrue(LastFactor.refuses(removing: .email, factors: factors(true, false, false)))
        XCTAssertFalse(LastFactor.refuses(removing: .email, factors: factors(true, true, false)))
        XCTAssertFalse(LastFactor.refuses(removing: .email, factors: factors(false, true, false)), "not the email")
        XCTAssertFalse(LastFactor.refuses(removing: .email, factors: nil), "the server still refuses")
        XCTAssertFalse(LastFactor.refuses(removing: .phrase, factors: factors(true, false, false)))
        // One of two passkeys may go; the last one of the only factor may not.
        XCTAssertTrue(LastFactor.refusesPasskeyRemoval(passkeyCount: 1, factors: factors(false, false, true)))
        XCTAssertFalse(LastFactor.refusesPasskeyRemoval(passkeyCount: 2, factors: factors(false, false, true)))
        XCTAssertFalse(LastFactor.refusesPasskeyRemoval(passkeyCount: 1, factors: factors(true, false, true)))
        XCTAssertEqual(LastFactor.messageKey, "protect.last_factor")

        // The server's refusal, from either client.
        let body = #"{"error":"last-factor"}"#
        XCTAssertTrue(LastFactor.isRefusal(AuthClientError.http(status: 409, endpoint: "/recovery-email", body: body)))
        XCTAssertTrue(LastFactor.isRefusal(
            WebauthnClientError.http(status: 409, endpoint: "/webauthn/credentials/x", body: body)))
        XCTAssertFalse(LastFactor.isRefusal(AuthClientError.http(status: 409, endpoint: "/x", body: #"{"error":"other"}"#)))
        XCTAssertFalse(LastFactor.isRefusal(AuthClientError.http(status: 403, endpoint: "/x", body: body)))
        XCTAssertFalse(LastFactor.isRefusal(URLError(.timedOut)))
    }

    func testTheAuthenticatorOnlyNoteInSettingsAndBeforeARemoval() {
        XCTAssertEqual(
            AuthenticatorOnly.settingsWarning(factors: factors(false, true, false), phraseSaved: false),
            .authenticatorOnly)
        XCTAssertEqual(
            AuthenticatorOnly.settingsWarning(factors: factors(false, true, false), phraseSaved: true),
            .authenticatorOnlyWithPhrase)
        XCTAssertNil(AuthenticatorOnly.settingsWarning(factors: factors(true, true, false), phraseSaved: false))
        XCTAssertNil(AuthenticatorOnly.settingsWarning(factors: nil, phraseSaved: false))

        // A removal that would leave only the authenticator says so first.
        XCTAssertEqual(
            AuthenticatorOnly.removalNoticeKey(removing: .email, factors: factors(true, true, false), phraseSaved: false),
            "protect.totp_only.remove_notice")
        XCTAssertEqual(
            AuthenticatorOnly.removalNoticeKey(removing: .email, factors: factors(true, true, false), phraseSaved: true),
            "protect.totp_only.remove_notice_phrase")
        XCTAssertEqual(
            AuthenticatorOnly.removalNoticeKey(
                removing: .passkey, passkeyCount: 1, factors: factors(false, true, true), phraseSaved: false),
            "protect.totp_only.remove_notice")
        // One of two passkeys: the factor stays.
        XCTAssertNil(AuthenticatorOnly.removalNoticeKey(
            removing: .passkey, passkeyCount: 2, factors: factors(false, true, true), phraseSaved: false))
        // Another way in remains, or there is no authenticator at all.
        XCTAssertNil(AuthenticatorOnly.removalNoticeKey(
            removing: .email, factors: factors(true, true, true), phraseSaved: false))
        XCTAssertNil(AuthenticatorOnly.removalNoticeKey(
            removing: .email, factors: factors(true, false, true), phraseSaved: false))
        XCTAssertNil(AuthenticatorOnly.removalNoticeKey(removing: .email, factors: nil, phraseSaved: false))
    }

    // MARK: - 19.1d: the chats hang on the password alone

    func testWhatKeepsTheChats() {
        XCTAssertEqual(none.keptBy, .passwordOnly)
        XCTAssertEqual(phrase.keptBy, .phrase)
        XCTAssertEqual(passkeySeal.keptBy, .passkey)
        XCTAssertEqual(AccountSeals(phraseSaved: true, passkeyTagsB64: ["dGFn"]).keptBy, .phrase, "the phrase first")
        let id = Data([7, 7, 7])
        let listed = AccountSeals(
            phraseSaved: false, passkeyTagsB64: [resetSealPasskeyTag(credentialId: id).base64EncodedString()])
        XCTAssertTrue(listed.passkeyKeepsChats(credentialId: id))
        XCTAssertFalse(listed.passkeyKeepsChats(credentialId: Data([1])))
    }

    func testTheNoticeAppliesWithASecondWayInAndNothingThatKeepsTheChats() {
        let email = factors(true, false, false)
        XCTAssertTrue(PasswordOnlyNotice.applies(factors: email, seals: none))
        XCTAssertFalse(PasswordOnlyNotice.applies(factors: email, seals: phrase))
        XCTAssertFalse(PasswordOnlyNotice.applies(factors: email, seals: passkeySeal))
        XCTAssertFalse(PasswordOnlyNotice.applies(factors: factors(false, false, false), seals: none), "the step first")
        XCTAssertFalse(PasswordOnlyNotice.applies(factors: email, seals: nil), "never read: nothing is shown")
        XCTAssertFalse(PasswordOnlyNotice.applies(factors: nil, seals: none))

        // Once per device, and not after a key reset.
        XCTAssertTrue(PasswordOnlyNotice.showsDialog(applies: true, closedOnThisDevice: false, afterKeyReset: false))
        XCTAssertFalse(PasswordOnlyNotice.showsDialog(applies: true, closedOnThisDevice: true, afterKeyReset: false))
        XCTAssertFalse(PasswordOnlyNotice.showsDialog(applies: true, closedOnThisDevice: false, afterKeyReset: true))
        XCTAssertFalse(PasswordOnlyNotice.showsDialog(applies: false, closedOnThisDevice: false, afterKeyReset: false))

        // "This passkey signs you in but does not keep your chats."
        XCTAssertTrue(PasswordOnlyNotice.showsPasskeyNoSeal(factors: factors(false, false, true), seals: none))
        XCTAssertFalse(PasswordOnlyNotice.showsPasskeyNoSeal(factors: factors(false, false, true), seals: passkeySeal))
        XCTAssertFalse(PasswordOnlyNotice.showsPasskeyNoSeal(factors: email, seals: none))
    }

    func testTheSettingsStatusLineSaysWhatKeepsTheChats() {
        let on = RecoveryEmailStatus(available: true, enabled: true, totp: false)
        XCTAssertEqual(ProtectionRows.decide(on, seals: phrase).statusKey, "protect.status.phrase")
        XCTAssertEqual(ProtectionRows.decide(on, seals: passkeySeal).statusKey, "protect.status.passkey_seal")
        let only = ProtectionRows.decide(on, seals: none)
        XCTAssertEqual(only.statusKey, "protect.status.password_only")
        XCTAssertTrue(only.statusIsWarning)
        XCTAssertFalse(ProtectionRows.decide(on, seals: phrase).statusIsWarning)
        // Unknown seals warn about nothing.
        XCTAssertFalse(ProtectionRows.decide(on, seals: nil).statusIsWarning)
        // `Show recovery phrase` and the three words only once it was saved.
        XCTAssertEqual(ProtectionRows.decide(on, seals: phrase).phrase, .show)
        XCTAssertTrue(ProtectionRows.decide(on, seals: phrase).showsThreeWords)
        XCTAssertEqual(only.phrase, .save)
        XCTAssertFalse(only.showsThreeWords)
        // A mailed code never brings the keys: Restore your keys offers the
        // phrase, and no email row.
        XCTAssertTrue(RestoreKeysRows.decide().showsPhrase)
    }

    // MARK: - 19.1b: the authenticator setup

    func testTheKeyIsWrittenInGroupsOfFourAndTheSixthDigitChecks() {
        XCTAssertEqual(AuthenticatorSetup.groupedKey("ABCDEFGHIJ"), "ABCD EFGH IJ")
        XCTAssertEqual(AuthenticatorSetup.groupedKey("AB CD\nEFGH"), "ABCD EFGH")
        XCTAssertEqual(AuthenticatorSetup.groupedKey(""), "")
        XCTAssertEqual(AuthenticatorSetup.digits(" 123 456 789"), "123456")
        XCTAssertEqual(AuthenticatorSetup.digits("12a3"), "123")
        XCTAssertNil(AuthenticatorSetup.completeCode("12345"))
        XCTAssertEqual(AuthenticatorSetup.completeCode("123 456"), "123456")
        // Digits of another script are not a code the server would take.
        XCTAssertNil(AuthenticatorSetup.completeCode("١٢٣٤٥٦"))
    }

    func testEveryAnswerOfTheTwoRoutes() {
        func http(_ status: Int, _ body: String = "{}") -> Error {
            AuthClientError.http(status: status, endpoint: "/totp/enroll/finish", body: body)
        }
        XCTAssertEqual(AuthenticatorSetup.failure(http(401)), .wrongCode)
        XCTAssertEqual(AuthenticatorSetup.failure(http(409)), .alreadyEnabled)
        XCTAssertEqual(AuthenticatorSetup.failure(http(403, #"{"error":"fresh-auth-required"}"#)), .proofNeeded)
        XCTAssertEqual(AuthenticatorSetup.failure(FreshAuthRequired()), .proofNeeded)
        XCTAssertEqual(
            AuthenticatorSetup.failure(http(429, #"{"error":"too-many-attempts","retry_after_secs":40}"#)),
            .limited(retryAfterSecs: 40))
        XCTAssertEqual(AuthenticatorSetup.failure(http(429)), .limited(retryAfterSecs: nil))
        XCTAssertEqual(
            AuthenticatorSetup.failure(AuthClientError.network(endpoint: "/x", underlying: URLError(.timedOut))),
            .offline)
        XCTAssertEqual(AuthenticatorSetup.failure(http(503)), .failed)
        XCTAssertEqual(AuthenticatorSetup.failure(http(500)), .failed)
    }

    private final class SetupWorld {
        var grant: String? = "grant"
        var startAnswers: [Result<(String, String), Error>] = []
        var finishErrors: [Error?] = []
        var started: [String] = []
        var finished: [(code: String, grant: String)] = []
        var now: Int64 = 1_000
    }

    private func setup(_ world: SetupWorld) -> AuthenticatorSetupModel {
        AuthenticatorSetupModel(ports: .init(
            liveGrant: { world.grant },
            start: { grant in
                world.started.append(grant)
                let next = world.startAnswers.isEmpty
                    ? .success(("otpauth://totp/Lakko:alice?secret=KEY\(world.started.count)", "KEY\(world.started.count)"))
                    : world.startAnswers.removeFirst()
                let answer = try next.get()
                return (otpauthUri: answer.0, secret: answer.1)
            },
            finish: { code, grant in
                world.finished.append((code, grant))
                if !world.finishErrors.isEmpty, let error = world.finishErrors.removeFirst() { throw error }
            },
            nowMs: { world.now }))
    }

    private func http(_ status: Int, _ body: String = "{}") -> Error {
        AuthClientError.http(status: status, endpoint: "/totp/enroll", body: body)
    }

    func testWithALiveProofTheKeyIsAskedForAndTheSixthDigitSetsItUp() async {
        let world = SetupWorld()
        let model = setup(world)
        XCTAssertEqual(model.phase, .starting, "a spinner where the QR code goes")
        await model.opened()
        XCTAssertEqual(model.phase, .ready)
        XCTAssertEqual(model.secret, "KEY1")
        XCTAssertTrue(model.otpauthUri.hasPrefix("otpauth://"))
        XCTAssertEqual(world.started, ["grant"])

        model.code = "12345"
        model.codeChanged()
        XCTAssertEqual(world.finished.count, 0, "five digits check nothing")
        await model.check("123456")
        XCTAssertEqual(world.finished.map(\.code), ["123456"])
        XCTAssertEqual(world.finished.first?.grant, "grant")
        XCTAssertEqual(model.phase, .done)
        XCTAssertEqual(model.code, "")
    }

    func testAWrongCodeClearsTheDigitsAndKeepsTheKey() async {
        let world = SetupWorld()
        world.finishErrors = [http(401), nil]
        let model = setup(world)
        await model.opened()
        await model.check("000000")
        XCTAssertEqual(model.phase, .ready)
        XCTAssertEqual(model.lineKey, "settings.recovery.totp.error.wrong")
        XCTAssertEqual(model.code, "")
        XCTAssertEqual(model.secret, "KEY1", "the same key: the next code of the same setup")
        XCTAssertTrue(model.fieldEnabled)
        await model.check("123456")
        XCTAssertEqual(model.phase, .done)
        XCTAssertEqual(world.started.count, 1)
    }

    func testTooManyTriesDisableTheFieldUntilTheWaitIsOver() async {
        let world = SetupWorld()
        world.finishErrors = [http(429, #"{"error":"too-many-attempts","retry_after_secs":30}"#)]
        let model = setup(world)
        await model.opened()
        await model.check("000000")
        XCTAssertEqual(model.phase, .ready)
        XCTAssertTrue(model.wait.waiting)
        XCTAssertFalse(model.fieldEnabled)
        model.wait.stop()
        XCTAssertTrue(model.fieldEnabled)
    }

    func testOfflineKeepsTheQrAndAnyOtherFailureStartsOverWithANewKey() async {
        let world = SetupWorld()
        world.finishErrors = [
            AuthClientError.network(endpoint: "/x", underlying: URLError(.notConnectedToInternet)), http(500),
        ]
        let model = setup(world)
        await model.opened()
        await model.check("111111")
        XCTAssertEqual(model.phase, .ready, "the QR code and the key stay")
        XCTAssertEqual(model.lineKey, "common.error.offline")
        XCTAssertEqual(model.secret, "KEY1")

        await model.check("222222")
        XCTAssertEqual(model.phase, .failed)
        XCTAssertEqual(model.lineKey, "settings.recovery.totp.error.generic")
        // `Try again` starts over with a NEW key: the one shown is never used.
        await model.begin()
        XCTAssertEqual(model.phase, .ready)
        XCTAssertEqual(model.secret, "KEY2")
        XCTAssertNil(model.lineKey)
    }

    func testAnAccountThatAlreadyHasOneAndAProofThatRanOut() async {
        let had = SetupWorld()
        had.startAnswers = [.failure(http(409))]
        let already = setup(had)
        await already.opened()
        XCTAssertEqual(already.phase, .alreadyEnabled)

        // No live proof (Settings): the block first, nothing is asked.
        let none = SetupWorld()
        none.grant = nil
        let blocked = setup(none)
        XCTAssertEqual(blocked.phase, .needsProof)
        await blocked.opened()
        XCTAssertEqual(none.started, [])
        XCTAssertEqual(blocked.phase, .needsProof)
        // The password was proven: the setup starts.
        none.grant = "grant-2"
        await blocked.begin()
        XCTAssertEqual(blocked.phase, .ready)
        XCTAssertEqual(none.started, ["grant-2"])

        // The proof ran out while the key was on screen: the block appears
        // with no error line, and the code is not sent.
        none.grant = nil
        await blocked.check("123456")
        XCTAssertEqual(blocked.phase, .needsProof)
        XCTAssertNil(blocked.lineKey)
        XCTAssertEqual(none.finished.count, 0)

        // The server says so on the way.
        let expired = SetupWorld()
        expired.finishErrors = [http(403, #"{"error":"fresh-auth-required"}"#)]
        let model = setup(expired)
        await model.opened()
        await model.check("123456")
        XCTAssertEqual(model.phase, .needsProof)
    }

    func testTheProofBlockOfTheSetupHasItsOwnHelper() {
        XCTAssertEqual(FreshAuthSurface.addAuthenticator.helpKey, "proof.add_totp.help")
        XCTAssertNil(FreshAuthSurface.addAuthenticator.cancelledKey)
        XCTAssertNotEqual(Localization.text("proof.add_totp.help"), "proof.add_totp.help")
    }
}
