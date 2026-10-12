// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import me.lakko.app.l10n.en
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for the KT-log S4 reset-aware supersession-refusal
 * copy. Pure mapping — no Compose, no native lib, no `ViewModel`. Mirror of
 * the web `supersession-message` vitest.
 *
 * Asserts the [supersessionRefusedMessage] mapping (verbatim
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
    // supersessionRefusedMessage — the pure map
    // -----------------------------------------------------------------

    @Test
    fun `a marker that is not a confirmed absence of a reset reads as unconfirmed`() {
        // An accepted reset is silent and never reaches this copy; a `true`
        // marker therefore says nothing more than "not confirmed".
        assertEquals(supersessionRefusedMessage(null, "@alice"), supersessionRefusedMessage(true, "@alice"))
        assertEquals("contacts.identity_changed.unconfirmed", supersessionRefusedMessage(true, "@alice").key)
    }

    @Test
    fun `loggedReset false flags a possible compromise`() {
        val msg = supersessionRefusedMessage(false, "@alice").en
        assertEquals(
            "@alice's secure identity changed, and the transparency log does " +
                "NOT show an owner reset — treat this as a possible account " +
                "compromise. Verify out of band before trusting.",
            msg,
        )
    }

    @Test
    fun `loggedReset null is inconclusive`() {
        val msg = supersessionRefusedMessage(null, "@alice").en
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
        assertTrue(supersessionRefusedMessage(false, null).en.startsWith("This contact's "))
        assertTrue(supersessionRefusedMessage(null, null).en.startsWith("This contact's "))
    }

    @Test
    fun `empty peer label falls back to This contact`() {
        assertTrue(supersessionRefusedMessage(false, "").en.startsWith("This contact's "))
    }

    @Test
    fun `every branch is a security-critical catalog message`() {
        assertEquals("contacts.identity_changed.unexplained", supersessionRefusedMessage(false, "@a").key)
        assertEquals("contacts.identity_changed.unexplained_unnamed", supersessionRefusedMessage(false, null).key)
        assertEquals("contacts.identity_changed.unconfirmed", supersessionRefusedMessage(null, "@a").key)
    }

    // -----------------------------------------------------------------
    // No emoji / pictographs in any branch (code-style invariant)
    // -----------------------------------------------------------------

    @Test
    fun `messages contain no emoji`() {
        for (reset in listOf(true, false, null)) {
            val msg = supersessionRefusedMessage(reset, "@alice").en
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
