// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.activity.ComponentActivity
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsNotSelected
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * The emoji picker's two tabs as TalkBack reads them. On iOS both tabs were
 * once read with the sheet's own label ("Emoji and GIFs"); here each tab must
 * be named by what it shows, carry the tab role, and say which one is
 * selected.
 */
@RunWith(RobolectricTestRunner::class)
class EmojiGifTabsTest {
    init {
        EnglishForTests.install()
    }

    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private val isTab = SemanticsMatcher.expectValue(SemanticsProperties.Role, Role.Tab)

    private fun mount() {
        compose.setContent {
            MaterialTheme {
                var tab by remember { mutableStateOf(0) }
                EmojiGifTabs(selected = tab, onSelect = { tab = it })
            }
        }
    }

    @Test
    fun `each tab is named by what it shows and has the tab role`() {
        mount()
        compose.onNodeWithText(en("chat.emoji.tab")).assert(isTab)
        compose.onNodeWithText(en("chat.media.gif")).assert(isTab)
        assertEquals("Emoji", en("chat.emoji.tab"))
        assertEquals("GIF", en("chat.media.gif"))
    }

    @Test
    fun `no tab carries the sheet's label`() {
        mount()
        assertEquals(
            0,
            compose.onAllNodesWithContentDescription(en("chat.composer.emoji_gifs"))
                .fetchSemanticsNodes().size,
        )
        compose.onNodeWithText(en("chat.emoji.tab"))
            .assert(hasContentDescription(en("chat.composer.emoji_gifs")).not())
    }

    @Test
    fun `the selected state follows the tab a person picks`() {
        mount()
        compose.onNodeWithText(en("chat.emoji.tab")).assertIsSelected()
        compose.onNodeWithText(en("chat.media.gif")).assertIsNotSelected()

        compose.onNodeWithText(en("chat.media.gif")).performClick()

        compose.onNodeWithText(en("chat.media.gif")).assertIsSelected()
        compose.onNodeWithText(en("chat.emoji.tab")).assertIsNotSelected()
    }
}
