// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.sframe

import org.webrtc.FrameEncryptor

/**
 * SFrame encryptor for outgoing RTP frames on a single
 * RTCRtpSender (G3-sframe-native N-5).
 *
 * Implements `org.webrtc.FrameEncryptor` — libwebrtc's Java-
 * side glue reads `getNativeFrameEncryptor()` and hands the
 * `long` (raw `void*`) to the C++ side, which wraps it in
 * `rtc::scoped_refptr<webrtc::FrameEncryptorInterface>`.
 * The pointer is owned by a `LakkoFrameEncryptor` C++ object
 * (apps/shared/sframe-bridge/sframe-bridge.cc N-3) that
 * trampolines through the lakko-sframe-native C ABI (N-1) for
 * every AEAD seal + RFC 9605 wire-header pack.
 *
 * Construct via [LakkoFrameEncryptor.create] — the constructor
 * is private so the only path into the class goes through the
 * boot-probe gate inside [create]. Returns `null` on released
 * call keys, HKDF failure, or if [SFrameAvailability] reports
 * calling is unavailable on this build.
 *
 * The encryptor is built on the call's shared [LakkoCallKeys]
 * and has no rekey of its own: a mid-call rekey on the call keys
 * (G-18, ADR-0029) reaches it on its next frame.
 *
 * The native pointer is freed by [release]; the caller MUST
 * call it once after detaching the encryptor from the sender.
 * libwebrtc's Java-side glue calls `getNativeFrameEncryptor()`
 * multiple times during the lifetime of an attached sender;
 * releasing the pointer while the sender is still attached
 * causes a use-after-free on the codec thread.
 */
public class LakkoFrameEncryptor private constructor(
    @get:JvmName("getNativeFrameEncryptorPointer")
    private var nativePtr: Long,
    localLeaf: Int,
    streamIndex: Int,
) : FrameEncryptor {

    /**
     * The KID this encryptor stamps on every frame it seals:
     * `(localLeaf << 8) | streamIndex` (ADR-0035).
     *
     * Carries no key material — it is the routing identifier that travels in
     * the clear in every RFC 9605 frame header, which is precisely why it is
     * safe to log and why the peer can derive the matching key from it without
     * being told.
     */
    public val sealKid: Long = (localLeaf.toLong() shl 8) or (streamIndex.toLong() and 0xFF)

    /** The leaf this encryptor was armed for. */
    public val leaf: Int = localLeaf

    /** The stream ordinal this encryptor was armed for (0 audio, 1 video). */
    public val stream: Int = streamIndex

    /**
     * libwebrtc has fetched the native pointer, i.e. this encryptor reached the
     * sender's media channel rather than merely having had a setter called on
     * it.
     *
     * The distinction matters: `setFrameEncryptor` succeeding proves nothing
     * about whether frames are actually being sealed, and a mid-call camera-on
     * whose encryptor never entered the frame path looks — from both ends —
     * exactly like one whose key is wrong. This is the strongest signal
     * available without a native callback on the first seal.
     */
    @Volatile
    public var pointerTaken: Boolean = false
        private set

    /**
     * libwebrtc's standard Java surface — returns the raw
     * native pointer the C++ side casts to
     * `webrtc::FrameEncryptorInterface*`.
     *
     * Returns 0 (libwebrtc's sentinel for "no encryptor") if
     * [release] has already been called.
     */
    override fun getNativeFrameEncryptor(): Long {
        if (nativePtr != 0L) pointerTaken = true
        return nativePtr
    }

    /**
     * Drop the native pointer. Idempotent; calling twice is a
     * no-op on the second call. Must run AFTER the
     * RTCRtpSender has been told to detach via
     * `sender.setFrameEncryptor(null)` — otherwise libwebrtc's
     * codec thread can still hold a live reference to the
     * pointer we're about to free.
     */
    public fun release() {
        val ptr = nativePtr
        if (ptr != 0L) {
            nativePtr = 0L
            nativeRelease(ptr)
        }
    }

    /**
     * The codec last given to [setCodec] ([SFrameMediaCodec.OTHER] until
     * the first call). Kept on the Kotlin side for logs and tests; the
     * native encryptor holds its own copy.
     */
    @Volatile
    public var codec: SFrameMediaCodec = SFrameMediaCodec.OTHER
        private set

    /**
     * Tell the encryptor which codec its sender negotiated (ADR-0071).
     * libwebrtc hands the native encryptor a media type and no codec, so
     * the caller reads it from the sender (the first entry of
     * `sender.parameters.codecs` once the answer is applied) and calls
     * this after every negotiation that could change it. A rekey of the
     * call keys keeps the codec; a fresh encryptor starts at [SFrameMediaCodec.OTHER].
     *
     * It decides how many leading bytes of each encoded frame stay in the
     * clear, so that a receiver without the generic frame descriptor
     * still finds key frames: 10 of a VP8 key frame, 3 of any other VP8
     * frame, none for Opus and every other codec. Until it is called a
     * video encryptor seals with nothing in the clear: the frames still
     * open, but such a receiver cannot decode them. Frames libwebrtc
     * hands over as audio keep nothing clear whatever is set here.
     *
     * Safe to call from any thread while media is flowing. Returns
     * `false` only if [release] already ran (nothing was changed).
     */
    public fun setCodec(codec: SFrameMediaCodec): Boolean {
        val ptr = nativePtr
        if (ptr == 0L) return false
        nativeSetCodec(ptr, codec.wire)
        this.codec = codec
        return true
    }

    /**
     * [setCodec] taking the codec by name: `RtpParameters.Codec.name`
     * ("VP8", "opus") or the MIME form ("video/VP8", "audio/opus"), any
     * case. Any other name, and `null`, is [SFrameMediaCodec.OTHER].
     */
    public fun setCodecName(name: String?): Boolean = setCodec(codecForName(name))

    @Suppress("ProtectedInFinal")
    protected fun finalize() {
        // Safety net for the case where the consumer forgot to
        // call release(). Native release is idempotent and
        // refcount-safe per N-3's documentation; this finalize
        // is just a defensive cleanup.
        release()
    }

    public companion object {
        init {
            // System.loadLibrary is idempotent — calling it
            // here and again from LakkoFrameDecryptor's
            // companion init both work; the runtime no-ops the
            // second invocation. Naming `sframe_jni` matches
            // `libsframe_jni.so` shipped under
            // `src/main/jniLibs/{abi}/`.
            System.loadLibrary("sframe_jni")
        }

        /**
         * Build a fresh encryptor on the call's shared [keys]
         * (built from the per-call MLS exporter seed,
         * `Group.export_secret("lakko/call/v1", call_id, 32)`) +
         * this device's leaf index + the local send track's
         * [streamIndex] (ADR-0035 — the per-sender
         * stream ordinal: 0 = audio, 1 = first video, 2 = screen-
         * share …). The seal KID is composed `(localLeaf << 8) |
         * streamIndex`, so every one of a participant's concurrent
         * streams keys into a disjoint `(key, nonce_salt)` space; the
         * encryptor is bound to one stream for its lifetime. Every
         * `0..=255` value is a valid stream index, so there is no
         * fail-closed rejection on it. The encryptor holds its own
         * native reference on [keys]. Returns `null` on any failure
         * (released keys, HKDF failure, boot-probe negative).
         */
        @JvmStatic
        public fun create(
            keys: LakkoCallKeys,
            localLeaf: Int,
            streamIndex: Int,
        ): LakkoFrameEncryptor? {
            if (!SFrameAvailability.isCallingAvailable) return null
            val keysPtr = keys.pointer
            if (keysPtr == 0L) return null
            val ptr = nativeCreate(keysPtr, localLeaf, streamIndex)
            return if (ptr == 0L) {
                null
            } else {
                LakkoFrameEncryptor(ptr, localLeaf, streamIndex)
            }
        }

        @JvmStatic
        private external fun nativeCreate(
            keysPtr: Long,
            localLeaf: Int,
            streamIndex: Int,
        ): Long

        @JvmStatic
        private external fun nativeRelease(nativePtr: Long)

        /**
         * The codec [setCodecName] would set for [name]. The mapping is
         * the native bridge's (`LakkoFrameEncryptor::CodecFromName`), the
         * one iOS uses too.
         */
        @JvmStatic
        public fun codecForName(name: String?): SFrameMediaCodec =
            SFrameMediaCodec.fromWire(nativeCodecFromName(name))

        @JvmStatic
        private external fun nativeSetCodec(nativePtr: Long, codec: Int)

        @JvmStatic
        private external fun nativeCodecFromName(name: String?): Int
    }
}
