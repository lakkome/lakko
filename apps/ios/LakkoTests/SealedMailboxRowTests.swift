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
        XCTAssertEqual(result.events.count, 3)
        guard case .welcome(let welcomeId, let welcomeBytes, _) = result.events[0] else {
            return XCTFail("first event: \(result.events[0])")
        }
        XCTAssertEqual(welcomeId, 1)
        XCTAssertEqual(welcomeBytes, welcome)
        guard case .contactRequest(let req, _, _) = result.events[1] else {
            return XCTFail("second event: \(result.events[1])")
        }
        XCTAssertEqual(req.messageId, 5)
        XCTAssertEqual(req.fromPubkey, bob.signaturePublicKey())
        XCTAssertEqual(req.intro, "hello")
        guard case .contactAck(let incomingAck) = result.events[2] else {
            return XCTFail("third event: \(result.events[2])")
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

    func testASuccessorWelcomeWithNoCarrierIsARecordedNoOp() throws {
        XCTAssertEqual(ContinuityWelcome.decide(plan: nil), .skipNoCarrier)
        let plan = MailboxWritePlan(target: try ownTarget(try identity(0x61)), candidates: [])
        XCTAssertEqual(ContinuityWelcome.decide(plan: plan), .deliver(plan))
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
