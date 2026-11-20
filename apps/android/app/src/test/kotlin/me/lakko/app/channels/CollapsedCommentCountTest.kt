// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.lakko_ffi_uniffi.CollapsedComments

/**
 * Section 13.2 — `Show {n} more comments` never counts a comment this reader
 * hides (base walk 2026-11-13, Pixel: "Show 2 more comments" over a banned
 * author's two comments, and none when opened).
 *
 * The rule itself is `lakko_ui_rules::comments` and is tested there (unit
 * tests and two property tests) and across the web and iOS boundaries. It is
 * an FFI call needing `liblakko_ffi_uniffi.so`, which the host JVM does not
 * load, so this suite covers the client's half: the answer's mapping to what
 * the post draws. The binding's types are ordinary classes and load nothing.
 */
class CollapsedCommentCountTest {
    @Test
    fun everyAnswerOfTheSharedRuleMapsToWhatThePostDraws() {
        assertEquals(CollapsedMore.None, collapsedMoreOf(CollapsedComments.Nothing))
        assertEquals(CollapsedMore.Count(13), collapsedMoreOf(CollapsedComments.Count(13u)))
        assertEquals(CollapsedMore.Unknown, collapsedMoreOf(CollapsedComments.Unknown))
        assertEquals(CollapsedMore.Comment, collapsedMoreOf(CollapsedComments.Comment))
    }
}
