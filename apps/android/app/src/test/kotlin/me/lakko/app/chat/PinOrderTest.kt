// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FolderConversationKind
import uniffi.lakko_ffi_uniffi.FolderConversationRef

/**
 * docs/ux/navigation.md section 2.2.1 — reordering pinned conversations, the
 * Android half: which register bytes name which rows, which lists each action
 * hands the shared rules, what each action writes, and where a dragged pin
 * lands.
 *
 * The rules themselves (`pin_order_after_*`, `pin_can_move`) are
 * `crates/lakko-ui-rules/src/pin_order.rs`, tested there; the host JVM has no
 * native library, so [FakeRule] stands in: a line-for-line port of those
 * rules that also records what it was asked, so a test can check both the
 * inputs this side hands over and the list it writes.
 */
class PinOrderTest {

    /** A port of `pin_order.rs`, recording its calls. */
    private class FakeRule : PinOrderRule {
        val calls = mutableListOf<String>()

        private fun dedup(ids: List<String>): List<String> = ids.distinct()

        private fun base(stored: List<String>, shown: List<String>): List<String> =
            dedup(shown.filter { it !in stored } + stored)

        override fun afterPin(stored: List<String>, shown: List<String>, id: String): List<String> {
            calls += "pin $stored $shown $id"
            return dedup(listOf(id) + base(stored, shown).filter { it != id })
        }

        override fun afterUnpin(stored: List<String>, shown: List<String>, id: String): List<String> {
            calls += "unpin $stored $shown $id"
            return base(stored, shown).filter { it != id }
        }

        override fun afterMove(stored: List<String>, shown: List<String>, id: String, toIndex: Int): List<String> {
            calls += "move $stored $shown $id $toIndex"
            val list = base(stored, shown).toMutableList()
            val visible = dedup(shown).toMutableList()
            val from = visible.indexOf(id)
            if (from < 0) return list
            val moved = visible.removeAt(from)
            visible.add(toIndex.coerceIn(0, visible.size), moved)
            val next = visible.iterator()
            for (i in list.indices) {
                if (list[i] in shown && next.hasNext()) list[i] = next.next()
            }
            return list
        }

        override fun canMove(shown: List<String>, id: String, up: Boolean): Boolean {
            val pins = dedup(shown)
            val at = pins.indexOf(id)
            if (at < 0) return false
            return if (up) at > 0 else at + 1 < pins.size
        }
    }

    private val a = "aa".repeat(32)
    private val b = "bb".repeat(32)
    private val c = "cc".repeat(32)
    private val d = "dd".repeat(32)
    private val channel = "ee".repeat(16)
    private val unheld = "ff".repeat(32)

    private fun row(id: String, pinned: Boolean = false, kind: ConversationRowKind = ConversationRowKind.GROUP) =
        ConversationListRow(
            id = id,
            kind = kind,
            title = id.take(4),
            preview = "",
            time = null,
            lastActivityMs = null,
            pinned = pinned,
        )

    private fun ref(id: String, kind: FolderConversationKind = FolderConversationKind.GROUP) =
        FolderConversationRef(kind = kind, id = bytesOfHex(id)!!)

    private fun folder(pinned: List<FolderConversationRef>) = ChatFolder(
        id = ByteArray(16) { 7 },
        name = "Work",
        includeKinds = IncludeKind.GROUPS,
        excludeFlags = 0u,
        included = emptyList(),
        excluded = emptyList(),
        pinned = pinned,
    )

    private fun counter(): StampSource {
        var n = 100uL
        return { n++ }
    }

    // ---- register bytes <-> row ids ---------------------------------------

    @Test
    fun register_entries_name_rows_by_the_rows_own_id_string() {
        // A row whose id string is upper-case is named by that exact string,
        // so the shared rule (which compares strings) finds it.
        val upper = row(b.uppercase(), pinned = true)
        val ids = pinOrderIds(
            listOf(bytesOfHex(b)!!, bytesOfHex(a)!!, bytesOfHex(b)!!, ByteArray(0), bytesOfHex(unheld)!!),
            listOf(row(a, pinned = true), upper),
        )
        assertEquals("repeats and empty entries are dropped", listOf(b.uppercase(), a, unheld), ids)
    }

    @Test
    fun row_ids_go_back_to_the_same_register_bytes() {
        val ids = listOf(a, unheld, channel, "not hex", a.uppercase())
        val bytes = pinOrderBytes(ids)
        assertEquals(3, bytes.size)
        assertTrue(bytes[0].contentEquals(bytesOfHex(a)!!))
        assertTrue("an entry this device does not hold survives", bytes[1].contentEquals(bytesOfHex(unheld)!!))
        assertTrue("a channel id is 16 bytes", bytes[2].contentEquals(bytesOfHex(channel)!!))
        // Round trip through the mapping.
        assertEquals(listOf(a, unheld, channel), pinOrderIds(bytes, listOf(row(a))))
    }

    @Test
    fun shown_pins_are_the_pinned_rows_in_drawn_order() {
        val ordered = listOf(row(c, pinned = true), row(a, pinned = true), row(b), row(d))
        assertEquals(listOf(c, a), shownPinIds(ordered))
    }

    @Test
    fun the_order_is_handed_to_the_shared_rule_with_the_rows() {
        var seen: List<String>? = null
        val rows = listOf(row(a, pinned = true), row(b, pinned = true))
        val ordered = orderConversationRows(rows, listOf(b, a)) { _, pins ->
            seen = pins
            listOf(1u, 0u)
        }
        assertEquals(listOf(b, a), seen)
        assertEquals(listOf(b, a), ordered.map { it.id })
    }

    // ---- All chats --------------------------------------------------------

    @Test
    fun pinning_in_all_chats_puts_the_new_pin_on_top() {
        val rule = FakeRule()
        val ordered = listOf(row(a, pinned = true), row(b, pinned = true), row(c))
        val next = allChatsPinToggleOrder(listOf(a, b), ordered, row(c), rule)
        assertEquals("pin [$a, $b] [$a, $b] $c", rule.calls.single())
        assertEquals(listOf(c, a, b), next)
    }

    @Test
    fun unpinning_in_all_chats_keeps_the_other_pins_in_their_order() {
        val rule = FakeRule()
        val ordered = listOf(row(a, pinned = true), row(b, pinned = true), row(c, pinned = true))
        val next = allChatsPinToggleOrder(listOf(a, b, c), ordered, ordered[1], rule)
        assertEquals("unpin [$a, $b, $c] [$a, $b, $c] $b", rule.calls.single())
        assertEquals(listOf(a, c), next)
    }

    @Test
    fun a_pin_made_before_the_order_existed_is_written_in_where_it_was_drawn() {
        // `x` is pinned but the register does not name it: drawn above.
        val rule = FakeRule()
        val ordered = listOf(row(d, pinned = true), row(a, pinned = true), row(b, pinned = true))
        assertEquals(listOf(a, b, d), allChatsMoveOrder(listOf(a, b), ordered, ordered[0], 2, rule))
    }

    @Test
    fun a_move_in_all_chats_writes_the_whole_list_and_nothing_else() {
        val rule = FakeRule()
        val ordered = listOf(row(a, pinned = true), row(b, pinned = true), row(c, pinned = true), row(d))
        // Move down from the first place.
        assertEquals(listOf(b, a, c), allChatsMoveOrder(listOf(a, b, c), ordered, ordered[0], 1, rule))
        // A drop below the last pin lands last, never among the unpinned.
        assertEquals(listOf(b, c, a), allChatsMoveOrder(listOf(a, b, c), ordered, ordered[0], 99, rule))
        assertEquals("move [$a, $b, $c] [$a, $b, $c] $a 1", rule.calls.first())
    }

    // ---- a folder ---------------------------------------------------------

    @Test
    fun a_new_folder_pin_goes_on_top_and_an_unheld_pin_stays() {
        val f = folder(listOf(ref(a), ref(unheld), ref(b)))
        val ordered = listOf(row(a, pinned = true), row(b, pinned = true), row(c))
        val changes = folderPinToggleChanges(f, ordered, row(c), counter(), FakeRule())
        assertNotNull(changes)
        val register = changes!!.pins.single()
        assertTrue(register.folderId.contentEquals(f.id))
        assertEquals(100uL, register.hlc)
        assertEquals(listOf(c, a, unheld, b), register.pinned.map(::rowIdOf))
        assertTrue(changes.members.isEmpty() && changes.metas.isEmpty() && changes.order == null)
    }

    @Test
    fun an_unpin_in_a_folder_keeps_the_others_and_the_unheld_pin() {
        val f = folder(listOf(ref(a), ref(unheld), ref(b)))
        val ordered = listOf(row(a, pinned = true), row(b, pinned = true))
        val changes = folderPinToggleChanges(f, ordered, ordered[0], counter(), FakeRule())
        assertEquals(listOf(unheld, b), changes!!.pins.single().pinned.map(::rowIdOf))
    }

    @Test
    fun the_tenth_folder_pin_is_the_last() {
        val ten = (0 until FolderLimits.MAX_PINS).map { i -> ref("%02x".format(i).repeat(32)) }
        assertNull(folderPinToggleChanges(folder(ten), emptyList(), row(c), counter(), FakeRule()))
    }

    @Test
    fun a_folder_move_keeps_the_stored_reference_and_kind_of_each_pin() {
        val chRef = ref(channel, FolderConversationKind.CHANNEL)
        val f = folder(listOf(ref(a), ref(unheld), chRef))
        val ordered = listOf(
            row(a, pinned = true),
            row(channel, pinned = true, kind = ConversationRowKind.CHANNEL),
        )
        val changes = folderMoveChanges(f, ordered, ordered[1], 0, counter(), FakeRule())
        val pins = changes!!.pins.single().pinned
        assertEquals(listOf(channel, unheld, a), pins.map(::rowIdOf))
        assertEquals(FolderConversationKind.CHANNEL, pins[0].kind)
    }

    @Test
    fun a_folder_move_to_its_own_place_writes_nothing() {
        val f = folder(listOf(ref(a), ref(b)))
        val ordered = listOf(row(a, pinned = true), row(b, pinned = true))
        assertNull(folderMoveChanges(f, ordered, ordered[0], 0, counter(), FakeRule()))
    }

    @Test
    fun folder_pin_ids_become_references_from_the_folder_then_the_rows() {
        val f = folder(listOf(ref(unheld, FolderConversationKind.ONE_TO_ONE)))
        val refs = folderPinRefs(
            listOf(unheld, c, "not a row", c),
            f,
            listOf(row(c, kind = ConversationRowKind.CHAT)),
        )
        assertEquals(listOf(unheld, c), refs.map(::rowIdOf))
        assertEquals(FolderConversationKind.ONE_TO_ONE, refs[0].kind)
        assertEquals(FolderConversationKind.ONE_TO_ONE, refs[1].kind)
    }

    // ---- the drag ---------------------------------------------------------

    @Test
    fun a_dropped_pin_lands_below_every_pin_it_passed() {
        // The other pins' centres, top to bottom.
        val others = listOf(50f, 150f, 250f)
        assertEquals("above the first pin: first", 0, pinDropIndex(-500f, others))
        assertEquals(0, pinDropIndex(40f, others))
        assertEquals(1, pinDropIndex(100f, others))
        assertEquals(2, pinDropIndex(200f, others))
        assertEquals("anywhere below the last pin: last", 3, pinDropIndex(10_000f, others))
        // Pins scrolled out of sight above and below.
        assertEquals(1, pinDropIndex(0f, listOf(Float.NEGATIVE_INFINITY, Float.POSITIVE_INFINITY)))
    }

    @Test
    fun the_other_pins_step_aside_toward_the_place_the_dragged_one_left() {
        // Dragging the first of four to the third place.
        assertEquals(listOf(0, -1, -1, 0), (0..3).map { pinShift(it, from = 0, to = 2) })
        // Dragging the last of four to the second place.
        assertEquals(listOf(0, 1, 1, 0), (0..3).map { pinShift(it, from = 3, to = 1) })
        // Back where it started: nothing moves.
        assertEquals(listOf(0, 0, 0), (0..2).map { pinShift(it, from = 1, to = 1) })
    }

    @Test
    fun move_up_and_down_are_offered_only_where_they_move() {
        val rule = FakeRule()
        val shown = listOf(a, b, c)
        assertNull(pinStepTarget(shown, a, up = true, rule = rule))
        assertEquals(1, pinStepTarget(shown, a, up = false, rule = rule))
        assertEquals(0, pinStepTarget(shown, b, up = true, rule = rule))
        assertNull(pinStepTarget(shown, c, up = false, rule = rule))
        assertNull("one pin offers neither", pinStepTarget(listOf(a), a, up = false, rule = rule))
        assertNull("not a pin", pinStepTarget(shown, d, up = true, rule = rule))
    }

    @Test
    fun a_drag_starts_only_on_a_pin_among_two_or_more_outside_search_and_the_archive() {
        assertTrue(pinDragAllowed(row(a, pinned = true), 2, searching = false, archiveShown = false))
        assertFalse("unpinned", pinDragAllowed(row(a), 2, searching = false, archiveShown = false))
        assertFalse("one pin", pinDragAllowed(row(a, pinned = true), 1, searching = false, archiveShown = false))
        assertFalse("searching", pinDragAllowed(row(a, pinned = true), 3, searching = true, archiveShown = false))
        assertFalse("archive", pinDragAllowed(row(a, pinned = true), 3, searching = false, archiveShown = true))
    }

    // ---- applied at once ---------------------------------------------------

    @Test
    fun a_written_order_is_drawn_until_the_register_changes() {
        val written = OptimisticPinOrder(ids = listOf(b, a), base = listOf(a, b))
        assertEquals("the register still reads as before", listOf(b, a), effectivePinOrder(listOf(a, b), written))
        assertEquals("the register moved on: it wins", listOf(c, a, b), effectivePinOrder(listOf(c, a, b), written))
        assertEquals(listOf(a, b), effectivePinOrder(listOf(a, b), null))
    }

    // ---- uneven row heights -------------------------------------------------

    @Test
    fun a_drop_reads_the_measured_slots_not_one_row_height() {
        // Three pins: 72 px, then a 140 px row (a muted call's Accept /
        // Decline under it), then 72 px. The first is dragged down.
        val slots = listOf(PinSlot(0f, 72), PinSlot(72f, 140), PinSlot(212f, 72))
        assertEquals(listOf(142f, 248f), measuredPinCenters(slots, dragged = 0))
        // Lifted to top 100 (centre 136): it has not passed the tall row's
        // centre (142) yet, although with every row 72 px high it would have
        // (centre 108).
        assertEquals(0, pinDropIndexMeasured(100f, 72, slots, dragged = 0))
        assertEquals(1, pinDropIndexMeasured(120f, 72, slots, dragged = 0))
        assertEquals(1, pinDropIndexMeasured(200f, 72, slots, dragged = 0))
        assertEquals(2, pinDropIndexMeasured(220f, 72, slots, dragged = 0))
        // The tall row itself, dragged up: its own height sets its centre.
        assertEquals(0, pinDropIndexMeasured(-40f, 140, slots, dragged = 1))
        assertEquals(1, pinDropIndexMeasured(-20f, 140, slots, dragged = 1))
    }

    @Test
    fun pins_out_of_sight_lie_above_or_below_the_measured_ones() {
        val slots = listOf(null, PinSlot(0f, 72), PinSlot(72f, 72), null)
        assertEquals(
            listOf(Float.NEGATIVE_INFINITY, 108f, Float.POSITIVE_INFINITY),
            measuredPinCenters(slots, dragged = 1),
        )
        // No other pin measured: placed by their side of the dragged one.
        assertEquals(
            listOf(Float.NEGATIVE_INFINITY, Float.POSITIVE_INFINITY),
            measuredPinCenters(listOf(null, PinSlot(0f, 72), null), dragged = 1),
        )
    }

    @Test
    fun the_others_step_aside_by_the_dragged_rows_own_height() {
        // The tall 140 px row dragged from the second place to the first:
        // the first pin steps down by 140, not by its own 72.
        assertEquals(140f, pinShiftPx(0, from = 1, to = 0, draggedSize = 140))
        assertEquals(-72f, pinShiftPx(1, from = 0, to = 1, draggedSize = 72))
        assertEquals(0f, pinShiftPx(2, from = 0, to = 1, draggedSize = 72))
    }

    // ---- a write that fails --------------------------------------------------

    @Test
    fun a_write_is_judged_by_what_the_register_reads_after_it() {
        assertEquals(PinWriteOutcome.Applied, pinWriteOutcome(listOf(b, a), listOf(b, a), threw = false))
        assertEquals(PinWriteOutcome.NotApplied, pinWriteOutcome(listOf(b, a), listOf(a, b), threw = false))
        assertEquals(PinWriteOutcome.Failed, pinWriteOutcome(listOf(b, a), listOf(b, a), threw = true))
    }

    @Test
    fun a_failed_or_refused_write_returns_the_screen_to_the_register_at_once() {
        val ours = OptimisticPinOrder(ids = listOf(b, a), base = listOf(a, b))
        assertNull(settleOptimistic(ours, ours, PinWriteOutcome.Failed))
        assertNull(settleOptimistic(ours, ours, PinWriteOutcome.NotApplied))
        // The register is then what is drawn.
        assertEquals(listOf(a, b), effectivePinOrder(listOf(a, b), settleOptimistic(ours, ours, PinWriteOutcome.Failed)))
        // Applied: kept until the screen's copy of the register catches up.
        assertEquals(ours, settleOptimistic(ours, ours, PinWriteOutcome.Applied))
        // A later write that replaced it is not touched by the earlier one.
        val later = OptimisticPinOrder(ids = listOf(a, b), base = listOf(b, a))
        assertEquals(later, settleOptimistic(later, ours, PinWriteOutcome.Failed))
        // Not the same write even when equal by value.
        val twin = ours.copy()
        assertEquals(twin, settleOptimistic(twin, ours, PinWriteOutcome.Failed))
    }

    @Test
    fun a_failed_folder_write_drops_only_that_folders_optimistic_pins() {
        val ours = OptimisticPinOrder(ids = listOf(b, a), base = listOf(a, b))
        val other = OptimisticPinOrder(ids = listOf(c), base = emptyList())
        val map = mapOf("f1" to ours, "f2" to other)
        assertEquals(mapOf("f2" to other), settleFolderOptimistic(map, "f1", ours, PinWriteOutcome.NotApplied))
        assertEquals(map, settleFolderOptimistic(map, "f1", ours, PinWriteOutcome.Applied))
        assertEquals(map, settleFolderOptimistic(map, "f2", ours, PinWriteOutcome.Failed))
    }
}
