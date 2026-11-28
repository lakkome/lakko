// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0073, amended 2026-11-26 / UX `identity-recovery.md` 15.1b and 15.1c —
/// the reset's second factor: what the panel draws, the answers of the
/// factor routes, the grant's kind, and the section's state machine over
/// scripted ports. Nothing here reaches a server.
@MainActor
final class ResetFactorTests: XCTestCase {

    private func factors(_ email: Bool, _ authenticator: Bool, _ passkey: Bool) -> AccountFactors {
        AccountFactors(email: email, authenticator: authenticator, passkey: passkey)
    }

    // MARK: - What the panel draws

    func testTheRowsAreTheFactorsTheAccountHasInTheirFixedOrder() {
        XCTAssertEqual(
            ResetFactorSection.rows(factors: factors(true, true, true), passkeysSupported: true),
            [.authenticator, .email, .passkey])
        // A row the account does not have is absent, never disabled.
        XCTAssertEqual(ResetFactorSection.rows(factors: factors(true, false, false), passkeysSupported: true), [.email])
        // A passkey this device cannot use is no row.
        XCTAssertEqual(ResetFactorSection.rows(factors: factors(false, false, true), passkeysSupported: false), [])
    }

    func testTheSectionAsksForThePasswordUnlessAPasswordProofIsLive() {
        XCTAssertEqual(
            ResetFactorSection.form(
                factors: factors(false, true, false), passkeysSupported: true, passwordProofLive: false,
                choseNone: false),
            .section(rows: [.authenticator], showsPassword: true, titleKey: "proof.reset.factor.title"))
        XCTAssertEqual(
            ResetFactorSection.form(
                factors: factors(false, true, false), passkeysSupported: true, passwordProofLive: true,
                choseNone: false),
            .section(rows: [.authenticator], showsPassword: false, titleKey: "proof.reset.factor.title_no_password"))
        XCTAssertEqual(
            ResetFactorSection.form(factors: nil, passkeysSupported: true, passwordProofLive: false, choseNone: false),
            .loading, "nothing is drawn on a guess")
    }

    func testThePanelThatCannotReset() {
        // None of the three: no way to the section, so no Back.
        XCTAssertEqual(
            ResetFactorSection.form(
                factors: factors(false, false, false), passkeysSupported: true, passwordProofLive: true,
                choseNone: false),
            .cannotReset(bodyKey: "proof.reset.no_factor.body_none", showsBack: false))
        // Only a passkey, on a device that cannot use one.
        XCTAssertEqual(
            ResetFactorSection.form(
                factors: factors(false, false, true), passkeysSupported: false, passwordProofLive: true,
                choseNone: false),
            .cannotReset(bodyKey: "proof.reset.no_factor.body", showsBack: false))
        // `I cannot use any of these`: the panel, with Back to the section.
        XCTAssertEqual(
            ResetFactorSection.form(
                factors: factors(true, true, false), passkeysSupported: true, passwordProofLive: false,
                choseNone: true),
            .cannotReset(bodyKey: "proof.reset.no_factor.body", showsBack: true))
    }

    func testResetNeedsAConfirmedFactorAndTheWord() {
        let section = ResetFactorSection.Form.section(rows: [.email], showsPassword: false, titleKey: "t")
        XCTAssertTrue(ResetFactorSection.canReset(form: section, factorConfirmed: true, confirmation: " RESET "))
        XCTAssertFalse(ResetFactorSection.canReset(form: section, factorConfirmed: false, confirmation: "RESET"))
        XCTAssertFalse(ResetFactorSection.canReset(form: section, factorConfirmed: true, confirmation: "reset"))
        XCTAssertFalse(ResetFactorSection.canReset(
            form: .cannotReset(bodyKey: "b", showsBack: true), factorConfirmed: true, confirmation: "RESET"),
            "there is no reset button in the panel")
        XCTAssertFalse(ResetFactorSection.canReset(form: .loading, factorConfirmed: true, confirmation: "RESET"))
    }

    func testAConfirmationLastsUntilItsExpiry() {
        let factor = ResetFactorConfirmation(kind: .email, grant: "f", expiresAtMs: 1_000)
        XCTAssertTrue(factor.live(nowMs: 999))
        XCTAssertFalse(factor.live(nowMs: 1_000))
        XCTAssertEqual(ResetFactorKind.authenticator.doneKey, "proof.reset.factor.done.totp")
        XCTAssertEqual(ResetFactorKind.email.doneKey, "proof.reset.factor.done.email")
        XCTAssertEqual(ResetFactorKind.passkey.doneKey, "proof.reset.factor.done.passkey")
    }

    // MARK: - The routes' answers, and the grant's kind

    func testEveryAnswerOfTheFactorRoutes() {
        func of(_ status: Int, _ code: String = "x", retry: Int? = nil) -> ResetFactorRefusal {
            let retryField = retry.map { #","retry_after_secs":\#($0)"# } ?? ""
            return ResetFactorRefusal.of(status: status, body: #"{"error":"\#(code)"\#(retryField)}"#)
        }
        XCTAssertEqual(of(401, "factor-refused"), .refused)
        XCTAssertEqual(of(403, "fresh-auth-required"), .passwordRequired)
        XCTAssertEqual(of(403, "reset-factor-required"), .factorRequired)
        XCTAssertEqual(of(410, "code-expired"), .codeExpired)
        XCTAssertEqual(of(429, "email-rate-limited", retry: 30), .lettersLimited(retryAfterSecs: 30))
        XCTAssertEqual(of(429, "too-many-attempts", retry: 60), .tooManyAttempts(retryAfterSecs: 60))
        XCTAssertEqual(of(429), .tooManyAttempts(retryAfterSecs: nil))
        XCTAssertEqual(of(502, "mail-send-failed"), .mailSendFailed)
        XCTAssertEqual(of(503, "recovery-email-disabled"), .unavailable)
        XCTAssertEqual(of(503, "rate-limiter-unavailable"), .failed)
        XCTAssertEqual(of(500), .failed)
        XCTAssertEqual(ResetFactorRefusal.of(FreshAuthRequired()), .passwordRequired)
        XCTAssertEqual(
            ResetFactorRefusal.of(AuthClientError.network(endpoint: "/x", underlying: URLError(.timedOut))), .offline)
        XCTAssertTrue(ResetFactorRefusal.isFactorRequired(AuthClientError.http(
            status: 403, endpoint: "/account/keys/reset/start", body: #"{"error":"reset-factor-required"}"#)))
        XCTAssertFalse(ResetFactorRefusal.isFactorRequired(AuthClientError.http(
            status: 403, endpoint: "/x", body: #"{"error":"fresh-auth-required"}"#)))
    }

    /// Only a password sign-in's grant opens the factor routes: a passkey
    /// sign-in does not stand in for the password on the reset.
    func testTheGrantSaysHowItWasMinted() {
        let password = FreshAuthGrant(grant: "k1.YWNjdA.3.p.1700000000000.dGFn", expiresAtMs: 10)
        let passkey = FreshAuthGrant(grant: "k1.YWNjdA.3.w.1700000000000.dGFn", expiresAtMs: 10)
        XCTAssertEqual(password.method, "p")
        XCTAssertTrue(password.isPasswordMinted)
        XCTAssertEqual(passkey.method, "w")
        XCTAssertFalse(passkey.isPasswordMinted)
        for other in ["", "grant-1", "k1.a.3.1700.tag", "k2.a.3.p.1700.tag", "k1.a.3.pp.1700.tag"] {
            XCTAssertNil(FreshAuthGrant(grant: other, expiresAtMs: 10).method, other)
            XCTAssertFalse(FreshAuthGrant(grant: other, expiresAtMs: 10).isPasswordMinted, other)
        }

        var holder = FreshAuthHolder()
        holder.put(passkey, accountIdB64: "acct")
        XCTAssertTrue(holder.holdsLive(accountIdB64: "acct", nowMs: 1))
        XCTAssertFalse(holder.holdsLivePassword(accountIdB64: "acct", nowMs: 1))
        XCTAssertNil(holder.livePassword(accountIdB64: "acct", nowMs: 1))
        holder.put(password, accountIdB64: "acct")
        XCTAssertTrue(holder.holdsLivePassword(accountIdB64: "acct", nowMs: 1))
        XCTAssertEqual(holder.livePassword(accountIdB64: "acct", nowMs: 1), password.grant)
        XCTAssertNil(holder.livePassword(accountIdB64: "acct", nowMs: 10), "expired")
        XCTAssertNil(holder.livePassword(accountIdB64: "other", nowMs: 1), "another account's")
    }

    func testTheWireOfTheFactorRoutes() throws {
        func json<T: Encodable>(_ value: T) throws -> [String: String] {
            try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(value)) as? [String: String])
        }
        XCTAssertEqual(try json(ResetFactorTotpRequest(freshGrant: "g", code: "123456")), [
            "fresh_grant": "g", "code": "123456"])
        XCTAssertEqual(try json(ResetFactorEmailFinishRequest(freshGrant: "g", challengeId: "ch", code: "1")), [
            "fresh_grant": "g", "challenge_id": "ch", "code": "1"])
        XCTAssertEqual(try json(ResetFactorPasskeyRequest(freshGrant: "g")), ["fresh_grant": "g"])
        XCTAssertEqual(try json(OwnedChannelsSecretRequest(resetGrant: "f")), ["reset_grant": "f"])
        let answer = try JSONDecoder().decode(
            ResetFactorGrantResponse.self,
            from: Data(#"{"reset_grant":"f1.x","reset_grant_expires_at_ms":99}"#.utf8))
        XCTAssertEqual(answer.resetGrant, "f1.x")
        XCTAssertEqual(answer.resetGrantExpiresAtMs, 99)
    }

    // MARK: - The section's state machine

    private final class World {
        var factors: AccountFactors? = AccountFactors(email: true, authenticator: true, passkey: true)
        var passwordProofLive = false
        var usernameKnown = true
        var passwordError: Error?
        var totpAnswers: [Result<ResetFactorConfirmation, Error>] = []
        var emailStart: Result<EmailChallenge, Error> = .success(
            EmailChallenge(challengeId: "ch", resendAfterSecs: 45, expiresInSecs: 600))
        var emailAnswers: [Result<ResetFactorConfirmation, Error>] = []
        var passkeyAnswer: Result<AuthRepository.ResetPasskeyFactor, Error>?
        /// Whether the proof built for a factor is sealed, per kind.
        var sealedKinds: Set<ResetFactorKind> = []
        var announcesPasskey = false
        var calls: [String] = []
        var proved: [(password: Data, username: String?)] = []
        var emailStarted: [(address: String, username: String?)] = []
        var now: Int64 = 1_000
    }

    private func confirmation(_ kind: ResetFactorKind, expires: Int64 = 601_000) -> ResetFactorConfirmation {
        ResetFactorConfirmation(kind: kind, grant: "f1.\(kind.rawValue)", expiresAtMs: expires)
    }

    private func http(_ status: Int, _ code: String, retry: Int? = nil) -> AuthClientError {
        let retryField = retry.map { #","retry_after_secs":\#($0)"# } ?? ""
        return .http(status: status, endpoint: "/account/keys/reset/factor", body: #"{"error":"\#(code)"\#(retryField)}"#)
    }

    private func model(_ world: World) -> ResetFactorModel {
        ResetFactorModel(ports: .init(
            factors: { world.factors },
            passwordProofLive: { world.passwordProofLive },
            usernameKnown: { world.usernameKnown },
            provePassword: { password, username in
                world.calls.append("password")
                world.proved.append((password, username))
                if let error = world.passwordError { throw error }
                world.passwordProofLive = true
            },
            confirmTotp: { _ in
                world.calls.append("totp")
                return try (world.totpAnswers.isEmpty
                    ? .success(self.confirmation(.authenticator)) : world.totpAnswers.removeFirst()).get()
            },
            startEmail: { address, username in
                world.calls.append("email-start")
                world.emailStarted.append((address, username))
                return try world.emailStart.get()
            },
            confirmEmail: { _, _ in
                world.calls.append("email-finish")
                return try (world.emailAnswers.isEmpty
                    ? .success(self.confirmation(.email)) : world.emailAnswers.removeFirst()).get()
            },
            confirmPasskey: {
                world.calls.append("passkey")
                return try (world.passkeyAnswer ?? .success(AuthRepository.ResetPasskeyFactor(
                    confirmation: self.confirmation(.passkey), credentialId: Data([1]),
                    prfOutput: Data(repeating: 3, count: 32)))).get()
            },
            proof: { factor, _ in
                let sealed: ResetSealPreview.Sealed? = world.sealedKinds.contains(factor.kind)
                    ? try? ResetSealPreview.Sealed(
                        key: ResetSealKey.fromPasswordExportKey(exportKey: Data(repeating: 1, count: 64)),
                        announcementSeq: 1, fromGeneration: 1)
                    : nil
                return KeyResetProof(
                    factor: factor, sealed: sealed, announcesPasskey: world.announcesPasskey,
                    ownedChannelsLock: nil, passwordExportKey: nil)
            },
            normalize: { try normalizeRecoveryEmail(address: $0) },
            nowMs: { world.now }))
    }

    func testThePasswordIsCheckedFirstAndAWrongOneUsesUpNoCode() async {
        let world = World()
        world.passwordError = FreshAuthWrongPassword()
        let model = model(world)
        await model.opened()
        XCTAssertEqual(
            model.form, .section(rows: [.authenticator, .email, .passkey], showsPassword: true,
                                 titleKey: "proof.reset.factor.title"))

        model.choose(.authenticator)
        model.password = "wrong"
        model.totpCode = "123456"
        await model.confirmTotp()
        XCTAssertEqual(world.calls, ["password"], "the factor is not tried")
        XCTAssertEqual(model.passwordLine, .wrongPassword)
        XCTAssertNil(model.confirmed)
        XCTAssertFalse(model.canReset(confirmation: "RESET"))

        // No password typed at all: the same line, nothing sent.
        let empty = World()
        let blank = self.model(empty)
        await blank.opened()
        blank.totpCode = "123456"
        await blank.confirmTotp()
        XCTAssertEqual(empty.calls, [])
        XCTAssertEqual(blank.passwordLine, .wrongPassword)
    }

    func testAnAuthenticatorCodeConfirmsAndTheSectionCollapsesToOneLine() async {
        let world = World()
        world.sealedKinds = [.authenticator]
        let model = model(world)
        await model.opened()
        model.choose(.authenticator)
        XCTAssertEqual(model.expanded, .authenticator)
        model.password = "correct horse"
        model.totpCode = "123 456"
        await model.confirmTotp()

        XCTAssertEqual(world.calls, ["password", "totp"])
        XCTAssertEqual(world.proved.first?.password, Data("correct horse".utf8))
        XCTAssertNil(world.proved.first?.username ?? nil, "the device knows the name")
        XCTAssertEqual(model.password, "", "never kept after it was used")
        XCTAssertEqual(model.confirmed?.kind, .authenticator)
        XCTAssertNil(model.expanded)
        XCTAssertEqual(model.sealLine, .sealed)
        XCTAssertTrue(model.canReset(confirmation: "RESET"))
        XCTAssertFalse(model.canReset(confirmation: ""))
        // A confirmed section does not open another row.
        model.choose(.email)
        XCTAssertNil(model.expanded)

        // Ten minutes later: the section opens again as it was, no error.
        world.now = 601_000
        XCTAssertNil(model.confirmed)
        XCTAssertNil(model.sealLine)
        XCTAssertFalse(model.canReset(confirmation: "RESET"))
        model.confirmationExpired()
        XCTAssertNil(model.proof)
        XCTAssertNil(model.line)
    }

    func testAWrongAuthenticatorCodeAndTheWait() async {
        let world = World()
        world.passwordProofLive = true
        world.totpAnswers = [
            .failure(http(401, "factor-refused")),
            .failure(http(429, "too-many-attempts", retry: 30)),
        ]
        let model = model(world)
        await model.opened()
        model.totpCode = "111111"
        await model.confirmTotp()
        XCTAssertEqual(world.calls, ["totp"], "a live password proof: nothing is asked")
        XCTAssertEqual(model.line, .wrongTotp)
        XCTAssertEqual(model.totpCode, "", "the digits clear")

        model.totpCode = "222222"
        await model.confirmTotp()
        XCTAssertTrue(model.totpWait.waiting, "the field is disabled until zero")
        XCTAssertNil(model.line)
        // While the wait runs the sixth digit sends nothing.
        model.totpCode = "333333"
        model.totpCodeChanged()
        await Task.yield()
        XCTAssertEqual(world.calls, ["totp", "totp"])
    }

    func testTheEmailRowSendsALetterThenChecksItsCode() async {
        let world = World()
        world.passwordProofLive = true
        world.emailAnswers = [.failure(http(401, "factor-refused"))]
        let model = model(world)
        await model.opened()
        model.choose(.email)

        model.address = "not an address"
        XCTAssertFalse(model.addressValid)
        await model.sendEmailCode()
        XCTAssertEqual(world.calls, [])
        XCTAssertEqual(model.line, .emailAddressInvalid)

        model.address = " Alice@Example.com "
        await model.sendEmailCode()
        XCTAssertEqual(world.emailStarted.first?.address, "alice@example.com")
        XCTAssertTrue(model.emailSent, "the same line whatever the address was")
        XCTAssertTrue(model.emailResend.waiting)

        model.emailCode = "000000"
        await model.confirmEmailCode()
        XCTAssertEqual(model.line, .emailWrongCode)
        XCTAssertEqual(model.emailCode, "")
        model.emailCode = "123456"
        await model.confirmEmailCode()
        XCTAssertEqual(model.confirmed?.kind, .email)
        XCTAssertEqual(model.sealLine, .unsealed(offersPasskey: false))

        // `Use a different address` starts the row over.
        let other = World()
        other.passwordProofLive = true
        let again = self.model(other)
        await again.opened()
        again.address = "a@b.example"
        await again.sendEmailCode()
        again.useOtherAddress()
        XCTAssertFalse(again.emailSent)
        XCTAssertEqual(again.address, "")
    }

    func testTheEmailRowsLimitsAndFailures() async {
        func run(_ error: AuthClientError, finish: Bool) async -> ResetFactorModel {
            let world = World()
            world.passwordProofLive = true
            if finish { world.emailAnswers = [.failure(error)] } else { world.emailStart = .failure(error) }
            let model = self.model(world)
            await model.opened()
            model.address = "a@b.example"
            await model.sendEmailCode()
            if finish {
                model.emailCode = "111111"
                await model.confirmEmailCode()
            }
            return model
        }
        let limited = await run(http(429, "email-rate-limited", retry: 60), finish: false)
        XCTAssertTrue(limited.emailWait.waiting)
        XCTAssertFalse(limited.emailSent)
        let failed = await run(http(502, "mail-send-failed"), finish: false)
        XCTAssertEqual(failed.line, .emailSendFailed)
        let down = await run(http(503, "recovery-email-disabled"), finish: false)
        XCTAssertEqual(down.line, .emailUnavailable)
        // Five wrong codes: a new letter is needed.
        let usedUp = await run(http(429, "too-many-attempts", retry: 30), finish: true)
        XCTAssertEqual(usedUp.line, .emailUsedUp)
        XCTAssertFalse(usedUp.emailSent)
        let expired = await run(http(410, "code-expired"), finish: true)
        XCTAssertEqual(expired.line, .emailExpired)
        XCTAssertFalse(expired.emailSent)
    }

    func testAPasskeyConfirmsAndACancelledSheetIsNotAnError() async {
        let world = World()
        world.passwordProofLive = true
        world.passkeyAnswer = .failure(WebauthnAssertion.AssertionError.cancelled)
        let model = model(world)
        await model.opened()
        await model.usePasskey()
        XCTAssertEqual(model.line, .passkeyCancelled)
        XCTAssertFalse(ResetFactorLine.passkeyCancelled.isError)
        XCTAssertNil(model.confirmed)

        world.passkeyAnswer = nil
        world.sealedKinds = [.passkey]
        await model.usePasskey()
        XCTAssertEqual(model.confirmed?.kind, .passkey)
        XCTAssertEqual(model.sealLine, .sealed)
        XCTAssertFalse(model.passkeySheet)
    }

    /// UX 15.1c — an unsealed line offers a passkey that keeps the chats;
    /// using it replaces the confirmation with a sealed one.
    func testAnUnsealedConfirmationCanBeMadeSealedWithAPasskey() async {
        let world = World()
        world.passwordProofLive = true
        world.announcesPasskey = true
        world.sealedKinds = [.passkey]
        let model = model(world)
        await model.opened()
        model.totpCode = "123456"
        await model.confirmTotp()
        XCTAssertEqual(model.sealLine, .unsealed(offersPasskey: true))

        await model.usePasskey()
        XCTAssertEqual(model.confirmed?.kind, .passkey)
        XCTAssertEqual(model.sealLine, .sealed)
    }

    func testAPasswordProofThatRanOutOnTheWayBringsTheFieldBack() async {
        let world = World()
        world.passwordProofLive = true
        world.totpAnswers = [.failure(http(403, "fresh-auth-required"))]
        let model = model(world)
        await model.opened()
        guard case .section(_, false, _) = model.form else { return XCTFail("\(model.form)") }
        model.totpCode = "123456"
        await model.confirmTotp()
        XCTAssertEqual(world.calls, ["totp"])
        XCTAssertNil(model.line, "nothing went wrong")
        XCTAssertNil(model.confirmed)
        // The password is asked for again before the next try.
        guard case .section(_, true, _) = model.form else { return XCTFail("\(model.form)") }
    }

    func testWithNoUsernameOnTheDeviceItIsAskedForWithThePassword() async {
        let world = World()
        world.usernameKnown = false
        let model = model(world)
        await model.opened()
        model.password = "pw"
        model.totpCode = "123456"
        await model.confirmTotp()
        XCTAssertEqual(world.calls, [])
        XCTAssertEqual(model.passwordLine, .usernameNeeded)

        model.username = "alice"
        model.totpCode = "123456"
        await model.confirmTotp()
        XCTAssertEqual(world.proved.first?.username ?? nil, "alice")
        XCTAssertEqual(model.confirmed?.kind, .authenticator)
    }

    func testICannotUseAnyOfTheseAndBackAndClosing() async {
        let world = World()
        world.factors = AccountFactors(email: false, authenticator: true, passkey: false)
        let model = model(world)
        await model.opened()
        model.chooseNone()
        XCTAssertEqual(model.form, .cannotReset(bodyKey: "proof.reset.no_factor.body", showsBack: true))
        XCTAssertFalse(model.canReset(confirmation: "RESET"))
        model.backToSection()
        guard case .section = model.form else { return XCTFail("\(model.form)") }

        model.password = "pw"
        model.totpCode = "12"
        model.choose(.authenticator)
        model.closed()
        XCTAssertEqual(model.password, "")
        XCTAssertEqual(model.totpCode, "")
        XCTAssertNil(model.expanded)
        XCTAssertNil(model.proof)
    }

    func testEveryLineOfTheSectionHasAString() {
        let lines: [ResetFactorLine] = [
            .wrongPassword, .usernameNeeded, .wrongTotp, .limited, .emailAddressInvalid, .emailWrongCode,
            .emailUsedUp, .emailExpired, .emailSendFailed, .emailUnavailable, .passkeyCancelled,
            .passkeyOtherAccount, .passkeyFailed, .offline, .failed,
        ]
        for line in lines {
            XCTAssertNotEqual(Localization.text(line.key), line.key, "\(line)")
        }
        for key in [
            "proof.reset.factor.heading", "proof.reset.factor.title", "proof.reset.factor.title_no_password",
            "proof.reset.factor.totp", "proof.reset.factor.email", "proof.reset.factor.passkey",
            "proof.reset.factor.none", "proof.reset.factor.totp.prompt", "proof.reset.factor.passkey.use",
            "proof.reset.factor.done.totp", "proof.reset.factor.done.email", "proof.reset.factor.done.passkey",
            "proof.reset.no_factor.title", "proof.reset.no_factor.body", "proof.reset.no_factor.body_none",
        ] {
            XCTAssertNotEqual(Localization.text(key), key, key)
        }
        XCTAssertTrue(Localization.text(
            "proof.reset.factor.email.sent", texts: ["username": "alice"]).contains("alice"))
    }
}
