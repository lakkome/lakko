// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** docs/ux/profile-screen.md §3-§6 — the pending/saved model, run without
 *  the core (the rules are injected). */
class ProfileEditModelTest {
    /** The display-name rule: trim; a line break is refused; empty is no name. */
    private val nameRule: (String) -> DisplayNameCheck = { s ->
        if (s.contains('\n')) DisplayNameCheck.Invalid(DisplayNameProblem.LINE_BREAK)
        else DisplayNameCheck.Valid(s.trim().ifEmpty { null })
    }

    /** The username rule: lowercase `[a-z0-9_]{3,32}`. */
    private val usernameRule: (String) -> UsernameCheck = { s ->
        val n = s.trim().removePrefix("@").lowercase()
        when {
            n.isEmpty() -> UsernameCheck.Invalid(UsernameProblem.EMPTY)
            n.length < 3 -> UsernameCheck.Invalid(UsernameProblem.TOO_SHORT)
            !n.all { it in 'a'..'z' || it in '0'..'9' || it == '_' } ->
                UsernameCheck.Invalid(UsernameProblem.INVALID_CHARACTERS)
            else -> UsernameCheck.Valid(n)
        }
    }

    private fun edit(name: String = "Alice", user: String = "alice") =
        ProfileEdit(InlineField(name), InlineField(user))

    private fun ProfileEdit.enabled() = saveEnabled(nameRule, usernameRule)

    private val blob = AvatarBlob("a".repeat(64), "k", "n", 100, 320)

    @Test
    fun nothing_changed_disables_save() {
        assertFalse(edit().enabled())
        assertEquals(LeaveDecision.LEAVE, leaveDecision(edit(), nameRule, usernameRule))
    }

    @Test
    fun a_new_name_enables_save_and_changing_it_back_disables_it() {
        val opened = edit().copy(name = edit().name.opened().typed("Alice Petrova"))
        assertTrue(opened.enabled())
        val back = opened.copy(name = opened.name.typed("Alice"))
        assertFalse(back.enabled())
        // Whitespace around the saved name is the same name.
        assertFalse(opened.copy(name = opened.name.typed("  Alice ")).enabled())
    }

    @Test
    fun an_open_invalid_field_disables_save_even_with_another_change() {
        val e = edit().copy(
            photo = PhotoEdit.Removed,
            savedAvatar = AvatarRefs(blob),
            name = edit().name.opened().typed("two\nlines"),
        )
        assertFalse(e.enabled())
        assertEquals(LeaveDecision.ASK, leaveDecision(e, nameRule, usernameRule))
    }

    @Test
    fun removing_a_photo_that_is_not_set_is_no_change() {
        assertFalse(edit().copy(photo = PhotoEdit.Removed).enabled())
        assertTrue(edit().copy(photo = PhotoEdit.Removed, savedAvatar = AvatarRefs(blob)).enabled())
    }

    @Test
    fun a_new_photo_is_a_change() {
        val e = edit().copy(photo = PhotoEdit.New(byteArrayOf(1), 100))
        assertTrue(e.enabled())
        assertEquals(LeaveDecision.ASK, leaveDecision(e, nameRule, usernameRule))
    }

    @Test
    fun a_running_save_stays_and_disables_save() {
        val e = edit().copy(photo = PhotoEdit.New(byteArrayOf(1), 100), saving = true)
        assertFalse(e.enabled())
        assertEquals(LeaveDecision.STAY, leaveDecision(e, nameRule, usernameRule))
    }

    @Test
    fun username_change_is_compared_canonically() {
        val open = edit().copy(username = edit().username.opened().typed("ALICE"))
        assertFalse(open.enabled())
        val renamed = open.copy(username = open.username.typed("alice2"))
        assertTrue(renamed.enabled())
        val invalid = open.copy(username = open.username.typed("a!"))
        assertFalse(invalid.enabled())
    }

    @Test
    fun field_state_machine_opens_closes_reverts_and_stays_open_when_invalid() {
        val f = InlineField("Alice")
        val open = f.opened()
        assertTrue(open.open)
        val typed = open.typed("Bob")
        // Leaving while invalid keeps it open with what was typed.
        assertEquals(typed, typed.closed(valid = false))
        // Leaving while valid closes it; the row shows the new value.
        val closed = typed.closed(valid = true)
        assertFalse(closed.open)
        assertEquals("Bob", closed.value)
        // Escape returns to what it was before THIS edit.
        val reopened = closed.opened().typed("Carol")
        val reverted = reopened.reverted()
        assertFalse(reverted.open)
        assertEquals("Bob", reverted.value)
    }

    @Test
    fun an_untouched_row_follows_a_saved_value_from_another_device() {
        assertEquals(InlineField("New"), InlineField("Old").withSaved("New"))
        val edited = InlineField("Old").opened().typed("Mine")
        val followed = edited.withSaved("New")
        assertEquals("Mine", followed.value)
        assertEquals("New", followed.saved)
    }

    @Test
    fun save_steps_run_in_the_spec_order() {
        val e = edit().copy(
            name = InlineField("Alice").typed("Al"),
            username = InlineField("alice").typed("al_2"),
            photo = PhotoEdit.New(byteArrayOf(1), 100),
        )
        assertEquals(
            listOf(SaveStep.UPLOAD_PHOTO, SaveStep.CHANGE_USERNAME, SaveStep.PUBLISH),
            e.saveSteps(nameRule, usernameRule),
        )
        // An upload that already succeeded is not repeated on a retry.
        val retried = e.copy(photo = PhotoEdit.New(byteArrayOf(1), 100, uploaded = AvatarRefs(blob)))
        assertEquals(listOf(SaveStep.CHANGE_USERNAME, SaveStep.PUBLISH), retried.saveSteps(nameRule, usernameRule))
        // A username change alone publishes nothing itself (the rename does).
        val onlyUser = edit().copy(username = InlineField("alice").typed("bob"))
        assertEquals(listOf(SaveStep.CHANGE_USERNAME), onlyUser.saveSteps(nameRule, usernameRule))
    }

    @Test
    fun the_published_avatar_follows_the_photo_edit() {
        val saved = AvatarRefs(blob)
        assertEquals(saved, edit().copy(savedAvatar = saved).avatarToPublish())
        assertEquals(null, edit().copy(savedAvatar = saved, photo = PhotoEdit.Removed).avatarToPublish())
        val fresh = AvatarRefs(blob.copy(blobIdHex = "b".repeat(64)))
        assertEquals(fresh, edit().copy(photo = PhotoEdit.New(byteArrayOf(1), 1, uploaded = fresh)).avatarToPublish())
    }

    @Test
    fun after_saved_nothing_is_pending() {
        val e = edit().copy(photo = PhotoEdit.New(byteArrayOf(1), 1)).afterSaved("Al", "alice", AvatarRefs(blob))
        assertFalse(e.enabled())
        assertEquals("Al", e.name.saved)
        assertEquals(AvatarRefs(blob), e.savedAvatar)
    }
}
