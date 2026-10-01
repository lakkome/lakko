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
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text

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
 *     drives `AuthRepository.completeFromDeviceRestore`. This device has
 *     no account id: the code carries the login handle of the typed
 *     username and the account id arrives with the grant redeem
 *     (ADR-0056).
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
    /** `loginHandle(username)` of the account's name (ADR-0056). */
    loginHandle: ByteArray,
    /** The account when this device already knows it (path (a)); `null`
     *  for the join by name of path (b). */
    known: PairingOrchestrator.KnownAccount?,
    onRecovered: suspend (PairingOrchestrator.JoinRecoveryOutcome) -> Unit,
    modifier: Modifier = Modifier,
) {
    // ADR-0049 section 2 (amended 2026-09-28) — the code route is the
    // default; the QR route stays one tap away ("Show a QR code instead").
    var showQr by remember { mutableStateOf(false) }
    if (!showQr) {
        DeviceLinkCodePanel(
            loginHandle = loginHandle,
            known = known,
            onRecovered = onRecovered,
            onShowQr = { showQr = true },
            modifier = modifier,
        )
        return
    }
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
            t("device_link.qr.intro"),
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
                            loginHandle = loginHandle,
                            known = known,
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
                                                UiText("device_link.error.no_identity"),
                                            )
                                        } else {
                                            onRecovered(recovery)
                                            ui = RestoreViaDeviceUi.Done
                                        }
                                    }
                                    WelcomeCourier.WelcomeOutcome.Timeout -> {
                                        ui = RestoreViaDeviceUi.Error(
                                            UiText("device_link.error.timeout"),
                                        )
                                    }
                                    WelcomeCourier.WelcomeOutcome.Cancelled -> {
                                        ui = RestoreViaDeviceUi.Idle
                                    }
                                }
                            } catch (cause: Throwable) {
                                ui = RestoreViaDeviceUi.Error(deviceRestoreErrorText(cause))
                            } finally {
                                session?.dispose()
                                session = null
                            }
                        }
                    } catch (cause: Throwable) {
                        ui = RestoreViaDeviceUi.Error(deviceRestoreErrorText(cause))
                    }
                },
            ) { Text(t("device_link.show_code")) }

            RestoreViaDeviceUi.Preparing -> CircularProgressIndicator()

            is RestoreViaDeviceUi.Waiting -> {
                QrImageComposable(payload = s.qrPayload)
                Text(
                    t("device_link.qr.waiting"),
                    style = MaterialTheme.typography.bodySmall,
                )
            }

            RestoreViaDeviceUi.Joining -> {
                CircularProgressIndicator()
                Text(
                    t("device_link.transferring"),
                    style = MaterialTheme.typography.bodySmall,
                )
            }

            is RestoreViaDeviceUi.Error -> {
                Text(
                    s.message.text(),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
                Button(onClick = { ui = RestoreViaDeviceUi.Idle }) {
                    Text(t("common.try_again"))
                }
            }

            RestoreViaDeviceUi.Done -> Text(
                t("device_link.restored"),
                style = MaterialTheme.typography.bodySmall,
            )
        }
    }
}

/**
 * The sentence a failed restore shows (ADR-0061 section 6: from its kind,
 * never from the exception's text). Mirrors web `deviceRestoreErrorMessage`.
 */
internal fun deviceRestoreErrorText(cause: Throwable): UiText = when (cause) {
    is me.lakko.app.auth.AuthNetworkError -> UiText("common.error.offline")
    is me.lakko.app.auth.AuthHttpError ->
        me.lakko.app.auth.authLimitText(cause) ?: UiText("device_link.error.generic")
    is me.lakko.app.auth.PhraseMismatch -> UiText("device_link.error.identity_mismatch")
    is me.lakko.app.auth.IdentityVerificationUnavailable -> UiText(
        if (cause.missing) "restore_identity.error.not_published" else "restore_identity.error.unreachable",
    )
    is me.lakko.app.auth.TypedNameOtherAccount -> UiText("device_link.error.name_other_account")
    is me.lakko.app.auth.TypedNameCheckUnavailable -> UiText("common.error.offline")
    is me.lakko.app.auth.AuthClientArgError -> cause.text ?: UiText("device_link.error.generic")
    else -> UiText("device_link.error.generic")
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
    data class Error(val message: UiText) : RestoreViaDeviceUi
    data object Done : RestoreViaDeviceUi
}
