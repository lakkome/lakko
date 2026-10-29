// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import me.lakko.app.ui.components.LakkoDialogTone
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.UniffiLoginHandleSource
import me.lakko.app.devices.AuthClientDeviceLinkApi
import me.lakko.app.devices.DEVICE_LINK_ADDING_COPY
import me.lakko.app.devices.DEVICE_LINK_CONSENT_LEAD
import me.lakko.app.devices.DEVICE_LINK_CONSENT_QUESTION
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
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import uniffi.lakko_ffi_uniffi.DecodedPairingQr
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction

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

    // The new device was linked but this one had no keys to hand over.
    var keysNotSent by remember { mutableStateOf(false) }

    // A guest account has no username, so no handle a request could name.
    val ownHandle = remember(account.username) { ownPairingHandle(account.username, UniffiLoginHandleSource) } ?: return

    val link = remember(account.accountIdB64) {
        OldDeviceLink<DecodedPairingQr>(
            api = AuthClientDeviceLinkApi(
                sessionToken = { authRepository.identitySecrets.value?.sessionToken },
                // UX 17.4 - the answer and the result carry the fresh-auth
                // proof; without a live one nothing is sent.
                freshGrant = { authRepository.freshGrant() },
                onProofRefused = { authRepository.dropFreshProof() },
            ),
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
                // The account's keys travel as the entropy of the recovery
                // phrase this device holds; with no stored phrase no recovery
                // row is published and the person is told below.
                // The single-use grant is minted here, before the pairing
                // starts, with the fresh-auth proof: a proof that ran out
                // since the answer stops with nothing sent and the block is
                // shown again ([OldDeviceLinkState.NeedsProof]).
                val recovery = withContext(Dispatchers.IO) {
                    PairingOrchestrator.scanRecoveryFromDevicePhrase(
                        context = context,
                        accountIdB64 = account.accountIdB64,
                        sessionToken = current.sessionToken,
                        freshGrant = authRepository.freshGrant(),
                    )
                }
                val scanned = PairingOrchestrator.runScanFlow(
                    context = context,
                    decoded = decoded,
                    accountIdBytes = current.accountId,
                    accountIdB64 = account.accountIdB64,
                    recovery = recovery,
                )
                if (!scanned.keysSent) keysNotSent = true
                // ADR-0050 section 2.8 — admit the new device to every chat.
                chatRepository?.requestAdmitDevice(scanned.newDeviceKey)
            },
            // UX second-device.md section 2.1 - "Pixel 7a" added.
            addedCopy = { decoded ->
                me.lakko.app.devices.DeviceCards.addedCopy(
                    decoded.deviceCard?.let(me.lakko.app.devices.DeviceCardCenter::fromFfi),
                )
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

    if (keysNotSent) {
        LakkoDialog(
            onDismissRequest = { keysNotSent = false },
            title = DEVICE_LINK_OLD_TITLE.text(),
            body = t("device_link.keys_not_sent"),
            confirm = LakkoDialogAction(t("common.close")) { keysNotSent = false },
            cancel = null,
        )
    }

    val modal by link.states.collectAsStateWithLifecycle()
    // UX 17.4 - the proof block of "Is this you?": one per request modal.
    val proof = me.lakko.app.ui.auth.rememberProofController(
        authRepository,
        me.lakko.app.ui.auth.ProofSurface.AddDeviceRequest,
        key = account.accountIdB64,
    )
    var working by remember { mutableStateOf(false) }
    // The typed password does not outlive the modal.
    LaunchedEffect(modal) {
        if (modal !is OldDeviceLinkState.AskConsent && modal !is OldDeviceLinkState.NeedsProof) proof.clear()
    }
    when (val m = modal) {
        OldDeviceLinkState.Idle -> Unit
        // Step 1 (finding 6): ask first; nothing is sent for the request until
        // the person both said yes and proved it (UX 17.4). Dismissing sends
        // nothing. `It is not me` needs no proof and works in every state.
        OldDeviceLinkState.AskConsent, OldDeviceLinkState.NeedsProof -> {
            val midFlow = m is OldDeviceLinkState.NeedsProof
            val proofView = proof.view()
            val yes: () -> Unit = yes@{
                if (working || !proof.view().mainEnabled) return@yes
                working = true
                scope.launch {
                    try {
                        // The proof is checked first; only when it passes does
                        // this device choose the code and answer the request.
                        // A wrong password or a cancelled gesture leaves the
                        // request pending, unanswered.
                        if (proof.ensure(context)) {
                            if (midFlow) link.resume() else link.confirm()
                        }
                    } catch (cause: kotlinx.coroutines.CancellationException) {
                        throw cause
                    } catch (cause: Throwable) {
                        android.util.Log.w("DeviceLinkRequestHost", "answering the request failed: ${cause.javaClass.simpleName}")
                    } finally {
                        working = false
                        // A `403 fresh-auth-required` mid-flow dropped the
                        // proof: the block appears, the request is kept.
                        proof.refresh()
                    }
                }
            }
            LakkoDialog(
                onDismissRequest = { if (!midFlow && !working) link.dismiss() },
                title = DEVICE_LINK_OLD_TITLE.text(),
                confirm = LakkoDialogAction(
                    label = if (working || proofView.working) t("common.checking") else DEVICE_LINK_YES_ITS_ME.text(),
                    enabled = proofView.mainEnabled && !working,
                    onClick = yes,
                ),
                cancel = LakkoDialogAction(
                    label = DEVICE_LINK_NOT_ME.text(),
                    tone = LakkoDialogTone.Ghost,
                    onClick = { scope.launch { link.decline() } },
                ),
            ) {
                Text(DEVICE_LINK_CONSENT_LEAD.text())
                Text(DEVICE_LINK_CONSENT_QUESTION.text())
                me.lakko.app.ui.auth.ProofBlock(
                    controller = proof,
                    onSubmit = yes,
                    // With a passkey the block opens on the notice and
                    // `Yes, it's me` opens the sheet; this button is the
                    // password route's way back to it.
                    onPasskey = { proof.setMode(me.lakko.app.ui.auth.ProofMode.PasskeyNotice) },
                    onEngaged = { link.noteEngaged() },
                )
            }
        }
        is OldDeviceLinkState.ShowCode -> LakkoDialog(
            onDismissRequest = {},
            title = DEVICE_LINK_OLD_TITLE.text(),
            confirm = null,
            cancel = LakkoDialogAction(
                label = DEVICE_LINK_NOT_ME.text(),
                tone = LakkoDialogTone.Ghost,
                onClick = { scope.launch { link.decline() } },
            ),
        ) {
            Text(DEVICE_LINK_OLD_LEAD.text())
            val codeA11y = t("device_link.request.code_a11y")
            Text(
                m.formattedCode,
                fontSize = 32.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.semantics { contentDescription = "$codeA11y ${m.formattedCode}" },
            )
            Text(
                DEVICE_LINK_OLD_WARNING.text(),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        OldDeviceLinkState.Adding -> LakkoDialog(
            onDismissRequest = {},
            title = DEVICE_LINK_OLD_TITLE.text(),
            confirm = null,
            cancel = null,
        ) { Text(DEVICE_LINK_ADDING_COPY.text()) }
        is OldDeviceLinkState.Notice -> LakkoDialog(
            onDismissRequest = { link.dismiss() },
            title = DEVICE_LINK_OLD_TITLE.text(),
            confirm = LakkoDialogAction(label = t("common.close"), onClick = { link.dismiss() }),
            cancel = null,
        ) { Text(m.message.text()) }
    }
}

/** How often a foregrounded signed-in device reads the open request. Chosen,
 *  not measured: a person walking between two devices sees the code within a
 *  few seconds, and the route is one small authenticated GET. */
private const val DEVICE_LINK_POLL_MS: Long = 4_000L
