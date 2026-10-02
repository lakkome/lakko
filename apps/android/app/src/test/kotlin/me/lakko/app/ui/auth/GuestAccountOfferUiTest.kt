// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.activity.ComponentActivity
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * What a guest is drawn outside a call (`docs/ux/call-links.md` section 9):
 * the question with its two buttons and the line that says what `No, thanks`
 * does, the form behind `Create account`, and, while a call runs, only the
 * way back to it.
 */
@RunWith(RobolectricTestRunner::class)
class GuestAccountOfferUiTest {
    init {
        // ADR-0061: the words under test are the English catalog's.
        EnglishForTests.install()
    }

    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private var discards = 0
    private var returns = 0

    private fun model(claimed: String? = null) = GuestAccountOfferViewModel(
        claim = { _, _ -> },
        discard = { discards += 1 },
        claimedUsername = { claimed },
        validateUsername = { it.trim().lowercase() },
        log = { _, _ -> },
    )

    @Composable
    private fun ScreenUnderTest(viewModel: GuestAccountOfferViewModel, claimed: String?, callInProgress: Boolean) {
        MaterialTheme {
            GuestAccountScreen(
                viewModel = viewModel,
                claimedUsername = claimed,
                callInProgress = callInProgress,
                onReturnToCall = { returns += 1 },
            )
        }
    }

    @Test
    fun theQuestionShowsBothAnswersAndSaysWhatDecliningDoes() {
        val viewModel = model()
        compose.setContent { ScreenUnderTest(viewModel, claimed = null, callInProgress = false) }

        compose.onNodeWithText(en("guest.offer.title")).assertIsDisplayed()
        compose.onNodeWithText(en("guest.offer.body")).assertIsDisplayed()
        compose.onNodeWithText(en("auth.signup.submit")).assertIsDisplayed()
        compose.onNodeWithText(en("guest.offer.decline")).assertIsDisplayed()
        compose.onNodeWithText(en("guest.offer.decline_note")).assertIsDisplayed()
    }

    @Test
    fun noThanksDeletesAtOnceWithNoConfirmationInBetween() {
        val viewModel = model()
        compose.setContent { ScreenUnderTest(viewModel, claimed = null, callInProgress = false) }

        compose.onNodeWithText(en("guest.offer.decline")).performClick()
        compose.waitForIdle()

        // One press is the whole of it: the line under the button said what
        // it does, and nothing asked again.
        assertEquals(1, discards)
    }

    @Test
    fun whileACallRunsOnlyTheWayBackIsOffered() {
        val viewModel = model()
        compose.setContent { ScreenUnderTest(viewModel, claimed = null, callInProgress = true) }

        compose.onNodeWithText(en("guest.in_call.title")).assertIsDisplayed()
        // The button that deletes the account is never one press away from a
        // live call.
        compose.onNodeWithText(en("guest.offer.decline")).assertDoesNotExist()
        compose.onNodeWithText(en("guest.offer.title")).assertDoesNotExist()

        compose.onNodeWithText(en("guest.in_call.return")).performClick()
        compose.waitForIdle()
        assertEquals(1, returns)
        assertEquals(0, discards)
    }

    @Test
    fun createAccountOpensTheFormWithAUsernameField() {
        val viewModel = model()
        compose.setContent { ScreenUnderTest(viewModel, claimed = null, callInProgress = false) }

        compose.onNodeWithText(en("auth.signup.submit")).performClick()
        compose.waitForIdle()

        compose.onNodeWithText(en("auth.signup.title")).assertIsDisplayed()
        compose.onNodeWithText(en("guest.claim.lede")).assertIsDisplayed()
        compose.onNodeWithText(en("auth.username.label")).assertIsDisplayed()
        compose.onNodeWithText(en("auth.password.label")).assertIsDisplayed()
        // The rest of the form is below the fold of the test window; the
        // column scrolls.
        compose.onNodeWithText(en("auth.password.confirm.label")).assertExists()
        compose.onNodeWithText(en("common.back")).assertExists()
        compose.onNodeWithText(en("guest.offer.decline")).assertDoesNotExist()
    }

    @Test
    fun theOfferOpensNoDialogOfAnyKind() {
        // The design system's rule: no operating-system surface inside a Lakko
        // screen, and here no confirmation at all. Read off the source, since
        // a dialog that is never opened leaves nothing in a semantics tree.
        val root = listOf(java.io.File("src/main/kotlin"), java.io.File("app/src/main/kotlin")).first { it.isDirectory }
        val files = listOf("GuestAccountOffer.kt", "GuestAccountOfferViewModel.kt")
            .map { java.io.File(root, "me/lakko/app/ui/auth/$it") }
        for (file in files) {
            val code = file.readLines().filterNot { line ->
                val trimmed = line.trim()
                trimmed.startsWith("//") || trimmed.startsWith("*") || trimmed.startsWith("/*")
            }
            assertEquals(file.name, emptyList<String>(), code.filter { "Dialog" in it || "BottomSheet" in it })
        }
    }

    @Test
    fun aUsernameAlreadyTakenForTheGuestIsShownAndNotAskedAgain() {
        val viewModel = model(claimed = "mira")
        compose.setContent { ScreenUnderTest(viewModel, claimed = "mira", callInProgress = false) }

        compose.onNodeWithText(en("auth.signup.submit")).performClick()
        compose.waitForIdle()

        compose.onNodeWithText("Your username is @mira.").assertIsDisplayed()
        compose.onNodeWithText(en("auth.username.label")).assertDoesNotExist()
    }
}
