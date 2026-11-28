// SPDX-License-Identifier: AGPL-3.0-or-later
package me.lakko.app.translation

import java.io.File
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * UX 3.1 / 3.7 as amended 2026-11-27: own messages are translated like
 * received ones (the menu row, the language line, automatic translation),
 * while the conversation bar and the conversation's languages count only
 * incoming messages. The offer rule itself carries no ownership any more, so
 * the only place an own-message guard could come back is the bubble code.
 */
class OwnMessagesTranslatedTest {
    private val source: String by lazy {
        val root = listOf(File("src/main/kotlin"), File("app/src/main/kotlin")).first { it.isDirectory }
        File(root, "me/lakko/app/ui/chat/BubbleTranslation.kt").readText()
    }

    @Test
    fun `the bubble asks the offer and the line without an ownership argument`() {
        assertTrue(source.contains("TranslationCenter.translateOffer(conversation, msg.messageId, readable)"))
        assertTrue(source.contains("TranslationCenter.languageLine(conversation, msg.messageId, msg.text)"))
    }

    @Test
    fun `own messages are detected and automatically translated`() {
        assertFalse(source.contains("!isOwn) TranslationCenter.detect"))
        assertFalse(source.contains("autoOn && !isOwn"))
        assertTrue(source.contains("TranslationCenter.detect(conversation, msg.messageId, readable, incoming = !isOwn)"))
    }

    @Test
    fun `only incoming messages feed the conversation's languages`() {
        val center = File(
            listOf(File("src/main/kotlin"), File("app/src/main/kotlin")).first { it.isDirectory },
            "me/lakko/app/translation/TranslationCenter.kt",
        ).readText()
        assertTrue(center.contains("if (incoming) rememberConfident("))
    }
}
