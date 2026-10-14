// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

// The recovery model on the repository: every landing path that gives the
// device keys also gives it the phrase, a passkey signs in on a device with
// no cached account and restores keys only when it carries them, the
// three-word check is local, and "Reset account keys" (ADR-0073) resets the
// keys in one server step and then owes the publish of the new keys.
//
// The auth service is a scripted `URLProtocol` (`NameRouteStub`), discovery
// a fake, the passkey sheet a closure, the reset's server step, attestation
// check and the two out-of-repository publish steps are scripted too. The
// core (`LakkoCore`) is real.

private func randomBytes(_ count: Int) -> Data {
    Data((0..<count).map { _ in UInt8.random(in: 0...255) })
}

/// Discovery, scripted: the published identity, the name lookup, and the
/// identity publish. Records what happened in order.
private final class ScriptedDirectory: AccountDirectory, DeviceRestoreDirectory {
    var publishedKey: Data?
    var lookup: Result<Data?, Error> = .success(nil)
    var registerOutcome: Result<RegisterResult, Error> = .success(RegisterResult(normalisedUsername: "alice"))
    var publishError: Error?
    private(set) var events: [String] = []
    private(set) var publishedKeys: [Data] = []

    func note(_ event: String) { events.append(event) }

    /// The generation the published identity reads (ADR-0073 section 4.1).
    var publishedGeneration: UInt64 = 1

    func fetchIdentityBundle(accountId: Data, sessionToken: String?) async throws -> IdentityFetchResponse? {
        events.append("identity")
        guard let publishedKey else { return nil }
        return IdentityFetchResponse(
            signaturePubkey: base64Encode(publishedKey), x25519Pubkey: "", keyGeneration: publishedGeneration)
    }

    func lookupAccount(username: String, accountId: Data, sessionToken: String) async throws -> Data? {
        events.append("lookup:\(username)")
        return try lookup.get()
    }

    func registerUsername(accountId: Data, username: String, sessionToken: String) async throws -> RegisterResult {
        events.append("register:\(username)")
        return try registerOutcome.get()
    }

    var replaceError: Error?
    func replaceUsername(accountId: Data, username: String, sessionToken: String) async throws -> ReplacedUsername {
        events.append("replace:\(username)")
        if let replaceError { throw replaceError }
        return ReplacedUsername(normalisedUsername: username, certificate: Data(repeating: 0xCE, count: 153))
    }

    func publishIdentityBundle(
        accountId: Data, signaturePubkey: Data, x25519Pubkey: Data,
        leafAuth: Data?, genesisAttest: Data?, keyResetAttest: Data?, sessionToken: String
    ) async throws {
        events.append(keyResetAttest == nil ? "publish" : "publish+attest")
        publishedAttestations.append(keyResetAttest)
        publishTokens.append(sessionToken)
        if let publishError { throw publishError }
        publishedKeys.append(signaturePubkey)
        publishedKey = signaturePubkey
    }

    private(set) var publishedAttestations: [Data?] = []
    private(set) var publishTokens: [String] = []
}

/// The Keychain seam of the relaunch restore, in memory.
private final class ScriptedPersistence: SessionPersistence {
    var deviceKey = false
    var seed: Data?
    private(set) var tokens: [String: PersistedSessionToken] = [:]

    func hasDeviceKey(accountIdB64: String) -> Bool { deviceKey }
    func accountIdentitySeed(accountIdB64: String) -> Data? { seed }
    func exportKey(accountIdB64: String) -> Data? { nil }
    func sessionToken(accountIdB64: String) -> PersistedSessionToken? { tokens[accountIdB64] }
    func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws { tokens[accountIdB64] = token }
    func deleteSessionToken(accountIdB64: String) { tokens[accountIdB64] = nil }
}

/// The key reset's server step, attestation check and out-of-repository
/// publish steps, scripted. Records the order everything happened in.
private final class ScriptedKeyReset: KeyResetAttestationVerifying {
    /// The reset's server step: what it throws, or the generation it answers.
    var resetError: Error?
    var generation: UInt64 = 2
    private(set) var resetCalls: [(token: String, grant: String, seed: Data, pubkey: Data)] = []
    var onReset: (() -> Void)?

    /// The password sign-in the reset runs when it holds no live grant
    /// (ADR-0073, amended 2026-10-04): the account it names, what it
    /// throws, the pair and the grant it answers.
    var loginAccount = Data()
    var loginError: Error?
    var loginSession: SessionPair = .fixture(access: "tok-access", refresh: "r1.totp")
    var loginGrant: FreshAuthGrant? = FreshAuthGrant(grant: "grant-1", expiresAtMs: 1_600_000)
    private(set) var loginCalls: [(handle: Data, password: Data)] = []

    func login(_ handle: Data, _ password: Data) async throws -> LoginResult {
        loginCalls.append((handle, password))
        if let loginError { throw loginError }
        return LoginResult(
            accountId: loginAccount, sessionKey: Data(), exportKey: Data(repeating: 0xE1, count: 64),
            session: loginSession, freshGrant: loginGrant)
    }
    /// The attestation bytes the server "mints": a function of the reset
    /// count, so a replay is told from a second mint.
    private(set) var minted: [Data] = []

    /// The attestation check. `stale` makes a check WITH a bound refuse as
    /// the core does for an attestation past it.
    var verifyError: Error?
    private(set) var verifyCalls: [(attestation: Data, pubkey: Data, maxAgeMs: Int64?)] = []
    var signedAtMs: Int64 = 1_000
    var stale = false
    var onVerify: (() -> Void)?

    /// The two hooks.
    var keyPackageError: Error?
    var deviceGroupError: Error?
    private(set) var keyPackageCalls: [IdentitySecrets] = []
    private(set) var deviceGroupCalls: [(accountIdB64: String, attestation: Data)] = []

    private(set) var events: [String] = []

    func reset(_ token: String, _ grant: String, _ seed: Data, _ pubkey: Data) async throws -> KeyResetResult {
        events.append("reset")
        resetCalls.append((token, grant, seed, pubkey))
        onReset?()
        if let resetError { throw resetError }
        let attestation = Data(repeating: UInt8(resetCalls.count), count: KeyResetResult.attestationLength)
        minted.append(attestation)
        return KeyResetResult(
            attestation: attestation,
            session: .fixture(access: "tok-gen\(generation)", refresh: "r1.gen\(generation)", generation: generation))
    }

    func verify(
        attestation: Data, accountId: Data, newSignaturePubkey: Data, nowMs: Int64, maxAgeMs: Int64?
    ) async throws -> KeyResetClaims {
        events.append("verify")
        verifyCalls.append((attestation, newSignaturePubkey, maxAgeMs))
        onVerify?()
        if let verifyError { throw verifyError }
        if stale, maxAgeMs != nil { throw KeyResetSignInRequired() }
        return KeyResetClaims(keyGeneration: generation, signedAtMs: signedAtMs)
    }

    var hooks: AuthRepository.KeyResetHooks {
        AuthRepository.KeyResetHooks(
            republishOwnKeyPackage: { [unowned self] secrets in
                self.events.append("keypackage")
                self.keyPackageCalls.append(secrets)
                if let error = self.keyPackageError { throw error }
            },
            refoundDeviceGroup: { [unowned self] _, accountIdB64, attestation, _ in
                self.events.append("devicegroup")
                self.deviceGroupCalls.append((accountIdB64, attestation))
                if let error = self.deviceGroupError { throw error }
            }
        )
    }
}

/// The passkey sheet, scripted.
private final class ScriptedPasskey {
    var result: PasskeyLoginResult?
    var error: Error?
    private(set) var askedFor: [String?] = []

    func run(_ accountIdB64Url: String?) async throws -> PasskeyLoginResult {
        askedFor.append(accountIdB64Url)
        if let error { throw error }
        guard let result else { throw AuthClientError.argument("no scripted passkey") }
        return result
    }
}

@MainActor
final class RecoveryFlowTests: XCTestCase {

    private var accountIdsToClean: [String] = []
    private var suites: [String] = []

    override func tearDown() async throws {
        for b64 in accountIdsToClean {
            DeviceKeyStore.deleteDeviceKey(accountIdB64: b64)
            AccountIdentitySeedStore.deleteAccountIdentitySeed(accountIdB64: b64)
            DeviceIdentityStore.deleteDeviceIdentitySeed(accountIdB64: b64)
            ExportKeyStore.deleteExportKey(accountIdB64: b64)
            SessionTokenStore.deleteSessionToken(accountIdB64: b64)
            try? PhraseStore().deletePhrase(accountIdB64: b64)
            ThreeWordsBudgetStore().clearAll(accountIdB64: b64)
        }
        accountIdsToClean = []
        for suite in suites { UserDefaults().removePersistentDomain(forName: suite) }
        suites = []
        try await super.tearDown()
    }

    // MARK: - Harness

    private struct Harness {
        let repository: AuthRepository
        let store: SessionStore
        let persistence: ScriptedPersistence
        let directory: ScriptedDirectory
        let reset: ScriptedKeyReset
        let passkey: ScriptedPasskey
        let markers: RecoveryMarkers
        let tokens: SessionTokenProvider
        let host: String
        /// The accounts the signed-out wipe ran for.
        let wiped: WipeLog
        /// The clock the reset grant's expiry is read against.
        let clock: Clock
    }

    private final class Clock {
        var nowMs: Int64 = 1_000_000
    }

    /// Reset the keys the way the warning screen does when no grant is
    /// held: with the password, for the account parked on the prompt.
    @discardableResult
    private func mint(_ h: Harness, password: String = "correct horse") async throws -> String {
        if case .needsRecoveryPhrase(let account) = h.repository.state,
           let bytes = base64Decode(account.accountIdB64) {
            h.reset.loginAccount = bytes
        }
        return try await h.repository.mintFreshIdentity(password: Data(password.utf8))
    }

    private final class WipeLog {
        private(set) var accounts: [String] = []
        func note(_ accountIdB64: String) { accounts.append(accountIdB64) }
    }

    private func defaults(_ name: String) -> UserDefaults {
        let suite = "\(name)-\(UUID().uuidString)"
        suites.append(suite)
        return UserDefaults(suiteName: suite)!
    }

    private func newAccount() -> (bytes: Data, b64: String) {
        let bytes = randomBytes(16)
        let b64 = base64Encode(bytes)
        accountIdsToClean.append(b64)
        return (bytes, b64)
    }

    /// A repository over a scripted auth service. `routes` answers by path;
    /// anything else is `503`.
    private func harness(
        marker: PersistedSession? = nil,
        persistence: ScriptedPersistence = ScriptedPersistence(),
        markers: RecoveryMarkers? = nil,
        routes: @escaping (_ path: String, _ json: [String: Any]) -> (Int, String)? = { _, _ in nil }
    ) -> Harness {
        let host = NameRouteStub.scope { _, path, json in routes(path, json) ?? (503, "") }
        let store = SessionStore(defaults: defaults("recovery-flow-session"))
        if let marker { store.save(marker) }
        let directory = ScriptedDirectory()
        let reset = ScriptedKeyReset()
        let passkey = ScriptedPasskey()
        let markers = markers ?? RecoveryMarkers(defaults: defaults("recovery-flow-markers"))
        let tokens = SessionTokenProvider()
        let wiped = WipeLog()
        let clock = Clock()
        let repository = AuthRepository(
            sessionStore: store,
            authClientFactory: { NameRouteStub.client(host) },
            persistence: persistence,
            nowMs: { 1_000_000 },
            accountDirectory: directory,
            loginHandleBinds: LoginHandleBindLedger(),
            restoreDirectory: directory,
            passkeyCeremony: { try await passkey.run($0) },
            markers: markers,
            tokenProvider: tokens,
            keyResetOperation: { try await reset.reset($0, $1, $2, $3) },
            proofLoginOperation: { try await reset.login($0, $1) },
            grantNowMs: { clock.nowMs },
            attestationVerifier: reset,
            keyResetHooks: reset.hooks,
            wipeAfterKeyReset: { wiped.note($0) },
            recallRecords: InMemoryRecallRecordStore(),
            recallOperations: .unreachable
        )
        return Harness(
            repository: repository, store: store, persistence: persistence, directory: directory,
            reset: reset, passkey: passkey, markers: markers, tokens: tokens, host: host, wiped: wiped,
            clock: clock)
    }

    private func totpRoute(_ account: Data) -> (String, [String: Any]) -> (Int, String)? {
        { path, _ in
            switch path {
            case "/totp/recover":
                return (200, """
                {"account_id_b64":"\(base64UrlEncodeNoPad(account))","session_token":"tok-access","session_expires_at_ms":9000000,"refresh_token":"r1.totp","refresh_expires_at_ms":9000000000,"key_generation":1}
                """)
            case "/auth/login-handle":
                return (200, #"{"bound":true}"#)
            default:
                return nil
            }
        }
    }

    private func identityKey(of mnemonic: RecoveryMnemonic) throws -> Data {
        try Identity.fromSeed(seed: mnemonic.toSeed()).signaturePublicKey()
    }

    private func waitUntil(_ condition: @MainActor () -> Bool, timeout: TimeInterval = 5) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 20_000_000)
        }
        return condition()
    }

    private func isSession(_ repository: AuthRepository) -> Bool {
        if case .session = repository.state { return true }
        return false
    }

    private func isNeedsKeys(_ repository: AuthRepository) -> Bool {
        if case .needsRecoveryPhrase = repository.state { return true }
        return false
    }

    /// Park a signed-in-without-keys state through the authenticator route.
    private func parkWithoutKeys(_ h: Harness) async throws {
        try await h.repository.recoverWithTotp(username: "alice", code: "123456")
        XCTAssertTrue(isNeedsKeys(h.repository))
        // A phrase-protected account: a key reset shows its new words
        // (ADR-0075; the other kind is `testAKeyResetWithoutASavedPhrase...`).
        if let accountIdB64 = h.store.load()?.accountIdB64 {
            AccountProtectionStore().setPhraseSaved(true, accountIdB64: accountIdB64)
        }
    }

    // MARK: - The phrase is stored on every landing path that gives keys

    func testSignupStoresThePhrase() async throws {
        let account = newAccount()
        let h = harness(routes: { path, _ in path == "/auth/login-handle" ? (200, #"{"bound":true}"#) : nil })
        let mnemonic = RecoveryMnemonic.generate()
        _ = try await h.repository.completeSignup(
            result: SignupResult(
                accountId: account.bytes, exportKey: Data(repeating: 0xE7, count: 64),
                recoveryPhrase: mnemonic.phrase(), sessionToken: "tok-signup", sessionExpiresAtMs: 9_000_000),
            username: "alice")
        XCTAssertTrue(isSession(h.repository))
        // The device keeps the phrase (it seals the email vault and is what
        // a phrase typed later is checked against)...
        XCTAssertEqual(DevicePhrase.read(accountIdB64: account.b64), mnemonic.phrase())
        XCTAssertEqual(h.repository.heldPhraseForEmailVault(), mnemonic.phrase())
        // ...but ADR-0075 D2: it is not shown at sign-up, and `Show recovery
        // phrase` has nothing to show until the person saved one.
        XCTAssertNil(h.repository.devicePhrase(), "no saved phrase, nothing to show")
        XCTAssertFalse(h.repository.threeWordsCheckAvailable)
        AccountProtectionStore().setPhraseSaved(true, accountIdB64: account.b64)
        XCTAssertEqual(h.repository.devicePhrase(), mnemonic.phrase(), "what Show recovery phrase reads")
    }

    func testAPhraseTypedInTheRestorePromptIsStoredOnlyAfterItMatchesTheAccount() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        XCTAssertTrue(h.repository.newPasswordOwed, "an authenticator sign-in is a forgot-password route")
        XCTAssertFalse(h.repository.newPasswordOffered, "not offered while the keys are missing")

        let mine = RecoveryMnemonic.generate()
        h.directory.publishedKey = try identityKey(of: mine)

        // Another account's phrase: refused, nothing written.
        do {
            try await h.repository.provideRecoveryPhrase(mnemonicPhrase: RecoveryMnemonic.generate().phrase())
            XCTFail("a phrase that is not the account's must be refused")
        } catch {
            XCTAssertTrue(error is PhraseMismatch, "\(error)")
        }
        XCTAssertNil(DevicePhrase.read(accountIdB64: account.b64))
        XCTAssertNil(try AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: account.b64))
        XCTAssertTrue(isNeedsKeys(h.repository))

        try await h.repository.provideRecoveryPhrase(mnemonicPhrase: mine.phrase())
        XCTAssertTrue(isSession(h.repository))
        XCTAssertEqual(DevicePhrase.read(accountIdB64: account.b64), mine.phrase())
        XCTAssertEqual(try AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: account.b64), mine.toSeed())
        // The keys are here now: the new-password step is offered, once.
        XCTAssertTrue(h.repository.newPasswordOffered)
        h.repository.dismissNewPasswordOffer()
        XCTAssertFalse(h.repository.newPasswordOffered)
    }

    func testKeysHandedOverByAnotherDeviceArriveAsEntropyAndStoreSeedAndPhrase() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        let mine = RecoveryMnemonic.generate()
        h.directory.publishedKey = try identityKey(of: mine)

        // Entropy of another phrase: refused before any write.
        do {
            try await h.repository.completeFromPairing(phraseEntropy: randomBytes(16))
            XCTFail("keys that are not the account's must be refused")
        } catch {
            XCTAssertTrue(error is PhraseMismatch, "\(error)")
        }
        XCTAssertNil(DevicePhrase.read(accountIdB64: account.b64))
        // A v1-sized seed is not entropy.
        do {
            try await h.repository.completeFromPairing(phraseEntropy: mine.toSeed())
            XCTFail("a 64-byte seed is not a v2 payload")
        } catch {
            XCTAssertTrue(error is RecoveryPayloadError, "\(error)")
        }

        try await h.repository.completeFromPairing(
            phraseEntropy: mine.toEntropy(),
            session: .fixture(access: "tok-redeemed", accessExpiresAtMs: 9_500_000))
        XCTAssertTrue(isSession(h.repository))
        XCTAssertEqual(h.repository.identitySecrets?.sessionToken, "tok-redeemed")
        XCTAssertEqual(DevicePhrase.read(accountIdB64: account.b64), mine.phrase())
        XCTAssertEqual(try AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: account.b64), mine.toSeed())
    }

    // MARK: - Reset account keys (ADR-0073)

    private func record(_ h: Harness, _ account: (bytes: Data, b64: String)) -> KeyResetPublishRecord? {
        h.markers.publishOwed(accountIdB64: account.b64)
    }

    func testAKeyResetWhoseServerStepFailsChangesNothing() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        h.reset.resetError = AuthClientError.network(
            endpoint: "/account/keys/reset/start", underlying: URLError(.notConnectedToInternet))

        do {
            _ = try await mint(h)
            XCTFail("a failed server step must fail the flow")
        } catch {}

        XCTAssertTrue(isNeedsKeys(h.repository), "back on the restore prompt")
        XCTAssertEqual(h.reset.events, ["reset"], "nothing after the server step")
        XCTAssertTrue(h.directory.events.filter { $0.hasPrefix("publish") }.isEmpty)
        XCTAssertNil(DevicePhrase.read(accountIdB64: account.b64))
        XCTAssertNil(try AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: account.b64))
        XCTAssertFalse(h.markers.isPublishOwed(accountIdB64: account.b64))
        XCTAssertFalse(h.markers.isPhraseOwed(accountIdB64: account.b64))
        XCTAssertEqual(h.tokens.current?.sessionToken, "tok-access", "the old pair stays")
    }

    func testAKeyResetWithoutASavedPhraseShowsNoWordsAndLandsTheSession() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        // Not phrase-protected (ADR-0075): no words on any screen.
        AccountProtectionStore().setPhraseSaved(false, accountIdB64: account.b64)

        _ = try await mint(h)

        XCTAssertTrue(isSession(h.repository), "no phrase screen, no withheld-words screen")
        XCTAssertNil(h.repository.freshPhrase())
        XCTAssertFalse(h.repository.freshPublishPending)
        XCTAssertFalse(h.markers.isPhraseOwed(accountIdB64: account.b64), "nothing waits to be confirmed")
        XCTAssertNil(h.repository.devicePhrase(), "and nothing to show in Settings")
        // The device keeps the new words unseen, and the publish still runs.
        XCTAssertNotNil(DevicePhrase.read(accountIdB64: account.b64))
        let published = await waitUntil { !h.markers.isPublishOwed(accountIdB64: account.b64) }
        XCTAssertTrue(published)
        XCTAssertEqual(h.directory.events, ["publish+attest"])
    }

    func testAnOwedPublishWithoutASavedPhraseShowsTheFinishingScreenThenLandsWithNoWords() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        AccountProtectionStore().setPhraseSaved(false, accountIdB64: account.b64)
        h.reset.deviceGroupError = DsStatementError.keyResetDisabled

        _ = try await mint(h)

        // The retry screen, with no words behind it.
        if case .saveFreshPhrase = h.repository.state {} else { XCTFail("the finishing screen") }
        XCTAssertTrue(h.repository.freshPublishPending)
        XCTAssertFalse(h.repository.freshShowsWords)
        XCTAssertNil(h.repository.freshPhrase())

        // `Try again` finishes the publish and the session lands at once.
        h.reset.deviceGroupError = nil
        try await h.repository.retryFreshPublish()
        XCTAssertTrue(isSession(h.repository), "no phrase screen")
        XCTAssertFalse(h.markers.isPublishOwed(accountIdB64: account.b64))
        XCTAssertFalse(h.markers.isPhraseOwed(accountIdB64: account.b64))
        XCTAssertNil(h.repository.devicePhrase())
    }

    func testAKeyResetPersistsThenPublishesInOrderThenShowsThePhrase() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        var seedStoredAtVerify: Data?
        var phraseStoredAtVerify: String?
        var pairStoredAtVerify: PersistedSessionToken?
        var ownedAtVerify = false
        h.reset.onVerify = { [persistence = h.persistence, markers = h.markers] in
            seedStoredAtVerify = try? AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: account.b64)
            phraseStoredAtVerify = DevicePhrase.read(accountIdB64: account.b64)
            pairStoredAtVerify = persistence.tokens[account.b64]
            ownedAtVerify = markers.isPublishOwed(accountIdB64: account.b64)
        }

        let phrase = try await mint(h)
        let seed = try RecoveryMnemonic.fromPhrase(phrase: phrase).toSeed()
        let newKey = try Identity.fromSeed(seed: seed).signaturePublicKey()

        XCTAssertEqual(PhraseWords.words(of: phrase).count, 12)
        // The server step is made with the OLD pair's token, over the new
        // seed and the new key.
        XCTAssertEqual(h.reset.resetCalls.count, 1)
        XCTAssertEqual(h.reset.resetCalls[0].token, "tok-access")
        XCTAssertEqual(h.reset.resetCalls[0].seed, seed)
        XCTAssertEqual(h.reset.resetCalls[0].pubkey, newKey)
        // Everything is on the device BEFORE the first publish call.
        XCTAssertEqual(seedStoredAtVerify, seed)
        XCTAssertEqual(phraseStoredAtVerify, phrase)
        XCTAssertEqual(pairStoredAtVerify?.sessionToken, "tok-gen2")
        XCTAssertEqual(pairStoredAtVerify?.keyGeneration, 2)
        XCTAssertTrue(ownedAtVerify, "the publish is owed from the moment the reset landed")
        // The publish, in the ADR's order, with the attestation verified first
        // and carried to the directory and the DeviceGroup.
        XCTAssertEqual(h.reset.events, ["reset", "verify", "keypackage", "devicegroup"])
        XCTAssertEqual(h.directory.events.filter { $0.hasPrefix("publish") }, ["publish+attest"])
        XCTAssertEqual(h.directory.publishedAttestations, [h.reset.minted[0]])
        XCTAssertEqual(h.directory.publishTokens, ["tok-gen2"], "the directory upload uses the NEW pair")
        XCTAssertEqual(h.directory.publishedKeys.last, newKey)
        XCTAssertEqual(h.reset.verifyCalls[0].attestation, h.reset.minted[0])
        XCTAssertEqual(h.reset.verifyCalls[0].pubkey, newKey)
        XCTAssertNil(h.reset.verifyCalls[0].maxAgeMs, "just minted: no freshness bound")
        XCTAssertEqual(h.reset.keyPackageCalls.first?.session.sessionToken, "tok-gen2")
        XCTAssertEqual(h.reset.deviceGroupCalls.first?.attestation, h.reset.minted[0])
        XCTAssertEqual(h.reset.deviceGroupCalls.first?.accountIdB64, account.b64)

        XCTAssertFalse(h.repository.freshPublishPending)
        XCTAssertFalse(h.markers.isPublishOwed(accountIdB64: account.b64), "nothing is owed once every step landed")
        XCTAssertEqual(h.repository.freshPhrase(), phrase)
        XCTAssertTrue(h.markers.isPhraseOwed(accountIdB64: account.b64), "owed until the person confirms it")
        XCTAssertTrue(h.repository.keysResetThisSession)
        XCTAssertEqual(h.tokens.current?.keyGeneration, 2)
        if case .saveFreshPhrase = h.repository.state {} else { XCTFail("the phrase screen comes first") }

        try h.repository.confirmFreshPhraseSaved()
        XCTAssertTrue(isSession(h.repository))
        XCTAssertEqual(h.repository.identitySecrets?.session.sessionToken, "tok-gen2")
        XCTAssertFalse(h.markers.isPhraseOwed(accountIdB64: account.b64))
        // Reached through a forgot-password route with a username: the new
        // credential makes the password reset work, so the offer stays.
        XCTAssertTrue(h.repository.newPasswordOffered)
    }

    func testAFailedPublishStepWithholdsThePhraseAndTryAgainRepeatsOnlyWhatIsLeft() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        h.directory.publishError = DiscoveryClientError.http(
            status: 429, endpoint: "/discovery/identity",
            body: #"{"error":"identity-changed-too-recently","retry_after_secs":90}"#)

        _ = try await mint(h)

        XCTAssertTrue(h.repository.freshPublishPending)
        XCTAssertTrue(h.markers.isPublishOwed(accountIdB64: account.b64))
        XCTAssertTrue(h.markers.isPhraseOwed(accountIdB64: account.b64))
        XCTAssertEqual(h.reset.events, ["reset", "verify"], "the discovery step failed first")
        XCTAssertThrowsError(try h.repository.confirmFreshPhraseSaved()) { error in
            XCTAssertTrue(error is FreshPublishPending, "\(error)")
        }
        if case .saveFreshPhrase = h.repository.state {} else { XCTFail("still on the phrase screen") }

        // Try again while the directory still refuses: still pending.
        do {
            try await h.repository.retryFreshPublish()
            XCTFail("the directory still refuses")
        } catch {
            XCTAssertEqual((error as? KeyResetPublishFailed)?.step, "discovery")
        }
        XCTAssertTrue(h.repository.freshPublishPending)

        // The directory accepts; the KeyPackage step fails: what landed is
        // recorded and not repeated.
        h.directory.publishError = nil
        h.reset.keyPackageError = DiscoveryClientError.argument("upload failed")
        do {
            try await h.repository.retryFreshPublish()
            XCTFail("the KeyPackage step fails")
        } catch {
            XCTAssertEqual((error as? KeyResetPublishFailed)?.step, "keyPackage")
        }
        XCTAssertEqual(record(h, account)?.discoveryDone, true)
        XCTAssertEqual(record(h, account)?.keyPackageDone, false)

        h.reset.keyPackageError = nil
        try await h.repository.retryFreshPublish()
        XCTAssertFalse(h.repository.freshPublishPending)
        XCTAssertFalse(h.markers.isPublishOwed(accountIdB64: account.b64))
        XCTAssertEqual(h.reset.resetCalls.count, 1, "never resets again")
        XCTAssertEqual(h.directory.events.filter { $0.hasPrefix("publish") }.count, 3)
        XCTAssertEqual(
            Set(h.directory.publishedAttestations.compactMap { $0 }).count, 1,
            "every attempt carries the SAME attestation")
        XCTAssertEqual(h.reset.verifyCalls.count, 4, "the attestation is checked on every run")
        XCTAssertEqual(h.reset.events, [
            "reset", "verify", "verify", "verify", "keypackage", "verify", "keypackage", "devicegroup",
        ])
        XCTAssertEqual(h.reset.verifyCalls.last?.maxAgeMs, KeyResetPublish.maxAttestationAgeMs,
                       "Try again applies the 30-day bound")

        try h.repository.confirmFreshPhraseSaved()
        XCTAssertTrue(isSession(h.repository))
    }

    func testADirectoryThatAlreadyHoldsTheResetCountsAsDone() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        h.directory.publishError = DiscoveryClientError.http(
            status: 409, endpoint: "/discovery/identity", body: #"{"error":"key-generation-not-newer"}"#)

        _ = try await mint(h)

        XCTAssertFalse(h.repository.freshPublishPending, "a replay of a landed reset is done")
        XCTAssertEqual(h.reset.events, ["reset", "verify", "keypackage", "devicegroup"])
    }

    func testARelaunchWithThePublishStillOwedRetriesItWithTheStoredAttestation() async throws {
        let account = newAccount()
        let mnemonic = RecoveryMnemonic.generate()
        let newKey = try Identity.fromSeed(seed: mnemonic.toSeed()).signaturePublicKey()
        try PhraseStore().putPhrase(accountIdB64: account.b64, phraseBytes: Data(mnemonic.phrase().utf8))
        let persistence = ScriptedPersistence()
        persistence.deviceKey = true
        persistence.seed = mnemonic.toSeed()
        try persistence.putSessionToken(
            PersistedSessionToken(pair: .fixture(access: "tok-kept", generation: 2)), accountIdB64: account.b64)
        let markers = RecoveryMarkers(defaults: defaults("recovery-flow-markers"))
        markers.markPhraseOwed(accountIdB64: account.b64)
        let stored = Data(repeating: 0x5C, count: KeyResetResult.attestationLength)
        var owed = KeyResetPublishRecord(
            attestation: stored, keyGeneration: 2, signedAtMs: 1_000, newSignaturePubkey: newKey)
        owed.discoveryDone = true
        markers.markPublishOwed(owed, accountIdB64: account.b64)

        let h = harness(
            marker: PersistedSession(accountIdB64: account.b64, username: "alice"),
            persistence: persistence, markers: markers)

        if case .saveFreshPhrase(let shown) = h.repository.state {
            XCTAssertEqual(shown.accountIdB64, account.b64)
        } else {
            XCTFail("the owed phrase comes before the session")
        }
        XCTAssertNil(h.repository.identitySecrets)
        XCTAssertEqual(h.repository.freshPhrase(), mnemonic.phrase())
        // The owed publish is retried in the background with the STORED
        // attestation, only for the steps still owed, under the 30-day bound.
        let settled = await waitUntil { !h.repository.freshPublishPending }
        XCTAssertTrue(settled)
        XCTAssertEqual(h.reset.events, ["verify", "keypackage", "devicegroup"], "the directory step was done")
        XCTAssertEqual(h.reset.verifyCalls.first?.attestation, stored)
        XCTAssertEqual(h.reset.verifyCalls.first?.maxAgeMs, KeyResetPublish.maxAttestationAgeMs)
        XCTAssertEqual(h.reset.deviceGroupCalls.first?.attestation, stored)
        XCTAssertEqual(h.reset.resetCalls.count, 0, "never resets")
        XCTAssertFalse(markers.isPublishOwed(accountIdB64: account.b64))

        try h.repository.confirmFreshPhraseSaved()
        XCTAssertTrue(isSession(h.repository))
        XCTAssertFalse(markers.isPhraseOwed(accountIdB64: account.b64))
    }

    func testAnAttestationOlderThanThirtyDaysAsksForASignInFirst() async throws {
        let account = newAccount()
        let mnemonic = RecoveryMnemonic.generate()
        let newKey = try Identity.fromSeed(seed: mnemonic.toSeed()).signaturePublicKey()
        try PhraseStore().putPhrase(accountIdB64: account.b64, phraseBytes: Data(mnemonic.phrase().utf8))
        let persistence = ScriptedPersistence()
        persistence.deviceKey = true
        persistence.seed = mnemonic.toSeed()
        try persistence.putSessionToken(
            PersistedSessionToken(pair: .fixture(access: "tok-kept", generation: 2)), accountIdB64: account.b64)
        let markers = RecoveryMarkers(defaults: defaults("recovery-flow-markers"))
        markers.markPhraseOwed(accountIdB64: account.b64)
        markers.markPublishOwed(
            KeyResetPublishRecord(
                attestation: Data(repeating: 0x5C, count: KeyResetResult.attestationLength),
                keyGeneration: 2, signedAtMs: 1_000, newSignaturePubkey: newKey),
            accountIdB64: account.b64)

        let h = harness(
            marker: PersistedSession(accountIdB64: account.b64, username: "alice"),
            persistence: persistence, markers: markers)
        // The scripted check reads the attestation as minted long ago.
        h.reset.stale = true

        let asked = await waitUntil { h.repository.freshPublishNeedsSignIn }
        XCTAssertTrue(asked, "the retry asks for a sign-in instead of applying a weeks-old reset")
        XCTAssertTrue(h.repository.freshPublishPending)
        XCTAssertTrue(markers.isPublishOwed(accountIdB64: account.b64), "nothing was sent")
        XCTAssertEqual(h.reset.events, ["verify"])
        do {
            try await h.repository.retryFreshPublish()
            XCTFail("still too old")
        } catch {
            XCTAssertTrue(error is KeyResetSignInRequired, "\(error)")
        }
    }

    func testASessionLandingRetriesAnOwedPublishInTheBackground() async throws {
        let account = newAccount()
        let mnemonic = RecoveryMnemonic.generate()
        let newKey = try Identity.fromSeed(seed: mnemonic.toSeed()).signaturePublicKey()
        let persistence = ScriptedPersistence()
        persistence.deviceKey = true
        persistence.seed = mnemonic.toSeed()
        try persistence.putSessionToken(
            PersistedSessionToken(pair: .fixture(access: "tok-kept", generation: 2)), accountIdB64: account.b64)
        let markers = RecoveryMarkers(defaults: defaults("recovery-flow-markers"))
        var owed = KeyResetPublishRecord(
            attestation: Data(repeating: 0x5C, count: KeyResetResult.attestationLength),
            keyGeneration: 2, signedAtMs: 1_000, newSignaturePubkey: newKey)
        owed.discoveryDone = true
        owed.keyPackageDone = true
        markers.markPublishOwed(owed, accountIdB64: account.b64)

        let h = harness(
            marker: PersistedSession(accountIdB64: account.b64, username: "alice"),
            persistence: persistence, markers: markers)

        XCTAssertTrue(isSession(h.repository))
        let settled = await waitUntil { !markers.isPublishOwed(accountIdB64: account.b64) }
        XCTAssertTrue(settled)
        XCTAssertEqual(h.reset.events, ["verify", "devicegroup"])
        XCTAssertEqual(h.reset.deviceGroupCalls.map(\.accountIdB64), [account.b64])
    }

    // MARK: - The password-fresh reset grant (ADR-0073, amended 2026-10-04)

    func testAResetWithoutAGrantOrAPasswordAsksForThePasswordBeforeAnyCall() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        XCTAssertFalse(h.repository.holdsLiveFreshAuthForPendingRestore(), "an authenticator sign-in mints none")

        do {
            _ = try await h.repository.mintFreshIdentity()
            XCTFail("no grant and no password")
        } catch {
            XCTAssertTrue(error is FreshAuthRequired, "\(error)")
        }
        XCTAssertTrue(h.reset.loginCalls.isEmpty)
        XCTAssertTrue(h.reset.resetCalls.isEmpty)
        XCTAssertTrue(isNeedsKeys(h.repository))
    }

    func testAPasswordOnTheWarningScreenSignsInInstallsThePairAndResetsWithTheGrant() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        h.reset.loginSession = .fixture(access: "tok-relogin", refresh: "r1.relogin")
        var tokenAtReset: String?
        var stateAtReset = false
        h.reset.onReset = { [tokens = h.tokens, repository = h.repository] in
            tokenAtReset = tokens.current?.sessionToken
            if case .needsRecoveryPhrase = repository.state { stateAtReset = true }
        }

        try await mint(h)

        XCTAssertEqual(h.reset.loginCalls.count, 1)
        XCTAssertEqual(h.reset.loginCalls[0].handle, try loginHandle(username: "alice"))
        XCTAssertEqual(h.reset.loginCalls[0].password, Data("correct horse".utf8))
        XCTAssertEqual(h.reset.resetCalls.count, 1)
        XCTAssertEqual(h.reset.resetCalls[0].grant, "grant-1")
        XCTAssertEqual(h.reset.resetCalls[0].token, "tok-relogin", "the reset runs on the re-login's pair")
        XCTAssertEqual(tokenAtReset, "tok-relogin", "installed before the reset")
        XCTAssertTrue(stateAtReset, "the re-login lands no session: still on the restore prompt")
        XCTAssertTrue(h.wiped.accounts.isEmpty, "the re-login triggers no wipe")
        XCTAssertTrue(h.directory.events.filter { $0.hasPrefix("publish") } == ["publish+attest"],
                      "the re-login publishes nothing itself")
        XCTAssertEqual(h.tokens.current?.sessionToken, "tok-gen2")
    }

    func testAWrongPasswordResetsNothing() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        h.reset.loginError = AuthClientError.http(status: 401, endpoint: "/login/finish", body: "")

        do {
            try await mint(h)
            XCTFail("a wrong password")
        } catch {
            XCTAssertTrue(error is FreshAuthWrongPassword, "\(error)")
        }
        XCTAssertTrue(h.reset.resetCalls.isEmpty)
        XCTAssertEqual(h.tokens.current?.sessionToken, "tok-access", "the parked pair stays")
        XCTAssertTrue(isNeedsKeys(h.repository))
        XCTAssertFalse(h.repository.holdsLiveFreshAuthForPendingRestore())
    }

    func testASignInThatNamesAnotherAccountIsRefusedAndInstallsNothing() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        h.reset.loginSession = .fixture(access: "tok-other")

        do {
            _ = try await h.repository.mintFreshIdentity(password: Data("pw".utf8))
            XCTFail("the scripted sign-in names no account of this prompt")
        } catch {
            XCTAssertTrue(error is FreshAuthWrongPassword, "\(error)")
        }
        XCTAssertTrue(h.reset.resetCalls.isEmpty)
        XCTAssertEqual(h.tokens.current?.sessionToken, "tok-access")
    }

    func testARateLimitedSignInPassesThroughForTheSharedLine() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        h.reset.loginError = AuthClientError.http(
            status: 429, endpoint: "/login/start", body: #"{"error":"too-many-attempts","retry_after_secs":40}"#)

        do {
            try await mint(h)
            XCTFail("rate limited")
        } catch {
            XCTAssertEqual(RetryCountdown.attemptsRetryAfterSecs(error), 40)
        }
        XCTAssertTrue(h.reset.resetCalls.isEmpty)
    }

    func testA403FromTheResetDropsTheGrantAndAsksForThePassword() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        h.reset.resetError = AuthClientError.http(
            status: 403, endpoint: "/account/keys/reset/start", body: #"{"error":"fresh-auth-required"}"#)

        do {
            try await mint(h)
            XCTFail("the server refused the grant")
        } catch {
            XCTAssertTrue(error is FreshAuthRequired, "\(error)")
        }
        XCTAssertFalse(h.repository.holdsLiveFreshAuthForPendingRestore(), "the refused grant is dropped")
        XCTAssertTrue(isNeedsKeys(h.repository))
        XCTAssertFalse(h.markers.isPublishOwed(accountIdB64: account.b64))
    }

    func testAGrantKeptAfterAFailedResetServesTheNextAttemptWithoutASecondSignIn() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        h.reset.resetError = AuthClientError.network(
            endpoint: "/account/keys/reset/start", underlying: URLError(.notConnectedToInternet))
        do { try await mint(h); XCTFail("offline") } catch {}
        XCTAssertTrue(h.repository.holdsLiveFreshAuthForPendingRestore())

        h.reset.resetError = nil
        _ = try await h.repository.mintFreshIdentity()

        XCTAssertEqual(h.reset.loginCalls.count, 1, "one sign-in")
        XCTAssertEqual(h.reset.resetCalls.map(\.grant), ["grant-1", "grant-1"])
        XCTAssertFalse(h.repository.holdsLiveFreshAuthForPendingRestore(), "a successful reset spends the grant")
    }

    func testAnExpiredGrantIsNotSent() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        h.reset.resetError = AuthClientError.network(
            endpoint: "/account/keys/reset/start", underlying: URLError(.notConnectedToInternet))
        do { try await mint(h); XCTFail("offline") } catch {}
        h.reset.resetError = nil

        h.clock.nowMs = 1_600_000
        XCTAssertFalse(h.repository.holdsLiveFreshAuthForPendingRestore())
        do {
            _ = try await h.repository.mintFreshIdentity()
            XCTFail("the grant expired")
        } catch {
            XCTAssertTrue(error is FreshAuthRequired, "\(error)")
        }
        XCTAssertEqual(h.reset.resetCalls.count, 1, "nothing sent with the expired grant")
    }

    func testSigningOutDropsTheGrant() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        h.reset.resetError = AuthClientError.network(
            endpoint: "/account/keys/reset/start", underlying: URLError(.notConnectedToInternet))
        do { try await mint(h); XCTFail("offline") } catch {}
        XCTAssertTrue(h.repository.holdsLiveFreshAuthForPendingRestore())

        h.repository.logout()
        try await parkWithoutKeys(h)

        XCTAssertFalse(h.repository.holdsLiveFreshAuthForPendingRestore())
    }

    // MARK: - The reset grant on a device that knows no username

    /// Park signed-in-without-keys through a passkey sign-in that left no
    /// name on the device (the typed hint was another account's).
    private func parkWithoutKeysOrName(_ h: Harness, _ account: Data) async throws {
        h.passkey.result = assertion(account)
        h.directory.lookup = .success(randomBytes(16))
        try await h.repository.loginWithPasskey(usernameHint: "mallory")
        guard case .needsRecoveryPhrase(let parked) = h.repository.state else {
            return XCTFail("expected the restore prompt")
        }
        XCTAssertEqual(parked.username, "")
    }

    func testWithNoUsernameOnTheDeviceTheTypedNameAddressesThePasswordSignIn() async throws {
        let account = newAccount()
        let h = harness()
        try await parkWithoutKeysOrName(h, account.bytes)
        h.reset.loginAccount = account.bytes
        h.reset.loginSession = .fixture(access: "tok-relogin", refresh: "r1.relogin")

        _ = try await h.repository.mintFreshIdentity(password: Data("pw".utf8), username: " @Alice ")

        XCTAssertEqual(h.reset.loginCalls.count, 1)
        XCTAssertEqual(h.reset.loginCalls[0].handle, try loginHandle(username: "alice"))
        XCTAssertEqual(h.reset.resetCalls.map(\.grant), ["grant-1"])
        XCTAssertEqual(h.reset.resetCalls[0].token, "tok-relogin")
    }

    func testWithNoUsernameOnTheDeviceAndNoneTypedNothingIsSent() async throws {
        let account = newAccount()
        let h = harness()
        try await parkWithoutKeysOrName(h, account.bytes)

        for typed in [nil, "", "   ", "no"] as [String?] {
            do {
                _ = try await h.repository.mintFreshIdentity(password: Data("pw".utf8), username: typed)
                XCTFail("no usable name: \(String(describing: typed))")
            } catch {
                XCTAssertTrue(error is FreshAuthUsernameRequired, "\(error)")
            }
        }
        XCTAssertTrue(h.reset.loginCalls.isEmpty)
        XCTAssertTrue(h.reset.resetCalls.isEmpty)
        XCTAssertTrue(isNeedsKeys(h.repository))
    }

    func testATypedNameOfAnotherAccountIsAWrongPasswordAndInstallsNothing() async throws {
        let account = newAccount()
        let h = harness()
        try await parkWithoutKeysOrName(h, account.bytes)
        // The name and password are valid, for somebody else's account.
        h.reset.loginAccount = randomBytes(16)
        h.reset.loginSession = .fixture(access: "tok-other", refresh: "r1.other")

        do {
            _ = try await h.repository.mintFreshIdentity(password: Data("pw".utf8), username: "bob")
            XCTFail("the typed name signs in to another account")
        } catch {
            XCTAssertTrue(error is FreshAuthWrongPassword, "\(error)")
        }
        XCTAssertEqual(h.reset.loginCalls.count, 1)
        XCTAssertTrue(h.reset.resetCalls.isEmpty)
        XCTAssertEqual(h.tokens.current?.sessionToken, "tok-passkey", "the parked pair stays")
        XCTAssertEqual(h.repository.pendingSessionToken, "tok-passkey")
        XCTAssertFalse(h.repository.holdsLiveFreshAuthForPendingRestore(), "the other account's grant is not kept")
        XCTAssertTrue(isNeedsKeys(h.repository))
    }

    func testAKnownUsernameIsUsedAndATypedOneIgnored() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        h.reset.loginAccount = account.bytes

        _ = try await h.repository.mintFreshIdentity(password: Data("pw".utf8), username: "mallory")

        XCTAssertEqual(h.reset.loginCalls.map(\.handle), [try loginHandle(username: "alice")])
    }

    // MARK: - Passkey

    private func assertion(
        _ account: Data, credentialId: Data = Data([1, 2, 3, 4]), prfOutput: Data? = nil
    ) -> PasskeyLoginResult {
        PasskeyLoginResult(
            accountIdB64: base64UrlEncodeNoPad(account), sessionToken: "tok-passkey",
            sessionExpiresAtMs: 9_000_000, credentialId: credentialId, prfOutput: prfOutput)
    }

    func testAPasskeySignsInOnADeviceWithNoCachedAccountAndParksWithoutKeys() async throws {
        let account = newAccount()
        let h = harness()
        h.passkey.result = assertion(account.bytes)
        h.directory.lookup = .success(account.bytes)

        try await h.repository.loginWithPasskey(usernameHint: "@Alice")

        XCTAssertEqual(h.passkey.askedFor, [nil], "discoverable: no account named")
        if case .needsRecoveryPhrase(let parked) = h.repository.state {
            XCTAssertEqual(parked.accountIdB64, account.b64)
            XCTAssertEqual(parked.username, "alice", "the hint is kept: discovery resolved it to this account")
        } else {
            XCTFail("signed in without keys: the restore prompt")
        }
        XCTAssertEqual(h.repository.restoreNotice, .passkeyNoKeys, "no PRF output: the passkey signs in only")
        XCTAssertEqual(h.repository.pendingSessionToken, "tok-passkey")
        XCTAssertEqual(h.store.load()?.accountIdB64, account.b64)
        XCTAssertTrue(NameRouteStub.requests(h.host).isEmpty, "without a PRF output the vault is not asked")
        XCTAssertFalse(h.repository.newPasswordOwed, "the Login button is not a forgot-password route")
    }

    func testATypedNameThatIsNotThePasskeysAccountIsNotKept() async throws {
        let account = newAccount()
        let h = harness()
        h.passkey.result = assertion(account.bytes)
        h.directory.lookup = .success(randomBytes(16))

        try await h.repository.loginWithPasskey(usernameHint: "mallory", forgotPassword: true)

        if case .needsRecoveryPhrase(let parked) = h.repository.state {
            XCTAssertEqual(parked.username, "")
        } else {
            XCTFail("expected the restore prompt")
        }
        XCTAssertEqual(h.store.load()?.username, "")
        XCTAssertTrue(h.repository.newPasswordOwed)
    }

    func testAPasskeyThatCarriesTheKeysRestoresThemAndThePrfOutputNeverLeavesTheDevice() async throws {
        let account = newAccount()
        let mine = RecoveryMnemonic.generate()
        let prf = randomBytes(32)
        let credentialId = randomBytes(20)
        let sealed = try passkeyVaultSeal(mnemonic: mine, prfOutput: prf)
        let h = harness(routes: { path, _ in
            path == "/vault/passkey/get"
                ? (200, #"{"sealed_entropy_b64":"\#(base64Encode(sealed))"}"#) : nil
        })
        h.passkey.result = assertion(account.bytes, credentialId: credentialId, prfOutput: prf)
        h.directory.publishedKey = try identityKey(of: mine)

        try await h.repository.loginWithPasskey()

        XCTAssertTrue(isSession(h.repository))
        XCTAssertNil(h.repository.restoreNotice)
        XCTAssertEqual(DevicePhrase.read(accountIdB64: account.b64), mine.phrase())
        XCTAssertEqual(try AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: account.b64), mine.toSeed())

        let requests = NameRouteStub.requests(h.host)
        let vault = try XCTUnwrap(requests.first { $0.path == "/vault/passkey/get" })
        XCTAssertEqual(vault.authorization, "Bearer tok-passkey")
        XCTAssertEqual(vault.json["credential_id_b64"] as? String, base64UrlEncodeNoPad(credentialId))
        // The PRF output is in no request this sign-in made.
        let secretForms = [base64Encode(prf), base64UrlEncodeNoPad(prf)]
        for request in requests {
            let body = String(data: try JSONSerialization.data(withJSONObject: request.json), encoding: .utf8) ?? ""
            for form in secretForms {
                XCTAssertFalse(body.contains(form), "\(request.path) carries the PRF output")
            }
        }
    }

    func testKeysThatDoNotOpenOrAreNotTheAccountsLeaveTheRestorePromptWithItsNotice() async throws {
        let mine = RecoveryMnemonic.generate()
        let prf = randomBytes(32)

        // Sealed under another passkey's output: it does not open.
        let account1 = newAccount()
        let foreign = try passkeyVaultSeal(mnemonic: mine, prfOutput: randomBytes(32))
        let h1 = harness(routes: { path, _ in
            path == "/vault/passkey/get"
                ? (200, #"{"sealed_entropy_b64":"\#(base64Encode(foreign))"}"#) : nil
        })
        h1.passkey.result = assertion(account1.bytes, prfOutput: prf)
        h1.directory.publishedKey = try identityKey(of: mine)
        try await h1.repository.loginWithPasskey()
        XCTAssertTrue(isNeedsKeys(h1.repository))
        XCTAssertEqual(h1.repository.restoreNotice, .passkeyUnusable)
        XCTAssertNil(DevicePhrase.read(accountIdB64: account1.b64))

        // It opens, but the phrase inside is not this account's.
        let account2 = newAccount()
        let sealed = try passkeyVaultSeal(mnemonic: mine, prfOutput: prf)
        let h2 = harness(routes: { path, _ in
            path == "/vault/passkey/get"
                ? (200, #"{"sealed_entropy_b64":"\#(base64Encode(sealed))"}"#) : nil
        })
        h2.passkey.result = assertion(account2.bytes, prfOutput: prf)
        h2.directory.publishedKey = try identityKey(of: RecoveryMnemonic.generate())
        try await h2.repository.loginWithPasskey()
        XCTAssertTrue(isNeedsKeys(h2.repository))
        XCTAssertEqual(h2.repository.restoreNotice, .passkeyUnusable)
        XCTAssertNil(DevicePhrase.read(accountIdB64: account2.b64))
        XCTAssertNil(try AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: account2.b64))

        // Nothing stored for the passkey, or a server that keeps no keys.
        for answer in [(200, #"{"sealed_entropy_b64":null}"#), (503, "")] {
            let account = newAccount()
            let h = harness(routes: { path, _ in path == "/vault/passkey/get" ? answer : nil })
            h.passkey.result = assertion(account.bytes, prfOutput: prf)
            try await h.repository.loginWithPasskey()
            XCTAssertTrue(isNeedsKeys(h.repository))
            XCTAssertEqual(h.repository.restoreNotice, .passkeyNoKeys, "\(answer.0)")
        }
    }

    func testAPasskeyOnADeviceThatHoldsTheKeysLandsSignedInAndOffersANewPasswordWhenForgotten() async throws {
        let account = newAccount()
        let mine = RecoveryMnemonic.generate()
        try AccountIdentitySeedStore.putAccountIdentitySeed(accountIdB64: account.b64, seed: mine.toSeed())
        let h = harness(marker: PersistedSession(accountIdB64: account.b64, username: "alice"))
        h.passkey.result = assertion(account.bytes)

        try await h.repository.loginWithPasskey(forgotPassword: true)

        XCTAssertTrue(isSession(h.repository))
        XCTAssertEqual(h.repository.identitySecrets?.accountIdentitySeed, mine.toSeed())
        XCTAssertEqual(h.repository.identitySecrets?.sessionToken, "tok-passkey")
        XCTAssertTrue(h.directory.events.filter { $0.hasPrefix("lookup") }.isEmpty, "the cached name is used")
        XCTAssertTrue(h.repository.newPasswordOffered)
    }

    func testTheRestorePromptsPasskeyRowIsLimitedToTheSignedInAccount() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)

        // Another account's passkey answered.
        h.passkey.result = assertion(randomBytes(16))
        do {
            _ = try await h.repository.restoreKeysWithPasskey()
            XCTFail("a passkey of another account must be refused")
        } catch {
            XCTAssertTrue(error is PasskeyWrongAccount, "\(error)")
        }
        XCTAssertEqual(h.passkey.askedFor, [base64UrlEncodeNoPad(account.bytes)])

        // This account's passkey, carrying no keys.
        h.passkey.result = assertion(account.bytes)
        let outcome = try await h.repository.restoreKeysWithPasskey()
        XCTAssertEqual(outcome, .noKeys)
        XCTAssertEqual(h.repository.restoreNotice, .passkeyNoKeys)
        XCTAssertTrue(isNeedsKeys(h.repository))
    }

    func testThePasskeyRowNeedsAParkedSignIn() async {
        let h = harness()
        do {
            _ = try await h.repository.restoreKeysWithPasskey()
            XCTFail("nothing is parked")
        } catch {
            XCTAssertTrue(error is RestoreNothingPending, "\(error)")
        }
        XCTAssertTrue(h.passkey.askedFor.isEmpty)
    }

    // MARK: - Three words

    /// A device that holds the keys and the phrase of `@alice`.
    private func deviceWithKeysAndPhrase(
        username: String = "alice",
        routes: @escaping (String, [String: Any]) -> (Int, String)? = { _, _ in nil }
    ) throws -> (Harness, RecoveryMnemonic, String) {
        let account = newAccount()
        let mnemonic = RecoveryMnemonic.generate()
        try AccountIdentitySeedStore.putAccountIdentitySeed(accountIdB64: account.b64, seed: mnemonic.toSeed())
        try PhraseStore().putPhrase(accountIdB64: account.b64, phraseBytes: Data(mnemonic.phrase().utf8))
        // ADR-0075: the account's phrase was saved (the three-word surfaces
        // exist only then).
        AccountProtectionStore().setPhraseSaved(true, accountIdB64: account.b64)
        let h = harness(marker: PersistedSession(accountIdB64: account.b64, username: username), routes: routes)
        return (h, mnemonic, account.b64)
    }

    func testTheThreeWordRouteIsOfferedOnlyWhereThePhraseTheKeysAndAUsernameAre() throws {
        let (h, _, accountIdB64) = try deviceWithKeysAndPhrase()
        XCTAssertEqual(h.repository.threeWordsAccount()?.username, "alice")

        try PhraseStore().deletePhrase(accountIdB64: accountIdB64)
        XCTAssertNil(h.repository.threeWordsAccount(), "no phrase on the device")

        let (guest, _, _) = try deviceWithKeysAndPhrase(username: "")
        XCTAssertNil(guest.repository.threeWordsAccount(), "a guest has no username to sign in by")

        let (noKeys, _, noKeysId) = try deviceWithKeysAndPhrase()
        AccountIdentitySeedStore.deleteAccountIdentitySeed(accountIdB64: noKeysId)
        XCTAssertNil(noKeys.repository.threeWordsAccount(), "no keys on the device")

        XCTAssertNil(harness().repository.threeWordsAccount(), "no cached account")

        // ADR-0075: the device holds the words of an account whose phrase was
        // never saved, but the person never saw them: no row.
        let (unsaved, _, unsavedId) = try deviceWithKeysAndPhrase()
        AccountProtectionStore().setPhraseSaved(false, accountIdB64: unsavedId)
        XCTAssertNil(unsaved.repository.threeWordsAccount(), "no saved phrase")
    }

    func testWrongWordsAreAMissAndNothingIsSentAnywhere() async throws {
        let (h, mnemonic, _) = try deviceWithKeysAndPhrase()
        let words = mnemonic.words()
        let matched = try await h.repository.recoverWithThreeWords(
            positions: Data([2, 9, 5]), words: [words[1], words[8], "zoo" == words[4] ? "zebra" : "zoo"])
        XCTAssertFalse(matched)
        XCTAssertTrue(NameRouteStub.requests(h.host).isEmpty, "the check is local")
        XCTAssertTrue(h.directory.events.isEmpty)
        if case .anon = h.repository.state {} else { XCTFail("still signed out") }
    }

    func testMatchingWordsSignInWithTheRecoveryLoginNamedByTheCachedUsernamesHandle() async throws {
        let (h, mnemonic, _) = try deviceWithKeysAndPhrase(routes: { path, _ in
            // A scripted server cannot answer the exchange: the finish
            // fails on this side, which the client reports as the `401` a
            // server gives for a credential that is not the account's.
            path == "/recovery/login/start" ? (200, #"{"session_id":"s","response":"AAAA"}"#) : nil
        })
        let words = mnemonic.words()
        do {
            _ = try await h.repository.recoverWithThreeWords(
                positions: Data([12, 1, 6]), words: [words[11], words[0], words[5]])
            XCTFail("a scripted server cannot complete the sign-in")
        } catch {
            // The refused credential, with nothing owed here, suspends the
            // route (see the tests below).
            XCTAssertEqual(error as? ThreeWordsError, .suspended, "\(error)")
            XCTAssertEqual(AuthErrorCopy.threeWords(error), Localization.text("recovery.words.error.rejected"))
        }
        let requests = NameRouteStub.requests(h.host)
        XCTAssertEqual(requests.map(\.path), ["/recovery/login/start"], "the recovery login, not the password reset")
        XCTAssertEqual(
            requests[0].json["handle"] as? String,
            base64UrlEncodeNoPad(try loginHandle(username: "alice")))
        XCTAssertNil(requests[0].authorization)
    }

    // MARK: - A refused sign-in after a match

    /// A scripted server cannot answer the OPAQUE exchange, so the finish
    /// fails on this side: the `401` of a credential that is not the
    /// account's.
    private let refusedCredential: (String, [String: Any]) -> (Int, String)? = { path, _ in
        path == "/recovery/login/start" ? (200, #"{"session_id":"s","response":"AAAA"}"#) : nil
    }

    func testARefusedCredentialWithNothingOwedSuspendsTheRouteClearsTheBudgetAndKeepsThePhrase() async throws {
        let (h, mnemonic, accountIdB64) = try deviceWithKeysAndPhrase(routes: refusedCredential)
        ThreeWordsBudgetStore().save(ThreeWords.Budget(misses: 3, pausedUntilMs: nil), accountIdB64: accountIdB64)
        defer { ThreeWordsBudgetStore().clearAll(accountIdB64: accountIdB64) }
        let words = mnemonic.words()
        do {
            _ = try await h.repository.recoverWithThreeWords(
                positions: Data([4, 10, 2]), words: [words[3], words[9], words[1]])
            XCTFail("the credential is refused")
        } catch {
            XCTAssertEqual(error as? ThreeWordsError, .suspended, "\(error)")
        }
        XCTAssertTrue(ThreeWordsBudgetStore().isSuspended(accountIdB64: accountIdB64))
        XCTAssertEqual(ThreeWordsBudgetStore().load(accountIdB64: accountIdB64), .fresh)
        XCTAssertNil(h.repository.threeWordsAccount(), "the row disappears")
        // Nothing is deleted: the phrase, the keys and the cached account stay.
        XCTAssertEqual(DevicePhrase.read(accountIdB64: accountIdB64), mnemonic.phrase())
        XCTAssertEqual(
            try AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: accountIdB64), mnemonic.toSeed())
        XCTAssertEqual(h.store.load()?.accountIdB64, accountIdB64)
        if case .anon = h.repository.state {} else { XCTFail("still signed out") }

        // A landing path that writes the phrase again brings the row back.
        DevicePhrase.store(mnemonic.phrase(), accountIdB64: accountIdB64, context: "test")
        XCTAssertFalse(ThreeWordsBudgetStore().isSuspended(accountIdB64: accountIdB64))
        XCTAssertNotNil(h.repository.threeWordsAccount())
    }

    func testALimitAnOutageOrAnotherFailureNeverSuspendsTheRoute() async throws {
        let answers: [(Int, String)] = [
            (429, #"{"error":"too-many-attempts","retry_after_secs":120}"#),
            (503, #"{"error":"rate-limiter-unavailable"}"#),
            (500, ""),
            (404, ""),
        ]
        for answer in answers {
            let (h, mnemonic, accountIdB64) = try deviceWithKeysAndPhrase(routes: { path, _ in
                path == "/recovery/login/start" ? answer : nil
            })
            let words = mnemonic.words()
            do {
                _ = try await h.repository.recoverWithThreeWords(
                    positions: Data([4, 10, 2]), words: [words[3], words[9], words[1]])
                XCTFail("status \(answer.0)")
            } catch {
                XCTAssertNil(error as? ThreeWordsError, "status \(answer.0): \(error)")
            }
            XCTAssertEqual(
                DevicePhrase.read(accountIdB64: accountIdB64), mnemonic.phrase(), "status \(answer.0)")
            XCTAssertFalse(ThreeWordsBudgetStore().isSuspended(accountIdB64: accountIdB64), "status \(answer.0)")
            XCTAssertNotNil(h.repository.threeWordsAccount(), "status \(answer.0)")
        }
    }

    func testTheFormGivesWayToOneSentenceWhenTheRouteIsSuspended() async throws {
        let (h, mnemonic, accountIdB64) = try deviceWithKeysAndPhrase(routes: refusedCredential)
        defer { ThreeWordsBudgetStore().clearAll(accountIdB64: accountIdB64) }
        let (model, budgets) = wordsModel(h, clock: { 1_700_000_000_000 }, draws: [[5, 1, 9]])
        budgets.save(ThreeWords.Budget(misses: 2, pausedUntilMs: nil), accountIdB64: accountIdB64)
        model.mode = .words
        let words = mnemonic.words()
        model.wordsEntries = model.wordsPositions.map { words[$0 - 1] }
        model.submitWords()
        let settled = await waitUntil { !model.busy && model.wordsRetired }
        XCTAssertTrue(settled)
        XCTAssertEqual(model.error, Localization.text("recovery.words.error.rejected"))
        XCTAssertEqual(model.signInMethods, [.totp], "the row is gone")
        XCTAssertEqual(model.accessOnlyNoteKey, "recovery.access_only_note")
        XCTAssertEqual(budgets.load(accountIdB64: accountIdB64), .fresh)
        XCTAssertEqual(DevicePhrase.read(accountIdB64: accountIdB64), mnemonic.phrase(), "the phrase is kept")
        model.leaveForm()
        XCTAssertFalse(model.wordsRetired)
    }

    // MARK: - ADR-0076: the handle sync, the own-username register, the rename

    /// A signed-in named account that holds its keys, with scripted handle routes.
    private func named(
        _ username: String = "alice",
        check: String = "same",
        put: @escaping ([String: Any]) -> (Int, String) = { _ in (200, #"{"bound":true}"#) }
    ) throws -> (Harness, String, Data) {
        let account = newAccount()
        let persistence = ScriptedPersistence()
        persistence.deviceKey = true
        persistence.seed = RecoveryMnemonic.generate().toSeed()
        try persistence.putSessionToken(
            PersistedSessionToken(pair: .fixture(access: "tok-named")), accountIdB64: account.b64)
        let h = harness(
            marker: PersistedSession(accountIdB64: account.b64, username: username),
            persistence: persistence,
            routes: { path, json in
                switch path {
                case "/auth/login-handle/check": return (200, #"{"state":"\#(check)"}"#)
                case "/auth/login-handle": return put(json)
                default: return nil
                }
            })
        XCTAssertTrue(isSession(h.repository))
        return (h, account.b64, account.bytes)
    }

    private func handleRequests(_ h: Harness) -> [RecordedAuthRequest] {
        NameRouteStub.requests(h.host).filter { $0.path == "/auth/login-handle" }
    }

    func testTheSessionChecksTheHandleAndBindsOnlyAFirstOne() async throws {
        // The bound handle is this name's: nothing is sent.
        let (same, sameId, _) = try named(check: "same")
        let asked = await waitUntil {
            NameRouteStub.requests(same.host).contains { $0.path == "/auth/login-handle/check" }
        }
        XCTAssertTrue(asked)
        XCTAssertEqual(handleRequests(same).count, 0)

        // The account has no handle: a first bind, with no grant.
        let (none, noneId, _) = try named(check: "none")
        let bound = await waitUntil { self.handleRequests(none).count == 1 }
        XCTAssertTrue(bound)
        XCTAssertEqual(handleRequests(none).first?.method, "PUT")
        XCTAssertNil(handleRequests(none).first?.json["fresh_grant"])

        // Renamed elsewhere: this device binds NOTHING (it would revert it).
        let (other, otherId, _) = try named(check: "other")
        let checked = await waitUntil {
            NameRouteStub.requests(other.host).contains { $0.path == "/auth/login-handle/check" }
        }
        XCTAssertTrue(checked)
        try? await Task.sleep(nanoseconds: 100_000_000)
        XCTAssertEqual(handleRequests(other).count, 0, "a stale name never re-binds")
        for id in [sameId, noneId, otherId] { AccountProtectionStore().clear(accountIdB64: id) }
    }

    func testTheRegisterSeedsOnceAndANewerNameUpdatesTheMarker() async throws {
        let (h, accountIdB64, _) = try named("alice", check: "same")
        var written: [String] = []
        h.repository.storeOwnUsername = { name in
            written.append(name)
            return true
        }
        var renamed: [(SessionAccount, Data?)] = []
        h.repository.onUsernameRenamed = { renamed.append(($0, $1)) }

        // An account from before the register: seeded once from the marker,
        // after auth confirmed the name's handle in this session.
        await h.repository.ownUsernameRegisterChanged(nil)
        for _ in 0..<200 where written.isEmpty { try? await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertEqual(written, ["alice"])
        await h.repository.ownUsernameRegisterChanged("alice")
        XCTAssertEqual(written, ["alice"], "the same name is nothing")
        XCTAssertTrue(renamed.isEmpty)

        // A newer name written on another device: the marker, the session
        // and what derives from the name follow.
        await h.repository.ownUsernameRegisterChanged("alice_b")
        XCTAssertEqual(h.store.load(), PersistedSession(accountIdB64: accountIdB64, username: "alice_b"))
        XCTAssertEqual(h.repository.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "alice_b")))
        XCTAssertEqual(renamed.count, 1)
        XCTAssertEqual(renamed.first?.0.username, "alice_b")
        XCTAssertNil(renamed.first?.1, "the certificate is fetched again")
        // The handle state is known for this session: no check, no bind for the new name.
        try? await Task.sleep(nanoseconds: 100_000_000)
        XCTAssertEqual(handleRequests(h).count, 0)
        XCTAssertEqual(
            NameRouteStub.requests(h.host).filter { $0.path == "/auth/login-handle/check" }.count, 1,
            "only the landing's check for the old name")
        AccountProtectionStore().clear(accountIdB64: accountIdB64)
    }

    /// The account was renamed elsewhere and this device has not heard yet:
    /// auth answers `other`, and the empty register is never seeded with
    /// the old name.
    func testAStaleDeviceDoesNotSeedTheRegister() async throws {
        let (h, accountIdB64, _) = try named("alice", check: "other")
        var written: [String] = []
        h.repository.storeOwnUsername = { name in
            written.append(name)
            return true
        }
        await h.repository.ownUsernameRegisterChanged(nil)
        for _ in 0..<200 where NameRouteStub.requests(h.host).filter({ $0.path == "/auth/login-handle/check" }).isEmpty {
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        try? await Task.sleep(nanoseconds: 100_000_000)
        await h.repository.ownUsernameRegisterChanged(nil)
        XCTAssertEqual(written, [], "a stale name is never written")
        XCTAssertEqual(handleRequests(h).count, 0)
        // The register arrives with the new name: adopted.
        await h.repository.ownUsernameRegisterChanged("alice_b")
        XCTAssertEqual(h.store.load()?.username, "alice_b")
        XCTAssertEqual(written, [])
        AccountProtectionStore().clear(accountIdB64: accountIdB64)
    }

    func testARenameMovesTheHandleThenDiscoveryThenTheMarkerAndTheRegister() async throws {
        let (h, accountIdB64, accountBytes) = try named("alice")
        h.reset.loginAccount = accountBytes
        var written: [String] = []
        var registerLoaded = false
        h.repository.storeOwnUsername = { name in
            guard registerLoaded else { return false }
            written.append(name)
            return true
        }
        var renamed: [(SessionAccount, Data?)] = []
        h.repository.onUsernameRenamed = { renamed.append(($0, $1)) }

        // Without a live proof nothing is sent.
        do {
            _ = try await h.repository.renameUsername(to: "alice_b")
            XCTFail("a rename needs the proof")
        } catch {
            XCTAssertTrue(error is FreshAuthRequired, "\(error)")
        }
        XCTAssertEqual(handleRequests(h).count, 0)

        try await h.repository.proveWithPassword(password: Data("correct horse".utf8))
        let new = try await h.repository.renameUsername(to: " @Alice_B ")

        XCTAssertEqual(new, "alice_b")
        // b. The handle, with the grant and the previous handle; no
        //    recovery email (the account has none).
        let put = try XCTUnwrap(handleRequests(h).first)
        XCTAssertEqual(put.method, "PUT")
        XCTAssertEqual(put.json["handle"] as? String, base64UrlEncodeNoPad(try loginHandle(username: "alice_b")))
        XCTAssertEqual(
            put.json["previous_handle"] as? String, base64UrlEncodeNoPad(try loginHandle(username: "alice")))
        XCTAssertEqual(put.json["fresh_grant"] as? String, "grant-1")
        XCTAssertNil(put.json["recovery_email"])
        // c. Discovery, after the handle.
        XCTAssertTrue(h.directory.events.contains("replace:alice_b"))
        // d. The marker, the session, the register, the stamp; the record is gone.
        XCTAssertEqual(h.store.load()?.username, "alice_b")
        XCTAssertEqual(h.repository.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "alice_b")))
        XCTAssertEqual(written, [], "the synced state was not loaded: the register is owed")
        XCTAssertNil(AccountProtectionStore().renameOwed(accountIdB64: accountIdB64))
        XCTAssertNotNil(h.repository.renameTooSoonUntilMs, "the next change is 30 days away")
        // e. The re-publish gets the certificate the replace answered.
        XCTAssertEqual(renamed.first?.0.username, "alice_b")
        XCTAssertEqual(renamed.first?.1, Data(repeating: 0xCE, count: 153))
        // The register still holds the OLD name (this device's write is
        // owed): it is not adopted back, and the owed write is made.
        await h.repository.ownUsernameRegisterChanged("alice")
        XCTAssertEqual(h.store.load()?.username, "alice_b")
        registerLoaded = true
        await h.repository.ownUsernameRegisterChanged("alice")
        XCTAssertEqual(written, ["alice_b"])
        XCTAssertEqual(h.store.load()?.username, "alice_b")
        await h.repository.ownUsernameRegisterChanged("alice_b")
        XCTAssertNil(AccountProtectionStore().usernameRegisterOwed(accountIdB64: accountIdB64))

        // Too soon by the device's own stamp: nothing is sent.
        let before = handleRequests(h).count
        do {
            _ = try await h.repository.renameUsername(to: "alice_c")
            XCTFail("one rename per 30 days")
        } catch let refused as UsernameRenameFailed {
            if case .tooSoon = refused.failure {} else { XCTFail("\(refused.failure)") }
        }
        XCTAssertEqual(handleRequests(h).count, before)
        AccountProtectionStore().clear(accountIdB64: accountIdB64)
    }

    func testATakenNameAtDiscoveryBindsTheOldHandleBack() async throws {
        let (h, accountIdB64, accountBytes) = try named("alice")
        h.reset.loginAccount = accountBytes
        h.directory.replaceError = DiscoveryClientError.http(
            status: 409, endpoint: "/discovery/username/replace", body: #"{"error":"username-taken"}"#)
        try await h.repository.proveWithPassword(password: Data("correct horse".utf8))

        do {
            _ = try await h.repository.renameUsername(to: "bob")
            XCTFail("a taken name is not a rename")
        } catch let refused as UsernameRenameFailed {
            XCTAssertEqual(refused.failure, .taken)
        }
        // The new handle, then the old one back.
        let puts = handleRequests(h)
        XCTAssertEqual(puts.count, 2)
        XCTAssertEqual(puts[1].json["handle"] as? String, base64UrlEncodeNoPad(try loginHandle(username: "alice")))
        XCTAssertEqual(
            puts[1].json["previous_handle"] as? String, base64UrlEncodeNoPad(try loginHandle(username: "bob")))
        // Nothing local moved, and nothing is owed.
        XCTAssertEqual(h.store.load()?.username, "alice")
        XCTAssertEqual(h.repository.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "alice")))
        XCTAssertNil(AccountProtectionStore().renameOwed(accountIdB64: accountIdB64))
        XCTAssertNil(h.repository.renameTooSoonUntilMs)

        // The server's "too soon" names the date.
        h.directory.replaceError = DiscoveryClientError.http(
            status: 429, endpoint: "/discovery/username/replace",
            body: #"{"error":"rename-too-soon","retry_after_secs":86400}"#)
        do {
            _ = try await h.repository.renameUsername(to: "bob")
            XCTFail("too soon")
        } catch let refused as UsernameRenameFailed {
            XCTAssertEqual(refused.failure, .tooSoon(untilMs: h.clock.nowMs + 86_400_000))
        }
        AccountProtectionStore().clear(accountIdB64: accountIdB64)
    }

    func testARenameTheHandleRefusesChangesNothing() async throws {
        let (h, accountIdB64, accountBytes) = try named("alice", put: { _ in (409, #"{"error":"handle-taken"}"#) })
        h.reset.loginAccount = accountBytes
        try await h.repository.proveWithPassword(password: Data("correct horse".utf8))
        do {
            _ = try await h.repository.renameUsername(to: "bob")
            XCTFail("taken at auth")
        } catch let refused as UsernameRenameFailed {
            XCTAssertEqual(refused.failure, .taken)
        }
        XCTAssertFalse(h.directory.events.contains("replace:bob"), "discovery is not asked")
        XCTAssertNil(AccountProtectionStore().renameOwed(accountIdB64: accountIdB64))
        XCTAssertEqual(h.store.load()?.username, "alice")
    }

    func testARenameInterruptedAfterTheHandleIsFinishedAtTheNextLanding() async throws {
        // The app closed between the handle and discovery: the record says so.
        let (h, accountIdB64, _) = try named("alice", check: "other")
        var written: [String] = []
        h.repository.storeOwnUsername = { name in
            written.append(name)
            return true
        }
        AccountProtectionStore().setRenameOwed(
            UsernameRename.Record(oldName: "alice", newName: "alice_b", phase: .bound, startedAtMs: 1),
            accountIdB64: accountIdB64)

        await h.repository.resumeOwedRename()

        XCTAssertTrue(h.directory.events.contains("replace:alice_b"))
        XCTAssertEqual(h.store.load()?.username, "alice_b")
        XCTAssertEqual(written, ["alice_b"])
        XCTAssertNil(AccountProtectionStore().renameOwed(accountIdB64: accountIdB64))
        XCTAssertEqual(handleRequests(h).count, 0, "no bind: the handle was already the new one")
        AccountProtectionStore().clear(accountIdB64: accountIdB64)
    }

    // MARK: - Show recovery phrase while a replace is owed

    func testThePhraseIsWithheldInSettingsUntilTheOwedPublishSucceeds() async throws {
        let account = newAccount()
        let mnemonic = RecoveryMnemonic.generate()
        let newKey = try Identity.fromSeed(seed: mnemonic.toSeed()).signaturePublicKey()
        try PhraseStore().putPhrase(accountIdB64: account.b64, phraseBytes: Data(mnemonic.phrase().utf8))
        // A phrase-protected account (ADR-0075): only it has words to show.
        AccountProtectionStore().setPhraseSaved(true, accountIdB64: account.b64)
        let persistence = ScriptedPersistence()
        persistence.deviceKey = true
        persistence.seed = mnemonic.toSeed()
        try persistence.putSessionToken(
            PersistedSessionToken(pair: .fixture(access: "tok-kept", generation: 2)), accountIdB64: account.b64)
        let markers = RecoveryMarkers(defaults: defaults("recovery-flow-markers"))
        var owed = KeyResetPublishRecord(
            attestation: Data(repeating: 0x5C, count: KeyResetResult.attestationLength),
            keyGeneration: 2, signedAtMs: 1_000, newSignaturePubkey: newKey)
        owed.discoveryDone = true
        owed.keyPackageDone = true
        markers.markPublishOwed(owed, accountIdB64: account.b64)

        let h = harness(
            marker: PersistedSession(accountIdB64: account.b64, username: "alice"),
            persistence: persistence, markers: markers)
        // The background retry of the landing fails, and so does `Try again`.
        h.reset.deviceGroupError = DsStatementError.keyResetDisabled
        XCTAssertTrue(isSession(h.repository))
        let tried = await waitUntil { h.reset.deviceGroupCalls.count == 1 }
        XCTAssertTrue(tried)

        XCTAssertTrue(h.repository.sessionPublishOwed)
        XCTAssertNil(h.repository.devicePhrase(), "withheld while the publish is owed")
        do {
            try await h.repository.retrySessionKeyResetPublish()
            XCTFail("the DS still refuses")
        } catch {}
        XCTAssertTrue(h.repository.sessionPublishOwed)
        XCTAssertNil(h.repository.devicePhrase())

        h.reset.deviceGroupError = nil
        try await h.repository.retrySessionKeyResetPublish()
        XCTAssertFalse(h.repository.sessionPublishOwed)
        XCTAssertEqual(h.reset.deviceGroupCalls.count, 3)
        XCTAssertEqual(h.reset.resetCalls.count, 0, "never resets")
        XCTAssertEqual(h.repository.devicePhrase(), mnemonic.phrase(), "shown once nothing is owed")
    }

    func testTheRouteRefusesOnADeviceWhereItIsNotOffered() async {
        let h = harness()
        do {
            _ = try await h.repository.recoverWithThreeWords(positions: Data([1, 2, 3]), words: ["a", "b", "c"])
            XCTFail("no cached account")
        } catch {
            XCTAssertEqual(error as? ThreeWordsError, .unavailable)
        }
    }

    // MARK: - The three-word form's budget

    private func wordsModel(
        _ h: Harness, clock: @escaping () -> Int64, draws: [[Int]]
    ) -> (RecoverViewModel, ThreeWordsBudgetStore) {
        let budgets = ThreeWordsBudgetStore(defaults: defaults("recovery-flow-budget"))
        var remaining = draws
        let model = RecoverViewModel(
            repository: h.repository, cachedUsername: "alice", budgets: budgets, nowMs: clock,
            drawPositions: { remaining.isEmpty ? [1, 2, 3] : remaining.removeFirst() })
        return (model, budgets)
    }

    /// Three list words that are wrong at `positions`.
    private func wrongWords(_ mnemonic: RecoveryMnemonic, at positions: [Int]) -> [String] {
        let words = mnemonic.words()
        return positions.map { words[$0 - 1] == "zoo" ? "zebra" : "zoo" }
    }

    func testFiveMissesPauseTheFormEachMissDrawsNewPositionsAndTheBudgetSurvivesANewForm() async throws {
        let (h, mnemonic, accountIdB64) = try deviceWithKeysAndPhrase()
        var now: Int64 = 1_700_000_000_000
        let draws = [[7, 3, 12], [1, 2, 3], [4, 5, 6], [9, 8, 7], [10, 11, 12], [2, 4, 6]]
        let (model, budgets) = wordsModel(h, clock: { now }, draws: draws)
        XCTAssertEqual(model.signInMethods, [.words, .totp])
        XCTAssertEqual(model.accessOnlyNoteKey, "recovery.access_only_note_words")

        model.mode = .words
        XCTAssertEqual(model.wordsPositions, [7, 3, 12], "in the order drawn")
        XCTAssertFalse(model.wordsComplete)
        XCTAssertNil(model.wordsTriesLeft)

        // A typo is not a guess: the form does not submit it.
        model.wordsEntries = ["zoo", "zoo", "zooo"]
        XCTAssertFalse(model.wordsComplete)
        model.submitWords()
        XCTAssertFalse(model.busy)
        XCTAssertEqual(budgets.load(accountIdB64: accountIdB64), .fresh)

        for miss in 1...5 {
            model.wordsEntries = wrongWords(mnemonic, at: model.wordsPositions)
            XCTAssertTrue(model.wordsComplete)
            model.submitWords()
            let settled = await waitUntil { !model.busy && model.wordsBudget.misses == miss }
            XCTAssertTrue(settled, "miss \(miss)")
            XCTAssertEqual(model.error, Localization.text("recovery.words.error.no_match"))
            XCTAssertEqual(model.wordsPositions, draws[miss], "new positions after every miss")
            XCTAssertEqual(model.wordsEntries, ["", "", ""])
            XCTAssertEqual(budgets.load(accountIdB64: accountIdB64).misses, miss)
            if miss < 5 { XCTAssertEqual(model.wordsTriesLeft, 5 - miss) }
        }
        XCTAssertTrue(model.wordsPause.waiting)
        XCTAssertEqual(model.wordsPause.remainingMs, ThreeWords.pauseMs)
        XCTAssertNil(model.wordsTriesLeft)
        XCTAssertTrue(NameRouteStub.requests(h.host).isEmpty, "five misses, nothing sent")

        // Paused: a submit does nothing, even with the right words.
        let words = mnemonic.words()
        model.wordsEntries = model.wordsPositions.map { words[$0 - 1] }
        model.submitWords()
        XCTAssertFalse(model.busy)
        XCTAssertTrue(NameRouteStub.requests(h.host).isEmpty)

        // A new form (a relaunch) still finds the pause.
        now += 60_000
        let reopened = RecoverViewModel(
            repository: h.repository, cachedUsername: "alice", budgets: budgets, nowMs: { now },
            drawPositions: { [1, 2, 3] })
        reopened.mode = .words
        XCTAssertTrue(reopened.wordsPause.waiting)
        XCTAssertEqual(reopened.wordsPause.remainingMs, ThreeWords.pauseMs - 60_000)

        // After the pause the count starts again.
        now += ThreeWords.pauseMs
        let later = RecoverViewModel(
            repository: h.repository, cachedUsername: "alice", budgets: budgets, nowMs: { now },
            drawPositions: { [1, 2, 3] })
        later.mode = .words
        XCTAssertFalse(later.wordsPause.waiting)
        XCTAssertEqual(later.wordsBudget, .fresh)
    }

    func testAFailureAfterAMatchIsNotChargedToTheBudget() async throws {
        let (h, mnemonic, accountIdB64) = try deviceWithKeysAndPhrase(routes: { path, _ in
            path == "/recovery/login/start"
                ? (429, #"{"error":"too-many-attempts","retry_after_secs":120}"#) : nil
        })
        let now: Int64 = 1_700_000_000_000
        let (model, budgets) = wordsModel(h, clock: { now }, draws: [[5, 1, 9]])
        model.mode = .words
        let words = mnemonic.words()
        model.wordsEntries = model.wordsPositions.map { words[$0 - 1] }
        model.submitWords()
        let settled = await waitUntil { !model.busy && model.serverWait.waiting }
        XCTAssertTrue(settled, "the server's limit is counted down")
        XCTAssertEqual(model.serverWait.remainingMs, 120_000)
        XCTAssertEqual(budgets.load(accountIdB64: accountIdB64), .fresh, "the words were right")
        XCTAssertEqual(model.wordsPositions, [5, 1, 9], "no new draw: this was not a miss")
        XCTAssertFalse(model.wordsPause.waiting)
    }

    func testOnADeviceWithoutThePhraseTheListHasNoThreeWordRow() {
        let h = harness(marker: PersistedSession(accountIdB64: base64Encode(randomBytes(16)), username: "alice"))
        let model = RecoverViewModel(repository: h.repository, cachedUsername: "alice")
        XCTAssertEqual(model.keysMethods, [.anotherDevice, .passkey, .phrase])
        XCTAssertEqual(model.signInMethods, [.totp])
        XCTAssertEqual(model.accessOnlyNoteKey, "recovery.access_only_note")
    }

    // MARK: - Choose a new password

    func testAGuestOrADeviceWithoutASessionCannotReplaceThePassword() async {
        let h = harness()
        XCTAssertFalse(h.repository.canReplacePassword)
        do {
            try await h.repository.replacePassword(newPassword: Data("new-password".utf8))
            XCTFail("no session")
        } catch {
            XCTAssertTrue(error is SessionNotReady, "\(error)")
        }
        XCTAssertTrue(NameRouteStub.requests(h.host).isEmpty)
    }

    func testThePasswordIsReplacedThroughTheResetNamedByTheSessionsUsernameAndNothingLocalIsWiped() async throws {
        let account = newAccount()
        let h = harness(routes: { path, json in
            switch path {
            case "/totp/recover":
                return (200, """
                {"account_id_b64":"\(base64UrlEncodeNoPad(account.bytes))","session_token":"tok-access","session_expires_at_ms":9000000,"refresh_token":"r1.t","refresh_expires_at_ms":9000000000,"key_generation":1}
                """)
            case "/auth/login-handle":
                return (200, #"{"bound":true}"#)
            default:
                return nil
            }
        })
        let mine = RecoveryMnemonic.generate()
        try AccountIdentitySeedStore.putAccountIdentitySeed(accountIdB64: account.b64, seed: mine.toSeed())
        try PhraseStore().putPhrase(accountIdB64: account.b64, phraseBytes: Data(mine.phrase().utf8))
        AccountProtectionStore().setPhraseSaved(true, accountIdB64: account.b64)
        try await h.repository.recoverWithTotp(username: "alice", code: "123456")
        XCTAssertTrue(isSession(h.repository), "the device holds the keys: signed in")
        XCTAssertTrue(h.repository.newPasswordOffered)

        // Not on the bare session (an authenticator sign-in is no proof):
        // refused before anything is sent.
        XCTAssertFalse(h.repository.passwordChangeAllowed)
        do {
            try await h.repository.replacePassword(newPassword: Data("new-password".utf8))
            XCTFail("the bare session must not change the password")
        } catch {
            XCTAssertTrue(error is FreshAuthRequired, "\(error)")
        }
        XCTAssertNil(NameRouteStub.requests(h.host).first { $0.path == "/password/reset/start" })

        // The local three-word check opens the gate: wrong words do not,
        // the right ones do, and nothing is sent for either.
        XCTAssertTrue(h.repository.threeWordsCheckAvailable)
        let words = mine.words()
        let wrong = words[0] == "abandon" ? "ability" : "abandon"
        XCTAssertFalse(try h.repository.checkThreeWordsForPasswordChange(
            positions: Data([1, 2, 3]), words: [wrong, words[1], words[2]]))
        XCTAssertFalse(h.repository.passwordChangeAllowed)
        XCTAssertTrue(try h.repository.checkThreeWordsForPasswordChange(
            positions: Data([3, 1, 7]), words: [words[2], words[0], words[6]]))
        XCTAssertTrue(h.repository.passwordChangeAllowed)
        XCTAssertNil(NameRouteStub.requests(h.host).first { $0.path == "/password/reset/start" })

        // A scripted server cannot answer the reset exchange: the request
        // that names the account is what can be observed.
        do {
            try await h.repository.replacePassword(newPassword: Data("new-password".utf8))
            XCTFail("a scripted server cannot complete the reset")
        } catch {
            XCTAssertFalse(error is FreshAuthRequired, "\(error)")
        }
        let reset = try XCTUnwrap(NameRouteStub.requests(h.host).first { $0.path == "/password/reset/start" })
        XCTAssertEqual(reset.json["handle"] as? String, base64UrlEncodeNoPad(try loginHandle(username: "alice")))
        XCTAssertNotNil(reset.json["recovery_request"])
        XCTAssertNotNil(reset.json["new_password_request"])
        // A failed save leaves the session and everything on the device.
        XCTAssertTrue(isSession(h.repository))
        XCTAssertEqual(h.repository.identitySecrets?.sessionToken, "tok-access")
        XCTAssertEqual(DevicePhrase.read(accountIdB64: account.b64), mine.phrase())
        XCTAssertEqual(try AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: account.b64), mine.toSeed())
        XCTAssertTrue(h.repository.newPasswordOffered, "still offered after a failed save")

        // The three-word window is ten minutes.
        h.clock.nowMs += PasswordChangeGate.threeWordsWindowMs
        XCTAssertFalse(h.repository.passwordChangeAllowed)
    }

    // MARK: - The fresh-auth proof (ADR-0073, amended 2026-10-06; UX 17)

    /// A device that holds the keys, signed in with the authenticator code.
    private func sessionWithKeys() async throws -> (Harness, (bytes: Data, b64: String)) {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        let mine = RecoveryMnemonic.generate()
        try AccountIdentitySeedStore.putAccountIdentitySeed(accountIdB64: account.b64, seed: mine.toSeed())
        try await h.repository.recoverWithTotp(username: "alice", code: "123456")
        XCTAssertTrue(isSession(h.repository))
        return (h, account)
    }

    func testAnAuthenticatorSignInIsNoProofAndRecordsItsLanding() async throws {
        let (h, _) = try await sessionWithKeys()
        XCTAssertEqual(h.repository.sessionLanding, .authenticator)
        XCTAssertFalse(h.repository.holdsLiveFreshAuth())
        XCTAssertNil(h.repository.liveFreshGrant())
        XCTAssertTrue(h.repository.proofUsernameKnown)
    }

    func testAPasswordProofInASessionInstallsThePairWithoutALanding() async throws {
        let (h, account) = try await sessionWithKeys()
        h.reset.loginAccount = account.bytes
        h.reset.loginSession = .fixture(access: "tok-proof", refresh: "r1.proof")
        let seedBefore = h.repository.identitySecrets?.accountIdentitySeed
        // The sign-in's own (fire-and-forget) handle bind lands first.
        _ = await waitUntil { NameRouteStub.requests(h.host).contains { $0.path == "/auth/login-handle" } }
        let bindsBefore = NameRouteStub.requests(h.host).filter { $0.path == "/auth/login-handle" }.count
        let stateBefore = h.repository.state

        try await h.repository.proveWithPassword(password: Data("correct horse".utf8))

        XCTAssertTrue(h.repository.holdsLiveFreshAuth())
        XCTAssertEqual(h.repository.liveFreshGrant(), "grant-1")
        XCTAssertEqual(h.reset.loginCalls.first?.handle, try loginHandle(username: "alice"))
        XCTAssertEqual(h.repository.identitySecrets?.sessionToken, "tok-proof", "the same account's new pair")
        XCTAssertEqual(h.repository.identitySecrets?.accountIdentitySeed, seedBefore, "the keys are untouched")
        XCTAssertEqual(h.repository.state, stateBefore, "no landing")
        XCTAssertEqual(h.repository.sessionLanding, .authenticator, "how the session was landed does not change")
        XCTAssertTrue(h.wiped.accounts.isEmpty)
        try? await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(
            NameRouteStub.requests(h.host).filter { $0.path == "/auth/login-handle" }.count, bindsBefore,
            "no handle re-bind")
        XCTAssertEqual(h.persistence.sessionToken(accountIdB64: account.b64)?.sessionToken, "tok-proof")
    }

    func testAWrongPasswordProofInstallsNothing() async throws {
        let (h, account) = try await sessionWithKeys()
        h.reset.loginAccount = account.bytes
        h.reset.loginError = AuthClientError.http(status: 401, endpoint: "/login/finish", body: "")
        do {
            try await h.repository.proveWithPassword(password: Data("nope".utf8))
            XCTFail("a wrong password")
        } catch {
            XCTAssertTrue(error is FreshAuthWrongPassword, "\(error)")
        }
        XCTAssertFalse(h.repository.holdsLiveFreshAuth())
        XCTAssertEqual(h.repository.identitySecrets?.sessionToken, "tok-access")
    }

    func testAPasskeyProofInstallsThePairAndTheGrant() async throws {
        let (h, account) = try await sessionWithKeys()
        h.passkey.result = PasskeyLoginResult(
            accountIdB64: base64UrlEncodeNoPad(account.bytes),
            session: .fixture(access: "tok-passkey", refresh: "r1.passkey"),
            freshGrant: FreshAuthGrant(grant: "grant-passkey", expiresAtMs: 1_600_000))

        try await h.repository.proveWithPasskey()

        XCTAssertEqual(h.passkey.askedFor, [base64UrlEncodeNoPad(account.bytes)], "limited to this account")
        XCTAssertEqual(h.repository.liveFreshGrant(), "grant-passkey")
        XCTAssertEqual(h.repository.identitySecrets?.sessionToken, "tok-passkey")
        XCTAssertTrue(isSession(h.repository))
    }

    func testAPasskeyOfAnotherAccountIsRefusedAndInstallsNothing() async throws {
        let (h, _) = try await sessionWithKeys()
        h.passkey.result = PasskeyLoginResult(
            accountIdB64: base64UrlEncodeNoPad(randomBytes(16)),
            session: .fixture(access: "tok-other", refresh: "r1.other"),
            freshGrant: FreshAuthGrant(grant: "grant-other", expiresAtMs: 1_600_000))
        do {
            try await h.repository.proveWithPasskey()
            XCTFail("another account's passkey")
        } catch {
            XCTAssertTrue(error is FreshAuthPasskeyOtherAccount, "\(error)")
        }
        XCTAssertFalse(h.repository.holdsLiveFreshAuth())
        XCTAssertEqual(h.repository.identitySecrets?.sessionToken, "tok-access")
    }

    func testAPasskeySignInWithoutAGrantIsNotAProof() async throws {
        let (h, account) = try await sessionWithKeys()
        h.passkey.result = PasskeyLoginResult(
            accountIdB64: base64UrlEncodeNoPad(account.bytes),
            session: .fixture(access: "tok-passkey", refresh: "r1.passkey"))
        do {
            try await h.repository.proveWithPasskey()
            XCTFail("no grant")
        } catch {
            XCTAssertTrue(error is FreshAuthRequired, "\(error)")
        }
        XCTAssertFalse(h.repository.holdsLiveFreshAuth())
        XCTAssertEqual(h.repository.identitySecrets?.sessionToken, "tok-access", "nothing installed")
    }

    func testTheProofRunsOutAfterItsLifetimeAndIsDroppedOnSignOut() async throws {
        let (h, account) = try await sessionWithKeys()
        h.reset.loginAccount = account.bytes
        try await h.repository.proveWithPassword(password: Data("correct horse".utf8))
        XCTAssertTrue(h.repository.holdsLiveFreshAuth())
        XCTAssertTrue(h.repository.passwordChangeAllowed, "a proof opens the password change")
        h.clock.nowMs = 1_600_000
        XCTAssertFalse(h.repository.holdsLiveFreshAuth())
        XCTAssertNil(h.repository.liveFreshGrant())

        h.clock.nowMs = 1_000_000
        try await h.repository.proveWithPassword(password: Data("correct horse".utf8))
        h.repository.dropFreshAuth()
        XCTAssertFalse(h.repository.holdsLiveFreshAuth(), "a refused grant is dropped")
        try await h.repository.proveWithPassword(password: Data("correct horse".utf8))
        h.repository.logout()
        XCTAssertFalse(h.repository.holdsLiveFreshAuth())
        XCTAssertNil(h.repository.sessionLanding)
    }

    /// UX 17.2 — a passkey gesture on the warning screen qualifies for the
    /// reset; no password is asked.
    func testAPasskeyProofQualifiesForTheReset() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        h.passkey.result = PasskeyLoginResult(
            accountIdB64: base64UrlEncodeNoPad(account.bytes),
            session: .fixture(access: "tok-passkey", refresh: "r1.passkey"),
            freshGrant: FreshAuthGrant(grant: "grant-passkey", expiresAtMs: 1_600_000))

        try await h.repository.proveWithPasskey()
        XCTAssertTrue(h.repository.holdsLiveFreshAuthForPendingRestore())
        XCTAssertTrue(isNeedsKeys(h.repository), "still on the restore prompt")
        _ = try await h.repository.mintFreshIdentity()

        XCTAssertTrue(h.reset.loginCalls.isEmpty, "no password sign-in")
        XCTAssertEqual(h.reset.resetCalls.map(\.grant), ["grant-passkey"])
        XCTAssertEqual(h.reset.resetCalls.map(\.token), ["tok-passkey"])
        XCTAssertFalse(h.repository.holdsLiveFreshAuth(), "a reset spends every grant")
    }

    func testThePasskeyRowOfTheRestorePromptIsAProofToo() async throws {
        let account = newAccount()
        let h = harness(routes: totpRoute(account.bytes))
        try await parkWithoutKeys(h)
        h.passkey.result = PasskeyLoginResult(
            accountIdB64: base64UrlEncodeNoPad(account.bytes),
            session: .fixture(access: "tok-passkey", refresh: "r1.passkey"),
            freshGrant: FreshAuthGrant(grant: "grant-row", expiresAtMs: 1_600_000))
        _ = try? await h.repository.restoreKeysWithPasskey()
        XCTAssertTrue(h.repository.holdsLiveFreshAuthForPendingRestore())
    }
}

/// The new-password form's rule.
final class NewPasswordRuleTests: XCTestCase {
    func testTheTwoFieldsMustMatchAndBeLongEnough() {
        XCTAssertEqual(NewPasswordRule.refusal(password: "longenough", confirm: "different"), .mismatch)
        XCTAssertEqual(NewPasswordRule.refusal(password: "short", confirm: "short"), .tooShort)
        XCTAssertNil(NewPasswordRule.refusal(password: "longenough", confirm: "longenough"))
        XCTAssertEqual(NewPasswordRule.minLength, 8)
    }
}

/// The hand-over side of a device that holds no phrase.
final class DevicePhraseTests: XCTestCase {

    func testTheStoreKeepsTheCanonicalFormAndGivesTheEntropyTheHandOverCarries() throws {
        let accountIdB64 = base64Encode(randomBytes(16))
        defer { try? PhraseStore().deletePhrase(accountIdB64: accountIdB64) }
        XCTAssertNil(DevicePhrase.read(accountIdB64: accountIdB64))
        XCTAssertFalse(DevicePhrase.isStored(accountIdB64: accountIdB64))
        XCTAssertNil(DevicePhrase.entropy(accountIdB64: accountIdB64), "no phrase: no keys to hand over")

        let mnemonic = RecoveryMnemonic.generate()
        let typed = "  " + mnemonic.words().joined(separator: "   ") + "\n"
        DevicePhrase.store(typed, accountIdB64: accountIdB64, context: "test")
        XCTAssertEqual(DevicePhrase.read(accountIdB64: accountIdB64), mnemonic.phrase())
        XCTAssertEqual(DevicePhrase.entropy(accountIdB64: accountIdB64), mnemonic.toEntropy())
        XCTAssertEqual(DevicePhrase.entropy(accountIdB64: accountIdB64)?.count, 16)

        // Something that is not a phrase is never written over a good one.
        DevicePhrase.store("not a recovery phrase", accountIdB64: accountIdB64, context: "test")
        XCTAssertEqual(DevicePhrase.read(accountIdB64: accountIdB64), mnemonic.phrase())
    }
}
