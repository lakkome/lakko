// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.auth.RestoreNotice
import me.lakko.app.auth.base64Decode
import me.lakko.app.webauthn.restoreOffersPasskey
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.R

/** The ways to bring the keys onto this phone, in the order the prompt
 *  lists them. Never a route that only signs in. */
private enum class RestoreWay { AnotherDevice, Passkey, Phrase, Neither }

/**
 * "Restore your keys": the person is signed in but this phone does not hold
 * the account's keys yet. The rows, in order: another device, a passkey
 * (hidden when the account is known to have no passkey that carries keys),
 * the 12-word phrase (only for an account whose owner saved it), and starting over with a new identity. After a passkey
 * sign-in that brought no keys, the first line says so.
 * [RestoreIdentityPromptViewModel] keeps every behaviour: whatever brings
 * the keys is checked against the published identity before the session
 * opens, and "start over" creates a new identity behind an explicit warning.
 */
@Composable
public fun RestoreIdentityPrompt(
    viewModel: RestoreIdentityPromptViewModel,
    username: String,
    accountIdB64: String,
    onSignOut: () -> Unit,
    modifier: Modifier = Modifier,
    /** Why the prompt is up after a passkey sign-in, or `null`. */
    restoreNotice: RestoreNotice? = null,
    /** G-56 (UX section 8a): this device holds the account's current keys
     *  but not the earlier keys its chats use. Only "Another device" and
     *  "I have none of these" are offered: a passkey and the phrase bring
     *  the keys this device already has. */
    earlierKeysMissing: Boolean = false,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    var way by rememberSaveable { mutableStateOf<RestoreWay?>(null) }
    val context = LocalContext.current
    // The confirmation word of the reset warning: kept across the
    // "Resetting..." state so a wrong password does not ask for it again.
    var resetTyped by rememberSaveable { mutableStateOf("") }
    // UX 15.1b - the reset warning's factor section: the password, then one
    // second factor the account has. Its choices live for this visit only.
    val scope = androidx.compose.runtime.rememberCoroutineScope()
    val factor = rememberResetFactorController(viewModel.repository)
    val closeWay: () -> Unit = {
        if (way == RestoreWay.Neither) {
            resetTyped = ""
            factor.clear()
            viewModel.onResetClosed()
        }
        way = null
    }

    BackHandler(enabled = way != null && !state.busy) { closeWay() }

    // UX section 8 - the 12-word phrase row only for an account whose owner
    // saved one, read from the account's published protection. No mailed
    // code brings keys, so there is no email row (2026-11-26).
    val phraseSaved by androidx.compose.runtime.produceState<Boolean?>(
        initialValue = null,
        viewModel.repository,
        accountIdB64,
    ) { value = viewModel.repository.restoreKeysPhraseSaved() }
    val accountRows = me.lakko.app.auth.restoreKeysRows(phraseSaved)

    when (way) {
        null -> AuthScaffold(title = t("restore_identity.title"), onBack = null, modifier = modifier) {
            when (restoreNotice) {
                RestoreNotice.PasskeyNoKeys -> {
                    Text(t("restore_identity.from_passkey_no_keys"), style = MaterialTheme.typography.bodyLarge)
                    Spacer(Modifier.height(8.dp))
                }
                RestoreNotice.PasskeyUnusable -> {
                    Text(t("restore_identity.from_passkey_unusable"), style = MaterialTheme.typography.bodyLarge)
                    Spacer(Modifier.height(8.dp))
                }
                null -> Unit
            }
            val rows = restoreRowsFor(
                earlierKeysMissing = earlierKeysMissing,
                passkeyOffered = restoreOffersPasskey(state.passkeysWithKeys),
                phrase = accountRows.phrase,
            )
            Lead(
                t(
                    if (earlierKeysMissing) "restore_identity.earlier_keys.lede" else "restore_identity.lede",
                    mapOf("username" to "@$username"),
                ),
            )
            Spacer(Modifier.height(16.dp))
            MethodRow(
                icon = R.drawable.ic_lk_smartphone_waves,
                title = t("recovery.method.device.title"),
                subtitle = t(
                    if (earlierKeysMissing) "restore_identity.earlier_keys.another_device.sub" else "recovery.method.device.sub",
                ),
                onClick = { way = RestoreWay.AnotherDevice },
            )
            if (rows.passkey) {
                MethodRow(
                    icon = R.drawable.ic_lk_passkey,
                    title = t("recovery.method.passkey.title"),
                    subtitle = t("restore_identity.method.passkey.sub"),
                    onClick = { way = RestoreWay.Passkey },
                )
            }
            if (rows.phrase) {
                MethodRow(
                    icon = R.drawable.ic_lk_key,
                    title = t("recovery.method.phrase.title"),
                    subtitle = t("recovery.method.phrase.restore_sub"),
                    onClick = { way = RestoreWay.Phrase },
                )
            }
            MethodRow(
                icon = R.drawable.ic_lk_exclamation_circle,
                title = t("restore_identity.lost.summary"),
                subtitle = t("restore_identity.lost.sub"),
                onClick = { way = RestoreWay.Neither },
            )
            FormError(state.error)
            Spacer(Modifier.height(24.dp))
            FooterLink(
                prompt = t("restore_identity.not_you"),
                action = t("restore_identity.log_out"),
                onClick = onSignOut,
                enabled = !state.busy,
            )
        }

        RestoreWay.Passkey -> AuthScaffold(
            title = t("recovery.method.passkey.title"),
            onBack = { if (!state.busy) way = null },
            modifier = modifier,
        ) {
            Lead(t("recovery.passkey.intro"))
            FormError(state.error)
            Spacer(Modifier.height(24.dp))
            BusyButton(
                label = t("recovery.passkey.submit"),
                busy = state.busy,
                busyLabel = t("recovery.passkey.waiting"),
                // A passkey that brought no keys returns to the list, whose
                // first line then says so.
                onClick = { viewModel.restoreWithPasskey(context) { way = null } },
            )
            Spacer(Modifier.height(16.dp))
        }

        RestoreWay.Phrase -> AuthScaffold(
            title = t("recovery.method.phrase.title"),
            onBack = { if (!state.busy) way = null },
            modifier = modifier,
        ) {
            Lead(t("restore_identity.phrase_intro"))
            Spacer(Modifier.height(16.dp))
            RecoveryPhraseEntryGrid(
                phrase = state.phrase,
                onPhraseChange = viewModel::onPhraseChange,
                enabled = !state.busy,
                onDone = viewModel::onSubmit,
            )
            FormError(state.error)
            Spacer(Modifier.height(24.dp))
            BusyButton(
                label = t("restore_identity.submit"),
                busy = state.busy,
                busyLabel = t("restore_identity.submitting"),
                onClick = viewModel::onSubmit,
            )
            Spacer(Modifier.height(16.dp))
        }

        RestoreWay.AnotherDevice -> AuthScaffold(
            title = t("recovery.method.device.title"),
            onBack = { if (!state.busy) way = null },
            modifier = modifier,
        ) {
            // ADR-0056 — the code names the account by the login handle of
            // its username; an account with none (a guest) cannot pair.
            val handle = remember(username) {
                me.lakko.app.devices.ownPairingHandle(username, me.lakko.app.auth.UniffiLoginHandleSource)
            }
            if (handle == null) {
                Lead(me.lakko.app.devices.PAIRING_NO_USERNAME_COPY.text())
            } else {
                me.lakko.app.ui.devices.RestoreViaDevicePanel(
                    loginHandle = handle,
                    known = me.lakko.app.devices.PairingOrchestrator.KnownAccount(
                        accountIdBytes = base64Decode(accountIdB64),
                        accountIdB64 = accountIdB64,
                    ),
                    onRecovered = { outcome -> viewModel.completeFromPairing(outcome) },
                    // G-56 — a device already in the DeviceGroup pairs again
                    // with a self-removal proof.
                    rejoinsHeldLeaf = earlierKeysMissing,
                )
            }
            Spacer(Modifier.height(16.dp))
        }

        // ADR-0073 / docs/ux/identity-recovery.md 15.1 - the warning before a
        // reset of the account keys, with the typed confirmation. Nothing is
        // sent until the button is tapped; the bullets are unconditional (the
        // device does not know how many devices or passkeys the account has).
        RestoreWay.Neither -> if (state.busy) {
            // A full-screen "Resetting..." with no cancel: the server step is
            // one operation and cannot be half done.
            AuthScaffold(title = t("restore_identity.lost.sub"), onBack = null, modifier = modifier) {
                Lead(t("restore_identity.reset.resetting"))
            }
        } else {
            AuthScaffold(
                title = t("restore_identity.lost.sub"),
                onBack = closeWay,
                modifier = modifier,
            ) {
                Lead(t("restore_identity.reset.lede"))
                Spacer(Modifier.height(12.dp))
                Text(t("restore_identity.reset.what_it_does"), style = MaterialTheme.typography.bodyMedium)
                Spacer(Modifier.height(4.dp))
                for (key in RESET_CONSEQUENCE_KEYS) {
                    Text("\u2022 " + t(key), style = MaterialTheme.typography.bodyMedium)
                }
                Spacer(Modifier.height(16.dp))
                val panel = factor.panel()
                when (panel) {
                    me.lakko.app.auth.ResetPanel.Loading -> {
                        // Nothing is offered until the account's factors
                        // are known.
                        if (factor.loadFailed) {
                            FormError(t("common.error.offline"))
                            Spacer(Modifier.height(12.dp))
                            OutlinedButton(
                                onClick = { scope.launch { factor.load() } },
                                modifier = Modifier.fillMaxWidth(),
                            ) { Text(t("common.try_again")) }
                        } else {
                            androidx.compose.material3.CircularProgressIndicator()
                        }
                        Spacer(Modifier.height(8.dp))
                        OutlinedButton(onClick = closeWay, modifier = Modifier.fillMaxWidth()) {
                            Text(t("common.cancel"))
                        }
                        Spacer(Modifier.height(16.dp))
                        return@AuthScaffold
                    }
                    me.lakko.app.auth.ResetPanel.CannotUse, me.lakko.app.auth.ResetPanel.NoFactor -> {
                        // UX 15.1b - in place of the section, the typed word
                        // and the button; the warning stays above.
                        ResetCannotPanel(panel = panel, onBack = { factor.saidCannot = false }, onCancel = closeWay)
                        Spacer(Modifier.height(16.dp))
                        return@AuthScaffold
                    }
                    me.lakko.app.auth.ResetPanel.Factors -> Unit
                }
                ResetFactorSection(controller = factor)
                Spacer(Modifier.height(16.dp))
                val confirmed = factor.confirmed
                val canReset = me.lakko.app.auth.resetButtonEnabled(
                    wordMatches = resetConfirmationMatches(resetTyped),
                    confirmed = confirmed,
                    nowMs = System.currentTimeMillis(),
                    busy = factor.working != null,
                )
                val submit: () -> Unit = {
                    if (canReset) viewModel.startFresh(onFactorRequired = factor::reopen)
                }
                Text(
                    t("restore_identity.reset.confirm_label", mapOf("word" to RESET_CONFIRMATION_WORD)),
                    style = MaterialTheme.typography.bodyMedium,
                )
                Spacer(Modifier.height(4.dp))
                androidx.compose.material3.OutlinedTextField(
                    value = resetTyped,
                    onValueChange = { resetTyped = it },
                    singleLine = true,
                    // Usable once a factor is confirmed (UX 15.1b).
                    enabled = confirmed != null && factor.working == null,
                    keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                        capitalization = androidx.compose.ui.text.input.KeyboardCapitalization.None,
                        autoCorrectEnabled = false,
                        keyboardType = androidx.compose.ui.text.input.KeyboardType.Ascii,
                        imeAction = androidx.compose.ui.text.input.ImeAction.Done,
                    ),
                    keyboardActions = androidx.compose.foundation.text.KeyboardActions(onDone = { submit() }),
                    modifier = Modifier.fillMaxWidth(),
                )
                FormError(state.error)
                Spacer(Modifier.height(24.dp))
                BusyButton(
                    label = t("restore_identity.lost.create"),
                    busy = false,
                    busyLabel = t("restore_identity.reset.resetting"),
                    enabled = canReset,
                    onClick = submit,
                )
                Spacer(Modifier.height(8.dp))
                OutlinedButton(
                    onClick = closeWay,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(t("common.cancel"))
                }
                Spacer(Modifier.height(16.dp))
            }
        }
    }
}

/** Which optional rows the restore prompt lists. */
public data class RestoreRows(val passkey: Boolean, val phrase: Boolean)

/**
 * The optional rows of the restore prompt (UX sections 8 and 8a): on the
 * "earlier keys missing" variant none of them (each brings only the keys this
 * device already holds); otherwise what the account and this device offer.
 * "Another device" and "I have none of these" are always listed.
 */
public fun restoreRowsFor(
    earlierKeysMissing: Boolean,
    passkeyOffered: Boolean,
    phrase: Boolean,
): RestoreRows =
    if (earlierKeysMissing) {
        RestoreRows(passkey = false, phrase = false)
    } else {
        RestoreRows(passkey = passkeyOffered, phrase = phrase)
    }

/** The word the person types to confirm a reset (UX 15.1): the same Latin
 *  letters in every language, like the slot numbers of the grid. */
public const val RESET_CONFIRMATION_WORD: String = "RESET"

/** Whether the typed confirmation enables the reset button: the exact word,
 *  surrounding whitespace ignored, case as typed (the line names it in
 *  capitals). */
public fun resetConfirmationMatches(typed: String): Boolean = typed.trim() == RESET_CONFIRMATION_WORD

/** The three ways out the "cannot reset" panel names (UX 15.1b), in order. */
public val RESET_CANNOT_OPTION_KEYS: List<String> = listOf(
    "proof.reset.cannot.option_device",
    "proof.reset.cannot.option_phrase",
    "proof.reset.cannot.option_passkey",
)

/** The consequence bullets of the warning, in the order of UX 15.1. */
private val RESET_CONSEQUENCE_KEYS: List<String> = listOf(
    "restore_identity.reset.effect_devices",
    "restore_identity.reset.effect_passkeys",
    "restore_identity.reset.effect_contacts",
    // What comes back is said once a factor is confirmed (UX 15.1c): the
    // sealed or the unsealed line, in place of the earlier bullet.
    "restore_identity.reset.effect_password",
)
