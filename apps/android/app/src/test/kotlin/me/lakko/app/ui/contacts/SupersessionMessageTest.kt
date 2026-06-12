// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for the KT-log S4 reset-aware supersession-refusal
 * copy. Pure mapping — no Compose, no native lib, no `ViewModel`. Mirror of
 * the web `supersession-message` vitest.
 *
 * Asserts the three-way [supersessionRefusedMessage] mapping (verbatim
 * strings, shared cross-platform) + the [atLabel] handle normalisation that
 * feeds its subject.
 */
class SupersessionMessageTest {

    // -----------------------------------------------------------------
    // atLabel — typed-username → @handle subject (or null fallback)
    // -----------------------------------------------------------------

    @Test
    fun `atLabel prefixes a bare username`() {
        assertEquals("@alice", atLabel("alice"))
    }

    @Test
    fun `atLabel does not double a leading at`() {
        assertEquals("@alice", atLabel("@alice"))
    }

    @Test
    fun `atLabel trims surrounding whitespace`() {
        assertEquals("@alice", atLabel("  alice  "))
    }

    @Test
    fun `atLabel returns null for an empty input`() {
        assertEquals(null, atLabel(""))
        assertEquals(null, atLabel("   "))
        assertEquals(null, atLabel("@"))
    }

    // -----------------------------------------------------------------
    // supersessionRefusedMessage — the pure 3-way map
    // -----------------------------------------------------------------

    @Test
    fun `loggedReset true confirms an owner-initiated reset`() {
        val msg = supersessionRefusedMessage(true, "@alice")
        assertEquals(
            "@alice reset their secure identity (for example, they lost " +
                "their device). The transparency log confirms this was an " +
                "owner-initiated reset. If you trust this is really them, " +
                "re-verify their safety number out of band before continuing.",
            msg,
        )
    }

    @Test
    fun `loggedReset false flags a possible compromise`() {
        val msg = supersessionRefusedMessage(false, "@alice")
        assertEquals(
            "@alice's secure identity changed, and the transparency log does " +
                "NOT show an owner reset — treat this as a possible account " +
                "compromise. Verify out of band before trusting.",
            msg,
        )
    }

    @Test
    fun `loggedReset null is inconclusive`() {
        val msg = supersessionRefusedMessage(null, "@alice")
        assertEquals(
            "@alice's secure identity changed and could not be confirmed " +
                "against the transparency log. Verify out of band before trusting.",
            msg,
        )
    }

    // -----------------------------------------------------------------
    // peerLabel fallback — "This contact" when no handle was typed
    // -----------------------------------------------------------------

    @Test
    fun `null peer label falls back to This contact`() {
        assertTrue(supersessionRefusedMessage(true, null).startsWith("This contact "))
        assertTrue(supersessionRefusedMessage(false, null).startsWith("This contact's "))
        assertTrue(supersessionRefusedMessage(null, null).startsWith("This contact's "))
    }

    @Test
    fun `empty peer label falls back to This contact`() {
        assertTrue(supersessionRefusedMessage(true, "").startsWith("This contact "))
    }

    // -----------------------------------------------------------------
    // No emoji / pictographs in any branch (code-style invariant)
    // -----------------------------------------------------------------

    @Test
    fun `messages contain no emoji`() {
        for (reset in listOf(true, false, null)) {
            val msg = supersessionRefusedMessage(reset, "@alice")
            // The em-dash U+2014 is allowed (it appears in the web copy);
            // assert there are no characters in the emoji / pictograph
            // ranges so a stray icon never sneaks into the copy.
            val hasEmoji = msg.any { ch ->
                val cp = ch.code
                cp in 0x1F000..0x1FAFF || cp in 0x2600..0x27BF
            }
            assertEquals("no emoji in: $msg", false, hasEmoji)
        }
    }
}
