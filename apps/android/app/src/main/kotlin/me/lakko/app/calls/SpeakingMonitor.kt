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
 * Reads the sound level of each person in a group call and tells the shared
 * speaking rule ([SpeakingRule]) what it read
 * (`docs/ux/group-call-grid.md` section 7.3): the Kotlin counterpart of the
 * web `apps/web/src/calls/speaking-monitor.ts`.
 *
 * The levels are read on this device from audio it has already received,
 * decrypted and decoded ([CallAudioLevels]). Nothing here reaches the
 * network, and the call server is never asked who is speaking.
 *
 * What the rule answers goes to the call's [CallUiState]: the ring on every
 * speaking tile, and the floor the stage of the Focus layout follows. A
 * reading that cannot be taken yields nothing: no ring is drawn and the stage
 * falls back to the roster order, which is the call as it was before this
 * existed.
 *
 * Nothing of the rule is decided here. This class says who this device's own
 * person is, who is muted, what level was read for whom and when, and who is
 * in the call, in order; whose ring is on and who holds the floor (never the
 * own person) is the rule's answer, passed on as it came.
 *
 * Group calls only; a one-to-one has one voice.
 *
 * @param sample one reading, or `null` when there is no connection to read.
 * @param participantIdOf the roster id of the person an incoming stream
 *   belongs to. On an SFU call the stream id is the peer id and the roster
 *   id is `peer-<peerId>`.
 * @param rules where the call's rule comes from: the shared one in
 *   production, a fake in a host test.
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
    private val rules: SpeakingRules = SharedSpeakingRules,
    private val onReport: (voices: List<VoiceReport>, floorId: String?) -> Unit = { voices, floorId ->
        android.util.Log.i(VOICES_LOG_TAG, voicesLine(ui, SharedGroupCallRules, voices, floorId))
    },
) {
    private class Report(
        var path: LevelPath = LevelPath.NONE,
        var peak: Double? = null,
        var speaking: Boolean = false,
    )

    /** The call's rule: made at the first reading, closed by [stop]. */
    private var rule: SpeakingRule? = null

    /** Who the rule was last told this device's own person is. */
    private var toldLocal: String? = null

    /** The roster the rule was last told to keep, or `null` before the first. */
    private var toldRoster: List<String>? = null

    /** Counts [stop]s, so a reading that was in flight across one is dropped. */
    private var stops = 0

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

    /** Stop reading, take every ring down and free the rule. Idempotent. */
    fun stop() {
        job?.cancel()
        job = null
        stops += 1
        rule?.close()
        rule = null
        toldLocal = null
        toldRoster = null
        reports.clear()
        ticksSinceReport = 0
        ui.setSpeech(emptySet(), StageSpeech.None)
    }

    /** The call's rule, or `null` when it cannot be had (the native library is
     *  not loaded): then nothing is drawn. */
    private fun ruleOrNull(): SpeakingRule? {
        rule?.let { return it }
        val made = try {
            rules.create()
        } catch (cause: RuntimeException) {
            null
        } catch (cause: LinkageError) {
            null
        }
        rule = made
        return made
    }

    /**
     * One reading, handed to the rule, and the rule's answer applied.
     *
     * Skipped while no call screen is showing the tiles: a ring nobody can see
     * is not worth asking the connection for its statistics several times a
     * second.
     */
    internal suspend fun tick() {
        if (!ui.speakingWatched.value) return
        val stopsBefore = stops
        val levels = try {
            sample()
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            null
        } ?: return
        // Stopped while the reading was being taken: the rule is closed and
        // the rings are down, and this reading must not bring either back.
        if (stops != stopsBefore) return
        val rule = ruleOrNull() ?: return
        // One clock in whole milliseconds; a clock before its own origin
        // reads as the origin.
        val now = nowMs().coerceAtLeast(0L).toULong()
        val people = ui.participants.value
        val order = people.map { it.id }
        val present = order.toSet()
        val local = people.firstOrNull { it.isLocal }?.id
        if (local != toldLocal) {
            rule.setLocal(local)
            toldLocal = local
        }
        if (order != toldRoster) {
            rule.keepOnly(order)
            toldRoster = order
        }
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
                rule.silence(person.id)
                report.path = LevelPath.NONE
                continue
            }
            val read: Pair<Double, LevelPath>? =
                if (person.isLocal) levels.local?.let { it to levels.localPath } else remoteLevels[person.id]
            report.path = read?.second ?: LevelPath.NONE
            if (read != null) report.peak = maxOf(report.peak ?: 0.0, read.first)
            // No level for someone in the call is "could not be read", and is
            // said so: what that means is the rule's to decide.
            rule.sample(person.id, read?.first, now)
        }
        // The whole roster, in order, the own person included: who may hold
        // the floor is the rule's to decide.
        val snapshot = rule.decideFloor(order, now)
        for (id in snapshot.speakingIds) reports[id]?.speaking = true
        ui.setSpeech(
            snapshot.speakingIds.toSet(),
            StageSpeech(
                activeSpeakerId = snapshot.activeSpeakerId,
                lastSpokeAtMs = snapshot.lastSpokeAtMs.mapValues { (_, at) ->
                    at.coerceAtMost(Long.MAX_VALUE.toULong()).toLong()
                },
            ),
        )
        ticksSinceReport += 1
        if (ticksSinceReport >= SPEAKING_REPORT_EVERY_TICKS) {
            ticksSinceReport = 0
            report(rule, people, snapshot.activeSpeakerId)
        }
    }

    /** Hand the diagnostics of the readings since the last report over. */
    private fun report(rule: SpeakingRule, people: List<CallUiState.Participant>, floorId: String?) {
        val voices = people.mapNotNull { person ->
            val r = reports[person.id] ?: return@mapNotNull null
            VoiceReport(
                id = person.id,
                isLocal = person.isLocal,
                path = if (person.micOn) r.path else LevelPath.NONE,
                peak = r.peak,
                speaking = r.speaking,
                threshold = rule.threshold(person.id),
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
 * The shared rule does not count readings: every time in it is measured
 * between the readings' own timestamps, so its answers do not depend on this
 * interval beyond when a reading happens to fall. Each reading here is the
 * connection's whole statistics report, which is why it is not taken more
 * often.
 */
internal const val SPEAKING_SAMPLE_INTERVAL_MS: Long = 150L

/** How many readings one diagnostics report covers (about 2 seconds, the
 *  web client's interval). */
internal const val SPEAKING_REPORT_EVERY_TICKS: Int = 13

/** The log tag of the `voices` line. */
private const val VOICES_LOG_TAG: String = "SfuCallSession"
