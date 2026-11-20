// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0087 section 15 — which seed a publisher seals with, and which it
/// reads with, now that a publisher's keys carry the channel epoch their seed
/// serves (``ChannelPublisherKeys/seedEpoch``). Pure; the twin of web's
/// `apps/web/src/channels/publisher-seed.ts` and Android's rule.
///
/// Only the channel's owner commits to the PublisherGroup (maintainer,
/// 2026-11-08: only the owner rotates), and each of its commits has exactly
/// one reseal moving the channel service by one epoch, so the group's epoch
/// IS the channel epoch the group's seed serves. A co-publisher applies the
/// owner's commit from the group journal before the owner's reseal moves the
/// channel service; the readers get the new seed only with that reseal. So
/// nothing is ever sealed under an epoch the service has not reached.
enum ChannelPublisherSeed {

    /// The (epoch, seed) to seal a post, an admission or a public seed with.
    ///
    /// - the channel epoch has reached `keys.seedEpoch` (or passed it, when
    ///   this device has not applied later commits yet): the composer's seed
    ///   under `seedEpoch`, an epoch every reader holds the seed of;
    /// - the channel epoch trails it: the seed this device holds for the
    ///   channel's current epoch, under that epoch;
    /// - it trails and no seed is held for it: `nil`, nothing is sealed (the
    ///   person can retry once the reseal lands).
    ///
    /// Keys with no `seedEpoch` keep the old rule: the channel's current
    /// epoch.
    static func composer(
        keys: ChannelPublisherKeys, channelEpoch: UInt32?, held: (UInt32) -> Data?
    ) -> (epoch: UInt32, seed: Data)? {
        guard let served = keys.seedEpoch else { return (channelEpoch ?? 0, keys.seed) }
        guard let channelEpoch, channelEpoch < served else { return (served, keys.seed) }
        return held(channelEpoch).map { (channelEpoch, $0) }
    }

    /// The epoch the composer's seed opens: the one it serves, else (not
    /// known yet) the channel's current one.
    static func servedEpoch(keys: ChannelPublisherKeys, channelEpoch: UInt32?) -> UInt32 {
        keys.seedEpoch ?? channelEpoch ?? 0
    }

    /// Every seed this device reads a channel with: the per-epoch seeds it
    /// keeps, and the composer's seed for the one epoch it serves only.
    static func readSeeds(
        kept: [UInt32: Data], keys: ChannelPublisherKeys?, channelEpoch: UInt32?
    ) -> [UInt32: Data] {
        var map = kept
        if let keys { map[servedEpoch(keys: keys, channelEpoch: channelEpoch)] = keys.seed }
        return map
    }

    /// The seed being replaced and the epoch it served, to keep, or `nil`:
    /// no previous seed, the same seed again, or that epoch already held (the
    /// first one filed wins).
    static func outgoing(
        previous: ChannelPublisherKeys?, nextEpoch: UInt32, nextSeed: Data, held: (UInt32) -> Bool
    ) -> (epoch: UInt32, seed: Data)? {
        guard let previous, previous.seed != nextSeed else { return nil }
        let epoch: UInt32
        if let served = previous.seedEpoch {
            epoch = served
        } else {
            guard nextEpoch > 0 else { return nil }
            epoch = nextEpoch - 1
        }
        guard !held(epoch) else { return nil }
        return (epoch, previous.seed)
    }
}

@MainActor
extension ChannelsUiState {

    /// ``ChannelPublisherSeed/composer(keys:channelEpoch:held:)`` for a
    /// channel this device publishes, against what it holds.
    func composerSeed(channelId: String) -> (epoch: UInt32, seed: Data)? {
        guard let keys = keysByChannel[channelId] else { return nil }
        let kept = subscribedSeeds(channelId: channelId)
        return ChannelPublisherSeed.composer(
            keys: keys, channelEpoch: epochByChannel[channelId], held: { kept[$0] })
    }

    /// Every seed this device reads `channelId` with.
    func readSeeds(channelId: String) -> [UInt32: Data] {
        ChannelPublisherSeed.readSeeds(
            kept: subscribedSeeds(channelId: channelId), keys: keysByChannel[channelId],
            channelEpoch: epochByChannel[channelId])
    }

    /// The composer's seed moves to `seed`, which serves channel epoch
    /// `epoch` (the PublisherGroup epoch it was read at). The seed it
    /// replaces stays filed under the epoch it served, and the new one is
    /// filed under its own. The channel's current epoch is left alone: on a
    /// co-publisher the owner's reseal may not have moved it yet, and
    /// sealing keeps to the committed epoch until it does. Idempotent.
    func notePublisherSeedAt(channelId: String, epoch: UInt32, seed: Data) {
        guard let previous = keysByChannel[channelId] else { return }
        let kept = subscribedSeeds(channelId: channelId)
        if let outgoing = ChannelPublisherSeed.outgoing(
            previous: previous, nextEpoch: epoch, nextSeed: seed, held: { kept[$0] != nil }) {
            setSubscribedSeed(channelId: channelId, epoch: outgoing.epoch, seed: outgoing.seed)
        }
        if previous.seedEpoch != epoch || previous.seed != seed {
            setKeys(channelId: channelId, keys: ChannelPublisherKeys(
                seed: seed, signingKeySeed: previous.signingKeySeed, seedEpoch: epoch))
        }
        if subscribedSeeds(channelId: channelId)[epoch] == nil {
            setSubscribedSeed(channelId: channelId, epoch: epoch, seed: seed)
        }
        // 2026-11-20 — moving DOWN means the old mapping did not hold
        // (commits whose reseal never landed): what it filed above is wrong.
        if let served = previous.seedEpoch, served > epoch {
            dropSubscribedSeeds(channelId: channelId, above: epoch)
        }
    }
}

/// No seed of an epoch the channel service has reached is on this device:
/// nothing is sealed now (``ChannelPublisherSeed/composer(keys:channelEpoch:held:)``).
struct ChannelPublisherSeedUnavailable: Error {}
