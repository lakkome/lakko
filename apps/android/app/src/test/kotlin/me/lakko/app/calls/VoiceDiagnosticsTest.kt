// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The `voices` line of the call log (`VoiceDiagnostics.kt`): the same layout
 * as the web client's, so a log from a phone and one from a browser read
 * alike. It explains what the stage rule answered; it decides nothing.
 */
class VoiceDiagnosticsTest {

    private val voices = listOf(
        VoiceReport("local", isLocal = true, path = LevelPath.STATS_LEVEL, peak = 0.314, speaking = true),
        VoiceReport(
            "peer-p1",
            isLocal = false,
            path = LevelPath.STATS_ENERGY,
            peak = 0.006,
            speaking = false,
            threshold = 0.0123,
        ),
        VoiceReport("peer-p2", isLocal = false, path = LevelPath.NONE, peak = null, speaking = false),
    )

    @Test
    fun `the line names each person by peer id, never by name, with source, peak and ring`() {
        assertEquals(
            "[call] voices me=stats-level:0.314:yes[thr=0.020] p1=stats-energy:0.006:no[thr=0.012]" +
                " p2=none:-:no[thr=0.020]" +
                " floor=p1 stage=p1(speaking) layout=focus",
            describeVoices("[call]", voices, "peer-p1", "peer-p1" to StageReason.SPEAKING, GroupLayoutKind.FOCUS),
        )
        assertEquals(
            "[call] voices floor=- stage=- layout=grid",
            describeVoices("[call]", emptyList(), null, null, GroupLayoutKind.GRID),
        )
    }

    @Test
    fun `why someone is on the large tile follows the stage rule's order`() {
        val people = listOf(person("local", isLocal = true), person("peer-a", camOn = true), person("peer-b"))
        val speech = StageSpeech(activeSpeakerId = "peer-a", lastSpokeAtMs = mapOf("peer-b" to 5L))
        assertEquals(StageReason.PINNED, stageReason("peer-a", people, "peer-a", speech))
        assertEquals(StageReason.SPEAKING, stageReason("peer-a", people, null, speech))
        assertEquals(StageReason.RECENT, stageReason("peer-b", people, null, speech))
        assertEquals(StageReason.PICTURE, stageReason("peer-a", people, null, StageSpeech.None))
        assertEquals(StageReason.FIRST, stageReason("peer-b", people, null, StageSpeech.None))
    }

    @Test
    fun `the line of a call says who the shared rule put on the stage, and none in the grid`() {
        val ui = CallUiState(CoroutineScope(Dispatchers.Unconfined))
        ui.setParticipants(listOf(person("local", isLocal = true), person("peer-a"), person("peer-b")))
        val rules = FakeGroupCallRules(stage = { _, pinned -> pinned ?: 1u })
        // The grid has no large tile, and the rule is not asked.
        assertEquals("[call] voices floor=- stage=- layout=grid", voicesLine(ui, rules, emptyList(), null))
        assertEquals(0, rules.stageQuestions.size)

        ui.setLayoutChoice(GroupLayoutKind.FOCUS)
        assertEquals(
            "[call] voices floor=- stage=a(first) layout=focus",
            voicesLine(ui, rules, emptyList(), null),
        )
        ui.pin("peer-b")
        assertEquals(
            "[call] voices floor=a stage=b(pinned) layout=focus",
            voicesLine(ui, rules, emptyList(), "peer-a"),
        )
        // While the rule cannot be asked the line still comes, with no stage.
        val broken = FakeGroupCallRules(stage = { _, _ -> throw UnsatisfiedLinkError("no library") })
        assertEquals("[call] voices floor=- stage=- layout=focus", voicesLine(ui, broken, emptyList(), null))
    }
}
