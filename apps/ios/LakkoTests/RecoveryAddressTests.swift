// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

// ADR-0075, amended 2026-10-13 — the account's devices keep the recovery
// address and put the email vault back by themselves; and a guest chooses a
// username and its first password. Scripted ports only: nothing here
// reaches a server.

@MainActor
final class RecoveryAddressTests: XCTestCase {

    private func status(
        enabled: Bool, vault: Bool, phraseSaved: Bool
    ) -> RecoveryEmailStatus {
        RecoveryEmailStatus(available: true, enabled: enabled, vault: vault, phraseSaved: phraseSaved)
    }

    private func defaults(_ name: String = #function) -> UserDefaults {
        let suite = "lakko.tests.recovery-address.\(name)"
        UserDefaults().removePersistentDomain(forName: suite)
        return UserDefaults(suiteName: suite)!
    }

    // MARK: - The rules

    func testTheResealRule() {
        typealias R = EmailReseal
        XCTAssertTrue(R.resealNeeded(enabled: true, vault: false, phraseSaved: false, addressHeld: true))
        // Each condition alone stops it.
        XCTAssertFalse(R.resealNeeded(enabled: false, vault: false, phraseSaved: false, addressHeld: true))
        XCTAssertFalse(R.resealNeeded(enabled: true, vault: true, phraseSaved: false, addressHeld: true))
        XCTAssertFalse(R.resealNeeded(enabled: true, vault: false, phraseSaved: true, addressHeld: true))
        XCTAssertFalse(R.resealNeeded(enabled: true, vault: false, phraseSaved: false, addressHeld: false))
    }

    func testWhatAResealAttemptCameTo() {
        func of(_ status: Int, _ code: String) -> EmailReseal.Outcome {
            EmailReseal.outcome(status: status, body: #"{"error":"\#(code)"}"#)
        }
        XCTAssertEqual(EmailReseal.outcome(of: nil), .done)
        // As good as stored: the status is read again.
        XCTAssertEqual(of(409, "vault-exists"), .done)
        XCTAssertEqual(of(409, "phrase-saved"), .done)
        // The address held here is stale.
        XCTAssertEqual(of(403, "address-mismatch"), .addressStale)
        // Everything else waits for the next landing.
        for (status, code) in [
            (400, "address-invalid"), (429, "too-many-attempts"), (409, "username-required"),
            (409, "handle-mismatch"), (409, "no-recovery-email"), (503, "recovery-email-disabled"), (500, "x"),
        ] {
            XCTAssertEqual(of(status, code), .retryAtNextLanding, code)
        }
        XCTAssertEqual(
            EmailReseal.outcome(of: AuthClientError.network(endpoint: "/v", underlying: URLError(.timedOut))),
            .retryAtNextLanding)
    }

    func testAddAgainOnlyWhereNoDeviceHoldsTheAddress() {
        let dropped = status(enabled: true, vault: false, phraseSaved: false)
        XCTAssertEqual(ProtectionRows.decide(dropped, addressHeld: false).email, .addAgain)
        XCTAssertEqual(ProtectBanner.copy(dropped, addressHeld: false).titleKey, "recovery_email.add_again.title")
        // A device that holds it re-seals by itself: the row reads On and
        // nothing is asked.
        XCTAssertEqual(ProtectionRows.decide(dropped, addressHeld: true).email, .on)
        XCTAssertFalse(ProtectBanner.shows(
            protection: dropped.protection, resealing: true, dismissedAtMs: nil, nowMs: 0))
        XCTAssertTrue(ProtectBanner.shows(
            protection: dropped.protection, resealing: false, dismissedAtMs: nil, nowMs: 0))

        // After a recall the stored key is gone (`enabled` false): the
        // ordinary add-email state, the field pre-filled when held.
        let recalled = status(enabled: false, vault: false, phraseSaved: false)
        XCTAssertEqual(ProtectionRows.decide(recalled, addressHeld: true).email, .off)
        XCTAssertEqual(EmailReseal.prefill(replacing: false, addressHeld: "a@b.example"), "a@b.example")
        XCTAssertEqual(EmailReseal.prefill(replacing: false, addressHeld: nil), "")
        // Replace: a new address is being typed.
        XCTAssertEqual(EmailReseal.prefill(replacing: true, addressHeld: "a@b.example"), "")
    }

    // MARK: - The register (lakko-sync, through the FFI)

    func testTheRegisterIsSetRemovedCarriedAndPersisted() throws {
        let a = SyncedState()
        XCTAssertNil(try a.recoveryEmail())
        XCTAssertNil(try a.recoveryEmailEnvelope())
        XCTAssertTrue(try a.setRecoveryEmail(hlc: 10, address: " Alice@Example.COM "))
        XCTAssertEqual(try a.recoveryEmail(), "alice@example.com", "the core normalises")

        // A sibling device applies the envelope; it is routed by its kind.
        let envelope = try XCTUnwrap(try a.recoveryEmailEnvelope())
        XCTAssertTrue(isRecoveryEmailEnvelope(bytes: envelope))
        XCTAssertFalse(isLineageEnvelope(bytes: envelope))
        let b = SyncedState()
        XCTAssertTrue(try b.applyRecoveryEmailEnvelope(envelope: envelope))
        XCTAssertEqual(try b.recoveryEmail(), "alice@example.com")
        XCTAssertFalse(try b.applyRecoveryEmailEnvelope(envelope: envelope), "idempotent")
        XCTAssertEqual(DeviceGroupSyncRepository.serviceEnvelopeSlot(envelope), "recovery-email")

        // Readable after a restart (the encrypted store keeps `toBytes`),
        // and carried by the backup (`mergeFrom`).
        let restored = try SyncedState.fromBytes(bytes: try a.toBytes())
        XCTAssertEqual(try restored.recoveryEmail(), "alice@example.com")
        let fresh = SyncedState()
        _ = SyncedStateMerge.merge(restored, into: fresh)
        XCTAssertEqual(try fresh.recoveryEmail(), "alice@example.com")

        // Removing writes the empty value, and the removal travels too.
        XCTAssertTrue(try a.setRecoveryEmail(hlc: 20, address: nil))
        XCTAssertNil(try a.recoveryEmail())
        XCTAssertNotNil(try a.recoveryEmailRegister(), "removed at a time, not never written")
        let removal = try XCTUnwrap(try a.recoveryEmailEnvelope())
        XCTAssertTrue(try b.applyRecoveryEmailEnvelope(envelope: removal))
        XCTAssertNil(try b.recoveryEmail())
        // An older write never brings it back.
        XCTAssertFalse(try b.applyRecoveryEmailEnvelope(envelope: envelope))
        XCTAssertNil(try b.recoveryEmail())
        // A refused address is not written.
        XCTAssertThrowsError(try a.setRecoveryEmail(hlc: 30, address: "no-at-sign"))
    }

    func testTheVaultPutWire() throws {
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(EmailVaultPutRequest(
            handle: "hd", address: "a@b.example", vaultSalt: "c2FsdA==", sealedEntropy: "c2VhbGVk"))) as? [String: Any])
        XCTAssertEqual(Set(json.keys), ["handle", "address", "vault_salt", "sealed_entropy"])
        XCTAssertNil(json["fresh_grant"], "no fresh-auth grant")
    }

    // MARK: - The centre: keeping the address, the automatic re-seal

    private final class World {
        var status: RecoveryEmailStatus
        var register: String?
        var syncReady = true
        var stored: [String?] = []
        var resealError: Error?
        var resealed: [String] = []
        var statusAfterReseal: RecoveryEmailStatus?
        var statusReads = 0
        init(_ status: RecoveryEmailStatus) { self.status = status }
    }

    private func center(_ world: World) -> AccountProtectionCenter {
        AccountProtectionCenter(
            ports: .init(
                session: { .init(accountIdB64: "acct", sessionToken: "tok") },
                fetchStatus: { _ in
                    world.statusReads += 1
                    return world.status
                },
                fetchNotices: { _, _ in AccountNoticePage(notices: [], nextAfter: 0, more: false) },
                hasPasskey: { false },
                addressHeld: { world.register },
                storeAddress: { address in
                    guard world.syncReady else { return false }
                    world.stored.append(address)
                    world.register = address
                    return true
                },
                reseal: { _, address in
                    world.resealed.append(address)
                    if let error = world.resealError { throw error }
                    if let after = world.statusAfterReseal { world.status = after }
                },
                nowMs: { 1_000_000 }),
            store: AccountProtectionStore(defaults: defaults("centre-\(UUID().uuidString)")))
    }

    func testADeviceThatHoldsTheAddressPutsTheVaultBackSilently() async {
        // After a key reset: the email is on, the vault is gone.
        let world = World(status(enabled: true, vault: false, phraseSaved: false))
        world.register = "alice@example.com"
        world.statusAfterReseal = status(enabled: true, vault: true, phraseSaved: false)
        let center = center(world)

        await center.land(accountIdB64: "acct")

        XCTAssertEqual(world.resealed, ["alice@example.com"], "sealed and put back, once")
        XCTAssertEqual(center.status?.protection, .email, "the status was read again")
        XCTAssertFalse(center.bannerVisible)
        XCTAssertEqual(center.rows?.email, .on)
        XCTAssertEqual(center.addressHeld, "alice@example.com", "shown in Settings on this device")
    }

    func testNoResealWithoutTheAddressOrWhereNoneIsWanted() async {
        // No device holds the address: "add your email again".
        let none = World(status(enabled: true, vault: false, phraseSaved: false))
        let noAddress = center(none)
        await noAddress.land(accountIdB64: "acct")
        XCTAssertEqual(none.resealed, [])
        XCTAssertEqual(noAddress.rows?.email, .addAgain)
        XCTAssertTrue(noAddress.bannerVisible)

        // A phrase-protected account never gets a vault.
        let phrase = World(status(enabled: true, vault: false, phraseSaved: true))
        phrase.register = "alice@example.com"
        await center(phrase).land(accountIdB64: "acct")
        XCTAssertEqual(phrase.resealed, [])

        // The vault is there: nothing to do.
        let whole = World(status(enabled: true, vault: true, phraseSaved: false))
        whole.register = "alice@example.com"
        await center(whole).land(accountIdB64: "acct")
        XCTAssertEqual(whole.resealed, [])
    }

    func testAFailedResealIsTriedAgainAtTheNextLandingNeverInALoop() async {
        let world = World(status(enabled: true, vault: false, phraseSaved: false))
        world.register = "alice@example.com"
        world.resealError = AuthClientError.http(
            status: 429, endpoint: "/recovery-email/vault", body: #"{"error":"too-many-attempts"}"#)
        let center = center(world)
        await center.land(accountIdB64: "acct")
        XCTAssertEqual(world.resealed.count, 1)

        // Reading the status again (Settings opening) does not try again.
        await center.refresh()
        await center.refresh()
        XCTAssertEqual(world.resealed.count, 1, "every request spends one of ten per day")
        XCTAssertFalse(center.bannerVisible, "still silent: this device will try again")

        // The next landing does.
        world.resealError = nil
        world.statusAfterReseal = status(enabled: true, vault: true, phraseSaved: false)
        await center.land(accountIdB64: "acct")
        XCTAssertEqual(world.resealed.count, 2)
        XCTAssertEqual(center.status?.vault, true)
    }

    func testVaultExistsAndPhraseSavedAreAsGoodAsDone() async {
        for code in ["vault-exists", "phrase-saved"] {
            let world = World(status(enabled: true, vault: false, phraseSaved: false))
            world.register = "alice@example.com"
            world.resealError = AuthClientError.http(
                status: 409, endpoint: "/recovery-email/vault", body: #"{"error":"\#(code)"}"#)
            let center = center(world)
            await center.land(accountIdB64: "acct")
            let readsAfterLanding = world.statusReads
            XCTAssertGreaterThanOrEqual(readsAfterLanding, 2, "\(code): the status is read again")
            XCTAssertEqual(world.register, "alice@example.com", "\(code): the address is kept")
        }
    }

    func testAStaleAddressIsClearedAndAddAgainTakesOver() async {
        let world = World(status(enabled: true, vault: false, phraseSaved: false))
        world.register = "old@example.com"
        world.resealError = AuthClientError.http(
            status: 403, endpoint: "/recovery-email/vault", body: #"{"error":"address-mismatch"}"#)
        let center = center(world)
        await center.land(accountIdB64: "acct")

        XCTAssertEqual(world.stored, [nil], "the register is cleared")
        XCTAssertNil(center.addressHeld)
        XCTAssertEqual(center.rows?.email, .addAgain)
        XCTAssertTrue(center.bannerVisible)
    }

    func testTheAddressIsKeptOnEnrolRemovedOnRemoveAndWaitsForTheSyncedState() async {
        let world = World(status(enabled: false, vault: false, phraseSaved: false))
        let center = center(world)
        await center.land(accountIdB64: "acct")

        // Enrol finish: the normalised address goes to the register.
        center.enrolled(EmailEnrolled(enabled: true, vault: true), replaced: false, address: "alice@example.com")
        await center.flushPendingAddress()
        XCTAssertEqual(world.stored, ["alice@example.com"])
        // Replace: the register takes the new address.
        center.enrolled(EmailEnrolled(enabled: true, vault: true), replaced: true, address: "new@example.com")
        await center.flushPendingAddress()
        XCTAssertEqual(world.register, "new@example.com")
        // Removing the recovery email writes the empty value.
        center.removed()
        await center.flushPendingAddress()
        XCTAssertNil(world.register)
        XCTAssertEqual(world.stored.last ?? "x", nil)

        // Got back in by email before the synced state is loaded: the write
        // waits for it.
        let late = World(status(enabled: true, vault: true, phraseSaved: false))
        late.syncReady = false
        let fresh = self.center(late)
        fresh.keepAddress("alice@example.com")
        await fresh.land(accountIdB64: "acct")
        XCTAssertEqual(late.stored, [])
        late.syncReady = true
        await fresh.flushPendingAddress()
        XCTAssertEqual(late.stored, ["alice@example.com"])
        await fresh.flushPendingAddress()
        XCTAssertEqual(late.stored.count, 1, "written once")
    }

    func testTheEnrolFormIsPrefilledOnlyWhenNothingIsBeingReplaced() {
        func model(replacing: Bool, held: String?) -> RecoveryEmailModel {
            RecoveryEmailModel(
                ports: .init(
                    loginHandle: { Data(repeating: 1, count: 32) }, sessionToken: { "tok" }, grant: { "g" },
                    phraseSaved: { false }, normalize: { try normalizeRecoveryEmail(address: $0) },
                    start: { _, _, _, _ in throw URLError(.badURL) },
                    seal: { _, _, _ in Data() },
                    finish: { _, _, _, _ in throw URLError(.badURL) },
                    remove: { _, _ in }, enrolled: { _, _, _ in }, removed: {}, proofExpired: {}),
                replacing: replacing,
                prefill: EmailReseal.prefill(replacing: replacing, addressHeld: held))
        }
        XCTAssertEqual(model(replacing: false, held: "alice@example.com").address, "alice@example.com")
        XCTAssertEqual(model(replacing: true, held: "alice@example.com").address, "")
        XCTAssertEqual(model(replacing: false, held: nil).address, "")
    }

    // MARK: - A guest chooses a username

    func testTheClaimIsOfferedOnlyToAnAccountWithoutAUsername() {
        XCTAssertTrue(UsernameClaim.offered(username: nil))
        XCTAssertTrue(UsernameClaim.offered(username: ""))
        XCTAssertTrue(UsernameClaim.offered(username: "  "))
        XCTAssertFalse(UsernameClaim.offered(username: "alice"))
        for key in [
            "username.claim.title", "username.claim.body", "username.claim.password_help",
            "username.claim.save", "username.claim.done",
        ] {
            XCTAssertNotEqual(
                Localization.text(key, texts: ["username": "@alice"]), key, "\(key) is missing from the catalog")
        }
        XCTAssertTrue(Localization.text("username.claim.done", texts: ["username": "@alice"]).contains("@alice"))
    }

    func testTheClaimStepMachine() {
        var state = UsernameClaim.initial(username: "", firstPasswordOwed: false)
        XCTAssertEqual(state.step, .form)
        XCTAssertTrue(UsernameClaim.nameEditable(state))

        // The name was not claimed: nothing was saved, the form stays.
        let taken = UsernameClaim.nameFailed(state, .taken)
        XCTAssertEqual(taken.step, .form)
        XCTAssertNil(taken.savedName)

        state = UsernameClaim.nameSaved(state, canonical: "alice")
        XCTAssertEqual(state.step, .passwordOwed)
        XCTAssertEqual(state.savedName, "alice")
        XCTAssertFalse(UsernameClaim.nameEditable(state), "the name is saved")

        // The password step failed: the name is kept, only it is retried.
        let failed = UsernameClaim.passwordFailed(state)
        XCTAssertEqual(failed.step, .passwordOwed)
        XCTAssertEqual(failed.savedName, "alice")
        XCTAssertEqual(failed.failure, .passwordFailed)

        XCTAssertEqual(UsernameClaim.passwordSet(failed).step, .done)
        XCTAssertNil(UsernameClaim.passwordSet(failed).failure)

        // Closed in between: the screen comes back on the password step.
        let resumed = UsernameClaim.initial(username: "alice", firstPasswordOwed: true)
        XCTAssertEqual(resumed.step, .passwordOwed)
        XCTAssertEqual(resumed.savedName, "alice")

        // The failures.
        XCTAssertEqual(UsernameClaim.failure(of: UsernameTaken(username: "alice")), .taken)
        XCTAssertEqual(
            UsernameClaim.failure(of: UsernameBindLimited(retryAfterSecs: 120)), .bindLimited(retryAfterSecs: 120))
        XCTAssertEqual(
            UsernameClaim.failure(of: AuthClientError.network(endpoint: "/x", underlying: URLError(.timedOut))),
            .offline)
        XCTAssertEqual(
            UsernameClaim.failure(of: DiscoveryClientError.http(status: 409, endpoint: "/r", body: "")), .taken)
        XCTAssertEqual(UsernameClaim.failure(of: URLError(.badURL)), .failed)
    }

    private final class ClaimWorld {
        var nameError: Error?
        var passwordError: Error?
        var calls: [String] = []
        var passwords: [Data] = []
    }

    private func claimModel(
        _ world: ClaimWorld, username: String? = "", owed: Bool = false
    ) -> UsernameClaimModel {
        UsernameClaimModel(
            ports: .init(
                ruleMessage: { checkUsername($0).inlineMessage },
                claimName: { typed in
                    world.calls.append("name")
                    if let error = world.nameError { throw error }
                    return typed.lowercased()
                },
                setPassword: { password in
                    world.calls.append("password")
                    if let error = world.passwordError { throw error }
                    world.passwords.append(password)
                }),
            username: username, firstPasswordOwed: owed)
    }

    func testTheClaimRunsTheNameThenThePassword() async {
        let world = ClaimWorld()
        let model = claimModel(world)

        // Validated locally before the first request.
        model.username = "alice"
        model.password = "short"
        model.confirm = "short"
        await model.submit()
        XCTAssertEqual(world.calls, [])
        XCTAssertNotNil(model.passwordError)

        model.password = "long enough"
        model.confirm = "long enough"
        XCTAssertTrue(model.canSubmit)
        await model.submit()
        XCTAssertEqual(world.calls, ["name", "password"])
        XCTAssertEqual(world.passwords, [Data("long enough".utf8)])
        XCTAssertEqual(model.state.step, .done)
        XCTAssertEqual(model.password, "", "the typed password is not kept")
    }

    func testATakenNameSavesNothingAndSendsNoPassword() async {
        let world = ClaimWorld()
        world.nameError = UsernameTaken(username: "alice")
        let model = claimModel(world)
        model.username = "alice"
        model.password = "long enough"
        model.confirm = "long enough"
        await model.submit()
        XCTAssertEqual(world.calls, ["name"])
        XCTAssertEqual(model.state.step, .form)
        XCTAssertEqual(model.state.failure, .taken)

        // A name that breaks the rule is refused before any request.
        let rule = ClaimWorld()
        let broken = claimModel(rule)
        broken.username = "A B"
        broken.password = "long enough"
        broken.confirm = "long enough"
        XCTAssertFalse(broken.canSubmit)
        await broken.submit()
        XCTAssertEqual(rule.calls, [])
    }

    func testAPasswordFailureKeepsTheNameAndRetriesOnlyThePassword() async {
        let world = ClaimWorld()
        world.passwordError = AuthClientError.network(endpoint: "/password/reset/start", underlying: URLError(.timedOut))
        let model = claimModel(world)
        model.username = "alice"
        model.password = "long enough"
        model.confirm = "long enough"
        await model.submit()
        XCTAssertEqual(model.state.step, .passwordOwed)
        XCTAssertEqual(model.state.savedName, "alice")
        XCTAssertEqual(model.state.failure, .passwordFailed)

        world.passwordError = nil
        await model.submit()
        XCTAssertEqual(world.calls, ["name", "password", "password"], "the name is not claimed twice")
        XCTAssertEqual(model.state.step, .done)
    }

    func testTheFirstPasswordOwedFlagIsPerAccountAndWiped() {
        let store = AccountProtectionStore(defaults: defaults())
        XCTAssertFalse(store.firstPasswordOwed(accountIdB64: "acct"))
        store.setFirstPasswordOwed(true, accountIdB64: "acct")
        XCTAssertTrue(store.firstPasswordOwed(accountIdB64: "acct"))
        XCTAssertFalse(store.firstPasswordOwed(accountIdB64: "other"))
        store.clear(accountIdB64: "acct")
        XCTAssertFalse(store.firstPasswordOwed(accountIdB64: "acct"))
    }
}
