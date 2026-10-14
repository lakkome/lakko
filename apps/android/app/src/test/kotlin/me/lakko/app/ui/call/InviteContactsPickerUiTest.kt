// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.activity.ComponentActivity
import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import me.lakko.app.calls.inviteContactRows
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.en
import me.lakko.app.ui.contacts.ContactRowModel
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * `Invite contacts`, drawn (`docs/ux/call-links.md` section 3): the person's
 * contacts with their usernames, an invited row that cannot be chosen twice,
 * and the sentence a person with no contacts reads.
 */
@RunWith(RobolectricTestRunner::class)
class InviteContactsPickerUiTest {
    init {
        // ADR-0061: the words under test are the English catalog's.
        EnglishForTests.install()
    }

    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private val rows = inviteContactRows(
        listOf(
            ContactRowModel(key = "k-zoe", name = "Zoe", username = "zoe_k"),
            ContactRowModel(key = "k-anna", name = "Anna", username = null),
        ),
    )

    private val picked = mutableListOf<String>()

    private fun show(
        rows: List<ContactRowModel> = this.rows,
        busyKey: String? = null,
        invited: Set<String> = emptySet(),
        error: UiText? = null,
    ) {
        compose.setContent {
            MaterialTheme {
                InviteContactsPicker(
                    rows = rows,
                    busyKey = busyKey,
                    invitedKeys = invited,
                    error = error,
                    onBack = {},
                    onInvite = { picked += it.key },
                )
            }
        }
    }

    @Test
    fun theListIsTheContactsUnderTheInviteTitle() {
        show()
        compose.onNodeWithText(en("call.invite.picker_title")).assertIsDisplayed()
        compose.onNodeWithText("Anna").assertIsDisplayed()
        compose.onNodeWithText("Zoe").assertIsDisplayed()
        compose.onNodeWithText("@zoe_k").assertIsDisplayed()
    }

    @Test
    fun choosingAContactInvitesThatContact() {
        show()
        compose.onNodeWithText("Anna").performClick()
        compose.waitForIdle()
        assertEquals(listOf("k-anna"), picked)
    }

    @Test
    fun anInvitedContactSaysSoAndCannotBeChosenTwice() {
        show(invited = setOf("k-zoe"))
        compose.onNodeWithText(en("call.invite.row_invited")).assertIsDisplayed()
        compose.onNodeWithText("Zoe").performClick()
        compose.waitForIdle()
        assertEquals(emptyList<String>(), picked)
        // The others stay pressable: the list is for inviting several people.
        compose.onNodeWithText("Anna").performClick()
        compose.waitForIdle()
        assertEquals(listOf("k-anna"), picked)
    }

    @Test
    fun whileOneInvitationIsBeingSentNothingElseCanBeChosen() {
        show(busyKey = "k-zoe")
        compose.onNodeWithText(en("common.sending")).assertIsDisplayed()
        compose.onNodeWithText("Anna").performClick()
        compose.waitForIdle()
        assertEquals(emptyList<String>(), picked)
    }

    @Test
    fun aFailureIsOneSentenceAboveTheListAndTheRowsStay() {
        show(error = UiText("call.invite.send_failed"))
        compose.onNodeWithText(en("call.invite.send_failed")).assertIsDisplayed()
        compose.onNodeWithText("Anna").performClick()
        compose.waitForIdle()
        assertEquals(listOf("k-anna"), picked)
    }

    @Test
    fun withNoContactsTheListSaysWhatToDoInstead() {
        show(rows = emptyList())
        compose.onNodeWithText(en("call.invite.picker_empty")).assertIsDisplayed()
        // Nothing to search either.
        compose.onNodeWithText(en("chats.search.placeholder")).assertDoesNotExist()
    }
}
