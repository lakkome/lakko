// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.webauthn

import me.lakko.app.auth.base64Decode
import me.lakko.app.auth.base64Encode
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * The account id on the passkey wire: `services/auth` takes and returns it
 * as URL-safe base64 without padding, while Android holds account ids as
 * standard base64 with padding. An id whose two spellings differ must still
 * name one account on both sides.
 */
class PasskeyAccountIdTest {

    // 0xFB 0xFF spells `+/` in the standard alphabet and `-_` in the URL-safe
    // one; 16 bytes pad to `==` in the standard form.
    private val id = ByteArray(16) { if (it % 2 == 0) 0xFB.toByte() else 0xFF.toByte() }

    @Test
    fun `the two spellings of the test id differ`() {
        val standard = base64Encode(id)
        val wire = passkeyStartAccountId(id)
        assertNotEquals(standard, wire)
        assertEquals(true, standard.contains('+') && standard.contains('/') && standard.endsWith("=="))
        assertEquals(true, wire.contains('-') && wire.contains('_') && !wire.contains('='))
    }

    @Test
    fun `the start body carries the server's spelling`() {
        assertEquals(
            java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(id),
            passkeyStartAccountId(id),
        )
    }

    @Test
    fun `the finish value decodes to the bytes the standard form names`() {
        val wire = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(id)
        val bytes = passkeyFinishAccountId(wire)
        assertArrayEquals(id, bytes)
        // The account Android keys its stores by is the same account.
        assertArrayEquals(base64Decode(base64Encode(id)), bytes)
        assertEquals(base64Encode(id), base64Encode(bytes!!))
    }

    @Test
    fun `a value that is not the server's form or not 16 bytes is refused`() {
        assertNull(passkeyFinishAccountId(base64Encode(id)))
        assertNull(passkeyFinishAccountId(passkeyStartAccountId(ByteArray(15))))
        assertNull(passkeyFinishAccountId("not base64!"))
    }
}
