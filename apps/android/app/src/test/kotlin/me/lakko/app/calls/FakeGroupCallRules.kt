// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import uniffi.lakko_ffi_uniffi.CallBarPhase
import uniffi.lakko_ffi_uniffi.CallGrid
import uniffi.lakko_ffi_uniffi.StageCandidate
import uniffi.lakko_ffi_uniffi.TileVideoQuality

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
    private val tileQuality: (people: UInt, tileWidthPx: UInt, pinnedOrStage: Boolean) -> TileVideoQuality =
        { _, _, _ -> TileVideoQuality.MAIN },
) : GroupCallRules {
    val gridQuestions = ArrayList<Triple<UInt, UInt, UInt>>()
    val stageQuestions = ArrayList<Pair<List<StageCandidate>, UInt?>>()
    val ringbackQuestions = ArrayList<Pair<Boolean, CallBarPhase>>()
    val tileQualityQuestions = ArrayList<Triple<UInt, UInt, Boolean>>()

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

    override fun tileVideoQuality(people: UInt, tileWidthPx: UInt, pinnedOrStage: Boolean): TileVideoQuality {
        tileQualityQuestions.add(Triple(people, tileWidthPx, pinnedOrStage))
        return tileQuality(people, tileWidthPx, pinnedOrStage)
    }
}

/**
 * Timers a test moves by hand: nothing runs until [advance] reaches its time.
 * Tasks due at the same instant run in the order they were scheduled.
 */
internal class FakeCallTimers : CallTimers {
    private class Task(val at: Long, val order: Long, val run: () -> Unit)

    private val tasks = ArrayList<Task>()
    private var scheduled = 0L
    var now: Long = 0L
        private set

    override fun nowMs(): Long = now

    override fun schedule(ms: Long, run: () -> Unit): Any {
        val task = Task(now + ms, scheduled++, run)
        tasks += task
        return task
    }

    override fun cancel(handle: Any) {
        tasks.removeAll { it === handle }
    }

    /** How many tasks are waiting. */
    val pending: Int get() = tasks.size

    /** Move the clock on by [ms], running every task that falls due. */
    fun advance(ms: Long) {
        val until = now + ms
        while (true) {
            val next = tasks.filter { it.at <= until }.minWithOrNull(compareBy({ it.at }, { it.order })) ?: break
            tasks.remove(next)
            now = maxOf(now, next.at)
            next.run()
        }
        now = until
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
