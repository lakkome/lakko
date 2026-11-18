// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.app.Activity
import android.app.PendingIntent
import android.app.PictureInPictureParams
import android.app.RemoteAction
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.drawable.Icon
import android.os.Build
import android.util.Log
import android.util.Rational
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import me.lakko.app.R
import me.lakko.app.l10n.UiText

/**
 * Leaving the app during a call: the system's own picture-in-picture window —
 * `docs/ux/calls.md` section 4.5.4.
 *
 * When the app leaves the screen during a call (home, the app switcher,
 * another app), whether the call was full screen or collapsed, the same
 * picture the floating window shows goes into the system window. It opens by
 * itself on Android 12 and later (auto-enter parameters kept current while a
 * call can be put away) and, on 8 to 11, when the person leaves the app with
 * home or a switch ([onUserLeaveHint]). A call that is only ringing has none,
 * the same as it does not collapse.
 *
 * Its one control is the microphone, drawn on and off. There is no End
 * (section 4.5.1). Closing the system window never ends the call: the call
 * lives in the process-wide [AppCallHost], not in the activity.
 */

/** What the system window should be for the current call, or `null` when
 *  there is none (no call, a ringing call, an ended call). */
public data class PictureInPictureSpec(
    val aspect: PipAspect,
    /** The microphone is on now; the action turns it off. */
    val micOn: Boolean,
    /** Whether the action can be pressed (not on the `Calling...` row, where
     *  section 4.1 leaves only End active and the window has no End). */
    val micEnabled: Boolean,
)

/**
 * The spec for a call the window can show ([live] = the `call_bar` rule says
 * it can be put away), or `null`. Pure.
 */
public fun pictureInPictureSpec(
    live: Boolean,
    screenShare: Boolean,
    micOn: Boolean,
    micEnabled: Boolean,
): PictureInPictureSpec? =
    if (!live) {
        null
    } else {
        PictureInPictureSpec(
            aspect = pictureInPictureAspect(screenShare),
            micOn = micOn,
            micEnabled = micEnabled,
        )
    }

/**
 * The action's words: what pressing it does, from the call screen's own
 * microphone control, so the system window names it as the call screen does.
 */
public fun pictureInPictureMicLabel(micOn: Boolean): UiText =
    UiText(if (micOn) "call.control.mute_a11y" else "call.control.unmute_a11y")

/** Intent action for the system window's microphone action. */
public const val PIP_ACTION_SET_MIC: String = "me.lakko.app.calls.PIP_SET_MIC"

/** Extra: the microphone state the press asks for. */
public const val PIP_EXTRA_MIC_ON: String = "on"

/**
 * The state of the system window, process-wide, and the activity side of it.
 *
 * Main thread only.
 */
public object CallPictureInPicture {
    private val mutableActive = MutableStateFlow(false)

    /** The activity is in the system's picture-in-picture window right now. */
    public val active: StateFlow<Boolean> = mutableActive.asStateFlow()

    /** The spec last applied, so the parameters are set only on a change. */
    private var applied: PictureInPictureSpec? = null

    /** Whether the current call wants the system window when the app leaves. */
    @Volatile
    private var wanted: PictureInPictureSpec? = null

    /** Told by the activity (`onPictureInPictureModeChanged`, `onCreate`). */
    public fun setActive(inPictureInPicture: Boolean) {
        mutableActive.value = inPictureInPicture
    }

    /** Whether this device offers picture-in-picture at all. */
    public fun supported(context: Context): Boolean =
        context.packageManager.hasSystemFeature(PackageManager.FEATURE_PICTURE_IN_PICTURE)

    /**
     * Keep the activity's parameters current for [spec] (a call that can be
     * put away) or turn auto-enter off ([spec] `null`). Cheap to call on every
     * recomposition: the platform is told only when something changed.
     */
    public fun update(activity: Activity, spec: PictureInPictureSpec?) {
        wanted = spec
        if (spec == applied) return
        if (!supported(activity)) {
            applied = spec
            return
        }
        try {
            activity.setPictureInPictureParams(params(activity, spec))
            applied = spec
        } catch (cause: IllegalStateException) {
            // The activity is finishing or the platform refuses the
            // parameters; the call itself is untouched.
            Log.w(TAG, "picture-in-picture parameters not applied: ${cause.javaClass.simpleName}")
        } catch (cause: IllegalArgumentException) {
            Log.w(TAG, "picture-in-picture parameters refused: ${cause.javaClass.simpleName}")
        }
    }

    /**
     * Android 8 to 11: the person left the app with home or a switch. Enter
     * the system window when a call can be put away. On 12 and later the
     * platform enters it by itself from the auto-enter parameters.
     */
    public fun onUserLeaveHint(activity: Activity) {
        val spec = wanted ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) return
        if (!supported(activity)) return
        try {
            activity.enterPictureInPictureMode(params(activity, spec))
        } catch (cause: IllegalStateException) {
            Log.w(TAG, "picture-in-picture not entered: ${cause.javaClass.simpleName}")
        } catch (cause: IllegalArgumentException) {
            Log.w(TAG, "picture-in-picture not entered: ${cause.javaClass.simpleName}")
        }
    }

    private fun params(context: Context, spec: PictureInPictureSpec?): PictureInPictureParams {
        val builder = PictureInPictureParams.Builder()
        if (spec != null) {
            builder.setAspectRatio(Rational(spec.aspect.width, spec.aspect.height))
            builder.setActions(listOf(micAction(context, spec)))
        } else {
            builder.setActions(emptyList())
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            builder.setAutoEnterEnabled(spec != null)
            // A video call's window is not resized seamlessly from the app's
            // layout; the system's cross-fade reads better.
            builder.setSeamlessResizeEnabled(false)
        }
        return builder.build()
    }

    private fun micAction(context: Context, spec: PictureInPictureSpec): RemoteAction {
        val label = pictureInPictureMicLabel(spec.micOn).render()
        val intent = Intent(context, CallPictureInPictureActionReceiver::class.java)
            .setAction(PIP_ACTION_SET_MIC)
            .putExtra(PIP_EXTRA_MIC_ON, !spec.micOn)
        val pending = PendingIntent.getBroadcast(
            context,
            // One request code per target state, so the system never hands
            // back a stale intent carrying the opposite request.
            if (spec.micOn) REQUEST_MUTE else REQUEST_UNMUTE,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        // Drawn on and off as the call screen draws it: a muted microphone is
        // the slashed one. The Lakko set is drawn in black to be tinted; the
        // system window's actions sit on a dark scrim, so the glyph is white.
        val icon = Icon.createWithResource(
            context,
            if (spec.micOn) R.drawable.ic_lk_mic_fill else R.drawable.ic_lk_mic_slash_fill,
        ).setTint(android.graphics.Color.WHITE)
        return RemoteAction(icon, label, label, pending).apply { isEnabled = spec.micEnabled }
    }

    private const val TAG = "lakko-call-pip"
    private const val REQUEST_MUTE = 4501
    private const val REQUEST_UNMUTE = 4502
}

/**
 * The system window's microphone action. Routed exactly as the floating
 * window's mute button: [AppCallHost.setCurrentCallMicrophoneEnabled].
 */
public class CallPictureInPictureActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != PIP_ACTION_SET_MIC) return
        if (!intent.hasExtra(PIP_EXTRA_MIC_ON)) return
        val on = intent.getBooleanExtra(PIP_EXTRA_MIC_ON, true)
        try {
            AppCallHost.setCurrentCallMicrophoneEnabled(on)
        } catch (cause: IllegalStateException) {
            Log.w("lakko-call-pip", "microphone action not applied: ${cause.javaClass.simpleName}")
        }
    }
}
