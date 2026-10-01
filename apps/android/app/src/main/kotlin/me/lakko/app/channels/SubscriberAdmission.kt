// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.util.Log
import java.util.WeakHashMap
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.sync.Mutex

/**
 * Letting new subscribers in (ADR-0010, ADR-0059 section 4): for a channel
 * this device can post to, read the subscriber list and seal the current
 * epoch's seed to every key that has not had it yet. Mirror of
 * `apps/web/src/channels/subscriber-admission.ts`.
 *
 * One object for both callers, so they share what has been delivered: the
 * app-wide loop in the session shell, which lets people in while the app is
 * in front whatever screen is showing, and the channel / Manage screens,
 * which show the same state and poll faster while open. Before this the
 * let-in loop lived in those screens alone, so a person who subscribed from
 * Discover waited until the owner happened to open the channel.
 *
 * What was delivered is kept in memory per (channel, key, epoch). A relaunch
 * delivers everyone once more, which the server takes as an upsert; a new
 * epoch delivers again, which the rotation fan-out would also do. Every
 * envelope is sealed on this device under the publisher's seed; the server
 * only stores it (invariant #2).
 */
public class SubscriberAdmission(
    private val port: Port,
    private val warn: (String, Throwable) -> Unit = { _, _ -> },
) {
    /** What the admission needs from the channel state and the server. */
    public interface Port {
        /** Whether this device holds publisher keys for the channel. */
        public fun canPost(channelIdHex: String): Boolean

        /** The subscriber list (owner-signed, co-publisher fallback). */
        public suspend fun listSubscribers(channelIdHex: String): List<String>

        /** The epoch deliveries are filed under now. */
        public suspend fun currentEpoch(channelIdHex: String): UInt

        /** Seal the current seed to [pubkeyHex] and deliver it at [epoch]. */
        public suspend fun sealAndDeliver(channelIdHex: String, pubkeyHex: String, epoch: UInt)
    }

    /** How one subscriber's envelope stands on this device. */
    public enum class Status { Ready, Failed }

    /** A key's status at the epoch it was sealed for. */
    public data class KeyStatus(val epoch: UInt, val status: Status)

    /** One channel's admission state. */
    public data class ChannelAdmission(
        /** The list as last read, `null` before the first read. */
        val subscribers: List<String>? = null,
        val statusByKey: Map<String, KeyStatus> = emptyMap(),
    )

    private val _state = MutableStateFlow<Map<String, ChannelAdmission>>(emptyMap())
    /** Channel id -> admission state. */
    public val state: StateFlow<Map<String, ChannelAdmission>> = _state.asStateFlow()

    private val passLocks = HashMap<String, Mutex>()

    private fun lockFor(channelIdHex: String): Mutex =
        synchronized(passLocks) { passLocks.getOrPut(channelIdHex) { Mutex() } }

    private fun update(channelIdHex: String, f: (ChannelAdmission) -> ChannelAdmission) {
        _state.update { m -> m + (channelIdHex to f(m[channelIdHex] ?: ChannelAdmission())) }
    }

    private fun record(channelIdHex: String, pubkeyHex: String, status: KeyStatus) {
        update(channelIdHex) { a -> a.copy(statusByKey = a.statusByKey + (pubkeyHex to status)) }
    }

    /**
     * Seal and deliver the current seed to one key (the manual Retry, and
     * each key of a pass). Records the outcome; returns whether it landed.
     */
    public suspend fun admit(channelIdHex: String, pubkeyHex: String): Boolean {
        if (!port.canPost(channelIdHex)) return false
        val epoch = try {
            port.currentEpoch(channelIdHex)
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (e: Throwable) {
            warn("channel epoch unavailable", e)
            return false
        }
        return admitAt(channelIdHex, pubkeyHex, epoch)
    }

    private suspend fun admitAt(channelIdHex: String, pubkeyHex: String, epoch: UInt): Boolean =
        try {
            port.sealAndDeliver(channelIdHex, pubkeyHex, epoch)
            record(channelIdHex, pubkeyHex, KeyStatus(epoch, Status.Ready))
            true
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (e: Throwable) {
            // Recorded as failed; the next pass tries again.
            warn("letting a subscriber in failed", e)
            record(channelIdHex, pubkeyHex, KeyStatus(epoch, Status.Failed))
            false
        }

    /**
     * Read the list and let in every key not yet delivered at the current
     * epoch (or whose last delivery failed). One pass per channel at a time;
     * a second caller while one runs returns at once. Deliveries are
     * sequential, so a burst of new subscribers does not open many core
     * calls at once.
     */
    public suspend fun admitNewSubscribers(channelIdHex: String) {
        if (!port.canPost(channelIdHex)) return
        val lock = lockFor(channelIdHex)
        if (!lock.tryLock()) return
        try {
            val list = try {
                port.listSubscribers(channelIdHex)
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (e: Throwable) {
                warn("list subscribers failed", e)
                return
            }
            update(channelIdHex) { it.copy(subscribers = list) }
            val epoch = try {
                port.currentEpoch(channelIdHex)
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (e: Throwable) {
                warn("channel epoch unavailable", e)
                return
            }
            for (pk in list) {
                val st = _state.value[channelIdHex]?.statusByKey?.get(pk)
                if (st != null && st.status == Status.Ready && st.epoch == epoch) continue
                admitAt(channelIdHex, pk, epoch)
            }
        } finally {
            lock.unlock()
        }
    }

    /** One pass over [channelIds] in turn (the app-wide loop). */
    public suspend fun admitAll(channelIds: Collection<String>) {
        for (id in channelIds) admitNewSubscribers(id)
    }

    /** Forget everything delivered (sign-out; another account may follow). */
    public fun clear() {
        _state.value = emptyMap()
    }

    public companion object {
        private const val TAG = "SubscriberAdmission"

        /** Between passes of the app-wide loop over the owned channels (chosen). */
        public const val APP_INTERVAL_MS: Long = 15_000L

        private val shared = WeakHashMap<ChannelsUiState, SubscriberAdmission>()

        /**
         * The one admission for the session's [state]: the session shell's
         * loop and the channel screens get the same instance, hence the same
         * record of who has been let in.
         */
        public fun forChannels(state: ChannelsUiState, client: ChannelsClient): SubscriberAdmission =
            synchronized(shared) {
                shared.getOrPut(state) {
                    SubscriberAdmission(ChannelsPort(state, client)) { msg, e -> Log.w(TAG, msg, e) }
                }
            }
    }
}

/** The production [SubscriberAdmission.Port] over the channel state and REST client. */
private class ChannelsPort(
    private val state: ChannelsUiState,
    private val client: ChannelsClient,
) : SubscriberAdmission.Port {
    override fun canPost(channelIdHex: String): Boolean =
        state.boundAccount != null && state.keysByChannel.value.containsKey(channelIdHex)

    override suspend fun listSubscribers(channelIdHex: String): List<String> {
        val pub = state.publisher ?: throw IllegalStateException("channel publisher identity not available")
        return pub.withIdentity(persist = false) { identity ->
            client.listSubscribersAsPublisher(channelIdHex, identity)
        }
    }

    // The epoch this screen state already knows, fetched when nothing has
    // learned it yet, so a delivery is never filed at a guessed 0 (ADR-0031 —
    // never a future one either: the cache only lags the server).
    override suspend fun currentEpoch(channelIdHex: String): UInt =
        state.epochByChannel.value[channelIdHex]
            ?: client.fetchEpoch(channelIdHex).also { state.setEpoch(channelIdHex, it) }

    override suspend fun sealAndDeliver(channelIdHex: String, pubkeyHex: String, epoch: UInt) {
        val keys = state.keysByChannel.value[channelIdHex]
            ?: throw IllegalStateException("this device cannot post to this channel")
        val parsed = hexToBytes(pubkeyHex)
        val wire = ChannelsComposer.sealSubscriberSeedEnvelope(
            subscriberPub = parsed,
            channelIdHex = channelIdHex,
            epochId = epoch,
            seed = keys.seed,
        )
        val pub = state.publisher ?: throw IllegalStateException("channel publisher identity not available")
        // ADR-0059 section 9: signed with this account's publisher role.
        ChannelPublisherSigners.derive(pub, channelIdHex).use {
            client.deliverEnvelope(channelIdHex, pubkeyHex, wire, it, epoch = epoch.toInt())
        }
    }

    private fun hexToBytes(hex: String): ByteArray {
        require(hex.length % 2 == 0) { "subscriber key is not hex" }
        return ByteArray(hex.length / 2) { i ->
            val byte = hex.substring(i * 2, i * 2 + 2).toIntOrNull(16)
                ?: throw IllegalArgumentException("subscriber key is not hex")
            byte.toByte()
        }
    }
}
