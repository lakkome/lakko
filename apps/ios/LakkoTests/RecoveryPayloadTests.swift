// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// The another-device recovery row, v3 (G-56): the codec and the row framing
/// are the core's (`composeRecoveryPayload`, `decodeRecoveryPayload`,
/// `wrapRecoveryRow`, `unwrapRecoveryRow`); the golden vector and every
/// refusal are in ``EarlierKeysGateTests``. Here: the phrase rebuild and the
/// sender-to-receiver chain through the real core.
final class RecoveryPayloadTests: XCTestCase {

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

    /// The sender's chain (compose -> seal -> row) and the receiver's (row ->
    /// unwrap -> open -> decode -> phrase + seed + predecessors), under a
    /// 32-byte key of the shape the exporter returns.
    func testSenderToReceiverRoundTripThroughTheRealCore() throws {
        let key = Data(repeating: 0x33, count: Int(recoveryExporterKeyLen()))
        let mnemonic = RecoveryMnemonic.generate()
        let older = RecoveryMnemonic.generate().toSeed()
        let grant = Data(repeating: 0x05, count: 32)
        let payload = try composeRecoveryPayload(
            grant: grant,
            phraseEntropy: mnemonic.toEntropy(),
            predecessors: [FfiRecoveryPredecessor(seed: older, adoptedAtMs: 1_700_000_000_000)])
        XCTAssertEqual(payload.count, 49 + 72)

        let sealed = try sealWithKey(key: key, plaintext: payload)
        // ChaCha20-Poly1305 wire: nonce(12) || ciphertext+tag(16).
        XCTAssertEqual(sealed.count, 12 + payload.count + 16)

        let row = wrapRecoveryRow(sealed: sealed)
        XCTAssertEqual(row.prefix(25), Data("lakko/recovery-payload/v3".utf8))
        // The published row never carries the entropy or a seed in the clear.
        XCTAssertNil(row.range(of: mnemonic.toEntropy()))
        XCTAssertNil(row.range(of: older))
        let unwrapped = try XCTUnwrap(try unwrapRecoveryRow(row: row))

        let opened = try openWithKey(key: key, wire: unwrapped)
        let decoded = try decodeRecoveryPayload(plaintext: opened)
        XCTAssertEqual(decoded.grant, grant)
        XCTAssertEqual(decoded.predecessors.map(\.seed), [older])
        let rebuilt = try RecoveryPayload.phraseAndSeed(fromEntropy: decoded.phraseEntropy)
        XCTAssertEqual(rebuilt.phrase, mnemonic.phrase())
        XCTAssertEqual(rebuilt.seed, mnemonic.toSeed())
    }

    func testOpenRefusesWrongKey() throws {
        let key = Data(repeating: 0x33, count: 32)
        let wrongKey = Data(repeating: 0x34, count: 32)
        let payload = try composeRecoveryPayload(
            grant: Data(repeating: 0x41, count: 32),
            phraseEntropy: RecoveryMnemonic.generate().toEntropy(),
            predecessors: [])
        let sealed = try sealWithKey(key: key, plaintext: payload)
        XCTAssertThrowsError(try openWithKey(key: wrongKey, wire: sealed))
    }
}
