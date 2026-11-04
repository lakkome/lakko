// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * docs/ux/reactions-and-emoji.md section 13 — the `:` suggestions: typing `:`
 * at the start of a word followed by two or more letters shows up to eight
 * matching emoji above the composer; a tap replaces `:word` with the emoji. A
 * space, or the caret moving away, closes the strip. No suggestion for a plain
 * word (D9). Pure, host-testable.
 */

/** The `:word` being typed: where its `:` is and the word after it. */
public data class ColonQuery(val start: Int, val word: String)

/** The most suggestions the strip shows. */
public const val COLON_SUGGESTIONS_MAX: Int = 8

/** The `:word` ending at the caret of [text], or `null`. [caret] is a char
 *  index; a selection that is not collapsed asks for nothing. */
public fun colonQueryAt(text: String, caret: Int, collapsed: Boolean = true): ColonQuery? {
    if (!collapsed || caret < 0 || caret > text.length) return null
    var i = caret - 1
    while (i >= 0 && text[i] != ':' && !text[i].isWhitespace()) i--
    if (i < 0 || text[i] != ':') return null
    if (i > 0 && !text[i - 1].isWhitespace()) return null
    val word = text.substring(i + 1, caret)
    if (word.length < 2 || !word.all { it.isLetterOrDigit() || it == '_' }) return null
    return ColonQuery(start = i, word = word)
}

/** [text] with the `:word` of [query] replaced by [emoji]; the new caret. */
public fun applyColonPick(text: String, caret: Int, query: ColonQuery, emoji: String): Pair<String, Int> {
    val out = text.substring(0, query.start) + emoji + text.substring(caret)
    return out to (query.start + emoji.length)
}
