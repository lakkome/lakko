// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import LakkoCore
import XCTest

@testable import Lakko

// `docs/ux/call-links.md` section 9 - guest accounts (2026-10-02).
//
// A person who opens a call link without an account presses Join and goes
// into the call: the guest account is made silently, no recovery phrase is
// shown first. Afterwards they are asked once whether to keep it. The web
// client is the reference (`apps/web/src/auth/guest-account.ts` and its
// test); the marker and screen-decision tests below port that test list.

// MARK: - The marker

final class GuestAccountMarkerTests: XCTestCase {

    private var suiteName = ""
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        suiteName = "guest-marker-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)!
    }

    override func tearDown() {
        UserDefaults().removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    private let t0: Int64 = 1_760_000_000_000

    func testAnAccountNobodyMarkedIsNotAGuest() {
        XCTAssertNil(GuestAccountMarker(defaults: defaults).state(accountIdB64: "acct"))
    }

    func testMarksAnAccountAsAGuestWithNoUsernameYet() {
        let marker = GuestAccountMarker(defaults: defaults)
        marker.mark(accountIdB64: "acct", nowMs: t0)
        XCTAssertEqual(
            marker.state(accountIdB64: "acct"),
            GuestAccountState(markedAtMs: t0, claimedUsername: nil))
        XCTAssertEqual(
            defaults.string(forKey: GuestAccountMarker.key(accountIdB64: "acct")), "guest@\(t0)")
    }

    func testIsKeptPerAccount() {
        let marker = GuestAccountMarker(defaults: defaults)
        marker.mark(accountIdB64: "one", nowMs: t0)
        XCTAssertNil(marker.state(accountIdB64: "two"))
    }

    func testRemembersTheUsernameAHalfFinishedCreateAccountAlreadyTook() {
        // A username cannot be changed, so the second attempt must go on with it.
        let marker = GuestAccountMarker(defaults: defaults)
        marker.mark(accountIdB64: "acct", nowMs: t0)
        marker.noteUsernameClaimed(accountIdB64: "acct", username: "mira")
        // The age is kept: taking a name does not buy the account another day.
        XCTAssertEqual(
            marker.state(accountIdB64: "acct"),
            GuestAccountState(markedAtMs: t0, claimedUsername: "mira"))
        XCTAssertEqual(
            defaults.string(forKey: GuestAccountMarker.key(accountIdB64: "acct")), "named@\(t0):mira")
    }

    func testNotingANameDoesNotTurnAnOrdinaryAccountIntoAGuest() {
        let marker = GuestAccountMarker(defaults: defaults)
        marker.noteUsernameClaimed(accountIdB64: "acct", username: "mira")
        XCTAssertNil(marker.state(accountIdB64: "acct"))
        XCTAssertNil(defaults.object(forKey: GuestAccountMarker.key(accountIdB64: "acct")))
    }

    func testStopsBeingAGuestOnceCleared() {
        let marker = GuestAccountMarker(defaults: defaults)
        marker.mark(accountIdB64: "acct", nowMs: t0)
        marker.noteUsernameClaimed(accountIdB64: "acct", username: "mira")
        marker.clear(accountIdB64: "acct")
        XCTAssertNil(marker.state(accountIdB64: "acct"))
        XCTAssertNil(defaults.object(forKey: GuestAccountMarker.key(accountIdB64: "acct")))
    }

    func testAValueThisBuildDidNotWriteIsNotAGuest() {
        // The offer includes deleting the account: never on the strength of
        // unknown bytes. The untimed `guest` / `named:<name>` values of the
        // first version are among them.
        let marker = GuestAccountMarker(defaults: defaults)
        let key = GuestAccountMarker.key(accountIdB64: "acct")
        for value in [
            "1", "guest", "guest@", "guest@soon", "named@12", "named@x:mira", "named:mira",
            "guest@-5", "guest@1.5", "guest@12345678901234567", "guest@9999999999999999",
            "guest@\u{0661}\u{0662}",
        ] {
            defaults.set(value, forKey: key)
            XCTAssertNil(marker.state(accountIdB64: "acct"), value)
        }
        defaults.set(true, forKey: key)
        XCTAssertNil(marker.state(accountIdB64: "acct"))
    }

    func testANamedMarkWithNoNameIsAGuestWithNoUsername() {
        defaults.set("named@\(t0):", forKey: GuestAccountMarker.key(accountIdB64: "acct"))
        XCTAssertEqual(
            GuestAccountMarker(defaults: defaults).state(accountIdB64: "acct"),
            GuestAccountState(markedAtMs: t0, claimedUsername: nil))
    }

    func testTheGuestPhraseKeyIsNeverTheAccountsOwnKeyInThePhraseStore() {
        // The automatic backup reads the phrase by the account id. A guest's
        // phrase under that key would upload a backup for an account the
        // person has not decided to keep.
        XCTAssertNotEqual(guestPhraseKey(accountIdB64: "acct"), "acct")
        XCTAssertNotEqual(guestPhraseKey(accountIdB64: "one"), guestPhraseKey(accountIdB64: "two"))
    }
}

// MARK: - What a guest is shown

final class GuestScreenTests: XCTestCase {

    func testLeavesAnOrdinaryAccountAlone() {
        XCTAssertEqual(guestScreen(guest: false, callInProgress: false), .none)
        XCTAssertEqual(guestScreen(guest: false, callInProgress: true), .none)
    }

    func testSendsAGuestBackToTheCallWhileOneRuns() {
        // Never the offer here: one of its two buttons deletes the account.
        XCTAssertEqual(guestScreen(guest: true, callInProgress: true), .inCall)
    }

    func testAsksTheQuestionOnceThereIsNoCall() {
        XCTAssertEqual(guestScreen(guest: true, callInProgress: false), .offer)
    }
}

// MARK: - Lifetime, restore, delete order

final class GuestLifetimeTests: XCTestCase {

    private let t0: Int64 = 1_760_000_000_000
    private var guest: GuestAccountState { GuestAccountState(markedAtMs: t0, claimedUsername: nil) }

    func testIsNotExpiredWhileYoungerThanTheLifetime() {
        XCTAssertFalse(guestExpired(guest, nowMs: t0))
        XCTAssertFalse(guestExpired(guest, nowMs: t0 + guestLifetimeMs - 1))
    }

    func testIsExpiredFromTheLifetimeOn() {
        XCTAssertTrue(guestExpired(guest, nowMs: t0 + guestLifetimeMs))
        XCTAssertTrue(guestExpired(guest, nowMs: t0 + 10 * guestLifetimeMs))
    }

    func testStopsBeforeTheServersOwnDayNeverAfterIt() {
        // The servers delete an unkept account 24 hours after it was made.
        // The device must stop offering Create account before that.
        XCTAssertEqual(guestLifetimeMs, 23 * 60 * 60 * 1000)
        XCTAssertLessThan(guestLifetimeMs, 24 * 60 * 60 * 1000)
    }

    func testAClockSetBackIsNotAnOldAccount() {
        XCTAssertFalse(guestExpired(guest, nowMs: t0 - 5 * guestLifetimeMs))
        XCTAssertFalse(guestExpired(guest, nowMs: Int64.min))
    }

    // MARK: guestRestoreAction

    func testAnOrdinaryAccountIsLeftToTheOrdinaryRestore() {
        let later = t0 + guestLifetimeMs
        XCTAssertEqual(guestRestoreAction(guest: nil, restorable: true, nowMs: later), .none)
        XCTAssertEqual(guestRestoreAction(guest: nil, restorable: false, nowMs: later), .none)
    }

    func testAYoungGuestIsRestored() {
        XCTAssertEqual(guestRestoreAction(guest: guest, restorable: true, nowMs: t0 + 1), .none)
    }

    func testAGuestPastItsLifetimeIsDeleted() {
        XCTAssertEqual(
            guestRestoreAction(guest: guest, restorable: true, nowMs: t0 + guestLifetimeMs), .discard)
    }

    func testAGuestWhoseSessionCannotBeRestoredIsWipedWhateverItsAge() {
        // No password exists that anyone could type: a signed-out guest
        // account is unreachable, and its phrase must not stay on the device.
        XCTAssertEqual(guestRestoreAction(guest: guest, restorable: false, nowMs: t0 + 1), .wipe)
        XCTAssertEqual(
            guestRestoreAction(guest: guest, restorable: false, nowMs: t0 + guestLifetimeMs), .wipe)
    }

    // MARK: guestDeletePlan

    func testTheAuthServiceDecidesForAnUnnamedGuest() {
        XCTAssertEqual(guestDeletePlan(guest), [
            GuestDeleteStep(service: .auth, required: true),
            GuestDeleteStep(service: .directory, required: false),
        ])
    }

    func testATakenUsernameIsFreedFirstAndRequired() {
        // The directory never sweeps an entry that has a username: skipping
        // this delete would leave the name taken for good.
        XCTAssertEqual(guestDeletePlan(GuestAccountState(markedAtMs: t0, claimedUsername: "mira")), [
            GuestDeleteStep(service: .directory, required: true),
            GuestDeleteStep(service: .auth, required: true),
        ])
    }
}

// MARK: - Create account / No, thanks: the order

@MainActor
final class GuestAccountFlowTests: XCTestCase {

    private struct Boom: Error, Equatable {}

    /// The claim's ports over one ordered log.
    private final class ClaimScript {
        var log: [String] = []
        var phrase: String? = "the phrase"
        var claimed: String?
        var claimError: Error?
        var passwordError: Error?

        var ports: GuestAccountClaim.Ports {
            GuestAccountClaim.Ports(
                readPhrase: { [self] in
                    log.append("read-phrase")
                    return phrase
                },
                claimedUsername: { [self] in claimed },
                claimUsername: { [self] name in
                    log.append("claim-username \(name)")
                    if let claimError { throw claimError }
                    return name.lowercased()
                },
                noteUsernameClaimed: { [self] name in
                    log.append("note-username \(name)")
                    claimed = name
                },
                deleteOwnKeyPackage: { [self] in log.append("delete-keypackage") },
                setPassword: { [self] phrase, password, name in
                    log.append("set-password \(name) \(phrase) \(String(decoding: password, as: UTF8.self))")
                    if let passwordError { throw passwordError }
                },
                finish: { [self] phrase in log.append("finish \(phrase)") }
            )
        }
    }

    func testCreateAccountRunsItsStepsInOrder() async throws {
        let script = ClaimScript()
        try await GuestAccountClaim.run(
            username: "Mira", password: Data("hunter2hunter2".utf8), ports: script.ports)
        XCTAssertEqual(script.log, [
            "read-phrase",
            "claim-username Mira",
            "note-username mira",
            // The KeyPackage leaves the directory before the password step.
            "delete-keypackage",
            "set-password mira the phrase hunter2hunter2",
            // Only after the password is set does the account stop being a guest.
            "finish the phrase",
        ])
    }

    func testWithoutThePhraseNothingStarts() async {
        let script = ClaimScript()
        script.phrase = nil
        do {
            try await GuestAccountClaim.run(username: "mira", password: Data("pw".utf8), ports: script.ports)
            XCTFail("an account whose phrase is gone cannot be kept")
        } catch {
            XCTAssertTrue(error is GuestPhraseUnavailable)
        }
        XCTAssertEqual(script.log, ["read-phrase"])
    }

    func testARefusedUsernameStopsBeforeAnythingElse() async {
        let script = ClaimScript()
        script.claimError = UsernameTaken(username: "mira")
        do {
            try await GuestAccountClaim.run(username: "mira", password: Data("pw".utf8), ports: script.ports)
            XCTFail("a taken name must not keep the account")
        } catch {
            XCTAssertEqual(error as? UsernameTaken, UsernameTaken(username: "mira"))
        }
        XCTAssertEqual(script.log, ["read-phrase", "claim-username mira"])
        XCTAssertNil(script.claimed, "nothing was recorded as taken")
    }

    func testAFailedPasswordStepKeepsTheGuestAndARetryKeepsTheUsername() async throws {
        let script = ClaimScript()
        script.passwordError = Boom()
        do {
            try await GuestAccountClaim.run(username: "mira", password: Data("pw1".utf8), ports: script.ports)
            XCTFail("the password step failed")
        } catch {
            XCTAssertTrue(error is Boom)
        }
        XCTAssertFalse(script.log.contains { $0.hasPrefix("finish") },
                       "the marker is cleared only after the password is set")
        XCTAssertEqual(script.claimed, "mira")

        // The retry: another name is typed (the form does not even ask), and
        // the username taken the first time is the one that is used.
        script.log = []
        script.passwordError = nil
        try await GuestAccountClaim.run(username: "other", password: Data("pw2".utf8), ports: script.ports)
        XCTAssertEqual(script.log, [
            "read-phrase",
            "delete-keypackage",
            "set-password mira the phrase pw2",
            "finish the phrase",
        ])
    }

    /// The discard's ports over one ordered log.
    private final class DiscardScript {
        var log: [String] = []
        var authError: Error?
        var directoryError: Error?

        var ports: GuestAccountDiscard.Ports {
            GuestAccountDiscard.Ports(
                servers: GuestServerDelete.Ports(
                    deleteAtAuth: { [self] in
                        log.append("auth")
                        if let authError { throw authError }
                    },
                    deleteInDirectory: { [self] in
                        log.append("directory")
                        if let directoryError { throw directoryError }
                    }
                ),
                wipe: { [self] in log.append("wipe") }
            )
        }
    }

    private let unnamed = GuestAccountState(markedAtMs: 1, claimedUsername: nil)
    private let named = GuestAccountState(markedAtMs: 1, claimedUsername: "mira")

    func testNoThanksDeletesOnAuthThenTheDirectoryThenTheDevice() async throws {
        let script = DiscardScript()
        try await GuestAccountDiscard.run(guest: unnamed, ports: script.ports)
        XCTAssertEqual(script.log, ["auth", "directory", "wipe"])
    }

    func testAFailedAuthDeleteLeavesEverythingInPlace() async {
        let script = DiscardScript()
        script.authError = Boom()
        do {
            try await GuestAccountDiscard.run(guest: unnamed, ports: script.ports)
            XCTFail("the account still exists")
        } catch {
            XCTAssertTrue(error is Boom)
        }
        XCTAssertEqual(script.log, ["auth"], "no directory call and no wipe")
    }

    func testAFailedDirectoryDeleteStillWipesAnUnnamedGuest() async throws {
        let script = DiscardScript()
        script.directoryError = Boom()
        try await GuestAccountDiscard.run(guest: unnamed, ports: script.ports)
        XCTAssertEqual(script.log, ["auth", "directory", "wipe"])
    }

    func testWithAClaimedNameTheDirectoryGoesFirst() async throws {
        let script = DiscardScript()
        try await GuestAccountDiscard.run(guest: named, ports: script.ports)
        XCTAssertEqual(script.log, ["directory", "auth", "wipe"])
    }

    func testWithAClaimedNameAFailedDirectoryDeleteStopsEverything() async {
        // The name would stay taken for good, so nothing after it runs and
        // nothing local is removed: a retry completes.
        let script = DiscardScript()
        script.directoryError = Boom()
        do {
            try await GuestAccountDiscard.run(guest: named, ports: script.ports)
            XCTFail("the username is still taken")
        } catch {
            XCTAssertTrue(error is Boom)
        }
        XCTAssertEqual(script.log, ["directory"], "no auth call and no wipe")
    }

    func testWithAClaimedNameAFailedAuthDeleteWipesNothing() async {
        let script = DiscardScript()
        script.authError = Boom()
        do {
            try await GuestAccountDiscard.run(guest: named, ports: script.ports)
            XCTFail("the account still exists")
        } catch {
            XCTAssertTrue(error is Boom)
        }
        XCTAssertEqual(script.log, ["directory", "auth"])
    }

    func testExpirySwallowsEveryFailureAndStillWipes() async {
        for guest in [unnamed, named] {
            let script = DiscardScript()
            script.authError = Boom()
            script.directoryError = Boom()
            await GuestAccountExpiry.run(guest: guest, ports: script.ports)
            XCTAssertEqual(script.log.last, "wipe")
            XCTAssertEqual(Set(script.log), ["auth", "directory", "wipe"], "both servers were asked")
            XCTAssertEqual(script.log.count, 3)
        }
    }

    // MARK: - The form's error copy

    func testTheCreateAccountErrorCopy() {
        XCTAssertEqual(
            AuthErrorCopy.guestClaim(GuestPhraseUnavailable()),
            Localization.text("guest.claim.error.phrase_missing"))
        XCTAssertEqual(
            AuthErrorCopy.guestClaim(UsernameTaken(username: "mira")),
            Localization.text("auth.signup.error.username_taken"))
        XCTAssertEqual(
            AuthErrorCopy.guestClaim(
                AuthClientError.http(status: 409, endpoint: "/auth/login-handle", body: "")),
            Localization.text("auth.signup.error.username_taken"))
        XCTAssertEqual(
            AuthErrorCopy.guestClaim(
                AuthClientError.http(status: 503, endpoint: "/password/reset/start", body: "")),
            AuthErrorCopy.unavailable)
        XCTAssertEqual(
            AuthErrorCopy.guestClaim(UsernameBindLimited(retryAfterSecs: 120)),
            AuthRateLimit.handleBindLimitedCopy(retryAfterSecs: 120))
        XCTAssertEqual(
            AuthErrorCopy.guestClaim(UsernameRuleViolation(reason: .tooShort)),
            UsernameRuleReason.tooShort.message)
        // The password step is the phrase-proved reset; its "no match" is not
        // something this form can say, the person typed no phrase.
        XCTAssertEqual(
            AuthErrorCopy.guestClaim(
                AuthClientError.http(status: 401, endpoint: "/password/reset/finish", body: "")),
            Localization.text("guest.claim.error.generic"))
        XCTAssertEqual(AuthErrorCopy.guestClaim(Boom()), Localization.text("guest.claim.error.generic"))
    }
}

// MARK: - Through the repository

/// Records every directory call and answers from its script.
private final class GuestDirectory: AccountDirectory {
    var registered: [String] = []
    var published = 0
    var publishError: Error?

    func registerUsername(accountId: Data, username: String, sessionToken: String) async throws -> RegisterResult {
        registered.append(username)
        return RegisterResult(normalisedUsername: username)
    }

    func publishIdentityBundle(
        accountId: Data, signaturePubkey: Data, x25519Pubkey: Data,
        leafAuth: Data?, genesisAttest: Data?, sessionToken: String
    ) async throws {
        published += 1
        if let publishError { throw publishError }
    }
}

/// Nothing published, nobody found: the landing's identity refresh has
/// nothing to compare and nothing to alert on.
private final class EmptyRestoreDirectory: DeviceRestoreDirectory {
    func fetchIdentityBundle(accountId: Data, sessionToken: String?) async throws -> IdentityFetchResponse? { nil }
    func lookupAccount(username: String, accountId: Data, sessionToken: String) async throws -> Data? { nil }
}

/// Session persistence in memory; what a relaunch would read is whatever a
/// test puts here.
private final class MemoryPersistence: SessionPersistence {
    var deviceKey = false
    var seed: Data?
    var tokens: [String: PersistedSessionToken] = [:]

    func hasDeviceKey(accountIdB64: String) -> Bool { deviceKey }
    func accountIdentitySeed(accountIdB64: String) -> Data? { seed }
    func exportKey(accountIdB64: String) -> Data? { nil }
    func sessionToken(accountIdB64: String) -> PersistedSessionToken? { tokens[accountIdB64] }
    func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws { tokens[accountIdB64] = token }
    func deleteSessionToken(accountIdB64: String) { tokens[accountIdB64] = nil }
}

@MainActor
final class GuestAccountRepositoryTests: XCTestCase {

    private var accountId = Data()
    private var accountIdB64 = ""
    private var suiteName = ""
    private var defaults: UserDefaults!
    private var phrase = ""
    private var seed = Data()
    private var pushSubscribed: [String] = []
    private var watch: AnyCancellable?
    /// The repository's clock. Session tokens in these tests expire far
    /// beyond a guest's lifetime, so moving it expires only the guest.
    private var now: Int64 = 1_000_000
    /// What a launch wiped (``AuthRepository`` `wipeAccountStores`).
    private var launchWiped: [String] = []
    private static let farFuture: Int64 = 1_000_000 + 10 * guestLifetimeMs

    override func setUp() async throws {
        try await super.setUp()
        accountId = Data((0..<16).map { _ in UInt8.random(in: 0...255) })
        accountIdB64 = base64Encode(accountId)
        suiteName = "guest-account-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)!
        let mnemonic = RecoveryMnemonic.generate()
        phrase = mnemonic.phrase()
        seed = mnemonic.toSeed()
        pushSubscribed = []
        now = 1_000_000
        launchWiped = []
    }

    override func tearDown() async throws {
        watch = nil
        DeviceKeyStore.deleteDeviceKey(accountIdB64: accountIdB64)
        ExportKeyStore.deleteExportKey(accountIdB64: accountIdB64)
        AccountIdentitySeedStore.deleteAccountIdentitySeed(accountIdB64: accountIdB64)
        DeviceIdentityStore.deleteDeviceIdentitySeed(accountIdB64: accountIdB64)
        try? PhraseStore().deletePhrase(accountIdB64: accountIdB64)
        try? PhraseStore().deletePhrase(accountIdB64: guestPhraseKey(accountIdB64: accountIdB64))
        AcceptedContactsStore.bind(accountIdB64: nil)
        AmbientSessionToken.shared.set(nil)
        AnonTokenPool.shared.clear()
        UserDefaults().removePersistentDomain(forName: suiteName)
        try await super.tearDown()
    }

    // MARK: - Fixtures

    /// An auth service that binds and releases login handles and refuses
    /// everything else with 503 (the genesis attestation, the password reset).
    private func authHost() -> String {
        NameRouteStub.scope { method, path, _ in
            if method == "PUT", path == "/auth/login-handle" { return (200, #"{"bound":true}"#) }
            if method == "DELETE", path == "/auth/login-handle" { return (200, #"{"removed":true}"#) }
            return (503, "")
        }
    }

    private func repository(
        host: String,
        directory: GuestDirectory,
        persistence: MemoryPersistence,
        sessionStore: SessionStore
    ) -> AuthRepository {
        AuthRepository(
            sessionStore: sessionStore,
            authClientFactory: { NameRouteStub.client(host) },
            persistence: persistence,
            nowMs: { [weak self] in self?.now ?? 0 },
            accountDirectory: directory,
            loginHandleBinds: LoginHandleBindLedger(),
            restoreDirectory: EmptyRestoreDirectory(),
            pushBinding: PushAccountBindingStore(defaults: defaults),
            removeDeliveredNotifications: {},
            backupDefaults: defaults,
            markerDefaults: defaults,
            pushSubscribe: { [weak self] accountIdB64, _ in self?.pushSubscribed.append(accountIdB64) },
            // What the account wipe does to the guest's rows
            // (`AccountLocalDataWipeTests` walks the real one).
            wipeAccountStores: { [weak self] accountIdB64 in
                guard let self else { return }
                self.launchWiped.append(accountIdB64)
                GuestAccountMarker(defaults: self.defaults).clear(accountIdB64: accountIdB64)
                try? PhraseStore().deletePhrase(accountIdB64: guestPhraseKey(accountIdB64: accountIdB64))
            }
        )
    }

    private var signupResult: SignupResult {
        SignupResult(
            accountId: accountId,
            exportKey: Data(repeating: 0xE7, count: 64),
            recoveryPhrase: phrase,
            sessionToken: "tok-guest",
            sessionExpiresAtMs: Self.farFuture
        )
    }

    private var guestSession: AuthState {
        .session(SessionAccount(accountIdB64: accountIdB64, username: ""))
    }

    private func storedGuestPhrase() throws -> String? {
        try PhraseStore().getPhrase(accountIdB64: guestPhraseKey(accountIdB64: accountIdB64))
            .map { String(decoding: $0, as: UTF8.self) }
    }

    /// Let the fire-and-forget tasks a landing starts run.
    private func settle() async {
        for _ in 0..<20 { await Task.yield() }
        try? await Task.sleep(nanoseconds: 50_000_000)
    }

    /// A repository with a landed guest session.
    private func landedGuest(
        host: String, directory: GuestDirectory, persistence: MemoryPersistence, sessionStore: SessionStore
    ) async throws -> AuthRepository {
        let repo = repository(
            host: host, directory: directory, persistence: persistence, sessionStore: sessionStore)
        try await repo.completeGuestSignup(result: signupResult)
        XCTAssertEqual(repo.state, guestSession)
        XCTAssertTrue(repo.isGuest)
        return repo
    }

    // MARK: - A. Guest signup lands the session at once

    func testGuestSignupShowsNoPhraseScreenAndLandsASession() async throws {
        let directory = GuestDirectory()
        let persistence = MemoryPersistence()
        let sessionStore = SessionStore(defaults: defaults)
        let repo = repository(
            host: authHost(), directory: directory, persistence: persistence, sessionStore: sessionStore)
        var states: [AuthState] = []
        watch = repo.$state.sink { states.append($0) }
        var readyFor: [String] = []
        var stateWhenReady: AuthState?

        try await repo.completeGuestSignup(result: signupResult, onAccountReady: { id in
            readyFor.append(id)
            stateWhenReady = repo.state
        })

        // The session, with the real export key, and no username.
        XCTAssertEqual(repo.state, guestSession)
        XCTAssertEqual(repo.identitySecrets?.exportKey, Data(repeating: 0xE7, count: 64))
        XCTAssertEqual(repo.identitySecrets?.accountIdentitySeed, seed)
        XCTAssertEqual(repo.identitySecrets?.sessionToken, "tok-guest")
        XCTAssertEqual(persistence.tokens[accountIdB64]?.sessionToken, "tok-guest",
                       "the next launch can restore the guest session")
        XCTAssertEqual(sessionStore.load(), PersistedSession(accountIdB64: accountIdB64, username: ""))

        // No phrase screen of either kind.
        XCTAssertFalse(states.contains { if case .saveFreshPhrase = $0 { return true } else { return false } },
                       "the guest no longer passes through the save-fresh-phrase state")
        XCTAssertNil(repo.freshPhrase())
        XCTAssertNil(repo.pendingSignupPhrase)
        XCTAssertFalse(SignupPhraseOwedStore(defaults: defaults).isOwed(accountIdB64: accountIdB64))

        // A guest, with no username taken.
        XCTAssertTrue(repo.isGuest)
        XCTAssertNil(repo.guestClaimedUsername)
        XCTAssertEqual(
            GuestAccountMarker(defaults: defaults).state(accountIdB64: accountIdB64),
            GuestAccountState(markedAtMs: 1_000_000, claimedUsername: nil))

        // The phrase is kept under the guest's key, and not the account's:
        // the automatic backup finds nothing to seal under.
        XCTAssertEqual(try storedGuestPhrase(), phrase)
        XCTAssertNil(try PhraseStore().getPhrase(accountIdB64: accountIdB64))

        // The hook ran once, before the session landed.
        XCTAssertEqual(readyFor, [accountIdB64])
        XCTAssertEqual(stateWhenReady, .anon(cachedUsername: nil))

        await settle()
        XCTAssertEqual(pushSubscribed, [], "a guest is never registered for push")
        XCTAssertTrue(directory.registered.isEmpty, "no username is claimed for a guest")
        XCTAssertGreaterThanOrEqual(directory.published, 1, "the identity bundle is published")
        XCTAssertFalse(
            BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64, defaults: defaults))
    }

    func testAFailedBundlePublishLandsNoGuestSession() async throws {
        let directory = GuestDirectory()
        directory.publishError = DiscoveryClientError.http(
            status: 503, endpoint: "/discovery/identity", body: "unavailable")
        let persistence = MemoryPersistence()
        let sessionStore = SessionStore(defaults: defaults)
        let repo = repository(
            host: authHost(), directory: directory, persistence: persistence, sessionStore: sessionStore)

        do {
            try await repo.completeGuestSignup(result: signupResult)
            XCTFail("a failed bundle publish must fail the guest signup")
        } catch DiscoveryClientError.http(let status, _, _) {
            XCTAssertEqual(status, 503)
        }

        XCTAssertEqual(repo.state, .anon(cachedUsername: nil))
        XCTAssertNil(repo.identitySecrets)
        XCTAssertFalse(repo.isGuest)
        XCTAssertNil(sessionStore.load())
        XCTAssertNil(GuestAccountMarker(defaults: defaults).state(accountIdB64: accountIdB64))
        XCTAssertNil(try storedGuestPhrase())
        XCTAssertTrue(persistence.tokens.isEmpty)
    }

    func testAGuestWithoutARecoveryPhraseIsRefused() async {
        let repo = repository(
            host: authHost(), directory: GuestDirectory(), persistence: MemoryPersistence(),
            sessionStore: SessionStore(defaults: defaults))
        let noPhrase = SignupResult(
            accountId: accountId, exportKey: Data(repeating: 0xE7, count: 64),
            recoveryPhrase: nil, sessionToken: "tok-guest", sessionExpiresAtMs: Self.farFuture)
        do {
            try await repo.completeGuestSignup(result: noPhrase)
            XCTFail("an account whose phrase is neither shown nor kept could never be kept")
        } catch {}
        XCTAssertEqual(repo.state, .anon(cachedUsername: nil))
    }

    // MARK: - B. A relaunch is still a guest

    func testARelaunchRestoresTheGuest() async throws {
        let directory = GuestDirectory()
        let persistence = MemoryPersistence()
        let sessionStore = SessionStore(defaults: defaults)
        let host = authHost()
        _ = try await landedGuest(
            host: host, directory: directory, persistence: persistence, sessionStore: sessionStore)

        // What the Keychain holds after the signup.
        persistence.deviceKey = true
        persistence.seed = seed
        let relaunched = repository(
            host: host, directory: directory, persistence: persistence, sessionStore: sessionStore)

        XCTAssertEqual(relaunched.state, guestSession)
        XCTAssertTrue(relaunched.isGuest)
        XCTAssertEqual(relaunched.guestAccount?.markedAtMs, 1_000_000)
        XCTAssertNil(relaunched.pendingSignupPhrase)
        await settle()
        XCTAssertEqual(pushSubscribed, [])
        XCTAssertEqual(launchWiped, [], "a young, restorable guest is restored")
    }

    /// Wait for a launch that parked in `.loading` to land.
    private func landed(_ repo: AuthRepository) async {
        for _ in 0..<500 {
            if repo.state != .loading { return }
            try? await Task.sleep(nanoseconds: 2_000_000)
        }
    }

    func testAGuestWhoseSessionCannotBeRestoredIsWipedAtLaunchAndLandsSignedOut() async throws {
        let directory = GuestDirectory()
        let host = authHost()
        // Three ways a relaunch cannot restore: the token is gone, the token
        // expired, the device key is unreadable.
        for variant in ["no-token", "expired-token", "no-device-key"] {
            let persistence = MemoryPersistence()
            let sessionStore = SessionStore(defaults: defaults)
            now = 1_000_000
            launchWiped = []
            _ = try await landedGuest(
                host: host, directory: directory, persistence: persistence, sessionStore: sessionStore)
            persistence.deviceKey = true
            persistence.seed = seed
            switch variant {
            case "no-token":
                persistence.tokens = [:]
            case "expired-token":
                persistence.tokens[accountIdB64] = PersistedSessionToken(
                    sessionToken: "tok-guest", sessionExpiresAtMs: 5)
            default:
                persistence.deviceKey = false
            }

            let relaunched = repository(
                host: host, directory: directory, persistence: persistence, sessionStore: sessionStore)
            var serverCalls = 0
            relaunched.guestEnvironment.deleteAccountAtAuth = { _ in serverCalls += 1 }
            relaunched.guestEnvironment.deleteAccountInDirectory = { _ in serverCalls += 1 }

            // Signed out at once, with no cached username: a guest never ends
            // up as a cached account nobody knows the password of.
            XCTAssertEqual(relaunched.state, .anon(cachedUsername: nil), variant)
            XCTAssertNil(relaunched.identitySecrets, variant)
            XCTAssertFalse(relaunched.isGuest, variant)
            XCTAssertEqual(launchWiped, [accountIdB64], variant)
            XCTAssertNil(sessionStore.load(), "\(variant): the cached-account record goes too")
            XCTAssertNil(persistence.tokens[accountIdB64], variant)
            XCTAssertNil(GuestAccountMarker(defaults: defaults).state(accountIdB64: accountIdB64), variant)
            XCTAssertNil(try storedGuestPhrase(), variant)
            await settle()
            XCTAssertEqual(serverCalls, 0, "\(variant): nothing can speak for the account, so no server is asked")
        }
    }

    func testARestorableGuestPastItsLifetimeIsDeletedThenWipedAtLaunch() async throws {
        let directory = GuestDirectory()
        let persistence = MemoryPersistence()
        let sessionStore = SessionStore(defaults: defaults)
        let host = authHost()
        _ = try await landedGuest(
            host: host, directory: directory, persistence: persistence, sessionStore: sessionStore)
        persistence.deviceKey = true
        persistence.seed = seed
        now += guestLifetimeMs

        let relaunched = repository(
            host: host, directory: directory, persistence: persistence, sessionStore: sessionStore)
        var calls: [String] = []
        relaunched.guestEnvironment.deleteAccountAtAuth = { [weak self] token in
            calls.append("auth \(token) wiped=\(self?.launchWiped.count ?? -1)")
        }
        relaunched.guestEnvironment.deleteAccountInDirectory = { [weak self] token in
            calls.append("directory \(token) wiped=\(self?.launchWiped.count ?? -1)")
        }

        // No session state lands while the servers are asked.
        XCTAssertEqual(relaunched.state, .loading)
        XCTAssertNil(relaunched.identitySecrets)
        XCTAssertEqual(launchWiped, [])

        await landed(relaunched)

        XCTAssertEqual(calls, ["auth tok-guest wiped=0", "directory tok-guest wiped=0"],
                       "the servers first, then the wipe")
        XCTAssertEqual(launchWiped, [accountIdB64])
        XCTAssertEqual(relaunched.state, .anon(cachedUsername: nil))
        XCTAssertFalse(relaunched.isGuest)
        XCTAssertNil(sessionStore.load())
        XCTAssertNil(persistence.tokens[accountIdB64])
        XCTAssertNil(try storedGuestPhrase())
    }

    func testAnExpiredGuestIsWipedAtLaunchEvenWhenTheServersFail() async throws {
        let directory = GuestDirectory()
        let persistence = MemoryPersistence()
        let sessionStore = SessionStore(defaults: defaults)
        let host = authHost()
        let first = try await landedGuest(
            host: host, directory: directory, persistence: persistence, sessionStore: sessionStore)
        // A username an unfinished Create account took: the directory first.
        GuestAccountMarker(defaults: defaults).noteUsernameClaimed(accountIdB64: accountIdB64, username: "mira")
        sessionStore.save(PersistedSession(accountIdB64: accountIdB64, username: "mira"))
        _ = first
        persistence.deviceKey = true
        persistence.seed = seed
        now += guestLifetimeMs + 1

        let relaunched = repository(
            host: host, directory: directory, persistence: persistence, sessionStore: sessionStore)
        var calls: [String] = []
        relaunched.guestEnvironment.deleteAccountAtAuth = { _ in
            calls.append("auth")
            throw AuthClientError.http(status: 500, endpoint: "/account", body: "")
        }
        relaunched.guestEnvironment.deleteAccountInDirectory = { _ in
            calls.append("directory")
            throw DiscoveryClientError.http(status: 500, endpoint: "/discovery/account", body: "")
        }

        await landed(relaunched)

        XCTAssertEqual(calls, ["directory", "auth"], "not strict: every failure is swallowed")
        XCTAssertEqual(launchWiped, [accountIdB64])
        XCTAssertEqual(relaunched.state, .anon(cachedUsername: nil), "no cached username, even a claimed one")
        XCTAssertNil(sessionStore.load())
    }

    func testAnOrdinaryCachedAccountIsLeftToTheOrdinaryRestore() {
        let persistence = MemoryPersistence()
        let sessionStore = SessionStore(defaults: defaults)
        sessionStore.save(PersistedSession(accountIdB64: accountIdB64, username: "mira"))

        let repo = repository(
            host: authHost(), directory: GuestDirectory(), persistence: persistence, sessionStore: sessionStore)

        XCTAssertEqual(repo.state, .anon(cachedUsername: "mira"))
        XCTAssertEqual(launchWiped, [])
        XCTAssertEqual(sessionStore.load(), PersistedSession(accountIdB64: accountIdB64, username: "mira"))
    }

    // MARK: - B2. The lifetime of a live guest session

    func testAYoungGuestDoesNotExpire() async throws {
        let repo = try await landedGuest(
            host: authHost(), directory: GuestDirectory(), persistence: MemoryPersistence(),
            sessionStore: SessionStore(defaults: defaults))
        var serverCalls = 0
        repo.guestEnvironment.deleteAccountAtAuth = { _ in serverCalls += 1 }
        repo.guestEnvironment.deleteAccountInDirectory = { _ in serverCalls += 1 }
        now += guestLifetimeMs - 1

        let expired = await repo.expireGuestIfDue()

        XCTAssertFalse(expired)
        XCTAssertEqual(serverCalls, 0)
        XCTAssertEqual(repo.state, guestSession)
        XCTAssertEqual(launchWiped, [])
    }

    func testAGuestPastItsLifetimeIsDeletedWipedAndSignedOutWhateverTheServersAnswer() async throws {
        let sessionStore = SessionStore(defaults: defaults)
        let repo = try await landedGuest(
            host: authHost(), directory: GuestDirectory(), persistence: MemoryPersistence(),
            sessionStore: sessionStore)
        var calls: [String] = []
        repo.guestEnvironment.deleteAccountAtAuth = { token in
            calls.append("auth \(token)")
            throw AuthClientError.http(status: 500, endpoint: "/account", body: "")
        }
        repo.guestEnvironment.deleteAccountInDirectory = { token in
            calls.append("directory \(token)")
            throw DiscoveryClientError.http(status: 500, endpoint: "/discovery/account", body: "")
        }
        now += guestLifetimeMs

        // The offer appearing and Create account pressed at the same moment:
        // one expiry.
        async let first = repo.expireGuestIfDue()
        async let second = repo.expireGuestIfDue()
        let results = await [first, second]

        XCTAssertEqual(results, [true, true])
        XCTAssertEqual(calls, ["auth tok-guest", "directory tok-guest"], "asked once, failures swallowed")
        XCTAssertEqual(launchWiped, [accountIdB64])
        XCTAssertEqual(repo.state, .anon(cachedUsername: nil))
        XCTAssertNil(repo.identitySecrets)
        XCTAssertTrue(repo.guestAccountDeleted)
        XCTAssertNil(sessionStore.load())
        XCTAssertNil(try storedGuestPhrase())
        let again = await repo.expireGuestIfDue()
        XCTAssertFalse(again, "nobody is signed in any more")
    }

    func testAnOrdinarySessionNeverExpires() async {
        let persistence = MemoryPersistence()
        persistence.deviceKey = true
        persistence.seed = seed
        persistence.tokens[accountIdB64] = PersistedSessionToken(
            sessionToken: "tok-live", sessionExpiresAtMs: Self.farFuture)
        let sessionStore = SessionStore(defaults: defaults)
        sessionStore.save(PersistedSession(accountIdB64: accountIdB64, username: "mira"))
        let repo = repository(
            host: authHost(), directory: GuestDirectory(), persistence: persistence, sessionStore: sessionStore)
        now += 5 * guestLifetimeMs

        let expired = await repo.expireGuestIfDue()

        XCTAssertFalse(expired)
        XCTAssertEqual(repo.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "mira")))
        XCTAssertEqual(launchWiped, [])
    }

    // MARK: - C. Create account

    func testCreateAccountTakesTheUsernameOnceAndStaysAGuestUntilThePasswordIsSet() async throws {
        let directory = GuestDirectory()
        let persistence = MemoryPersistence()
        let sessionStore = SessionStore(defaults: defaults)
        let host = authHost()
        let repo = try await landedGuest(
            host: host, directory: directory, persistence: persistence, sessionStore: sessionStore)
        var keyPackageDeletes = 0
        repo.guestEnvironment.deleteOwnKeyPackage = { keyPackageDeletes += 1 }

        // The password step (the phrase-proved reset) is refused by the stub.
        do {
            try await repo.claimGuestAccount(username: "@Mira", password: Data("correct horse".utf8))
            XCTFail("the password step failed")
        } catch AuthClientError.http(let status, let endpoint, _) {
            XCTAssertEqual(status, 503)
            XCTAssertTrue(endpoint.contains("/password/reset/start"), endpoint)
        }

        // The username was taken, in the signup order: the handle at auth,
        // then the directory.
        XCTAssertEqual(directory.registered, ["mira"])
        let afterFirst = NameRouteStub.requests(host).map { "\($0.method) \($0.path)" }
        XCTAssertEqual(afterFirst.filter { $0 == "PUT /auth/login-handle" }.count, 1)
        XCTAssertFalse(afterFirst.contains("DELETE /auth/login-handle"), "a claim that succeeded keeps its handle")
        let bindIndex = try XCTUnwrap(afterFirst.firstIndex(of: "PUT /auth/login-handle"))
        let resetIndex = try XCTUnwrap(afterFirst.firstIndex(of: "POST /password/reset/start"))
        XCTAssertLessThan(bindIndex, resetIndex)
        XCTAssertEqual(keyPackageDeletes, 1, "the KeyPackage delete ran before the password step")

        // Still a guest, with the name remembered for the retry.
        XCTAssertTrue(repo.isGuest)
        XCTAssertEqual(repo.guestClaimedUsername, "mira")
        XCTAssertEqual(
            GuestAccountMarker(defaults: defaults).state(accountIdB64: accountIdB64),
            GuestAccountState(markedAtMs: 1_000_000, claimedUsername: "mira"))
        XCTAssertEqual(sessionStore.load(), PersistedSession(accountIdB64: accountIdB64, username: "mira"))
        XCTAssertNil(repo.pendingSignupPhrase, "the phrase is not owed until the password is set")
        XCTAssertFalse(SignupPhraseOwedStore(defaults: defaults).isOwed(accountIdB64: accountIdB64))
        XCTAssertEqual(try storedGuestPhrase(), phrase, "the guest phrase row stays for the retry")

        // The retry goes on with the SAME username, whatever is typed.
        do {
            try await repo.claimGuestAccount(username: "someone_else", password: Data("correct horse".utf8))
            XCTFail("the password step failed again")
        } catch AuthClientError.http(let status, _, _) {
            XCTAssertEqual(status, 503)
        }
        XCTAssertEqual(directory.registered, ["mira"], "the directory is not asked a second time")
        let requests = NameRouteStub.requests(host)
        XCTAssertEqual(
            requests.filter { $0.method == "PUT" && $0.path == "/auth/login-handle" }.count, 1)
        let resets = requests.filter { $0.path == "/password/reset/start" }
        XCTAssertEqual(resets.count, 2)
        let miraHandle = base64UrlEncodeNoPad(try loginHandle(username: "mira"))
        for reset in resets {
            XCTAssertEqual(reset.json["handle"] as? String, miraHandle)
        }
        XCTAssertEqual(keyPackageDeletes, 2)
        XCTAssertTrue(repo.isGuest)

        await settle()
        XCTAssertEqual(pushSubscribed, [], "no push registration while the account is a guest")
    }

    func testCreateAccountWithoutThePhraseRowCannotKeepTheAccount() async throws {
        let directory = GuestDirectory()
        let host = authHost()
        let repo = try await landedGuest(
            host: host, directory: directory, persistence: MemoryPersistence(),
            sessionStore: SessionStore(defaults: defaults))
        try PhraseStore().deletePhrase(accountIdB64: guestPhraseKey(accountIdB64: accountIdB64))

        do {
            try await repo.claimGuestAccount(username: "mira", password: Data("correct horse".utf8))
            XCTFail("the phrase is gone")
        } catch {
            XCTAssertTrue(error is GuestPhraseUnavailable)
        }
        XCTAssertTrue(directory.registered.isEmpty)
        XCTAssertFalse(NameRouteStub.requests(host).contains { $0.path == "/auth/login-handle" },
                       "no username is taken for an account that cannot be kept")
        XCTAssertTrue(repo.isGuest)
    }

    func testCreateAccountReadsThePhraseFromTheAccountsOwnRowWhenTheGuestRowIsGone() async throws {
        // An earlier attempt whose password step went through cached the
        // phrase under the account's key before it was interrupted.
        let directory = GuestDirectory()
        let host = authHost()
        let repo = try await landedGuest(
            host: host, directory: directory, persistence: MemoryPersistence(),
            sessionStore: SessionStore(defaults: defaults))
        try PhraseStore().deletePhrase(accountIdB64: guestPhraseKey(accountIdB64: accountIdB64))
        try PhraseStore().putPhrase(accountIdB64: accountIdB64, phraseBytes: Data(phrase.utf8))

        do {
            try await repo.claimGuestAccount(username: "mira", password: Data("correct horse".utf8))
            XCTFail("the password step is refused by the stub")
        } catch AuthClientError.http(let status, _, _) {
            // Past the phrase and the username, into the password step.
            XCTAssertEqual(status, 503)
        }
        XCTAssertEqual(directory.registered, ["mira"])
        XCTAssertTrue(NameRouteStub.requests(host).contains { $0.path == "/password/reset/start" })
    }

    func testCreateAccountOnAGuestPastItsLifetimeDeletesItAndTakesNoUsername() async throws {
        let directory = GuestDirectory()
        let host = authHost()
        let repo = try await landedGuest(
            host: host, directory: directory, persistence: MemoryPersistence(),
            sessionStore: SessionStore(defaults: defaults))
        var serverCalls: [String] = []
        repo.guestEnvironment.deleteAccountAtAuth = { _ in serverCalls.append("auth") }
        repo.guestEnvironment.deleteAccountInDirectory = { _ in serverCalls.append("directory") }
        now += guestLifetimeMs

        do {
            try await repo.claimGuestAccount(username: "mira", password: Data("correct horse".utf8))
            XCTFail("an account past its lifetime is not kept")
        } catch {
            XCTAssertTrue(error is GuestAccountExpired)
        }

        XCTAssertEqual(serverCalls, ["auth", "directory"])
        XCTAssertEqual(launchWiped, [accountIdB64])
        XCTAssertTrue(directory.registered.isEmpty)
        XCTAssertFalse(NameRouteStub.requests(host).contains { $0.path == "/auth/login-handle" })
        XCTAssertEqual(repo.state, .anon(cachedUsername: nil))
        XCTAssertTrue(repo.guestAccountDeleted)
    }

    func testCreateAccountAndNoThanksNeedAGuestSession() async {
        let repo = repository(
            host: authHost(), directory: GuestDirectory(), persistence: MemoryPersistence(),
            sessionStore: SessionStore(defaults: defaults))
        do {
            try await repo.claimGuestAccount(username: "mira", password: Data("correct horse".utf8))
            XCTFail("nobody is signed in")
        } catch {
            XCTAssertTrue(error is NoGuestAccount)
        }
        do {
            try await repo.discardGuestAccount()
            XCTFail("nobody is signed in")
        } catch {
            XCTAssertTrue(error is NoGuestAccount)
        }
    }

    // MARK: - D. No, thanks

    func testNoThanksLeavesEverythingWhenTheAuthDeleteFails() async throws {
        let directory = GuestDirectory()
        let repo = try await landedGuest(
            host: authHost(), directory: directory, persistence: MemoryPersistence(),
            sessionStore: SessionStore(defaults: defaults))
        var calls: [String] = []
        repo.guestEnvironment.deleteAccountAtAuth = { _ in
            calls.append("auth")
            throw AuthClientError.http(status: 500, endpoint: "/account", body: "")
        }
        repo.guestEnvironment.deleteAccountInDirectory = { _ in calls.append("directory") }

        do {
            try await repo.discardGuestAccount()
            XCTFail("the account still exists")
        } catch AuthClientError.http(let status, _, _) {
            XCTAssertEqual(status, 500)
        }

        XCTAssertEqual(calls, ["auth"])
        XCTAssertEqual(launchWiped, [])
        XCTAssertEqual(repo.state, guestSession)
        XCTAssertTrue(repo.isGuest)
        XCTAssertFalse(repo.guestAccountDeleted)
        XCTAssertEqual(try storedGuestPhrase(), phrase)
    }

    func testNoThanksDeletesOnTheServersThenWipesTheDevice() async throws {
        let directory = GuestDirectory()
        let sessionStore = SessionStore(defaults: defaults)
        let repo = try await landedGuest(
            host: authHost(), directory: directory, persistence: MemoryPersistence(),
            sessionStore: sessionStore)
        var calls: [String] = []
        repo.guestEnvironment.deleteAccountAtAuth = { token in calls.append("auth \(token)") }
        repo.guestEnvironment.deleteAccountInDirectory = { token in
            calls.append("directory \(token)")
            // Best effort for an unnamed guest: a directory failure does not
            // stop the wipe.
            throw DiscoveryClientError.http(status: 500, endpoint: "/discovery/account", body: "")
        }
        var appWiped: [String] = []
        repo.guestEnvironment.wipeLocalData = { [weak self, weak repo] accountIdB64 in
            // What `AppServices.wipeAccountLocalData` does: the stores, then
            // the session end.
            appWiped.append(accountIdB64)
            guard let self else { return }
            GuestAccountMarker(defaults: self.defaults).clear(accountIdB64: accountIdB64)
            try? PhraseStore().deletePhrase(accountIdB64: guestPhraseKey(accountIdB64: accountIdB64))
            repo?.endSessionAfterLocalWipe()
        }

        try await repo.discardGuestAccount()

        XCTAssertEqual(calls, ["auth tok-guest", "directory tok-guest"])
        XCTAssertEqual(appWiped, [accountIdB64], "the app's wipe, when the composition root wired one")
        XCTAssertEqual(launchWiped, [])
        XCTAssertEqual(repo.state, .anon(cachedUsername: nil))
        XCTAssertNil(repo.identitySecrets)
        XCTAssertFalse(repo.isGuest)
        XCTAssertTrue(repo.guestAccountDeleted, "the call-link landing says the guest account was deleted")
        XCTAssertNil(sessionStore.load())
        XCTAssertNil(GuestAccountMarker(defaults: defaults).state(accountIdB64: accountIdB64))
        XCTAssertNil(try storedGuestPhrase())
    }

    func testNoThanksWithAClaimedNameFreesTheNameFirstAndStopsWhenThatFails() async throws {
        let directory = GuestDirectory()
        let sessionStore = SessionStore(defaults: defaults)
        let repo = try await landedGuest(
            host: authHost(), directory: directory, persistence: MemoryPersistence(),
            sessionStore: sessionStore)
        // An unfinished Create account: the username is taken, the password
        // step failed.
        do {
            try await repo.claimGuestAccount(username: "mira", password: Data("correct horse".utf8))
            XCTFail("the password step is refused by the stub")
        } catch {}
        XCTAssertEqual(repo.guestClaimedUsername, "mira")

        var calls: [String] = []
        var directoryFails = true
        repo.guestEnvironment.deleteAccountAtAuth = { _ in calls.append("auth") }
        repo.guestEnvironment.deleteAccountInDirectory = { _ in
            calls.append("directory")
            if directoryFails {
                throw DiscoveryClientError.http(status: 500, endpoint: "/discovery/account", body: "")
            }
        }

        do {
            try await repo.discardGuestAccount()
            XCTFail("the username is still taken")
        } catch DiscoveryClientError.http(let status, _, _) {
            XCTAssertEqual(status, 500)
        }
        // The directory first, and nothing after its failure: the account
        // still exists everywhere, so the person can try again.
        XCTAssertEqual(calls, ["directory"])
        XCTAssertEqual(launchWiped, [], "nothing local is removed")
        XCTAssertTrue(repo.isGuest)
        XCTAssertEqual(repo.guestClaimedUsername, "mira")
        XCTAssertFalse(repo.guestAccountDeleted)
        XCTAssertEqual(try storedGuestPhrase(), phrase)
        XCTAssertEqual(sessionStore.load(), PersistedSession(accountIdB64: accountIdB64, username: "mira"))

        // The retry completes.
        directoryFails = false
        try await repo.discardGuestAccount()
        XCTAssertEqual(calls, ["directory", "directory", "auth"])
        XCTAssertEqual(launchWiped, [accountIdB64])
        XCTAssertEqual(repo.state, .anon(cachedUsername: nil))
        XCTAssertTrue(repo.guestAccountDeleted)
        XCTAssertNil(sessionStore.load())
    }

    // MARK: - E. The owed recovery phrase

    func testAnOwedPhraseComesBackAfterARelaunchUntilItIsAcknowledged() throws {
        let persistence = MemoryPersistence()
        persistence.deviceKey = true
        persistence.seed = seed
        persistence.tokens[accountIdB64] = PersistedSessionToken(
            sessionToken: "tok-live", sessionExpiresAtMs: Self.farFuture)
        let sessionStore = SessionStore(defaults: defaults)
        sessionStore.save(PersistedSession(accountIdB64: accountIdB64, username: "mira"))
        // What a kept guest account (or a sign-up) leaves behind when the app
        // is closed on the phrase screen: the mark, and the phrase in the
        // Keychain cache under the account's own key.
        SignupPhraseOwedStore(defaults: defaults).mark(accountIdB64: accountIdB64)
        try PhraseStore().putPhrase(accountIdB64: accountIdB64, phraseBytes: Data(phrase.utf8))

        let repo = repository(
            host: authHost(), directory: GuestDirectory(), persistence: persistence, sessionStore: sessionStore)

        XCTAssertEqual(repo.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "mira")))
        XCTAssertFalse(repo.isGuest)
        XCTAssertEqual(repo.pendingSignupPhrase, phrase)

        repo.acknowledgeSignupPhrase()
        XCTAssertNil(repo.pendingSignupPhrase)
        XCTAssertFalse(SignupPhraseOwedStore(defaults: defaults).isOwed(accountIdB64: accountIdB64))

        let again = repository(
            host: authHost(), directory: GuestDirectory(), persistence: persistence, sessionStore: sessionStore)
        XCTAssertNil(again.pendingSignupPhrase, "acknowledged once, never shown again")
    }

    func testAnOwedPhraseThatIsNotOnTheDeviceDoesNotBlockTheAccount() {
        let persistence = MemoryPersistence()
        persistence.deviceKey = true
        persistence.seed = seed
        persistence.tokens[accountIdB64] = PersistedSessionToken(
            sessionToken: "tok-live", sessionExpiresAtMs: Self.farFuture)
        let sessionStore = SessionStore(defaults: defaults)
        sessionStore.save(PersistedSession(accountIdB64: accountIdB64, username: "mira"))
        SignupPhraseOwedStore(defaults: defaults).mark(accountIdB64: accountIdB64)

        let repo = repository(
            host: authHost(), directory: GuestDirectory(), persistence: persistence, sessionStore: sessionStore)

        XCTAssertEqual(repo.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "mira")))
        XCTAssertNil(repo.pendingSignupPhrase)
    }

    func testLogOutDropsThePhraseFromMemoryAndKeepsItOwed() throws {
        let persistence = MemoryPersistence()
        persistence.deviceKey = true
        persistence.seed = seed
        persistence.tokens[accountIdB64] = PersistedSessionToken(
            sessionToken: "tok-live", sessionExpiresAtMs: Self.farFuture)
        let sessionStore = SessionStore(defaults: defaults)
        sessionStore.save(PersistedSession(accountIdB64: accountIdB64, username: "mira"))
        SignupPhraseOwedStore(defaults: defaults).mark(accountIdB64: accountIdB64)
        try PhraseStore().putPhrase(accountIdB64: accountIdB64, phraseBytes: Data(phrase.utf8))
        let repo = repository(
            host: authHost(), directory: GuestDirectory(), persistence: persistence, sessionStore: sessionStore)
        XCTAssertEqual(repo.pendingSignupPhrase, phrase)

        repo.logout()

        XCTAssertNil(repo.pendingSignupPhrase)
        XCTAssertTrue(SignupPhraseOwedStore(defaults: defaults).isOwed(accountIdB64: accountIdB64))
    }
}

// MARK: - The call-link landing's latch

/// `docs/ux/call-links.md` section 9 - after `No, thanks` on the call-link
/// landing, Join must work again on that same screen. On web a double-tap
/// latch set by the guest signup was never released after a successful
/// signup, so Join did nothing after the delete (found live 2026-10-02).
@MainActor
final class JoinCallLandingGuestLatchTests: XCTestCase {

    private var suiteName = ""
    private var defaults: UserDefaults!

    override func setUp() async throws {
        try await super.setUp()
        suiteName = "guest-landing-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)!
    }

    override func tearDown() async throws {
        UserDefaults().removePersistentDomain(forName: suiteName)
        try await super.tearDown()
    }

    /// A signed-out repository: every Join takes the guest path.
    private func signedOutRepository() -> AuthRepository {
        let host = NameRouteStub.scope { _, _, _ in (503, "") }
        return AuthRepository(
            sessionStore: SessionStore(defaults: defaults),
            authClientFactory: { NameRouteStub.client(host) },
            persistence: MemoryPersistence(),
            nowMs: { 1_000_000 },
            accountDirectory: GuestDirectory(),
            loginHandleBinds: LoginHandleBindLedger(),
            restoreDirectory: EmptyRestoreDirectory(),
            pushBinding: PushAccountBindingStore(defaults: defaults),
            removeDeliveredNotifications: {},
            backupDefaults: defaults,
            markerDefaults: defaults,
            pushSubscribe: { _, _ in },
            wipeAccountStores: { _ in }
        )
    }

    private var decoded: JoinCallLandingViewModel.Decoded {
        JoinCallLandingViewModel.Decoded(
            callId: Data(repeating: 0xC1, count: 16),
            ownerAccountId: Data(repeating: 0x0A, count: 16),
            ownerSignatureKey: Data(repeating: 0x0B, count: 32),
            ownerFingerprint: "",
            mediaVideo: false,
            valid: true,
            isOwner: false)
    }

    func testJoinWorksAgainAfterAGuestSignupThatSucceeded() async {
        let repo = signedOutRepository()
        var signups = 0
        let model = JoinCallLandingViewModel(
            slug: "slug#secret", authRepository: repo, intake: InviteIntake(),
            linkCallHost: LinkCallHost.live(authRepository: repo),
            // The sign-up succeeds; whether its session is still there when
            // Join is pressed again is the repository's (it is not, here, as
            // after `No, thanks`).
            guestSignup: { _, _ in signups += 1 })

        await model.onStart(decoded)
        XCTAssertEqual(signups, 1)
        XCTAssertEqual(model.phase, .idle, "the Join button is back")
        XCTAssertFalse(model.creatingGuest)

        await model.onStart(decoded)
        XCTAssertEqual(signups, 2, "the latch was released: Join makes a new guest account")
    }

    func testJoinWorksAgainAfterAGuestSignupThatFailed() async {
        struct Refused: Error {}
        let repo = signedOutRepository()
        var signups = 0
        let model = JoinCallLandingViewModel(
            slug: "slug#secret", authRepository: repo, intake: InviteIntake(),
            linkCallHost: LinkCallHost.live(authRepository: repo),
            guestSignup: { _, _ in
                signups += 1
                throw Refused()
            })

        await model.onStart(decoded)
        if case .error = model.phase {} else { XCTFail("the failure is shown, got \(model.phase)") }
        await model.onStart(decoded)
        XCTAssertEqual(signups, 2)
    }

    func testADoubleTapStillMakesOneGuestAccount() async {
        let repo = signedOutRepository()
        var signups = 0
        var release: CheckedContinuation<Void, Never>?
        let model = JoinCallLandingViewModel(
            slug: "slug#secret", authRepository: repo, intake: InviteIntake(),
            linkCallHost: LinkCallHost.live(authRepository: repo),
            guestSignup: { _, _ in
                signups += 1
                await withCheckedContinuation { release = $0 }
            })

        let d = decoded
        let first = Task { await model.onStart(d) }
        for _ in 0..<200 where release == nil { await Task.yield() }
        XCTAssertNotNil(release, "the first sign-up is in flight")
        // The second tap while the first is working does nothing.
        await model.onStart(d)
        XCTAssertEqual(signups, 1)

        release?.resume()
        await first.value
        XCTAssertEqual(signups, 1)
    }
}
