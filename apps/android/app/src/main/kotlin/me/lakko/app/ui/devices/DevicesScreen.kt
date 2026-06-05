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
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
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
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
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
            }

            when (tab) {
                0 -> DevicesListTab(
                    accountIdBytes = secrets.accountId,
                    accountIdB64 = account.accountIdB64,
                    sessionToken = secrets.sessionToken,
                )
                1 -> PairTab(
                    accountIdBytes = secrets.accountId,
                    accountIdB64 = account.accountIdB64,
                )
                else -> ScanTab(
                    accountIdBytes = secrets.accountId,
                    accountIdB64 = account.accountIdB64,
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
    data object Processing : ScanUi
    data object Done : ScanUi
    data class Error(val message: String) : ScanUi
}

@Composable
private fun ScanTab(
    accountIdBytes: ByteArray,
    accountIdB64: String,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    val initiallyGranted = ContextCompat.checkSelfPermission(
        context,
        Manifest.permission.CAMERA,
    ) == PackageManager.PERMISSION_GRANTED
    var state: ScanUi by remember { mutableStateOf(ScanUi.Idle) }

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
                            // Once we have a detected payload, only process if
                            // we're still in Scanning state — `hasDetected`
                            // already guards re-entry from the analyser side,
                            // but the Compose-state check is the second line.
                            if (state !is ScanUi.Scanning) return@QrScannerComposable
                            val bytes = base64UrlDecodeNoPad(raw)
                            if (bytes == null) {
                                // Not our pairing payload — keep the scanner
                                // running by NOT flipping state. The
                                // `hasDetected` AtomicBoolean inside the
                                // composable already latched though, so
                                // technically we'd need a re-mount to keep
                                // scanning. For now we surface a soft error
                                // and let the user retry.
                                state = ScanUi.Error(
                                    "Scanned code wasn't a Lakko pairing QR.",
                                )
                                return@QrScannerComposable
                            }
                            state = ScanUi.Processing
                            scope.launch {
                                try {
                                    PairingOrchestrator.runScanFlow(
                                        context = context,
                                        scannedBytes = bytes,
                                        accountIdBytes = accountIdBytes,
                                        accountIdB64 = accountIdB64,
                                    )
                                    state = ScanUi.Done
                                } catch (cause: Throwable) {
                                    state = ScanUi.Error(
                                        cause.message ?: "Pairing failed",
                                    )
                                }
                            }
                        },
                        onError = { msg ->
                            state = ScanUi.Error(msg)
                        },
                    )
                }
                OutlinedButton(
                    onClick = { state = ScanUi.Idle },
                ) {
                    Text("Cancel")
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
                                            if (sessionToken != null) {
                                                WebauthnClient().use { wc ->
                                                    wc.setSessionToken(sessionToken)
                                                    try {
                                                        val actionHashB64 =
                                                            actionHashForRemoveDevice(
                                                                signaturePublicKey,
                                                            )
                                                        wc.assertAction(
                                                            context = context,
                                                            actionHashB64 = actionHashB64,
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
                                            )
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
