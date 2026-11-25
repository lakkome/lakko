// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import uniffi.lakko_ffi_uniffi.FolderChangeSet
import uniffi.lakko_ffi_uniffi.FolderConversationRef
import uniffi.lakko_ffi_uniffi.FolderPinsRegister

/**
 * Reordering pinned conversations (`docs/ux/navigation.md` section 2.2.1,
 * `docs/ux/chat-folders.md` section 2.3): the client half.
 *
 * **The rules are not here.** Where a pinned row is drawn is
 * `order_conversations` / `order_conversations_in_folder`, and the list each
 * action writes (pin, unpin, move) is `pin_order_after_*` in
 * `crates/lakko-ui-rules`, shared with web and iOS. What this file owns is the
 * mapping between the synced registers (bytes, folder references) and the row
 * ids the rules speak, which list each action hands them, and the geometry of
 * a drag (where a dropped row lands, which rows step aside).
 *
 * Pure of Compose and Android; the shared rules arrive through [PinOrderRule]
 * so the host JVM tests, which cannot load the native library, can stand in.
 */

/** The shared pin-order rules, as one seam. */
public interface PinOrderRule {
    /** `pin_order_after_pin`. */
    public fun afterPin(stored: List<String>, shown: List<String>, id: String): List<String>

    /** `pin_order_after_unpin`. */
    public fun afterUnpin(stored: List<String>, shown: List<String>, id: String): List<String>

    /** `pin_order_after_move`; [toIndex] is the pin's index among the shown
     *  pins after the move. */
    public fun afterMove(stored: List<String>, shown: List<String>, id: String, toIndex: Int): List<String>

    /** `pin_can_move`. */
    public fun canMove(shown: List<String>, id: String, up: Boolean): Boolean
}

/** The rules over the native library. */
public object SharedPinOrderRule : PinOrderRule {
    override fun afterPin(stored: List<String>, shown: List<String>, id: String): List<String> =
        uniffi.lakko_ffi_uniffi.pinOrderAfterPin(stored, shown, id)

    override fun afterUnpin(stored: List<String>, shown: List<String>, id: String): List<String> =
        uniffi.lakko_ffi_uniffi.pinOrderAfterUnpin(stored, shown, id)

    override fun afterMove(stored: List<String>, shown: List<String>, id: String, toIndex: Int): List<String> =
        uniffi.lakko_ffi_uniffi.pinOrderAfterMove(stored, shown, id, toIndex.coerceAtLeast(0).toUInt())

    override fun canMove(shown: List<String>, id: String, up: Boolean): Boolean =
        uniffi.lakko_ffi_uniffi.pinCanMove(shown, id, up)
}

// ---------------------------------------------------------------------------
// Register <-> row ids
// ---------------------------------------------------------------------------

/**
 * The `ConversationOrder` register as row ids. Its entries are the keys of
 * the synced flags register (a chat's or group's conversation id, a channel's
 * id), so each maps to a row the way the flags do: its lower-case hex. Where
 * this device draws that conversation the row's own id string is used, so the
 * shared rule compares it with the row exactly; an entry with no row here
 * stays as its hex, which [pinOrderBytes] turns back into the same bytes.
 */
public fun pinOrderIds(groupIds: List<ByteArray>, rows: List<ConversationListRow>): List<String> {
    if (groupIds.isEmpty()) return emptyList()
    val byKey = HashMap<String, String>(rows.size)
    for (row in rows) byKey.putIfAbsent(row.id.lowercase(), row.id)
    val out = ArrayList<String>(groupIds.size)
    for (bytes in groupIds) {
        if (bytes.isEmpty()) continue
        val hex = hexOf(bytes)
        val id = byKey[hex] ?: hex
        if (id !in out) out += id
    }
    return out
}

/** Row ids back to the register's bytes. An id that is not hex (it cannot
 *  name a conversation) is dropped; a repeat is written once. */
public fun pinOrderBytes(ids: List<String>): List<ByteArray> {
    val seen = HashSet<String>()
    val out = ArrayList<ByteArray>(ids.size)
    for (id in ids) {
        val bytes = bytesOfHex(id) ?: continue
        if (seen.add(id.lowercase())) out += bytes
    }
    return out
}

/** The pinned rows, in the order the list draws them: what the rules call
 *  `shown`. [ordered] is the list already ordered by the shared rule. */
public fun shownPinIds(ordered: List<ConversationListRow>): List<String> =
    ordered.filter { it.pinned }.map { it.id }

// ---------------------------------------------------------------------------
// All chats: what each action writes to the ConversationOrder register
// ---------------------------------------------------------------------------

/**
 * The order register a Pin / Unpin of [row] in All chats writes beside the
 * flag: a new pin goes on top, an unpin leaves the others in their order.
 * [row] carries its state BEFORE the toggle.
 */
public fun allChatsPinToggleOrder(
    stored: List<String>,
    ordered: List<ConversationListRow>,
    row: ConversationListRow,
    rule: PinOrderRule = SharedPinOrderRule,
): List<String> {
    val shown = shownPinIds(ordered)
    return if (row.pinned) {
        rule.afterUnpin(stored, shown, row.id)
    } else {
        rule.afterPin(stored, shown, row.id)
    }
}

/** The order register a drag drop or Move up / Move down writes: [row] lands
 *  at [toIndex] among the shown pins. No flag changes: a drag never unpins. */
public fun allChatsMoveOrder(
    stored: List<String>,
    ordered: List<ConversationListRow>,
    row: ConversationListRow,
    toIndex: Int,
    rule: PinOrderRule = SharedPinOrderRule,
): List<String> = rule.afterMove(stored, shownPinIds(ordered), row.id, toIndex)

// ---------------------------------------------------------------------------
// A folder: what each action writes to the folder's own pin list
// ---------------------------------------------------------------------------

/**
 * Row ids back to the folder's references. A pin the folder already holds
 * keeps its stored reference (its kind included, even for a chat this device
 * does not hold); a new one is named from its row. An id neither names is
 * dropped: it cannot be written as a reference.
 */
public fun folderPinRefs(
    ids: List<String>,
    folder: ChatFolder,
    rows: List<ConversationListRow>,
): List<FolderConversationRef> {
    val held = folder.pinned.associateBy(::refKey)
    val fromRows = HashMap<String, ConversationListRow>()
    for (row in rows) fromRows.putIfAbsent(row.id.lowercase(), row)
    val seen = HashSet<String>()
    val out = ArrayList<FolderConversationRef>(ids.size)
    for (id in ids) {
        val key = id.lowercase()
        if (!seen.add(key)) continue
        val ref = held[key] ?: fromRows[key]?.let(::refOf) ?: continue
        out += ref
    }
    return out
}

/** The pin ids a folder's rows draw, in their order, in the folder's key form
 *  (lower-case hex, which is what [ChatFolder.pinnedRowIds] holds). */
private fun folderShown(ordered: List<ConversationListRow>): List<String> =
    ordered.filter { it.pinned }.map { it.id.lowercase() }

/**
 * Pin or unpin [row] inside [folder] (UX spec 2.3): a new pin on top, an unpin
 * keeping the others' order, a pin the folder names but this device does not
 * draw kept at its place. [ordered] is the folder's rows as drawn
 * ([folderRows]); [row] carries the folder's pin BEFORE the toggle. `null`
 * when a new pin would pass the folder's limit or the row cannot be named.
 */
public fun folderPinToggleChanges(
    folder: ChatFolder,
    ordered: List<ConversationListRow>,
    row: ConversationListRow,
    stamp: StampSource,
    rule: PinOrderRule = SharedPinOrderRule,
): FolderChangeSet? {
    refOf(row) ?: return null
    val id = row.id.lowercase()
    val pinned = folder.pinned.any { refNames(it, row) }
    if (!pinned && folder.pinned.size >= FolderLimits.MAX_PINS) return null
    val stored = folder.pinnedRowIds
    val shown = folderShown(ordered)
    val next = if (pinned) rule.afterUnpin(stored, shown, id) else rule.afterPin(stored, shown, id)
    return pinsChange(folder, folderPinRefs(next, folder, ordered + row), stamp)
}

/** A drag drop or Move up / Move down inside [folder]: [row] lands at
 *  [toIndex] among the folder's shown pins. `null` when nothing moves. */
public fun folderMoveChanges(
    folder: ChatFolder,
    ordered: List<ConversationListRow>,
    row: ConversationListRow,
    toIndex: Int,
    stamp: StampSource,
    rule: PinOrderRule = SharedPinOrderRule,
): FolderChangeSet? {
    val next = rule.afterMove(folder.pinnedRowIds, folderShown(ordered), row.id.lowercase(), toIndex)
    if (next == folder.pinnedRowIds) return null
    return pinsChange(folder, folderPinRefs(next, folder, ordered), stamp)
}

private fun pinsChange(
    folder: ChatFolder,
    refs: List<FolderConversationRef>,
    stamp: StampSource,
): FolderChangeSet? {
    if (refs.map(::refKey) == folder.pinned.map(::refKey)) return null
    return FolderChangeSet(
        metas = emptyList(),
        members = emptyList(),
        pins = listOf(FolderPinsRegister(stamp(), folder.id, refs)),
        removed = emptyList(),
        order = null,
    )
}

// ---------------------------------------------------------------------------
// The drag
// ---------------------------------------------------------------------------

/** Whether a drag may start on [row] (navigation 2.2.1): a pinned row, two or
 *  more pins, nothing typed in search, not in the archive view. */
public fun pinDragAllowed(
    row: ConversationListRow,
    pinCount: Int,
    searching: Boolean,
    archiveShown: Boolean,
): Boolean = row.pinned && pinCount >= 2 && !searching && !archiveShown

/**
 * Where a dragged pin lands: its index among the shown pins after the move.
 * [draggedCenter] is the centre of the lifted row where the finger has taken
 * it; [otherCenters] the centres of the OTHER pins, in their drawn order, at
 * their places before the drag (a pin scrolled out above is
 * [Float.NEGATIVE_INFINITY], below [Float.POSITIVE_INFINITY]). The row lands
 * below every pin whose centre it passed, so a drop above the first pin is 0
 * and one anywhere below the last is the last place: never among the
 * unpinned rows.
 */
public fun pinDropIndex(draggedCenter: Float, otherCenters: List<Float>): Int =
    otherCenters.count { it < draggedCenter }

/**
 * Where one pin's slot is in the list, as measured: its top and its height in
 * pixels (`LazyListState.layoutInfo.visibleItemsInfo`). Rows differ in height
 * (a muted call's Accept / Decline row is taller), so no single row height
 * stands in for these.
 */
public data class PinSlot(val top: Float, val size: Int) {
    /** The slot's centre. */
    val center: Float get() = top + size / 2f
}

/**
 * The centres [pinDropIndex] compares with: every pin but the dragged one
 * (index [dragged] in [slots]), in drawn order, from its measured slot. A pin
 * with no measured slot (`null`, scrolled out of sight) lies above when it
 * comes before the first measured other pin and below otherwise; with no
 * other pin measured, a pin before the dragged one lies above.
 */
public fun measuredPinCenters(slots: List<PinSlot?>, dragged: Int): List<Float> {
    val firstMeasured = slots.indices.firstOrNull { it != dragged && slots[it] != null } ?: -1
    return slots.indices.filter { it != dragged }.map { i ->
        val slot = slots[i]
        when {
            slot != null -> slot.center
            firstMeasured >= 0 -> if (i < firstMeasured) Float.NEGATIVE_INFINITY else Float.POSITIVE_INFINITY
            else -> if (i < dragged) Float.NEGATIVE_INFINITY else Float.POSITIVE_INFINITY
        }
    }
}

/**
 * Where a dragged pin lands, from measured slots: the lifted row's top is
 * [draggedTop] (where the finger has taken it) and its own measured height
 * [draggedSize]; [slots] are every pin's measured slot, the dragged one at
 * [dragged].
 */
public fun pinDropIndexMeasured(draggedTop: Float, draggedSize: Int, slots: List<PinSlot?>, dragged: Int): Int =
    pinDropIndex(draggedTop + draggedSize / 2f, measuredPinCenters(slots, dragged))

/**
 * How far, in pixels, the pin at [index] is drawn moved aside while the pin
 * dragged from [from] would land at [to]: by the dragged row's own measured
 * height [draggedSize], the room it leaves and takes.
 */
public fun pinShiftPx(index: Int, from: Int, to: Int, draggedSize: Int): Float =
    pinShift(index, from, to) * draggedSize.toFloat()

/**
 * How a pin steps aside while another is dragged from [from] to [to] (both
 * indices among the shown pins): +1 one row down, -1 one row up, 0 stays.
 * [index] is the pin's own index among the shown pins, the dragged one
 * included; the dragged pin itself is 0 (it follows the finger instead).
 */
public fun pinShift(index: Int, from: Int, to: Int): Int = when {
    index == from -> 0
    to < from && index in to until from -> 1
    to > from && index in (from + 1)..to -> -1
    else -> 0
}

/** Move up (`up = true`) or Move down: the index the pin lands at, or `null`
 *  when the shared rule offers no such move. */
public fun pinStepTarget(
    shown: List<String>,
    id: String,
    up: Boolean,
    rule: PinOrderRule = SharedPinOrderRule,
): Int? {
    if (!rule.canMove(shown, id, up)) return null
    val at = shown.indexOf(id)
    if (at < 0) return null
    return if (up) at - 1 else at + 1
}

// ---------------------------------------------------------------------------
// Applied at once
// ---------------------------------------------------------------------------

/**
 * A pin order written on this device and not yet back from the synced state.
 * The write folds in locally a moment later; until the register differs from
 * [base] (the value the write was made over), the list draws [ids], so a
 * dropped row never snaps back to its old place for a frame.
 */
public data class OptimisticPinOrder(val ids: List<String>, val base: List<String>)

/** The pin order to draw: the optimistic one while the register still reads
 *  as it did when it was written, else the register. */
public fun effectivePinOrder(stored: List<String>, optimistic: OptimisticPinOrder?): List<String> =
    if (optimistic != null && optimistic.base == stored) optimistic.ids else stored

/** How a write of a pin order ended, as far as this device can see. */
public enum class PinWriteOutcome {
    /** The register now reads what was written. */
    Applied,

    /** The write returned but the register does not read it (refused). */
    NotApplied,

    /** The write threw before it was applied. */
    Failed,
}

/** [written] against what the register reads right after the local write. */
public fun pinWriteOutcome(written: List<String>, registerNow: List<String>, threw: Boolean): PinWriteOutcome =
    when {
        threw -> PinWriteOutcome.Failed
        registerNow == written -> PinWriteOutcome.Applied
        else -> PinWriteOutcome.NotApplied
    }

/**
 * The optimistic order once the local write of [ours] has run. Applied: kept,
 * until the screen's copy of the register catches up (it reads what was
 * written, and the list's effect drops [ours] then), so the row does not snap
 * back for a frame. Refused or thrown: dropped at once, so the screen returns
 * to the register's order. A later write that already replaced [ours] is kept
 * either way.
 */
public fun settleOptimistic(
    current: OptimisticPinOrder?,
    ours: OptimisticPinOrder,
    outcome: PinWriteOutcome,
): OptimisticPinOrder? =
    if (outcome != PinWriteOutcome.Applied && current === ours) null else current

/** [settleOptimistic] for the per-folder map, keyed by folder id hex. */
public fun settleFolderOptimistic(
    current: Map<String, OptimisticPinOrder>,
    folderIdHex: String,
    ours: OptimisticPinOrder,
    outcome: PinWriteOutcome,
): Map<String, OptimisticPinOrder> =
    if (outcome != PinWriteOutcome.Applied && current[folderIdHex] === ours) current - folderIdHex else current
