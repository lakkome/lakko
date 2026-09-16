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
                title = { Text("Recovery") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
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
                "If you lose this phone, these are the ways back into your account.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
            )
            if (account == null || secrets == null) {
                Text(
                    "Sign in first to manage recovery.",
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.padding(16.dp),
                )
                return@Column
            }

            SectionLabel("Identity")
            ListItem(
                headlineContent = { Text("Recovery phrase") },
                supportingContent = {
                    Text(
                        when (phraseOnDevice) {
                            null -> "Checking..."
                            true -> "Saved on this phone. Your 24 words restore your messages and identity on a new device."
                            false -> "Not stored on this phone. Keep your 24 words somewhere safe; they are the only way to restore your identity."
                        },
                    )
                },
                leadingContent = { Icon(Icons.Filled.Key, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant) },
                colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
            )

            SectionLabel("Access")
            ExpandableRow(
                title = "Authenticator app",
                supporting = "A 6-digit code from an app such as Aegis or Google Authenticator.",
                icon = Icons.Filled.Password,
                expanded = authenticatorOpen,
                onToggle = { authenticatorOpen = !authenticatorOpen },
            ) {
                TotpEnrollSection(sessionToken = secrets.sessionToken)
            }
            HorizontalDivider()
            ExpandableRow(
                title = "Email or phone",
                supporting = "A one-time code sent to you.",
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
                contentDescription = if (expanded) "Collapse" else "Expand",
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
        val error: String?,
    ) : TotpUi
    data object Done : TotpUi
    data object AlreadyEnabled : TotpUi
    data object Unavailable : TotpUi
    data class Error(val message: String) : TotpUi
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
            "If you lose this phone, a 6-digit code from your authenticator " +
                "app gets you back into your account. Your messages then " +
                "come back from your recovery phrase or another device.",
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
                                else -> TotpUi.Error(
                                    err.message ?: "Could not set this up (${err.status}).",
                                )
                            }
                        } catch (err: Throwable) {
                            TotpUi.Error(err.message ?: "Could not set this up")
                        }
                    }
                },
            ) { Text("Set up authenticator") }

            TotpUi.Starting -> CircularProgressIndicator()

            is TotpUi.Confirm -> {
                Text(
                    "Scan this QR with your authenticator app (or type " +
                        "the secret), then enter the current 6-digit code " +
                        "to confirm.",
                    style = MaterialTheme.typography.bodySmall,
                )
                QrTextImageComposable(text = s.otpauthUri)
                Text(
                    "Secret: ${s.secretB32}",
                    style = MaterialTheme.typography.bodySmall,
                    fontWeight = FontWeight.SemiBold,
                )
                OutlinedTextField(
                    value = s.code,
                    onValueChange = { v -> ui = s.copy(code = v, error = null) },
                    label = { Text("6-digit code") },
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
                        s.error,
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                Button(
                    onClick = {
                        val normalized = me.lakko.app.auth.normalizeTotpCode(s.code)
                        if (normalized == null) {
                            ui = s.copy(
                                error = "Enter the 6-digit code from the app.",
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
                                    401 -> s.copy(
                                        busy = false,
                                        error = "Wrong code. Check the app and try again.",
                                    )
                                    409 -> TotpUi.AlreadyEnabled
                                    else -> TotpUi.Error(
                                        err.message ?: "Confirmation failed (${err.status}).",
                                    )
                                }
                            } catch (err: Throwable) {
                                TotpUi.Error(err.message ?: "Confirmation failed")
                            }
                        }
                    },
                    enabled = !s.busy,
                ) { Text(if (s.busy) "Confirming..." else "Confirm") }
            }

            TotpUi.Done -> Text(
                "Authenticator enabled. A 6-digit code can now recover " +
                    "access to this account.",
                color = MaterialTheme.colorScheme.primary,
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.SemiBold,
            )

            TotpUi.AlreadyEnabled -> Text(
                "An authenticator is already enabled on this account.",
                style = MaterialTheme.typography.bodyMedium,
            )

            TotpUi.Unavailable -> Text(
                "Authenticator setup is not available right now.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.error,
            )

            is TotpUi.Error -> Column(
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    s.message,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                )
                Button(onClick = { ui = TotpUi.Idle }) { Text("Try again") }
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
        val error: String?,
    ) : ContactUi
    data object Starting : ContactUi
    /** A code was sent; collecting the confirm code. The kind +
     *  contact are carried so the finish targets the exact pair. */
    data class Confirm(
        val kind: Int,
        val contact: String,
        val code: String,
        val busy: Boolean,
        val error: String?,
    ) : ContactUi
    data object Done : ContactUi
    data object AlreadyEnabled : ContactUi
    data object Unavailable : ContactUi
    data class Error(val message: String) : ContactUi
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
            "If you lose this phone, a one-time code sent to your email " +
                "or phone gets you back into your account. Your messages " +
                "then come back from your recovery phrase or another device. " +
                "Lakko never stores your email or phone number in readable form.",
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
                    ) { Text("Email") }
                    SegmentedButton(
                        selected = s.kind == ContactKind.PHONE,
                        onClick = { ui = s.copy(kind = ContactKind.PHONE, error = null) },
                        shape = SegmentedButtonDefaults.itemShape(index = 1, count = 2),
                    ) { Text("SMS") }
                }
                OutlinedTextField(
                    value = s.contact,
                    onValueChange = { v -> ui = s.copy(contact = v, error = null) },
                    label = {
                        Text(
                            if (s.kind == ContactKind.EMAIL) {
                                "Email address"
                            } else {
                                "Phone number"
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
                        s.error,
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                Button(
                    onClick = {
                        // Light client-side shape check — saves a PoW
                        // solve on an obviously-malformed contact. The
                        // server is authoritative.
                        if (!isPlausibleContact(s.kind, s.contact)) {
                            ui = s.copy(
                                error = if (s.kind == ContactKind.EMAIL) {
                                    "Enter a valid email address."
                                } else {
                                    "Enter a phone number in international " +
                                        "format (e.g. +15551234567)."
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
                                    409 -> ContactUi.AlreadyEnabled
                                    503 -> ContactUi.Unavailable
                                    400 -> ContactUi.Idle(
                                        kind = kind,
                                        contact = contact,
                                        error = "That does not look right. " +
                                            "Check it and try again.",
                                    )
                                    else -> ContactUi.Error(
                                        err.message
                                            ?: "Could not set this up (${err.status}).",
                                    )
                                }
                            } catch (err: Throwable) {
                                ContactUi.Error(err.message ?: "Could not set this up")
                            }
                        }
                    },
                ) { Text("Send code") }
            }

            ContactUi.Starting -> CircularProgressIndicator()

            is ContactUi.Confirm -> {
                Text(
                    "Enter the 6-digit code we sent to " +
                        (if (s.kind == ContactKind.EMAIL) "your email." else "your phone.") +
                        " to confirm.",
                    style = MaterialTheme.typography.bodySmall,
                )
                OutlinedTextField(
                    value = s.code,
                    onValueChange = { v -> ui = s.copy(code = v, error = null) },
                    label = { Text("6-digit code") },
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
                        s.error,
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                Button(
                    onClick = {
                        val normalized = me.lakko.app.auth.normalizeTotpCode(s.code)
                        if (normalized == null) {
                            ui = s.copy(error = "Enter the 6-digit code we sent.")
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
                                    401 -> s.copy(
                                        busy = false,
                                        error = "Wrong or expired code. Check " +
                                            "the code we sent and try again.",
                                    )
                                    409 -> ContactUi.AlreadyEnabled
                                    else -> ContactUi.Error(
                                        err.message
                                            ?: "Confirmation failed (${err.status}).",
                                    )
                                }
                            } catch (err: Throwable) {
                                ContactUi.Error(err.message ?: "Confirmation failed")
                            }
                        }
                    },
                    enabled = !s.busy,
                ) { Text(if (s.busy) "Confirming..." else "Confirm") }
            }

            ContactUi.Done -> Text(
                "Recovery contact enabled. A one-time code can now " +
                    "recover access to this account.",
                color = MaterialTheme.colorScheme.primary,
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.SemiBold,
            )

            ContactUi.AlreadyEnabled -> Text(
                "A recovery contact is already enabled on this account.",
                style = MaterialTheme.typography.bodyMedium,
            )

            ContactUi.Unavailable -> Text(
                "Email or phone recovery is not available right now.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.error,
            )

            is ContactUi.Error -> Column(
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    s.message,
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
                ) { Text("Try again") }
            }
        }
    }
}
