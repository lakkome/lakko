// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0078 phase 1 — sealed mailbox rows on iOS: every row is a sealed box
/// to the recipient's mailbox key with the kind inside the seal, the reader
/// opens each row with its own mailbox key and dispatches by that kind, a
/// writer seals only to a mailbox whose binding verified against the account
/// key it holds, a write without a reach proof spends a `mailbox-write`
/// token, and the continuity Welcome is a recorded no-op.
@MainActor
final class SealedMailboxRowTests: XCTestCase {

    private func identity(_ byte: UInt8) throws -> Identity {
        try Identity.fromSeed(seed: Data(repeating: byte, count: 32))
    }

    private func ownTarget(_ id: Identity) throws -> MailboxTarget {
        try MailboxTarget.verified(
            binding: signMailboxBinding(accountIdentity: id), accountKey: id.signaturePublicKey())
    }

    private static let allKinds: [FfiMailboxKind] = [.contactRequest, .welcome, .contactAck, .welcomeRendezvous]

    // MARK: - Sealing

    func testEveryKindSealsToAWellFormedRowThatOnlyItsMailboxOpens() throws {
        let alice = try identity(0x61), bob = try identity(0x62)
        let target = try ownTarget(alice)
        let aliceKey = MailboxKey.fromAccountIdentity(accountIdentity: alice)
        let bobKey = MailboxKey.fromAccountIdentity(accountIdentity: bob)
        XCTAssertEqual(target.address, aliceKey.address())
        XCTAssertEqual(target.mailboxPk, aliceKey.publicKey())
        XCTAssertEqual(target, MailboxTarget.own(accountIdentity: alice))

        for kind in Self.allKinds {
            let body = Data("the inner wire of \(kind)".utf8)
            let row = try target.seal(kind: kind, body: body)
            XCTAssertTrue(isWellFormedMailboxRow(row: row))
            XCTAssertNil(row.range(of: body), "the body never travels in the clear")
            let opened = try XCTUnwrap(MailboxRows.open(row, key: aliceKey))
            XCTAssertEqual(opened.kind, kind)
            XCTAssertEqual(opened.body, body)
            XCTAssertNil(MailboxRows.open(row, key: bobKey), "a row for another mailbox does not open")
        }
    }

    /// The clear one-byte kind is gone: every row starts with the envelope
    /// version, whatever it carries.
    func testNoRowCarriesItsKindInTheClear() throws {
        let target = try ownTarget(try identity(0x61))
        let firstBytes = Set(try Self.allKinds.map { try target.seal(kind: $0, body: Data([1, 2, 3])).first })
        XCTAssertEqual(firstBytes, [0x01])
    }

    // MARK: - Opening and dispatching

    func testThePollOpensEveryRowAndDispatchesByTheKindInsideTheSeal() throws {
        let alice = try identity(0x61), bob = try identity(0x62)
        let toAlice = try ownTarget(alice), toBob = try ownTarget(bob)
        let request = try composeContactRequest(
            senderSigningSeed: bob.ed25519Seed(), senderAccountId: Data(repeating: 0xB0, count: 16),
            recipientX25519Pub: alice.x25519PublicKey(), fromPseudonym: Data(repeating: 0xB1, count: 32),
            validAfterMs: 1, privacyPassToken: Data(), profile: nil, replyGrant: nil, linkAnswer: nil,
            introMessage: Data("hello".utf8))
        let ack = try composeContactAck(
            accepterSigningSeed: bob.ed25519Seed(), accepterAccountId: Data(repeating: 0xB0, count: 16),
            accepterX25519Pub: bob.x25519PublicKey(), ackToPseudonym: Data(repeating: 0xA1, count: 32),
            acceptedAtMs: 2)
        let welcome = Data("an MLS welcome".utf8)
        let rows = [
            MailboxMessage(id: 1, payload: try toAlice.seal(kind: .welcome, body: welcome), createdAt: "t1"),
            MailboxMessage(
                id: 2, payload: try toAlice.seal(kind: .welcomeRendezvous, body: welcome), createdAt: "t2"),
            MailboxMessage(id: 3, payload: Data(repeating: 0, count: 1073), createdAt: "t3"),
            MailboxMessage(id: 4, payload: try toBob.seal(kind: .welcome, body: welcome), createdAt: "t4"),
            MailboxMessage(id: 5, payload: try toAlice.seal(kind: .contactRequest, body: request), createdAt: "t5"),
            MailboxMessage(id: 6, payload: try toAlice.seal(kind: .contactAck, body: ack), createdAt: "t6"),
        ]

        let result = ContactsClient.dispatch(
            rows: rows, after: 0, hasMore: false,
            mailboxKey: MailboxKey.fromAccountIdentity(accountIdentity: alice),
            myX25519Secret: alice.x25519SecretKey())

        XCTAssertEqual(result.cursor, 6, "the cursor moves past every row, dropped ones included")
        XCTAssertEqual(result.events.count, 4)
        guard case .welcome(let welcomeId, let welcomeBytes, _) = result.events[0] else {
            return XCTFail("first event: \(result.events[0])")
        }
        XCTAssertEqual(welcomeId, 1)
        XCTAssertEqual(welcomeBytes, welcome)
        // ADR-0045 decision 2 — a Welcome rendezvous (an approved publisher
        // request) is surfaced, its body being the Welcome, so the mailbox
        // loop lands it whenever it arrives.
        XCTAssertEqual(result.events[1], .publisherWelcome(messageId: 2, welcomeBytes: welcome))
        guard case .contactRequest(let req, _, _) = result.events[2] else {
            return XCTFail("third event: \(result.events[2])")
        }
        XCTAssertEqual(req.messageId, 5)
        XCTAssertEqual(req.fromPubkey, bob.signaturePublicKey())
        XCTAssertEqual(req.intro, "hello")
        guard case .contactAck(let incomingAck) = result.events[3] else {
            return XCTFail("fourth event: \(result.events[3])")
        }
        XCTAssertEqual(incomingAck.accepterPubkey, bob.signaturePublicKey())
    }

    func testAnUndecodableRowIsDroppedAndNothingElseIsLost() throws {
        let alice = try identity(0x61)
        let toAlice = try ownTarget(alice)
        let result = ContactsClient.dispatch(
            rows: [
                MailboxMessage(id: 9, payload: Data("not a row".utf8), createdAt: ""),
                MailboxMessage(id: 10, payload: try toAlice.seal(kind: .welcome, body: Data([7])), createdAt: ""),
            ],
            after: 8, hasMore: true,
            mailboxKey: MailboxKey.fromAccountIdentity(accountIdentity: alice),
            myX25519Secret: alice.x25519SecretKey())
        XCTAssertEqual(result.cursor, 10)
        XCTAssertTrue(result.hasMore)
        XCTAssertEqual(result.events.count, 1)
    }

    // MARK: - Binding verification before every write

    func testABindingSignedBySomeoneElseNamesNoMailbox() throws {
        let alice = try identity(0x61), mallory = try identity(0x6D)
        let malloryBinding = signMailboxBinding(accountIdentity: mallory)
        XCTAssertThrowsError(try MailboxTarget.verified(
            binding: malloryBinding, accountKey: alice.signaturePublicKey()))
        XCTAssertNil(MailboxTarget.verifiedOrNil(binding: malloryBinding, accountKey: alice.signaturePublicKey()))
        XCTAssertNil(MailboxTarget.verifiedOrNil(binding: Data([1, 2, 3]), accountKey: alice.signaturePublicKey()))

        let grant = try deriveReachGrant(
            accountSeed: mallory.ed25519Seed(), class: .contact, scopeId: Data(), version: 0)
        XCTAssertNil(MailboxTarget.verifiedOrNil(grant: grant, accountKey: alice.signaturePublicKey()))
        XCTAssertEqual(
            MailboxTarget.verifiedOrNil(grant: grant, accountKey: mallory.signaturePublicKey())?.address,
            MailboxKey.fromAccountIdentity(accountIdentity: mallory).address())
    }

    private func sendRequest(
        to recipient: Identity, from sender: Identity, reach: ContactRequestReach, scope: String
    ) async throws -> SendResult {
        let client = ContactsClient(ds: DsAccessURLProtocol.client(scope))
        return try await client.sendContactRequestToBundle(
            senderSigningSeed: sender.ed25519Seed(),
            senderAccountId: Data(repeating: 0xB0, count: 16),
            senderSignaturePubkey: sender.signaturePublicKey(),
            recipientAccountId: Data(repeating: 0xA0, count: 16),
            recipientSignaturePubkey: recipient.signaturePublicKey(),
            recipientX25519Pubkey: recipient.x25519PublicKey(),
            introMessage: Data("hi".utf8),
            reach: reach)
    }

    func testARequestWithNoVerifiedMailboxWritesNothing() async throws {
        let alice = try identity(0x61), bob = try identity(0x62), carol = try identity(0x63)
        let scope = DsAccessURLProtocol.freshScope()
        do {
            _ = try await sendRequest(to: alice, from: bob, reach: .none, scope: scope)
            XCTFail("no mailbox, no request")
        } catch ContactsError.requestRefused {}
        // A mailbox verified for someone else is not the recipient's.
        let carolPlan = MailboxWritePlan(target: try ownTarget(carol), candidates: [])
        do {
            _ = try await sendRequest(
                to: alice, from: bob,
                reach: ContactRequestReach(replyGrant: { _ in nil }, plan: { _ in carolPlan }),
                scope: scope)
            XCTFail("a mailbox verified under another key is refused")
        } catch ContactsError.requestRefused {}
        XCTAssertTrue(DsAccessURLProtocol.requests(scope).isEmpty, "nothing reached the DS")
    }

    func testARequestIsOneSealedRowToTheVerifiedMailboxSignedWithItsGrant() async throws {
        let alice = try identity(0x61), bob = try identity(0x62)
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [
            .init(status: 201, body: Data(#"{"id":5,"created_at":"now"}"#.utf8)),
        ])
        let seed = Data(repeating: 0x33, count: 32)
        let plan = MailboxWritePlan(target: try ownTarget(alice), candidates: [DsMailboxWriteAuth(reachSeed: seed)])
        let result = try await sendRequest(
            to: alice, from: bob,
            reach: ContactRequestReach(replyGrant: { _ in nil }, plan: { _ in plan }),
            scope: scope)
        XCTAssertEqual(result.messageId, 5)

        let seen = try XCTUnwrap(DsAccessURLProtocol.requests(scope).first)
        let aliceKey = MailboxKey.fromAccountIdentity(accountIdentity: alice)
        XCTAssertEqual(seen.url.path, "/native/ds/ds/mailbox/\(DsClient.pseudonymHex(aliceKey.address()))/messages")
        XCTAssertNotNil(seen.proof, "signed with the grant")
        XCTAssertNil(seen.anonToken, "a write with a reach proof spends no token")
        let opened = try XCTUnwrap(MailboxRows.open(seen.body, key: aliceKey))
        XCTAssertEqual(opened.kind, .contactRequest)
        XCTAssertEqual(try ContactRequest.decodeAndVerify(wire: opened.body).fromPubkey(), bob.signaturePublicKey())
    }

    func testARequestsFromPseudonymIsFreshRandomBytesNotTheSendersMailbox() async throws {
        let alice = try identity(0x61), bob = try identity(0x62)
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [
            .init(status: 201, body: Data(#"{"id":5,"created_at":"now"}"#.utf8)),
            .init(status: 201, body: Data(#"{"id":6,"created_at":"now"}"#.utf8)),
        ])
        let plan = MailboxWritePlan(
            target: try ownTarget(alice),
            candidates: [DsMailboxWriteAuth(reachSeed: Data(repeating: 0x33, count: 32))])
        let reach = ContactRequestReach(replyGrant: { _ in nil }, plan: { _ in plan })
        _ = try await sendRequest(to: alice, from: bob, reach: reach, scope: scope)
        _ = try await sendRequest(to: alice, from: bob, reach: reach, scope: scope)

        let aliceKey = MailboxKey.fromAccountIdentity(accountIdentity: alice)
        let bobAddress = MailboxKey.fromAccountIdentity(accountIdentity: bob).address()
        let pseudonyms = try DsAccessURLProtocol.requests(scope).map { seen -> Data in
            let opened = try XCTUnwrap(MailboxRows.open(seen.body, key: aliceKey))
            return try ContactRequest.decodeAndVerify(wire: opened.body).fromPseudonym()
        }
        XCTAssertEqual(pseudonyms.count, 2)
        for pseudonym in pseudonyms {
            XCTAssertEqual(pseudonym.count, 32)
            XCTAssertNotEqual(pseudonym, bobAddress, "the sender's own mailbox address is not carried")
        }
        XCTAssertNotEqual(pseudonyms[0], pseudonyms[1], "a fresh value per request")
    }

    // MARK: - The `mailbox-write` token

    func testAProoflessWriteSpendsOneMailboxWriteToken() async throws {
        let target = try ownTarget(try identity(0x61))
        let tokens = FakeAnonTokens()
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [.init(status: 201, body: Data(#"{"id":1,"created_at":"t"}"#.utf8))])
        _ = try await DsAccessURLProtocol.client(scope, tokens: tokens).publishMailbox(
            pseudonym: target.address, payload: try target.seal(kind: .welcome, body: Data([1])))
        XCTAssertEqual(tokens.taken, [.mailboxWrite])
        let seen = try XCTUnwrap(DsAccessURLProtocol.requests(scope).first)
        XCTAssertEqual(seen.anonToken, "mailbox-write-1")
        XCTAssertNil(seen.proof)
        XCTAssertNil(seen.authorization)
    }

    func testAWriteWithAReachProofSpendsNoToken() async throws {
        let target = try ownTarget(try identity(0x61))
        let tokens = FakeAnonTokens()
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [.init(status: 201, body: Data(#"{"id":1,"created_at":"t"}"#.utf8))])
        _ = try await DsAccessURLProtocol.client(scope, tokens: tokens).publishMailbox(
            pseudonym: target.address, payload: try target.seal(kind: .welcome, body: Data([1])),
            writeAuth: DsMailboxWriteAuth(reachSeed: Data(repeating: 0x44, count: 32)))
        XCTAssertTrue(tokens.taken.isEmpty)
        XCTAssertNotNil(try XCTUnwrap(DsAccessURLProtocol.requests(scope).first).proof)
    }

    func testAPayloadThatIsNotASealedRowNeverLeavesTheDevice() async throws {
        let scope = DsAccessURLProtocol.freshScope()
        let tokens = FakeAnonTokens()
        do {
            _ = try await DsAccessURLProtocol.client(scope, tokens: tokens).publishMailbox(
                pseudonym: Data(repeating: 7, count: 32), payload: Data([0x02, 0x01]))
            XCTFail("a malformed row is refused")
        } catch DsClientError.argument {}
        XCTAssertTrue(DsAccessURLProtocol.requests(scope).isEmpty)
        XCTAssertTrue(tokens.taken.isEmpty, "no token is spent on a refused row")
    }

    // MARK: - Grants checked at receipt

    private func center() throws -> ReachCenter {
        let c = ReachCenter(persistence: InMemoryReachPersistence(), ds: Offline.ds())
        c.activate(accountIdB64: "sealed-rows", accountIdentity: try identity(0x31))
        return c
    }

    /// A `/g/` join attribution's reply grant is kept only under the joiner's
    /// account key it was resolved to, and only when its binding verifies
    /// there.
    func testAJoinAttributionReplyGrantIsCheckedAgainstTheJoinersAccountKey() throws {
        let c = try center()
        let joiner = try identity(0x71), other = try identity(0x72)
        let reply = try deriveReachGrant(
            accountSeed: joiner.ed25519Seed(), class: .reply, scopeId: Data(repeating: 1, count: 16), version: 0)
        XCTAssertFalse(c.keep(
            reply, carrier: .request,
            fromAccountKeyB64: other.signaturePublicKey().base64EncodedString(), scopeHex: nil))
        XCTAssertNil(c.writePlan(toAccountKey: other.signaturePublicKey()))
        XCTAssertTrue(c.keep(
            reply, carrier: .request,
            fromAccountKeyB64: joiner.signaturePublicKey().base64EncodedString(), scopeHex: nil))
        let plan = try XCTUnwrap(c.writePlan(toAccountKey: joiner.signaturePublicKey()))
        XCTAssertEqual(plan.target.address, MailboxKey.fromAccountIdentity(accountIdentity: joiner).address())
        XCTAssertEqual(plan.candidates.map(\.reachSeed), [reply.seed])
    }

    /// A joiner's reply grant read from a knock or an attribution survives an
    /// app restart between reading the request and approving it: the
    /// Welcome the approve sends is addressed through it, so a grant held in
    /// memory only would leave the joiner a member nobody told.
    func testAJoinReplyGrantSurvivesAReopenOfTheApp() throws {
        let persistence = InMemoryReachPersistence()
        let admin = try identity(0x31), joiner = try identity(0x75)
        let first = ReachCenter(persistence: persistence, ds: Offline.ds())
        first.activate(accountIdB64: "sealed-rows", accountIdentity: admin)
        let reply = try deriveReachGrant(
            accountSeed: joiner.ed25519Seed(), class: .reply, scopeId: Data(repeating: 2, count: 16), version: 0)
        XCTAssertTrue(first.keep(
            reply, carrier: .request,
            fromAccountKeyB64: joiner.signaturePublicKey().base64EncodedString(), scopeHex: nil))

        // The app is closed and opened again: a fresh centre over the same
        // store, as at launch.
        let reopened = ReachCenter(persistence: persistence, ds: Offline.ds())
        reopened.activate(accountIdB64: "sealed-rows", accountIdentity: admin)
        let plan = try XCTUnwrap(reopened.writePlan(toAccountKey: joiner.signaturePublicKey()))
        XCTAssertEqual(plan.target.address, MailboxKey.fromAccountIdentity(accountIdentity: joiner).address())
        XCTAssertEqual(plan.candidates.map(\.reachSeed), [reply.seed])
    }

    /// A profile grant is kept only from the sender resolved at receipt whose
    /// key its binding verifies under.
    func testAProfileGrantIsCheckedAgainstTheSenderResolvedAtReceipt() throws {
        let c = try center()
        let sender = try identity(0x73), impostor = try identity(0x74)
        let member = try deriveReachGrant(
            accountSeed: impostor.ed25519Seed(), class: .member, scopeId: Data(), version: 0)
        XCTAssertFalse(c.keep(
            member, carrier: .groupProfile,
            fromAccountKeyB64: sender.signaturePublicKey().base64EncodedString(), scopeHex: "aa"),
            "a grant naming someone else's mailbox, carried in the sender's profile")
        XCTAssertNil(c.writePlan(toAccountKey: sender.signaturePublicKey()))
    }

    func testAPersonWithNoGrantHasNoMailboxAndOwnMailboxIsTheAccountsOwn() throws {
        let c = try center()
        XCTAssertNil(c.writePlan(toAccountKey: try identity(0x75).signaturePublicKey()))
        let own = try XCTUnwrap(c.ownAccountKey)
        let plan = try XCTUnwrap(c.writePlan(toAccountKey: own))
        XCTAssertEqual(plan.target.address, MailboxKey.fromAccountIdentity(accountIdentity: try identity(0x31)).address())
        XCTAssertEqual(plan.candidates.map(\.reachSeed), [c.ownContactSeed()])
    }

    // MARK: - The `/c/` link grant

    private func callLink(owner: Identity, grantSeed: Data) throws -> CallInviteCapability {
        let linkId = Data(repeating: 0x4C, count: Int(linkIdLen()))
        let mint = try composeCallInviteCapability(
            ownerSigningSeed: owner.ed25519Seed(), ownerAccountId: Data(repeating: 1, count: 16),
            callId: Data(repeating: 2, count: 16), expiryMs: 0, maxUses: 0,
            media: FfiCallInviteMedia(audio: true, video: false),
            oneTimeKeyPackage: try owner.createKeyPackage(), label: "",
            linkId: linkId,
            linkGrant: try deriveReachGrant(accountSeed: grantSeed, class: .link, scopeId: linkId, version: 1))
        return try CallInviteCapability.openLink(sealed: mint.sealed, linkSecret: mint.linkSecret)
    }

    func testACallLinkGrantMustVerifyAgainstTheOwnersAccountKey() throws {
        let owner = try identity(0x81)
        let good = try callLink(owner: owner, grantSeed: owner.ed25519Seed())
        XCTAssertEqual(
            try good.verifiedLinkGrantMailboxPk(),
            MailboxKey.fromAccountIdentity(accountIdentity: owner).publicKey())
        XCTAssertTrue(CallInviteAcceptor.linkGrantVerifies(good))
        let foreign = try callLink(owner: owner, grantSeed: try identity(0x82).ed25519Seed())
        XCTAssertThrowsError(try foreign.verifiedLinkGrantMailboxPk()) { error in
            guard case FfiError.Mailbox = error else {
                return XCTFail("expected FfiError.Mailbox, got \(error)")
            }
        }
        XCTAssertFalse(
            CallInviteAcceptor.linkGrantVerifies(foreign), "a grant naming another mailbox refuses the link")
    }

    // MARK: - The continuity Welcome (ADR-0078 open question)

    func testASuccessorWelcomeWithNoCarrierIsUnreachable() throws {
        XCTAssertEqual(ContinuityWelcome.decide(plan: nil), .unreachable)
        let plan = MailboxWritePlan(target: try ownTarget(try identity(0x61)), candidates: [])
        XCTAssertEqual(ContinuityWelcome.decide(plan: plan), .deliver(plan))
    }

    /// 2026-11-25 — a target with no carrier stops the WHOLE delivery before
    /// anything is written; it is never skipped and marked delivered.
    func testOneTargetWithNoCarrierPlansNothing() throws {
        let plan = MailboxWritePlan(target: try ownTarget(try identity(0x62)), candidates: [])
        let reached = Data([1])
        let unreached = Data([2])
        let lookup: (Data) -> MailboxWritePlan? = { $0 == reached ? plan : nil }
        XCTAssertNil(ContinuityWelcome.planAll(targets: [reached, unreached], plan: lookup))
        XCTAssertNil(ContinuityWelcome.planAll(targets: [unreached, reached], plan: lookup))
        let all = ContinuityWelcome.planAll(targets: [reached], plan: lookup)
        XCTAssertEqual(all?.map(\.key), [reached])
        XCTAssertEqual(all?.map(\.plan), [plan])
    }

    /// The reachability gate before prepare and claim: a plan with no
    /// candidate is no way in.
    func testTheReachabilityGateWantsACandidate() throws {
        XCTAssertFalse(ContinuityWelcome.reachable(plan: nil))
        let target = try ownTarget(try identity(0x63))
        XCTAssertFalse(ContinuityWelcome.reachable(plan: MailboxWritePlan(target: target, candidates: [])))
        let seed = try identity(0x64).ed25519Seed()
        XCTAssertTrue(ContinuityWelcome.reachable(
            plan: MailboxWritePlan(target: target, candidates: [DsMailboxWriteAuth(reachSeed: seed)])))
    }

    /// A write the planner found nothing for is "unreachable" (retry later),
    /// through any wrapping; a write that was tried and failed is not.
    func testAWelcomeWriteWithNoMailboxReadsAsUnreachable() {
        XCTAssertTrue(ContinuityWelcome.isUnreachable(MailboxUnknown()))
        XCTAssertTrue(ContinuityWelcome.isUnreachable(
            ChatError.wrapped("DS publishMailbox (Welcome) failed", underlying: MailboxUnknown())))
        XCTAssertFalse(ContinuityWelcome.isUnreachable(ChatError.argument("offline")))
    }

    /// The reset account's new key alone (no grant from it) names no mailbox:
    /// nothing is derived from the identity key, so the plan the continuity
    /// path asks for is empty.
    func testAResetAccountsNewKeyAloneNamesNoMailbox() throws {
        let c = try center()
        XCTAssertNil(c.writePlan(toAccountKey: try identity(0x91).signaturePublicKey()))
        XCTAssertTrue(c.writeAuthCandidates(toMailbox: Data(repeating: 9, count: 32)).isEmpty)
    }
}

/// A publisher request has no deadline (ADR-0045 decision 2, ADR-0087
/// section 15): it is recorded before the knock, and the Welcome the owner's
/// device sends whenever it approves lands through the mailbox receive loop.
/// Mirror of `apps/web/src/channels/publisher-welcome-landing.test.ts`,
/// driven through the real core: an owner retains a bare KeyPackage and
/// approves it, the Welcome is sealed raw into a `welcomeRendezvous` row, and
/// the joiner's mailbox dispatch surfaces it and lands the request.
@MainActor
final class PendingPublisherRequestTests: XCTestCase {
    private let account = "pending-publisher-test-\(UUID().uuidString)"

    override func tearDown() {
        PendingPublisherRequests.clear(accountIdB64: account)
        super.tearDown()
    }

    private func hex(_ data: Data) -> String { data.map { String(format: "%02x", $0) }.joined() }

    private func record(
        channel: String, group: String, joiner: Data, name: String = "Pixel Pub Walk",
        ownerKey: Data = Data(repeating: 0x0A, count: 32)
    ) throws {
        try PendingPublisherRequests.record(
            PendingPublisherRequest(
                channelIdHex: channel, groupIdHex: group, joinerKeyB64: base64Encode(joiner),
                ownerAccountKeyB64: base64Encode(ownerKey),
                name: name, description: "", requestedAtMs: 1),
            accountIdB64: account)
    }

    private let keys = ChannelPublisherKeys(
        seed: Data(repeating: 0x11, count: 32), signingKeySeed: Data(repeating: 0x22, count: 32))

    func testTheStoreRecordsReplacesDropsAndClears() throws {
        let joiner = Data(repeating: 1, count: 32)
        try record(channel: "aa", group: "01", joiner: joiner)
        try record(channel: "bb", group: "02", joiner: joiner)
        // A second request for the same channel replaces the first.
        try record(channel: "aa", group: "03", joiner: joiner)
        XCTAssertEqual(PendingPublisherRequests.load(accountIdB64: account).map(\.channelIdHex), ["bb", "aa"])
        XCTAssertEqual(PendingPublisherRequests.load(accountIdB64: account).last?.groupIdHex, "03")
        XCTAssertTrue(PendingPublisherRequests.isPending(channelIdHex: "aa", accountIdB64: account))
        PendingPublisherRequests.drop(channelIdHex: "aa", accountIdB64: account)
        XCTAssertFalse(PendingPublisherRequests.isPending(channelIdHex: "aa", accountIdB64: account))
        XCTAssertTrue(PendingPublisherRequests.isPending(channelIdHex: "bb", accountIdB64: account))
        PendingPublisherRequests.clear(accountIdB64: account)
        XCTAssertTrue(PendingPublisherRequests.load(accountIdB64: account).isEmpty)
    }

    func testARawWelcomeInARendezvousRowLandsTheRecordedRequest() throws {
        // ADR-0090 — PublisherGroup leaves are bound DEVICE identities.
        let ownerSeed = Data(repeating: 0x51, count: 32)
        let owner = try Identity.fromSeedBound(
            seed: Data(repeating: 0x61, count: 32), accountSeed: ownerSeed, issuedAtUnixSecs: 1_700_000_000)
        let joinerAccount = try Identity.fromSeed(seed: Data(repeating: 0x52, count: 32))
        let joiner = try Identity.fromSeedBound(
            seed: Data(repeating: 0x62, count: 32), accountSeed: joinerAccount.ed25519Seed(),
            issuedAtUnixSecs: 1_700_000_000)
        let channelId = Data(repeating: 0x07, count: 16)
        let group = try PublisherGroup.create(identity: owner, ownerAccountSeed: ownerSeed, channelId: channelId)
        let ownerKey = try publisherOwnerAccountKey(ownerAccountSeed: ownerSeed)
        // The joiner knocks with a KeyPackage and records the request.
        let keyPackage = try joiner.createKeyPackage()
        try record(
            channel: hex(channelId), group: hex(try group.groupId()), joiner: joiner.signaturePublicKey(),
            ownerKey: ownerKey)
        // The owner approves, any time later: staged, merged once the DS took it.
        let request = try group.retainJoinRequest(identity: owner, keyPackage: keyPackage)
        let welcome = try group.stageApproveExternalJoin(identity: owner, proposalRef: request.joinerSignatureKey).welcome
        XCTAssertTrue(try group.mergeStagedCommit(identity: owner))

        // Its Welcome reaches the joiner's mailbox as a rendezvous row whose
        // body is the raw Welcome.
        let target = try MailboxTarget.verified(
            binding: signMailboxBinding(accountIdentity: joinerAccount), accountKey: joinerAccount.signaturePublicKey())
        let row = MailboxMessage(id: 40, payload: try target.seal(kind: .welcomeRendezvous, body: welcome), createdAt: "t")
        let result = ContactsClient.dispatch(
            rows: [row], after: 0, hasMore: false,
            mailboxKey: MailboxKey.fromAccountIdentity(accountIdentity: joinerAccount),
            myX25519Secret: joinerAccount.x25519SecretKey())
        guard case .publisherWelcome(_, let welcomeBytes) = result.events.first else {
            return XCTFail("expected a publisher welcome, got \(result.events)")
        }

        let state = ChannelsUiState(accountIdB64: account)
        var persisted = 0
        var joinedGroups: [Data] = []
        let landed = try CoPublisherJoin.landPendingWelcome(
            welcomeBytes, accountIdB64: account, identity: joiner, state: state,
            join: { identity, welcome, groupId, channelIdHex, ownerKey, _ in
                // The real join through the PublisherGroup entry point.
                _ = try publisherGroupJoinFromWelcome(
                    identity: identity, welcome: welcome, groupId: groupId,
                    channelId: CoPublisher.hexToData(channelIdHex) ?? Data(), ownerAccountKey: ownerKey)
                joinedGroups.append(groupId)
                return self.keys
            },
            persist: { persisted += 1 })

        XCTAssertEqual(landed, hex(channelId))
        XCTAssertEqual(joinedGroups, [try group.groupId()])
        XCTAssertEqual(state.keysByChannel[hex(channelId)], keys)
        XCTAssertTrue(state.channels.contains { $0.id == hex(channelId) && $0.localName == "Pixel Pub Walk" })
        XCTAssertEqual(persisted, 1)
        XCTAssertTrue(PendingPublisherRequests.load(accountIdB64: account).isEmpty)
        state.removeChannel(hex(channelId))
    }

    func testAWelcomeThatIsNoPendingRequestsChangesNothing() throws {
        let joiner = try Identity.fromSeed(seed: Data(repeating: 0x53, count: 32))
        let state = ChannelsUiState(accountIdB64: account)
        // No request recorded.
        XCTAssertNil(try CoPublisherJoin.landPendingWelcome(
            Data([1, 2, 3]), accountIdB64: account, identity: joiner, state: state))
        try record(channel: String(repeating: "07", count: 16), group: String(repeating: "ab", count: 32),
                   joiner: joiner.signaturePublicKey())
        // Garbage, or another group's Welcome: the request stays.
        XCTAssertNil(try CoPublisherJoin.landPendingWelcome(
            Data([1, 2, 3]), accountIdB64: account, identity: joiner, state: state, persist: {}))
        XCTAssertEqual(PendingPublisherRequests.load(accountIdB64: account).count, 1)
        XCTAssertTrue(state.keysByChannel.isEmpty)
    }

    func testARequestMadeUnderAnotherKeyIsNotTried() throws {
        let joiner = try Identity.fromSeed(seed: Data(repeating: 0x54, count: 32))
        try record(channel: "cc", group: "0102", joiner: Data(repeating: 9, count: 32))
        var tried = false
        XCTAssertNil(try CoPublisherJoin.landPendingWelcome(
            Data([1]), accountIdB64: account, identity: joiner, state: ChannelsUiState(accountIdB64: account),
            join: { _, _, _, _, _, _ in tried = true; return self.keys }, persist: {}))
        XCTAssertFalse(tried)
    }

    func testAJoinThatCannotBeMadeDurableKeepsTheRecordAndThrows() throws {
        let joiner = try Identity.fromSeed(seed: Data(repeating: 0x55, count: 32))
        try record(channel: "dd", group: "0a0b", joiner: joiner.signaturePublicKey())
        struct DiskFull: Error {}
        XCTAssertThrowsError(try CoPublisherJoin.landPendingWelcome(
            Data([1]), accountIdB64: account, identity: joiner, state: ChannelsUiState(accountIdB64: account),
            join: { _, _, _, _, _, _ in self.keys }, persist: { throw DiskFull() }))
        XCTAssertTrue(PendingPublisherRequests.isPending(channelIdHex: "dd", accountIdB64: account))
    }
}
