// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore

/// G-26 cross-stack SFrame interop — the iOS leg.
///
/// Pins the SAME golden vector the Rust core
/// (`crates/lakko-calls/src/sframe.rs::golden_vector_*`), the wasm/web
/// (`apps/web/src/calls/sframe-golden.test.ts`), and the Android FFI
/// (`apps/android/.../calls/SframeGoldenTest.kt`) pin: sealing
/// `(seed=[0x42;32], leaf=3, streamIndex=0, plaintext="lakko sframe golden
/// vector v1")` on a fresh `ParticipantKey` (counter 0) MUST produce these
/// EXACT bytes via the UniFFI `sealFramed`, and `openFramed` MUST recover the
/// plaintext. The KID is `(leaf << 8) | streamIndex` (ADR-0035). Because all
/// four stacks pin the identical wire, a frame sealed on any one opens on any
/// other (the cross-runtime half of G-26), no live SFU needed. Any drift in
/// the iOS SFrame stack fails here.
final class SFrameGoldenTests: XCTestCase {

    private static let goldenSeed = Data(repeating: 0x42, count: 32)
    private static let goldenLeaf: UInt32 = 3
    private static let goldenStreamIndex: UInt8 = 0
    private static let goldenPlaintext = "lakko sframe golden vector v1"
    private static let goldenWireHex =
        "910300fe2c2af65799594a4c693ff4c9e0d49b0565756ddd7299b0945fc8adb1c9c12fced57c6937f372e6b94bdae40a"

    func testSealsToTheCanonicalCrossStackWire() throws {
        let pk = try CallKey
            .fromSeed(seed: Self.goldenSeed)
            .participantKey(senderLeaf: Self.goldenLeaf, streamIndex: Self.goldenStreamIndex)
        let wire = try pk.sealFramed(plaintext: Data(Self.goldenPlaintext.utf8))
        let hex = wire.map { String(format: "%02x", $0) }.joined()
        XCTAssertEqual(hex, Self.goldenWireHex)
    }

    func testOpensTheCanonicalWireToThePlaintext() throws {
        let pk = try CallKey
            .fromSeed(seed: Self.goldenSeed)
            .participantKey(senderLeaf: Self.goldenLeaf, streamIndex: Self.goldenStreamIndex)
        let opened = try pk.openFramed(wire: Self.hexToData(Self.goldenWireHex))
        XCTAssertEqual(String(data: opened, encoding: .utf8), Self.goldenPlaintext)
    }

    private static func hexToData(_ hex: String) -> Data {
        var data = Data(capacity: hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            data.append(UInt8(hex[index..<next], radix: 16)!)
            index = next
        }
        return data
    }
}
