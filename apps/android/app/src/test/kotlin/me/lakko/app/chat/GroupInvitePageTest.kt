// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.auth.base64UrlEncodeNoPad
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Test

/**
 * ADR-0028 v2 (`docs/ux/group-invite-page.md`) — the pure half of the group
 * invite page.
 *
 *  1. The link key rides the URL fragment on both link forms and parses back
 *     out; anything that is not a 32-byte base64url key reads as "no preview".
 *  2. The revoke path parses the slug BEFORE the `#`.
 *  3. The one route argument carries slug + fragment and splits back.
 *  4. The card copy: both names, one missing, both missing, count 1 vs N, and
 *     no preview at all.
 */
class GroupInvitePageTest {

    private val key = ByteArray(GROUP_INVITE_LINK_KEY_LEN) { (it * 7 + 3).toByte() }
    private val keyText = base64UrlEncodeNoPad(key)
    private val blobId = ByteArray(GROUP_INVITE_BLOB_ID_LEN) { it.toByte() }
    private val origin = "https://lakko.me"

    // ---- 1. the fragment -------------------------------------------------

    @Test
    fun `the pointer link carries the key as its fragment`() {
        val url = appendGroupInviteLinkKey(composePointerGroupInviteUrl(blobId, origin), key)
        assertEquals("$origin/g/${base64UrlEncodeNoPad(blobId)}#$keyText", url)
        assertEquals(43, keyText.length)
    }

    @Test
    fun `the inline link carries the key as its fragment too`() {
        val wire = ByteArray(160) { (it + 1).toByte() }
        val url = appendGroupInviteLinkKey(composeInlineGroupInviteUrl(wire, origin), key)
        assertEquals("$origin/g/${base64UrlEncodeNoPad(wire)}#$keyText", url)
        // The slug still discriminates as inline once the fragment is stripped.
        val slug = groupInviteSlugOf(url)
        assertEquals(ParsedGroupInvite.Inline(wire), parseGroupInviteSlug(slug!!))
    }

    @Test
    fun `a key of the wrong length never produces a link`() {
        assertThrows(IllegalArgumentException::class.java) {
            appendGroupInviteLinkKey("$origin/g/x", ByteArray(31))
        }
    }

    @Test
    fun `the key parses back out of the fragment`() {
        val url = appendGroupInviteLinkKey(composePointerGroupInviteUrl(blobId, origin), key)
        val fragment = groupInviteFragmentOf(url)
        assertEquals(keyText, fragment)
        assertArrayEquals(key, parseGroupInviteLinkKey(fragment))
    }

    @Test
    fun `an absent, blank, wrong-length or undecodable fragment is no preview, not an error`() {
        assertNull(parseGroupInviteLinkKey(null))
        assertNull(parseGroupInviteLinkKey(""))
        assertNull(parseGroupInviteLinkKey("   "))
        assertNull(parseGroupInviteLinkKey(base64UrlEncodeNoPad(ByteArray(31))))
        assertNull(parseGroupInviteLinkKey(base64UrlEncodeNoPad(ByteArray(33))))
        assertNull(parseGroupInviteLinkKey("not*base64url!"))
        assertNull(groupInviteFragmentOf(composePointerGroupInviteUrl(blobId, origin)))
    }

    // ---- 2. the revoke slug ---------------------------------------------

    @Test
    fun `the revoke path reads the slug before the fragment`() {
        val url = appendGroupInviteLinkKey(composePointerGroupInviteUrl(blobId, origin), key)
        val slug = groupInviteSlugOf(url)
        assertEquals(base64UrlEncodeNoPad(blobId), slug)
        assertEquals(ParsedGroupInvite.Pointer(blobId), parseGroupInviteSlug(slug!!))
        // A link without a fragment parses the same.
        assertEquals(slug, groupInviteSlugOf(composePointerGroupInviteUrl(blobId, origin)))
        assertNull(groupInviteSlugOf("https://lakko.me/i/abc"))
        assertNull(groupInviteSlugOf("https://lakko.me/g/#$keyText"))
    }

    // ---- 3. the route argument -------------------------------------------

    @Test
    fun `the route argument carries the slug and the fragment and splits back`() {
        val link = GroupJoinLink(slug = "abc", fragment = keyText)
        val argument = composeGroupJoinArgument(link)
        assertEquals("abc#$keyText", argument)
        assertEquals(link, parseGroupJoinArgument(argument))
    }

    @Test
    fun `a link without a fragment is just the slug`() {
        val link = GroupJoinLink(slug = "abc", fragment = null)
        assertEquals("abc", composeGroupJoinArgument(link))
        assertEquals(link, parseGroupJoinArgument("abc"))
        assertEquals(link, composeGroupJoinArgument(GroupJoinLink("abc", "")).let(::parseGroupJoinArgument))
        assertEquals(link, parseGroupJoinArgument("abc#"))
    }

    // ---- 4. the card copy ------------------------------------------------

    @Test
    fun `both names, several members`() {
        val lines = groupInvitePageLines(GroupInvitePreviewUi("Hiking club", "Anna", 12))
        assertEquals("Anna invites you to", lines.headline)
        assertEquals("Hiking club", lines.groupLine)
        assertEquals("12 members", lines.membersLine)
    }

    @Test
    fun `no inviter name reads You are invited to`() {
        val lines = groupInvitePageLines(GroupInvitePreviewUi("Hiking club", null, 3))
        assertEquals("You are invited to", lines.headline)
        assertEquals("Hiking club", lines.groupLine)
        assertEquals("3 members", lines.membersLine)
    }

    @Test
    fun `no group name folds a group into the headline and omits the line`() {
        val lines = groupInvitePageLines(GroupInvitePreviewUi(null, "Anna", 2))
        assertEquals("Anna invites you to a group", lines.headline)
        assertNull(lines.groupLine)
        assertEquals("2 members", lines.membersLine)
    }

    @Test
    fun `both names missing`() {
        val lines = groupInvitePageLines(GroupInvitePreviewUi(null, null, 5))
        assertEquals("You are invited to a group", lines.headline)
        assertNull(lines.groupLine)
        assertEquals("5 members", lines.membersLine)
    }

    @Test
    fun `one member is singular`() {
        assertEquals("1 member", groupInvitePageLines(GroupInvitePreviewUi("Solo", "Anna", 1)).membersLine)
        assertEquals("1 member", memberCountLine(1))
        assertEquals("0 members", memberCountLine(0))
        assertEquals("2 members", memberCountLine(2))
    }

    @Test
    fun `blank names are absent`() {
        val lines = groupInvitePageLines(GroupInvitePreviewUi("  ", " ", 4))
        assertEquals("You are invited to a group", lines.headline)
        assertNull(lines.groupLine)
    }

    @Test
    fun `no preview at all reads You are invited to a group and nothing else`() {
        val lines = groupInvitePageLines(null)
        assertEquals("You are invited to a group", lines.headline)
        assertNull(lines.groupLine)
        assertNull(lines.membersLine)
    }

    @Test
    fun `the foot names the fingerprint`() {
        assertEquals("Invited by 12345 67890", invitedByLine("12345 67890"))
    }
}
