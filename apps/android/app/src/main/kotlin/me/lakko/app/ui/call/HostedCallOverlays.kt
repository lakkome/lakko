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
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
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
import me.lakko.app.calls.CallUiState
import me.lakko.app.calls.GroupCallHost
import me.lakko.app.calls.LinkCallHost
import me.lakko.app.calls.LinkCallNoticeAction
import me.lakko.app.calls.linkCallNotice

/**
 * The in-call screens of the calls the app-level host drives through a
 * [CallUiState] alone — the group (SFU) call and the call-link call — mounted
 * once above every screen by `MainActivity`, beside [OneToOneCallOverlay].
 *
 * Each is a full-screen dialog, so it covers the screens' own dialogs too; its
 * exits are the in-screen End button (the host leaves the call) and the
 * screen's own exit after the ended state was shown. The calls live in
 * [GroupCallHost] / [LinkCallHost]; these composables only render them, so a
 * destination change never hides or ends a call.
 */
@Composable
public fun GroupCallOverlay(host: GroupCallHost) {
    val visible by host.overlayVisible.collectAsStateWithLifecycle()
    if (!visible) return
    HostedCallDialog(
        state = host.ui,
        onEndCall = { host.leave() },
        onExit = { host.onOverlayExit() },
    )
}

/** See [GroupCallOverlay]. Up from the moment the link call is live. */
@Composable
public fun LinkCallOverlay(host: LinkCallHost) {
    val visible by host.overlayVisible.collectAsStateWithLifecycle()
    if (!visible) return
    HostedCallDialog(
        state = host.ui,
        onEndCall = { host.leave() },
        onExit = { host.onOverlayExit() },
    )
}

@Composable
private fun HostedCallDialog(state: CallUiState, onEndCall: () -> Unit, onExit: () -> Unit) {
    Dialog(
        onDismissRequest = { /* exit only via the End button */ },
        properties = DialogProperties(
            usePlatformDefaultWidth = false,
            dismissOnBackPress = false,
            dismissOnClickOutside = false,
        ),
    ) {
        Box(modifier = Modifier.fillMaxSize()) {
            CallScreen(onExit = onExit, state = state, onEndCall = onEndCall)
        }
    }
}

/**
 * A call-link call's status on a screen other than its landing: connecting or
 * waiting to be admitted (Open / Cancel), or how it ended (Dismiss). Nothing
 * while a landing shows the call itself or while it is live
 * ([linkCallNotice]). Same visual pattern as the incoming-call banner.
 *
 * @param onOpen return to the landing of the link with this slug.
 */
@Composable
public fun BoxScope.LinkCallNoticeBar(host: LinkCallHost, onOpen: (slug: String) -> Unit) {
    val slot by host.slot.collectAsStateWithLifecycle()
    val claims by host.landingClaims.collectAsStateWithLifecycle()
    val current = slot
    val notice = linkCallNotice(current?.phase, claims > 0, current?.errorMessage) ?: return
    Surface(
        modifier = Modifier
            .align(Alignment.BottomCenter)
            .fillMaxWidth()
            .navigationBarsPadding()
            .padding(horizontal = 12.dp, vertical = 8.dp)
            .semantics { liveRegion = LiveRegionMode.Polite },
        shape = RoundedCornerShape(16.dp),
        color = MaterialTheme.colorScheme.primaryContainer,
        contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
        tonalElevation = 6.dp,
        shadowElevation = 6.dp,
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(text = "Call from a link", style = MaterialTheme.typography.labelLarge)
            Text(text = notice.text, style = MaterialTheme.typography.bodyMedium)
            Spacer(Modifier.height(12.dp))
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
            ) {
                for (action in notice.actions) {
                    when (action) {
                        LinkCallNoticeAction.OPEN ->
                            OutlinedButton(onClick = { current?.let { onOpen(it.slug) } }) { Text("Open") }
                        LinkCallNoticeAction.CANCEL ->
                            OutlinedButton(onClick = { host.leave() }) { Text("Cancel") }
                        LinkCallNoticeAction.DISMISS ->
                            OutlinedButton(onClick = { host.dismissOutcome() }) { Text("Dismiss") }
                    }
                }
            }
        }
    }
}
