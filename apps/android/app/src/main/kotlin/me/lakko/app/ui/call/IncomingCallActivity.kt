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
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.calls.AppCallHost
import me.lakko.app.calls.CALL_RING_ACTION_ACCEPT
import me.lakko.app.calls.CALL_RING_ACTION_ACCEPT_AUDIO_ONLY
import me.lakko.app.calls.CALL_RING_EXTRA_CALL_ID
import me.lakko.app.calls.CALL_RING_EXTRA_GROUP
import me.lakko.app.calls.IncomingCallRing
import me.lakko.app.calls.IncomingCallRinger
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
        // The ring can be the first screen of this process (a push woke it):
        // load the interface language before drawing (ADR-0061). Idempotent.
        me.lakko.app.l10n.Localization.init(applicationContext)

        readAnswerAction(intent)

        setContent {
            // Call surfaces are dark by design (design system, section 6): the
            // ring is the same surface as the call it becomes.
            LakkoTheme(dark = true) { me.lakko.app.l10n.ProvideLocalization {
                val ring by IncomingCallRinger.ringing.collectAsStateWithLifecycle()
                val current = ring
                LaunchedEffect(current) {
                    // Nothing is ringing any more: declined here, answered on
                    // another device, cancelled by the caller, or out of time.
                    if (current == null) finishRing()
                }
                if (current != null) {
                    IncomingCallContent(
                        callerName = current.callerName,
                        video = current.video,
                        avatarSeed = current.callIdHex,
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
            } }
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
