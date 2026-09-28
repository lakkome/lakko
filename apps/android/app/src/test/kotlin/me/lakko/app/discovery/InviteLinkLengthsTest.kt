// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import me.lakko.app.auth.base64UrlEncodeNoPad
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The short-link widths every host JVM test passes in place of
 * [InviteLinkLengths.core]. The host JVM has no native library, so the core
 * getters (`inviteBlobIdLen()` and friends) cannot be called here; these are
 * the values `crates/lakko-ffi-uniffi` pins in `short_link_lengths_are_exported`
 * (6 / 8 / 12 / 16), restated once for the whole test tree.
 */
internal val TEST_INVITE_LINK_LENGTHS: InviteLinkLengths = InviteLinkLengths(
    blobIdLen = 6,
    blobIdUrlLen = 8,
    linkSecretLen = 12,
    linkSecretUrlLen = 16,
)

class InviteLinkLengthsTest {

    @Test
    fun `the fixture widths are self-consistent base64url`() {
        val l = TEST_INVITE_LINK_LENGTHS
        assertEquals(l.blobIdUrlLen, base64UrlEncodeNoPad(ByteArray(l.blobIdLen)).length)
        assertEquals(l.linkSecretUrlLen, base64UrlEncodeNoPad(ByteArray(l.linkSecretLen)).length)
    }

    @Test
    fun `an 8-character id decodes to its 6 bytes`() {
        val id = ByteArray(6) { (it * 37).toByte() }
        assertArrayEquals(id, decodeInviteBlobId(base64UrlEncodeNoPad(id), TEST_INVITE_LINK_LENGTHS))
    }

    @Test
    fun `the old 43-character id and every other width are refused`() {
        val old = base64UrlEncodeNoPad(ByteArray(32) { 1 })
        assertEquals(43, old.length)
        assertNull(decodeInviteBlobId(old, TEST_INVITE_LINK_LENGTHS))
        assertNull(decodeInviteBlobId("", TEST_INVITE_LINK_LENGTHS))
        assertNull(decodeInviteBlobId("AAAAAAA", TEST_INVITE_LINK_LENGTHS))
        assertNull(decodeInviteBlobId("AAAAAAAAA", TEST_INVITE_LINK_LENGTHS))
        assertNull(decodeInviteBlobId("AAAA*AAA", TEST_INVITE_LINK_LENGTHS))
    }

    @Test
    fun `a 429 anywhere in the cause chain reads as rate limited`() {
        val limited = DiscoveryHttpError(429, "/discovery/group-invite/x (GET)", "")
        assertTrue(isDiscoveryRateLimited(limited))
        assertTrue(isDiscoveryRateLimited(RuntimeException("wrapped", limited)))
    }

    @Test
    fun `other statuses and failures are not rate limited`() {
        assertFalse(isDiscoveryRateLimited(DiscoveryHttpError(404, "/x", "")))
        assertFalse(isDiscoveryRateLimited(DiscoveryHttpError(500, "/x", "")))
        assertFalse(isDiscoveryRateLimited(DiscoveryNetworkError("/x", RuntimeException("dns"))))
        assertFalse(isDiscoveryRateLimited(IllegalStateException("nope")))
    }
}
