// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0087 section 15 — a publisher's keys carry the channel epoch their
/// seed serves; nothing is sealed under an epoch the channel service has not
/// reached; a co-publisher keeps the replaced seed; only the owner rotates;
/// the record re-post after a commit is owed until it lands. Mirrors web's
/// `publisher-seed.test.ts`.
final class ChannelPublisherSeedTests: XCTestCase {

    private let old = Data(repeating: 0x0A, count: 32)
    private let new = Data(repeating: 0x0B, count: 32)
    private let signing = Data(repeating: 0x5E, count: 32)

    private func keys(_ seed: Data, epoch: UInt32?) -> ChannelPublisherKeys {
        ChannelPublisherKeys(seed: seed, signingKeySeed: signing, seedEpoch: epoch)
    }

    // MARK: - Sealing

    func testKeysWithNoServedEpochSealUnderTheChannelsCurrentEpoch() {
        let sealing = ChannelPublisherSeed.composer(keys: keys(new, epoch: nil), channelEpoch: 4, held: { _ in nil })
        XCTAssertEqual(sealing?.epoch, 4)
        XCTAssertEqual(sealing?.seed, new)
    }

    func testTheComposerSeedSealsUnderTheEpochItServesOnceTheChannelReachedIt() {
        let reached = ChannelPublisherSeed.composer(keys: keys(new, epoch: 5), channelEpoch: 5, held: { _ in nil })
        XCTAssertEqual(reached?.epoch, 5)
        XCTAssertEqual(reached?.seed, new)
        // The service is ahead of this device (later commits not applied
        // yet): still the epoch the seed serves, which every reader holds.
        let ahead = ChannelPublisherSeed.composer(keys: keys(new, epoch: 5), channelEpoch: 7, held: { _ in nil })
        XCTAssertEqual(ahead?.epoch, 5)
        XCTAssertEqual(ahead?.seed, new)
    }

    /// A co-publisher applied the owner's commit before the owner's reseal
    /// landed: it seals with the seed it kept for the epoch the service is at.
    func testATrailingChannelSealsWithTheKeptSeedOfItsEpoch() {
        let sealing = ChannelPublisherSeed.composer(
            keys: keys(new, epoch: 5), channelEpoch: 4, held: { $0 == 4 ? self.old : nil })
        XCTAssertEqual(sealing?.epoch, 4)
        XCTAssertEqual(sealing?.seed, old)
    }

    /// The join race: a publisher that joined before the owner's reseal moved
    /// the service holds no seed of the epoch the service is at, so nothing
    /// is sealed until the service reaches its epoch.
    func testNothingIsSealedUnderAnEpochTheServiceHasNotReached() {
        XCTAssertNil(ChannelPublisherSeed.composer(keys: keys(new, epoch: 1), channelEpoch: 0, held: { _ in nil }))
    }

    func testThePublicSeedIsFiledOnlyUnderAnEpochTheChannelReached() {
        XCTAssertEqual(ChannelPublisherLoops.publicSeedEpoch(keys: keys(new, epoch: 3), channelEpoch: 3), 3)
        XCTAssertEqual(ChannelPublisherLoops.publicSeedEpoch(keys: keys(new, epoch: 3), channelEpoch: 5), 3)
        XCTAssertNil(ChannelPublisherLoops.publicSeedEpoch(keys: keys(new, epoch: 4), channelEpoch: 3))
        XCTAssertEqual(ChannelPublisherLoops.publicSeedEpoch(keys: keys(new, epoch: nil), channelEpoch: 3), 3)
    }

    // MARK: - Reading

    func testTheComposerSeedOpensOnlyTheEpochItServes() {
        let map = ChannelPublisherSeed.readSeeds(
            kept: [3: old], keys: keys(new, epoch: 4), channelEpoch: 3)
        XCTAssertEqual(map, [3: old, 4: new])
        let legacy = ChannelPublisherSeed.readSeeds(kept: [:], keys: keys(new, epoch: nil), channelEpoch: 6)
        XCTAssertEqual(legacy, [6: new])
    }

    // MARK: - The replaced seed

    func testTheReplacedSeedIsKeptUnderTheEpochItServed() {
        let kept = ChannelPublisherSeed.outgoing(
            previous: keys(old, epoch: 2), nextEpoch: 3, nextSeed: new, held: { _ in false })
        XCTAssertEqual(kept?.epoch, 2)
        XCTAssertEqual(kept?.seed, old)
        // Not known: the epoch before the new one.
        XCTAssertEqual(ChannelPublisherSeed.outgoing(
            previous: keys(old, epoch: nil), nextEpoch: 3, nextSeed: new, held: { _ in false })?.epoch, 2)
        XCTAssertNil(ChannelPublisherSeed.outgoing(
            previous: keys(old, epoch: nil), nextEpoch: 0, nextSeed: new, held: { _ in false }))
        XCTAssertNil(ChannelPublisherSeed.outgoing(
            previous: keys(new, epoch: 2), nextEpoch: 3, nextSeed: new, held: { _ in false }), "the same seed")
        XCTAssertNil(ChannelPublisherSeed.outgoing(
            previous: keys(old, epoch: 2), nextEpoch: 3, nextSeed: new, held: { $0 == 2 }), "already held")
    }

    /// A co-publisher's poll applied the owner's commit: the old seed stays
    /// under its epoch, the composer takes the new one at the group's epoch,
    /// the channel's current epoch is left alone, and posts keep sealing
    /// under the committed epoch until the owner's reseal moves the channel.
    @MainActor
    func testACoPublisherFollowsTheOwnersCommitWithoutSealingAhead() {
        let channels = ChannelsUiState()
        let id = String(repeating: "d", count: 32)
        channels.addChannel(ChannelHandle(id: id, localName: "C", localDescription: "", addedAtMs: 1))
        channels.setKeys(channelId: id, keys: keys(old, epoch: 2))
        channels.setEpoch(channelId: id, epoch: 2)

        channels.notePublisherSeedAt(channelId: id, epoch: 3, seed: new)

        XCTAssertEqual(channels.keysByChannel[id]?.seed, new)
        XCTAssertEqual(channels.keysByChannel[id]?.seedEpoch, 3)
        XCTAssertEqual(channels.keysByChannel[id]?.signingKeySeed, signing)
        XCTAssertEqual(channels.epochByChannel[id], 2, "the channel's epoch is the service's, not the group's")
        XCTAssertEqual(channels.subscribedSeeds(channelId: id)[2], old)
        XCTAssertEqual(channels.readSeeds(channelId: id), [2: old, 3: new])
        // The reseal has not landed: sealed under 2 with the old seed.
        XCTAssertEqual(channels.composerSeed(channelId: id)?.epoch, 2)
        XCTAssertEqual(channels.composerSeed(channelId: id)?.seed, old)
        // It landed.
        channels.setEpoch(channelId: id, epoch: 3)
        XCTAssertEqual(channels.composerSeed(channelId: id)?.epoch, 3)
        XCTAssertEqual(channels.composerSeed(channelId: id)?.seed, new)
        // Idempotent.
        channels.notePublisherSeedAt(channelId: id, epoch: 3, seed: new)
        XCTAssertEqual(channels.subscribedSeeds(channelId: id), [2: old, 3: new])
    }

    // MARK: - Who rotates

    func testOnlyTheOwnersDeviceRotates() {
        XCTAssertTrue(ChannelPublisherLoops.rotates(publishes: true, coPublished: false))
        XCTAssertFalse(ChannelPublisherLoops.rotates(publishes: true, coPublished: true), "a co-publisher")
        XCTAssertFalse(ChannelPublisherLoops.rotates(publishes: false, coPublished: false), "a subscriber")
    }

    // MARK: - The owed record

    private struct Failure: Error {}

    @MainActor
    func testTheRecordRePostLandsOnlyWhenEveryPartLanded() async {
        let meta = SharedChannelMeta(
            name: "News", description: "", updatedAtMs: 1, topic: 0, language: "", listed: false,
            handle: nil, avatar: nil, reactions: nil)
        var sent: [String] = []
        let ok = await ChannelPublisherLoops.reissueRecord(
            meta: meta, banList: Data([1]),
            post: { _ in sent.append("ban") }, postMeta: { _ in sent.append("meta") })
        XCTAssertTrue(ok)
        XCTAssertEqual(sent, ["meta", "ban"])

        let metaFails = await ChannelPublisherLoops.reissueRecord(
            meta: meta, banList: nil, post: { _ in }, postMeta: { _ in throw Failure() })
        XCTAssertFalse(metaFails, "a failed record stays owed")
        let banFails = await ChannelPublisherLoops.reissueRecord(
            meta: meta, banList: Data([1]), post: { _ in throw Failure() }, postMeta: { _ in })
        XCTAssertFalse(banFails, "a failed ban list stays owed")
        let nothing = await ChannelPublisherLoops.reissueRecord(
            meta: nil, banList: nil, post: { _ in throw Failure() }, postMeta: { _ in throw Failure() })
        XCTAssertTrue(nothing, "nothing to re-post is nothing owed")
    }

    /// The owed mark is part of the stored extras, and a row written before
    /// it decodes as not owed.
    func testTheOwedMarkIsStoredAndOldRowsDecode() throws {
        var extras = ChannelExtras()
        extras.recordOwed = true
        let decoded = try JSONDecoder().decode(ChannelExtras.self, from: JSONEncoder().encode(extras))
        XCTAssertEqual(decoded.recordOwed, true)
        var json = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: JSONEncoder().encode(ChannelExtras())) as? [String: Any])
        json.removeValue(forKey: "recordOwed")
        let old = try JSONDecoder().decode(ChannelExtras.self, from: JSONSerialization.data(withJSONObject: json))
        XCTAssertNil(old.recordOwed)
    }
}
