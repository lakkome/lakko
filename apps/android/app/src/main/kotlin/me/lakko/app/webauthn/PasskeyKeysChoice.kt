// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.webauthn

import android.content.Context
import android.util.Log
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.base64Encode
import me.lakko.app.devices.DeviceRemoval
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.auth.RecoveryPhraseEntryGrid

/** How "Let this passkey restore your keys?" ended. */
public enum class KeysChoiceEnd {
    /** The keys are stored for the passkey. */
    Stored,

    /** The person chose sign-in only; nothing was stored. */
    SignInOnly,

    /** The passkey gave no secret to protect keys with. */
    NoPrf,

    /** The server does not store keys for passkeys. */
    Unavailable,
}

/** The line that says how the choice ended, or `null` for sign-in only
 *  (nothing changed, nothing to say). */
public fun keysChoiceEndText(end: KeysChoiceEnd): UiText? = when (end) {
    KeysChoiceEnd.Stored -> UiText("settings.passkeys.keys_choice.done")
    KeysChoiceEnd.NoPrf -> UiText("settings.passkeys.keys_choice.no_prf")
    KeysChoiceEnd.Unavailable -> UiText("settings.passkeys.keys_choice.unavailable")
    KeysChoiceEnd.SignInOnly -> null
}

/**
 * Create a passkey for the signed-in account and, now that the account has
 * one, turn on attested device removals. Nothing is stored for the passkey
 * beyond the credential: whether it restores keys is the person's choice
 * afterwards ([PasskeyKeysChoice]).
 *
 * @throws WebauthnUserCancelledError when the sheet was dismissed.
 * @throws me.lakko.app.auth.FreshAuthRequired when no fresh-auth proof is
 *   live, or the server refused the one sent. Nothing was added.
 */
public suspend fun addPasskeyForSession(
    context: Context,
    repo: AuthRepository,
    nickname: String,
    client: WebauthnClient = WebauthnClient(),
    registration: WebauthnRegistration = WebauthnRegistration(),
): PasskeyAdded {
    val session = repo.state.value as? AuthState.Session
    val secrets = repo.identitySecrets.value
    check(session != null && secrets != null) { "no session" }
    client.setSessionToken(secrets.sessionToken)
    // UX section 17: a session alone may not add a passkey. The caller shows
    // the proof block first; with no live proof nothing is sent.
    val freshGrant = repo.freshGrant() ?: throw me.lakko.app.auth.FreshAuthRequired()
    val added = try {
        client.registerPasskey(context, session.account.username, nickname, freshGrant, registration)
    } catch (cause: WebauthnHttpError) {
        if (me.lakko.app.auth.isFreshAuthRequired(cause)) {
            // The proof ran out between the block and the call.
            repo.dropFreshProof()
            throw me.lakko.app.auth.FreshAuthRequired()
        }
        throw cause
    }
    // G-13 (ADR-0015): the account has a passkey now, so device removals
    // require a peer-verified gesture. Non-fatal.
    try {
        DeviceRemoval.enableAttestedRemovalsIfNeeded(
            context = context,
            accountIdBytes = secrets.accountId,
            accountIdB64 = base64Encode(secrets.accountId),
        )
    } catch (cause: Throwable) {
        Log.w("lakko-passkeys", "failed to enable attested removals: ${cause.javaClass.simpleName}")
    }
    return added
}

/**
 * "Let this passkey restore your keys?": the choice shown after a passkey is
 * added, and from a passkey's row later. Used by Settings > Security >
 * Passkeys and by the passkey offer at the end of sign-up.
 *
 * Storing the keys for a passkey is OPT-IN, per passkey, and informed: the
 * safety of the keys then also rests on whoever syncs the passkey. Nothing
 * is stored unless the person presses "Turn on"; the two buttons carry the
 * same weight and neither is the default.
 *
 * Turning it on needs the recovery phrase (read from this device, else
 * typed and checked against the identity this device holds) and the
 * passkey's secret: [prfOutput] when the registration gave it, else one more
 * passkey prompt for this credential. That secret never leaves the device.
 */
@Composable
public fun PasskeyKeysChoice(
    repo: AuthRepository,
    credentialIdB64: String,
    nickname: String,
    prfOutput: ByteArray?,
    onDone: (KeysChoiceEnd) -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var phraseStep by remember(credentialIdB64) { mutableStateOf(false) }
    var typedPhrase by remember(credentialIdB64) { mutableStateOf("") }
    var error by remember(credentialIdB64) { mutableStateOf<UiText?>(null) }
    var busy by remember(credentialIdB64) { mutableStateOf(false) }

    fun turnOn() {
        if (busy) return
        busy = true
        error = null
        scope.launch {
            try {
                val result = repo.turnOnPasskeyKeys(
                    activityContext = context,
                    credentialIdB64 = credentialIdB64,
                    registrationPrfOutput = prfOutput,
                    typedPhrase = typedPhrase.takeIf { phraseStep },
                )
                when (result) {
                    PasskeyKeysChoiceResult.Stored -> onDone(KeysChoiceEnd.Stored)
                    PasskeyKeysChoiceResult.NoPrf -> onDone(KeysChoiceEnd.NoPrf)
                    PasskeyKeysChoiceResult.Unavailable -> onDone(KeysChoiceEnd.Unavailable)
                    PasskeyKeysChoiceResult.PhraseNeeded -> phraseStep = true
                    PasskeyKeysChoiceResult.PhraseInvalid -> error = UiText("restore_identity.error.invalid_phrase")
                    PasskeyKeysChoiceResult.PhraseMismatch ->
                        error = UiText("settings.passkeys.keys_choice.error.phrase_mismatch")
                    PasskeyKeysChoiceResult.OtherPasskey ->
                        error = UiText("settings.passkeys.keys_choice.error.other_passkey")
                }
            } catch (_: WebauthnUserCancelledError) {
                // Cancelled in the system sheet: the passkey stays sign-in only.
                error = UiText("settings.passkeys.keys_choice.cancelled")
            } catch (err: AuthHttpError) {
                if (err.status == 503) {
                    onDone(KeysChoiceEnd.Unavailable)
                } else {
                    error = UiText("settings.passkeys.error.generic")
                }
            } catch (_: AuthNetworkError) {
                error = UiText("settings.passkeys.error.network")
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                Log.w("lakko-passkeys", "turning on restoring keys failed: ${cause.javaClass.simpleName}")
                error = UiText("settings.passkeys.error.generic")
            } finally {
                busy = false
            }
        }
    }

    Surface(
        shape = MaterialTheme.shapes.medium,
        color = MaterialTheme.colorScheme.surfaceVariant,
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                t("settings.passkeys.keys_choice.title"),
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            if (nickname.isNotBlank()) {
                Text(
                    nickname,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Text(t("settings.passkeys.keys_choice.body_on"), style = MaterialTheme.typography.bodyMedium)
            Text(t("settings.passkeys.keys_choice.body_off"), style = MaterialTheme.typography.bodyMedium)
            Text(
                t("settings.passkeys.keys_choice.later"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (phraseStep) {
                Text(t("settings.passkeys.keys_choice.phrase_needed"), style = MaterialTheme.typography.bodyMedium)
                RecoveryPhraseEntryGrid(
                    phrase = typedPhrase,
                    onPhraseChange = { typedPhrase = it },
                    enabled = !busy,
                )
            }
            error?.let {
                Text(it.text(), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium)
            }
            // Same weight, no default: the person must pick one.
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
                OutlinedButton(onClick = ::turnOn, enabled = !busy, modifier = Modifier.weight(1f)) {
                    Text(if (busy) t("common.checking") else t("settings.passkeys.keys_choice.turn_on"))
                }
                OutlinedButton(
                    onClick = { onDone(KeysChoiceEnd.SignInOnly) },
                    enabled = !busy,
                    modifier = Modifier.weight(1f),
                ) {
                    Text(t("settings.passkeys.keys_choice.signin_only"))
                }
            }
        }
    }
}
