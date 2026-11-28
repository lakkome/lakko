// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AuthenticationServices
import XCTest
import LakkoCore
@testable import Lakko

/// A passkey that restores keys: the pure rules, the core's seal / open run
/// for real, storing the keys for a passkey (after it is added, and the
/// "Restore keys with this passkey" repair).
final class PasskeyVaultTests: XCTestCase {

    private func bytes(_ count: Int) -> Data {
        Data((0..<count).map { _ in UInt8.random(in: 0...255) })
    }

    // MARK: - Constants

    func testThePrfSaltIsTheFixedOneEveryClientEvaluates() {
        XCTAssertEqual(PasskeyVault.prfSalt, Data("lakko/passkey-vault/prf/v1".utf8))
        XCTAssertEqual(PasskeyVault.prfOutputLength, 32)
    }

    func testOnlyA32ByteOutputIsUsable() {
        XCTAssertNil(PasskeyVault.usablePrfOutput(nil))
        XCTAssertNil(PasskeyVault.usablePrfOutput(Data()))
        XCTAssertNil(PasskeyVault.usablePrfOutput(Data(count: 31)))
        XCTAssertNil(PasskeyVault.usablePrfOutput(Data(count: 64)))
        let output = bytes(32)
        XCTAssertEqual(PasskeyVault.usablePrfOutput(output), output)
    }

    // MARK: - The keys outcome of a passkey sign-in on a device without keys

    func testNoPrfOutputOrNoBlobMeansThePasskeySignsInOnly() {
        XCTAssertEqual(
            PasskeyVault.keysOutcome(prfOutput: false, blobStored: false, opened: false, matched: false), .noKeys)
        XCTAssertEqual(
            PasskeyVault.keysOutcome(prfOutput: false, blobStored: true, opened: false, matched: false), .noKeys)
        XCTAssertEqual(
            PasskeyVault.keysOutcome(prfOutput: true, blobStored: false, opened: false, matched: false), .noKeys)
    }

    func testABlobThatDoesNotOpenOrIsNotTheAccountsIsUnusable() {
        XCTAssertEqual(
            PasskeyVault.keysOutcome(prfOutput: true, blobStored: true, opened: false, matched: false), .unusable)
        XCTAssertEqual(
            PasskeyVault.keysOutcome(prfOutput: true, blobStored: true, opened: true, matched: false), .unusable)
    }

    func testOnlyAnOpenedAndMatchedBlobRestores() {
        XCTAssertEqual(
            PasskeyVault.keysOutcome(prfOutput: true, blobStored: true, opened: true, matched: true), .restored)
    }

    func testTheNoticeKeysOfTheRestorePrompt() {
        XCTAssertEqual(RestoreNotice.passkeyNoKeys.catalogKey, "restore_identity.from_passkey_no_keys")
        XCTAssertEqual(RestoreNotice.passkeyUnusable.catalogKey, "restore_identity.from_passkey_unusable")
    }

    // MARK: - After adding a passkey

    func testStoringTheKeysFollowsOnlyWhenThePlatformCanProtectKeys() {
        XCTAssertTrue(PasskeyVault.canCarryKeys(prfOutput: true, prfSupported: nil))
        XCTAssertTrue(PasskeyVault.canCarryKeys(prfOutput: false, prfSupported: true))
        // Only an explicit "cannot" skips the second sheet.
        XCTAssertFalse(PasskeyVault.canCarryKeys(prfOutput: false, prfSupported: false))
        // Nothing said: the second sheet runs.
        XCTAssertTrue(PasskeyVault.canCarryKeys(prfOutput: false, prfSupported: nil))
    }

    func testASystemWithoutThePrfApiReportsThatItCannotCarryKeys() {
        XCTAssertEqual(PasskeyVault.reportedPrfSupport(registrationReported: nil, prfApiAvailable: false), false)
        XCTAssertEqual(PasskeyVault.reportedPrfSupport(registrationReported: true, prfApiAvailable: false), false)
        XCTAssertNil(PasskeyVault.reportedPrfSupport(registrationReported: nil, prfApiAvailable: true))
        XCTAssertEqual(PasskeyVault.reportedPrfSupport(registrationReported: false, prfApiAvailable: true), false)
        XCTAssertEqual(PasskeyVault.reportedPrfSupport(registrationReported: true, prfApiAvailable: true), true)
    }

    func testPasskeysFoundToCarryNoKeysAreRememberedPerAccount() throws {
        let suite = "lakko.tests.passkeys-no-prf.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = PasskeyNoPrfStore(defaults: defaults)
        store.insert("one", accountIdB64: "accountA")
        store.insert("two", accountIdB64: "accountA")
        store.insert("one", accountIdB64: "accountA")
        XCTAssertEqual(store.load(accountIdB64: "accountA"), ["one", "two"])
        XCTAssertEqual(PasskeyNoPrfStore(defaults: defaults).load(accountIdB64: "accountA"), ["one", "two"],
                       "a new store reads what was written: it survives a restart")
        XCTAssertTrue(store.load(accountIdB64: "accountB").isEmpty)
        store.clear(accountIdB64: "accountA")
        XCTAssertTrue(store.load(accountIdB64: "accountA").isEmpty)
    }

    func testARowRepairAnsweredByAnotherPasskeySaysChooseThisPasskey() {
        XCTAssertEqual(
            PasskeyKeysRepair.refusalKey("settings.passkeys.keys_choice.error.other_passkey"),
            "settings.passkeys.keys.error.other_passkey")
        XCTAssertEqual(
            PasskeyKeysRepair.refusalKey("settings.passkeys.keys_choice.error.phrase_mismatch"),
            "settings.passkeys.keys_choice.error.phrase_mismatch")
    }

    // MARK: - A refused registration

    func testAMatchedExcludedCredentialReadsAsAlreadyExisting() throws {
        guard #available(iOS 18.0, *) else { throw XCTSkip("the refusal has its own code from iOS 18") }
        let refused = WebauthnRegistration.registrationError(for: ASAuthorizationError(.matchedExcludedCredential))
        guard case .alreadyExists = refused else { return XCTFail("got \(refused)") }
    }

    func testOtherRegistrationFailuresKeepTheirMeaning() {
        let cancelled = WebauthnRegistration.registrationError(for: ASAuthorizationError(.canceled))
        guard case .cancelled = cancelled else { return XCTFail("got \(cancelled)") }
        let failed = WebauthnRegistration.registrationError(for: ASAuthorizationError(.failed))
        guard case .underlying = failed else { return XCTFail("got \(failed)") }
        let other = WebauthnRegistration.registrationError(for: URLError(.notConnectedToInternet))
        guard case .underlying = other else { return XCTFail("got \(other)") }
    }

    // MARK: - Settings labels and the restore prompt's row

    func testAPasskeyRestoresKeysExactlyWhenTheServerHoldsABlobForIt() {
        XCTAssertEqual(PasskeyVault.keysLabel(credentialIdB64: "a", withKeys: ["a", "b"]), .restoresKeys)
        XCTAssertEqual(PasskeyVault.keysLabel(credentialIdB64: "c", withKeys: ["a", "b"]), .signInOnly)
        XCTAssertEqual(PasskeyVault.keysLabel(credentialIdB64: "a", withKeys: []), .signInOnly)
        // Not loaded, or the server keeps no keys: sign-in only.
        XCTAssertEqual(PasskeyVault.keysLabel(credentialIdB64: "a", withKeys: nil), .signInOnly)
        XCTAssertEqual(PasskeyVault.KeysLabel.restoresKeys.catalogKey, "settings.passkeys.label.restores_keys")
        XCTAssertEqual(PasskeyVault.KeysLabel.signInOnly.catalogKey, "settings.passkeys.label.signin_only")
    }

    func testTheRestorePromptHidesThePasskeyRowWhenNoPasskeyCarriesKeys() {
        XCTAssertTrue(PasskeyVault.restoreOffersPasskey(passkeysSupported: true, passkeysWithKeys: nil))
        XCTAssertTrue(PasskeyVault.restoreOffersPasskey(passkeysSupported: true, passkeysWithKeys: 2))
        XCTAssertFalse(PasskeyVault.restoreOffersPasskey(passkeysSupported: true, passkeysWithKeys: 0))
        XCTAssertFalse(PasskeyVault.restoreOffersPasskey(passkeysSupported: false, passkeysWithKeys: 2))
    }

    // MARK: - Seal / open (real core)

    func testSealThenOpenUnderTheSameOutputGivesThePhraseBack() throws {
        let mnemonic = RecoveryMnemonic.generate()
        let prf = bytes(32)
        let sealed = try passkeyVaultSeal(mnemonic: mnemonic, prfOutput: prf)
        XCTAssertEqual(sealed.count, PasskeyVault.sealedEntropyLength)
        XCTAssertNil(sealed.range(of: mnemonic.toEntropy()), "the entropy is not in the blob in the clear")
        let opened = try passkeyVaultOpen(prfOutput: prf, sealed: sealed)
        XCTAssertEqual(opened.phrase(), mnemonic.phrase())
        XCTAssertEqual(opened.toSeed(), mnemonic.toSeed())
    }

    func testAnotherPasskeysOutputOrAChangedBlobDoesNotOpen() throws {
        let mnemonic = RecoveryMnemonic.generate()
        let sealed = try passkeyVaultSeal(mnemonic: mnemonic, prfOutput: bytes(32))
        XCTAssertThrowsError(try passkeyVaultOpen(prfOutput: bytes(32), sealed: sealed)) { error in
            guard case .PasskeyVaultNotOpened = error as? FfiError else {
                return XCTFail("expected PasskeyVaultNotOpened, got \(error)")
            }
        }
        let prf = bytes(32)
        var changed = try passkeyVaultSeal(mnemonic: mnemonic, prfOutput: prf)
        changed[changed.count - 1] ^= 0x01
        XCTAssertThrowsError(try passkeyVaultOpen(prfOutput: prf, sealed: changed))
    }

    // MARK: - Storing the keys for a passkey

    /// The enrollment over recorded ports.
    private final class Ports {
        var devicePhrase: String?
        var identitySeed: Data?
        var prf: (credentialId: Data, prfOutput: Data?)?
        var prfError: Error?
        var storeResult = true
        private(set) var prfAsked: [String] = []
        private(set) var stored: [(credentialId: Data, sealed: Data)] = []
        private(set) var remembered: [String] = []
        private(set) var noPrf: [String] = []

        var enrollment: PasskeyKeysEnrollment {
            PasskeyKeysEnrollment(
                devicePhrase: { self.devicePhrase },
                identitySeed: { self.identitySeed },
                evaluatePrf: { id in
                    self.prfAsked.append(id)
                    if let error = self.prfError { throw error }
                    let answer = self.prf ?? (Data(), nil)
                    return (answer.0, answer.1, nil)
                },
                store: { id, sealed in
                    self.stored.append((id, sealed))
                    return self.storeResult
                },
                rememberPhrase: { self.remembered.append($0) },
                rememberNoPrf: { self.noPrf.append($0) }
            )
        }
    }

    private let credentialId = Data([0xC0, 0xFF, 0xEE, 0x01, 0x02])
    private var credentialIdB64: String { base64UrlEncodeNoPad(credentialId) }

    func testWithThePhraseOnTheDeviceAndTheRegistrationsOutputTheBlobIsStored() async throws {
        let mnemonic = RecoveryMnemonic.generate()
        let ports = Ports()
        ports.devicePhrase = mnemonic.phrase()
        let prf = bytes(32)
        let step = try await ports.enrollment.turnOn(
            credentialIdB64: credentialIdB64, registrationPrfOutput: prf, typedPhrase: nil)
        XCTAssertEqual(step, .done(.stored))
        XCTAssertTrue(ports.prfAsked.isEmpty, "the registration already gave the secret: no second prompt")
        XCTAssertEqual(ports.stored.count, 1)
        XCTAssertEqual(ports.stored[0].credentialId, credentialId)
        // What was stored opens under that passkey's output into the phrase.
        XCTAssertEqual(
            try passkeyVaultOpen(prfOutput: prf, sealed: ports.stored[0].sealed).phrase(), mnemonic.phrase())
    }

    func testWithoutARegistrationOutputThePasskeyIsAskedOnceMore() async throws {
        let ports = Ports()
        ports.devicePhrase = RecoveryMnemonic.generate().phrase()
        ports.prf = (credentialId, bytes(32))
        let step = try await ports.enrollment.turnOn(
            credentialIdB64: credentialIdB64, registrationPrfOutput: nil, typedPhrase: nil)
        XCTAssertEqual(step, .done(.stored))
        XCTAssertEqual(ports.prfAsked, [credentialIdB64], "limited to the credential it is turned on for")
    }

    func testAnotherPasskeyAnsweringStoresNothing() async throws {
        let ports = Ports()
        ports.devicePhrase = RecoveryMnemonic.generate().phrase()
        ports.prf = (Data([0x09]), bytes(32))
        let step = try await ports.enrollment.turnOn(
            credentialIdB64: credentialIdB64, registrationPrfOutput: nil, typedPhrase: nil)
        XCTAssertEqual(step, .refused("settings.passkeys.keys_choice.error.other_passkey"))
        XCTAssertTrue(ports.stored.isEmpty)
    }

    func testAPasskeyThatGivesNoSecretSignsInOnly() async throws {
        let ports = Ports()
        ports.devicePhrase = RecoveryMnemonic.generate().phrase()
        ports.prf = (credentialId, nil)
        let step = try await ports.enrollment.turnOn(
            credentialIdB64: credentialIdB64, registrationPrfOutput: nil, typedPhrase: nil)
        XCTAssertEqual(step, .done(.noPrf))
        XCTAssertTrue(ports.stored.isEmpty)
    }

    func testAServerWithoutAVaultKeyIsUnavailable() async throws {
        let ports = Ports()
        ports.devicePhrase = RecoveryMnemonic.generate().phrase()
        ports.storeResult = false
        let step = try await ports.enrollment.turnOn(
            credentialIdB64: credentialIdB64, registrationPrfOutput: bytes(32), typedPhrase: nil)
        XCTAssertEqual(step, .done(.unavailable))
    }

    func testWithoutThePhraseOnTheDeviceThe12WordsAreAskedFor() async throws {
        let ports = Ports()
        let step = try await ports.enrollment.turnOn(
            credentialIdB64: credentialIdB64, registrationPrfOutput: bytes(32), typedPhrase: nil)
        XCTAssertEqual(step, .needsPhrase)
        XCTAssertTrue(ports.stored.isEmpty)
    }

    func testATypedPhraseMustBeTheOneThisDevicesKeysComeFrom() async throws {
        let mine = RecoveryMnemonic.generate()
        let ports = Ports()
        ports.identitySeed = mine.toSeed()

        let other = try await ports.enrollment.turnOn(
            credentialIdB64: credentialIdB64, registrationPrfOutput: bytes(32),
            typedPhrase: RecoveryMnemonic.generate().phrase())
        XCTAssertEqual(other, .refused("settings.passkeys.keys_choice.error.phrase_mismatch"))

        let invalid = try await ports.enrollment.turnOn(
            credentialIdB64: credentialIdB64, registrationPrfOutput: bytes(32), typedPhrase: "not a phrase")
        XCTAssertEqual(invalid, .refused("restore_identity.error.invalid_phrase"))
        XCTAssertTrue(ports.stored.isEmpty)
        XCTAssertTrue(ports.remembered.isEmpty)

        let right = try await ports.enrollment.turnOn(
            credentialIdB64: credentialIdB64, registrationPrfOutput: bytes(32),
            typedPhrase: "  \(mine.phrase())  ")
        XCTAssertEqual(right, .done(.stored))
        XCTAssertEqual(ports.remembered, [mine.phrase()], "the device keeps the phrase it just proved")
    }

    // MARK: - After adding a passkey: stored with nothing to choose

    private func added(prfOutput: Data?, prfSupported: Bool?) -> AddedPasskey {
        AddedPasskey(
            credentialIdB64: credentialIdB64, nickname: "Phone",
            prfOutput: prfOutput, prfSupported: prfSupported)
    }

    func testANewPasskeyWithTheRegistrationsOutputIsStoredWithNoSecondSheet() async throws {
        let mnemonic = RecoveryMnemonic.generate()
        let ports = Ports()
        ports.devicePhrase = mnemonic.phrase()
        let prf = bytes(32)
        let outcome = await ports.enrollment.storeAfterAdd(added(prfOutput: prf, prfSupported: true))
        XCTAssertEqual(outcome, .stored)
        XCTAssertTrue(ports.prfAsked.isEmpty)
        XCTAssertEqual(ports.stored.count, 1)
        XCTAssertEqual(
            try passkeyVaultOpen(prfOutput: prf, sealed: ports.stored[0].sealed).phrase(), mnemonic.phrase())
        XCTAssertEqual(outcome.messageKey, "settings.passkeys.added_restores")
        XCTAssertFalse(outcome.isError)
    }

    func testANewPasskeyWithoutARegistrationOutputIsAskedOnceForItself() async throws {
        let ports = Ports()
        ports.devicePhrase = RecoveryMnemonic.generate().phrase()
        ports.prf = (credentialId, bytes(32))
        let outcome = await ports.enrollment.storeAfterAdd(added(prfOutput: nil, prfSupported: true))
        XCTAssertEqual(outcome, .stored)
        XCTAssertEqual(ports.prfAsked, [credentialIdB64])
        XCTAssertEqual(ports.stored.count, 1)
    }

    func testANewPasskeyThePlatformSaysCannotCarryKeysAsksNothing() async throws {
        let ports = Ports()
        ports.devicePhrase = RecoveryMnemonic.generate().phrase()
        let outcome = await ports.enrollment.storeAfterAdd(added(prfOutput: nil, prfSupported: false))
        XCTAssertEqual(outcome, .noPrf)
        XCTAssertTrue(ports.prfAsked.isEmpty, "no second sheet when the platform said no")
        XCTAssertTrue(ports.stored.isEmpty)
        XCTAssertEqual(ports.noPrf, [credentialIdB64], "remembered, so its row offers no repair")
        XCTAssertEqual(PasskeyKeysAddOutcome.noPrf.messageKey, "settings.passkeys.added_no_keys")
        XCTAssertFalse(PasskeyKeysAddOutcome.noPrf.isError)
    }

    func testANewPasskeyWhoseSecondSheetGivesNoSecretLogsInOnly() async throws {
        let ports = Ports()
        ports.devicePhrase = RecoveryMnemonic.generate().phrase()
        ports.prf = (credentialId, nil)
        let outcome = await ports.enrollment.storeAfterAdd(added(prfOutput: nil, prfSupported: true))
        XCTAssertEqual(outcome, .noPrf)
        XCTAssertTrue(ports.stored.isEmpty)
        XCTAssertEqual(ports.noPrf, [credentialIdB64])
    }

    func testWhenCreationSaidNothingTheSecondSheetRuns() async throws {
        let ports = Ports()
        ports.devicePhrase = RecoveryMnemonic.generate().phrase()
        ports.prf = (credentialId, bytes(32))
        let outcome = await ports.enrollment.storeAfterAdd(added(prfOutput: nil, prfSupported: nil))
        XCTAssertEqual(outcome, .stored)
        XCTAssertEqual(ports.prfAsked, [credentialIdB64])
        XCTAssertTrue(ports.noPrf.isEmpty)
    }

    func testWithoutThePhraseOnTheDeviceANewPasskeyIsAddedForLoggingInOnly() async throws {
        let ports = Ports()
        let outcome = await ports.enrollment.storeAfterAdd(added(prfOutput: bytes(32), prfSupported: true))
        XCTAssertEqual(outcome, .phraseMissing)
        XCTAssertTrue(ports.prfAsked.isEmpty)
        XCTAssertTrue(ports.stored.isEmpty)
        XCTAssertEqual(outcome.messageKey, "settings.passkeys.added")
        XCTAssertFalse(outcome.isError)
    }

    func testCancellingTheSecondSheetStoresNothing() async throws {
        let ports = Ports()
        ports.devicePhrase = RecoveryMnemonic.generate().phrase()
        ports.prfError = WebauthnAssertion.AssertionError.cancelled
        let outcome = await ports.enrollment.storeAfterAdd(added(prfOutput: nil, prfSupported: true))
        XCTAssertEqual(outcome, .cancelled)
        XCTAssertTrue(ports.stored.isEmpty)
        XCTAssertEqual(outcome.messageKey, "settings.passkeys.keys_choice.cancelled")
        XCTAssertTrue(outcome.isError)
    }

    func testADifferentPasskeyAnsweringTheSecondSheetStoresNothing() async throws {
        let ports = Ports()
        ports.devicePhrase = RecoveryMnemonic.generate().phrase()
        ports.prf = (Data([0x09]), bytes(32))
        let outcome = await ports.enrollment.storeAfterAdd(added(prfOutput: nil, prfSupported: true))
        XCTAssertEqual(outcome, .otherPasskey)
        XCTAssertTrue(ports.stored.isEmpty)
        XCTAssertEqual(outcome.messageKey, "settings.passkeys.keys_choice.error.other_passkey")
    }

    func testAServerWithoutAVaultKeyMakesANewPasskeyUnavailable() async throws {
        let ports = Ports()
        ports.devicePhrase = RecoveryMnemonic.generate().phrase()
        ports.storeResult = false
        let outcome = await ports.enrollment.storeAfterAdd(added(prfOutput: bytes(32), prfSupported: true))
        XCTAssertEqual(outcome, .unavailable)
        XCTAssertEqual(outcome.messageKey, "settings.passkeys.keys_choice.unavailable")
    }

    func testOfflineAndOtherFailuresWhileStoring() async throws {
        let ports = Ports()
        ports.devicePhrase = RecoveryMnemonic.generate().phrase()
        ports.prfError = WebauthnClientError.network(endpoint: "/auth/action/start", underlying: URLError(.notConnectedToInternet))
        let offline = await ports.enrollment.storeAfterAdd(added(prfOutput: nil, prfSupported: true))
        XCTAssertEqual(offline, .keysFailed)
        XCTAssertEqual(offline.messageKey, "settings.passkeys.added_keys_failed")

        ports.prfError = WebauthnClientError.http(status: 500, endpoint: "/auth/action/start", body: "")
        let failed = await ports.enrollment.storeAfterAdd(added(prfOutput: nil, prfSupported: true))
        XCTAssertEqual(failed, .keysFailed)
        XCTAssertTrue(ports.stored.isEmpty)
        XCTAssertTrue(ports.noPrf.isEmpty, "a failure is not a passkey that cannot carry keys")
    }
}
