// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import me.lakko.app.devices.PairingOrchestrator
import me.lakko.app.devices.WelcomeCourier

/**
 * Shared "restore from your other device" panel (ADR-0018 factor (a))
 * — the QR-join flow with the recovery payload opted in. Used by both
 * completion paths:
 *
 *   * path (a): [me.lakko.app.ui.auth.RestoreIdentityPrompt] — the
 *     user password-authenticated on this fresh device but has no
 *     cached identity seed; `onRecovered` drives
 *     `AuthRepository.completeFromPairing`.
 *   * path (b): the Recover screen's no-password mode; `onRecovered`
 *     drives `AuthRepository.completeFromDeviceRestore`.
 *
 * Mirror of `apps/web/src/devices/RestoreViaDevicePanel.tsx`. The
 * panel renders the pairing QR; the user scans it with their
 * already-linked device, which adds this device to the DeviceGroup
 * AND (best-effort) publishes the sealed `(grant ‖ identity seed)`
 * recovery row. A scanner that did not publish the row (older build,
 * or its best-effort half failed) completes the pairing but yields no
 * recovery payload — surfaced as a retryable error suggesting the
 * phrase instead.
 *
 * `onRecovered` may throw (`PhraseMismatch` /
 * `IdentityVerificationUnavailable` from the repository's fail-closed
 * verify) — surfaced inline.
 */
@Composable
public fun RestoreViaDevicePanel(
    accountIdBytes: ByteArray,
    accountIdB64: String,
    onRecovered: suspend (PairingOrchestrator.JoinRecoveryOutcome) -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var ui: RestoreViaDeviceUi by remember { mutableStateOf(RestoreViaDeviceUi.Idle) }
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
        modifier = modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(
            "Show this QR code to a device that still has your account. " +
                "It links this device and securely transfers your identity " +
                "— no phrase entry needed.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        when (val s = ui) {
            RestoreViaDeviceUi.Idle -> Button(
                onClick = {
                    ui = RestoreViaDeviceUi.Preparing
                    try {
                        val js = PairingOrchestrator.prepareJoinFlow(
                            context = context,
                            accountIdBytes = accountIdBytes,
                            accountIdB64 = accountIdB64,
                        )
                        session = js
                        ui = RestoreViaDeviceUi.Waiting(js.qrPayload)
                        pollJob = scope.launch {
                            try {
                                when (val outcome = js.awaitWelcome()) {
                                    is WelcomeCourier.WelcomeOutcome.Received -> {
                                        ui = RestoreViaDeviceUi.Joining
                                        val recovery = js.accept(
                                            welcomeBytes = outcome.welcomeBytes,
                                            welcomeCursor = outcome.welcomeCursor,
                                            recovery = true,
                                        )
                                        if (recovery == null) {
                                            ui = RestoreViaDeviceUi.Error(
                                                "The other device linked this one but " +
                                                    "didn't send recovery data. Update " +
                                                    "it, or restore with your recovery " +
                                                    "phrase instead.",
                                            )
                                        } else {
                                            onRecovered(recovery)
                                            ui = RestoreViaDeviceUi.Done
                                        }
                                    }
                                    WelcomeCourier.WelcomeOutcome.Timeout -> {
                                        ui = RestoreViaDeviceUi.Error(
                                            "Timed out waiting for the other device. " +
                                                "Try again.",
                                        )
                                    }
                                    WelcomeCourier.WelcomeOutcome.Cancelled -> {
                                        ui = RestoreViaDeviceUi.Idle
                                    }
                                }
                            } catch (cause: Throwable) {
                                ui = RestoreViaDeviceUi.Error(
                                    cause.message ?: "Restore failed",
                                )
                            } finally {
                                session?.dispose()
                                session = null
                            }
                        }
                    } catch (cause: Throwable) {
                        ui = RestoreViaDeviceUi.Error(
                            cause.message ?: "Could not prepare the QR code",
                        )
                    }
                },
            ) { Text("Show QR code") }

            RestoreViaDeviceUi.Preparing -> CircularProgressIndicator()

            is RestoreViaDeviceUi.Waiting -> {
                QrImageComposable(payload = s.qrPayload)
                Text(
                    "Waiting for your other device to scan…",
                    style = MaterialTheme.typography.bodySmall,
                )
            }

            RestoreViaDeviceUi.Joining -> {
                CircularProgressIndicator()
                Text(
                    "Linking and restoring your identity…",
                    style = MaterialTheme.typography.bodySmall,
                )
            }

            is RestoreViaDeviceUi.Error -> {
                Text(
                    s.message,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
                Button(onClick = { ui = RestoreViaDeviceUi.Idle }) {
                    Text("Try again")
                }
            }

            RestoreViaDeviceUi.Done -> Text(
                "Identity restored.",
                style = MaterialTheme.typography.bodySmall,
            )
        }
    }
}

/** Internal UI state machine for [RestoreViaDevicePanel]. */
private sealed interface RestoreViaDeviceUi {
    data object Idle : RestoreViaDeviceUi
    data object Preparing : RestoreViaDeviceUi
    data class Waiting(val qrPayload: ByteArray) : RestoreViaDeviceUi {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Waiting) return false
            return qrPayload.contentEquals(other.qrPayload)
        }

        override fun hashCode(): Int = qrPayload.contentHashCode()
    }
    data object Joining : RestoreViaDeviceUi
    data class Error(val message: String) : RestoreViaDeviceUi
    data object Done : RestoreViaDeviceUi
}
