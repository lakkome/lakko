// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0066 section 3.5a — the mailbox after a key rotation: senders resolve
/// before they address, grants are kept for the person, the profile is
/// re-sent, and a predecessor mailbox is read for 30 days and then left.
@MainActor
final class MailboxAfterRotationTests: XCTestCase {

    private func key(_ byte: UInt8) throws -> Data {
        try Identity.fromSeed(seed: Data(repeating: byte, count: 32)).signaturePublicKey()
    }

    private func b64(_ data: Data) -> String { data.base64EncodedString() }

    private func proven(_ edges: [(Data, Data)]) -> ProvenPredecessors {
        var map = ProvenPredecessors()
        for (old, new) in edges {
            XCTAssertTrue(map.record(oldKeyB64: b64(old), newKeyB64: b64(new), evidence: .verifiedProof))
        }
        return map
    }

    // MARK: - Rule 1: resolve before addressing

    func testAProvenPredecessorResolvesToTheCurrentKeyAndItsMailbox() throws {
        let old = try key(0x01), new = try key(0x02), stranger = try key(0x03)
        let resolver = AccountKeyResolver(proven: proven([(old, new)]))

        XCTAssertEqual(resolver.current(of: old), new)
        XCTAssertEqual(resolver.current(of: new), new)
        XCTAssertEqual(resolver.current(of: stranger), stranger, "a key nobody superseded is itself")
        XCTAssertEqual(
            MailboxAddress.pseudonym(forAccountKey: old, resolver: resolver),
            MailboxAddress.pseudonym(of: new))
        XCTAssertNotEqual(MailboxAddress.pseudonym(of: old), MailboxAddress.pseudonym(of: new))
    }

    func testAnUnprovenChangeIsNeverResolved() throws {
        let old = try key(0x01), new = try key(0x02)
        var map = ProvenPredecessors()
        XCTAssertFalse(map.record(oldKeyB64: b64(old), newKeyB64: b64(new), evidence: .attestationOnly))
        XCTAssertFalse(map.record(oldKeyB64: b64(old), newKeyB64: b64(new), evidence: .none))
        let resolver = AccountKeyResolver(proven: map)

        XCTAssertEqual(resolver.current(of: old), old, "a reset's old key still addresses its own mailbox")
        XCTAssertFalse(resolver.samePerson(old, new))
        XCTAssertEqual(
            MailboxAddress.pseudonym(forAccountKey: old, resolver: resolver),
            MailboxAddress.pseudonym(of: old))
    }

    func testAChainOfProvenRotationsResolvesToItsEnd() throws {
        let a = try key(0x01), b = try key(0x02), c = try key(0x03)
        let resolver = AccountKeyResolver(proven: proven([(a, b), (b, c)]))

        XCTAssertEqual(resolver.current(of: a), c)
        XCTAssertEqual(resolver.current(of: b), c)
        XCTAssertEqual(resolver.allKeys(of: a), [c, b, a], "the current key first, then nearest predecessor")
        XCTAssertTrue(resolver.samePerson(a, c))
    }

    func testALaterResetDropsThePredecessorsSoNothingResolvesAnyMore() throws {
        let a = try key(0x01), b = try key(0x02)
        var map = proven([(a, b)])
        XCTAssertTrue(map.forgetPerson(currentKeyB64: b64(b)))
        XCTAssertEqual(AccountKeyResolver(proven: map).current(of: a), a)
    }

    func testTheAccountsOwnOldKeyResolvesToItsCurrentKey() throws {
        let oldOwn = try key(0x10), olderOwn = try key(0x11), own = try key(0x12), other = try key(0x13)
        let resolver = AccountKeyResolver(ownCurrentKey: own, ownPredecessorKeys: [olderOwn, oldOwn])

        XCTAssertEqual(resolver.current(of: oldOwn), own)
        XCTAssertEqual(resolver.current(of: olderOwn), own)
        XCTAssertTrue(resolver.isOwn(oldOwn))
        XCTAssertTrue(resolver.isOwn(own))
        XCTAssertFalse(resolver.isOwn(other))
        XCTAssertEqual(resolver.allKeys(of: oldOwn).first, own)
        // With no account bound nothing is "own".
        XCTAssertEqual(
            AccountKeyResolver(ownCurrentKey: nil, ownPredecessorKeys: [oldOwn]).current(of: oldOwn), oldOwn)
    }

    func testAMailboxRecordedBeforeTheRotationWasSeenIsReAddressed() throws {
        let old = try key(0x01), new = try key(0x02), oldOwn = try key(0x10), own = try key(0x12)
        let resolver = AccountKeyResolver(
            proven: proven([(old, new)]), ownCurrentKey: own, ownPredecessorKeys: [oldOwn])
        let oldBox = try XCTUnwrap(MailboxAddress.pseudonym(of: old))
        let newBox = try XCTUnwrap(MailboxAddress.pseudonym(of: new))
        let unrelated = try XCTUnwrap(MailboxAddress.pseudonym(of: try key(0x44)))

        XCTAssertEqual(MailboxAddress.current(of: oldBox, resolver: resolver), newBox)
        XCTAssertEqual(MailboxAddress.current(of: newBox, resolver: resolver), newBox)
        XCTAssertEqual(MailboxAddress.current(of: unrelated, resolver: resolver), unrelated)
        XCTAssertEqual(
            MailboxAddress.current(of: try XCTUnwrap(MailboxAddress.pseudonym(of: oldOwn)), resolver: resolver),
            MailboxAddress.pseudonym(of: own))
    }

    // MARK: - Rule 2: grants are kept for the person

    private struct Party {
        let center: ReachCenter
        let ownSeed: Data
        let ownKey: Data
    }

    private func center(resolver: @escaping @MainActor (Data?) -> AccountKeyResolver) throws -> Party {
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x31, count: 32))
        let c = ReachCenter(persistence: InMemoryReachPersistence(), ds: UnusedReachDs())
        c.keyResolverProvider = { _, own in resolver(own) }
        c.activate(accountIdB64: "mailbox-after-rotation", accountIdentity: identity)
        return Party(center: c, ownSeed: identity.ed25519Seed(), ownKey: identity.signaturePublicKey())
    }

    private func contactGrant(seedByte: UInt8) throws -> FfiReachGrant {
        try deriveReachGrant(
            accountSeed: Identity.fromSeed(seed: Data(repeating: seedByte, count: 32)).ed25519Seed(),
            class: .contact, scopeId: Data(), version: 0)
    }

    func testAGrantFromALeafBoundToAPredecessorKeyIsFiledUnderTheCurrentKey() throws {
        let old = try key(0x01), new = try key(0x02)
        let map = proven([(old, new)])
        let party = try center { own in AccountKeyResolver(proven: map, ownCurrentKey: own) }
        let grant = try contactGrant(seedByte: 0x02)

        // The leaf's binding still names the OLD key (Stage A).
        XCTAssertTrue(party.center.keep(
            grant, carrier: .oneToOneProfile(fromOtherParticipant: true),
            fromAccountKeyB64: b64(old), scopeHex: "aa"))

        XCTAssertEqual(party.center.heldAccountKeys(), [b64(new)], "kept for the person, not the key it came under")
        XCTAssertEqual(party.center.writeAuth(toAccountKey: new)?.reachSeed, grant.seed)
        XCTAssertEqual(party.center.writeAuth(toAccountKey: old)?.reachSeed, grant.seed)
        let newBox = try XCTUnwrap(MailboxAddress.pseudonym(of: new))
        XCTAssertEqual(party.center.writeAuth(toMailbox: newBox)?.reachSeed, grant.seed)
        XCTAssertEqual(party.center.mailbox(forAccountKey: old), newBox)
    }

    func testAGrantKeptBeforeTheRotationWasSeenIsStillFoundAfterIt() throws {
        let old = try key(0x01), new = try key(0x02)
        var map = ProvenPredecessors()
        let party = try center { own in AccountKeyResolver(proven: map, ownCurrentKey: own) }
        let before = try contactGrant(seedByte: 0x01)

        XCTAssertTrue(party.center.keep(
            before, carrier: .oneToOneProfile(fromOtherParticipant: true),
            fromAccountKeyB64: b64(old), scopeHex: "aa"))
        XCTAssertNil(party.center.writeAuth(toAccountKey: new), "nothing ties the two keys yet")
        let oldBox = try XCTUnwrap(MailboxAddress.pseudonym(of: old))
        XCTAssertEqual(party.center.currentMailbox(for: oldBox), oldBox)

        // The gate records the proven rotation.
        var kept: [String] = []
        party.center.onGrantKept = { kept.append($0) }
        map = proven([(old, new)])
        party.center.reloadKeyResolver()

        XCTAssertEqual(kept, [b64(new)], "a Welcome owed for this person is retried")
        XCTAssertEqual(party.center.writeAuth(toAccountKey: new)?.reachSeed, before.seed)
        let newBox = try XCTUnwrap(MailboxAddress.pseudonym(of: new))
        XCTAssertEqual(party.center.currentMailbox(for: oldBox), newBox)
        XCTAssertEqual(
            party.center.writeAuthCandidates(toMailbox: newBox).map(\.reachSeed), [before.seed])
        XCTAssertTrue(
            party.center.writeAuthCandidates(toMailbox: oldBox).isEmpty,
            "the old mailbox is never the address of a write again")

        // The re-sent profile's grant (same class, scope and version, derived
        // from the new seed) replaces it for the person.
        let after = try contactGrant(seedByte: 0x02)
        XCTAssertTrue(party.center.keep(
            after, carrier: .oneToOneProfile(fromOtherParticipant: true),
            fromAccountKeyB64: b64(old), scopeHex: "aa"))
        XCTAssertEqual(party.center.writeAuth(toAccountKey: old)?.reachSeed, after.seed)
        XCTAssertEqual(party.center.heldAccountKeys(), [b64(new)])

        party.center.dropHeld(accountKeyB64: b64(new))
        XCTAssertNil(party.center.writeAuth(toAccountKey: old), "dropping the person drops every key's grants")
    }

    func testAnUnprovenNewKeyDoesNotInheritTheOldKeysGrants() throws {
        let old = try key(0x01), reset = try key(0x02)
        let party = try center { own in AccountKeyResolver(ownCurrentKey: own) }
        XCTAssertTrue(party.center.keep(
            try contactGrant(seedByte: 0x01), carrier: .oneToOneProfile(fromOtherParticipant: true),
            fromAccountKeyB64: b64(old), scopeHex: "aa"))

        XCTAssertNil(party.center.writeAuth(toAccountKey: reset))
        XCTAssertEqual(party.center.mailbox(forAccountKey: old), MailboxAddress.pseudonym(of: old))
    }

    func testAWriteToTheAccountsOwnOldKeyIsSignedForItsCurrentMailbox() throws {
        let oldOwn = try key(0x10)
        let party = try center { own in
            AccountKeyResolver(ownCurrentKey: own, ownPredecessorKeys: [oldOwn])
        }
        let ownSeed = try XCTUnwrap(party.center.ownContactSeed())

        // "Another device of my own account": its leaf is bound to the old key.
        XCTAssertEqual(party.center.writeAuth(toAccountKey: oldOwn)?.reachSeed, ownSeed)
        XCTAssertEqual(party.center.mailbox(forAccountKey: oldOwn), MailboxAddress.pseudonym(of: party.ownKey))
        let oldOwnBox = try XCTUnwrap(MailboxAddress.pseudonym(of: oldOwn))
        let target = party.center.currentMailbox(for: oldOwnBox)
        XCTAssertEqual(target, MailboxAddress.pseudonym(of: party.ownKey))
        XCTAssertEqual(party.center.writeAuthCandidates(toMailbox: target).map(\.reachSeed), [ownSeed])
    }

    func testACommitterDelegationNamesTheCurrentKeysOfTheAddedPersonAndTheCommitter() throws {
        let old = try key(0x01), new = try key(0x02)
        let committerOld = try key(0x05), committerNew = try key(0x06)
        let map = proven([(old, new), (committerOld, committerNew)])
        let party = try center { own in AccountKeyResolver(proven: map, ownCurrentKey: own) }
        XCTAssertTrue(party.center.keep(
            try contactGrant(seedByte: 0x02), carrier: .oneToOneProfile(fromOtherParticipant: true),
            fromAccountKeyB64: b64(old), scopeHex: "aa"))

        // The role table and the leaf binding both still name old keys.
        let wire = try XCTUnwrap(party.center.composeDelegations(
            addedAccountKey: old, candidateAccountKeys: [committerOld], groupIdHex: "aa"))
        let envelope = try decodeCommitterDelegationEnvelope(plaintext: wire)
        let delegations = try checkCommitterDelegations(envelope: envelope)

        XCTAssertEqual(envelope.addedAccountKey, new)
        XCTAssertEqual(delegations.map(\.mailbox), [try XCTUnwrap(MailboxAddress.pseudonym(of: new))])
        XCTAssertEqual(delegations.map(\.delegatePk), [committerNew])
    }

    // MARK: - Rule 4: a predecessor mailbox is read for 30 days, then left

    func testAPredecessorMailboxIsReadForThirtyDaysAfterAdoptingAndThenLeft() {
        let day: Int64 = 24 * 60 * 60 * 1000
        let adopted: Int64 = 1_700_000_000_000

        XCTAssertEqual(PredecessorMailbox.readWindowMs, 30 * day)
        XCTAssertTrue(PredecessorMailbox.reads(adoptedAtMs: adopted, nowMs: adopted))
        XCTAssertTrue(PredecessorMailbox.reads(adoptedAtMs: adopted, nowMs: adopted + 29 * day))
        XCTAssertTrue(PredecessorMailbox.reads(adoptedAtMs: adopted, nowMs: adopted + 30 * day - 1))
        XCTAssertFalse(PredecessorMailbox.reads(adoptedAtMs: adopted, nowMs: adopted + 30 * day))
        XCTAssertFalse(PredecessorMailbox.reads(adoptedAtMs: adopted, nowMs: adopted + 400 * day))
        XCTAssertTrue(
            PredecessorMailbox.reads(adoptedAtMs: adopted, nowMs: adopted - day),
            "a clock that stepped back never ends the window early")
    }

    func testTheAdoptionTimeIsKeptOnceAndDecidesWhichMailboxesAreRead() throws {
        let suite = "mailbox-after-rotation-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = PredecessorMailbox.AdoptionStore(defaults: defaults)
        let day: Int64 = 24 * 60 * 60 * 1000
        let t0: Int64 = 1_700_000_000_000

        XCTAssertNil(store.adoptedAtMs(accountIdB64: "acct", keyB64: "old"))
        XCTAssertEqual(store.recordIfAbsent(accountIdB64: "acct", keyB64: "old", nowMs: t0), t0)
        XCTAssertEqual(
            store.recordIfAbsent(accountIdB64: "acct", keyB64: "old", nowMs: t0 + 20 * day), t0,
            "re-adopting never reopens the window")

        // A second rotation 20 days later retires another key.
        store.recordIfAbsent(accountIdB64: "acct", keyB64: "newer", nowMs: t0 + 20 * day)
        XCTAssertEqual(
            PredecessorMailbox.readable(
                keysB64: ["old", "newer"], accountIdB64: "acct", nowMs: t0 + 25 * day, adoptions: store),
            ["old", "newer"])
        XCTAssertEqual(
            PredecessorMailbox.readable(
                keysB64: ["old", "newer"], accountIdB64: "acct", nowMs: t0 + 31 * day, adoptions: store),
            ["newer"])
        XCTAssertEqual(
            PredecessorMailbox.readable(
                keysB64: ["old", "newer"], accountIdB64: "acct", nowMs: t0 + 51 * day, adoptions: store),
            [])

        // A key adopted before the time was kept starts its window when first seen.
        XCTAssertEqual(
            PredecessorMailbox.readable(
                keysB64: ["unstamped"], accountIdB64: "acct", nowMs: t0, adoptions: store),
            ["unstamped"])
        XCTAssertEqual(store.adoptedAtMs(accountIdB64: "acct", keyB64: "unstamped"), t0)

        store.clear(accountIdB64: "acct")
        XCTAssertNil(store.adoptedAtMs(accountIdB64: "acct", keyB64: "old"))
    }

    // MARK: - Rule 3: the profile re-send reports what did not land

    func testTheProfileReSendReportsTheConversationsItDidNotReach() async {
        struct Down: Error {}
        var down: Set<String> = ["g2"]
        var sent: [String] = []
        let publisher = ProfilePublisher(ports: ProfilePublisher.Ports(
            send: { group, _ in
                if down.contains(group) { throw Down() }
                sent.append(group)
            },
            envelope: { _ in Data([1]) },
            canSend: { $0 != "evicted" },
            nowMs: { 100_000 },
            jitterMs: { 0 },
            sleep: { _ in }))

        let failed = await publisher.publishToAllReporting(groupIdHexes: ["g1", "g2", "evicted", "g3"])

        XCTAssertEqual(failed, ["g2"], "a failed send is reported; a group this device left is not a failure")
        XCTAssertEqual(sent, ["g1", "g3"])

        down = []
        let retried = await publisher.publishToAllReporting(groupIdHexes: failed)
        XCTAssertEqual(retried, [])
        XCTAssertEqual(sent, ["g1", "g3", "g2"])
    }
}

/// A DS transport for tests that never register: any call is a failure.
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
