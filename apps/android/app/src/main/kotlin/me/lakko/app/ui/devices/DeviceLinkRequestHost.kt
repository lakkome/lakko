// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.UniffiLoginHandleSource
import me.lakko.app.devices.AuthClientDeviceLinkApi
import me.lakko.app.devices.DEVICE_LINK_ADDING_COPY
import me.lakko.app.devices.DEVICE_LINK_CONSENT_LEAD
import me.lakko.app.devices.DEVICE_LINK_YES_ITS_ME
import me.lakko.app.devices.DEVICE_LINK_NOT_ME
import me.lakko.app.devices.DEVICE_LINK_OLD_LEAD
import me.lakko.app.devices.DEVICE_LINK_OLD_TITLE
import me.lakko.app.devices.DEVICE_LINK_OLD_WARNING
import me.lakko.app.devices.FfiDeviceLinkResponder
import me.lakko.app.devices.OldDeviceLink
import me.lakko.app.devices.OldDeviceLinkState
import me.lakko.app.devices.PairingCodec
import me.lakko.app.devices.PairingOrchestrator
import me.lakko.app.devices.PairingScanDecision
import me.lakko.app.devices.decodeForOwnAccount
import me.lakko.app.devices.deviceLinkAnswerLimit
import me.lakko.app.devices.ownPairingHandle
import uniffi.lakko_ffi_uniffi.DecodedPairingQr

/**
 * ADR-0049 section 2 — the OLD device's side of "confirm on another device"
 * (`docs/ux/identity-recovery.md` section 4): while the app is in the
 * foreground (RESUMED) it polls the account's open sign-in request and shows
 * the modal. The modal first asks "Is this you?" and sends nothing until
 * "Yes, it's me" (second security pass, finding 6); the code appears only
 * after this device's answer landed; the
 * right code runs the unchanged add-device pairing. No notification is posted
 * for a request that appears while the app is in the background: the request
 * is shown the next time the app is opened within its 10 minutes.
 */
@Composable
public fun DeviceLinkRequestHost(
    authRepository: AuthRepository,
    chatRepository: me.lakko.app.chat.ChatRepository?,
) {
    val authState by authRepository.state.collectAsStateWithLifecycle()
    val secrets by authRepository.identitySecrets.collectAsStateWithLifecycle()
    val account = (authState as? AuthState.Session)?.account ?: return
    val s = secrets ?: return
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val scope = rememberCoroutineScope()

    // A guest account has no username, so no handle a request could name.
    val ownHandle = remember(account.username) { ownPairingHandle(account.username, UniffiLoginHandleSource) } ?: return

    val link = remember(account.accountIdB64) {
        OldDeviceLink<DecodedPairingQr>(
            api = AuthClientDeviceLinkApi(sessionToken = { authRepository.identitySecrets.value?.sessionToken }),
            responderFor = { id -> FfiDeviceLinkResponder(id, ownHandle) },
            formatCode = { raw -> uniffi.lakko_ffi_uniffi.deviceLinkFormatCode(raw) },
            openPayload = { payload ->
                when (
                    val decision = decodeForOwnAccount(
                        scanned = payload,
                        ownUsername = account.username,
                        source = UniffiLoginHandleSource,
                        decode = PairingCodec::parsePairingQrPayload,
                    )
                ) {
                    is PairingScanDecision.Accepted -> decision.decoded
                    is PairingScanDecision.Refused -> throw IllegalStateException(decision.text.render())
                }
            },
            pair = { decoded ->
                val current = authRepository.identitySecrets.value ?: throw IllegalStateException("no session")
                val scanned = PairingOrchestrator.runScanFlow(
                    context = context,
                    decoded = decoded,
                    accountIdBytes = current.accountId,
                    accountIdB64 = account.accountIdB64,
                    recovery = PairingOrchestrator.ScanRecovery(
                        accountIdentitySeed = current.accountIdentitySeed.copyOf(),
                        deviceAuthorizeStart = {
                            me.lakko.app.auth.AuthClient().use { client ->
                                client.deviceAuthorizeStart(current.sessionToken)
                            }
                        },
                    ),
                )
                // ADR-0050 section 2.8 — admit the new device to every chat.
                chatRepository?.requestAdmitDevice(scanned.newDeviceKey)
            },
            limit = deviceLinkAnswerLimit(context, account.accountIdB64),
        )
    }

    LaunchedEffect(link, s.sessionToken) {
        lifecycleOwner.lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            while (true) {
                try {
                    link.poll()
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (cause: Throwable) {
                    android.util.Log.w("DeviceLinkRequestHost", "sign-in request poll failed: ${cause.javaClass.simpleName}")
                }
                delay(DEVICE_LINK_POLL_MS)
            }
        }
    }

    val modal by link.states.collectAsStateWithLifecycle()
    when (val m = modal) {
        OldDeviceLinkState.Idle -> Unit
        // Step 1 (finding 6): ask first; nothing is sent for the request until
        // "Yes, it's me". Dismissing sends nothing.
        OldDeviceLinkState.AskConsent -> AlertDialog(
            onDismissRequest = { link.dismiss() },
            title = { Text(DEVICE_LINK_OLD_TITLE) },
            text = { Text(DEVICE_LINK_CONSENT_LEAD) },
            confirmButton = {
                TextButton(onClick = { scope.launch { runCatching { link.confirm() } } }) { Text(DEVICE_LINK_YES_ITS_ME) }
            },
            dismissButton = {
                TextButton(onClick = { scope.launch { link.decline() } }) { Text(DEVICE_LINK_NOT_ME) }
            },
        )
        is OldDeviceLinkState.ShowCode -> AlertDialog(
            onDismissRequest = {},
            title = { Text(DEVICE_LINK_OLD_TITLE) },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    Text(DEVICE_LINK_OLD_LEAD)
                    Text(m.formattedCode, fontSize = 32.sp, fontWeight = FontWeight.SemiBold)
                    Text(
                        DEVICE_LINK_OLD_WARNING,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            },
            confirmButton = {
                TextButton(onClick = { scope.launch { link.decline() } }) { Text(DEVICE_LINK_NOT_ME) }
            },
        )
        OldDeviceLinkState.Adding -> AlertDialog(
            onDismissRequest = {},
            title = { Text(DEVICE_LINK_OLD_TITLE) },
            text = { Text(DEVICE_LINK_ADDING_COPY) },
            confirmButton = {},
        )
        is OldDeviceLinkState.Notice -> AlertDialog(
            onDismissRequest = { link.dismiss() },
            title = { Text(DEVICE_LINK_OLD_TITLE) },
            text = { Text(m.message) },
            confirmButton = {
                TextButton(onClick = { link.dismiss() }) { Text("Close") }
            },
        )
    }
}

/** How often a foregrounded signed-in device reads the open request. Chosen,
 *  not measured: a person walking between two devices sees the code within a
 *  few seconds, and the route is one small authenticated GET. */
private const val DEVICE_LINK_POLL_MS: Long = 4_000L
