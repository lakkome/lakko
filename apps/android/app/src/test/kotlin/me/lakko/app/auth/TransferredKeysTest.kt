// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * What the receiving device does with the entropy of recovery payload v2
 * before anything is checked or written. The core's expansion (entropy to
 * phrase and seed) is a fake here.
 */
class TransferredKeysTest {

    private fun expand(entropy: ByteArray): Pair<String, ByteArray> =
        "twelve words" to ByteArray(ACCOUNT_IDENTITY_SEED_LENGTH) { entropy[it % entropy.size] }

    @Test
    fun `sixteen bytes become the phrase and the seed, and are consumed`() {
        val entropy = ByteArray(16) { (it + 1).toByte() }
        val keys = keysFromTransferredEntropy(entropy, ::expand)
        assertEquals("twelve words", keys.phrase)
        assertEquals(64, keys.seed.size)
        assertTrue(keys.seed.any { it != 0.toByte() })
        assertTrue("the transferred entropy is zeroed", entropy.all { it == 0.toByte() })
    }

    @Test
    fun `a version one seed where the entropy belongs is refused and zeroed`() {
        var expanded = false
        for (length in listOf(0, 15, 17, 32, 64)) {
            val bytes = ByteArray(length) { 7 }
            assertThrows(IllegalArgumentException::class.java) {
                keysFromTransferredEntropy(bytes) {
                    expanded = true
                    expand(it)
                }
            }
            assertTrue(bytes.all { it == 0.toByte() })
        }
        assertFalse("nothing is expanded for a wrong length", expanded)
    }

    @Test
    fun `a seed of the wrong length from the core is refused and zeroed`() {
        val entropy = ByteArray(16) { 3 }
        val shortSeed = ByteArray(32) { 9 }
        assertThrows(IllegalStateException::class.java) {
            keysFromTransferredEntropy(entropy) { "p" to shortSeed }
        }
        assertTrue(shortSeed.all { it == 0.toByte() })
        assertTrue(entropy.all { it == 0.toByte() })
    }

    @Test
    fun `entropy the core cannot expand is still consumed`() {
        val entropy = ByteArray(16) { 3 }
        assertThrows(IllegalStateException::class.java) {
            keysFromTransferredEntropy(entropy) { throw IllegalStateException("core refused") }
        }
        assertTrue(entropy.all { it == 0.toByte() })
    }
}
