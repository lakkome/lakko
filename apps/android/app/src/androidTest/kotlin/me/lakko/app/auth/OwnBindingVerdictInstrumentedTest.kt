// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.lakko_ffi_uniffi.FfiBindingVerdict
import uniffi.lakko_ffi_uniffi.FfiOwnLink
import uniffi.lakko_ffi_uniffi.ownBindingVerdict

/**
 * G-56: the shared truth table of the contract (section 2.1), all 17 vectors,
 * through the core's `ownBindingVerdict` on `liblakko_ffi_uniffi.so`. The host
 * suite ([EarlierKeysTest]) watches the gate's ordering around it.
 */
@RunWith(AndroidJUnit4::class)
class OwnBindingVerdictInstrumentedTest {

    private fun key(tag: Int) = ByteArray(32) { tag.toByte() }

    private val cur = key(9)
    private val k0 = key(1)
    private val k1 = key(2)
    private val foreign = key(7)

    private sealed interface Expected {
        data object Keep : Expected
        data object KeepUnderCurrentKey : Expected
        data object Wait : Expected
        class Mint(val key: ByteArray) : Expected
    }

    private class Vector(
        val bound: ByteArray?,
        val preds: List<ByteArray>,
        val link: FfiOwnLink,
        val conv: Boolean,
        val expected: Expected,
    )

    private val absent = FfiOwnLink.Absent
    private val unknown = FfiOwnLink.Unknown
    private fun keyLink(k: ByteArray) = FfiOwnLink.Key(k)

    private val vectors = listOf(
        Vector(null, listOf(), absent, false, Expected.Mint(cur)),
        Vector(null, listOf(k0), absent, false, Expected.Mint(k0)),
        Vector(null, listOf(k0), keyLink(k0), false, Expected.Mint(k0)),
        Vector(null, listOf(), keyLink(k0), false, Expected.Wait),
        Vector(null, listOf(k1), keyLink(k0), false, Expected.Wait),
        Vector(null, listOf(k0, k1), keyLink(k0), false, Expected.Mint(k0)),
        Vector(null, listOf(), unknown, false, Expected.Wait),
        Vector(cur, listOf(), absent, false, Expected.Keep),
        Vector(cur, listOf(), keyLink(k0), false, Expected.Wait),
        Vector(cur, listOf(k0), keyLink(k0), false, Expected.Mint(k0)),
        Vector(cur, listOf(k0), keyLink(k0), true, Expected.KeepUnderCurrentKey),
        Vector(cur, listOf(), keyLink(k0), true, Expected.KeepUnderCurrentKey),
        Vector(k0, listOf(k0), keyLink(k0), true, Expected.Keep),
        Vector(k1, listOf(k0, k1), keyLink(k0), true, Expected.Keep),
        Vector(foreign, listOf(), absent, false, Expected.Mint(cur)),
        Vector(foreign, listOf(), keyLink(k0), false, Expected.Wait),
        Vector(foreign, listOf(k0), unknown, false, Expected.Wait),
    )

    @Test
    fun theSeventeenSharedVectors() {
        assertEquals(17, vectors.size)
        vectors.forEachIndexed { index, v ->
            val got = ownBindingVerdict(v.bound, cur, v.preds, v.link, v.conv)
            val what = "vector ${index + 1}"
            when (val e = v.expected) {
                Expected.Keep -> assertEquals(what, FfiBindingVerdict.Keep, got)
                Expected.KeepUnderCurrentKey -> assertEquals(what, FfiBindingVerdict.KeepUnderCurrentKey, got)
                Expected.Wait -> assertEquals(what, FfiBindingVerdict.Wait, got)
                is Expected.Mint -> {
                    assertTrue(what, got is FfiBindingVerdict.Mint)
                    assertArrayEquals(what, e.key, (got as FfiBindingVerdict.Mint).key)
                }
            }
        }
    }
}
