// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0060 section 6 — a proposer re-sends its committer envelope while its
/// Add waits, and a committer keeps what it received across a restart.
@MainActor
final class CommitterEnvelopeResendTests: XCTestCase {

    private let twelveHours = committerEnvelopeResendAfterMs()

    private func identity(_ byte: UInt8) throws -> Identity {
        try Identity.fromSeed(seed: Data(repeating: byte, count: 32))
    }

    private func key(_ byte: UInt8) throws -> Data { try identity(byte).signaturePublicKey() }

    private func b64(_ data: Data) -> String { data.base64EncodedString() }

    private func fofGrant(_ byte: UInt8) throws -> FfiReachGrant {
        try deriveReachGrant(accountSeed: identity(byte).ed25519Seed(), class: .fof, scopeId: Data(), version: 0)
    }

    private func openMailbox(_ byte: UInt8) throws -> MailboxTarget {
        MailboxTarget.own(accountIdentity: try identity(byte))
    }

    private func center(
        _ ownByte: UInt8, persistence: ReachPersistence = InMemoryReachPersistence(), account: String? = nil
    ) throws -> ReachCenter {
        let c = ReachCenter(persistence: persistence, ds: UnusedReachDs())
        c.keyResolverProvider = { _, own in AccountKeyResolver(ownCurrentKey: own) }
        c.ownPredecessorSigningSeeds = { _ in [] }
        c.activate(accountIdB64: account ?? "resend-\(ownByte)", accountIdentity: try identity(ownByte))
        return c
    }

    /// A signed envelope for the person seeded by `added`, naming `delegates`,
    /// from a proposer holding a grant for them.
    private func signedEnvelope(
        added: UInt8, delegates: [Data], proposer: UInt8 = 0x31, nowMs: UInt64? = nil
    ) throws -> FfiCommitterDelegationEnvelope {
        let proposing = try center(proposer)
        if let nowMs { proposing.clockMs = { nowMs } }
        XCTAssertTrue(proposing.keepFoundGrant(try fofGrant(added), accountKey: try key(added)))
        return try decodeCommitterDelegationEnvelope(plaintext: try XCTUnwrap(proposing.composeDelegations(
            addedAccountKey: try key(added), candidateAccountKeys: delegates, groupIdHex: "aa")))
    }

    // MARK: - Which of this device's requests the rule is asked about

    private func snapshot(
        people: [String] = ["me"], pending: [String] = [], ended: Bool = false, torn: Bool = false
    ) -> SupersededSnapshot {
        SupersededSnapshot(
            epoch: 5,
            leaves: torn ? nil : people.enumerated().map {
                SupersededLeaf(leafIndex: UInt32($0.offset), leafKeyB64: "leaf-\($0.element)", personKeyB64: $0.element)
            },
            roleTable: RetryRoleTable(ownerKeyB64: "me", adminKeysB64: []),
            pendingKeys: pending,
            membershipEnded: ended,
            retainedJoinKeys: [])
    }

    private func addIntent(person: String = "ann", leaves: [String] = ["ann-1", "ann-2"]) -> MembershipIntent {
        MembershipIntent(request: .add(personKeyB64: person, accountIdB64: "id", leafKeysB64: leaves), epoch: 5)
    }

    func testAnAddWithAPendingProposalWaits() {
        XCTAssertTrue(ownAddStillWaiting(addIntent(), snapshot(pending: ["add:ann-2"])))
    }

    func testAnAddThatLandedDoesNotWait() {
        XCTAssertFalse(ownAddStillWaiting(addIntent(), snapshot(people: ["me", "ann"], pending: ["add:ann-2"])))
    }

    func testAnAddThatWasDroppedDoesNotWait() {
        XCTAssertFalse(ownAddStillWaiting(addIntent(), snapshot(pending: ["add:someone-else"])))
    }

    func testOtherRequestsAreNotAsked() {
        let removal = MembershipIntent(request: .remove(personKeyB64: "ann"), epoch: 5)
        XCTAssertFalse(ownAddStillWaiting(removal, snapshot(pending: ["add:ann-1"])))
    }

    func testATornReadOrAnEndedMembershipSendsNothing() {
        XCTAssertFalse(ownAddStillWaiting(addIntent(), snapshot(pending: ["add:ann-1"], torn: true)))
        XCTAssertFalse(ownAddStillWaiting(addIntent(), snapshot(pending: ["add:ann-1"], ended: true)))
    }

    // MARK: - The core's rule, over what is remembered

    func testNothingSentYetIsDue() throws {
        XCTAssertTrue(committerEnvelopeIsDue(nil, candidates: [try key(1)], nowMs: 10))
    }

    func testASignedEnvelopeIsDueTwelveHoursLater() throws {
        let a = try key(1)
        let sent = SentCommitterEnvelope(sentAtMs: 1_000, signed: true, coveredB64: [b64(a)])
        XCTAssertFalse(committerEnvelopeIsDue(sent, candidates: [a], nowMs: 1_000 + twelveHours - 1))
        XCTAssertTrue(committerEnvelopeIsDue(sent, candidates: [a], nowMs: 1_000 + twelveHours))
    }

    func testANewCandidateIsDueAtOnceAndADepartedOneIsNot() throws {
        let a = try key(1), b = try key(2)
        let sent = SentCommitterEnvelope(sentAtMs: 1_000, signed: true, coveredB64: [b64(a)])
        XCTAssertTrue(committerEnvelopeIsDue(sent, candidates: [a, b], nowMs: 1_001))
        XCTAssertFalse(committerEnvelopeIsDue(sent, candidates: [], nowMs: 1_001))
    }

    func testAnOpenEnvelopeNeverAges() throws {
        let a = try key(1), b = try key(2)
        let sent = SentCommitterEnvelope(sentAtMs: 1_000, signed: false, coveredB64: [b64(a)])
        XCTAssertFalse(committerEnvelopeIsDue(sent, candidates: [a], nowMs: .max))
        XCTAssertTrue(committerEnvelopeIsDue(sent, candidates: [a, b], nowMs: 1_001))
    }

    // MARK: - What is remembered of a send

    private func store() -> PendingRequestStore {
        PendingRequestStore(defaults: UserDefaults(suiteName: "resend-\(UUID().uuidString)")!)
    }

    func testTheSendIsKeptOnTheIntentAndSurvivesARead() {
        let requests = store()
        requests.recordIntent(accountIdB64: "a", groupIdB64: "g", addIntent())
        let sent = SentCommitterEnvelope(sentAtMs: 7, signed: true, coveredB64: ["x", "y"])
        requests.recordCommitterEnvelope(accountIdB64: "a", groupIdB64: "g", personKeyB64: "ann", sent)

        let intent = requests.intents(accountIdB64: "a", groupIdB64: "g").first
        XCTAssertEqual(intent?.lastEnvelope, sent)
        XCTAssertEqual(intent?.request, addIntent().request)
        XCTAssertEqual(intent?.epoch, 5)
    }

    func testNoIntentNoRecord() {
        let requests = store()
        requests.recordCommitterEnvelope(
            accountIdB64: "a", groupIdB64: "g", personKeyB64: "ann",
            SentCommitterEnvelope(sentAtMs: 7, signed: true, coveredB64: []))
        XCTAssertTrue(requests.intents(accountIdB64: "a", groupIdB64: "g").isEmpty)
    }

    func testARecordIsForgottenWithItsIntentAndTheAccount() {
        let requests = store()
        requests.recordIntent(accountIdB64: "a", groupIdB64: "g", addIntent())
        requests.recordCommitterEnvelope(
            accountIdB64: "a", groupIdB64: "g", personKeyB64: "ann",
            SentCommitterEnvelope(sentAtMs: 7, signed: true, coveredB64: []))
        // A re-request replaces the intent: the fresh proposal gets a fresh envelope.
        requests.recordIntent(accountIdB64: "a", groupIdB64: "g", addIntent())
        XCTAssertNil(requests.intents(accountIdB64: "a", groupIdB64: "g").first?.lastEnvelope)

        requests.clearForAccount(accountIdB64: "a")
        XCTAssertTrue(requests.intents(accountIdB64: "a", groupIdB64: "g").isEmpty)
    }

    func testAnEnvelopeRowOfTheWrongShapeReadsAsNeverSent() throws {
        var plist = addIntent().plist
        plist["envelope"] = ["sentAtMs": "soon"]
        let intent = try XCTUnwrap(MembershipIntent(plist: plist))
        XCTAssertNil(intent.lastEnvelope)
        XCTAssertEqual(intent.request, addIntent().request)
    }

    // MARK: - Composing

    func testAFreshEnvelopeCarriesFreshDelegations() throws {
        let c = try center(0x30)
        let person = try key(0x01), committer = try key(0x31)
        XCTAssertTrue(c.keepFoundGrant(try fofGrant(0x01), accountKey: person))
        var now: UInt64 = 1_000_000
        c.clockMs = { now }

        let first = try XCTUnwrap(c.composeDelegationEnvelope(
            addedAccountKey: person, candidateAccountKeys: [committer], groupIdHex: "aa"))
        now += twelveHours
        let second = try XCTUnwrap(c.composeDelegationEnvelope(
            addedAccountKey: person, candidateAccountKeys: [committer], groupIdHex: "aa"))

        let delegations = try [first, second].map {
            try XCTUnwrap(checkCommitterDelegations(
                envelope: decodeCommitterDelegationEnvelope(plaintext: $0.wire)).first)
        }
        XCTAssertEqual(delegations[1].expiresAtMs - delegations[0].expiresAtMs, twelveHours)
        XCTAssertNotEqual(delegations[0].nonce, delegations[1].nonce)
        XCTAssertEqual(first.covered, [committer])
    }

    func testCandidatesPastTheCapAreNotNamedAndNeverCountAsUncovered() throws {
        let c = try center(0x30)
        let person = try key(0x01)
        XCTAssertTrue(c.keepFoundGrant(try fofGrant(0x01), accountKey: person))
        let max = Int(maxCommitterDelegations())
        let all = try (0..<(max + 3)).map { try key(UInt8(0x40 + $0)) }

        let candidates = c.delegationCandidates(all + [all[0]])
        XCTAssertEqual(candidates, Array(all.prefix(max)))
        let composed = try XCTUnwrap(c.composeDelegationEnvelope(
            addedAccountKey: person, candidateAccountKeys: all, groupIdHex: "aa"))
        XCTAssertEqual(composed.covered, candidates)

        let sent = SentCommitterEnvelope(sentAtMs: 1_000, signed: true, coveredB64: composed.covered.map(b64))
        XCTAssertFalse(committerEnvelopeIsDue(sent, candidates: candidates, nowMs: 1_001), "or it would go every poll")
    }

    // MARK: - A committer keeps what it received

    func testAKeptEnvelopeSurvivesARestart() throws {
        let persistence = InMemoryReachPersistence()
        let person = try key(0x01)
        let envelope = try signedEnvelope(added: 0x01, delegates: [try key(0x30)])
        let before = try center(0x30, persistence: persistence, account: "committer")
        before.keepDelegation(envelope, fromAccountKeyB64: b64(try key(0x31)), groupIdHex: "AA")

        before.deactivate()
        XCTAssertNil(before.writePlan(toAccountKey: person))
        let restarted = try center(0x30, persistence: persistence, account: "committer")

        let plan = try XCTUnwrap(restarted.writePlan(toAccountKey: person, groupIdHex: "aa"))
        XCTAssertEqual(plan.target, try openMailbox(0x01))
        XCTAssertNotNil(plan.candidates.first?.delegationHeader)
        XCTAssertEqual(persistence.load(accountIdB64: "committer")?.keptEnvelopes?.first?.groupIdHex, "aa")
    }

    func testAnOpenEnvelopeSurvivesARestartAndGoesWithTheWelcome() throws {
        let persistence = InMemoryReachPersistence()
        let person = try key(0x02)
        let open = try decodeCommitterDelegationEnvelope(
            plaintext: try XCTUnwrap(try center(0x31).composeOpenEnvelope(target: try openMailbox(0x02))))
        try center(0x30, persistence: persistence, account: "committer")
            .keepDelegation(open, fromAccountKeyB64: b64(try key(0x31)), groupIdHex: "aa")

        let restarted = try center(0x30, persistence: persistence, account: "committer")
        XCTAssertEqual(restarted.openWelcomeTarget(ofPerson: person, groupIdHex: "aa"), try openMailbox(0x02))

        restarted.dropOpenAdd(groupIdHex: "aa", person: person)
        XCTAssertNil(restarted.openWelcomeTarget(ofPerson: person, groupIdHex: "aa"))
        XCTAssertNil(persistence.load(accountIdB64: "committer")?.keptEnvelopes)
    }

    func testTheNewestPerGroupSenderAndPersonIsKept() throws {
        let persistence = InMemoryReachPersistence()
        let committing = try center(0x30, persistence: persistence, account: "committer")
        let envelope = try signedEnvelope(added: 0x01, delegates: [try key(0x30)])
        let proposer = b64(try key(0x31)), otherProposer = b64(try key(0x32))

        committing.keepDelegation(envelope, fromAccountKeyB64: proposer, groupIdHex: "aa")
        committing.keepDelegation(envelope, fromAccountKeyB64: proposer, groupIdHex: "aa")
        committing.keepDelegation(envelope, fromAccountKeyB64: proposer, groupIdHex: "bb")
        committing.keepDelegation(envelope, fromAccountKeyB64: otherProposer, groupIdHex: "aa")

        XCTAssertEqual(persistence.load(accountIdB64: "committer")?.keptEnvelopes?.count, 3)
    }

    func testAnExpiredEnvelopeIsPrunedAndNeverOffered() throws {
        let persistence = InMemoryReachPersistence()
        let person = try key(0x01)
        let committing = try center(0x30, persistence: persistence, account: "committer")
        let envelope = try signedEnvelope(added: 0x01, delegates: [try key(0x30)])
        let expires = try XCTUnwrap(checkCommitterDelegations(envelope: envelope).first).expiresAtMs
        committing.keepDelegation(envelope, fromAccountKeyB64: b64(try key(0x31)), groupIdHex: "aa")
        XCTAssertNotNil(committing.writeAuth(toAccountKey: person))

        // Past its expiry nothing is offered, before anything prunes it.
        committing.clockMs = { expires }
        XCTAssertNil(committing.writeAuth(toAccountKey: person))

        // The next envelope prunes it.
        let another = try signedEnvelope(added: 0x02, delegates: [try key(0x30)], nowMs: expires)
        committing.keepDelegation(another, fromAccountKeyB64: b64(try key(0x32)), groupIdHex: "aa")
        XCTAssertEqual(
            persistence.load(accountIdB64: "committer")?.keptEnvelopes?.map(\.senderKeyB64),
            [b64(try key(0x32))])
    }

    func testARestartPrunesWhatExpired() throws {
        let persistence = InMemoryReachPersistence()
        let envelope = try signedEnvelope(added: 0x01, delegates: [try key(0x30)])
        let expires = try XCTUnwrap(checkCommitterDelegations(envelope: envelope).first).expiresAtMs
        var stored = ReachRecord()
        stored.keptEnvelopes = [ReachRecord.KeptEnvelope(
            groupIdHex: "aa", senderKeyB64: b64(try key(0x31)),
            envelopeB64: try composeCommitterDelegationEnvelope(envelope: envelope).base64EncodedString(),
            storedAtMs: 1)]
        persistence.save(stored, accountIdB64: "committer")

        let c = ReachCenter(persistence: persistence, ds: UnusedReachDs())
        c.keyResolverProvider = { _, own in AccountKeyResolver(ownCurrentKey: own) }
        c.clockMs = { expires + 1 }
        c.activate(accountIdB64: "committer", accountIdentity: try identity(0x30))

        XCTAssertNil(persistence.load(accountIdB64: "committer")?.keptEnvelopes)
        XCTAssertNil(c.writePlan(toAccountKey: try key(0x01)))
    }

    func testARowThatDoesNotDecodeIsDropped() throws {
        let persistence = InMemoryReachPersistence()
        var stored = ReachRecord()
        stored.keptEnvelopes = [ReachRecord.KeptEnvelope(
            groupIdHex: "aa", senderKeyB64: "x", envelopeB64: "not an envelope", storedAtMs: 1)]
        persistence.save(stored, accountIdB64: "committer")
        _ = try center(0x30, persistence: persistence, account: "committer")
        XCTAssertNil(persistence.load(accountIdB64: "committer")?.keptEnvelopes)
    }

    func testTheOldestIsDroppedOverTheCap() throws {
        let persistence = InMemoryReachPersistence()
        let committing = try center(0x30, persistence: persistence, account: "committer")
        let envelope = try signedEnvelope(added: 0x01, delegates: [try key(0x30)])
        for sender in 0..<(ReachCenter.maxKeptEnvelopes + 2) {
            committing.keepDelegation(envelope, fromAccountKeyB64: "sender-\(sender)", groupIdHex: "aa")
        }
        let kept = try XCTUnwrap(persistence.load(accountIdB64: "committer")?.keptEnvelopes)
        XCTAssertEqual(kept.count, ReachCenter.maxKeptEnvelopes)
        XCTAssertEqual(kept.first?.senderKeyB64, "sender-2")
    }

    func testForgettingTheAccountWipesThem() throws {
        let persistence = InMemoryReachPersistence()
        let committing = try center(0x30, persistence: persistence, account: "committer")
        committing.keepDelegation(
            try signedEnvelope(added: 0x01, delegates: [try key(0x30)]),
            fromAccountKeyB64: b64(try key(0x31)), groupIdHex: "aa")
        committing.forget(accountIdB64: "committer")
        XCTAssertNil(persistence.load(accountIdB64: "committer"))
    }

    // MARK: - An envelope from another device of this account

    /// Device B of this account proposed the Add and holds the grant; device
    /// A sweeps. A has no other way to the person.
    func testAnEnvelopeFromAnotherOwnDeviceOpensTheWelcome() throws {
        let persistence = InMemoryReachPersistence()
        let sweeping = try center(0x30, persistence: persistence, account: "own-a")
        let person = try key(0x01)
        XCTAssertNil(sweeping.writePlan(toAccountKey: person, groupIdHex: "aa"))
        let envelope = try signedEnvelope(added: 0x01, delegates: [try key(0x30)], proposer: 0x30)

        sweeping.keepDelegation(envelope, fromAccountKeyB64: b64(try key(0x30)), groupIdHex: "aa")

        let plan = try XCTUnwrap(sweeping.writePlan(toAccountKey: person, groupIdHex: "aa"))
        XCTAssertEqual(plan.target, try openMailbox(0x01))
        XCTAssertNotNil(plan.candidates.first?.delegationHeader)
    }

    func testTheReceivePathKeepsAnEnvelopeOfThisAccount() async throws {
        let fx = try await makeFixture(grant: false)
        defer { fx.cleanUp() }
        let person = try key(0x01)
        let envelope = try signedEnvelope(
            added: 0x01, delegates: [fx.owner], proposer: 0x30, nowMs: fx.clock.nowMs)

        fx.repo.receiveScopedDiscovery(
            groupIdHex: fx.groupIdHex, envelope: .committerDelegation(envelope), senderKey: b64(fx.owner))

        XCTAssertNotNil(fx.reach.writePlan(toAccountKey: person, groupIdHex: fx.groupIdHex))
    }

    /// What this device wrote itself is its own record: the other device's
    /// envelope for the same person leaves it alone.
    func testAnotherOwnDeviceDoesNotReplaceThisDevicesOwnRecord() throws {
        let persistence = InMemoryReachPersistence()
        let own = try center(0x30, persistence: persistence, account: "own-record")
        let target = try openMailbox(0x02)
        own.recordOpenAdd(groupIdHex: "aa", target: target)
        let before = persistence.load(accountIdB64: "own-record")?.openAdds
        let open = try decodeCommitterDelegationEnvelope(
            plaintext: try XCTUnwrap(try center(0x30).composeOpenEnvelope(target: target)))

        own.keepDelegation(open, fromAccountKeyB64: b64(try key(0x30)), groupIdHex: "bb")

        XCTAssertEqual(persistence.load(accountIdB64: "own-record")?.openAdds, before)
        XCTAssertNotNil(own.openWelcomeTarget(ofPerson: try key(0x02), groupIdHex: "aa"))
    }

    /// An envelope with no delegation has the same bytes for every group, and
    /// which group it is for is what authorises the write.
    func testTheSameOpenEnvelopeInTwoGroupsIsKeptForEach() throws {
        let persistence = InMemoryReachPersistence()
        let own = try center(0x30, persistence: persistence, account: "own-two")
        let open = try decodeCommitterDelegationEnvelope(
            plaintext: try XCTUnwrap(try center(0x30).composeOpenEnvelope(target: try openMailbox(0x02))))
        let sender = b64(try key(0x30))

        own.keepDelegation(open, fromAccountKeyB64: sender, groupIdHex: "aa")
        own.keepDelegation(open, fromAccountKeyB64: sender, groupIdHex: "bb")

        XCTAssertEqual(persistence.load(accountIdB64: "own-two")?.keptEnvelopes?.count, 2)
        XCTAssertNotNil(own.openWelcomeTarget(ofPerson: try key(0x02), groupIdHex: "aa"))
        own.dropOpenAdd(groupIdHex: "aa", person: try key(0x02))
        XCTAssertNil(own.openWelcomeTarget(ofPerson: try key(0x02), groupIdHex: "aa"))
        XCTAssertNotNil(own.openWelcomeTarget(ofPerson: try key(0x02), groupIdHex: "bb"))
    }

    // MARK: - A fresh envelope wakes an owed Welcome

    func testAReSentEnvelopeTellsTheWelcomeRetry() throws {
        let committing = try center(0x30)
        var woken: [String] = []
        committing.onGrantKept = { woken.append($0) }
        let person = try key(0x01)
        let envelope = try signedEnvelope(added: 0x01, delegates: [try key(0x30)])

        committing.keepDelegation(envelope, fromAccountKeyB64: b64(try key(0x31)), groupIdHex: "aa")
        committing.keepDelegation(
            try signedEnvelope(added: 0x01, delegates: [try key(0x30)]),
            fromAccountKeyB64: b64(try key(0x31)), groupIdHex: "aa")

        XCTAssertEqual(woken, [b64(person), b64(person)])
    }

    func testAnEnvelopeReachesTheOwedWelcomeOfItsGroupOnly() async throws {
        let fx = try await makeFixture()
        defer { fx.cleanUp() }
        let otherGroupId = Data(repeating: 0x5A, count: 32)
        fx.repo.groups["5a5a"] = GroupRecord(
            group: try Group.create(identity: try identity(0x77), kind: .group), groupId: otherGroupId)
        try fx.outbox.putEntryDurable(
            accountIdB64: fx.accountIdB64,
            entry: PendingCommitEntry(
                id: "owed", accountId: fx.accountIdB64, groupId: base64Encode(fx.groupId),
                targetEpoch: "1", commitB64: "", sweepWelcomesOwed: true))
        var retried: [String] = []
        let expectation = expectation(description: "retry")
        fx.repo.owedWelcomeRetry = { group in
            retried.append(group)
            expectation.fulfill()
        }

        // The hook the repository hands the reach store.
        fx.repo.claimOwnMailboxes()
        ReachCenter.shared.onGrantKept?(b64(try key(0x01)))

        await fulfillment(of: [expectation], timeout: 5)
        XCTAssertEqual(retried, [fx.groupIdHex])
    }

    // MARK: - The proposing device

    private final class Clock {
        var nowMs: UInt64 = 1_800_000_000_000
    }

    private struct Fixture {
        let repo: ChatRepository
        let groupIdHex: String
        let groupId: Data
        let accountIdB64: String
        let snapshotStore: IdentitySnapshotStore
        let requests: PendingRequestStore
        let outbox: CommitOutboxStore
        let reach: ReachCenter
        let clock: Clock
        let owner: Data
        let admin: Data

        var intent: MembershipIntent? {
            requests.intents(accountIdB64: accountIdB64, groupIdB64: base64Encode(groupId)).first
        }

        /// Journal rows since the fixture settled.
        var rows: Int { SendEpochStraddleTests.JournalURLProtocol.journal.count - 1 }

        func cleanUp() { snapshotStore.deleteSnapshot(accountIdB64: accountIdB64) }
    }

    private let personAccountId = Data(repeating: 0x71, count: 16)

    /// An owner on a live group with one admin key in its role table, and a
    /// real journal and directory behind it. `grant`: the reach store holds
    /// one for the person who is added.
    private func makeFixture(grant: Bool = true) async throws -> Fixture {
        let seed = Data(repeating: 0x30, count: 32)
        let founder = try Identity.fromSeed(seed: seed)
        let group = try Group.create(identity: founder, kind: .group)
        let admin = try key(0x77)
        _ = try group.setGroupRoles(
            identity: founder, owner: try founder.signaturePublicKey(), admins: [admin])
        let groupId = try group.groupId()
        let groupIdHex = DsClient.pseudonymHex(groupId)
        let snapshot = try founder.snapshotStorage()

        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
        }
        let accountIdB64 = base64Encode(accountIdBytes)
        let snapshotStore = IdentitySnapshotStore()
        let knownGroups = KnownGroupsStore(
            defaults: UserDefaults(suiteName: "resend-k-\(UUID().uuidString)")!)
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: snapshot)
        knownGroups.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)

        try await Task.sleep(nanoseconds: 60_000_000)
        SendEpochStraddleTests.JournalURLProtocol.reset(host: "resend.journal.test")
        let dsConfig = URLSessionConfiguration.ephemeral
        dsConfig.protocolClasses = [SendEpochStraddleTests.JournalURLProtocol.self]
        let ds = DsClient(baseURL: "http://resend.journal.test", session: URLSession(configuration: dsConfig))

        // The person's one device, bound to their account.
        let device = try Identity.fromSeedBound(
            seed: Data(repeating: 0x11, count: 32), accountSeed: try identity(0x01).ed25519Seed(),
            issuedAtUnixSecs: 1_700_000_000)
        AddMemberJunctionTests.DirectoryStub.serve(
            [(deviceId: device.signaturePublicKey(), keyPackage: try device.createKeyPackage())])
        let directoryConfig = URLSessionConfiguration.ephemeral
        directoryConfig.protocolClasses = [AddMemberJunctionTests.DirectoryStub.self]

        let outbox = CommitOutboxStore(
            rootDir: FileManager.default.temporaryDirectory
                .appendingPathComponent("resend-\(UUID().uuidString)"))
        let requests = store()
        let repo = ChatRepository(
            chatClient: ChatClient(ds: ds),
            dsClient: ds,
            discovery: DiscoveryClient(
                baseURL: "http://directory.resend.test",
                session: URLSession(configuration: directoryConfig),
                tokens: OfflineAnonTokens()),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroups,
            messageLogStore: MessageLogStore(),
            commitOutbox: outbox,
            pushRouting: Offline.pushRouting(),
            beaconStore: BeaconObservationStore(
                defaults: UserDefaults(suiteName: "resend-b-\(UUID().uuidString)")!),
            pendingRequestStore: requests)
        repo.deviceIdentityInstaller = { _, _ in nil }
        repo.addIdentityGate = { _ in }
        let clock = Clock()
        let reach = try center(0x30)
        reach.clockMs = { clock.nowMs }
        repo.committerEnvelopeReach = { reach }
        repo.committerEnvelopeClock = { clock.nowMs }
        repo.committerEnvelopeForeground = { true }
        if grant {
            XCTAssertTrue(reach.keepFoundGrant(try fofGrant(0x01), accountKey: try key(0x01)))
        }
        repo.bootstrap(secrets: IdentitySecrets(
            accountId: accountIdBytes,
            exportKey: Data(repeating: 0x22, count: 32),
            accountIdentitySeed: seed,
            sessionToken: "t",
            sessionExpiresAtMs: 0))
        // The owner mints the group's push routing secret on its own at
        // bootstrap and posts it: one row, none of these tests' business.
        let settled = Date().addingTimeInterval(5)
        while SendEpochStraddleTests.JournalURLProtocol.journal.isEmpty, Date() < settled {
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTAssertEqual(SendEpochStraddleTests.JournalURLProtocol.journal.count, 1)
        return Fixture(
            repo: repo, groupIdHex: groupIdHex, groupId: groupId, accountIdB64: accountIdB64,
            snapshotStore: snapshotStore, requests: requests, outbox: outbox, reach: reach, clock: clock,
            owner: try founder.signaturePublicKey(), admin: admin)
    }

    private func propose(_ fx: Fixture, open: MailboxTarget? = nil) async throws {
        let outcome = try await fx.repo.addMemberToGroup(
            groupIdHex: fx.groupIdHex, recipientAccountId: personAccountId,
            recipientSignaturePubkey: try key(0x01), openMailbox: open)
        guard case .proposed = outcome else { return XCTFail("the add must wait for a sweep") }
    }

    /// Rewrite the record as if `covered` were all the last send reached (the
    /// person made admin since then).
    private func forget(_ fx: Fixture, candidate: Data) throws {
        let last = try XCTUnwrap(fx.intent?.lastEnvelope)
        fx.requests.recordCommitterEnvelope(
            accountIdB64: fx.accountIdB64, groupIdB64: base64Encode(fx.groupId),
            personKeyB64: b64(try key(0x01)),
            SentCommitterEnvelope(
                sentAtMs: last.sentAtMs, signed: last.signed,
                coveredB64: last.coveredB64.filter { $0 != b64(candidate) }))
    }

    private func resend(_ fx: Fixture) async {
        await fx.repo.resendCommitterEnvelopesIfDue(groupIdHex: fx.groupIdHex)
    }

    func testTheFirstSendIsRecorded() async throws {
        let fx = try await makeFixture()
        defer { fx.cleanUp() }
        try await propose(fx)

        XCTAssertEqual(fx.rows, 2, "the proposal and its envelope")
        let last = try XCTUnwrap(fx.intent?.lastEnvelope)
        XCTAssertEqual(last.sentAtMs, fx.clock.nowMs)
        XCTAssertTrue(last.signed)
        XCTAssertEqual(last.coveredB64, [b64(fx.owner), b64(fx.admin)])
    }

    func testNoReSendWhileTheEnvelopeIsYoung() async throws {
        let fx = try await makeFixture()
        defer { fx.cleanUp() }
        try await propose(fx)
        fx.clock.nowMs += twelveHours - 1
        await resend(fx)
        XCTAssertEqual(fx.rows, 2)
    }

    func testAfterTwelveHoursAFreshEnvelopeGoesOutAndTheRecordMoves() async throws {
        let fx = try await makeFixture()
        defer { fx.cleanUp() }
        try await propose(fx)
        fx.clock.nowMs += twelveHours
        await resend(fx)

        XCTAssertEqual(fx.rows, 3)
        XCTAssertEqual(fx.intent?.lastEnvelope?.sentAtMs, fx.clock.nowMs)
        XCTAssertEqual(fx.intent?.lastEnvelope?.signed, true)

        // Young again.
        await resend(fx)
        XCTAssertEqual(fx.rows, 3)
    }

    func testANewAdminMakesItDueAtOnce() async throws {
        let fx = try await makeFixture()
        defer { fx.cleanUp() }
        try await propose(fx)
        try forget(fx, candidate: fx.admin)
        await resend(fx)

        XCTAssertEqual(fx.rows, 3)
        XCTAssertEqual(fx.intent?.lastEnvelope?.coveredB64, [b64(fx.owner), b64(fx.admin)])
    }

    func testAnOpenEnvelopeIsReSentOnlyForANewCandidate() async throws {
        let fx = try await makeFixture(grant: false)
        defer { fx.cleanUp() }
        try await propose(fx, open: try openMailbox(0x01))
        XCTAssertEqual(fx.intent?.lastEnvelope?.signed, false)
        XCTAssertEqual(fx.rows, 2)

        fx.clock.nowMs += 30 * twelveHours
        await resend(fx)
        XCTAssertEqual(fx.rows, 2, "a binding does not expire")

        try forget(fx, candidate: fx.admin)
        await resend(fx)
        XCTAssertEqual(fx.rows, 3)
        XCTAssertEqual(fx.intent?.lastEnvelope?.signed, false)
        XCTAssertEqual(fx.intent?.lastEnvelope?.coveredB64, [b64(fx.owner), b64(fx.admin)])
    }

    func testNoReSendForAnAddThatLandedAndTheRecordGoesWithIt() async throws {
        let fx = try await makeFixture()
        defer { fx.cleanUp() }
        try await propose(fx)
        let record = try XCTUnwrap(fx.repo.groups[fx.groupIdHex])
        _ = try record.group.commitPendingProposals(identity: try XCTUnwrap(fx.repo.canonicalIdentity))
        fx.clock.nowMs += twelveHours

        await resend(fx)
        XCTAssertEqual(fx.rows, 2)

        await fx.repo.reRequestSupersededIfDue(groupIdHex: fx.groupIdHex)
        XCTAssertNil(fx.intent)
    }

    func testNoReSendForAnotherDevicesAdd() async throws {
        let fx = try await makeFixture()
        defer { fx.cleanUp() }
        try await propose(fx)
        // The proposal is pending, but no request of this device names it.
        fx.requests.clearIntents(accountIdB64: fx.accountIdB64, groupIdB64: base64Encode(fx.groupId))
        fx.clock.nowMs += twelveHours
        await resend(fx)
        XCTAssertEqual(fx.rows, 2)
    }

    func testNothingToSendSendsNothingAndComesBackWithAGrant() async throws {
        let fx = try await makeFixture()
        defer { fx.cleanUp() }
        try await propose(fx)
        let last = fx.intent?.lastEnvelope
        fx.reach.dropHeld(accountKeyB64: b64(try key(0x01)))
        fx.clock.nowMs += twelveHours
        for _ in 0..<3 { await resend(fx) }

        XCTAssertEqual(fx.rows, 2)
        XCTAssertEqual(fx.intent?.lastEnvelope, last, "nothing went out, so nothing is recorded")

        XCTAssertTrue(fx.reach.keepFoundGrant(try fofGrant(0x01), accountKey: try key(0x01)))
        await resend(fx)
        XCTAssertEqual(fx.rows, 3)
    }

    func testAFailedPublishRecordsNothingAndTheNextPollTries() async throws {
        let fx = try await makeFixture()
        defer {
            SendEpochStraddleTests.JournalURLProtocol.failPublishes(false)
            fx.cleanUp()
        }
        try await propose(fx)
        let last = fx.intent?.lastEnvelope
        fx.clock.nowMs += twelveHours

        SendEpochStraddleTests.JournalURLProtocol.failPublishes(true)
        await resend(fx)
        XCTAssertEqual(fx.intent?.lastEnvelope, last)

        SendEpochStraddleTests.JournalURLProtocol.failPublishes(false)
        await resend(fx)
        XCTAssertEqual(fx.rows, 3)
        XCTAssertEqual(fx.intent?.lastEnvelope?.sentAtMs, fx.clock.nowMs)
    }

    func testABackgroundPollSendsNothing() async throws {
        let fx = try await makeFixture()
        defer { fx.cleanUp() }
        try await propose(fx)
        fx.repo.committerEnvelopeForeground = { false }
        fx.clock.nowMs += twelveHours
        await resend(fx)
        XCTAssertEqual(fx.rows, 2)
    }

    func testOverlappingPollsSendOneEnvelope() async throws {
        let fx = try await makeFixture()
        defer { fx.cleanUp() }
        try await propose(fx)
        fx.clock.nowMs += twelveHours

        async let first: Void = resend(fx)
        async let second: Void = resend(fx)
        _ = await (first, second)

        XCTAssertEqual(fx.rows, 3)
    }

    func testTheTailOfAPollAsks() async throws {
        let fx = try await makeFixture()
        defer { fx.cleanUp() }
        let asked = expectation(description: "the re-send check ran")
        asked.assertForOverFulfill = false
        fx.repo.committerEnvelopeForeground = {
            asked.fulfill()
            return false
        }
        try await fx.repo.pollGroup(groupIdHex: fx.groupIdHex, waitSecs: 0)
        await fulfillment(of: [asked], timeout: 5)
    }
}

private final class UnusedReachDs: ReachDsTransport {
    struct Unused: Error {}

    func updateReach(
        pseudonym: Data, add: [DsReachKeyAdd], remove: [Data], open: Bool?, signer: DsRequestSigner
    ) async throws {
        throw Unused()
    }

    func fetchReach(pseudonym: Data, signer: DsRequestSigner) async throws -> DsReachListing {
        throw Unused()
    }
}
