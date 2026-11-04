// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertFalse

/** The words a notification must never say in place of a person: the
 *  catalog's placeholders for a person nothing is known about. */
internal val PLACEHOLDER_WORDS: List<String>
    get() = listOf(
        EnglishForTests.strings["chat.members.unnamed"],
        EnglishForTests.strings["contacts.request.someone"],
        "Someone",
        "Member",
    ).distinct()

/** Fails when any of [texts] carries a placeholder word. */
internal fun assertNoPlaceholderWord(vararg texts: String?) {
    for (text in texts) {
        if (text == null) continue
        for (word in PLACEHOLDER_WORDS) {
            assertFalse("\"$text\" carries the placeholder \"$word\"", text.contains(word))
        }
    }
}
