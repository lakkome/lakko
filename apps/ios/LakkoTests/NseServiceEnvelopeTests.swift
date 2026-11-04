// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
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

    /// The frozen 17-byte ADR-0048 domain separator `lakko/profile/v1\0`,
    /// built from ASCII + an explicit NUL (no raw NUL in this source).
    private static let profileDomain: Data = {
        var d = Data("lakko/profile/v1".utf8)
        d.append(0x00)
        return d
    }()

    /// Stand-in for the slim FFI's `profileEnvelopeMinLen()...profileEnvelopeMaxLen()`
    /// (32...926 — profile v4 (ADR-0084) with ADR-0078's 134-byte reach grants:
    /// domain 17 + version 1 + stamp 8 + six length / count bytes at the least;
    /// at most a 255-byte name, a 32-byte username, a 16-byte account id, a
    /// 153-byte username certificate, two grants and a 170-byte avatar). Restated for the same reason as
    /// ``beaconEnvelopeLen``; the Rust golden in
    /// `crates/lakko-ffi-extension/src/profile.rs` pins the exported values,
    /// and ``testProfileFixtureBoundsMatchTheAdvertisedRange`` keeps this
    /// fixture honest against the envelopes this file builds.
    private static let profileEnvelopeLens: ClosedRange<Int> = 32...926

    /// ADR-0084 — the avatar field's lengths: none, a still, a still and a
    /// video with its duration.
    private static let avatarLens: Set<Int> = [0, 84, 170]

    /// The ADR-0078 reach grant: `class(1) || version(4 BE) || seed(32) ||
    /// binding(97)`.
    private static let reachGrantLen = 134

    /// A well-formed profile v4 envelope: `domain(17) || version(1) ||
    /// updated_at_ms(8 BE) || name_len || name || username_len || username ||
    /// account_id_len || account_id || cert_len || cert || grant_count ||
    /// grants || avatar_len || avatar`. `full` fills the account id, the
    /// certificate, two grants and a still-and-video avatar.
    private func profileEnvelope(
        name: String = "", username: String = "", updatedAtMs: UInt64 = 7, full: Bool = false
    ) -> Data {
        var out = Self.profileDomain
        out.append(0x04)
        for shift in stride(from: 56, through: 0, by: -8) {
            out.append(UInt8((updatedAtMs >> UInt64(shift)) & 0xFF))
        }
        let nameBytes = Data(name.utf8)
        out.append(UInt8(nameBytes.count))
        out.append(nameBytes)
        let usernameBytes = Data(username.utf8)
        out.append(UInt8(usernameBytes.count))
        out.append(usernameBytes)
        out.append(full ? 16 : 0)
        if full { out.append(Data(repeating: 7, count: 16)) }
        out.append(full ? 153 : 0)
        if full { out.append(Data(repeating: 9, count: 153)) }
        out.append(full ? 2 : 0)
        if full { out.append(Data(repeating: 3, count: 2 * Self.reachGrantLen)) }
        out.append(full ? 170 : 0)
        if full { out.append(Data(repeating: 5, count: 170)) }
        return out
    }

    /// Test double for the slim FFI's `decodeProfileEnvelope`: domain, version,
    /// then the exact length structure (the field-content rules are the Rust
    /// codec's and are covered there).
    private func decodesAsProfile(_ bytes: Data) -> Bool {
        let domain = Self.profileDomain
        guard bytes.count > domain.count, bytes.prefix(domain.count) == domain else { return false }
        let payload = Array(bytes.dropFirst(domain.count))
        guard payload.count >= 1 + 8 + 6, payload[0] == 0x04 else { return false }
        var at = 9
        func field(_ allowed: (Int) -> Bool) -> Bool {
            guard payload.count > at, allowed(Int(payload[at])) else { return false }
            at += 1 + Int(payload[at])
            return payload.count >= at
        }
        guard field({ $0 <= 255 }), field({ $0 <= 32 }), field({ $0 == 0 || $0 == 16 }),
              field({ $0 == 0 || $0 == 153 }), payload.count > at else { return false }
        let grants = Int(payload[at])
        guard grants <= 2 else { return false }
        at += 1 + grants * Self.reachGrantLen
        guard payload.count > at, Self.avatarLens.contains(Int(payload[at])) else { return false }
        return payload.count == at + 1 + Int(payload[at])
    }

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

    /// The frozen 20-byte ADR-0051 domain separator `lakko/group-name/v1\0`,
    /// built from ASCII + an explicit NUL (no raw NUL in this source).
    private static let groupNameDomain: Data = {
        var d = Data("lakko/group-name/v1".utf8)
        d.append(0x00)
        return d
    }()

    /// Stand-in for the slim FFI's `groupNameEnvelopeMinLen()...groupNameEnvelopeMaxLen()`
    /// (30...285 — domain 20 + version 1 + stamp 8 + one length byte, plus a
    /// 255-byte name at most; the empty "clear" envelope is exactly the
    /// minimum). Restated for the same reason as ``beaconEnvelopeLen``; the
    /// Rust side pins the exported values (`crates/lakko-contacts/src/group_name.rs`
    /// asserts 30 and 30 + 255). ``testGroupNameFixtureBoundsMatchTheAdvertisedRange``
    /// keeps it honest against the envelopes this file builds.
    private static let groupNameEnvelopeLens: ClosedRange<Int> = 30...285

    /// A well-formed group-name envelope:
    /// `domain(20) || version(1) || updated_at_ms(8 BE) || name_len || name`.
    private func groupNameEnvelope(name: String = "Trio", updatedAtMs: UInt64 = 7) -> Data {
        var out = Self.groupNameDomain
        out.append(0x01)
        for shift in stride(from: 56, through: 0, by: -8) {
            out.append(UInt8((updatedAtMs >> UInt64(shift)) & 0xFF))
        }
        let nameBytes = Data(name.utf8)
        out.append(UInt8(nameBytes.count))
        out.append(nameBytes)
        return out
    }

    /// Test double for the slim FFI's `decodeGroupNameEnvelope`: domain,
    /// version, then the exact length structure (the character rules are the
    /// Rust codec's and are covered there).
    private func decodesAsGroupName(_ bytes: Data) -> Bool {
        let domain = Self.groupNameDomain
        guard bytes.count > domain.count, bytes.prefix(domain.count) == domain else { return false }
        let payload = Array(bytes.dropFirst(domain.count))
        guard payload.count >= 1 + 8 + 1, payload[0] == 0x01 else { return false }
        let nameLen = Int(payload[9])
        return payload.count == 10 + nameLen
    }

    /// The frozen 25-byte ADR-0048 profile-request domain separator
    /// `lakko/profile-request/v1\0`, built from ASCII + an explicit trailing
    /// NUL (no raw NUL in this source).
    private static let profileRequestDomain: Data = {
        var d = Data("lakko/profile-request/v1".utf8)
        d.append(0x00)
        return d
    }()

    /// Stand-in for the slim FFI's `profileRequestEnvelopeLen()` (26 — domain
    /// 25 + version 1). Restated for the same reason as ``beaconEnvelopeLen``;
    /// the Rust golden `golden_pins_the_prefilter_shape` in `lakko_contacts`
    /// pins the composed wire, and
    /// ``testProfileRequestFixtureMatchesTheAdvertisedLength`` keeps this
    /// fixture honest against the request this file builds.
    private static let profileRequestEnvelopeLen = 26

    /// A well-formed profile request: `domain(25) || version(1)=0x01`, 26
    /// bytes, field-less like the beacon.
    private func profileRequest(version: UInt8 = 0x01) -> Data {
        var out = Self.profileRequestDomain
        out.append(version)
        return out
    }

    /// Test double for the slim FFI's `decodeProfileRequest`, applying the
    /// same gate `lakko_contacts::profile_request::decode_profile_request`
    /// does — domain prefix, THEN exact payload length, THEN version.
    private func decodesAsProfileRequest(_ bytes: Data) -> Bool {
        let domain = Self.profileRequestDomain
        guard bytes.count > domain.count, bytes.prefix(domain.count) == domain else { return false }
        let payload = bytes.dropFirst(domain.count)
        guard payload.count == 1 else { return false }
        return payload.first == 0x01
    }

    /// The frozen 13-byte ADR-0052 pin domain separator `lakko/pin/v1\0`,
    /// built from ASCII + an explicit trailing NUL (no raw NUL in this source).
    private static let pinDomain: Data = {
        var d = Data("lakko/pin/v1".utf8)
        d.append(0x00)
        return d
    }()

    /// Stand-in for the slim FFI's `pinEnvelopeLen()` (31 — domain 13 +
    /// version 1 + stamp 8 + action 1 + message id 8). Restated for the same
    /// reason as ``beaconEnvelopeLen``; the Rust golden in
    /// `crates/lakko-ffi-extension/src/pin.rs` pins the exported value against
    /// the composed wire, and ``testPinFixtureMatchesTheAdvertisedLength``
    /// keeps this fixture honest against the envelope this file builds.
    private static let pinEnvelopeLen = 31

    /// Stand-in for the slim FFI's `scopedDiscoveryEnvelopeMinLen()...MaxLen()`
    /// (the NSE's pre-filter range for the five ADR-0060 envelopes). Wide on
    /// purpose: the exported bounds are pinned against composed envelopes by
    /// the Rust goldens in `crates/lakko-ffi-extension/src/scoped_discovery.rs`;
    /// this fixture only has to admit the envelopes this file composes.
    static let scopedDiscoveryEnvelopeLens: ClosedRange<Int> = 16...1_000_000

    /// The ADR-0060 verdict, through the FULL core's dispatcher (the test target
    /// links `LakkoCore`, not the slim NSE framework): one of the five "who can
    /// find me" kinds.
    static func decodesAsScopedDiscoveryReal(_ bytes: Data) -> Bool {
        guard let env = try? dispatchIncomingEnvelope(plaintext: bytes) else { return false }
        switch env.kind {
        case .introductionOffer, .introductions, .visibility, .reachState, .committerDelegation:
            return true
        default:
            return false
        }
    }

    private func decodesAsScopedDiscovery(_ bytes: Data) -> Bool {
        Self.decodesAsScopedDiscoveryReal(bytes)
    }

    /// A well-formed pin envelope:
    /// `domain(13) || version(1)=0x01 || updated_at_ms(8, BE) || action(1) || message_id(8, BE)`.
    private func pinEnvelope(
        version: UInt8 = 0x01, updatedAtMs: UInt64 = 1_700_000_000_000, action: UInt8 = 0x01,
        messageId: Int64 = 41
    ) -> Data {
        var out = Self.pinDomain
        out.append(version)
        for shift in stride(from: 56, through: 0, by: -8) {
            out.append(UInt8((updatedAtMs >> UInt64(shift)) & 0xFF))
        }
        out.append(action)
        let id = UInt64(bitPattern: messageId)
        for shift in stride(from: 56, through: 0, by: -8) {
            out.append(UInt8((id >> UInt64(shift)) & 0xFF))
        }
        return out
    }

    /// Test double for the slim FFI's `decodePinEnvelope`, applying the same
    /// gate `lakko_contacts::pin::decode_pin` does — domain prefix, THEN
    /// version, THEN exact payload length, THEN a known action byte.
    private func decodesAsPin(_ bytes: Data) -> Bool {
        let domain = Self.pinDomain
        guard bytes.count > domain.count, bytes.prefix(domain.count) == domain else { return false }
        let payload = Array(bytes.dropFirst(domain.count))
        guard payload.first == 0x01 else { return false }
        guard payload.count == 18 else { return false }
        return payload[9] == 0x01 || payload[9] == 0x02
    }

    /// The frozen 27-byte ADR-0057 registrar-seed domain
    /// `lakko/ds-registrar-seed/v1\0`, ASCII + an explicit trailing NUL.
    fileprivate static let registrarSeedDomain: Data = {
        var d = Data("lakko/ds-registrar-seed/v1".utf8)
        d.append(0x00)
        return d
    }()

    /// Stand-in for the slim FFI's `registrarSeedEnvelopeLen()` (205 — domain
    /// 27 + version 1 + journal 32 + addressee 32 + rank 1 + registrar public
    /// key 32 + sealed 80; v2, ADR-0057 amended 2026-10-01).
    fileprivate static let registrarSeedEnvelopeLen = 205

    /// A well-formed registrar-seed envelope (the sealed part is opaque here).
    private func registrarSeedEnvelope(version: UInt8 = 0x02, rank: UInt8 = 0x01) -> Data {
        var out = Self.registrarSeedDomain
        out.append(version)
        out.append(Data(repeating: 0x11, count: 32))
        out.append(Data(repeating: 0x22, count: 32))
        out.append(rank)
        out.append(Data(repeating: 0x44, count: 32))
        out.append(Data(repeating: 0x33, count: 80))
        return out
    }

    /// Test double for the slim FFI's `decodeRegistrarSeedEnvelope`: domain,
    /// version, exact length, known rank.
    private func decodesAsRegistrarSeed(_ bytes: Data) -> Bool {
        Self.decodesAsRegistrarSeedDouble(bytes)
    }

    fileprivate static func decodesAsRegistrarSeedDouble(_ bytes: Data) -> Bool {
        let domain = Self.registrarSeedDomain
        guard bytes.count == Self.registrarSeedEnvelopeLen,
              bytes.prefix(domain.count) == domain else { return false }
        let payload = Array(bytes.dropFirst(domain.count))
        return payload[0] == 0x02 && (payload[65] == 0x01 || payload[65] == 0x02)
    }

    // MARK: - Kind 9: group closed (ADR-0073 phase 2, UX 16.5)

    /// The frozen 22-byte domain separator `lakko/group-closed/v1\0`.
    private static let groupClosedDomain: Data = {
        var d = Data("lakko/group-closed/v1".utf8)
        d.append(0x00)
        return d
    }()

    /// Stand-in for the slim FFI's `groupClosedEnvelopeMinLen()...MaxLen()`:
    /// 32 without a successor, 64 with one.
    private static let groupClosedEnvelopeLens: ClosedRange<Int> = 32...64

    /// A well-formed group-closed envelope:
    /// `domain(22) || version(1) || closed_at_ms(8 BE) || successor_len || successor`.
    private func groupClosedEnvelope(successor: Bool, version: UInt8 = 0x01) -> Data {
        var out = Self.groupClosedDomain
        out.append(version)
        out.append(contentsOf: [UInt8](repeating: 0, count: 7))
        out.append(9)
        if successor {
            out.append(32)
            out.append(Data(repeating: 0xAB, count: 32))
        } else {
            out.append(0)
        }
        return out
    }

    /// Re-implements the gate `lakko_contacts::group_closed::decode_group_closed`
    /// applies: domain, version, then exactly 32 or 64 bytes with a matching
    /// successor length.
    private func decodesAsGroupClosed(_ bytes: Data) -> Bool {
        let domain = Self.groupClosedDomain
        guard bytes.count > domain.count, bytes.prefix(domain.count) == domain else { return false }
        let payload = Array(bytes.dropFirst(domain.count))
        guard payload.count >= 10, payload[0] == 0x01 else { return false }
        let successorLen = Int(payload[9])
        return (successorLen == 0 || successorLen == 32) && payload.count == 10 + successorLen
    }

    /// WITHOUT THE ARM a closed notice reaches `parseBodyEnvelope` and the
    /// notification body reads `lakko/group-closed/v1` plus binary.
    func testGroupClosedEnvelopeIsAServiceEnvelope() {
        for envelope in [groupClosedEnvelope(successor: false), groupClosedEnvelope(successor: true)] {
            XCTAssertTrue(Self.groupClosedEnvelopeLens.contains(envelope.count))
            XCTAssertEqual(envelope.first, 0x6C)
            XCTAssertTrue(
                nseIsServiceEnvelope(
                    envelope,
                    decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                    decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                    committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                    decodesAsProfile: decodesAsProfile,
                    profileEnvelopeLens: Self.profileEnvelopeLens,
                    decodesAsGroupName: decodesAsGroupName,
                    groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                    decodesAsProfileRequest: decodesAsProfileRequest,
                    profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                    decodesAsPin: decodesAsPin,
                    pinEnvelopeLen: Self.pinEnvelopeLen,
                    decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                    registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                    decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                    scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                    decodesAsSharedChannels: { _ in false },
                    sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens,
                    decodesAsGroupClosed: decodesAsGroupClosed,
                    groupClosedEnvelopeLens: Self.groupClosedEnvelopeLens
                ),
                "a closed notice must never be rendered"
            )
        }
        // A wrong version, or a byte short, is not the envelope; and the arm
        // left at its default lets the bytes through, which is the defect the
        // injected decoder closes.
        for wrong in [groupClosedEnvelope(successor: false, version: 0x02),
                      Data(groupClosedEnvelope(successor: true).dropLast())] {
            XCTAssertFalse(decodesAsGroupClosed(wrong))
        }
        XCTAssertFalse(
            nseIsServiceEnvelope(
                groupClosedEnvelope(successor: false),
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                decodesAsProfile: decodesAsProfile,
                profileEnvelopeLens: Self.profileEnvelopeLens,
                decodesAsGroupName: decodesAsGroupName,
                groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                decodesAsProfileRequest: decodesAsProfileRequest,
                profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                decodesAsPin: decodesAsPin,
                pinEnvelopeLen: Self.pinEnvelopeLen,
                decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                decodesAsSharedChannels: { _ in false },
                sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
            ),
            "without the injected decoder (the default) no other arm recognises it, which is why the production site injects it"
        )
    }

    // MARK: - Kind 8: registrar seed (ADR-0057)

    /// WITHOUT THE ARM a registrar seed reaches `parseBodyEnvelope` and the
    /// notification body reads `lakko/ds-registrar-seed/v1` plus binary.
    func testRegistrarSeedEnvelopeIsAServiceEnvelope() {
        XCTAssertEqual(registrarSeedEnvelope().count, Self.registrarSeedEnvelopeLen)
        XCTAssertEqual(registrarSeedEnvelope().first, 0x6C)
        XCTAssertTrue(
            nseIsServiceEnvelope(
                registrarSeedEnvelope(),
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                decodesAsProfile: decodesAsProfile,
                profileEnvelopeLens: Self.profileEnvelopeLens,
                decodesAsGroupName: decodesAsGroupName,
                groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                decodesAsProfileRequest: decodesAsProfileRequest,
                profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                decodesAsPin: decodesAsPin,
                pinEnvelopeLen: Self.pinEnvelopeLen,
                decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                decodesAsSharedChannels: { _ in false },
                sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
            ),
            "a registrar seed must never be rendered"
        )
        // One byte short, or an unknown version (the retired v1 included), is
        // not the envelope.
        for wrong in [registrarSeedEnvelope().dropLast(), registrarSeedEnvelope(version: 0x01),
                      registrarSeedEnvelope(version: 0x03)] {
            XCTAssertFalse(
                nseIsServiceEnvelope(
                    Data(wrong),
                    decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                    decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                    committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                    decodesAsProfile: decodesAsProfile,
                    profileEnvelopeLens: Self.profileEnvelopeLens,
                    decodesAsGroupName: decodesAsGroupName,
                    groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                    decodesAsProfileRequest: decodesAsProfileRequest,
                    profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                    decodesAsPin: decodesAsPin,
                    pinEnvelopeLen: Self.pinEnvelopeLen,
                    decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                    registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                    decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                    scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                    decodesAsSharedChannels: { _ in false },
                    sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
                ))
        }
    }

    // MARK: - Kind 9: shared channels (ADR-0059 section 6)

    /// `lakko/shared-channels/v1` plus its trailing NUL (no raw NUL here).
    private static let sharedChannelsDomain: Data = {
        var d = Data("lakko/shared-channels/v1".utf8)
        d.append(0x00)
        return d
    }()

    /// Stand-in for the slim FFI's `sharedChannelsEnvelopeMinLen()...MaxLen()`
    /// (35...9795 — domain 25 + version 1 + stamp 8 + count 1, plus at most 32
    /// entries of 16 + 1 + 32 + 1 + 255 bytes). The Rust test in
    /// `crates/lakko-contacts/src/shared_channels.rs` pins both values.
    static let sharedChannelsEnvelopeLens: ClosedRange<Int> = 35...9795

    /// A well-formed list:
    /// `domain(25) || version(1) || updated_at_ms(8 BE) || count(1) || entries`.
    private func sharedChannelsEnvelope(channels: [(String, String)] = [("books", "Books")]) -> Data {
        var out = Self.sharedChannelsDomain
        out.append(0x01)
        let stamp: UInt64 = 1_700_000_000_000
        for shift in stride(from: 56, through: 0, by: -8) {
            out.append(UInt8((stamp >> UInt64(shift)) & 0xFF))
        }
        out.append(UInt8(channels.count))
        for (i, (handle, name)) in channels.enumerated() {
            out.append(Data(repeating: UInt8(i + 1), count: 16))
            out.append(UInt8(handle.utf8.count))
            out.append(Data(handle.utf8))
            out.append(UInt8(name.utf8.count))
            out.append(Data(name.utf8))
        }
        return out
    }

    /// Test double for the NSE's shared-channels decoder: the domain prefix
    /// (a domain match is suppressed whatever its bounds, the web rule).
    private func decodesAsSharedChannels(_ bytes: Data) -> Bool {
        bytes.starts(with: Self.sharedChannelsDomain)
    }

    /// WITHOUT THE ARM a list reaches `parseBodyEnvelope` and the notification
    /// body reads `lakko/shared-channels/v1` plus the channel names — for a
    /// list and for the empty list a member sends when the switch goes off.
    func testSharedChannelsListsAreServiceEnvelopes() {
        let empty = sharedChannelsEnvelope(channels: [])
        XCTAssertEqual(empty.count, Self.sharedChannelsEnvelopeLens.lowerBound)
        for envelope in [empty, sharedChannelsEnvelope(), sharedChannelsEnvelope(channels: [("a_b", "A"), ("xyz", "Z")])] {
            XCTAssertTrue(nseLooksLikeSharedChannels(envelope, envelopeLens: Self.sharedChannelsEnvelopeLens))
            XCTAssertTrue(isService(envelope), "a shared-channels list must never be rendered")
        }
    }

    /// The peek rejects a chat envelope, empty bytes and a too-short `l` run,
    /// and a plain chat is not a service envelope.
    func testSharedChannelsPreFilter() {
        XCTAssertFalse(nseLooksLikeSharedChannels(chatEnvelope, envelopeLens: Self.sharedChannelsEnvelopeLens))
        XCTAssertFalse(nseLooksLikeSharedChannels(Data(), envelopeLens: Self.sharedChannelsEnvelopeLens))
        XCTAssertFalse(
            nseLooksLikeSharedChannels(Data(repeating: 0x6C, count: 34), envelopeLens: Self.sharedChannelsEnvelopeLens))
        XCTAssertFalse(isService(Data("lakko is great, see you at 5 o'clock sharp".utf8)))
    }

    // MARK: - Kind 7: shared pin (ADR-0052 decision 3)

    /// WITHOUT THE ARM a pin reaches `parseBodyEnvelope` and the notification
    /// body reads `lakko/pin/v1` plus binary — for a pin AND for an unpin.
    func testPinAndUnpinEnvelopesAreServiceEnvelopes() {
        for envelope in [pinEnvelope(), pinEnvelope(action: 0x02), pinEnvelope(messageId: -1)] {
            XCTAssertTrue(
                nseIsServiceEnvelope(
                    envelope,
                    decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                    decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                    committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                    decodesAsProfile: decodesAsProfile,
                    profileEnvelopeLens: Self.profileEnvelopeLens,
                    decodesAsGroupName: decodesAsGroupName,
                    groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                    decodesAsProfileRequest: decodesAsProfileRequest,
                    profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                    decodesAsPin: decodesAsPin,
                    pinEnvelopeLen: Self.pinEnvelopeLen,
                    decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                    registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                    decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                    scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                    decodesAsSharedChannels: { _ in false },
                    sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
                ),
                "a pin envelope must never be rendered"
            )
        }
    }

    func testPinFixtureMatchesTheAdvertisedLength() {
        XCTAssertEqual(pinEnvelope().count, Self.pinEnvelopeLen)
    }

    /// The peek accepts the real envelope and rejects the near-misses — a
    /// beacon, a chat envelope, empty bytes.
    func testPinPreFilter() {
        XCTAssertTrue(nseLooksLikePin(pinEnvelope(), envelopeLen: Self.pinEnvelopeLen))
        XCTAssertFalse(nseLooksLikePin(committerBeacon(), envelopeLen: Self.pinEnvelopeLen))
        XCTAssertFalse(nseLooksLikePin(chatEnvelope, envelopeLen: Self.pinEnvelopeLen))
        XCTAssertFalse(nseLooksLikePin(Data(), envelopeLen: Self.pinEnvelopeLen))
    }

    /// Bytes that PASS the peek but fail the codec are NOT suppressed BY THIS
    /// ARM: a 31-byte legacy plaintext starting with `l`, a pin of an unknown
    /// version, and one with an unknown action all still render. A 31-byte
    /// group-name envelope (same first byte, same length) also clears the pin
    /// peek and is refused by the pin codec — it is the GROUP-NAME arm that
    /// suppresses it, which is why it is asserted on the codec and not on the
    /// whole filter.
    func testPinPeekAloneDoesNotSuppress() {
        let oneByteName = groupNameEnvelope(name: "x")
        XCTAssertEqual(oneByteName.count, Self.pinEnvelopeLen, "the collision this test exists for")
        XCTAssertTrue(nseLooksLikePin(oneByteName, envelopeLen: Self.pinEnvelopeLen))
        XCTAssertFalse(decodesAsPin(oneByteName), "the pin codec refuses a group name")
        for decoy in [Data(repeating: 0x6C, count: 31), pinEnvelope(version: 0x02), pinEnvelope(action: 0x03)] {
            XCTAssertTrue(nseLooksLikePin(decoy, envelopeLen: Self.pinEnvelopeLen))
            XCTAssertFalse(
                nseIsServiceEnvelope(
                    decoy,
                    decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                    decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                    committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                    decodesAsProfile: decodesAsProfile,
                    profileEnvelopeLens: Self.profileEnvelopeLens,
                    decodesAsGroupName: decodesAsGroupName,
                    groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                    decodesAsProfileRequest: decodesAsProfileRequest,
                    profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                    decodesAsPin: decodesAsPin,
                    pinEnvelopeLen: Self.pinEnvelopeLen,
                    decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                    registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                    decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                    scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                    decodesAsSharedChannels: { _ in false },
                    sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
                ))
        }
    }

    /// A pin landing as the newest row falls back to the real chat before it.
    func testPinAfterAChatFallsBackToTheChat() async {
        let (result, suppressed) = await preview(rows: [
            NseFetchedRow(id: 10, payload: Data("real message".utf8)),
            NseFetchedRow(id: 11, payload: pinEnvelope()),
            NseFetchedRow(id: 12, payload: pinEnvelope(action: 0x02, messageId: 7)),
        ])
        XCTAssertEqual(result?.body, "real message")
        XCTAssertEqual(suppressed, 2)
    }

    // MARK: - Kind 6: profile request (ADR-0048 §2 trigger 4)

    /// WITHOUT THE ARM a newcomer's "introduce yourselves" reaches
    /// `parseBodyEnvelope` and the notification body reads
    /// `lakko/profile-request/v1` plus a control byte.
    func testProfileRequestIsAServiceEnvelope() {
        XCTAssertTrue(
            nseIsServiceEnvelope(
                profileRequest(),
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                decodesAsProfile: decodesAsProfile,
                profileEnvelopeLens: Self.profileEnvelopeLens,
                decodesAsGroupName: decodesAsGroupName,
                groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                decodesAsProfileRequest: decodesAsProfileRequest,
                profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                decodesAsPin: decodesAsPin,
                pinEnvelopeLen: Self.pinEnvelopeLen,
                decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                decodesAsSharedChannels: { _ in false },
                sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
            ),
            "a profile request must never be rendered"
        )
    }

    func testProfileRequestFixtureMatchesTheAdvertisedLength() {
        XCTAssertEqual(profileRequest().count, Self.profileRequestEnvelopeLen)
    }

    /// The peek accepts the real request and rejects the near-misses — a beacon
    /// (one byte longer), a chat envelope, empty bytes.
    func testProfileRequestPreFilter() {
        XCTAssertTrue(
            nseLooksLikeProfileRequest(profileRequest(), envelopeLen: Self.profileRequestEnvelopeLen))
        XCTAssertFalse(
            nseLooksLikeProfileRequest(committerBeacon(), envelopeLen: Self.profileRequestEnvelopeLen),
            "the beacon is 27 bytes; the request is 26")
        XCTAssertFalse(nseLooksLikeProfileRequest(chatEnvelope, envelopeLen: Self.profileRequestEnvelopeLen))
        XCTAssertFalse(nseLooksLikeProfileRequest(Data(), envelopeLen: Self.profileRequestEnvelopeLen))
    }

    /// Bytes that PASS the peek but fail the codec are NOT suppressed: a
    /// 26-byte legacy plaintext starting with `l`, and a request of a schema
    /// version this build does not know, both still render.
    func testProfileRequestPeekAloneDoesNotSuppress() {
        for decoy in [Data(repeating: 0x6C, count: 26), profileRequest(version: 0x02)] {
            XCTAssertTrue(nseLooksLikeProfileRequest(decoy, envelopeLen: Self.profileRequestEnvelopeLen))
            XCTAssertFalse(
                nseIsServiceEnvelope(
                    decoy,
                    decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                    decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                    committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                    decodesAsProfile: decodesAsProfile,
                    profileEnvelopeLens: Self.profileEnvelopeLens,
                    decodesAsGroupName: decodesAsGroupName,
                    groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                    decodesAsProfileRequest: decodesAsProfileRequest,
                    profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                    decodesAsPin: decodesAsPin,
                    pinEnvelopeLen: Self.pinEnvelopeLen,
                    decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                    registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                    decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                    scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                    decodesAsSharedChannels: { _ in false },
                    sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
                ))
        }
    }

    /// A request landing as the newest row falls back to the real chat before
    /// it; only requests yields the generic banner.
    func testRequestAfterAChatFallsBackToTheChat() async {
        let (result, suppressed) = await preview(rows: [
            NseFetchedRow(id: 10, payload: Data("real message".utf8)),
            NseFetchedRow(id: 11, payload: profileRequest()),
        ])
        XCTAssertEqual(result?.body, "real message")
        XCTAssertEqual(suppressed, 1)
        let (none, onlyRequests) = await preview(rows: [
            NseFetchedRow(id: 12, payload: profileRequest()),
        ])
        XCTAssertNil(none)
        XCTAssertEqual(onlyRequests, 1)
    }

    // MARK: - Kind 5: group name (ADR-0051)

    /// WITHOUT THE ARM a rename reaches `parseBodyEnvelope` and the
    /// notification body reads `lakko/group-name/v1` plus binary.
    func testGroupNameEnvelopeIsAServiceEnvelope() {
        for envelope in [
            groupNameEnvelope(),
            groupNameEnvelope(name: ""),                                  // the "clear" envelope
            groupNameEnvelope(name: String(repeating: "n", count: 255)), // the longest name
        ] {
            XCTAssertTrue(
                nseIsServiceEnvelope(
                    envelope,
                    decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                    decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                    committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                    decodesAsProfile: decodesAsProfile,
                    profileEnvelopeLens: Self.profileEnvelopeLens,
                    decodesAsGroupName: decodesAsGroupName,
                    groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                    decodesAsProfileRequest: decodesAsProfileRequest,
                    profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                    decodesAsPin: decodesAsPin,
                    pinEnvelopeLen: Self.pinEnvelopeLen,
                    decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                    registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                    decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                    scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                    decodesAsSharedChannels: { _ in false },
                    sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
                ),
                "a group-name envelope of \(envelope.count) bytes must never be rendered"
            )
        }
    }

    /// The fixture's own shortest and longest envelopes sit exactly on the
    /// advertised bounds, so the restated range cannot silently drift narrow.
    func testGroupNameFixtureBoundsMatchTheAdvertisedRange() {
        XCTAssertEqual(groupNameEnvelope(name: "").count, Self.groupNameEnvelopeLens.lowerBound)
        XCTAssertEqual(
            groupNameEnvelope(name: String(repeating: "n", count: 255)).count,
            Self.groupNameEnvelopeLens.upperBound)
    }

    /// The peek accepts every real envelope and rejects the near-misses.
    func testGroupNamePreFilter() {
        XCTAssertTrue(nseLooksLikeGroupName(groupNameEnvelope(), envelopeLens: Self.groupNameEnvelopeLens))
        XCTAssertFalse(nseLooksLikeGroupName(chatEnvelope, envelopeLens: Self.groupNameEnvelopeLens))
        XCTAssertFalse(nseLooksLikeGroupName(Data(), envelopeLens: Self.groupNameEnvelopeLens))
        XCTAssertFalse(
            nseLooksLikeGroupName(
                Data(repeating: 0x6C, count: 286), envelopeLens: Self.groupNameEnvelopeLens),
            "one past the longest envelope")
    }

    /// Bytes that PASS the peek but fail the codec are NOT suppressed: a
    /// 40-byte legacy plaintext starting with `l` must still render.
    func testGroupNamePeekAloneDoesNotSuppress() {
        let decoy = Data(repeating: 0x6C, count: 40)
        XCTAssertTrue(nseLooksLikeGroupName(decoy, envelopeLens: Self.groupNameEnvelopeLens))
        XCTAssertFalse(
            nseIsServiceEnvelope(
                decoy,
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                decodesAsProfile: decodesAsProfile,
                profileEnvelopeLens: Self.profileEnvelopeLens,
                decodesAsGroupName: decodesAsGroupName,
                groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                decodesAsProfileRequest: decodesAsProfileRequest,
                profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                decodesAsPin: decodesAsPin,
                pinEnvelopeLen: Self.pinEnvelopeLen,
                decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                decodesAsSharedChannels: { _ in false },
                sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
            ))
    }

    /// A rename landing as the newest row falls back to the real chat before
    /// it; only renames yields the generic banner.
    func testRenameAfterAChatFallsBackToTheChat() async {
        let (result, suppressed) = await preview(rows: [
            NseFetchedRow(id: 10, payload: Data("real message".utf8)),
            NseFetchedRow(id: 11, payload: groupNameEnvelope(name: "Renamed")),
        ])
        XCTAssertEqual(result?.body, "real message")
        XCTAssertEqual(suppressed, 1)
        let (none, onlyRenames) = await preview(rows: [
            NseFetchedRow(id: 12, payload: groupNameEnvelope(name: "A")),
            NseFetchedRow(id: 13, payload: groupNameEnvelope(name: "")),
        ])
        XCTAssertNil(none)
        XCTAssertEqual(onlyRenames, 2)
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
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                decodesAsProfile: decodesAsProfile,
                profileEnvelopeLens: Self.profileEnvelopeLens,
                decodesAsGroupName: decodesAsGroupName,
                groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                decodesAsProfileRequest: decodesAsProfileRequest,
                profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                decodesAsPin: decodesAsPin,
                pinEnvelopeLen: Self.pinEnvelopeLen,
                decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                decodesAsSharedChannels: { _ in false },
                sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
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
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                decodesAsProfile: decodesAsProfile,
                profileEnvelopeLens: Self.profileEnvelopeLens,
                decodesAsGroupName: decodesAsGroupName,
                groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                decodesAsProfileRequest: decodesAsProfileRequest,
                profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                decodesAsPin: decodesAsPin,
                pinEnvelopeLen: Self.pinEnvelopeLen,
                decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                decodesAsSharedChannels: { _ in false },
                sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
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
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                decodesAsProfile: decodesAsProfile,
                profileEnvelopeLens: Self.profileEnvelopeLens,
                decodesAsGroupName: decodesAsGroupName,
                groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                decodesAsProfileRequest: decodesAsProfileRequest,
                profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                decodesAsPin: decodesAsPin,
                pinEnvelopeLen: Self.pinEnvelopeLen,
                decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                decodesAsSharedChannels: { _ in false },
                sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
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
                    committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                decodesAsProfile: decodesAsProfile,
                profileEnvelopeLens: Self.profileEnvelopeLens,
                    decodesAsGroupName: decodesAsGroupName,
                    groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                    decodesAsProfileRequest: decodesAsProfileRequest,
                    profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                    decodesAsPin: decodesAsPin,
                    pinEnvelopeLen: Self.pinEnvelopeLen,
                    decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                    registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                    decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                    scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                    decodesAsSharedChannels: { _ in false },
                    sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
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
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                decodesAsProfile: decodesAsProfile,
                profileEnvelopeLens: Self.profileEnvelopeLens,
                decodesAsGroupName: decodesAsGroupName,
                groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                decodesAsProfileRequest: decodesAsProfileRequest,
                profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                decodesAsPin: decodesAsPin,
                pinEnvelopeLen: Self.pinEnvelopeLen,
                decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                decodesAsSharedChannels: { _ in false },
                sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
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
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                decodesAsProfile: decodesAsProfile,
                profileEnvelopeLens: Self.profileEnvelopeLens,
                decodesAsGroupName: decodesAsGroupName,
                groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                decodesAsProfileRequest: decodesAsProfileRequest,
                profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                decodesAsPin: decodesAsPin,
                pinEnvelopeLen: Self.pinEnvelopeLen,
                decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                decodesAsSharedChannels: { _ in false },
                sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
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
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                decodesAsProfile: decodesAsProfile,
                profileEnvelopeLens: Self.profileEnvelopeLens,
                decodesAsGroupName: decodesAsGroupName,
                groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                decodesAsProfileRequest: decodesAsProfileRequest,
                profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                decodesAsPin: decodesAsPin,
                pinEnvelopeLen: Self.pinEnvelopeLen,
                decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                decodesAsSharedChannels: { _ in false },
                sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
            )
        )
    }

    /// Cross-kind isolation: neither codec may accept the other's bytes, or the
    /// filter chain would classify one kind as the other.
    func testBeaconAndRoutingCodecsDoNotCrossAccept() {
        XCTAssertFalse(decodesAsCommitterBeacon(routingEnvelope()))
        XCTAssertFalse(decodesAsPushRoutingSecret(committerBeacon()))
    }

    // MARK: - Kind 4: the ADR-0048 profile envelope

    /// WITHOUT THE ARM this returns false and the profile reaches
    /// `parseBodyEnvelope`, whose lossy-UTF-8 fallback renders
    /// `lakko/profile/v1`, a timestamp and the person's name as a notification.
    func testProfileEnvelopeIsAServiceEnvelope() {
        for envelope in [
            profileEnvelope(),
            profileEnvelope(name: "Alice Petrova", username: "alice"),
            profileEnvelope(name: String(repeating: "a", count: 255), username: String(repeating: "b", count: 32)),
        ] {
            XCTAssertTrue(
                nseIsServiceEnvelope(
                    envelope,
                    decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                    decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                    committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                    decodesAsProfile: decodesAsProfile,
                    profileEnvelopeLens: Self.profileEnvelopeLens,
                    decodesAsGroupName: decodesAsGroupName,
                    groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                    decodesAsProfileRequest: decodesAsProfileRequest,
                    profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                    decodesAsPin: decodesAsPin,
                    pinEnvelopeLen: Self.pinEnvelopeLen,
                    decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                    registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                    decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                    scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                    decodesAsSharedChannels: { _ in false },
                    sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
                ),
                "a \(envelope.count)-byte profile must never be rendered"
            )
        }
    }

    /// The fixtures at the extremes are exactly the advertised range's ends,
    /// so the range test below is about the real bounds, not invented ones.
    func testProfileFixtureBoundsMatchTheAdvertisedRange() {
        XCTAssertEqual(profileEnvelope().count, Self.profileEnvelopeLens.lowerBound)
        XCTAssertEqual(
            profileEnvelope(
                name: String(repeating: "a", count: 255), username: String(repeating: "b", count: 32),
                full: true).count,
            Self.profileEnvelopeLens.upperBound
        )
    }

    /// The range peek accepts both ends and nothing outside them. An upper
    /// bound that is too low fails dangerously (long profiles stop being
    /// suppressed) — which is why it is injected from the FFI.
    func testProfilePreFilterIsARangeAndTheFirstByte() {
        let lens = Self.profileEnvelopeLens
        XCTAssertTrue(nseLooksLikeProfile(profileEnvelope(), envelopeLens: lens))
        XCTAssertTrue(nseLooksLikeProfile(
            profileEnvelope(
                name: String(repeating: "a", count: 255), username: String(repeating: "b", count: 32),
                full: true),
            envelopeLens: lens))
        XCTAssertFalse(nseLooksLikeProfile(profileEnvelope().dropLast(), envelopeLens: lens), "one byte short")
        XCTAssertFalse(
            nseLooksLikeProfile(Data([0x6C]) + Data(repeating: 0x61, count: lens.upperBound), envelopeLens: lens),
            "one byte past the longest well-formed profile")
        XCTAssertFalse(nseLooksLikeProfile(chatEnvelope, envelopeLens: lens), "a JSON body starts with `{`")
    }

    /// The peek alone must not suppress: a real message of profile length that
    /// starts with `l` clears it, and the codec refuses it.
    func testProfilePeekAloneDoesNotSuppress() {
        let realMessage = Data("let us meet at the station at nine tonight".utf8)
        XCTAssertTrue(nseLooksLikeProfile(realMessage, envelopeLens: Self.profileEnvelopeLens))
        XCTAssertFalse(
            nseIsServiceEnvelope(
                realMessage,
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                decodesAsProfile: decodesAsProfile,
                profileEnvelopeLens: Self.profileEnvelopeLens,
                decodesAsGroupName: decodesAsGroupName,
                groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                decodesAsProfileRequest: decodesAsProfileRequest,
                profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                decodesAsPin: decodesAsPin,
                pinEnvelopeLen: Self.pinEnvelopeLen,
                decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                decodesAsSharedChannels: { _ in false },
                sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
            ),
            "the codec, not the peek, is the authority"
        )
    }

    /// No two codecs accept each other's bytes.
    func testProfileCodecDoesNotCrossAcceptTheOtherKinds() {
        XCTAssertFalse(decodesAsProfile(routingEnvelope()))
        XCTAssertFalse(decodesAsProfile(committerBeacon()))
        XCTAssertFalse(decodesAsPushRoutingSecret(profileEnvelope(name: "x")))
        XCTAssertFalse(decodesAsCommitterBeacon(profileEnvelope()))
    }

    /// A profile as the NEWEST row must fall back to the real chat before it —
    /// on growth every member republishes, so this is an ordinary shape.
    func testProfileAfterChatFallsBackToTheChat() async {
        let (result, suppressed) = await preview(rows: [
            NseFetchedRow(id: 10, payload: Data("real message".utf8)),
            NseFetchedRow(id: 11, payload: profileEnvelope(name: "Alice", username: "alice")),
            NseFetchedRow(id: 12, payload: profileEnvelope(name: "Bob")),
        ])
        XCTAssertEqual(result?.body, "real message")
        XCTAssertEqual(suppressed, 2)
    }

    // MARK: - A real chat message is never suppressed

    func testChatEnvelopeIsNotAServiceEnvelope() {
        XCTAssertFalse(nseLooksLikeCallSignaling(chatEnvelope))
        XCTAssertFalse(
            nseIsServiceEnvelope(
                chatEnvelope,
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                decodesAsProfile: decodesAsProfile,
                profileEnvelopeLens: Self.profileEnvelopeLens,
                decodesAsGroupName: decodesAsGroupName,
                groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                decodesAsProfileRequest: decodesAsProfileRequest,
                profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                decodesAsPin: decodesAsPin,
                pinEnvelopeLen: Self.pinEnvelopeLen,
                decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                decodesAsSharedChannels: { _ in false },
                sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
            ),
            "a chat envelope must still render"
        )
        XCTAssertFalse(
            nseIsServiceEnvelope(
                Data(),
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
                decodesAsProfile: decodesAsProfile,
                profileEnvelopeLens: Self.profileEnvelopeLens,
                decodesAsGroupName: decodesAsGroupName,
                groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
                decodesAsProfileRequest: decodesAsProfileRequest,
                profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
                decodesAsPin: decodesAsPin,
                pinEnvelopeLen: Self.pinEnvelopeLen,
                decodesAsRegistrarSeed: decodesAsRegistrarSeed,
                registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
                decodesAsScopedDiscovery: decodesAsScopedDiscovery,
                scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
                decodesAsSharedChannels: { _ in false },
                sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
            ),
            "empty bytes are not a service envelope"
        )
    }

    // MARK: - Kind 8: the ADR-0060 "who can find me" envelopes

    private func isService(_ bytes: Data) -> Bool {
        nseIsServiceEnvelope(
            bytes,
            decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
            decodesAsCommitterBeacon: decodesAsCommitterBeacon,
            committerBeaconEnvelopeLen: Self.beaconEnvelopeLen,
            decodesAsProfile: decodesAsProfile,
            profileEnvelopeLens: Self.profileEnvelopeLens,
            decodesAsGroupName: decodesAsGroupName,
            groupNameEnvelopeLens: Self.groupNameEnvelopeLens,
            decodesAsProfileRequest: decodesAsProfileRequest,
            profileRequestEnvelopeLen: Self.profileRequestEnvelopeLen,
            decodesAsPin: decodesAsPin,
            pinEnvelopeLen: Self.pinEnvelopeLen,
            decodesAsRegistrarSeed: decodesAsRegistrarSeed,
            registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLen,
            decodesAsScopedDiscovery: decodesAsScopedDiscovery,
            scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLens,
            decodesAsSharedChannels: decodesAsSharedChannels,
            sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLens
        )
    }

    /// WITHOUT THE ARM a "who can find me" setting reaches `parseBodyEnvelope`
    /// and a notification reads `lakko/visibility/...` plus binary.
    func testVisibilityEnvelopeIsAServiceEnvelope() throws {
        var setting = defaultVisibilitySetting()
        setting.updatedAtMs = 1_700_000_000_000
        XCTAssertTrue(isService(try composeVisibilityEnvelope(setting: setting)))
    }

    func testReachStateEnvelopeIsAServiceEnvelope() throws {
        let state = FfiReachState(
            entries: [FfiReachStateEntry(class: .contact, scopeId: Data(), version: 2)],
            updatedAtMs: 5)
        XCTAssertTrue(isService(try composeReachStateEnvelope(state: state)))
    }

    /// A committer delegation (v2, ADR-0078: it carries the added person's
    /// mailbox binding) for `count` candidates, naming the added person's
    /// real mailbox.
    private func delegationEnvelope(count: Int) throws -> Data {
        let added = try Identity.fromSeed(seed: Data(repeating: 9, count: 64))
        let key = MailboxKey.fromAccountIdentity(accountIdentity: added)
        var blobs: [Data] = []
        for index in 0..<count {
            blobs.append(try signDelegationBlob(
                issuerSeed: Data(repeating: 1, count: 32), mailbox: key.address(),
                delegatePk: try reachVerifyingKey(seed: Data(repeating: UInt8(10 + index), count: 32)),
                expiresAtMs: 1_700_000_000_000, nonce: Data(repeating: UInt8(index), count: 16)))
        }
        return try composeCommitterDelegationEnvelope(
            envelope: FfiCommitterDelegationEnvelope(
                addedAccountKey: added.signaturePublicKey(),
                addedMailboxBinding: signMailboxBinding(accountIdentity: added),
                delegations: blobs))
    }

    func testCommitterDelegationEnvelopeIsAServiceEnvelope() throws {
        XCTAssertTrue(isService(try delegationEnvelope(count: 1)))
    }

    /// ADR-0078 — a committer delegation envelope is 346 bytes with one
    /// delegation and 3121 with sixteen (the same bounds the Rust golden in
    /// `crates/lakko-ffi-extension/src/scoped_discovery.rs` pins), both inside
    /// the range the NSE pre-filter admits.
    func testCommitterDelegationEnvelopeLengthsAreInsideThePreFilterRange() throws {
        let smallest = try delegationEnvelope(count: 1)
        let largest = try delegationEnvelope(count: Int(maxCommitterDelegations()))
        XCTAssertEqual(smallest.count, 346)
        XCTAssertEqual(largest.count, 3121)
        XCTAssertTrue(nseLooksLikeScopedDiscovery(smallest, envelopeLens: Self.scopedDiscoveryEnvelopeLens))
        XCTAssertTrue(nseLooksLikeScopedDiscovery(largest, envelopeLens: Self.scopedDiscoveryEnvelopeLens))
    }

    /// A chat body and a profile are never classified as one of the five.
    func testChatBodyIsNotAScopedDiscoveryEnvelope() {
        XCTAssertFalse(Self.decodesAsScopedDiscoveryReal(Data("{\"v\":1,\"text\":\"hi\"}".utf8)))
        XCTAssertFalse(Self.decodesAsScopedDiscoveryReal(pinEnvelope()))
        XCTAssertFalse(nseLooksLikeScopedDiscovery(Data(), envelopeLens: 1...10))
    }

    // MARK: - Pipeline: the filter drops the row and the loop falls back

    /// A throwaway group mirroring the production `NseRealThrowawayGroup
    /// .process` shape: run the service filter FIRST, then classify. The MLS
    /// decrypt + the Rust codec are the only stubbed parts.
    private final class FilteringFakeGroup: NseThrowawayGroup {
        private let decodesAsPushRoutingSecret: (Data) -> Bool
        private let decodesAsCommitterBeacon: (Data) -> Bool
        private let decodesAsProfile: (Data) -> Bool
        private let decodesAsGroupName: (Data) -> Bool
        private let decodesAsProfileRequest: (Data) -> Bool
        private let decodesAsPin: (Data) -> Bool
        private let text: (Data) -> String
        private(set) var suppressed = 0
        var closed = false

        init(
            decodesAsPushRoutingSecret: @escaping (Data) -> Bool,
            decodesAsCommitterBeacon: @escaping (Data) -> Bool,
            decodesAsProfile: @escaping (Data) -> Bool,
            decodesAsGroupName: @escaping (Data) -> Bool,
            decodesAsProfileRequest: @escaping (Data) -> Bool,
            decodesAsPin: @escaping (Data) -> Bool,
            text: @escaping (Data) -> String
        ) {
            self.decodesAsPushRoutingSecret = decodesAsPushRoutingSecret
            self.decodesAsCommitterBeacon = decodesAsCommitterBeacon
            self.decodesAsProfile = decodesAsProfile
            self.decodesAsGroupName = decodesAsGroupName
            self.decodesAsProfileRequest = decodesAsProfileRequest
            self.decodesAsPin = decodesAsPin
            self.text = text
        }

        func process(_ payload: Data) -> NseProcessedRow? {
            if nseIsServiceEnvelope(
                payload,
                decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
                decodesAsCommitterBeacon: decodesAsCommitterBeacon,
                committerBeaconEnvelopeLen: NseServiceEnvelopeTests.beaconEnvelopeLen,
                decodesAsProfile: decodesAsProfile,
                profileEnvelopeLens: NseServiceEnvelopeTests.profileEnvelopeLens,
                decodesAsGroupName: decodesAsGroupName,
                groupNameEnvelopeLens: NseServiceEnvelopeTests.groupNameEnvelopeLens,
                decodesAsProfileRequest: decodesAsProfileRequest,
                profileRequestEnvelopeLen: NseServiceEnvelopeTests.profileRequestEnvelopeLen,
                decodesAsPin: decodesAsPin,
                pinEnvelopeLen: NseServiceEnvelopeTests.pinEnvelopeLen,
                decodesAsRegistrarSeed: NseServiceEnvelopeTests.decodesAsRegistrarSeedDouble,
                registrarSeedEnvelopeLen: NseServiceEnvelopeTests.registrarSeedEnvelopeLen,
                decodesAsScopedDiscovery: NseServiceEnvelopeTests.decodesAsScopedDiscoveryReal,
                scopedDiscoveryEnvelopeLens: NseServiceEnvelopeTests.scopedDiscoveryEnvelopeLens,
                decodesAsSharedChannels: { _ in false },
                sharedChannelsEnvelopeLens: NseServiceEnvelopeTests.sharedChannelsEnvelopeLens
            ) {
                suppressed += 1
                return nil
            }
            return NseProcessedRow(kind: .chat, text: text(payload), attachmentCount: 0)
        }

        func dsProofHeader(method: String, path: String, query: String, body: Data) -> String? { nil }
        func close() { closed = true }
    }

    private struct StubDeps: NsePushDecryptDeps {
        func personName(accountIdB64: String, keyB64: String) -> String? { nil }
        func reactionNotificationsOn() -> Bool { true }
        let group: NseThrowawayGroup
        let rows: [NseFetchedRow]
        let groupIdBytes = Data(repeating: 0xAA, count: 32)

        func boundAccount(wakeSubscriptionIdHex: String?) -> String? { "acct" }
        func lookupRoute(accountIdB64: String, pushGidHex: String, subscriptionIdHex: String?) -> String? {
            accountIdB64 == "acct" ? pushGidHex : nil
        }
        func accountKnowsGroup(accountIdB64: String, groupHex: String) -> Bool {
            accountIdB64 == "acct"
        }
        func loadIdentitySeed(accountIdB64: String) -> Data? { Data(repeating: 1, count: 64) }
        func loadSnapshot(accountIdB64: String) -> Data? { Data(repeating: 2, count: 8) }
        func openThrowawayGroup(
            seed: Data,
            snapshot: Data,
            groupIdBytes: Data
        ) throws -> NseThrowawayGroup? { group }
        func dsFetch(
            groupIdBytes: Data, afterId: Int64, group: NseThrowawayGroup
        ) async throws -> [NseFetchedRow] { rows }
        func startCursor(accountIdB64: String, groupHex: String) -> Int64? { nil }
        func isAntiCensorshipArmed() -> Bool { false }
    }

    private let groupHex = String(repeating: "a", count: 64)

    private func preview(rows: [NseFetchedRow]) async -> (NsePushPreview?, Int) {
        let group = FilteringFakeGroup(
            decodesAsPushRoutingSecret: decodesAsPushRoutingSecret,
            decodesAsCommitterBeacon: decodesAsCommitterBeacon,
            decodesAsProfile: decodesAsProfile,
            decodesAsGroupName: decodesAsGroupName,
            decodesAsProfileRequest: decodesAsProfileRequest,
            decodesAsPin: decodesAsPin,
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
