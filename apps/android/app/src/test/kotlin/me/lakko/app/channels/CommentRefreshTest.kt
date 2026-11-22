// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * `docs/ux/group-and-channel-privacy.md` 13.2 (defect seen live 2026-11-22:
 * a comment written on another device did not reach a channel open on
 * screen) — what the open channel's tick does for one post's thread. The
 * same table as web `comment-thread.test.ts` and iOS
 * `ChannelCommentRefreshTests`.
 */
class CommentRefreshTest {
    private fun r(
        firstRead: Boolean,
        expanded: Boolean,
        morePages: Boolean,
        rowsRead: Int,
        countAtRead: Long?,
        server: Long?,
    ) = commentRefresh(firstRead, expanded, morePages, rowsRead, countAtRead, server)

    @Test
    fun theSharedTable() {
        assertEquals("nothing before the first read", CommentRefresh.NONE, r(false, false, false, 0, null, 3))
        assertEquals("no count, open, read to the end", CommentRefresh.NEWER, r(true, true, false, 2, null, null))
        assertEquals("no count, collapsed", CommentRefresh.NONE, r(true, false, false, 2, null, null))
        assertEquals("no count, open, pages unread", CommentRefresh.NONE, r(true, true, true, 20, null, null))
        assertEquals("count as read", CommentRefresh.NONE, r(true, false, false, 2, 2, 2))
        assertEquals("count up, collapsed", CommentRefresh.NEWER, r(true, false, false, 1, 1, 2))
        assertEquals("count up, open", CommentRefresh.NEWER, r(true, true, false, 1, 1, 2))
        assertEquals("count up, pages unread", CommentRefresh.NONE, r(true, true, true, 20, 25, 26))
        assertEquals("count down, collapsed", CommentRefresh.REREAD, r(true, false, false, 3, 3, 2))
        assertEquals("count down, open", CommentRefresh.REBASE, r(true, true, false, 3, 3, 2))
        assertEquals("the rows read stand in", CommentRefresh.NONE, r(true, false, false, 2, null, 2))
        assertEquals("one more than the rows read", CommentRefresh.NEWER, r(true, false, false, 2, null, 3))
    }
}
