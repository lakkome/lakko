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
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.PhoneAndroid
import androidx.compose.material.icons.filled.QrCode
import androidx.compose.material.icons.filled.QrCodeScanner
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material3.AlertDialog
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
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthClient
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.devices.DeviceRemoval
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
                accountIdBytes = secrets.accountId,
                accountIdB64 = account.accountIdB64,
                sessionToken = secrets.sessionToken,
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
                    ownUsername = account.username,
                    accountIdBytes = secrets.accountId,
                    accountIdB64 = account.accountIdB64,
                    accountIdentitySeed = secrets.accountIdentitySeed,
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
                                            me.lakko.app.rethrowIfCancelled(cause)
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
                                me.lakko.app.rethrowIfCancelled(cause)
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
     *  parks here until the user explicitly confirms: the scan now
     *  transfers DeviceGroup membership AND the identity seed AND a
     *  session grant, so it must not run on a bare camera glance. */
    /** ADR-0056 — the payload was checked against this account's own
     *  login handle before this state is reached, so a code made for
     *  another account never asks for confirmation. */
    class ConfirmTransfer(val decoded: DecodedPairingQr) : ScanUi

    data object Processing : ScanUi
    data object Done : ScanUi
    data class Error(val message: UiText) : ScanUi
}

@Composable
private fun ScanSection(
    ownUsername: String,
    accountIdBytes: ByteArray,
    accountIdB64: String,
    accountIdentitySeed: ByteArray,
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
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(
                        onClick = {
                            state = ScanUi.Processing
                            scope.launch {
                                try {
                                    // ADR-0018 factor (a): seal the
                                    // identity seed + a single-use
                                    // grant to the new device alongside
                                    // the Welcome (best-effort inside
                                    // runScanFlow; the copy is consumed
                                    // + zeroized there).
                                    val scanned = PairingOrchestrator.runScanFlow(
                                        context = context,
                                        decoded = s.decoded,
                                        accountIdBytes = accountIdBytes,
                                        accountIdB64 = accountIdB64,
                                        recovery = PairingOrchestrator.ScanRecovery(
                                            accountIdentitySeed =
                                                accountIdentitySeed.copyOf(),
                                            deviceAuthorizeStart = {
                                                AuthClient().use { client ->
                                                    client.deviceAuthorizeStart(
                                                        sessionToken,
                                                    )
                                                }
                                            },
                                        ),
                                    )
                                    // ADR-0050 section 2.8 — admit the new
                                    // device to every chat. Not awaited: it
                                    // waits for the device to land and publish
                                    // its KeyPackage.
                                    chatRepository?.requestAdmitDevice(scanned.newDeviceKey)
                                    state = ScanUi.Done
                                } catch (cause: Throwable) {
                                    me.lakko.app.rethrowIfCancelled(cause)
                                    state = ScanUi.Error(PAIRING_FAILED_COPY)
                                }
                            }
                        },
                    ) { Text(t("devices.scan.confirm.submit")) }
                    OutlinedButton(
                        onClick = { state = ScanUi.Idle },
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

            ScanUi.Done -> Text(
                t("device_link.request.added"),
                color = MaterialTheme.colorScheme.primary,
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.SemiBold,
            )

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
    accountIdBytes: ByteArray,
    accountIdB64: String,
    sessionToken: String?,
    chatRepository: me.lakko.app.chat.ChatRepository?,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var state: DevicesListUi by remember { mutableStateOf(DevicesListUi.Loading) }
    var removingLeaf: UInt? by remember { mutableStateOf(null) }
    var confirming: Pair<DeviceRemoval.DeviceListItem, String>? by remember { mutableStateOf(null) }

    fun refresh() {
        state = DevicesListUi.Loading
        try {
            val list = DeviceRemoval.listDevices(
                context = context,
                accountIdBytes = accountIdBytes,
                accountIdB64 = accountIdB64,
            )
            state = DevicesListUi.Ready(list.devices)
        } catch (cause: Throwable) {
            Log.w(TAG, "WARN device list failed: ${cause.javaClass.simpleName}")
            state = DevicesListUi.Error(UiText("settings.devices.error.load"))
        }
    }

    LaunchedEffect(Unit) { refresh() }

    fun remove(device: DeviceRemoval.DeviceListItem) {
        val leaf = device.leafIndex
        val signaturePublicKey = device.signaturePublicKey
        removingLeaf = leaf
        scope.launch {
            try {
                // G-13 (ADR-0014): gate the eviction on a fresh passkey gesture
                // cryptographically bound to the removed leaf, but only if the
                // account actually has a passkey. Accounts without one (a 404
                // from the ceremony start) fall through on session-token auth
                // alone. A cancelled gesture or any other failure aborts the
                // removal (fail-closed).
                // G-13 (ADR-0015): the ceremony yields an attestation bound to the
                // removed leaf; it rides with the Remove commit so peers verify it.
                var attestation: ByteArray? = null
                if (sessionToken != null) {
                    WebauthnClient().use { wc ->
                        wc.setSessionToken(sessionToken)
                        try {
                            val actionHashB64 = actionHashForRemoveDevice(signaturePublicKey)
                            val result = wc.assertAction(
                                context = context,
                                actionHashB64 = actionHashB64,
                            )
                            attestation = base64UrlDecodeNoPad(result.attestationB64)
                        } catch (e: WebauthnHttpError) {
                            if (e.status != 404) throw e
                        }
                    }
                }

                DeviceRemoval.removeDeviceAt(
                    context = context,
                    accountIdBytes = accountIdBytes,
                    accountIdB64 = accountIdB64,
                    leafIndex = leaf,
                    attestation = attestation,
                )
                // ADR-0050 section 2.10 — out of every chat and out of the
                // directory.
                chatRepository?.requestRetireDevice(signaturePublicKey)
                removingLeaf = null
                refresh()
            } catch (cause: Throwable) {
                me.lakko.app.rethrowIfCancelled(cause) {
                    removingLeaf = null
                }
                removingLeaf = null
                Log.w(TAG, "WARN device removal failed: ${cause.javaClass.simpleName}")
                state = DevicesListUi.Error(UiText("settings.devices.error.remove"))
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
                ListItem(
                    headlineContent = { Text(label, fontWeight = FontWeight.Medium) },
                    supportingContent = if (device.isSelf) {
                        { Text(t("settings.devices.this_device.sub")) }
                    } else {
                        null
                    },
                    leadingContent = {
                        Icon(
                            if (device.isSelf) Icons.Filled.PhoneAndroid else Icons.Filled.Devices,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    },
                    trailingContent = if (device.isSelf) {
                        null
                    } else {
                        {
                            TextButton(
                                onClick = { confirming = device to label },
                                enabled = removingLeaf == null,
                            ) {
                                Text(
                                    if (removingLeaf == device.leafIndex) t("common.removing") else t("common.remove"),
                                    color = MaterialTheme.colorScheme.error,
                                )
                            }
                        }
                    },
                    colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
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

    confirming?.let { (device, label) ->
        AlertDialog(
            onDismissRequest = { confirming = null },
            title = { Text(t("settings.devices.remove.title", mapOf("device" to label))) },
            text = { Text(t("settings.devices.remove.body")) },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirming = null
                        remove(device)
                    },
                ) { Text(t("settings.devices.remove.confirm_named", mapOf("device" to label)), color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = {
                TextButton(onClick = { confirming = null }) { Text(t("common.cancel")) }
            },
        )
    }
}
