// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import me.lakko.app.auth.base64UrlEncodeNoPad
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiException

/**
 * The sealed invite link shared by `/i/`, `/c/` and `/g/` (ADR-0028 amendment
 * of 2026-09-28): the fragment secret, the link builder, the landing argument
 * that carries the fragment through the app, the `/i/` slug, and the
 * damaged-link classification. Pure: the widths are passed in, no native
 * library is loaded.
 */
class InviteLinkTest {

    private val L = TEST_INVITE_LINK_LENGTHS
    private val secret = ByteArray(L.linkSecretLen) { (it * 5 + 2).toByte() }
    private val secretText = base64UrlEncodeNoPad(secret)

    // ---- fragment parsing -------------------------------------------------

    @Test
    fun `no fragment, a blank one, or an empty one is absent and has no secret`() {
        for (fragment in listOf(null, "", "   ")) {
            assertSame(InviteLinkFragment.Absent, parseInviteLinkFragment(fragment, L))
            assertNull(inviteLinkSecretOrNull(fragment, L))
        }
    }

    @Test
    fun `a fragment of the wrong length is malformed`() {
        val old = base64UrlEncodeNoPad(ByteArray(32) { 1 })
        for (fragment in listOf(old, secretText.dropLast(1), secretText + "A", "AAAA")) {
            assertSame(InviteLinkFragment.Malformed, parseInviteLinkFragment(fragment, L))
            assertNull(inviteLinkSecretOrNull(fragment, L))
        }
    }

    @Test
    fun `a 16-character fragment outside the base64url alphabet is malformed`() {
        val wrongAlphabet = "not*base64url!!!"
        assertEquals(L.linkSecretUrlLen, wrongAlphabet.length)
        assertSame(InviteLinkFragment.Malformed, parseInviteLinkFragment(wrongAlphabet, L))
        // Standard-alphabet `+` and `/` are not base64url either.
        val standard = "AAAAAAAAAAAAAA+/"
        assertSame(InviteLinkFragment.Malformed, parseInviteLinkFragment(standard, L))
    }

    @Test
    fun `a valid fragment yields the 12-byte secret`() {
        val parsed = parseInviteLinkFragment(secretText, L)
        assertTrue(parsed is InviteLinkFragment.Secret)
        assertArrayEquals(secret, (parsed as InviteLinkFragment.Secret).bytes)
        assertArrayEquals(secret, inviteLinkSecretOrNull(secretText, L))
    }

    @Test
    fun `the secret is never printed`() {
        assertFalse(InviteLinkFragment.Secret(secret).toString().contains(secretText))
        assertFalse(InviteLinkTap("abc", secretText).toString().contains(secretText))
    }

    // ---- the link builder ---------------------------------------------------

    @Test
    fun `appending the secret gives the 16-character fragment`() {
        val url = appendInviteLinkSecret("https://lakko.me/i/AQIDBAUG", secret, L)
        assertEquals("https://lakko.me/i/AQIDBAUG#$secretText", url)
        assertEquals(secretText, inviteLinkFragmentOf(url))
        assertEquals("AQIDBAUG", inviteLinkSlugOf(url, "/i/"))
    }

    @Test
    fun `a secret of the wrong width is refused`() {
        for (bad in listOf(ByteArray(0), ByteArray(11), ByteArray(32))) {
            assertThrows(IllegalArgumentException::class.java) {
                appendInviteLinkSecret("https://lakko.me/c/x", bad, L)
            }
        }
    }

    @Test
    fun `the slug of a link is read before the fragment and after its own prefix only`() {
        assertEquals("abc", inviteLinkSlugOf("https://lakko.me/c/abc/#$secretText", "/c/"))
        assertNull(inviteLinkSlugOf("https://lakko.me/g/abc#$secretText", "/c/"))
        assertNull(inviteLinkSlugOf("https://lakko.me/c/#$secretText", "/c/"))
        // A `/c/` that appears only inside the fragment is not a slug.
        assertNull(inviteLinkSlugOf("https://lakko.me/g/x#/c/abc", "/c/"))
    }

    // ---- the landing argument ----------------------------------------------

    @Test
    fun `the landing argument carries the fragment and splits back`() {
        val tap = InviteLinkTap(slug = "AQIDBAUG", fragment = secretText)
        val argument = composeInviteLinkArgument(tap)
        assertEquals("AQIDBAUG#$secretText", argument)
        assertEquals(tap, parseInviteLinkArgument(argument))
    }

    @Test
    fun `a tap without a fragment stays without one`() {
        val tap = InviteLinkTap(slug = "AQIDBAUG", fragment = null)
        assertEquals("AQIDBAUG", composeInviteLinkArgument(tap))
        assertEquals(tap, parseInviteLinkArgument("AQIDBAUG"))
        assertEquals(tap, parseInviteLinkArgument("AQIDBAUG#"))
        assertNull(inviteLinkSecretOrNull(parseInviteLinkArgument("AQIDBAUG").fragment, L))
    }

    // ---- the `/i/` slug ------------------------------------------------------

    @Test
    fun `an 8-character slug is a pointer`() {
        val id = ByteArray(L.blobIdLen) { (it + 1).toByte() }
        val parsed = parseContactInviteSlug(base64UrlEncodeNoPad(id), L)
        assertEquals(ParsedContactInvite.Pointer(id), parsed)
    }

    @Test
    fun `a long slug is the inline sealed blob`() {
        val sealed = ByteArray(200) { (it * 3).toByte() }
        assertEquals(ParsedContactInvite.Inline(sealed), parseContactInviteSlug(base64UrlEncodeNoPad(sealed), L))
    }

    @Test
    fun `the old 43-character id, the dead zone and bad characters are refused`() {
        assertNull(parseContactInviteSlug(base64UrlEncodeNoPad(ByteArray(32)), L))
        assertNull(parseContactInviteSlug(base64UrlEncodeNoPad(ByteArray(CONTACT_INVITE_INLINE_MIN_LEN - 1)), L))
        assertNull(parseContactInviteSlug("AAAA*AAA", L))
        assertNull(parseContactInviteSlug("", L))
    }

    // ---- damaged-link classification ----------------------------------------

    @Test
    fun `the core's damaged refusal and ours read as damaged anywhere in the chain`() {
        assertTrue(isInviteLinkDamaged(FfiException.InviteLinkDamaged("link damaged")))
        assertTrue(isInviteLinkDamaged(RuntimeException("wrapped", FfiException.InviteLinkDamaged("x"))))
        assertTrue(isInviteLinkDamaged(InviteLinkDamagedException()))
        assertFalse(isInviteLinkDamaged(FfiException.Contacts("other")))
        assertFalse(isInviteLinkDamaged(DiscoveryHttpError(404, "/x", "")))
    }

    @Test
    fun `the seal overhead matches the server's column widths`() {
        // migrations/0022_sealed_invite_links.sql: 254 / 4338 / 938, each a
        // codec's MAX_WIRE_LEN plus this overhead.
        assertEquals(30, INVITE_LINK_SEAL_OVERHEAD)
        assertEquals(254, DiscoveryClient.MAX_SEALED_INVITE_BYTES)
        assertEquals(4338, DiscoveryClient.MAX_SEALED_CALL_INVITE_BYTES)
        assertEquals(938, DiscoveryClient.MAX_SEALED_GROUP_INVITE_BYTES)
    }
}
