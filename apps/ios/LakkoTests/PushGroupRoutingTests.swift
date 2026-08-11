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
///     secret, an ADMIN may SEED a hole (first-ever) **and (ADR-0043 option
///     (b)) OVERWRITE an established one**, a plain member may only re-send
///     the byte-identical value (which keeps `redistribute` idempotent); an
///     unreadable role table refuses a change.
///
/// ADR-0043 additionally pins the rotation-on-removal arc:
///
///   * the pure membership-SHRINK decision core (a set difference over
///     member signature keys, so a remove+add in one batch still rotates);
///   * the rotation flow — old secret read BEFORE the in-place overwrite,
///     new gid subscribed + indexed, OLD gid unsubscribed + de-indexed, in
///     that order;
///   * a failed rotation broadcast leaves the row PENDING for the existing
///     any-holder retry;
///   * the receive-side teardown, which is what keeps a rotation from
///     leaving one dead push-gw row per member.
///
/// The push-gw subscribe + unsubscribe seams are injected (spies); the store
/// is a transient `UserDefaults` suite; the FFI + owner `Group` are REAL.
@MainActor
final class PushGroupRoutingTests: XCTestCase {

    /// Records the `push_gid`s the orchestrator subscribed under (never the
    /// network) — the iOS analog of the web `PushClient` `vi.mock`.
    private final class SubscribeSpy {
        private(set) var targets: [Data] = []
        func record(_ pushGid: Data) { targets.append(pushGid) }
    }

    /// ADR-0043 — records the `push_gid`s the orchestrator UNSUBSCRIBED from,
    /// plus how many subscribes had already happened at that moment. The
    /// latter pins the load-bearing ordering: the device arms the NEW id
    /// before dropping the OLD one, so it is never unsubscribed from both at
    /// once.
    private final class UnsubscribeSpy {
        private(set) var targets: [Data] = []
        private(set) var subscribesBefore: [Int] = []
        func record(_ pushGid: Data, subscribesSoFar: Int) {
            targets.append(pushGid)
            subscribesBefore.append(subscribesSoFar)
        }
    }

    /// Records the E2EE distribution-envelope bytes the orchestrator asked
    /// the (mocked) journal to broadcast.
    private final class BroadcastRecorder {
        private(set) var envelopes: [Data] = []
        func record(_ bytes: Data) { envelopes.append(bytes) }
    }

    /// A broadcast seam that can be made to FAIL — the offline / DS-error
    /// case that leaves a minted secret durably stored but undelivered.
    private final class FlakyBroadcastRecorder {
        struct Failure: Error {}
        private(set) var envelopes: [Data] = []
        var shouldFail: Bool
        init(shouldFail: Bool) { self.shouldFail = shouldFail }
        /// Records the attempt FIRST, then throws when armed — so a test can
        /// tell "never attempted" from "attempted and failed".
        func send(_ bytes: Data) throws {
            envelopes.append(bytes)
            if shouldFail { throw Failure() }
        }
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
        spy: SubscribeSpy,
        unsubscribeSpy: UnsubscribeSpy? = nil
    ) -> PushGroupRouting {
        PushGroupRouting(
            store: store,
            subscribeUnderPushGid: { pushGid in
                spy.record(pushGid)
            },
            unsubscribeUnderPushGid: { pushGid in
                // Snapshot the subscribe count AT the unsubscribe, so the
                // arm-new-before-drop-old ordering is directly assertable.
                unsubscribeSpy?.record(pushGid, subscribesSoFar: spy.targets.count)
            }
        )
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
            accountIdB64: "acct", groupIdHex: "grp", secret: secret, pushGidHex: pushGidHex,
            distributed: true)

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

    // MARK: - durable `distributed` flag: the undelivered-mint repair

    /// **The regression this flag closes.** A mint whose broadcast fails
    /// leaves the secret durably stored, so the old `isOwner && !hasSecret`
    /// guard short-circuited on every later launch and NEVER retried — no
    /// other member ever learned the secret and the whole group deferred
    /// push permanently. The owner must re-broadcast until it lands.
    func testFailedBroadcastReBroadcastsOnTheNextProvision() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let broadcaster = FlakyBroadcastRecorder(shouldFail: true)

        let identity = try makeIdentity(0x30)
        let (group, groupIdHex) = try makeOwnerGroup(identity)

        // 1st provision — the mint succeeds, the journal write FAILS.
        await routing.ensureProvisioned(
            accountIdB64: "acctOwner", groupIdHex: groupIdHex, group: group,
            identity: identity, broadcast: { try broadcaster.send($0) })

        XCTAssertEqual(broadcaster.envelopes.count, 1, "the mint attempted a broadcast")
        // The secret is durably stored (persist-before-broadcast is kept) ...
        let minted = try XCTUnwrap(
            store.getRecord(accountIdB64: "acctOwner", groupIdHex: groupIdHex))
        // ... but marked NOT distributed: nobody in the group learned it.
        XCTAssertFalse(
            minted.distributed,
            "a failed broadcast must leave the secret marked undistributed")

        // 2nd provision (a relaunch / chat re-open) — the repair branch.
        broadcaster.shouldFail = false
        await routing.ensureProvisioned(
            accountIdB64: "acctOwner", groupIdHex: groupIdHex, group: group,
            identity: identity, broadcast: { try broadcaster.send($0) })

        XCTAssertEqual(
            broadcaster.envelopes.count, 2,
            "an undistributed secret must be RE-BROADCAST on the next provision")
        // The retry carries the SAME secret, recomposed from the store — a
        // second MINT would diverge the group's routing id. The envelope is
        // deterministic in the secret, so byte-equality proves no re-mint.
        XCTAssertEqual(broadcaster.envelopes[0], broadcaster.envelopes[1])
        XCTAssertEqual(
            PushGroupRouting.tryDecodeSecretEnvelope(broadcaster.envelopes[1]), minted.secret)

        // The successful re-broadcast settles the flag ...
        XCTAssertEqual(
            store.getRecord(accountIdB64: "acctOwner", groupIdHex: groupIdHex)?.distributed,
            true)

        // ... so a 3rd provision is silent again.
        await routing.ensureProvisioned(
            accountIdB64: "acctOwner", groupIdHex: groupIdHex, group: group,
            identity: identity, broadcast: { try broadcaster.send($0) })
        XCTAssertEqual(
            broadcaster.envelopes.count, 2, "a settled secret is never re-broadcast")
    }

    /// The other half: a mint whose broadcast SUCCEEDS is settled
    /// immediately, so the repair branch never fires for a healthy group.
    func testSuccessfulMintIsMarkedDistributedAndNotReBroadcast() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let broadcaster = FlakyBroadcastRecorder(shouldFail: false)

        let identity = try makeIdentity(0x32)
        let (group, groupIdHex) = try makeOwnerGroup(identity)

        await routing.ensureProvisioned(
            accountIdB64: "acctOwner", groupIdHex: groupIdHex, group: group,
            identity: identity, broadcast: { try broadcaster.send($0) })

        XCTAssertEqual(broadcaster.envelopes.count, 1)
        XCTAssertEqual(
            store.getRecord(accountIdB64: "acctOwner", groupIdHex: groupIdHex)?.distributed,
            true, "a landed broadcast settles the flag")

        await routing.ensureProvisioned(
            accountIdB64: "acctOwner", groupIdHex: groupIdHex, group: group,
            identity: identity, broadcast: { try broadcaster.send($0) })
        XCTAssertEqual(
            broadcaster.envelopes.count, 1,
            "a distributed secret must not be re-broadcast on a later provision")
    }

    /// Upgrade rule: a row written before the flag existed (a BARE 32-byte
    /// sealed secret, no flag byte) reads as ALREADY distributed. Those
    /// groups are provisioned and working today — re-broadcasting for every
    /// one of them on upgrade would be a pointless journal write.
    func testLegacyRowWithoutTheFlagIsTreatedAsDistributed() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let broadcaster = FlakyBroadcastRecorder(shouldFail: false)

        let identity = try makeIdentity(0x34)
        let (group, groupIdHex) = try makeOwnerGroup(identity)

        // The pre-flag on-disk shape: the sealed plaintext IS the secret.
        let legacy = try XCTUnwrap(PushGroupRouting.mintSecret())
        store.putRawRecordForTesting(
            accountIdB64: "acctLegacy", groupIdHex: groupIdHex, plaintext: legacy)

        // It reads back intact, and as already distributed.
        let record = try XCTUnwrap(
            store.getRecord(accountIdB64: "acctLegacy", groupIdHex: groupIdHex))
        XCTAssertEqual(record.secret, legacy, "a legacy row must stay readable")
        XCTAssertTrue(record.distributed, "a legacy row is treated as distributed")

        await routing.ensureProvisioned(
            accountIdB64: "acctLegacy", groupIdHex: groupIdHex, group: group,
            identity: identity, broadcast: { try broadcaster.send($0) })

        XCTAssertTrue(
            broadcaster.envelopes.isEmpty,
            "upgrading must not re-broadcast for every already-working group")
        XCTAssertEqual(
            store.getSecret(accountIdB64: "acctLegacy", groupIdHex: groupIdHex), legacy,
            "and must not re-mint either")
        // The owner still subscribes under the legacy secret's push_gid.
        XCTAssertEqual(
            spy.targets, [try XCTUnwrap(PushGroupRouting.derivePushGid(secret: legacy))])
    }

    /// A secret that ARRIVED over the journal is trivially already on the
    /// wire, so a receiver stores it settled and never enters the owner's
    /// re-broadcast branch.
    func testReceivedSecretIsStoredDistributed() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let (group, groupIdHex, ownerLeaf, _) = try makeTwoMemberGroup(
            owner: try makeIdentity(0x36), member: try makeIdentity(0x37))

        let secret = try XCTUnwrap(PushGroupRouting.mintSecret())
        await routing.receiveSecret(
            accountIdB64: "acctPeer", groupIdHex: groupIdHex, secret: secret,
            group: group, senderLeaf: ownerLeaf)

        let record = try XCTUnwrap(
            store.getRecord(accountIdB64: "acctPeer", groupIdHex: groupIdHex))
        XCTAssertEqual(record.secret, secret)
        XCTAssertTrue(
            record.distributed,
            "an arrived envelope is proof the secret is on the wire")
    }

    /// The `storedUnreadable` three-way distinction must survive the new
    /// record framing: an uninterpretable row is UNREADABLE (`getRecord`
    /// nil while `hasSecret` stays true), never a provable absence — that is
    /// what the receive gate's fail-closed admin-seed rule keys off. And an
    /// owner holding one must not re-mint.
    func testUnreadableRecordIsNotAbsentAndOwnerDoesNotReMint() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let broadcaster = FlakyBroadcastRecorder(shouldFail: false)

        let identity = try makeIdentity(0x38)
        let (group, groupIdHex) = try makeOwnerGroup(identity)

        // Neither a legacy bare secret nor a valid flagged record.
        store.putRawRecordForTesting(
            accountIdB64: "acctOwner", groupIdHex: groupIdHex,
            plaintext: Data(repeating: 0x09, count: 7))

        XCTAssertTrue(
            store.hasSecret(accountIdB64: "acctOwner", groupIdHex: groupIdHex),
            "the key-free presence check still sees the row")
        XCTAssertNil(store.getRecord(accountIdB64: "acctOwner", groupIdHex: groupIdHex))
        XCTAssertNil(store.getSecret(accountIdB64: "acctOwner", groupIdHex: groupIdHex))

        // An unknown flag byte is unreadable too (never a guessed flag).
        var unknownFlag = Data([0x02])
        unknownFlag.append(Data(repeating: 0xAB, count: 32))
        store.putRawRecordForTesting(
            accountIdB64: "acctOwner", groupIdHex: "otherGrp", plaintext: unknownFlag)
        XCTAssertTrue(store.hasSecret(accountIdB64: "acctOwner", groupIdHex: "otherGrp"))
        XCTAssertNil(store.getRecord(accountIdB64: "acctOwner", groupIdHex: "otherGrp"))

        // The owner falls through: no re-mint (that would diverge the
        // group's routing id) and no broadcast (the envelope is not
        // recomposable without the secret).
        await routing.ensureProvisioned(
            accountIdB64: "acctOwner", groupIdHex: groupIdHex, group: group,
            identity: identity, broadcast: { try broadcaster.send($0) })
        XCTAssertTrue(broadcaster.envelopes.isEmpty)
        XCTAssertNil(store.getSecret(accountIdB64: "acctOwner", groupIdHex: groupIdHex))
        XCTAssertTrue(spy.targets.isEmpty, "an unreadable secret defers the subscription")
    }

    // MARK: - durable `distributed` flag: the failed-REDISTRIBUTE repair

    /// **The regression this closes.** The `distributed` flag is per-GROUP,
    /// not per-member. The owner mints and broadcasts successfully (flag
    /// `true`), a member is added, and the post-add `redistribute` FAILS
    /// (offline / DS error). With the flag left at `true` no sweep would ever
    /// re-send, so that new member never learns the secret and defers push
    /// forever — the same permanent-no-push bug as the failed-mint case,
    /// scoped to one member. A failed redistribute must un-settle the row.
    func testFailedRedistributeReBroadcastsOnTheNextProvision() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let broadcaster = FlakyBroadcastRecorder(shouldFail: false)

        let identity = try makeIdentity(0x3A)
        let (group, groupIdHex) = try makeOwnerGroup(identity)

        // Mint + broadcast LANDS — the group is healthy and settled.
        await routing.ensureProvisioned(
            accountIdB64: "acctOwner", groupIdHex: groupIdHex, group: group,
            identity: identity, broadcast: { try broadcaster.send($0) })
        XCTAssertEqual(broadcaster.envelopes.count, 1)
        let minted = try XCTUnwrap(
            store.getRecord(accountIdB64: "acctOwner", groupIdHex: groupIdHex))
        XCTAssertTrue(minted.distributed, "a landed mint settles the flag")

        // A member is added -> redistribute, and THAT broadcast fails.
        broadcaster.shouldFail = true
        await routing.redistribute(
            accountIdB64: "acctOwner", groupIdHex: groupIdHex,
            broadcast: { try broadcaster.send($0) })

        XCTAssertEqual(broadcaster.envelopes.count, 2, "the redistribute attempted a broadcast")
        XCTAssertEqual(
            store.getRecord(accountIdB64: "acctOwner", groupIdHex: groupIdHex)?.distributed,
            false,
            "a failed redistribute must un-settle the row, else the newly-added "
                + "member is never re-sent the secret")
        // The secret itself is untouched — only the flag moved.
        XCTAssertEqual(
            store.getSecret(accountIdB64: "acctOwner", groupIdHex: groupIdHex), minted.secret)

        // The next provision (relaunch / chat re-open) picks the row up and
        // re-broadcasts the SAME secret, then settles it.
        broadcaster.shouldFail = false
        await routing.ensureProvisioned(
            accountIdB64: "acctOwner", groupIdHex: groupIdHex, group: group,
            identity: identity, broadcast: { try broadcaster.send($0) })

        XCTAssertEqual(
            broadcaster.envelopes.count, 3,
            "a redistribute-failed row must be RE-BROADCAST on the next provision")
        XCTAssertEqual(
            broadcaster.envelopes[0], broadcaster.envelopes[2],
            "the retry carries the same secret — a re-mint would diverge the routing id")
        XCTAssertEqual(
            store.getRecord(accountIdB64: "acctOwner", groupIdHex: groupIdHex)?.distributed,
            true, "the landed retry settles the flag again")

        // ... and a further provision is silent.
        await routing.ensureProvisioned(
            accountIdB64: "acctOwner", groupIdHex: groupIdHex, group: group,
            identity: identity, broadcast: { try broadcaster.send($0) })
        XCTAssertEqual(broadcaster.envelopes.count, 3)
    }

    /// The other half: a redistribute that LANDS leaves the row settled, so
    /// the healthy add-member path costs no extra journal write.
    func testSuccessfulRedistributeLeavesTheSecretDistributed() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let broadcaster = FlakyBroadcastRecorder(shouldFail: false)

        let identity = try makeIdentity(0x3C)
        let (group, groupIdHex) = try makeOwnerGroup(identity)

        await routing.ensureProvisioned(
            accountIdB64: "acctOwner", groupIdHex: groupIdHex, group: group,
            identity: identity, broadcast: { try broadcaster.send($0) })
        XCTAssertEqual(broadcaster.envelopes.count, 1)

        await routing.redistribute(
            accountIdB64: "acctOwner", groupIdHex: groupIdHex,
            broadcast: { try broadcaster.send($0) })
        XCTAssertEqual(broadcaster.envelopes.count, 2)
        XCTAssertEqual(
            store.getRecord(accountIdB64: "acctOwner", groupIdHex: groupIdHex)?.distributed,
            true, "a landed redistribute keeps the row settled")

        await routing.ensureProvisioned(
            accountIdB64: "acctOwner", groupIdHex: groupIdHex, group: group,
            identity: identity, broadcast: { try broadcaster.send($0) })
        XCTAssertEqual(
            broadcaster.envelopes.count, 2,
            "a settled row must not be re-broadcast after a successful redistribute")
    }

    /// **The other half of the retry hole.** ADR-0022 lets an ADMIN add a
    /// member too, so a NON-owner reaches `redistribute` and marks its OWN row
    /// pending when that broadcast fails. Gating the re-broadcast branch on
    /// `ownRole == .owner` made that flag inert — and nothing else would ever
    /// repair it, because the owner's row is still settled and the owner never
    /// learns the admin's broadcast failed. The newly-added member would get no
    /// push, permanently.
    ///
    /// So the retry branch is open to any secret HOLDER. That grants no new
    /// authority: it re-sends the exact stored bytes, and the ADR-0042 receive
    /// gate accepts a byte-identical secret from ANY sender *before* consulting
    /// authority — a member could always have replayed an identical envelope.
    /// Only the MINT stays owner-only (see
    /// ``testNonOwnerMemberWithoutSecretNeitherMintsNorBroadcasts``), because
    /// that is where the random value is CHOSEN and two mints would diverge the
    /// group's `push_gid`.
    func testNonOwnerPendingRowIsReBroadcast() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let broadcaster = FlakyBroadcastRecorder(shouldFail: true)

        let ownerIdentity = try makeIdentity(0x3E)
        let memberIdentity = try makeIdentity(0x3F)
        let (group, groupIdHex, ownerLeaf, _) = try makeTwoMemberGroup(
            owner: ownerIdentity, member: memberIdentity)
        // Sanity: this device really is NOT the ADR-0022 owner, so the branch
        // under test is genuinely the non-owner one.
        XCTAssertFalse(
            try group.ownRole(identity: memberIdentity) == Role.owner,
            "this device must not be the ADR-0022 owner for this test to mean anything")

        // This device is the non-owner MEMBER; it received the secret over the
        // journal, so its row is stored settled.
        let secret = try XCTUnwrap(PushGroupRouting.mintSecret())
        await routing.receiveSecret(
            accountIdB64: "acctMember", groupIdHex: groupIdHex, secret: secret,
            group: group, senderLeaf: ownerLeaf)
        XCTAssertEqual(
            store.getRecord(accountIdB64: "acctMember", groupIdHex: groupIdHex)?.distributed,
            true)

        // It adds a member (ADR-0022 admin add) -> redistribute -> fails.
        await routing.redistribute(
            accountIdB64: "acctMember", groupIdHex: groupIdHex,
            broadcast: { try broadcaster.send($0) })
        XCTAssertEqual(broadcaster.envelopes.count, 1)
        XCTAssertEqual(
            store.getRecord(accountIdB64: "acctMember", groupIdHex: groupIdHex)?.distributed,
            false, "the failed redistribute un-settled this device's row")

        // The next provision (relaunch / chat re-open) RETRIES on this
        // non-owner device — the whole point: nobody else is watching this row.
        broadcaster.shouldFail = false
        await routing.ensureProvisioned(
            accountIdB64: "acctMember", groupIdHex: groupIdHex, group: group,
            identity: memberIdentity, broadcast: { try broadcaster.send($0) })
        XCTAssertEqual(
            broadcaster.envelopes.count, 2,
            "a pending row must be re-broadcast by ANY holder, owner or not — "
                + "an owner-only retry strands the member the admin just added")
        // `first`/`last` rather than subscripts: a regression that skips the
        // retry leaves ONE envelope, and an out-of-range subscript would kill
        // the whole test process instead of reporting the count assertion above.
        XCTAssertEqual(
            broadcaster.envelopes.first, broadcaster.envelopes.last,
            "the retry carries the SAME secret — re-sending identical bytes is "
                + "what makes it authority-free (the receive gate accepts it "
                + "from anyone); a fresh mint would diverge the routing id")
        XCTAssertEqual(
            store.getRecord(accountIdB64: "acctMember", groupIdHex: groupIdHex)?.distributed,
            true, "the landed retry settles the row")
        XCTAssertEqual(
            store.getSecret(accountIdB64: "acctMember", groupIdHex: groupIdHex), secret,
            "the retry must not disturb the secret")

        // ... and a further provision is silent (the retry loop terminates).
        await routing.ensureProvisioned(
            accountIdB64: "acctMember", groupIdHex: groupIdHex, group: group,
            identity: memberIdentity, broadcast: { try broadcaster.send($0) })
        XCTAssertEqual(broadcaster.envelopes.count, 2)

        // It subscribed under the secret it holds on each provision.
        let pushGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: secret))
        XCTAssertEqual(spy.targets, [pushGid, pushGid, pushGid])
    }

    /// The half of the split guard that did NOT move: a non-owner MEMBER of a
    /// real group that holds NO secret still never mints and never broadcasts.
    /// The mint is where the 32 RANDOM bytes are chosen, so a second minter
    /// would diverge the group's `push_gid` and break routing for everyone —
    /// that branch stays owner-only. Such a member simply defers push until the
    /// distribution envelope arrives.
    ///
    /// (``testNonOwnerNeverMints`` pins the same for an identity that is not in
    /// the roster at all; this one pins it for a genuine member, which is the
    /// case the split guard actually has to keep straight.)
    func testNonOwnerMemberWithoutSecretNeitherMintsNorBroadcasts() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let routing = makeRouting(store: store, spy: spy)
        let recorder = BroadcastRecorder()

        let ownerIdentity = try makeIdentity(0x4A)
        let memberIdentity = try makeIdentity(0x4B)
        let (group, groupIdHex, _, _) = try makeTwoMemberGroup(
            owner: ownerIdentity, member: memberIdentity)
        XCTAssertFalse(
            try group.ownRole(identity: memberIdentity) == Role.owner,
            "this device must not be the ADR-0022 owner for this test to mean anything")

        await routing.ensureProvisioned(
            accountIdB64: "acctMember", groupIdHex: groupIdHex, group: group,
            identity: memberIdentity, broadcast: { bytes in recorder.record(bytes) })

        XCTAssertTrue(
            recorder.envelopes.isEmpty,
            "a non-owner member holding no secret must never mint or broadcast")
        XCTAssertFalse(
            store.hasSecret(accountIdB64: "acctMember", groupIdHex: groupIdHex),
            "nothing may be persisted — a second mint would diverge the push_gid")
        XCTAssertTrue(spy.targets.isEmpty, "no secret held -> push deferred, never group_id")

        // Repeat provisions stay silent (no drift into minting on a later open).
        await routing.ensureProvisioned(
            accountIdB64: "acctMember", groupIdHex: groupIdHex, group: group,
            identity: memberIdentity, broadcast: { bytes in recorder.record(bytes) })
        XCTAssertTrue(recorder.envelopes.isEmpty)
        XCTAssertFalse(store.hasSecret(accountIdB64: "acctMember", groupIdHex: groupIdHex))
    }

    /// `markPending` mirrors `markDistributed`: a no-op on an absent record,
    /// and bound by the same `matching:` discipline so a failed OLD-secret
    /// broadcast can never un-settle a NEWLY rotated secret that did travel.
    func testMarkPendingIsANoOpOnAnAbsentRecordAndHonoursMatching() throws {
        let store = makeStore()
        let stored = try XCTUnwrap(PushGroupRouting.mintSecret())
        let other = try XCTUnwrap(PushGroupRouting.mintSecret())
        XCTAssertNotEqual(stored, other)

        // Absent record -> no-op, and nothing is created.
        XCTAssertFalse(store.markPending(
            accountIdB64: "acct", groupIdHex: "missing", matching: stored))
        XCTAssertFalse(store.hasSecret(accountIdB64: "acct", groupIdHex: "missing"))
        XCTAssertNil(store.getRecord(accountIdB64: "acct", groupIdHex: "missing"))

        let gidHex = PushGroupRouting.hex(
            try XCTUnwrap(PushGroupRouting.derivePushGid(secret: stored)))
        store.putRouting(
            accountIdB64: "acct", groupIdHex: "grp", secret: stored, pushGidHex: gidHex,
            distributed: true)

        // A DIFFERENT secret's failed broadcast must not un-settle this row.
        XCTAssertFalse(store.markPending(
            accountIdB64: "acct", groupIdHex: "grp", matching: other))
        XCTAssertEqual(
            store.getRecord(accountIdB64: "acct", groupIdHex: "grp")?.distributed, true)

        // The matching one does, and stays idempotent.
        XCTAssertTrue(store.markPending(
            accountIdB64: "acct", groupIdHex: "grp", matching: stored))
        XCTAssertTrue(store.markPending(
            accountIdB64: "acct", groupIdHex: "grp", matching: stored))
        let record = try XCTUnwrap(store.getRecord(accountIdB64: "acct", groupIdHex: "grp"))
        XCTAssertFalse(record.distributed)
        XCTAssertEqual(record.secret, stored, "un-settling must not disturb the secret")

        // And the row can be settled again afterwards (the flag is a toggle,
        // not a one-way latch) — what makes the retry loop terminate.
        XCTAssertTrue(store.markDistributed(
            accountIdB64: "acct", groupIdHex: "grp", matching: stored))
        XCTAssertEqual(
            store.getRecord(accountIdB64: "acct", groupIdHex: "grp")?.distributed, true)
    }

    /// The flag can only ever describe the secret that actually travelled:
    /// `markDistributed` is a no-op when the stored secret has since
    /// changed (an owner rotation racing an in-flight broadcast).
    func testMarkDistributedOnlyMatchesTheBroadcastSecret() throws {
        let store = makeStore()
        let stored = try XCTUnwrap(PushGroupRouting.mintSecret())
        let other = try XCTUnwrap(PushGroupRouting.mintSecret())
        XCTAssertNotEqual(stored, other)
        let gidHex = PushGroupRouting.hex(
            try XCTUnwrap(PushGroupRouting.derivePushGid(secret: stored)))

        store.putRouting(
            accountIdB64: "acct", groupIdHex: "grp", secret: stored, pushGidHex: gidHex,
            distributed: false)

        // A different secret's broadcast must not settle this row.
        XCTAssertFalse(store.markDistributed(
            accountIdB64: "acct", groupIdHex: "grp", matching: other))
        XCTAssertEqual(
            store.getRecord(accountIdB64: "acct", groupIdHex: "grp")?.distributed, false)

        // The matching one does, and stays idempotent.
        XCTAssertTrue(store.markDistributed(
            accountIdB64: "acct", groupIdHex: "grp", matching: stored))
        XCTAssertTrue(store.markDistributed(
            accountIdB64: "acct", groupIdHex: "grp", matching: stored))
        let record = try XCTUnwrap(store.getRecord(accountIdB64: "acct", groupIdHex: "grp"))
        XCTAssertTrue(record.distributed)
        XCTAssertEqual(record.secret, stored, "settling must not disturb the secret")
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
        // ADMIN authored the change -> ACCEPT (ADR-0043 ratified option (b):
        // ADR-0022 lets an admin commit the Remove, so an admin must be able
        // to retire the evicted member's routing id).
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: b, stored: a, senderKey: adminKey, authority: roles),
            .accept)
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

        // --- ADMIN WRITES OVER AN UNREADABLE STORE -------------------------
        // `stored == nil` also happens when the store could not be READ (no
        // device wrap key / AEAD open failed), which makes the write a ROTATE
        // rather than a provable SEED. Under ADR-0043 option (b) the admin is
        // authorised either way, so this no longer changes the outcome — the
        // distinction survives only in the diagnostic log.
        XCTAssertEqual(
            PushGroupRouting.classifyIncomingSecret(
                incoming: a, stored: nil, storedUnreadable: true,
                senderKey: adminKey, authority: roles),
            .accept)
        // The OWNER may still write over an unreadable store — the repair path.
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
        // ... and a PLAIN MEMBER stays refused in every one of those states —
        // the admin widening must not leak downward.
        for unreadable in [true, false] {
            XCTAssertEqual(
                PushGroupRouting.classifyIncomingSecret(
                    incoming: b, stored: a, storedUnreadable: unreadable,
                    senderKey: memberKey, authority: roles),
                .reject)
            XCTAssertEqual(
                PushGroupRouting.classifyIncomingSecret(
                    incoming: a, stored: nil, storedUnreadable: unreadable,
                    senderKey: memberKey, authority: roles),
                .reject)
        }

        // --- MULTI-ADMIN: every listed admin may seed AND rotate -----------
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
            .accept)
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

    /// **ADR-0043 ratified option (b) — the gate widening.** This test used to
    /// pin the opposite contract ("an admin may seed a hole but must NOT be
    /// able to rotate"). ADR-0022 lets an ADMIN commit the Remove that makes
    /// an ADR-0043 rotation necessary, so refusing the overwrite left an
    /// admin able to evict a member but unable to retire that member's
    /// routing id — the eviction completed with the ex-member's wake stream
    /// intact. The admin's changed secret is now APPLIED, and the device
    /// re-subscribes under the new id + retires the old one.
    func testReceiveAdminMayRotateAHeldSecret() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let unsub = UnsubscribeSpy()
        let routing = makeRouting(store: store, spy: spy, unsubscribeSpy: unsub)
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

        let rotated = try XCTUnwrap(PushGroupRouting.mintSecret())
        XCTAssertNotEqual(rotated, ownerSecret)
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: rotated,
            group: group, senderLeaf: adminLeaf)

        let rotatedGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: rotated))
        XCTAssertEqual(
            store.getSecret(accountIdB64: "acct", groupIdHex: groupIdHex), rotated,
            "ADR-0043 option (b): an admin MAY overwrite the group's routing secret")
        XCTAssertEqual(spy.targets, [ownerPushGid, rotatedGid],
                       "and the device re-subscribes under the rotated id")
        XCTAssertEqual(unsub.targets, [ownerPushGid],
                       "and retires its OWN row under the superseded id")
        XCTAssertNil(
            store.lookupRoute(pushGidHex: PushGroupRouting.hex(ownerPushGid)),
            "the superseded reverse-index row is deleted, not accumulated")
        XCTAssertNotNil(store.lookupRoute(pushGidHex: PushGroupRouting.hex(rotatedGid)))
    }

    /// The tier boundary the widening did NOT cross: a PLAIN member's changed
    /// secret is still refused whole — no persist, no re-subscribe, and
    /// nothing torn down (the device keeps the id it is on).
    func testReceivePlainMemberChangedSecretIsStillRejected() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let unsub = UnsubscribeSpy()
        let routing = makeRouting(store: store, spy: spy, unsubscribeSpy: unsub)
        let (group, groupIdHex, ownerLeaf, _, memberLeaf) = try makeGroupWithAdmin(
            owner: try makeIdentity(0x84),
            admin: try makeIdentity(0x85),
            member: try makeIdentity(0x86))

        let ownerSecret = try XCTUnwrap(PushGroupRouting.mintSecret())
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: ownerSecret,
            group: group, senderLeaf: ownerLeaf)
        let ownerPushGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: ownerSecret))

        let rogue = try XCTUnwrap(PushGroupRouting.mintSecret())
        XCTAssertNotEqual(rogue, ownerSecret)
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: rogue,
            group: group, senderLeaf: memberLeaf)

        XCTAssertEqual(
            store.getSecret(accountIdB64: "acct", groupIdHex: groupIdHex), ownerSecret,
            "widening the gate to admins must not have widened it to everyone")
        XCTAssertEqual(spy.targets, [ownerPushGid], "no re-subscribe on a refused secret")
        XCTAssertTrue(unsub.targets.isEmpty, "a refused secret retires nothing")
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

    // MARK: - ADR-0043: the per-gid route delete (new store surface)

    /// The store asymmetry ADR-0043 closes: the secret row is overwritten in
    /// place by a rotation, but a route row is keyed by `push_gid` so a
    /// rotation only ADDS one. Without a per-gid delete every rotation grows
    /// the reverse map by a dead entry nothing will ever notify again.
    func testRemoveRouteRetiresOneRowAndLeavesTheSecretIntact() throws {
        let store = makeStore()
        let secret = try XCTUnwrap(PushGroupRouting.mintSecret())
        let other = try XCTUnwrap(PushGroupRouting.mintSecret())
        let gidHex = PushGroupRouting.hex(
            try XCTUnwrap(PushGroupRouting.derivePushGid(secret: secret)))
        let otherGidHex = PushGroupRouting.hex(
            try XCTUnwrap(PushGroupRouting.derivePushGid(secret: other)))

        store.putRouting(
            accountIdB64: "acct", groupIdHex: "grp", secret: secret, pushGidHex: gidHex,
            distributed: true)
        store.putRouting(
            accountIdB64: "acct", groupIdHex: "grp2", secret: other, pushGidHex: otherGidHex,
            distributed: true)

        store.removeRoute(pushGidHex: gidHex)

        XCTAssertNil(store.lookupRoute(pushGidHex: gidHex), "the named row is gone")
        XCTAssertNotNil(
            store.lookupRoute(pushGidHex: otherGidHex),
            "and ONLY the named row — a sibling group's route survives")
        // Scoped to the reverse index: it must never strand a group by
        // deleting the material a live push_gid is derived from.
        XCTAssertEqual(store.getSecret(accountIdB64: "acct", groupIdHex: "grp"), secret)
        XCTAssertTrue(store.hasSecret(accountIdB64: "acct", groupIdHex: "grp"))
    }

    /// A rotation on a device that had not yet indexed the old gid must not
    /// have to check first.
    func testRemoveRouteOnAnAbsentRowIsANoOp() throws {
        let store = makeStore()
        store.removeRoute(pushGidHex: String(repeating: "ab", count: 32))
        XCTAssertNil(store.lookupRoute(pushGidHex: String(repeating: "ab", count: 32)))
    }

    // MARK: - ADR-0043: the pure membership-shrink decision core

    /// The verbatim port of the web `membershipRotationDecision` matrix.
    func testMembershipRotationDecisionMatrix() {
        let a = String(repeating: "aa", count: 32)
        let b = String(repeating: "bb", count: 32)
        let c = String(repeating: "cc", count: 32)

        // A SHRINK rotates for the OWNER ...
        XCTAssertEqual(
            PushGroupRouting.membershipRotationDecision(
                previousMemberKeys: [a, b, c], currentMemberKeys: [a, b], ownRole: .owner),
            .rotate)
        // ... and for an ADMIN (ADR-0043 ratified option (b)): ADR-0022 lets
        // an admin commit the Remove, so an admin must be able to retire the
        // evicted member's routing id, else the eviction completes with the
        // ex-member's wake stream intact.
        XCTAssertEqual(
            PushGroupRouting.membershipRotationDecision(
                previousMemberKeys: [a, b, c], currentMemberKeys: [a, b], ownRole: .admin),
            .rotate)
        // ... but NOT for a plain member.
        XCTAssertEqual(
            PushGroupRouting.membershipRotationDecision(
                previousMemberKeys: [a, b, c], currentMemberKeys: [a, b], ownRole: .member),
            .skipNotAuthorized)
        // ... and NOT in a group with no role table: no role table ⇒ no owner
        // ⇒ nobody ever minted a secret for it, so there is nothing to
        // rotate; and letting every member rotate would have them each mint a
        // divergent secret.
        XCTAssertEqual(
            PushGroupRouting.membershipRotationDecision(
                previousMemberKeys: [a, b, c], currentMemberKeys: [a, b], ownRole: nil),
            .skipNotAuthorized)

        // GROWTH (an add) never rotates — the added member is SUPPOSED to
        // route, and rotating on the commonest membership event would churn
        // the id for zero privacy gain.
        XCTAssertEqual(
            PushGroupRouting.membershipRotationDecision(
                previousMemberKeys: [a, b], currentMemberKeys: [a, b, c], ownRole: .owner),
            .skipNoShrink)
        // An UNCHANGED roster never rotates (an ordinary commit / a bare poll).
        XCTAssertEqual(
            PushGroupRouting.membershipRotationDecision(
                previousMemberKeys: [a, b], currentMemberKeys: [a, b], ownRole: .owner),
            .skipNoShrink)

        // A remove+add in ONE poll batch still rotates: the COUNT is equal,
        // the SET is not. This is why the diff is over member signature keys
        // and not over counts or leaf indices — OpenMLS reuses a vacated leaf
        // for the next Add, so both of those look unchanged while a member
        // really was evicted.
        XCTAssertEqual(
            PushGroupRouting.membershipRotationDecision(
                previousMemberKeys: [a, b], currentMemberKeys: [a, c], ownRole: .owner),
            .rotate)

        // An UNKNOWN snapshot never rotates — a torn `members()` read is not
        // "everyone left".
        XCTAssertEqual(
            PushGroupRouting.membershipRotationDecision(
                previousMemberKeys: nil, currentMemberKeys: [a], ownRole: .owner),
            .skipUnknownMembership)
        XCTAssertEqual(
            PushGroupRouting.membershipRotationDecision(
                previousMemberKeys: [a, b], currentMemberKeys: nil, ownRole: .owner),
            .skipUnknownMembership)
        // A FIRST observation (empty baseline) has nothing to diff against.
        XCTAssertEqual(
            PushGroupRouting.membershipRotationDecision(
                previousMemberKeys: [], currentMemberKeys: [], ownRole: .owner),
            .skipUnknownMembership)
    }

    /// `memberSignatureKeys` hexes a REAL roster, and the shrink it feeds is
    /// the real one: removing a leaf from a live 3-member group produces a
    /// set difference the decision core reads as `rotate`.
    func testMemberSignatureKeysOnARealGroupAndARealRemoval() throws {
        let owner = try makeIdentity(0x66)
        let (group, _) = try makeOwnerGroup(owner)
        _ = try group.addMemberFromWire(
            identity: owner, keyPackage: try (try makeIdentity(0x67)).createKeyPackage())
        _ = try group.addMemberFromWire(
            identity: owner, keyPackage: try (try makeIdentity(0x68)).createKeyPackage())

        let before = try XCTUnwrap(PushGroupRouting.memberSignatureKeys(group: group))
        XCTAssertEqual(before.count, 3)
        XCTAssertEqual(
            before, try group.members().map { PushGroupRouting.hex($0.signaturePublicKey) })

        // A REAL removal commit, then the post-removal snapshot.
        let ownerKey = try XCTUnwrap(try group.groupRoles()).owner
        let victim = try XCTUnwrap(
            try group.members().first { $0.signaturePublicKey != ownerKey })
        _ = try group.removeMember(identity: owner, leafIndex: victim.leafIndex)
        let after = try XCTUnwrap(PushGroupRouting.memberSignatureKeys(group: group))

        XCTAssertEqual(after.count, 2)
        XCTAssertEqual(
            PushGroupRouting.membershipRotationDecision(
                previousMemberKeys: before, currentMemberKeys: after, ownRole: .owner),
            .rotate,
            "a real MLS removal must be visible to the diff")
    }

    // MARK: - ADR-0043: the rotation flow

    /// Seed a settled (already-distributed) secret + its reverse-index row.
    private func seedSettled(
        store: PushRoutingStore,
        accountIdB64: String,
        groupIdHex: String,
        secret: Data
    ) throws {
        let gidHex = PushGroupRouting.hex(
            try XCTUnwrap(PushGroupRouting.derivePushGid(secret: secret)))
        store.putRouting(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, secret: secret,
            pushGidHex: gidHex, distributed: true)
    }

    /// The whole rotation, end to end: a genuinely NEW secret is persisted +
    /// broadcast, the NEW gid is subscribed + indexed, and the OLD gid is
    /// unsubscribed + de-indexed.
    func testOwnerRotationRekeysAndRetiresTheOldPushGid() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let unsub = UnsubscribeSpy()
        let routing = makeRouting(store: store, spy: spy, unsubscribeSpy: unsub)
        let recorder = BroadcastRecorder()

        let old = try XCTUnwrap(PushGroupRouting.mintSecret())
        try seedSettled(store: store, accountIdB64: "acct", groupIdHex: "grp", secret: old)
        let oldGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: old))

        let outcome = await routing.rotateSecret(
            accountIdB64: "acct", groupIdHex: "grp", ownRole: .owner,
            broadcast: { bytes in recorder.record(bytes) })
        XCTAssertEqual(outcome, .rotated)

        // A genuinely NEW secret replaced the old one, in place.
        let stored = try XCTUnwrap(store.getSecret(accountIdB64: "acct", groupIdHex: "grp"))
        XCTAssertEqual(stored.count, 32)
        XCTAssertNotEqual(stored, old)
        let newGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: stored))
        XCTAssertNotEqual(newGid, oldGid)

        // Redistributed to the group — the envelope carries the NEW secret ...
        XCTAssertEqual(recorder.envelopes.count, 1)
        XCTAssertEqual(
            PushGroupRouting.tryDecodeSecretEnvelope(try XCTUnwrap(recorder.envelopes.first)),
            stored)
        // ... and is confirmed on the wire.
        XCTAssertEqual(
            store.getRecord(accountIdB64: "acct", groupIdHex: "grp")?.distributed, true)

        // Subscribed under the NEW gid.
        XCTAssertEqual(spy.targets, [newGid])
        // Unsubscribed from the OLD gid — the FIRST live call site of
        // `PushClient.unsubscribeGroup`, dead code before this ADR.
        XCTAssertEqual(unsub.targets, [oldGid])

        // The reverse index carries the new entry and NOT the retired one:
        // without the delete every rotation grows the map by a dead row.
        XCTAssertNotNil(store.lookupRoute(pushGidHex: PushGroupRouting.hex(newGid)))
        XCTAssertNil(store.lookupRoute(pushGidHex: PushGroupRouting.hex(oldGid)))
    }

    /// ADR-0043 ratified option (b): an ADMIN may rotate too.
    func testAdminMayRotate() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let unsub = UnsubscribeSpy()
        let routing = makeRouting(store: store, spy: spy, unsubscribeSpy: unsub)
        let recorder = BroadcastRecorder()

        let old = try XCTUnwrap(PushGroupRouting.mintSecret())
        try seedSettled(store: store, accountIdB64: "acct", groupIdHex: "grp", secret: old)

        let outcome = await routing.rotateSecret(
            accountIdB64: "acct", groupIdHex: "grp", ownRole: .admin,
            broadcast: { bytes in recorder.record(bytes) })
        XCTAssertEqual(outcome, .rotated)
        XCTAssertEqual(recorder.envelopes.count, 1)
        XCTAssertEqual(unsub.targets.count, 1)
        XCTAssertNotEqual(store.getSecret(accountIdB64: "acct", groupIdHex: "grp"), old)
    }

    /// A plain member (and a group with no role table) never rotates:
    /// nothing minted, nothing sent, nothing torn down.
    func testPlainMemberNeverRotates() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let unsub = UnsubscribeSpy()
        let routing = makeRouting(store: store, spy: spy, unsubscribeSpy: unsub)
        let recorder = BroadcastRecorder()

        let old = try XCTUnwrap(PushGroupRouting.mintSecret())
        try seedSettled(store: store, accountIdB64: "acct", groupIdHex: "grp", secret: old)

        for role: Role? in [.member, nil] {
            let outcome = await routing.rotateSecret(
                accountIdB64: "acct", groupIdHex: "grp", ownRole: role,
                broadcast: { bytes in recorder.record(bytes) })
            XCTAssertEqual(outcome, .skipNotAuthorized)
        }
        XCTAssertTrue(recorder.envelopes.isEmpty)
        XCTAssertTrue(unsub.targets.isEmpty)
        XCTAssertTrue(spy.targets.isEmpty)
        XCTAssertEqual(
            store.getSecret(accountIdB64: "acct", groupIdHex: "grp"), old,
            "a plain member still ends up on the new id — via the gated "
                + "receive path, when the rotator's envelope arrives")
    }

    /// **Rotation NEVER mints a first secret.** With nothing held, an admin
    /// rotating here would become a SECOND minter and diverge the group's
    /// `push_gid` — exactly what the owner-mints rule exists to prevent.
    /// ADR-0043 option (b) widened OVERWRITE authority, never MINT authority.
    func testRotationNeverMintsAFirstSecret() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let unsub = UnsubscribeSpy()
        let routing = makeRouting(store: store, spy: spy, unsubscribeSpy: unsub)
        let recorder = BroadcastRecorder()

        for role: Role in [.admin, .owner] {
            let outcome = await routing.rotateSecret(
                accountIdB64: "acct", groupIdHex: "emptyGrp", ownRole: role,
                broadcast: { bytes in recorder.record(bytes) })
            XCTAssertEqual(outcome, .skipNoSecret)
        }
        XCTAssertTrue(recorder.envelopes.isEmpty)
        XCTAssertFalse(store.hasSecret(accountIdB64: "acct", groupIdHex: "emptyGrp"))
        XCTAssertTrue(spy.targets.isEmpty)
        XCTAssertTrue(unsub.targets.isEmpty)
    }

    /// An UNREADABLE row cannot be rotated either: the OLD id could never be
    /// named from bytes we cannot read, so a rotation would strand it
    /// subscribed forever.
    func testRotationSkipsAnUnreadableRow() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let unsub = UnsubscribeSpy()
        let routing = makeRouting(store: store, spy: spy, unsubscribeSpy: unsub)
        let recorder = BroadcastRecorder()

        store.putRawRecordForTesting(
            accountIdB64: "acct", groupIdHex: "grp", plaintext: Data(repeating: 0x09, count: 7))
        XCTAssertTrue(store.hasSecret(accountIdB64: "acct", groupIdHex: "grp"))
        XCTAssertNil(store.getSecret(accountIdB64: "acct", groupIdHex: "grp"))

        let outcome = await routing.rotateSecret(
            accountIdB64: "acct", groupIdHex: "grp", ownRole: .owner,
            broadcast: { bytes in recorder.record(bytes) })
        XCTAssertEqual(outcome, .skipNoSecret)
        XCTAssertTrue(recorder.envelopes.isEmpty)
        XCTAssertTrue(unsub.targets.isEmpty)
        XCTAssertNil(store.getSecret(accountIdB64: "acct", groupIdHex: "grp"),
                     "and the unreadable row is not overwritten by a fresh mint")
    }

    /// **Ordering (b): the OLD secret is read BEFORE the overwrite.** The
    /// secret row is keyed `(account, group)` and clobbered in place. If the
    /// rotation persisted first, the id being retired could no longer be
    /// named: an undeletable orphan that keeps receiving wakes — the exact
    /// defect this ADR fixes. Pinned by asserting the unsubscribe TARGET.
    func testTheTeardownTargetsTheOldGidNotTheNewOne() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let unsub = UnsubscribeSpy()
        let routing = makeRouting(store: store, spy: spy, unsubscribeSpy: unsub)
        let recorder = BroadcastRecorder()

        let old = try XCTUnwrap(PushGroupRouting.mintSecret())
        try seedSettled(store: store, accountIdB64: "acct", groupIdHex: "grp", secret: old)
        let oldGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: old))

        await routing.rotateSecret(
            accountIdB64: "acct", groupIdHex: "grp", ownRole: .owner,
            broadcast: { bytes in recorder.record(bytes) })

        let newGid = try XCTUnwrap(PushGroupRouting.derivePushGid(
            secret: try XCTUnwrap(store.getSecret(accountIdB64: "acct", groupIdHex: "grp"))))
        XCTAssertEqual(unsub.targets, [oldGid], "the teardown names the OLD id")
        XCTAssertNotEqual(unsub.targets.first, newGid, "and never the new one")
        // ... and the device is armed under the new id BEFORE the old one is
        // dropped, so it is never unsubscribed from both at once.
        XCTAssertEqual(
            unsub.subscribesBefore, [1],
            "the NEW gid must already be subscribed when the OLD one is retired")
        XCTAssertEqual(spy.targets, [newGid])
    }

    /// **The dangerous window.** A failed rotation broadcast leaves the
    /// rotator switched locally while every other member is still on the old
    /// id, so nobody notifies either and the whole group's wakes stop. The
    /// `distributed` flag — reused verbatim from the mint path — is what
    /// makes it recoverable: the row stays PENDING and the existing
    /// any-holder re-broadcast branch re-sends the NEW secret. Never a
    /// re-mint.
    func testFailedRotationBroadcastLeavesTheRowPendingForTheRetry() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let unsub = UnsubscribeSpy()
        let routing = makeRouting(store: store, spy: spy, unsubscribeSpy: unsub)
        let broadcaster = FlakyBroadcastRecorder(shouldFail: true)

        let identity = try makeIdentity(0x69)
        let (group, groupIdHex) = try makeOwnerGroup(identity)
        let old = try XCTUnwrap(PushGroupRouting.mintSecret())
        try seedSettled(store: store, accountIdB64: "acct", groupIdHex: groupIdHex, secret: old)

        let outcome = await routing.rotateSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, ownRole: .owner,
            broadcast: { try broadcaster.send($0) })
        XCTAssertEqual(outcome, .rotated)

        let rotated = try XCTUnwrap(store.getSecret(accountIdB64: "acct", groupIdHex: groupIdHex))
        XCTAssertNotEqual(rotated, old)
        XCTAssertEqual(broadcaster.envelopes.count, 1, "the rotation attempted a broadcast")
        XCTAssertEqual(
            store.getRecord(accountIdB64: "acct", groupIdHex: groupIdHex)?.distributed, false,
            "a failed rotation broadcast must leave the row PENDING")
        // The local switch is durable regardless of the failed send.
        XCTAssertEqual(unsub.targets.count, 1)

        // Next chat-open / launch: the re-broadcast branch re-sends the
        // STORED (i.e. the newly rotated) secret.
        broadcaster.shouldFail = false
        await routing.ensureProvisioned(
            accountIdB64: "acct", groupIdHex: groupIdHex, group: group,
            identity: identity, broadcast: { try broadcaster.send($0) })

        XCTAssertEqual(broadcaster.envelopes.count, 2)
        XCTAssertEqual(
            PushGroupRouting.tryDecodeSecretEnvelope(try XCTUnwrap(broadcaster.envelopes.last)),
            rotated,
            "the retry re-sends the ROTATED secret — a re-mint would diverge the routing id")
        XCTAssertEqual(
            store.getRecord(accountIdB64: "acct", groupIdHex: groupIdHex)?.distributed, true)
        XCTAssertEqual(
            store.getSecret(accountIdB64: "acct", groupIdHex: groupIdHex), rotated)
    }

    /// Two concurrent triggers (this device's own removal + a poll-loop diff)
    /// coalesce into ONE rotation. Two mints would strand the first new
    /// `push_gid` as a subscribed-but-never-notified id — the orphan this ADR
    /// exists to prevent.
    func testConcurrentTriggersCoalesceIntoOneRotation() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let unsub = UnsubscribeSpy()
        let routing = makeRouting(store: store, spy: spy, unsubscribeSpy: unsub)
        let recorder = BroadcastRecorder()

        let old = try XCTUnwrap(PushGroupRouting.mintSecret())
        try seedSettled(store: store, accountIdB64: "acct", groupIdHex: "grp", secret: old)

        async let first = routing.rotateSecret(
            accountIdB64: "acct", groupIdHex: "grp", ownRole: .owner,
            broadcast: { bytes in recorder.record(bytes) })
        async let second = routing.rotateSecret(
            accountIdB64: "acct", groupIdHex: "grp", ownRole: .owner,
            broadcast: { bytes in recorder.record(bytes) })
        let outcomes = await [first, second]

        XCTAssertEqual(outcomes, [.rotated, .rotated])
        XCTAssertEqual(recorder.envelopes.count, 1, "exactly ONE mint + broadcast")
        XCTAssertEqual(spy.targets.count, 1)
        XCTAssertEqual(unsub.targets.count, 1)
    }

    // MARK: - ADR-0043: the membership-shrink trigger entry point

    /// An observed SHRINK rotates on the owner and on an admin, and the
    /// teardown names the id the device was actually on.
    func testObservedShrinkRotatesForOwnerAndAdmin() async throws {
        for role: Role in [.owner, .admin] {
            let store = makeStore()
            let spy = SubscribeSpy()
            let unsub = UnsubscribeSpy()
            let routing = makeRouting(store: store, spy: spy, unsubscribeSpy: unsub)
            let recorder = BroadcastRecorder()

            let held = try XCTUnwrap(PushGroupRouting.mintSecret())
            try seedSettled(store: store, accountIdB64: "acct", groupIdHex: "grp", secret: held)
            let heldGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: held))

            let outcome = await routing.rotateIfMembershipShrank(
                accountIdB64: "acct", groupIdHex: "grp", ownRole: role,
                previousMemberKeys: ["a", "b", "c"], currentMemberKeys: ["a", "b"],
                broadcast: { bytes in recorder.record(bytes) })
            XCTAssertEqual(outcome, .rotated)
            XCTAssertEqual(recorder.envelopes.count, 1)
            XCTAssertEqual(unsub.targets, [heldGid])
        }
    }

    /// A plain member observing the same shrink rotates nothing — it will
    /// pick up the new id from the rotator's envelope via the gated receive.
    func testObservedShrinkDoesNotRotateForAPlainMember() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let unsub = UnsubscribeSpy()
        let routing = makeRouting(store: store, spy: spy, unsubscribeSpy: unsub)
        let recorder = BroadcastRecorder()

        let held = try XCTUnwrap(PushGroupRouting.mintSecret())
        try seedSettled(store: store, accountIdB64: "acct", groupIdHex: "grp", secret: held)

        let outcome = await routing.rotateIfMembershipShrank(
            accountIdB64: "acct", groupIdHex: "grp", ownRole: .member,
            previousMemberKeys: ["a", "b", "c"], currentMemberKeys: ["a", "b"],
            broadcast: { bytes in recorder.record(bytes) })
        XCTAssertEqual(outcome, .skipNotAuthorized)
        XCTAssertTrue(recorder.envelopes.isEmpty)
        XCTAssertTrue(unsub.targets.isEmpty)
        XCTAssertEqual(store.getSecret(accountIdB64: "acct", groupIdHex: "grp"), held)
    }

    /// **No rotation on ADD** — the routing id stays stable across every
    /// ordinary commit, which is the whole point of rotating only at
    /// removals. A poll that changed nothing is likewise silent, and an
    /// unknown snapshot never reads as "everyone left".
    func testAddAndNoChangeAndUnknownNeverRotate() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let unsub = UnsubscribeSpy()
        let routing = makeRouting(store: store, spy: spy, unsubscribeSpy: unsub)
        let recorder = BroadcastRecorder()

        let held = try XCTUnwrap(PushGroupRouting.mintSecret())
        try seedSettled(store: store, accountIdB64: "acct", groupIdHex: "grp", secret: held)

        // An ADD.
        let onAdd = await routing.rotateIfMembershipShrank(
            accountIdB64: "acct", groupIdHex: "grp", ownRole: .owner,
            previousMemberKeys: ["a", "b"], currentMemberKeys: ["a", "b", "c"],
            broadcast: { bytes in recorder.record(bytes) })
        XCTAssertEqual(onAdd, .skipNoShrink)
        // A poll that changed nothing.
        let onNoChange = await routing.rotateIfMembershipShrank(
            accountIdB64: "acct", groupIdHex: "grp", ownRole: .owner,
            previousMemberKeys: ["a", "b"], currentMemberKeys: ["a", "b"],
            broadcast: { bytes in recorder.record(bytes) })
        XCTAssertEqual(onNoChange, .skipNoShrink)
        // A torn / absent snapshot.
        let onUnknown = await routing.rotateIfMembershipShrank(
            accountIdB64: "acct", groupIdHex: "grp", ownRole: .owner,
            previousMemberKeys: nil, currentMemberKeys: ["a"],
            broadcast: { bytes in recorder.record(bytes) })
        XCTAssertEqual(onUnknown, .skipUnknownMembership)

        XCTAssertTrue(recorder.envelopes.isEmpty, "no rotation on add / no-change / unknown")
        XCTAssertTrue(unsub.targets.isEmpty)
        XCTAssertEqual(
            store.getSecret(accountIdB64: "acct", groupIdHex: "grp"), held,
            "the routing id stays stable across every ordinary commit")
    }

    // MARK: - ADR-0043: the receive-side teardown

    /// **What keeps rotation from being a net loss.** A non-rotating member
    /// that applies a CHANGED secret must unsubscribe its OWN old gid:
    /// `group_unsubscribe` deletes exactly the `(device, group)` row named in
    /// the request and a device knows only its own pseudonym, so the rotator
    /// cannot do it for anyone else. Skip this and each rotation leaves one
    /// dead row PER MEMBER — the "live cluster plus an archive of every past
    /// cluster" ADR-0043 calls strictly worse than not rotating at all.
    func testNonRotatorApplyingAChangedSecretRetiresItsOwnOldGid() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let unsub = UnsubscribeSpy()
        let routing = makeRouting(store: store, spy: spy, unsubscribeSpy: unsub)
        let (group, groupIdHex, ownerLeaf, _) = try makeTwoMemberGroup(
            owner: try makeIdentity(0x6A), member: try makeIdentity(0x6B))

        let held = try XCTUnwrap(PushGroupRouting.mintSecret())
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: held,
            group: group, senderLeaf: ownerLeaf)
        let heldGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: held))
        XCTAssertTrue(unsub.targets.isEmpty, "a first-ever secret retires nothing")

        // The owner rotated; this device applies the CHANGED secret.
        let rotated = try XCTUnwrap(PushGroupRouting.mintSecret())
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: rotated,
            group: group, senderLeaf: ownerLeaf)
        let rotatedGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: rotated))

        XCTAssertEqual(unsub.targets, [heldGid], "the OWN old row is torn down")
        XCTAssertEqual(
            unsub.subscribesBefore, [2],
            "and only after the new id is armed (never unsubscribed from both)")
        XCTAssertNil(store.lookupRoute(pushGidHex: PushGroupRouting.hex(heldGid)))
        XCTAssertNotNil(store.lookupRoute(pushGidHex: PushGroupRouting.hex(rotatedGid)))
    }

    /// An IDENTICAL re-broadcast retires nothing — the id did not change, so
    /// tearing down would unsubscribe the device from its own live routing.
    func testIdenticalReBroadcastRetiresNothing() async throws {
        let store = makeStore()
        let spy = SubscribeSpy()
        let unsub = UnsubscribeSpy()
        let routing = makeRouting(store: store, spy: spy, unsubscribeSpy: unsub)
        let (group, groupIdHex, ownerLeaf, memberLeaf) = try makeTwoMemberGroup(
            owner: try makeIdentity(0x6C), member: try makeIdentity(0x6D))

        let held = try XCTUnwrap(PushGroupRouting.mintSecret())
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: held,
            group: group, senderLeaf: ownerLeaf)
        // The idempotent relay from a plain member.
        await routing.receiveSecret(
            accountIdB64: "acct", groupIdHex: groupIdHex, secret: held,
            group: group, senderLeaf: memberLeaf)

        XCTAssertTrue(unsub.targets.isEmpty)
        XCTAssertNotNil(store.lookupRoute(
            pushGidHex: PushGroupRouting.hex(
                try XCTUnwrap(PushGroupRouting.derivePushGid(secret: held)))))
    }
}
