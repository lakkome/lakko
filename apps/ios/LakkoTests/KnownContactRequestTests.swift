// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0065 R.7 case 1 / UX 16.6 - a request from an account that is already
/// a contact. The cases are those of
/// `apps/web/src/contacts/known-contact-request.test.ts`; `RequestTarget` has
/// no web test, so its table is pinned row by row here.
final class KnownContactRequestTests: XCTestCase {

    private typealias Row = KnownContactRequest.ContactRow
    private typealias Verified = KnownContactRequest.Verified

    // MARK: - verdict

    private func verdict(
        senderIsContact: Bool = true,
        check: KnownContactRequest.Check = .current,
        requestKeyB64: String = "new",
        trustedKeyB64: String? = "new"
    ) -> KnownContactRequest.Verdict {
        KnownContactRequest.verdict(
            senderIsContact: senderIsContact, check: check,
            requestKeyB64: requestKeyB64, trustedKeyB64: trustedKeyB64)
    }

    func testAStrangersRequestIsTheOrdinaryRequestRow() {
        XCTAssertEqual(verdict(senderIsContact: false), .show)
        XCTAssertEqual(verdict(senderIsContact: false, check: .refused, trustedKeyB64: nil), .show)
    }

    func testAContactsRequestFromTheTrustedKeyIsAnsweredSilently() {
        XCTAssertEqual(verdict(), .answer)
    }

    func testARefusedChangeIsDropped() {
        XCTAssertEqual(verdict(check: .refused, trustedKeyB64: "old"), .drop)
    }

    func testNoVerdictYetItWaitsUnshown() {
        XCTAssertEqual(verdict(check: .inconclusive, trustedKeyB64: "old"), .wait)
    }

    func testARequestFromAnyOtherKeyOfAContactsAccountIsDropped() {
        XCTAssertEqual(verdict(requestKeyB64: "old"), .drop)
        XCTAssertEqual(verdict(trustedKeyB64: nil), .drop)
    }

    // MARK: - establishedContactOfAccount

    private let none = Verified(pinKeysB64: [], bindingKeyB64: nil)

    private func established(_ rows: [Row], _ accountIdHex: String?, _ verified: Verified) -> Row? {
        KnownContactRequest.establishedContactOfAccount(
            accepted: rows, accountIdHex: accountIdHex, verified: verified)
    }

    func testARowWhoseKeyThePinNamesIsAnEstablishedContact() {
        let rows = [Row(signaturePubkeyB64: "real", accountIdHex: "aabb")]
        XCTAssertEqual(established(rows, "AABB", Verified(pinKeysB64: ["real"], bindingKeyB64: nil)), rows[0])
    }

    func testARowWhoseKeyAVerifiedBindingNamesIsAnEstablishedContact() {
        let rows = [Row(signaturePubkeyB64: "real", accountIdHex: "aabb")]
        XCTAssertEqual(established(rows, "aabb", Verified(pinKeysB64: [], bindingKeyB64: "real")), rows[0])
    }

    func testARowWithNeitherIsNotTheAccountIdMayBeThePeersOwnClaim() {
        let rows = [Row(signaturePubkeyB64: "real", accountIdHex: "aabb")]
        XCTAssertNil(established(rows, "aabb", none))
    }

    func testIsNilForAnUnknownOrMissingId() {
        let rows = [Row(signaturePubkeyB64: "k", accountIdHex: "aabb")]
        let pinned = Verified(pinKeysB64: ["k"], bindingKeyB64: nil)
        XCTAssertNil(established(rows, "ccdd", pinned))
        XCTAssertNil(established(rows, nil, pinned))
        XCTAssertNil(established(rows, "", pinned))
    }

    func testARowWithNoAccountIdOrNoKeyEstablishesNothing() {
        let pinned = Verified(pinKeysB64: ["k", ""], bindingKeyB64: "")
        XCTAssertNil(established([Row(signaturePubkeyB64: "k", accountIdHex: nil)], "aabb", pinned))
        XCTAssertNil(established([Row(signaturePubkeyB64: "", accountIdHex: "aabb")], "aabb", pinned))
    }

    func testAMemberWhoClaimedAnotherAccountsIdDoesNotSwallowThatAccountsLaterRequest() {
        // The member's row carries the victim's account id from the member's
        // own profile. Nothing this device verified names the member's key
        // for it.
        let rows = [Row(signaturePubkeyB64: "member-key", accountIdHex: "aabb")]
        // No pin and no binding for the victim's account: a stranger.
        let before = established(rows, "aabb", none)
        XCTAssertNil(before)
        XCTAssertEqual(
            verdict(
                senderIsContact: before != nil, check: .current,
                requestKeyB64: "victim-key", trustedKeyB64: "victim-key"),
            .show)
        // The same when this device already holds the victim's real key (a
        // pin from any earlier fetch): it names the victim's key, not the
        // member's.
        XCTAssertNil(established(rows, "aabb", Verified(pinKeysB64: ["victim-key"], bindingKeyB64: nil)))
    }

    func testAfterAProvenRotationTheRowsKeptKeyIsStillNamedByThePin() {
        let rows = [Row(signaturePubkeyB64: "old", accountIdHex: "aabb")]
        let keys = KnownContactRequest.pinKeys(
            of: KnownContactRequest.PinKeys(currentKeyB64: "new", provenPredecessorsB64: ["old"]))
        XCTAssertEqual(keys, ["new", "old"])
        XCTAssertEqual(established(rows, "aabb", Verified(pinKeysB64: keys, bindingKeyB64: nil)), rows[0])
        XCTAssertEqual(KnownContactRequest.pinKeys(of: nil), [])
    }

    func testAClaimedRowDoesNotHideTheRealContactRowOfTheSameAccount() {
        let rows = [
            Row(signaturePubkeyB64: "member-key", accountIdHex: "aabb"),
            Row(signaturePubkeyB64: "victim-key", accountIdHex: "aabb"),
        ]
        XCTAssertEqual(
            established(rows, "aabb", Verified(pinKeysB64: ["victim-key"], bindingKeyB64: nil)), rows[1])
    }

    func testTheGenericFormHandsBackTheCallersOwnRow() {
        struct Contact: Equatable {
            let name: String
            let key: String
            let account: String?
        }
        let contacts = [
            Contact(name: "claimed", key: "member-key", account: "aabb"),
            Contact(name: "real", key: "victim-key", account: "AABB"),
        ]
        let found = KnownContactRequest.establishedContactOfAccount(
            accepted: contacts, accountIdHex: "aabb",
            verified: Verified(pinKeysB64: [], bindingKeyB64: "victim-key"),
            row: { Row(signaturePubkeyB64: $0.key, accountIdHex: $0.account) })
        XCTAssertEqual(found?.name, "real")
    }

    // MARK: - RequestTarget

    func testTheKeysInHandAreThePublishedOnes() {
        XCTAssertEqual(
            RequestTarget.afterFetch(suppliedKeyB64: "k", publishedKeyB64: "k", publishedIsProvenSuccessor: false),
            .asSupplied)
        XCTAssertEqual(
            RequestTarget.afterFetch(suppliedKeyB64: "k", publishedKeyB64: "k", publishedIsProvenSuccessor: true),
            .asSupplied)
    }

    func testAProvenPredecessorInHandGoesToTheCurrentKeys() {
        XCTAssertEqual(
            RequestTarget.afterFetch(suppliedKeyB64: "old", publishedKeyB64: "new", publishedIsProvenSuccessor: true),
            .current)
    }

    func testAnyOtherKeyInHandIsRefused() {
        XCTAssertEqual(
            RequestTarget.afterFetch(suppliedKeyB64: "old", publishedKeyB64: "new", publishedIsProvenSuccessor: false),
            .refuse)
    }
}
