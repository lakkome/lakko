// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.components

import org.junit.Assert.assertEquals
import org.junit.Test

/** One person, one avatar colour on every screen: the colour follows the
 *  person's key, not the id a screen happens to hold. */
class AvatarTintSeedTest {

    private val key = ByteArray(32) { it.toByte() }
    private val keyHex = key.joinToString("") { "%02x".format(it) }

    @Test
    fun `a person's colour follows their key whatever seed the screen passes`() {
        val chats = avatarTintSeed("aa".repeat(32), key, group = false)
        val picker = avatarTintSeed("route-key", key, group = false)
        val contacts = avatarTintSeed("account-id", key, group = false)
        assertEquals(keyHex, chats)
        assertEquals(chats, picker)
        assertEquals(chats, contacts)
    }

    @Test
    fun `groups, channels and an unknown key keep the seed`() {
        assertEquals("group", avatarTintSeed("group", key, group = true))
        assertEquals("seed", avatarTintSeed("seed", null, group = false))
        assertEquals("seed", avatarTintSeed("seed", ByteArray(16), group = false))
    }
}
