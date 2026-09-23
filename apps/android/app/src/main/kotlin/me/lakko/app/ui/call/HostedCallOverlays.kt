// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Link
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.calls.CallInviteIdentity
import me.lakko.app.calls.CallInviteLinkState
import me.lakko.app.calls.CallUiState
import me.lakko.app.calls.GroupCallHost
import me.lakko.app.calls.LinkCallHost
import me.lakko.app.calls.LinkCallNoticeAction
import me.lakko.app.calls.linkCallNotice
import me.lakko.app.calls.showsCallInvite
import me.lakko.app.ui.contacts.launchShareIntent

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
public fun GroupCallOverlay(
    host: GroupCallHost,
    /**
     * `docs/ux/calls.md` section 4 — the group's own name (ADR-0051), or `null`
     * when it has none. Resolved from the slot's group rather than from the
     * roster, because a call in a group is named by the group and a participant
     * label is not a name for it.
     */
    groupNameFor: (groupIdHex: String) -> String? = { null },
) {
    val visible by host.overlayVisible.collectAsStateWithLifecycle()
    if (!visible) return
    val slot by host.slot.collectAsStateWithLifecycle()
    HostedCallDialog(
        state = host.ui,
        onEndCall = { host.leave() },
        onExit = { host.onOverlayExit() },
        groupName = slot?.groupIdHex?.let(groupNameFor),
    )
}

/**
 * See [GroupCallOverlay]. Up from the moment the link call is live.
 *
 * This is the only call screen that carries `Invite`
 * (`docs/ux/call-links.md` section 3): a link names the call it was minted for,
 * and only this kind of call is the one a `/c/` link leads into.
 *
 * @param identity read at the moment `Invite` is pressed, never held — see
 *   [CallInviteIdentity]. `null` means the session is gone, which the control
 *   reports in one sentence without touching the call.
 */
@Composable
public fun LinkCallOverlay(
    host: LinkCallHost,
    identity: () -> CallInviteIdentity? = { null },
) {
    val visible by host.overlayVisible.collectAsStateWithLifecycle()
    if (!visible) return
    val slot by host.slot.collectAsStateWithLifecycle()
    val inviteState by host.inviteLink.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    // Only the host of a live call has a link to offer; everyone else gets no
    // control at all rather than one that cannot work.
    val canInvite = slot?.let { showsCallInvite(it.owner, it.phase) } == true
    HostedCallDialog(
        state = host.ui,
        onEndCall = { host.leave() },
        onExit = { host.onOverlayExit() },
        inviteState = inviteState.takeIf { canInvite },
        onInvite = {
            scope.launch {
                // The mint runs on the host's scope, so it survives this
                // composition; only the wait for it is scoped here.
                host.inviteLinkUrl(identity())?.let { url -> launchShareIntent(context, url) }
            }
        },
    )
}

@Composable
private fun HostedCallDialog(
    state: CallUiState,
    onEndCall: () -> Unit,
    onExit: () -> Unit,
    groupName: String? = null,
    /** `docs/ux/call-links.md` section 3; `null` draws no `Invite` control. */
    inviteState: CallInviteLinkState? = null,
    onInvite: () -> Unit = {},
) {
    Dialog(
        onDismissRequest = { /* exit only via the End button */ },
        properties = DialogProperties(
            usePlatformDefaultWidth = false,
            dismissOnBackPress = false,
            dismissOnClickOutside = false,
            // Full-screen and edge to edge: with the default the dialog is
            // placed below the status bar but keeps the full screen height,
            // so the controls row ran off the bottom edge. The call screen
            // pads itself with the safe-drawing insets.
            decorFitsSystemWindows = false,
        ),
    ) {
        EdgeToEdgeDialogWindow()
        Box(modifier = Modifier.fillMaxSize()) {
            // Both of these are multi-party calls, so the screen keeps the
            // grid, the roster and the in-call chat even while only one other
            // person is on it (docs/ux/calls.md section 4.1, section 9).
            CallScreen(
                onExit = onExit,
                state = state,
                onEndCall = onEndCall,
                groupCall = true,
                groupName = groupName,
                inviteState = inviteState,
                onInvite = onInvite,
            )
        }
    }
}

/**
 * Make the enclosing dialog window full screen and edge to edge, so the call
 * screen's safe-drawing padding is the only thing that keeps its content off
 * the system bars.
 *
 * Setting `DialogProperties.decorFitsSystemWindows = false` alone did not
 * change the window on the Compose version in use (observed on an API 35
 * emulator: the dialog window kept its fit-insets types), so the window is
 * configured directly. Doing so lets the safe-drawing inset reach the call
 * screen and lifted the controls row by the navigation bar height.

 */
@Composable
internal fun EdgeToEdgeDialogWindow() {
    val view = androidx.compose.ui.platform.LocalView.current
    androidx.compose.runtime.SideEffect {
        var provider: android.view.ViewParent? = view.parent
        while (provider != null && provider !is androidx.compose.ui.window.DialogWindowProvider) {
            provider = provider.parent
        }
        val window = (provider as? androidx.compose.ui.window.DialogWindowProvider)?.window
        if (window == null) return@SideEffect
        androidx.core.view.WindowCompat.setDecorFitsSystemWindows(window, false)
        window.setLayout(
            android.view.ViewGroup.LayoutParams.MATCH_PARENT,
            android.view.ViewGroup.LayoutParams.MATCH_PARENT,
        )
        window.attributes = window.attributes.apply {
            if (android.os.Build.VERSION.SDK_INT >= 30) {
                fitInsetsTypes = 0
                // Lay the window out over the display cutout too. A dialog
                // window defaults to keeping out of a short-edge cutout, and the
                // window manager then moved the full-height window down by the
                // status-bar height, pushing the controls row past the bottom
                // edge. The activity window already uses this mode.
                layoutInDisplayCutoutMode =
                    android.view.WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
            } else if (android.os.Build.VERSION.SDK_INT >= 28) {
                layoutInDisplayCutoutMode =
                    android.view.WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
            }
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
    val notice = linkCallNotice(
        phase = current?.phase,
        landingShowingIt = claims > 0,
        errorMessage = current?.errorMessage,
        // A call started from `New call` has no `/c/` landing to return to.
        hasLanding = current?.slug != null,
    ) ?: return
    Surface(
        modifier = Modifier
            .align(Alignment.BottomCenter)
            .fillMaxWidth()
            .navigationBarsPadding()
            .padding(horizontal = 12.dp, vertical = 8.dp)
            .semantics { liveRegion = LiveRegionMode.Polite },
        shape = RoundedCornerShape(12.dp),
        color = MaterialTheme.colorScheme.surfaceVariant,
        contentColor = MaterialTheme.colorScheme.onSurface,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        shadowElevation = 4.dp,
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    imageVector = Icons.Filled.Link,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.primary,
                )
                Spacer(Modifier.width(12.dp))
                Column {
                    Text(
                        text = "Call from a link",
                        style = MaterialTheme.typography.labelLarge,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Text(text = notice.text, style = MaterialTheme.typography.bodyMedium)
                }
            }
            Spacer(Modifier.height(12.dp))
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
            ) {
                for (action in notice.actions) {
                    when (action) {
                        LinkCallNoticeAction.OPEN ->
                            OutlinedButton(
                                onClick = { current?.slug?.let(onOpen) },
                            ) { Text("Open") }
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
