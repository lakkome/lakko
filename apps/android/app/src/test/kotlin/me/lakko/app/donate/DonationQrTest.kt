// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.donate

import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.RGBLuminanceSource
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeReader
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** docs/ux/donations.md section 2.4: the crypto QR stays scannable with the
 *  centre cleared for the coin mark. */
class DonationQrTest {
    private val addresses = listOf(
        "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh",
        "0x52908400098527886E0F7030069857D2E4169EE7",
        "UQBvW8Z5huBkMJYdnfAEM5JqTNkuWX3diqYENkWsIL0XggGG",
        "TXYZopYRdj2D9XRtbG411XZZ3kM5VkAeBf",
    )

    /** Render [plan] at [scale] pixels per module and read it back. */
    private fun decode(plan: DonationQr.Plan, scale: Int = 4): String? {
        val side = plan.sideModules * scale
        val pixels = IntArray(side * side) { i ->
            if (plan.isDark((i % side) / scale, (i / side) / scale)) 0xFF1A1D2B.toInt() else -1
        }
        val bitmap = BinaryBitmap(HybridBinarizer(RGBLuminanceSource(side, side, pixels)))
        return try {
            QRCodeReader().decode(bitmap, mapOf(DecodeHintType.PURE_BARCODE to true)).text
        } catch (_: Exception) {
            null
        }
    }

    @Test
    fun `a code with the centre cleared for a mark still reads back the address`() {
        for (address in addresses) {
            val plan = DonationQr.plan(address, withMark = true)
            assertNotNull(plan)
            val hole = plan!!.hole
            assertNotNull("no room for a mark on $address", hole)
            for (y in hole!!.from until hole.from + hole.span) {
                for (x in hole.from until hole.from + hole.span) {
                    assertFalse(plan.dark[y * plan.size + x])
                }
            }
            assertEquals(address, decode(plan))
        }
    }

    @Test
    fun `a code without a mark clears nothing and reads back`() {
        val plan = DonationQr.plan(addresses[0], withMark = false)!!
        assertNull(plan.hole)
        assertEquals(addresses[0], decode(plan))
    }

    @Test
    fun `the quiet zone is four light modules on every side`() {
        val plan = DonationQr.plan(addresses[1], withMark = true)!!
        assertEquals(plan.size + 8, plan.sideModules)
        for (i in 0 until plan.sideModules) {
            for (q in 0 until DonationQr.QUIET_MODULES) {
                assertFalse(plan.isDark(i, q))
                assertFalse(plan.isDark(q, i))
                assertFalse(plan.isDark(i, plan.sideModules - 1 - q))
                assertFalse(plan.isDark(plan.sideModules - 1 - q, i))
            }
        }
        // The top-left finder pattern starts right after the quiet zone.
        assertTrue(plan.isDark(DonationQr.QUIET_MODULES, DonationQr.QUIET_MODULES))
    }

    @Test
    fun `the cleared span is odd and at most a fifth of the side`() {
        for (n in 21..177 step 4) {
            val k = DonationQr.holeSpan(n)
            if (k != 0) {
                assertEquals(1, k % 2)
                assertTrue(k <= n / 5)
                assertTrue(k >= 5)
            }
        }
        assertEquals(0, DonationQr.holeSpan(21))
    }
}
