// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0017 (Part 1) — iOS phrase-seeded AccountIdentity tests.
///
/// The security-critical property of the overhaul: the canonical
/// AccountIdentity is derived from the stable BIP39 recovery-phrase
/// seed (`RecoveryMnemonic.toSeed()`, 64 bytes), NOT the
/// password-derived OPAQUE export key — so a routine recover /
/// password change preserves the identity (the pubkey contacts pin
/// never churns).
///
/// These run against the real `RecoveryMnemonic` + `Identity` FFI —
/// no mocking — so they pin the exact derivation the production flows
/// use. They are network-free: the orchestration that resolves
/// account_id + HARD-BLOCKs against the published bundle is exercised
/// by integration paths, but the pure derivation invariant is what
/// makes recover safe, and that is what these assert.
final class PhraseSeededIdentityTests: XCTestCase {

    /// A fixed, valid 12-word BIP39 phrase (the all-`abandon` vector
    /// with the canonical 12th word `about`).
    private static let phrase =
        "abandon abandon abandon abandon abandon abandon " +
        "abandon abandon abandon abandon abandon about"

    /// Derive the canonical AccountIdentity signature pubkey from a
    /// recovery phrase, exactly as the production flows do
    /// (`fromPhrase().toSeed()` → `Identity.fromSeed(seed:)`).
    private func accountPubkey(forPhrase phrase: String) throws -> Data {
        let seed = try RecoveryMnemonic.fromPhrase(phrase: phrase).toSeed()
        return try Identity.fromSeed(seed: seed).signaturePublicKey()
    }

    /// The BIP39 seed is exactly 64 bytes — the
    /// `AccountIdentitySeedStore` payload length contract.
    func testPhraseSeedIs64Bytes() throws {
        let seed = try RecoveryMnemonic.fromPhrase(phrase: Self.phrase).toSeed()
        XCTAssertEqual(seed.count, 64)
        XCTAssertEqual(seed.count, AccountIdentitySeedStore.seedLength)
    }

    /// CORE ADR-0017 PROPERTY: signup→recover with the SAME phrase
    /// yields the SAME AccountIdentity pubkey.
    ///
    /// Signup derives the identity from `toSeed(phrase)`; a later
    /// recover re-enters the SAME phrase (the password rotates, the
    /// phrase does not), so it re-derives `toSeed(phrase)` and the
    /// pubkey must be byte-for-byte identical. This is what lets
    /// contacts keep their pin across a recover.
    func testSignupThenRecoverSamePhraseYieldsSameAccountIdentity() throws {
        let signupPubkey = try accountPubkey(forPhrase: Self.phrase)
        // A fresh `RecoveryMnemonic` handle from the same words — the
        // recover path constructs a new one from the typed phrase.
        let recoverPubkey = try accountPubkey(forPhrase: Self.phrase)
        XCTAssertEqual(
            signupPubkey,
            recoverPubkey,
            "ADR-0017 invariant: same recovery phrase => same " +
            "AccountIdentity pubkey (recover must not rotate the identity)"
        )
    }

    /// The decoupling is real: the phrase-seeded AccountIdentity is a
    /// DIFFERENT key than one derived from a password export key. If
    /// these were equal the overhaul would be a no-op; the assertion
    /// proves recover now preserves identity precisely because the
    /// identity no longer rides on the (rotating) export key.
    func testPhraseSeededIdentityDiffersFromExportKeySeeded() throws {
        let phrasePubkey = try accountPubkey(forPhrase: Self.phrase)
        // A stand-in 64-byte "export key" (the OPAQUE export key is the
        // pre-ADR-0017 seed). It must derive a different identity.
        let exportKey = Data(repeating: 0x5A, count: 64)
        let exportPubkey = try Identity
            .fromSeed(seed: exportKey)
            .signaturePublicKey()
        XCTAssertNotEqual(
            phrasePubkey,
            exportPubkey,
            "the phrase-seeded identity must differ from an " +
            "export-key-seeded one (otherwise the decoupling is moot)"
        )
    }

    /// A DIFFERENT phrase yields a DIFFERENT AccountIdentity — the
    /// derivation actually depends on the phrase bytes (so the
    /// HARD-BLOCK verify can reject a wrong phrase).
    func testDifferentPhraseYieldsDifferentAccountIdentity() throws {
        let other =
            "legal winner thank year wave sausage worth useful legal " +
            "winner thank yellow"
        let a = try accountPubkey(forPhrase: Self.phrase)
        let b = try accountPubkey(forPhrase: other)
        XCTAssertNotEqual(a, b)
    }

    /// ADR-0064 decision 3: a phrase is exactly 12 words. The retired
    /// 24-word shape and a 12-word phrase with a bad checksum are both
    /// refused by the core, so no screen can accept either.
    func testOnlyAValidTwelveWordPhraseIsAccepted() {
        let twentyFour = Array(repeating: "abandon", count: 23).joined(separator: " ") + " art"
        XCTAssertThrowsError(try RecoveryMnemonic.fromPhrase(phrase: twentyFour))
        let badChecksum = Array(repeating: "abandon", count: 12).joined(separator: " ")
        XCTAssertThrowsError(try RecoveryMnemonic.fromPhrase(phrase: badChecksum))
        XCTAssertEqual(
            PhraseWords.words(of: RecoveryMnemonic.generate().phrase()).count,
            PhraseWords.count)
    }

    /// The `AccountIdentitySeedStore` round-trips the 64-byte seed
    /// through the Keychain and refuses a wrong-length payload.
    func testAccountIdentitySeedStoreRoundTrip() throws {
        let acct = base64Encode(Data(repeating: 0xE1, count: 16))
        // Clean any prior row from a previous run.
        AccountIdentitySeedStore.deleteAccountIdentitySeed(accountIdB64: acct)
        defer { AccountIdentitySeedStore.deleteAccountIdentitySeed(accountIdB64: acct) }

        XCTAssertNil(try AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: acct))

        let seed = try RecoveryMnemonic.fromPhrase(phrase: Self.phrase).toSeed()
        try AccountIdentitySeedStore.putAccountIdentitySeed(
            accountIdB64: acct,
            seed: seed
        )
        let readBack = try AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: acct)
        XCTAssertEqual(readBack, seed)

        // Wrong length is refused.
        XCTAssertThrowsError(
            try AccountIdentitySeedStore.putAccountIdentitySeed(
                accountIdB64: acct,
                seed: Data(repeating: 0, count: 32)
            )
        )

        AccountIdentitySeedStore.deleteAccountIdentitySeed(accountIdB64: acct)
        XCTAssertNil(try AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: acct))
    }
}
