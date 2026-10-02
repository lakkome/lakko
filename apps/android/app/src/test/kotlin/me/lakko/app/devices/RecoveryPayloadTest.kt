// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import me.lakko.app.auth.keysFromTransferredEntropy
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-JVM tests for the [RecoveryPayload] codec, version 2: the 48-byte
 * payload (`grant(32) || phrase_entropy(16)`), the tagged mailbox row, and
 * the refusal of version 1 in every place it could slip through.
 * Byte-compatibility with the web reference
 * (`apps/web/src/devices/recovery-payload.ts`) is pinned by the magic, the
 * exporter label and the layout assertions.
 *
 * The real seal (`sealWithKey` / `openWithKey`, ChaCha20-Poly1305) is in the
 * native core and does not load on the host JVM. The whole sender-to-receiver
 * path is therefore driven here with a stand-in cipher of the same wire
 * shape (`nonce(12) || ciphertext || tag(16)`), which is enough to pin
 * everything this client owns: what goes into the seal, what the row looks
 * like, what the receiver accepts.
 */
public class RecoveryPayloadTest {

    private fun grant(fill: Byte = 0x41): ByteArray =
        ByteArray(RecoveryPayload.RECOVERY_GRANT_TOKEN_LENGTH) { fill }

    private fun entropy(fill: Byte = 0x42): ByteArray =
        ByteArray(RecoveryPayload.RECOVERY_PHRASE_ENTROPY_LENGTH) { fill }

    // ---- a stand-in for the core's seal --------------------------------

    private val key = ByteArray(32) { (it * 7 + 3).toByte() }

    private fun fakeSeal(key: ByteArray, plaintext: ByteArray): ByteArray {
        val nonce = ByteArray(12) { (0x10 + it).toByte() }
        val body = ByteArray(plaintext.size) { (plaintext[it].toInt() xor key[it % key.size].toInt() xor 0x5A).toByte() }
        val tag = ByteArray(16) { key[it] }
        return nonce + body + tag
    }

    private fun fakeOpen(key: ByteArray, sealed: ByteArray): ByteArray {
        require(sealed.size >= 28) { "sealed blob too short" }
        val tag = sealed.copyOfRange(sealed.size - 16, sealed.size)
        require(tag.contentEquals(ByteArray(16) { key[it] })) { "wrong key" }
        val body = sealed.copyOfRange(12, sealed.size - 16)
        return ByteArray(body.size) { (body[it].toInt() xor key[it % key.size].toInt() xor 0x5A).toByte() }
    }

    // ---- payload ---------------------------------------------------------

    @Test
    public fun buildAndParseRoundTrip() {
        val payload = RecoveryPayload.buildRecoveryPayload(grant(), entropy())
        assertEquals(RecoveryPayload.RECOVERY_PAYLOAD_LENGTH, payload.size)
        val parsed = RecoveryPayload.parseRecoveryPayload(payload)
        assertArrayEquals(grant(), parsed.grantToken)
        assertArrayEquals(entropy(), parsed.phraseEntropy)
    }

    @Test
    public fun payloadIsFortyEightBytesGrantThenEntropy() {
        val payload = RecoveryPayload.buildRecoveryPayload(grant(0x01), entropy(0x02))
        assertEquals(48, payload.size)
        assertEquals(48, RecoveryPayload.RECOVERY_PAYLOAD_LENGTH)
        assertEquals(16, RecoveryPayload.RECOVERY_PHRASE_ENTROPY_LENGTH)
        assertTrue(payload.take(32).all { it == 0x01.toByte() })
        assertTrue(payload.drop(32).all { it == 0x02.toByte() })
    }

    @Test
    public fun buildRejectsWrongGrantLength() {
        assertThrows(IllegalArgumentException::class.java) {
            RecoveryPayload.buildRecoveryPayload(ByteArray(31), entropy())
        }
    }

    @Test
    public fun buildRejectsASeedWhereTheEntropyBelongs() {
        // The v1 payload carried the 64-byte seed here.
        assertThrows(IllegalArgumentException::class.java) {
            RecoveryPayload.buildRecoveryPayload(grant(), ByteArray(64))
        }
        assertThrows(IllegalArgumentException::class.java) {
            RecoveryPayload.buildRecoveryPayload(grant(), ByteArray(15))
        }
    }

    @Test
    public fun parseRefusesTheVersionOnePayloadAndEveryOtherLength() {
        // v1 was grant(32) || seed(64) = 96 bytes. No dual accept.
        assertThrows(IllegalArgumentException::class.java) {
            RecoveryPayload.parseRecoveryPayload(ByteArray(96))
        }
        for (length in listOf(0, 47, 49, 64)) {
            assertThrows(IllegalArgumentException::class.java) {
                RecoveryPayload.parseRecoveryPayload(ByteArray(length))
            }
        }
    }

    // ---- wire constants ---------------------------------------------------

    @Test
    public fun exporterLabelIsVersionTwo() {
        assertEquals("lakko/device-recovery/v2", RecoveryPayload.RECOVERY_EXPORTER_LABEL)
        assertEquals(32, RecoveryPayload.RECOVERY_EXPORTER_KEY_LENGTH)
    }

    @Test
    public fun rowMagicMatchesWebReference() {
        val expectedMagic = "lakko/recovery-payload/v2".toByteArray(Charsets.US_ASCII)
        val row = RecoveryPayload.wrapRecoveryRow(ByteArray(1) { 0x7F })
        assertArrayEquals(expectedMagic, row.copyOfRange(0, expectedMagic.size))
        assertEquals(0x7F.toByte(), row[expectedMagic.size])
    }

    @Test
    public fun aVersionOneRowIsNotARecoveryRow() {
        val v1 = "lakko/recovery-payload/v1".toByteArray(Charsets.US_ASCII) + ByteArray(124) { 9 }
        assertFalse(RecoveryPayload.isRecoveryPayloadRow(v1))
        assertNull(RecoveryPayload.unwrapRecoveryRow(v1))
    }

    @Test
    public fun rowWrapUnwrapRoundTrip() {
        val sealed = ByteArray(76) { (it % 251).toByte() }
        val row = RecoveryPayload.wrapRecoveryRow(sealed)
        assertTrue(RecoveryPayload.isRecoveryPayloadRow(row))
        assertArrayEquals(sealed, RecoveryPayload.unwrapRecoveryRow(row))
    }

    @Test
    public fun nonRecoveryRowsAreNotMisclassified() {
        // A Welcome / history-bundle-shaped row must not be taken
        // for a recovery row.
        assertFalse(RecoveryPayload.isRecoveryPayloadRow(ByteArray(64) { 0x00 }))
        assertNull(RecoveryPayload.unwrapRecoveryRow(ByteArray(64) { 0x00 }))
        // A bare magic with no sealed blob is refused too.
        val bareMagic = "lakko/recovery-payload/v2".toByteArray(Charsets.US_ASCII)
        assertFalse(RecoveryPayload.isRecoveryPayloadRow(bareMagic))
    }

    // ---- sender to receiver, the seal faked -------------------------------

    @Test
    public fun theSenderRowOpensIntoTheGrantAndThePhraseOnTheReceiver() {
        val sentEntropy = ByteArray(16) { (0xA0 + it).toByte() }
        val row = RecoveryPayload.wrapRecoveryRow(
            fakeSeal(key, RecoveryPayload.buildRecoveryPayload(grant(0x33), sentEntropy)),
        )

        // The published row never carries the entropy in the clear.
        assertFalse(containsSubsequence(row, sentEntropy))

        val sealed = RecoveryPayload.unwrapRecoveryRow(row)!!
        val parsed = RecoveryPayload.parseRecoveryPayload(fakeOpen(key, sealed))
        assertArrayEquals(grant(0x33), parsed.grantToken)
        assertArrayEquals(sentEntropy, parsed.phraseEntropy)

        // The receiver expands the entropy into the phrase and the seed.
        val received = parsed.phraseEntropy
        val keys = keysFromTransferredEntropy(received) { e ->
            "phrase-of-${e.size}-bytes" to ByteArray(64) { e[it % e.size] }
        }
        assertEquals("phrase-of-16-bytes", keys.phrase)
        assertEquals(64, keys.seed.size)
        // The transferred entropy is consumed.
        assertTrue(received.all { it == 0.toByte() })
    }

    @Test
    public fun aVersionOnePayloadSealedUnderTheRightKeyIsStillRefused() {
        // Even with the row magic and the key right, a 96-byte plaintext
        // (grant || 64-byte seed) does not parse.
        val v1Plaintext = grant() + ByteArray(64) { 0x55 }
        val row = RecoveryPayload.wrapRecoveryRow(fakeSeal(key, v1Plaintext))
        val opened = fakeOpen(key, RecoveryPayload.unwrapRecoveryRow(row)!!)
        assertThrows(IllegalArgumentException::class.java) {
            RecoveryPayload.parseRecoveryPayload(opened)
        }
    }

    private fun containsSubsequence(haystack: ByteArray, needle: ByteArray): Boolean {
        if (needle.isEmpty() || haystack.size < needle.size) return false
        for (start in 0..haystack.size - needle.size) {
            var match = true
            for (i in needle.indices) {
                if (haystack[start + i] != needle[i]) {
                    match = false
                    break
                }
            }
            if (match) return true
        }
        return false
    }
}
