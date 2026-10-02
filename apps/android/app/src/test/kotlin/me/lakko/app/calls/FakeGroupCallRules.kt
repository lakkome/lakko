// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import uniffi.lakko_ffi_uniffi.CallBarPhase
import uniffi.lakko_ffi_uniffi.CallGrid
import uniffi.lakko_ffi_uniffi.StageCandidate

/**
 * A stand-in for the shared call rules, for host tests.
 *
 * The real rules are `crates/lakko-ui-rules/src/call_group.rs`, reached over
 * UniFFI, and the host JVM cannot load `liblakko_ffi_uniffi.so`. So a test
 * here never checks WHAT a rule answers (that is tested in the core, and on
 * the web client through wasm): it checks what this client ASKS the rule and
 * what it does with the answer. This records every question and answers what
 * the test scripted.
 */
internal class FakeGroupCallRules(
    private val grid: (count: UInt, width: UInt, height: UInt) -> CallGrid = { count, width, height ->
        // One row of equal tiles: enough shape for the wiring to be visible.
        val cols = count.coerceAtLeast(1u)
        CallGrid(
            cols = cols,
            rows = 1u,
            tileWidth = width / cols,
            tileHeight = height,
            gap = 8u,
            perPage = count,
            pages = 1u,
        )
    },
    private val stage: (people: List<StageCandidate>, pinned: UInt?) -> UInt? = { _, _ -> null },
    private val ringback: (groupCall: Boolean, phase: CallBarPhase) -> Boolean = { _, _ -> false },
) : GroupCallRules {
    val gridQuestions = ArrayList<Triple<UInt, UInt, UInt>>()
    val stageQuestions = ArrayList<Pair<List<StageCandidate>, UInt?>>()
    val ringbackQuestions = ArrayList<Pair<Boolean, CallBarPhase>>()

    override fun callGrid(count: UInt, width: UInt, height: UInt): CallGrid {
        gridQuestions.add(Triple(count, width, height))
        return grid(count, width, height)
    }

    override fun callStage(people: List<StageCandidate>, pinned: UInt?): UInt? {
        stageQuestions.add(people to pinned)
        return stage(people, pinned)
    }

    override fun ringbackPlays(groupCall: Boolean, phase: CallBarPhase): Boolean {
        ringbackQuestions.add(groupCall to phase)
        return ringback(groupCall, phase)
    }
}

/** A participant for a layout test. */
internal fun person(
    id: String,
    isLocal: Boolean = false,
    camOn: Boolean = false,
    micOn: Boolean = true,
    speaking: Boolean = false,
): CallUiState.Participant = CallUiState.Participant(
    id = id,
    displayName = id,
    isLocal = isLocal,
    micOn = micOn,
    camOn = camOn,
    quality = CallUiState.QualityTier.Good,
    speaking = speaking,
)
