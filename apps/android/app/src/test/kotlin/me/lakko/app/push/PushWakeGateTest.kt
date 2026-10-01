// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0042, "A wake is shown only for the signed-in account" (amended
 * 2026-09-30) — the pure gate [wakeAccountFor] and the token-rotation rule
 * [shouldRegisterOnNewToken].
 *
 * A wake is handled only when a binding exists and the wake's `d` is the
 * bound pseudonym; it is then handled as the bound account. A token rotation
 * registers only while that binding exists and names the pseudonym this
 * device holds.
 */
class PushWakeGateTest {

    private val device = "0a".repeat(32)
    private val otherDevice = "0b".repeat(32)
    private val account = "acct-A=="
    private val binding = PushWakeBinding(devicePseudonymHex = device, accountIdB64 = account)

    @Test
    fun `no binding refuses every wake`() {
        assertNull(wakeAccountFor(device, null))
        assertNull(wakeAccountFor(null, null))
    }

    @Test
    fun `a wake for another pseudonym is refused`() {
        assertNull(wakeAccountFor(otherDevice, binding))
    }

    @Test
    fun `a wake without d or with a malformed d is refused`() {
        assertNull(wakeAccountFor(null, binding))
        assertNull(wakeAccountFor("", binding))
        assertNull(wakeAccountFor("zz".repeat(32), binding))
        assertNull(wakeAccountFor(device.dropLast(1), binding))
    }

    @Test
    fun `the right d is handled as the bound account`() {
        assertEquals(account, wakeAccountFor(device, binding))
    }

    @Test
    fun `the compare is on normalised hex`() {
        assertEquals(account, wakeAccountFor(" " + device.uppercase() + "\n", binding))
        val upperBinding = binding.copy(devicePseudonymHex = device.uppercase())
        assertEquals(account, wakeAccountFor(device, upperBinding))
    }

    @Test
    fun `a binding with no account refuses`() {
        assertNull(wakeAccountFor(device, binding.copy(accountIdB64 = "")))
    }

    @Test
    fun `a signed-out device does not register on a token rotation`() {
        assertFalse(shouldRegisterOnNewToken(null, device))
        assertFalse(shouldRegisterOnNewToken(null, null))
    }

    @Test
    fun `a token rotation registers only the bound pseudonym`() {
        assertTrue(shouldRegisterOnNewToken(binding, device))
        assertFalse(shouldRegisterOnNewToken(binding, otherDevice))
        assertFalse(shouldRegisterOnNewToken(binding, null))
        assertFalse(shouldRegisterOnNewToken(binding.copy(accountIdB64 = ""), device))
    }
}
