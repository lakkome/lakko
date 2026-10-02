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
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
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
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.calls.CallInviteIdentity
import me.lakko.app.calls.InCallInvite
import me.lakko.app.calls.CallUiState
import me.lakko.app.calls.GroupCallHost
import me.lakko.app.calls.LinkCallHost
import me.lakko.app.calls.LinkCallNoticeAction
import me.lakko.app.calls.linkCallNotice
import me.lakko.app.calls.showsCallInvite
import me.lakko.app.chat.ChatRepository
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import me.lakko.app.l10n.UiText
import me.lakko.app.ui.chat.ConversationPicker

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
    // `docs/ux/calls.md` section 4.5 — the call has been put away into the bar.
    // The same flag is handed to the bar's rule as `callScreenUp`, so the screen
    // and the bar are never both up. It covers the ended state on purpose: a
    // collapsed call that ends must not throw its screen across whatever the
    // person went off to read.
    val collapsed by host.collapsed.collectAsStateWithLifecycle()
    if (!visible || collapsed) return
    val slot by host.slot.collectAsStateWithLifecycle()
    HostedCallDialog(
        state = host.ui,
        onEndCall = { host.leave() },
        onExit = { host.onOverlayExit() },
        onCollapse = { host.collapse() },
        // Section 4.5 — the CALL's connect moment, so the screen's running time
        // survives being put away and brought back.
        connectedSinceMs = host.connectedSinceMs.collectAsStateWithLifecycle().value,
        groupName = slot?.groupIdHex?.let(groupNameFor),
    )
}

/**
 * See [GroupCallOverlay]. Up from the moment the link call is live.
 *
 * This is the only call screen whose Participants control carries the invite
 * items (`docs/ux/call-links.md` section 3): a link names the call it was
 * minted for, and only this kind of call is the one a `/c/` link leads into.
 *
 * @param identity read at the moment an invite item is chosen, never held —
 *   see [CallInviteIdentity]. `null` means the session is gone, which the
 *   status line reports in one sentence without touching the call.
 * @param chatRepository where an invitation is sent from and where the list
 *   of conversations comes from; `null` leaves `Invite contacts` out.
 */
@Composable
public fun LinkCallOverlay(
    host: LinkCallHost,
    identity: () -> CallInviteIdentity? = { null },
    /**
     * Leave the `/c/` landing this call was reached through.
     *
     * Called when the call is put away, and not only when it ends. The landing
     * is drawn ABOVE the session screens — including the collapsed call bar's
     * strip at the top of them — so a call put away while its landing was still
     * up would leave the person looking at neither the call nor the bar.
     * Section 4.5 collapses a call to give the app back, and the landing of a
     * link is not the app.
     */
    onLeaveLanding: () -> Unit = {},
    chatRepository: ChatRepository? = null,
    deviceGroupSync: DeviceGroupSyncRepository? = null,
) {
    val visible by host.overlayVisible.collectAsStateWithLifecycle()
    /** See [GroupCallOverlay] — section 4.5's collapsed presentation. */
    val collapsed by host.collapsed.collectAsStateWithLifecycle()
    if (!visible || collapsed) return
    val slot by host.slot.collectAsStateWithLifecycle()
    val invite by host.invite.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    // Only the host of a live call has a link to offer; everyone else gets the
    // participant list alone rather than an item that cannot work.
    val canInvite = slot?.let { showsCallInvite(it.owner, it.phase) } == true
    // Someone with no conversations (a guest who came by a link) has nowhere
    // to send an invitation: for them the menu is the link alone.
    val chatState = chatRepository?.state?.collectAsStateWithLifecycle()
    val hasConversations = chatState?.value?.threads?.any { !it.membershipEnded } == true
    var pickerOpen by remember { mutableStateOf(false) }
    HostedCallDialog(
        state = host.ui,
        onEndCall = { host.leave() },
        onExit = { host.onOverlayExit() },
        onCollapse = {
            host.collapse()
            onLeaveLanding()
        },
        // See [GroupCallOverlay] — section 4.5's running time.
        connectedSinceMs = host.connectedSinceMs.collectAsStateWithLifecycle().value,
        invite = invite.takeIf { canInvite },
        onCopyInviteLink = {
            scope.launch {
                // The mint and the copy run on the host's scope, so they
                // survive this composition; only the wait is scoped here.
                host.copyInviteLink(identity(), clipboardCopier(context))
            }
        },
        onInviteContact = if (chatRepository != null && hasConversations) {
            { pickerOpen = true }
        } else {
            null
        },
    )
    if (pickerOpen && chatRepository != null) {
        CallInvitePickerDialog(
            host = host,
            identity = identity,
            chatRepository = chatRepository,
            deviceGroupSync = deviceGroupSync,
            onClose = { pickerOpen = false },
        )
    }
}

/**
 * Put a string on the clipboard, or `null` where there is no clipboard to
 * reach; the call's link is then shown to be copied by hand.
 */
private fun clipboardCopier(context: Context): ((String) -> Unit)? {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager ?: return null
    return { text ->
        clipboard.setPrimaryClip(
            ClipData.newPlainText(me.lakko.app.l10n.Localization.strings.value["common.clipboard.link"], text),
        )
    }
}

/**
 * `Invite contacts` (`docs/ux/call-links.md` section 3): the list of the
 * person's conversations, the same list `Forward to` shows, over the call.
 *
 * Picking a row sends the invitation into it: one plain text message whose
 * whole text is the call's link, through the conversation's ordinary send, so
 * it is recorded there like any other sent message and every client draws it
 * as an invitation with a `Join` button. The list stays open so several
 * conversations can be invited; a row that was sent to shows a check and takes
 * no second pick. A failure says so above the list and leaves the row
 * pressable; the call is untouched either way. The person closes the list
 * with its own back control.
 */
@Composable
private fun CallInvitePickerDialog(
    host: LinkCallHost,
    identity: () -> CallInviteIdentity?,
    chatRepository: ChatRepository,
    deviceGroupSync: DeviceGroupSyncRepository?,
    onClose: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<UiText?>(null) }
    var invited by remember { mutableStateOf(emptySet<String>()) }
    Dialog(
        onDismissRequest = { if (!busy) onClose() },
        properties = DialogProperties(
            usePlatformDefaultWidth = false,
            dismissOnBackPress = true,
            dismissOnClickOutside = false,
        ),
    ) {
        ConversationPicker(
            title = t("call.invite.picker_title"),
            repository = chatRepository,
            deviceGroupSync = deviceGroupSync,
            busy = busy,
            busyLabel = t("common.sending"),
            error = error,
            onBack = onClose,
            onPick = { row ->
                if (!busy) {
                    busy = true
                    error = null
                    scope.launch {
                        val sent = try {
                            host.sendInvite(identity(), row.title) { url ->
                                chatRepository.sendMessage(groupIdHex = row.id, plaintext = url)
                            }
                        } finally {
                            busy = false
                        }
                        if (sent) invited = invited + row.id else error = UiText("call.invite.send_failed")
                    }
                }
            },
            modifier = Modifier.fillMaxSize(),
            doneIds = invited,
            doneLabel = t("call.invite.row_invited"),
        )
    }
}

@Composable
private fun HostedCallDialog(
    state: CallUiState,
    onEndCall: () -> Unit,
    onExit: () -> Unit,
    /** `docs/ux/calls.md` section 4.5 — put the call away into the bar. */
    onCollapse: () -> Unit,
    /** `docs/ux/calls.md` section 4.5 — the CALL's connect moment, so the
     *  running time counts from the call and not from this screen's mount. */
    connectedSinceMs: Long?,
    groupName: String? = null,
    /** `docs/ux/call-links.md` section 3; `null` for a call with no link to
     *  offer. */
    invite: InCallInvite? = null,
    onCopyInviteLink: () -> Unit = {},
    onInviteContact: (() -> Unit)? = null,
) {
    Dialog(
        // Section 4.5 — the system back COLLAPSES the call and never ends it.
        // Routed through the dialog's own back path rather than a `BackHandler`
        // inside it, because this window's back is the platform's to deliver and
        // that is the path it delivers it on. Returning here does not dismiss
        // the dialog; the caller's collapse gate is what stops drawing it.
        onDismissRequest = onCollapse,
        properties = DialogProperties(
            usePlatformDefaultWidth = false,
            dismissOnBackPress = true,
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
            // tiles and the roster even while only one other person is on it
            // (docs/ux/calls.md section 4.1).
            CallScreen(
                onExit = onExit,
                state = state,
                onEndCall = onEndCall,
                groupCall = true,
                groupName = groupName,
                invite = invite,
                onCopyInviteLink = onCopyInviteLink,
                onInviteContact = onInviteContact,
                // Section 4.5 — the chevron, beside the system back.
                onCollapse = onCollapse,
                connectedSinceMs = connectedSinceMs,
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
        error = current?.error,
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
                        text = t("call.link.notice.a11y"),
                        style = MaterialTheme.typography.labelLarge,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Text(text = notice.text.text(), style = MaterialTheme.typography.bodyMedium)
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
                            ) { Text(t("common.open")) }
                        LinkCallNoticeAction.CANCEL ->
                            OutlinedButton(onClick = { host.leave() }) { Text(t("common.cancel")) }
                        LinkCallNoticeAction.DISMISS ->
                            OutlinedButton(onClick = { host.dismissOutcome() }) { Text(t("common.dismiss")) }
                    }
                }
            }
        }
    }
}
