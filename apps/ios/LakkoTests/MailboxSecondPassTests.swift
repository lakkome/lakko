// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0074 section 3.5a, rules 5 and 6 and the durable re-send: what was
/// granted before a rotation is spent, a refused write re-checks the address
/// once, a kept device's profile re-send survives a restart, people search
/// accepts a proven predecessor, and a committer signs with the key a
/// delegation names.
@MainActor
final class MailboxSecondPassTests: XCTestCase {

    private var suites: [String] = []

    override func tearDown() {
        for suite in suites { UserDefaults().removePersistentDomain(forName: suite) }
        PersonAddressRecheck.reset()
        super.tearDown()
    }

    private func defaults() -> UserDefaults {
        let suite = "mailbox-second-pass-\(UUID().uuidString)"
        suites.append(suite)
        return UserDefaults(suiteName: suite)!
    }

    private func identity(_ byte: UInt8) throws -> Identity {
        try Identity.fromSeed(seed: Data(repeating: byte, count: 32))
    }

    private func key(_ byte: UInt8) throws -> Data { try identity(byte).signaturePublicKey() }

    private func b64(_ data: Data) -> String { data.base64EncodedString() }

    private func proven(_ edges: [(Data, Data)]) -> ProvenPredecessors {
        var map = ProvenPredecessors()
        for (old, new) in edges {
            XCTAssertTrue(map.record(oldKeyB64: b64(old), newKeyB64: b64(new), evidence: .verifiedProof))
        }
        return map
    }

    private func contactGrant(seedByte: UInt8, version: UInt32 = 0) throws -> FfiReachGrant {
        try deriveReachGrant(
            accountSeed: identity(seedByte).ed25519Seed(), class: .contact, scopeId: Data(), version: version)
    }

    private func held(_ grant: FfiReachGrant, at ms: UInt64, scope: String? = nil) -> ReachRules.HeldGrant {
        ReachRules.HeldGrant(grant: grant, scopeHex: scope, storedAtMs: ms)
    }

    private func center(
        ownSeedByte: UInt8 = 0x31,
        resolver: @escaping @MainActor (Data?) -> AccountKeyResolver
    ) throws -> ReachCenter {
        let c = ReachCenter(persistence: InMemoryReachPersistence(), ds: NeverRegisteringDs())
        c.keyResolverProvider = { _, own in resolver(own) }
        c.ownPredecessorSigningSeeds = { _ in [] }
        c.activate(accountIdB64: "second-pass-\(ownSeedByte)", accountIdentity: try identity(ownSeedByte))
        return c
    }

    // MARK: - C: own predecessors are a chain

    func testEveryEarlierOwnKeyResolvesToTheCurrentOneAndHasItsOwnReadWindow() throws {
        let first = try key(0x10), second = try key(0x11), current = try key(0x12)
        let resolver = AccountKeyResolver(ownCurrentKey: current, ownPredecessorKeys: [first, second])

        XCTAssertEqual(resolver.current(of: first), current)
        XCTAssertEqual(resolver.current(of: second), current)
        XCTAssertEqual(resolver.allKeys(of: first), [current, first, second])

        // The first rotation was adopted 31 days ago, the second today: only
        // the second key's mailbox is still read.
        let day: Int64 = 24 * 60 * 60 * 1000
        let now: Int64 = 100 * day
        let adoptions = PredecessorMailbox.AdoptionStore(defaults: defaults())
        adoptions.recordIfAbsent(accountIdB64: "acct", keyB64: b64(first), nowMs: now - 31 * day)
        adoptions.recordIfAbsent(accountIdB64: "acct", keyB64: b64(second), nowMs: now)
        XCTAssertEqual(
            PredecessorMailbox.readable(
                keysB64: [b64(first), b64(second)], accountIdB64: "acct", nowMs: now, adoptions: adoptions),
            [b64(second)])
    }

    // MARK: - B: the kept device's profile re-send is durable

    func testTheReSendMarkerIsClearedOnlyByARoundInWhichNothingFailed() throws {
        let record = ProfileResendOwed.marking(nil, accountKeyB64: "new")
        XCTAssertEqual(ProfileResendOwed.targets(groups: ["g2", "g1", "g3"], record: record), ["g1", "g2", "g3"])

        let afterFailure = ProfileResendOwed.after(record, attempted: ["g1", "g2", "g3"], failed: ["g2"])
        XCTAssertEqual(afterFailure, ProfileResendOwed.Record(accountKeyB64: "new", reached: ["g1", "g3"]))

        // The retry sends only to the conversation still missing.
        let still = try XCTUnwrap(afterFailure)
        XCTAssertEqual(ProfileResendOwed.targets(groups: ["g1", "g2", "g3"], record: still), ["g2"])
        XCTAssertNil(ProfileResendOwed.after(still, attempted: ["g2"], failed: []), "nothing failed: cleared")
    }

    func testAMarkerForTheSameKeyIsKeptAndOneForAnEarlierKeyIsReplaced() {
        let held = ProfileResendOwed.Record(accountKeyB64: "new", reached: ["g1"])
        XCTAssertEqual(ProfileResendOwed.marking(held, accountKeyB64: "new"), held)
        XCTAssertEqual(
            ProfileResendOwed.marking(held, accountKeyB64: "newer"),
            ProfileResendOwed.Record(accountKeyB64: "newer"),
            "a second rotation owes every conversation again")
    }

    func testTheReSendMarkerSurvivesARestartAndIsClearedByTheWipe() {
        let shared = defaults()
        ProfileResendOwed.Store(defaults: shared).put(
            ProfileResendOwed.Record(accountKeyB64: "new", reached: ["g1"]), accountIdB64: "acct")

        // Another instance over the same store: the next process.
        let reopened = ProfileResendOwed.Store(defaults: shared)
        XCTAssertEqual(
            reopened.load(accountIdB64: "acct"),
            ProfileResendOwed.Record(accountKeyB64: "new", reached: ["g1"]))
        XCTAssertNil(reopened.load(accountIdB64: "other"))

        reopened.put(nil, accountIdB64: "acct")
        XCTAssertNil(ProfileResendOwed.Store(defaults: shared).load(accountIdB64: "acct"))
    }

    // MARK: - D: a refused write re-checks the address once

    private struct Refused: Error {}
    private struct Offline: Error {}

    func testAWriteThatLandsNeverReChecks() async throws {
        var rechecks = 0
        let out = try await RefusedMailboxWrite.write(
            resolve: { "old-mailbox" },
            refused: { $0 is Refused },
            recheck: { rechecks += 1 },
            publish: { $0 })
        XCTAssertEqual(out, "old-mailbox")
        XCTAssertEqual(rechecks, 0)
    }

    func testARefusedWriteReChecksAndGoesOnceMoreToTheNewAddress() async throws {
        var address = "old-mailbox"
        var writes: [String] = []
        var rechecks = 0
        let out = try await RefusedMailboxWrite.write(
            resolve: { address },
            refused: { $0 is Refused },
            recheck: {
                rechecks += 1
                address = "new-mailbox"
            },
            publish: { attempt -> String in
                writes.append(attempt)
                if attempt == "old-mailbox" { throw Refused() }
                return attempt
            })
        XCTAssertEqual(out, "new-mailbox")
        XCTAssertEqual(writes, ["old-mailbox", "new-mailbox"])
        XCTAssertEqual(rechecks, 1)
    }

    func testAReCheckThatChangesNothingLeavesTheRefusalAndDoesNotWriteAgain() async {
        var writes = 0
        do {
            _ = try await RefusedMailboxWrite.write(
                resolve: { "old-mailbox" },
                refused: { $0 is Refused },
                recheck: {},
                publish: { _ -> String in
                    writes += 1
                    throw Refused()
                })
            XCTFail("the refusal must stand")
        } catch {
            XCTAssertTrue(error is Refused)
        }
        XCTAssertEqual(writes, 1, "an identical second write would be refused exactly as the first")
    }

    func testASecondRefusalIsNotReCheckedAgain() async {
        var address = "old-mailbox"
        var rechecks = 0
        var writes = 0
        do {
            _ = try await RefusedMailboxWrite.write(
                resolve: { address },
                refused: { $0 is Refused },
                recheck: {
                    rechecks += 1
                    address = "new-mailbox"
                },
                publish: { _ -> String in
                    writes += 1
                    throw Refused()
                })
            XCTFail("both writes were refused")
        } catch {
            XCTAssertTrue(error is Refused)
        }
        XCTAssertEqual(rechecks, 1)
        XCTAssertEqual(writes, 2, "one retry, never a loop")
    }

    func testAFailureThatIsNotARefusalIsNotReChecked() async {
        var rechecks = 0
        do {
            _ = try await RefusedMailboxWrite.write(
                resolve: { "old-mailbox" },
                refused: { $0 is Refused },
                recheck: { rechecks += 1 },
                publish: { _ -> String in throw Offline() })
            XCTFail("offline is a failure")
        } catch {
            XCTAssertTrue(error is Offline)
        }
        XCTAssertEqual(rechecks, 0)
    }

    func testARefusalIsRecognisedThroughTheContactAndChatWrappers() {
        let refusal = DsClientError.http(status: 401, endpoint: "/ds/mailbox", body: "")
        XCTAssertTrue(RefusedMailboxWrite.isRefusal(refusal))
        XCTAssertTrue(RefusedMailboxWrite.isRefusal(ContactsError.wrapped(refusal)))
        XCTAssertTrue(RefusedMailboxWrite.isRefusal(ContactsError.requestRefused))
        XCTAssertTrue(RefusedMailboxWrite.isRefusal(ChatError.wrapped("Welcome", underlying: refusal)))
        XCTAssertFalse(RefusedMailboxWrite.isRefusal(
            DsClientError.http(status: 503, endpoint: "/ds/mailbox", body: "")))
        XCTAssertFalse(RefusedMailboxWrite.isRefusal(Offline()))
    }

    func testOnlyAMovedAddressOrAMovedGrantEarnsTheRetry() {
        let seed = Data(repeating: 1, count: 32), other = Data(repeating: 2, count: 32)
        let first = MailboxWriteAttempt(mailbox: Data([1]), auth: DsMailboxWriteAuth(reachSeed: seed))
        XCTAssertFalse(RefusedMailboxWrite.retries(first: first, rechecked: first))
        XCTAssertFalse(RefusedMailboxWrite.retries(first: first, rechecked: nil))
        XCTAssertTrue(RefusedMailboxWrite.retries(
            first: first,
            rechecked: MailboxWriteAttempt(mailbox: Data([2]), auth: DsMailboxWriteAuth(reachSeed: seed))))
        XCTAssertTrue(RefusedMailboxWrite.retries(
            first: first,
            rechecked: MailboxWriteAttempt(mailbox: Data([1]), auth: DsMailboxWriteAuth(reachSeed: other))))
    }

    func testAPersonIsReCheckedAtMostOncePerInterval() {
        XCTAssertTrue(PersonAddressRecheck.due(lastMs: nil, nowMs: 1_000))
        XCTAssertFalse(PersonAddressRecheck.due(lastMs: 1_000, nowMs: 1_000 + PersonAddressRecheck.minIntervalMs - 1))
        XCTAssertTrue(PersonAddressRecheck.due(lastMs: 1_000, nowMs: 1_000 + PersonAddressRecheck.minIntervalMs))
        XCTAssertTrue(PersonAddressRecheck.due(lastMs: 5_000, nowMs: 1_000), "a clock that stepped back re-checks")
    }

    func testTheAccountIdOfAPersonComesFromAContactRowThenFromAReceivedProfile() throws {
        let old = try key(0x01), new = try key(0x02)
        let fromContact = Data(repeating: 0xC1, count: 16), fromProfile = Data(repeating: 0xD1, count: 16)

        XCTAssertEqual(
            PersonAddressRecheck.accountId(
                ofPersonWithKeys: [new, old],
                contacts: [(old, fromContact)],
                profileAccountId: { _ in fromProfile }),
            fromContact, "the contact row wins, under any key of the person")
        XCTAssertEqual(
            PersonAddressRecheck.accountId(
                ofPersonWithKeys: [new, old],
                contacts: [(old, nil)],
                profileAccountId: { $0 == old ? fromProfile : nil }),
            fromProfile)
        XCTAssertNil(PersonAddressRecheck.accountId(
            ofPersonWithKeys: [new], contacts: [], profileAccountId: { _ in Data([1, 2, 3]) }),
            "an id of the wrong length names nobody")
    }

    // A grant found under a predecessor key is kept beside the newer one.

    func testAGrantDisplacedByARotationIsKeptBesideTheNewOneAndTriedAfterIt() throws {
        let old = try contactGrant(seedByte: 0x01), new = try contactGrant(seedByte: 0x02)
        let merged = ReachRules.merging(held(new, at: 20), into: [held(old, at: 10)])

        XCTAssertEqual(merged.count, 2, "same class, scope and version under another seed: two mailboxes")
        XCTAssertEqual(merged.filter(\.isSuperseded).compactMap(\.seed), [old.seed])
        XCTAssertEqual(ReachRules.writeSeeds(from: merged, groupIdHex: nil), [new.seed, old.seed])
        XCTAssertEqual(ReachRules.writeSeed(from: merged, groupIdHex: nil), new.seed)
        XCTAssertEqual(ReachRules.droppingSuperseded(merged).compactMap(\.seed), [new.seed])
    }

    func testAVersionBumpStillReplacesAndAReplayChangesNothing() throws {
        let v0 = try contactGrant(seedByte: 0x01, version: 0), v1 = try contactGrant(seedByte: 0x01, version: 1)

        // The owner rotated a reach key: the old version opens nothing.
        XCTAssertEqual(ReachRules.merging(held(v1, at: 20), into: [held(v0, at: 10)]).compactMap(\.seed), [v1.seed])
        // A replayed older version never displaces a newer one.
        XCTAssertEqual(ReachRules.merging(held(v0, at: 30), into: [held(v1, at: 20)]).compactMap(\.seed), [v1.seed])
        // The same grant again moves nothing, and never revives a displaced one.
        let other = try contactGrant(seedByte: 0x02)
        let beside = ReachRules.merging(held(other, at: 20), into: [held(v0, at: 10)])
        XCTAssertEqual(ReachRules.merging(held(v0, at: 40), into: beside), beside)
        XCTAssertEqual(ReachRules.merging(held(other, at: 40), into: beside), beside)
    }

    func testOnlyOneDisplacedGrantIsKeptPerClassAndScope() throws {
        let a = try contactGrant(seedByte: 0x01), b = try contactGrant(seedByte: 0x02)
        let c = try contactGrant(seedByte: 0x03)
        var merged = ReachRules.merging(held(b, at: 20), into: [held(a, at: 10)])
        merged = ReachRules.merging(held(c, at: 30), into: merged)
        XCTAssertEqual(merged.filter(\.isSuperseded).compactMap(\.seed), [b.seed])
        XCTAssertEqual(merged.filter { !$0.isSuperseded }.compactMap(\.seed), [c.seed])
    }

    func testAPeerThatGotTheReSentProfileFirstCanStillWriteToTheOldMailbox() throws {
        let old = try key(0x01), new = try key(0x02)
        var map = ProvenPredecessors()
        let c = try center { own in AccountKeyResolver(proven: map, ownCurrentKey: own) }
        let before = try contactGrant(seedByte: 0x01), after = try contactGrant(seedByte: 0x02)
        let carrier = ReachRules.Carrier.oneToOneProfile(fromOtherParticipant: true)

        // Both arrive from the leaf bound to the OLD key; the proof is unseen.
        XCTAssertTrue(c.keep(before, carrier: carrier, fromAccountKeyB64: b64(old), scopeHex: "aa"))
        XCTAssertTrue(c.keep(after, carrier: carrier, fromAccountKeyB64: b64(old), scopeHex: "aa"))

        let oldBox = try XCTUnwrap(MailboxAddress.pseudonym(of: old))
        XCTAssertEqual(c.writeAuth(toAccountKey: old)?.reachSeed, after.seed, "the newest goes first")
        XCTAssertEqual(
            c.writeAuthCandidates(toMailbox: oldBox).map(\.reachSeed), [after.seed, before.seed],
            "the displaced grant still opens the old mailbox when the new one is refused there")
        XCTAssertEqual(c.accountKey(forMailbox: oldBox), old)

        // The re-check proves the rotation: the address is current, and only
        // the grant for the new mailbox is offered.
        map = proven([(old, new)])
        c.reloadKeyResolver()
        let newBox = try XCTUnwrap(MailboxAddress.pseudonym(of: new))
        XCTAssertEqual(c.currentMailbox(for: oldBox), newBox)
        XCTAssertEqual(c.writeAuthCandidates(toMailbox: newBox).map(\.reachSeed), [after.seed])
        XCTAssertEqual(c.accountKey(forMailbox: oldBox), new, "found under any key they have had")
        XCTAssertEqual(c.accountKey(forMailbox: newBox), new)
    }

    func testAMailboxNothingHereNamesHasNobodyToReCheck() throws {
        let c = try center { own in AccountKeyResolver(ownCurrentKey: own) }
        XCTAssertNil(c.accountKey(forMailbox: try XCTUnwrap(MailboxAddress.pseudonym(of: try key(0x44)))))
        let ownBox = try XCTUnwrap(MailboxAddress.pseudonym(of: try key(0x31)))
        XCTAssertNil(c.accountKey(forMailbox: ownBox), "this account's own mailbox is never re-checked")
    }

    // MARK: - E: people search accepts a proven predecessor

    func testAHitUnderAProvenPredecessorKeyIsTheSamePersonAndAnUnprovenMismatchIsNot() throws {
        let old = try key(0x01), new = try key(0x02), unrelated = try key(0x03)
        let resolver = AccountKeyResolver(proven: proven([(old, new)]))

        XCTAssertTrue(PeopleSearch.acceptsPublishedKey(
            found: new, published: new, currentOfFound: resolver.current(of: new)))
        XCTAssertTrue(PeopleSearch.acceptsPublishedKey(
            found: old, published: new, currentOfFound: resolver.current(of: old)))
        XCTAssertFalse(PeopleSearch.acceptsPublishedKey(
            found: unrelated, published: new, currentOfFound: resolver.current(of: unrelated)),
            "an unproven mismatch is refused")
        XCTAssertFalse(PeopleSearch.acceptsPublishedKey(
            found: new, published: old, currentOfFound: resolver.current(of: new)),
            "a published key the hit's key superseded is not the current person")
    }

    // MARK: - F: what was granted before the rotation is spent

    func testARotationSpendsEveryLiveLinkAndReplyScopeOnce() {
        let link = LinkReach(linkIdB64: "bGluaw==", version: 0, expiresAtMs: 9, blobIdB64: "YmxvYg==")
        let reply = ReplyReach(scopeIdB64: "c2NvcGU=", expiresAtMs: 9, purpose: "contact:x")
        let spent = ReachRules.spending(links: [link], replies: [reply], spentLinks: [link], spentReplies: [])
        XCTAssertEqual(spent.links, [link], "a link spent by an earlier rotation is not listed twice")
        XCTAssertEqual(spent.replies, [reply])
    }

    func testTheLinksToRevokeAreTheSpentOnesAtTheirLedgerThenTheListedInvites() {
        let invite = Data([1, 1, 1, 1, 1, 1]), call = Data([2, 2, 2, 2, 2, 2]), elsewhere = Data([3, 3, 3, 3, 3, 3])
        let spent = [
            LinkReach(linkIdB64: "aQ==", version: 0, expiresAtMs: 9, blobIdB64: invite.base64EncodedString()),
            LinkReach(
                linkIdB64: "Yw==", version: 0, expiresAtMs: 9, blobIdB64: call.base64EncodedString(),
                kind: LinkReach.callKind),
            LinkReach(linkIdB64: "aW5saW5l", version: 0, expiresAtMs: 9),
        ]
        XCTAssertEqual(
            SpentGrants.linksToRevoke(spent: spent, listedInviteBlobIds: [invite, elsewhere]),
            [
                SpentGrants.LinkToRevoke(blobId: invite, call: false, linkIdB64: "aQ=="),
                SpentGrants.LinkToRevoke(blobId: call, call: true, linkIdB64: "Yw=="),
                SpentGrants.LinkToRevoke(blobId: elsewhere, call: false, linkIdB64: nil),
            ],
            "an inline link has no ledger row; a row another device minted is revoked too")
    }

    func testALinkLeavesTheListsOnlyWhenItIsGoneOrWasMintedHere() {
        XCTAssertEqual(SpentGrants.linkVerdict([.notFoundOrNotOwner, .revoked]), .gone)
        XCTAssertEqual(SpentGrants.linkVerdict([.notFoundOrNotOwner, .notFoundOrNotOwner]), .notOurs)
        XCTAssertEqual(SpentGrants.linkVerdict([.notFoundOrNotOwner, .failed]), .owed)
        XCTAssertEqual(SpentGrants.linkVerdict([]), .owed, "no old key at hand is no verdict")

        XCTAssertTrue(SpentGrants.dropsLocally(.gone, mintedHere: false))
        XCTAssertTrue(SpentGrants.dropsLocally(.notOurs, mintedHere: true), "expired or revoked before")
        XCTAssertFalse(
            SpentGrants.dropsLocally(.notOurs, mintedHere: false),
            "a row only the owner list named and no old key issued belongs to the new key")
        XCTAssertFalse(SpentGrants.dropsLocally(.owed, mintedHere: true))
    }

    func testTheRequestsToSendAgainAreTheUnansweredContactRequestsOnceEach() {
        let ada = Data(repeating: 0xA1, count: 16), bob = Data(repeating: 0xB1, count: 16)
        let replies = [
            ReplyReach(scopeIdB64: "MQ==", expiresAtMs: 9, purpose: ReplyPurpose.contact(ada)),
            ReplyReach(scopeIdB64: "Mg==", expiresAtMs: 9, purpose: ReplyPurpose.contact(ada)),
            ReplyReach(scopeIdB64: "Mw==", expiresAtMs: 9, purpose: ReplyPurpose.contact(bob)),
            ReplyReach(scopeIdB64: "NA==", expiresAtMs: 9, purpose: ReplyPurpose.groupJoin(Data([7]))),
        ]
        XCTAssertEqual(SpentGrants.requestsToResend(spentReplies: replies, acceptedAccountIds: []), [ada, bob])
        XCTAssertEqual(
            SpentGrants.requestsToResend(spentReplies: replies, acceptedAccountIds: [bob]), [ada],
            "a person who is a contact already was answered")
        XCTAssertEqual(ReplyPurpose.contactAccountId(of: ReplyPurpose.contact(ada)), ada)
        XCTAssertNil(ReplyPurpose.contactAccountId(of: ReplyPurpose.groupJoin(ada)))
        XCTAssertNil(ReplyPurpose.contactAccountId(of: "contact:not-base64"))
    }

    func testAReSentRequestReplacesTheStoredEntryAndADeadOneIsDropped() {
        let ada = Data(repeating: 0xA1, count: 16).base64EncodedString()
        let bob = Data(repeating: 0xB1, count: 16).base64EncodedString()
        let sent = [
            SentContactRequest(recipientUsername: "ada", recipientAccountIdB64: ada, messageId: 1, createdAt: "t1"),
            SentContactRequest(recipientUsername: "bob", recipientAccountIdB64: bob, messageId: 2, createdAt: "t2"),
        ]
        let fresh = SentContactRequest(recipientUsername: "", recipientAccountIdB64: ada, messageId: 9, createdAt: "t9")

        XCTAssertEqual(
            SpentGrants.sentList(sent, recipientAccountIdB64: ada, verdict: .sent, replacement: fresh),
            [
                sent[1],
                SentContactRequest(recipientUsername: "ada", recipientAccountIdB64: ada, messageId: 9, createdAt: "t9"),
            ],
            "one entry per person, under the new request's id, the name kept")
        XCTAssertEqual(
            SpentGrants.sentList(sent, recipientAccountIdB64: ada, verdict: .dead, replacement: nil), [sent[1]])
        XCTAssertEqual(
            SpentGrants.sentList(sent, recipientAccountIdB64: ada, verdict: .owed, replacement: nil), sent)

        XCTAssertEqual(SpentGrants.resendVerdict(nil), .sent)
        XCTAssertEqual(SpentGrants.resendVerdict(ContactsError.requestRefused), .dead)
        XCTAssertEqual(SpentGrants.resendVerdict(ContactsError.noIdentityBundle(Data())), .dead)
        XCTAssertEqual(
            SpentGrants.resendVerdict(ContactsError.wrapped(
                SupersessionRefused(reason: .proofAbsent, accountIdB64: ada, message: "changed"))),
            .dead, "the recipient's keys were reset: nothing is sealed to an unproven key")
        XCTAssertEqual(SpentGrants.resendVerdict(ContactsError.wrapped(Offline())), .owed)
    }

    func testTheRotationOwesTheSpentGrantsStepAfterTheDirectoryAndTheReachKeys() throws {
        var record = KeyRotationRecord(targetDeviceIdB64: "ZGV2", startedAtMs: 1)
        record.phase = .committed
        XCTAssertTrue(KeyRotationPlan.remaining(record).contains(.grants))
        let steps = KeyRotationPlan.remaining(record)
        XCTAssertLessThan(
            try XCTUnwrap(steps.firstIndex(of: .reach)), try XCTUnwrap(steps.firstIndex(of: .grants)))
        XCTAssertFalse(KeyRotationPlan.stepsWithholdingWords.contains(.grants), "the words do not wait for it")

        record = KeyRotationPlan.marking(record, done: .grants)
        XCTAssertEqual(record.grantsDone, true)
        XCTAssertFalse(KeyRotationPlan.remaining(record).contains(.grants))

        // A record written before the step existed owes it.
        var json = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(record)) as? [String: Any])
        json["grants_done"] = nil
        let old = try JSONDecoder().decode(
            KeyRotationRecord.self, from: JSONSerialization.data(withJSONObject: json))
        XCTAssertTrue(KeyRotationPlan.remaining(old).contains(.grants))
    }

    func testTheRotateSheetSaysThatLinksStopWorking() {
        XCTAssertEqual(RotateDeviceCopy.effectLinks, "devices.rotate.effect_links")
        XCTAssertEqual(RotateDeviceCopy.effects.last, RotateDeviceCopy.effectLinks)
        XCTAssertTrue(RotateDeviceCopy.all.contains(RotateDeviceCopy.effectLinks))
        XCTAssertNotEqual(
            Localization.text(RotateDeviceCopy.effectLinks), RotateDeviceCopy.effectLinks,
            "the compiled catalog holds the line")
    }

    // Incoming: one request per person.

    private func request(_ messageId: Int64, from key: Data, account: UInt8) -> IncomingContactRequest {
        IncomingContactRequest(
            messageId: messageId, fromPubkey: key, fromAccountId: Data(repeating: account, count: 16),
            fromPseudonym: Data(repeating: 0xEE, count: 32), validAfterMs: 1, intro: nil,
            createdAt: "2026-10-11T10:00:00Z")
    }

    func testARequestFromTheProvenSuccessorReplacesThePendingOneSilently() throws {
        let old = try key(0x01), new = try key(0x02), other = try key(0x03)
        let resolver = AccountKeyResolver(proven: proven([(old, new)]))
        let pending = [request(5, from: old, account: 0xA1), request(6, from: other, account: 0xB1)]

        let merged = ContactsInboxMerge.merge(
            fetched: [request(9, from: new, account: 0xA1)], pending: pending, accepted: [],
            currentKey: resolver.current(of:))

        XCTAssertEqual(merged.replaced, [5])
        XCTAssertEqual(merged.kept.map(\.messageId), [9])
        XCTAssertEqual(merged.applied(to: pending).map(\.messageId), [6, 9], "one row per person, no second row")
    }

    func testWithoutAProofTwoKeysAreTwoPeople() throws {
        let old = try key(0x01), new = try key(0x02)
        let pending = [request(5, from: old, account: 0xA1)]
        let merged = ContactsInboxMerge.merge(
            fetched: [request(9, from: new, account: 0xA1)], pending: pending, accepted: [])
        XCTAssertEqual(merged.replaced, [])
        XCTAssertEqual(merged.applied(to: pending).map(\.messageId), [5, 9])
    }

    func testARequestFromAKeyThePersonLeftDoesNotAddASecondRow() throws {
        let old = try key(0x01), new = try key(0x02)
        let resolver = AccountKeyResolver(proven: proven([(old, new)]))
        let pending = [request(9, from: new, account: 0xA1)]

        let late = ContactsInboxMerge.merge(
            fetched: [request(5, from: old, account: 0xA1)], pending: pending, accepted: [],
            currentKey: resolver.current(of:))
        XCTAssertEqual(late.kept, [])
        XCTAssertEqual(late.dropped.map(\.reason), [.alreadyPending])

        // Both in one batch, the old one first.
        let batch = ContactsInboxMerge.merge(
            fetched: [request(5, from: old, account: 0xA1), request(9, from: new, account: 0xA1)],
            pending: [], accepted: [], currentKey: resolver.current(of:))
        XCTAssertEqual(batch.kept.map(\.messageId), [9])
    }

    func testAContactIsRecognisedUnderTheKeyTheyRotatedTo() throws {
        let old = try key(0x01), new = try key(0x02)
        let resolver = AccountKeyResolver(proven: proven([(old, new)]))
        let contact = AcceptedContact(request: request(1, from: old, account: 0xA1))
        let merged = ContactsInboxMerge.merge(
            fetched: [request(9, from: new, account: 0xA1)], pending: [], accepted: [contact],
            currentKey: resolver.current(of:))
        XCTAssertEqual(merged.kept, [])
        XCTAssertEqual(merged.dropped.map(\.reason), [.alreadyAccepted])
    }

    func testOnlyASharedAccountIdUnderTwoKeysIsWorthAskingTheDirectoryAbout() throws {
        let old = try key(0x01), new = try key(0x02), other = try key(0x03)
        let pending = [request(5, from: old, account: 0xA1), request(6, from: other, account: 0xB1)]
        XCTAssertEqual(
            ContactsInboxMerge.rotationCandidates(
                fetched: [request(9, from: new, account: 0xA1), request(10, from: other, account: 0xB1)],
                pending: pending),
            [ContactsInboxMerge.RotationCandidate(
                accountId: Data(repeating: 0xA1, count: 16), oldKey: old, newKey: new)])
    }

    // MARK: - G: the committer signs with the key the delegation names

    func testTheDelegationSignerIsTheFirstOwnKeyTheEnvelopeNames() {
        let current = Data([1]), old = Data([2]), older = Data([3])
        let ownKeys = [(key: current, seed: "current-seed"), (key: old, seed: "old-seed"), (key: older, seed: "older-seed")]

        let namesOld = ReachRules.delegationSigner(ownKeys: ownKeys) { $0 == old ? "header-old" : nil }
        XCTAssertEqual(namesOld?.seed, "old-seed")
        XCTAssertEqual(namesOld?.header, "header-old")

        let namesBoth = ReachRules.delegationSigner(ownKeys: ownKeys) { $0 == older ? nil : "h" }
        XCTAssertEqual(namesBoth?.seed, "current-seed", "the current key first")

        XCTAssertNil(ReachRules.delegationSigner(ownKeys: ownKeys) { _ in nil })
    }

    func testACommitterFindsADelegationNamingItsOldKeyAndSignsWithThatKey() throws {
        let added = try key(0x01), proposer = try key(0x07)
        let committerOld = try identity(0x05), committerNew = try identity(0x06)

        // The proposer has not observed the committer's rotation: it names
        // the key the role table carries.
        let proposing = try center(ownSeedByte: 0x07) { own in AccountKeyResolver(ownCurrentKey: own) }
        XCTAssertTrue(proposing.keep(
            try contactGrant(seedByte: 0x01), carrier: .oneToOneProfile(fromOtherParticipant: true),
            fromAccountKeyB64: b64(added), scopeHex: "aa"))
        let wire = try XCTUnwrap(proposing.composeDelegations(
            addedAccountKey: added, candidateAccountKeys: [committerOld.signaturePublicKey()],
            groupIdHex: "aa"))
        let envelope = try decodeCommitterDelegationEnvelope(plaintext: wire)

        let committing = try center(ownSeedByte: 0x06) { own in
            AccountKeyResolver(ownCurrentKey: own, ownPredecessorKeys: [committerOld.signaturePublicKey()])
        }
        XCTAssertEqual(committing.ownAccountKey, committerNew.signaturePublicKey())
        let box = try XCTUnwrap(MailboxAddress.pseudonym(of: added))

        // Without the old key's seed nothing can be signed.
        committing.keepDelegation(envelope, fromAccountKeyB64: b64(proposer))
        XCTAssertTrue(committing.writeAuthCandidates(toMailbox: box).isEmpty)

        committing.ownPredecessorSigningSeeds = { _ in
            [(committerOld.signaturePublicKey(), committerOld.ed25519Seed())]
        }
        let auth = try XCTUnwrap(committing.writeAuthCandidates(toMailbox: box).first)
        XCTAssertEqual(auth.reachSeed, committerOld.ed25519Seed(), "signed with the key the delegation names")
        XCTAssertEqual(
            auth.delegationHeader,
            try committerDelegationFor(envelope: envelope, ownAccountKey: committerOld.signaturePublicKey()))
        XCTAssertEqual(committing.accountKey(forMailbox: box), added)
    }
}

/// A DS transport for tests that never register: any call is a failure.
private final class NeverRegisteringDs: ReachDsTransport {
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
