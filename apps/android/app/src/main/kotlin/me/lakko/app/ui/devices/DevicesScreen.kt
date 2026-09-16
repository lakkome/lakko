// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import android.Manifest
import android.content.pm.PackageManager
import android.util.Base64
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.TabRow
import androidx.compose.material3.Tab
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.ui.text.input.KeyboardCapitalization
import me.lakko.app.auth.AuthClient
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.ContactKind
import me.lakko.app.auth.isPlausibleContact
import me.lakko.app.devices.DeviceRemoval
import me.lakko.app.devices.PairingOrchestrator
import me.lakko.app.devices.WelcomeCourier
import me.lakko.app.webauthn.WebauthnClient
import me.lakko.app.webauthn.WebauthnHttpError
import me.lakko.app.webauthn.actionHashForRemoveDevice

/**
 * `DevicesScreen` — multi-device management surface
 * (Phase 2a3b4 Android).
 *
 * Mirror of:
 *
 *   * `apps/web/src/routes/Devices.tsx`
 *   * `apps/ios/Lakko/UI/Devices/DevicesScreen.swift`
 *
 * One screen, two flows picked via a [TabRow]:
 *
 *   1. **Pair this device** — joining side. Generates an
 *      ephemeral DeviceIdentity-derived KeyPackage + rendezvous
 *      pseudonym via [PairingOrchestrator.prepareJoinFlow],
 *      renders the pairing QR with [QrImageComposable],
 *      long-polls the DS mailbox for the Welcome, runs
 *      `accept` on receipt.
 *   2. **Add a device** — scanner side. Opens the camera via
 *      [QrScannerComposable], decodes the first QR, base64url-
 *      decodes the text payload, drives
 *      [PairingOrchestrator.runScanFlow].
 *
 * Both flows pull `accountIdBytes` + `accountIdB64` from the
 * injected [AuthRepository]; the screen renders an "auth
 * required" stub when the session isn't ready.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun DevicesScreen(
    authRepository: AuthRepository,
    onBack: () -> Unit,
    /** ADR-0050 — the chat side of a pairing (admit the new device to every
     *  chat) and of a removal (retire its leaf from every chat). `null` in
     *  previews / tests. */
    chatRepository: me.lakko.app.chat.ChatRepository? = null,
) {
    val authState by authRepository.state.collectAsStateWithLifecycle()
    val identitySecrets by authRepository.identitySecrets.collectAsStateWithLifecycle()

    val account = (authState as? AuthState.Session)?.account
    val secrets = identitySecrets

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Devices") },
                navigationIcon = {
                    TextButton(onClick = onBack) { Text("Back") }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text(
                "Link a second device to this account. The pairing is " +
                    "end-to-end-encrypted: the server sees only the rendezvous " +
                    "pseudonym and opaque MLS bytes.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            if (account == null || secrets == null) {
                Text(
                    "Sign in first to manage devices.",
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                )
                return@Column
            }

            var tab by remember { mutableStateOf(0) }
            TabRow(selectedTabIndex = tab) {
                Tab(
                    selected = tab == 0,
                    onClick = { tab = 0 },
                    text = { Text("Your devices") },
                )
                Tab(
                    selected = tab == 1,
                    onClick = { tab = 1 },
                    text = { Text("Pair this device") },
                )
                Tab(
                    selected = tab == 2,
                    onClick = { tab = 2 },
                    text = { Text("Add a device") },
                )
                Tab(
                    selected = tab == 3,
                    onClick = { tab = 3 },
                    text = { Text("Authenticator") },
                )
                Tab(
                    selected = tab == 4,
                    onClick = { tab = 4 },
                    text = { Text("Email or phone") },
                )
            }

            when (tab) {
                0 -> DevicesListTab(
                    accountIdBytes = secrets.accountId,
                    accountIdB64 = account.accountIdB64,
                    sessionToken = secrets.sessionToken,
                    chatRepository = chatRepository,
                )
                1 -> PairTab(
                    accountIdBytes = secrets.accountId,
                    accountIdB64 = account.accountIdB64,
                )
                2 -> ScanTab(
                    accountIdBytes = secrets.accountId,
                    accountIdB64 = account.accountIdB64,
                    accountIdentitySeed = secrets.accountIdentitySeed,
                    sessionToken = secrets.sessionToken,
                    chatRepository = chatRepository,
                )
                3 -> TotpEnrollTab(
                    sessionToken = secrets.sessionToken,
                )
                else -> ContactEnrollTab(
                    sessionToken = secrets.sessionToken,
                )
            }
        }
    }
}

// =============================================================================
// Pair tab — joining device side
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
    data class Error(val message: String) : PairUi
}

@Composable
private fun PairTab(
    accountIdBytes: ByteArray,
    accountIdB64: String,
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
            "Show this QR code to your already-linked device. It scans the " +
                "code, runs the MLS handshake, and your new device joins " +
                "the DeviceGroup.",
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
                            accountIdBytes = accountIdBytes,
                            accountIdB64 = accountIdB64,
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
                                            state = PairUi.Error(
                                                cause.message ?: "Failed to accept welcome",
                                            )
                                        }
                                    }
                                    WelcomeCourier.WelcomeOutcome.Timeout -> {
                                        state = PairUi.Error(
                                            "Timed out waiting for the other device. Try again.",
                                        )
                                    }
                                    WelcomeCourier.WelcomeOutcome.Cancelled -> {
                                        state = PairUi.Idle
                                    }
                                }
                            } catch (cause: Throwable) {
                                state = PairUi.Error(cause.message ?: "Pairing failed")
                            }
                        }
                    } catch (cause: Throwable) {
                        state = PairUi.Error(cause.message ?: "Could not prepare pairing")
                    }
                },
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text("Show pairing code")
            }

            PairUi.Preparing -> Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                CircularProgressIndicator()
                Text("Preparing pairing material…")
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
                    "Waiting for the other device to scan this code…",
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
                    Text("Cancel")
                }
            }

            PairUi.Joining -> Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                CircularProgressIndicator()
                Text("Joining the device group…")
            }

            PairUi.Done -> Text(
                "Linked. This device is now part of your DeviceGroup.",
                color = Color(0xFF1B5E20), // material green 900
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.SemiBold,
            )

            is PairUi.Error -> Column(
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    s.message,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                )
                OutlinedButton(onClick = { state = PairUi.Idle }) {
                    Text("Try again")
                }
            }
        }
    }
}

// =============================================================================
// Scan tab — scanner device side
// =============================================================================

private sealed interface ScanUi {
    data object Idle : ScanUi
    data object NeedsPermission : ScanUi
    data object Scanning : ScanUi

    /** ADR-0018 / lakko-security finding #1 — a decoded pairing QR
     *  parks here until the user explicitly confirms: the scan now
     *  transfers DeviceGroup membership AND the identity seed AND a
     *  session grant, so it must not run on a bare camera glance. */
    data class ConfirmTransfer(val scannedBytes: ByteArray) : ScanUi {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is ConfirmTransfer) return false
            return scannedBytes.contentEquals(other.scannedBytes)
        }

        override fun hashCode(): Int = scannedBytes.contentHashCode()
    }

    data object Processing : ScanUi
    data object Done : ScanUi
    data class Error(val message: String) : ScanUi
}

@Composable
private fun ScanTab(
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
    var scanHint: String? by remember { mutableStateOf(null) }

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
            "Aim your camera at the QR code your new device is showing. " +
                "The pairing completes within a couple of seconds.",
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
                Text("Open camera")
            }

            ScanUi.NeedsPermission -> Column(
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    "Lakko needs camera access to scan the pairing QR. " +
                        "Open Android settings to grant the permission.",
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                )
                OutlinedButton(
                    onClick = {
                        cameraPermissionLauncher.launch(Manifest.permission.CAMERA)
                    },
                ) {
                    Text("Try again")
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
                                scanHint =
                                    "That code isn't a Lakko pairing QR. Point " +
                                    "the camera at the QR on your other device."
                                return@QrScannerComposable
                            }
                            // ADR-0018 / lakko-security finding #1 —
                            // park for explicit confirmation instead of
                            // running the transfer on a bare decode.
                            scanHint = null
                            state = ScanUi.ConfirmTransfer(bytes)
                        },
                        onError = { msg ->
                            state = ScanUi.Error(msg)
                        },
                    )
                }
                scanHint?.let { hint ->
                    Text(
                        hint,
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
                    Text("Cancel")
                }
            }

            is ScanUi.ConfirmTransfer -> Column(
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(
                    "Link this device?",
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    "This gives the scanned device FULL access to your " +
                        "account: your messages, your contacts, and your " +
                        "secure identity. Only continue if the QR code is " +
                        "on YOUR OWN new device.",
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
                                        scannedBytes = s.scannedBytes,
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
                                    state = ScanUi.Error(
                                        cause.message ?: "Pairing failed",
                                    )
                                }
                            }
                        },
                    ) { Text("Link device") }
                    OutlinedButton(
                        onClick = { state = ScanUi.Idle },
                    ) { Text("Cancel") }
                }
            }

            ScanUi.Processing -> Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                CircularProgressIndicator()
                Text("Adding the device…")
            }

            ScanUi.Done -> Text(
                "Device added. The other device should land on its success " +
                    "screen any moment now.",
                color = Color(0xFF1B5E20),
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.SemiBold,
            )

            is ScanUi.Error -> Column(
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    s.message,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                )
                OutlinedButton(onClick = { state = ScanUi.Idle }) {
                    Text("Try again")
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
// Your devices (list + remove) tab
// =============================================================================

private sealed interface DevicesListUi {
    data object Loading : DevicesListUi
    data class Ready(val devices: List<DeviceRemoval.DeviceListItem>) : DevicesListUi
    data class Error(val message: String) : DevicesListUi
}

@Composable
private fun DevicesListTab(
    accountIdBytes: ByteArray,
    accountIdB64: String,
    sessionToken: String?,
    chatRepository: me.lakko.app.chat.ChatRepository?,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var state: DevicesListUi by remember { mutableStateOf(DevicesListUi.Loading) }
    var removingLeaf: UInt? by remember { mutableStateOf(null) }

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
            state = DevicesListUi.Error(cause.message ?: "Failed to load devices")
        }
    }

    LaunchedEffect(Unit) { refresh() }

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            "These are the devices currently linked to your account. " +
                "Removing a device revokes its access via an MLS commit " +
                "broadcast on the DeviceGroup journal — the revoked " +
                "device can no longer decrypt any subsequent traffic.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        when (val s = state) {
            DevicesListUi.Loading -> Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                CircularProgressIndicator()
                Text("Loading devices…")
            }

            is DevicesListUi.Ready -> {
                s.devices.forEach { device ->
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(vertical = 4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(modifier = Modifier.weight(1f)) {
                            Text(
                                "#${device.leafIndex} ${hexPrefix(device.signaturePublicKey, 8)}",
                                style = MaterialTheme.typography.bodyMedium,
                                fontWeight = FontWeight.Medium,
                            )
                            if (device.isSelf) {
                                Text(
                                    "this device",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                        if (device.isSelf) {
                            Text(
                                "—",
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        } else {
                            OutlinedButton(
                                onClick = {
                                    val leaf = device.leafIndex
                                    val signaturePublicKey = device.signaturePublicKey
                                    removingLeaf = leaf
                                    scope.launch {
                                        try {
                                            // G-13 (ADR-0014): gate the eviction on a
                                            // fresh passkey gesture cryptographically
                                            // bound to the removed leaf — but only if the
                                            // account actually has a passkey. OPAQUE-only
                                            // accounts (no passkey) fall through on
                                            // session-token auth alone: a 404 from the
                                            // ceremony start signals "no credentials". A
                                            // user-cancelled gesture or any other failure
                                            // aborts the removal (fail-closed).
                                            // G-13 (ADR-0015): the ceremony yields an
                                            // attestation bound to the removed leaf; carry it
                                            // with the Remove commit so peers verify it.
                                            var attestation: ByteArray? = null
                                            if (sessionToken != null) {
                                                WebauthnClient().use { wc ->
                                                    wc.setSessionToken(sessionToken)
                                                    try {
                                                        val actionHashB64 =
                                                            actionHashForRemoveDevice(
                                                                signaturePublicKey,
                                                            )
                                                        val result = wc.assertAction(
                                                            context = context,
                                                            actionHashB64 = actionHashB64,
                                                        )
                                                        attestation = base64UrlDecodeNoPad(
                                                            result.attestationB64,
                                                        )
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
                                            // ADR-0050 section 2.10 — out of every
                                            // chat and out of the directory.
                                            chatRepository?.requestRetireDevice(signaturePublicKey)
                                            removingLeaf = null
                                            refresh()
                                        } catch (cause: Throwable) {
                                            removingLeaf = null
                                            state = DevicesListUi.Error(
                                                cause.message ?: "Failed to remove device",
                                            )
                                        }
                                    }
                                },
                                enabled = removingLeaf == null,
                            ) {
                                Text(
                                    if (removingLeaf == device.leafIndex) "Removing…" else "Remove",
                                )
                            }
                        }
                    }
                }
                OutlinedButton(
                    onClick = { refresh() },
                    enabled = removingLeaf == null,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text("Refresh")
                }
            }

            is DevicesListUi.Error -> Column(
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    s.message,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                )
                OutlinedButton(onClick = { refresh() }) {
                    Text("Try again")
                }
            }
        }
    }
}

private fun hexPrefix(bytes: ByteArray, n: Int): String {
    val take = minOf(n, bytes.size)
    val sb = StringBuilder(take * 2)
    for (i in 0 until take) {
        val v = bytes[i].toInt() and 0xFF
        sb.append(HEX_CHARS[v ushr 4])
        sb.append(HEX_CHARS[v and 0x0F])
    }
    return sb.toString()
}

private const val HEX_CHARS: String = "0123456789abcdef"

// ---- Authenticator (TOTP) enrollment — ADR-0018 factor (c) -------

/** UI state machine for [TotpEnrollTab]. */
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
private fun TotpEnrollTab(sessionToken: String) {
    val scope = rememberCoroutineScope()
    var ui: TotpUi by remember { mutableStateOf(TotpUi.Idle) }

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(
            "Add an authenticator app (TOTP) as a recovery factor. " +
                "If you lose this device, a 6-digit code recovers " +
                "ACCESS to your account; your identity then restores " +
                "from the recovery phrase or another device.",
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
                                    err.message ?: "Enrollment failed (${err.status}).",
                                )
                            }
                        } catch (err: Throwable) {
                            TotpUi.Error(err.message ?: "Enrollment failed")
                        }
                    }
                },
            ) { Text("Enable authenticator") }

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
                                        error = "Wrong code — check the app and try again.",
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
                ) { Text(if (s.busy) "Confirming…" else "Confirm") }
            }

            TotpUi.Done -> Text(
                "Authenticator enabled. A 6-digit code can now recover " +
                    "access to this account.",
                color = Color(0xFF1B5E20),
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.SemiBold,
            )

            TotpUi.AlreadyEnabled -> Text(
                "An authenticator is already enabled on this account.",
                style = MaterialTheme.typography.bodyMedium,
            )

            TotpUi.Unavailable -> Text(
                "Authenticator enrollment is not available on this server.",
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

/** UI state machine for [ContactEnrollTab]. */
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
private fun ContactEnrollTab(sessionToken: String) {
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
            "Add a phone or email as a recovery factor. If you lose " +
                "this device, a one-time code sent to it recovers " +
                "ACCESS to your account; your identity then restores " +
                "from the recovery phrase or another device. The server " +
                "never stores the phone / email in plaintext — only a " +
                "keyed hash.",
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
                        autoCorrect = false,
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
                                        error = "That contact looks malformed. " +
                                            "Check it and retry.",
                                    )
                                    else -> ContactUi.Error(
                                        err.message
                                            ?: "Enrollment failed (${err.status}).",
                                    )
                                }
                            } catch (err: Throwable) {
                                ContactUi.Error(err.message ?: "Enrollment failed")
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
                                        error = "Wrong or expired code — check " +
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
                ) { Text(if (s.busy) "Confirming…" else "Confirm") }
            }

            ContactUi.Done -> Text(
                "Recovery contact enabled. A one-time code can now " +
                    "recover access to this account.",
                color = Color(0xFF1B5E20),
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.SemiBold,
            )

            ContactUi.AlreadyEnabled -> Text(
                "A recovery contact is already enabled on this account.",
                style = MaterialTheme.typography.bodyMedium,
            )

            ContactUi.Unavailable -> Text(
                "Email / phone recovery is not available on this server.",
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
