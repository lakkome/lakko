// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.auth.GuestScreen
import me.lakko.app.auth.guestScreen
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text

/**
 * What a guest is shown outside a call (`docs/ux/call-links.md` section 9).
 *
 * A guest account is made for a person who opened a call link without an
 * account. They were not asked to save a recovery phrase and they were not
 * asked to choose a username: they went into the call. This is the one
 * question, asked once the call is over:
 *
 *   - `Create account`: a username and a password, then the recovery phrase
 *     (the app root shows it, as after any signup);
 *   - `No, thanks`: the account is deleted at once, on the servers and here.
 *     There is no confirmation dialog: the line under the button says what
 *     it does.
 *
 * Two forms of it:
 *
 *   - [GuestAccountOffer]: the block, drawn by the call-link landing under
 *     "The call has ended.";
 *   - [GuestAccountScreen]: the whole screen, drawn in place of the app,
 *     which a guest never lands in. While a call runs it shows only the way
 *     back to the call.
 *
 * @param claimedUsername the username an earlier `Create account` already
 *   took for this guest: shown, never asked again.
 */
@Composable
public fun GuestAccountOffer(
    viewModel: GuestAccountOfferViewModel,
    claimedUsername: String?,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val busy = state.busy != GuestOfferBusy.NONE

    BackHandler(enabled = state.step == GuestOfferStep.FORM && !busy) { viewModel.onBackToAsk() }
    // A guest account past its lifetime is not offered anything: it is
    // deleted, and whoever drew this block lands on the signed-out screen.
    LaunchedEffect(viewModel) { viewModel.onShown() }

    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        when (state.step) {
            GuestOfferStep.ASK -> {
                Text(t("guest.offer.title"), style = MaterialTheme.typography.titleLarge)
                Text(t("guest.offer.body"), style = MaterialTheme.typography.bodyMedium)
                FormError(state.error)
                if (state.deleteFailed) FormError(t("guest.offer.delete_failed"))
                if (!state.cannotKeep) {
                    Button(
                        onClick = viewModel::onChooseCreate,
                        enabled = !busy,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(t("auth.signup.submit"))
                    }
                }
                OutlinedButton(
                    onClick = viewModel::onDecline,
                    enabled = !busy,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    if (state.busy == GuestOfferBusy.DELETING) {
                        CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
                        Spacer(Modifier.width(8.dp))
                        Text(t("guest.offer.deleting"))
                    } else {
                        Text(t("guest.offer.decline"))
                    }
                }
                Lead(t("guest.offer.decline_note"))
            }
            GuestOfferStep.FORM -> {
                Text(t("auth.signup.title"), style = MaterialTheme.typography.titleLarge)
                Lead(t("guest.claim.lede"))
                if (claimedUsername != null) {
                    Lead(t("guest.claim.username_taken_for_you", mapOf("username" to claimedUsername)))
                } else {
                    OutlinedTextField(
                        value = state.username,
                        onValueChange = viewModel::onUsernameChange,
                        label = { Text(t("auth.username.label")) },
                        singleLine = true,
                        enabled = !busy,
                        isError = state.usernameError != null,
                        supportingText = {
                            val line = (state.usernameError ?: state.usernameHint)?.text()
                                ?: t("auth.signup.username.hint")
                            Text(line)
                        },
                        keyboardOptions = KeyboardOptions(
                            capitalization = KeyboardCapitalization.None,
                            autoCorrectEnabled = false,
                            keyboardType = KeyboardType.Ascii,
                            imeAction = ImeAction.Next,
                        ),
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
                PasswordField(
                    value = state.password,
                    onValueChange = viewModel::onPasswordChange,
                    label = t("auth.password.label"),
                    enabled = !busy,
                    supportingText = t(
                        "auth.password.hint",
                        emptyMap(),
                        mapOf("count" to SignupViewModel.MIN_PASSWORD_LEN.toULong()),
                    ),
                )
                PasswordField(
                    value = state.confirm,
                    onValueChange = viewModel::onConfirmChange,
                    label = t("auth.password.confirm.label"),
                    enabled = !busy,
                    imeAction = ImeAction.Done,
                    onDone = viewModel::onCreate,
                )
                FormError(state.error)
                BusyButton(
                    label = t("auth.signup.submit"),
                    busy = state.busy == GuestOfferBusy.CREATING,
                    busyLabel = t("guest.claim.working"),
                    onClick = viewModel::onCreate,
                    enabled = !busy,
                )
                TextButton(
                    onClick = viewModel::onBackToAsk,
                    enabled = !busy,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(t("common.back"))
                }
            }
        }
    }
}

/**
 * The whole screen a guest sees in place of the app.
 *
 * While a call runs the person belongs in it: the screen says so and offers
 * the way back. The question is asked only when no call is running, so the
 * button that deletes the account is never one press away from a live call.
 */
@Composable
public fun GuestAccountScreen(
    viewModel: GuestAccountOfferViewModel,
    claimedUsername: String?,
    callInProgress: Boolean,
    onReturnToCall: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(modifier = modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .windowInsetsPadding(WindowInsets.safeDrawing)
                .verticalScroll(rememberScrollState())
                .padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            when (guestScreen(guest = true, callInProgress = callInProgress)) {
                GuestScreen.IN_CALL -> {
                    Text(t("guest.in_call.title"), style = MaterialTheme.typography.titleLarge)
                    Spacer(Modifier.height(4.dp))
                    Button(onClick = onReturnToCall, modifier = Modifier.fillMaxWidth()) {
                        Text(t("guest.in_call.return"))
                    }
                }
                GuestScreen.OFFER -> GuestAccountOffer(viewModel = viewModel, claimedUsername = claimedUsername)
                GuestScreen.NONE -> Unit
            }
        }
    }
}
