// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import me.lakko.app.calls.callRingContent
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.t
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.theme.LakkoDark
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

/**
 * The incoming call screen of `docs/ux/calls.md` section 2.2, with no opinion
 * about who is showing it.
 *
 * Two callers, and the split is the point. [IncomingCallActivity] shows it
 * through the ring notification's full-screen intent, which is the ONLY way a
 * locked or backgrounded phone can put a call on screen. The app shows the
 * same composable itself whenever it is in the foreground, because the
 * platform does not honour a full-screen intent on an unlocked, in-use device
 * -- it downgrades it to a heads-up that folds away after a few seconds, so
 * the spec's first row ("App open, any screen: full-screen incoming call over
 * the app") cannot be delivered by the notification at all. Before this
 * existed the in-app case was a top banner, which the heads-up then covered:
 * four of seven calls on the 2026-09-21 walk were lost to tapping a
 * notification that was no longer there.
 *
 * It owns no call state and decides nothing: the caller supplies what to show
 * and what the two buttons do.
 */
@Composable
internal fun IncomingCallContent(
    callerName: String?,
    video: Boolean,
    avatarSeed: String,
    pendingAnswer: Boolean?,
    /** ADR-0084 — the caller's signature key, for their profile photo. */
    personKey: ByteArray? = null,
    onAnswerConsumed: () -> Unit,
    onAccept: (withVideo: Boolean) -> Unit,
    onDecline: () -> Unit,
) {
    val permissions = rememberCallPermissionGate()
    val content = callRingContent(callerName, video, LocalStrings.current)
    val e2eeLabel = t("call.e2ee")
    val acceptWithoutVideoSpoken = t("call.incoming.accept_without_video_a11y", mapOf("name" to content.title))
    LaunchedEffect(avatarSeed, pendingAnswer) {
        val choice = pendingAnswer ?: return@LaunchedEffect
        onAnswerConsumed()
        // Even an answer taken on the notification goes through the gate: a
        // call without the microphone is not a call, and this is the one place
        // that can ask for it (docs/ux/calls.md section 2.1).
        permissions.request(video = choice && video) { v -> onAccept(v) }
    }
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(LakkoDark.background)
            .windowInsetsPadding(WindowInsets.safeDrawing),
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(horizontal = 24.dp, vertical = 32.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.SpaceBetween,
        ) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 48.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                LakkoAvatar(name = content.title, seed = avatarSeed, size = 96.dp, personKey = personKey)
                Spacer(Modifier.height(20.dp))
                Text(
                    text = content.title,
                    color = LakkoDark.text,
                    fontSize = 28.sp,
                    fontWeight = FontWeight.SemiBold,
                    maxLines = 1,
                )
                Spacer(Modifier.height(8.dp))
                Text(text = content.body, color = LakkoDark.textMuted, fontSize = 15.sp)
                Spacer(Modifier.height(16.dp))
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.clearAndSetSemantics {
                        contentDescription = e2eeLabel
                    },
                ) {
                    LkIcon(
                        R.drawable.ic_lk_lock_fill,
                        contentDescription = null,
                        tint = LakkoDark.textMuted,
                        size = LkIconSize.Mark,
                    )
                    Spacer(Modifier.width(6.dp))
                    Text(
                        text = e2eeLabel,
                        color = LakkoDark.textMuted,
                        fontSize = 13.sp,
                    )
                }
            }

            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(bottom = 24.dp),
                horizontalArrangement = Arrangement.SpaceEvenly,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                RoundCallButton(
                    onClick = onDecline,
                    container = LakkoDark.danger,
                    contentColor = LakkoDark.dangerFg,
                    icon = R.drawable.ic_lk_phone_down_fill,
                    label = t("call.incoming.decline"),
                    spokenLabel = t(
                        if (video) "call.incoming.decline_video_a11y" else "call.incoming.decline_voice_a11y",
                        mapOf("name" to content.title),
                    ),
                )
                // A video call's lesser third option, between the two
                // (docs/ux/calls.md section 2.2).
                if (video) {
                    TextButton(
                        onClick = { permissions.request(video = false) { onAccept(false) } },
                        modifier = Modifier.clearAndSetSemantics {
                            contentDescription = acceptWithoutVideoSpoken
                        },
                    ) {
                        Text(
                            text = t("call.incoming.accept_without_video"),
                            color = LakkoDark.textMuted,
                            fontSize = 13.sp,
                        )
                    }
                }
                RoundCallButton(
                    onClick = { permissions.request(video = video) { v -> onAccept(v) } },
                    container = LakkoDark.success,
                    contentColor = LakkoDark.accentFg,
                    // A video call is accepted with the camera glyph, as on iOS and web.
                    icon = if (video) R.drawable.ic_lk_video_fill else R.drawable.ic_lk_phone_fill,
                    label = t("call.incoming.accept"),
                    spokenLabel = t(
                        if (video) "call.incoming.accept_video_a11y" else "call.incoming.accept_voice_a11y",
                        mapOf("name" to content.title),
                    ),
                )
            }
        }
    }
}

@Composable
private fun RoundCallButton(
    onClick: () -> Unit,
    container: androidx.compose.ui.graphics.Color,
    contentColor: androidx.compose.ui.graphics.Color,
    @androidx.annotation.DrawableRes icon: Int,
    label: String,
    spokenLabel: String,
) {
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        Button(
            onClick = onClick,
            shape = CircleShape,
            colors = ButtonDefaults.buttonColors(
                containerColor = container,
                contentColor = contentColor,
            ),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(0.dp),
            modifier = Modifier
                .size(72.dp)
                .clearAndSetSemantics { contentDescription = spokenLabel },
        ) {
            // 32 in the 72 circle, the iOS proportion.
            LkIcon(icon, contentDescription = null, size = 32.dp)
        }
        Spacer(Modifier.height(8.dp))
        // The label carries the meaning so nothing rests on colour alone
        // (docs/ux/calls.md section 8); TalkBack reads the button's own
        // spoken label instead of this duplicate.
        Text(
            text = label,
            color = LakkoDark.textMuted,
            fontSize = 13.sp,
            modifier = Modifier.clearAndSetSemantics { },
        )
    }
}
