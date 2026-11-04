// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0060 section 6, "An add of a person found by search", and ADR-0050
/// section 2.5a item 7 — a person picked on Add members from the network
/// search, who is not a contact, gets the group Welcome: the pick keeps what
/// the result carried, a found grant is held, a person found in Everyone is
/// named by a committer-delegation envelope with no delegation, and a
/// committer writes unsigned only for such a person and only when it holds no
/// signed way in.
@MainActor
final class FoundPersonWelcomeTests: XCTestCase {

    private func identity(_ byte: UInt8) throws -> Identity {
        try Identity.fromSeed(seed: Data(repeating: byte, count: 32))
    }

    private func key(_ byte: UInt8) throws -> Data { try identity(byte).signaturePublicKey() }

    private func b64(_ data: Data) -> String { data.base64EncodedString() }

    /// The verified Everyone mailbox of the account seeded by `byte` (what its
    /// public record's binding names).
    private func openMailbox(_ byte: UInt8) throws -> MailboxTarget {
        MailboxTarget.own(accountIdentity: try identity(byte))
    }

    private func fofGrant(_ byte: UInt8) throws -> FfiReachGrant {
        try deriveReachGrant(accountSeed: identity(byte).ed25519Seed(), class: .fof, scopeId: Data(), version: 0)
    }

    private func contactGrant(_ byte: UInt8) throws -> FfiReachGrant {
        try deriveReachGrant(accountSeed: identity(byte).ed25519Seed(), class: .contact, scopeId: Data(), version: 0)
    }

    private func center(_ ownByte: UInt8) throws -> ReachCenter {
        let c = ReachCenter(persistence: InMemoryReachPersistence(), ds: FailingReachDs())
        c.keyResolverProvider = { _, own in AccountKeyResolver(ownCurrentKey: own) }
        c.ownPredecessorSigningSeeds = { _ in [] }
        c.activate(accountIdB64: "found-welcome-\(ownByte)", accountIdentity: try identity(ownByte))
        return c
    }

    private let group = "a1b2"

    // MARK: - The pick keeps what the result carries

    func testAGrantPickKeepsTheFoundGrant() throws {
        let person = try key(0x01)
        let grant = try fofGrant(0x01)
        let found = FoundPerson(
            accountKey: person, accountId: Data(repeating: 1, count: 16), displayName: "Ann",
            username: nil, writeSeed: grant.seed, reach: .grant,
            mailbox: MailboxTarget.verifiedOrNil(grant: grant, accountKey: person), grant: grant)
        let pick = try XCTUnwrap(AddMembersRules.pick(found, fallbackName: "Someone"))
        XCTAssertEqual(pick.foundGrant, grant)
        XCTAssertNil(pick.openMailbox, "a grant person is not written to unsigned")
    }

    func testAnEveryonePickKeepsTheVerifiedMailbox() throws {
        let person = try key(0x02)
        let found = FoundPerson(
            accountKey: person, accountId: Data(repeating: 2, count: 16), displayName: "Bob",
            username: "bob", writeSeed: nil, reach: .open, mailbox: try openMailbox(0x02))
        let pick = try XCTUnwrap(AddMembersRules.pick(found, fallbackName: "Someone"))
        XCTAssertEqual(pick.openMailbox, try openMailbox(0x02))
        XCTAssertNil(pick.foundGrant)
    }

    func testAMailboxOfAnotherKeyIsNotCarried() throws {
        let found = FoundPerson(
            accountKey: try key(0x02), accountId: Data(repeating: 2, count: 16), displayName: "Bob",
            username: nil, writeSeed: nil, reach: .open, mailbox: try openMailbox(0x03))
        XCTAssertNil(AddMembersRules.pick(found, fallbackName: "Someone")?.openMailbox)
    }

    func testAGrantWhoseSeedIsNotTheResultsIsNotCarried() throws {
        let grant = try fofGrant(0x01)
        let found = FoundPerson(
            accountKey: try key(0x01), accountId: Data(repeating: 1, count: 16), displayName: "Ann",
            username: nil, writeSeed: Data(repeating: 9, count: 32), reach: .grant, grant: grant)
        XCTAssertNil(found.grant)
    }

    func testMergingResultsKeepsTheGrantOfTheSeedKept() throws {
        let person = try key(0x01)
        let grant = try fofGrant(0x01)
        let local = FoundPerson(
            accountKey: person, accountId: nil, displayName: "Ann", username: nil, writeSeed: nil, reach: PersonReach.none)
        let network = FoundPerson(
            accountKey: person, accountId: Data(repeating: 1, count: 16), displayName: "Ann", username: nil,
            writeSeed: grant.seed, reach: .grant, grant: grant)
        XCTAssertEqual(network.grant, grant)
        let merged = PeopleSearch.merge([[local], [network]], ownAccountKey: nil)
        XCTAssertEqual(merged.count, 1)
        XCTAssertEqual(merged.first?.reach, .grant)
        XCTAssertEqual(merged.first?.writeSeed, grant.seed)
        XCTAssertEqual(merged.first?.grant, grant)
    }

    // MARK: - A found grant is held

    /// UX section 6.1 — a search carries the found grant too, with the
    /// verified mailbox it writes to.
    func testASearchCarriesAFoundGrantWithItsMailbox() throws {
        let c = try center(0x30)
        let person = try key(0x01)
        XCTAssertTrue(c.searchGrants(forAccountKeyB64: b64(person)).isEmpty)
        XCTAssertTrue(c.keepFoundGrant(try fofGrant(0x01), accountKey: person))
        let grants = c.searchGrants(forAccountKeyB64: b64(person))
        XCTAssertEqual(grants.map(\.seed), [try fofGrant(0x01).seed])
        XCTAssertEqual(grants.first?.mailbox, try openMailbox(0x01))
    }

    func testAFoundGrantIsHeldAndSignsTheDelegations() throws {
        let c = try center(0x30)
        let person = try key(0x01)
        let committer = try key(0x31)
        XCTAssertNil(c.composeDelegations(addedAccountKey: person, candidateAccountKeys: [committer], groupIdHex: group))

        XCTAssertTrue(c.keepFoundGrant(try fofGrant(0x01), accountKey: person))
        XCTAssertEqual(c.heldSeed(forAccountKeyB64: b64(person), groupIdHex: group), try fofGrant(0x01).seed)
        let plan = try XCTUnwrap(c.writePlan(toAccountKey: person, groupIdHex: group))
        XCTAssertEqual(plan.target, try openMailbox(0x01), "the grant writes to the person's mailbox")
        XCTAssertEqual(plan.candidates.first?.reachSeed, try fofGrant(0x01).seed)
        let wire = try XCTUnwrap(c.composeDelegations(
            addedAccountKey: person, candidateAccountKeys: [committer], groupIdHex: group))
        XCTAssertEqual(try decodeCommitterDelegationEnvelope(plaintext: wire).delegations.count, 1)
    }

    func testAFoundGrantIsStoredDurably() throws {
        let persistence = InMemoryReachPersistence()
        let c = ReachCenter(persistence: persistence, ds: FailingReachDs())
        c.keyResolverProvider = { _, own in AccountKeyResolver(ownCurrentKey: own) }
        c.activate(accountIdB64: "durable", accountIdentity: try identity(0x30))
        XCTAssertTrue(c.keepFoundGrant(try fofGrant(0x01), accountKey: try key(0x01)))
        let stored = try XCTUnwrap(persistence.load(accountIdB64: "durable"))
        XCTAssertEqual(stored.held[b64(try key(0x01))]?.first?.reachClass, .fof)
    }

    func testAFoundGrantOfAnotherPersonOrClassIsRefused() throws {
        let c = try center(0x30)
        // Its binding names someone else's mailbox.
        XCTAssertFalse(c.keepFoundGrant(try fofGrant(0x01), accountKey: try key(0x02)))
        // Only a `fof` grant is read by a search without being held already.
        XCTAssertFalse(c.keepFoundGrant(try contactGrant(0x01), accountKey: try key(0x01)))
        XCTAssertNil(c.writePlan(toAccountKey: try key(0x01)))
        XCTAssertNil(c.writePlan(toAccountKey: try key(0x02)))
    }

    // MARK: - An Everyone pick: a no-delegation envelope and a local record

    func testAnEveryoneAddPostsAnEnvelopeWithNoDelegation() throws {
        let c = try center(0x30)
        let target = try openMailbox(0x02)
        let open = try XCTUnwrap(c.composeOpenEnvelope(target: target))
        let envelope = try decodeCommitterDelegationEnvelope(plaintext: open)
        XCTAssertTrue(envelope.delegations.isEmpty)
        XCTAssertEqual(envelope.addedAccountKey, try key(0x02))
        XCTAssertEqual(envelope.addedMailboxBinding, target.binding)
        XCTAssertEqual(try checkCommitterDelegations(envelope: envelope), [], "the binding checks")

        // No grant held: the add posts this one. A grant held: the signed one.
        XCTAssertEqual(CommitterDelegationChoice.envelope(signed: nil, open: open), open)
        XCTAssertEqual(CommitterDelegationChoice.envelope(signed: Data([1]), open: open), Data([1]))
        XCTAssertNil(CommitterDelegationChoice.envelope(signed: nil, open: nil))
    }

    func testTheProposersOwnRecordLetsItsOwnSweepDeliver() throws {
        let persistence = InMemoryReachPersistence()
        let c = ReachCenter(persistence: persistence, ds: FailingReachDs())
        c.keyResolverProvider = { _, own in AccountKeyResolver(ownCurrentKey: own) }
        c.activate(accountIdB64: "own-record", accountIdentity: try identity(0x30))
        let person = try key(0x02)
        XCTAssertNil(c.openWelcomeTarget(ofPerson: person, groupIdHex: group))

        c.recordOpenAdd(groupIdHex: group, target: try openMailbox(0x02))
        XCTAssertEqual(c.openWelcomeTarget(ofPerson: person, groupIdHex: group), try openMailbox(0x02))
        XCTAssertEqual(
            c.openWelcomeTarget(forMailbox: try openMailbox(0x02).address, groupIdHex: group),
            try openMailbox(0x02))
        XCTAssertNil(c.openWelcomeTarget(ofPerson: person, groupIdHex: "ffff"), "only in the group it was added to")
        XCTAssertEqual(persistence.load(accountIdB64: "own-record")?.openAdds?.count, 1, "durable")

        c.dropOpenAdd(groupIdHex: group, person: person)
        XCTAssertNil(c.openWelcomeTarget(ofPerson: person, groupIdHex: group))
        XCTAssertNil(persistence.load(accountIdB64: "own-record")?.openAdds)
    }

    func testTheAddNamesOnlyTheAddedPersonsMailbox() throws {
        let identityKey: (Data) -> Data = { $0 }
        XCTAssertEqual(
            ChatRepository.openMailboxFor(try openMailbox(0x02), addedAccountKey: try key(0x02), current: identityKey),
            try openMailbox(0x02))
        XCTAssertNil(ChatRepository.openMailboxFor(
            try openMailbox(0x03), addedAccountKey: try key(0x02), current: identityKey))
        XCTAssertNil(ChatRepository.openMailboxFor(nil, addedAccountKey: try key(0x02), current: identityKey))
    }

    // MARK: - The committer side

    func testACommitterKeepsANoDelegationEnvelopeForItsGroup() throws {
        let proposerCenter = try center(0x31)
        let wire = try XCTUnwrap(proposerCenter.composeOpenEnvelope(target: try openMailbox(0x02)))
        let envelope = try decodeCommitterDelegationEnvelope(plaintext: wire)
        let committing = try center(0x30)
        let person = try key(0x02)

        // Without the group it arrived in it names nothing.
        committing.keepDelegation(envelope, fromAccountKeyB64: b64(try key(0x31)))
        XCTAssertNil(committing.openWelcomeTarget(ofPerson: person, groupIdHex: group))

        committing.keepDelegation(envelope, fromAccountKeyB64: b64(try key(0x31)), groupIdHex: group)
        XCTAssertEqual(committing.openWelcomeTarget(ofPerson: person, groupIdHex: group), try openMailbox(0x02))
        XCTAssertNil(committing.openWelcomeTarget(ofPerson: person, groupIdHex: "ffff"))
        // It is no signed way in.
        XCTAssertNil(committing.writePlan(toAccountKey: person, groupIdHex: group))
        XCTAssertTrue(committing.writeAuthCandidates(toMailbox: try openMailbox(0x02).address).isEmpty)
    }

    func testANewerSignedEnvelopeFromTheSameProposerReplacesTheOpenOne() throws {
        let person = try key(0x01), proposer = try key(0x31)
        let committing = try center(0x30)
        let proposing = try center(0x31)
        let open = try decodeCommitterDelegationEnvelope(
            plaintext: try XCTUnwrap(proposing.composeOpenEnvelope(target: try openMailbox(0x01))))
        committing.keepDelegation(open, fromAccountKeyB64: b64(proposer), groupIdHex: group)
        XCTAssertNotNil(committing.openWelcomeTarget(ofPerson: person, groupIdHex: group))

        XCTAssertTrue(proposing.keepFoundGrant(try fofGrant(0x01), accountKey: person))
        let signed = try decodeCommitterDelegationEnvelope(plaintext: try XCTUnwrap(proposing.composeDelegations(
            addedAccountKey: person, candidateAccountKeys: [try key(0x30)], groupIdHex: group)))
        committing.keepDelegation(signed, fromAccountKeyB64: b64(proposer), groupIdHex: group)
        XCTAssertNil(committing.openWelcomeTarget(ofPerson: person, groupIdHex: group))
        XCTAssertNotNil(committing.writePlan(toAccountKey: person, groupIdHex: group))
    }

    func testANoDelegationEnvelopeWithAForeignBindingIsNotKept() throws {
        // The binding of 0x03's mailbox under 0x02's key does not verify.
        let forged = FfiCommitterDelegationEnvelope(
            addedAccountKey: try key(0x02), addedMailboxBinding: try openMailbox(0x03).binding, delegations: [])
        let committing = try center(0x30)
        committing.keepDelegation(forged, fromAccountKeyB64: b64(try key(0x31)), groupIdHex: group)
        XCTAssertNil(committing.openWelcomeTarget(ofPerson: try key(0x02), groupIdHex: group))
    }

    func testTheWelcomeGoesUnsignedOnlyWhenNothingSignedIsHeld() throws {
        let open = try openMailbox(0x02)
        let signed = MailboxWritePlan(
            target: open, candidates: [DsMailboxWriteAuth(reachSeed: Data(repeating: 7, count: 32))])

        // The signed order first, even with an open record.
        let first = SweepWelcomeAttempt.decide(plan: signed, open: open)
        XCTAssertFalse(first.openUnsigned)
        XCTAssertEqual(first.candidates, signed.candidates)

        // Nothing signed and an open record: unsigned, to that mailbox.
        let unsigned = SweepWelcomeAttempt.decide(plan: nil, open: open)
        XCTAssertTrue(unsigned.openUnsigned)
        XCTAssertEqual(unsigned.target, open)
        XCTAssertTrue(unsigned.candidates.isEmpty)
        let emptyPlan = MailboxWritePlan(target: open, candidates: [])
        XCTAssertTrue(SweepWelcomeAttempt.decide(plan: emptyPlan, open: open).openUnsigned)

        // Neither: nothing is written, the Welcome stays owed.
        let neither = SweepWelcomeAttempt.decide(plan: nil, open: nil)
        XCTAssertFalse(neither.openUnsigned)
        XCTAssertNil(neither.target)
        XCTAssertTrue(neither.candidates.isEmpty)
    }

    func testAPersonWithNeitherStaysOwed() async throws {
        var writes = 0
        do {
            _ = try await deliverSweepWelcome(candidates: [DsMailboxWriteAuth]()) { _ in
                writes += 1
                return 1
            }
            XCTFail("nothing to write with")
        } catch let error as SweepWelcomeUndeliverable {
            XCTAssertEqual(error, SweepWelcomeUndeliverable(triedKeys: 0))
        }
        XCTAssertEqual(writes, 0)
    }

    func testAnOpenWelcomeIsDeliveredOn2xxAndKeptOwedOtherwise() async throws {
        let id = try await deliverOpenSweepWelcome { 42 }
        XCTAssertEqual(id, 42)
        do {
            _ = try await deliverOpenSweepWelcome {
                throw DsClientError.http(status: 401, endpoint: "/ds/mailbox/x/messages", body: "")
            }
            XCTFail("refused")
        } catch let error as SweepWelcomeUndeliverable {
            XCTAssertEqual(error.triedKeys, 1)
        }
        do {
            _ = try await deliverOpenSweepWelcome { throw AnonTokenUnavailable.quotaExhausted(retryAfterSecs: nil) }
            XCTFail("no token")
        } catch let error as SweepWelcomeUndeliverable {
            XCTAssertEqual(error.triedKeys, 0)
        }
    }

    // MARK: - The wiring

    func testAGrantPickHandsTheGrantToTheKeeperBeforeTheAdd() async throws {
        let model = ChatViewModel(repository: ChatRepository(), groupIdHex: "0102")
        var kept: [(FfiReachGrant, Data)] = []
        model.foundGrantKeeper = { grant, key in kept.append((grant, key)) }
        let grant = try fofGrant(0x01)
        let person = try key(0x01)
        model.onAddMembers([
            AddMembersPick(personKey: person, accountId: Data(repeating: 1, count: 16), name: "Ann", foundGrant: grant),
            AddMembersPick(personKey: try key(0x02), accountId: Data(repeating: 2, count: 16), name: "Bob"),
        ])
        for _ in 0..<200 where model.membershipBusy {
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTAssertFalse(model.membershipBusy)
        XCTAssertEqual(kept.count, 1)
        XCTAssertEqual(kept.first?.0, grant)
        XCTAssertEqual(kept.first?.1, person)
    }

    func testTheAddRecordsTheOpenMailboxBeforeProposingAndNamesItAfter() throws {
        let file = URL(fileURLWithPath: "\(#filePath)").deletingLastPathComponent()
            .deletingLastPathComponent().appendingPathComponent("Lakko/Chat/ChatRepository.swift")
        let source = try String(contentsOf: file, encoding: .utf8)
        let start = try XCTUnwrap(source.range(of: "public func addMemberToGroup("))
        let rest = source[start.lowerBound...]
        let body = String(rest[..<(rest.range(of: "\n    }\n")?.lowerBound ?? rest.endIndex)])
        let record = try XCTUnwrap(body.range(of: "recordOpenAdd(groupIdHex: groupIdHex"))
        let propose = try XCTUnwrap(body.range(of: "addOneDeviceToGroup("))
        XCTAssertTrue(record.lowerBound < propose.lowerBound, "recorded before the proposal")
        XCTAssertTrue(body.contains("addedAccountKey: recipientSignaturePubkey, openMailbox: open)"))
    }
}

/// A reach DS that is never reached in these tests.
private final class FailingReachDs: ReachDsTransport {
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
