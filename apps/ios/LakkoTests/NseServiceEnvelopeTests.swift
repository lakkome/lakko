// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

/// Unit tests for the NSE service-envelope filter — the background half of the
/// gate the foreground `ChatClient.toEvent` has always run.
///
/// ## The defect these pin
///
/// Service envelopes ride the SAME group journal as chat, as ordinary MLS
/// application messages. The NSE fed every decrypted application body straight
/// to `parseBodyEnvelope`, whose deliberate legacy fallback degrades non-JSON
/// bytes to a `Chat` body via `from_utf8_lossy` (pre-1.19 wire compatibility).
/// So a service envelope surfaced as a notification containing its domain
/// string plus raw binary. TWO kinds were affected:
///
///   1. ADR-0042 / ADR-0044 push-routing-secret envelopes (70 bytes, domain
///      `lakko/push-routing-secret/v1\0`),
///   2. prost `CallSignalingPayload` bytes — which the foreground filtered and
///      the background did not, so a fix covering only (1) would leave this
///      one broken.
///
/// ## What is real here and what is stubbed
///
/// `nseIsServiceEnvelope` / `nseLooksLikeCallSignaling` /
/// `nseLooksLikePushRoutingSecret` are the PRODUCTION functions (the pure NSE
/// core is compiled into this test target). Only the authoritative Rust codec
/// is stubbed — the test target links no `LakkoCoreExtension.xcframework` — by
/// ``decodesAsPushRoutingSecret(_:)`` below, which re-implements the exact gate
/// `lakko_contacts::push_routing::decode_push_routing_secret` applies (domain
/// prefix, then version, then exact payload length). The codec itself is
/// covered by `crates/lakko-ffi-extension`'s Rust tests, including a golden
/// that pins the two constants this file's pre-filter keys on.
final class NseServiceEnvelopeTests: XCTestCase {

    // MARK: - Wire fixtures

    /// The frozen 29-byte domain separator `lakko/push-routing-secret/v1\0`.
    /// Written from the ASCII text + an explicit trailing NUL rather than a
    /// literal, so this source file never carries a raw NUL byte.
    private static let routingDomain: Data = {
        var d = Data("lakko/push-routing-secret/v1".utf8)
        d.append(0x00)
        return d
    }()

    /// A well-formed v2 push-routing-secret envelope:
    /// `domain(29) || version(1)=0x02 || mint_epoch(8, BE) || secret(32)`.
    private func routingEnvelope(mintEpoch: UInt64 = 9, secretByte: UInt8 = 0x42) -> Data {
        var out = Self.routingDomain
        out.append(0x02)
        for shift in stride(from: 56, through: 0, by: -8) {
            out.append(UInt8((mintEpoch >> UInt64(shift)) & 0xFF))
        }
        out.append(Data(repeating: secretByte, count: 32))
        return out
    }

    /// A prost `CallSignalingPayload` first byte — the `body` oneof fields
    /// 10..13 wire tags.
    private let callSignalingTags: [UInt8] = [0x52, 0x5A, 0x62, 0x6A]

    /// A realistic chat envelope (the ADR-0023 JSON wire shape).
    private let chatEnvelope = Data(#"{"v":1,"text":"hello there"}"#.utf8)

    /// Test double for the slim FFI's `decodePushRoutingSecret`, applying the
    /// same gate the Rust codec does — domain, THEN version, THEN exact length.
    private func decodesAsPushRoutingSecret(_ bytes: Data) -> Bool {
        let domain = Self.routingDomain
        guard bytes.count > domain.count, bytes.prefix(domain.count) == domain else { return false }
        let payload = bytes.dropFirst(domain.count)
        guard payload.first == 0x02 else { return false }   // ADR-0044 v2 only
        return payload.count == 1 + 8 + 32                  // exact-length gate
    }

    // MARK: - Kind 1: push-routing-secret

    /// WITHOUT THE FIX this returns false and the envelope reaches
    /// `parseBodyEnvelope`, which renders the domain string + binary secret.
    func testPushRoutingSecretEnvelopeIsAServiceEnvelope() {
        XCTAssertTrue(
            nseIsServiceEnvelope(
                routingEnvelope(),
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret
            ),
            "a v2 push-routing-secret envelope must never be rendered"
        )
    }

    /// The cheap peek must ACCEPT a real envelope — a too-narrow pre-filter
    /// would short-circuit before the codec and reopen the defect.
    func testPreFilterAcceptsARealEnvelope() {
        XCTAssertTrue(nseLooksLikePushRoutingSecret(routingEnvelope()))
        XCTAssertTrue(nseLooksLikePushRoutingSecret(routingEnvelope(mintEpoch: .max)))
        XCTAssertTrue(nseLooksLikePushRoutingSecret(routingEnvelope(secretByte: 0x00)))
    }

    /// ...and REJECT the near-misses, so the FFI stays off the hot path.
    func testPreFilterRejectsNearMisses() {
        XCTAssertFalse(nseLooksLikePushRoutingSecret(chatEnvelope), "wrong first byte + length")
        XCTAssertFalse(
            nseLooksLikePushRoutingSecret(routingEnvelope().dropLast()),
            "one byte short"
        )
        var wrongFirst = routingEnvelope()
        wrongFirst[wrongFirst.startIndex] = 0x7B
        XCTAssertFalse(nseLooksLikePushRoutingSecret(wrongFirst), "wrong first byte")
        XCTAssertFalse(nseLooksLikePushRoutingSecret(Data()), "empty")
    }

    /// Bytes that PASS the cheap peek but fail the codec are NOT suppressed —
    /// the codec is the authority, not the peek. A 70-byte legacy plaintext
    /// starting with `l` must still render.
    func testPeekAloneDoesNotSuppress() {
        let decoy = Data(repeating: 0x6C, count: 70)
        XCTAssertTrue(nseLooksLikePushRoutingSecret(decoy), "the decoy does pass the cheap peek")
        XCTAssertFalse(
            nseIsServiceEnvelope(decoy, decodesAsPushRoutingSecret: decodesAsPushRoutingSecret),
            "the codec, not the peek, is the authority"
        )
    }

    /// ADR-0044 is a HARD flag day: a retired v1 (62-byte) envelope is refused
    /// by the codec AND misses the v2-length peek, so it is not suppressed.
    /// Pinned so a future dual-accept in the codec is caught as a real change.
    func testRetiredV1EnvelopeIsNotSuppressed() {
        var v1 = Self.routingDomain
        v1.append(0x01)
        v1.append(Data(repeating: 0x42, count: 32))
        XCTAssertEqual(v1.count, 62)
        XCTAssertFalse(
            nseIsServiceEnvelope(v1, decodesAsPushRoutingSecret: decodesAsPushRoutingSecret)
        )
    }

    // MARK: - Kind 2: call signaling

    /// WITHOUT THE FIX every one of these four tags reaches `parseBodyEnvelope`
    /// and renders as garbage — the kind a routing-secret-only fix would miss.
    func testCallSignalingBytesAreServiceEnvelopes() {
        for tag in callSignalingTags {
            var payload = Data([tag])
            payload.append(Data(repeating: 0x01, count: 24))
            XCTAssertTrue(
                nseLooksLikeCallSignaling(payload),
                "0x\(String(tag, radix: 16)) is a CallSignalingPayload body tag"
            )
            XCTAssertTrue(
                nseIsServiceEnvelope(
                    payload,
                    decodesAsPushRoutingSecret: decodesAsPushRoutingSecret
                ),
                "call signaling must never be rendered"
            )
        }
    }

    // MARK: - A real chat message is never suppressed

    func testChatEnvelopeIsNotAServiceEnvelope() {
        XCTAssertFalse(nseLooksLikeCallSignaling(chatEnvelope))
        XCTAssertFalse(
            nseIsServiceEnvelope(
                chatEnvelope,
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret
            ),
            "a chat envelope must still render"
        )
        XCTAssertFalse(
            nseIsServiceEnvelope(Data(), decodesAsPushRoutingSecret: decodesAsPushRoutingSecret),
            "empty bytes are not a service envelope"
        )
    }

    // MARK: - Pipeline: the filter drops the row and the loop falls back

    /// A throwaway group mirroring the production `NseRealThrowawayGroup
    /// .process` shape: run the service filter FIRST, then classify. The MLS
    /// decrypt + the Rust codec are the only stubbed parts.
    private final class FilteringFakeGroup: NseThrowawayGroup {
        private let decodesAsPushRoutingSecret: (Data) -> Bool
        private let text: (Data) -> String
        private(set) var suppressed = 0
        var closed = false

        init(
            decodesAsPushRoutingSecret: @escaping (Data) -> Bool,
            text: @escaping (Data) -> String
        ) {
            self.decodesAsPushRoutingSecret = decodesAsPushRoutingSecret
            self.text = text
        }

        func process(_ payload: Data) -> NseProcessedRow? {
            if nseIsServiceEnvelope(payload, decodesAsPushRoutingSecret: decodesAsPushRoutingSecret) {
                suppressed += 1
                return nil
            }
            return NseProcessedRow(kind: .chat, text: text(payload), attachmentCount: 0)
        }

        func close() { closed = true }
    }

    private struct StubDeps: NsePushDecryptDeps {
        let group: NseThrowawayGroup
        let rows: [NseFetchedRow]
        let groupIdBytes = Data(repeating: 0xAA, count: 32)

        func lookupRoute(pushGidHex: String) -> (accountIdB64: String, groupIdHex: String)? { nil }
        func resolveAccount(groupHex: String) -> NseResolvedAccount? {
            NseResolvedAccount(accountIdB64: "acct", groupIdBytes: groupIdBytes)
        }
        func loadIdentitySeed(accountIdB64: String) -> Data? { Data(repeating: 1, count: 64) }
        func loadSnapshot(accountIdB64: String) -> Data? { Data(repeating: 2, count: 8) }
        func openThrowawayGroup(
            seed: Data,
            snapshot: Data,
            groupIdBytes: Data
        ) throws -> NseThrowawayGroup? { group }
        func dsFetch(groupIdBytes: Data, afterId: Int64) async throws -> [NseFetchedRow] { rows }
        func startCursor(accountIdB64: String, groupHex: String) -> Int64? { nil }
        func isAntiCensorshipArmed() -> Bool { false }
    }

    private let groupHex = String(repeating: "a", count: 64)

    private func preview(rows: [NseFetchedRow]) async -> (NsePushPreview?, Int) {
        let group = FilteringFakeGroup(
            decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
            text: { String(decoding: $0, as: UTF8.self) }
        )
        let result = await decryptPushPreview(
            NsePushWake(groupHex: groupHex, pointer: 99),
            StubDeps(group: group, rows: rows)
        )
        XCTAssertTrue(group.closed, "the throwaway group is always closed")
        return (result, group.suppressed)
    }

    /// The NEWEST row is a service envelope; the real chat BEFORE it must
    /// survive as the preview. Without the fix the routing envelope wins
    /// (newest-wins) and the body is its domain string + binary secret.
    func testServiceEnvelopeAfterChatFallsBackToTheChat() async {
        let (result, suppressed) = await preview(rows: [
            NseFetchedRow(id: 10, payload: Data("real message".utf8)),
            NseFetchedRow(id: 11, payload: routingEnvelope()),
        ])
        XCTAssertEqual(result?.body, "real message")
        XCTAssertEqual(suppressed, 1)
    }

    /// A service envelope BEFORE a real chat must not disturb it either.
    func testServiceEnvelopeBeforeChatKeepsTheChat() async {
        let (result, suppressed) = await preview(rows: [
            NseFetchedRow(id: 10, payload: routingEnvelope()),
            NseFetchedRow(id: 11, payload: Data("real message".utf8)),
        ])
        XCTAssertEqual(result?.body, "real message")
        XCTAssertEqual(suppressed, 1)
    }

    /// Both kinds sandwiching a real chat — the exact shape a rotation burst
    /// plus an in-progress call produces on a busy group.
    func testBothKindsSandwichingAChatKeepTheChat() async {
        let (result, suppressed) = await preview(rows: [
            NseFetchedRow(id: 10, payload: Data([0x52] + Array(repeating: 0x01, count: 16))),
            NseFetchedRow(id: 11, payload: Data("real message".utf8)),
            NseFetchedRow(id: 12, payload: routingEnvelope()),
            NseFetchedRow(id: 13, payload: Data([0x6A] + Array(repeating: 0x02, count: 16))),
        ])
        XCTAssertEqual(result?.body, "real message")
        XCTAssertEqual(suppressed, 3)
    }

    /// A batch of ONLY service envelopes yields no preview at all, so the
    /// handler degrades to its generic "New message" banner. Without the fix
    /// this rendered a garbage body instead of staying generic.
    func testOnlyServiceEnvelopesYieldNoPreview() async {
        let (result, suppressed) = await preview(rows: [
            NseFetchedRow(id: 10, payload: routingEnvelope(mintEpoch: 3)),
            NseFetchedRow(id: 11, payload: Data([0x5A] + Array(repeating: 0x07, count: 8))),
            NseFetchedRow(id: 12, payload: routingEnvelope(mintEpoch: 4, secretByte: 0xFF)),
        ])
        XCTAssertNil(result, "no real message → generic banner")
        XCTAssertEqual(suppressed, 3)
    }
}
