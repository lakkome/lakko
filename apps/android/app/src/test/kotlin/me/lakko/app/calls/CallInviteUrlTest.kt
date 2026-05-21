// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for the call-invite URL plumbing. Run
 * as `:app:testDebugUnitTest` — no emulator, no UniFFI native
 * library load required (the helpers operate on plain
 * `ByteArray` / `String` without touching the FFI).
 *
 * The sibling test file [me.lakko.app.contacts] covers the
 * `/i/` (contact-invite) variant; this one covers the `/c/`
 * (call-invite) variant. The two MUST remain semantically
 * parallel — any change to one is a candidate for porting to
 * the other.
 */
class CallInviteUrlTest {

    // -----------------------------------------------------------
    // Pointer-URL round-trip
    // -----------------------------------------------------------

    @Test
    fun `pointer URL has the c slash prefix`() {
        val blobId = ByteArray(CALL_INVITE_BLOB_ID_LEN) { it.toByte() }
        val url = composePointerCallInviteUrl(blobId)
        assertTrue(
            "expected URL to contain '/c/' prefix, got: $url",
            url.contains("/c/"),
        )
    }

    @Test
    fun `pointer URL uses the default origin`() {
        val blobId = ByteArray(CALL_INVITE_BLOB_ID_LEN) { 0xAB.toByte() }
        val url = composePointerCallInviteUrl(blobId)
        assertTrue(
            "expected URL to start with default origin, got: $url",
            url.startsWith(CALL_INVITE_DEFAULT_ORIGIN),
        )
    }

    @Test
    fun `pointer URL respects a custom origin`() {
        val blobId = ByteArray(CALL_INVITE_BLOB_ID_LEN) { 0xCD.toByte() }
        val url = composePointerCallInviteUrl(blobId, origin = "https://example.test")
        assertTrue(
            "expected URL to start with custom origin, got: $url",
            url.startsWith("https://example.test/c/"),
        )
    }

    @Test
    fun `pointer URL rejects wrong blob id length`() {
        val tooShort = ByteArray(16) { 0x01 }
        val ex = assertThrows(CallInviteUrlException.WrongBlobIdLength::class.java) {
            composePointerCallInviteUrl(tooShort)
        }
        assertEquals(CALL_INVITE_BLOB_ID_LEN, ex.expected)
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
        val blobId = ByteArray(CALL_INVITE_BLOB_ID_LEN) { (it xor 0xA5).toByte() }
        val url = composePointerCallInviteUrl(blobId)
        // Extract the slug after the prefix.
        val slug = url.substringAfter("/c/")
        val parsed = parseCallInviteSlug(slug)
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
        val parsed = parseCallInviteSlug(slug)
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
        // 32-byte pointer and the 400-byte inline floor.
        val bytes = ByteArray(100) { 0x77 }
        val slug = me.lakko.app.auth.base64UrlEncodeNoPad(bytes)
        val ex = assertThrows(CallInviteUrlException.SlugWrongLength::class.java) {
            parseCallInviteSlug(slug)
        }
        assertEquals(CALL_INVITE_BLOB_ID_LEN, ex.blobIdLen)
        assertEquals(CALL_INVITE_INLINE_MIN_LEN, ex.inlineMinLen)
        assertEquals(100, ex.got)
    }

    @Test
    fun `parse rejects malformed base64`() {
        // `*` is not in the base64url alphabet — decoder returns
        // null which the parser maps to InvalidBase64Url.
        assertThrows(CallInviteUrlException.InvalidBase64Url::class.java) {
            parseCallInviteSlug("not*valid*base64*url")
        }
    }

    // -----------------------------------------------------------
    // Form-side enums — the wire-value mapping the composer relies on
    // -----------------------------------------------------------

    @Test
    fun `expiry preset never resolves to zero sentinel`() {
        assertEquals(0L, CallInviteExpiryPreset.Never.toMs(1_700_000_000_000L))
    }

    @Test
    fun `expiry preset 7d resolves to now plus seven days`() {
        val now = 1_700_000_000_000L
        val expected = now + 7L * 86_400_000
        assertEquals(expected, CallInviteExpiryPreset.SevenDays.toMs(now))
    }

    @Test
    fun `max uses preset single resolves to zero sentinel`() {
        assertEquals(0u, CallInviteMaxUsesPreset.Single.toValue())
    }

    @Test
    fun `max uses preset unlimited resolves to u32 max`() {
        assertEquals(UInt.MAX_VALUE, CallInviteMaxUsesPreset.Unlimited.toValue())
    }

    @Test
    fun `max uses preset ten resolves to ten`() {
        assertEquals(10u, CallInviteMaxUsesPreset.Ten.toValue())
    }

    // -----------------------------------------------------------
    // Human-visible renderers — sentinel cases
    // -----------------------------------------------------------

    @Test
    fun `describe max uses surfaces single-use sentinel`() {
        assertEquals("single-use", describeCallInviteMaxUses(0u))
    }

    @Test
    fun `describe max uses surfaces unlimited sentinel`() {
        assertEquals("unlimited", describeCallInviteMaxUses(UInt.MAX_VALUE))
    }

    @Test
    fun `describe max uses surfaces N for finite ceiling`() {
        assertEquals("up to 5 uses", describeCallInviteMaxUses(5u))
    }

    @Test
    fun `describe expiry surfaces never sentinel`() {
        assertEquals("never expires", describeCallInviteExpiry(expiryMs = 0L, nowMs = 100L))
    }

    @Test
    fun `describe expiry surfaces future relative time`() {
        val now = 1_700_000_000_000L
        val sixDays = now + 6L * 86_400_000
        val rendered = describeCallInviteExpiry(expiryMs = sixDays, nowMs = now)
        assertTrue(
            "expected `in 6 days` rendering, got: $rendered",
            rendered.contains("in 6 days"),
        )
    }

    @Test
    fun `describe expiry surfaces past relative time`() {
        val now = 1_700_000_000_000L
        val threeHoursAgo = now - 3L * 3_600_000
        val rendered = describeCallInviteExpiry(expiryMs = threeHoursAgo, nowMs = now)
        assertTrue(
            "expected `expired 3 hours ago` rendering, got: $rendered",
            rendered.contains("expired 3 hours") && rendered.endsWith(" ago"),
        )
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
    fun `blob id length matches the discovery service`() {
        // Pinned to SHA-256 → 32 bytes; the discovery service
        // (`services/discovery/src/call_invite.rs::BLOB_ID_LEN`)
        // uses the same.
        assertEquals(32, CALL_INVITE_BLOB_ID_LEN)
    }

    @Test
    fun `label max len matches the rust constant`() {
        // Pinned to `lakko_contacts::invite::MAX_LABEL_LEN`.
        assertEquals(64, CALL_INVITE_LABEL_MAX_LEN)
    }

    @Test
    fun `inline minimum is documented at 400`() {
        // Below this, the parser cannot discriminate inline
        // from a malformed pointer URL.
        assertEquals(400, CALL_INVITE_INLINE_MIN_LEN)
    }

    @Test
    fun `auto compose forwards to the pointer URL on 32 bytes`() {
        val blobId = ByteArray(CALL_INVITE_BLOB_ID_LEN) { 0x01 }
        val urlAuto = composeCallInviteUrl(blobId)
        val urlPointer = composePointerCallInviteUrl(blobId)
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
        val blobId = ByteArray(CALL_INVITE_BLOB_ID_LEN) { 0xEE.toByte() }
        val parsed = parseCallInviteSlug(
            me.lakko.app.auth.base64UrlEncodeNoPad(blobId),
        )
        val pointer = parsed as ParsedCallInvite.Pointer
        assertNotNull(pointer.blobId)
        assertEquals(CALL_INVITE_BLOB_ID_LEN, pointer.blobId.size)
        for (i in 0 until CALL_INVITE_BLOB_ID_LEN) {
            assertEquals(
                "byte at index $i must round-trip",
                blobId[i].toInt() and 0xFF,
                pointer.blobId[i].toInt() and 0xFF,
            )
        }
    }
}
