// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors
//
// JNI bridge for the capture noise suppressor (ADR-0088): the Kotlin
// `me.lakko.sframe.NativeNoiseSuppressor` object -> the
// `lakko_noise_*` C ABI of crates/lakko-sframe-native (src/noise.rs) ->
// `lakko_noise::NoiseSuppressor` (RNNoise).
//
// Why hand-written JNI and not UniFFI: libwebrtc's capture post-processing
// hook (`org.webrtc.ExternalAudioProcessingFactory.AudioProcessing.process`)
// hands Kotlin a direct ByteBuffer over libwebrtc's own capture buffer every
// 10 ms, on the audio processing thread. `nativeProcess` takes that buffer's
// address and the suppressor cleans the samples in place: no copy, no
// allocation, no lock. UniFFI would copy the samples into a new buffer on
// every call.
//
// Threading: nativeCreate / nativeFree run on a control thread;
// nativeProcess / nativeProcessAtRate on libwebrtc's audio processing
// thread. The Kotlin side guarantees a handle is never freed while a
// process call holds it.

#include <jni.h>

#include <cstdint>

// The header cbindgen emits is plain C (no `extern "C"` guard of its own):
// included bare in C++ its functions are looked up under C++-mangled names
// the Rust archive does not define, and `System.loadLibrary` then fails on
// the device with "cannot locate symbol", taking SFrame down with it.
extern "C" {
#include "lakko_sframe_native.h"
}

extern "C" {

JNIEXPORT jlong JNICALL
Java_me_lakko_sframe_NativeNoiseSuppressor_nativeCreate(
    JNIEnv* /*env*/,
    jobject /*thiz*/) {
    return static_cast<jlong>(reinterpret_cast<intptr_t>(lakko_noise_new()));
}

JNIEXPORT void JNICALL
Java_me_lakko_sframe_NativeNoiseSuppressor_nativeFree(
    JNIEnv* /*env*/,
    jobject /*thiz*/,
    jlong handle) {
    lakko_noise_free(
        reinterpret_cast<lakko_noise_suppressor_t*>(static_cast<intptr_t>(handle)));
}

// Returns a lakko_noise_status_t, or 3 when the buffer is not a direct
// buffer of at least `frames` floats. Nothing is touched on a non-zero
// return.
JNIEXPORT jint JNICALL
Java_me_lakko_sframe_NativeNoiseSuppressor_nativeProcess(
    JNIEnv* env,
    jobject /*thiz*/,
    jlong handle,
    jobject buffer,
    jint frames) {
    if (handle == 0 || buffer == nullptr || frames < 0) {
        return LAKKO_NOISE_STATUS_T_NULL_POINTER;
    }
    void* address = env->GetDirectBufferAddress(buffer);
    const jlong capacity = env->GetDirectBufferCapacity(buffer);
    if (address == nullptr || capacity < 0 ||
        static_cast<uint64_t>(capacity) <
            static_cast<uint64_t>(frames) * sizeof(float) ||
        (reinterpret_cast<uintptr_t>(address) % alignof(float)) != 0) {
        return 3;
    }
    return lakko_noise_process_s16_in_place(
        reinterpret_cast<lakko_noise_suppressor_t*>(static_cast<intptr_t>(handle)),
        static_cast<float*>(address),
        static_cast<size_t>(frames));
}

// The capture rates nativeProcessAtRate takes: 48000, or a rate 48000 is a
// whole multiple of (24000, 16000, 12000, 8000).
JNIEXPORT jboolean JNICALL
Java_me_lakko_sframe_NativeNoiseSuppressor_nativeSupportsRate(
    JNIEnv* /*env*/,
    jobject /*thiz*/,
    jint rate_hz) {
    if (rate_hz <= 0) {
        return JNI_FALSE;
    }
    return lakko_noise_supports_rate(static_cast<uint32_t>(rate_hz)) ? JNI_TRUE
                                                                     : JNI_FALSE;
}

// nativeProcess for a mono stream at `rate_hz` (ADR-0088 amendment of
// 2026-11-10): a rate below 48 kHz is raised to 48 kHz, cleaned and lowered
// back in place. Returns a lakko_noise_status_t (4 = rate not supported), or
// 3 when the buffer is not a direct buffer of at least `frames` floats.
// Nothing is touched on a non-zero return.
JNIEXPORT jint JNICALL
Java_me_lakko_sframe_NativeNoiseSuppressor_nativeProcessAtRate(
    JNIEnv* env,
    jobject /*thiz*/,
    jlong handle,
    jint rate_hz,
    jobject buffer,
    jint frames) {
    if (handle == 0 || buffer == nullptr || frames < 0) {
        return LAKKO_NOISE_STATUS_T_NULL_POINTER;
    }
    if (rate_hz <= 0) {
        return LAKKO_NOISE_STATUS_T_UNSUPPORTED_RATE;
    }
    void* address = env->GetDirectBufferAddress(buffer);
    const jlong capacity = env->GetDirectBufferCapacity(buffer);
    if (address == nullptr || capacity < 0 ||
        static_cast<uint64_t>(capacity) <
            static_cast<uint64_t>(frames) * sizeof(float) ||
        (reinterpret_cast<uintptr_t>(address) % alignof(float)) != 0) {
        return 3;
    }
    return lakko_noise_process_s16_in_place_at_rate(
        reinterpret_cast<lakko_noise_suppressor_t*>(static_cast<intptr_t>(handle)),
        static_cast<uint32_t>(rate_hz),
        static_cast<float*>(address),
        static_cast<size_t>(frames));
}

JNIEXPORT jint JNICALL
Java_me_lakko_sframe_NativeNoiseSuppressor_nativeSampleRate(
    JNIEnv* /*env*/,
    jobject /*thiz*/) {
    return static_cast<jint>(LAKKO_NOISE_SAMPLE_RATE);
}

}  // extern "C"
