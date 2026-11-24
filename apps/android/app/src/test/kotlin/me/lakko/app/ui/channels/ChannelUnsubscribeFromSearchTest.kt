// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import androidx.activity.ComponentActivity
import androidx.compose.material3.Text
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import me.lakko.app.navigateClearingTo
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * `docs/ux/group-and-channel-privacy.md` section 9.5 — Unsubscribe from a
 * channel opened from Chats search (or a link) leaves it: the screen goes to
 * Chats and the channel is not added back.
 *
 * Seen live on 2026-11-24: search opens the channel under the route
 * `channel/{id}/subscribe?...`, the info screen's Unsubscribe popped to
 * `channel/{id}` (absent: "Ignoring popBackStack"), the info screen then went
 * Back to that subscribe screen, whose pressed Subscribe added the channel to
 * the list again and registered it on the server again.
 */
@RunWith(RobolectricTestRunner::class)
class ChannelUnsubscribeFromSearchTest {
    @get:Rule
    val rule = createAndroidComposeRule<ComponentActivity>()

    private val id = "f0a0c3f1abdc44d9505e4f76fed3f969"

    private fun host(): NavHostController {
        lateinit var nav: NavHostController
        rule.setContent {
            nav = rememberNavController()
            NavHost(nav, startDestination = "chats") {
                composable("chats") { Text("Chats") }
                composable("channel/{channelId}") { Text("Channel") }
                composable("channel/{channelId}/subscribe?handle={handle}&pressed={pressed}") { Text("Channel (search)") }
                composable("channel/{channelId}/info") { Text("Info") }
            }
        }
        return nav
    }

    @Test
    fun `unsubscribe from a channel opened from search lands on Chats with nothing of the channel below`() {
        val nav = host()
        rule.runOnIdle {
            nav.navigate("channel/$id/subscribe?handle=night_walk&pressed=true")
            nav.navigate("channel/$id/info")
        }
        // What the old code did: nothing was popped.
        rule.runOnIdle { assertFalse(nav.popBackStack("channel/$id", inclusive = true)) }
        rule.runOnIdle { nav.navigateClearingTo("chats") }
        rule.runOnIdle {
            assertEquals("chats", nav.currentDestination?.route)
            val routes = nav.currentBackStack.value.mapNotNull { it.destination.route }
            assertTrue("no channel screen left: $routes", routes.none { it.startsWith("channel/") })
        }
    }

    @Test
    fun `unsubscribe from a channel opened from the list lands on Chats too`() {
        val nav = host()
        rule.runOnIdle {
            nav.navigate("channel/$id")
            nav.navigate("channel/$id/info")
            nav.navigateClearingTo("chats")
        }
        rule.runOnIdle {
            assertEquals("chats", nav.currentDestination?.route)
            assertTrue(nav.currentBackStack.value.none { it.destination.route?.startsWith("channel/") == true })
        }
    }

    @Test
    fun `a Subscribe pressed before opening no longer reads the channel once it was left`() {
        // Opened from search with Subscribe pressed: reads (and registers).
        assertTrue(channelReads(isPublisher = false, followed = false, pressed = true, leftSincePressed = false))
        // Followed.
        assertTrue(channelReads(isPublisher = false, followed = true, pressed = true, leftSincePressed = false))
        // Unsubscribed (here or on another device): the card, nothing re-added.
        assertFalse(channelReads(isPublisher = false, followed = false, pressed = true, leftSincePressed = true))
        // Opened from the list, never pressed: the card.
        assertFalse(channelReads(isPublisher = false, followed = false, pressed = false, leftSincePressed = false))
        // A publisher always reads.
        assertTrue(channelReads(isPublisher = true, followed = false, pressed = false, leftSincePressed = true))
    }
}
