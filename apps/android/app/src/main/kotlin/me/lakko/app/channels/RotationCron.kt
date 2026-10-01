// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.util.Base64
import kotlin.coroutines.cancellation.CancellationException
import kotlin.time.Duration
import kotlin.time.Duration.Companion.minutes
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.serialization.Serializable
import uniffi.lakko_ffi_uniffi.ChannelRotationPolicy
import uniffi.lakko_ffi_uniffi.ChannelRotationState
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.PublisherGroup
import uniffi.lakko_ffi_uniffi.channelRotationStateAfterRotation
import uniffi.lakko_ffi_uniffi.channelShouldRotate
import uniffi.lakko_ffi_uniffi.sealSubscriberSeedEnvelope

/**
 * G4-2-rotation-timer-android — one entry in the rotation
 * batch the publisher posts to
 * `POST /v1/channels/:id/rotate`. Snake-case field names
 * match the server's `RotateBatchEntry` wire shape; the
 * kotlinx.serialization Json codec emits them verbatim.
 */
@Serializable
public data class RotateBatchEntry(
    val subscriber_pubkey_hex: String,
    val envelope_b64: String,
)

/**
 * G4-2-rotation-timer-android — Android-side rotation
 * orchestrator.
 *
 * Kotlin mirror of `apps/web/src/channels/rotation-cron.ts`
 * and `apps/ios/Lakko/Channels/RotationCron.swift`. The
 * Rust core (`crates/lakko-channels/src/rotation_policy.rs`)
 * decides _when_ to rotate via the pure
 * `channelShouldRotate(policy, state, nowMs)` UniFFI free
 * function. The orchestrator here decides _how_: it walks
 * the subscriber list, reseals the new content seed for
 * each subscriber via `sealSubscriberSeedEnvelope`, then
 * fans the batch out via the ADR-0030 chunked
 * staged-rotation protocol — bounded `POST .../rotate/stage`
 * chunks (epoch NOT bumped, staged envelopes never served)
 * followed by a single atomic `POST .../rotate/commit`
 * (server promotes the staged set + bumps the epoch in one
 * transaction — see G4-4 / C-42).
 *
 * Two entry points:
 *   - [runOnce] — single-pass; returns the new rotation
 *     state. Use this from a "Rotate now" button in
 *     Compose.
 *   - [start] — periodic poller. Calls `runOnce` on every
 *     tick, persists state via the caller-supplied
 *     `saveState`, surfaces errors via `onError`. Returns
 *     a `Job` whose `cancel()` stops the timer.
 *
 * The orchestrator is **idempotent under no-op** — if the
 * policy says "no rotation needed", it returns the input
 * state unchanged + does no network round-trip.
 *
 * **Failure semantics:**
 *   - If `listSubscribers` / `stageRotation` /
 *     `commitRotation` throws, the state is NOT updated (so
 *     the next tick retries). A failed stage chunk aborts
 *     the whole pass before commit, so the epoch never
 *     bumps — staging is idempotent, so the retry re-sends
 *     the chunks and commits (fail-closed, invariant #4).
 *   - If a single envelope seal fails, the WHOLE batch is
 *     aborted — same atomic posture the server enforces.
 *     Partial fanouts would leave some subscribers on the
 *     old seed + some on the new, which is worse than
 *     retrying.
 *   - If `commitRotation` succeeds but `saveState` throws,
 *     the rotation is committed server-side; the caller's
 *     storage is stale and the next tick will trigger
 *     another rotation. That's the deliberate trade-off:
 *     we cannot un-commit the server, but we MUST persist
 *     the new state to avoid an infinite rotation loop.
 */
public class RotationCron(
    private val publisherGroup: PublisherGroup,
    private val identity: Identity,
    private val channelIdHex: String,
    private val channelsClient: ChannelsClient,
    private val saveState: suspend (ChannelRotationState) -> Unit,
    private val onRotated: ((UInt) -> Unit)? = null,
    /** ADR-0053 — registers the rotated epoch's journal key. Tests pass a
     *  no-op; production builds a DS client per registration. */
    private val registerRotatedEpoch: suspend (groupId: ByteArray, before: me.lakko.app.ds.DsJournalSigner?, after: me.lakko.app.ds.DsJournalSigner?) -> Unit =
        { groupId, before, after ->
            me.lakko.app.ds.DsClient().use { ds ->
                me.lakko.app.ds.registerBeforePublish(before, after) { next, current ->
                    ds.registerNextEpoch(groupId, next, current)
                }
            }
        },
) {

    /** Result of one single-pass rotation attempt. */
    public data class OnceResult(
        /** `true` if the policy fired AND the rotation
         *  completed end-to-end. */
        val rotated: Boolean,
        /** New rotation state to persist. For
         *  `rotated:false` cases this is byte-identical to
         *  the input state. */
        val newState: ChannelRotationState,
        /** New server-side epoch id after the rotation,
         *  when `rotated:true`. */
        val newEpoch: UInt? = null,
    )

    /** Errors `runOnce` can throw before any network
     *  round-trip. */
    public sealed class CronException(msg: String) : Exception(msg) {
        /** One subscriber's hex pubkey did not parse. */
        public class MalformedSubscriberHex(val hex: String) :
            CronException("subscriber pubkey hex is not valid: $hex")

        /** The server returned a wraparound epoch (== UInt.MAX_VALUE). */
        public class UnexpectedServerEpoch(val epoch: UInt) :
            CronException("server epoch wraparound at $epoch")
    }

    /**
     * Run a single rotation pass. Returns
     * `{ rotated = false }` if the policy says no;
     * otherwise drives the full chain.
     *
     * The caller MUST persist `result.newState` (the
     * [start] driver does this automatically).
     */
    public suspend fun runOnce(
        policy: ChannelRotationPolicy,
        state: ChannelRotationState,
        nowMs: ULong = currentMs(),
    ): OnceResult {
        if (!channelShouldRotate(policy, state, nowMs)) {
            return OnceResult(rotated = false, newState = state)
        }

        // 1. Advance the local MLS epoch + derive the new
        //    seed BEFORE talking to the server. If the
        //    server-side POST /rotate fails later, we have
        //    already rotated the local PublisherGroup —
        //    that's fine because the next tick retries
        //    from the new local state, and the server's
        //    atomic-rotation guarantee means no partial
        //    landings are observable.
        val signerBeforeRotate = CoPublisher.signerOf(publisherGroup, identity)
        publisherGroup.rotate(identity)
        // ADR-0053 — the rotation advanced the PublisherGroup epoch, so its
        // journal's key must be registered or every later journal request of
        // this owner (the co-publisher panel's poll and approve) is refused.
        // The rotation commit itself is never published, so this is best
        // effort: a refusal is logged and the next claim-on-open retries.
        try {
            registerRotatedEpoch(
                publisherGroup.groupId(),
                signerBeforeRotate,
                CoPublisher.signerOf(publisherGroup, identity),
            )
        } catch (e: CancellationException) {
            throw e
        } catch (e: Throwable) {
            android.util.Log.w("RotationCron", "ADR-0053: rotated epoch not registered", e)
        }
        val newSeed = publisherGroup.channelSeed(identity)
        val channelIdBytes = publisherGroup.channelId()

        // 2. The new server epoch the envelopes will bind
        //    to. fetchEpoch returns the CURRENT server
        //    epoch; the rotate endpoint will bump to
        //    current+1.
        val currentEpoch = channelsClient.fetchEpoch(channelIdHex)
        if (currentEpoch == UInt.MAX_VALUE) {
            throw CronException.UnexpectedServerEpoch(currentEpoch)
        }
        val newServerEpoch = currentEpoch + 1u

        // 3. Walk the subscriber list + seal new envelopes.
        //    Every publisher request below is signed (ADR-0059 section 9)
        //    with the role this account holds, learned once and cached.
        return ChannelPublisherSigners.derive(identity, channelIdHex).use { proofs ->
            fanOut(proofs, channelIdBytes, newServerEpoch, newSeed, nowMs)
        }
    }

    private suspend fun fanOut(
        proofs: ChannelPublisherProofs,
        channelIdBytes: ByteArray,
        newServerEpoch: UInt,
        newSeed: ByteArray,
        nowMs: ULong,
    ): OnceResult {
        val subscribers = channelsClient.listSubscribersAsPublisher(channelIdHex, proofs)
        val batch = ArrayList<RotateBatchEntry>(subscribers.size)
        for (subscriberHex in subscribers) {
            val subscriberBytes = hexToBytes(subscriberHex)
                ?: throw CronException.MalformedSubscriberHex(subscriberHex)
            val envelope = sealSubscriberSeedEnvelope(
                subscriberBytes,
                channelIdBytes,
                newServerEpoch,
                newSeed,
            )
            batch.add(
                RotateBatchEntry(
                    subscriber_pubkey_hex = subscriberHex,
                    envelope_b64 = Base64.encodeToString(envelope, Base64.NO_WRAP),
                ),
            )
        }

        // 4. Fan the batch out via the ADR-0030 chunked
        //    staged-rotation protocol: upload the sealed
        //    envelopes in bounded, resumable, idempotent
        //    chunks to the staging area (epoch NOT bumped,
        //    staged envelopes never served), then a single
        //    atomic commit promotes them + bumps the epoch.
        //    Staging is idempotent (upsert keyed on
        //    subscriber), so a chunk failure aborts this tick
        //    and the next tick re-sends only the missing
        //    chunks; the rotation state advances only after
        //    commit succeeds (fail-closed, invariant #4 —
        //    C-42 preserved as "the commit is atomic").
        //
        //    Empty batch: no stage chunks, commit still bumps
        //    the epoch — covers the "no subscribers yet" case
        //    where the publisher wants to keep rotating on
        //    schedule before the audience grows.
        var chunkStart = 0
        while (chunkStart < batch.size) {
            val chunkEnd =
                minOf(chunkStart + ChannelsClient.MAX_STAGE_CHUNK_ENTRIES, batch.size)
            channelsClient.stageRotation(
                channelIdHex,
                newServerEpoch,
                batch.subList(chunkStart, chunkEnd),
                proofs,
            )
            chunkStart = chunkEnd
        }
        val committedEpoch = channelsClient.commitRotation(channelIdHex, newServerEpoch, proofs)

        // 5. Stamp the new state + persist.
        val newState = channelRotationStateAfterRotation(nowMs)
        saveState(newState)
        onRotated?.invoke(committedEpoch)

        return OnceResult(rotated = true, newState = newState, newEpoch = committedEpoch)
    }

    /**
     * Start a periodic rotation poller on [scope].
     * Returns a [Job] whose `cancel()` stops the loop.
     * Safe to leave running for the lifetime of a channel
     * screen — one [runOnce] per [pollInterval] is a
     * cheap heartbeat (zero network calls when the policy
     * says no).
     *
     * [pollInterval] defaults to 5 minutes — small enough
     * that a 24-hour interval rotation lands within ~5 min
     * of the scheduled moment, large enough that an idle
     * app does not burn battery on policy checks.
     *
     * [loadState] is called on every tick so the cron
     * picks up state changes written by other paths
     * (e.g. the "Rotate now" button or a post-counter
     * bump from `composePost`).
     */
    public fun start(
        scope: CoroutineScope,
        policy: ChannelRotationPolicy,
        loadState: suspend () -> ChannelRotationState,
        pollInterval: Duration = 5.minutes,
        onError: ((Throwable) -> Unit)? = null,
    ): Job = scope.launch {
        // Fire immediately so a session that opens past
        // the rotation deadline catches up rather than
        // waiting one full pollInterval.
        tick(policy, loadState, onError)
        while (isActive) {
            try {
                delay(pollInterval)
            } catch (e: CancellationException) {
                throw e
            }
            if (!isActive) break
            tick(policy, loadState, onError)
        }
    }

    private suspend fun tick(
        policy: ChannelRotationPolicy,
        loadState: suspend () -> ChannelRotationState,
        onError: ((Throwable) -> Unit)?,
    ) {
        try {
            val state = loadState()
            runOnce(policy, state)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Throwable) {
            onError?.invoke(e)
        }
    }

    private companion object {
        fun currentMs(): ULong = System.currentTimeMillis().toULong()

        /** Parse a lowercase / mixed-case hex string into
         *  bytes. Returns `null` on any malformed input. */
        fun hexToBytes(hex: String): ByteArray? {
            if (hex.isEmpty() || hex.length % 2 != 0) return null
            val out = ByteArray(hex.length / 2)
            for (i in out.indices) {
                val hi = hexNibble(hex[2 * i].code)
                val lo = hexNibble(hex[2 * i + 1].code)
                if (hi < 0 || lo < 0) return null
                out[i] = ((hi shl 4) or lo).toByte()
            }
            return out
        }

        fun hexNibble(code: Int): Int = when (code) {
            in 0x30..0x39 -> code - 0x30
            in 0x61..0x66 -> code - 0x61 + 10
            in 0x41..0x46 -> code - 0x41 + 10
            else -> -1
        }
    }
}
