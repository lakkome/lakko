// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import me.lakko.app.auth.OwnPredecessor
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * G-56, recovery payload v3: what the app does around the core's codec (the
 * codec itself, its golden vector and every refusal are pinned through the
 * FFI in `RecoveryPayloadV3InstrumentedTest` and in `lakko-mnemonic`). A fake
 * codec stands in for the native library.
 */
class RecoveryRowTest {

    /** Records what it was handed; refuses a row whose first byte is 2 the
     *  way the core refuses a v2 row. */
    private class FakeCodec : RecoveryRow.Codec {
        var composedWith: List<Pair<ByteArray, Long>> = emptyList()
        var decodedPlaintext: ByteArray? = null

        override fun exporterLabel(): String = "lakko/device-recovery/v3"

        override fun exporterKeyLen(): UInt = 32u

        override fun compose(
            grant: ByteArray,
            phraseEntropy: ByteArray,
            predecessors: List<StateBundle.OwnPredecessorSeed>,
        ): ByteArray {
            composedWith = predecessors.map { it.seed.copyOf() to it.adoptedAtMs }
            return grant + phraseEntropy
        }

        override fun decode(plaintext: ByteArray): RecoveryRow.Parsed {
            decodedPlaintext = plaintext
            return RecoveryRow.Parsed(plaintext.copyOfRange(0, 32), plaintext.copyOfRange(32, 48), emptyList())
        }

        override fun wrap(sealed: ByteArray): ByteArray = byteArrayOf(3) + sealed

        override fun unwrap(row: ByteArray): ByteArray? = when (row.firstOrNull()) {
            3.toByte() -> row.copyOfRange(1, row.size)
            2.toByte() -> throw RecoveryRow.Refused()
            else -> null
        }
    }

    private fun predecessor(tag: Int, at: Long) =
        OwnPredecessor(ByteArray(32) { tag.toByte() }, ByteArray(64) { tag.toByte() }, at)

    @Test
    fun `the confirming side carries the own predecessors oldest first with their adoption times`() {
        val codec = FakeCodec()
        val rows = listOf(predecessor(3, 1_700_000_000_000L), predecessor(4, 1_700_000_100_000L))
        RecoveryRow.compose(ByteArray(32) { 1 }, ByteArray(16) { 2 }, rows, codec)
        assertEquals(listOf<Byte>(3, 4), codec.composedWith.map { it.first[0] })
        assertEquals(listOf(1_700_000_000_000L, 1_700_000_100_000L), codec.composedWith.map { it.second })
        // The store's own rows are the caller's; only the copies handed to
        // the codec are zeroed here.
        assertTrue(rows.all { row -> row.phraseSeed.all { it != 0.toByte() } })
    }

    @Test
    fun `no earlier keys sends count zero, never a refusal`() {
        val codec = FakeCodec()
        RecoveryRow.compose(ByteArray(32), ByteArray(16), emptyList(), codec)
        assertTrue(codec.composedWith.isEmpty())
    }

    @Test
    fun `a row of another version is still a recovery row, so the other polls skip it`() {
        val codec = FakeCodec()
        assertTrue(RecoveryRow.isRecoveryRow(byteArrayOf(3, 9), codec))
        assertTrue(RecoveryRow.isRecoveryRow(byteArrayOf(2, 9), codec))
        assertFalse(RecoveryRow.isRecoveryRow(byteArrayOf(5, 9), codec))
    }

    @Test
    fun `the opened plaintext is zeroed once parsed`() {
        val codec = FakeCodec()
        val parsed = RecoveryRow.open(ByteArray(32), byteArrayOf(0), { _, _ -> ByteArray(48) { 7 } }, codec)
        assertTrue(codec.decodedPlaintext!!.all { it == 0.toByte() })
        assertArrayEquals(ByteArray(32) { 7 }, parsed.grantToken)
        parsed.zero()
        assertTrue(parsed.grantToken.all { it == 0.toByte() })
        assertTrue(parsed.phraseEntropy.all { it == 0.toByte() })
    }

    @Test
    fun `parsed secrets do not show in a log line`() {
        val parsed = RecoveryRow.Parsed(
            ByteArray(32) { 1 },
            ByteArray(16) { 2 },
            listOf(StateBundle.OwnPredecessorSeed(ByteArray(64) { 5 }, 1L)),
        )
        assertEquals("RecoveryRow.Parsed(<redacted>, 1 earlier keys)", parsed.toString())
    }
}
