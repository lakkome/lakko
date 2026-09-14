// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.calls.CallSession
import me.lakko.app.calls.OneToOneCall
import me.lakko.app.calls.OneToOneCallHost
import me.lakko.app.calls.OneToOneCallPresentation
import me.lakko.app.calls.oneToOneCallPresentation

/**
 * The app-wide presentation of the 1x1 call, mounted once above every session
 * screen by `MainActivity` (chat list, contacts, home, any chat).
 *
 *  - An incoming call that has not been answered is a banner at the top of
 *    whatever screen is showing: Accept, Accept with video (only when the offer
 *    carries video, since an answer cannot add a media section the offer does
 *    not have), Decline.
 *  - Everything else — the outgoing prejoin, a call being set up or live, the
 *    ended state — is the full-screen in-call screen, as a dialog so it covers
 *    the screen's own dialogs too. Its only exit is the in-screen End button.
 *
 * The call lives in [OneToOneCallHost]; this composable only renders it, so
 * navigating between screens never ends a call.
 *
 * @param nameForGroup the other person's resolved name for a 1x1 chat, or `null`.
 */
@Composable
public fun BoxScope.OneToOneCallOverlay(
    host: OneToOneCallHost<CallSession>,
    nameForGroup: (groupIdHex: String) -> String?,
) {
    val permissions = rememberCallPermissionGate()
    val active by host.active.collectAsStateWithLifecycle()
    val call = active ?: return
    val state by call.session.state.collectAsStateWithLifecycle()
    val name = nameForGroup(call.groupIdHex)
    when (oneToOneCallPresentation(state, call.accepted)) {
        OneToOneCallPresentation.RINGING_BANNER -> IncomingCallBanner(
            call = call,
            callerName = name,
            onAccept = { permissions.request(video = false) { host.accept(withVideo = false) } },
            onAcceptWithVideo = { permissions.request(video = true) { v -> host.accept(withVideo = v) } },
            onDecline = { host.decline() },
            modifier = Modifier.align(Alignment.TopCenter),
        )
        OneToOneCallPresentation.IN_CALL_SCREEN -> Dialog(
            onDismissRequest = { /* exit only via the End button */ },
            properties = DialogProperties(
                usePlatformDefaultWidth = false,
                dismissOnBackPress = false,
                dismissOnClickOutside = false,
            ),
        ) {
            Box(modifier = Modifier.fillMaxSize()) {
                CallScreen(
                    onExit = { host.clearFinished() },
                    session = call.session,
                    // ADR-0048 UX §5 — the other person's resolved name on their tile.
                    remoteName = name,
                    // G3-2-prejoin-join — Join dials the prepared session with the
                    // chosen mic / camera; Cancel abandons it. The call button
                    // already asked for permissions, but the camera can be switched
                    // on in the prejoin and a grant can be revoked in between.
                    onPrejoinJoin = { mic, cam ->
                        permissions.request(video = cam) { v -> host.dial(audio = mic, video = v) }
                    },
                    onPrejoinCancel = { host.cancelPrepared() },
                )
            }
        }
    }
}

@Composable
private fun IncomingCallBanner(
    call: OneToOneCall<CallSession>,
    callerName: String?,
    onAccept: () -> Unit,
    onAcceptWithVideo: () -> Unit,
    onDecline: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        modifier = modifier
            .fillMaxWidth()
            .statusBarsPadding()
            .padding(horizontal = 12.dp, vertical = 8.dp)
            .semantics { liveRegion = LiveRegionMode.Assertive },
        shape = RoundedCornerShape(16.dp),
        color = MaterialTheme.colorScheme.primaryContainer,
        contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
        tonalElevation = 6.dp,
        shadowElevation = 6.dp,
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                text = if (call.remoteOffersVideo) "Incoming video call" else "Incoming call",
                style = MaterialTheme.typography.labelLarge,
            )
            Text(
                text = callerName ?: "Unknown caller",
                style = MaterialTheme.typography.titleMedium,
                maxLines = 1,
            )
            Spacer(Modifier.height(12.dp))
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
            ) {
                OutlinedButton(onClick = onDecline) { Text("Decline") }
                Button(onClick = onAccept) { Text("Accept") }
            }
            if (call.remoteOffersVideo) {
                Spacer(Modifier.height(8.dp))
                Button(
                    onClick = onAcceptWithVideo,
                    modifier = Modifier.fillMaxWidth(),
                    colors = ButtonDefaults.buttonColors(
                        containerColor = MaterialTheme.colorScheme.secondary,
                        contentColor = MaterialTheme.colorScheme.onSecondary,
                    ),
                ) { Text("Accept with video") }
            }
        }
    }
}
