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
 * [SpeakingTracker] (`docs/ux/group-call-grid.md` section 7.3): the Kotlin
 * counterpart of the web `apps/web/src/calls/speaking-monitor.ts`.
 *
 * The levels are read on this device from audio it has already received,
 * decrypted and decoded ([CallAudioLevels]). Nothing here reaches the
 * network, and the call server is never asked who is speaking.
 *
 * What comes out goes to the call's [CallUiState]: the ring on every speaking
 * tile, and the floor the stage of the Focus layout follows. A reading that
 * cannot be taken yields nothing: no ring is drawn and the stage falls back
 * to the roster order, which is the call as it was before this existed.
 *
 * This device's own microphone rings its own tile and never takes the floor:
 * the floor exists for the large tile, which never shows the own tile while
 * anyone else is present. While the microphone is off its level is not read
 * at all.
 *
 * Group calls only; a one-to-one has one voice.
 *
 * @param sample one reading, or `null` when there is no connection to read.
 * @param participantIdOf the roster id of the person an incoming stream
 *   belongs to. On an SFU call the stream id is the peer id and the roster
 *   id is `peer-<peerId>`.
 * @param onReport told every [SPEAKING_REPORT_EVERY_TICKS] readings what each
 *   person's level was over them and who holds the floor. Diagnostics only.
 */
internal class SpeakingMonitor(
    private val scope: CoroutineScope,
    private val ui: CallUiState,
    private val sample: suspend () -> CallAudioLevels?,
    private val participantIdOf: (streamId: String) -> String = { SfuCallUi.participantId(it) },
    private val nowMs: () -> Long = { System.currentTimeMillis() },
    private val intervalMs: Long = SPEAKING_SAMPLE_INTERVAL_MS,
    private val onReport: (voices: List<VoiceReport>, floorId: String?) -> Unit = { voices, floorId ->
        android.util.Log.i(VOICES_LOG_TAG, voicesLine(ui, SharedGroupCallRules, voices, floorId))
    },
) {
    private class Report(
        var path: LevelPath = LevelPath.NONE,
        var peak: Double? = null,
        var speaking: Boolean = false,
    )

    private val tracker = SpeakingTracker()
    private val reports = HashMap<String, Report>()
    private var ticksSinceReport = 0
    private var job: Job? = null

    /** Start reading. Idempotent. */
    fun start() {
        if (job != null) return
        job = scope.launch {
            while (isActive) {
                delay(intervalMs)
                tick()
            }
        }
    }

    /** Stop reading and take every ring down. Idempotent. */
    fun stop() {
        job?.cancel()
        job = null
        reports.clear()
        ticksSinceReport = 0
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
        reports.keys.retainAll(present)
        val remoteLevels = HashMap<String, Pair<Double, LevelPath>>()
        for ((streamId, level) in levels.remoteByStreamId) {
            val id = participantIdOf(streamId)
            if (id in present) {
                remoteLevels[id] = level to (levels.remotePathByStreamId[streamId] ?: LevelPath.STATS_LEVEL)
            }
        }
        for (person in people) {
            val report = reports.getOrPut(person.id) { Report() }
            if (!person.micOn) {
                // A muted person is never speaking, whatever a level reading
                // says; this device's own level is not looked at while its
                // microphone is off.
                tracker.silence(person.id)
                report.path = LevelPath.NONE
                continue
            }
            val read: Pair<Double, LevelPath>? =
                if (person.isLocal) levels.local?.let { it to levels.localPath } else remoteLevels[person.id]
            report.path = read?.second ?: LevelPath.NONE
            if (read != null) report.peak = maxOf(report.peak ?: 0.0, read.first)
            // No level for someone in the call is silence: a ring must not
            // stay on for a stream that stopped reporting.
            tracker.sample(person.id, read?.first ?: 0.0, now)
        }
        // The floor is for everyone but this device's own person.
        val candidates = people.filter { !it.isLocal }.map { it.id }
        val snapshot = tracker.snapshot(candidates, now)
        for (id in snapshot.speakingIds) reports[id]?.speaking = true
        ui.setSpeech(
            snapshot.speakingIds,
            StageSpeech(
                activeSpeakerId = snapshot.activeSpeakerId,
                lastSpokeAtMs = snapshot.lastSpokeAtMs,
            ),
        )
        ticksSinceReport += 1
        if (ticksSinceReport >= SPEAKING_REPORT_EVERY_TICKS) {
            ticksSinceReport = 0
            report(people, snapshot.activeSpeakerId)
        }
    }

    /** Hand the diagnostics of the readings since the last report over. */
    private fun report(people: List<CallUiState.Participant>, floorId: String?) {
        val voices = people.mapNotNull { person ->
            val r = reports[person.id] ?: return@mapNotNull null
            VoiceReport(
                id = person.id,
                isLocal = person.isLocal,
                path = if (person.micOn) r.path else LevelPath.NONE,
                peak = r.peak,
                speaking = r.speaking,
            ).also {
                r.peak = null
                r.speaking = false
            }
        }
        onReport(voices, floorId)
    }
}

/**
 * How often the levels are read: about seven times a second. The web client
 * reads every 100 ms (`SAMPLE_INTERVAL_MS` in `speaking-monitor.ts`).
 *
 * The difference does not move the stage. Every threshold of
 * [SpeakingTracker] is a time in milliseconds measured between the readings'
 * own timestamps, none is a number of readings, and the floor's onset
 * ([FLOOR_ONSET_MS]) is a whole number of both intervals, so on either grid a
 * person takes the floor on the reading 1.5 s after the first one that heard
 * them (`SpeakingTest`). What the coarser grid does cost is the ring: it
 * comes on at the first reading at or past [RING_ON_MS], which is 300 ms
 * after the first one with sound here and 200 ms on the web, and goes off at
 * 900 ms of silence against 800 ms. Each reading here is the connection's
 * whole statistics report, which is why it is not taken more often.
 */
internal const val SPEAKING_SAMPLE_INTERVAL_MS: Long = 150L

/** How many readings one diagnostics report covers (about 2 seconds, the
 *  web client's interval). */
internal const val SPEAKING_REPORT_EVERY_TICKS: Int = 13

/** The log tag of the `voices` line. */
private const val VOICES_LOG_TAG: String = "SfuCallSession"
