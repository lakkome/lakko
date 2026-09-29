// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.discovery.InviteLinkFragment
import me.lakko.app.discovery.inviteLinkFragmentOf
import me.lakko.app.discovery.parseInviteLinkFragment
import me.lakko.app.discovery.TEST_INVITE_LINK_LENGTHS
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0028 v2 (`docs/ux/group-invite-page.md`), the short-link amendment of
 * 2026-09-27 and the sealed-link amendment of 2026-09-28 — the pure half of
 * the group invite page.
 *
 *  1. The 12-byte link secret rides the URL fragment (16 characters) on both
 *     link forms and parses back out; an absent fragment, or one of any other
 *     shape (the old 43-character key included), makes the link damaged.
 *  2. The preview decision: a missing / malformed secret or a preview that
 *     does not open refuses the link; a capability without a preview (it
 *     already opened under the secret) is shown without one.
 *  3. The pointer slug is the 8-character id; the old 43-character id and the
 *     dead zone are refused.
 *  4. The revoke path parses the slug BEFORE the `#`.
 *  5. The one route argument carries slug + fragment and splits back.
 *  6. The card copy.
 *
 * The widths are passed in ([TEST_INVITE_LINK_LENGTHS]): the host JVM has no
 * native library to read the core's.
 */
class GroupInvitePageTest {

    private val L = TEST_INVITE_LINK_LENGTHS
    private val key = ByteArray(L.linkSecretLen) { (it * 7 + 3).toByte() }
    private val keyText = base64UrlEncodeNoPad(key)
    private val blobId = ByteArray(L.blobIdLen) { it.toByte() }
    private val origin = "https://lakko.me"

    private fun pointerUrl(): String = composePointerGroupInviteUrl(blobId, key, origin, L)

    // ---- 1. the fragment -------------------------------------------------

    @Test
    fun `the pointer link is id plus secret, about 45 characters`() {
        val url = pointerUrl()
        assertEquals("$origin/g/${base64UrlEncodeNoPad(blobId)}#$keyText", url)
        assertEquals(8, base64UrlEncodeNoPad(blobId).length)
        assertEquals(16, keyText.length)
        assertEquals("https://lakko.me/g/".length + 8 + 1 + 16, url.length)
    }

    @Test
    fun `the inline link carries the sealed blob and the secret as its fragment`() {
        val sealed = ByteArray(160) { (it + 1).toByte() }
        val url = composeInlineGroupInviteUrl(sealed, key, origin, L)
        assertEquals("$origin/g/${base64UrlEncodeNoPad(sealed)}#$keyText", url)
        // The slug still discriminates as inline once the fragment is stripped.
        val slug = groupInviteSlugOf(url)
        assertEquals(ParsedGroupInvite.Inline(sealed), parseGroupInviteSlug(slug!!, L))
    }

    @Test
    fun `a secret of the wrong length never produces a link`() {
        assertThrows(IllegalArgumentException::class.java) {
            composePointerGroupInviteUrl(blobId, ByteArray(32), origin, L)
        }
        assertThrows(IllegalArgumentException::class.java) {
            composeInlineGroupInviteUrl(ByteArray(GROUP_INVITE_INLINE_MIN_LEN), ByteArray(11), origin, L)
        }
    }

    @Test
    fun `the secret parses back out of the fragment`() {
        val fragment = inviteLinkFragmentOf(pointerUrl())
        assertEquals(keyText, fragment)
        val parsed = parseInviteLinkFragment(fragment, L)
        assertTrue(parsed is InviteLinkFragment.Secret)
        assertArrayEquals(key, (parsed as InviteLinkFragment.Secret).bytes)
    }

    @Test
    fun `an absent or blank fragment is absent`() {
        assertSame(InviteLinkFragment.Absent, parseInviteLinkFragment(null, L))
        assertSame(InviteLinkFragment.Absent, parseInviteLinkFragment("", L))
        assertSame(InviteLinkFragment.Absent, parseInviteLinkFragment("   ", L))
        assertNull(inviteLinkFragmentOf("$origin/g/${base64UrlEncodeNoPad(blobId)}"))
    }

    @Test
    fun `a fragment that is not exactly the 16-character secret is malformed`() {
        val old = base64UrlEncodeNoPad(ByteArray(32) { 1 })
        assertEquals(43, old.length)
        assertSame(InviteLinkFragment.Malformed, parseInviteLinkFragment(old, L))
        assertSame(InviteLinkFragment.Malformed, parseInviteLinkFragment(keyText.dropLast(1), L))
        assertSame(InviteLinkFragment.Malformed, parseInviteLinkFragment(keyText + "A", L))
        assertSame(InviteLinkFragment.Malformed, parseInviteLinkFragment("not*base64url!!!", L))
    }

    // ---- 2. the preview decision ------------------------------------------

    private val preview = GroupInvitePreviewUi("Hiking club", "Anna", 12)

    @Test
    fun `a secret that opens the preview shows it`() {
        var opened: ByteArray? = null
        val outcome = resolveGroupInvitePreview(InviteLinkFragment.Secret(key)) { opened = it; preview }
        assertEquals(GroupInvitePreviewOutcome.Shown(preview), outcome)
        assertArrayEquals(key, opened)
    }

    @Test
    fun `no fragment refuses the link and never opens`() {
        val outcome = resolveGroupInvitePreview(InviteLinkFragment.Absent) {
            error("no secret, nothing to open")
        }
        assertSame(GroupInvitePreviewOutcome.Damaged, outcome)
    }

    @Test
    fun `a secret the preview does not open under refuses the link`() {
        val outcome = resolveGroupInvitePreview(InviteLinkFragment.Secret(key)) {
            throw IllegalStateException("does not open")
        }
        assertSame(GroupInvitePreviewOutcome.Damaged, outcome)
    }

    @Test
    fun `a capability minted without a preview is shown without one`() {
        // The capability itself already opened under the same secret, so a
        // server cannot have swapped in a preview-less one.
        val outcome = resolveGroupInvitePreview(InviteLinkFragment.Secret(key)) { null }
        assertEquals(GroupInvitePreviewOutcome.Shown(null), outcome)
    }

    @Test
    fun `a malformed fragment refuses the link without opening`() {
        val outcome = resolveGroupInvitePreview(InviteLinkFragment.Malformed) {
            error("must not open")
        }
        assertSame(GroupInvitePreviewOutcome.Damaged, outcome)
    }

    @Test
    fun `a damaged link reads as damaged in the join copy and offers no retry`() {
        val copy = joinFailureCopy(ChatError("join failed", GroupInviteLinkDamaged()))
        assertEquals(me.lakko.app.discovery.INVITE_LINK_DAMAGED_COPY, copy)
        assertTrue(!joinFailureOffersRetry(copy))
        assertEquals(
            me.lakko.app.discovery.INVITE_LINK_DAMAGED_COPY,
            joinFailureCopy(me.lakko.app.discovery.InviteLinkDamagedException()),
        )
    }

    @Test
    fun `open failures classify as damaged, rate limited or invalid`() {
        assertEquals(
            GroupInviteOpenFailure.Damaged,
            groupInviteOpenFailure(GroupInviteLinkDamaged()),
        )
        assertEquals(
            GroupInviteOpenFailure.RateLimited,
            groupInviteOpenFailure(
                ChatError("group invite fetch failed", DiscoveryHttpError(429, "/discovery/group-invite/x (GET)", "")),
            ),
        )
        assertEquals(
            GroupInviteOpenFailure.Invalid,
            groupInviteOpenFailure(ChatError("group invite not found or expired", IllegalStateException("404"))),
        )
        assertEquals(
            GroupInviteOpenFailure.Invalid,
            groupInviteOpenFailure(
                ChatError("group invite fetch failed", DiscoveryHttpError(500, "/x", "")),
            ),
        )
    }

    @Test
    fun `a 429 on consume reads as try again later with a retry, never invalid`() {
        val copy = joinFailureCopy(
            ChatError("group invite consume failed", DiscoveryHttpError(429, "/discovery/group-invite/x/consume", "")),
        )
        assertEquals(me.lakko.app.discovery.INVITE_RATE_LIMITED_COPY, copy)
        assertTrue(joinFailureOffersRetry(copy))
    }

    // ---- 3. the slug -----------------------------------------------------

    @Test
    fun `the old 43-character pointer and the dead zone are refused`() {
        val old = base64UrlEncodeNoPad(ByteArray(32) { 2 })
        assertEquals(43, old.length)
        val ex = assertThrows(GroupInviteUrlException.SlugWrongLength::class.java) {
            parseGroupInviteSlug(old, L)
        }
        assertEquals(32, ex.got)
        assertEquals(6, ex.blobIdLen)
        assertThrows(GroupInviteUrlException.SlugWrongLength::class.java) {
            parseGroupInviteSlug(base64UrlEncodeNoPad(ByteArray(99)), L)
        }
        assertThrows(GroupInviteUrlException.SlugWrongLength::class.java) {
            parseGroupInviteSlug(base64UrlEncodeNoPad(ByteArray(7)), L)
        }
        assertThrows(GroupInviteUrlException.WrongBlobIdLength::class.java) {
            composePointerGroupInviteUrl(ByteArray(32), key, origin, L)
        }
    }

    @Test
    fun `a 100-byte slug is inline`() {
        val wire = ByteArray(GROUP_INVITE_INLINE_MIN_LEN) { 4 }
        assertEquals(ParsedGroupInvite.Inline(wire), parseGroupInviteSlug(base64UrlEncodeNoPad(wire), L))
    }

    // ---- 4. the revoke slug ---------------------------------------------

    @Test
    fun `the revoke path reads the slug before the fragment`() {
        val slug = groupInviteSlugOf(pointerUrl())
        assertEquals(base64UrlEncodeNoPad(blobId), slug)
        assertEquals(ParsedGroupInvite.Pointer(blobId), parseGroupInviteSlug(slug!!, L))
        // A link without a fragment parses the same.
        assertEquals(slug, groupInviteSlugOf("$origin/g/${base64UrlEncodeNoPad(blobId)}"))
        assertNull(groupInviteSlugOf("https://lakko.me/i/abc"))
        assertNull(groupInviteSlugOf("https://lakko.me/g/#$keyText"))
    }

    // ---- 5. the route argument -------------------------------------------

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

    // ---- 6. the card copy ------------------------------------------------

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
