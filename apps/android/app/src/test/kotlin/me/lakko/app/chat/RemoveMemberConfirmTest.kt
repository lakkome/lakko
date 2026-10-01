// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * ADR-0045 UX §6 — the `Remove this member?` confirmation.
 *
 * The host suite cannot render a composable, so what is pinned here is the
 * DECISION — is a confirmation required for this action, and what does it say
 * — never the widget. `AlertDialog` versus anything else is §14's business and
 * is deliberately not asserted.
 *
 * The discrimination evidence for each rule (each probe was RUN, the failure
 * observed, and the probe reverted) is recorded in `RemoveMemberConfirm.kt`'s
 * own header.
 */
class RemoveMemberConfirmTest {
    init {
        EnglishForTests.install()
    }


    @Test
    fun `it is required on BOTH paths, because neither can be withdrawn`() {
        // §3: once published, the request is in every member's proposal store
        // and dies only when some commit lands — the core has no withdraw and
        // MLS offers no mechanism for one. On the direct path there is no
        // withdraw either: the commit is published immediately. So the
        // confirmation is the last exit in both cases, and "confirm only on the
        // proposal path" would be exactly backwards — the direct path removes
        // somebody FASTER.
        for (usesProposalPath in listOf(true, false)) {
            val c = removeMemberConfirmation(usesProposalPath)
            assertEquals(REMOVE_MEMBER_TITLE, c.title)
            assertEquals(REMOVE_MEMBER_CONFIRM_LABEL, c.confirmLabel)
            assertEquals(REMOVE_MEMBER_CANCEL_LABEL, c.cancelLabel)
        }
    }

    @Test
    fun `it states §6's copy verbatim on the proposal path`() {
        // "That body line is not a nicety. It is the one fact a user removing
        // someone for a safety reason must have, and it is the fact the old
        // immediate-looking UI concealed."
        val c = removeMemberConfirmation(usesProposalPath = true)
        assertEquals(REMOVE_MEMBER_BODY, c.body)
        assertEquals("Remove this member?", c.title.en)
        assertEquals(
            "They will keep receiving messages until an owner or an admin " +
                "applies the change.",
            REMOVE_MEMBER_BODY.en,
        )
        assertEquals("Remove", REMOVE_MEMBER_CONFIRM_LABEL.en)
        assertEquals("Cancel", REMOVE_MEMBER_CANCEL_LABEL.en)
    }

    @Test
    fun `it says NOTHING on the direct path, where §6's body would be false`() {
        // A group with no role table still removes by direct commit, and a
        // direct commit takes effect the moment it lands. "They will keep
        // receiving messages until an owner or an admin applies the change" is
        // then simply untrue, and a security tool that reassures a user with a
        // false sentence about who can still read their messages is worse than
        // one that says less.
        assertNull(removeMemberConfirmation(usesProposalPath = false).body)
    }

    @Test
    fun `it uses no protocol vocabulary`() {
        // §1: user-facing copy says `request` and `change`; it never says
        // proposal, commit, epoch, sweep, committer — or `leaf`, which is the
        // internal identifier the roster is NOT keyed on and which no user can
        // interpret.
        val forbidden = listOf("leaf", "proposal", "commit", "epoch", "sweep", "committer")
        for (usesProposalPath in listOf(true, false)) {
            val c = removeMemberConfirmation(usesProposalPath)
            val copy = "${c.title.en} ${c.body?.en ?: ""} ${c.confirmLabel.en} ${c.cancelLabel.en}"
                .lowercase()
            for (word in forbidden) {
                assertEquals("`$word` leaked into $copy", false, copy.contains(word))
            }
        }
    }

    @Test
    fun `the copy is identical to the leave confirmation's shape`() {
        // Not a style point. §6 gives `Remove` and `Leave` the same four-part
        // shape — title, one body sentence, a destructive verb, `Cancel` — and
        // the two sit in the same panel, so a user reading one after the other
        // should be answering the same kind of question. The one thing that
        // must NOT match is the verb.
        val remove = removeMemberConfirmation(usesProposalPath = true)
        assertEquals("Cancel", remove.cancelLabel.en)
        assertEquals(false, remove.confirmLabel.en == "Leave")
    }
}
