// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore

/// G-26 cross-stack SFrame interop — the iOS leg.
///
/// Pins the SAME golden vector the Rust core
/// (`crates/lakko-calls/src/sframe.rs::golden_vector_*`), the wasm/web
/// (`apps/web/src/calls/sframe-golden.test.ts`), and the Android FFI
/// (`apps/android/.../calls/SframeGoldenTest.kt`) pin: the frame
/// `(seed=[0x42;32], leaf=3, streamIndex=0, plaintext="lakko sframe golden
/// vector v1")` sealed at counter 1. The UniFFI `openFramed` MUST recover the
/// plaintext from these EXACT bytes. Since ADR-0069 a `ParticipantKey` starts
/// at a random counter below 2^62 and nothing on the FFI sets a counter, so
/// `sealFramed` no longer reproduces the golden bytes; the seal is proved by
/// opening what it sealed and by reading the header (the KID, and the 64-bit
/// counter). The KID is `(leaf << 8) | streamIndex` (ADR-0035). Because all
/// four stacks open the identical wire, a frame sealed on any one opens on any
/// other (the cross-runtime half of G-26), no live SFU needed. Any drift in
/// the iOS SFrame stack fails here.
final class SFrameGoldenTests: XCTestCase {

    private static let goldenSeed = Data(repeating: 0x42, count: 32)
    private static let goldenLeaf: UInt32 = 3
    private static let goldenStreamIndex: UInt8 = 0
    private static let goldenPlaintext = "lakko sframe golden vector v1"
    private static let goldenWireHex =
        "910300fe2c2af65799594a4c693ff4c9e0d49b0565756ddd7299b0945fc8adb1c9c12fced57c6937f372e6b94bdae40a"

    /// A fresh key seals a frame that carries the composite KID and a 64-bit
    /// counter one above the key's random start, and the frame opens under an
    /// independently derived key (ADR-0069). The name is kept from when a
    /// fresh key sealed the golden bytes themselves.
    func testSealsToTheCanonicalCrossStackWire() throws {
        let callKey = try CallKey.fromSeed(seed: Self.goldenSeed)
        let sender = try callKey
            .participantKey(senderLeaf: Self.goldenLeaf, streamIndex: Self.goldenStreamIndex)
        let start: UInt64 = sender.counter()
        XCTAssertLessThan(start, UInt64(1) << 62)

        let wire = try sender.sealFramed(plaintext: Data(Self.goldenPlaintext.utf8))
        let header = try unwrapSframeFrame(wire: wire)
        // The whole composite KID: (leaf 3 << 8) | stream 0.
        XCTAssertEqual(header.senderLeaf, 0x0300)
        XCTAssertEqual(header.counter, start + 1)
        XCTAssertEqual(sender.counter(), start + 1)

        // An independently derived key (its own random counter) opens it.
        let receiver = try callKey
            .participantKey(senderLeaf: Self.goldenLeaf, streamIndex: Self.goldenStreamIndex)
        XCTAssertNotEqual(receiver.counter(), start)
        let opened = try receiver.openFramed(wire: wire)
        XCTAssertEqual(String(data: opened, encoding: .utf8), Self.goldenPlaintext)
    }

    func testOpensTheCanonicalWireToThePlaintext() throws {
        let pk = try CallKey
            .fromSeed(seed: Self.goldenSeed)
            .participantKey(senderLeaf: Self.goldenLeaf, streamIndex: Self.goldenStreamIndex)
        let wire = Self.hexToData(Self.goldenWireHex)
        let opened = try pk.openFramed(wire: wire)
        XCTAssertEqual(String(data: opened, encoding: .utf8), Self.goldenPlaintext)

        let header = try unwrapSframeFrame(wire: wire)
        XCTAssertEqual(header.senderLeaf, 0x0300)
        XCTAssertEqual(header.counter, 1)
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
