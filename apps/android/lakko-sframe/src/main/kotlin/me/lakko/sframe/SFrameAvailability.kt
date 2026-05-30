// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.sframe

import android.util.Log
import java.util.concurrent.atomic.AtomicReference

/**
 * Process-wide gate for SFrame calling availability — Android
 * mirror of the iOS `SFrameAvailability` (G3-sframe-native N-5).
 *
 * Runs a one-shot capability probe checking that the libwebrtc
 * binary the io.github.webrtc-sdk:android 125.6422.07 AAR
 * ships exposes the public `setFrameEncryptor(FrameEncryptor)`
 * and `setFrameDecryptor(FrameDecryptor)` methods on
 * `org.webrtc.RtpSender` / `org.webrtc.RtpReceiver`.
 *
 * If the probe returns `false`, the app MUST disable every
 * call entry point + surface a "calling unavailable on this
 * build" banner. There is no silent fallback to non-E2EE
 * media — security invariant #4 in `CLAUDE.md` forbids it.
 *
 * Idempotent — repeated calls to [bootProbe] re-use the cached
 * result.
 */
public object SFrameAvailability {
    private const val TAG = "lakko.sframe"

    private val cached: AtomicReference<Boolean?> = AtomicReference(null)

    /**
     * `true` only after [bootProbe] has run AND it returned
     * positive. Fail-closed: any state other than confirmed-OK
     * reports `false`, including the pre-probe state — callers
     * who skip [bootProbe] cannot accidentally let the call
     * code path through.
     */
    public val isCallingAvailable: Boolean
        get() = cached.get() == true

    /**
     * One-shot boot probe. Run from `MainActivity.onCreate`
     * (or any Application-class init) before any call surface
     * renders. Returns the cached result on repeated calls.
     *
     * Logs `[lakko.sframe]`-tagged warning at WARN level on
     * failure so the audit pack can grep TestFlight / Crash
     * lytics output for the same prefix as iOS.
     */
    @JvmStatic
    public fun bootProbe(): Boolean {
        cached.get()?.let { return it }

        val ok = try {
            val rtpSenderClass = Class.forName("org.webrtc.RtpSender")
            val rtpReceiverClass = Class.forName("org.webrtc.RtpReceiver")
            val frameEncryptorClass = Class.forName("org.webrtc.FrameEncryptor")
            val frameDecryptorClass = Class.forName("org.webrtc.FrameDecryptor")

            // Both setters must accept the FrameEncryptor /
            // FrameDecryptor interface parameter that lakko-
            // sframe-android implements via the long-pointer
            // pattern. A milestone bump that renames either
            // method surfaces here as a NoSuchMethodException
            // — the probe goes negative + the call UI refuses.
            val setEnc = rtpSenderClass.getMethod(
                "setFrameEncryptor",
                frameEncryptorClass,
            )
            val setDec = rtpReceiverClass.getMethod(
                "setFrameDecryptor",
                frameDecryptorClass,
            )
            setEnc != null && setDec != null
        } catch (e: ClassNotFoundException) {
            Log.w(TAG, "bootProbe: webrtc class missing: ${e.message}")
            false
        } catch (e: NoSuchMethodException) {
            Log.w(
                TAG,
                "bootProbe: setFrameEncryptor / setFrameDecryptor " +
                    "missing on RtpSender/RtpReceiver: ${e.message} — " +
                    "calling will be disabled per security " +
                    "invariant #4 (no fallback to non-E2EE).",
            )
            false
        }

        // CAS so two threads racing on first call agree on the
        // same result. Whoever wins, both return the same value.
        cached.compareAndSet(null, ok)
        return cached.get() == true
    }
}
