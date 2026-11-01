// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// Accepting a contact request must complete, every time: the keys tried, the
/// order of the steps, what a failure leaves, and what the requester sees.
@MainActor
final class ContactAcceptTests: XCTestCase {

    private struct Refused: Error {}
    private let refused = DsClientError.http(status: 401, endpoint: "/ds/mailbox", body: "")

    private func auth(_ byte: UInt8) -> DsMailboxWriteAuth {
        DsMailboxWriteAuth(reachSeed: Data(repeating: byte, count: 32))
    }

    private func plan(_ byte: UInt8, _ keys: [UInt8]) -> MailboxWritePlan {
        let bytes = Data(repeating: byte, count: 32)
        return MailboxWritePlan(
            target: MailboxTarget(
                mailboxPk: bytes, address: bytes, binding: Data(count: 97), accountKey: Data(count: 32)),
            candidates: keys.map(auth))
    }

    private func request(accountId: Data? = Data(repeating: 1, count: 16)) -> IncomingContactRequest {
        IncomingContactRequest(
            messageId: 7, fromPubkey: Data(repeating: 0xA1, count: 32), fromAccountId: accountId,
            fromPseudonym: Data(count: 32), validAfterMs: 1, intro: nil,
            createdAt: "2026-10-30T12:00:00Z")
    }

    // MARK: - Every key in turn

    func testAStaleKeyBeforeALiveOneMovesOnAtARefusal() async throws {
        var tried: [DsMailboxWriteAuth?] = []
        let id = try await RefusedMailboxWrite.tryingCandidates([auth(1), auth(2), auth(3)]) { candidate in
            tried.append(candidate)
            if candidate == self.auth(1) { throw self.refused }
            return 42
        }
        XCTAssertEqual(id, 42)
        XCTAssertEqual(tried, [auth(1), auth(2)], "it stops at the key that opened the mailbox")
    }

    func testEveryKeyRefusedIsARefusal() async {
        var tried = 0
        do {
            let _: Int = try await RefusedMailboxWrite.tryingCandidates([auth(1), auth(2)]) { _ in
                tried += 1
                throw self.refused
            }
            XCTFail("a refusal was swallowed")
        } catch {
            XCTAssertTrue(RefusedMailboxWrite.isRefusal(error))
        }
        XCTAssertEqual(tried, 2)
    }

    func testANetworkFailureDoesNotTryTheNextKey() async {
        var tried = 0
        do {
            let _: Int = try await RefusedMailboxWrite.tryingCandidates([auth(1), auth(2)]) { _ in
                tried += 1
                throw URLError(.notConnectedToInternet)
            }
            XCTFail("a failure was swallowed")
        } catch {
            XCTAssertFalse(RefusedMailboxWrite.isRefusal(error))
        }
        XCTAssertEqual(tried, 1)
    }

    func testNoKeyIsOneWriteWithoutAProof() async throws {
        var tried: [DsMailboxWriteAuth?] = []
        _ = try await RefusedMailboxWrite.tryingCandidates([]) { candidate -> Int in
            tried.append(candidate)
            return 1
        }
        XCTAssertEqual(tried, [nil])
    }

    func testARefusedMailboxMovesOnToTheNext() async throws {
        var mailboxes: [Data] = []
        let id = try await RefusedMailboxWrite.tryingPlans([plan(1, [1]), plan(2, [2])]) { plan -> Int in
            mailboxes.append(plan.target.address)
            if plan.target.address == self.plan(1, [1]).target.address { throw self.refused }
            return 9
        }
        XCTAssertEqual(id, 9)
        XCTAssertEqual(mailboxes.count, 2)
    }

    func testNoPlanWritesNothing() async {
        do {
            let _: Int = try await RefusedMailboxWrite.tryingPlans([]) { _ in
                XCTFail("nothing may be written")
                return 0
            }
            XCTFail("no plan must throw")
        } catch {
            XCTAssertTrue(error is MailboxUnknown)
        }
    }

    // MARK: - The accept

    private final class Steps {
        var log: [String] = []
    }

    private func ports(
        _ steps: Steps, found: @escaping () throws -> AnswerFounding, ackFails: Bool = false
    ) -> ContactAccept.Ports {
        ContactAccept.Ports(
            found: { _, _ in
                steps.log.append("found")
                return try found()
            },
            file: { _, groupIdHex in steps.log.append("file:\(groupIdHex)") },
            ack: { _ in
                steps.log.append("ack")
                if ackFails { throw URLError(.notConnectedToInternet) }
            })
    }

    func testADeliveredWelcomeFilesTheContactThenAcks() async {
        let steps = Steps()
        let outcome = await ContactAccept.run(
            request: request(), ports: ports(steps, found: { .delivered(groupIdHex: "ab") }))
        XCTAssertEqual(outcome, .accepted(groupIdHex: "ab"))
        XCTAssertEqual(steps.log, ["found", "file:ab", "ack"])
    }

    func testAnOwedWelcomeShowsTheChatAndHoldsTheAck() async {
        let steps = Steps()
        let outcome = await ContactAccept.run(
            request: request(), ports: ports(steps, found: { .welcomeOwed(groupIdHex: "ab") }))
        XCTAssertEqual(outcome, .welcomeOwed(groupIdHex: "ab"))
        XCTAssertEqual(steps.log, ["found", "file:ab"], "no ack before the Welcome landed")
    }

    func testAFailureFilesNothing() async {
        let steps = Steps()
        let outcome = await ContactAccept.run(
            request: request(), ports: ports(steps, found: { throw self.refused }))
        XCTAssertEqual(outcome, .failed)
        XCTAssertEqual(steps.log, ["found"], "no accepted contact without a chat, and no ack")
    }

    func testARequestWithNoAccountIdFails() async {
        let steps = Steps()
        let outcome = await ContactAccept.run(
            request: request(accountId: nil), ports: ports(steps, found: { .delivered(groupIdHex: "ab") }))
        XCTAssertEqual(outcome, .failed)
        XCTAssertTrue(steps.log.isEmpty)
    }

    func testAFailedAckLeavesTheAcceptStanding() async {
        let steps = Steps()
        let outcome = await ContactAccept.run(
            request: request(), ports: ports(steps, found: { .delivered(groupIdHex: "ab") }, ackFails: true))
        XCTAssertEqual(outcome, .accepted(groupIdHex: "ab"))
    }

    func testAFailureLogsAFixedReason() {
        XCTAssertEqual(ContactAccept.reason(of: refused), "refused")
        XCTAssertEqual(ContactAccept.reason(of: MailboxUnknown()), "no-mailbox")
        XCTAssertEqual(
            ContactAccept.reason(of: ChatError.wrapped("x", underlying: MailboxUnknown())), "no-mailbox")
        XCTAssertEqual(ContactAccept.reason(of: ContactAccept.Unavailable()), "no-chat")
        XCTAssertEqual(ContactAccept.reason(of: URLError(.timedOut)), "failed")
    }

    func testARequestWithAFoundingOnRecordFoundsNoSecondGroup() {
        var row = owedRow("one", messageId: 7)
        row.groupId = base64Encode(Data([0xAB, 0xCD]))
        XCTAssertEqual(
            OwedAnswers.foundedGroupHex(requestMessageId: 7, rows: [row]),
            DsClient.pseudonymHex(Data([0xAB, 0xCD])))
        XCTAssertNil(OwedAnswers.foundedGroupHex(requestMessageId: 8, rows: [row]))
        XCTAssertNil(OwedAnswers.foundedGroupHex(requestMessageId: 7, rows: []))
    }

    // MARK: - The owed Welcome

    private func owedRow(_ id: String, messageId: Int64? = 7) -> PendingCommitEntry {
        PendingCommitEntry(
            id: id, accountId: "a", groupId: "g", targetEpoch: "1", commitB64: "",
            secondArtifactB64: "AA==", secondArtifactKind: .welcome,
            sweepVerdictPending: true, sweepWelcomesOwed: true,
            answerRequesterKeyB64: "AA==", answerRequestMessageId: messageId)
    }

    func testALandedWelcomeDropsItsEntryThenAcks() async {
        let steps = Steps()
        await OwedAnswers.resolve(rows: [owedRow("one")], ports: OwedAnswers.Ports(
            deliver: { steps.log.append("deliver:\($0.id)") },
            landed: { steps.log.append("landed:\($0.id)") },
            ack: { steps.log.append("ack:\($0)") }))
        XCTAssertEqual(steps.log, ["deliver:one", "landed:one", "ack:7"])
    }

    func testAnUndeliveredWelcomeStaysOwedAndSendsNoAck() async {
        let steps = Steps()
        await OwedAnswers.resolve(rows: [owedRow("one"), owedRow("two")], ports: OwedAnswers.Ports(
            deliver: { row in
                if row.id == "one" { throw self.refused }
                steps.log.append("deliver:\(row.id)")
            },
            landed: { steps.log.append("landed:\($0.id)") },
            ack: { steps.log.append("ack:\($0)") }))
        XCTAssertEqual(steps.log, ["deliver:two", "landed:two", "ack:7"])
    }

    func testAnOwedEntrySurvivesAWriteAndAnOldEntryStillDecodes() throws {
        let store = CommitOutboxStore(rootDir: FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString))
        let entry = owedRow("one")
        try store.putEntryDurable(accountIdB64: "a", entry: entry)
        XCTAssertEqual(store.loadUnacked(accountIdB64: "a", groupIdB64: "g"), [entry])

        let old = #"{"id":"x","accountId":"a","groupId":"g","targetEpoch":"1","commitB64":"","acked":false}"#
        let decoded = try JSONDecoder().decode(PendingCommitEntry.self, from: Data(old.utf8))
        XCTAssertNil(decoded.answerRequesterKeyB64)
        XCTAssertNil(decoded.answerRequestMessageId)
    }

    // MARK: - The badge

    func testTheContactsBadgeCountsWaitingRequests() {
        XCTAssertNil(ContactsViewModel.requestsBadge(waiting: 0))
        XCTAssertEqual(ContactsViewModel.requestsBadge(waiting: 1), 1.formatted(.number.locale(Localization.shared.locale)))
        XCTAssertEqual(ContactsViewModel.requestsBadge(waiting: 99), 99.formatted(.number.locale(Localization.shared.locale)))
        XCTAssertEqual(ContactsViewModel.requestsBadge(waiting: 100), Localization.text("chats.row.unread_overflow"))
    }

    // MARK: - The requester's side

    private func sentRow(_ byte: UInt8) -> SentContactRequest {
        SentContactRequest(
            recipientUsername: "u\(byte)", recipientAccountIdB64: "acct\(byte)", messageId: Int64(byte),
            createdAt: "")
    }

    func testAWelcomeRetiresTheSentRowOfThatPersonOnly() {
        let anna = Data(repeating: 0xA1, count: 32)
        let bob = Data(repeating: 0xB2, count: 32)
        let answered = SentRequests.answered(
            byPeer: anna, in: [sentRow(1), sentRow(2)],
            keyOfAccount: { $0 == "acct1" ? anna : bob }, current: { $0 })
        XCTAssertEqual(answered.remaining.map(\.recipientAccountIdB64), ["acct2"])
        XCTAssertEqual(answered.accountIdB64, "acct1")
    }

    func testAWelcomeFromARotatedKeyRetiresTheRow() {
        let old = Data(repeating: 0xA1, count: 32)
        let new = Data(repeating: 0xA2, count: 32)
        let answered = SentRequests.answered(
            byPeer: new, in: [sentRow(1)], keyOfAccount: { _ in old },
            current: { $0 == old ? new : $0 })
        XCTAssertTrue(answered.remaining.isEmpty)
    }

    func testAWelcomeFromSomeoneElseOrAnUnpinnedAccountRetiresNothing() {
        let anna = Data(repeating: 0xA1, count: 32)
        let answered = SentRequests.answered(
            byPeer: anna, in: [sentRow(1)], keyOfAccount: { _ in nil }, current: { $0 })
        XCTAssertEqual(answered.remaining.count, 1)
        XCTAssertNil(answered.accountIdB64)
    }
}

/// The rule behind the Contacts tab: a one-to-one conversation IS a contact.
final class OneToOneIsAContactTests: XCTestCase {

    private let anna = Data(repeating: 0xA1, count: 32)
    private let bob = Data(repeating: 0xB2, count: 32)

    private func row(_ key: Data, groupIdHex: String?, displayName: String? = nil) -> AcceptedContact {
        AcceptedContact(
            request: IncomingContactRequest(
                messageId: 7, fromPubkey: key, fromAccountId: nil,
                fromPseudonym: Data(count: 32), validAfterMs: 1, intro: nil,
                createdAt: "2026-09-26T12:00:00Z"),
            groupIdHex: groupIdHex, displayName: displayName)
    }

    func testAPeerWithoutARowGetsOne() {
        let next = ContactsRepository.adopting(
            [], signatureKey: anna, accountId: Data(repeating: 1, count: 16),
            groupIdHex: "abcd", nowMs: 5, createdAt: "2026-09-26T12:00:00Z")
        XCTAssertEqual(next?.count, 1)
        XCTAssertEqual(next?.first?.request.fromPubkey, anna)
        XCTAssertEqual(next?.first?.groupIdHex, "abcd")
        XCTAssertEqual(next?.first?.request.fromAccountId, Data(repeating: 1, count: 16))
        // A synthetic row never collides with a mailbox id.
        XCTAssertLessThan(next?.first?.request.messageId ?? 0, 0)
    }

    func testAnExistingRowWithoutAChatIsCompletedAndKeepsItsName() {
        let next = ContactsRepository.adopting(
            [row(anna, groupIdHex: nil, displayName: "Anna W.")],
            signatureKey: anna, accountId: nil, groupIdHex: "abcd", nowMs: 5,
            createdAt: "2026-09-26T12:00:00Z")
        XCTAssertEqual(next?.count, 1)
        XCTAssertEqual(next?.first?.groupIdHex, "abcd")
        XCTAssertEqual(next?.first?.displayName, "Anna W.")
        XCTAssertEqual(next?.first?.request.messageId, 7)
    }

    func testACompleteRowIsLeftAlone() {
        XCTAssertNil(ContactsRepository.adopting(
            [row(anna, groupIdHex: "abcd")],
            signatureKey: anna, accountId: nil, groupIdHex: "ffff", nowMs: 5,
            createdAt: "2026-09-26T12:00:00Z"))
    }

    // ADR-0074 section 3.7: a person met after a rotation asks with their
    // newest key while the conversation is under the key their leaves are
    // bound to. Seen as two rows of one person on 2026-11-01.

    func testTheConversationKeyOfAPersonAddsNoSecondRow() {
        let next = ContactsRepository.adopting(
            [row(anna, groupIdHex: nil, displayName: "Anna W.")],
            signatureKey: bob, accountId: nil, groupIdHex: "abcd", nowMs: 5,
            createdAt: "2026-09-26T12:00:00Z", samePerson: { _, _ in true })
        XCTAssertEqual(next?.map(\.request.fromPubkey), [anna], "the row stays under the key it holds")
        XCTAssertEqual(next?.first?.groupIdHex, "abcd")
        XCTAssertEqual(next?.first?.displayName, "Anna W.")
        XCTAssertEqual(next?.first?.previousKeysB64, [base64Encode(bob)])
    }

    func testARowThatKnowsTheConversationKeyIsLeftAlone() {
        var held = row(anna, groupIdHex: "abcd")
        held.previousKeysB64 = [base64Encode(bob)]
        XCTAssertNil(ContactsRepository.adopting(
            [held], signatureKey: bob, accountId: nil, groupIdHex: "ffff", nowMs: 5,
            createdAt: "2026-09-26T12:00:00Z", samePerson: { _, _ in true }))
    }

    func testTwoKeysNothingProvedAreTwoPeople() {
        let next = ContactsRepository.adopting(
            [row(anna, groupIdHex: "abcd")], signatureKey: bob, accountId: nil, groupIdHex: "ffff",
            nowMs: 5, createdAt: "2026-09-26T12:00:00Z")
        XCTAssertEqual(next?.map(\.request.fromPubkey), [anna, bob])
    }

    func testOtherRowsSurviveUntouched() {
        let next = ContactsRepository.adopting(
            [row(bob, groupIdHex: "b0b0", displayName: "Bob")],
            signatureKey: anna, accountId: nil, groupIdHex: "abcd", nowMs: 5,
            createdAt: "2026-09-26T12:00:00Z")
        XCTAssertEqual(next?.map(\.request.fromPubkey), [bob, anna])
        XCTAssertEqual(next?.first?.displayName, "Bob")
    }

    func testSyntheticIdIsStableNegativeAndPerKey() {
        let a1 = ContactsRepository.syntheticContactMessageId(signatureKey: anna)
        let a2 = ContactsRepository.syntheticContactMessageId(signatureKey: anna)
        let b = ContactsRepository.syntheticContactMessageId(signatureKey: bob)
        XCTAssertEqual(a1, a2)
        XCTAssertLessThan(a1, 0)
        XCTAssertNotEqual(a1, b)
    }
}
