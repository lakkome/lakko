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

    // MARK: walk of 2026-11-14 — a returning old owner device

    /// A channel whose keys this device still holds is a Welcome target; a
    /// subscriber's channel is not.
    func testTheWelcomeTargetsIncludeAChannelThisDeviceHoldsKeysFor() {
        let state = ChannelsUiState(accountIdB64: "targets-\(UUID().uuidString)")
        let held = String(repeating: "a1", count: 16)
        let waiting = String(repeating: "a2", count: 16)
        let subscribed = String(repeating: "a3", count: 16)
        for id in [held, waiting, subscribed] {
            state.addChannel(ChannelHandle(id: id, localName: "c", localDescription: "", addedAtMs: 1))
        }
        state.updateExtras(held) { $0.accountRole = ChannelAccountRole.owner }
        state.updateExtras(waiting) { $0.accountRole = ChannelAccountRole.coPublisher }
        state.setKeys(channelId: held, keys: ChannelPublisherKeys(
            seed: Data(repeating: 1, count: 32), signingKeySeed: Data(repeating: 2, count: 32)))
        XCTAssertEqual(Set(ChannelDeviceAdmission.welcomeTargets(state)), [held, waiting])
        XCTAssertEqual(ChannelDeviceAdmission.waitingChannels(state), [waiting],
                       "the waiting line is still only for a device with no keys")
    }

    /// The verdict for an inactive held group, the same table as web and
    /// Android `inactivePublisherGroupVerdict`.
    func testTheInactiveGroupVerdictMatchesWebAndAndroid() {
        typealias V = ChannelDeviceAdmission.InactiveGroupVerdict
        func v(_ owner: Bool, _ leaf: Bool?, _ other: Bool?) -> V {
            ChannelDeviceAdmission.inactiveGroupVerdict(ownerDevice: owner, ownAccountHasLeaf: leaf, otherOwnDevices: other)
        }
        XCTAssertEqual(v(true, nil, nil), .undecided, "roster not loaded")
        XCTAssertEqual(v(false, false, nil), .undecided)
        XCTAssertEqual(v(true, nil, true), .waitForWelcome, "another owner device admits it")
        XCTAssertEqual(v(true, nil, false), .reader, "the account's only device takes the channel")
        XCTAssertEqual(v(true, false, false), .reader)
        XCTAssertEqual(v(false, true, true), .waitForWelcome, "a co-publisher's device retired")
        XCTAssertEqual(v(false, false, true), .removed, "the co-publisher account was removed")
        XCTAssertEqual(v(false, nil, true), .removed)
    }

    /// Security pass 2026-11-15 (F4) — a leaf of this account left in the
    /// inactive group is another own device (this device's leaf is gone):
    /// wait for its Welcome, whatever a stale DeviceGroup roster says.
    func testAnOwnAccountLeafLeftInTheGroupMeansWaitNotReader() {
        typealias V = ChannelDeviceAdmission.InactiveGroupVerdict
        func v(_ owner: Bool, _ leaf: Bool?, _ other: Bool?) -> V {
            ChannelDeviceAdmission.inactiveGroupVerdict(ownerDevice: owner, ownAccountHasLeaf: leaf, otherOwnDevices: other)
        }
        XCTAssertEqual(v(true, true, false), .waitForWelcome, "a stale roster showing only this device")
        XCTAssertEqual(v(true, true, nil), .waitForWelcome, "the group alone says another device is there")
        XCTAssertEqual(v(false, true, false), .waitForWelcome, "a co-publisher's account still publishes")
        XCTAssertEqual(v(false, true, nil), .waitForWelcome)
    }

    /// F4 on the real core: B (same account) retired A; A's inactive group
    /// still holds B's leaf, so A waits even when its roster shows only A.
    func testAnInactiveGroupHoldingAnotherOwnLeafIsNotAReaderOnAStaleRoster() throws {
        try withWalkDevices { ctx in
            let held = try PublisherGroup.create(identity: ctx.a, ownerAccountSeed: ctx.accountSeed, channelId: self.channel)
            try ctx.retireA(held)
            XCTAssertFalse(try held.isActive())
            let account = try XCTUnwrap(ctx.a.accountPublicKey())
            let ownLeafLeft = ChannelDeviceAdmission.leaves(held).contains { $0.account == account }
            XCTAssertTrue(ownLeafLeft, "B's leaf is the account's")
            XCTAssertEqual(ChannelDeviceAdmission.inactiveGroupVerdict(
                ownerDevice: true, ownAccountHasLeaf: ownLeafLeft, otherOwnDevices: false), .waitForWelcome)
        }
    }

    /// Security pass 2026-11-15 (F5) — New channel founds under a kept id
    /// only when that create never finished here.
    func testAKeptChannelIdIsReusedOnlyForAnUnfinishedCreate() {
        XCTAssertTrue(PendingChannelHandleStore.keptIdUsable(listedHere: false, inRegister: false))
        XCTAssertFalse(PendingChannelHandleStore.keptIdUsable(listedHere: true, inRegister: false))
        XCTAssertFalse(PendingChannelHandleStore.keptIdUsable(listedHere: false, inRegister: true))
    }

    /// Security pass 2026-11-15 (F2) — a Welcome this device already joined
    /// is refused when served again under reset suspicion; a new re-founded
    /// group's Welcome is still taken.
    func testAReplayedJoinedWelcomeIsRefusedUnderSuspicionAndANewOneIsTaken() throws {
        try withWalkDevices { ctx in
            let held = try PublisherGroup.create(identity: ctx.a, ownerAccountSeed: ctx.accountSeed, channelId: self.channel)
            ctx.state.addChannel(ChannelHandle(id: ctx.id, localName: "News", localDescription: "", addedAtMs: 1))
            ctx.state.updateExtras(ctx.id) {
                $0.accountRole = ChannelAccountRole.owner
                $0.resetSuspected = true
            }
            XCTAssertTrue(try held.isActive())
            // B founds again and admits A: joined (W1).
            let first = try PublisherGroup.create(identity: ctx.b, ownerAccountSeed: ctx.accountSeed, channelId: self.channel)
            let s1 = try first.stageCommit(identity: ctx.b, admit: [try ctx.a.createKeyPackage()], retire: [], rotate: false)
            XCTAssertTrue(try first.mergeStagedCommit(identity: ctx.b))
            let w1 = try XCTUnwrap(s1.welcome)
            XCTAssertEqual(ChannelDeviceAdmission.landWelcome(w1), ctx.id)
            XCTAssertTrue(ctx.store.hasJoinedWelcome(ChannelDeviceAdmission.welcomeReplayHash(w1), channelId: ctx.id))
            // The group moves on; A applies it.
            let a1 = try XCTUnwrap(try CoPublisher.loadPublisherGroup(identity: ctx.a, channelIdHex: ctx.id))
            let rotate = try first.stageCommit(identity: ctx.b, admit: [], retire: [], rotate: true)
            XCTAssertTrue(try first.mergeStagedCommit(identity: ctx.b))
            _ = try a1.processIncomingVerifyingRoles(identity: ctx.a, bytes: rotate.commit)
            let epochNow = try a1.epoch()

            // The DS raises suspicion again and serves W1 once more.
            ctx.state.updateExtras(ctx.id) { $0.resetSuspected = true }
            XCTAssertNil(ChannelDeviceAdmission.landWelcome(w1), "an already-joined Welcome is old")
            XCTAssertNil(ctx.store.heldWelcomes()[ctx.id], "and not held either")
            let still = try XCTUnwrap(try CoPublisher.loadPublisherGroup(identity: ctx.a, channelIdHex: ctx.id))
            XCTAssertEqual(try still.epoch(), epochNow, "not rolled back")

            // A genuinely new re-founding (W2) is taken.
            let c = try Identity.fromSeedBound(
                seed: Data(repeating: 0x5C, count: 32), accountSeed: ctx.accountSeed, issuedAtUnixSecs: 1_700_000_000)
            let second = try PublisherGroup.create(identity: c, ownerAccountSeed: ctx.accountSeed, channelId: self.channel)
            let s2 = try second.stageCommit(identity: c, admit: [try ctx.a.createKeyPackage()], retire: [], rotate: false)
            XCTAssertTrue(try second.mergeStagedCommit(identity: c))
            XCTAssertEqual(ChannelDeviceAdmission.landWelcome(try XCTUnwrap(s2.welcome)), ctx.id)
            let joined = try XCTUnwrap(try CoPublisher.loadPublisherGroup(identity: ctx.a, channelIdHex: ctx.id))
            XCTAssertEqual(try joined.channelSeed(identity: ctx.a), try second.channelSeed(identity: c))

            // Without the replay set the core joins W1 again (the KeyPackage
            // is last-resort) and the device would be rolled back: the set is
            // the guard.
            let rejoined = ChannelDeviceAdmission.replaceHeldGroup(joined, identity: ctx.a) {
                try publisherGroupJoinFromWelcome(
                    identity: ctx.a, welcome: w1, groupId: try first.groupId(), channelId: self.channel,
                    ownerAccountKey: try publisherOwnerAccountKey(ownerAccountSeed: ctx.accountSeed))
            }
            XCTAssertNotNil(rejoined)
        }
    }

    /// Security pass 2026-11-15 (F3) — after the device deleted its inactive
    /// group, a Welcome that joins at an epoch not above the deleted one is
    /// undone; one above it is taken.
    func testAfterDeletingAnInactiveGroupAWelcomeNotAboveItsEpochIsRefused() throws {
        try withWalkDevices { ctx in
            let ownerKey = try publisherOwnerAccountKey(ownerAccountSeed: ctx.accountSeed)
            let held = try PublisherGroup.create(identity: ctx.a, ownerAccountSeed: ctx.accountSeed, channelId: self.channel)
            for _ in 0..<3 {
                _ = try held.stageCommit(identity: ctx.a, admit: [], retire: [], rotate: true)
                XCTAssertTrue(try held.mergeStagedCommit(identity: ctx.a))
            }
            try ctx.retireA(held)
            XCTAssertFalse(try held.isActive())
            let deletedEpoch = try held.epoch()
            ctx.state.addChannel(ChannelHandle(id: ctx.id, localName: "News", localDescription: "", addedAtMs: 1))
            ctx.state.updateExtras(ctx.id) { $0.accountRole = ChannelAccountRole.owner }
            XCTAssertTrue(ChannelDeviceAdmission.deleteInactiveGroup(identity: ctx.a, channelId: ctx.id))
            XCTAssertEqual(ctx.store.deletedGroupEpoch(channelId: ctx.id), deletedEpoch)

            // An old Welcome for the same group id at a low epoch.
            let restored = try Identity.fromSeedBound(
                seed: Data(repeating: 0x5B, count: 32), accountSeed: ctx.accountSeed, issuedAtUnixSecs: 1_700_000_000)
            let other = try PublisherGroup.create(identity: restored, ownerAccountSeed: ctx.accountSeed, channelId: self.channel)
            let old = try other.stageCommit(
                identity: restored, admit: [try ctx.a.createKeyPackage()], retire: [], rotate: false)
            XCTAssertTrue(try other.mergeStagedCommit(identity: restored))
            XCTAssertLessThanOrEqual(try other.epoch(), deletedEpoch)
            XCTAssertNil(ChannelDeviceAdmission.landWelcome(try XCTUnwrap(old.welcome)))
            XCTAssertNil(try CoPublisher.loadPublisherGroup(identity: ctx.a, channelIdHex: ctx.id), "nothing joined")

            // B admits A in the live group (above the deleted epoch): taken.
            let bGroup = try ctx.bGroup()
            let admit = try bGroup.stageCommit(
                identity: ctx.b, admit: [try ctx.a.createKeyPackage()], retire: [], rotate: false)
            XCTAssertTrue(try bGroup.mergeStagedCommit(identity: ctx.b))
            XCTAssertGreaterThan(try bGroup.epoch(), deletedEpoch)
            XCTAssertEqual(ChannelDeviceAdmission.landWelcome(try XCTUnwrap(admit.welcome)), ctx.id)
            let joined = try XCTUnwrap(try CoPublisher.loadPublisherGroup(identity: ctx.a, channelIdHex: ctx.id))
            XCTAssertEqual(try joined.epoch(), try bGroup.epoch())
            _ = ownerKey
        }
    }

    func testTheDeletedEpochFloorRule() {
        XCTAssertTrue(ChannelDeviceAdmission.joinAboveDeletedEpoch(joined: 1, floor: nil, resetSuspected: false))
        XCTAssertFalse(ChannelDeviceAdmission.joinAboveDeletedEpoch(joined: 5, floor: 5, resetSuspected: false))
        XCTAssertTrue(ChannelDeviceAdmission.joinAboveDeletedEpoch(joined: 6, floor: 5, resetSuspected: false))
        XCTAssertTrue(ChannelDeviceAdmission.joinAboveDeletedEpoch(joined: 1, floor: 5, resetSuspected: true),
                      "a re-founded chain restarts low")
    }

    /// Replay guard — only a group this device can no longer use is replaced,
    /// and only by a Welcome that joins above its epoch.
    func testTheReplaceRuleNeedsAnUnusableGroupAndANewerEpoch() {
        XCTAssertFalse(ChannelDeviceAdmission.mayReplaceHeldGroup(isActive: true, resetSuspected: false))
        XCTAssertFalse(ChannelDeviceAdmission.mayReplaceHeldGroup(isActive: nil, resetSuspected: false))
        XCTAssertTrue(ChannelDeviceAdmission.mayReplaceHeldGroup(isActive: false, resetSuspected: false))
        XCTAssertTrue(ChannelDeviceAdmission.mayReplaceHeldGroup(isActive: true, resetSuspected: true))
        XCTAssertTrue(ChannelDeviceAdmission.joinedReplacesHeld(joined: 4, held: 3, heldActive: false))
        XCTAssertFalse(ChannelDeviceAdmission.joinedReplacesHeld(joined: 3, held: 3, heldActive: false))
        XCTAssertFalse(ChannelDeviceAdmission.joinedReplacesHeld(joined: 2, held: 3, heldActive: false))
        XCTAssertTrue(ChannelDeviceAdmission.joinedReplacesHeld(joined: 1, held: 9, heldActive: true),
                      "an active held group (reset suspected) is replaced without the epoch check")
    }

    /// Replay guard on the real core — for an INACTIVE held group, a Welcome
    /// that joins at an epoch not above the held group's puts it back.
    func testAWelcomeThatJoinsNoHigherThanAnInactiveHeldGroupRestoresIt() throws {
        try withWalkDevices { ctx in
            let ownerKey = try publisherOwnerAccountKey(ownerAccountSeed: ctx.accountSeed)
            // The held group moved on (three rotations), then A was retired.
            let held = try PublisherGroup.create(identity: ctx.a, ownerAccountSeed: ctx.accountSeed, channelId: self.channel)
            for _ in 0..<3 {
                _ = try held.stageCommit(identity: ctx.a, admit: [], retire: [], rotate: true)
                XCTAssertTrue(try held.mergeStagedCommit(identity: ctx.a))
            }
            try ctx.retireA(held)
            XCTAssertFalse(try held.isActive())
            let heldEpoch = try held.epoch()
            // An older Welcome for the same group id: founded and admitting at once.
            let restored = try Identity.fromSeedBound(
                seed: Data(repeating: 0x5B, count: 32), accountSeed: ctx.accountSeed, issuedAtUnixSecs: 1_700_000_000)
            let other = try PublisherGroup.create(identity: restored, ownerAccountSeed: ctx.accountSeed, channelId: self.channel)
            let groupId = try other.groupId()
            let staged = try other.stageCommit(
                identity: restored, admit: [try ctx.a.createKeyPackage()], retire: [], rotate: false)
            XCTAssertTrue(try other.mergeStagedCommit(identity: restored))
            XCTAssertLessThanOrEqual(try other.epoch(), heldEpoch)
            let welcome = try XCTUnwrap(staged.welcome)

            let result = ChannelDeviceAdmission.replaceHeldGroup(held, identity: ctx.a) {
                try publisherGroupJoinFromWelcome(
                    identity: ctx.a, welcome: welcome, groupId: groupId, channelId: self.channel, ownerAccountKey: ownerKey)
            }
            XCTAssertNil(result, "not joined")
            let back = try XCTUnwrap(try CoPublisher.loadPublisherGroup(identity: ctx.a, channelIdHex: ctx.id))
            XCTAssertEqual(try back.epoch(), heldEpoch)
            XCTAssertFalse(try back.isActive(), "the held group is back as it was")
        }
    }

    /// Replay guard — an ACTIVE held group under reset suspicion is replaced
    /// by a re-founded group (decision A) at a LOWER epoch: no epoch check.
    func testASuspectedActiveHeldGroupIsReplacedByALowerEpochRefoundedGroup() throws {
        try withWalkDevices { ctx in
            let held = try PublisherGroup.create(identity: ctx.a, ownerAccountSeed: ctx.accountSeed, channelId: self.channel)
            for _ in 0..<3 {
                _ = try held.stageCommit(identity: ctx.a, admit: [], retire: [], rotate: true)
                XCTAssertTrue(try held.mergeStagedCommit(identity: ctx.a))
            }
            XCTAssertTrue(try held.isActive())
            let heldEpoch = try held.epoch()
            ctx.state.addChannel(ChannelHandle(id: ctx.id, localName: "News", localDescription: "", addedAtMs: 1))
            ctx.state.updateExtras(ctx.id) {
                $0.accountRole = ChannelAccountRole.owner
                $0.resetSuspected = true
            }
            // B founded the channel again and admits A.
            let refounded = try PublisherGroup.create(
                identity: ctx.b, ownerAccountSeed: ctx.accountSeed, channelId: self.channel)
            let staged = try refounded.stageCommit(
                identity: ctx.b, admit: [try ctx.a.createKeyPackage()], retire: [], rotate: false)
            XCTAssertTrue(try refounded.mergeStagedCommit(identity: ctx.b))
            XCTAssertLessThan(try refounded.epoch(), heldEpoch)

            XCTAssertEqual(ChannelDeviceAdmission.landWelcome(try XCTUnwrap(staged.welcome)), ctx.id)
            let joined = try XCTUnwrap(try CoPublisher.loadPublisherGroup(identity: ctx.a, channelIdHex: ctx.id))
            XCTAssertEqual(try joined.epoch(), try refounded.epoch())
            XCTAssertEqual(ctx.state.keysByChannel[ctx.id]?.seed, try refounded.channelSeed(identity: ctx.b))
            XCTAssertNil(ctx.state.extras(ctx.id).resetSuspected)
        }
    }

    /// An inactive stored group is deleted before an entry; an active one
    /// stops it.
    func testOnlyAnInactiveGroupIsDeletedBeforeAnEntry() throws {
        try withWalkDevices { ctx in
            XCTAssertTrue(ChannelDeviceAdmission.deleteInactiveGroup(identity: ctx.a, channelId: ctx.id),
                          "nothing stored")
            let held = try PublisherGroup.create(identity: ctx.a, ownerAccountSeed: ctx.accountSeed, channelId: self.channel)
            XCTAssertFalse(ChannelDeviceAdmission.deleteInactiveGroup(identity: ctx.a, channelId: ctx.id),
                           "an active group is kept")
            XCTAssertNotNil(try CoPublisher.loadPublisherGroup(identity: ctx.a, channelIdHex: ctx.id))
            try ctx.retireA(held)
            XCTAssertFalse(try held.isActive())
            XCTAssertTrue(ChannelDeviceAdmission.deleteInactiveGroup(identity: ctx.a, channelId: ctx.id))
            XCTAssertNil(try CoPublisher.loadPublisherGroup(identity: ctx.a, channelIdHex: ctx.id))
            // The core takes an entry now.
            XCTAssertNoThrow(try publisherGroupEnter(
                identity: ctx.a, groupInfo: try ctx.bGroup().groupInfo(identity: ctx.b),
                ownerAccountSeed: ctx.accountSeed, channelId: self.channel,
                dsAccessSeed: try ctx.bGroup().dsAccessSeed(identity: ctx.b)))
        }
    }

    /// Two devices A and B of one owner account, A bound as this device.
    /// `accountSeed` is the owner account's 32-byte Ed25519 seed (what the
    /// core and the device bindings take).
    private final class WalkDevices {
        let accountSeed: Data
        let a: Identity
        let b: Identity
        let id: String
        let state: ChannelsUiState
        let channel: Data
        var store: ChannelDeviceAdmissionStore {
            ChannelDeviceAdmissionStore(accountIdB64: AcceptedContactsStore.boundAccountIdB64 ?? "")
        }
        private var entered: PublisherGroup?

        init(accountSeed: Data, a: Identity, b: Identity, id: String, state: ChannelsUiState, channel: Data) {
            self.accountSeed = accountSeed; self.a = a; self.b = b; self.id = id; self.state = state
            self.channel = channel
        }

        func bGroup() throws -> PublisherGroup { try XCTUnwrap(entered) }

        /// B enters from `held`'s GroupInfo and retires A (rotating); A
        /// applies both commits, so its `held` group goes inactive.
        func retireA(_ held: PublisherGroup) throws {
            let entry = try publisherGroupEnter(
                identity: b, groupInfo: try held.groupInfo(identity: a), ownerAccountSeed: accountSeed,
                channelId: channel, dsAccessSeed: try held.dsAccessSeed(identity: a))
            XCTAssertTrue(try entry.group.mergeStagedCommit(identity: b))
            _ = try held.processIncomingVerifyingRoles(identity: a, bytes: entry.commit)
            let retire = try entry.group.stageCommit(identity: b, admit: [], retire: [a.signaturePublicKey()], rotate: true)
            XCTAssertTrue(try entry.group.mergeStagedCommit(identity: b))
            _ = try held.processIncomingVerifyingRoles(identity: a, bytes: retire.commit)
            entered = entry.group
        }
    }

    private func withWalkDevices(_ body: (WalkDevices) throws -> Void) throws {
        let account = "walk-\(UUID().uuidString)"
        // As in the app: the account's current seed is the 64-byte identity
        // seed; the owner seed the core takes and the device binding names is
        // that identity's 32-byte Ed25519 seed.
        let identitySeed = Data((0..<64).map { _ in UInt8.random(in: 0...255) })
        let accountSeed = try Identity.fromSeed(seed: identitySeed).ed25519Seed()
        let a = try Identity.fromSeedBound(
            seed: Data((0..<32).map { _ in UInt8.random(in: 0...255) }), accountSeed: accountSeed,
            issuedAtUnixSecs: 1_700_000_000)
        let b = try Identity.fromSeedBound(
            seed: Data((0..<32).map { _ in UInt8.random(in: 0...255) }), accountSeed: accountSeed,
            issuedAtUnixSecs: 1_700_000_000)
        let previousBound = AcceptedContactsStore.boundAccountIdB64
        let previousProvider = ChannelPublisherIdentity.shared.deviceIdentityProvider
        let previousPersister = ChannelPublisherIdentity.shared.devicePersister
        let previousState = ChannelDeviceAdmission.waitingState
        let previousSeed = ChannelDeviceAdmission.currentSeed
        AcceptedContactsStore.bind(accountIdB64: account)
        ChannelPublisherIdentity.shared.unbind()
        ChannelPublisherIdentity.shared.deviceIdentityProvider = { a }
        ChannelPublisherIdentity.shared.devicePersister = {}
        let state = ChannelsUiState(accountIdB64: account)
        ChannelDeviceAdmission.waitingState = state
        ChannelDeviceAdmission.currentSeed = { identitySeed }
        defer {
            ChannelDeviceAdmissionStore.clear(accountIdB64: account)
            ChannelDeviceAdmission.waitingState = previousState
            ChannelDeviceAdmission.currentSeed = previousSeed
            ChannelPublisherIdentity.shared.deviceIdentityProvider = previousProvider
            ChannelPublisherIdentity.shared.devicePersister = previousPersister
            ChannelPublisherIdentity.shared.unbind()
            AcceptedContactsStore.bind(accountIdB64: previousBound)
        }
        // Notes the owner seed (``ChannelPublisherIdentity/ownerAccountSeed``).
        _ = try ChannelPublisherIdentity.shared.identity(currentSeed: identitySeed)
        XCTAssertEqual(ChannelPublisherIdentity.ownerAccountEd25519Seed, accountSeed)
        let id = channel.map { String(format: "%02x", $0) }.joined()
        try body(WalkDevices(accountSeed: accountSeed, a: a, b: b, id: id, state: state, channel: channel))
    }

    /// The scenario of the walk, on the real core: device A holds the
    /// channel's group and its keys; device B (same account) entered from the
    /// saved GroupInfo, retired A's leaf, and admitted A's new KeyPackage. The
    /// chat intake refuses that Welcome as not a conversation without the
    /// join-failed banner, and the channel side replaces A's held group with
    /// it: A publishes again under the joined group's seed.
    func testAnOwnerDeviceWelcomeForAHeldChannelReplacesTheOldGroupAndTheDevicePublishesAgain() throws {
        try withWalkDevices { ctx in
            // A founded the channel and holds its keys.
            let held = try PublisherGroup.create(identity: ctx.a, ownerAccountSeed: ctx.accountSeed, channelId: self.channel)
            let oldSeed = try held.channelSeed(identity: ctx.a)
            ctx.state.addChannel(ChannelHandle(id: ctx.id, localName: "News", localDescription: "", addedAtMs: 1))
            ctx.state.updateExtras(ctx.id) { $0.accountRole = ChannelAccountRole.owner }
            ctx.state.setKeys(channelId: ctx.id, keys: ChannelPublisherKeys(
                seed: oldSeed, signingKeySeed: Data(repeating: 2, count: 32)))

            // B enters and retires A; A applied both (its group is inactive).
            try ctx.retireA(held)
            let bGroup = try ctx.bGroup()
            XCTAssertFalse(try held.isActive())
            // B admits A again.
            let admit = try bGroup.stageCommit(
                identity: ctx.b, admit: [try ctx.a.createKeyPackage()], retire: [], rotate: false)
            XCTAssertTrue(try bGroup.mergeStagedCommit(identity: ctx.b))
            let welcome = try XCTUnwrap(admit.welcome)
            let newSeed = try bGroup.channelSeed(identity: ctx.b)
            XCTAssertNotEqual(newSeed, oldSeed)

            // The chat intake: refused as not a conversation, no banner.
            do {
                _ = try Group.joinFromWelcome(welcome: welcome, identity: ctx.a)
                XCTFail("a PublisherGroup Welcome is not a conversation")
            } catch {
                let refusal = ContactsRepository.mailboxWelcomeRefusal(
                    ChatError.wrapped("joinFromWelcome failed", underlying: error))
                XCTAssertEqual(refusal, .notAConversation)
                XCTAssertFalse(refusal.raisesJoinFailureBanner)
            }

            // The channel side takes it although this device holds the channel.
            XCTAssertTrue(ChannelDeviceAdmission.hasWelcomeTargets)
            XCTAssertEqual(ChannelDeviceAdmission.landWelcome(welcome), ctx.id)
            let joined = try XCTUnwrap(try CoPublisher.loadPublisherGroup(identity: ctx.a, channelIdHex: ctx.id))
            XCTAssertTrue(try joined.isActive())
            XCTAssertEqual(try joined.channelSeed(identity: ctx.a), newSeed)
            XCTAssertEqual(ctx.state.keysByChannel[ctx.id]?.seed, newSeed, "A publishes under the joined group's seed")
            XCTAssertNil(ctx.state.extras(ctx.id).resetSuspected)
        }
    }

    /// The early Welcome — the re-admission Welcome arrives before this
    /// device applied the commit that removed its leaf: it is held, and once
    /// the removal is applied the retry joins it and the device publishes.
    func testAWelcomeThatArrivesBeforeTheRemovalIsHeldAndJoinedAfterIt() throws {
        try withWalkDevices { ctx in
            let held = try PublisherGroup.create(identity: ctx.a, ownerAccountSeed: ctx.accountSeed, channelId: self.channel)
            ctx.state.addChannel(ChannelHandle(id: ctx.id, localName: "News", localDescription: "", addedAtMs: 1))
            ctx.state.updateExtras(ctx.id) { $0.accountRole = ChannelAccountRole.owner }
            ctx.state.setKeys(channelId: ctx.id, keys: ChannelPublisherKeys(
                seed: try held.channelSeed(identity: ctx.a), signingKeySeed: Data(repeating: 2, count: 32)))
            let entry = try publisherGroupEnter(
                identity: ctx.b, groupInfo: try held.groupInfo(identity: ctx.a), ownerAccountSeed: ctx.accountSeed,
                channelId: self.channel, dsAccessSeed: try held.dsAccessSeed(identity: ctx.a))
            XCTAssertTrue(try entry.group.mergeStagedCommit(identity: ctx.b))
            let retire = try entry.group.stageCommit(
                identity: ctx.b, admit: [], retire: [ctx.a.signaturePublicKey()], rotate: true)
            XCTAssertTrue(try entry.group.mergeStagedCommit(identity: ctx.b))
            let admit = try entry.group.stageCommit(
                identity: ctx.b, admit: [try ctx.a.createKeyPackage()], retire: [], rotate: false)
            XCTAssertTrue(try entry.group.mergeStagedCommit(identity: ctx.b))
            let welcome = try XCTUnwrap(admit.welcome)

            // The mailbox first: the group is still active here.
            XCTAssertNil(ChannelDeviceAdmission.landWelcome(welcome))
            XCTAssertEqual(ctx.store.heldWelcomes()[ctx.id]?.count, 1)
            ChannelDeviceAdmission.retryHeldWelcomes()
            XCTAssertEqual(ctx.store.heldWelcomes()[ctx.id]?.count, 1, "still active: it waits")

            // The journal second: the removal is applied.
            _ = try held.processIncomingVerifyingRoles(identity: ctx.a, bytes: entry.commit)
            _ = try held.processIncomingVerifyingRoles(identity: ctx.a, bytes: retire.commit)
            XCTAssertFalse(try held.isActive())
            ChannelDeviceAdmission.retryHeldWelcomes()

            XCTAssertNil(ctx.store.heldWelcomes()[ctx.id], "the hold is gone")
            let joined = try XCTUnwrap(try CoPublisher.loadPublisherGroup(identity: ctx.a, channelIdHex: ctx.id))
            XCTAssertTrue(try joined.isActive())
            let newSeed = try entry.group.channelSeed(identity: ctx.b)
            XCTAssertEqual(try joined.channelSeed(identity: ctx.a), newSeed)
            XCTAssertEqual(ctx.state.keysByChannel[ctx.id]?.seed, newSeed, "A publishes again")
        }
    }

    /// Held Welcomes are bounded per channel (newest kept), dropped when old,
    /// and dropped for a channel the account no longer publishes.
    func testHeldWelcomesAreBoundedAgedAndDroppedForAChannelLeft() throws {
        XCTAssertFalse(ChannelDeviceAdmission.heldWelcomeExpired(heldAtMs: 1_000, nowMs: 2_000))
        XCTAssertTrue(ChannelDeviceAdmission.heldWelcomeExpired(
            heldAtMs: 1_000, nowMs: 1_001 + ChannelDeviceAdmission.heldWelcomeMaxAgeMs))
        XCTAssertFalse(ChannelDeviceAdmission.heldWelcomeExpired(heldAtMs: 5_000, nowMs: 1_000),
                       "a clock moved back keeps it")
        try withWalkDevices { ctx in
            for i in 0..<5 { ctx.store.holdWelcome(Data([UInt8(i)]), channelId: ctx.id, nowMs: UInt64(i)) }
            let kept = try XCTUnwrap(ctx.store.heldWelcomes()[ctx.id])
            XCTAssertEqual(kept.count, ChannelDeviceAdmissionStore.maxHeldWelcomesPerChannel)
            XCTAssertEqual(kept.last?.welcomeB64, Data([4]).base64EncodedString(), "the newest wins")
            // Not a channel the account owns or co-publishes: dropped.
            ChannelDeviceAdmission.retryHeldWelcomes(nowMs: 10)
            XCTAssertNil(ctx.store.heldWelcomes()[ctx.id])
            // An aged one on an owned channel is dropped too.
            ctx.state.addChannel(ChannelHandle(id: ctx.id, localName: "News", localDescription: "", addedAtMs: 1))
            ctx.state.updateExtras(ctx.id) { $0.accountRole = ChannelAccountRole.owner }
            _ = try PublisherGroup.create(identity: ctx.a, ownerAccountSeed: ctx.accountSeed, channelId: self.channel)
            ctx.store.holdWelcome(Data([9]), channelId: ctx.id, nowMs: 1)
            ChannelDeviceAdmission.retryHeldWelcomes(nowMs: 2 + ChannelDeviceAdmission.heldWelcomeMaxAgeMs)
            XCTAssertNil(ctx.store.heldWelcomes()[ctx.id])
        }
    }

    /// Replay guard — an owner-signed Welcome for the same group does not
    /// replace an ACTIVE, unsuspected held group; the Welcome is ignored.
    func testAnActiveUnsuspectedHeldGroupIsNotReplaced() throws {
        try withWalkDevices { ctx in
            let held = try PublisherGroup.create(identity: ctx.a, ownerAccountSeed: ctx.accountSeed, channelId: self.channel)
            let heldEpoch = try held.epoch()
            let heldSeed = try held.channelSeed(identity: ctx.a)
            ctx.state.addChannel(ChannelHandle(id: ctx.id, localName: "News", localDescription: "", addedAtMs: 1))
            ctx.state.updateExtras(ctx.id) { $0.accountRole = ChannelAccountRole.owner }
            // B entered and admitted a fresh KeyPackage of A; A applied nothing.
            let entry = try publisherGroupEnter(
                identity: ctx.b, groupInfo: try held.groupInfo(identity: ctx.a), ownerAccountSeed: ctx.accountSeed,
                channelId: self.channel, dsAccessSeed: try held.dsAccessSeed(identity: ctx.a))
            XCTAssertTrue(try entry.group.mergeStagedCommit(identity: ctx.b))
            _ = try entry.group.stageCommit(
                identity: ctx.b, admit: [], retire: [ctx.a.signaturePublicKey()], rotate: true)
            XCTAssertTrue(try entry.group.mergeStagedCommit(identity: ctx.b))
            let admit = try entry.group.stageCommit(
                identity: ctx.b, admit: [try ctx.a.createKeyPackage()], retire: [], rotate: false)
            XCTAssertTrue(try entry.group.mergeStagedCommit(identity: ctx.b))
            let welcome = try XCTUnwrap(admit.welcome)

            XCTAssertNil(ChannelDeviceAdmission.landWelcome(welcome))
            XCTAssertEqual(ctx.store.heldWelcomes()[ctx.id]?.count, 1, "held, not dropped")
            let still = try XCTUnwrap(try CoPublisher.loadPublisherGroup(identity: ctx.a, channelIdHex: ctx.id))
            XCTAssertEqual(try still.epoch(), heldEpoch)
            XCTAssertEqual(try still.channelSeed(identity: ctx.a), heldSeed)

            // Under a reset suspicion the same Welcome replaces it.
            ctx.state.updateExtras(ctx.id) { $0.resetSuspected = true }
            XCTAssertEqual(ChannelDeviceAdmission.landWelcome(welcome), ctx.id)
            XCTAssertEqual(try XCTUnwrap(try CoPublisher.loadPublisherGroup(identity: ctx.a, channelIdHex: ctx.id))
                .channelSeed(identity: ctx.a), try entry.group.channelSeed(identity: ctx.b))
        }
    }

    /// No refusal of an account-mailbox Welcome raises the join-failed banner.
    func testNoMailboxWelcomeRefusalRaisesTheBanner() {
        struct Other: Error {}
        XCTAssertEqual(ContactsRepository.mailboxWelcomeRefusal(
            FfiError.MembershipNoRoleTable(message: "welcome refused")), .notAConversation)
        XCTAssertEqual(ContactsRepository.mailboxWelcomeRefusal(Other()), .notOpenedHere)
        for refusal in [ContactsRepository.MailboxWelcomeRefusal.notAConversation, .notOpenedHere,
                        .groupStillActive("ab")] {
            XCTAssertFalse(refusal.raisesJoinFailureBanner)
        }
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
