// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

/*
 * ADR-0087 section 15 — which seed a publisher seals with and reads with.
 * Pure; the twin of web `apps/web/src/channels/publisher-seed.ts`
 * (`composerSeedFor`) and `channel-access.ts` (`channelSeedFor`,
 * `notePublisherSeedAt`).
 *
 * Only the channel owner commits to the PublisherGroup (an approve, a
 * removal, a rotation), and each owner commit has exactly one reseal moving
 * the channel service by one epoch. A co-publisher applies the owner's
 * commit from the group's journal, possibly before the owner's reseal has
 * moved the channel service there; the readers get the new seed only with
 * that reseal. So the composer's seed serves [ChannelPublisherKeys.seedEpoch],
 * and nothing is sealed under an epoch the service has not reached.
 */

/** A seed and the channel epoch it is sealed under. */
public class SealingSeed(public val epoch: UInt, public val seed: ByteArray)

/**
 * The seed to seal a post (or a subscriber envelope, or the public seed)
 * with, and its epoch: the held seed whose epoch the channel service has
 * reached, the newest at or below [channelEpoch].
 *
 * - The service has reached the composer's epoch (or passed it, when this
 *   device has not applied later commits yet): the composer's seed under its
 *   epoch.
 * - The service trails it: the seed this device holds for the channel's
 *   current epoch ([held]), under that epoch.
 * - It trails and no such seed is on the device: `null`; the caller refuses
 *   the post, which the person can retry once the reseal lands.
 *
 * Keys with no [ChannelPublisherKeys.seedEpoch] seal under [channelEpoch].
 */
internal fun composerSeedFor(
    keys: ChannelPublisherKeys,
    channelEpoch: UInt,
    held: (UInt) -> ByteArray?,
): SealingSeed? {
    val served = keys.seedEpoch ?: return SealingSeed(channelEpoch, keys.seed)
    if (channelEpoch >= served) return SealingSeed(served, keys.seed)
    return held(channelEpoch)?.let { SealingSeed(channelEpoch, it) }
}

/**
 * The seed this publisher reads [epoch] with: the composer's seed only for
 * the epoch it serves ([ChannelPublisherKeys.seedEpoch], else the channel's
 * current epoch); any other epoch from the seeds kept per epoch ([held]).
 */
internal fun publisherReadSeed(
    keys: ChannelPublisherKeys?,
    channelEpoch: UInt?,
    epoch: UInt,
    held: (UInt) -> ByteArray?,
): ByteArray? {
    if (keys != null) {
        val served = keys.seedEpoch ?: channelEpoch
        if (served == null || served == epoch) return keys.seed
    }
    return held(epoch)
}

/** What [movePublisherSeed] decides when the composer's seed moves. */
internal class PublisherSeedMove(
    /** The outgoing seed to keep under the epoch it served, if any. */
    val kept: Pair<UInt, ByteArray>?,
    /** The composer's keys from now on, or `null` when nothing changes. */
    val keys: ChannelPublisherKeys?,
    /** The new seed to file under its own epoch, or `null` when one is held. */
    val filed: Pair<UInt, ByteArray>?,
)

/**
 * The composer's seed moves to [seed], which serves channel epoch [epoch].
 * The seed it replaces stays filed under the epoch it served (or `epoch - 1`
 * when unknown); the new one is filed under its own epoch (the first one
 * filed for an epoch wins). The channel's current epoch is not touched here.
 */
internal fun movePublisherSeed(
    prev: ChannelPublisherKeys?,
    epoch: UInt,
    seed: ByteArray,
    held: Map<UInt, ByteArray>,
): PublisherSeedMove {
    val previousEpoch = prev?.seedEpoch ?: if (epoch > 0u) epoch - 1u else null
    val kept = outgoingPublisherSeed(prev?.seed, previousEpoch, seed, held)
    val keys = if (prev != null && (prev.seedEpoch != epoch || !prev.seed.contentEquals(seed))) {
        ChannelPublisherKeys(seed = seed.copyOf(), signingKeySeed = prev.signingKeySeed, seedEpoch = epoch)
    } else {
        null
    }
    val heldAfter = if (kept != null) held + kept else held
    val filed = if (heldAfter.containsKey(epoch)) null else epoch to seed.copyOf()
    return PublisherSeedMove(kept, keys, filed)
}

/** Only the channel owner's device rotates the channel key (maintainer,
 *  2026-11-08): a co-publisher's never does, scheduled or on demand. */
internal fun rotatesChannelKeyHere(isOwner: Boolean): Boolean = isOwner
