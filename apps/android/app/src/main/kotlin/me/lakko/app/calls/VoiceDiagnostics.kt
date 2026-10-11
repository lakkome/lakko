// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.util.Locale

/**
 * The `voices` line of the call log: for each person where their sound level
 * was read from, how loud they were and whether that counted as speaking;
 * who holds the floor; who is on the large tile and why. The twin of the web
 * `voice-diagnostics.ts`, with the same field layout, so a log from a phone
 * and one from a browser read alike.
 *
 * A person who speaks and never reaches the large tile is either not
 * measured (`none`), measured below the threshold (a small peak, `no`), or
 * measured and outranked (a pin, or a floor someone else holds).
 *
 * Diagnostics only, on this device only; it decides nothing. Pure.
 */

/** One person over one interval. */
internal data class VoiceReport(
    val id: String,
    val isLocal: Boolean,
    /** Where the last level of the interval came from. */
    val path: LevelPath,
    /** The loudest level of the interval, `0..1`; `null` when none was read. */
    val peak: Double?,
    /** Whether the person counted as speaking at any reading of the interval. */
    val speaking: Boolean,
)

/** Why a person is on the large tile; the order of the shared rule. */
internal enum class StageReason(val wireName: String) {
    PINNED("pinned"),
    SPEAKING("speaking"),
    RECENT("recent"),
    PICTURE("picture"),
    FIRST("first"),
}

/** The short name of a person in the line: the SFU peer id, `me` for this
 *  device. Never a display name. */
internal fun voiceLabel(id: String, isLocal: Boolean): String =
    if (isLocal) "me" else peerIdOfParticipant(id)

/**
 * Why [stageId] is on the large tile, given what the shared rule was asked
 * with. Mirrors the rule's order; it explains an answer, it does not make one.
 */
internal fun stageReason(
    stageId: String,
    people: List<CallUiState.Participant>,
    pinnedId: String?,
    speech: StageSpeech,
): StageReason = when {
    pinnedId == stageId -> StageReason.PINNED
    speech.activeSpeakerId == stageId -> StageReason.SPEAKING
    (speech.lastSpokeAtMs[stageId] ?: 0L) > 0L -> StageReason.RECENT
    people.any { it.id == stageId && it.camOn } -> StageReason.PICTURE
    else -> StageReason.FIRST
}

/** The line. [stage] is `null` when no large tile is drawn. */
internal fun describeVoices(
    prefix: String,
    voices: List<VoiceReport>,
    floorId: String?,
    stage: Pair<String, StageReason>?,
    layout: GroupLayoutKind,
): String {
    fun label(id: String): String = voiceLabel(id, voices.firstOrNull { it.id == id }?.isLocal == true)
    val people = voices.map { v ->
        val peak = v.peak?.let { String.format(Locale.ROOT, "%.2f", it) } ?: "-"
        "${voiceLabel(v.id, v.isLocal)}=${v.path.wireName}:$peak:${if (v.speaking) "yes" else "no"}"
    }
    return (
        listOf("$prefix voices") + people + listOf(
            "floor=${floorId?.let(::label) ?: "-"}",
            "stage=${stage?.let { (id, reason) -> "${label(id)}(${reason.wireName})" } ?: "-"}",
            "layout=${if (layout == GroupLayoutKind.FOCUS) "focus" else "grid"}",
        )
        ).joinToString(" ")
}

/**
 * The `voices` line for the call [ui] shows now: the layout on screen and,
 * in Focus, who the shared rule puts on the large tile and why.
 */
internal fun voicesLine(
    ui: CallUiState,
    rules: GroupCallRules,
    voices: List<VoiceReport>,
    floorId: String?,
): String {
    val people = ui.participants.value
    val pinned = ui.pinnedId.value
    val speech = ui.speech.value
    val layout = groupLayoutKind(ui.layoutChoice.value, pinned, people)
    val onStage = if (layout == GroupLayoutKind.FOCUS) {
        try {
            stagePerson(rules, people, pinned, speech)
        } catch (cause: RuntimeException) {
            null
        } catch (cause: LinkageError) {
            // The native library is not loaded.
            null
        }
    } else {
        null
    }
    return describeVoices(
        "[call]",
        voices,
        floorId,
        onStage?.let { it.id to stageReason(it.id, people, pinned, speech) },
        layout,
    )
}
