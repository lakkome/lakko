// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0060 — the pure rules of "who can find me" on iOS (`Reach/ReachRules.swift`)
/// and the ``ReachCenter`` decisions built on them, against the real core
/// derivation and setting predicates.
@MainActor
final class ReachRulesTests: XCTestCase {

    /// The peer every grant in this file is derived from: its account seed is
    /// the Ed25519 seed of `peerIdentity`, so a grant's mailbox binding
    /// (ADR-0078) verifies under `peerIdentity`'s key.
    private let peerIdentity = try! Identity.fromSeed(seed: Data(repeating: 0x41, count: 32))
    private var accountSeed: Data { peerIdentity.ed25519Seed() }
    private let groupA = Data(repeating: 0xA1, count: 32)
    private let groupB = Data(repeating: 0xB2, count: 32)
    private let emptyState = FfiReachState(entries: [], updatedAtMs: 0)

    private func setting(
        mode: FfiVisibilityMode, fof: Bool = false, groups: FfiGroupsForm? = nil,
        chosen: [FfiScopeId] = []
    ) -> FfiVisibilitySetting {
        FfiVisibilitySetting(
            mode: mode, contactsOfContacts: fof, contactsForm: .all, chosenContacts: [],
            groups: groups, chosen: chosen, lastContactsOfContacts: false, lastGroups: nil,
            updatedAtMs: 1)
    }

    private func contactsSetting(
        mode: FfiVisibilityMode = .some, form: FfiContactsForm = .all, chosen: [Data] = []
    ) -> FfiVisibilitySetting {
        var s = setting(mode: mode, fof: true)
        s.contactsForm = form
        s.chosenContacts = chosen
        return s
    }

    private func narrows(_ old: FfiVisibilitySetting, _ new: FfiVisibilitySetting) -> Bool {
        ReachRules.narrowsContacts(from: old, to: new, allowsFof: { allowsContactsOfContacts(setting: $0) })
    }

    func testMovingToOnlyTheOnesIChooseNarrows() {
        let a = Data(repeating: 1, count: 16)
        XCTAssertTrue(narrows(contactsSetting(), contactsSetting(form: .chosen, chosen: [a])))
    }

    func testSwitchingAContactOffNarrowsAndSwitchingOneOnDoesNot() {
        let a = Data(repeating: 1, count: 16)
        let b = Data(repeating: 2, count: 16)
        let both = contactsSetting(form: .chosen, chosen: [a, b])
        let one = contactsSetting(form: .chosen, chosen: [a])
        XCTAssertTrue(narrows(both, one))
        XCTAssertFalse(narrows(one, both), "widening needs no bump")
        XCTAssertFalse(narrows(one, one))
    }

    func testExcludingAContactUnderAllNarrows() throws {
        let a = Data(repeating: 1, count: 16)
        let b = Data(repeating: 2, count: 16)
        let next = try excludeContactUnderAll(
            setting: contactsSetting(), allContactIds: [a, b], excluded: b)
        XCTAssertEqual(next.contactsForm, .chosen)
        XCTAssertEqual(next.chosenContacts, [a])
        XCTAssertTrue(narrows(contactsSetting(), next))
    }

    func testChoosingAllAndTurningThePartOffDoNotMoveTheKey() {
        let a = Data(repeating: 1, count: 16)
        XCTAssertFalse(narrows(contactsSetting(form: .chosen, chosen: [a]), contactsSetting()))
        XCTAssertFalse(narrows(contactsSetting(), setting(mode: .some, fof: false)))
    }

    func testTheOfferGoesOnlyToAllowedContacts() {
        let a = Data(repeating: 1, count: 16)
        let b = Data(repeating: 2, count: 16)
        func offers(_ id: Data?, _ s: FfiVisibilitySetting) -> Bool {
            ReachRules.offersRecord(to: id, setting: s, allows: { allowsContact(setting: $0, accountId: $1) })
        }
        let chosen = contactsSetting(form: .chosen, chosen: [a])
        XCTAssertTrue(offers(a, chosen))
        XCTAssertFalse(offers(b, chosen))
        XCTAssertFalse(offers(nil, chosen), "no known account id gets none under Chosen")
        XCTAssertTrue(offers(b, contactsSetting()))
        XCTAssertTrue(offers(nil, contactsSetting()), "under All every contact does")
        XCTAssertFalse(offers(nil, setting(mode: .nobody)))
        XCTAssertFalse(offers(a, setting(mode: .some, fof: false)))
    }

    private func desired(_ setting: FfiVisibilitySetting, links: [LinkReach] = [],
                         replies: [ReplyReach] = [], now: UInt64 = 1_000) -> [ReachKeySpec] {
        ReachRules.desiredKeys(
            setting: setting, state: emptyState, groupIds: [groupA, groupB], links: links,
            replies: replies, nowMs: now,
            allowsScope: { allowsScope(setting: $0, scope: $1) },
            allowsFof: { allowsContactsOfContacts(setting: $0) })
    }

    private func classes(_ specs: [ReachKeySpec]) -> [String] {
        specs.map { ReachRules.className($0.reachClass) + ($0.scopeId.first.map { ":\($0)" } ?? "") }
    }

    // MARK: - Which keys are registered

    /// Nobody: still `contact` (contacts always write) and the one `member`
    /// key while in any group (a co-member must always deliver a Welcome for
    /// a new device), never `group` or `fof`.
    func testNobodyRegistersContactAndMemberKeysOnly() {
        let specs = desired(setting(mode: .nobody))
        XCTAssertEqual(classes(specs), ["contact", "member"])
        XCTAssertFalse(ReachRules.mailboxOpen(setting(mode: .nobody)))
    }

    /// ADR-0078 section 9 — the default for a new account (2.1 + 2.2.1): ONE
    /// `group` and ONE `member` key for every group, never one per group.
    func testTheDefaultRegistersOneGroupAndOneMemberKey() {
        let specs = desired(defaultVisibilitySetting())
        XCTAssertEqual(Set(classes(specs)), ["contact", "member", "group", "fof"])
        XCTAssertEqual(specs.count, 4, "no key per group")
        XCTAssertTrue(specs.allSatisfy { $0.scopeId.isEmpty }, "no group id names a key")
    }

    /// Only the ones I choose: the one `group` key while any group is chosen,
    /// none while none is.
    func testChosenRegistersTheGroupKeyWhileAnyGroupIsChosen() {
        let specs = desired(setting(
            mode: .some, groups: .chosen, chosen: [FfiScopeId(kind: .group, id: groupB)]))
        XCTAssertTrue(classes(specs).contains("group"))
        XCTAssertFalse(classes(specs).contains("fof"))
        let none = desired(setting(mode: .some, groups: .chosen, chosen: []))
        XCTAssertFalse(classes(none).contains("group"))
    }

    /// The one `channel` key while any channel the account is in is allowed.
    func testOneChannelKeyWhileAnyChannelIsAllowed() {
        let channel = Data(repeating: 0xC1, count: 32)
        let specs = ReachRules.desiredKeys(
            setting: defaultVisibilitySetting(), state: emptyState, groupIds: [], links: [],
            replies: [], channelIds: [channel, Data(repeating: 0xC2, count: 32)], nowMs: 1,
            allowsScope: { allowsScope(setting: $0, scope: $1) },
            allowsFof: { allowsContactsOfContacts(setting: $0) })
        XCTAssertEqual(specs.filter { $0.reachClass == .channel }.map(\.scopeId), [Data()])
        XCTAssertFalse(classes(specs).contains("member"), "no group, no member key")
    }

    /// ADR-0078 section 9 — leaving a group (or a channel) rotates that
    /// class; a first registration, a join and an empty (not yet loaded)
    /// list rotate nothing.
    func testDeparturesAreASetDifferenceOverGroupsAndChannels() {
        let c1 = Data(repeating: 0xC1, count: 32)
        let first = ReachRules.departures(previous: nil, groupIds: [groupA, groupB], channelIds: [c1])
        XCTAssertFalse(first.leftGroup)
        XCTAssertFalse(first.leftChannel)
        let joined = ReachRules.departures(
            previous: first.membership, groupIds: [groupA, groupB, Data([7])], channelIds: [c1])
        XCTAssertFalse(joined.leftGroup)
        let left = ReachRules.departures(previous: first.membership, groupIds: [groupA], channelIds: [c1])
        XCTAssertTrue(left.leftGroup)
        XCTAssertFalse(left.leftChannel)
        let unloaded = ReachRules.departures(previous: first.membership, groupIds: [], channelIds: [])
        XCTAssertFalse(unloaded.leftGroup)
        XCTAssertFalse(unloaded.leftChannel)
        XCTAssertEqual(unloaded.membership, first.membership, "an empty list keeps the last membership")
        let channelLeft = ReachRules.departures(
            previous: first.membership, groupIds: [groupA, groupB], channelIds: [Data([9])])
        XCTAssertTrue(channelLeft.leftChannel)
    }

    /// Everyone opens the mailbox and allows every group and fof.
    func testEveryoneOpensTheMailbox() {
        let everyone = setting(mode: .everyone)
        XCTAssertTrue(ReachRules.mailboxOpen(everyone))
        XCTAssertTrue(classes(desired(everyone)).contains("fof"))
    }

    /// An expired link or reply is left out, so the next sync unregisters it.
    func testExpiredLinksAndRepliesAreLeftOut() {
        let live = LinkReach(linkIdB64: Data([1, 2]).base64EncodedString(), version: 1, expiresAtMs: 5_000)
        let dead = LinkReach(linkIdB64: Data([3, 4]).base64EncodedString(), version: 1, expiresAtMs: 500)
        let reply = ReplyReach(scopeIdB64: Data([9]).base64EncodedString(), expiresAtMs: 999, purpose: "x")
        let specs = desired(setting(mode: .nobody), links: [live, dead], replies: [reply])
        XCTAssertEqual(specs.filter { $0.reachClass == .link }.map(\.scopeId), [Data([1, 2])])
        XCTAssertTrue(specs.filter { $0.reachClass == .reply }.isEmpty)
    }

    func testTheRegistrationDiffAddsDesiredAndRemovesStale() {
        let a = Data(repeating: 1, count: 32), b = Data(repeating: 2, count: 32)
        let c = Data(repeating: 3, count: 32)
        let diff = ReachRules.registrationDiff(
            desired: [(a, nil), (b, 7), (a, nil)], registered: [b, c])
        XCTAssertEqual(diff.add.map(\.publicKey), [a, b])
        XCTAssertEqual(diff.add.last?.expiresAtMs, 7)
        XCTAssertEqual(diff.remove, [c])
    }

    /// ADR-0078 section 9 — a rotation of an account-wide class moves its one
    /// key, whatever scope the caller names; a `link` scope stays per link.
    func testARotationBumpsTheWholeClass() {
        // Every class starts at 0, as on web and Android (ADR-0060 section 1).
        let bumped = ReachRules.bumping(.member, scope: groupA, in: emptyState, nowMs: 50)
        XCTAssertEqual(ReachRules.version(of: .member, scope: Data(), in: bumped), 1)
        XCTAssertEqual(ReachRules.version(of: .member, scope: groupB, in: bumped), 1)
        XCTAssertEqual(ReachRules.version(of: .contact, scope: Data(), in: bumped), 0)
        let again = ReachRules.bumping(.member, scope: Data(), in: bumped, nowMs: 10)
        XCTAssertEqual(ReachRules.version(of: .member, scope: groupA, in: again), 2)
        XCTAssertGreaterThan(again.updatedAtMs, bumped.updatedAtMs, "the stamp never goes back")
        let link = ReachRules.bumping(.link, scope: Data([1]), in: emptyState, nowMs: 1)
        XCTAssertEqual(ReachRules.version(of: .link, scope: Data([1]), in: link), 1)
        XCTAssertEqual(ReachRules.version(of: .link, scope: Data([2]), in: link), 0)
    }

    // MARK: - Which received grant is kept (C-68)

    private func grant(
        _ c: FfiReachClass, scope: Data = Data(repeating: 7, count: 16), version: UInt32 = 1
    ) throws -> FfiReachGrant {
        let s: Data = ReachRules.isAccountWide(c) ? Data() : scope
        return try deriveReachGrant(accountSeed: accountSeed, class: c, scopeId: s, version: version)
    }

    func testAContactGrantIsKeptOnlyFromTheOtherParticipantOfAOneToOne() throws {
        let contact = try grant(.contact)
        XCTAssertTrue(ReachRules.keeps(
            contact, carrier: .oneToOneProfile(fromOtherParticipant: true), senderResolved: true))
        XCTAssertFalse(ReachRules.keeps(
            contact, carrier: .oneToOneProfile(fromOtherParticipant: false), senderResolved: true))
        XCTAssertFalse(ReachRules.keeps(contact, carrier: .groupProfile, senderResolved: true))
        XCTAssertFalse(ReachRules.keeps(contact, carrier: .request, senderResolved: true))
    }

    func testGroupAndMemberGrantsAreKeptFromGroupProfiles() throws {
        XCTAssertTrue(ReachRules.keeps(try grant(.group), carrier: .groupProfile, senderResolved: true))
        XCTAssertTrue(ReachRules.keeps(try grant(.member), carrier: .groupProfile, senderResolved: true))
        XCTAssertFalse(ReachRules.keeps(try grant(.channel), carrier: .groupProfile, senderResolved: true))
        XCTAssertTrue(ReachRules.keeps(try grant(.channel), carrier: .channelMemberCard, senderResolved: true))
    }

    /// A grant with no identity resolved at receipt is kept nowhere: it could
    /// only be filed under a key nothing authenticated.
    func testAnUnresolvedSenderKeepsNothing() throws {
        for c: FfiReachClass in [.contact, .group, .member, .reply, .channel] {
            for carrier: ReachRules.Carrier in [
                .oneToOneProfile(fromOtherParticipant: true), .groupProfile, .channelMemberCard, .request,
            ] {
                XCTAssertFalse(ReachRules.keeps(try grant(c), carrier: carrier, senderResolved: false))
            }
        }
    }

    func testLinkAndFofGrantsAreNeverKeptFromACarrier() throws {
        for carrier: ReachRules.Carrier in [
            .oneToOneProfile(fromOtherParticipant: true), .groupProfile, .channelMemberCard, .request,
        ] {
            XCTAssertFalse(ReachRules.keeps(try grant(.link), carrier: carrier, senderResolved: true))
            XCTAssertFalse(ReachRules.keeps(try grant(.fof), carrier: carrier, senderResolved: true))
        }
        XCTAssertTrue(ReachRules.keeps(try grant(.reply), carrier: .request, senderResolved: true))
    }

    // MARK: - Which grant signs a write

    func testTheWriteSeedPrefersContactThenThisGroupsMember() throws {
        let member = ReachRules.HeldGrant(grant: try grant(.member), scopeHex: "aa", storedAtMs: 1)
        let otherMember = ReachRules.HeldGrant(
            grant: try grant(.member, version: 2), scopeHex: "bb", storedAtMs: 2)
        let reply = ReachRules.HeldGrant(grant: try grant(.reply), scopeHex: nil, storedAtMs: 3)
        let contact = ReachRules.HeldGrant(grant: try grant(.contact), scopeHex: nil, storedAtMs: 4)
        XCTAssertEqual(
            ReachRules.writeSeed(from: [reply, otherMember, member], groupIdHex: "aa"), member.seed)
        XCTAssertEqual(
            ReachRules.writeSeed(from: [reply, member, contact], groupIdHex: "aa"), contact.seed)
        XCTAssertEqual(ReachRules.writeSeed(from: [reply], groupIdHex: nil), reply.seed)
        XCTAssertNil(ReachRules.writeSeed(from: [], groupIdHex: "aa"))
    }

    // MARK: - Answering a contact request

    /// The reply grant, then a contact grant, and nothing else.
    func testAnAnswerUsesTheReplyGrantThenAContactGrantAndNothingElse() throws {
        func held(_ cls: FfiReachClass, scope: String?, at: UInt64) throws -> ReachRules.HeldGrant {
            ReachRules.HeldGrant(grant: try grant(cls), scopeHex: scope, storedAtMs: at)
        }
        let group = try held(.group, scope: "aa", at: 9)
        let member = try held(.member, scope: "aa", at: 8)
        let channel = try held(.channel, scope: "cc", at: 7)
        let fof = try held(.fof, scope: nil, at: 6)
        let contact = try held(.contact, scope: nil, at: 5)
        let reply = try held(.reply, scope: nil, at: 1)
        XCTAssertEqual(
            ReachRules.answerGrants(from: [group, member, channel, fof, contact, reply]),
            [reply, contact])
        XCTAssertTrue(ReachRules.answerGrants(from: [group, member, channel, fof]).isEmpty)
    }

    func testAGrantARotationDisplacedAnswersLast() throws {
        var old = ReachRules.HeldGrant(grant: try grant(.reply), scopeHex: nil, storedAtMs: 5)
        old.superseded = true
        let contact = ReachRules.HeldGrant(grant: try grant(.contact), scopeHex: nil, storedAtMs: 1)
        XCTAssertEqual(ReachRules.answerGrants(from: [old, contact]), [contact, old])
    }

    private func answerCenter() throws -> (ReachCenter, String) {
        let (c, _) = try introductionCenter()
        return (c, peerIdentity.signaturePublicKey().base64EncodedString())
    }

    /// Seen live: a key from a group the person left sits first in the write
    /// order, and the accept never reached the person.
    func testAStaleGroupKeyIsNeverTriedOnAnAnswer() throws {
        let (c, person) = try answerCenter()
        let key = peerIdentity.signaturePublicKey()
        XCTAssertTrue(c.keep(try grant(.group), carrier: .groupProfile, fromAccountKeyB64: person, scopeHex: "aa"))
        XCTAssertTrue(c.keep(try grant(.member), carrier: .groupProfile, fromAccountKeyB64: person, scopeHex: "aa"))
        XCTAssertTrue(c.keep(try grant(.reply), carrier: .request, fromAccountKeyB64: person, scopeHex: nil))

        XCTAssertEqual(
            c.writePlan(toAccountKey: key)?.candidates.first?.reachSeed, try grant(.member).seed,
            "the general order puts the stale key first")

        let plans = c.answerPlans(toRequester: key)
        XCTAssertEqual(plans.count, 1)
        XCTAssertEqual(plans.first?.candidates.map(\.reachSeed), [try grant(.reply).seed])
        XCTAssertEqual(plans.first?.target, try MailboxTarget.verified(grant: try grant(.reply), accountKey: key))
    }

    func testAContactGrantFollowsTheReplyGrantOnAnAnswer() throws {
        let (c, person) = try answerCenter()
        let key = peerIdentity.signaturePublicKey()
        XCTAssertTrue(c.keep(
            try grant(.contact), carrier: .oneToOneProfile(fromOtherParticipant: true),
            fromAccountKeyB64: person, scopeHex: nil))
        XCTAssertTrue(c.keep(try grant(.reply), carrier: .request, fromAccountKeyB64: person, scopeHex: nil))
        XCTAssertEqual(
            c.answerPlans(toRequester: key).flatMap(\.candidates).map(\.reachSeed),
            [try grant(.reply).seed, try grant(.contact).seed])
    }

    func testWithOnlyAGroupKeyThereIsNoAnswer() throws {
        let (c, person) = try answerCenter()
        XCTAssertTrue(c.keep(try grant(.group), carrier: .groupProfile, fromAccountKeyB64: person, scopeHex: "aa"))
        XCTAssertTrue(c.answerPlans(toRequester: peerIdentity.signaturePublicKey()).isEmpty)
    }

    // MARK: - A device that holds nothing asks (ADR-0060 section 4)

    func testAOneToOneWithNoOfferFromThePersonAsks() {
        let nothing = ReachRules.HeldIntroductions(offer: false, bundle: false, contactGrant: false)
        XCTAssertTrue(ReachRules.asksForIntroductions(isOneToOne: true, held: nothing))
        XCTAssertFalse(ReachRules.asksForIntroductions(isOneToOne: false, held: nothing), "a group")
        for held in [
            ReachRules.HeldIntroductions(offer: true, bundle: false, contactGrant: false),
            ReachRules.HeldIntroductions(offer: true, bundle: true, contactGrant: true),
        ] {
            XCTAssertFalse(ReachRules.asksForIntroductions(isOneToOne: true, held: held))
        }
        // 2026-11-04 — a bundle without the person's own offer still asks.
        XCTAssertTrue(
            ReachRules.asksForIntroductions(
                isOneToOne: true,
                held: ReachRules.HeldIntroductions(offer: false, bundle: true, contactGrant: false)),
            "a bundle alone does not stand for the person's own offer")
        XCTAssertTrue(
            ReachRules.asksForIntroductions(
                isOneToOne: true,
                held: ReachRules.HeldIntroductions(offer: false, bundle: false, contactGrant: true)),
            "a contact grant does not stop the ask")
    }

    func testWithAContactGrantItAsksFirstThenEverySevenDays() {
        let week = ReachRules.introductionsAskWithGrantMinMs
        XCTAssertEqual(week, 604_800_000)
        XCTAssertTrue(ReachRules.asksWithContactGrant(lastRequestedAtMs: nil, nowMs: 5), "the first ask")
        XCTAssertFalse(ReachRules.asksWithContactGrant(lastRequestedAtMs: 1_000, nowMs: 1_000 + week - 1))
        XCTAssertTrue(ReachRules.asksWithContactGrant(lastRequestedAtMs: 1_000, nowMs: 1_000 + week))
        XCTAssertFalse(
            ReachRules.asksWithContactGrant(lastRequestedAtMs: 1_000, nowMs: 999),
            "a clock that went backwards reads as recent")
    }

    func testAnAskResetsOnlyFromTheOtherParticipantOnceAnHour() {
        let hour = ReachRules.introductionsResendMinMs
        XCTAssertEqual(hour, 3_600_000)
        XCTAssertTrue(ReachRules.resetsIntroductions(isOneToOneWithAsker: true, lastResetAtMs: nil, nowMs: 5))
        XCTAssertFalse(
            ReachRules.resetsIntroductions(isOneToOneWithAsker: false, lastResetAtMs: nil, nowMs: 5),
            "a group, or this account's own device")
        XCTAssertFalse(ReachRules.resetsIntroductions(
            isOneToOneWithAsker: true, lastResetAtMs: 1_000, nowMs: 1_000 + hour - 1))
        XCTAssertTrue(ReachRules.resetsIntroductions(
            isOneToOneWithAsker: true, lastResetAtMs: 1_000, nowMs: 1_000 + hour))
        XCTAssertFalse(
            ReachRules.resetsIntroductions(isOneToOneWithAsker: true, lastResetAtMs: 1_000, nowMs: 999),
            "a clock that went backwards reads as recent")
    }

    private func introductionCenter() throws -> (ReachCenter, InMemoryReachPersistence) {
        let persistence = InMemoryReachPersistence()
        let c = ReachCenter(persistence: persistence, ds: FakeReachDs())
        c.keyResolverProvider = { _, own in AccountKeyResolver(ownCurrentKey: own) }
        c.ownPredecessorSigningSeeds = { _ in [] }
        c.activate(
            accountIdB64: "introductions",
            accountIdentity: try Identity.fromSeed(seed: Data(repeating: 0x40, count: 32)))
        return (c, persistence)
    }

    /// The reset forgets one conversation in both sent records, and only
    /// that one, and only once an hour.
    func testTheResetForgetsOnlyThatConversation() throws {
        let (c, persistence) = try introductionCenter()
        for group in ["aa", "bb"] {
            c.markOfferSent(groupIdHex: group, key: "offer")
            c.markBundleSent(groupIdHex: group, digest: "bundle")
        }
        let now: UInt64 = 10_000_000

        XCTAssertFalse(c.noteIntroductionsAsked(groupIdHex: "aa", isOneToOneWithAsker: false, nowMs: now))
        XCTAssertFalse(c.offerNeedsSending(groupIdHex: "aa", key: "offer"))

        XCTAssertTrue(c.noteIntroductionsAsked(groupIdHex: "aa", isOneToOneWithAsker: true, nowMs: now))
        XCTAssertTrue(c.offerNeedsSending(groupIdHex: "aa", key: "offer"))
        XCTAssertTrue(c.bundleNeedsSending(groupIdHex: "aa", digest: "bundle"))
        XCTAssertFalse(c.offerNeedsSending(groupIdHex: "bb", key: "offer"))
        XCTAssertFalse(c.bundleNeedsSending(groupIdHex: "bb", digest: "bundle"))
        XCTAssertEqual(persistence.load(accountIdB64: "introductions")?.introductionsResetAtMs?["aa"], now)

        // Sent again; a second ask within the hour leaves that alone.
        c.markOfferSent(groupIdHex: "aa", key: "offer")
        XCTAssertFalse(c.noteIntroductionsAsked(
            groupIdHex: "aa", isOneToOneWithAsker: true, nowMs: now + ReachRules.introductionsResendMinMs - 1))
        XCTAssertFalse(c.offerNeedsSending(groupIdHex: "aa", key: "offer"))
        XCTAssertTrue(c.noteIntroductionsAsked(groupIdHex: "bb", isOneToOneWithAsker: true, nowMs: now),
                      "the hour is per conversation")
    }

    func testWhatIsHeldFromAPersonIsReadByPerson() throws {
        let (c, _) = try introductionCenter()
        let person = peerIdentity.signaturePublicKey().base64EncodedString()
        XCTAssertEqual(
            c.heldIntroductions(fromAccountKeyB64: person),
            ReachRules.HeldIntroductions(offer: false, bundle: false, contactGrant: false))

        XCTAssertTrue(c.keep(
            try grant(.contact), carrier: .oneToOneProfile(fromOtherParticipant: true),
            fromAccountKeyB64: person, scopeHex: nil))
        XCTAssertTrue(c.heldIntroductions(fromAccountKeyB64: person).contactGrant)

        c.storeBundle(
            FfiIntroductionsBundle(updatedAtMs: 1, records: []), fromAccountKeyB64: person)
        XCTAssertEqual(
            c.heldIntroductions(fromAccountKeyB64: person),
            ReachRules.HeldIntroductions(offer: false, bundle: true, contactGrant: true))

        c.storeOffer(Data([1]), fromAccountKeyB64: person)
        XCTAssertTrue(c.heldIntroductions(fromAccountKeyB64: person).offer)
        let other = Data(repeating: 0x42, count: 32).base64EncodedString()
        XCTAssertFalse(c.heldIntroductions(fromAccountKeyB64: other).offer, "one person's only")
    }

    /// UX section 6.1 — a search tries every grant in turn: contact, each
    /// group, each channel, the found one; never a member's or a reply's.
    func testTheSearchGrantsAreOrderedAndNeverAMemberOrAReply() throws {
        func held(_ cls: FfiReachClass, version: UInt32, scope: String?, at: UInt64) throws -> ReachRules.HeldGrant {
            ReachRules.HeldGrant(grant: try grant(cls, version: version), scopeHex: scope, storedAtMs: at)
        }
        let fof = try held(.fof, version: 1, scope: nil, at: 9)
        let channel = try held(.channel, version: 1, scope: "cc", at: 8)
        let olderGroup = try held(.group, version: 1, scope: "aa", at: 1)
        let newerGroup = try held(.group, version: 2, scope: "bb", at: 2)
        let contact = try held(.contact, version: 1, scope: nil, at: 0)
        let member = try held(.member, version: 1, scope: "aa", at: 10)
        let reply = try held(.reply, version: 1, scope: nil, at: 11)
        XCTAssertEqual(
            ReachRules.searchGrants(from: [fof, member, channel, olderGroup, reply, newerGroup, contact]),
            [contact, newerGroup, olderGroup, channel, fof])
        XCTAssertTrue(ReachRules.searchGrants(from: [member, reply]).isEmpty)
    }

    /// One key per class per account (ADR-0078 section 9): two groups carry
    /// the same seed, which is asked about once.
    func testTheSearchGrantsAskAboutOneSeedOnce() throws {
        let one = ReachRules.HeldGrant(grant: try grant(.group), scopeHex: "aa", storedAtMs: 1)
        let two = ReachRules.HeldGrant(grant: try grant(.group), scopeHex: "bb", storedAtMs: 2)
        XCTAssertEqual(ReachRules.searchGrants(from: [one, two]), [two])
    }

    func testASupersededGrantIsTriedAfterEveryCurrentOne() throws {
        var old = ReachRules.HeldGrant(grant: try grant(.contact), scopeHex: nil, storedAtMs: 5)
        old.superseded = true
        let channel = ReachRules.HeldGrant(grant: try grant(.channel), scopeHex: "cc", storedAtMs: 1)
        XCTAssertEqual(ReachRules.searchGrants(from: [old, channel]), [channel, old])
    }

    /// A replayed older version never displaces the newer grant of one scope.
    func testMergingKeepsTheNewestVersion() throws {
        let v2 = try deriveReachGrant(accountSeed: accountSeed, class: .member, scopeId: Data(), version: 2)
        let v1 = try deriveReachGrant(accountSeed: accountSeed, class: .member, scopeId: Data(), version: 1)
        let held = ReachRules.merging(
            ReachRules.HeldGrant(grant: v2, scopeHex: "aa", storedAtMs: 1), into: [])
        let replayed = ReachRules.merging(
            ReachRules.HeldGrant(grant: v1, scopeHex: "aa", storedAtMs: 9), into: held)
        XCTAssertEqual(replayed.map(\.version), [2])
        let v3 = try deriveReachGrant(accountSeed: accountSeed, class: .member, scopeId: Data(), version: 3)
        let rotated = ReachRules.merging(
            ReachRules.HeldGrant(grant: v3, scopeHex: "aa", storedAtMs: 10), into: held)
        XCTAssertEqual(rotated.map(\.version), [3])
    }

    // MARK: - Welcomes and shrink

    /// One write per added ACCOUNT: two leaves of one person collapse; an
    /// unresolvable leaf is kept as itself for a post-merge resolution.
    func testWelcomeTargetsAreOnePerAccount() {
        let leaf1 = Data([1]), leaf2 = Data([2]), leaf3 = Data([3]), account = Data([9])
        let targets = ReachRules.welcomeTargets(addedLeafKeys: [leaf1, leaf2, leaf3]) {
            $0 == leaf3 ? nil : account
        }
        XCTAssertEqual(targets, [account, leaf3])
    }

    func testAShrinkIsASetDifferenceNeverACount() {
        XCTAssertTrue(ReachRules.membershipShrank(previous: ["a", "b"], current: ["a", "c"]))
        XCTAssertFalse(ReachRules.membershipShrank(previous: ["a"], current: ["a", "b"]))
        XCTAssertFalse(ReachRules.membershipShrank(previous: nil, current: ["a"]))
        XCTAssertFalse(ReachRules.membershipShrank(previous: ["a"], current: []),
                       "an empty (torn) roster is never evidence of a shrink")
    }

    // MARK: - ReachCenter

    private func center() throws -> ReachCenter {
        let c = ReachCenter(
            persistence: InMemoryReachPersistence(),
            // Unreachable on purpose: a unit test never registers anything.
            ds: Offline.ds())
        c.activate(accountIdB64: "acct", accountIdentity: try Identity.fromSeed(seed: Data(repeating: 0x31, count: 32)))
        return c
    }

    /// A new account starts at the default of UX section 1, at stamp 0: a
    /// device newly admitted to the account must not hold a default stamped
    /// later than the setting its first device carries over the DeviceGroup
    /// (ADR-0060 section 7), or it would refuse it.
    func testANewAccountStartsAtTheDefaultSetting() throws {
        let c = try center()
        XCTAssertEqual(c.setting.mode, .some)
        XCTAssertTrue(c.setting.contactsOfContacts)
        XCTAssertEqual(c.setting.groups, .all)
        XCTAssertEqual(c.setting.updatedAtMs, 0)
    }

    /// A one-to-one profile carries `contact` + `member`; a group profile
    /// `group` (only where allowed) + `member`.
    func testProfileGrantsFollowTheConversationAndTheSetting() throws {
        let c = try center()
        XCTAssertEqual(
            c.profileGrants(groupId: groupA, oneToOne: true).map(\.class), [.contact, .member])
        XCTAssertEqual(
            c.profileGrants(groupId: groupA, oneToOne: false).map(\.class), [.group, .member])
        c.updateSetting(chooseVisibilityMode(setting: c.setting, mode: .nobody))
        XCTAssertEqual(c.profileGrants(groupId: groupA, oneToOne: false).map(\.class), [.member])
    }

    /// ADR-0078 section 9 — a shrink of one group rotates the account's one
    /// `group` and `member` key: every group's profile carries the new grants.
    func testRotatingAGroupChangesTheGrantsOfEveryGroup() throws {
        let c = try center()
        let before = c.profileGrants(groupId: groupA, oneToOne: false)
        XCTAssertEqual(before, c.profileGrants(groupId: groupB, oneToOne: false), "one key for every group")
        var republished = 0
        c.onGrantsChanged = { republished += 1 }
        c.rotateGroup(groupA)
        let after = c.profileGrants(groupId: groupA, oneToOne: false)
        XCTAssertEqual(after.map(\.version), before.map { $0.version + 1 })
        XCTAssertNotEqual(after.map(\.seed), before.map(\.seed))
        XCTAssertEqual(c.profileGrants(groupId: groupB, oneToOne: false), after)
        XCTAssertEqual(republished, 1, "the profiles are republished once")
    }

    /// ADR-0078 section 9, decision 4 — a setting change made on this device
    /// rotates the `group` and `channel` keys and republishes the profiles.
    func testASettingChangeRotatesTheGroupAndChannelKeys() throws {
        let c = try center()
        let channel = Data(repeating: 0xC1, count: 32)
        let groupBefore = try XCTUnwrap(c.profileGrants(groupId: groupA, oneToOne: false).first { $0.class == .group })
        let channelBefore = try XCTUnwrap(c.channelGrant(channelId: channel))
        let memberBefore = try XCTUnwrap(c.profileGrants(groupId: groupA, oneToOne: false).first { $0.class == .member })
        var republished = 0
        c.onGrantsChanged = { republished += 1 }
        c.updateSetting(setContactsOfContacts(setting: c.setting, on: false))
        let groupAfter = try XCTUnwrap(c.profileGrants(groupId: groupA, oneToOne: false).first { $0.class == .group })
        XCTAssertEqual(groupAfter.version, groupBefore.version + 1)
        XCTAssertEqual(c.channelGrant(channelId: channel)?.version, channelBefore.version + 1)
        XCTAssertEqual(
            c.profileGrants(groupId: groupA, oneToOne: false).first { $0.class == .member }?.version,
            memberBefore.version, "the member key is not the setting's")
        XCTAssertGreaterThan(republished, 0)
    }

    /// A kept grant authorises writes to its giver's mailbox; one refused by
    /// the carrier rule is not kept.
    func testAKeptGrantSignsWritesToItsGiver() throws {
        let c = try center()
        let peer = try Identity.fromSeed(seed: Data(repeating: 0x41, count: 32)).signaturePublicKey()
        let contact = try grant(.contact)
        XCTAssertFalse(c.keep(contact, carrier: .groupProfile,
                              fromAccountKeyB64: peer.base64EncodedString(), scopeHex: "aa"))
        XCTAssertNil(c.writeAuth(toAccountKey: peer))
        XCTAssertTrue(c.keep(contact, carrier: .oneToOneProfile(fromOtherParticipant: true),
                             fromAccountKeyB64: peer.base64EncodedString(), scopeHex: "aa"))
        XCTAssertEqual(c.writeAuth(toAccountKey: peer)?.reachSeed, contact.seed)
        // ADR-0078 — the mailbox is the one the grant's binding names.
        let mailbox = MailboxKey.fromAccountIdentity(accountIdentity: peerIdentity).address()
        XCTAssertEqual(c.writePlan(toAccountKey: peer)?.target.address, mailbox)
        XCTAssertEqual(c.writeAuth(toMailbox: mailbox)?.reachSeed, contact.seed)
    }

    /// This account's own mailbox is written with its own `contact` key, which
    /// every device of the account derives and registers.
    func testTheOwnMailboxIsWrittenWithTheOwnContactKey() throws {
        let c = try center()
        guard let own = c.ownAccountKey else { return XCTFail("no own key") }
        XCTAssertEqual(c.writeAuth(toAccountKey: own)?.reachSeed, c.ownContactSeed())
        XCTAssertNotNil(c.ownContactSeed())
    }

    /// UX section 5 — turning one group off under "All" switches to "chosen"
    /// with every other scope on.
    func testTurningAGroupOffUnderAllSwitchesToChosen() throws {
        let c = try center()
        let a = FfiScopeId(kind: .group, id: groupA), b = FfiScopeId(kind: .group, id: groupB)
        c.allScopesProvider = { [a, b] }
        XCTAssertTrue(c.setMembersCanFindMe(a, on: false))
        XCTAssertEqual(c.setting.groups, .chosen)
        XCTAssertFalse(WhoCanFindMeCopy.isChosen(a, in: c.setting))
        XCTAssertTrue(WhoCanFindMeCopy.isChosen(b, in: c.setting))
        XCTAssertEqual(WhoCanFindMeCopy.chosenHelper(setting: c.setting, all: [a, b]),
                       Localization.text("who_can_find_me.groups_chosen_helper",
                                         texts: ["chosen": "1", "total": "2"]))
    }

    // MARK: - The five envelopes never render

    func testTheFiveEnvelopesClassifyAndAChatBodyDoesNot() throws {
        var s = defaultVisibilitySetting()
        s.updatedAtMs = 3
        guard case .visibility = ScopedDiscoveryEnvelope.decode(try composeVisibilityEnvelope(setting: s))
        else { return XCTFail("visibility") }
        let state = FfiReachState(
            entries: [FfiReachStateEntry(class: .fof, scopeId: Data(), version: 4)], updatedAtMs: 2)
        guard case .reachState = ScopedDiscoveryEnvelope.decode(try composeReachStateEnvelope(state: state))
        else { return XCTFail("reach state") }
        XCTAssertNil(ScopedDiscoveryEnvelope.decode(Data("{\"v\":1,\"text\":\"lakko\"}".utf8)))
        XCTAssertNil(ScopedDiscoveryEnvelope.decode(Data("lakko".utf8)))
    }

    /// Classified in the foreground chain BEFORE the chat parser, with the
    /// sender resolved at receipt.
    func testTheChatClientRoutesAScopedEnvelopeAwayFromTheTimeline() throws {
        let state = FfiReachState(entries: [], updatedAtMs: 1)
        let bytes = try composeReachStateEnvelope(state: state)
        let event = ChatClient.toEvent(
            messageId: 4, createdAt: "2026-09-29T00:00:00Z",
            processed: .application(data: bytes, senderLeaf: 2, senderAccount: nil),
            resolveSender: { _ in "member-2" })
        guard case .scopedDiscovery(4, .reachState, let sender, _) = event else {
            return XCTFail("expected .scopedDiscovery, got \(event)")
        }
        XCTAssertEqual(sender, "member-2")
    }
}

// MARK: - ADR-0050 2.5a item 7 / ADR-0060 "Versions after a restore"

/// A fake of the two reach-key DS routes: records every registration and
/// answers the owner's read with the keys it was given (or a failure).
private final class FakeReachDs: ReachDsTransport {
    var registered: [DsReachKeyAdd] = []
    var readFails = false
    var reads = 0
    var updates: [(add: [DsReachKeyAdd], remove: [Data])] = []

    func updateReach(
        pseudonym: Data, add: [DsReachKeyAdd], remove: [Data], open: Bool?, signer: DsRequestSigner
    ) async throws {
        updates.append((add, remove))
    }

    func fetchReach(pseudonym: Data, signer: DsRequestSigner) async throws -> DsReachListing {
        reads += 1
        if readFails {
            throw DsClientError.network(endpoint: "/ds/mailbox/x/reach", underlying: URLError(.notConnectedToInternet))
        }
        return DsReachListing(keys: registered, open: false)
    }
}

extension ReachRulesTests {

    private func restoredCenter(_ ds: FakeReachDs) throws -> (ReachCenter, Data) {
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x31, count: 32))
        let c = ReachCenter(persistence: InMemoryReachPersistence(), ds: ds)
        c.activate(accountIdB64: "acct", accountIdentity: identity)
        return (c, identity.ed25519Seed())
    }

    private func pk(_ seed: Data, _ cls: FfiReachClass, _ scope: Data = Data(), _ v: UInt32) throws -> Data {
        try ReachCenter.publicKey(accountSeed: seed, spec: ReachKeySpec(
            reachClass: cls, scopeId: scope, version: v, expiresAtMs: nil))
    }

    /// G — a restored device (no version state) whose mailbox holds the
    /// `contact` key at v1 resumes at v1 and never registers v0; a class with
    /// no registered key (`fof` here) starts at 0.
    func testARestoredDeviceResumesAtTheRegisteredVersionAndNeverRegistersALowerOne() async throws {
        let ds = FakeReachDs()
        let (c, seed) = try restoredCenter(ds)
        ds.registered = [DsReachKeyAdd(publicKey: try pk(seed, .contact, Data(), 1), expiresAtMs: nil)]

        let applied = await c.syncNow()

        XCTAssertTrue(applied)
        XCTAssertEqual(ds.reads, 1, "the DS was read before anything was registered")
        let added = Set(ds.updates.flatMap { $0.add.map(\.publicKey) })
        XCTAssertTrue(added.contains(try pk(seed, .contact, Data(), 1)))
        XCTAssertFalse(added.contains(try pk(seed, .contact, Data(), 0)), "v0 is never registered again")
        XCTAssertEqual(c.profileGrants(groupId: groupA, oneToOne: true).first?.version, 1,
                       "the contact grant handed out is the current version")
        XCTAssertEqual(c.fofGrant()?.version, 0, "nothing registered for fof: it starts at 0")

        // Checked once: the next sync reads nothing.
        _ = await c.syncNow()
        XCTAssertEqual(ds.reads, 1)
    }

    /// G — the read fails: nothing is registered (a guessed version could be
    /// one the owner rotated away); the next sync retries the read.
    func testAReadFailureRegistersNothing() async throws {
        let ds = FakeReachDs()
        ds.readFails = true
        let (c, _) = try restoredCenter(ds)

        let applied = await c.syncNow()

        XCTAssertFalse(applied)
        XCTAssertTrue(ds.updates.isEmpty, "nothing registered while the versions are unknown")
        XCTAssertEqual(c.status, .pendingOffline)

        ds.readFails = false
        let retried = await c.syncNow()
        XCTAssertTrue(retried)
        XCTAssertEqual(ds.reads, 2)
        XCTAssertFalse(ds.updates.isEmpty)
    }

    /// G — a group this device is admitted to later (re-admission after a
    /// restore) is checked before its keys are registered: its `member` key at
    /// v2 is resumed, v0 and v1 are never registered.
    func testAGroupJoinedAfterTheRestoreIsCheckedToo() async throws {
        let ds = FakeReachDs()
        let (c, seed) = try restoredCenter(ds)
        _ = await c.syncNow()
        XCTAssertEqual(ds.reads, 1)

        ds.registered = [DsReachKeyAdd(publicKey: try pk(seed, .member, Data(), 2), expiresAtMs: nil)]
        c.groupIdsProvider = { [self.groupA] }
        ds.updates.removeAll()
        _ = await c.syncNow()

        XCTAssertEqual(ds.reads, 2, "the member slot was looked up once a group appeared")
        let added = Set(ds.updates.flatMap { $0.add.map(\.publicKey) })
        XCTAssertTrue(added.contains(try pk(seed, .member, Data(), 2)))
        XCTAssertFalse(added.contains(try pk(seed, .member, Data(), 0)))
        XCTAssertFalse(added.contains(try pk(seed, .member, Data(), 1)))
    }

    /// ADR-0078 section 9 — leaving a group rotates the class key and
    /// republishes the profile in every remaining group: the next
    /// registration removes the old `member` key and adds the new one.
    func testLeavingAGroupRotatesTheClassKeyAndRepublishes() async throws {
        let ds = FakeReachDs()
        let (c, seed) = try restoredCenter(ds)
        c.groupIdsProvider = { [self.groupA, self.groupB] }
        _ = await c.syncNow()
        let memberV0 = try pk(seed, .member, Data(), 0)
        XCTAssertTrue(Set(ds.updates.flatMap { $0.add.map(\.publicKey) }).contains(memberV0))

        var republished = 0
        c.onGrantsChanged = { republished += 1 }
        c.groupIdsProvider = { [self.groupA] }
        ds.updates.removeAll()
        _ = await c.syncNow()

        XCTAssertEqual(republished, 1, "the remaining groups get the new grants")
        XCTAssertEqual(
            c.profileGrants(groupId: groupA, oneToOne: false).first { $0.class == .member }?.version, 1)
        let update = try XCTUnwrap(ds.updates.last)
        XCTAssertTrue(update.add.map(\.publicKey).contains(try pk(seed, .member, Data(), 1)))
        XCTAssertTrue(update.remove.contains(memberV0), "the key the departed group's members hold goes")
    }

    /// The pure recovery: the HIGHEST registered version per slot, never one
    /// above the probe limit, nothing for a slot with no registered key.
    func testRecoveredVersionsTakeTheHighestRegisteredOne() throws {
        let contact = ReachRules.VersionSlot(reachClass: .contact, scopeId: Data())
        let member = ReachRules.VersionSlot(reachClass: .member, scopeId: Data())
        let fof = ReachRules.VersionSlot(reachClass: .fof, scopeId: Data())
        func key(_ slot: ReachRules.VersionSlot, _ v: UInt32) -> Data {
            Data(ReachRules.className(slot.reachClass).utf8) + slot.scopeId + Data([UInt8(truncatingIfNeeded: v)])
        }
        let found = ReachRules.recoveredVersions(
            slots: [contact, member, fof],
            registered: [key(contact, 1), key(contact, 3), key(member, 0)],
            limit: 4, publicKey: key)
        XCTAssertEqual(found, [contact: 3, member: 0])
        XCTAssertEqual(ReachRules.versionRecoveryProbeLimit, 256)
    }

    // MARK: F — every key held for a mailbox, in order

    /// The Welcome writer gets every grant held for the person (contact first,
    /// then the member key of the group the write concerns), this account's
    /// own key alone for its own mailbox, and nothing for a stranger.
    func testWriteAuthCandidatesListEveryHeldGrantInOrder() throws {
        let c = try center()
        let peer = try Identity.fromSeed(seed: Data(repeating: 0x41, count: 32)).signaturePublicKey()
        let peerB64 = peer.base64EncodedString()
        let groupHex = DsClient.pseudonymHex(groupA)
        let member = try deriveReachGrant(accountSeed: accountSeed, class: .member, scopeId: Data(), version: 3)
        let contact = try grant(.contact)
        XCTAssertTrue(c.keep(member, carrier: .groupProfile, fromAccountKeyB64: peerB64, scopeHex: groupHex))
        XCTAssertTrue(c.keep(contact, carrier: .oneToOneProfile(fromOtherParticipant: true),
                             fromAccountKeyB64: peerB64, scopeHex: "aa"))
        let mailbox = MailboxKey.fromAccountIdentity(accountIdentity: peerIdentity).address()

        XCTAssertEqual(
            c.writeAuthCandidates(toMailbox: mailbox, groupIdHex: groupHex).map(\.reachSeed),
            [contact.seed, member.seed])

        let ownMailbox = try XCTUnwrap(c.ownMailbox?.address)
        XCTAssertEqual(c.writeAuthCandidates(toMailbox: ownMailbox).map(\.reachSeed), [c.ownContactSeed()])

        let stranger = MailboxKey.fromAccountIdentity(
            accountIdentity: try Identity.fromSeed(seed: Data(repeating: 0x42, count: 32))).address()
        XCTAssertTrue(c.writeAuthCandidates(toMailbox: stranger).isEmpty, "no key, no candidate")
    }

    /// F — a grant kept for someone fires the retry hook with their key.
    func testKeepingAGrantFiresTheRetryHook() throws {
        let c = try center()
        var fired: [String] = []
        c.onGrantKept = { fired.append($0) }
        let peerB64 = peerIdentity.signaturePublicKey().base64EncodedString()
        XCTAssertTrue(c.keep(try grant(.contact), carrier: .oneToOneProfile(fromOtherParticipant: true),
                             fromAccountKeyB64: peerB64, scopeHex: "aa"))
        XCTAssertEqual(fired, [peerB64])
    }

    /// ADR-0078 — a grant is kept only when its mailbox binding verifies
    /// against the sender resolved at receipt; one whose binding is someone
    /// else's is refused and fires nothing.
    func testAGrantWhoseBindingDoesNotVerifyAgainstTheSenderIsRefused() throws {
        let c = try center()
        var fired: [String] = []
        c.onGrantKept = { fired.append($0) }
        let other = try Identity.fromSeed(seed: Data(repeating: 0x42, count: 32)).signaturePublicKey()
        XCTAssertFalse(c.keep(try grant(.contact), carrier: .oneToOneProfile(fromOtherParticipant: true),
                              fromAccountKeyB64: other.base64EncodedString(), scopeHex: "aa"))
        XCTAssertTrue(fired.isEmpty)
        XCTAssertNil(c.writePlan(toAccountKey: other))
    }

    func testOnlyTermsNotYetKeptAreEvaluated() {
        XCTAssertEqual(
            ReachRules.termsToEvaluate(cached: ["u:vgi", "n:val"], terms: ["u:vgi", "n:val", "n:gon", "n:gon"]),
            ["n:gon"])
        XCTAssertTrue(ReachRules.termsToEvaluate(cached: ["n:val"], terms: ["n:val"]).isEmpty)
    }
}

// MARK: - ADR-0060 section 4: the scheduled `fof` rotation

extension ReachRulesTests {

    func testTheFofScheduleIsSevenDaysAndABackwardClockIsNotDue() {
        let week = ReachRules.fofRotationMs
        XCTAssertEqual(week, 7 * 24 * 3_600_000)
        XCTAssertFalse(ReachRules.fofRotationDue(lastRotatedAtMs: nil, nowMs: 10 * week),
                       "no schedule started yet")
        XCTAssertFalse(ReachRules.fofRotationDue(lastRotatedAtMs: 1_000, nowMs: 1_000 + week - 1))
        XCTAssertTrue(ReachRules.fofRotationDue(lastRotatedAtMs: 1_000, nowMs: 1_000 + week))
        XCTAssertFalse(ReachRules.fofRotationDue(lastRotatedAtMs: 5 * week, nowMs: week),
                       "a clock that went backwards is not due")
    }

    /// The first registration starts the schedule without rotating; seven
    /// days later the next registration rotates `fof`: the new key is
    /// registered and the old one unregistered in the same pass. The schedule
    /// is in the persisted record, so a restart keeps it.
    func testTheFofKeyRotatesOnItsScheduleAndTheScheduleSurvivesARestart() async throws {
        let ds = FakeReachDs()
        let persistence = InMemoryReachPersistence()
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x31, count: 32))
        let seed = identity.ed25519Seed()
        var now: UInt64 = 1_000_000
        let first = ReachCenter(persistence: persistence, ds: ds)
        first.clockMs = { now }
        first.activate(accountIdB64: "acct", accountIdentity: identity)

        _ = await first.syncNow()
        XCTAssertEqual(first.fofGrant()?.version, 0, "the first registration starts the schedule only")
        XCTAssertEqual(persistence.records["acct"]?.fofRotatedAtMs, now)

        now += ReachRules.fofRotationMs - 1
        _ = await first.syncNow()
        XCTAssertEqual(first.fofGrant()?.version, 0, "not due a millisecond early")

        // A restart: a new center over the same stored record.
        first.deactivate()
        let second = ReachCenter(persistence: persistence, ds: ds)
        now += 1
        second.clockMs = { now }
        second.activate(accountIdB64: "acct", accountIdentity: identity)
        ds.updates.removeAll()
        _ = await second.syncNow()

        XCTAssertEqual(second.fofGrant()?.version, 1, "rotated once the seven days passed")
        XCTAssertEqual(persistence.records["acct"]?.fofRotatedAtMs, now, "the schedule restarts")
        let added = Set(ds.updates.flatMap { $0.add.map(\.publicKey) })
        let removed = Set(ds.updates.flatMap(\.remove))
        XCTAssertTrue(added.contains(try pk(seed, .fof, Data(), 1)), "the new key is registered")
        XCTAssertTrue(removed.contains(try pk(seed, .fof, Data(), 0)), "the old key goes at once")

        ds.updates.removeAll()
        _ = await second.syncNow()
        XCTAssertEqual(second.fofGrant()?.version, 1, "rotated once, not on every pass")
    }

    /// Coming back to the foreground runs a pass only when a rotation is due.
    func testForegroundRunsAPassOnlyWhenTheRotationIsDue() async throws {
        let ds = FakeReachDs()
        let persistence = InMemoryReachPersistence()
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x31, count: 32))
        var now: UInt64 = 2_000_000
        let c = ReachCenter(persistence: persistence, ds: ds)
        c.clockMs = { now }
        c.activate(accountIdB64: "acct", accountIdentity: identity)
        _ = await c.syncNow()
        let updatesAfterFirst = ds.updates.count

        c.noteForeground()
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(ds.updates.count, updatesAfterFirst, "nothing due, no pass")

        now += ReachRules.fofRotationMs
        c.noteForeground()
        for _ in 0..<40 where c.fofGrant()?.version != 1 {
            try await Task.sleep(nanoseconds: 50_000_000)
        }
        XCTAssertEqual(c.fofGrant()?.version, 1, "the due rotation ran when the app came back")
    }
}
