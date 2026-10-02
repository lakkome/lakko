// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// A passkey that restores keys: the pure rules, the core's seal / open run
/// for real, and the "Turn on" sequence of the keys choice.
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

    func testTheChoiceFollowsOnlyWhenThePlatformCanProtectKeys() {
        XCTAssertEqual(PasskeyVault.addOutcome(prfOutput: true, prfSupported: nil), .offerKeysChoice)
        XCTAssertEqual(PasskeyVault.addOutcome(prfOutput: false, prfSupported: true), .offerKeysChoice)
        XCTAssertEqual(PasskeyVault.addOutcome(prfOutput: false, prfSupported: false), .signInOnly)
        // Nothing reported: a platform older than iOS 18.
        XCTAssertEqual(PasskeyVault.addOutcome(prfOutput: false, prfSupported: nil), .signInOnly)
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

    // MARK: - "Turn on"

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

        var enrollment: PasskeyKeysEnrollment {
            PasskeyKeysEnrollment(
                devicePhrase: { self.devicePhrase },
                identitySeed: { self.identitySeed },
                evaluatePrf: { id in
                    self.prfAsked.append(id)
                    if let error = self.prfError { throw error }
                    return self.prf ?? (Data(), nil)
                },
                store: { id, sealed in
                    self.stored.append((id, sealed))
                    return self.storeResult
                },
                rememberPhrase: { self.remembered.append($0) }
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

    func testTheResultLines() {
        XCTAssertEqual(KeysChoiceResult.stored.messageKey, "settings.passkeys.keys_choice.done")
        XCTAssertEqual(KeysChoiceResult.noPrf.messageKey, "settings.passkeys.keys_choice.no_prf")
        XCTAssertEqual(KeysChoiceResult.unavailable.messageKey, "settings.passkeys.keys_choice.unavailable")
        XCTAssertNil(KeysChoiceResult.signInOnly.messageKey)
    }
}
