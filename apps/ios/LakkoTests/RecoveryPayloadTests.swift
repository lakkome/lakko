// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// The another-device recovery payload, v2: `grant(32) || phrase
/// entropy(16)`, sealed under the per-pairing exporter key and tagged
/// `lakko/recovery-payload/v2`.
///
/// The simulator test target links the real `LakkoCore`, so `sealWithKey` /
/// `openWithKey` / `RecoveryMnemonic.fromEntropy` run for real here.
///
/// Byte-compatibility with the web reference
/// (`apps/web/src/devices/recovery-payload.ts`) is pinned by the magic,
/// label and layout assertions: an iOS device hands keys to a web or
/// Android device and receives them from one.
final class RecoveryPayloadTests: XCTestCase {

    private func grant(_ fill: UInt8 = 0x41) -> Data {
        Data(repeating: fill, count: RecoveryPayload.recoveryGrantTokenLength)
    }

    private func entropy(_ fill: UInt8 = 0x42) -> Data {
        Data(repeating: fill, count: RecoveryPayload.recoveryPhraseEntropyLength)
    }

    // MARK: - Wire constants

    func testTheWireConstantsAreTheV2Ones() {
        XCTAssertEqual(RecoveryPayload.recoveryExporterLabel, "lakko/device-recovery/v2")
        XCTAssertEqual(RecoveryPayload.recoveryGrantTokenLength, 32)
        XCTAssertEqual(RecoveryPayload.recoveryPhraseEntropyLength, 16)
        XCTAssertEqual(RecoveryPayload.recoveryPayloadLength, 48)
    }

    // MARK: - Payload layout

    func testBuildAndParseRoundTrip() throws {
        let payload = try RecoveryPayload.buildRecoveryPayload(
            grantToken: grant(),
            phraseEntropy: entropy()
        )
        XCTAssertEqual(payload.count, 48)
        let parsed = try RecoveryPayload.parseRecoveryPayload(payload)
        XCTAssertEqual(parsed.grantToken, grant())
        XCTAssertEqual(parsed.phraseEntropy, entropy())
    }

    func testPayloadLayoutIsGrantThenEntropy() throws {
        let payload = try RecoveryPayload.buildRecoveryPayload(
            grantToken: grant(0x01),
            phraseEntropy: entropy(0x02)
        )
        XCTAssertTrue(payload.prefix(32).allSatisfy { $0 == 0x01 })
        XCTAssertTrue(payload.dropFirst(32).allSatisfy { $0 == 0x02 })
    }

    func testBuildRejectsWrongGrantLength() {
        for count in [0, 31, 33] {
            XCTAssertThrowsError(
                try RecoveryPayload.buildRecoveryPayload(
                    grantToken: Data(count: count), phraseEntropy: entropy()))
        }
    }

    func testBuildRejectsWrongEntropyLengthIncludingTheOldSeed() {
        // 64 is the v1 seed, 32 the entropy of a 24-word phrase.
        for count in [0, 15, 17, 32, 64] {
            XCTAssertThrowsError(
                try RecoveryPayload.buildRecoveryPayload(
                    grantToken: grant(), phraseEntropy: Data(count: count)),
                "\(count) bytes")
        }
    }

    func testParseRejectsEveryOtherLengthIncludingAV1Payload() {
        // 96 = grant(32) || seed(64): the v1 plaintext.
        for count in [0, 47, 49, 96] {
            XCTAssertThrowsError(
                try RecoveryPayload.parseRecoveryPayload(Data(count: count)), "\(count) bytes")
        }
    }

    func testParseAcceptsASliceThatDoesNotStartAtZero() throws {
        let padded = Data([0xFF, 0xFF]) + grant(0x05) + entropy(0x06)
        let parsed = try RecoveryPayload.parseRecoveryPayload(padded.dropFirst(2))
        XCTAssertEqual(parsed.grantToken, grant(0x05))
        XCTAssertEqual(parsed.phraseEntropy, entropy(0x06))
    }

    // MARK: - Mailbox row wrap / unwrap

    func testRowWrapUnwrapRoundTrip() {
        let sealed = Data((0..<76).map { UInt8($0 % 251) })
        let row = RecoveryPayload.wrapRecoveryRow(sealedBlob: sealed)
        XCTAssertTrue(RecoveryPayload.isRecoveryPayloadRow(row))
        XCTAssertEqual(RecoveryPayload.unwrapRecoveryRow(row), sealed)
    }

    func testRowMagicMatchesWebReference() {
        let expectedMagic = Data("lakko/recovery-payload/v2".utf8)
        let row = RecoveryPayload.wrapRecoveryRow(sealedBlob: Data([0x7F]))
        XCTAssertEqual(row.prefix(expectedMagic.count), expectedMagic)
        XCTAssertEqual(row[expectedMagic.count], 0x7F)
    }

    /// No dual accept: a row tagged v1 is not a recovery row at all.
    func testAV1RowIsNotRecognised() {
        let v1 = Data("lakko/recovery-payload/v1".utf8) + Data(count: 124)
        XCTAssertFalse(RecoveryPayload.isRecoveryPayloadRow(v1))
        XCTAssertNil(RecoveryPayload.unwrapRecoveryRow(v1))
    }

    func testNonRecoveryRowsAreNotMisclassified() {
        XCTAssertFalse(RecoveryPayload.isRecoveryPayloadRow(Data(count: 64)))
        XCTAssertNil(RecoveryPayload.unwrapRecoveryRow(Data(count: 64)))
        // A bare magic with no sealed blob is refused too.
        let bareMagic = Data("lakko/recovery-payload/v2".utf8)
        XCTAssertFalse(RecoveryPayload.isRecoveryPayloadRow(bareMagic))
    }

    // MARK: - Entropy -> phrase and seed (real core)

    func testTheTransferredEntropyRebuildsThePhraseAndItsSeed() throws {
        let mnemonic = RecoveryMnemonic.generate()
        let rebuilt = try RecoveryPayload.phraseAndSeed(fromEntropy: mnemonic.toEntropy())
        XCTAssertEqual(mnemonic.toEntropy().count, 16)
        XCTAssertEqual(rebuilt.phrase, mnemonic.phrase())
        XCTAssertEqual(rebuilt.seed, mnemonic.toSeed())
        XCTAssertEqual(rebuilt.seed.count, 64)
        XCTAssertEqual(PhraseWords.words(of: rebuilt.phrase).count, 12)
    }

    func testEntropyOfAnyOtherLengthIsRefused() {
        for count in [0, 15, 17, 32, 64] {
            XCTAssertThrowsError(
                try RecoveryPayload.phraseAndSeed(fromEntropy: Data(count: count)), "\(count) bytes")
        }
    }

    // MARK: - Seal / open round-trip (real core)

    /// The sender's chain (stored phrase -> entropy -> payload -> seal ->
    /// row) and the receiver's (row -> open -> parse -> phrase + seed),
    /// under a 32-byte key of the shape the exporter returns. The exporter
    /// derivation itself needs two live DeviceGroup members and is covered
    /// by the Rust core's tests.
    func testSenderToReceiverRoundTripThroughTheRealCore() throws {
        let key = Data(repeating: 0x33, count: RecoveryPayload.recoveryExporterKeyLength)
        let mnemonic = RecoveryMnemonic.generate()
        let payload = try RecoveryPayload.buildRecoveryPayload(
            grantToken: grant(0x05),
            phraseEntropy: try RecoveryMnemonic.fromPhrase(phrase: mnemonic.phrase()).toEntropy()
        )

        let sealed = try sealWithKey(key: key, plaintext: payload)
        // ChaCha20-Poly1305 wire: nonce(12) || ciphertext+tag(16).
        XCTAssertEqual(sealed.count, 12 + 48 + 16)

        let row = RecoveryPayload.wrapRecoveryRow(sealedBlob: sealed)
        // The published row never carries the entropy in the clear.
        XCTAssertNil(row.range(of: mnemonic.toEntropy()))
        let unwrapped = try XCTUnwrap(RecoveryPayload.unwrapRecoveryRow(row))

        let opened = try openWithKey(key: key, wire: unwrapped)
        let parsed = try RecoveryPayload.parseRecoveryPayload(opened)
        XCTAssertEqual(parsed.grantToken, grant(0x05))
        let rebuilt = try RecoveryPayload.phraseAndSeed(fromEntropy: parsed.phraseEntropy)
        XCTAssertEqual(rebuilt.phrase, mnemonic.phrase())
        XCTAssertEqual(rebuilt.seed, mnemonic.toSeed())
    }

    func testOpenRefusesWrongKey() throws {
        let key = Data(repeating: 0x33, count: RecoveryPayload.recoveryExporterKeyLength)
        let wrongKey = Data(repeating: 0x34, count: RecoveryPayload.recoveryExporterKeyLength)
        let payload = try RecoveryPayload.buildRecoveryPayload(
            grantToken: grant(),
            phraseEntropy: entropy()
        )
        let sealed = try sealWithKey(key: key, plaintext: payload)
        XCTAssertThrowsError(try openWithKey(key: wrongKey, wire: sealed))
    }
}
