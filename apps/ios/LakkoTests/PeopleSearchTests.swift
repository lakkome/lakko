// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0058 section 4 / UX section 6.1 — people search: introduction records
/// composed and opened through the real core, the network faked, and every
/// result reach-checked before it is shown.
@MainActor
final class PeopleSearchTests: XCTestCase {

    /// Stand-in for the server OPRF: 64 bytes, deterministic per input and per
    /// domain, which is all the record's tag / key derivation needs.
    private func fakeOprf(_ domain: String, _ s: String) -> Data {
        Data(SHA512.hash(data: Data((domain + "|" + s).utf8)))
    }

    private let annaKey = try! Identity.fromSeed(seed: Data(repeating: 0x51, count: 32)) // swiftlint:disable:this force_try
    private let annaAccount = Data(repeating: 0xA9, count: 16)

    private func annaRecord(name: String = "Anna Weber") throws -> (Data, FfiReachGrant) {
        let fof = try deriveReachGrant(
            accountSeed: annaKey.ed25519Seed(), class: .fof, scopeId: Data(), version: 1)
        let payload = FfiIntroductionPayload(
            identityPk: annaKey.signaturePublicKey(), accountId: annaAccount,
            displayName: name, username: nil, usernameCertificate: nil, fof: fof)
        let terms = nameSearchTerms(displayName: name).map { fakeOprf("name", $0) }
        return (try composeIntroduction(payload: payload, termOprfs: terms, issuedAtMs: 1), fof)
    }

    private func ports(
        reachCheck: @escaping (Data, Data) async throws -> Bool = { _, _ in true },
        lookupUsername: @escaping (String) async throws -> Data? = { _ in nil },
        identityKey: @escaping (Data) async throws -> Data? = { _ in nil },
        nameLookup: @escaping (Data) async throws -> [Data] = { _ in [] }
    ) -> PeopleSearchPorts {
        PeopleSearchPorts(
            lookupUsername: lookupUsername,
            usernameOprf: { self.fakeOprf("user", $0) },
            nameTermOprf: { self.fakeOprf("name", $0) },
            nameLookup: nameLookup,
            identityKey: identityKey,
            reachCheck: reachCheck,
            verifyCertificate: { _, _, _ in false })
    }

    /// `anna web` finds `Anna Weber` through a contact's relayed record, with
    /// the `fof` key the record carried as the way to reach her.
    func testAContactOfAContactIsFoundByWordPrefixes() async throws {
        let (record, fof) = try annaRecord()
        let outcome = await PeopleSearch.network(
            query: "anna web", records: [record], knownProfiles: [:], ports: ports())
        guard case .found(let people) = outcome else { return XCTFail("\(outcome)") }
        XCTAssertEqual(people.map(\.accountKey), [annaKey.signaturePublicKey()])
        XCTAssertEqual(people.first?.displayName, "Anna Weber")
        XCTAssertEqual(people.first?.writeSeed, fof.seed)
        XCTAssertNil(people.first?.username, "no verified username, none shown")
    }

    /// Several words narrow the result: `anna x` does not find `Anna Weber`.
    func testTheWholeQueryFiltersTheResult() async throws {
        let (record, _) = try annaRecord()
        let outcome = await PeopleSearch.network(
            query: "anna x", records: [record], knownProfiles: [:], ports: ports())
        XCTAssertEqual(outcome, .notFound)
    }

    /// Narrowing takes effect at the next search: a key the owner removed
    /// fails the reach check and the person is not shown.
    func testARefusedReachCheckHidesThePerson() async throws {
        let (record, _) = try annaRecord()
        let outcome = await PeopleSearch.network(
            query: "anna", records: [record], knownProfiles: [:],
            ports: ports(reachCheck: { _, _ in false }))
        XCTAssertEqual(outcome, .notFound)
    }

    /// A network failure is never "not found".
    func testANetworkFailureIsNotNotFound() async throws {
        let (record, _) = try annaRecord()
        let outcome = await PeopleSearch.network(
            query: "anna", records: [record], knownProfiles: [:],
            ports: ports(reachCheck: { _, _ in throw URLError(.notConnectedToInternet) }))
        XCTAssertEqual(outcome, .failed)
        XCTAssertEqual(
            PeopleSearch.message(for: .failed),
            "Couldn't search. Check your connection and try again.")
    }

    /// A person in Everyone answers the username lookup; nothing to sign with,
    /// their mailbox is open.
    func testAUsernameInEveryoneIsFoundUnsigned() async throws {
        let key = annaKey.signaturePublicKey()
        let outcome = await PeopleSearch.network(
            query: "@anna_w", records: [], knownProfiles: [:],
            ports: ports(
                lookupUsername: { $0 == "anna_w" ? self.annaAccount : nil },
                identityKey: { $0 == self.annaAccount ? key : nil }))
        guard case .found(let people) = outcome else { return XCTFail("\(outcome)") }
        XCTAssertEqual(people.first?.username, "anna_w")
        XCTAssertNil(people.first?.writeSeed)
    }

    /// Found wins over a partial failure; a used-up allowance is its own
    /// state; otherwise a failure is "couldn't search".
    func testTheOutcomeOrder() {
        let person = FoundPerson(
            accountKey: Data([1]), accountId: nil, displayName: "A", username: nil, writeSeed: nil)
        XCTAssertEqual(PeopleSearch.outcome(found: [person], allowanceUsed: true, failed: true),
                       .found([person]))
        XCTAssertEqual(PeopleSearch.outcome(found: [], allowanceUsed: true, failed: true), .allowanceUsed)
        XCTAssertEqual(PeopleSearch.outcome(found: [], allowanceUsed: false, failed: true), .failed)
        XCTAssertEqual(PeopleSearch.outcome(found: [], allowanceUsed: false, failed: false), .notFound)
    }

    /// Local matching is as loose as local search, and only people this device
    /// holds a grant for are offered.
    func testLocalSearchMatchesAnyPartOfAnyWord() {
        let key = Data(repeating: 3, count: 32).base64EncodedString()
        let other = Data(repeating: 4, count: 32).base64EncodedString()
        let profiles = [
            key: ProfileClaim(displayName: "Zoë Müller", username: nil, updatedAtMs: 1),
            other: ProfileClaim(displayName: "Zoe Mull", username: nil, updatedAtMs: 1),
        ]
        let found = PeopleSearch.local(
            query: "ull", profiles: profiles,
            seedFor: { $0 == key ? Data(repeating: 9, count: 32) : nil })
        XCTAssertEqual(found.map { $0.accountKey.base64EncodedString() }, [key])
        XCTAssertTrue(PeopleSearch.localMatches("Zoë Müller", query: "zoe"))
        XCTAssertFalse(PeopleSearch.offersNetworkSearch("a"))
        XCTAssertTrue(PeopleSearch.offersNetworkSearch("an"))
    }
}
