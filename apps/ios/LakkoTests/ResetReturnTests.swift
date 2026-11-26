// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0101 — reaching an account after its key reset. Mirrors the web
/// suites `auth/reset-continuity.test.ts`, `reach/continuity.test.ts`,
/// `channels/channel-return.test.ts`, `channels/recovered-channels.test.ts`,
/// `chat/history-return.test.ts`, `profile/profile-signer.test.ts` and
/// `devices/history-page.test.ts`.
final class ResetReturnTests: XCTestCase {
    private var defaults: UserDefaults!
    private var suite: String!
    private let account = "QUNDT1VOVC0wMTAxLTAwMQ=="

    override func setUp() {
        super.setUp()
        suite = "reset-return-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suite)
        super.tearDown()
    }

    // MARK: - The window (reset-continuity.test.ts)

    func testTheWindowIsTheCoresCountedFromSignedAt() {
        XCTAssertEqual(UInt64(ResetContinuityRules.windowMs), continuityWindowMs())
        XCTAssertEqual(UInt64(ContinuityTargets.keyTtlMs), continuityWindowMs())
        XCTAssertEqual(ResetContinuityRules.expiryMs(signedAtMs: 5), 5 + ResetContinuityRules.windowMs)
    }

    func testABindingSignedForThatExpiryVerifies() throws {
        let id = try Identity.fromSeed(seed: Data(repeating: 3, count: 32))
        let now: Int64 = 1_700_000_000_000
        let binding = signContinuityBinding(
            accountIdentity: id, expiresAtMs: UInt64(ResetContinuityRules.expiryMs(signedAtMs: now)))
        XCTAssertEqual(try verifyContinuityBinding(
            binding: binding, identityPk: id.signaturePublicKey(), nowMs: UInt64(now)).count, 32)
        XCTAssertEqual(ContinuityTargets.bindingExpiryMs(binding), ResetContinuityRules.expiryMs(signedAtMs: now))
    }

    func testTheMailboxIsReadStrictlyBeforeTheExpiryAndNeverWithoutOne() {
        XCTAssertTrue(ResetContinuityRules.mailboxReadable(expiresAtMs: 10, nowMs: 9))
        XCTAssertFalse(ResetContinuityRules.mailboxReadable(expiresAtMs: 10, nowMs: 10))
        XCTAssertFalse(ResetContinuityRules.mailboxReadable(expiresAtMs: nil, nowMs: 0))
        XCTAssertFalse(ResetContinuityRules.mailboxReadable(expiresAtMs: 0, nowMs: 0))
    }

    func testTheRecordRoundTripsAndTheAnnouncementIsOwedOnlyInsideTheWindow() {
        let store = ResetContinuityStore(defaults: defaults)
        let rec = ResetContinuity(expiresAtMs: 42, previousKeyB64: "T0xE", keyGeneration: 3)
        store.write(rec, accountIdB64: account)
        XCTAssertEqual(store.read(accountIdB64: account), rec)
        XCTAssertTrue(ResetContinuityRules.announcementOwed(rec, nowMs: 41))
        var announced = rec
        announced.announced = true
        XCTAssertFalse(ResetContinuityRules.announcementOwed(announced, nowMs: 41))
        XCTAssertFalse(ResetContinuityRules.announcementOwed(rec, nowMs: 42))
        // Before the attestation's signed_at is known nothing is announced.
        XCTAssertFalse(ResetContinuityRules.announcementOwed(
            ResetContinuity(expiresAtMs: 0, previousKeyB64: nil, keyGeneration: 3), nowMs: 0))
        store.markAnnounced(accountIdB64: account)
        XCTAssertEqual(store.read(accountIdB64: account)?.announced, true)
        defaults.set(Data("{".utf8), forKey: ResetContinuityStore.prefix + "b")
        XCTAssertNil(store.read(accountIdB64: "b"))
        store.clear(accountIdB64: account)
        XCTAssertNil(store.read(accountIdB64: account))
    }

    func testTheExpiryIsCompletedFromTheAttestationsSignedAt() {
        let store = ResetContinuityStore(defaults: defaults)
        store.write(ResetContinuity(expiresAtMs: 0, previousKeyB64: nil, keyGeneration: 2), accountIdB64: account)
        store.setSignedAt(1000, accountIdB64: account)
        XCTAssertEqual(store.read(accountIdB64: account)?.expiresAtMs, 1000 + ResetContinuityRules.windowMs)
    }

    func testThePreviousOwnKey() {
        XCTAssertEqual(ResetContinuityRules.previousKeyStanding(
            servedKeyB64: "OLD", logStatus: .confirmed, newKeyB64: "NEW"), "OLD")
        XCTAssertEqual(ResetContinuityRules.previousKeyStanding(
            servedKeyB64: "OLD", logStatus: .off, newKeyB64: "NEW"), "OLD")
        XCTAssertNil(ResetContinuityRules.previousKeyStanding(
            servedKeyB64: "OLD", logStatus: .other, newKeyB64: "NEW"))
        XCTAssertNil(ResetContinuityRules.previousKeyStanding(
            servedKeyB64: "OLD", logStatus: nil, newKeyB64: "NEW"))
        XCTAssertNil(ResetContinuityRules.previousKeyStanding(
            servedKeyB64: nil, logStatus: .confirmed, newKeyB64: "NEW"))
        XCTAssertNil(ResetContinuityRules.previousKeyStanding(
            servedKeyB64: "NEW", logStatus: .confirmed, newKeyB64: "NEW"))
    }

    // MARK: - The peer side (continuity.test.ts)

    func testAServedBindingIsKeptOnlyWhenItVerifiesUnderTheAttestedKey() throws {
        let person = try Identity.fromSeed(seed: Data(repeating: 9, count: 32))
        let other = try Identity.fromSeed(seed: Data(repeating: 8, count: 32))
        let now: Int64 = 1_700_000_000_000
        let binding = signContinuityBinding(accountIdentity: person, expiresAtMs: UInt64(now + 1000))
        let verify: (Data, Data, UInt64) throws -> Data = {
            try verifyContinuityBinding(binding: $0, identityPk: $1, nowMs: $2)
        }
        let target = ContinuityTargets.peerTarget(
            bindingB64: base64Encode(binding), attestedKey: person.signaturePublicKey(), nowMs: now, verify: verify)
        XCTAssertEqual(target?.expiresAtMs, now + 1000)
        XCTAssertEqual(
            target?.mailboxPkB64,
            base64Encode(MailboxKey.continuityFromAccountIdentity(accountIdentity: person).publicKey()))
        XCTAssertNil(ContinuityTargets.peerTarget(
            bindingB64: base64Encode(binding), attestedKey: other.signaturePublicKey(), nowMs: now, verify: verify))
        XCTAssertNil(ContinuityTargets.peerTarget(
            bindingB64: base64Encode(binding), attestedKey: person.signaturePublicKey(), nowMs: now + 1000,
            verify: verify))
        XCTAssertNil(ContinuityTargets.peerTarget(
            bindingB64: nil, attestedKey: person.signaturePublicKey(), nowMs: now, verify: verify))
        XCTAssertNil(ContinuityTargets.peerTarget(
            bindingB64: "AAAA", attestedKey: person.signaturePublicKey(), nowMs: now, verify: verify))
    }

    func testNewIdentityReachable() {
        let live = ContinuityTarget(mailboxPkB64: "x", expiresAtMs: 100)
        XCTAssertTrue(ContinuityTargets.newIdentityReachable(
            ordinaryCandidates: 1, continuity: nil, resetSignedAtMs: nil, nowMs: 0))
        XCTAssertTrue(ContinuityTargets.newIdentityReachable(
            ordinaryCandidates: 0, continuity: live, resetSignedAtMs: 0, nowMs: 50))
        XCTAssertFalse(ContinuityTargets.newIdentityReachable(
            ordinaryCandidates: 0, continuity: live, resetSignedAtMs: nil, nowMs: 50))
        XCTAssertFalse(ContinuityTargets.newIdentityReachable(
            ordinaryCandidates: 0, continuity: live, resetSignedAtMs: 0, nowMs: 100))
        XCTAssertFalse(ContinuityTargets.newIdentityReachable(
            ordinaryCandidates: 0, continuity: nil, resetSignedAtMs: 0, nowMs: 50))
        // The continuity key's own window, counted from signed_at.
        let far = ContinuityTarget(mailboxPkB64: "x", expiresAtMs: Int64.max)
        XCTAssertFalse(ContinuityTargets.newIdentityReachable(
            ordinaryCandidates: 0, continuity: far, resetSignedAtMs: 0, nowMs: ContinuityTargets.keyTtlMs))
    }

    func testTheTargetStoreKeepsLiveTargetsAndDropsExpiredOnes() {
        let store = ContinuityTargetStore(accountIdB64: account, defaults: defaults)
        store.remember(newKeyB64: "A", target: ContinuityTarget(mailboxPkB64: "m", expiresAtMs: 10), nowMs: 0)
        store.remember(newKeyB64: "B", target: ContinuityTarget(mailboxPkB64: "n", expiresAtMs: 100), nowMs: 0)
        XCTAssertEqual(store.target(of: "A", nowMs: 5)?.mailboxPkB64, "m")
        XCTAssertNil(store.target(of: "A", nowMs: 10))
        store.remember(newKeyB64: "B", target: nil, nowMs: 20)
        XCTAssertNil(store.target(of: "B", nowMs: 21))
        ContinuityTargetStore.clear(accountIdB64: account, defaults: defaults)
        XCTAssertNil(defaults.data(forKey: ContinuityTargetStore.prefix + account))
    }

    // MARK: - Channel returns (channel-return.test.ts)

    func testAnUnverifiedReturnIsShownNeverActedOnSilently() {
        for role in [ChannelReturnRoleName.subscriber, .publisher, .owner] {
            XCTAssertEqual(ChannelReturnRules.action(
                verified: false, role: role, hasInvitation: true, alreadyInRole: false, ownsAlready: false),
                .showInvitation)
            XCTAssertEqual(ChannelReturnRules.action(
                verified: false, role: role, hasInvitation: false, alreadyInRole: false, ownsAlready: false),
                .drop)
        }
    }

    func testTheVerifiedActions() {
        func act(_ role: ChannelReturnRoleName, inv: Bool = true, inRole: Bool = false, owns: Bool = false)
            -> ChannelReturnAction {
            ChannelReturnRules.action(
                verified: true, role: role, hasInvitation: inv, alreadyInRole: inRole, ownsAlready: owns)
        }
        XCTAssertEqual(act(.subscriber), .subscribe)
        XCTAssertEqual(act(.subscriber, inRole: true), .addSeeds)
        XCTAssertEqual(act(.subscriber, inv: false), .drop)
        XCTAssertEqual(act(.publisher), .acceptPublisher)
        XCTAssertEqual(act(.publisher, inRole: true), .addSeeds)
        XCTAssertEqual(act(.publisher, inv: false), .addSeeds)
        XCTAssertEqual(act(.owner), .recoverOwner)
        XCTAssertEqual(act(.owner, owns: true), .addSeeds)
    }

    private let card = { (k: String) in ReturnCard(identityPkB64: k, subscriberPkB64: "s" + k) }

    func testAnOwnerSendsSubscriberReturnsAndAdmitsPublishers() {
        let ch = ReturnChannel(
            idHex: "c1", ownRole: .owner, cards: [card("S"), card("P"), card("X")],
            ownerKeyB64: "ME", publisherKeysB64: ["P"])
        let out = ChannelReturnRules.returnsFor(
            channels: [ch], superseded: ["S": "acctS", "P": "acctP"], ownKeysB64: ["ME"])
        XCTAssertEqual(out.map { [$0.role.rawValue, $0.accountIdB64] }, [["subscriber", "acctS"], ["publisher", "acctP"]])
    }

    func testACoPublisherSendsSubscriberReturnsAndAnOwnerReturn() {
        let ch = ReturnChannel(
            idHex: "c1", ownRole: .publisher, cards: [card("O"), card("S"), card("P")],
            ownerKeyB64: "O", publisherKeysB64: ["P", "ME"])
        let out = ChannelReturnRules.returnsFor(
            channels: [ch], superseded: ["O": "acctO", "S": "acctS", "P": "acctP"], ownKeysB64: ["ME"])
        XCTAssertEqual(out.map { [$0.role.rawValue, $0.accountIdB64] }, [["owner", "acctO"], ["subscriber", "acctS"]])
    }

    func testASubscriberSendsOnlyAnOwnerReturnAndOnlyWithTheOwnersCard() {
        let with = ReturnChannel(
            idHex: "c1", ownRole: .subscriber, cards: [card("O"), card("S")], ownerKeyB64: "O", publisherKeysB64: [])
        XCTAssertEqual(ChannelReturnRules.returnsFor(
            channels: [with], superseded: ["O": "acctO", "S": "acctS"], ownKeysB64: []).map(\.role), [.owner])
        let without = ReturnChannel(
            idHex: "c1", ownRole: .subscriber, cards: [card("S")], ownerKeyB64: "O", publisherKeysB64: [])
        XCTAssertTrue(ChannelReturnRules.returnsFor(
            channels: [without], superseded: ["O": "acctO"], ownKeysB64: []).isEmpty)
    }

    func testNothingForThisAccountsOwnKeysAndOncePerGeneration() {
        let ch = ReturnChannel(
            idHex: "c1", ownRole: .owner, cards: [card("ME"), card("S")], ownerKeyB64: "ME", publisherKeysB64: [])
        XCTAssertEqual(ChannelReturnRules.returnsFor(
            channels: [ch], superseded: ["ME": "own", "S": "acctS"], ownKeysB64: ["ME"]).map(\.accountIdB64),
            ["acctS"])
        let planned = ChannelReturnRules.returnsFor(
            channels: [ch], superseded: ["S": "acctS"], ownKeysB64: ["ME"])
        let sent = [ChannelReturnRules.sentKey(idHex: "c1", role: .subscriber, accountIdB64: "acctS"): UInt64(3)]
        XCTAssertTrue(ChannelReturnRules.unsent(planned, sent: sent, generationOf: { _ in 3 }).isEmpty)
        XCTAssertEqual(ChannelReturnRules.unsent(planned, sent: sent, generationOf: { _ in 4 }).count, 1)
        XCTAssertTrue(ChannelReturnRules.unsent(planned, sent: [:], generationOf: { _ in nil }).isEmpty)
    }

    func testTheSeedsAReturnCarries() {
        let seeds = (0..<70).map { (epoch: UInt32($0), seed: Data(repeating: UInt8($0), count: 32)) }
            + [(epoch: UInt32(99), seed: Data([1]))]
        let out = ChannelReturnRules.newestSeeds(seeds)
        XCTAssertEqual(out.count, ChannelReturnRules.maxSeeds)
        XCTAssertEqual(out.first?.epoch, 6)
        XCTAssertEqual(out.last?.epoch, 69)
        XCTAssertEqual(out.first?.seed.first, 6)
    }

    func testTheHeldReturnsAreKeptOnceBoundedAndDropped() throws {
        let store = HeldChannelReturnStore(defaults: defaults)
        try store.hold("x", accountIdB64: account)
        try store.hold("y", accountIdB64: account)
        try store.hold("x", accountIdB64: account)
        XCTAssertEqual(store.read(accountIdB64: account), ["y", "x"])
        store.drop(["x"], accountIdB64: account)
        XCTAssertEqual(store.read(accountIdB64: account), ["y"])
        for i in 0..<(HeldChannelReturnStore.maxHeld + 5) { try store.hold("r\(i)", accountIdB64: "b") }
        XCTAssertEqual(store.read(accountIdB64: "b").count, HeldChannelReturnStore.maxHeld)
    }

    func testAReturnBuiltFromACardVerifiesUnderThePreviousKeyAndNothingElse() throws {
        let old = try Identity.fromSeed(seed: Data(repeating: 4, count: 32))
        let other = try Identity.fromSeed(seed: Data(repeating: 5, count: 32))
        let channelId = Data(repeating: 0xC1, count: 16)
        let sub = ChannelSubscriberKeypair.generate()
        let subPk = sub.public()
        let seed = Data(repeating: 7, count: 32)
        let profile = try LakkoCore.composeProfileEnvelope(
            profile: FfiProfile(displayName: "Old name", username: nil, updatedAtMs: 5,
                                accountId: nil, usernameCertificate: nil, grants: []),
            accountIdentity: old)
        let sealed = try sealMemberCard(
            seed: seed, epochId: 1, channelId: channelId, subscriberPk: subPk,
            accountIdentitySeed: old.ed25519Seed(), profile: profile)
        let opened = try openMemberCard(seed: seed, card: sealed, channelId: channelId, subscriberPk: subPk)
        let envelope = try composeChannelReturnEnvelope(ret: FfiChannelReturn(
            channelId: channelId, role: .subscriber, oldBindingSig: opened.bindingSig,
            oldSubscriberPk: subPk, profileHash: opened.profileHash, invitation: Data([1, 2, 3]),
            epochSeeds: [FfiChannelEpochSeed(epoch: 1, seed: seed)], profile: profile))
        let ret = try verifyChannelReturnCardBinding(plaintext: envelope, previousIdentityPk: old.signaturePublicKey())
        XCTAssertEqual(ChannelReturnRoleName(ret.role), .subscriber)
        XCTAssertEqual(ret.epochSeeds.map(\.epoch), [1])
        let carried = try verifyProfileEnvelope(plaintext: try XCTUnwrap(ret.profile), identityPk: old.signaturePublicKey())
        XCTAssertEqual(carried.displayName, "Old name")
        XCTAssertThrowsError(try verifyChannelReturnCardBinding(
            plaintext: envelope, previousIdentityPk: other.signaturePublicKey()))
        // A card whose profile is signed by someone else: the profile counts
        // only under the card's own key.
        XCTAssertTrue(ProfileReturn.verifies(opened.profile, underKeyB64: base64Encode(opened.identityPk)))
        XCTAssertFalse(ProfileReturn.verifies(opened.profile, underKeyB64: base64Encode(other.signaturePublicKey())))
    }

    func testAReturnCarriesTheNewestProfileHeldUnderThatAccountsRetiredKeysOnly() {
        let held: [String: (envelope: Data, updatedAtMs: UInt64)] = [
            "A1": (Data([1]), 5), "A2": (Data([2]), 9), "B1": (Data([3]), 99),
        ]
        let superseded = ["A1": "acctA", "A2": "acctA", "B1": "acctB"]
        XCTAssertEqual(
            ChannelReturnRules.returnProfile(accountIdB64: "acctA", superseded: superseded, heldEnvelopeOf: { held[$0] }),
            Data([2]), "newest stamp of that account's keys; another account's newer one is never taken")
        XCTAssertEqual(
            ChannelReturnRules.returnProfile(accountIdB64: "acctB", superseded: superseded, heldEnvelopeOf: { held[$0] }),
            Data([3]))
        XCTAssertNil(ChannelReturnRules.returnProfile(
            accountIdB64: "acctC", superseded: superseded, heldEnvelopeOf: { held[$0] }))
        XCTAssertNil(ChannelReturnRules.returnProfile(
            accountIdB64: "acctA", superseded: superseded, heldEnvelopeOf: { _ in nil }))
    }

    func testTheRecoveryKeyRegistrationsOwed() {
        XCTAssertEqual(ChannelReturnRules.owingRecoveryKey(owned: ["b", "a", "c"], registered: ["c"]), ["a", "b"])
        XCTAssertEqual(ChannelReturnRules.owingRecoveryKey(owned: [], registered: []), [])
        let store = ChannelRecoveryRegisteredStore(defaults: defaults)
        store.mark("a", accountIdB64: account)
        store.mark("a", accountIdB64: account)
        XCTAssertEqual(store.read(accountIdB64: account), ["a"])
    }

    // MARK: - Recovered channels (recovered-channels.test.ts)

    func testTheRecoveryMark() {
        XCTAssertEqual(RecoveredChannelRules.refoundAdmittedThrough(42), 42)
        XCTAssertEqual(RecoveredChannelRules.refoundAdmittedThrough(nil), 0)
        XCTAssertEqual(RecoveredChannelRules.highestSeq([3, 9, 1]), 9)
        XCTAssertEqual(RecoveredChannelRules.highestSeq([], held: 5), 5)
        let store = RecoveredChannelStore(defaults: defaults)
        store.mark("c", throughSeq: 9, accountIdB64: account)
        store.mark("c", throughSeq: 4, accountIdB64: account)
        XCTAssertEqual(store.read(accountIdB64: account), ["c": 9])
        store.clear("c", accountIdB64: account)
        XCTAssertEqual(store.read(accountIdB64: account), [:])
        XCTAssertEqual(RecoveredChannelRules.visibility(metaListed: true), .publicChannel)
        XCTAssertEqual(RecoveredChannelRules.visibility(metaListed: false), .privateChannel)
        XCTAssertNil(RecoveredChannelRules.visibility(metaListed: nil))
    }

    @MainActor
    func testReturnedInvitationsAreShownUntilTheChannelIsRead() throws {
        let c1 = String(repeating: "11", count: 16)
        let c2 = String(repeating: "22", count: 16)
        let store = ReturnedInvitationStore(defaults: defaults)
        try store.keep(channelIdHex: c2, cardB64: "card2", accountIdB64: account)
        try store.keep(channelIdHex: c1, cardB64: "card1", accountIdB64: account)
        try store.keep(channelIdHex: c1, cardB64: "card1b", accountIdB64: account)
        let held = store.read(accountIdB64: account)
        let shown = ReturnedInvitationStore.toShow(held: held, reading: [])
        XCTAssertEqual(shown.map(\.channelIdHex), [c1, c2])
        XCTAssertEqual(shown.map(\.cardB64), ["card1b", "card2"])
        XCTAssertEqual(ReturnedInvitationStore.toShow(held: held, reading: [c1]).map(\.channelIdHex), [c2])
        store.drop(channelIdsHex: [c1], accountIdB64: account)
        XCTAssertEqual(Array(store.read(accountIdB64: account).keys), [c2])
    }

    // MARK: - History (history-return.test.ts, history-page.test.ts)

    func testWhoAnswersAHistoryRequest() {
        func allowed(fromSelf: Bool = false, accepted: Bool = true, wasIn: Bool = true, held: Bool = true,
                     oneToOne: Bool = true, admin: Bool = false) -> Bool {
            HistoryReturnRules.answerAllowed(
                fromSelf: fromSelf, requesterResetAccepted: accepted, requesterWasInEarlierGroup: wasIn,
                earlierGroupHeld: held, isOneToOne: oneToOne, ownIsOwnerOrAdmin: admin)
        }
        XCTAssertTrue(allowed())
        XCTAssertTrue(allowed(oneToOne: false, admin: true))
        XCTAssertFalse(allowed(oneToOne: false, admin: false))
        XCTAssertFalse(allowed(fromSelf: true))
        XCTAssertFalse(allowed(accepted: false))
        XCTAssertFalse(allowed(wasIn: false))
        XCTAssertFalse(allowed(held: false))
    }

    func testPagingMovesBeforeToTheOldestIdAndStops() {
        let one = HistoryReturnRules.nextStep(.first, filedMessageIds: [900, 500, 700])
        XCTAssertEqual(one, HistoryRequestState(before: 500, pages: 1))
        let two = HistoryReturnRules.nextStep(one, filedMessageIds: [499, 300])
        XCTAssertEqual(two.before, 300)
        XCTAssertTrue(HistoryReturnRules.nextStep(two, filedMessageIds: []).done)
        let s = HistoryRequestState(before: 300, pages: 3, sentAtMs: 1)
        XCTAssertTrue(HistoryReturnRules.nextStep(s, filedMessageIds: [300, 400]).done)
        var capped = s
        capped.pages = HistoryReturnRules.maxPages - 1
        XCTAssertTrue(HistoryReturnRules.nextStep(capped, filedMessageIds: [10]).done)
    }

    func testARequestIsPostedOnceAgainAfterTheResendTimeAndStopsAtTheAskCap() {
        XCTAssertTrue(HistoryReturnRules.requestDue(.first, nowMs: 5))
        let sent = HistoryRequestState(sentAtMs: 1000)
        XCTAssertFalse(HistoryReturnRules.requestDue(sent, nowMs: 1000 + HistoryReturnRules.resendMs - 1))
        XCTAssertTrue(HistoryReturnRules.requestDue(sent, nowMs: 1000 + HistoryReturnRules.resendMs))
        var done = sent
        done.done = true
        XCTAssertFalse(HistoryReturnRules.requestDue(done, nowMs: Int64.max / 2))
        var state = HistoryRequestState.first
        for i in 0..<HistoryReturnRules.maxAsks {
            let at = Int64(i + 1) * HistoryReturnRules.resendMs * 2
            XCTAssertTrue(HistoryReturnRules.requestDue(state, nowMs: at))
            state = HistoryReturnRules.asked(state, nowMs: at)
        }
        XCTAssertFalse(HistoryReturnRules.requestDue(state, nowMs: Int64.max / 2))
        XCTAssertTrue(HistoryReturnRules.requestDue(
            HistoryReturnRules.nextStep(state, filedMessageIds: [7]), nowMs: Int64.max / 2))
    }

    func testTheHistoryStatesStartOnceAndRoundTrip() {
        let store = HistoryReturnStore(defaults: defaults)
        store.noteReturned(groupIdB64: "G", accountIdB64: account)
        store.write(HistoryRequestState(before: 5, pages: 2, sentAtMs: 3), groupIdB64: "G", accountIdB64: account)
        store.noteReturned(groupIdB64: "G", accountIdB64: account)
        XCTAssertEqual(store.read(accountIdB64: account), ["G": HistoryRequestState(before: 5, pages: 2, sentAtMs: 3)])
    }

    func testTheRowsOfAHistoryPage() {
        let rows: [String] = ["10", "3", "7", "x", "0", "12", "5"]
        let id: (String) -> Int64? = { Int64($0) }
        XCTAssertEqual(HistoryReturnRules.pageRows(rows, messageId: id, before: 0, pageSize: 3), ["7", "10", "12"])
        XCTAssertEqual(HistoryReturnRules.pageRows(rows, messageId: id, before: 7, pageSize: 10), ["3", "5"])
        XCTAssertEqual(HistoryReturnRules.pageRows(rows, messageId: id, before: 3, pageSize: 10), [])
        XCTAssertEqual(HistoryReturnRules.pageRows(rows, messageId: id, before: 0, pageSize: 0), [])
    }

    // MARK: - Profiles (profile-signer.test.ts)

    func testTheRoute() {
        XCTAssertEqual(ProfileReturn.route(verifiesUnderSender: true, verifiesUnderPreviousOwnKey: true), .file)
        XCTAssertEqual(ProfileReturn.route(verifiesUnderSender: false, verifiesUnderPreviousOwnKey: true), .returnedOwn)
        XCTAssertEqual(ProfileReturn.route(verifiesUnderSender: false, verifiesUnderPreviousOwnKey: false), .drop)
    }

    func testAProfileAPeerCarriesBackVerifiesUnderThePersonsOldKeyNotThePeers() throws {
        let person = try Identity.fromSeed(seed: Data(repeating: 1, count: 32))
        let peer = try Identity.fromSeed(seed: Data(repeating: 2, count: 32))
        let env = try LakkoCore.composeProfileEnvelope(
            profile: FfiProfile(displayName: "Anna", username: nil, updatedAtMs: 7,
                                accountId: nil, usernameCertificate: nil, grants: []),
            accountIdentity: person)
        let underPeer = ProfileReturn.verifies(env, underKeyB64: base64Encode(peer.signaturePublicKey()))
        let underPerson = ProfileReturn.verifies(env, underKeyB64: base64Encode(person.signaturePublicKey()))
        XCTAssertFalse(underPeer)
        XCTAssertTrue(underPerson)
        XCTAssertEqual(
            ProfileReturn.route(verifiesUnderSender: underPeer, verifiesUnderPreviousOwnKey: underPerson),
            .returnedOwn)
        XCTAssertFalse(ProfileReturn.verifies(env, underKeyB64: nil))
    }

    func testAdoptingAReturnedOwnProfileTakesOnlyAStrictlyNewerNonZeroStamp() {
        XCTAssertTrue(ProfileReturn.adopts(candidateStamp: 5, heldStamp: 4))
        XCTAssertFalse(ProfileReturn.adopts(candidateStamp: 5, heldStamp: 5))
        XCTAssertFalse(ProfileReturn.adopts(candidateStamp: 0, heldStamp: 0))
    }

    func testTheNewestProfileHeldForAResetPerson() {
        let held: [String: (envelope: Data, updatedAtMs: UInt64)] = [
            "A": (Data([1]), 5), "B": (Data([2]), 9),
        ]
        XCTAssertEqual(ProfileReturn.newestHeldEnvelope(keysB64: ["A", "B", "C"], heldEnvelopeOf: { held[$0] }), Data([2]))
        XCTAssertNil(ProfileReturn.newestHeldEnvelope(keysB64: ["C"], heldEnvelopeOf: { _ in nil }))
        let carried = ProfileCarriedStore(defaults: defaults)
        XCTAssertFalse(carried.carried(accountIdB64: account, successorGroupIdB64: "S"))
        carried.mark(accountIdB64: account, successorGroupIdB64: "S")
        XCTAssertTrue(carried.carried(accountIdB64: account, successorGroupIdB64: "S"))
    }

    // MARK: - Raw profile envelopes and their route (ADR-0101 section 8)

    private final class Memory: ProfilePersistence {
        var blobs: [String: Data] = [:]
        func load(accountIdB64: String) -> Data? { blobs[accountIdB64] }
        func save(_ data: Data, accountIdB64: String) { blobs[accountIdB64] = data }
        func delete(accountIdB64: String) { blobs[accountIdB64] = nil }
    }

    @MainActor
    private func directory(account: String) -> ProfileDirectory {
        let directory = ProfileDirectory(
            received: ReceivedProfileStore(
                persistence: Memory(),
                isNewer: { profileIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1) }),
            verifier: UsernameVerifier(resolve: { _ in .notRegistered }, defaults: defaults),
            ownStore: OwnProfileStore(defaults: defaults))
        directory.accountIdentityProvider = TestProfileSigning.provider
        directory.verifiesOnResolve = false
        directory.setRenames(from: [])
        directory.activate(accountIdB64: account, username: "me")
        return directory
    }

    private func signedClaim(_ identity: Identity, name: String, stamp: UInt64) throws -> ProfileClaim {
        let envelope = try LakkoCore.composeProfileEnvelope(
            profile: FfiProfile(displayName: name, username: nil, updatedAtMs: stamp,
                                accountId: nil, usernameCertificate: nil, grants: []),
            accountIdentity: identity)
        return try XCTUnwrap(ChatClient.decodeProfileIfEnvelope(envelope))
    }

    @MainActor
    func testAGroupProfileIsFiledOnlyUnderTheKeyThatSignedIt() throws {
        let account = "route-\(UUID().uuidString)"
        let d = directory(account: account)
        let anna = try Identity.fromSeed(seed: Data(repeating: 0x21, count: 32))
        let bob = try Identity.fromSeed(seed: Data(repeating: 0x22, count: 32))
        let claim = try signedClaim(anna, name: "Anna", stamp: 9)
        XCTAssertNotNil(claim.envelope, "the receive path keeps the envelope verbatim")
        // Sent by Bob, signed by Anna: names nobody.
        d.fileReceivedGroupProfile(claim, senderKeyB64: base64Encode(bob.signaturePublicKey()), accountIdB64: account)
        XCTAssertNil(d.storedProfile(signatureKeyB64: base64Encode(bob.signaturePublicKey())))
        XCTAssertNil(d.storedProfile(signatureKeyB64: base64Encode(anna.signaturePublicKey())))
        // Sent by Anna herself: filed, with its envelope, and carried as is.
        d.fileReceivedGroupProfile(claim, senderKeyB64: base64Encode(anna.signaturePublicKey()), accountIdB64: account)
        let stored = try XCTUnwrap(d.storedProfile(signatureKeyB64: base64Encode(anna.signaturePublicKey())))
        XCTAssertEqual(stored.displayName, "Anna")
        XCTAssertEqual(stored.envelope, claim.envelope)
        XCTAssertEqual(d.heldEnvelope(signatureKeyB64: base64Encode(anna.signaturePublicKey()))?.envelope, claim.envelope)
        let carried = StateBundle.carriedProfiles(d.receivedProfiles(), ownKeysB64: [])
        XCTAssertEqual(carried.map(\.envelopeB64), [base64Encode(try XCTUnwrap(claim.envelope))],
                       "the received bytes, never a re-signed copy")
    }

    @MainActor
    func testAProfileSignedByThisAccountsPreviousKeyComesBackAsItsOwn() throws {
        let account = "returned-\(UUID().uuidString)"
        let store = ResetContinuityStore()
        defer { store.clear(accountIdB64: account) }
        let old = try Identity.fromSeed(seed: Data(repeating: 0x31, count: 32))
        let peer = try Identity.fromSeed(seed: Data(repeating: 0x32, count: 32))
        store.write(
            ResetContinuity(expiresAtMs: Int64.max, previousKeyB64: base64Encode(old.signaturePublicKey()), keyGeneration: 2),
            accountIdB64: account)
        let d = directory(account: account)
        var returned = 0
        d.onOwnProfileReturned = { returned += 1 }
        let claim = try signedClaim(old, name: "Old me", stamp: 50)
        d.fileReceivedGroupProfile(claim, senderKeyB64: base64Encode(peer.signaturePublicKey()), accountIdB64: account)
        XCTAssertEqual(d.ownProfile?.displayName, "Old me")
        XCTAssertEqual(d.ownProfile?.updatedAtMs, 50)
        XCTAssertEqual(returned, 1)
        XCTAssertNil(d.storedProfile(signatureKeyB64: base64Encode(peer.signaturePublicKey())), "never filed under the peer")
        // Not newer than what is held now: not adopted again.
        XCTAssertFalse(d.offerReturnedOwnProfile(envelope: try XCTUnwrap(claim.envelope), accountIdB64: account))
        XCTAssertEqual(returned, 1)
    }

    func testAStoredClaimKeepsItsEnvelopeAcrossCodableAndTheSameStampLearnsIt() throws {
        let claim = ProfileClaim.signed(displayName: "Anna", updatedAtMs: 7)
        let decoded = try JSONDecoder().decode(ProfileClaim.self, from: try JSONEncoder().encode(claim))
        XCTAssertEqual(decoded.envelope, claim.envelope)
    }

    // MARK: - Live walk 2026-11-26

    /// D3 — the answerer leaves out of a one-to-one's history page the own
    /// messages it sends again into the successor (after the reset was
    /// signed); a group sends nothing again and leaves nothing out.
    func testAHistoryPageLeavesOutWhatIsSentAgainAsAReSend() {
        func row(_ id: Int64, _ direction: String, _ at: String) -> PersistedMessage {
            PersistedMessage(messageId: id, direction: direction, text: "m\(id)", attachments: [], createdAt: at)
        }
        let signedAt = ChatRepository.parseDsTimestampMs("2026-11-26T10:50:00Z")!
        let rows = [
            row(1, "outgoing", "2026-11-26T10:00:00Z"),
            row(2, "incoming", "2026-11-26T10:55:00Z"),
            row(3, "outgoing", "2026-11-26T10:55:00Z"),
        ]
        let page = HistoryReturnRules.withoutResent(rows, isOneToOne: true, resetSignedAtMs: signedAt)
        XCTAssertEqual(page.map(\.messageId), [1, 2])
        XCTAssertEqual(HistoryReturnRules.withoutResent(rows, isOneToOne: false, resetSignedAtMs: signedAt), rows)
        XCTAssertEqual(HistoryReturnRules.withoutResent(rows, isOneToOne: true, resetSignedAtMs: nil), rows)
    }

    /// D4 — the founder holds the ownership back until the history was
    /// served, at most ``HistoryServedStore/waitMs``.
    func testTheOwnershipHandOverWaitsForTheHistoryBounded() {
        let store = HistoryServedStore(defaults: defaults)
        XCTAssertFalse(store.transferDue(successorGroupIdB64: "S", accountIdB64: account, nowMs: 1_000),
                       "the first ask starts the wait")
        XCTAssertFalse(store.transferDue(successorGroupIdB64: "S", accountIdB64: account, nowMs: 2_000))
        XCTAssertTrue(store.transferDue(
            successorGroupIdB64: "S", accountIdB64: account, nowMs: 1_000 + HistoryServedStore.waitMs))
        XCTAssertFalse(store.transferDue(successorGroupIdB64: "T", accountIdB64: account, nowMs: 5_000))
        store.markServed(successorGroupIdB64: "T", accountIdB64: account, nowMs: 6_000)
        XCTAssertTrue(store.transferDue(successorGroupIdB64: "T", accountIdB64: account, nowMs: 6_001))
        XCTAssertFalse(HistoryServedStore.transferDue(
            HistoryServedStore.Entry(firstHeldMs: 10_000, served: false), nowMs: 9_000),
            "a clock that went back reads as just held back")
        store.clear(accountIdB64: account)
        XCTAssertTrue(store.read(accountIdB64: account).isEmpty)
    }

    /// D1 — a subscriber is told no owner: the owner return goes to the
    /// key that signed its invitation; a publishing device never uses it.
    func testTheOwnerReturnTargetOfASubscriberIsItsInviter() {
        let inviter = Data(repeating: 0x51, count: 32)
        let owner = Data(repeating: 0x52, count: 32)
        var extras = ChannelExtras()
        XCTAssertNil(ChannelReturnRules.ownerKeyB64(extras: extras, ownRole: .subscriber))
        extras.inviterAccountKey = inviter
        XCTAssertEqual(ChannelReturnRules.ownerKeyB64(extras: extras, ownRole: .subscriber), base64Encode(inviter))
        XCTAssertNil(ChannelReturnRules.ownerKeyB64(extras: extras, ownRole: .publisher))
        extras.ownerAccountKey = owner
        XCTAssertEqual(ChannelReturnRules.ownerKeyB64(extras: extras, ownRole: .subscriber), base64Encode(owner))
        XCTAssertEqual(ChannelReturnRules.ownerKeyB64(extras: extras, ownRole: .publisher), base64Encode(owner))
        // A subscriber channel with the inviter as owner plans an owner
        // return when that key reset and its card is listed.
        let card = ReturnCard(identityPkB64: base64Encode(inviter), subscriberPkB64: "c3Vi")
        let planned = ChannelReturnRules.returnsFor(
            channels: [ReturnChannel(
                idHex: "ab", ownRole: .subscriber, cards: [card],
                ownerKeyB64: ChannelReturnRules.ownerKeyB64(
                    extras: { var e = ChannelExtras(); e.inviterAccountKey = inviter; return e }(),
                    ownRole: .subscriber),
                publisherKeysB64: [])],
            superseded: [base64Encode(inviter): "acct-anya"], ownKeysB64: [])
        XCTAssertEqual(planned.map(\.role), [.owner])
    }
}
