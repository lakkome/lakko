// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

/**
 * `Decline` from the incoming-call notification.
 *
 * A receiver rather than an activity because declining must NOT open the app
 * (`docs/ux/calls.md` section 2.2). It does two things, in this order:
 *
 *  1. stops the ring on this device — always, and first, so the phone falls
 *     silent even if everything after it fails;
 *  2. tells the call host to decline, which publishes the `End` that stops the
 *     caller ringing.
 *
 * ## What step 2 can and cannot do
 *
 * Publishing an `End` means encrypting a message to the group, which needs the
 * live MLS identity the app builds in `MainActivity`. When this process is
 * alive — the poll path, and any push that woke an already-running app — that
 * identity is there and the caller stops ringing at once. When the process was
 * spawned cold by the push alone it is not, and the host has no port attached:
 * the ring stops here and the caller stops at its own 45-second timeout
 * instead. Publishing from that process is not a matter of wiring: the
 * background path is read-only by construction (C-59), and encrypting a message
 * advances the ratchet it must not touch.
 *
 * ## Holding the process until the End is out
 *
 * The publish runs on the call host's own coroutines, after [onReceive] has
 * returned. With the app in the background, a process whose receiver has
 * finished is cached again and may be frozen before that publish completes,
 * and the caller then rings out its whole window (the iOS twin of this was seen
 * live on 2026-11-22). So the receiver uses [goAsync] and finishes only once
 * the host reports the `End` published, or after [DECLINE_PUBLISH_WAIT_MS].
 */
public class IncomingCallActionReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        val command = callRingCommand(
            action = intent.action,
            groupIdHex = intent.getStringExtra(CALL_RING_EXTRA_GROUP),
            callIdHex = intent.getStringExtra(CALL_RING_EXTRA_CALL_ID),
        ) ?: return
        if (command.action != CallRingAction.DECLINE) return
        IncomingCallRinger.attach(context)
        IncomingCallRinger.stopRinging(command.callIdHex)
        val pending = goAsync()
        val published = try {
            AppCallHost.instance.declineRingingPublished(command.groupIdHex, command.callIdHex)
        } catch (cause: Throwable) {
            Log.w(TAG, "decline could not reach the call host", cause)
            pending.finish()
            return
        }
        receiverScope.launch {
            try {
                if (withTimeoutOrNull(DECLINE_PUBLISH_WAIT_MS) { published.await() } == null) {
                    Log.w(TAG, "decline: the End was not published within the wait")
                }
            } finally {
                pending.finish()
            }
        }
    }

    internal companion object {
        const val TAG = "lakko-call-ring"

        /**
         * The longest the receiver holds the process for the `End`. Chosen,
         * not measured: inside the ten seconds a receiver may stay pending
         * before the system treats it as unresponsive.
         */
        const val DECLINE_PUBLISH_WAIT_MS: Long = 8_000L

        private val receiverScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    }
}
