// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import androidx.test.ext.junit.runners.AndroidJUnit4
import java.security.MessageDigest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.FfiRecoveryPredecessor
import uniffi.lakko_ffi_uniffi.composeRecoveryPayload
import uniffi.lakko_ffi_uniffi.decodeRecoveryPayload
import uniffi.lakko_ffi_uniffi.recoveryExporterKeyLen
import uniffi.lakko_ffi_uniffi.recoveryExporterLabel
import uniffi.lakko_ffi_uniffi.recoveryPredecessorsMax
import uniffi.lakko_ffi_uniffi.unwrapRecoveryRow
import uniffi.lakko_ffi_uniffi.wrapRecoveryRow

/**
 * G-56, recovery payload v3, against `liblakko_ffi_uniffi.so`: the golden
 * plaintext of the contract (193 bytes, SHA-256 `43260742...6a41`), the
 * labels, and every refusal. The host suite ([RecoveryRowTest]) cannot load
 * the native library.
 */
@RunWith(AndroidJUnit4::class)
class RecoveryPayloadV3InstrumentedTest {

    private fun hex(bytes: ByteArray) = bytes.joinToString("") { "%02x".format(it) }

    private fun golden(): ByteArray = composeRecoveryPayload(
        ByteArray(32) { 0x01 },
        ByteArray(16) { 0x02 },
        listOf(
            FfiRecoveryPredecessor(ByteArray(64) { 0x03 }, 1_700_000_000_000uL),
            FfiRecoveryPredecessor(ByteArray(64) { 0x04 }, 1_700_000_100_000uL),
        ),
    )

    @Test
    fun theGoldenVectorIsPinned() {
        val plaintext = golden()
        assertEquals(193, plaintext.size)
        assertEquals(
            "43260742d3c1e705179d7ac8baf1f0566b5fd9621eb1df08bb9a48e34dd56a41",
            hex(MessageDigest.getInstance("SHA-256").digest(plaintext)),
        )
        val decoded = decodeRecoveryPayload(plaintext)
        assertArrayEquals(ByteArray(32) { 0x01 }, decoded.grant)
        assertArrayEquals(ByteArray(16) { 0x02 }, decoded.phraseEntropy)
        assertEquals(listOf(1_700_000_000_000uL, 1_700_000_100_000uL), decoded.predecessors.map { it.adoptedAtMs })
        assertArrayEquals(ByteArray(64) { 0x03 }, decoded.predecessors[0].seed)
    }

    @Test
    fun theLabelsAreV3() {
        assertEquals("lakko/device-recovery/v3", recoveryExporterLabel())
        assertEquals(32u, recoveryExporterKeyLen())
        assertEquals(8u, recoveryPredecessorsMax())
        val row = wrapRecoveryRow(byteArrayOf(9, 9))
        assertTrue(String(row, Charsets.US_ASCII).startsWith("lakko/recovery-payload/v3"))
        assertArrayEquals(byteArrayOf(9, 9), unwrapRecoveryRow(row))
        assertNull(unwrapRecoveryRow("not a recovery row".toByteArray()))
    }

    private fun refused(code: String, block: () -> Unit) {
        try {
            block()
            fail("expected $code")
        } catch (e: FfiException.RecoveryPayload) {
            assertTrue(e.message.orEmpty(), e.message.orEmpty().startsWith(code))
        }
    }

    @Test
    fun everyRefusal() {
        val plaintext = golden()
        refused("bad-length") { decodeRecoveryPayload(plaintext.copyOf(plaintext.size - 1)) }
        // A v2 payload (48 bytes) is refused as a length.
        refused("bad-length") { decodeRecoveryPayload(ByteArray(48)) }
        // The length is bounded first (at most 49 + 72 x 8), then the count.
        refused("bad-length") { decodeRecoveryPayload(plaintext.copyOf(49 + 72 * 9).also { it[48] = 9 }) }
        refused("too-many-predecessors") { decodeRecoveryPayload(plaintext.copyOf().also { it[48] = 9 }) }
        val twice = plaintext.copyOf().also { System.arraycopy(it, 49, it, 49 + 72, 64) }
        refused("duplicate-predecessor") { decodeRecoveryPayload(twice) }
        val currentSeed = uniffi.lakko_ffi_uniffi.RecoveryMnemonic.fromEntropy(ByteArray(16) { 0x02 }).use { it.toSeed() }
        refused("current-key-predecessor") {
            composeRecoveryPayload(
                ByteArray(32),
                ByteArray(16) { 0x02 },
                listOf(FfiRecoveryPredecessor(currentSeed, 1uL)),
            )
        }
        val v2Row = "lakko/recovery-payload/v2".toByteArray(Charsets.US_ASCII) + byteArrayOf(1, 2, 3)
        refused("unsupported-row") { unwrapRecoveryRow(v2Row) }
    }
}
