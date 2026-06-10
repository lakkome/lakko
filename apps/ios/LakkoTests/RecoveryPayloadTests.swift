// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0018 factor (a) — iOS `RecoveryPayload` codec tests.
///
/// Mirror of the Android `RecoveryPayloadTest` (host-JVM) plus the
/// seal/open round-trip the Android suite defers to instrumentation:
/// the simulator test target links the real `LakkoCore` FFI (same as
/// `PhraseSeededIdentityTests`), so `sealWithKey` / `openWithKey` run
/// for real here.
///
/// Byte-compatibility with the web reference
/// (`apps/web/src/devices/recovery-payload.ts`) is pinned by the
/// magic-prefix + layout assertions — cross-platform pairing (an iOS
/// scanner publishing rows a web/Android joiner opens, and vice
/// versa) depends on it.
final class RecoveryPayloadTests: XCTestCase {

    private func grant(_ fill: UInt8 = 0x41) -> Data {
        Data(repeating: fill, count: RecoveryPayload.recoveryGrantTokenLength)
    }

    private func seed(_ fill: UInt8 = 0x42) -> Data {
        Data(repeating: fill, count: RecoveryPayload.recoveryPhraseSeedLength)
    }

    // MARK: - Payload layout

    func testBuildAndParseRoundTrip() throws {
        let payload = try RecoveryPayload.buildRecoveryPayload(
            grantToken: grant(),
            phraseSeed: seed()
        )
        XCTAssertEqual(payload.count, RecoveryPayload.recoveryPayloadLength)
        let parsed = try RecoveryPayload.parseRecoveryPayload(payload)
        XCTAssertEqual(parsed.grantToken, grant())
        XCTAssertEqual(parsed.phraseSeed, seed())
    }

    func testPayloadLayoutIsGrantThenSeed() throws {
        // Pin the byte layout (grant(32) then seed(64)) — web compat.
        let payload = try RecoveryPayload.buildRecoveryPayload(
            grantToken: grant(0x01),
            phraseSeed: seed(0x02)
        )
        XCTAssertEqual(payload.count, 96)
        XCTAssertTrue(payload.prefix(32).allSatisfy { $0 == 0x01 })
        XCTAssertTrue(payload.dropFirst(32).allSatisfy { $0 == 0x02 })
    }

    func testBuildRejectsWrongGrantLength() {
        XCTAssertThrowsError(
            try RecoveryPayload.buildRecoveryPayload(
                grantToken: Data(count: 31),
                phraseSeed: seed()
            )
        )
    }

    func testBuildRejectsWrongSeedLength() {
        XCTAssertThrowsError(
            try RecoveryPayload.buildRecoveryPayload(
                grantToken: grant(),
                phraseSeed: Data(count: 63)
            )
        )
    }

    func testParseRejectsWrongLength() {
        XCTAssertThrowsError(
            try RecoveryPayload.parseRecoveryPayload(Data(count: 95))
        )
    }

    // MARK: - Mailbox row wrap / unwrap

    func testRowWrapUnwrapRoundTrip() {
        let sealed = Data((0..<124).map { UInt8($0 % 251) })
        let row = RecoveryPayload.wrapRecoveryRow(sealedBlob: sealed)
        XCTAssertTrue(RecoveryPayload.isRecoveryPayloadRow(row))
        XCTAssertEqual(RecoveryPayload.unwrapRecoveryRow(row), sealed)
    }

    func testRowMagicMatchesWebReference() {
        // The ASCII magic must be byte-identical to the web's
        // RECOVERY_ROW_MAGIC ("lakko/recovery-payload/v1") —
        // cross-platform pairing depends on it.
        let expectedMagic = Data("lakko/recovery-payload/v1".utf8)
        let row = RecoveryPayload.wrapRecoveryRow(sealedBlob: Data([0x7F]))
        XCTAssertEqual(row.prefix(expectedMagic.count), expectedMagic)
        XCTAssertEqual(row[expectedMagic.count], 0x7F)
    }

    func testNonRecoveryRowsAreNotMisclassified() {
        // A Welcome / history-bundle-shaped row must not be taken
        // for a recovery row.
        XCTAssertFalse(RecoveryPayload.isRecoveryPayloadRow(Data(count: 64)))
        XCTAssertNil(RecoveryPayload.unwrapRecoveryRow(Data(count: 64)))
        // A bare magic with no sealed blob is refused too.
        let bareMagic = Data("lakko/recovery-payload/v1".utf8)
        XCTAssertFalse(RecoveryPayload.isRecoveryPayloadRow(bareMagic))
    }

    // MARK: - Seal / open round-trip (real FFI)

    func testSealOpenRoundTripUnderExporterShapedKey() throws {
        // A 32-byte raw key — the exact shape
        // `DeviceGroup.exportSecret(..., length: 32)` returns. The
        // exporter derivation itself needs two live DeviceGroup
        // members and is covered by the Rust core property tests;
        // here we pin the client-side seal -> wrap -> unwrap -> open
        // -> parse chain end-to-end through the real FFI.
        let key = Data(repeating: 0x33, count: RecoveryPayload.recoveryExporterKeyLength)
        let payload = try RecoveryPayload.buildRecoveryPayload(
            grantToken: grant(0x05),
            phraseSeed: seed(0x06)
        )

        let sealed = try sealWithKey(key: key, plaintext: payload)
        // ChaCha20-Poly1305 wire: nonce(12) || ciphertext+tag(16).
        XCTAssertEqual(sealed.count, 12 + payload.count + 16)

        let row = RecoveryPayload.wrapRecoveryRow(sealedBlob: sealed)
        XCTAssertTrue(RecoveryPayload.isRecoveryPayloadRow(row))
        let unwrapped = try XCTUnwrap(RecoveryPayload.unwrapRecoveryRow(row))

        let opened = try openWithKey(key: key, wire: unwrapped)
        let parsed = try RecoveryPayload.parseRecoveryPayload(opened)
        XCTAssertEqual(parsed.grantToken, grant(0x05))
        XCTAssertEqual(parsed.phraseSeed, seed(0x06))
    }

    func testOpenRefusesWrongKey() throws {
        let key = Data(repeating: 0x33, count: RecoveryPayload.recoveryExporterKeyLength)
        let wrongKey = Data(repeating: 0x34, count: RecoveryPayload.recoveryExporterKeyLength)
        let payload = try RecoveryPayload.buildRecoveryPayload(
            grantToken: grant(),
            phraseSeed: seed()
        )
        let sealed = try sealWithKey(key: key, plaintext: payload)
        XCTAssertThrowsError(try openWithKey(key: wrongKey, wire: sealed))
    }
}
