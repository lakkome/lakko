// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import android.Manifest
import android.content.pm.PackageManager
import android.util.Base64
import android.util.Log
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.PhoneAndroid
import androidx.compose.material.icons.filled.QrCode
import androidx.compose.material.icons.filled.QrCodeScanner
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.auth.AuthClient
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.devices.DeviceRemoval
import me.lakko.app.devices.DeviceRemovalRunner
import me.lakko.app.devices.RemovalCopy
import me.lakko.app.devices.RemovalSheetOutcome
import me.lakko.app.devices.removalGate
import me.lakko.app.devices.removalGateLine
import me.lakko.app.devices.removalSheetOutcome
import me.lakko.app.auth.UniffiLoginHandleSource
import me.lakko.app.devices.PAIRING_FAILED_COPY
import me.lakko.app.devices.PAIRING_NO_USERNAME_COPY
import me.lakko.app.devices.PairingCodec
import me.lakko.app.devices.PairingOrchestrator
import me.lakko.app.devices.PairingScanDecision
import me.lakko.app.devices.decodeForOwnAccount
import me.lakko.app.devices.ownPairingHandle
import uniffi.lakko_ffi_uniffi.DecodedPairingQr
import me.lakko.app.devices.WelcomeCourier
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.webauthn.WebauthnClient
import me.lakko.app.webauthn.WebauthnHttpError
import me.lakko.app.webauthn.actionHashForRemoveDevice

/**
 * Settings > Security > Devices — docs/ux/client-redesign.md section 5.5.
 *
 * The list of this account's devices ("This device" for the local one,
 * otherwise "Device N": the device record carries no name, platform or added
 * date yet) with a Remove action behind a confirmation and the existing
 * passkey-gated removal (G-13, ADR-0014 / ADR-0015), and one "Add a device"
 * button that opens [AddDeviceScreen]. The former five-way tab row is gone:
 * the authenticator and email-or-phone enrolments live under Settings >
 * Recovery.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun DevicesScreen(
    authRepository: AuthRepository,
    onBack: () -> Unit,
    onAddDevice: () -> Unit,
    /** ADR-0050 — the chat side of a removal (retire its leaf from every
     *  chat). `null` in previews / tests. */
    chatRepository: me.lakko.app.chat.ChatRepository? = null,
    modifier: Modifier = Modifier,
    /** UX 17.5 — `Change password` on the line shown after a device that
     *  carried `New` was removed. */
    onChangePassword: () -> Unit = {},
) {
    val authState by authRepository.state.collectAsStateWithLifecycle()
    val identitySecrets by authRepository.identitySecrets.collectAsStateWithLifecycle()
    val account = (authState as? AuthState.Session)?.account
    val secrets = identitySecrets

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(t("settings.devices.title")) },
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
            // UX 15.3 (ADR-0065) - on the device that reset the account keys,
            // one dismissible note for the rest of the session: the other
            // devices were signed out and removed, add them again from here.
            val resetThisSession by authRepository.resetThisSession.collectAsStateWithLifecycle()
            if (resetThisSession) {
                androidx.compose.material3.Card(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 8.dp),
                ) {
                    Row(
                        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
                        modifier = Modifier.padding(start = 16.dp, top = 8.dp, bottom = 8.dp, end = 4.dp),
                    ) {
                        Text(
                            t("settings.devices.reset_note"),
                            style = MaterialTheme.typography.bodyMedium,
                            modifier = Modifier.weight(1f),
                        )
                        IconButton(onClick = { authRepository.dismissResetNote() }) {
                            Icon(Icons.Filled.Close, contentDescription = t("common.dismiss"))
                        }
                    }
                }
            }
            // UX 17.5 - a device was added to the account (the same banner
            // as on the Chats list; no `Open Devices` here, this is it), or
            // the line that replaces it after a `New` device was removed.
            NewDeviceNoticeBanner(onOpenDevices = null)
            RemovedNewDeviceLine(onChangePassword = onChangePassword)
            Text(
                t("settings.devices.lede"),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
            )

            if (account == null || secrets == null) {
                Text(
                    t("settings.devices.sign_in_first"),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.padding(16.dp),
                )
                return@Column
            }

            DevicesList(
                authRepository = authRepository,
                accountIdBytes = secrets.accountId,
                accountIdB64 = account.accountIdB64,
                chatRepository = chatRepository,
            )

            HorizontalDivider(modifier = Modifier.padding(vertical = 8.dp))
            Button(
                onClick = onAddDevice,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp),
            ) {
                Icon(Icons.Filled.Add, contentDescription = null)
                Text("  " + t("devices.add.title"))
            }
            Text(
                t("settings.devices.list.lede"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(16.dp),
            )
        }
    }
}

/** Which side of the pairing this phone plays. */
private enum class AddDeviceMode { Show, Scan }

/**
 * Settings > Devices > Add a device — two options: "Show a code" (this phone
 * is the new device; another signed-in device scans it) and "Scan a code"
 * (this phone admits a new device by scanning the code it shows). Each opens
 * the unchanged pairing flow of [PairingOrchestrator].
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun AddDeviceScreen(
    authRepository: AuthRepository,
    onBack: () -> Unit,
    /** ADR-0050 — admit the new device to every chat after a scan. */
    chatRepository: me.lakko.app.chat.ChatRepository? = null,
    modifier: Modifier = Modifier,
) {
    val authState by authRepository.state.collectAsStateWithLifecycle()
    val identitySecrets by authRepository.identitySecrets.collectAsStateWithLifecycle()
    val account = (authState as? AuthState.Session)?.account
    val secrets = identitySecrets
    var mode: AddDeviceMode? by remember { mutableStateOf(null) }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        when (mode) {
                            null -> t("devices.add.title")
                            AddDeviceMode.Show -> t("devices.add.side.show")
                            AddDeviceMode.Scan -> t("devices.add.side.scan")
                        },
                    )
                },
                navigationIcon = {
                    IconButton(onClick = { if (mode == null) onBack() else mode = null }) {
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
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (account == null || secrets == null) {
                Text(
                    t("devices.add.sign_in_first"),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                )
                return@Column
            }
            // ADR-0056 — a pairing code names the account by the login
            // handle of its username; an account with none (a guest) can
            // neither show nor confirm one.
            val ownHandle = remember(account.username) {
                ownPairingHandle(account.username, UniffiLoginHandleSource)
            }
            if (ownHandle == null) {
                Text(
                    PAIRING_NO_USERNAME_COPY.text(),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                return@Column
            }
            when (mode) {
                null -> {
                    Text(
                        t("settings.devices.add.lede"),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    OptionRow(
                        title = t("devices.add.side.show"),
                        supporting = t("devices.add.side.show_detail"),
                        icon = Icons.Filled.QrCode,
                        onClick = { mode = AddDeviceMode.Show },
                    )
                    OptionRow(
                        title = t("devices.add.side.scan"),
                        supporting = t("devices.add.side.scan_detail"),
                        icon = Icons.Filled.QrCodeScanner,
                        onClick = { mode = AddDeviceMode.Scan },
                    )
                }
                AddDeviceMode.Show -> PairSection(
                    loginHandle = ownHandle,
                    known = PairingOrchestrator.KnownAccount(
                        accountIdBytes = secrets.accountId,
                        accountIdB64 = account.accountIdB64,
                    ),
                )
                AddDeviceMode.Scan -> ScanSection(
                    authRepository = authRepository,
                    ownUsername = account.username,
                    accountIdBytes = secrets.accountId,
                    accountIdB64 = account.accountIdB64,
                    sessionToken = secrets.sessionToken,
                    chatRepository = chatRepository,
                )
            }
        }
    }
}

@Composable
private fun OptionRow(
    title: String,
    supporting: String,
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    onClick: () -> Unit,
) {
    ListItem(
        headlineContent = { Text(title, fontWeight = FontWeight.Medium) },
        supportingContent = { Text(supporting) },
        leadingContent = { Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.primary) },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surface),
        modifier = Modifier
            .clip(RoundedCornerShape(12.dp))
            .clickable(onClick = onClick),
    )
}

// =============================================================================
// Show a code — this phone is the new device
// =============================================================================

private sealed interface PairUi {
    data object Idle : PairUi
    data object Preparing : PairUi
    data class Waiting(val qrPayload: ByteArray) : PairUi {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Waiting) return false
            return qrPayload.contentEquals(other.qrPayload)
        }
        override fun hashCode(): Int = qrPayload.contentHashCode()
    }
    data object Joining : PairUi
    data object Done : PairUi
    data class Error(val message: UiText) : PairUi
}

@Composable
private fun PairSection(
    loginHandle: ByteArray,
    known: PairingOrchestrator.KnownAccount,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var state: PairUi by remember { mutableStateOf(PairUi.Idle) }
    var session: PairingOrchestrator.JoinSession? by remember { mutableStateOf(null) }
    var pollJob: Job? by remember { mutableStateOf(null) }

    DisposableEffect(Unit) {
        onDispose {
            pollJob?.cancel()
            pollJob = null
            session?.dispose()
            session = null
        }
    }

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(
            t("settings.devices.show.lede"),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        when (val s = state) {
            PairUi.Idle -> Button(
                onClick = {
                    state = PairUi.Preparing
                    try {
                        val js = PairingOrchestrator.prepareJoinFlow(
                            context = context,
                            loginHandle = loginHandle,
                            known = known,
                        )
                        session = js
                        state = PairUi.Waiting(js.qrPayload)
                        pollJob = scope.launch {
                            try {
                                when (val outcome = js.awaitWelcome()) {
                                    is WelcomeCourier.WelcomeOutcome.Received -> {
                                        state = PairUi.Joining
                                        try {
                                            // Phase 2a3d8-ui — pass the
                                            // Welcome cursor so `accept`
                                            // polls past it for the
                                            // scanner-side history bundle.
                                            js.accept(
                                                welcomeBytes = outcome.welcomeBytes,
                                                welcomeCursor = outcome.welcomeCursor,
                                            )
                                            state = PairUi.Done
                                        } catch (cause: Throwable) {
                                            Log.w(TAG, "WARN accept failed: ${cause.javaClass.simpleName}")
                                            state = PairUi.Error(LINK_FAILED)
                                        }
                                    }
                                    WelcomeCourier.WelcomeOutcome.Timeout -> {
                                        state = PairUi.Error(UiText("settings.devices.show.error.timeout"))
                                    }
                                    WelcomeCourier.WelcomeOutcome.Cancelled -> {
                                        state = PairUi.Idle
                                    }
                                }
                            } catch (cause: Throwable) {
                                Log.w(TAG, "WARN pairing failed: ${cause.javaClass.simpleName}")
                                state = PairUi.Error(LINK_FAILED)
                            }
                        }
                    } catch (cause: Throwable) {
                        Log.w(TAG, "WARN prepare failed: ${cause.javaClass.simpleName}")
                        state = PairUi.Error(LINK_FAILED)
                    }
                },
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(t("settings.devices.show.start"))
            }

            PairUi.Preparing -> Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                CircularProgressIndicator()
                Text(t("common.preparing"))
            }

            is PairUi.Waiting -> Column(
                modifier = Modifier.fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                QrImageComposable(
                    payload = s.qrPayload,
                    size = 240.dp,
                )
                Text(
                    t("device_link.qr.waiting"),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedButton(
                    onClick = {
                        pollJob?.cancel()
                        pollJob = null
                        session?.dispose()
                        session = null
                        state = PairUi.Idle
                    },
                ) {
                    Text(t("common.cancel"))
                }
            }

            PairUi.Joining -> Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                CircularProgressIndicator()
                Text(t("devices.add.joining"))
            }

            PairUi.Done -> Text(
                t("devices.add.linked"),
                color = MaterialTheme.colorScheme.primary,
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.SemiBold,
            )

            is PairUi.Error -> Column(
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    s.message.text(),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                )
                OutlinedButton(onClick = { state = PairUi.Idle }) {
                    Text(t("common.try_again"))
                }
            }
        }
    }
}

// =============================================================================
// Scan a code — this phone admits the new device
// =============================================================================

private sealed interface ScanUi {
    data object Idle : ScanUi
    data object NeedsPermission : ScanUi
    data object Scanning : ScanUi

    /** ADR-0018 / lakko-security finding #1 — a decoded pairing QR
     *  parks here until the user explicitly confirms: the scan
     *  transfers DeviceGroup membership AND the account's keys (the
     *  entropy of the recovery phrase) AND a session grant, so it must
     *  not run on a bare camera glance. */
    /** ADR-0056 — the payload was checked against this account's own
     *  login handle before this state is reached, so a code made for
     *  another account never asks for confirmation. */
    class ConfirmTransfer(val decoded: DecodedPairingQr) : ScanUi

    data object Processing : ScanUi
    /** [keysSent] is `false` when this device had no recovery phrase to hand
     *  over (or the hand-over failed): the new device is linked but has to
     *  bring its keys another way, and the screen says so. */
    data class Done(val keysSent: Boolean) : ScanUi
    data class Error(val message: UiText) : ScanUi
}

@Composable
private fun ScanSection(
    authRepository: AuthRepository,
    ownUsername: String,
    accountIdBytes: ByteArray,
    accountIdB64: String,
    sessionToken: String,
    chatRepository: me.lakko.app.chat.ChatRepository?,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    val initiallyGranted = ContextCompat.checkSelfPermission(
        context,
        Manifest.permission.CAMERA,
    ) == PackageManager.PERMISSION_GRANTED
    var state: ScanUi by remember { mutableStateOf(ScanUi.Idle) }
    // Transient inline hint shown over the live scanner when a non-Lakko QR is
    // seen — we keep scanning (no dead-end to Error / re-mount) and just nudge
    // the user (G-29). Cleared once a valid pairing QR is decoded.
    var scanHint: UiText? by remember { mutableStateOf(null) }
    // UX 17.4 - the proof block of `Link this device?`: the person proves
    // who they are before anything is sent to the scanned device.
    val proof = me.lakko.app.ui.auth.rememberProofController(
        authRepository,
        me.lakko.app.ui.auth.ProofSurface.AddDeviceScan,
    )
    var linking by remember { mutableStateOf(false) }

    val cameraPermissionLauncher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.RequestPermission(),
    ) { granted ->
        state = if (granted) ScanUi.Scanning else ScanUi.NeedsPermission
    }

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(
            t("devices.scan.intro"),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        when (val s = state) {
            ScanUi.Idle -> Button(
                onClick = {
                    if (initiallyGranted) {
                        state = ScanUi.Scanning
                    } else {
                        cameraPermissionLauncher.launch(Manifest.permission.CAMERA)
                    }
                },
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(t("devices.scan.open_camera"))
            }

            ScanUi.NeedsPermission -> Column(
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    t("devices.scan.error.camera_denied"),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                )
                OutlinedButton(
                    onClick = {
                        cameraPermissionLauncher.launch(Manifest.permission.CAMERA)
                    },
                ) {
                    Text(t("common.try_again"))
                }
            }

            ScanUi.Scanning -> Column(
                modifier = Modifier.fillMaxWidth(),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .aspectRatio(1f)
                        .clip(RoundedCornerShape(12.dp)),
                ) {
                    QrScannerComposable(
                        onDetected = { raw ->
                            // The analyser already de-dups by decoded value, so
                            // this fires once per distinct QR. Only act while
                            // still Scanning (second line of defence).
                            if (state !is ScanUi.Scanning) return@QrScannerComposable
                            val bytes = base64UrlDecodeNoPad(raw)
                            if (bytes == null) {
                                // Not a Lakko pairing payload — keep the scanner
                                // running and nudge the user. The composable
                                // won't re-fire for this same QR, but will for a
                                // different one, so the camera can pan straight
                                // onto the real code with no re-mount (G-29).
                                scanHint = UiText("devices.scan.error.not_a_code")
                                return@QrScannerComposable
                            }
                            // ADR-0056 — the code must name this
                            // account: its login handle is checked
                            // against the handle of our own username.
                            scanHint = null
                            state = when (
                                val decision = decodeForOwnAccount(
                                    scanned = bytes,
                                    ownUsername = ownUsername,
                                    source = UniffiLoginHandleSource,
                                    decode = PairingCodec::parsePairingQrPayload,
                                )
                            ) {
                                // ADR-0018 / lakko-security finding #1 —
                                // park for explicit confirmation instead
                                // of running the transfer on a bare decode.
                                is PairingScanDecision.Accepted ->
                                    ScanUi.ConfirmTransfer(decision.decoded)
                                is PairingScanDecision.Refused ->
                                    ScanUi.Error(decision.text)
                            }
                        },
                        onError = { _ ->
                            state = ScanUi.Error(PAIRING_FAILED_COPY)
                        },
                    )
                }
                scanHint?.let { hint ->
                    Text(
                        hint.text(),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
                OutlinedButton(
                    onClick = {
                        scanHint = null
                        state = ScanUi.Idle
                    },
                ) {
                    Text(t("common.cancel"))
                }
            }

            is ScanUi.ConfirmTransfer -> Column(
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(
                    t("devices.scan.confirm.title"),
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    t("devices.scan.confirm.body"),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                val proofView = proof.view()
                val link: () -> Unit = link@{
                    if (linking || !proof.view().mainEnabled) return@link
                    linking = true
                    scope.launch {
                        try {
                            // The proof FIRST (the typed password, or the
                            // passkey gesture); a failure leaves this step
                            // open with the scanned code kept and its line
                            // on the block. Nothing has been sent.
                            if (!proof.ensure(context)) return@launch
                            // Hand the new device the account's keys (the
                            // entropy of the recovery phrase this device
                            // holds) and a single-use grant alongside the
                            // Welcome. The grant is minted BEFORE the
                            // pairing starts and carries the proof. No
                            // stored phrase: no recovery row, and the
                            // result says so.
                            val recovery = PairingOrchestrator.scanRecoveryFromDevicePhrase(
                                context = context,
                                accountIdB64 = accountIdB64,
                                sessionToken = authRepository.identitySecrets.value?.sessionToken ?: sessionToken,
                                freshGrant = authRepository.freshGrant(),
                            )
                            state = ScanUi.Processing
                            val scanned = PairingOrchestrator.runScanFlow(
                                context = context,
                                decoded = s.decoded,
                                accountIdBytes = accountIdBytes,
                                accountIdB64 = accountIdB64,
                                recovery = recovery,
                            )
                            // ADR-0050 section 2.8 — admit the new
                            // device to every chat. Not awaited: it
                            // waits for the device to land and publish
                            // its KeyPackage.
                            chatRepository?.requestAdmitDevice(scanned.newDeviceKey)
                            state = ScanUi.Done(keysSent = scanned.keysSent)
                        } catch (cause: Throwable) {
                            if (cause is kotlinx.coroutines.CancellationException) throw cause
                            when (me.lakko.app.ui.auth.gatedFailureOf(cause)) {
                                // The proof ran out between the block and
                                // the grant: the block appears again, the
                                // scanned code is kept, nothing was sent.
                                me.lakko.app.ui.auth.GatedFailure.ProofRequired -> {
                                    proof.onProofRefused()
                                    state = ScanUi.ConfirmTransfer(s.decoded)
                                }
                                me.lakko.app.ui.auth.GatedFailure.Other ->
                                    state = ScanUi.Error(PAIRING_FAILED_COPY)
                            }
                        } finally {
                            linking = false
                        }
                    }
                }
                me.lakko.app.ui.auth.ProofBlock(
                    controller = proof,
                    onSubmit = link,
                    onPasskey = { scope.launch { proof.provePasskey(context) } },
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(
                        onClick = link,
                        enabled = proofView.mainEnabled && !linking,
                    ) { Text(if (proofView.working) t("common.checking") else t("devices.scan.confirm.submit")) }
                    OutlinedButton(
                        onClick = {
                            proof.clear()
                            state = ScanUi.Idle
                        },
                    ) { Text(t("common.cancel")) }
                }
            }

            ScanUi.Processing -> Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                CircularProgressIndicator()
                Text(t("device_link.request.adding"))
            }

            is ScanUi.Done -> Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    t("device_link.request.added"),
                    color = MaterialTheme.colorScheme.primary,
                    style = MaterialTheme.typography.bodyMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                if (!s.keysSent) {
                    Text(
                        t("device_link.keys_not_sent"),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            is ScanUi.Error -> Column(
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    s.message.text(),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                )
                OutlinedButton(onClick = { state = ScanUi.Idle }) {
                    Text(t("common.try_again"))
                }
            }
        }
    }
}

/**
 * Reverse [QrImageComposable]'s base64url-no-pad text →
 * bytes encoding. Returns `null` on malformed input. Mirror
 * of `apps/ios`'s `base64UrlDecodeNoPad` + `apps/web`'s same
 * decoder.
 */
private fun base64UrlDecodeNoPad(text: String): ByteArray? {
    return try {
        Base64.decode(text, Base64.URL_SAFE or Base64.NO_PADDING or Base64.NO_WRAP)
    } catch (_: IllegalArgumentException) {
        null
    }
}

// =============================================================================
// The device list (list + remove)
// =============================================================================

private sealed interface DevicesListUi {
    data object Loading : DevicesListUi
    data class Ready(val devices: List<DeviceRemoval.DeviceListItem>) : DevicesListUi
    data class Error(val message: UiText) : DevicesListUi
}

private const val TAG = "DevicesScreen"
private val LINK_FAILED = UiText("devices.add.error.generic")

/** The row label: "This device" for the local one, else "Device N" by list
 *  order (no name, platform or added date is recorded yet). */
internal fun deviceRowLabel(isSelf: Boolean, index: Int): UiText =
    if (isSelf) {
        UiText("settings.devices.this_device")
    } else {
        UiText("settings.devices.device_n", texts = mapOf("number" to (index + 1).toString()))
    }

@Composable
private fun DevicesList(
    authRepository: AuthRepository,
    accountIdBytes: ByteArray,
    accountIdB64: String,
    chatRepository: me.lakko.app.chat.ChatRepository?,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var state: DevicesListUi by remember { mutableStateOf(DevicesListUi.Loading) }
    var confirming: Pair<DeviceRemoval.DeviceListItem, String>? by remember { mutableStateOf(null) }
    // ADR-0066 - the devices already signed out at auth whose removal is
    // still finishing: their rows read `Removing...` and carry no control.
    var pending: Set<String> by remember { mutableStateOf(emptySet()) }
    // Of those, the ones whose owed step waits on the person's passkey:
    // their rows also carry `Finish removing`.
    var waitingOnPerson: Set<String> by remember { mutableStateOf(emptySet()) }
    var finishing: String? by remember { mutableStateOf(null) }
    // `Device removed and signed out.` after a removal whose every step answered.
    var removedDone by remember { mutableStateOf(false) }
    // `Your account keys were changed...` after "remove and change keys".
    var rotatedDone by remember { mutableStateOf(false) }
    // Non-null while the new phrase screen is up; the value says whether
    // the words are withheld (the publish is still owed).
    var rotatedPhrase: Boolean? by remember { mutableStateOf(null) }
    // UX 17.5 - which rows carry `New`.
    val notice by me.lakko.app.devices.NewDeviceNotices.state.collectAsStateWithLifecycle()

    fun runner(): DeviceRemovalRunner? = DeviceRemovalRunner.onDevice(context, authRepository, chatRepository)

    fun refresh() {
        state = DevicesListUi.Loading
        try {
            val list = DeviceRemoval.listDevices(
                context = context,
                accountIdBytes = accountIdBytes,
                accountIdB64 = accountIdB64,
            )
            val active = runner()
            pending = active?.pendingDeviceIds().orEmpty()
            waitingOnPerson = active?.waitingOnPersonDeviceIds().orEmpty()
            state = DevicesListUi.Ready(list.devices)
        } catch (cause: Throwable) {
            Log.w(TAG, "WARN device list failed: ${cause.javaClass.simpleName}")
            state = DevicesListUi.Error(UiText("settings.devices.error.load"))
        }
    }

    LaunchedEffect(Unit) {
        refresh()
        // A removal an earlier session left unfinished is finished now (it
        // is also retried at every landing by the app root).
        val resumed = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
            try {
                runner()?.resume().orEmpty()
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                emptyMap()
            }
        }
        if (resumed.isNotEmpty()) refresh()
        // A rotation that passed its commit finishes by itself too.
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
            try {
                me.lakko.app.devices.KeyRotationDevice.runner(context, authRepository, chatRepository)?.resume()
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
            }
        }
    }

    when (val s = state) {
        DevicesListUi.Loading -> Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.padding(16.dp),
        ) {
            CircularProgressIndicator()
            Text(t("common.loading"))
        }

        is DevicesListUi.Ready -> {
            s.devices.forEachIndexed { index, device ->
                val label = deviceRowLabel(device.isSelf, index).text()
                val isNew = !device.isSelf && me.lakko.app.devices.isNewDevice(
                    notice,
                    me.lakko.app.devices.newDeviceKey(device.signaturePublicKey),
                )
                val deviceIdB64Url = me.lakko.app.auth.base64UrlEncodeNoPad(device.signaturePublicKey)
                val isPending = !device.isSelf && deviceIdB64Url in pending
                val needsFinish = isPending && deviceIdB64Url in waitingOnPerson
                ListItem(
                    headlineContent = { Text(label, fontWeight = FontWeight.Medium) },
                    supportingContent = when {
                        device.isSelf -> {
                            { Text(t("settings.devices.this_device.sub")) }
                        }
                        // Signed out at auth; the rest finishes by itself.
                        isPending -> {
                            { Text(t(RemovalCopy.ROW_PENDING)) }
                        }
                        // A text label, never colour alone.
                        isNew -> {
                            {
                                Text(
                                    t("settings.devices.new_badge"),
                                    color = MaterialTheme.colorScheme.primary,
                                    fontWeight = FontWeight.SemiBold,
                                )
                            }
                        }
                        else -> null
                    },
                    leadingContent = {
                        Icon(
                            if (device.isSelf) Icons.Filled.PhoneAndroid else Icons.Filled.Devices,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    },
                    trailingContent = if (needsFinish) {
                        {
                            // The owed step needs the passkey again (after a
                            // relaunch, or once the attestation aged out):
                            // the gesture, then the steps still owed.
                            TextButton(
                                enabled = finishing == null,
                                onClick = {
                                    finishing = deviceIdB64Url
                                    scope.launch {
                                        try {
                                            val attestation =
                                                removalAttestation(context, authRepository, device.signaturePublicKey)
                                            val result = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                                                runner()?.remove(device.signaturePublicKey, attestation)
                                            }
                                            if (result == me.lakko.app.devices.RemovalResult.Done) removedDone = true
                                        } catch (cause: Throwable) {
                                            if (cause is kotlinx.coroutines.CancellationException) throw cause
                                            // A dismissed gesture, or no
                                            // answer: the row keeps its button.
                                            Log.w(TAG, "WARN finishing a device removal: ${cause.javaClass.simpleName}")
                                        } finally {
                                            finishing = null
                                            refresh()
                                        }
                                    }
                                },
                            ) { Text(t(RemovalCopy.ROW_FINISH)) }
                        }
                    } else if (device.isSelf || isPending) {
                        null
                    } else {
                        {
                            TextButton(onClick = { confirming = device to label }) {
                                Text(t("common.remove"), color = MaterialTheme.colorScheme.error)
                            }
                        }
                    },
                    colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
                )
            }
            if (rotatedDone) {
                Text(
                    t(me.lakko.app.devices.RotationCopy.DONE),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier
                        .padding(horizontal = 16.dp, vertical = 8.dp)
                        .semantics { liveRegion = LiveRegionMode.Polite },
                )
            }
            if (removedDone) {
                Text(
                    t(RemovalCopy.DONE),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier
                        .padding(horizontal = 16.dp, vertical = 8.dp)
                        .semantics { liveRegion = LiveRegionMode.Polite },
                )
            }
        }

        is DevicesListUi.Error -> Column(
            verticalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.padding(16.dp),
        ) {
            Text(
                s.message.text(),
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodyMedium,
            )
            OutlinedButton(onClick = { refresh() }) { Text(t("common.try_again")) }
        }
    }

    // ADR-0066 part 3 - the new recovery phrase after "remove and change
    // keys" (withheld while the publish is owed), then the done line.
    rotatedPhrase?.let { withheld ->
        RotatedPhraseScreen(
            authRepository = authRepository,
            chatRepository = chatRepository,
            initiallyWithheld = withheld,
            onClosed = {
                rotatedPhrase = null
                rotatedDone = true
                refresh()
            },
        )
    }

    confirming?.let { (device, label) ->
        RemoveDeviceSheet(
            authRepository = authRepository,
            chatRepository = chatRepository,
            onRotated = { withheld ->
                confirming = null
                // ADR-0067 - new words are shown only to a phrase-protected
                // account; otherwise the change is done as far as the person
                // is concerned.
                if (authRepository.showsNewWordsHere()) {
                    rotatedPhrase = withheld
                } else {
                    rotatedDone = true
                    refresh()
                }
            },
            device = device,
            label = label,
            runner = ::runner,
            wasNew = me.lakko.app.devices.isNewDevice(
                notice,
                me.lakko.app.devices.newDeviceKey(device.signaturePublicKey),
            ),
            onClosed = { removedOrPending, done ->
                confirming = null
                if (done) removedDone = true
                if (removedOrPending) refresh()
            },
        )
    }
}

/**
 * `Remove this device?` (`docs/ux/identity-recovery.md` section 18.1; ADR-0066
 * section 2.3): the app's own modal, with the proof block. The main button
 * runs the whole removal as one operation ([DeviceRemovalRunner]): the proof,
 * the sign-out at auth, the directory row, the device list, the chats.
 *
 * Nothing is removed unless the sign-out at auth answered; from there on the
 * rest finishes by itself, now or the next time this device is online.
 *
 * The radio pair of 18.1 chooses between the plain removal and "It was lost
 * or stolen": remove AND change the account keys, keeping the other devices
 * ([me.lakko.app.devices.KeyRotationRunner], ADR-0066 part 3).
 */
@Composable
private fun RemoveDeviceSheet(
    authRepository: AuthRepository,
    chatRepository: me.lakko.app.chat.ChatRepository?,
    /** "Remove and change keys" changed the keys: close the sheet and show
     *  the new phrase ([withheld] while the publish is owed). */
    onRotated: (withheld: Boolean) -> Unit,
    device: DeviceRemoval.DeviceListItem,
    label: String,
    runner: () -> DeviceRemovalRunner?,
    wasNew: Boolean,
    onClosed: (removedOrPending: Boolean, done: Boolean) -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val proof = me.lakko.app.ui.auth.rememberProofController(
        authRepository,
        me.lakko.app.ui.auth.ProofSurface.RemoveDevice,
        key = device.leafIndex,
    )
    var removing by remember { mutableStateOf(false) }
    var line: UiText? by remember { mutableStateOf(null) }
    // UX 18.1 - the radio pair; neither is selected at first and the main
    // button is disabled until one is.
    var reason: me.lakko.app.devices.RemovalReason? by remember { mutableStateOf(null) }
    // "Remove and change keys": the active line of the step list.
    var rotationStep: me.lakko.app.devices.RotationUiStep? by remember { mutableStateOf(null) }
    // The device WAS removed and the keys were not changed: Try again / Not now.
    var notStarted by remember { mutableStateOf(false) }
    val view = proof.view()
    val working = removing || view.working
    val stolen = reason == me.lakko.app.devices.RemovalReason.Stolen

    fun rotationRunner(attestation: ByteArray?): me.lakko.app.devices.KeyRotationRunner? =
        me.lakko.app.devices.KeyRotationDevice.runner(
            context = context,
            authRepository = authRepository,
            chatRepository = chatRepository,
            deviceId = device.signaturePublicKey,
            attestation = attestation,
            onStep = { step -> rotationStep = step },
        )

    val rotate: () -> Unit = rotate@{
        if (removing || !proof.view().mainEnabled) return@rotate
        removalGateLine(
            removalGate(holdsAccountKeys = true, keyResetPublishOwed = authRepository.publishOwed()),
        )?.let {
            line = it
            return@rotate
        }
        removing = true
        line = null
        scope.launch {
            try {
                // The proof FIRST: the removal and the key change both need it.
                if (!proof.ensure(context)) return@launch
                val deviceKey = device.signaturePublicKey
                kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                    authRepository.ensureIdentityKeyRecorded()
                }
                // The passkey gesture for the device list's removal comes
                // before anything is sent, as for a plain removal. A retry of
                // a rotation whose device is already out of the list needs none.
                val removalAlreadyWon = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                    rotationRunner(null)?.owed()?.removalWon == true
                }
                val attestation = if (removalAlreadyWon) null else removalAttestation(context, authRepository, deviceKey)
                val (result, withheld) = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                    val active = rotationRunner(attestation) ?: throw IllegalStateException("no session")
                    val result = active.rotate(deviceKey)
                    result to (active.owed()?.let { me.lakko.app.devices.rotationWithholdsPhrase(it) } == true)
                }
                when (val outcome = me.lakko.app.devices.rotationSheetOutcome(result, withheld)) {
                    is me.lakko.app.devices.RotationSheetOutcome.ShowPhrase -> {
                        me.lakko.app.devices.NewDeviceNotices.onRemovedHere(
                            me.lakko.app.devices.newDeviceKey(deviceKey),
                            wasNew,
                        )
                        proof.clear()
                        onRotated(outcome.withheld)
                    }
                    me.lakko.app.devices.RotationSheetOutcome.ShowProofBlock -> proof.onProofRefused()
                    is me.lakko.app.devices.RotationSheetOutcome.NothingRemoved -> line = outcome.text
                    is me.lakko.app.devices.RotationSheetOutcome.NotStarted -> {
                        notStarted = true
                        line = outcome.extra
                    }
                }
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                // Thrown before anything was sent (the passkey gesture, a
                // missing session): nothing was removed.
                Log.w(TAG, "WARN key rotation not started: ${cause.javaClass.simpleName}")
                line = UiText(RemovalCopy.ERROR_NOT_REVOKED)
            } finally {
                removing = false
                rotationStep = null
            }
        }
    }

    val remove: () -> Unit = remove@{
        if (removing || !proof.view().mainEnabled) return@remove
        // A reset's publish still owed: auth holds the new key and the
        // directory the old one, so a removal waits for it (existing copy).
        removalGateLine(
            removalGate(holdsAccountKeys = true, keyResetPublishOwed = authRepository.publishOwed()),
        )?.let {
            line = it
            return@remove
        }
        removing = true
        line = null
        scope.launch {
            try {
                // The proof FIRST; a failure leaves its line on the block
                // and nothing has been sent.
                if (!proof.ensure(context)) return@launch
                val deviceKey = device.signaturePublicKey
                // Auth must hold the account's identity key before it will
                // take a removal; an account that never recorded one does so
                // now (idempotent).
                kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                    authRepository.ensureIdentityKeyRecorded()
                }
                // G-13 (ADR-0015): when the account has a passkey, the device
                // list accepts the removal only with an attestation bound to
                // the removed leaf. The gesture comes BEFORE the sign-out at
                // auth, so a cancelled gesture removes nothing. A 404 = no
                // passkey. (On the caller's dispatcher: it opens the
                // credential sheet.)
                val attestation = removalAttestation(context, authRepository, deviceKey)
                val result = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                    val active = runner() ?: throw IllegalStateException("no session")
                    active.remove(deviceKey, attestation)
                }
                when (val outcome = removalSheetOutcome(result)) {
                    RemovalSheetOutcome.Close -> {
                        // UX 17.5 - a removed `New` device: the line that
                        // replaces the banner.
                        me.lakko.app.devices.NewDeviceNotices.onRemovedHere(
                            me.lakko.app.devices.newDeviceKey(deviceKey),
                            wasNew,
                        )
                        proof.clear()
                        onClosed(true, result == me.lakko.app.devices.RemovalResult.Done)
                    }
                    RemovalSheetOutcome.ShowProofBlock -> proof.onProofRefused()
                    is RemovalSheetOutcome.Line -> line = outcome.text
                }
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                // Thrown before the sign-out at auth (the passkey gesture, a
                // missing session): nothing was removed.
                Log.w(TAG, "WARN device removal not started: ${cause.javaClass.simpleName}")
                line = UiText(RemovalCopy.ERROR_NOT_REVOKED)
            } finally {
                removing = false
            }
        }
    }

    val main: () -> Unit = if (stolen) rotate else remove
    me.lakko.app.ui.components.LakkoModal(
        onDismissRequest = { if (!working) onClosed(notStarted, false) },
        title = if (stolen) {
            t(me.lakko.app.devices.RotationCopy.TITLE)
        } else {
            t(RemovalCopy.SHEET_TITLE, mapOf("device" to label))
        },
        actions = {
            if (notStarted) {
                // The device was removed; the keys were not changed.
                TextButton(
                    enabled = !working,
                    onClick = {
                        scope.launch {
                            kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                                rotationRunner(null)?.abandon()
                            }
                            proof.clear()
                            onClosed(true, false)
                        }
                    },
                ) { Text(t(me.lakko.app.devices.RotationCopy.NOT_NOW)) }
                TextButton(onClick = rotate, enabled = view.mainEnabled && !working) {
                    Text(t(me.lakko.app.devices.RotationCopy.RETRY))
                }
            } else {
                TextButton(
                    onClick = {
                        proof.clear()
                        onClosed(false, false)
                    },
                    // UX 18.1: Cancel is disabled from the first step.
                    enabled = !removing,
                ) { Text(t("common.cancel")) }
                TextButton(onClick = main, enabled = reason != null && view.mainEnabled && !working) {
                    Text(
                        when {
                            removing && !stolen -> t(RemovalCopy.WORKING)
                            view.working -> t("common.checking")
                            stolen -> t(me.lakko.app.devices.RotationCopy.CONFIRM)
                            else -> t(RemovalCopy.CONFIRM)
                        },
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }
        },
    ) {
        if (notStarted) {
            Text(
                t(me.lakko.app.devices.RotationCopy.NOT_STARTED),
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            )
        } else if (stolen) {
            Text(t(me.lakko.app.devices.RotationCopy.LEDE))
        } else {
            Text(t(RemovalCopy.SHEET_BODY))
        }
        val activeStep = rotationStep
        if (stolen && removing && activeStep != null) {
            // The step list replaces the bullets, one line active at a time.
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                for ((step, key) in me.lakko.app.devices.ROTATION_STEP_LINES) {
                    val active = step == activeStep
                    Text(
                        t(key),
                        fontWeight = if (active) FontWeight.SemiBold else FontWeight.Normal,
                        color = if (active) {
                            MaterialTheme.colorScheme.onSurface
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                        // Each step is announced once, when it becomes active.
                        modifier = if (active) {
                            Modifier.semantics { liveRegion = LiveRegionMode.Polite }
                        } else {
                            Modifier
                        },
                    )
                }
            }
        } else if (stolen && !notStarted) {
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                for (key in me.lakko.app.devices.RotationCopy.EFFECTS) {
                    Text("- " + t(key), style = MaterialTheme.typography.bodyMedium)
                }
            }
        }
        if (!notStarted && !removing) {
            RemovalReasonPair(selected = reason, onSelect = { reason = it })
        }
        me.lakko.app.ui.auth.ProofBlock(
            controller = proof,
            onSubmit = main,
            onPasskey = { scope.launch { proof.provePasskey(context) } },
        )
        line?.let {
            Text(
                it.text(),
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodyMedium,
            )
        }
    }
}

/** UX 18.1 - the two reasons as ONE radio group; neither selected at first. */
@Composable
private fun RemovalReasonPair(
    selected: me.lakko.app.devices.RemovalReason?,
    onSelect: (me.lakko.app.devices.RemovalReason) -> Unit,
) {
    val groupLabel = t(RemovalCopy.SHEET_BODY)
    Column(
        modifier = Modifier
            .selectableGroup()
            .semantics { contentDescription = groupLabel },
    ) {
        for (
            (reason, key) in listOf(
                me.lakko.app.devices.RemovalReason.Unused to me.lakko.app.devices.RotationCopy.REASON_UNUSED,
                me.lakko.app.devices.RemovalReason.Stolen to me.lakko.app.devices.RotationCopy.REASON_STOLEN,
            )
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .selectable(
                        selected = selected == reason,
                        onClick = { onSelect(reason) },
                        role = androidx.compose.ui.semantics.Role.RadioButton,
                    )
                    .padding(vertical = 8.dp),
            ) {
                androidx.compose.material3.RadioButton(selected = selected == reason, onClick = null)
                Text(t(key), modifier = Modifier.padding(start = 12.dp))
            }
        }
    }
}

/**
 * The new recovery phrase after "remove and change keys" (UX 18.1 "Done":
 * section 9.1's screen, unchanged). While the publish of the new keys is
 * still owed the words are withheld with 15.4's copy and `Try again`
 * repeats only the steps that did not land.
 */
@Composable
internal fun RotatedPhraseScreen(
    authRepository: AuthRepository,
    chatRepository: me.lakko.app.chat.ChatRepository?,
    initiallyWithheld: Boolean,
    onClosed: () -> Unit,
    /** ADR-0067 / UX 19.5 - "Save a recovery phrase" shows the same screen
     *  with its own words around the grid. */
    title: String = t(me.lakko.app.devices.RotationCopy.PHRASE_TITLE),
    lead: String = t("devices.rotate.effect_phrase") + "\n\n" + t("devices.rotate.effect_devices"),
    /** The line while the publish of the new keys is still owed. */
    withheldLine: String = t(me.lakko.app.devices.RotationCopy.PUBLISH_OWED),
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var withheld by remember { mutableStateOf(initiallyWithheld) }
    var retrying by remember { mutableStateOf(false) }
    var phrase: String? by remember { mutableStateOf(null) }
    LaunchedEffect(withheld) {
        if (!withheld) {
            phrase = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { authRepository.devicePhrase() }
        }
    }
    androidx.compose.ui.window.Dialog(
        onDismissRequest = {},
        properties = androidx.compose.ui.window.DialogProperties(
            usePlatformDefaultWidth = false,
            dismissOnBackPress = false,
            dismissOnClickOutside = false,
        ),
    ) {
        androidx.compose.material3.Surface(modifier = Modifier.fillMaxSize()) {
            val words = phrase
            if (withheld) {
                Column(
                    verticalArrangement = Arrangement.spacedBy(16.dp),
                    modifier = Modifier.padding(24.dp),
                ) {
                    Text(title, style = MaterialTheme.typography.headlineSmall)
                    Text(
                        withheldLine,
                        modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                    )
                    OutlinedButton(
                        enabled = !retrying,
                        onClick = {
                            retrying = true
                            scope.launch {
                                try {
                                    val still = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                                        val runner = me.lakko.app.devices.KeyRotationDevice
                                            .runner(context, authRepository, chatRepository)
                                        runner?.resume()
                                        runner?.owed()?.let { me.lakko.app.devices.rotationWithholdsPhrase(it) } == true
                                    }
                                    withheld = still
                                } catch (cause: Throwable) {
                                    if (cause is kotlinx.coroutines.CancellationException) throw cause
                                    Log.w(TAG, "WARN key rotation publish retry: ${cause.javaClass.simpleName}")
                                } finally {
                                    retrying = false
                                }
                            }
                        },
                    ) { Text(if (retrying) t("common.checking") else t(me.lakko.app.devices.RotationCopy.RETRY)) }
                    TextButton(onClick = onClosed, enabled = !retrying) {
                        Text(t(me.lakko.app.devices.RotationCopy.NOT_NOW))
                    }
                }
            } else if (words != null) {
                me.lakko.app.ui.auth.RecoveryPhraseOnboarding(
                    phrase = words,
                    onDone = onClosed,
                    title = title,
                    lead = lead,
                )
            } else {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    modifier = Modifier.padding(24.dp),
                ) {
                    CircularProgressIndicator()
                    Text(t("common.loading"))
                }
            }
        }
    }
}

/** The ADR-0015 removal attestation for [deviceKey], or `null` when the
 *  account has no passkey. A cancelled or failed gesture throws. */
private suspend fun removalAttestation(
    context: android.content.Context,
    authRepository: AuthRepository,
    deviceKey: ByteArray,
): ByteArray? {
    val sessionToken = authRepository.identitySecrets.value?.sessionToken ?: return null
    return WebauthnClient().use { wc ->
        wc.setSessionToken(sessionToken)
        try {
            val result = wc.assertAction(
                context = context,
                actionHashB64 = actionHashForRemoveDevice(deviceKey),
            )
            base64UrlDecodeNoPad(result.attestationB64)
        } catch (e: WebauthnHttpError) {
            if (e.status != 404) throw e
            null
        }
    }
}
