// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

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
