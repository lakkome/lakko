// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-JVM tests for the [RecoveryPayload] codec (ADR-0018 factor
 * (a)) — the pure-Kotlin halves: the 96-byte payload layout and the
 * tagged mailbox-row wrap/unwrap. Byte-compatibility with the web
 * reference (`apps/web/src/devices/recovery-payload.ts`) is pinned
 * by the magic prefix + layout assertions.
 *
 * The seal/open halves ride the JNI-backed `sealWithKey` /
 * `openWithKey` (covered by the Rust core + the web vitest suite);
 * they cannot load on the host JVM, so they are exercised by the
 * (deferred) instrumented suite instead.
 */
public class RecoveryPayloadTest {

    private fun grant(fill: Byte = 0x41): ByteArray =
        ByteArray(RecoveryPayload.RECOVERY_GRANT_TOKEN_LENGTH) { fill }

    private fun seed(fill: Byte = 0x42): ByteArray =
        ByteArray(RecoveryPayload.RECOVERY_PHRASE_SEED_LENGTH) { fill }

    @Test
    public fun buildAndParseRoundTrip() {
        val payload = RecoveryPayload.buildRecoveryPayload(grant(), seed())
        assertEquals(RecoveryPayload.RECOVERY_PAYLOAD_LENGTH, payload.size)
        val parsed = RecoveryPayload.parseRecoveryPayload(payload)
        assertArrayEquals(grant(), parsed.grantToken)
        assertArrayEquals(seed(), parsed.phraseSeed)
    }

    @Test
    public fun payloadLayoutIsGrantThenSeed() {
        // Pin the byte layout (grant(32) then seed(64)) — web compat.
        val payload = RecoveryPayload.buildRecoveryPayload(grant(0x01), seed(0x02))
        assertEquals(96, payload.size)
        assertTrue(payload.take(32).all { it == 0x01.toByte() })
        assertTrue(payload.drop(32).all { it == 0x02.toByte() })
    }

    @Test
    public fun buildRejectsWrongGrantLength() {
        assertThrows(IllegalArgumentException::class.java) {
            RecoveryPayload.buildRecoveryPayload(ByteArray(31), seed())
        }
    }

    @Test
    public fun buildRejectsWrongSeedLength() {
        assertThrows(IllegalArgumentException::class.java) {
            RecoveryPayload.buildRecoveryPayload(grant(), ByteArray(63))
        }
    }

    @Test
    public fun parseRejectsWrongLength() {
        assertThrows(IllegalArgumentException::class.java) {
            RecoveryPayload.parseRecoveryPayload(ByteArray(95))
        }
    }

    @Test
    public fun rowWrapUnwrapRoundTrip() {
        val sealed = ByteArray(124) { (it % 251).toByte() }
        val row = RecoveryPayload.wrapRecoveryRow(sealed)
        assertTrue(RecoveryPayload.isRecoveryPayloadRow(row))
        assertArrayEquals(sealed, RecoveryPayload.unwrapRecoveryRow(row))
    }

    @Test
    public fun rowMagicMatchesWebReference() {
        // The ASCII magic must be byte-identical to the web's
        // RECOVERY_ROW_MAGIC ("lakko/recovery-payload/v1") —
        // cross-platform pairing depends on it.
        val expectedMagic = "lakko/recovery-payload/v1"
            .toByteArray(Charsets.US_ASCII)
        val row = RecoveryPayload.wrapRecoveryRow(ByteArray(1) { 0x7F })
        assertArrayEquals(expectedMagic, row.copyOfRange(0, expectedMagic.size))
        assertEquals(0x7F.toByte(), row[expectedMagic.size])
    }

    @Test
    public fun nonRecoveryRowsAreNotMisclassified() {
        // A Welcome / history-bundle-shaped row must not be taken
        // for a recovery row.
        assertFalse(RecoveryPayload.isRecoveryPayloadRow(ByteArray(64) { 0x00 }))
        assertNull(RecoveryPayload.unwrapRecoveryRow(ByteArray(64) { 0x00 }))
        // A bare magic with no sealed blob is refused too.
        val bareMagic = "lakko/recovery-payload/v1".toByteArray(Charsets.US_ASCII)
        assertFalse(RecoveryPayload.isRecoveryPayloadRow(bareMagic))
    }
}
