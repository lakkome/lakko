// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.donate

import java.io.IOException
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/** The screen's states (docs/ux/donations.md sections 2.3 and 4.1). */
class DonationsModelTest {
    private val sbp = DonateMethod("sbp", DonateKind.SBP, "https://pay.example", ByteArray(32), true, null)
    private val noQr = DonateMethod("bmc", DonateKind.BMC, "https://bmc.example", null, false, null)

    private class FakeSource(
        var methods: () -> List<DonateMethod> = { emptyList() },
        var phone: (String) -> String = { throw IOException("down") },
        var qr: ByteArray? = null,
    ) : DonationsSource {
        var qrCalls = 0

        override suspend fun methods(): List<DonateMethod> = methods.invoke()
        override suspend fun qr(id: String, expectedSha256: ByteArray): ByteArray? {
            qrCalls += 1
            return qr
        }
        override suspend fun phone(id: String): String = phone.invoke(id)
    }

    @Test
    fun `verified methods are the loaded state`() = runTest {
        val model = DonationsModel(FakeSource(methods = { listOf(sbp, noQr) }))
        model.load()
        val s = model.state.value
        assertTrue(s is DonationsState.Loaded)
        assertEquals(listOf("sbp", "bmc"), (s as DonationsState.Loaded).methods.map { it.id })
    }

    @Test
    fun `nothing verified is the nothing-for-this-country state`() = runTest {
        val model = DonationsModel(FakeSource())
        model.load()
        assertSame(DonationsState.None, model.state.value)
    }

    @Test
    fun `a network failure is the error state and try again loads`() = runTest {
        val source = FakeSource(methods = { throw DonationsUnavailable("down") })
        val model = DonationsModel(source)
        model.load()
        assertSame(DonationsState.Error, model.state.value)
        source.methods = { listOf(noQr) }
        model.load()
        assertTrue(model.state.value is DonationsState.Loaded)
    }

    @Test
    fun `show replaces the mask with the verified number`() = runTest {
        val model = DonationsModel(FakeSource(phone = { "+7 900 000 00 00" }))
        assertSame(PhoneState.Masked, model.phoneState("sbp"))
        model.revealPhone("sbp")
        assertEquals(PhoneState.Shown("+7 900 000 00 00"), model.phoneState("sbp"))
    }

    @Test
    fun `a failed show keeps the mask and can be retried`() = runTest {
        val source = FakeSource(phone = { throw IllegalStateException("id-mismatch: test") })
        val model = DonationsModel(source)
        model.revealPhone("sbp")
        assertSame(PhoneState.Failed, model.phoneState("sbp"))
        source.phone = { "+7 900 000 00 00" }
        model.revealPhone("sbp")
        assertEquals(PhoneState.Shown("+7 900 000 00 00"), model.phoneState("sbp"))
    }

    @Test
    fun `the QR is asked only for a method with a signed digest, and a mismatch shows none`() = runTest {
        val source = FakeSource(qr = byteArrayOf(1, 2))
        val model = DonationsModel(source)
        assertNull(model.qr(noQr))
        assertEquals(0, source.qrCalls)
        assertArrayEquals(byteArrayOf(1, 2), model.qr(sbp))
        source.qr = null
        assertNull(model.qr(sbp))
        assertTrue(model.phoneState("sbp") is PhoneState.Masked)
    }
}
