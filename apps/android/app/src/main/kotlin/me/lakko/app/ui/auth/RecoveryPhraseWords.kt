// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import kotlin.random.Random

/**
 * How many words a Lakko recovery phrase has: 12 (128 bits of BIP39 entropy),
 * fixed by `lakko-mnemonic` since ADR-0064 decision 3. Every grid, paste
 * spread and confirmation draw reads this one constant.
 */
public const val RECOVERY_PHRASE_WORDS: Int = 12

/** How many words the confirmation step asks for. */
public const val CONFIRMATION_WORDS: Int = 3

/** One word of the phrase with its 1-based position, as the grid shows it. */
public data class NumberedWord(val position: Int, val word: String)

/**
 * Split a phrase as typed or pasted into its words: any whitespace run is a
 * separator, case is folded, and the result never carries empty entries.
 */
public fun phraseWords(text: String): List<String> =
    text.trim().lowercase().split(Regex("\\s+")).filter { it.isNotEmpty() }

/** Columns and rows of the recovery-phrase grid. */
public data class PhraseGridShape(val columns: Int, val rows: Int)

/** Phone width: 3 columns x 4 rows. */
public val PHRASE_GRID_NARROW: PhraseGridShape = PhraseGridShape(columns = 3, rows = 4)

/** Wide screens: 4 columns x 3 rows. */
public val PHRASE_GRID_WIDE: PhraseGridShape = PhraseGridShape(columns = 4, rows = 3)

/** The window width, in dp, from which the wide shape applies (the web
 *  client switches at the same number of CSS pixels). */
public const val PHRASE_GRID_WIDE_MIN_DP: Int = 720

/** The grid shape for a window [widthDp] wide. */
public fun phraseGridShape(widthDp: Int): PhraseGridShape =
    if (widthDp >= PHRASE_GRID_WIDE_MIN_DP) PHRASE_GRID_WIDE else PHRASE_GRID_NARROW

/**
 * Lay the words out row-major: the words read ACROSS the rows (1 2 3 /
 * 4 5 6 / ...), so row `r` holds positions `r * columns + 1` through
 * `(r + 1) * columns`. The read-only grid, the entry grid and the order a
 * person copies and types the words in are all this one order. The last row
 * is shorter when the count does not divide evenly.
 */
public fun phraseGridRows(
    words: List<String>,
    columns: Int = PHRASE_GRID_NARROW.columns,
): List<List<NumberedWord>> {
    if (words.isEmpty() || columns < 1) return emptyList()
    return words.mapIndexed { i, w -> NumberedWord(i + 1, w) }.chunked(columns)
}

/**
 * Fill the entry grid from pasted text: the first [count] words in order,
 * padded with empty slots, so a partial paste lands in the leading fields.
 */
public fun slotsFromPasted(text: String, count: Int = RECOVERY_PHRASE_WORDS): List<String> {
    val words = phraseWords(text).take(count)
    return words + List(count - words.size) { "" }
}

/** The phrase a filled entry grid represents: the non-empty slots, in order. */
public fun phraseFromSlots(slots: List<String>): String =
    slots.map { it.trim().lowercase() }.filter { it.isNotEmpty() }.joinToString(" ")

/**
 * Choose which positions the confirmation step asks for: [count] distinct
 * 1-based positions out of [wordCount], ascending, drawn from [random].
 * Decided on the device only; nothing about the choice leaves it.
 */
public fun pickConfirmationPositions(
    wordCount: Int = RECOVERY_PHRASE_WORDS,
    count: Int = CONFIRMATION_WORDS,
    random: Random = Random.Default,
): List<Int> {
    require(wordCount >= 1) { "wordCount must be positive" }
    val wanted = count.coerceIn(0, wordCount)
    val chosen = mutableSetOf<Int>()
    while (chosen.size < wanted) {
        chosen += random.nextInt(wordCount) + 1
    }
    return chosen.sorted()
}

/** The verdict of one confirmation attempt. */
public sealed interface ConfirmationVerdict {
    /** Every asked word matched. */
    public data object Confirmed : ConfirmationVerdict

    /** The 1-based positions whose typed word did not match, ascending. */
    public data class Mismatch(val positions: List<Int>) : ConfirmationVerdict
}

/**
 * Compare the typed answers with the phrase at the asked positions. Answers
 * are trimmed and case-folded, so `Apple ` confirms `apple`; an empty answer
 * is a mismatch. [answers] is keyed on the 1-based position.
 */
public fun checkConfirmation(
    words: List<String>,
    positions: List<Int>,
    answers: Map<Int, String>,
): ConfirmationVerdict {
    val wrong = positions.filter { position ->
        val expected = words.getOrNull(position - 1)?.trim()?.lowercase()
        val typed = answers[position]?.trim()?.lowercase().orEmpty()
        expected == null || typed.isEmpty() || typed != expected
    }
    return if (wrong.isEmpty()) ConfirmationVerdict.Confirmed else ConfirmationVerdict.Mismatch(wrong)
}
