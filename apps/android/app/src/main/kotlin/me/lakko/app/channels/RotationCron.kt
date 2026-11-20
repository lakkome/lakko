// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

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

/**
 * G4-2-rotation-timer-android — one entry in the rotation
 * batch the publisher posts to
 * `POST /v1/channels/:id/rotate`. Snake-case field names
 * match the server's `RotateBatchEntry` wire shape; the
 * kotlinx.serialization Json codec emits them verbatim.
 */
@Serializable
public data class RotateBatchEntry(
    val subscriber_id: String,
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
    /** ADR-0090 — the account identity and this device's leaf. */
    private val ids: ChannelIdentity,
    private val channelIdHex: String,
    private val channelsClient: ChannelsClient,
    private val saveState: suspend (ChannelRotationState) -> Unit,
    /** G-50 — the account the rotation is recorded for. */
    private val accountIdB64: String,
    /** G-50 — where the rotation is recorded before any of its steps runs. */
    private val store: PublisherCommitStore,
    private val onRotated: ((UInt) -> Unit)? = null,
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
        /** ADR-0090 — another owner device committed at this epoch first;
         *  this device abandoned its rotation. */
        val lost: Boolean = false,
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

        // 1. The server epoch the envelopes will bind to, read
        //    before the stage (a network read).
        val newServerEpoch = CoPublisher.nextServerEpoch(channelsClient, channelIdHex).toUInt()

        // 2. ADR-0090 section 3 — STAGE the rotation (the group stays at
        //    epoch N holding it) and record it with the staged snapshot and
        //    the epoch-N access seed, before anything leaves the device.
        val entry = CoPublisher.recordCommit(ids, publisherGroup, accountIdB64, store) { device ->
            val proofEpoch = publisherGroup.epoch().toLong()
            val proofSeed = publisherGroup.dsAccessSeed(device)
            val commit = publisherGroup.stageRotate(device)
            PublisherCommitEntry(
                id = PublisherCommitEntry.newId(),
                kind = PublisherCommitEntry.Kind.ROTATION,
                channelIdHex = channelIdHex.lowercase(),
                groupIdHex = me.lakko.app.ds.DsClient.pseudonymHex(publisherGroup.groupId()),
                targetEpoch = proofEpoch + 1,
                serverTargetEpoch = newServerEpoch.toLong(),
                commitB64 = java.util.Base64.getEncoder().encodeToString(commit),
                proofEpoch = proofEpoch,
                proofSeedB64 = java.util.Base64.getEncoder().encodeToString(proofSeed),
            ).also { proofSeed.fill(0) }
        }

        // 3. The DS decides (ADR-0090 section 4: any owner device may
        //    rotate, one commit per epoch lands): taken, the stage merges
        //    and the new seed is resealed to every subscriber (ADR-0030
        //    chunked fanout, C-42); `409`, the stage is abandoned and this
        //    device does nothing more (the journal read applies the winner).
        val outcome = try {
            me.lakko.app.ds.DsClient().use { ds ->
                runPublisherCommit(
                    entry,
                    CoPublisher.commitPorts(
                        ds, channelsClient, ids, publisherGroup, channelIdHex, accountIdB64, store,
                    ),
                )
            }.also { PublisherCommitRetry.shared.noteSuccess(accountIdB64, channelIdHex) }
        } catch (e: kotlin.coroutines.cancellation.CancellationException) {
            throw e
        } catch (e: Throwable) {
            // 2026-11-20 — the entry stays. A failure after the journal step
            // holds the channel (a refusal: until the next start); an
            // unanswered journal step is sent again next pass.
            val after = store.entries(accountIdB64).firstOrNull { it.id == entry.id } ?: entry
            if (!PublisherCommitRetry.owesJournal(after)) {
                PublisherCommitRetry.shared.noteFailure(accountIdB64, channelIdHex, e)
            }
            throw e
        }
        if (outcome == PublisherCommitOutcome.LOST) {
            return OnceResult(rotated = false, newState = state, lost = true)
        }

        // 4. Stamp the new state + persist.
        val newState = channelRotationStateAfterRotation(nowMs)
        saveState(newState)
        onRotated?.invoke(newServerEpoch)

        return OnceResult(rotated = true, newState = newState, newEpoch = newServerEpoch)
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
    }
}
