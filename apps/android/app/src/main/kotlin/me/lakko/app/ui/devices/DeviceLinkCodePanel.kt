// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import me.lakko.app.devices.AuthClientDeviceLinkApi
import me.lakko.app.devices.DEVICE_LINK_DECLINED_COPY
import me.lakko.app.devices.DEVICE_LINK_EXPIRED_COPY
import me.lakko.app.devices.DEVICE_LINK_NEW_LEAD
import me.lakko.app.devices.DEVICE_LINK_NEW_TITLE
import me.lakko.app.devices.DEVICE_LINK_RESTORING_COPY
import me.lakko.app.devices.DEVICE_LINK_SHOW_QR
import me.lakko.app.devices.DEVICE_LINK_WAITING_COPY
import me.lakko.app.devices.DEVICE_LINK_WRONG_CODE_COPY
import me.lakko.app.devices.DeviceCardCenter
import me.lakko.app.devices.DeviceLinkInvalidCode
import me.lakko.app.devices.FfiDeviceLinkInitiator
import me.lakko.app.devices.NewDeviceLink
import me.lakko.app.devices.NewDeviceLinkState
import me.lakko.app.devices.PairingOrchestrator
import me.lakko.app.devices.WelcomeCourier
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text

/**
 * ADR-0049 section 2 — the new device's "Confirm on another device" route
 * (`docs/ux/identity-recovery.md` section 4). Posts a request for the account
 * [loginHandle] names, waits for the other device's answer, takes the six
 * digits it shows, and only after the key confirmation verifies waits for the
 * Welcome and completes exactly as the QR route does.
 */
@Composable
internal fun DeviceLinkCodePanel(
    loginHandle: ByteArray,
    known: PairingOrchestrator.KnownAccount?,
    onRecovered: suspend (PairingOrchestrator.JoinRecoveryOutcome) -> Unit,
    onShowQr: () -> Unit,
    /** The city for this pairing, started when the screen opened (UX
     *  second-device.md section 2.1); settled when the code is sealed. */
    place: DeviceCardCenter.PairingPlace,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var attempt by remember { mutableIntStateOf(0) }
    var state: NewDeviceLinkState? by remember { mutableStateOf(null) }
    var error: UiText? by remember { mutableStateOf(null) }
    var code by remember { mutableStateOf("") }
    var link: NewDeviceLink? by remember { mutableStateOf(null) }
    var session: PairingOrchestrator.JoinSession? by remember { mutableStateOf(null) }

    DisposableEffect(Unit) {
        onDispose {
            session?.dispose()
            session = null
        }
    }

    // One request per attempt; "Start again" bumps the attempt.
    LaunchedEffect(attempt) {
        state = NewDeviceLinkState.WaitingForOtherDevice
        error = null
        code = ""
        try {
            // The payload is sealed after the code is typed; its card takes
            // the place that has arrived by then, so nothing is built with a
            // card here.
            val js = session ?: PairingOrchestrator.prepareJoinFlow(
                context = context,
                loginHandle = loginHandle,
                known = known,
                deviceCardEnvelope = null,
            ).also { session = it }
            val l = NewDeviceLink(
                api = AuthClientDeviceLinkApi(),
                initiatorFor = { id -> FfiDeviceLinkInitiator(id, loginHandle) },
                payloadForSeal = {
                    js.payloadWithCard(DeviceCardCenter.pairingCardEnvelope(place.settled()))
                },
            )
            link = l
            l.start(loginHandle)
            state = l.awaitAnswer()
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            error = deviceLinkFailureCopy(cause)
            state = null
        }
    }

    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(DEVICE_LINK_NEW_TITLE.text(), style = MaterialTheme.typography.titleMedium)
        Text(
            DEVICE_LINK_NEW_LEAD.text(),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        val s = state
        val fieldEnabled = s == NewDeviceLinkState.EnterCode
        OutlinedTextField(
            value = code,
            onValueChange = { typed -> code = typed.filter(Char::isDigit).take(DEVICE_LINK_CODE_DIGITS) },
            label = { Text(t("device_link.code.field_a11y")) },
            singleLine = true,
            enabled = fieldEnabled,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
            modifier = Modifier.fillMaxWidth(),
        )
        when (s) {
            NewDeviceLinkState.WaitingForOtherDevice -> Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                CircularProgressIndicator()
                Text(DEVICE_LINK_WAITING_COPY.text(), style = MaterialTheme.typography.bodySmall)
            }
            NewDeviceLinkState.Checking -> CircularProgressIndicator()
            NewDeviceLinkState.WrongCode -> Text(DEVICE_LINK_WRONG_CODE_COPY.text(), color = MaterialTheme.colorScheme.error)
            NewDeviceLinkState.Declined -> Text(DEVICE_LINK_DECLINED_COPY.text(), color = MaterialTheme.colorScheme.error)
            NewDeviceLinkState.Expired -> Text(DEVICE_LINK_EXPIRED_COPY.text(), color = MaterialTheme.colorScheme.error)
            NewDeviceLinkState.Confirmed -> Text(DEVICE_LINK_RESTORING_COPY.text())
            NewDeviceLinkState.EnterCode, null -> Unit
        }
        error?.let { Text(it.text(), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            when (s) {
                NewDeviceLinkState.EnterCode -> Button(
                    enabled = code.length == DEVICE_LINK_CODE_DIGITS,
                    onClick = {
                        val l = link ?: return@Button
                        val js = session ?: return@Button
                        state = NewDeviceLinkState.Checking
                        scope.launch {
                            try {
                                val next = l.submitCode(code)
                                state = next
                                if (next == NewDeviceLinkState.Confirmed) {
                                    // Only now is the Welcome trusted: the old
                                    // device proved it holds the code.
                                    when (val outcome = js.awaitWelcome()) {
                                        is WelcomeCourier.WelcomeOutcome.Received -> {
                                            val recovery = js.accept(
                                                welcomeBytes = outcome.welcomeBytes,
                                                welcomeCursor = outcome.welcomeCursor,
                                                recovery = true,
                                            )
                                            if (recovery == null) {
                                                error = UiText("device_link.error.no_identity")
                                            } else {
                                                onRecovered(recovery)
                                            }
                                        }
                                        WelcomeCourier.WelcomeOutcome.Timeout -> state = NewDeviceLinkState.Expired
                                        WelcomeCourier.WelcomeOutcome.Cancelled -> Unit
                                    }
                                }
                            } catch (cause: kotlinx.coroutines.CancellationException) {
                                throw cause
                            } catch (_: DeviceLinkInvalidCode) {
                                state = NewDeviceLinkState.EnterCode
                            } catch (cause: Throwable) {
                                me.lakko.app.rethrowIfCancelled(cause)
                                error = deviceLinkFailureCopy(cause)
                                state = NewDeviceLinkState.EnterCode
                            }
                        }
                    },
                ) { Text(t("common.continue")) }
                NewDeviceLinkState.WrongCode, NewDeviceLinkState.Declined, NewDeviceLinkState.Expired, null ->
                    Button(onClick = { attempt += 1 }) { Text(t("device_link.code.start_again")) }
                else -> Unit
            }
            OutlinedButton(onClick = onShowQr) { Text(DEVICE_LINK_SHOW_QR.text()) }
        }
    }
}

/** Six digits (spec section 4). */
private const val DEVICE_LINK_CODE_DIGITS: Int = 6

/** One sentence for a failure outside the named states (a limit, the
 *  network, login by name switched off), rendered from its kind. */
private fun deviceLinkFailureCopy(cause: Throwable): UiText {
    val http = cause as? me.lakko.app.auth.AuthHttpError
    if (http != null) {
        me.lakko.app.auth.authLimitText(http)?.let { return it }
        if (http.status == HTTP_UNAVAILABLE) return UiText("device_link.error.ask_unavailable")
    }
    if (cause is me.lakko.app.auth.AuthNetworkError) return UiText("device_link.code.unreachable")
    if (cause is me.lakko.app.devices.JoinWrongAccount) return UiText("device_link.error.wrong_account")
    return UiText("device_link.error.generic")
}

private const val HTTP_UNAVAILABLE: Int = 503
