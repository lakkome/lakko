// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** Host-JVM tests for [normalizeTotpCode] (ADR-0018 factor (c)) —
 *  the client-side mirror of the server's `parse_code` gate. */
public class TotpCodeTest {

    @Test
    public fun acceptsExactlySixAsciiDigits() {
        assertEquals("123456", normalizeTotpCode("123456"))
        assertEquals("000000", normalizeTotpCode("000000"))
    }

    @Test
    public fun trimsSurroundingWhitespace() {
        assertEquals("123456", normalizeTotpCode("  123456  "))
        assertEquals("123456", normalizeTotpCode("\t123456\n"))
    }

    @Test
    public fun rejectsWrongLength() {
        assertNull(normalizeTotpCode("12345"))
        assertNull(normalizeTotpCode("1234567"))
        assertNull(normalizeTotpCode(""))
    }

    @Test
    public fun rejectsNonDigits() {
        assertNull(normalizeTotpCode("12345a"))
        assertNull(normalizeTotpCode("12 456"))
        assertNull(normalizeTotpCode("12-456"))
    }

    @Test
    public fun rejectsNonAsciiNumerals() {
        // Arabic-Indic digits pass Kotlin's Char.isDigit() but must be
        // refused (the server only accepts is_ascii_digit).
        assertNull(normalizeTotpCode("١٢٣٤٥٦"))
    }
}
