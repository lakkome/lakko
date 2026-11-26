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
    /** 2026-11-20 — the per-channel holds after a failed list read: a
     *  refusal (`401 owner-proof-required`) waits for [clear] (sign-out, a
     *  new session), anything else 30 s, doubling to 10 minutes. */
    private val retry: PublisherCommitRetry = PublisherCommitRetry(warn = {}),
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
        if (!retry.mayAttempt(HOLD_ACCOUNT, channelIdHex)) return
        val lock = lockFor(channelIdHex)
        if (!lock.tryLock()) return
        try {
            val list = try {
                port.listSubscribers(channelIdHex)
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (e: Throwable) {
                val hold = retry.noteFailure(HOLD_ACCOUNT, channelIdHex, e)
                warn(
                    "list subscribers failed (${PublisherCommitFailure.logLabel(e)}); " +
                        if (hold.refused) "not tried again until the next start" else "tried again after a wait",
                    e,
                )
                return
            }
            retry.noteSuccess(HOLD_ACCOUNT, channelIdHex)
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
    /** Whether the list read of [channelIdHex] was refused (2026-11-20). */
    public fun isRefused(channelIdHex: String): Boolean = retry.hold(HOLD_ACCOUNT, channelIdHex)?.refused == true

    /** Drop the hold of [channelIdHex] (its owner key was repaired). */
    public fun clearHold(channelIdHex: String) {
        retry.noteSuccess(HOLD_ACCOUNT, channelIdHex)
    }

    public fun clear() {
        retry.reset()
        _state.value = emptyMap()
    }

    public companion object {
        private const val TAG = "SubscriberAdmission"

        /** The holds of one instance serve one bound account. */
        private const val HOLD_ACCOUNT: String = ""

        /** Between passes of the app-wide loop over the owned channels (chosen). */
        public const val APP_INTERVAL_MS: Long = 60_000L

        /** The application context the ticket check reads the admission
         *  seeds with; set by the session shell. */
        @Volatile
        public var appContext: android.content.Context? = null

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
    private val appContext: android.content.Context? get() = SubscriberAdmission.appContext
    override fun canPost(channelIdHex: String): Boolean =
        state.boundAccount != null && state.keysByChannel.value.containsKey(channelIdHex)

    override suspend fun listSubscribers(channelIdHex: String): List<String> {
        // ADR-0087 section 7 — a private channel this device holds an
        // admission seed for lets in only the rows whose ticket names a live
        // invite key; a co-publisher with none leaves it to the owner.
        state.boundAccount?.let { account ->
            val context = appContext
            // ADR-0101 section 7 — a channel taken back with its recovery key
            // whose type is still unknown lets every row in.
            if (context != null && !ChannelReturnRuntime.recoveredTypeUnknown(context, state, channelIdHex)) {
                if (ChannelAdmissionKeys.all(context, account, channelIdHex).also { l -> l.forEach { it.second.fill(0) } }.isNotEmpty()) {
                    return ChannelSubscriberRows.admissible(context, state, client, channelIdHex).orEmpty()
                }
                if (state.joinedGroupIdOf(channelIdHex) != null) return emptyList()
            }
        }
        val pub = state.publisher ?: throw IllegalStateException("channel publisher identity not available")
        return pub.withIdentity(persist = false) { identity ->
            client.listSubscribersAsPublisher(channelIdHex, identity)
        }
    }

    // The epoch this screen state already knows, fetched when nothing has
    // learned it yet, so a delivery is never filed at a guessed 0 (ADR-0031 —
    // never a future one either: the cache only lags the server).
    // Section 15 — the epoch a delivery is sealed and filed under: the one
    // the composer's seed can serve and the service has reached, never the
    // composer's epoch before the owner's reseal moved the channel there.
    override suspend fun currentEpoch(channelIdHex: String): UInt {
        val current = state.epochByChannel.value[channelIdHex]
            ?: client.fetchEpoch(channelIdHex).also { state.setEpoch(channelIdHex, it) }
        return state.sealingSeed(channelIdHex, current)?.epoch
            ?: throw IllegalStateException("no seed of an epoch the channel has reached")
    }

    override suspend fun sealAndDeliver(channelIdHex: String, pubkeyHex: String, epoch: UInt) {
        if (!state.keysByChannel.value.containsKey(channelIdHex)) {
            throw IllegalStateException("this device cannot post to this channel")
        }
        // The seed of exactly [epoch] (the composer's only for the epoch it
        // serves), never a seed of another epoch under its number.
        val seed = state.readSeed(channelIdHex, epoch)
            ?: throw IllegalStateException("no seed of this epoch on this device")
        val parsed = hexToBytes(pubkeyHex)
        val wire = ChannelsComposer.sealSubscriberSeedEnvelope(
            subscriberPub = parsed,
            channelIdHex = channelIdHex,
            epochId = epoch,
            seed = seed,
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
