// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Host-side tests for the Invite links "Create link" form rules. */
class InviteCreateFormTest {

    private val hour = 3_600_000L
    private val day = 24 * hour

    @Test
    fun `the form opens on 7 days and one person`() {
        val p = InviteCreateForm().toParams()!!
        assertEquals("", p.label)
        assertEquals(7 * day, p.expiryMillisFromNow)
        assertEquals(0u, p.maxUses)
    }

    @Test
    fun `presets map to the wire values`() {
        val f = InviteCreateForm()
        assertEquals(day, f.copy(expiry = InviteExpiryChoice.OneDay).toParams()!!.expiryMillisFromNow)
        assertEquals(30 * day, f.copy(expiry = InviteExpiryChoice.ThirtyDays).toParams()!!.expiryMillisFromNow)
        // Never: no period, so the repository writes the NEVER_EXPIRES sentinel 0.
        assertNull(f.copy(expiry = InviteExpiryChoice.Never).toParams()!!.expiryMillisFromNow)
        assertEquals(10u, f.copy(uses = InviteUsesChoice.UpToTen).toParams()!!.maxUses)
        assertEquals(UInt.MAX_VALUE, f.copy(uses = InviteUsesChoice.Unlimited).toParams()!!.maxUses)
        assertEquals(INVITE_UNLIMITED_USES, UInt.MAX_VALUE.toLong())
        assertEquals("Mum", f.copy(label = "  Mum ").toParams()!!.label)
    }

    @Test
    fun `custom expiry accepts 1 hour to 365 days`() {
        assertEquals(hour, customInviteExpiryMs("1", InviteExpiryUnit.Hours))
        assertEquals(12 * day, customInviteExpiryMs("12", InviteExpiryUnit.Days))
        assertEquals(365 * day, customInviteExpiryMs("365", InviteExpiryUnit.Days))
        assertEquals(8760 * hour, customInviteExpiryMs(" 8760 ", InviteExpiryUnit.Hours))
        for (bad in listOf("", "0", "-1", "1.5", "abc", "1e3", "366", "9999999999")) {
            assertNull(bad, customInviteExpiryMs(bad, InviteExpiryUnit.Days))
        }
        assertNull(customInviteExpiryMs("8761", InviteExpiryUnit.Hours))
    }

    @Test
    fun `custom uses accepts 1 to 10000, one person is single use`() {
        assertEquals(0u, customInviteMaxUses("1"))
        assertEquals(25u, customInviteMaxUses("25"))
        assertEquals(10_000u, customInviteMaxUses("10000"))
        for (bad in listOf("", "0", "-3", "2.5", "x", "10001")) {
            assertNull(bad, customInviteMaxUses(bad))
        }
    }

    @Test
    fun `custom fields gate Create and show errors only for typed invalid text`() {
        val expiry = InviteCreateForm(expiry = InviteExpiryChoice.Custom)
        assertNull(expiry.toParams())
        assertFalse(expiry.showsCustomExpiryError())
        val badExpiry = expiry.copy(customExpiryAmount = "400")
        assertNull(badExpiry.toParams())
        assertTrue(badExpiry.showsCustomExpiryError())
        val okExpiry = expiry.copy(customExpiryAmount = "48", customExpiryUnit = InviteExpiryUnit.Hours)
        assertEquals(48 * hour, okExpiry.toParams()!!.expiryMillisFromNow)
        assertFalse(okExpiry.showsCustomExpiryError())

        val uses = InviteCreateForm(uses = InviteUsesChoice.Custom)
        assertNull(uses.toParams())
        assertFalse(uses.showsCustomUsesError())
        assertTrue(uses.copy(customUses = "0").showsCustomUsesError())
        assertEquals(42u, uses.copy(customUses = "42").toParams()!!.maxUses)
        // A stale invalid custom value does not matter once a preset is chosen.
        assertEquals(0u, uses.copy(customUses = "0", uses = InviteUsesChoice.One).toParams()!!.maxUses)
    }

    @Test
    fun `label is clamped to the core's byte limit at a code point`() {
        val ascii = "a".repeat(80)
        assertEquals(INVITE_LABEL_MAX_BYTES, clampInviteLabel(ascii).length)
        val cyrillic = "ж".repeat(40) // 2 bytes each
        assertEquals(32, clampInviteLabel(cyrillic).length)
        val emoji = "a" + "😀".repeat(20) // 1 + 4 bytes each
        val clamped = clampInviteLabel(emoji)
        assertTrue(clamped.toByteArray(Charsets.UTF_8).size <= INVITE_LABEL_MAX_BYTES)
        assertEquals(1 + 15 * 2, clamped.length)
        assertEquals("Mum", clampInviteLabel("Mum"))
    }
}
