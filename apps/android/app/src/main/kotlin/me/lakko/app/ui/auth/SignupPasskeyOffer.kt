// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import android.util.Log
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.FreshAuthRequired
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.webauthn.WebauthnClient
import me.lakko.app.webauthn.WebauthnUserCancelledError
import me.lakko.app.webauthn.addPasskeyWithKeys
import me.lakko.app.webauthn.addedPasskeyText

/**
 * The passkey row of "Protect your account" (ADR-0075, UX 19.1a): "Add a
 * passkey so you can sign in with your face, fingerprint or screen lock."
 * Skippable, and nothing here can fail the sign-up: the account exists. Every outcome, a
 * failure included, leaves "Not now" available.
 *
 * The passkey restores keys when it can, with no choice (UX 10.2, amended
 * 2026-11-11): the keys are stored for it right after the system sheet
 * created it ([addPasskeyWithKeys]), and the line says how that ended.
 * Offered only to an account with a username (a passkey is registered under
 * it; a guest account has none).
 *
 * Adding a passkey needs a fresh-auth proof (UX section 17). The sign-up
 * minted one; when the person took more than ten minutes over the phrase it
 * has run out, and the same proof block as Settings > Passkeys sits above
 * the button. "Not now" never needs it.
 */
@Composable
public fun SignupPasskeyOffer(
    repository: AuthRepository,
    onDone: () -> Unit,
    modifier: Modifier = Modifier,
    /** Told once the passkey exists (the "Protect your account" step marks
     *  its row done), before its keys are stored. */
    onAdded: () -> Unit = {},
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val nickname = t("auth.signup.passkey.default_name")
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<UiText?>(null) }
    // After the passkey exists: the line that says what the passkey does.
    var result by remember { mutableStateOf<UiText?>(null) }
    val proof = rememberProofController(repository, ProofSurface.AddPasskey)

    AuthScaffold(title = t("auth.signup.passkey.title"), onBack = null, modifier = modifier) {
        val shown = result
        when {
            shown != null -> {
                Lead(shown.text())
                Spacer(Modifier.height(24.dp))
                BusyButton(label = t("common.done"), busy = false, onClick = onDone)
            }
            else -> {
                Lead(t("auth.signup.passkey.lede"))
                val view = proof.view()
                val add: () -> Unit = add@{
                    if (busy || !proof.view().mainEnabled) return@add
                    busy = true
                    error = null
                    scope.launch {
                        try {
                            // The proof first; the registration only after.
                            if (!proof.ensure(context)) return@launch
                            val passkey = WebauthnClient().use { client ->
                                addPasskeyWithKeys(context, repository, nickname, client, onAdded = onAdded)
                            }
                            // Every outcome is said; the passkey exists in each.
                            result = addedPasskeyText(passkey.keys, nickname)
                        } catch (_: FreshAuthRequired) {
                            // The proof ran out in between: nothing was
                            // added, the block appears, no error.
                            proof.onProofRefused()
                        } catch (_: WebauthnUserCancelledError) {
                            // Dismissed: back to the offer.
                        } catch (_: me.lakko.app.webauthn.PasskeyAlreadyExistsError) {
                            // UX 10.2: this device has one for the account
                            // already; never "cannot create passkeys".
                            error = UiText("settings.passkeys.error.already_exists")
                        } catch (cause: Throwable) {
                            if (cause is kotlinx.coroutines.CancellationException) throw cause
                            Log.w("lakko-auth", "adding a passkey at sign-up failed: ${cause.javaClass.simpleName}")
                            error = UiText("auth.signup.passkey.error")
                        } finally {
                            busy = false
                        }
                    }
                }
                if (view.visible) Spacer(Modifier.height(16.dp))
                ProofBlock(controller = proof, onSubmit = add)
                FormError(error)
                Spacer(Modifier.height(24.dp))
                BusyButton(
                    label = t("auth.signup.passkey.add"),
                    busy = busy,
                    busyLabel = if (view.working) t("common.checking") else t("recovery.passkey.waiting"),
                    enabled = view.mainEnabled,
                    onClick = add,
                )
                TextButton(onClick = onDone, enabled = !busy, modifier = Modifier.fillMaxWidth()) {
                    Text(t("common.not_now"))
                }
            }
        }
        Spacer(Modifier.height(16.dp))
    }
}
