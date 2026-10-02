// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/**
 * Reads the sound level of each person in a group call and feeds
 * [SpeakingTracker] (`docs/ux/group-call-grid.md` section 7.3) — the Kotlin
 * counterpart of the web `apps/web/src/calls/speaking-monitor.ts`.
 *
 * The levels are read on this device from audio it has already received,
 * decrypted and decoded ([CallAudioLevels]). Nothing here reaches the
 * network, and the call server is never asked who is speaking.
 *
 * What comes out goes to the call's [CallUiState]: the ring on every speaking
 * tile, and the floor the stage of the Focus layout follows. A reading that
 * cannot be taken yields nothing: no ring is drawn and the stage falls back
 * to the roster order, which is the call as it was before this existed. A
 * level is never guessed.
 *
 * Group calls only; a one-to-one has one voice.
 *
 * @param sample one reading, or `null` when there is no connection to read.
 * @param participantIdOf the roster id of the person an incoming stream
 *   belongs to. On an SFU call the stream id is the peer id and the roster
 *   id is `peer-<peerId>`.
 */
public class SpeakingMonitor(
    private val scope: CoroutineScope,
    private val ui: CallUiState,
    private val sample: suspend () -> CallAudioLevels?,
    private val participantIdOf: (streamId: String) -> String = { SfuCallUi.participantId(it) },
    private val nowMs: () -> Long = { System.currentTimeMillis() },
    private val intervalMs: Long = SPEAKING_SAMPLE_INTERVAL_MS,
) {
    private val tracker = SpeakingTracker()
    private var job: Job? = null

    /** Start reading. Idempotent. */
    public fun start() {
        if (job != null) return
        job = scope.launch {
            while (isActive) {
                delay(intervalMs)
                tick()
            }
        }
    }

    /** Stop reading and take every ring down. Idempotent. */
    public fun stop() {
        job?.cancel()
        job = null
        ui.setSpeech(emptySet(), StageSpeech.None)
    }

    /**
     * One reading, applied.
     *
     * Skipped while no call screen is showing the tiles: a ring nobody can see
     * is not worth asking the connection for its statistics several times a
     * second.
     */
    internal suspend fun tick() {
        if (!ui.speakingWatched.value) return
        val levels = try {
            sample()
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            null
        } ?: return
        val now = nowMs()
        val people = ui.participants.value
        val order = people.map { it.id }
        val present = order.toSet()
        tracker.keepOnly(present)
        val local = people.firstOrNull { it.isLocal }
        if (local != null && levels.local != null) {
            // A muted microphone sends nothing, so its tile shows no ring,
            // whatever the capture device hears.
            tracker.sample(local.id, if (local.micOn) levels.local else 0.0, now)
        }
        for ((streamId, level) in levels.remoteByStreamId) {
            val id = participantIdOf(streamId)
            if (id in present) tracker.sample(id, level, now)
        }
        val snapshot = tracker.snapshot(order, now)
        ui.setSpeech(
            snapshot.speakingIds,
            StageSpeech(
                activeSpeakerId = snapshot.activeSpeakerId,
                lastSpokeAtMs = snapshot.lastSpokeAtMs,
            ),
        )
    }
}

/** How often the levels are read: about seven times a second. */
public const val SPEAKING_SAMPLE_INTERVAL_MS: Long = 150L
