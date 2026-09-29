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
import androidx.compose.material.icons.filled.Email
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Key
import androidx.compose.material.icons.filled.Password
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
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
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.auth.AuthClient
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.ContactKind
import me.lakko.app.auth.isPlausibleContact
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.devices.QrTextImageComposable

/**
 * Settings > Security > Recovery — docs/ux/client-redesign.md section 5.5.
 *
 * The ways back into the account, on one screen: the recovery phrase (the
 * only thing that restores the identity, ADR-0017) and the access factors of
 * ADR-0018 — an authenticator app and an email or phone. The two enrolment
 * flows used to be tabs on the Devices screen; they moved here unchanged. The
 * three-word vault of ADR-0049 has no Android client yet, so it is not listed.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun RecoveryScreen(
    authRepository: AuthRepository,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val authState by authRepository.state.collectAsStateWithLifecycle()
    val identitySecrets by authRepository.identitySecrets.collectAsStateWithLifecycle()
    val account = (authState as? AuthState.Session)?.account
    val secrets = identitySecrets
    val context = LocalContext.current

    // Whether the phrase is cached on this phone (the backup store row); read
    // off the main thread — it is an EncryptedSharedPreferences open.
    var phraseOnDevice: Boolean? by remember { mutableStateOf(null) }
    LaunchedEffect(account?.accountIdB64) {
        val id = account?.accountIdB64 ?: return@LaunchedEffect
        phraseOnDevice = withContext(Dispatchers.IO) {
            val bytes = me.lakko.app.backup.PhraseStore.getPhrase(context, id)
            val present = bytes != null
            bytes?.fill(0)
            present
        }
    }

    var authenticatorOpen by remember { mutableStateOf(false) }
    var contactOpen by remember { mutableStateOf(false) }

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

            SectionLabel(t("settings.recovery.section.identity"))
            ListItem(
                headlineContent = { Text(t("recovery.method.phrase.title")) },
                supportingContent = {
                    Text(
                        when (phraseOnDevice) {
                            null -> t("common.checking")
                            true -> t("settings.recovery.phrase.saved_here")
                            false -> t("settings.recovery.phrase.not_saved_here")
                        },
                    )
                },
                leadingContent = { Icon(Icons.Filled.Key, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant) },
                colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
            )

            SectionLabel(t("settings.recovery.section.access"))
            ExpandableRow(
                title = t("settings.recovery.totp.title"),
                supporting = t("recovery.method.totp.sub"),
                icon = Icons.Filled.Password,
                expanded = authenticatorOpen,
                onToggle = { authenticatorOpen = !authenticatorOpen },
            ) {
                TotpEnrollSection(sessionToken = secrets.sessionToken)
            }
            HorizontalDivider()
            ExpandableRow(
                title = t("recovery.method.contact.title"),
                supporting = t("settings.recovery.contact.sub"),
                icon = Icons.Filled.Email,
                expanded = contactOpen,
                onToggle = { contactOpen = !contactOpen },
            ) {
                ContactEnrollSection(sessionToken = secrets.sessionToken)
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

// ---- Email / phone OTP enrollment — ADR-0018 factor (d) ----------

/** UI state machine for [ContactEnrollSection]. */
private sealed interface ContactUi {
    /** Collecting the kind + contact; not yet sent. */
    data class Idle(
        val kind: Int,
        val contact: String,
        val error: UiText?,
    ) : ContactUi
    data object Starting : ContactUi
    /** A code was sent; collecting the confirm code. The kind +
     *  contact are carried so the finish targets the exact pair. */
    data class Confirm(
        val kind: Int,
        val contact: String,
        val code: String,
        val busy: Boolean,
        val error: UiText?,
    ) : ContactUi
    data object Done : ContactUi
    data object AlreadyEnabled : ContactUi
    data object Unavailable : ContactUi
    data class Error(val message: UiText) : ContactUi
}

/**
 * Enroll a phone / email as an ACCESS-recovery factor (ADR-0018
 * factor (d)): `/recovery-contact/enroll/start` sends a 6-digit code
 * to the contact, `/recovery-contact/enroll/finish` confirms it with
 * the code. Mirror of the web `ContactEnrollView`.
 *
 * There is no server "status" endpoint — the form renders by default
 * and a 409 from either call flips the card to "already enabled"
 * (honest lazy state derivation, same as the TOTP tab and web);
 * a 503 means the factor is disabled server-side.
 */
@Composable
private fun ContactEnrollSection(sessionToken: String) {
    val scope = rememberCoroutineScope()
    var ui: ContactUi by remember {
        mutableStateOf(
            ContactUi.Idle(kind = ContactKind.EMAIL, contact = "", error = null),
        )
    }

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(
            t("settings.recovery.contact.lede"),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        when (val s = ui) {
            is ContactUi.Idle -> {
                // Kind toggle: Email / SMS.
                SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
                    SegmentedButton(
                        selected = s.kind == ContactKind.EMAIL,
                        onClick = { ui = s.copy(kind = ContactKind.EMAIL, error = null) },
                        shape = SegmentedButtonDefaults.itemShape(index = 0, count = 2),
                    ) { Text(t("common.email")) }
                    SegmentedButton(
                        selected = s.kind == ContactKind.PHONE,
                        onClick = { ui = s.copy(kind = ContactKind.PHONE, error = null) },
                        shape = SegmentedButtonDefaults.itemShape(index = 1, count = 2),
                    ) { Text(t("common.phone")) }
                }
                OutlinedTextField(
                    value = s.contact,
                    onValueChange = { v -> ui = s.copy(contact = v, error = null) },
                    label = {
                        Text(
                            if (s.kind == ContactKind.EMAIL) {
                                t("recovery.contact.email_label")
                            } else {
                                t("recovery.contact.phone_label")
                            },
                        )
                    },
                    placeholder = {
                        Text(
                            if (s.kind == ContactKind.EMAIL) {
                                "alice@example.com"
                            } else {
                                "+15551234567"
                            },
                        )
                    },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(
                        capitalization = KeyboardCapitalization.None,
                        autoCorrectEnabled = false,
                        keyboardType = if (s.kind == ContactKind.EMAIL) {
                            KeyboardType.Email
                        } else {
                            KeyboardType.Phone
                        },
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
                        // Light client-side shape check — saves a round
                        // trip on an obviously-malformed contact. The
                        // server is authoritative.
                        if (!isPlausibleContact(s.kind, s.contact)) {
                            ui = s.copy(
                                error = if (s.kind == ContactKind.EMAIL) {
                                    UiText("recovery.contact.error.invalid_email")
                                } else {
                                    UiText("recovery.contact.error.invalid_phone")
                                },
                            )
                            return@Button
                        }
                        val kind = s.kind
                        val contact = s.contact
                        ui = ContactUi.Starting
                        scope.launch {
                            ui = try {
                                AuthClient().use { client ->
                                    client.recoveryContactEnrollStart(
                                        sessionToken,
                                        kind,
                                        contact,
                                    )
                                }
                                ContactUi.Confirm(
                                    kind = kind,
                                    contact = contact,
                                    code = "",
                                    busy = false,
                                    error = null,
                                )
                            } catch (err: me.lakko.app.auth.AuthHttpError) {
                                when (err.status) {
                                    429 -> ContactUi.Idle(
                                        kind = kind,
                                        contact = contact,
                                        error = enrollFailure(err, CONTACT_GENERIC),
                                    )
                                    409 -> ContactUi.AlreadyEnabled
                                    503 -> ContactUi.Unavailable
                                    400 -> ContactUi.Idle(
                                        kind = kind,
                                        contact = contact,
                                        error = UiText("settings.recovery.contact.error.bad_contact"),
                                    )
                                    else -> ContactUi.Error(enrollFailure(err, CONTACT_GENERIC))
                                }
                            } catch (err: Throwable) {
                                ContactUi.Error(enrollFailure(err, CONTACT_GENERIC))
                            }
                        }
                    },
                ) { Text(t("recovery.contact.send")) }
            }

            ContactUi.Starting -> CircularProgressIndicator()

            is ContactUi.Confirm -> {
                Text(
                    t(
                        if (s.kind == ContactKind.EMAIL) {
                            "settings.recovery.contact.sent_email"
                        } else {
                            "settings.recovery.contact.sent_phone"
                        },
                        mapOf("contact" to s.contact),
                    ),
                    style = MaterialTheme.typography.bodySmall,
                )
                OutlinedTextField(
                    value = s.code,
                    onValueChange = { v -> ui = s.copy(code = v, error = null) },
                    label = { Text(t("recovery.contact.code_label")) },
                    placeholder = { Text("123456") },
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
                            ui = s.copy(error = UiText("recovery.contact.error.enter_code"))
                            return@Button
                        }
                        ui = s.copy(busy = true, error = null)
                        scope.launch {
                            ui = try {
                                AuthClient().use { client ->
                                    client.recoveryContactEnrollFinish(
                                        sessionToken,
                                        s.kind,
                                        normalized,
                                    )
                                }
                                ContactUi.Done
                            } catch (err: me.lakko.app.auth.AuthHttpError) {
                                when (err.status) {
                                    429 -> s.copy(
                                        busy = false,
                                        error = enrollFailure(err, CONTACT_GENERIC),
                                    )
                                    401 -> s.copy(
                                        busy = false,
                                        error = UiText("recovery.contact.error.wrong"),
                                    )
                                    409 -> ContactUi.AlreadyEnabled
                                    else -> ContactUi.Error(enrollFailure(err, CONTACT_GENERIC))
                                }
                            } catch (err: Throwable) {
                                ContactUi.Error(enrollFailure(err, CONTACT_GENERIC))
                            }
                        }
                    },
                    enabled = !s.busy,
                ) { Text(if (s.busy) t("common.confirming") else t("common.confirm")) }
            }

            ContactUi.Done -> Text(
                t("settings.recovery.contact.done"),
                color = MaterialTheme.colorScheme.primary,
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.SemiBold,
            )

            ContactUi.AlreadyEnabled -> Text(
                t("settings.recovery.contact.already_enabled"),
                style = MaterialTheme.typography.bodyMedium,
            )

            ContactUi.Unavailable -> Text(
                t("settings.recovery.contact.unavailable"),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.error,
            )

            is ContactUi.Error -> Column(
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    s.message.text(),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                )
                Button(
                    onClick = {
                        ui = ContactUi.Idle(
                            kind = ContactKind.EMAIL,
                            contact = "",
                            error = null,
                        )
                    },
                ) { Text(t("common.try_again")) }
            }
        }
    }
}

private val TOTP_GENERIC = UiText("settings.recovery.totp.error.generic")
private val CONTACT_GENERIC = UiText("settings.recovery.contact.error.generic")

/**
 * What an enrolment failure reads as (ADR-0058: from its kind, never from
 * server prose): a limit says when to try again, a network failure says so,
 * anything else is the flow's generic sentence.
 */
private fun enrollFailure(err: Throwable, generic: UiText): UiText = when (err) {
    is me.lakko.app.auth.AuthHttpError -> me.lakko.app.auth.authLimitText(err) ?: generic
    is me.lakko.app.auth.AuthNetworkError -> UiText("common.error.offline")
    else -> generic
}
