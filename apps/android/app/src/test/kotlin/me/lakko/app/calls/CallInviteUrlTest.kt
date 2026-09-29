// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.discovery.TEST_INVITE_LINK_LENGTHS
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for the call-invite URL plumbing. Run
 * as `:app:testDebugUnitTest` — no emulator, no UniFFI native
 * library load required: every helper takes the short-link
 * widths as a parameter, and these tests pass
 * [TEST_INVITE_LINK_LENGTHS] instead of the core's.
 *
 * The sibling test file [me.lakko.app.contacts] covers the
 * `/i/` (contact-invite) variant; this one covers the `/c/`
 * (call-invite) variant. The two MUST remain semantically
 * parallel — any change to one is a candidate for porting to
 * the other.
 */
class CallInviteUrlTest {

    private val L = TEST_INVITE_LINK_LENGTHS
    private val BLOB_ID_LEN = L.blobIdLen

    // -----------------------------------------------------------
    // Pointer-URL round-trip
    // -----------------------------------------------------------

    @Test
    fun `pointer URL has the c slash prefix`() {
        val blobId = ByteArray(BLOB_ID_LEN) { it.toByte() }
        val url = composePointerCallInviteUrl(blobId, lengths = L)
        assertTrue(
            "expected URL to contain '/c/' prefix, got: $url",
            url.contains("/c/"),
        )
    }

    @Test
    fun `pointer URL uses the default origin`() {
        val blobId = ByteArray(BLOB_ID_LEN) { 0xAB.toByte() }
        val url = composePointerCallInviteUrl(blobId, lengths = L)
        assertTrue(
            "expected URL to start with default origin, got: $url",
            url.startsWith(CALL_INVITE_DEFAULT_ORIGIN),
        )
    }

    @Test
    fun `pointer URL respects a custom origin`() {
        val blobId = ByteArray(BLOB_ID_LEN) { 0xCD.toByte() }
        val url = composePointerCallInviteUrl(blobId, origin = "https://example.test", lengths = L)
        assertTrue(
            "expected URL to start with custom origin, got: $url",
            url.startsWith("https://example.test/c/"),
        )
    }

    @Test
    fun `pointer URL rejects wrong blob id length`() {
        val tooShort = ByteArray(16) { 0x01 }
        val ex = assertThrows(CallInviteUrlException.WrongBlobIdLength::class.java) {
            composePointerCallInviteUrl(tooShort, lengths = L)
        }
        assertEquals(BLOB_ID_LEN, ex.expected)
        assertEquals(16, ex.got)
    }

    // -----------------------------------------------------------
    // Inline-URL round-trip
    // -----------------------------------------------------------

    @Test
    fun `inline URL accepts a capability above the floor`() {
        val bytes = ByteArray(CALL_INVITE_INLINE_MIN_LEN) { 0x42 }
        val url = composeInlineCallInviteUrl(bytes)
        assertTrue(url.contains("/c/"))
    }

    @Test
    fun `inline URL rejects a too-small capability`() {
        val tooSmall = ByteArray(CALL_INVITE_INLINE_MIN_LEN - 1) { 0x42 }
        val ex = assertThrows(CallInviteUrlException.InlineTooSmall::class.java) {
            composeInlineCallInviteUrl(tooSmall)
        }
        assertEquals(CALL_INVITE_INLINE_MIN_LEN, ex.minimum)
        assertEquals(CALL_INVITE_INLINE_MIN_LEN - 1, ex.got)
    }

    // -----------------------------------------------------------
    // parseCallInviteSlug — discrimination on byte length
    // -----------------------------------------------------------

    @Test
    fun `parse recovers a pointer payload`() {
        val blobId = ByteArray(BLOB_ID_LEN) { (it xor 0xA5).toByte() }
        val url = composePointerCallInviteUrl(blobId, lengths = L)
        // Extract the slug after the prefix.
        val slug = url.substringAfter("/c/")
        val parsed = parseCallInviteSlug(slug, L)
        assertTrue(
            "expected Pointer, got ${parsed::class.simpleName}",
            parsed is ParsedCallInvite.Pointer,
        )
        assertEquals(
            ParsedCallInvite.Pointer(blobId),
            parsed,
        )
    }

    @Test
    fun `parse recovers an inline payload`() {
        val bytes = ByteArray(CALL_INVITE_INLINE_MIN_LEN + 50) { 0x33 }
        val url = composeInlineCallInviteUrl(bytes)
        val slug = url.substringAfter("/c/")
        val parsed = parseCallInviteSlug(slug, L)
        assertTrue(
            "expected Inline, got ${parsed::class.simpleName}",
            parsed is ParsedCallInvite.Inline,
        )
        assertEquals(
            ParsedCallInvite.Inline(bytes),
            parsed,
        )
    }

    @Test
    fun `parse rejects a slug of disallowed length`() {
        // Decoded length 100 falls in the dead zone between the
        // 6-byte pointer and the 400-byte inline floor.
        val bytes = ByteArray(100) { 0x77 }
        val slug = me.lakko.app.auth.base64UrlEncodeNoPad(bytes)
        val ex = assertThrows(CallInviteUrlException.SlugWrongLength::class.java) {
            parseCallInviteSlug(slug, L)
        }
        assertEquals(BLOB_ID_LEN, ex.blobIdLen)
        assertEquals(CALL_INVITE_INLINE_MIN_LEN, ex.inlineMinLen)
        assertEquals(100, ex.got)
    }

    @Test
    fun `parse rejects malformed base64`() {
        // `*` is not in the base64url alphabet — the decoder THROWS
        // `IllegalArgumentException`, which the parser catches and
        // re-raises as the typed InvalidBase64Url.
        assertThrows(CallInviteUrlException.InvalidBase64Url::class.java) {
            parseCallInviteSlug("not*valid*base64*url", L)
        }
    }

    // -----------------------------------------------------------
    // Constants pin — drift sentinel against the web side
    // -----------------------------------------------------------

    @Test
    fun `url prefix matches the web side`() {
        // Web: `export const CALL_INVITE_URL_PREFIX = "/c/"`.
        // Drift surfaces here so a one-character typo on the
        // Android side doesn't break universal-link handoff.
        assertEquals("/c/", CALL_INVITE_URL_PREFIX)
    }

    @Test
    fun `a pointer URL carries the 8-character id`() {
        val url = composePointerCallInviteUrl(ByteArray(BLOB_ID_LEN) { 9 }, origin = "https://example.test", lengths = L)
        assertEquals(L.blobIdUrlLen, url.substringAfter("/c/").length)
    }

    @Test
    fun `parse refuses the pre-amendment 43-character 32-byte pointer`() {
        val old = me.lakko.app.auth.base64UrlEncodeNoPad(ByteArray(32) { 5 })
        assertEquals(43, old.length)
        val ex = assertThrows(CallInviteUrlException.SlugWrongLength::class.java) {
            parseCallInviteSlug(old, L)
        }
        assertEquals(32, ex.got)
    }

    @Test
    fun `pointer URL refuses the old 32-byte id`() {
        assertThrows(CallInviteUrlException.WrongBlobIdLength::class.java) {
            composePointerCallInviteUrl(ByteArray(32), lengths = L)
        }
    }

    @Test
    fun `inline minimum is documented at 400`() {
        // Below this, the parser cannot discriminate inline
        // from a malformed pointer URL.
        assertEquals(400, CALL_INVITE_INLINE_MIN_LEN)
    }

    @Test
    fun `auto compose forwards to the pointer URL on the id width`() {
        val blobId = ByteArray(BLOB_ID_LEN) { 0x01 }
        val urlAuto = composeCallInviteUrl(blobId)
        val urlPointer = composePointerCallInviteUrl(blobId, lengths = L)
        assertEquals(urlPointer, urlAuto)
    }

    @Test
    fun `parse pointer payload returned ByteArray is preserved verbatim`() {
        // Equality + hash semantics on the data class above are
        // override-driven (`contentEquals` / `contentHashCode`);
        // the joiner side reads `pointer.blobId` to call
        // `DiscoveryClient.fetchCallInvite(...)` directly, so the
        // bytes MUST be byte-for-byte identical to what we put
        // in. Independent re-encoding round-trip pins this.
        val blobId = ByteArray(BLOB_ID_LEN) { 0xEE.toByte() }
        val parsed = parseCallInviteSlug(
            me.lakko.app.auth.base64UrlEncodeNoPad(blobId),
            L,
        )
        val pointer = parsed as ParsedCallInvite.Pointer
        assertNotNull(pointer.blobId)
        assertEquals(BLOB_ID_LEN, pointer.blobId.size)
        for (i in 0 until BLOB_ID_LEN) {
            assertEquals(
                "byte at index $i must round-trip",
                blobId[i].toInt() and 0xFF,
                pointer.blobId[i].toInt() and 0xFF,
            )
        }
    }
}
