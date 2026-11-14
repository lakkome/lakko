// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0050 section 2.9 (amended 2026-11-13), `docs/ux/second-device.md`
/// section 4a — channels on another device of the account: the channel
/// subscription register written and applied, the channels subscribed before
/// this build reaching it, the pairing bundle's seeds both ways, the reader
/// device's screen, and the trap (nothing founds a PublisherGroup outside
/// New channel).
@MainActor
final class ChannelSubscriptionSyncTests: XCTestCase {

    private let idA = Data(repeating: 0xA1, count: 16)
    private let idB = Data(repeating: 0xB2, count: 16)
    private let secret1 = Data(repeating: 0x11, count: 32)
    private let secret2 = Data(repeating: 0x22, count: 32)
    private let readKey = Data(repeating: 0x33, count: 32)
    private let groupId = Data(repeating: 0x44, count: 32)
    private let minted = Data(repeating: 0x55, count: 32)

    private func hex(_ d: Data) -> String { d.map { String(format: "%02x", $0) }.joined() }
    /// ADR-0090 contract 3a — the owner key a co-publisher recorded at join.
    private let ownerAccountKey = Data(repeating: 0x0A, count: 32)

    private func register(
        _ id: Data, subscribed: Bool = true, role: ChannelSubscriptionRole = .subscriber,
        secret: Data? = nil, readKey: Data = Data(), groupId: Data = Data(), ownerKey: Data? = nil,
        name: String = "", handle: String = "", hlc: UInt64 = 10
    ) -> ChannelSubscriptionRegister {
        ChannelSubscriptionRegister(
            hlc: hlc, channelId: id, subscribed: subscribed, role: subscribed ? role : .subscriber,
            subscriberSecret: subscribed ? (secret ?? secret1) : Data(), readKey: subscribed ? readKey : Data(),
            publisherGroupId: subscribed ? groupId : Data(),
            ownerAccountKey: subscribed && role == .coPublisher ? (ownerKey ?? ownerAccountKey) : Data(),
            name: subscribed ? name : "",
            handle: subscribed ? handle : "")
    }

    private func local(
        _ id: Data, listed: Bool = true, secret: Data? = nil, publishes: Bool = false,
        coPublished: Data? = nil, readKey: Data? = nil, role: String? = nil, ownerKey: Data? = nil,
        name: String = "News", handle: String = "news", seeds: Bool = false
    ) -> ChannelSubscriptionPlan.Local {
        ChannelSubscriptionPlan.Local(
            id: hex(id), listed: listed, subscriberSecret: secret, publishesHere: publishes,
            coPublishedGroupIdHex: coPublished.map(hex), readKey: readKey, accountRole: role,
            ownerAccountKey: ownerKey ?? (coPublished != nil ? ownerAccountKey : nil),
            name: name, handle: handle, holdsSeeds: seeds)
    }

    private func plan(
        _ registers: [ChannelSubscriptionRegister], _ locals: [ChannelSubscriptionPlan.Local],
        ended: Set<String> = [], owedLeaves: Set<String> = [], leftAlone: Set<String> = []
    ) -> ChannelSubscriptionPlan.Outcome {
        ChannelSubscriptionPlan.plan(
            registers: registers, locals: locals, ended: ended, owedLeaves: owedLeaves,
            leftAlone: leftAlone, mintSecret: { self.minted })
    }

    // MARK: - Apply: a register from another device

    func testALiveRegisterForAnUnlistedChannelListsItWithTheAccountsKey() {
        let out = plan([register(idA, role: .owner, readKey: readKey, name: "News", handle: "news")], [])
        XCTAssertEqual(out.applies, [
            .add(id: hex(idA), name: "News", handle: "news"),
            .setKey(id: hex(idA), secret: secret1, replacing: nil),
            .extras(id: hex(idA), role: ChannelAccountRole.owner, readKey: readKey, coPublishedGroupId: nil),
        ])
        XCTAssertEqual(out.writes, [], "a device that only received the register writes none back")
    }

    func testACoPublishersRegisterCarriesItsGroupIdToTheReaderDevice() {
        let out = plan([register(idA, role: .coPublisher, groupId: groupId)], [])
        XCTAssertTrue(out.applies.contains(
            .extras(id: hex(idA), role: ChannelAccountRole.coPublisher, readKey: nil, coPublishedGroupId: groupId,
                    ownerAccountKey: ownerAccountKey)))
    }

    /// ADR-0090 contract 3a — a co-publisher's register carries the owner key
    /// its device recorded at join; a co-publisher with no recorded key
    /// writes nothing (the core would refuse it).
    func testACoPublishersRegisterCarriesTheRecordedOwnerKey() {
        let coPub = plan([], [local(idB, publishes: true, coPublished: groupId)])
        XCTAssertEqual(coPub.writes.first?.ownerAccountKey, ownerAccountKey)
        let noKey = ChannelSubscriptionPlan.desired(
            local: ChannelSubscriptionPlan.Local(
                id: hex(idB), listed: true, subscriberSecret: secret1, publishesHere: true,
                coPublishedGroupIdHex: hex(groupId), readKey: nil, accountRole: nil, ownerAccountKey: nil,
                name: "", handle: "", holdsSeeds: false),
            register: nil, forceSubscriber: false, mintSecret: { self.minted })
        XCTAssertNil(noKey)
        XCTAssertEqual(plan([], [local(idA, publishes: true)]).writes.first?.ownerAccountKey, Data(),
                       "an owner records none")
    }

    func testALeftRegisterDropsAReadersChannelButNeverOneThisDevicePublishes() {
        let left = register(idA, subscribed: false)
        XCTAssertEqual(plan([left], [local(idA, secret: secret1)]).applies, [.drop(id: hex(idA))])
        let publisher = plan([left], [local(idA, publishes: true)])
        XCTAssertEqual(publisher.applies, [])
        XCTAssertEqual(publisher.writes.map(\.role), [.owner], "the account still owns it: written live again")
    }

    func testALeftRegisterForgetsSeedsAPairingCarriedForAnUnlistedChannel() {
        let out = plan([register(idA, subscribed: false)], [local(idA, listed: false, seeds: true)])
        XCTAssertEqual(out.applies, [.dropCarried(id: hex(idA))])
    }

    func testOneKeyPerAccount_aDifferentLocalKeyIsReplacedByTheRegisters() {
        let out = plan([register(idA, secret: secret2, name: "News", handle: "news")], [local(idA, secret: secret1)])
        XCTAssertEqual(out.applies, [.setKey(id: hex(idA), secret: secret2, replacing: secret1)])
        XCTAssertEqual(out.writes, [], "the register's key is the account's: nothing to write")
    }

    func testAnEndedChannelOrAnOwedLeaveIsNotListedAgain() {
        XCTAssertEqual(plan([register(idA)], [], ended: [hex(idA)]).applies, [])
        XCTAssertEqual(plan([register(idA)], [], owedLeaves: [hex(idA)]).applies, [])
        XCTAssertEqual(plan([], [local(idA, secret: secret1)], owedLeaves: [hex(idA)]).writes, [])
    }

    func testAReaderDeviceKeepsTheOwnersRoleAndWritesNothing() {
        let held = register(idA, role: .owner, readKey: readKey, name: "News", handle: "news")
        let out = plan([held], [local(idA, secret: secret1, readKey: readKey, role: ChannelAccountRole.owner)])
        XCTAssertEqual(out.applies, [])
        XCTAssertEqual(out.writes, [], "a reader device never turns the owner into a subscriber")
    }

    // MARK: - Write: this device's channels (the migration included)

    func testAChannelSubscribedBeforeThisBuildIsWrittenOnce() {
        let out = plan([], [local(idA, secret: secret1, name: "News", handle: "@news")])
        XCTAssertEqual(out.writes, [ChannelSubscriptionPlan.Write(
            channelId: idA, role: .subscriber, subscriberSecret: secret1, readKey: Data(),
            publisherGroupId: Data(), name: "News", handle: "news")])
        let again = plan([register(idA, name: "News", handle: "news")], [local(idA, secret: secret1)])
        XCTAssertEqual(again.writes, [], "an identical register is not written again")
    }

    func testTheOwnersDeviceWritesTheOwnerWithASecretMintedForTheRegister() {
        let out = plan([], [local(idA, publishes: true, readKey: readKey)])
        XCTAssertEqual(out.writes.first?.role, .owner)
        XCTAssertEqual(out.writes.first?.subscriberSecret, minted)
        XCTAssertEqual(out.writes.first?.readKey, readKey)
        let coPub = plan([], [local(idB, publishes: true, coPublished: groupId)])
        XCTAssertEqual(coPub.writes.first?.role, .coPublisher)
        XCTAssertEqual(coPub.writes.first?.publisherGroupId, groupId)
    }

    func testAReadKeyLearnedHereIsWrittenToTheRegister() {
        let out = plan([register(idA, name: "News", handle: "news")], [local(idA, secret: secret1, readKey: readKey)])
        XCTAssertEqual(out.writes.first?.readKey, readKey)
    }

    func testAPublisherRemovedButStillSubscribedIsWrittenAsASubscriber() {
        let held = register(idA, role: .coPublisher, groupId: groupId, name: "News", handle: "news")
        let write = ChannelSubscriptionPlan.desired(
            local: local(idA, secret: secret1, coPublished: groupId), register: held,
            forceSubscriber: true, mintSecret: { self.minted })
        XCTAssertEqual(write?.role, .subscriber)
        XCTAssertEqual(write?.publisherGroupId, Data())
        XCTAssertEqual(write?.subscriberSecret, secret1)
    }

    func testANameOrHandleTheRegisterRefusesIsLeftOut() {
        XCTAssertEqual(ChannelSubscriptionPlan.sanitizedName("  News  "), "News")
        XCTAssertEqual(ChannelSubscriptionPlan.sanitizedName("a\nb"), "")
        XCTAssertEqual(ChannelSubscriptionPlan.sanitizedName(String(repeating: "x", count: 256)), "")
        XCTAssertEqual(ChannelSubscriptionPlan.sanitizedHandle("@news_1"), "news_1")
        XCTAssertEqual(ChannelSubscriptionPlan.sanitizedHandle("News"), "")
        XCTAssertEqual(ChannelSubscriptionPlan.sanitizedHandle("ab"), "")
    }

    func testTheCoreTakesWhatThePlanWritesAndASecondDeviceReadsIt() throws {
        // Device 1 writes what the plan says; device 2 applies the envelope.
        let device1 = SyncedState()
        let write = try XCTUnwrap(plan([], [local(idA, publishes: true, readKey: readKey)]).writes.first)
        XCTAssertTrue(try device1.setChannelSubscription(
            hlc: 100, channelId: write.channelId, role: write.role, subscriberSecret: write.subscriberSecret,
            readKey: write.readKey, publisherGroupId: write.publisherGroupId,
            ownerAccountKey: write.ownerAccountKey, name: write.name, handle: write.handle))
        let envelope = try XCTUnwrap(try device1.channelSubscriptionEnvelopeFor(channelId: idA))
        XCTAssertTrue(isChannelSubscriptionEnvelope(bytes: envelope))
        let device2 = SyncedState()
        XCTAssertTrue(try device2.applyChannelSubscriptionEnvelope(envelope: envelope))
        let out = plan(try device2.allChannelSubscriptions(), [])
        XCTAssertTrue(out.applies.contains(.setKey(id: hex(idA), secret: minted, replacing: nil)))
        XCTAssertTrue(out.applies.contains(
            .extras(id: hex(idA), role: ChannelAccountRole.owner, readKey: readKey, coPublishedGroupId: nil)))
    }

    // MARK: - The coordinator over a live channel state

    private final class FakePort: ChannelSubscriptionSync.RegisterPort {
        let state = SyncedState()
        var registersReady = true
        var endedChannelIds: Set<String> = []
        var registers: [ChannelSubscriptionRegister] { (try? state.allChannelSubscriptions()) ?? [] }
        var hlc: UInt64 = 1_000
        func writeLive(_ write: ChannelSubscriptionPlan.Write) -> Bool? {
            guard registersReady else { return nil }
            hlc += 1
            return (try? state.setChannelSubscription(
                hlc: hlc, channelId: write.channelId, role: write.role, subscriberSecret: write.subscriberSecret,
                readKey: write.readKey, publisherGroupId: write.publisherGroupId,
                ownerAccountKey: write.ownerAccountKey, name: write.name,
                handle: write.handle)) != nil
        }
        func writeLeave(channelId: Data) -> Bool? {
            guard registersReady else { return nil }
            hlc += 1
            return (try? state.leaveChannelSubscription(hlc: hlc, channelId: channelId)) != nil
        }
    }

    private func bound(_ port: FakePort) -> (ChannelSubscriptionSync, ChannelsUiState, UserDefaults) {
        let suite = "channel-subscription-sync-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        let sync = ChannelSubscriptionSync()
        sync.mintSecret = { self.minted }
        sync.unsubscribeOldKey = { _, _ in }
        let channels = ChannelsUiState()
        sync.bind(
            accountIdB64: "acct-\(UUID().uuidString)", channels: channels, port: port,
            changes: Empty().eraseToAnyPublisher(), defaults: defaults)
        return (sync, channels, defaults)
    }

    func testARegisterFromAnotherDeviceListsTheChannelWithTheSharedKey() throws {
        let port = FakePort()
        XCTAssertTrue(try port.state.setChannelSubscription(
            hlc: 5, channelId: idA, role: .owner, subscriberSecret: secret1, readKey: readKey,
            publisherGroupId: Data(), ownerAccountKey: Data(), name: "News", handle: "news"))
        let (sync, channels, _) = bound(port)
        sync.reconcileNow()
        let id = hex(idA)
        XCTAssertEqual(channels.channels.map(\.id), [id])
        XCTAssertEqual(channels.subscriberKeysByChannel[id]?.secret, secret1)
        XCTAssertEqual(channels.extras(id).readKey, readKey)
        XCTAssertEqual(channels.readerDeviceRole(id), .owner, "no PublisherGroup here: a reader")
        XCTAssertNil(channels.keysByChannel[id])
    }

    func testAnUnsubscribeHereIsALeaveAtOnceAndTheChannelIsNotListedAgain() throws {
        let port = FakePort()
        let (sync, channels, _) = bound(port)
        let id = hex(idA)
        channels.addChannel(ChannelHandle(id: id, localName: "News", localDescription: "", addedAtMs: 1))
        _ = channels.subscriberKeysForSubscribe(id)
        sync.reconcileNow()
        XCTAssertEqual(try port.state.isChannelSubscribed(channelId: idA), true)
        // What ``ChannelUnsubscribe/stopFollowing(state:channelId:)`` does
        // locally, then the shell's end hook.
        channels.clearSubscriberKeys(id)
        channels.removeChannel(id)
        channels.onRelationshipEnded?(id)
        XCTAssertEqual(try port.state.isChannelSubscribed(channelId: idA), false, "folded before any reconcile")
        sync.reconcileNow()
        XCTAssertEqual(channels.channels, [])
    }

    func testAnUnsubscribeBeforeTheFoldIsLoadedStaysOwed() throws {
        let port = FakePort()
        XCTAssertTrue(try port.state.setChannelSubscription(
            hlc: 5, channelId: idA, role: .subscriber, subscriberSecret: secret1, readKey: Data(),
            publisherGroupId: Data(), ownerAccountKey: Data(), name: "", handle: ""))
        port.registersReady = false
        let (sync, channels, _) = bound(port)
        channels.onRelationshipEnded?(hex(idA))
        port.registersReady = true
        sync.reconcileNow()
        XCTAssertEqual(channels.channels, [], "the held live register did not list it back")
        XCTAssertEqual(try port.state.isChannelSubscribed(channelId: idA), false)
    }

    func testASubscribeHereReusesTheRegistersKeyAndASubscribeAfterALeaveWins() throws {
        let port = FakePort()
        XCTAssertTrue(try port.state.setChannelSubscription(
            hlc: 5, channelId: idA, role: .subscriber, subscriberSecret: secret1, readKey: Data(),
            publisherGroupId: Data(), ownerAccountKey: Data(), name: "", handle: ""))
        let (sync, channels, _) = bound(port)
        XCTAssertEqual(channels.subscriberKeysForSubscribe(hex(idA)).secret, secret1)

        XCTAssertTrue(try port.state.setChannelSubscription(
            hlc: 6, channelId: idB, role: .subscriber, subscriberSecret: secret2, readKey: Data(),
            publisherGroupId: Data(), ownerAccountKey: Data(), name: "", handle: ""))
        XCTAssertTrue(try port.state.leaveChannelSubscription(hlc: 7, channelId: idB))
        let id = hex(idB)
        channels.addChannel(ChannelHandle(id: id, localName: "Other", localDescription: "", addedAtMs: 1))
        let fresh = channels.subscriberKeysForSubscribe(id)
        XCTAssertNotEqual(fresh.secret, secret2, "no live register: a key of its own")
        sync.reconcileNow()
        XCTAssertEqual(channels.channels.map(\.id).contains(id), true, "the held leave did not drop it")
        XCTAssertEqual(try port.state.channelSubscription(channelId: idB)?.subscriberSecret, fresh.secret)
    }

    // MARK: - The reader device's screen (UX 4a)

    func testAReaderDeviceShowsTheLineHidesTheExitBlockAndAnnouncesNothing() {
        let channels = ChannelsUiState()
        let id = hex(idA)
        channels.addChannel(ChannelHandle(id: id, localName: "News", localDescription: "", addedAtMs: 1))
        XCTAssertNil(channels.readerDeviceRole(id), "a subscriber is no reader device")
        channels.updateExtras(id) { $0.accountRole = ChannelAccountRole.owner }
        XCTAssertEqual(channels.readerDeviceRole(id)?.lineKey, "channels.reader_device.owner")
        channels.updateExtras(id) { $0.accountRole = ChannelAccountRole.coPublisher }
        XCTAssertEqual(channels.readerDeviceRole(id)?.lineKey, "channels.reader_device.publisher")
        channels.setKeys(channelId: id, keys: ChannelPublisherKeys(
            seed: Data(repeating: 1, count: 32), signingKeySeed: Data(repeating: 2, count: 32)))
        XCTAssertNil(channels.readerDeviceRole(id), "the publishing device posts")
        XCTAssertFalse(ChannelOwnership.isOwner(channelId: id, state: channels), "a co-publisher is never the owner")
        channels.noteNoPublisherGroupHere(id)
        XCTAssertEqual(channels.readerDeviceRole(id), .publisher, "no group in the provider: a reader")

        let subscriber: [InfoSection] = [.handle, .channelDescription, .subscribers, .membersCanFindMe, .unsubscribe]
        XCTAssertEqual(
            ChannelReaderDeviceInfo.sections(readerDevice: true, subscriber),
            [.handle, .channelDescription, .subscribers, .membersCanFindMe])
        XCTAssertEqual(ChannelReaderDeviceInfo.sections(readerDevice: false, subscriber), subscriber)
    }

    // MARK: - The trap: nothing founds a PublisherGroup outside New channel

    func testOpeningAChannelWithNoGroupHereNeverFoundsOne() throws {
        // ADR-0090 — a bound DEVICE identity and the owner account seed.
        // As in the app: the noted owner seed is the 64-byte account identity
        // seed; the core and the device binding take its 32-byte Ed25519 seed.
        let identitySeed = Data(repeating: 0x6C, count: 64)
        let accountSeed = try Identity.fromSeed(seed: identitySeed).ed25519Seed()
        let identity = try Identity.fromSeedBound(
            seed: Data(repeating: 0x6D, count: 32), accountSeed: accountSeed, issuedAtUnixSecs: 1_700_000_000)
        ChannelPublisherIdentity.noteOwnerSeed(identitySeed)
        defer { ChannelPublisherIdentity.noteOwnerSeed(nil) }
        let channelHex = hex(idA)
        XCTAssertThrowsError(try CoPublisher.openPublisherGroup(
            identity: identity, accountIdB64: nil, channelIdHex: channelHex)) { error in
            guard case .notOnThisDevice = error as? CoPublisherError else {
                return XCTFail("expected notOnThisDevice, got \(error)")
            }
        }
        XCTAssertThrowsError(try ChannelsComposer.freshPublisherKeys(
            identity: identity, accountIdB64: nil, channelIdHex: channelHex)) { error in
            guard case .notOnThisDevice = error as? CoPublisherError else {
                return XCTFail("expected notOnThisDevice, got \(error)")
            }
        }
        XCTAssertNil(try publisherGroupLoad(identity: identity, ownerAccountSeed: accountSeed, channelId: idA), "nothing was founded")
        // New channel founds.
        _ = try ChannelsComposer.freshPublisherKeys(
            identity: identity, accountIdB64: nil, channelIdHex: channelHex, mayFound: true)
        XCTAssertNotNil(try publisherGroupLoad(identity: identity, ownerAccountSeed: accountSeed, channelId: idA))
        XCTAssertNoThrow(try CoPublisher.openPublisherGroup(
            identity: identity, accountIdB64: nil, channelIdHex: channelHex))
    }

    // MARK: - The pairing bundle's channel seeds (v4)

    func testTheContractGoldenDecodes() throws {
        let wire = #"""
        {"v":4,"contacts":[],"pins":[],"knownGroupIdsB64":[],"ownPredecessors":[],"groupNames":[],\#
        "profiles":[],"pendingContacts":[],"channelSeeds":[{"channelIdB64":"wcHBwcHBwcHBwcHBwcHBwQ==",\#
        "seeds":[{"epoch":41,"seedB64":"AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE="},\#
        {"epoch":42,"seedB64":"AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI="}]}]}
        """#
        let bundle = try XCTUnwrap(StateBundle.decode(Data(wire.utf8)))
        let seeds = StateBundle.decodedChannelSeeds(bundle.channelSeeds)
        XCTAssertEqual(seeds, [String(repeating: "c1", count: 16): [
            41: Data(repeating: 1, count: 32), 42: Data(repeating: 2, count: 32)]])
        // The sender builds the same entry from the same seeds.
        XCTAssertEqual(
            StateBundle.carriedChannelSeeds(seeds, perChannel: 32), bundle.channelSeeds)
        XCTAssertEqual(StateBundle.decode(StateBundle.encode(bundle)), bundle)
    }

    func testTheSenderCarriesTheNewest32AscendingInChannelIdOrder() {
        var many: [UInt32: Data] = [:]
        for epoch in UInt32(1)...40 { many[epoch] = Data(repeating: UInt8(epoch), count: 32) }
        let held = [hex(idB): many, hex(idA): [7: Data(repeating: 7, count: 32), 3: Data(count: 31)]]
        let carried = StateBundle.carriedChannelSeeds(held, perChannel: 32)
        XCTAssertEqual(carried.map(\.channelIdB64), [idA.base64EncodedString(), idB.base64EncodedString()])
        XCTAssertEqual(carried[0].seeds.map(\.epoch), [7], "a seed that is not 32 bytes is left out")
        XCTAssertEqual(carried[1].seeds.map(\.epoch), Array(UInt32(9)...40))
    }

    func testTheDecoderRefusesAMalformedSeedArray() {
        func entry(_ id: Data, _ seeds: [(UInt32, Int)]) -> StateBundle.CarriedChannelSeeds {
            StateBundle.CarriedChannelSeeds(channelIdB64: id.base64EncodedString(), seeds: seeds.map {
                StateBundle.CarriedSeed(epoch: $0.0, seedB64: Data(repeating: 1, count: $0.1).base64EncodedString())
            })
        }
        XCTAssertTrue(StateBundle.validChannelSeeds([entry(idA, [(1, 32), (2, 32)])]))
        XCTAssertFalse(StateBundle.validChannelSeeds([entry(idA, [(1, 32)]), entry(idA, [(2, 32)])]), "an id twice")
        XCTAssertFalse(StateBundle.validChannelSeeds([entry(Data(count: 15), [(1, 32)])]), "an id not 16 bytes")
        XCTAssertFalse(StateBundle.validChannelSeeds([entry(idA, [])]), "no seed")
        XCTAssertFalse(StateBundle.validChannelSeeds([entry(idA, (0..<33).map { ($0, 32) })]), "33 seeds")
        XCTAssertFalse(StateBundle.validChannelSeeds([entry(idA, [(2, 32), (2, 32)])]), "not strictly ascending")
        XCTAssertFalse(StateBundle.validChannelSeeds([entry(idA, [(1, 31)])]), "a seed not 32 bytes")
        let bundle = StateBundle.V4(
            contacts: [], pins: [], knownGroupIdsB64: [], channelSeeds: [entry(idA, [(2, 32), (1, 32)])])
        XCTAssertNil(StateBundle.decode(StateBundle.encode(bundle)), "the whole bundle is refused")
    }

    func testSeedsStepDownForEveryChannelAndNeverPushOutAnotherArray() {
        var many: [UInt32: Data] = [:]
        for epoch in UInt32(1)...32 { many[epoch] = Data(repeating: UInt8(epoch), count: 32) }
        let seeds = StateBundle.carriedChannelSeeds([hex(idA): many, hex(idB): many], perChannel: 32)
        let base = StateBundle.V4(contacts: [], pins: [], knownGroupIdsB64: ["Z3JvdXA="])
        let unbounded = StateBundle.withChannelSeeds(base, seeds, cap: 1 << 20)
        XCTAssertEqual(unbounded.channelSeeds.map(\.seeds.count), [32, 32])
        // A cap that fits 8 per channel but not 16.
        let at8 = StateBundle.encode(StateBundle.withChannelSeeds(base, seeds.map {
            StateBundle.CarriedChannelSeeds(channelIdB64: $0.channelIdB64, seeds: Array($0.seeds.suffix(8)))
        }, cap: 1 << 20)).count
        let fitted = StateBundle.withChannelSeeds(base, seeds, cap: at8)
        XCTAssertEqual(fitted.channelSeeds.map(\.seeds.count), [8, 8])
        XCTAssertEqual(fitted.channelSeeds.first?.seeds.first?.epoch, 25, "the newest are kept")
        let none = StateBundle.withChannelSeeds(base, seeds, cap: StateBundle.encode(base).count + 40)
        XCTAssertEqual(none.channelSeeds, [])
        XCTAssertEqual(none.knownGroupIdsB64, base.knownGroupIdsB64)
    }

    func testTheReceiverNeverOverwritesAHeldSeed() {
        let held: [String: [UInt32: Data]] = [hex(idA): [1: Data(repeating: 9, count: 32)]]
        let carried: [String: [UInt32: Data]] = [
                       hex(idA): [1: Data(repeating: 1, count: 32), 2: Data(repeating: 2, count: 32)],
                       hex(idB): [5: Data(repeating: 5, count: 32)]]
        let merged = StateBundle.mergedChannelSeeds(held: held, carried: carried)
        XCTAssertEqual(merged[hex(idA)]?[1], Data(repeating: 9, count: 32))
        XCTAssertEqual(merged[hex(idA)]?[2], Data(repeating: 2, count: 32))
        XCTAssertEqual(merged[hex(idB)]?[5], Data(repeating: 5, count: 32), "kept before its register arrives")
    }

    func testCarriedSeedsLandInTheLiveStateAndTheBundleCarriesThemBack() {
        let port = FakePort()
        let (sync, channels, _) = bound(port)
        let account = sync.boundAccountIdB64!
        channels.setSubscribedSeed(channelId: hex(idA), epoch: 1, seed: Data(repeating: 9, count: 32))
        sync.storeCarriedSeeds(accountIdB64: account, carried: StateBundle.carriedChannelSeeds(
            [hex(idA): [1: Data(repeating: 1, count: 32), 2: Data(repeating: 2, count: 32)]], perChannel: 32))
        XCTAssertEqual(channels.subscribedSeeds(channelId: hex(idA))[1], Data(repeating: 9, count: 32))
        XCTAssertEqual(channels.subscribedSeeds(channelId: hex(idA))[2], Data(repeating: 2, count: 32))
        // No live register: nothing to hand over yet.
        XCTAssertEqual(sync.carriedSeedsForBundle(), [])
        channels.addChannel(ChannelHandle(id: hex(idA), localName: "News", localDescription: "", addedAtMs: 1))
        _ = port.writeLive(ChannelSubscriptionPlan.Write(
            channelId: idA, role: .subscriber, subscriberSecret: secret1, readKey: Data(),
            publisherGroupId: Data(), ownerAccountKey: Data(), name: "", handle: ""))
        XCTAssertEqual(sync.carriedSeedsForBundle().first?.seeds.map(\.epoch), [1, 2])
    }

    // MARK: - Follow-up decision 1: the account's reader key is a real reader

    func testARotationResealsToTheAccountsReaderKeyAboveTheMark() {
        let rows = [
            ChannelsClient.SubscriberRow(seq: 1, pubkeyHex: "aa", sealedTicket: nil),
            ChannelsClient.SubscriberRow(seq: 9, pubkeyHex: "bb", sealedTicket: nil),
        ]
        let throughMark = ChannelReseal.resealKeys(rows: rows, throughSeq: 3)
        XCTAssertEqual(throughMark, ["aa"], "a row above the mark is not resealed to")
        XCTAssertEqual(ChannelReseal.withOwnReaderKey(throughMark, "BB"), ["aa", "bb"],
                       "the account's own reader key is, above the mark too")
        XCTAssertEqual(ChannelReseal.withOwnReaderKey(["aa", "bb"], "bb"), ["aa", "bb"], "added once")
        XCTAssertEqual(ChannelReseal.withOwnReaderKey(["aa"], nil), ["aa"])
    }

    func testTheLetInPassNeverRemovesTheAccountsReaderKey() {
        XCTAssertTrue(SubscriberAdmission.isOwnRow("BB", ownKeyHex: nil, accountReaderHex: "bb"))
        XCTAssertTrue(SubscriberAdmission.isOwnRow("aa", ownKeyHex: "aa", accountReaderHex: nil))
        XCTAssertFalse(SubscriberAdmission.isOwnRow("cc", ownKeyHex: "aa", accountReaderHex: "bb"))
        XCTAssertEqual(SubscriberAdmission.letInVerdict(
            seq: 9, ticketLive: false, mark: 3, markSet: true,
            isOwnKey: SubscriberAdmission.isOwnRow("bb", ownKeyHex: nil, accountReaderHex: "bb")), .leave)
    }

    private final class ReaderKeyCalls {
        var publicCalls: [String] = []
        var privateCalls: [String] = []
        var seeds: [String] = []
        var fail = false
        func ports() -> ChannelAccountReaderKey.Ports {
            ChannelAccountReaderKey.Ports(
                subscribePublic: { _, key in
                    if self.fail { throw URLError(.notConnectedToInternet) }
                    self.publicCalls.append(key)
                },
                subscribePrivate: { _, key in
                    if self.fail { throw URLError(.notConnectedToInternet) }
                    self.privateCalls.append(key)
                },
                handCurrentSeed: { _, key in self.seeds.append(key) })
        }
    }

    func testThePublishingDeviceRegistersTheAccountsKeyOnceAndRetriesAFailureLater() async {
        ChannelAccountReaderKey.reset()
        let channels = ChannelsUiState()
        let id = hex(idA)
        let key = String(repeating: "ab", count: 32)
        channels.addChannel(ChannelHandle(id: id, localName: "News", localDescription: "", addedAtMs: 1))
        let calls = ReaderKeyCalls()
        // A reader device (no PublisherGroup here) registers nothing.
        await ChannelAccountReaderKey.ensure(channelId: id, state: channels, keyHex: key, ports: calls.ports())
        XCTAssertEqual(calls.privateCalls + calls.publicCalls, [])

        channels.setKeys(channelId: id, keys: ChannelPublisherKeys(
            seed: Data(repeating: 1, count: 32), signingKeySeed: Data(repeating: 2, count: 32)))
        channels.updateExtras(id) { $0.recordPublic = false }
        calls.fail = true
        await ChannelAccountReaderKey.ensure(
            channelId: id, state: channels, keyHex: key, ports: calls.ports(), nowMs: 1_000)
        XCTAssertNil(channels.extras(id).readerKeyRegisteredHex, "not recorded after a failure")
        calls.fail = false
        await ChannelAccountReaderKey.ensure(
            channelId: id, state: channels, keyHex: key, ports: calls.ports(), nowMs: 2_000)
        XCTAssertEqual(calls.privateCalls, [], "a failure waits before the next try")
        let later = 1_000 + ChannelAccountReaderKey.retryAfterMs
        await ChannelAccountReaderKey.ensure(
            channelId: id, state: channels, keyHex: key, ports: calls.ports(), nowMs: later)
        XCTAssertEqual(calls.privateCalls, [key], "a private channel: the invite path")
        XCTAssertEqual(calls.seeds, [key], "handed the current seed at once")
        XCTAssertEqual(channels.extras(id).readerKeyRegisteredHex, key)
        await ChannelAccountReaderKey.ensure(
            channelId: id, state: channels, keyHex: key, ports: calls.ports(), nowMs: later + 1)
        XCTAssertEqual(calls.privateCalls, [key], "registered once")
        // A new register key (another device's won) is registered again.
        let other = String(repeating: "cd", count: 32)
        channels.updateExtras(id) { $0.recordPublic = true }
        await ChannelAccountReaderKey.ensure(
            channelId: id, state: channels, keyHex: other, ports: calls.ports(), nowMs: later + 2)
        XCTAssertEqual(calls.publicCalls, [other], "a public channel: the plain subscribe")
    }

    /// A channel whose flag is not known yet registers nothing this pass (a
    /// public subscribe of a private channel would only fail).
    func testAnUnknownFlagWaitsForTheNextPass() async {
        ChannelAccountReaderKey.reset()
        let channels = ChannelsUiState()
        let id = hex(idA)
        channels.addChannel(ChannelHandle(id: id, localName: "News", localDescription: "", addedAtMs: 1))
        channels.setKeys(channelId: id, keys: ChannelPublisherKeys(
            seed: Data(repeating: 1, count: 32), signingKeySeed: Data(repeating: 2, count: 32)))
        XCTAssertNil(channels.extras(id).isPublic)
        let calls = ReaderKeyCalls()
        await ChannelAccountReaderKey.ensure(
            channelId: id, state: channels, keyHex: String(repeating: "ab", count: 32), ports: calls.ports())
        XCTAssertEqual(calls.privateCalls + calls.publicCalls, [])
        XCTAssertNil(channels.extras(id).readerKeyRegisteredHex)
    }

    /// Live iPhone walk (2026-11-14) — a new private channel has no admission
    /// seed until its first invite link: the owner's device mints version 0
    /// for the reader key's ticket; a co-publisher's device never mints.
    func testTheOwnersDeviceMintsTheFirstAdmissionSeedForTheReaderKey() {
        let account = "reader-\(UUID().uuidString)"
        let previousBound = AcceptedContactsStore.boundAccountIdB64
        AcceptedContactsStore.bind(accountIdB64: account)
        defer {
            ChannelAdmissionKeyStore.delete(accountIdB64: account)
            AcceptedContactsStore.bind(accountIdB64: previousBound)
        }
        let channels = ChannelsUiState(accountIdB64: account)
        defer { ChannelsUiState.forget(accountIdB64: account) }
        let owned = hex(idA)
        let copublished = hex(idB)
        for id in [owned, copublished] {
            channels.addChannel(ChannelHandle(id: id, localName: "News", localDescription: "", addedAtMs: 1))
            channels.setKeys(channelId: id, keys: ChannelPublisherKeys(
                seed: Data(repeating: 1, count: 32), signingKeySeed: Data(repeating: 2, count: 32)))
        }
        channels.updateExtras(copublished) { $0.accountRole = ChannelAccountRole.coPublisher }
        XCTAssertTrue(ChannelInvites.admissionSeeds(channelIdHex: owned).isEmpty, "none before")

        let minted = ChannelAccountReaderKey.admissionForReaderKey(channelId: owned, state: channels)
        XCTAssertEqual(minted?.version, 0)
        XCTAssertEqual(minted?.seed.count, 32)
        XCTAssertEqual(ChannelInvites.admissionSeeds(channelIdHex: owned).map(\.seed), minted.map { [$0.seed] })
        // The same seed the next time: minted once.
        XCTAssertEqual(ChannelAccountReaderKey.admissionForReaderKey(channelId: owned, state: channels)?.seed,
                       minted?.seed)

        XCTAssertNil(ChannelAccountReaderKey.admissionForReaderKey(channelId: copublished, state: channels),
                     "a co-publisher waits for the owner's seed")
        XCTAssertTrue(ChannelInvites.admissionSeeds(channelIdHex: copublished).isEmpty)
    }

    // MARK: - Follow-up decision 2: orphans of a key reset are left alone

    func testAnOrphanIsNeverListedBackNorWritten() {
        let out = plan(
            [register(idA, role: .owner)], [local(idB, secret: secret1)],
            leftAlone: [hex(idA), hex(idB)])
        XCTAssertEqual(out.applies, [])
        XCTAssertEqual(out.writes, [])
    }

    func testRemoveFromThisDeviceIsRememberedAndLeftAlone() {
        let account = "acct-\(UUID().uuidString)"
        let channels = ChannelsUiState(accountIdB64: account)
        defer { ChannelsUiState.forget(accountIdB64: account) }
        let id = hex(idA)
        channels.addChannel(ChannelHandle(id: id, localName: "News", localDescription: "", addedAtMs: 1))
        channels.recordOwnerKeyGeneration(channelId: id, generation: 1)
        channels.currentKeyGeneration = 2
        XCTAssertEqual(channels.channelsLeftAloneByRegister(), [id], "orphaned by the reset")
        var ended: [String] = []
        channels.onRelationshipEnded = { ended.append($0) }
        channels.removeOrphanedChannel(id)
        XCTAssertEqual(channels.channelsLeftAloneByRegister(), [id], "removed, still left alone")
        XCTAssertEqual(ended, [], "no leave is written for it")
        XCTAssertEqual(RemovedOrphanChannels.load(accountIdB64: account), [id])
        ChannelsUiState.forget(accountIdB64: account)
        XCTAssertEqual(RemovedOrphanChannels.load(accountIdB64: account), [])
    }
}
