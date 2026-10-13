// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * ADR-0074 section 3.5, "Reach" - what a bind does with the registrations a
 * device recorded when the account key it binds is, or is not, the one they
 * were made under.
 */
class ReachRekeyTest {

    private val old = "aa".repeat(32)
    private val new = "bb".repeat(32)

    @Test
    fun `registrations made under the bound key are kept`() {
        assertEquals(ReachRekey.KEEP, rekeyDecision(boundKeyHex = old, registeredForKeyHex = old, previousKeyHex = null))
        assertEquals(ReachRekey.KEEP, rekeyDecision(boundKeyHex = new, registeredForKeyHex = new, previousKeyHex = old))
    }

    @Test
    fun `a first bind records which key the registrations belong to`() {
        assertEquals(ReachRekey.STAMP, rekeyDecision(boundKeyHex = old, registeredForKeyHex = null, previousKeyHex = null))
        // The same key bound again in this process changes nothing either.
        assertEquals(ReachRekey.STAMP, rekeyDecision(boundKeyHex = old, registeredForKeyHex = null, previousKeyHex = old))
    }

    @Test
    fun `a rotation in this session moves the registrations to the new mailbox`() {
        assertEquals(ReachRekey.MOVE, rekeyDecision(boundKeyHex = new, registeredForKeyHex = old, previousKeyHex = old))
        // Even when the document never recorded a key.
        assertEquals(ReachRekey.MOVE, rekeyDecision(boundKeyHex = new, registeredForKeyHex = null, previousKeyHex = old))
    }

    @Test
    fun `a rotation finished before a restart is still moved at the next bind`() {
        // The process died between adopting the keys and the reach step: the
        // first bind after the restart is under the new key, the document
        // still describes the old mailbox.
        assertEquals(ReachRekey.MOVE, rekeyDecision(boundKeyHex = new, registeredForKeyHex = old, previousKeyHex = null))
    }
}
