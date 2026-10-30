// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0060 section 4 / UX section 6.1 — people search: introduction records
/// composed and opened through the real core, the network faked, and every
/// result reach-checked before it is shown.
@MainActor
final class PeopleSearchTests: XCTestCase {

    /// Stand-in for the server OPRF: 64 bytes, deterministic per input and per
    /// domain, which is all the record's tag / key derivation needs.
    private func fakeOutput(_ domain: String, _ s: String) -> Data {
        Data(SHA512.hash(data: Data((domain + "|" + s).utf8)))
    }

    /// The pair `finalize` returns: the output and its real lookup key.
    private func fakeOprf(_ domain: String, _ s: String) -> OprfResult {
        let output = fakeOutput(domain, s)
        return OprfResult(output: output, lookupKey: try! discoveryLookupKey(output: output)) // swiftlint:disable:this force_try
    }

    private let annaKey = try! Identity.fromSeed(seed: Data(repeating: 0x51, count: 32)) // swiftlint:disable:this force_try
    private let annaAccount = Data(repeating: 0xA9, count: 16)

    private func annaRecord(name: String = "Anna Weber") throws -> (Data, FfiReachGrant) {
        let fof = try deriveReachGrant(
            accountSeed: annaKey.ed25519Seed(), class: .fof, scopeId: Data(), version: 1)
        let payload = FfiIntroductionPayload(
            identityPk: annaKey.signaturePublicKey(), accountId: annaAccount,
            displayName: name, username: nil, usernameCertificate: nil,
            mailboxBinding: signMailboxBinding(accountIdentity: annaKey), fof: fof)
        let terms = nameSearchTerms(displayName: name).map { fakeOutput("name", $0) }
        return (try composeIntroduction(payload: payload, termOprfs: terms, issuedAtMs: 1), fof)
    }

    /// Anna's public record (ADR-0060 section 5): no grant, sealed under the
    /// OUTPUTS of her name terms and username.
    private func annaPublicRecord(name: String = "Anna Weber", username: String? = nil) throws -> Data {
        let payload = FfiIntroductionPayload(
            identityPk: annaKey.signaturePublicKey(), accountId: annaAccount,
            displayName: name, username: nil, usernameCertificate: nil,
            mailboxBinding: signMailboxBinding(accountIdentity: annaKey), fof: nil)
        var terms = nameSearchTerms(displayName: name).map { fakeOutput("name", $0) }
        if let username { terms.append(fakeOutput("user", username)) }
        return try composePublicRecord(payload: payload, termOprfs: terms, issuedAtMs: 1)
    }

    private func ports(
        reachCheck: @escaping (MailboxTarget, Data) async throws -> Bool = { _, _ in true },
        lookupUsername: @escaping (Data) async throws -> PublicAccountHit? = { _ in nil },
        identityKey: ((Data) async throws -> Data?)? = nil,
        nameLookup: @escaping (Data) async throws -> [PublicAccountHit] = { _ in [] },
        usernameOprf: ((String) async throws -> OprfResult)? = nil,
        verifyCertificate: @escaping (Data, Data, Data) async -> Bool = { _, _, _ in false }
    ) -> PeopleSearchPorts {
        // By default the identity bundle serves Anna's key for Anna's account.
        let annaAccount = self.annaAccount
        let annaPk = annaKey.signaturePublicKey()
        return PeopleSearchPorts(
            lookupUsername: lookupUsername,
            usernameOprf: usernameOprf ?? { self.fakeOprf("user", $0) },
            nameTermOprf: { self.fakeOprf("name", $0) },
            nameLookup: nameLookup,
            identityKey: identityKey ?? { $0 == annaAccount ? annaPk : nil },
            reachCheck: reachCheck,
            verifyCertificate: verifyCertificate)
    }

    /// `anna web` finds `Anna Weber` through a contact's relayed record, with
    /// the `fof` key the record carried as the way to reach her.
    func testAContactOfAContactIsFoundByWordPrefixes() async throws {
        let (record, fof) = try annaRecord()
        let outcome = await PeopleSearch.network(
            query: "anna web", records: [record], ports: ports())
        guard case .found(let people) = outcome else { return XCTFail("\(outcome)") }
        XCTAssertEqual(people.map(\.accountKey), [annaKey.signaturePublicKey()])
        XCTAssertEqual(people.first?.displayName, "Anna Weber")
        XCTAssertEqual(people.first?.writeSeed, fof.seed)
        XCTAssertNil(people.first?.username, "no verified username, none shown")
        // ADR-0078 — the request goes to the mailbox the record's binding
        // names, verified against Anna's key.
        XCTAssertEqual(
            people.first?.mailbox?.address,
            MailboxKey.fromAccountIdentity(accountIdentity: annaKey).address())
    }

    /// ADR-0078 — the reach check asks at the mailbox the record's verified
    /// binding names, never at one derived from the identity key.
    func testTheReachCheckAsksAtTheVerifiedMailbox() async throws {
        let (record, fof) = try annaRecord()
        var asked: [(Data, Data)] = []
        _ = await PeopleSearch.network(
            query: "anna", records: [record],
            ports: ports(reachCheck: { target, seed in asked.append((target.address, seed)); return true }))
        XCTAssertEqual(asked.map(\.0), [MailboxKey.fromAccountIdentity(accountIdentity: annaKey).address()])
        XCTAssertEqual(asked.map(\.1), [fof.seed])
    }

    /// Several words narrow the result: `anna x` does not find `Anna Weber`.
    func testTheWholeQueryFiltersTheResult() async throws {
        let (record, _) = try annaRecord()
        let outcome = await PeopleSearch.network(
            query: "anna x", records: [record], ports: ports())
        XCTAssertEqual(outcome, .notFound)
    }

    /// Narrowing takes effect at the next search: a key the owner removed
    /// fails the reach check and the person is not shown.
    func testARefusedReachCheckHidesThePerson() async throws {
        let (record, _) = try annaRecord()
        let outcome = await PeopleSearch.network(
            query: "anna", records: [record],
            ports: ports(reachCheck: { _, _ in false }))
        XCTAssertEqual(outcome, .notFound)
    }

    /// A network failure is never "not found".
    func testANetworkFailureIsNotNotFound() async throws {
        let (record, _) = try annaRecord()
        let outcome = await PeopleSearch.network(
            query: "anna", records: [record],
            ports: ports(reachCheck: { _, _ in throw URLError(.notConnectedToInternet) }))
        XCTAssertEqual(outcome, .failed)
        XCTAssertEqual(
            PeopleSearch.message(for: .failed),
            Localization.text("search.people.failed"))
    }

    /// A person in Everyone answers the username lookup; nothing to sign with,
    /// their mailbox is open.
    func testAUsernameInEveryoneIsFoundUnsigned() async throws {
        let key = annaKey.signaturePublicKey()
        let outcome = await PeopleSearch.network(
            query: "@anna_w", records: [],
            ports: ports(
                lookupUsername: {
                    $0 == self.fakeOprf("user", "anna_w").lookupKey
                        ? PublicAccountHit(accountId: self.annaAccount, publicRecord: nil) : nil
                },
                identityKey: { $0 == self.annaAccount ? key : nil }))
        guard case .found(let people) = outcome else { return XCTFail("\(outcome)") }
        XCTAssertEqual(people.first?.username, "anna_w")
        XCTAssertNil(people.first?.writeSeed)
    }

    /// ADR-0060 section 5 — discovery is asked with the LOOKUP KEY only; the
    /// OPRF output never reaches a network port.
    func testOnlyLookupKeysReachDiscovery() async throws {
        var sent: [Data] = []
        _ = await PeopleSearch.network(
            query: "anna_w", records: [],
            ports: ports(
                lookupUsername: { sent.append($0); return nil },
                nameLookup: { sent.append($0); return [] }))
        let user = fakeOprf("user", "anna_w")
        let term = fakeOprf("name", nameQueryTerm(query: "anna_w") ?? "")
        XCTAssertEqual(sent, [user.lookupKey, term.lookupKey])
        XCTAssertFalse(sent.contains(user.output))
        XCTAssertFalse(sent.contains(term.output))
        XCTAssertNotEqual(user.lookupKey, user.output)
    }

    /// A person in Everyone this device has never seen is named from the
    /// public record served beside the name hit, opened with the term's
    /// OUTPUT; the whole query still filters it.
    func testANameHitIsNamedFromItsPublicRecord() async throws {
        let record = try annaPublicRecord()
        let hit = PublicAccountHit(accountId: annaAccount, publicRecord: record)
        let key = annaKey.signaturePublicKey()
        let bundle: (Data) async throws -> Data? = { $0 == self.annaAccount ? key : nil }
        let outcome = await PeopleSearch.network(
            query: "anna web", records: [],
            ports: ports(identityKey: bundle, nameLookup: { _ in [hit] }))
        guard case .found(let people) = outcome else { return XCTFail("\(outcome)") }
        XCTAssertEqual(people.map(\.accountKey), [key])
        XCTAssertEqual(people.first?.displayName, "Anna Weber")
        XCTAssertNil(people.first?.writeSeed, "a public person's mailbox is open")

        let narrowed = await PeopleSearch.network(
            query: "anna x", records: [],
            ports: ports(identityKey: bundle, nameLookup: { _ in [hit] }))
        XCTAssertEqual(narrowed, .notFound)
    }

    /// A public record naming the right account but another identity key than
    /// the one the identity bundle serves is not trusted: the hit is not
    /// named from it (the rule web applies).
    func testAPublicRecordWithAnotherIdentityKeyIsIgnored() async throws {
        let record = try annaPublicRecord()
        let hit = PublicAccountHit(accountId: annaAccount, publicRecord: record)
        let other = try Identity.fromSeed(seed: Data(repeating: 0x52, count: 32)).signaturePublicKey()
        let outcome = await PeopleSearch.network(
            query: "anna", records: [],
            ports: ports(identityKey: { _ in other }, nameLookup: { _ in [hit] }))
        XCTAssertEqual(outcome, .notFound)
    }

    /// A name hit whose account has no verified identity key is dropped, even
    /// when its public record opens.
    func testANameHitWithoutAnIdentityKeyIsDropped() async throws {
        let hit = PublicAccountHit(accountId: annaAccount, publicRecord: try annaPublicRecord())
        let outcome = await PeopleSearch.network(
            query: "anna", records: [],
            ports: ports(identityKey: { _ in nil }, nameLookup: { _ in [hit] }))
        XCTAssertEqual(outcome, .notFound)
    }

    /// A profile this device holds for the served key names the hit.
    func testANameHitIsNamedFromAProfileHeldForItsKey() async throws {
        let key = annaKey.signaturePublicKey()
        let hit = PublicAccountHit(accountId: annaAccount, publicRecord: nil)
        let outcome = await PeopleSearch.network(
            query: "anna", records: [],
            known: { $0 == key
                ? KnownPerson(name: "Anna W", verifiedUsername: nil, isContact: false)
                : .unknown },
            ports: ports(identityKey: { _ in key }, nameLookup: { _ in [hit] }))
        guard case .found(let people) = outcome else { return XCTFail("\(outcome)") }
        XCTAssertEqual(people.map(\.accountKey), [key])
        XCTAssertEqual(people.first?.displayName, "Anna W")
    }

    /// A contact this device holds no name for (only a fingerprint) is found
    /// by a name search under the name they published, and stays a contact;
    /// the row carries nothing about why they were found.
    func testAPublishedNameBeatsAContactsFingerprint() async throws {
        let record = try annaPublicRecord()
        let hit = PublicAccountHit(accountId: annaAccount, publicRecord: record)
        let key = annaKey.signaturePublicKey()
        let fingerprint = displayFingerprint(key)
        let outcome = await PeopleSearch.network(
            query: "anna", records: [],
            known: { $0 == key
                ? KnownPerson(name: nil, fallbackName: fingerprint, verifiedUsername: nil,
                              isContact: true)
                : .unknown },
            ports: ports(identityKey: { _ in key }, nameLookup: { _ in [hit] }))
        guard case .found(let people) = outcome else { return XCTFail("\(outcome)") }
        XCTAssertEqual(people.map(\.accountKey), [key])
        XCTAssertEqual(people.first?.displayName, "Anna Weber")
        XCTAssertEqual(people.first?.reach, .contact)
        XCTAssertNil(people.first?.writeSeed)
    }

    /// With neither a held nor a published name, a contact still shows, by
    /// fingerprint — never dropped, never named "Member".
    func testAContactWithNothingElseKnownShowsByFingerprint() {
        let key = annaKey.signaturePublicKey()
        let fingerprint = displayFingerprint(key)
        let known = KnownPerson(
            name: nil, fallbackName: fingerprint, verifiedUsername: nil, isContact: true)

        let found = PeopleSearch.result(
            key: key, accountId: annaAccount, known: known, foundReach: .open,
            foundName: nil, foundUsername: nil, seed: nil)
        XCTAssertEqual(found?.displayName, fingerprint)
        XCTAssertEqual(found?.reach, .contact)

        let local = PeopleSearch.local(
            query: String(fingerprint.prefix(4)),
            candidates: [LocalCandidate(accountKey: key, accountId: annaAccount, known: known)],
            ownAccountKey: nil)
        XCTAssertEqual(local.map(\.displayName), [fingerprint])

        // A held name still wins over a found one.
        let held = KnownPerson(
            name: "Anna W", fallbackName: nil, verifiedUsername: nil, isContact: true)
        XCTAssertEqual(PeopleSearch.result(
            key: key, accountId: annaAccount, known: held, foundReach: .open,
            foundName: "Anna Weber", foundUsername: nil, seed: nil)?.displayName, "Anna W")
    }

    /// The fallback a contact gets from the name resolver: a verified
    /// username when there is one, else the fingerprint, never the neutral
    /// placeholder.
    func testAContactsFallbackNameIsItsUsernameElseItsFingerprint() {
        let key = annaKey.signaturePublicKey()
        let nothing = resolveDisplayName(signatureKey: key, localRename: nil, profile: nil, verification: nil)
        XCTAssertEqual(nothing.source, .placeholder)
        XCTAssertEqual(PeopleSearchWiring.contactFallbackName(nothing), displayFingerprint(key))

        let withUsername = resolveDisplayName(
            signatureKey: key, localRename: nil,
            profile: ProfileClaim(displayName: nil, username: "anna_w", updatedAtMs: 1),
            verification: .verified)
        XCTAssertEqual(PeopleSearchWiring.contactFallbackName(withUsername), "@anna_w")
    }

    /// A public record served beside another account id is not trusted.
    func testAPublicRecordForAnotherAccountIsIgnored() async throws {
        let record = try annaPublicRecord()
        let hit = PublicAccountHit(accountId: Data(repeating: 0x01, count: 16), publicRecord: record)
        let key = annaKey.signaturePublicKey()
        let outcome = await PeopleSearch.network(
            query: "anna", records: [],
            ports: ports(identityKey: { _ in key }, nameLookup: { _ in [hit] }))
        XCTAssertEqual(outcome, .notFound)
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

    // MARK: - Local half (web `people.ts`)

    private func kept(_ seed: Data) -> KeptGrant {
        KeptGrant(seed: seed, mailbox: MailboxTarget(
            mailboxPk: seed, address: seed, binding: Data(count: 97), accountKey: Data(count: 32)))
    }

    private func candidate(_ seed: UInt8, _ known: KnownPerson, accountId: Data? = nil) -> LocalCandidate {
        LocalCandidate(accountKey: Data(repeating: seed, count: 32), accountId: accountId, known: known)
    }

    /// Every word of the query is a part of some word of the name; case and
    /// accents never matter.
    func testLocalMatchingIsPerWord() {
        XCTAssertTrue(PeopleSearch.localMatches("Anna Weber", query: "ann"))
        XCTAssertTrue(PeopleSearch.localMatches("Anna Weber", query: "nna"))
        XCTAssertTrue(PeopleSearch.localMatches("Anna Weber", query: "weber an"))
        XCTAssertFalse(PeopleSearch.localMatches("Anna Weber", query: "anna x"))
        XCTAssertFalse(PeopleSearch.localMatches("Anna Weber", query: "annaweber"),
                       "a query word matches within one name word, not across a space")
        XCTAssertTrue(PeopleSearch.localMatches("Zoë Müller", query: "zoe"))
    }

    /// Contacts and known members are listed even without a grant: a known
    /// member without one is shown by name only (reach `none`, no username,
    /// no Add contact); someone with a kept grant can be added; a contact has
    /// nothing to add; a candidate with no name is never listed.
    func testLocalListsKnownPeopleWithAndWithoutAGrant() {
        let seed = Data(repeating: 9, count: 32)
        let account = Data(repeating: 0x0A, count: 16)
        let found = PeopleSearch.local(
            query: "ull",
            candidates: [
                candidate(1, KnownPerson(name: "Zoë Müller", verifiedUsername: "zoe_m",
                                         isContact: false), accountId: account),
                candidate(2, KnownPerson(name: "Zoe Mull", verifiedUsername: "zoe2",
                                         isContact: false, keptGrants: [kept(seed)]), accountId: account),
                candidate(3, KnownPerson(name: "Paul Mullins", verifiedUsername: nil,
                                         isContact: true), accountId: account),
                candidate(4, KnownPerson(name: nil, verifiedUsername: "ullr",
                                         isContact: false, keptGrants: [kept(seed)])),
            ],
            ownAccountKey: nil)
        XCTAssertEqual(found.map(\.reach), [.none, .grant, .contact])
        XCTAssertNil(found[0].username, "a hidden person's username never shows")
        XCTAssertFalse(found[0].canAddContact)
        XCTAssertNil(found[0].writeSeed)
        XCTAssertEqual(found[1].username, "zoe2")
        XCTAssertTrue(found[1].canAddContact)
        XCTAssertEqual(found[1].writeSeed, seed)
        XCTAssertFalse(found[2].canAddContact, "a contact has nothing to add")
    }

    /// A verified username matches too, but only for someone this account
    /// may find; the own account never appears.
    func testLocalMatchesAUsernameOnlyWhereItMayShow() {
        let seed = Data(repeating: 9, count: 32)
        let own = Data(repeating: 7, count: 32)
        let found = PeopleSearch.local(
            query: "@anna_w",
            candidates: [
                candidate(1, KnownPerson(name: "A", verifiedUsername: "anna_w", isContact: false)),
                candidate(2, KnownPerson(name: "B", verifiedUsername: "anna_w", isContact: false, keptGrants: [kept(seed)])),
                LocalCandidate(accountKey: own, accountId: nil, known: KnownPerson(
                    name: "Me", verifiedUsername: "anna_w", isContact: true)),
            ],
            ownAccountKey: own)
        XCTAssertEqual(found.map(\.accountKey), [Data(repeating: 2, count: 32)])
    }

    /// Merging keeps the strongest reach and the first list's name, and gates
    /// the username by the merged reach.
    func testMergeKeepsTheStrongestReachAndTheLocalName() {
        let key = Data(repeating: 5, count: 32)
        let local = FoundPerson(accountKey: key, accountId: nil, displayName: "Renamed",
                                username: nil, writeSeed: nil, reach: PersonReach.none)
        let network = FoundPerson(accountKey: key, accountId: annaAccount, displayName: "Published",
                                  username: "anna_w", writeSeed: nil, reach: .open)
        let merged = PeopleSearch.merge([[local], [network]], ownAccountKey: nil)
        XCTAssertEqual(merged.count, 1)
        XCTAssertEqual(merged[0].displayName, "Renamed")
        XCTAssertEqual(merged[0].reach, .open)
        XCTAssertEqual(merged[0].username, "anna_w")
        XCTAssertEqual(merged[0].accountId, annaAccount)
    }

    /// With equal reach, the grant that passed a check wins, whichever list
    /// carries it: a kept grant no check spoke for never hides a live one.
    func testMergePrefersTheGrantThatPassedACheck() {
        let key = Data(repeating: 5, count: 32)
        let staleSeed = Data(repeating: 1, count: 32)
        let liveSeed = Data(repeating: 2, count: 32)
        let local = FoundPerson(
            accountKey: key, accountId: nil, displayName: "Renamed", username: nil,
            writeSeed: staleSeed, reach: .grant, mailbox: kept(staleSeed).mailbox)
        let network = FoundPerson(
            accountKey: key, accountId: annaAccount, displayName: "Published", username: "anna_w",
            writeSeed: liveSeed, reach: .grant, mailbox: kept(liveSeed).mailbox
        ).passingReachCheck()

        let merged = PeopleSearch.merge([[local], [network]], ownAccountKey: nil)
        XCTAssertEqual(merged.first?.writeSeed, liveSeed)
        XCTAssertEqual(merged.first?.mailbox, kept(liveSeed).mailbox)
        XCTAssertEqual(merged.first?.displayName, "Renamed", "the first list's name is kept")
        XCTAssertEqual(merged.first?.reachChecked, true)

        let bothChecked = PeopleSearch.merge(
            [[local.passingReachCheck()], [network]], ownAccountKey: nil)
        XCTAssertEqual(bothChecked.first?.writeSeed, staleSeed, "both passed: the first list stays")
    }

    /// A network result that passed the reach check says so.
    func testANetworkGrantThatPassedIsMarkedChecked() async throws {
        let (record, _) = try annaRecord()
        let outcome = await PeopleSearch.network(
            query: "anna w", records: [record], ports: ports())
        guard case .found(let people) = outcome else { return XCTFail("\(outcome)") }
        XCTAssertEqual(people.first?.reachChecked, true)
    }

    // MARK: - Verdicts over kept grants (UX section 6.1)

    func testAStaleGrantThenALiveOneGivesTheLiveOne() async {
        let stale = kept(Data(repeating: 1, count: 32))
        let live = kept(Data(repeating: 2, count: 32))
        let never = kept(Data(repeating: 3, count: 32))
        var asked: [Data] = []
        let verdict = await PeopleSearch.reachVerdict(grants: [stale, live, never]) { _, seed in
            asked.append(seed)
            return seed == live.seed
        }
        XCTAssertEqual(verdict, .live(live))
        XCTAssertEqual(asked, [stale.seed, live.seed], "it stops at the first that passes")
    }

    func testEveryGrantRefusedIsDead() async {
        let grants = [kept(Data(repeating: 1, count: 32)), kept(Data(repeating: 2, count: 32))]
        let verdict = await PeopleSearch.reachVerdict(grants: grants) { _, _ in false }
        XCTAssertEqual(verdict, .dead(checkedSeeds: Set(grants.map(\.seed))))
    }

    func testACheckThatThrowsIsNoVerdict() async {
        let grants = [kept(Data(repeating: 1, count: 32)), kept(Data(repeating: 2, count: 32))]
        let verdict = await PeopleSearch.reachVerdict(grants: grants) { _, _ in
            throw URLError(.notConnectedToInternet)
        }
        XCTAssertNil(verdict)
    }

    /// A grant received after the verdict is a new question.
    func testAVerdictDoesNotCoverAGrantReceivedSince() {
        let old = kept(Data(repeating: 1, count: 32))
        let new = kept(Data(repeating: 2, count: 32))
        XCTAssertTrue(ReachVerdict.dead(checkedSeeds: [old.seed]).covers([old]))
        XCTAssertFalse(ReachVerdict.dead(checkedSeeds: [old.seed]).covers([old, new]))
        XCTAssertTrue(ReachVerdict.live(old).covers([old, new]))
        XCTAssertFalse(ReachVerdict.live(old).covers([new]))
    }

    /// The candidate's verdict decides what the existing match sees.
    func testVerdictsAreAppliedToTheCandidatesBeforeTheMatch() {
        let stale = kept(Data(repeating: 1, count: 32))
        let live = kept(Data(repeating: 2, count: 32))
        let dead = candidate(1, KnownPerson(
            name: "Zoe", verifiedUsername: "zoe_d", isContact: false, keptGrants: [stale]))
        let alive = candidate(2, KnownPerson(
            name: "Zoe", verifiedUsername: "zoe_l", isContact: false, keptGrants: [stale, live]))
        let waiting = candidate(3, KnownPerson(
            name: "Zoe", verifiedUsername: "zoe_w", isContact: false, keptGrants: [stale]))
        let unanswered = candidate(4, KnownPerson(
            name: "Zoe", verifiedUsername: "zoe_u", isContact: false, keptGrants: [stale]))
        let contact = candidate(5, KnownPerson(
            name: "Zoe", verifiedUsername: "zoe_c", isContact: true, keptGrants: [stale]))
        let applied = PeopleSearch.applying(
            verdicts: [
                dead.accountKey: .dead(checkedSeeds: [stale.seed]),
                alive.accountKey: .live(live),
            ],
            unanswered: [unanswered.accountKey],
            to: [dead, alive, waiting, unanswered, contact])

        let found = PeopleSearch.local(query: "zoe", candidates: applied, ownAccountKey: nil)
        XCTAssertEqual(found.map(\.reach), [.none, .grant, .none, .grant, .contact])
        XCTAssertEqual(found.map(\.username), [nil, "zoe_l", nil, "zoe_u", "zoe_c"])
        XCTAssertEqual(found.map(\.canAddContact), [false, false, false, false, false],
                       "no account id in this fixture")
        XCTAssertEqual(found[1].writeSeed, live.seed)
        XCTAssertTrue(found[1].reachChecked)
        XCTAssertEqual(found[3].writeSeed, stale.seed, "a failed check behaves as before")
        XCTAssertFalse(found[3].reachChecked)

        XCTAssertEqual(
            PeopleSearch.local(query: "zoe_d", candidates: applied, ownAccountKey: nil).map(\.accountKey),
            [], "a dead person is matched by name only")
        XCTAssertEqual(
            PeopleSearch.local(query: "zoe_w", candidates: applied, ownAccountKey: nil).map(\.accountKey),
            [waiting.accountKey], "a waiting one still matches, as its name alone")
    }

    func testOnlyAMatchingCandidateWithGrantsAndNoVerdictIsChecked() {
        let grant = kept(Data(repeating: 1, count: 32))
        let own = Data(repeating: 9, count: 32)
        let ask = candidate(1, KnownPerson(
            name: "Anna", verifiedUsername: "anna_w", isContact: false, keptGrants: [grant]))
        let byUsername = candidate(2, KnownPerson(
            name: "Zed", verifiedUsername: "anna_z", isContact: false, keptGrants: [grant]))
        let other = candidate(3, KnownPerson(
            name: "Bob", verifiedUsername: nil, isContact: false, keptGrants: [grant]))
        let noGrant = candidate(4, KnownPerson(name: "Anna", verifiedUsername: nil, isContact: false))
        let contact = candidate(5, KnownPerson(
            name: "Anna", verifiedUsername: nil, isContact: true, keptGrants: [grant]))
        let answered = candidate(6, KnownPerson(
            name: "Anna", verifiedUsername: nil, isContact: false, keptGrants: [grant]))
        let me = LocalCandidate(accountKey: own, accountId: nil, known: KnownPerson(
            name: "Anna", verifiedUsername: nil, isContact: false, keptGrants: [grant]))
        let needed = PeopleSearch.needingReachCheck(
            query: "ann",
            candidates: [ask, byUsername, other, noGrant, contact, answered, me],
            verdicts: [answered.accountKey: .live(grant)],
            ownAccountKey: own)
        XCTAssertEqual(needed.map(\.accountKey), [ask.accountKey, byUsername.accountKey])
    }

    func testARefusalKeepsItsTextWhenTheButtonIsGone() {
        XCTAssertEqual(PersonRequestState.refused.message(canAddContact: false),
                       Localization.text("search.people.request_refused"))
        XCTAssertEqual(PersonRequestState.failed.message(canAddContact: true),
                       Localization.text("search.people.request_failed"))
        XCTAssertNil(PersonRequestState.failed.message(canAddContact: false))
    }

    // MARK: - Contacts of contacts (web `people-search.ts`)

    /// An opened introduction is dropped unless the identity bundle fetched
    /// for its account id names its identity key.
    func testAnIntroductionWhoseBundleNamesAnotherKeyIsDropped() async throws {
        let (record, _) = try annaRecord()
        let other = try Identity.fromSeed(seed: Data(repeating: 0x52, count: 32)).signaturePublicKey()
        let outcome = await PeopleSearch.network(
            query: "anna web", records: [record],
            ports: ports(identityKey: { _ in other }))
        XCTAssertEqual(outcome, .notFound)
        let unpublished = await PeopleSearch.network(
            query: "anna web", records: [record],
            ports: ports(identityKey: { _ in nil }))
        XCTAssertEqual(unpublished, .notFound)
    }

    /// A bundle lookup that fails is a failed search, never "not found".
    func testAnIntroductionBundleLookupFailureIsAFailure() async throws {
        let (record, _) = try annaRecord()
        let outcome = await PeopleSearch.network(
            query: "anna web", records: [record],
            ports: ports(identityKey: { _ in throw URLError(.notConnectedToInternet) }))
        XCTAssertEqual(outcome, .failed)
    }

    /// Anna's introduction record naming a username, with a certificate.
    private func annaRecordWithUsername(name: String, username: String) throws -> Data {
        let fof = try deriveReachGrant(
            accountSeed: annaKey.ed25519Seed(), class: .fof, scopeId: Data(), version: 1)
        let payload = FfiIntroductionPayload(
            identityPk: annaKey.signaturePublicKey(), accountId: annaAccount,
            displayName: name, username: username, usernameCertificate: Data(repeating: 1, count: 153),
            mailboxBinding: signMailboxBinding(accountIdentity: annaKey), fof: fof)
        var terms = nameSearchTerms(displayName: name).map { fakeOutput("name", $0) }
        terms.append(fakeOutput("user", username))
        return try composeIntroduction(payload: payload, termOprfs: terms, issuedAtMs: 1)
    }

    /// A name-matched introduction's username is verified with an extra OPRF
    /// lookup of that username, against its lookup key.
    func testANameMatchedIntroductionsUsernameIsVerifiedByAnExtraLookup() async throws {
        let record = try annaRecordWithUsername(name: "Anna Weber", username: "anna_w")
        var looked: [String] = []
        let outcome = await PeopleSearch.network(
            query: "weber", records: [record],
            ports: ports(
                usernameOprf: { looked.append($0); return self.fakeOprf("user", $0) },
                verifyCertificate: { _, lookupKey, _ in lookupKey == self.fakeOprf("user", "anna_w").lookupKey }))
        guard case .found(let people) = outcome else { return XCTFail("\(outcome)") }
        XCTAssertEqual(people.first?.username, "anna_w")
        XCTAssertTrue(looked.contains("anna_w"))
    }

    /// Extra username checks are capped at `maxExtraUsernameChecks` per search;
    /// the rest show their name without a username.
    func testExtraUsernameChecksAreCapped() async throws {
        var records: [Data] = []
        let n = PeopleSearch.maxExtraUsernameChecks + 2
        for i in 0..<n {
            records.append(try annaRecordWithUsername(name: "Anna Weber", username: "anna_\(i)"))
        }
        var looked: [String] = []
        _ = await PeopleSearch.network(
            query: "weber", records: records,
            ports: ports(
                usernameOprf: { looked.append($0); return self.fakeOprf("user", $0) },
                verifyCertificate: { _, _, _ in true }))
        // The typed text "weber" is itself a username candidate: one round for
        // it, then at most the cap for the records.
        XCTAssertEqual(looked.filter { $0.hasPrefix("anna_") }.count, PeopleSearch.maxExtraUsernameChecks)
    }
}
