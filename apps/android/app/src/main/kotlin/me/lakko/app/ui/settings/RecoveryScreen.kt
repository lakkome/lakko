// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.automirrored.filled.ShortText
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.Fingerprint
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Key
import androidx.compose.material.icons.filled.Password
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.auth.AuthClient
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.PhraseRevealGate
import me.lakko.app.auth.attemptsRetryAfterSecs
import me.lakko.app.auth.formatCountdown
import me.lakko.app.auth.phraseRevealGate
import me.lakko.app.auth.threeWordsSettingsLine
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.devices.QrTextImageComposable

/**
 * Settings > Security > Recovery: the ways back into the account, on one
 * screen, in the two groups the recovery list itself uses.
 *
 *   - **Brings back your chats on a new device**: the recovery phrase
 *     (whether it is saved on this device, and "Show recovery phrase" behind
 *     the device's own authentication), the passkeys (how many, how many
 *     restore keys), the other devices.
 *   - **Signs you in when you forget your password**: one informational
 *     line about three words (nothing to configure: the check is local and
 *     works wherever the phrase is saved), and the authenticator app with
 *     its enrolment.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun RecoveryScreen(
    authRepository: AuthRepository,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
    onOpenPasskeys: () -> Unit = {},
    onOpenDevices: () -> Unit = {},
) {
    val authState by authRepository.state.collectAsStateWithLifecycle()
    val identitySecrets by authRepository.identitySecrets.collectAsStateWithLifecycle()
    val account = (authState as? AuthState.Session)?.account
    val secrets = identitySecrets
    val context = LocalContext.current

    // Whether the phrase is saved on this phone; read off the main thread —
    // it is an EncryptedSharedPreferences open.
    var phraseOnDevice: Boolean? by remember { mutableStateOf(null) }
    // Whether three words are paused here after a refused log in.
    var wordsSuspended by remember { mutableStateOf(false) }
    LaunchedEffect(account?.accountIdB64) {
        val id = account?.accountIdB64 ?: return@LaunchedEffect
        // Read before the phrase flag flips, so the row never shows
        // "available" for a route that is paused.
        wordsSuspended = withContext(Dispatchers.IO) { authRepository.threeWordsSuspended() }
        phraseOnDevice = withContext(Dispatchers.IO) {
            me.lakko.app.backup.DevicePhrase.isStored(context, id)
        }
    }

    // The account's passkeys and which of them restore keys; `null` while
    // loading or when the server could not say.
    var passkeys: PasskeyCounts? by remember { mutableStateOf(null) }
    LaunchedEffect(secrets?.sessionToken) {
        val token = secrets?.sessionToken ?: return@LaunchedEffect
        passkeys = try {
            val total = me.lakko.app.webauthn.WebauthnClient().use { client ->
                client.setSessionToken(token)
                client.listCredentials().credentials.size
            }
            PasskeyCounts(total = total, restoring = authRepository.passkeysWithKeys(token)?.size ?: 0)
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            null
        }
    }

    var authenticatorOpen by remember { mutableStateOf(false) }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(t("settings.recovery.title")) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState()),
        ) {
            Text(
                t("settings.recovery.lede"),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
            )
            if (account == null || secrets == null) {
                Text(
                    t("settings.recovery.sign_in_first"),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.padding(16.dp),
                )
                return@Column
            }

            // ADR-0067 / UX 19.1 - the phrase and the three words exist only
            // for an account whose owner saved the phrase.
            val protection by authRepository.recoveryEmail.collectAsStateWithLifecycle()
            val phraseProtected = protection?.phraseSaved
                ?: remember(account.accountIdB64) { authRepository.phraseProtected() }
            SectionLabel(t("settings.recovery.group.keys"))
            if (phraseProtected) {
                RecoveryPhraseRow(
                    authRepository = authRepository,
                    phraseOnDevice = phraseOnDevice,
                    hasUsername = account.username.isNotBlank(),
                )
            }
            ListItem(
                headlineContent = { Text(t("settings.passkeys.title")) },
                supportingContent = {
                    Column {
                        Text(t("settings.recovery.passkeys.sub"))
                        passkeys?.let { Text(passkeyCountsLine(it)) }
                    }
                },
                leadingContent = { Icon(Icons.Filled.Fingerprint, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant) },
                trailingContent = {
                    Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
                },
                colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
                modifier = Modifier.clickable(onClick = onOpenPasskeys),
            )
            ListItem(
                headlineContent = { Text(t("settings.recovery.devices.title")) },
                supportingContent = { Text(t("settings.security.devices.sub")) },
                leadingContent = { Icon(Icons.Filled.Devices, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant) },
                trailingContent = {
                    Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
                },
                colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
                modifier = Modifier.clickable(onClick = onOpenDevices),
            )

            SectionLabel(t("settings.recovery.group.signin"))
            // Informational only: there is nothing to turn on or off.
            if (phraseProtected) ListItem(
                headlineContent = { Text(t("recovery.method.words.title")) },
                supportingContent = {
                    Text(
                        when (val saved = phraseOnDevice) {
                            null -> t("common.checking")
                            else -> t(threeWordsSettingsLine(phraseSaved = saved, suspended = wordsSuspended))
                        },
                    )
                },
                leadingContent = { Icon(Icons.AutoMirrored.Filled.ShortText, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant) },
                colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
            )
            ExpandableRow(
                title = t("settings.recovery.totp.title"),
                supporting = t("recovery.method.totp.sub"),
                icon = Icons.Filled.Password,
                expanded = authenticatorOpen,
                onToggle = { authenticatorOpen = !authenticatorOpen },
            ) {
                TotpEnrollSection(sessionToken = secrets.sessionToken)
            }
        }
    }
}

/** How many passkeys the account has and how many of them restore keys. */
internal class PasskeyCounts(val total: Int, val restoring: Int)

@Composable
private fun passkeyCountsLine(counts: PasskeyCounts): String =
    if (counts.total == 0) {
        t("settings.recovery.passkeys.none")
    } else {
        t("settings.recovery.passkeys.count", emptyMap(), mapOf("count" to counts.total.toULong())) + " " +
            t("settings.recovery.passkeys.restoring", emptyMap(), mapOf("count" to counts.restoring.toULong()))
    }

/** What "Show recovery phrase" is doing. */
private sealed interface PhraseReveal {
    data object Hidden : PhraseReveal

    /** No screen lock on this device: the account's password is asked. */
    data class AskPassword(val password: String = "", val busy: Boolean = false, val error: UiText? = null) :
        PhraseReveal

    /** The publish of a reset's new keys is owed: the words are withheld. */
    data class PublishPending(
        val busy: Boolean = false,
        val error: UiText? = null,
        val waitUntilMs: Long? = null,
    ) : PhraseReveal

    data class Shown(val words: List<String>) : PhraseReveal
    data class Failed(val message: UiText) : PhraseReveal
}

/**
 * The recovery-phrase row: what the phrase is for, whether it is saved on
 * this device, and "Show recovery phrase". Showing it needs the device to
 * authenticate its owner first (fingerprint, face or the screen lock). On a
 * device with no screen lock the account's password is asked instead; an
 * account without a password (a guest) on such a device has nothing to be
 * asked, and the phrase is shown after the warning line.
 */
@Composable
private fun RecoveryPhraseRow(
    authRepository: AuthRepository,
    phraseOnDevice: Boolean?,
    hasUsername: Boolean,
) {
    val scope = rememberCoroutineScope()
    var reveal: PhraseReveal by remember { mutableStateOf(PhraseReveal.Hidden) }

    // The gate ([phraseRevealGate]): while the publish of a reset is owed
    // the words are withheld, before and after the device's own
    // authentication; they are read only once nothing is owed.
    fun show() {
        scope.launch {
            val owed = withContext(Dispatchers.IO) { authRepository.publishOwed() }
            val phrase = if (owed) null else withContext(Dispatchers.IO) { authRepository.devicePhrase() }
            reveal = when (phraseRevealGate(publishOwed = owed, phraseOnDevice = phrase != null)) {
                PhraseRevealGate.PublishPending -> PhraseReveal.PublishPending()
                PhraseRevealGate.NotSaved -> PhraseReveal.Failed(UiText("settings.recovery.phrase.not_saved_here"))
                PhraseRevealGate.Show -> PhraseReveal.Shown(me.lakko.app.ui.auth.phraseWords(phrase.orEmpty()))
            }
        }
    }

    // A publish that settles (or comes to be owed) while this row is open.
    val publishRevision by authRepository.publishOwedRevision.collectAsStateWithLifecycle()
    LaunchedEffect(publishRevision) {
        val owed = withContext(Dispatchers.IO) { authRepository.publishOwed() }
        if (owed && reveal is PhraseReveal.Shown) reveal = PhraseReveal.PublishPending()
        if (!owed && reveal is PhraseReveal.PublishPending) reveal = PhraseReveal.Hidden
    }

    val authenticateDevice = rememberDeviceAuthenticator(title = t("settings.recovery.phrase.show")) { result ->
        when (result) {
            DeviceAuthResult.Authenticated -> show()
            DeviceAuthResult.NotAuthenticated -> Unit
            DeviceAuthResult.NoDeviceLock -> if (hasUsername) reveal = PhraseReveal.AskPassword() else show()
        }
    }

    // "Show recovery phrase": nothing is asked of the person while the words
    // would be withheld anyway.
    val authenticate: () -> Unit = {
        scope.launch {
            val owed = withContext(Dispatchers.IO) { authRepository.publishOwed() }
            if (owed) reveal = PhraseReveal.PublishPending() else authenticateDevice()
        }
    }

    ListItem(
        headlineContent = { Text(t("recovery.method.phrase.title")) },
        supportingContent = {
            Column {
                Text(t("settings.recovery.phrase.sub"))
                Text(
                    when (phraseOnDevice) {
                        null -> t("common.checking")
                        true -> t("settings.recovery.phrase.saved_here")
                        false -> t("settings.recovery.phrase.not_saved_here")
                    },
                )
            }
        },
        leadingContent = { Icon(Icons.Filled.Key, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant) },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
    )
    if (phraseOnDevice != true) return
    Column(
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        when (val r = reveal) {
            PhraseReveal.Hidden -> Button(onClick = authenticate) { Text(t("settings.recovery.phrase.show")) }
            is PhraseReveal.AskPassword -> {
                Text(t("settings.recovery.phrase.password_prompt"), style = MaterialTheme.typography.bodyMedium)
                me.lakko.app.ui.auth.PasswordField(
                    value = r.password,
                    onValueChange = { reveal = r.copy(password = it, error = null) },
                    label = t("auth.password.label"),
                    enabled = !r.busy,
                    imeAction = ImeAction.Done,
                )
                r.error?.let {
                    Text(it.text(), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                }
                Button(
                    enabled = !r.busy && r.password.isNotEmpty(),
                    onClick = {
                        reveal = r.copy(busy = true, error = null)
                        scope.launch {
                            val bytes = r.password.toByteArray(Charsets.UTF_8)
                            try {
                                if (authRepository.verifyPassword(bytes)) {
                                    show()
                                } else {
                                    reveal = r.copy(
                                        busy = false,
                                        error = UiText("settings.recovery.phrase.wrong_password"),
                                    )
                                }
                            } catch (cause: Throwable) {
                                if (cause is kotlinx.coroutines.CancellationException) throw cause
                                reveal = r.copy(
                                    busy = false,
                                    error = enrollFailure(cause, UiText("settings.recovery.phrase.error.generic")),
                                )
                            } finally {
                                bytes.fill(0)
                            }
                        }
                    },
                ) { Text(if (r.busy) t("common.checking") else t("settings.recovery.phrase.show")) }
            }
            is PhraseReveal.PublishPending -> {
                Text(t("recovery_phrase.fresh.publish_owed"), style = MaterialTheme.typography.bodyMedium)
                val waitLeft = me.lakko.app.ui.auth.rememberRemainingMs(r.waitUntilMs)
                val problem = if (waitLeft > 0L) {
                    t("recovery.error.rate_limited", mapOf("time" to formatCountdown(waitLeft)))
                } else {
                    r.error?.text()
                }
                problem?.let {
                    Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                }
                Button(
                    enabled = !r.busy && waitLeft <= 0L,
                    onClick = {
                        reveal = r.copy(busy = true, error = null)
                        scope.launch {
                            try {
                                // The publish steps that did not land, awaited;
                                // never a second reset.
                                authRepository.retryOwedPublish()
                                // Nothing is owed any more: back to the
                                // button, which asks the device to
                                // authenticate before the words appear.
                                reveal = PhraseReveal.Hidden
                            } catch (cause: Throwable) {
                                if (cause is kotlinx.coroutines.CancellationException) throw cause
                                val secs = attemptsRetryAfterSecs(cause)
                                reveal = if (secs != null) {
                                    r.copy(busy = false, waitUntilMs = System.currentTimeMillis() + secs * 1000L)
                                } else {
                                    r.copy(busy = false, error = me.lakko.app.ui.auth.freshPublishFailure(cause))
                                }
                            }
                        }
                    },
                ) { Text(if (r.busy) t("common.checking") else t("common.try_again")) }
            }
            is PhraseReveal.Shown -> {
                Text(
                    t("settings.recovery.phrase.confirm_show"),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                me.lakko.app.ui.auth.RecoveryPhraseGrid(r.words)
                Button(onClick = { reveal = PhraseReveal.Hidden }) { Text(t("settings.recovery.phrase.hide")) }
            }
            is PhraseReveal.Failed -> Text(
                r.message.text(),
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodySmall,
            )
        }
    }
}

/**
 * Settings > Security > Password: choose a new password. This device already
 * holds the account's keys, so only the new password is typed. Not offered
 * to an account without a username (it has no password).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun PasswordScreen(
    authRepository: AuthRepository,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var saved by remember { mutableStateOf(false) }
    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(t("settings.password.title")) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp, vertical = 12.dp),
        ) {
            Text(
                t("settings.password.lede"),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            androidx.compose.foundation.layout.Spacer(Modifier.padding(top = 16.dp))
            if (saved) {
                Text(
                    t("recovery.new_password.saved"),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.primary,
                    fontWeight = FontWeight.SemiBold,
                )
            } else {
                me.lakko.app.ui.auth.NewPasswordForm(
                    repository = authRepository,
                    onSaved = { saved = true },
                )
            }
        }
    }
}

@Composable
private fun SectionLabel(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, top = 16.dp, bottom = 4.dp),
    )
}

@Composable
private fun ExpandableRow(
    title: String,
    supporting: String,
    icon: ImageVector,
    expanded: Boolean,
    onToggle: () -> Unit,
    content: @Composable () -> Unit,
) {
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = { Text(supporting) },
        leadingContent = { Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant) },
        trailingContent = {
            Icon(
                imageVector = if (expanded) Icons.Filled.ExpandLess else Icons.Filled.ExpandMore,
                contentDescription = if (expanded) t("common.collapse") else t("common.expand"),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
        modifier = Modifier.clickable(onClick = onToggle),
    )
    if (expanded) {
        Column(modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
            content()
        }
    }
}

// ---- Authenticator (TOTP) enrollment — ADR-0018 factor (c) -------

/** UI state machine for [TotpEnrollSection]. */
private sealed interface TotpUi {
    data object Idle : TotpUi
    data object Starting : TotpUi
    data class Confirm(
        val otpauthUri: String,
        val secretB32: String,
        val code: String,
        val busy: Boolean,
        val error: UiText?,
    ) : TotpUi
    data object Done : TotpUi
    data object AlreadyEnabled : TotpUi
    data object Unavailable : TotpUi
    data class Error(val message: UiText) : TotpUi
}

/**
 * Enroll an authenticator app as an ACCESS-recovery factor:
 * `/totp/enroll/start` renders the `otpauth://` QR (RAW TEXT —
 * authenticator apps parse the URI straight out of the QR) + the
 * manual-entry secret; `/totp/enroll/finish` confirms with a live
 * 6-digit code. Mirror of the web `TotpEnrollView`.
 *
 * There is no server "status" endpoint — the Enable button renders
 * by default and a 409 from either call flips the card to "already
 * enabled" (honest lazy state derivation, same as web).
 */
@Composable
private fun TotpEnrollSection(sessionToken: String) {
    val scope = rememberCoroutineScope()
    var ui: TotpUi by remember { mutableStateOf(TotpUi.Idle) }

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(
            t("settings.recovery.totp.lede"),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        when (val s = ui) {
            TotpUi.Idle -> Button(
                onClick = {
                    ui = TotpUi.Starting
                    scope.launch {
                        ui = try {
                            val res = AuthClient().use { client ->
                                client.totpEnrollStart(sessionToken)
                            }
                            TotpUi.Confirm(
                                otpauthUri = res.otpauthUri,
                                secretB32 = res.secretB32,
                                code = "",
                                busy = false,
                                error = null,
                            )
                        } catch (err: me.lakko.app.auth.AuthHttpError) {
                            when (err.status) {
                                409 -> TotpUi.AlreadyEnabled
                                503 -> TotpUi.Unavailable
                                else -> TotpUi.Error(enrollFailure(err, TOTP_GENERIC))
                            }
                        } catch (err: Throwable) {
                            TotpUi.Error(enrollFailure(err, TOTP_GENERIC))
                        }
                    }
                },
            ) { Text(t("settings.recovery.totp.set_up")) }

            TotpUi.Starting -> CircularProgressIndicator()

            is TotpUi.Confirm -> {
                Text(
                    t("settings.recovery.totp.scan"),
                    style = MaterialTheme.typography.bodySmall,
                )
                QrTextImageComposable(text = s.otpauthUri)
                Text(
                    t("settings.recovery.totp.key", mapOf("key" to s.secretB32)),
                    style = MaterialTheme.typography.bodySmall,
                    fontWeight = FontWeight.SemiBold,
                )
                OutlinedTextField(
                    value = s.code,
                    onValueChange = { v -> ui = s.copy(code = v, error = null) },
                    label = { Text(t("recovery.totp.code_label")) },
                    singleLine = true,
                    enabled = !s.busy,
                    keyboardOptions = KeyboardOptions(
                        keyboardType = KeyboardType.Number,
                        imeAction = ImeAction.Done,
                    ),
                    modifier = Modifier.fillMaxWidth(),
                )
                if (s.error != null) {
                    Text(
                        s.error.text(),
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                Button(
                    onClick = {
                        val normalized = me.lakko.app.auth.normalizeTotpCode(s.code)
                        if (normalized == null) {
                            ui = s.copy(
                                error = UiText("recovery.totp.error.enter_code"),
                            )
                            return@Button
                        }
                        ui = s.copy(busy = true, error = null)
                        scope.launch {
                            ui = try {
                                AuthClient().use { client ->
                                    client.totpEnrollFinish(sessionToken, normalized)
                                }
                                TotpUi.Done
                            } catch (err: me.lakko.app.auth.AuthHttpError) {
                                when (err.status) {
                                    429 -> s.copy(
                                        busy = false,
                                        error = enrollFailure(err, TOTP_GENERIC),
                                    )
                                    401 -> s.copy(
                                        busy = false,
                                        error = UiText("settings.recovery.totp.error.wrong"),
                                    )
                                    409 -> TotpUi.AlreadyEnabled
                                    else -> TotpUi.Error(enrollFailure(err, TOTP_GENERIC))
                                }
                            } catch (err: Throwable) {
                                TotpUi.Error(enrollFailure(err, TOTP_GENERIC))
                            }
                        }
                    },
                    enabled = !s.busy,
                ) { Text(if (s.busy) t("common.confirming") else t("common.confirm")) }
            }

            TotpUi.Done -> Text(
                t("settings.recovery.totp.done"),
                color = MaterialTheme.colorScheme.primary,
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.SemiBold,
            )

            TotpUi.AlreadyEnabled -> Text(
                t("settings.recovery.totp.already_enabled"),
                style = MaterialTheme.typography.bodyMedium,
            )

            TotpUi.Unavailable -> Text(
                t("settings.recovery.totp.unavailable"),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.error,
            )

            is TotpUi.Error -> Column(
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    s.message.text(),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                )
                Button(onClick = { ui = TotpUi.Idle }) { Text(t("common.try_again")) }
            }
        }
    }
}

private val TOTP_GENERIC = UiText("settings.recovery.totp.error.generic")

/**
 * What an enrolment failure reads as (ADR-0061: from its kind, never from
 * server prose): a limit says when to try again, a network failure says so,
 * anything else is the flow's generic sentence.
 */
private fun enrollFailure(err: Throwable, generic: UiText): UiText = when (err) {
    is me.lakko.app.auth.AuthHttpError -> me.lakko.app.auth.authLimitText(err) ?: generic
    is me.lakko.app.auth.AuthNetworkError -> UiText("common.error.offline")
    else -> generic
}
