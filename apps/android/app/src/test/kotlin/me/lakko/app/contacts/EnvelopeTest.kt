// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for the 1-byte mailbox-envelope
 * discriminator helpers.
 *
 * Pure Kotlin — no native lib loaded, runs under
 * `:app:testDebugUnitTest`. The wire format is shared with the
 * web side (`apps/web/src/contacts/envelope.ts`), so the
 * properties below are the contract both clients must agree on:
 *
 *   1. **Wrap prepends exactly one byte** — no length prefix, no
 *      MAC.
 *   2. **Parse dispatches on the leading byte** + returns a
 *      fresh body copy.
 *   3. **Unknown kinds surface as a discriminated value**
 *      (`Unknown(rawKind, body)`) rather than throwing — the
 *      poll loop on the receiver wants to skip-and-log, not
 *      crash on a future kind byte.
 *   4. **Empty input is "unknown" with `rawKind = -1`** — the
 *      DS doesn't lower-bound payload size on read, and the
 *      receiver wants this case to be a no-op.
 */
class EnvelopeTest {

    @Test
    fun `wrap prepends the kind byte verbatim`() {
        val body = byteArrayOf(0x10, 0x20, 0x30)
        val wrapped = wrapEnvelope(KIND_CONTACT_REQUEST, body)
        assertEquals(4, wrapped.size)
        assertEquals(KIND_CONTACT_REQUEST, wrapped[0])
        assertArrayEquals(body, wrapped.copyOfRange(1, wrapped.size))
    }

    @Test
    fun `wrap with empty body emits a single kind byte`() {
        val wrapped = wrapEnvelope(KIND_WELCOME, ByteArray(0))
        assertEquals(1, wrapped.size)
        assertEquals(KIND_WELCOME, wrapped[0])
    }

    @Test
    fun `wrap rejects an unknown kind byte`() {
        assertThrows(IllegalArgumentException::class.java) {
            wrapEnvelope(0x42, byteArrayOf(0x00))
        }
    }

    // ===================================================================
    // parse dispatch
    // ===================================================================

    @Test
    fun `parse dispatches KIND_CONTACT_REQUEST`() {
        val body = byteArrayOf(1, 2, 3, 4)
        val wrapped = wrapEnvelope(KIND_CONTACT_REQUEST, body)
        val parsed = parseEnvelope(wrapped)
        assertTrue(parsed is ParsedEnvelope.ContactRequest)
        assertArrayEquals(body, (parsed as ParsedEnvelope.ContactRequest).body)
    }

    @Test
    fun `parse dispatches KIND_WELCOME`() {
        val body = byteArrayOf(7, 8, 9)
        val wrapped = wrapEnvelope(KIND_WELCOME, body)
        val parsed = parseEnvelope(wrapped)
        assertTrue(parsed is ParsedEnvelope.Welcome)
        assertArrayEquals(body, (parsed as ParsedEnvelope.Welcome).body)
    }

    @Test
    fun `parse returns Unknown with rawKind for an unknown kind byte`() {
        val bytes = byteArrayOf(0x42, 0x00, 0xFF.toByte())
        val parsed = parseEnvelope(bytes)
        assertTrue(parsed is ParsedEnvelope.Unknown)
        parsed as ParsedEnvelope.Unknown
        assertEquals(0x42, parsed.rawKind)
        assertArrayEquals(byteArrayOf(0x00, 0xFF.toByte()), parsed.body)
    }

    @Test
    fun `parse handles 0xFF kind byte without sign extension`() {
        // 0xFF as a Byte is -1; the raw-kind output must stay
        // 0xFF (255) — sign-extension would surface as -1 and
        // collide with the empty-input sentinel.
        val bytes = byteArrayOf(0xFF.toByte(), 0x42)
        val parsed = parseEnvelope(bytes)
        assertTrue(parsed is ParsedEnvelope.Unknown)
        assertEquals(0xFF, (parsed as ParsedEnvelope.Unknown).rawKind)
    }

    @Test
    fun `parse on empty input returns Unknown with rawKind minus one`() {
        val parsed = parseEnvelope(ByteArray(0))
        assertTrue(parsed is ParsedEnvelope.Unknown)
        assertEquals(-1, (parsed as ParsedEnvelope.Unknown).rawKind)
        assertEquals(0, parsed.body.size)
    }

    @Test
    fun `wrap then parse round-trips both known kinds`() {
        for (kind in listOf(KIND_CONTACT_REQUEST, KIND_WELCOME)) {
            val body = ByteArray(64) { it.toByte() }
            val parsed = parseEnvelope(wrapEnvelope(kind, body))
            when (kind) {
                KIND_CONTACT_REQUEST -> {
                    assertTrue(parsed is ParsedEnvelope.ContactRequest)
                    assertArrayEquals(body, (parsed as ParsedEnvelope.ContactRequest).body)
                }
                KIND_WELCOME -> {
                    assertTrue(parsed is ParsedEnvelope.Welcome)
                    assertArrayEquals(body, (parsed as ParsedEnvelope.Welcome).body)
                }
            }
        }
    }
}
