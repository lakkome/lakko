// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import kotlin.random.Random

/** How many words a Lakko recovery phrase has (`lakko-mnemonic` fixes 24). */
public const val RECOVERY_PHRASE_WORDS: Int = 24

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

/**
 * Lay the words out for a two-column grid read top to bottom, left to right:
 * positions 1..n/2 down the first column, the rest down the second. Returns
 * the grid row by row so a `Row` per entry renders it; an odd count leaves
 * the last row with a single entry.
 */
public fun phraseGridRows(words: List<String>, columns: Int = 2): List<List<NumberedWord>> {
    if (words.isEmpty() || columns < 1) return emptyList()
    val numbered = words.mapIndexed { i, w -> NumberedWord(i + 1, w) }
    val perColumn = (numbered.size + columns - 1) / columns
    return (0 until perColumn).map { row ->
        (0 until columns).mapNotNull { col ->
            numbered.getOrNull(col * perColumn + row)
        }
    }
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
