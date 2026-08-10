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
///   * `tryDecodeSecretEnvelope` is fail-closed on a non-envelope plaintext;
///   * **the receive-side authority gate** (`lakko-security` MEDIUM-1):
///     owner outranks admin outranks member — the OWNER may always set the
///     secret, an ADMIN may only SEED a hole (first-ever) and never
///     overwrite, a plain member may only re-send the byte-identical value
///     (which keeps `redistribute` idempotent); an unreadable role table
///     refuses a change.
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

    /// A real 2-member MLS group: `owner` founds it (ADR-0022 owner) and
    /// `member` is added via a real `addMemberFromWire` commit, so
    /// `group.members()` carries BOTH leaves with their real signature keys —
    /// what the owner gate resolves `senderLeaf` against.
    private func makeTwoMemberGroup(
        owner: Identity,
        member: Identity
    ) throws -> (group: Group, groupIdHex: String, ownerLeaf: UInt32, memberLeaf: UInt32) {
        let (group, groupIdHex) = try makeOwnerGroup(owner)
        _ = try group.addMemberFromWire(
            identity: owner,
            keyPackage: try member.createKeyPackage()
        )
        let ownerKey = try XCTUnwrap(try group.groupRoles()).owner
        let roster = try group.members()
        let ownerLeaf = try XCTUnwrap(
            roster.first { $0.signaturePublicKey == ownerKey }?.leafIndex)
        let memberLeaf = try XCTUnwrap(
            roster.first { $0.signaturePublicKey != ownerKey }?.leafIndex)
        XCTAssertNotEqual(ownerLeaf, memberLeaf)
        return (group, groupIdHex, ownerLeaf, memberLeaf)
    }

    /// A real 3-member MLS group: `owner` founds it, adds `admin` and
    /// `member`, then promotes `admin` into the ADR-0022 role table's admin
    /// set via a REAL owner-only `setGroupRoles` GroupContextExtensions
    /// commit — so `group.groupRoles()!.admins` really contains the admin's
    /// signature key and the gate is exercised against genuine role state,
    /// not a hand-built table.
    private func makeGroupWithAdmin(
        owner: Identity,
        admin: Identity,
        member: Identity
    ) throws -> (
        group: Group, groupIdHex: String,
        ownerLeaf: UInt32, adminLeaf: UInt32, memberLeaf: UInt32
    ) {
        let (group, groupIdHex) = try makeOwnerGroup(owner)
        _ = try group.addMemberFromWire(
            identity: owner, keyPackage: try admin.createKeyPackage())
        _ = try group.addMemberFromWire(
            identity: owner, keyPackage: try member.createKeyPackage())

        let ownerKey = try XCTUnwrap(try group.groupRoles()).owner
        let roster = try group.members()
        let ownerLeaf = try XCTUnwrap(
            roster.first { $0.signaturePublicKey == ownerKey }?.leafIndex)
        // Leaf order is an MLS-tree detail; pin the admin/member split on
        // the identity's own signature key, resolved through the roster.
        let others = roster.filter { $0.signaturePublicKey != ownerKey }
        XCTAssertEqual(others.count, 2)
        let adminEntry = try XCTUnwrap(others.first)
        let memberEntry = try XCTUnwrap(others.last)

        // The owner-only role-table commit promoting exactly one of them.
        _ = try group.setGroupRoles(
            identity: owner,
            owner: ownerKey,
            admins: [adminEntry.signaturePublicKey]
        )
        let table = try XCTUnwrap(try group.groupRoles())
        XCTAssertEqual(table.owner, ownerKey)
        XCTAssertEqual(table.admins, [adminEntry.signaturePublicKey],
                       "the promotion commit must be reflected in the live role table")
        XCTAssertFalse(table.admins.contains(memberEntry.signaturePublicKey),
                       "the plain member must NOT be an admin")

        return (group, groupIdHex, ownerLeaf, adminEntry.leafIndex, memberEntry.leafIndex)
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

        // A real 2-member group: the OWNER authors the distribution envelope,
        // the non-owner `member` device is the one receiving it.
        let (group, groupIdHex, ownerLeaf, _) = try makeTwoMemberGroup(
            owner: try makeIdentity(0xD1), member: try makeIdentity(0xD2))

        // Owner mints a secret + composes the distribution envelope.
        let ownerSecret = try XCTUnwrap(PushGroupRouting.mintSecret())
        let envelope = try PushGroupRouting.composeSecretEnvelope(secret: ownerSecret)
        let ownerPushGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: ownerSecret))

        // Non-owner decodes the envelope back to the exact secret ...
        let decoded = try XCTUnwrap(PushGroupRouting.tryDecodeSecretEnvelope(envelope))
        XCTAssertEqual(decoded, ownerSecret)

        // ... persists it under its own account + subscribes under push_gid.
        await routing.receiveSecret(
            accountIdB64: "acctPeer",
            groupIdHex: groupIdHex,
            secret: decoded,
            group: group,
            senderLeaf: ownerLeaf
        )

        // The golden-equality: the non-owner's push_gid == the owner's.
        let route = store.lookupRoute(pushGidHex: PushGroupRouting.hex(ownerPushGid))
        XCTAssertEqual(route?.accountIdB64, "acctPeer")
        XCTAssertEqual(route?.groupIdHex, groupIdHex)
        XCTAssertEqual(spy.targets, [ownerPushGid])
    }

    // MARK: - receive-side OWNER GATE (lakko-security MEDIUM-1)

    /// The pure decision function, exhaustively. This is the layer that
    /// covers the two authority states a real `Group` cannot be coaxed into
    /// (`.noRoleTable` — `Group.create` always seeds the ADR-0022 owner
    /// table; `.unreadable` — the FFI roster/role read does not fail on a
    /// live handle).
    func testClassifyIncomingSecretMatrix() {
        let a = Data(repeating: 0xA1, count: 32)
        let b = Data(repeating: 0xB2, count: 32)
        let ownerKey = Data(repeating: 0x01, count: 32)
        let adminKey = Data(repeating: 0x02, count: 32)
        let memberKey = Data(repeating: 0x03, count: 32)
        let roles = PushGroupRouting.SecretAuthority.roleTable(
            owner: ownerKey, admins: [adminKey])

        // --- CHANGED secret (something already stored) ---------------------
        // OWNER authored the change -> accept (the owner may always rotate)
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: b, stored: a, senderKey: ownerKey, authority: roles),
            .accept)
        // ADMIN authored the change -> REJECT (an admin may not OVERWRITE)
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: b, stored: a, senderKey: adminKey, authority: roles),
            .reject)
        // a PLAIN MEMBER authored the change -> REJECT
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: b, stored: a, senderKey: memberKey, authority: roles),
            .reject)
        // unattributable sender (no leaf / leaf not in roster) -> REJECT
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: b, stored: a, senderKey: nil, authority: roles),
            .reject)
        // legacy / open group with no role table -> accept (pre-gate behaviour)
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: b, stored: a, senderKey: memberKey, authority: .noRoleTable),
            .accept)
        // roster / role read threw -> REJECT (fail-closed, invariant #4)
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: b, stored: a, senderKey: memberKey, authority: .unreadable),
            .reject)

        // --- IDENTICAL secret: accepted from ANYONE (idempotent redistribute)
        for authority: PushGroupRouting.SecretAuthority in
            [roles, .noRoleTable, .unreadable] {
            for sender: Data? in [ownerKey, adminKey, memberKey, nil] {
                XCTAssertEqual(
                    PushGroupRouting.classifyIncomingSecret(
                        incoming: a, stored: a, senderKey: sender, authority: authority),
                    .accept)
            }
        }

        // --- FIRST-EVER secret (nothing stored) ----------------------------
        // OWNER seeds -> accept
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: a, stored: nil, senderKey: ownerKey, authority: roles),
            .accept)
        // ADMIN seeds a HOLE -> accept (closes the admin-add delivery gap)
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: a, stored: nil, senderKey: adminKey, authority: roles),
            .accept)
        // a PLAIN MEMBER seeds -> REJECT (even with nothing stored)
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: a, stored: nil, senderKey: memberKey, authority: roles),
            .reject)
        // unattributable / unreadable seeds -> REJECT
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: a, stored: nil, senderKey: nil, authority: roles),
            .reject)
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: a, stored: nil, senderKey: nil, authority: .unreadable),
            .reject)

        // --- OWNER PRIORITY: the owner overwrites an ADMIN-SEEDED secret ----
        // Arrival order must not matter — whatever an admin seeded, the
        // owner's value wins.
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: b, stored: a, senderKey: ownerKey, authority: roles),
            .accept)

        // --- DEGENERATE keys are never a match -----------------------------
        // An empty resolved sender key must not match an empty owner/admin
        // entry (no authority level is inferred from a degenerate table).
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: a, stored: nil, senderKey: Data(),
                authority: .roleTable(owner: Data(), admins: [Data()])),
            .reject)

        // --- ADMIN HOLE MUST BE PROVABLE -----------------------------------
        // `stored == nil` also happens when the store could not be READ (no
        // device wrap key / AEAD open failed). An admin must not be able to
        // exploit that to overwrite a secret this device really holds.
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: a, stored: nil, storedUnreadable: true,
                senderKey: adminKey, authority: roles),
            .reject)
        // ... but the OWNER may still write over an unreadable store — that
        // is the repair path.
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: a, stored: nil, storedUnreadable: true,
                senderKey: ownerKey, authority: roles),
            .accept)
        // A provable hole (the default) still lets the admin seed.
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: a, stored: nil, storedUnreadable: false,
                senderKey: adminKey, authority: roles),
            .accept)

        // --- MULTI-ADMIN: every listed admin may seed ----------------------
        let admin2 = Data(repeating: 0x04, count: 32)
        let multi = PushGroupRouting.SecretAuthority.roleTable(
            owner: ownerKey, admins: [adminKey, admin2])
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: a, stored: nil, senderKey: admin2, authority: multi),
            .accept)
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: b, stored: a, senderKey: admin2, authority: multi),
            .reject)
    }

    /// The leaf -> signature-key mapping the gate depends on, against a REAL
    /// 2-member group: `Group.members()` keyed by `leafIndex`, compared to
    /// `Group.groupRoles()?.owner`.
    func testResolveSenderAuthorityOnRealGroup() throws {
        let (group, _, ownerLeaf, memberLeaf) = try makeTwoMemberGroup(
            owner: try makeIdentity(0xE1), member: try makeIdentity(0xE2))
        let ownerKey = try XCTUnwrap(try group.groupRoles()).owner

        // The owner's leaf resolves to the role-table owner key. A freshly
        // founded group has an owner and an EMPTY admin set.
        let fromOwner = PushGroupRouting.resolveSenderAuthority(
            group: group, senderLeaf: ownerLeaf)
        XCTAssertEqual(fromOwner.authority, .roleTable(owner: ownerKey, admins: []))
        XCTAssertEqual(fromOwner.senderKey, ownerKey)

        // The non-owner member's leaf resolves to a DIFFERENT key.
        let fromMember = PushGroupRouting.resolveSenderAuthority(
            group: group, senderLeaf: memberLeaf)
        XCTAssertEqual(fromMember.authority, .roleTable(owner: ownerKey, admins: []))
        XCTAssertNotNil(fromMember.senderKey)
        XCTAssertNotEqual(fromMember.senderKey, ownerKey)

        // A leaf that is not in the roster, and no leaf at all, resolve to nil
        // (rejected on a change) — never to a wrong key.
        XCTAssertNil(
            PushGroupRouting.resolveSenderAuthority(group: group, senderLeaf: 9999).senderKey)
        XCTAssertNil(
            PushGroupRouting.resolveSenderAuthority(group: group, senderLeaf: nil).senderKey)
    }

    /// End-to-end through `receiveSecret` on a REAL 2-member group: the owner
    /// may rotate; a non-owner member may not.
    func testReceiveOwnerMayChangeTheSecret() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let (group, groupIdHex, ownerLeaf, _) = try makeTwoMemberGroup(
            owner: try makeIdentity(0xF1), member: try makeIdentity(0xF2))

        let first = try XCTUnwrap(PushGroupRouting.mintSecret())
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: first,
            group: group, senderLeaf: ownerLeaf)
        XCTAssertEqual(store.getSecret(accountIdB64: "acct", groupIdHex: groupIdHex), first)

        // The OWNER rotates it -> accepted, stored value swaps, re-subscribed
        // under the NEW push_gid.
        let rotated = try XCTUnwrap(PushGroupRouting.mintSecret())
        XCTAssertNotEqual(rotated, first)
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: rotated,
            group: group, senderLeaf: ownerLeaf)

        XCTAssertEqual(store.getSecret(accountIdB64: "acct", groupIdHex: groupIdHex), rotated)
        XCTAssertEqual(spy.targets, [
            try XCTUnwrap(PushGroupRouting.derivePushGid(secret: first)),
            try XCTUnwrap(PushGroupRouting.derivePushGid(secret: rotated)),
        ])
    }

    func testReceiveNonOwnerChangedSecretIsRejected() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let (group, groupIdHex, ownerLeaf, memberLeaf) = try makeTwoMemberGroup(
            owner: try makeIdentity(0x21), member: try makeIdentity(0x22))

        // The owner provisions the group's real secret.
        let ownerSecret = try XCTUnwrap(PushGroupRouting.mintSecret())
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: ownerSecret,
            group: group, senderLeaf: ownerLeaf)
        let ownerPushGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: ownerSecret))
        XCTAssertEqual(spy.targets, [ownerPushGid])

        // A NON-owner member broadcasts a DIFFERENT secret — the unilateral
        // group-wide rotation MEDIUM-1 describes. It must be dropped whole:
        // no persist, no re-subscribe, no orphan push-gw subscription.
        let rogue = try XCTUnwrap(PushGroupRouting.mintSecret())
        XCTAssertNotEqual(rogue, ownerSecret)
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: rogue,
            group: group, senderLeaf: memberLeaf)

        XCTAssertEqual(
            store.getSecret(accountIdB64: "acct", groupIdHex: groupIdHex), ownerSecret,
            "a non-owner must not be able to overwrite the group's routing secret")
        XCTAssertEqual(spy.targets, [ownerPushGid], "no re-subscribe on a refused secret")
        // The rogue push_gid never entered the reverse index either.
        let rogueGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: rogue))
        XCTAssertNil(store.lookupRoute(pushGidHex: PushGroupRouting.hex(rogueGid)))
    }

    // MARK: - ADMIN rows of the gate, against a REAL promoted admin

    /// The resolver carries the ADR-0022 admin set alongside the owner, off
    /// a REAL `setGroupRoles` promotion commit.
    func testResolveSenderAuthorityCarriesTheAdminSet() throws {
        let (group, _, ownerLeaf, adminLeaf, memberLeaf) = try makeGroupWithAdmin(
            owner: try makeIdentity(0x61),
            admin: try makeIdentity(0x62),
            member: try makeIdentity(0x63))
        let table = try XCTUnwrap(try group.groupRoles())

        let fromOwner = PushGroupRouting.resolveSenderAuthority(
            group: group, senderLeaf: ownerLeaf)
        XCTAssertEqual(
            fromOwner.authority, .roleTable(owner: table.owner, admins: table.admins))
        XCTAssertEqual(fromOwner.senderKey, table.owner)

        // The promoted admin's leaf resolves to a key IN the admin set.
        let fromAdmin = PushGroupRouting.resolveSenderAuthority(
            group: group, senderLeaf: adminLeaf)
        let adminKey = try XCTUnwrap(fromAdmin.senderKey)
        XCTAssertTrue(table.admins.contains(adminKey))
        XCTAssertNotEqual(adminKey, table.owner)

        // The plain member's leaf is neither the owner nor an admin.
        let memberKey = try XCTUnwrap(
            PushGroupRouting.resolveSenderAuthority(
                group: group, senderLeaf: memberLeaf).senderKey)
        XCTAssertNotEqual(memberKey, table.owner)
        XCTAssertFalse(table.admins.contains(memberKey))
    }

    /// **The delivery regression this refinement closes.** ADR-0022 lets an
    /// ADMIN add a member, and `redistribute` fires after that add — so the
    /// new member's first-ever secret arrives from the admin, not the owner.
    /// It must be accepted, else that member never gets push routing.
    func testReceiveAdminSeedsFirstEverSecret() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let (group, groupIdHex, _, adminLeaf, _) = try makeGroupWithAdmin(
            owner: try makeIdentity(0x71),
            admin: try makeIdentity(0x72),
            member: try makeIdentity(0x73))

        // Nothing stored — this device was just added by the admin.
        XCTAssertNil(store.getSecret(accountIdB64: "acctNew", groupIdHex: groupIdHex))

        let secret = try XCTUnwrap(PushGroupRouting.mintSecret())
        await routing.receiveSecret(
            accountIdB64: "acctNew", groupIdHex: groupIdHex, secret: secret,
            group: group, senderLeaf: adminLeaf)

        let pushGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: secret))
        XCTAssertEqual(
            store.getSecret(accountIdB64: "acctNew", groupIdHex: groupIdHex), secret,
            "an admin must be able to SEED the first-ever secret")
        XCTAssertEqual(spy.targets, [pushGid], "and the seeded member subscribes under push_gid")
    }

    /// An admin may seed a hole but must NOT be able to rotate a secret the
    /// device already holds — that power stays owner-only.
    func testReceiveAdminChangedSecretIsRejected() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let (group, groupIdHex, ownerLeaf, adminLeaf, _) = try makeGroupWithAdmin(
            owner: try makeIdentity(0x81),
            admin: try makeIdentity(0x82),
            member: try makeIdentity(0x83))

        let ownerSecret = try XCTUnwrap(PushGroupRouting.mintSecret())
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: ownerSecret,
            group: group, senderLeaf: ownerLeaf)
        let ownerPushGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: ownerSecret))
        XCTAssertEqual(spy.targets, [ownerPushGid])

        let rogue = try XCTUnwrap(PushGroupRouting.mintSecret())
        XCTAssertNotEqual(rogue, ownerSecret)
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: rogue,
            group: group, senderLeaf: adminLeaf)

        XCTAssertEqual(
            store.getSecret(accountIdB64: "acct", groupIdHex: groupIdHex), ownerSecret,
            "an admin must not be able to OVERWRITE the group's routing secret")
        XCTAssertEqual(spy.targets, [ownerPushGid], "no re-subscribe on a refused secret")
        let rogueGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: rogue))
        XCTAssertNil(store.lookupRoute(pushGidHex: PushGroupRouting.hex(rogueGid)))
    }

    /// Owner priority: whatever an admin seeded, a later owner broadcast
    /// overwrites it (the "owner takes priority" half of the rule).
    func testReceiveOwnerOverwritesAnAdminSeededSecret() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let (group, groupIdHex, ownerLeaf, adminLeaf, _) = try makeGroupWithAdmin(
            owner: try makeIdentity(0x91),
            admin: try makeIdentity(0x92),
            member: try makeIdentity(0x93))

        let adminSeeded = try XCTUnwrap(PushGroupRouting.mintSecret())
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: adminSeeded,
            group: group, senderLeaf: adminLeaf)
        XCTAssertEqual(store.getSecret(accountIdB64: "acct", groupIdHex: groupIdHex), adminSeeded)

        let ownerSecret = try XCTUnwrap(PushGroupRouting.mintSecret())
        XCTAssertNotEqual(ownerSecret, adminSeeded)
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: ownerSecret,
            group: group, senderLeaf: ownerLeaf)

        XCTAssertEqual(
            store.getSecret(accountIdB64: "acct", groupIdHex: groupIdHex), ownerSecret,
            "the owner outranks the admin regardless of arrival order")
        XCTAssertEqual(spy.targets, [
            try XCTUnwrap(PushGroupRouting.derivePushGid(secret: adminSeeded)),
            try XCTUnwrap(PushGroupRouting.derivePushGid(secret: ownerSecret)),
        ])
    }

    /// A PLAIN member may not even seed — widening the gate to admins must
    /// not have widened it to everyone.
    func testReceivePlainMemberSeedIsRejected() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let (group, groupIdHex, _, _, memberLeaf) = try makeGroupWithAdmin(
            owner: try makeIdentity(0xA1),
            admin: try makeIdentity(0xA2),
            member: try makeIdentity(0xA3))

        let secret = try XCTUnwrap(PushGroupRouting.mintSecret())
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: secret,
            group: group, senderLeaf: memberLeaf)

        XCTAssertNil(
            store.getSecret(accountIdB64: "acct", groupIdHex: groupIdHex),
            "a plain member must not be able to seed the routing secret")
        XCTAssertTrue(spy.targets.isEmpty)
    }

    /// A non-owner relaying the SAME bytes we already hold is a no-op
    /// re-persist — accepted, so the owner's post-add-member `redistribute`
    /// stays idempotent no matter who the relay attribution resolves to.
    func testReceiveNonOwnerIdenticalSecretIsAccepted() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let (group, groupIdHex, ownerLeaf, memberLeaf) = try makeTwoMemberGroup(
            owner: try makeIdentity(0x31), member: try makeIdentity(0x32))

        let secret = try XCTUnwrap(PushGroupRouting.mintSecret())
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: secret,
            group: group, senderLeaf: ownerLeaf)

        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: secret,
            group: group, senderLeaf: memberLeaf)

        let pushGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: secret))
        XCTAssertEqual(store.getSecret(accountIdB64: "acct", groupIdHex: groupIdHex), secret)
        XCTAssertEqual(spy.targets, [pushGid, pushGid], "identical re-send stays idempotent")
    }

    /// An envelope with no MLS sender attribution cannot authorise a change.
    func testReceiveUnattributedChangedSecretIsRejected() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let (group, groupIdHex, _, _) = try makeTwoMemberGroup(
            owner: try makeIdentity(0x41), member: try makeIdentity(0x42))

        let secret = try XCTUnwrap(PushGroupRouting.mintSecret())
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: secret,
            group: group, senderLeaf: nil)

        XCTAssertNil(store.getSecret(accountIdB64: "acct", groupIdHex: groupIdHex))
        XCTAssertTrue(spy.targets.isEmpty)
    }

    /// A sender leaf that is not in the current roster is not the owner.
    func testReceiveUnknownLeafChangedSecretIsRejected() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let (group, groupIdHex, _, _) = try makeTwoMemberGroup(
            owner: try makeIdentity(0x51), member: try makeIdentity(0x52))

        let secret = try XCTUnwrap(PushGroupRouting.mintSecret())
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: secret,
            group: group, senderLeaf: 9999)

        XCTAssertNil(store.getSecret(accountIdB64: "acct", groupIdHex: groupIdHex))
        XCTAssertTrue(spy.targets.isEmpty)
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
