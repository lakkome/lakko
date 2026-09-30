// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import me.lakko.app.calls.QuietCallOffer
import me.lakko.app.calls.callRingContent
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.t
import me.lakko.app.ui.components.LakkoAvatar

/**
 * A muted conversation's incoming call, offered without interrupting anything
 * (`docs/ux/calls.md` section 2.1.1).
 *
 * ## Why there are two shapes and not one
 *
 * The spec names two quiet surfaces because they are read in two different
 * situations, and each carries what that situation is missing.
 * [QuietIncomingCallPanel] is drawn inside the conversation, where the person
 * can already see who it is, so it says what KIND of call it is and offers the
 * full control set. [QuietIncomingCallRowActions] is drawn under a row in a list
 * of conversations, where the row itself names the person, so it adds only the
 * two buttons the spec asks a row to carry.
 *
 * ## What both of them are not
 *
 * Neither is a dialog, a popup, or anything drawn over the screen. Both sit in
 * the layout of whatever is showing them: the panel is the first child of the
 * chat's own column, above the message list, and the row actions are a second
 * line inside the row. So nothing the person was reading moves out from under
 * them and nothing takes focus away from what they were typing — which is the
 * whole difference between offering a muted call and interrupting with it.
 *
 * Neither counts down either. The host's RING timer ends a call nobody answered
 * and clears the slot these read from, so they disappear when the window runs
 * out, by the same reading that files the missed call.
 */
@Composable
public fun QuietIncomingCallPanel(
    offer: QuietCallOffer,
    /** The caller as the surrounding screen already names them, or `null`. */
    callerName: String?,
    onAccept: (withVideo: Boolean) -> Unit,
    onDecline: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val permissions = rememberCallPermissionGate()
    // The same two lines the ring screen and the call notification use, from
    // the same function, so a muted call is described in the words an unmuted
    // one would have used.
    val content = callRingContent(callerName, offer.video, LocalStrings.current)
    Surface(
        color = MaterialTheme.colorScheme.secondaryContainer,
        modifier = modifier
            .fillMaxWidth()
            // Announced once when it appears: a person who muted a conversation
            // is by definition not watching it, and this is the only notice
            // they get.
            .semantics { liveRegion = LiveRegionMode.Polite },
    ) {
        Column(modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                LakkoAvatar(name = content.title, seed = offer.groupIdHex, size = 36.dp)
                Spacer(Modifier.width(10.dp))
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = content.title,
                        style = MaterialTheme.typography.titleSmall,
                        color = MaterialTheme.colorScheme.onSecondaryContainer,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        text = content.body,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSecondaryContainer,
                    )
                }
            }
            Spacer(Modifier.height(4.dp))
            // The controls on their own line rather than beside the name. A
            // video offer has three of them (section 2.2's set), and three
            // buttons next to two lines of text do not fit a phone's width —
            // the accept button is what would have been pushed off the edge.
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.End,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                TextButton(
                    onClick = onDecline,
                    colors = ButtonDefaults.textButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                ) { Text(t("call.incoming.decline")) }
                if (offer.video) {
                    // Answering a video call with the camera off. The panel has
                    // room for it; the list row does not, and the permission
                    // gate offers the same escape when the camera is refused.
                    //
                    // It is the one of the three that yields width, and it wraps
                    // rather than clipping: on a narrow phone the row is Decline
                    // plus this plus Accept, and the button that must survive
                    // that squeeze intact is the one that answers the call.
                    TextButton(
                        onClick = { permissions.request(video = false) { onAccept(false) } },
                        contentPadding = PaddingValues(horizontal = 8.dp),
                        modifier = Modifier.weight(1f, fill = false),
                    ) {
                        Text(
                            text = t("call.incoming.accept_without_video"),
                            style = MaterialTheme.typography.labelLarge,
                            maxLines = 2,
                            textAlign = TextAlign.Center,
                        )
                    }
                }
                Button(
                    onClick = { permissions.request(video = offer.video) { withVideo -> onAccept(withVideo) } },
                ) { Text(t("call.incoming.accept")) }
            }
        }
    }
    HorizontalDivider()
}

/**
 * The two buttons a ringing conversation's ROW carries (`docs/ux/calls.md`
 * section 2.1.1), drawn under the row's own two lines.
 *
 * `Accept` answers the call that was offered, video and all: the row names only
 * two buttons and a third would not fit beside them. Nothing is lost by that —
 * a person who does not want the camera on refuses it at the permission prompt,
 * which the gate turns into an audio-only answer, and the panel inside the
 * conversation offers the choice outright.
 */
@Composable
public fun QuietIncomingCallRowActions(
    offer: QuietCallOffer,
    /** The caller as the row already names them — for the spoken labels only. */
    callerName: String?,
    onAccept: (withVideo: Boolean) -> Unit,
    onDecline: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val permissions = rememberCallPermissionGate()
    val content = callRingContent(callerName, offer.video, LocalStrings.current)
    val declineSpoken = t(
        if (offer.video) "call.incoming.decline_video_a11y" else "call.incoming.decline_voice_a11y",
        mapOf("name" to content.title),
    )
    val acceptSpoken = t(
        if (offer.video) "call.incoming.accept_video_a11y" else "call.incoming.accept_voice_a11y",
        mapOf("name" to content.title),
    )
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = modifier
            .fillMaxWidth()
            .padding(start = 68.dp, end = 12.dp, bottom = 8.dp)
            .semantics { liveRegion = LiveRegionMode.Polite },
    ) {
        Text(
            text = content.body,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.primary,
            maxLines = 1,
            modifier = Modifier.weight(1f),
        )
        TextButton(
            onClick = onDecline,
            colors = ButtonDefaults.textButtonColors(
                contentColor = MaterialTheme.colorScheme.error,
            ),
            // The row is a list of conversations, so a button in it has to say
            // WHICH conversation it answers; the visible label cannot, at this
            // width, and a screen reader has no row context to borrow. Set on
            // the merged node rather than cleared onto it: clearing would take
            // the click action off the one affordance whose entire purpose is
            // being pressable.
            modifier = Modifier.semantics {
                contentDescription = declineSpoken
            },
        ) { Text(t("call.incoming.decline")) }
        Button(
            onClick = { permissions.request(video = offer.video) { withVideo -> onAccept(withVideo) } },
            modifier = Modifier.semantics {
                contentDescription = acceptSpoken
            },
        ) { Text(t("call.incoming.accept")) }
    }
}
