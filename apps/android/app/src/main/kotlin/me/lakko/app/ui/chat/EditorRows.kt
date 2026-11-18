// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

/**
 * One entry of the poll / checklist editor's list (UX `attachments.md` 7.1a).
 * [id] is stable for the life of the editor so a dragged row, its focus and a
 * quiz's correct option follow the row and not its position.
 */
internal data class EditorRow(val id: Long, val text: String)

/**
 * The list of options / items in the poll and checklist editors (UX 7.1a):
 * between [min] and [max] rows, no empty trailing row, rows added only by
 * `Add option` / `Add item` or IME Next on the last row, removed only by the
 * row's remove icon (shown above the minimum), reordered by drag or by the
 * accessibility move actions. Immutable; every edit returns a new value.
 */
internal data class EditorRows(
    val rows: List<EditorRow>,
    val min: Int,
    val max: Int,
    private val nextId: Long,
) {
    val canAdd: Boolean get() = rows.size < max
    val canRemove: Boolean get() = rows.size > min

    /** The texts of the filled rows, trimmed, in order. */
    val filled: List<String> get() = rows.map { it.text.trim() }.filter { it.isNotEmpty() }

    /** Whether any row holds text. */
    val anyText: Boolean get() = rows.any { it.text.isNotBlank() }

    fun indexOf(id: Long): Int = rows.indexOfFirst { it.id == id }

    fun setText(id: Long, text: String, maxChars: Int): EditorRows =
        copy(rows = rows.map { if (it.id == id) it.copy(text = text.take(maxChars)) else it })

    /** Adds an empty row at the end; returns the new state and the new row's id, or `null` at the maximum. */
    fun add(): Pair<EditorRows, Long>? {
        if (!canAdd) return null
        val id = nextId
        return copy(rows = rows + EditorRow(id, ""), nextId = nextId + 1) to id
    }

    /** Removes the row [id]; unchanged at the minimum. */
    fun remove(id: Long): EditorRows = if (!canRemove) this else copy(rows = rows.filterNot { it.id == id })

    /** Moves the row at [from] to [to] (both clamped); unchanged when out of range. */
    fun move(from: Int, to: Int): EditorRows {
        if (from !in rows.indices) return this
        val target = to.coerceIn(0, rows.lastIndex)
        if (target == from) return this
        val list = rows.toMutableList()
        val row = list.removeAt(from)
        list.add(target, row)
        return copy(rows = list)
    }

    /**
     * IME Next in the row [id]: the next row's id to focus, adding a row when
     * [id] is the last one. `null` when there is nowhere to go (the last row
     * at the maximum), which closes the keyboard.
     */
    fun next(id: Long): Pair<EditorRows, Long>? {
        val i = indexOf(id)
        if (i < 0) return null
        if (i < rows.lastIndex) return this to rows[i + 1].id
        return add()
    }

    companion object {
        /** The rows to start with: [texts], padded with empty rows up to [min], capped at [max]. */
        fun of(texts: List<String>, min: Int, max: Int): EditorRows {
            val start = texts.take(max).toMutableList()
            while (start.size < min) start.add("")
            return EditorRows(start.mapIndexed { i, t -> EditorRow(i.toLong(), t) }, min, max, start.size.toLong())
        }
    }
}

/** UX 7.1 — the poll editor's bounds (chosen). */
internal const val POLL_OPTIONS_MIN = 2
internal const val POLL_OPTIONS_MAX = 10

/** UX 8.1 — the checklist editor's bounds (chosen). */
internal const val CHECKLIST_ITEMS_MIN = 1
internal const val CHECKLIST_ITEMS_MAX = 30

/** UX 7.1 / 8.1 — the character limit of one option or item (chosen). */
internal const val EDITOR_ROW_MAX_CHARS = 100

/**
 * The index among the poll's filled options of the row [correctId], or
 * `null` when it is not set or that row is empty.
 */
internal fun pollCorrectIndex(rows: EditorRows, correctId: Long?): Int? {
    val id = correctId ?: return null
    var kept = 0
    for (row in rows.rows) {
        if (row.text.isBlank()) continue
        if (row.id == id) return kept
        kept++
    }
    return null
}
