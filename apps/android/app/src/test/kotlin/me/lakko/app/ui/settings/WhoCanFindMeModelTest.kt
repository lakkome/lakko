// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.settings

import me.lakko.app.l10n.en
import me.lakko.app.reach.ReachApplyStatus
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiContactsForm
import uniffi.lakko_ffi_uniffi.FfiVisibilityMode
import uniffi.lakko_ffi_uniffi.FfiVisibilitySetting

/** `docs/ux/who-can-find-me.md` sections 3, 4 and 8 — what the screen shows. */
class WhoCanFindMeModelTest {

    private val g1 = FindMeScope(FindMeScopeKind.GROUP, "AA11", "Hiking")
    private val g2 = FindMeScope(FindMeScopeKind.GROUP, "bb22", "alpha")
    private val c1 = FindMeScope(FindMeScopeKind.CHANNEL, "cc33", "News")

    @Test
    fun `the parts show only under Some people`() {
        val nobody = findMeRows(FindMeModel(FindMeMode.NOBODY, false, null, emptySet()), listOf(g1))
        assertFalse(nobody.showParts)
        assertFalse(nobody.showForm)
        val some = findMeRows(FindMeModel(FindMeMode.SOME, true, null, emptySet()), listOf(g1))
        assertTrue(some.showParts)
        assertFalse(some.showForm)
        val everyone = findMeRows(FindMeModel(FindMeMode.EVERYONE, false, FindMeForm.ALL, emptySet()), listOf(g1))
        assertFalse(everyone.showParts)
        assertFalse(everyone.showForm)
    }

    @Test
    fun `the form shows only while members is on`() {
        val rows = findMeRows(FindMeModel(FindMeMode.SOME, false, FindMeForm.ALL, emptySet()), listOf(g1))
        assertTrue(rows.showForm)
        assertEquals(FIND_ME_MEMBERS_ALL_HELPER, rows.membersHelper)
    }

    @Test
    fun `chosen counts N of M live, and a scope left since is not counted`() {
        val model = FindMeModel(
            FindMeMode.SOME,
            false,
            FindMeForm.CHOSEN,
            setOf(g1.chosenKey(), "GROUP:dead"),
        )
        val rows = findMeRows(model, listOf(g1, g2, c1))
        assertEquals(1, rows.chosenCount)
        assertEquals(3, rows.scopeCount)
        assertEquals("Only the ones you choose (1 of 3).", rows.membersHelper.en)
    }

    @Test
    fun `a chosen key is case-insensitive in the id and names its kind`() {
        assertEquals("GROUP:aa11", g1.chosenKey())
        assertEquals("CHANNEL:cc33", c1.chosenKey())
    }

    @Test
    fun `groups list before channels, each by name`() {
        assertEquals(listOf(g2, g1, c1), sortedFindMeScopes(listOf(c1, g1, g2)))
    }

    @Test
    fun `the status line says what has not happened yet`() {
        assertNull(findMeStatusLine(ReachApplyStatus.APPLIED))
        assertNull(findMeStatusLine(ReachApplyStatus.APPLYING))
        assertEquals(FIND_ME_OFFLINE, findMeStatusLine(ReachApplyStatus.OFFLINE))
        assertEquals(FIND_ME_FAILED, findMeStatusLine(ReachApplyStatus.FAILED))
    }

    private fun contact(idHex: String, name: String, placeholder: Boolean = false, username: String? = null) =
        FindMeContact(idHex, "k$idHex", name, username, placeholder)

    @Test
    fun `the contacts form shows only under the part`() {
        val on = FindMeModel(FindMeMode.SOME, true, null, emptySet())
        assertTrue(findMeRows(on, emptyList()).showContactsForm)
        assertFalse(findMeRows(on.copy(contactsOfContacts = false), emptyList()).showContactsForm)
        assertFalse(findMeRows(on.copy(mode = FindMeMode.EVERYONE), emptyList()).showContactsForm)
    }

    @Test
    fun `the helper says N of M under chosen and the usual text under all`() {
        val model = FindMeModel(
            FindMeMode.SOME, true, null, emptySet(),
            contactsForm = FindMeForm.CHOSEN,
            chosenContacts = setOf("aa", "gone"),
        )
        val helper = findMeRows(model, emptyList(), listOf(contact("aa", "Ann"), contact("bb", "Bob"))).contactsHelper
        assertEquals("who_can_find_me.contacts_chosen_helper", helper.key)
        assertEquals(mapOf("chosen" to "1", "total" to "2"), helper.texts)
        assertEquals(
            FIND_ME_FOF_HELPER,
            findMeRows(model.copy(contactsForm = FindMeForm.ALL), emptyList()).contactsHelper,
        )
    }

    @Test
    fun `nobody switched on is zero of M`() {
        val model = FindMeModel(FindMeMode.SOME, true, null, emptySet(), contactsForm = FindMeForm.CHOSEN)
        val helper = findMeRows(model, emptyList(), listOf(contact("aa", "Ann"))).contactsHelper
        assertEquals(mapOf("chosen" to "0", "total" to "1"), helper.texts)
    }

    @Test
    fun `a contact's switch shows on under all and its own state under chosen`() {
        val model = FindMeModel(FindMeMode.SOME, true, null, emptySet())
        assertTrue(contactSwitchOn(model, "AA"))
        val chosen = model.copy(contactsForm = FindMeForm.CHOSEN, chosenContacts = setOf("aa"))
        assertTrue(contactSwitchOn(chosen, "AA"))
        assertFalse(contactSwitchOn(chosen, "bb"))
    }

    @Test
    fun `the switch on a contact needs the part on, under either form`() {
        val model = FindMeModel(FindMeMode.SOME, true, null, emptySet())
        assertTrue(model.contactsPartOn)
        assertTrue(model.copy(contactsForm = FindMeForm.CHOSEN).contactsPartOn)
        assertFalse(model.copy(contactsOfContacts = false).contactsPartOn)
        assertFalse(model.copy(mode = FindMeMode.NOBODY).contactsPartOn)
    }

    @Test
    fun `contacts sort as the Contacts screen sorts them, an unnamed one by username under the hash`() {
        val sorted = sortedFindMeContacts(
            listOf(
                contact("cc", "Zoe"),
                contact("aa", "anna"),
                contact("bb", "Member", placeholder = true, username = "1bot"),
            ),
        )
        assertEquals(listOf("aa", "cc", "bb"), sorted.map { it.accountIdHex })
    }

    @Test
    fun `the model reads the form and the ids from the setting`() {
        val id = ByteArray(16) { 0xAB.toByte() }
        val model = FfiVisibilitySetting(
            mode = FfiVisibilityMode.SOME,
            contactsOfContacts = true,
            contactsForm = FfiContactsForm.CHOSEN,
            chosenContacts = listOf(id),
            groups = null,
            chosen = emptyList(),
            lastContactsOfContacts = true,
            lastGroups = null,
            updatedAtMs = 1uL,
        ).toModel()
        assertEquals(FindMeForm.CHOSEN, model.contactsForm)
        assertEquals(setOf("ab".repeat(16)), model.chosenContacts)
    }
}
