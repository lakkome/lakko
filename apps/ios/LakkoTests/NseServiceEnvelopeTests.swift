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
/// string plus raw binary. THREE kinds are affected:
///
///   1. ADR-0042 / ADR-0044 push-routing-secret envelopes (70 bytes, domain
///      `lakko/push-routing-secret/v1\0`),
///   2. prost `CallSignalingPayload` bytes — which the foreground filtered and
///      the background did not, so a fix covering only (1) would leave this
///      one broken,
///   3. ADR-0045 committer-liveness beacons (27 bytes, domain
///      `lakko/committer-beacon/v1\0`) — the MOST reachable of the three, since
///      a routing secret is broadcast only on a membership change while a
///      beacon is emitted by every owner and admin every 60 s, so in an active
///      group the newest journal row (the one previewed) is frequently one.
///
/// ## What is real here and what is stubbed
///
/// `nseIsServiceEnvelope` / `nseLooksLikeCallSignaling` /
/// `nseLooksLikePushRoutingSecret` / `nseLooksLikeCommitterBeacon` are the
/// PRODUCTION functions (the pure NSE core is compiled into this test target).
/// Only the authoritative Rust codecs are stubbed — the test target links no
/// `LakkoCoreExtension.xcframework` — by ``decodesAsPushRoutingSecret(_:)`` and
/// ``decodesAsCommitterBeacon(_:)`` below, which re-implement the exact gates
/// `lakko_contacts::push_routing::decode_push_routing_secret` and
/// `lakko_contacts::committer_beacon::decode_committer_beacon` apply (domain
/// prefix, then length / version). Those codecs themselves are covered by
/// `crates/lakko-ffi-extension`'s Rust tests, including one golden per envelope
/// kind pinning the constants each pre-filter keys on — `(70, 0x6c)` for the
/// routing peek and `(27, 0x6c)` for the beacon peek, the latter also pinning
/// the exported `committerBeaconEnvelopeLen()` the NSE injects — and a test
/// pinning that an unfiltered beacon really does render as chat text.
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

    /// The frozen 26-byte ADR-0045 domain separator
    /// `lakko/committer-beacon/v1\0`. Built from ASCII + an explicit trailing
    /// NUL for the same reason as ``routingDomain``: no raw NUL in this source.
    private static let beaconDomain: Data = {
        var d = Data("lakko/committer-beacon/v1".utf8)
        d.append(0x00)
        return d
    }()

    /// Stand-in for the slim FFI's `committerBeaconEnvelopeLen()`, which the
    /// NSE passes in production. This target links no
    /// `LakkoCoreExtension.xcframework`, so the value is restated here for the
    /// same reason the codecs below are stubbed — and, like them, it is pinned
    /// on the Rust side: `golden_pins_the_swift_prefilter_shape` in
    /// `crates/lakko-ffi-extension` asserts the exported length equals the
    /// composed wire, so a revision fails Rust CI. ``testBeaconFixtureMatchesTheAdvertisedLength``
    /// keeps this fixture honest against the beacon this file builds.
    private static let beaconEnvelopeLen = 27

    /// A well-formed committer beacon: `domain(26) || version(1)=0x01`, 27
    /// bytes. FIELD-LESS, so every beacon on the wire is byte-identical — the
    /// property that lets the codec be a total test.
    private func committerBeacon(version: UInt8 = 0x01) -> Data {
        var out = Self.beaconDomain
        out.append(version)
        return out
    }

    /// Test double for the slim FFI's `decodeCommitterBeacon`, applying the
    /// same gate `lakko_contacts::committer_beacon::decode_committer_beacon`
    /// does — domain prefix, THEN exact payload length, THEN version. Returning
    /// `true` models the Rust `Ok(())`: the beacon carries nothing to hand back.
    private func decodesAsCommitterBeacon(_ bytes: Data) -> Bool {
        let domain = Self.beaconDomain
        guard bytes.count > domain.count, bytes.prefix(domain.count) == domain else { return false }
        let payload = bytes.dropFirst(domain.count)
        guard payload.count == 1 else { return false }   // exact-length gate
        return payload.first == 0x01                     // schema version v1
    }

    // MARK: - Kind 1: push-routing-secret

    /// WITHOUT THE FIX this returns false and the envelope reaches
    /// `parseBodyEnvelope`, which renders the domain string + binary secret.
    func testPushRoutingSecretEnvelopeIsAServiceEnvelope() {
        XCTAssertTrue(
            nseIsServiceEnvelope(
                routingEnvelope(),
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen
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
            nseIsServiceEnvelope(
                decoy,
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen
            ),
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
            nseIsServiceEnvelope(
                v1,
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen
            )
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
                    decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                    decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                    committerBeaconEnvelopeLen: Self.beaconEnvelopeLen
                ),
                "call signaling must never be rendered"
            )
        }
    }

    // MARK: - Kind 3: the ADR-0045 committer-liveness beacon

    /// WITHOUT THE FIX this returns false and the beacon reaches
    /// `parseBodyEnvelope`, whose lossy-UTF-8 fallback renders the literal text
    /// `lakko/committer-beacon/v1` plus two control bytes.
    ///
    /// This kind is MORE reachable than the routing secret, not less: a routing
    /// secret is broadcast on a membership change, a beacon every 60 s from
    /// every owner and admin.
    func testCommitterBeaconIsAServiceEnvelope() {
        XCTAssertEqual(committerBeacon().count, 27, "domain(26) || version(1)")
        XCTAssertTrue(
            nseIsServiceEnvelope(
                committerBeacon(),
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen
            ),
            "a committer beacon must never be rendered"
        )
    }

    /// The fixture length must be the length this file actually BUILDS, or
    /// every exact-length assertion below would be self-consistent nonsense.
    /// The corresponding Rust golden pins the exported value against the
    /// composed Rust wire; between them the Swift peek and the codec cannot
    /// disagree about the size of a beacon.
    func testBeaconFixtureMatchesTheAdvertisedLength() {
        XCTAssertEqual(committerBeacon().count, Self.beaconEnvelopeLen)
        XCTAssertEqual(Self.beaconDomain.count, 26, "domain(26) || version(1)")
    }

    /// The beacon peek is the exact envelope length PLUS the leading domain
    /// byte — the same two-part shape as the routing peek, with the length
    /// injected from the slim FFI's `committerBeaconEnvelopeLen()` rather than
    /// hand-copied. Pinned here so narrowing or widening it is a visible change.
    func testBeaconPreFilterIsLengthAndFirstByte() {
        XCTAssertTrue(
            nseLooksLikeCommitterBeacon(committerBeacon(), envelopeLen: Self.beaconEnvelopeLen)
        )
        XCTAssertFalse(
            nseLooksLikeCommitterBeacon(chatEnvelope, envelopeLen: Self.beaconEnvelopeLen),
            "a JSON body starts with `{`"
        )
        XCTAssertFalse(
            nseLooksLikeCommitterBeacon(Data(), envelopeLen: Self.beaconEnvelopeLen),
            "empty"
        )
    }

    /// The LENGTH half must discriminate: bytes with the right leading `l` but
    /// the wrong size do not clear the peek. This is the assertion the
    /// first-byte-only peek could not make, and it is why the FFI export exists
    /// — a length is only safe to test when it is sourced, not guessed.
    func testBeaconPreFilterRejectsAWrongLength() {
        XCTAssertFalse(
            nseLooksLikeCommitterBeacon(Data([0x6C]), envelopeLen: Self.beaconEnvelopeLen),
            "one leading `l` is not a beacon"
        )
        XCTAssertFalse(
            nseLooksLikeCommitterBeacon(
                committerBeacon().dropLast(),
                envelopeLen: Self.beaconEnvelopeLen
            ),
            "one byte short"
        )
        XCTAssertFalse(
            nseLooksLikeCommitterBeacon(
                committerBeacon() + Data([0x00]),
                envelopeLen: Self.beaconEnvelopeLen
            ),
            "one byte long"
        )
    }

    /// A beacon is FIELD-LESS, so the exact-length peek can never be too narrow
    /// for a legitimate one: every beacon on the wire is byte-identical, and
    /// each of them clears it. This is the property that makes an exact length
    /// safe here — the reasoning the first-byte-only peek had to avoid.
    func testEveryLegitimateBeaconClearsTheLengthPeek() {
        XCTAssertEqual(committerBeacon(), committerBeacon(), "field-less ⇒ byte-identical")
        XCTAssertTrue(
            nseLooksLikeCommitterBeacon(committerBeacon(), envelopeLen: Self.beaconEnvelopeLen)
        )
        // Even a wrong-VERSION beacon is the right length, so it reaches the
        // codec — which is what refuses it (see the fail-closed test below).
        XCTAssertTrue(
            nseLooksLikeCommitterBeacon(
                committerBeacon(version: 0xFF),
                envelopeLen: Self.beaconEnvelopeLen
            ),
            "the version gate belongs to the codec, not the peek"
        )
    }

    /// The peek alone must NOT suppress: a plaintext that is the right length
    /// and begins with `l` clears it, and the codec is what decides. Without
    /// this the filter would silently swallow real messages.
    func testBeaconPeekAloneDoesNotSuppress() {
        // 27 bytes, leading `l` — clears the tightened peek exactly.
        let realMessage = Data("lunch at six? bring a map".utf8) + Data([0x21, 0x21])
        XCTAssertEqual(realMessage.count, Self.beaconEnvelopeLen)
        XCTAssertTrue(
            nseLooksLikeCommitterBeacon(realMessage, envelopeLen: Self.beaconEnvelopeLen),
            "it does clear the cheap peek"
        )
        XCTAssertFalse(
            nseIsServiceEnvelope(
                realMessage,
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen
            ),
            "the codec, not the peek, is the authority"
        )
    }

    /// An unknown schema version is refused by the codec, so the bytes are NOT
    /// suppressed — the same fail-closed direction the routing v1 case takes.
    func testBeaconWithUnknownVersionIsNotSuppressed() {
        XCTAssertFalse(
            nseIsServiceEnvelope(
                committerBeacon(version: 0xFF),
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen
            )
        )
    }

    /// Cross-kind isolation: neither codec may accept the other's bytes, or the
    /// filter chain would classify one kind as the other.
    func testBeaconAndRoutingCodecsDoNotCrossAccept() {
        XCTAssertFalse(decodesAsCommitterBeacon(routingEnvelope()))
        XCTAssertFalse(decodesAsPushRoutingSecret(committerBeacon()))
    }

    // MARK: - A real chat message is never suppressed

    func testChatEnvelopeIsNotAServiceEnvelope() {
        XCTAssertFalse(nseLooksLikeCallSignaling(chatEnvelope))
        XCTAssertFalse(
            nseIsServiceEnvelope(
                chatEnvelope,
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen
            ),
            "a chat envelope must still render"
        )
        XCTAssertFalse(
            nseIsServiceEnvelope(
                Data(),
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen
            ),
            "empty bytes are not a service envelope"
        )
    }

    // MARK: - Pipeline: the filter drops the row and the loop falls back

    /// A throwaway group mirroring the production `NseRealThrowawayGroup
    /// .process` shape: run the service filter FIRST, then classify. The MLS
    /// decrypt + the Rust codec are the only stubbed parts.
    private final class FilteringFakeGroup: NseThrowawayGroup {
        private let decodesAsPushRoutingSecret: (Data) -> Bool
        private let decodesAsCommitterBeacon: (Data) -> Bool
        private let text: (Data) -> String
        private(set) var suppressed = 0
        var closed = false

        init(
            decodesAsPushRoutingSecret: @escaping (Data) -> Bool,
            decodesAsCommitterBeacon: @escaping (Data) -> Bool,
            text: @escaping (Data) -> String
        ) {
            self.decodesAsPushRoutingSecret = decodesAsPushRoutingSecret
            self.decodesAsCommitterBeacon = decodesAsCommitterBeacon
            self.text = text
        }

        func process(_ payload: Data) -> NseProcessedRow? {
            if nseIsServiceEnvelope(
                payload,
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: NseServiceEnvelopeTests.beaconEnvelopeLen
            ) {
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
            decodesAsCommitterBeacon: decodesAsCommitterBeacon,
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

    /// A beacon as the NEWEST row — the common case in an active group, since
    /// every owner and admin emits one a minute. The real chat before it must
    /// survive. Without the fix the beacon wins (newest-wins) and the body is
    /// `lakko/committer-beacon/v1` plus two control bytes.
    func testCommitterBeaconAfterChatFallsBackToTheChat() async {
        let (result, suppressed) = await preview(rows: [
            NseFetchedRow(id: 10, payload: Data("real message".utf8)),
            NseFetchedRow(id: 11, payload: committerBeacon()),
        ])
        XCTAssertEqual(result?.body, "real message")
        XCTAssertEqual(suppressed, 1)
    }

    /// A beacon BEFORE a real chat must not disturb it either.
    func testCommitterBeaconBeforeChatKeepsTheChat() async {
        let (result, suppressed) = await preview(rows: [
            NseFetchedRow(id: 10, payload: committerBeacon()),
            NseFetchedRow(id: 11, payload: Data("real message".utf8)),
        ])
        XCTAssertEqual(result?.body, "real message")
        XCTAssertEqual(suppressed, 1)
    }

    /// A beacon burst around a real chat — three admins beaconing while one
    /// person types, which is an ordinary minute on a busy group.
    func testBeaconBurstAroundAChatKeepsTheChat() async {
        let (result, suppressed) = await preview(rows: [
            NseFetchedRow(id: 10, payload: committerBeacon()),
            NseFetchedRow(id: 11, payload: Data("real message".utf8)),
            NseFetchedRow(id: 12, payload: committerBeacon()),
            NseFetchedRow(id: 13, payload: committerBeacon()),
        ])
        XCTAssertEqual(result?.body, "real message")
        XCTAssertEqual(suppressed, 3)
    }

    /// A batch of ONLY beacons yields no preview, so the handler degrades to
    /// its generic banner rather than a garbage body.
    func testOnlyBeaconsYieldNoPreview() async {
        let (result, suppressed) = await preview(rows: [
            NseFetchedRow(id: 10, payload: committerBeacon()),
            NseFetchedRow(id: 11, payload: committerBeacon()),
        ])
        XCTAssertNil(result, "no real message → generic banner")
        XCTAssertEqual(suppressed, 2)
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
