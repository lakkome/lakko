// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0042 — the iOS `PushGroupRouting` orchestration + `PushRoutingStore`.
///
/// Mirror of the web `apps/web/src/push/group-routing.test.ts` +
/// `group-routing-store.test.ts`. Pins the load-bearing rules with the REAL
/// FFI derivation (so the golden-equality claim is proven, not asserted
/// against a mock) and a REAL owner `Group` (so the owner-mint gate is
/// exercised end-to-end):
///
///   * the OWNER mints the secret exactly once, persists it, broadcasts the
///     distribution envelope, and subscribes under the derived opaque
///     `push_gid` — a second provision is a no-op (no re-mint / re-broadcast);
///   * a NON-owner that receives the envelope decodes the SAME secret and
///     derives the SAME `push_gid` (golden-equal to the owner's) — what makes
///     the publisher/subscriber notify JOIN match;
///   * subscribe routes under `push_gid`, never the DS `group_id`;
///   * a member WITHOUT the secret DEFERS (no subscription under group_id);
///   * the reverse index (`push_gid -> account/group`) is populated from the
///     derived `push_gid` — the map the Sprint-4 decrypt reverse-resolves;
///   * `tryDecodeSecretEnvelope` is fail-closed on a non-envelope plaintext.
///
/// The push-gw subscribe seam is injected (spy); the store is a transient
/// `UserDefaults` suite; the FFI + owner `Group` are REAL.
@MainActor
final class PushGroupRoutingTests: XCTestCase {

    /// Records the `push_gid`s the orchestrator subscribed under (never the
    /// network) — the iOS analog of the web `PushClient` `vi.mock`.
    private final class SubscribeSpy {
        private(set) var targets: [Data] = []
        func record(_ pushGid: Data) { targets.append(pushGid) }
    }

    /// Records the E2EE distribution-envelope bytes the orchestrator asked
    /// the (mocked) journal to broadcast.
    private final class BroadcastRecorder {
        private(set) var envelopes: [Data] = []
        func record(_ bytes: Data) { envelopes.append(bytes) }
    }

    private var suiteName: String = ""
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        suiteName = "test.pushrouting.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        defaults = nil
        super.tearDown()
    }

    // MARK: - helpers

    private func makeStore() -> PushRoutingStore {
        PushRoutingStore(defaults: defaults)
    }

    private func makeRouting(
        store: PushRoutingStore,
        spy: SubscribeSpy
    ) -> PushGroupRouting {
        PushGroupRouting(store: store, subscribeUnderPushGid: { pushGid in
            spy.record(pushGid)
        })
    }

    private func makeIdentity(_ seedByte: UInt8) throws -> Identity {
        try Identity.fromSeed(seed: Data(repeating: seedByte, count: 32))
    }

    /// A real MLS group whose founder (`identity`) is the ADR-0022 owner.
    private func makeOwnerGroup(_ identity: Identity) throws -> (Group, String) {
        let group = try Group.create(identity: identity)
        let groupIdHex = PushGroupRouting.hex(try group.groupId())
        return (group, groupIdHex)
    }

    // MARK: - primitives

    func testMintSecretIs32Bytes() {
        let secret = PushGroupRouting.mintSecret()
        XCTAssertEqual(secret?.count, PushGroupRouting.secretLen)
    }

    func testDeriveIsDeterministicAndDistinct() {
        let secret = Data(repeating: 0x11, count: 32)
        let a = PushGroupRouting.derivePushGid(secret: secret)
        let b = PushGroupRouting.derivePushGid(secret: secret)
        XCTAssertNotNil(a)
        XCTAssertEqual(a, b, "same secret must derive the same push_gid")
        XCTAssertEqual(a?.count, 32)

        var other = secret
        other[0] ^= 0xFF
        let c = PushGroupRouting.derivePushGid(secret: other)
        XCTAssertNotEqual(a, c, "a different secret must derive a different push_gid")
    }

    func testComposeDecodeRoundTrip() throws {
        let secret = try XCTUnwrap(PushGroupRouting.mintSecret())
        let envelope = try PushGroupRouting.composeSecretEnvelope(secret: secret)
        XCTAssertEqual(envelope.count, PushGroupRouting.envelopeLen)
        let decoded = PushGroupRouting.tryDecodeSecretEnvelope(envelope)
        XCTAssertEqual(decoded, secret)
    }

    func testTryDecodeFailClosedRejectsNonEnvelope() {
        // A plain chat JSON envelope is not a push-routing envelope.
        let chat = Data("{\"v\":1,\"t\":\"hello\"}".utf8)
        XCTAssertNil(PushGroupRouting.tryDecodeSecretEnvelope(chat))
        // Right length, wrong first byte.
        XCTAssertNil(PushGroupRouting.tryDecodeSecretEnvelope(
            Data(repeating: 0x7b, count: PushGroupRouting.envelopeLen)))
        // Right first byte, wrong length.
        XCTAssertNil(PushGroupRouting.tryDecodeSecretEnvelope(Data([0x6c, 0x00])))
    }

    // MARK: - store round-trip + reverse index

    func testStoreRoundTripAndReverseIndex() throws {
        let store = makeStore()
        let secret = try XCTUnwrap(PushGroupRouting.mintSecret())
        let pushGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: secret))
        let pushGidHex = PushGroupRouting.hex(pushGid)

        XCTAssertFalse(store.hasSecret(accountIdB64: "acct", groupIdHex: "grp"))
        store.putRouting(
            accountIdB64: "acct", groupIdHex: "grp", secret: secret, pushGidHex: pushGidHex)

        // Presence check (no device key needed).
        XCTAssertTrue(store.hasSecret(accountIdB64: "acct", groupIdHex: "grp"))
        // Device-key-encrypted round-trip returns the exact 32 bytes.
        XCTAssertEqual(store.getSecret(accountIdB64: "acct", groupIdHex: "grp"), secret)
        // Plaintext reverse index maps push_gid -> (account, group_id).
        let route = store.lookupRoute(pushGidHex: pushGidHex)
        XCTAssertEqual(route?.accountIdB64, "acct")
        XCTAssertEqual(route?.groupIdHex, "grp")

        // A wipe removes both the secret + the reverse-index entry.
        store.clear(accountIdB64: "acct")
        XCTAssertFalse(store.hasSecret(accountIdB64: "acct", groupIdHex: "grp"))
        XCTAssertNil(store.getSecret(accountIdB64: "acct", groupIdHex: "grp"))
        XCTAssertNil(store.lookupRoute(pushGidHex: pushGidHex))
    }

    // MARK: - owner-mint funnel

    func testOwnerMintsOncePersistsBroadcastsSubscribesUnderPushGid() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let recorder = BroadcastRecorder()

        let identity = try makeIdentity(0xAA)
        let (group, groupIdHex) = try makeOwnerGroup(identity)
        let groupIdBytes = try group.groupId()

        await routing.ensureProvisioned(
            accountIdB64: "acctOwner",
            groupIdHex: groupIdHex,
            group: group,
            identity: identity,
            broadcast: { bytes in recorder.record(bytes) }
        )

        // Broadcast exactly once; the envelope decodes back to a 32-byte secret.
        XCTAssertEqual(recorder.envelopes.count, 1)
        let secret = try XCTUnwrap(
            PushGroupRouting.tryDecodeSecretEnvelope(recorder.envelopes[0]))
        XCTAssertEqual(secret.count, 32)

        // The secret is persisted (device-key-encrypted) + hasSecret true.
        XCTAssertTrue(store.hasSecret(accountIdB64: "acctOwner", groupIdHex: groupIdHex))
        XCTAssertEqual(store.getSecret(accountIdB64: "acctOwner", groupIdHex: groupIdHex), secret)

        // push_gid = keyed-BLAKE3(secret); the reverse index maps it to the group.
        let pushGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: secret))
        let route = store.lookupRoute(pushGidHex: PushGroupRouting.hex(pushGid))
        XCTAssertEqual(route?.accountIdB64, "acctOwner")
        XCTAssertEqual(route?.groupIdHex, groupIdHex)

        // Subscribed under push_gid — NOT the DS group_id.
        XCTAssertEqual(spy.targets, [pushGid])
        XCTAssertNotEqual(spy.targets.first, groupIdBytes)
    }

    func testSecondProvisionIsNoOp() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let recorder = BroadcastRecorder()

        let identity = try makeIdentity(0xBB)
        let (group, groupIdHex) = try makeOwnerGroup(identity)

        for _ in 0..<2 {
            await routing.ensureProvisioned(
                accountIdB64: "acctO",
                groupIdHex: groupIdHex,
                group: group,
                identity: identity,
                broadcast: { bytes in recorder.record(bytes) }
            )
        }

        // Minted + broadcast exactly once across two provisions.
        XCTAssertEqual(recorder.envelopes.count, 1)
        XCTAssertTrue(store.hasSecret(accountIdB64: "acctO", groupIdHex: groupIdHex))
    }

    func testNonOwnerNeverMints() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let recorder = BroadcastRecorder()

        // The group is owned by `owner`; `stranger` is not a member, so
        // `group.ownRole(stranger)` is not `.owner` → never mints.
        let owner = try makeIdentity(0xC1)
        let stranger = try makeIdentity(0xC2)
        let (group, groupIdHex) = try makeOwnerGroup(owner)

        await routing.ensureProvisioned(
            accountIdB64: "acctStranger",
            groupIdHex: groupIdHex,
            group: group,
            identity: stranger,
            broadcast: { bytes in recorder.record(bytes) }
        )

        XCTAssertTrue(recorder.envelopes.isEmpty, "a non-owner must never mint/broadcast")
        XCTAssertFalse(store.hasSecret(accountIdB64: "acctStranger", groupIdHex: groupIdHex))
        // No secret held → subscription deferred (never under group_id).
        XCTAssertTrue(spy.targets.isEmpty)
    }

    // MARK: - receive side

    func testNonOwnerReceiveDerivesGoldenPushGid() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)

        // Owner mints a secret + composes the distribution envelope.
        let ownerSecret = try XCTUnwrap(PushGroupRouting.mintSecret())
        let envelope = try PushGroupRouting.composeSecretEnvelope(secret: ownerSecret)
        let ownerPushGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: ownerSecret))

        // Non-owner decodes the envelope back to the exact secret ...
        let decoded = try XCTUnwrap(PushGroupRouting.tryDecodeSecretEnvelope(envelope))
        XCTAssertEqual(decoded, ownerSecret)

        // ... persists it under its own account + subscribes under push_gid.
        await routing.receiveSecret(
            accountIdB64: "acctPeer", groupIdHex: "grpPeer", secret: decoded)

        // The golden-equality: the non-owner's push_gid == the owner's.
        let route = store.lookupRoute(pushGidHex: PushGroupRouting.hex(ownerPushGid))
        XCTAssertEqual(route?.accountIdB64, "acctPeer")
        XCTAssertEqual(route?.groupIdHex, "grpPeer")
        XCTAssertEqual(spy.targets, [ownerPushGid])
    }

    func testMemberWithoutSecretDefers() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)

        await routing.subscribeUnderPushGidBestEffort(
            accountIdB64: "acctNoSecret", groupIdHex: "unknownGrp")

        // Deferred — no subscription under group_id (or anything at all).
        XCTAssertTrue(spy.targets.isEmpty)
    }
}
