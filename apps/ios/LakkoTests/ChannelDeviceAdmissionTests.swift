// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0090 — the client rules of a PublisherGroup of devices.
@MainActor
final class ChannelDeviceAdmissionTests: XCTestCase {

    private let owner = Data(repeating: 0x0A, count: 32)
    private let copub = Data(repeating: 0x0C, count: 32)
    private let channel = Data(repeating: 0x11, count: 16)
    private func leaf(_ b: UInt8) -> Data { Data(repeating: b, count: 32) }

    // MARK: section 2.1

    func testAMissingRosterDeviceIsAdmittedAndAGoneOneRetiredButNeverSelf() {
        let leaves = [(leaf: leaf(1), account: owner), (leaf: leaf(2), account: owner),
                      (leaf: leaf(9), account: copub)]
        let diff = ChannelDeviceAdmission.ownDeviceDiff(
            leaves: leaves, accountKey: owner, roster: [leaf(1), leaf(3)], selfLeaf: leaf(1))
        XCTAssertEqual(diff.missing, [leaf(3)])
        XCTAssertEqual(diff.retire, [leaf(2)])
        let selfGone = ChannelDeviceAdmission.ownDeviceDiff(
            leaves: leaves, accountKey: owner, roster: [leaf(2)], selfLeaf: leaf(1))
        XCTAssertEqual(selfGone.retire, [], "a device never retires its own leaf")
        let none = ChannelDeviceAdmission.ownDeviceDiff(
            leaves: leaves, accountKey: owner, roster: [leaf(1), leaf(2)], selfLeaf: leaf(1))
        XCTAssertTrue(none.missing.isEmpty && none.retire.isEmpty)
    }

    func testAKeyPackageIsAdmittedOnlyForAMissingDeviceBoundToTheAccount() {
        let packages = [
            DeviceKeyPackage(deviceId: leaf(3), keyPackage: Data("kp-own".utf8)),
            DeviceKeyPackage(deviceId: leaf(4), keyPackage: Data("kp-foreign".utf8)),
            DeviceKeyPackage(deviceId: leaf(5), keyPackage: Data("kp-not-missing".utf8)),
        ]
        let admitted = ChannelDeviceAdmission.admittable(
            keyPackages: packages, missing: [leaf(3), leaf(4)], accountKey: owner,
            accountKeyOf: { kp in kp == Data("kp-foreign".utf8) ? self.copub : self.owner })
        XCTAssertEqual(admitted, [Data("kp-own".utf8)])
    }

    // MARK: section 2.2 / 2.4

    private func action(
        _ a: FfiPublisherDeviceAction, sender: Data?, requestChannel: Data? = nil,
        leaves: [(leaf: Data, account: Data)]? = nil, kpAccount: Data? = nil, kpLeaf: Data? = nil
    ) -> ChannelDeviceAdmission.DeviceRequestAction? {
        ChannelDeviceAdmission.deviceRequestAction(
            channelId: channel, requestChannelId: requestChannel ?? channel, action: a, sender: sender,
            ownerAccountKey: owner,
            leaves: leaves ?? [(leaf(1), owner), (leaf(9), copub)],
            keyPackageAccountKey: { _ in kpAccount ?? self.copub },
            keyPackageLeaf: { _ in kpLeaf ?? self.leaf(10) })
    }

    func testAnOwnerDeviceAdmitsAPublishersDeviceOnlyWhenEveryRuleHolds() {
        let admit = FfiPublisherDeviceAction.admit(
            keyPackage: Data("kp".utf8), mailboxBinding: Data(count: 97), delegations: [])
        XCTAssertEqual(action(admit, sender: copub), .admit(keyPackage: Data("kp".utf8), account: copub))
        XCTAssertNil(action(admit, sender: nil), "sender unresolved")
        XCTAssertNil(action(admit, sender: leaf(77)), "not a current publisher")
        XCTAssertNil(action(admit, sender: copub, requestChannel: Data(repeating: 0x22, count: 16)),
                     "another channel")
        XCTAssertNil(action(admit, sender: copub, kpAccount: owner), "KeyPackage bound to another account")
        XCTAssertNil(action(admit, sender: owner, kpAccount: owner), "owner devices come by the roster")
        XCTAssertNil(action(admit, sender: copub, kpLeaf: leaf(9)), "already a member")
        let full = (0..<8).map { (leaf: leaf(UInt8(20 + $0)), account: copub) } + [(leaf: leaf(1), account: owner)]
        XCTAssertNil(action(admit, sender: copub, leaves: full), "eight leaves already")
    }

    func testARetireRequestNamesALeafOfTheSendersOwnAccount() {
        XCTAssertEqual(action(.retire(deviceKey: leaf(9)), sender: copub), .retire(leaf: leaf(9), account: copub))
        XCTAssertNil(action(.retire(deviceKey: leaf(1)), sender: copub), "not the sender's leaf")
        XCTAssertEqual(action(.retire(deviceKey: leaf(1)), sender: owner), .retire(leaf: leaf(1), account: owner),
                       "a forgotten owner device asks another owner device")
    }

    // MARK: section 6

    func testTheAdmissionMarkIsKeptAsTheMaximumAndSentOnlyWhenItAdvanced() {
        XCTAssertEqual(ChannelDeviceAdmission.mergedMark(held: 5, received: 3), 5)
        XCTAssertEqual(ChannelDeviceAdmission.mergedMark(held: nil, received: 3), 3)
        XCTAssertEqual(ChannelDeviceAdmission.markOwed(mark: 7, sent: 5), 7)
        XCTAssertNil(ChannelDeviceAdmission.markOwed(mark: 5, sent: 5))
        XCTAssertNil(ChannelDeviceAdmission.markOwed(mark: nil, sent: nil))
    }

    /// Security pass — a co-publisher's mark is ignored; only the owner's
    /// devices' marks are taken.
    func testACoPublishersAdmissionMarkIsIgnored() {
        XCTAssertFalse(ChannelDeviceAdmission.takesMark(sender: copub, ownerAccountKey: owner))
        XCTAssertFalse(ChannelDeviceAdmission.takesMark(sender: nil, ownerAccountKey: owner))
        XCTAssertFalse(ChannelDeviceAdmission.takesMark(sender: owner, ownerAccountKey: nil))
        XCTAssertTrue(ChannelDeviceAdmission.takesMark(sender: owner, ownerAccountKey: owner))
    }

    /// Security pass — an owner's mark above the rows this device has read is
    /// clamped to them; the rest counts once this device has read that far.
    func testAnOwnersMarkAboveWhatThisDeviceReadIsClamped() {
        XCTAssertEqual(ChannelDeviceAdmission.clampedMark(held: 3, received: 1_000_000, readThrough: 40), 40)
        XCTAssertEqual(ChannelDeviceAdmission.clampedMark(held: 3, received: 1_000_000, readThrough: nil), 3,
                       "nothing read yet: nothing counts")
        XCTAssertEqual(ChannelDeviceAdmission.clampedMark(held: 50, received: 60, readThrough: 40), 50,
                       "never lowers the held mark")
        XCTAssertEqual(ChannelDeviceAdmission.clampedMark(held: nil, received: 20, readThrough: 40), 20)
        let state = ChannelsUiState(accountIdB64: "mark-\(UUID().uuidString)")
        let id = String(repeating: "ab", count: 16)
        state.updateExtras(id) {
            $0.admittedThroughSeq = 3
            $0.admissionMarkReceived = 1_000
            $0.subscriberRowsReadSeq = 40
        }
        ChannelDeviceAdmission.applyReceivedMark(channelId: id, state: state)
        XCTAssertEqual(state.extras(id).admittedThroughSeq, 40)
        state.updateExtras(id) { $0.subscriberRowsReadSeq = 2_000 }
        ChannelDeviceAdmission.applyReceivedMark(channelId: id, state: state)
        XCTAssertEqual(state.extras(id).admittedThroughSeq, 1_000, "pending until read that far")
    }

    func testEverySeedOfAVersionIsHeldAndHandedOver() {
        let a = Data(repeating: 0x01, count: 32)
        let b = Data(repeating: 0x02, count: 32)
        let merged = ChannelInvites.mergedAdmissionSeeds(
            local: [(seed: a, version: 1)], register: [(seed: b, version: 1), (seed: a, version: 1)])
        XCTAssertEqual(merged.map(\.seed), [a, b], "two seeds of one version, each once")
        let owed = ChannelAdmissionSeedShare.seedsOwed(
            held: merged, sentIds: [ChannelAdmissionSeedShare.seedId(a)])
        XCTAssertEqual(owed.map(\.seed), [b], "the second seed of a version still goes")
        XCTAssertNotEqual(ChannelAdmissionSeedShare.seedId(a), ChannelAdmissionSeedShare.seedId(b))
    }

    // MARK: section 2.5 / section 4

    func testOnlyALoneDeviceThatFoundedItsDeviceGroupRefoundsAnOwnedChannel() {
        XCTAssertTrue(ChannelDeviceAdmission.shouldRefound(
            foundedAlone: true, rosterCount: 1, accountRole: ChannelAccountRole.owner, holdsGroup: false))
        XCTAssertFalse(ChannelDeviceAdmission.shouldRefound(
            foundedAlone: false, rosterCount: 1, accountRole: ChannelAccountRole.owner, holdsGroup: false))
        XCTAssertFalse(ChannelDeviceAdmission.shouldRefound(
            foundedAlone: true, rosterCount: 2, accountRole: ChannelAccountRole.owner, holdsGroup: false),
            "a device with siblings waits")
        XCTAssertFalse(ChannelDeviceAdmission.shouldRefound(
            foundedAlone: true, rosterCount: 1, accountRole: ChannelAccountRole.coPublisher, holdsGroup: false))
        XCTAssertFalse(ChannelDeviceAdmission.shouldRefound(
            foundedAlone: true, rosterCount: 1, accountRole: ChannelAccountRole.owner, holdsGroup: true))
        XCTAssertFalse(ChannelDeviceAdmission.shouldRefound(
            foundedAlone: true, rosterCount: nil, accountRole: ChannelAccountRole.owner, holdsGroup: false))
    }

    /// Security pass — refused reads (an unauthenticated DS signal) make a
    /// reset suspected only on a device with a sibling, and then the device
    /// stops posting but its group stays.
    func testRefusedReadsKeepTheGroupAndStopPosting() throws {
        XCTAssertFalse(ChannelDeviceAdmission.suspectsReset(refusedReads: 2, rosterCount: 2))
        XCTAssertFalse(ChannelDeviceAdmission.suspectsReset(refusedReads: 9, rosterCount: 1),
                       "never on the account's only device")
        XCTAssertTrue(ChannelDeviceAdmission.suspectsReset(refusedReads: 3, rosterCount: 2))

        let accountSeed = Data(repeating: 0x47, count: 32)
        let a = try Identity.fromSeedBound(
            seed: Data(repeating: 0x57, count: 32), accountSeed: accountSeed, issuedAtUnixSecs: 1_700_000_000)
        let group = try PublisherGroup.create(identity: a, ownerAccountSeed: accountSeed, channelId: channel)
        let epoch = try group.epoch()
        let state = ChannelsUiState(accountIdB64: "reset-\(UUID().uuidString)")
        let id = channel.map { String(format: "%02x", $0) }.joined()
        state.setKeys(channelId: id, keys: ChannelPublisherKeys(
            seed: Data(repeating: 1, count: 32), signingKeySeed: Data(repeating: 2, count: 32)))
        let roster = ChannelDeviceAdmission.deviceGroupRoster
        ChannelDeviceAdmission.deviceGroupRoster = { [self.leaf(1), self.leaf(2)] }
        defer { ChannelDeviceAdmission.deviceGroupRoster = roster }
        for _ in 0..<ChannelDeviceAdmission.refusedReadsToDrop {
            ChannelDeviceAdmission.noteRefusedRead(channelId: id, state: state, identity: a, group: group)
        }
        XCTAssertNil(state.keysByChannel[id], "no more posts under the old seed")
        XCTAssertEqual(state.extras(id).resetSuspected, true)
        XCTAssertEqual(try group.epoch(), epoch, "the group state is kept")
        XCTAssertTrue(try group.isActive())
    }

    /// A suspicion from refused reads lifts by itself: two signed reads in a
    /// row with the held group's key and nothing unaccounted lift it; a row
    /// past the held epoch the group did not apply keeps it.
    func testASuspectedResetLiftsAfterTwoGoodReadsAtTheMatchingEpoch() {
        XCTAssertFalse(ChannelDeviceAdmission.liftsSuspicion(successfulReads: 1, unaccountedRows: 0))
        XCTAssertTrue(ChannelDeviceAdmission.liftsSuspicion(successfulReads: 2, unaccountedRows: 0))
        XCTAssertFalse(ChannelDeviceAdmission.liftsSuspicion(successfulReads: 2, unaccountedRows: 1),
                       "a mismatching epoch keeps it suspended")
        XCTAssertTrue(ChannelDeviceAdmission.isUnaccounted(rowEpoch: 8, heldEpoch: 7, processed: false),
                      "a commit of a chain this group does not hold")
        XCTAssertFalse(ChannelDeviceAdmission.isUnaccounted(rowEpoch: 8, heldEpoch: 7, processed: true))
        XCTAssertFalse(ChannelDeviceAdmission.isUnaccounted(rowEpoch: 7, heldEpoch: 7, processed: false),
                       "this device's own message at its epoch")
        XCTAssertFalse(ChannelDeviceAdmission.isUnaccounted(rowEpoch: nil, heldEpoch: 7, processed: false))
    }

    /// Security pass — a Welcome for the same group from a device of the
    /// owner replaces a held group; any other does not.
    func testAnOwnerSignedWelcomeForTheSameGroupReplacesTheHeldOne() throws {
        let accountSeed = Data(repeating: 0x48, count: 32)
        let restored = try Identity.fromSeedBound(
            seed: Data(repeating: 0x58, count: 32), accountSeed: accountSeed, issuedAtUnixSecs: 1_700_000_000)
        let old = try Identity.fromSeedBound(
            seed: Data(repeating: 0x59, count: 32), accountSeed: accountSeed, issuedAtUnixSecs: 1_700_000_000)
        let ownerKey = try publisherOwnerAccountKey(ownerAccountSeed: accountSeed)
        // The old device holds a group at the channel's id; the restored one
        // founded it again (decision A).
        let held = try PublisherGroup.create(identity: old, ownerAccountSeed: accountSeed, channelId: channel)
        let refounded = try PublisherGroup.create(identity: restored, ownerAccountSeed: accountSeed, channelId: channel)
        let groupId = try refounded.groupId()
        let staged = try refounded.stageCommit(
            identity: restored, admit: [try old.createKeyPackage()], retire: [], rotate: false)
        XCTAssertTrue(try refounded.mergeStagedCommit(identity: restored))
        let welcome = try XCTUnwrap(staged.welcome)

        let founder = try peekWelcomeFounder(welcome: welcome, identity: old)
        XCTAssertTrue(ChannelDeviceAdmission.welcomeReplaces(
            founder: founder, groupId: groupId, ownerAccountKey: ownerKey))
        XCTAssertFalse(ChannelDeviceAdmission.welcomeReplaces(
            founder: founder, groupId: groupId, ownerAccountKey: copub), "not from the owner")
        XCTAssertFalse(ChannelDeviceAdmission.welcomeReplaces(
            founder: founder, groupId: Data(repeating: 0x99, count: 32), ownerAccountKey: ownerKey),
            "another group")
        XCTAssertFalse(ChannelDeviceAdmission.welcomeReplaces(founder: nil, groupId: groupId, ownerAccountKey: ownerKey))

        let joined = try XCTUnwrap(ChannelDeviceAdmission.replaceHeldGroup(held, identity: old) {
            try publisherGroupJoinFromWelcome(
                identity: old, welcome: welcome, groupId: groupId, channelId: self.channel, ownerAccountKey: ownerKey)
        })
        XCTAssertEqual(try joined.channelSeed(identity: old), try refounded.channelSeed(identity: restored))
    }

    /// A replacing join that fails puts the held group back (real core): the
    /// delete is undone from the snapshot taken before it.
    func testAFailedReplacingJoinPutsTheHeldGroupBack() throws {
        let accountSeed = Data(repeating: 0x49, count: 32)
        let device = try Identity.fromSeedBound(
            seed: Data(repeating: 0x5A, count: 32), accountSeed: accountSeed, issuedAtUnixSecs: 1_700_000_000)
        let held = try PublisherGroup.create(identity: device, ownerAccountSeed: accountSeed, channelId: channel)
        let epoch = try held.epoch()
        let seed = try held.channelSeed(identity: device)
        struct JoinFailed: Error {}
        let result = ChannelDeviceAdmission.replaceHeldGroup(held, identity: device) {
            // The held group is gone at this point.
            XCTAssertNil(try publisherGroupLoad(identity: device, ownerAccountSeed: accountSeed, channelId: self.channel))
            throw JoinFailed()
        }
        XCTAssertNil(result)
        let back = try XCTUnwrap(try publisherGroupLoad(
            identity: device, ownerAccountSeed: accountSeed, channelId: channel))
        XCTAssertEqual(try back.epoch(), epoch)
        XCTAssertEqual(try back.channelSeed(identity: device), seed)
    }

    func testOnlyARefusedSignedReadCounts() {
        XCTAssertTrue(ChannelDeviceAdmission.isRefusedRead(
            DsClientError.http(status: 401, endpoint: "/ds/group", body: "")))
        XCTAssertFalse(ChannelDeviceAdmission.isRefusedRead(
            DsClientError.http(status: 500, endpoint: "/ds/group", body: "")))
        XCTAssertFalse(ChannelDeviceAdmission.isRefusedRead(URLError(.timedOut)))
    }

    func testAnOwnerDeviceTakesOverAResealOnlyAfterFiveMinutesAtTheNextEpoch() {
        let t0: UInt64 = 1_000_000
        XCTAssertFalse(ChannelPublisherLoops.takesOverReseal(
            groupEpoch: 8, serverEpoch: 7, trailingSinceMs: t0, nowMs: t0 + 299_999))
        XCTAssertTrue(ChannelPublisherLoops.takesOverReseal(
            groupEpoch: 8, serverEpoch: 7, trailingSinceMs: t0, nowMs: t0 + 300_000))
        XCTAssertFalse(ChannelPublisherLoops.takesOverReseal(
            groupEpoch: 7, serverEpoch: 7, trailingSinceMs: t0, nowMs: t0 + 600_000), "nothing trails")
        XCTAssertFalse(ChannelPublisherLoops.takesOverReseal(
            groupEpoch: 9, serverEpoch: 7, trailingSinceMs: t0, nowMs: t0 + 600_000),
            "two epochs ahead is not one reseal")
        XCTAssertFalse(ChannelPublisherLoops.takesOverReseal(
            groupEpoch: 8, serverEpoch: 7, trailingSinceMs: nil, nowMs: t0))
    }

    func testTheRotationPolicyCountsOnlyThePostsOfTheCurrentEpoch() {
        XCTAssertEqual(ChannelPublisherLoops.postsUnder(epoch: 3, payloads: ["not-a-post", ""]), 0)
    }

    // MARK: decision A, the epoch offset

    func testAReFoundedGroupMapsItsMlsEpochToTheChannelEpoch() {
        XCTAssertEqual(ChannelDeviceAdmission.channelEpoch(mls: 3, offset: nil), 3)
        XCTAssertEqual(ChannelDeviceAdmission.channelEpoch(mls: 1, offset: 12), 13)
        let learned = ChannelDeviceAdmission.learnedOffset(serverEpoch: 13, mls: 1)
        XCTAssertEqual(learned, 12)
        XCTAssertEqual(ChannelDeviceAdmission.channelEpoch(mls: 2, offset: learned), 14)
        XCTAssertEqual(ChannelDeviceAdmission.channelEpoch(mls: 0, offset: -5), 0, "never below zero")
    }

    // MARK: the owner seed

    func testTheOwnerSeedIsTheLineageUnlessTheBindingNamesTheCurrentKey() throws {
        let lineage = Data(repeating: 0x31, count: 32)
        let current = Data(repeating: 0x32, count: 32)
        let lineageKey = try Identity.fromSeed(seed: lineage).signaturePublicKey()
        let currentKey = try Identity.fromSeed(seed: current).signaturePublicKey()
        XCTAssertEqual(ChannelPublisherIdentity.ownerSeed(
            lineage: lineage, currentSeed: current, boundAccountKey: lineageKey), lineage)
        XCTAssertEqual(ChannelPublisherIdentity.ownerSeed(
            lineage: lineage, currentSeed: current, boundAccountKey: currentKey), current)
        XCTAssertEqual(ChannelPublisherIdentity.ownerSeed(
            lineage: lineage, currentSeed: current, boundAccountKey: nil), lineage)
    }

    // MARK: the FFI boundary: one channel, two owner devices

    /// Two owner devices of one account: the first founds, the second is
    /// admitted by a staged commit merged on acceptance, and both derive the
    /// same channel seed (ADR-0090 section 1).
    func testTwoOwnerDevicesHoldDistinctLeavesAndOneSeed() throws {
        let accountSeed = Data(repeating: 0x41, count: 32)
        let a = try Identity.fromSeedBound(
            seed: Data(repeating: 0x51, count: 32), accountSeed: accountSeed, issuedAtUnixSecs: 1_700_000_000)
        let b = try Identity.fromSeedBound(
            seed: Data(repeating: 0x52, count: 32), accountSeed: accountSeed, issuedAtUnixSecs: 1_700_000_000)
        let group = try PublisherGroup.create(identity: a, ownerAccountSeed: accountSeed, channelId: channel)
        XCTAssertTrue(try group.isOwnerDevice(identity: a))
        let staged = try group.stageCommit(identity: a, admit: [try b.createKeyPackage()], retire: [], rotate: false)
        XCTAssertTrue(try group.hasStagedCommit())
        XCTAssertTrue(try group.mergeStagedCommit(identity: a))
        let welcome = try XCTUnwrap(staged.welcome)
        let joined = try publisherGroupJoinFromWelcome(
            identity: b, welcome: welcome, groupId: try group.groupId(), channelId: channel,
            ownerAccountKey: try publisherOwnerAccountKey(ownerAccountSeed: accountSeed))
        XCTAssertEqual(try joined.channelSeed(identity: b), try group.channelSeed(identity: a))
        XCTAssertTrue(try joined.isOwnerDevice(identity: b))
        let leaves = ChannelDeviceAdmission.ownDeviceDiff(
            leaves: try group.members().map { (leaf: $0.signaturePublicKey, account: $0.accountKey) },
            accountKey: try group.ownerAccountKey(),
            roster: [a.signaturePublicKey(), b.signaturePublicKey()], selfLeaf: a.signaturePublicKey())
        XCTAssertTrue(leaves.missing.isEmpty && leaves.retire.isEmpty)
    }

    /// ADR-0090 contract 3a — a co-publisher's device joins a PublisherGroup
    /// Welcome only from a device of the owner key its account recorded: a
    /// Welcome from a device of another account is refused, one from the
    /// recorded owner lands; with no key recorded nothing is tried.
    func testACoPublisherDeviceJoinsOnlyFromTheRecordedOwner() throws {
        let ownerSeed = Data(repeating: 0x43, count: 32)
        let otherSeed = Data(repeating: 0x44, count: 32)
        let copubAccount = try Identity.fromSeed(seed: Data(repeating: 0x45, count: 32))
        let owner = try Identity.fromSeedBound(
            seed: Data(repeating: 0x54, count: 32), accountSeed: ownerSeed, issuedAtUnixSecs: 1_700_000_000)
        let other = try Identity.fromSeedBound(
            seed: Data(repeating: 0x55, count: 32), accountSeed: otherSeed, issuedAtUnixSecs: 1_700_000_000)
        let joiner = try Identity.fromSeedBound(
            seed: Data(repeating: 0x56, count: 32), accountSeed: copubAccount.ed25519Seed(),
            issuedAtUnixSecs: 1_700_000_000)
        let ownerKey = try publisherOwnerAccountKey(ownerAccountSeed: ownerSeed)

        // Another account's channel: its Welcome names its own group, and the
        // recorded owner key refuses it even under that group's id.
        let foreign = try PublisherGroup.create(identity: other, ownerAccountSeed: otherSeed, channelId: channel)
        let foreignRequest = try foreign.retainJoinRequest(identity: other, keyPackage: try joiner.createKeyPackage())
        let foreignWelcome = try XCTUnwrap(
            try foreign.stageApproveExternalJoin(identity: other, proposalRef: foreignRequest.joinerSignatureKey).welcome)
        XCTAssertTrue(try foreign.mergeStagedCommit(identity: other))
        XCTAssertThrowsError(try publisherGroupJoinFromWelcome(
            identity: joiner, welcome: foreignWelcome, groupId: try foreign.groupId(), channelId: channel,
            ownerAccountKey: ownerKey), "a Welcome from a device of another account is refused")

        // The recorded owner's Welcome lands.
        let group = try PublisherGroup.create(identity: owner, ownerAccountSeed: ownerSeed, channelId: channel)
        let request = try group.retainJoinRequest(identity: owner, keyPackage: try joiner.createKeyPackage())
        let welcome = try XCTUnwrap(
            try group.stageApproveExternalJoin(identity: owner, proposalRef: request.joinerSignatureKey).welcome)
        XCTAssertTrue(try group.mergeStagedCommit(identity: owner))
        var extras = ChannelExtras()
        XCTAssertNil(ChannelDeviceAdmission.recordedOwnerKey(extras), "none recorded: nothing is joined")
        extras.ownerAccountKey = ownerKey
        let recorded = try XCTUnwrap(ChannelDeviceAdmission.recordedOwnerKey(extras))
        let joined = try publisherGroupJoinFromWelcome(
            identity: joiner, welcome: welcome, groupId: try group.groupId(), channelId: channel,
            ownerAccountKey: recorded)
        XCTAssertEqual(try joined.channelSeed(identity: joiner), try group.channelSeed(identity: owner))
        XCTAssertFalse(try joined.isOwnerDevice(identity: joiner))
    }

    /// The owed hand-over and the queued device requests survive a relaunch
    /// (a fresh store reads them back) and go with the account wipe.
    func testTheOwedHandoverAndQueuedRequestsAreDurablePerAccount() throws {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "admission-\(UUID().uuidString)"))
        let a = ChannelDeviceAdmissionStore(accountIdB64: "acct-a", defaults: defaults)
        let b = ChannelDeviceAdmissionStore(accountIdB64: "acct-b", defaults: defaults)
        a.markHandoverOwed("AB")
        a.enqueue([(plaintext: Data([1]), sender: copub), (plaintext: Data([1]), sender: copub)], channelId: "ab")
        let reread = ChannelDeviceAdmissionStore(accountIdB64: "acct-a", defaults: defaults)
        XCTAssertTrue(reread.handoverOwed("ab"))
        XCTAssertEqual(reread.queued(channelId: "ab").map(\.sender), [copub], "each request once")
        XCTAssertFalse(b.handoverOwed("ab"))
        XCTAssertTrue(b.queued(channelId: "ab").isEmpty)
        reread.clearHandoverOwed("ab")
        reread.dropQueued(channelId: "ab")
        XCTAssertNil(defaults.data(forKey: ChannelDeviceAdmissionStore.key("acct-a")), "an empty record is removed")
        a.markHandoverOwed("cd")
        ChannelDeviceAdmissionStore.clear(accountIdB64: "acct-a", defaults: defaults)
        XCTAssertFalse(a.handoverOwed("cd"))
    }

    /// An abandoned staged commit leaves the group at its epoch and seed.
    func testAnAbandonedCommitLeavesTheGroupAtItsEpoch() throws {
        let accountSeed = Data(repeating: 0x42, count: 32)
        let a = try Identity.fromSeedBound(
            seed: Data(repeating: 0x53, count: 32), accountSeed: accountSeed, issuedAtUnixSecs: 1_700_000_000)
        let group = try PublisherGroup.create(identity: a, ownerAccountSeed: accountSeed, channelId: channel)
        let epoch = try group.epoch()
        let seed = try group.channelSeed(identity: a)
        _ = try group.stageRotate(identity: a)
        XCTAssertEqual(try group.channelSeed(identity: a), seed, "posts keep the current seed while staged")
        XCTAssertNotNil(try group.stagedChannelSeed(identity: a))
        XCTAssertTrue(try group.abandonStagedCommit(identity: a))
        XCTAssertEqual(try group.epoch(), epoch)
        XCTAssertEqual(try group.channelSeed(identity: a), seed)
    }
}
