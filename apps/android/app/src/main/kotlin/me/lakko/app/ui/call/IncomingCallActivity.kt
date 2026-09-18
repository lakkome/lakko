// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import android.app.KeyguardManager
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Call
import androidx.compose.material.icons.filled.CallEnd
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.calls.AppCallHost
import me.lakko.app.calls.CALL_RING_ACTION_ACCEPT
import me.lakko.app.calls.CALL_RING_ACTION_ACCEPT_AUDIO_ONLY
import me.lakko.app.calls.CALL_RING_EXTRA_CALL_ID
import me.lakko.app.calls.CALL_RING_EXTRA_GROUP
import me.lakko.app.calls.IncomingCallRing
import me.lakko.app.calls.IncomingCallRinger
import me.lakko.app.calls.callRingContent
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.theme.LakkoDark
import me.lakko.app.ui.theme.LakkoTheme

/**
 * The incoming call screen, `docs/ux/calls.md` section 2.2.
 *
 * Shown by the ring notification's full-screen intent, which is how a call
 * reaches a locked phone: the window turns the screen on and draws over the
 * keyguard, so the call can be answered without unlocking.
 *
 * The screen renders exactly what section 2.2 asks for and nothing else — the
 * caller, one line saying what kind of call it is, a lock with one word, and
 * the two (or three) buttons. No code, no digits, no protocol words; the safety
 * number belongs to the call screen's encryption sheet, not to a ring.
 *
 * It owns no call state. [IncomingCallRinger.ringing] is the single source of
 * truth for "is this phone ringing, and for what", so a call that ends anywhere
 * — declined here, answered on another device, cancelled by the caller, or
 * simply out of time — closes this screen with no second mechanism.
 */
public class IncomingCallActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        showOverLockScreen()
        IncomingCallRinger.attach(applicationContext)

        readAnswerAction(intent)

        setContent {
            // Call surfaces are dark by design (design system, section 6): the
            // ring is the same surface as the call it becomes.
            LakkoTheme(dark = true) {
                val ring by IncomingCallRinger.ringing.collectAsStateWithLifecycle()
                val current = ring
                LaunchedEffect(current) {
                    // Nothing is ringing any more: declined here, answered on
                    // another device, cancelled by the caller, or out of time.
                    if (current == null) finishRing()
                }
                if (current != null) {
                    IncomingCallContent(
                        ring = current,
                        // Set when the user tapped Accept on the NOTIFICATION
                        // rather than opening this screen: their choice is
                        // already made, so it is taken as soon as the screen
                        // can put it through the microphone permission gate.
                        pendingAnswer = pendingAnswer,
                        onAnswerConsumed = { pendingAnswer = null },
                        onAccept = { withVideo -> answer(current, withVideo) },
                        onDecline = { decline(current) },
                    )
                }
            }
        }
    }

    /** `true` = answer with video if the offer carries it, `false` = audio only. */
    private var pendingAnswer: Boolean? by mutableStateOf(null)

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        readAnswerAction(intent)
    }

    /** An intent naming an Accept action carries a choice the user already made. */
    private fun readAnswerAction(intent: Intent?) {
        pendingAnswer = when (intent?.action) {
            CALL_RING_ACTION_ACCEPT -> true
            CALL_RING_ACTION_ACCEPT_AUDIO_ONLY -> false
            // CALL_RING_ACTION_SHOW and anything else: show the screen and let
            // the user choose.
            else -> null
        }
    }

    private fun answer(ring: IncomingCallRing, withVideo: Boolean) {
        IncomingCallRinger.stopRinging(ring.callIdHex)
        // The host may not have a session for this call yet (the offer is only
        // held while the app is backgrounded, and a cold start has not polled).
        // It remembers the answer and takes it the moment the call rings there.
        AppCallHost.instance.acceptRinging(ring.callIdHex, withVideo)
        dismissKeyguardThen {
            startActivity(
                Intent(this, me.lakko.app.MainActivity::class.java).apply {
                    action = if (withVideo) CALL_RING_ACTION_ACCEPT else CALL_RING_ACTION_ACCEPT_AUDIO_ONLY
                    addFlags(
                        Intent.FLAG_ACTIVITY_NEW_TASK or
                            Intent.FLAG_ACTIVITY_CLEAR_TOP or
                            Intent.FLAG_ACTIVITY_SINGLE_TOP,
                    )
                    putExtra(CALL_RING_EXTRA_GROUP, ring.groupIdHex)
                    putExtra(CALL_RING_EXTRA_CALL_ID, ring.callIdHex)
                },
            )
            finishRing()
        }
    }

    private fun decline(ring: IncomingCallRing) {
        IncomingCallRinger.stopRinging(ring.callIdHex)
        AppCallHost.instance.declineRinging(ring.groupIdHex, ring.callIdHex)
        finishRing()
    }

    private fun finishRing() {
        finish()
    }

    /**
     * Draw over the keyguard and turn the screen on. The API 27+ calls are the
     * supported route; the window flags are the pre-27 equivalent and are set
     * on every version because `showWhenLocked` in the manifest alone does not
     * dismiss a secure keyguard's scrim on some OEM builds.
     */
    private fun showOverLockScreen() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
            setShowWhenLocked(true)
            setTurnScreenOn(true)
        }
        @Suppress("DEPRECATION")
        window.addFlags(
            WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or
                WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON or
                WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON,
        )
    }

    /** Answering needs the app itself, so a secure keyguard is dismissed first. */
    private fun dismissKeyguardThen(onDone: () -> Unit) {
        val keyguard = getSystemService(Context.KEYGUARD_SERVICE) as? KeyguardManager
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1 &&
            keyguard != null &&
            keyguard.isKeyguardLocked
        ) {
            keyguard.requestDismissKeyguard(
                this,
                object : KeyguardManager.KeyguardDismissCallback() {
                    override fun onDismissSucceeded() = onDone()

                    // Dismissal refused or cancelled: still hand the call to the
                    // app, which the user reaches as soon as they unlock.
                    override fun onDismissError() = onDone()

                    override fun onDismissCancelled() = onDone()
                },
            )
        } else {
            onDone()
        }
    }
}

@Composable
private fun IncomingCallContent(
    ring: IncomingCallRing,
    pendingAnswer: Boolean?,
    onAnswerConsumed: () -> Unit,
    onAccept: (withVideo: Boolean) -> Unit,
    onDecline: () -> Unit,
) {
    val permissions = rememberCallPermissionGate()
    val content = callRingContent(ring.callerName, ring.video)
    LaunchedEffect(ring.callIdHex, pendingAnswer) {
        val choice = pendingAnswer ?: return@LaunchedEffect
        onAnswerConsumed()
        // Even an answer taken on the notification goes through the gate: a
        // call without the microphone is not a call, and this is the one place
        // that can ask for it (docs/ux/calls.md section 2.1).
        permissions.request(video = choice && ring.video) { v -> onAccept(v) }
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
                LakkoAvatar(name = content.title, seed = ring.callIdHex, size = 96.dp)
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
                        contentDescription = "This call is end-to-end encrypted"
                    },
                ) {
                    Icon(
                        Icons.Filled.Lock,
                        contentDescription = null,
                        tint = LakkoDark.textMuted,
                        modifier = Modifier.size(14.dp),
                    )
                    Spacer(Modifier.width(6.dp))
                    Text(
                        text = "End-to-end encrypted",
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
                    icon = Icons.Filled.CallEnd,
                    label = "Decline",
                    spokenLabel = "Decline ${spokenKind(ring.video)} from ${content.title}",
                )
                // A video call's lesser third option, between the two
                // (docs/ux/calls.md section 2.2).
                if (ring.video) {
                    TextButton(
                        onClick = { permissions.request(video = false) { onAccept(false) } },
                        modifier = Modifier.clearAndSetSemantics {
                            contentDescription = "Accept video call from ${content.title} without video"
                        },
                    ) {
                        Text(
                            text = "Accept without\nvideo",
                            color = LakkoDark.textMuted,
                            fontSize = 13.sp,
                        )
                    }
                }
                RoundCallButton(
                    onClick = { permissions.request(video = ring.video) { v -> onAccept(v) } },
                    container = LakkoDark.success,
                    contentColor = LakkoDark.accentFg,
                    icon = Icons.Filled.Call,
                    label = "Accept",
                    spokenLabel = "Accept ${spokenKind(ring.video)} from ${content.title}",
                )
            }
        }
    }
}

/** `voice call` / `video call`, for a spoken label that reads as a sentence. */
private fun spokenKind(video: Boolean): String = if (video) "video call" else "voice call"

@Composable
private fun RoundCallButton(
    onClick: () -> Unit,
    container: androidx.compose.ui.graphics.Color,
    contentColor: androidx.compose.ui.graphics.Color,
    icon: androidx.compose.ui.graphics.vector.ImageVector,
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
            Icon(icon, contentDescription = null, modifier = Modifier.size(28.dp))
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
