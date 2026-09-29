// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0058 section 6 — a Welcome goes to each added ACCOUNT's mailbox, once
 * per account, and a Welcome sealed to another device of this account is not
 * this device's failure.
 */
class WelcomeRoutingTest {

    private val alice = byteArrayOf(1)
    private val bob = byteArrayOf(2)

    @Test
    fun `two devices of one account get one Welcome write`() {
        val accounts = welcomeAccounts(
            listOf(byteArrayOf(10), byteArrayOf(11), byteArrayOf(20)),
        ) { leaf -> if (leaf[0] < 20) alice else bob }
        assertEquals(2, accounts.size)
        assertArrayEquals(alice, accounts[0])
        assertArrayEquals(bob, accounts[1])
    }

    @Test
    fun `a leaf with no binding stands for its own account`() {
        val leaf = byteArrayOf(7)
        val accounts = welcomeAccounts(listOf(leaf)) { null }
        assertArrayEquals(leaf, accounts.single())
        val empty = welcomeAccounts(listOf(leaf)) { ByteArray(0) }
        assertArrayEquals(leaf, empty.single())
    }

    @Test
    fun `no added leaves means no write`() {
        assertTrue(welcomeAccounts(emptyList()) { alice }.isEmpty())
    }

    @Test
    fun `a Welcome for another device is recognised anywhere in the cause chain`() {
        val core = RuntimeException("Group(\"staged welcome: NoMatchingKeyPackage\")")
        assertTrue(isWelcomeForAnotherDevice(core))
        assertTrue(isWelcomeForAnotherDevice(IllegalStateException("joinFromWelcome failed", core)))
        assertFalse(isWelcomeForAnotherDevice(IllegalStateException("staged welcome: InvalidSignature")))
    }
}
