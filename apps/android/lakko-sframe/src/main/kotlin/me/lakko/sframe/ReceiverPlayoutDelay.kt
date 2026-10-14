// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.sframe

import android.util.Log
import org.webrtc.LakkoRtpReceiverAccess
import org.webrtc.RtpReceiver

/**
 * Sets a remote receiver's minimum playout delay through the engine's C++
 * `webrtc::RtpReceiverInterface::SetJitterBufferMinimumDelay`, which
 * `org.webrtc.RtpReceiver` does not bind (ADR-0070, note of 2026-10-12).
 *
 * The engine object is reached through the SDK's own package-private
 * `getNativeRtpReceiver()` ([LakkoRtpReceiverAccess]) and called by vtable
 * slot in `libsframe_jni.so` (`apps/shared/sframe-bridge/receiver-delay.h`,
 * where the evidence for the slot, each ABI's slot encoding and the calling
 * convention is written down). That evidence was read from the machine code
 * of the SDK version [VERIFIED_SDK_VERSION], so the native side calls nothing
 * unless the engine library loaded in this process is that binary, by its
 * ELF build id; for any other binary [isAvailable] is `false` and Lakko
 * measures without setting.
 */
public object ReceiverPlayoutDelay {
    private const val TAG = "lakko.receiver-delay"

    /**
     * The `io.github.webrtc-sdk:android` version whose binaries the evidence
     * was gathered for. The app's host tests pin the version catalog to it,
     * so a dependency bump fails until the evidence is redone and the new
     * binaries' build ids are pinned in `sframe_jni.cc`.
     */
    public const val VERIFIED_SDK_VERSION: String = "125.6422.07"

    /** The delay was handed to the engine. */
    public const val STATUS_OK: Int = 0

    /** Nothing was set on this receiver: the setter is not available in this
     *  process, the receiver is disposed, it has no Lakko decryptor to run
     *  the probe against yet, or the decryptor passed for it is not the one
     *  the engine holds for it (the caller's record of the receiver is
     *  stale). Says nothing about the engine; other receivers are
     *  unaffected. */
    public const val STATUS_NOT_AVAILABLE: Int = 1

    /** The engine answered a probe question with something that is not
     *  Lakko's (the wrong media kind, or a decryptor Lakko did not create),
     *  so its layout cannot be trusted. Nothing is set for the rest of the
     *  process. */
    public const val STATUS_PROBE_FAILED: Int = 2

    private val loaded: Boolean by lazy {
        try {
            System.loadLibrary("sframe_jni")
            true
        } catch (cause: UnsatisfiedLinkError) {
            Log.w(TAG, "libsframe_jni.so did not load: ${cause.message}")
            false
        }
    }

    /**
     * `true` when a set can be attempted in this process: the native library
     * loaded, the engine library is loaded and is the verified binary, and
     * the probe has not failed. A failed probe, or a loaded engine library
     * that is another binary, turns the answer to `false` for the rest of
     * the process; asked before the engine library is loaded it is `false`
     * for that call only.
     */
    @JvmStatic
    public val isAvailable: Boolean
        get() = try {
            loaded && nativeIsAvailable()
        } catch (cause: UnsatisfiedLinkError) {
            false
        }

    /** The ELF build id of the loaded engine library as lowercase hex, or
     *  `null` when it cannot be read. For the log. */
    @JvmStatic
    public fun engineBuildId(): String? = try {
        if (loaded) nativeEngineBuildId() else null
    } catch (cause: UnsatisfiedLinkError) {
        null
    }

    /**
     * Set the minimum playout delay of [receiver] to [delayMs] milliseconds;
     * `0` gives the delay back to the engine. [audio] is what the receiver's
     * track is; [decryptor] is the SFrame decryptor attached to this very
     * receiver, needed at least for the first set of the process (the probe
     * asks the engine for it and compares).
     *
     * Blocks until the engine's worker thread has run the call (one or two
     * round trips to it), so it must never run on the main thread nor on a
     * libwebrtc thread: the app runs it on its one playout-delay thread
     * (`SerialReceiverDelayWrites`). The caller must keep [receiver]
     * undisposed for the call, under a lock that excludes the disposal.
     *
     * Returns one of [STATUS_OK], [STATUS_NOT_AVAILABLE],
     * [STATUS_PROBE_FAILED].
     */
    @JvmStatic
    public fun set(
        receiver: RtpReceiver,
        decryptor: LakkoFrameDecryptor?,
        audio: Boolean,
        delayMs: Int,
    ): Int {
        if (delayMs < 0 || !isAvailable) return STATUS_NOT_AVAILABLE
        val receiverPtr = try {
            LakkoRtpReceiverAccess.nativePointer(receiver)
        } catch (cause: IllegalStateException) {
            // Disposed: there is no engine object to call.
            return STATUS_NOT_AVAILABLE
        }
        if (receiverPtr == 0L) return STATUS_NOT_AVAILABLE
        val decryptorPtr = decryptor?.getNativeFrameDecryptor() ?: 0L
        return try {
            nativeSet(receiverPtr, delayMs, if (audio) 0 else 1, decryptorPtr)
        } catch (cause: UnsatisfiedLinkError) {
            STATUS_NOT_AVAILABLE
        }
    }

    @JvmStatic
    private external fun nativeIsAvailable(): Boolean

    @JvmStatic
    private external fun nativeEngineBuildId(): String?

    @JvmStatic
    private external fun nativeSet(
        receiverPtr: Long,
        delayMs: Int,
        mediaType: Int,
        decryptorPtr: Long,
    ): Int
}
