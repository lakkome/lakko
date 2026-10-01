// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// MARK: - Fakes

/// A statement signer that records which statement it was asked for and
/// returns a recognisable request.
private final class FakeStatementSigner: DsStatementSignerProtocol, @unchecked Sendable {
    let name: String
    let key: Data
    var calls: [String] = []
    var lastRevoke: [Data] = []
    var lastChanges: [DsRegistrarChange] = []
    var lastEntries: [DsRegistrarEntry] = []

    init(_ name: String, key: UInt8 = 0x11) {
        self.name = name
        self.key = Data(repeating: key, count: 32)
    }

    private func req(_ segment: String, _ body: String) -> DsStatementRequest {
        DsStatementRequest(path: "/ds/group/ab/\(segment)", bodyJson: "{\"by\":\"\(name)\",\(body)}")
    }

    func claim(journalId: Data, epoch: UInt64, epochKey: Data) throws -> DsStatementRequest {
        calls.append("claim:\(epoch)")
        return req("claim", "\"epoch\":\(epoch)")
    }

    func publicKey() -> Data { key }

    func pushBinding(journalId: Data, pushGid: Data) throws -> DsStatementRequest {
        calls.append("push-binding")
        return req("push-binding", "\"x\":0")
    }

    func registerEpoch(journalId: Data, epoch: UInt64, epochKey: Data, revoke: [Data]) throws
        -> DsStatementRequest {
        calls.append("access:\(epoch)")
        lastRevoke = revoke
        return req("access", "\"epoch\":\(epoch)")
    }

    func registrars(journalId: Data, changes: [DsRegistrarChange]) throws -> DsStatementRequest {
        calls.append("registrars")
        lastChanges = changes
        return req("registrars", "\"x\":0")
    }

    func registrarsSet(journalId: Data, entries: [DsRegistrarEntry]) throws -> DsStatementRequest {
        calls.append("registrars-set")
        lastEntries = entries
        return req("registrars-set", "\"x\":0")
    }

    func replaceEpoch(journalId: Data, epoch: UInt64, epochKey: Data, revoke: [Data]) throws
        -> DsStatementRequest {
        calls.append("replace:\(epoch)")
        return req("access", "\"replace\":\(epoch)")
    }

    func reset(journalId: Data, epoch: UInt64, epochKey: Data, registrars: [DsRegistrarEntry]?) throws
        -> DsStatementRequest {
        calls.append("reset:\(epoch)")
        return req("reset", "\"epoch\":\(epoch)")
    }

    func rootTransfer(journalId: Data, newRoot: Data) throws -> DsStatementRequest {
        calls.append("root")
        return req("root", "\"x\":0")
    }
}

/// A DS that records the statements it saw and answers scripted refusals.
private final class FakeStatementDs: DsStatementSending {
    var sent: [DsStatementRequest] = []
    var answers: [DsStatementError?] = []

    func putStatement(_ request: DsStatementRequest) async throws {
        sent.append(request)
        if !answers.isEmpty, let error = answers.removeFirst() { throw error }
    }

    var segments: [String] { sent.map { String($0.path.split(separator: "/").last ?? "") } }
}

// MARK: - Tests

/// ADR-0057 — the iOS obligations: the owner claims with its root, a member
/// never claims, the owner appoints registrars, the receive gate, register
/// before publish with the right signer and revoke list, no self-heal, reset
/// on a re-found, the push binding, and the statement transport.
@MainActor
final class JournalOwnershipTests: XCTestCase {

    private let gid = Data(repeating: 0xAB, count: 32)

    // MARK: Obligation 1 — the owner claims with its root

    func testTheOwnerClaimsWithTheRootAtTheEpochAndKey() async throws {
        let root = FakeStatementSigner("root")
        let ds = FakeStatementDs()
        try await JournalOwnership.claim(root: root, journalId: gid, epoch: 1, epochKey: Data(count: 32), ds: ds)
        XCTAssertEqual(root.calls, ["claim:1"])
        XCTAssertEqual(ds.segments, ["claim"])
    }

    /// The real root signer produces the route the DS takes, with a statement
    /// body and nothing that looks like an ADR-0053 claim.
    func testTheRealRootSignerProducesAClaimStatement() throws {
        let account = try Identity.fromSeed(seed: Data(repeating: 0x21, count: 32))
        let group = try Group.create(identity: account, kind: .group)
        let groupId = try group.groupId()
        let signer = try group.dsAccessSigner(identity: account)
        let root = try dsJournalRootSigner(accountIdentity: account, journalId: groupId)
        let request = try root.claim(journalId: groupId, epoch: signer.epoch(), epochKey: signer.verifyingKey())
        XCTAssertEqual(request.path, "/ds/group/\(DsClient.pseudonymHex(groupId))/claim")
        XCTAssertTrue(request.bodyJson.contains("statement_b64"), request.bodyJson)
        // Deterministic on every device of the account.
        let again = try dsJournalRootSigner(accountIdentity: account, journalId: groupId)
        XCTAssertEqual(root.publicKey(), again.publicKey())
        XCTAssertEqual(
            try dsStatementSigner(seed: dsJournalRootSeed(
                accountIdentitySeed: account.ed25519Seed(), journalId: groupId)).publicKey(),
            root.publicKey())
    }

    // MARK: Obligation 2 — the lazy claim is the owner's only

    func testAMemberNeverClaims() async {
        let ds = FakeStatementDs()
        let outcome = await JournalOwnership.lazyClaim(
            isOwner: false, root: { XCTFail("a member derives no root"); return FakeStatementSigner("x") },
            journalId: gid, epoch: 3, epochKey: Data(count: 32), ds: ds)
        XCTAssertEqual(outcome, .notOwner)
        XCTAssertTrue(ds.sent.isEmpty)
    }

    func testTheOwnerClaimsAnUnclaimedJournal() async {
        let root = FakeStatementSigner("root")
        let ds = FakeStatementDs()
        let outcome = await JournalOwnership.lazyClaim(
            isOwner: true, root: { root }, journalId: gid, epoch: 3, epochKey: Data(count: 32), ds: ds)
        XCTAssertEqual(outcome, .claimed)
        XCTAssertEqual(root.calls, ["claim:3"])
    }

    /// A lazy claim that meets `409` (claimed already) registers NOTHING: a
    /// root registration replaces every epoch at and above it, so an owner
    /// device that is behind would cut the current epochs off for everyone.
    func testALazyClaimThatMeetsAConflictRegistersNothing() async {
        let root = FakeStatementSigner("root")
        let ds = FakeStatementDs()
        ds.answers = [.conflict]
        let outcome = await JournalOwnership.lazyClaim(
            isOwner: true, root: { root }, journalId: gid, epoch: 3, epochKey: Data(count: 32), ds: ds)
        XCTAssertEqual(outcome, .alreadyClaimed)
        XCTAssertEqual(root.calls, ["claim:3"], "no access statement after the 409")
        XCTAssertEqual(ds.segments, ["claim"])
    }

    // MARK: Only the channel owner claims a PublisherGroup journal

    /// A co-publisher's root is derived from its own account seed, so its
    /// claim would squat the journal against the owner. A device marked as a
    /// co-publisher never claims (on open or lazily); the owner does.
    func testACoPublisherNeverClaimsThePublisherGroupJournal() async throws {
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x61, count: 32))
        let channelId = Data((0..<16).map { UInt8($0) &+ 0x40 })
        let group = try publisherGroupOpenOrCreate(identity: identity, channelId: channelId)
        let hex = DsClient.pseudonymHex(try group.groupId())
        let marks = JournalFoundingMarks(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "marks-\(UUID().uuidString)")))
        let coPublished = CoPublishedJournals(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "copub-\(UUID().uuidString)")),
            accountIdB64: { "acct" })

        coPublished.mark(journalIdHex: hex)
        XCTAssertTrue(coPublished.contains(journalIdHex: hex.uppercased()))
        let copub = FakeStatementDs()
        await CoPublisher.claimJournal(
            ds: copub, identity: identity, publisherGroup: group, marks: marks, coPublished: coPublished)
        XCTAssertTrue(copub.sent.isEmpty, "a co-publisher claims on open: never")
        let lazy = await CoPublisher.lazyClaim(
            ds: copub, identity: identity, publisherGroup: group, coPublished: coPublished)
        XCTAssertNil(lazy, "an unclaimed journal: the co-publisher waits for the owner")
        XCTAssertTrue(copub.sent.isEmpty, "a co-publisher claims lazily: never")

        let owner = FakeStatementDs()
        let ownerMarks = CoPublishedJournals(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "copub-\(UUID().uuidString)")),
            accountIdB64: { "acct" })
        await CoPublisher.claimJournal(
            ds: owner, identity: identity, publisherGroup: group, marks: marks, coPublished: ownerMarks)
        XCTAssertEqual(owner.segments, ["claim"], "the channel owner claims")
        let ownerLazy = FakeStatementDs()
        ownerLazy.answers = [.conflict]
        let signer = await CoPublisher.lazyClaim(
            ds: ownerLazy, identity: identity, publisherGroup: group, coPublished: ownerMarks)
        XCTAssertNil(signer, "a 409 is left alone")
        XCTAssertEqual(ownerLazy.segments, ["claim"], "and nothing is registered after it")
    }

    // MARK: Obligation 7 — reset on a re-found only

    func testAReFoundedJournalIsResetUnderTheSameRoot() async throws {
        let root = FakeStatementSigner("root")
        let ds = FakeStatementDs()
        ds.answers = [.conflict]
        try await JournalOwnership.claimOrReset(
            root: root, journalId: gid, epoch: 0, epochKey: Data(count: 32), ds: ds)
        XCTAssertEqual(root.calls, ["claim:0", "reset:0"])
        XCTAssertEqual(ds.segments, ["claim", "reset"])
    }

    func testAnUnclaimedReFoundIsAPlainClaim() async throws {
        let root = FakeStatementSigner("root")
        let ds = FakeStatementDs()
        try await JournalOwnership.claimOrReset(
            root: root, journalId: gid, epoch: 0, epochKey: Data(count: 32), ds: ds)
        XCTAssertEqual(root.calls, ["claim:0"])
    }

    func testFoundingMarksAreSetReadAndCleared() throws {
        let marks = JournalFoundingMarks(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "marks-\(UUID().uuidString)")))
        XCTAssertFalse(marks.isMarked(journalIdHex: "aa"))
        marks.mark(journalIdHex: "aa")
        XCTAssertTrue(marks.isMarked(journalIdHex: "aa"))
        marks.clear(journalIdHex: "aa")
        XCTAssertFalse(marks.isMarked(journalIdHex: "aa"))
    }

    /// The DeviceGroup claim: founded here -> claim, reset on 409; an existing
    /// DeviceGroup that finds the journal ahead leaves it alone.
    func testTheDeviceGroupClaimResetsOnlyWhenFoundedHere() async throws {
        let seed = Data(repeating: 0x55, count: 64)
        let accountIdB64 = "dg-\(UUID().uuidString)"
        try AccountIdentitySeedStore.putAccountIdentitySeed(accountIdB64: accountIdB64, seed: seed)
        defer { AccountIdentitySeedStore.deleteAccountIdentitySeed(accountIdB64: accountIdB64) }
        let device = try Identity.fromSeed(seed: Data(repeating: 0x56, count: 32))
        let group = try DeviceGroup.createFounder(identity: device, accountId: Data(repeating: 7, count: 16))
        let marks = JournalFoundingMarks(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "marks-\(UUID().uuidString)")))
        let hex = DsClient.pseudonymHex(try group.groupId())

        let behind = FakeStatementDs()
        behind.answers = [.conflict]
        await DeviceGroupDsAccess.claim(
            group: group, identity: device, accountIdB64: accountIdB64, ds: behind, marks: marks)
        XCTAssertEqual(behind.segments, ["claim"], "not founded here: a 409 never resets")

        marks.mark(journalIdHex: hex)
        let refound = FakeStatementDs()
        refound.answers = [.conflict]
        await DeviceGroupDsAccess.claim(
            group: group, identity: device, accountIdB64: accountIdB64, ds: refound, marks: marks)
        XCTAssertEqual(refound.segments, ["claim", "reset"])
        XCTAssertFalse(marks.isMarked(journalIdHex: hex), "the mark is spent")
    }

    // MARK: Obligation 3 — appointing registrars

    func testDesiredRanksAreAdminForAdminsMemberForTheRestAndNoneForTheOwner() {
        let owner = Data(repeating: 1, count: 32)
        let admin = Data(repeating: 2, count: 32)
        let member = Data(repeating: 3, count: 32)
        let ranks = JournalOwnership.desiredRanks(
            memberAccountKeys: [owner, owner, admin, member], owner: owner, admins: [admin])
        XCTAssertEqual(ranks, [admin: .admin, member: .member])
    }

    func testEnvelopesAreOwedToNewAndReRankedAccountsOnly() {
        let a = Data(repeating: 2, count: 32)
        let b = Data(repeating: 3, count: 32)
        let c = Data(repeating: 4, count: 32)
        let owed = JournalOwnership.envelopesOwed(
            desired: [a: .admin, b: .member],
            delivered: [a: .member, c: .member])
        XCTAssertEqual(owed.map(\.accountKey), [a, b])
        XCTAssertEqual(owed.map(\.rank), [.admin, .member])
        XCTAssertTrue(JournalOwnership.envelopesOwed(desired: [a: .admin], delivered: [a: .admin]).isEmpty)
    }

    // MARK: Second security pass, finding 2 — converge the set from the roster

    private final class ReconcileHarness {
        var desired: [Data: DsRegistrarRank]? = [:]
        var authority: JournalAuthority?
        var sentSet: String?
        var delivered: [Data: DsRegistrarRank] = [:]
        var envelopes: [(Data, DsRegistrarRank)] = []
        var unresolvable: Set<Data> = []
        let ds = FakeStatementDs()

        func ports(gid: Data) -> JournalOwnership.RegistrarReconcilePorts {
            .init(
                journalId: gid, ds: ds,
                desired: { self.desired },
                authority: { self.authority },
                registrarPublicKey: { Data($0.reversed()) },
                lastSentSet: { self.sentSet },
                noteSentSet: { self.sentSet = $0 },
                loadDelivered: { self.delivered },
                saveDelivered: { self.delivered = $0 },
                sendEnvelope: { key, rank in
                    if self.unresolvable.contains(key) { return false }
                    self.envelopes.append((key, rank))
                    return true
                })
        }
    }

    /// The owner sends ONE `registrars-set` with the whole set the roster
    /// calls for, derived registrar keys sorted by account, with no
    /// dependence on any local record: an evicted member's key goes because
    /// it is not listed, even when nothing local ever recorded appointing it.
    func testTheOwnerSendsTheWholeSetFromTheRosterWithoutALedger() async throws {
        let root = FakeStatementSigner("root")
        let h = ReconcileHarness()
        let a = Data(repeating: 2, count: 32) + Data([1])
        let b = Data(repeating: 1, count: 32) + Data([2])
        h.authority = .root(root)
        h.desired = [a: .admin, b: .member]
        let outcome = try await JournalOwnership.reconcileRegistrars(h.ports(gid: gid))
        XCTAssertEqual(outcome, .reconciled(setSent: true, pending: 0))
        XCTAssertEqual(h.ds.segments, ["registrars-set"])
        XCTAssertEqual(root.lastEntries.map(\.publicKey), [Data(b.reversed()), Data(a.reversed())],
                       "registrar keys, not account keys, sorted by account key")
        XCTAssertEqual(root.lastEntries.map(\.rank), [.member, .admin])
        XCTAssertEqual(h.envelopes.count, 2)

        // Same roster, same session: nothing more.
        let again = try await JournalOwnership.reconcileRegistrars(h.ports(gid: gid))
        XCTAssertEqual(again, .unchanged)
        XCTAssertEqual(h.ds.segments, ["registrars-set"])

        // `b` removed: a new whole set, no per-key dismissal, the delivered
        // record drops `b` without a statement of its own.
        h.desired = [a: .admin]
        _ = try await JournalOwnership.reconcileRegistrars(h.ports(gid: gid))
        XCTAssertEqual(h.ds.segments, ["registrars-set", "registrars-set"])
        XCTAssertEqual(root.lastEntries.map(\.publicKey), [Data(a.reversed())])
        XCTAssertEqual(h.delivered, [a: .admin])
        XCTAssertFalse(root.calls.contains("registrars"), "no diff statements")
    }

    /// A fresh session (no set sent yet) sends the set even when every
    /// envelope was delivered in an earlier session; an unresolved X25519
    /// key leaves only that member pending.
    func testEachSessionConvergesOnceAndAnUnresolvedMemberStaysPending() async throws {
        let root = FakeStatementSigner("root")
        let h = ReconcileHarness()
        let a = Data(repeating: 2, count: 32)
        let b = Data(repeating: 3, count: 32)
        h.authority = .root(root)
        h.desired = [a: .member, b: .member]
        h.delivered = [a: .member]
        h.unresolvable = [b]
        let outcome = try await JournalOwnership.reconcileRegistrars(h.ports(gid: gid))
        XCTAssertEqual(outcome, .reconciled(setSent: true, pending: 1))
        XCTAssertEqual(h.ds.segments, ["registrars-set"])
        XCTAssertTrue(h.envelopes.isEmpty)
        XCTAssertEqual(h.delivered, [a: .member])
    }

    /// Only the root converges the set: a registrar (an owner whose takeover
    /// is not confirmed) or no authority sends nothing.
    func testOnlyTheRootConvergesTheSet() async throws {
        let h = ReconcileHarness()
        h.desired = [Data(repeating: 2, count: 32): .member]
        h.authority = .registrar(FakeStatementSigner("admin"), .admin)
        let asRegistrar = try await JournalOwnership.reconcileRegistrars(h.ports(gid: gid))
        XCTAssertEqual(asRegistrar, .notOwner)
        h.authority = nil
        let asNone = try await JournalOwnership.reconcileRegistrars(h.ports(gid: gid))
        XCTAssertEqual(asNone, .notOwner)
        h.authority = .root(FakeStatementSigner("root"))
        h.desired = nil
        let notOwner = try await JournalOwnership.reconcileRegistrars(h.ports(gid: gid))
        XCTAssertEqual(notOwner, .notOwner)
        XCTAssertTrue(h.ds.sent.isEmpty)
    }

    /// A refused set records nothing, so the next pass retries it.
    func testARefusedSetIsRetriedOnTheNextPass() async throws {
        let h = ReconcileHarness()
        h.authority = .root(FakeStatementSigner("root"))
        h.desired = [Data(repeating: 2, count: 32): .member]
        h.ds.answers = [.rankShortfall]
        do {
            _ = try await JournalOwnership.reconcileRegistrars(h.ports(gid: gid))
            XCTFail("the refusal is thrown")
        } catch {}
        XCTAssertNil(h.sentSet)
        XCTAssertTrue(h.envelopes.isEmpty)
        _ = try await JournalOwnership.reconcileRegistrars(h.ports(gid: gid))
        XCTAssertEqual(h.ds.segments, ["registrars-set", "registrars-set"])
    }

    /// The real FFI signs a `registrars-set` statement on its own route.
    func testTheRealRootSignsARegistrarsSet() throws {
        let owner = try Identity.fromSeed(seed: Data(repeating: 0x51, count: 32))
        let root = try dsJournalRootSigner(accountIdentity: owner, journalId: gid)
        let request = try root.registrarsSet(
            journalId: gid,
            entries: [DsRegistrarEntry(publicKey: try dsStatementSigner(
                seed: Data(repeating: 4, count: 32)).publicKey(), rank: .member)])
        XCTAssertEqual(request.path, "/ds/group/\(DsClient.pseudonymHex(gid))/registrars-set")
    }

    /// The seed the owner sends is the one the member opens: derivation,
    /// sealing to the member's ACCOUNT X25519 key, the envelope recognised on
    /// the receive path, opened with the member's account identity.
    func testTheAppointedSeedRoundTripsToTheMember() throws {
        let owner = try Identity.fromSeed(seed: Data(repeating: 0x31, count: 32))
        let member = try Identity.fromSeed(seed: Data(repeating: 0x32, count: 32))
        let stranger = try Identity.fromSeed(seed: Data(repeating: 0x33, count: 32))
        let rootSeed = try dsJournalRootSeed(accountIdentitySeed: owner.ed25519Seed(), journalId: gid)
        let seed = try dsJournalRegistrarSeed(rootSeed: rootSeed, memberAccountKey: member.signaturePublicKey())
        let envelope = try composeRegistrarSeedEnvelope(
            journalId: gid, memberAccountKey: member.signaturePublicKey(),
            memberX25519PublicKey: member.x25519PublicKey(), rank: .admin, registrarSeed: seed)

        XCTAssertEqual(envelope.count, Int(registrarSeedEnvelopeLen()))
        XCTAssertEqual(envelope.count, 205)
        XCTAssertEqual(envelope.first, 0x6C)
        XCTAssertTrue(ChatClient.looksLikeRegistrarSeed(envelope))
        XCTAssertEqual(try dispatchIncomingEnvelope(plaintext: envelope).kind, .registrarSeed)
        let decoded = try decodeRegistrarSeedEnvelope(plaintext: envelope)
        XCTAssertEqual(decoded.memberAccountKey, member.signaturePublicKey())
        XCTAssertEqual(decoded.rank, .admin)
        XCTAssertEqual(try openRegistrarSeedEnvelope(plaintext: envelope, accountIdentity: member), seed)
        XCTAssertThrowsError(try openRegistrarSeedEnvelope(plaintext: envelope, accountIdentity: stranger))
        XCTAssertFalse(ChatClient.looksLikeRegistrarSeed(Data("{\"t\":\"hi\"}".utf8)))
    }

    // MARK: Obligation 4 — the receive gate

    func testTheReceiveGate() {
        let me = Data(repeating: 1, count: 32)
        let owner = Data(repeating: 9, count: 32)
        let other = Data(repeating: 5, count: 32)
        func verdict(addressee: Data = me, journal: Data? = nil, sender: Data?, ownerKey: Data? = owner)
            -> JournalOwnership.SeedVerdict {
            JournalOwnership.seedVerdict(
                envelopeJournal: journal ?? gid, envelopeAddressee: addressee, groupId: gid,
                ownAccountKey: me, senderAccountKey: sender, owner: ownerKey)
        }
        XCTAssertEqual(verdict(sender: owner), .accept)
        XCTAssertEqual(verdict(addressee: other, sender: owner), .notAddressedToUs)
        XCTAssertEqual(verdict(sender: other), .senderNotOwner, "only the root appoints")
        XCTAssertEqual(verdict(sender: nil), .senderNotOwner, "an unresolved sender is refused")
        XCTAssertEqual(verdict(sender: owner, ownerKey: nil), .senderNotOwner, "no role table, no owner")
        XCTAssertEqual(verdict(journal: Data(repeating: 0xCD, count: 32), sender: owner), .wrongJournal)
    }

    func testTheSeedStoreKeepsReplacesAndClears() throws {
        let store = RegistrarSeedStore(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "seeds-\(UUID().uuidString)")))
        XCTAssertNil(store.get(accountIdB64: "acct", groupIdHex: "g1"))
        store.put(accountIdB64: "acct", groupIdHex: "g1", seed: Data(repeating: 1, count: 32), rank: .member)
        XCTAssertEqual(store.get(accountIdB64: "acct", groupIdHex: "g1"),
                       .init(seed: Data(repeating: 1, count: 32), rank: .member))
        store.put(accountIdB64: "acct", groupIdHex: "g1", seed: Data(repeating: 2, count: 32), rank: .admin)
        XCTAssertEqual(store.get(accountIdB64: "acct", groupIdHex: "g1")?.rank, .admin, "a newer one replaces")
        store.saveDelivered(accountIdB64: "acct", groupIdHex: "g1", [Data([7]): .member])
        XCTAssertEqual(store.delivered(accountIdB64: "acct", groupIdHex: "g1"), [Data([7]): .member])
        store.forgetDelivered(accountIdB64: "acct", groupIdHex: "g1")
        XCTAssertTrue(store.delivered(accountIdB64: "acct", groupIdHex: "g1").isEmpty)
        store.clear(accountIdB64: "acct")
        XCTAssertNil(store.get(accountIdB64: "acct", groupIdHex: "g1"))
    }

    // MARK: Second security pass, finding 4 — root held

    func testTheRootHeldMarkerIsPerAccountAndGroupAndSurvivesARestart() throws {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "held-\(UUID().uuidString)"))
        let store = RegistrarSeedStore(defaults: defaults)
        XCTAssertFalse(store.isRootHeld(accountIdB64: "acct", groupIdHex: "AB"))
        store.markRootHeld(accountIdB64: "acct", groupIdHex: "AB")
        XCTAssertTrue(RegistrarSeedStore(defaults: defaults).isRootHeld(accountIdB64: "acct", groupIdHex: "ab"))
        XCTAssertFalse(store.isRootHeld(accountIdB64: "other", groupIdHex: "ab"))
        XCTAssertFalse(store.isRootHeld(accountIdB64: "acct", groupIdHex: "cd"))
        store.clearRootHeld(accountIdB64: "acct", groupIdHex: "ab")
        XCTAssertFalse(store.isRootHeld(accountIdB64: "acct", groupIdHex: "ab"))
        store.markRootHeld(accountIdB64: "acct", groupIdHex: "ab")
        store.clear(accountIdB64: "acct")
        XCTAssertFalse(store.isRootHeld(accountIdB64: "acct", groupIdHex: "ab"))
    }

    /// The new owner keeps using its registrar seed until the marker is set;
    /// with the marker it signs with the root. With no registrar seed it falls
    /// back to the root (a group founded before the marker; the probe sets it).
    func testTheNewOwnerUsesItsRegistrarSeedUntilTheMarker() {
        let root = FakeStatementSigner("root")
        let registrar = FakeStatementSigner("registrar")
        let before = JournalAuthority.resolve(
            isOwner: true, rootHeld: false, rootSigner: { root }, registrar: { (registrar, .admin) })
        XCTAssertTrue(before?.signer === registrar)
        XCTAssertEqual(before?.isRootOrAdmin, true)
        let after = JournalAuthority.resolve(
            isOwner: true, rootHeld: true, rootSigner: { root }, registrar: { (registrar, .admin) })
        XCTAssertTrue(after?.isRoot == true)
        let noSeed = JournalAuthority.resolve(
            isOwner: true, rootHeld: false, rootSigner: { root }, registrar: { nil })
        XCTAssertTrue(noSeed?.isRoot == true)
    }

    func testTheProbeReadsA204AsRootAndA401AsNotRoot() async throws {
        let root = FakeStatementSigner("root")
        let yes = FakeStatementDs()
        let held = try await JournalOwnership.probeRoot(root: root, journalId: gid, ds: yes)
        XCTAssertEqual(held, .root)
        XCTAssertEqual(yes.segments, ["root"])
        let no = FakeStatementDs()
        no.answers = [.unauthorized]
        let notHeld = try await JournalOwnership.probeRoot(root: root, journalId: gid, ds: no)
        XCTAssertEqual(notHeld, .notRoot)
        let down = FakeStatementDs()
        down.answers = [.http(status: 502, body: "")]
        do {
            _ = try await JournalOwnership.probeRoot(root: root, journalId: gid, ds: down)
            XCTFail("a 5xx is not an answer")
        } catch {}
    }

    // MARK: Obligation 5 — register before publish, with the right signer

    func testTheAuthorityIsTheRootForTheOwnerTheSeedOtherwiseNoneWithout() {
        let root = FakeStatementSigner("root")
        let registrar = FakeStatementSigner("registrar")
        XCTAssertTrue(JournalAuthority.resolve(
            isOwner: true, rootSigner: { root }, registrar: { (registrar, .admin) })?.isRoot == true)
        let member = JournalAuthority.resolve(
            isOwner: false, rootSigner: { XCTFail("no root for a member"); return root },
            registrar: { (registrar, .member) })
        XCTAssertTrue(member?.signer === registrar)
        XCTAssertNil(JournalAuthority.resolve(isOwner: false, rootSigner: { root }, registrar: { nil }),
                     "neither: this device cannot commit")
    }

    func testTheRootRevokesTheEvictedRegistrarsAndARegistrarRevokesNothing() {
        let evicted = [Data(repeating: 4, count: 32)]
        let derived = Data(repeating: 0xEE, count: 32)
        XCTAssertEqual(JournalOwnership.revokeList(
            authority: .root(FakeStatementSigner("root")), evictedAccountKeys: evicted,
            registrarPublicKey: { _ in derived }), [derived])
        XCTAssertEqual(JournalOwnership.revokeList(
            authority: .registrar(FakeStatementSigner("admin"), .admin), evictedAccountKeys: evicted,
            registrarPublicKey: { _ in derived }), [])
    }

    func testRegisterEpochSignsWithTheAuthorityAndCarriesTheRevokeList() async throws {
        let admin = FakeStatementSigner("admin")
        let ds = FakeStatementDs()
        try await JournalOwnership.registerEpoch(
            authority: .registrar(admin, .admin), journalId: gid, epoch: 8,
            epochKey: Data(count: 32), revoke: [Data(repeating: 1, count: 32)], ds: ds)
        XCTAssertEqual(admin.calls, ["access:8"])
        XCTAssertEqual(admin.lastRevoke, [Data(repeating: 1, count: 32)])
        XCTAssertTrue(ds.sent.first?.bodyJson.contains("\"by\":\"admin\"") == true)
    }

    // MARK: Second security pass, finding 1 — Append, and a 409 abandons

    /// An Append 409 is a lost race: the registration throws (the commit is
    /// abandoned by its caller), the conflict hook is told the epoch this
    /// device is at, and nothing is replaced from the commit path.
    func testAnAppend409ThrowsAndNeverReplaces() async {
        let admin = FakeStatementSigner("admin")
        let ds = FakeStatementDs()
        ds.answers = [.conflict]
        var noted: [UInt64] = []
        do {
            try await JournalOwnership.registerEpoch(
                authority: .registrar(admin, .admin), journalId: gid, epoch: 8,
                epochKey: Data(count: 32), revoke: [], ds: ds, onConflict: { noted.append($0) })
            XCTFail("a 409 aborts the commit")
        } catch let error as DsStatementError {
            XCTAssertEqual(error, .conflict)
        } catch {
            XCTFail("unexpected \(error)")
        }
        XCTAssertEqual(admin.calls, ["access:8"], "Append only; no replace in a commit path")
        XCTAssertEqual(noted, [7])
    }

    /// The real FFI: registerEpoch and replaceEpoch sign distinct statements
    /// on the same route.
    func testTheRealSignerSignsAppendAndReplaceDifferently() throws {
        let owner = try Identity.fromSeed(seed: Data(repeating: 0x52, count: 32))
        let root = try dsJournalRootSigner(accountIdentity: owner, journalId: gid)
        let key = Data(repeating: 3, count: 32)
        let append = try root.registerEpoch(journalId: gid, epoch: 4, epochKey: key, revoke: [])
        let replace = try root.replaceEpoch(journalId: gid, epoch: 4, epochKey: key, revoke: [])
        XCTAssertEqual(append.path, replace.path)
        XCTAssertNotEqual(append.bodyJson, replace.bodyJson)
    }

    /// ADR-0050: removing one device of an account keeps the account a member.
    func testOnlyAnAccountWithEveryLeafRemovedIsEvicted() {
        let a = Data(repeating: 1, count: 32)
        let b = Data(repeating: 2, count: 32)
        let members: [(leaf: UInt32, accountKey: Data)] = [(0, a), (1, b), (2, b)]
        XCTAssertEqual(JournalOwnership.evictedAccountKeys(members: members, removedLeaves: [1]), [])
        XCTAssertEqual(JournalOwnership.evictedAccountKeys(members: members, removedLeaves: [1, 2]), [b])
        XCTAssertEqual(JournalOwnership.evictedAccountKeys(members: members, removedLeaves: []), [])
    }

    /// The real FFI: the registrar key the root revokes is the key the member
    /// signs with.
    func testTheDerivedRegistrarKeyIsTheOneTheMemberSignsWith() throws {
        let owner = try Identity.fromSeed(seed: Data(repeating: 0x41, count: 32))
        let member = try Identity.fromSeed(seed: Data(repeating: 0x42, count: 32))
        let rootSeed = try dsJournalRootSeed(accountIdentitySeed: owner.ed25519Seed(), journalId: gid)
        let seed = try dsJournalRegistrarSeed(rootSeed: rootSeed, memberAccountKey: member.signaturePublicKey())
        let signer = try dsStatementSigner(seed: seed)
        let request = try signer.registerEpoch(
            journalId: gid, epoch: 5, epochKey: Data(repeating: 3, count: 32), revoke: [])
        XCTAssertEqual(request.path, "/ds/group/\(DsClient.pseudonymHex(gid))/access")
        XCTAssertNotEqual(signer.publicKey(), try dsJournalRootSigner(accountIdentity: owner, journalId: gid).publicKey())
    }

    // MARK: Obligation 8 — the push binding

    func testThePushBindingReportsItsOutcome() async {
        let signer = FakeStatementSigner("root")
        let cases: [(DsStatementError, PushBindOutcome)] = [
            (.conflict, .conflict), (.pushBindingDisabled, .disabled), (.http(status: 500, body: ""), .failed),
        ]
        for (refusal, expected) in cases {
            let ds = FakeStatementDs()
            ds.answers = [refusal]
            let outcome = await JournalOwnership.bindPush(
                signer: signer, journalId: gid, pushGid: Data(repeating: 9, count: 32), ds: ds)
            XCTAssertEqual(outcome, expected)
            XCTAssertEqual(ds.segments, ["push-binding"])
        }
        let ok = FakeStatementDs()
        let outcome = await JournalOwnership.bindPush(
            signer: signer, journalId: gid, pushGid: Data(repeating: 9, count: 32), ds: ok)
        XCTAssertEqual(outcome, .bound)
    }

    // MARK: Transport

    func testAStatementIsAPutWithoutAProofAndA409IsRetriedOnce() async throws {
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [
            .init(status: 409, body: Data("concurrent registration; retry".utf8)),
            .init(status: 204, body: Data()),
        ])
        let request = DsStatementRequest(path: "/ds/group/ab/access", bodyJson: #"{"statement_b64":"AA"}"#)
        try await DsAccessURLProtocol.client(scope).putStatement(request)
        let seen = DsAccessURLProtocol.requests(scope)
        XCTAssertEqual(seen.count, 2, "one retry after a lock race")
        XCTAssertEqual(seen.first?.method, "PUT")
        XCTAssertEqual(seen.first?.url.path, "/native/ds/ds/group/ab/access")
        XCTAssertNil(seen.first?.proof, "a statement carries no proof header")
        XCTAssertEqual(seen.first?.body, Data(request.bodyJson.utf8))
    }

    /// Live walk 2026-09-29 — a DECIDED conflict is not re-sent: the DS spent
    /// the statement's nonce before deciding, so the identical re-send would be
    /// refused `401` and the conflict would surface as `unauthorized`.
    func testADecidedConflictIsNotResent() async {
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [
            .init(status: 409, body: Data("journal already claimed; a reset replaces its chain".utf8)),
            .init(status: 401, body: Data("proof required".utf8)),
        ])
        let request = DsStatementRequest(path: "/ds/group/ab/claim", bodyJson: #"{"statement_b64":"AA"}"#)
        do {
            try await DsAccessURLProtocol.client(scope).putStatement(request)
            XCTFail("expected a conflict")
        } catch {
            XCTAssertEqual(error as? DsStatementError, .conflict)
        }
        XCTAssertEqual(DsAccessURLProtocol.requests(scope).count, 1)
    }

    func testTheStatementRefusalsAreTyped() async {
        let request = DsStatementRequest(path: "/ds/group/ab/access", bodyJson: "{}")
        let cases: [(Int, String, DsStatementError)] = [
            (401, "proof required", .unauthorized),
            (403, "rank", .rankShortfall),
            (503, #"{"error":"push-binding-disabled"}"#, .pushBindingDisabled),
        ]
        for (status, body, expected) in cases {
            let scope = DsAccessURLProtocol.freshScope()
            DsAccessURLProtocol.script(scope, [.init(status: status, body: Data(body.utf8))])
            do {
                try await DsAccessURLProtocol.client(scope).putStatement(request)
                XCTFail("expected \(expected)")
            } catch let error as DsStatementError {
                XCTAssertEqual(error, expected)
            } catch {
                XCTFail("\(error)")
            }
        }
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [.init(status: 409, body: Data()), .init(status: 409, body: Data())])
        do {
            try await DsAccessURLProtocol.client(scope).putStatement(request)
            XCTFail("a second 409 surfaces")
        } catch let error as DsStatementError {
            XCTAssertEqual(error, .conflict)
        } catch {
            XCTFail("\(error)")
        }
    }
}

// MARK: - ADR-0057 amended 2026-10-01 / ADR-0050 2.5a item 8

extension JournalOwnershipTests {

    /// D(1) — every device of the addressed account opens and stores the seed
    /// from the same envelope, whichever device read it first: the seed is
    /// sealed to the ACCOUNT key both devices hold, and nothing in the gate is
    /// per device.
    func testASecondDeviceStoresTheSeedAnotherDeviceProcessed() throws {
        let owner = try Identity.fromSeed(seed: Data(repeating: 0x31, count: 32))
        let member = try Identity.fromSeed(seed: Data(repeating: 0x32, count: 32))
        let rootSeed = try dsJournalRootSeed(accountIdentitySeed: owner.ed25519Seed(), journalId: gid)
        let seed = try dsJournalRegistrarSeed(rootSeed: rootSeed, memberAccountKey: member.signaturePublicKey())
        let envelope = try composeRegistrarSeedEnvelope(
            journalId: gid, memberAccountKey: member.signaturePublicKey(),
            memberX25519PublicKey: member.x25519PublicKey(), rank: .member, registrarSeed: seed)
        let decoded = try decodeRegistrarSeedEnvelope(plaintext: envelope)
        XCTAssertEqual(decoded.registrarPublicKey, try dsStatementSigner(seed: seed).publicKey(),
                       "the clear header names the key the seed derives")

        func deviceReceives() throws -> RegistrarSeedStore {
            let store = RegistrarSeedStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "dev-\(UUID().uuidString)")))
            let outcome = RootHandover.receiveSeedEnvelope(
                rank: decoded.rank, envelopeJournal: decoded.journalId,
                envelopeAddressee: decoded.memberAccountKey, groupId: gid,
                ownAccountKey: member.signaturePublicKey(),
                senderAccountKey: owner.signaturePublicKey(), owner: owner.signaturePublicKey(),
                open: { try openRegistrarSeedEnvelope(plaintext: envelope, accountIdentity: member) },
                storeRegistrarSeed: { store.put(accountIdB64: "acct", groupIdHex: "g", seed: $0, rank: $1) },
                takeOver: { _ in XCTFail("not a handover") })
            XCTAssertEqual(outcome, .registrarStored)
            return store
        }
        let first = try deviceReceives()
        let second = try deviceReceives()
        XCTAssertEqual(first.get(accountIdB64: "acct", groupIdHex: "g")?.seed, seed)
        XCTAssertEqual(second.get(accountIdB64: "acct", groupIdHex: "g")?.seed, seed,
                       "the second device of the account holds the seed too")
    }

    /// D(2) — deliveries are recorded per DEVICE: a device of an account that
    /// joined after its account's envelope was delivered is owed one, and the
    /// owner re-sends the account's envelope at its next reconcile.
    func testTheOwnerReSendsToANewDeviceOfTheSameAccount() async throws {
        let root = FakeStatementSigner("root")
        let h = ReconcileHarness()
        let account = Data(repeating: 2, count: 32)
        let phone = Data(repeating: 0x21, count: 32)
        let laptop = Data(repeating: 0x22, count: 32)
        var leaves: [Data: [Data]] = [account: [phone]]
        h.authority = .root(root)
        h.desired = [account: .member]
        var ports = h.ports(gid: gid)
        ports.memberLeaves = { leaves }

        _ = try await JournalOwnership.reconcileRegistrars(ports)
        XCTAssertEqual(h.envelopes.count, 1)
        XCTAssertEqual(h.delivered, [phone: .member])
        let unchanged = try await JournalOwnership.reconcileRegistrars(ports)
        XCTAssertEqual(unchanged, .unchanged)

        // The account's second device joined: one more envelope, for the
        // account, and both devices are recorded.
        leaves[account] = [phone, laptop]
        let outcome = try await JournalOwnership.reconcileRegistrars(ports)
        XCTAssertEqual(outcome, .reconciled(setSent: false, pending: 0))
        XCTAssertEqual(h.envelopes.map(\.0), [account, account])
        XCTAssertEqual(h.delivered, [phone: .member, laptop: .member])

        // The phone is gone (removed device): the record drops it, nothing sent.
        leaves[account] = [laptop]
        _ = try await JournalOwnership.reconcileRegistrars(ports)
        XCTAssertEqual(h.envelopes.count, 2)
        XCTAssertEqual(h.delivered, [laptop: .member])
    }

    /// E — an ADMIN that removes a person revokes that person's registrar key,
    /// read from the owner's envelopes; it skips an account the live table
    /// ranks equal or higher (another admin, the owner) and a recorded admin
    /// key: the DS refuses the whole registration over one of those.
    func testAnAdminsRemovalRevokesTheRemovedRegistrarAndSkipsEqualRank() {
        let member = Data(repeating: 4, count: 32)
        let otherAdmin = Data(repeating: 5, count: 32)
        let promoted = Data(repeating: 6, count: 32)
        let unknown = Data(repeating: 7, count: 32)
        let known: [Data: RegistrarSeedStore.KnownRegistrar] = [
            member: .init(publicKey: Data(repeating: 0xE4, count: 32), rank: .member),
            otherAdmin: .init(publicKey: Data(repeating: 0xE5, count: 32), rank: .admin),
            promoted: .init(publicKey: Data(repeating: 0xE6, count: 32), rank: .member),
        ]
        let revoke = JournalOwnership.revokeList(
            authority: .registrar(FakeStatementSigner("admin"), .admin),
            evictedAccountKeys: [member, otherAdmin, promoted, unknown],
            registrarPublicKey: { _ in XCTFail("an admin never derives"); return nil },
            knownRegistrar: { known[$0] },
            equalOrHigherAccounts: [promoted])
        XCTAssertEqual(revoke, [Data(repeating: 0xE4, count: 32)],
                       "only the member's key; the admin (recorded or live) and the unknown are skipped")

        // A member registrar revokes nothing: nobody ranks below it.
        XCTAssertEqual(JournalOwnership.revokeList(
            authority: .registrar(FakeStatementSigner("member"), .member),
            evictedAccountKeys: [member], registrarPublicKey: { _ in nil },
            knownRegistrar: { known[$0] }), [])
        // The root keeps deriving.
        XCTAssertEqual(JournalOwnership.revokeList(
            authority: .root(FakeStatementSigner("root")), evictedAccountKeys: [member],
            registrarPublicKey: { _ in Data(repeating: 0xDD, count: 32) },
            knownRegistrar: { known[$0] }), [Data(repeating: 0xDD, count: 32)])
    }

    /// The admin's revocation travels in its epoch registration.
    func testAnAdminsRegistrationCarriesTheRevocation() async throws {
        let admin = FakeStatementSigner("admin")
        let ds = FakeStatementDs()
        let evicted = Data(repeating: 4, count: 32)
        let key = Data(repeating: 0xE4, count: 32)
        let revoke = JournalOwnership.revokeList(
            authority: .registrar(admin, .admin), evictedAccountKeys: [evicted],
            registrarPublicKey: { _ in nil },
            knownRegistrar: { $0 == evicted ? .init(publicKey: key, rank: .member) : nil })
        try await JournalOwnership.registerEpoch(
            authority: .registrar(admin, .admin), journalId: gid, epoch: 9,
            epochKey: Data(repeating: 9, count: 32), revoke: revoke, ds: ds)
        XCTAssertEqual(admin.lastRevoke, [key])
        XCTAssertEqual(ds.segments, ["access"])
    }

    /// The known-registrar record: the newest envelope wins, scoped per
    /// account and group, wiped with the account.
    func testKnownRegistrarsAreKeptReplacedAndCleared() throws {
        let store = RegistrarSeedStore(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "known-\(UUID().uuidString)")))
        let account = Data(repeating: 4, count: 32)
        store.noteRegistrar(accountIdB64: "acct", groupIdHex: "AB", accountKey: account,
                            publicKey: Data(repeating: 1, count: 32), rank: .member)
        store.noteRegistrar(accountIdB64: "acct", groupIdHex: "ab", accountKey: account,
                            publicKey: Data(repeating: 2, count: 32), rank: .admin)
        XCTAssertEqual(store.knownRegistrars(accountIdB64: "acct", groupIdHex: "ab"),
                       [account: .init(publicKey: Data(repeating: 2, count: 32), rank: .admin)])
        XCTAssertTrue(store.knownRegistrars(accountIdB64: "other", groupIdHex: "ab").isEmpty)
        store.clear(accountIdB64: "acct")
        XCTAssertTrue(store.knownRegistrars(accountIdB64: "acct", groupIdHex: "ab").isEmpty)
    }
}
