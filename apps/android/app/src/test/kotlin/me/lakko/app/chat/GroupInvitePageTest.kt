// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.l10n.en
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.discovery.TEST_INVITE_LINK_LENGTHS
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0028 v2 (`docs/ux/group-invite-page.md`) and the short-link amendment of
 * 2026-09-27 — the pure half of the group invite page.
 *
 *  1. The 12-byte link secret rides the URL fragment (16 characters) on both
 *     link forms and parses back out; an absent fragment is "no preview", a
 *     present one of any other shape (the old 43-character key included) is
 *     malformed and refuses the link.
 *  2. A fragment whose preview does not open refuses the link as damaged — no
 *     preview-less fallback.
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

    private fun pointerUrl(): String = composePointerGroupInviteUrl(blobId, origin, L)

    // ---- 1. the fragment -------------------------------------------------

    @Test
    fun `the pointer link is id plus secret, about 45 characters`() {
        val url = appendGroupInviteLinkKey(pointerUrl(), key, L)
        assertEquals("$origin/g/${base64UrlEncodeNoPad(blobId)}#$keyText", url)
        assertEquals(8, base64UrlEncodeNoPad(blobId).length)
        assertEquals(16, keyText.length)
        assertEquals("https://lakko.me/g/".length + 8 + 1 + 16, url.length)
    }

    /** The inline form is gone (2026-10-19): a `/g/` link carrying the
     *  sealed blob in its path is refused as malformed, so it opens as a
     *  dead link, its secret notwithstanding. */
    @Test
    fun `an inline link is refused as malformed`() {
        val wire = ByteArray(160) { (it + 1).toByte() }
        val url = appendGroupInviteLinkKey("$origin/g/${base64UrlEncodeNoPad(wire)}", key, L)
        val slug = groupInviteSlugOf(url)
        assertThrows(GroupInviteUrlException.SlugWrongLength::class.java) {
            parseGroupInviteSlug(slug!!, L)
        }
    }

    @Test
    fun `a secret of the wrong length never produces a link`() {
        assertThrows(IllegalArgumentException::class.java) {
            appendGroupInviteLinkKey("$origin/g/x", ByteArray(32), L)
        }
        assertThrows(IllegalArgumentException::class.java) {
            appendGroupInviteLinkKey("$origin/g/x", ByteArray(11), L)
        }
    }

    @Test
    fun `the secret parses back out of the fragment`() {
        val url = appendGroupInviteLinkKey(pointerUrl(), key, L)
        val fragment = groupInviteFragmentOf(url)
        assertEquals(keyText, fragment)
        val parsed = parseGroupInviteFragment(fragment, L)
        assertTrue(parsed is GroupInviteFragment.Secret)
        assertArrayEquals(key, (parsed as GroupInviteFragment.Secret).bytes)
    }

    @Test
    fun `an absent or blank fragment parses as absent and carries no secret (the link is damaged)`() {
        assertSame(GroupInviteFragment.Absent, parseGroupInviteFragment(null, L))
        assertSame(GroupInviteFragment.Absent, parseGroupInviteFragment("", L))
        assertSame(GroupInviteFragment.Absent, parseGroupInviteFragment("   ", L))
        assertNull(groupInviteFragmentOf(pointerUrl()))
        // Sealed links: no fragment opens nothing.
        assertNull(GroupInviteFragment.Absent.secretOrNull())
        assertNull(GroupInviteFragment.Malformed.secretOrNull())
        assertArrayEquals(key, GroupInviteFragment.Secret(key).secretOrNull())
    }

    @Test
    fun `a fragment that is not exactly the 16-character secret is malformed`() {
        val old = base64UrlEncodeNoPad(ByteArray(32) { 1 })
        assertEquals(43, old.length)
        assertSame(GroupInviteFragment.Malformed, parseGroupInviteFragment(old, L))
        assertSame(GroupInviteFragment.Malformed, parseGroupInviteFragment(keyText.dropLast(1), L))
        assertSame(GroupInviteFragment.Malformed, parseGroupInviteFragment(keyText + "A", L))
        assertSame(GroupInviteFragment.Malformed, parseGroupInviteFragment("not*base64url!!!", L))
    }

    // ---- 2. the damaged-preview refusal ----------------------------------

    private val preview = GroupInvitePreviewUi("Hiking club", "Anna", 12)

    @Test
    fun `a secret that opens the preview shows it`() {
        var opened: ByteArray? = null
        val outcome = resolveGroupInvitePreview(hasPreview = true, secret = key) { opened = it; preview }
        assertEquals(GroupInvitePreviewOutcome.Shown(preview), outcome)
        assertArrayEquals(key, opened)
    }

    @Test
    fun `a capability minted without a preview shows the page without one and never opens`() {
        val outcome = resolveGroupInvitePreview(hasPreview = false, secret = key) {
            error("no preview, nothing to open")
        }
        assertEquals(GroupInvitePreviewOutcome.Shown(null), outcome)
    }

    @Test
    fun `a present preview that does not open refuses the link`() {
        // The core throws when the sealed preview does not open (a wrong or
        // truncated secret).
        val outcome = resolveGroupInvitePreview(hasPreview = true, secret = key) {
            throw IllegalStateException("does not open")
        }
        assertSame(GroupInvitePreviewOutcome.Damaged, outcome)
    }

    @Test
    fun `a present preview that answers nothing refuses the link`() {
        val outcome = resolveGroupInvitePreview(hasPreview = true, secret = key) { null }
        assertSame(GroupInvitePreviewOutcome.Damaged, outcome)
    }

    @Test
    fun `open failures classify as damaged, rate limited, failed to fetch or invalid`() {
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
        // A failure to ask the ledger says nothing about the link: offered
        // again, a 429 inside it still a rate limit.
        assertEquals(
            GroupInviteOpenFailure.OpenFailed,
            groupInviteOpenFailure(
                ChatError("group invite fetch failed", InviteLinkUnreachable(DiscoveryHttpError(500, "/x", ""))),
            ),
        )
        assertEquals(
            GroupInviteOpenFailure.OpenFailed,
            groupInviteOpenFailure(
                ChatError("group invite fetch failed", InviteLinkUnreachable(java.io.IOException("reset"))),
            ),
        )
        assertEquals(
            GroupInviteOpenFailure.RateLimited,
            groupInviteOpenFailure(
                ChatError(
                    "group invite fetch failed",
                    InviteLinkUnreachable(DiscoveryHttpError(429, "/discovery/group-invite/x (GET)", "")),
                ),
            ),
        )
        // Anything after the ledger answered is the link's own problem.
        assertEquals(
            GroupInviteOpenFailure.Invalid,
            groupInviteOpenFailure(ChatError("group invite link is malformed", IllegalArgumentException("slug"))),
        )
    }

    @Test
    fun `a 429 on consume reads as try again later with a retry, never invalid`() {
        val copy = joinFailureCopy(
            ChatError("group invite consume failed", DiscoveryHttpError(429, "/discovery/group-invite/x/consume", "")),
        )
        assertEquals(me.lakko.app.discovery.INVITE_RATE_LIMITED_TEXT, copy)
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
            composePointerGroupInviteUrl(ByteArray(32), origin, L)
        }
    }

    @Test
    fun `a 100-byte slug is refused, it is not a ledger id`() {
        assertThrows(GroupInviteUrlException.SlugWrongLength::class.java) {
            parseGroupInviteSlug(base64UrlEncodeNoPad(ByteArray(100) { 4 }), L)
        }
    }

    // ---- 4. the revoke slug ---------------------------------------------

    @Test
    fun `the revoke path reads the slug before the fragment`() {
        val url = appendGroupInviteLinkKey(pointerUrl(), key, L)
        val slug = groupInviteSlugOf(url)
        assertEquals(base64UrlEncodeNoPad(blobId), slug)
        assertEquals(ParsedGroupInvite.Pointer(blobId), parseGroupInviteSlug(slug!!, L))
        // A link without a fragment parses the same.
        assertEquals(slug, groupInviteSlugOf(pointerUrl()))
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
        assertEquals("Anna invites you to", lines.headline.en)
        assertEquals("Hiking club", lines.groupLine)
        assertEquals("12 members", lines.membersLine?.en)
    }

    @Test
    fun `no inviter name reads You are invited to`() {
        val lines = groupInvitePageLines(GroupInvitePreviewUi("Hiking club", null, 3))
        assertEquals("You are invited to", lines.headline.en)
        assertEquals("Hiking club", lines.groupLine)
        assertEquals("3 members", lines.membersLine?.en)
    }

    @Test
    fun `no group name folds a group into the headline and omits the line`() {
        val lines = groupInvitePageLines(GroupInvitePreviewUi(null, "Anna", 2))
        assertEquals("Anna invites you to a group", lines.headline.en)
        assertNull(lines.groupLine)
        assertEquals("2 members", lines.membersLine?.en)
    }

    @Test
    fun `both names missing`() {
        val lines = groupInvitePageLines(GroupInvitePreviewUi(null, null, 5))
        assertEquals("You are invited to a group", lines.headline.en)
        assertNull(lines.groupLine)
        assertEquals("5 members", lines.membersLine?.en)
    }

    @Test
    fun `one member is singular`() {
        assertEquals("1 member", groupInvitePageLines(GroupInvitePreviewUi("Solo", "Anna", 1)).membersLine?.en)
        assertEquals("1 member", memberCountLine(1).en)
        assertEquals("0 members", memberCountLine(0).en)
        assertEquals("2 members", memberCountLine(2).en)
    }

    @Test
    fun `blank names are absent`() {
        val lines = groupInvitePageLines(GroupInvitePreviewUi("  ", " ", 4))
        assertEquals("You are invited to a group", lines.headline.en)
        assertNull(lines.groupLine)
    }

    @Test
    fun `no preview at all reads You are invited to a group and nothing else`() {
        val lines = groupInvitePageLines(null)
        assertEquals("You are invited to a group", lines.headline.en)
        assertNull(lines.groupLine)
        assertNull(lines.membersLine)
    }

    @Test
    fun `the foot names the inviter from the preview, and is gone with no name`() {
        val named = GroupInvitePreviewUi(groupName = "Trip", inviterName = "Anna", memberCount = 3)
        assertEquals("Invited by Anna", invitedByLine(named)?.en)
        assertNull(invitedByLine(named.copy(inviterName = "  ")))
        assertNull(invitedByLine(null))
    }

    // ADR-0087 15.2 — seen live 2026-11-12: a publisher link opened as
    // "Icons QA invites you to a group, 0 members".
    @Test
    fun `a co-publisher link reads as an invitation to publish, with no member count`() {
        val channel = ByteArray(16) { 0x42 }
        val lines = groupInvitePageLines(GroupInvitePreviewUi("Walk 22 channel", "Icons QA", 0, channelId = channel))
        assertEquals("Icons QA invites you to be a publisher of Walk 22 channel.", lines.headline.en)
        assertNull(lines.groupLine)
        assertNull(lines.membersLine)
        assertEquals(
            "You are invited to be a publisher of Channel.",
            groupInvitePageLines(GroupInvitePreviewUi(null, null, 0, channelId = channel)).headline.en,
        )
    }
}
