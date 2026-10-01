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

    private let accountSeed = Data(repeating: 0x21, count: 32)
    private let groupA = Data(repeating: 0xA1, count: 32)
    private let groupB = Data(repeating: 0xB2, count: 32)
    private let emptyState = FfiReachState(entries: [], updatedAtMs: 0)

    private func setting(
        mode: FfiVisibilityMode, fof: Bool = false, groups: FfiGroupsForm? = nil,
        chosen: [FfiScopeId] = []
    ) -> FfiVisibilitySetting {
        FfiVisibilitySetting(
            mode: mode, contactsOfContacts: fof, groups: groups, chosen: chosen,
            lastContactsOfContacts: false, lastGroups: nil, updatedAtMs: 1)
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

    /// Nobody: still `contact` (contacts always write) and `member` per group
    /// (a co-member must always deliver a Welcome for a new device), never
    /// `group` or `fof`.
    func testNobodyRegistersContactAndMemberKeysOnly() {
        let specs = desired(setting(mode: .nobody))
        XCTAssertEqual(classes(specs), ["contact", "member:161", "member:178"])
        XCTAssertFalse(ReachRules.mailboxOpen(setting(mode: .nobody)))
    }

    /// The default for a new account (2.1 + 2.2.1): `group` for every group
    /// and `fof`.
    func testTheDefaultRegistersGroupAndFofKeys() {
        let specs = desired(defaultVisibilitySetting())
        XCTAssertEqual(
            Set(classes(specs)),
            ["contact", "member:161", "member:178", "group:161", "group:178", "fof"])
    }

    /// Only the ones I choose: `group` only for the chosen group.
    func testChosenRegistersGroupKeysForTheChosenGroupOnly() {
        let specs = desired(setting(
            mode: .some, groups: .chosen, chosen: [FfiScopeId(kind: .group, id: groupB)]))
        XCTAssertTrue(classes(specs).contains("group:178"))
        XCTAssertFalse(classes(specs).contains("group:161"))
        XCTAssertFalse(classes(specs).contains("fof"))
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

    func testARotationBumpsOneScopeOnly() {
        // Every scope starts at 0, as on web and Android (ADR-0060 section 1).
        let bumped = ReachRules.bumping(.member, scope: groupA, in: emptyState, nowMs: 50)
        XCTAssertEqual(ReachRules.version(of: .member, scope: groupA, in: bumped), 1)
        XCTAssertEqual(ReachRules.version(of: .member, scope: groupB, in: bumped), 0)
        XCTAssertEqual(ReachRules.version(of: .contact, scope: Data(), in: bumped), 0)
        let again = ReachRules.bumping(.member, scope: groupA, in: bumped, nowMs: 10)
        XCTAssertEqual(ReachRules.version(of: .member, scope: groupA, in: again), 2)
        XCTAssertGreaterThan(again.updatedAtMs, bumped.updatedAtMs, "the stamp never goes back")
    }

    // MARK: - Which received grant is kept (C-68)

    private func grant(_ c: FfiReachClass, scope: Data = Data(repeating: 7, count: 16)) throws -> FfiReachGrant {
        let s: Data = (c == .contact || c == .fof) ? Data() : scope
        return try deriveReachGrant(accountSeed: accountSeed, class: c, scopeId: s, version: 1)
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
            grant: try grant(.member, scope: Data(repeating: 8, count: 16)), scopeHex: "bb", storedAtMs: 2)
        let reply = ReachRules.HeldGrant(grant: try grant(.reply), scopeHex: nil, storedAtMs: 3)
        let contact = ReachRules.HeldGrant(grant: try grant(.contact), scopeHex: nil, storedAtMs: 4)
        XCTAssertEqual(
            ReachRules.writeSeed(from: [reply, otherMember, member], groupIdHex: "aa"), member.seed)
        XCTAssertEqual(
            ReachRules.writeSeed(from: [reply, member, contact], groupIdHex: "aa"), contact.seed)
        XCTAssertEqual(ReachRules.writeSeed(from: [reply], groupIdHex: nil), reply.seed)
        XCTAssertNil(ReachRules.writeSeed(from: [], groupIdHex: "aa"))
    }

    /// A replayed older version never displaces the newer grant of one scope.
    func testMergingKeepsTheNewestVersion() throws {
        let v2 = try deriveReachGrant(accountSeed: accountSeed, class: .member, scopeId: groupA, version: 2)
        let v1 = try deriveReachGrant(accountSeed: accountSeed, class: .member, scopeId: groupA, version: 1)
        let held = ReachRules.merging(
            ReachRules.HeldGrant(grant: v2, scopeHex: "aa", storedAtMs: 1), into: [])
        let replayed = ReachRules.merging(
            ReachRules.HeldGrant(grant: v1, scopeHex: "aa", storedAtMs: 9), into: held)
        XCTAssertEqual(replayed.map(\.version), [2])
        let v3 = try deriveReachGrant(accountSeed: accountSeed, class: .member, scopeId: groupA, version: 3)
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
            ds: DsClient(baseURL: "http://127.0.0.1:1"))
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

    /// A rotation changes the grant handed out for exactly that scope.
    func testRotatingAGroupChangesItsGrants() throws {
        let c = try center()
        let before = c.profileGrants(groupId: groupA, oneToOne: false)
        c.rotateGroup(groupA)
        let after = c.profileGrants(groupId: groupA, oneToOne: false)
        XCTAssertEqual(after.map(\.version), before.map { $0.version + 1 })
        XCTAssertNotEqual(after.map(\.seed), before.map(\.seed))
        XCTAssertEqual(c.profileGrants(groupId: groupB, oneToOne: false).map(\.version), [0, 0])
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
        let mailbox = try IdentityPseudonym.fromIdentity(pubkey: peer).asBytes()
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

        ds.registered = [DsReachKeyAdd(publicKey: try pk(seed, .member, groupA, 2), expiresAtMs: nil)]
        c.groupIdsProvider = { [self.groupA] }
        ds.updates.removeAll()
        _ = await c.syncNow()

        XCTAssertEqual(ds.reads, 2, "the new group's slots were looked up")
        let added = Set(ds.updates.flatMap { $0.add.map(\.publicKey) })
        XCTAssertTrue(added.contains(try pk(seed, .member, groupA, 2)))
        XCTAssertFalse(added.contains(try pk(seed, .member, groupA, 0)))
        XCTAssertFalse(added.contains(try pk(seed, .member, groupA, 1)))
    }

    /// The pure recovery: the HIGHEST registered version per slot, never one
    /// above the probe limit, nothing for a slot with no registered key.
    func testRecoveredVersionsTakeTheHighestRegisteredOne() throws {
        let contact = ReachRules.VersionSlot(reachClass: .contact, scopeId: Data())
        let member = ReachRules.VersionSlot(reachClass: .member, scopeId: groupA)
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
        let member = try deriveReachGrant(accountSeed: accountSeed, class: .member, scopeId: groupA, version: 3)
        let contact = try grant(.contact)
        XCTAssertTrue(c.keep(member, carrier: .groupProfile, fromAccountKeyB64: peerB64, scopeHex: groupHex))
        XCTAssertTrue(c.keep(contact, carrier: .oneToOneProfile(fromOtherParticipant: true),
                             fromAccountKeyB64: peerB64, scopeHex: "aa"))
        let mailbox = try IdentityPseudonym.fromIdentity(pubkey: peer).asBytes()

        XCTAssertEqual(
            c.writeAuthCandidates(toMailbox: mailbox, groupIdHex: groupHex).map(\.reachSeed),
            [contact.seed, member.seed])

        let own = try XCTUnwrap(c.ownAccountKey)
        let ownMailbox = try IdentityPseudonym.fromIdentity(pubkey: own).asBytes()
        XCTAssertEqual(c.writeAuthCandidates(toMailbox: ownMailbox).map(\.reachSeed), [c.ownContactSeed()])

        let stranger = try IdentityPseudonym.fromIdentity(
            pubkey: try Identity.fromSeed(seed: Data(repeating: 0x42, count: 32)).signaturePublicKey()
        ).asBytes()
        XCTAssertTrue(c.writeAuthCandidates(toMailbox: stranger).isEmpty, "no key, no candidate")
    }

    /// F — a grant kept for someone fires the retry hook with their key.
    func testKeepingAGrantFiresTheRetryHook() throws {
        let c = try center()
        var fired: [String] = []
        c.onGrantKept = { fired.append($0) }
        let peerB64 = Data(repeating: 9, count: 32).base64EncodedString()
        XCTAssertTrue(c.keep(try grant(.contact), carrier: .oneToOneProfile(fromOtherParticipant: true),
                             fromAccountKeyB64: peerB64, scopeHex: "aa"))
        XCTAssertEqual(fired, [peerB64])
    }
}
