// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors
//
// JNI bridge for the camera background segmenter (ADR-0089): the Kotlin
// `me.lakko.sframe.NativeSegmenter` object -> the `lakko_segment_*` C ABI of
// crates/lakko-sframe-native (src/segment.rs) -> `lakko_segment`.
//
// Why hand-written JNI and not UniFFI: the camera pipeline hands over a
// small picture on every inference and judges every camera frame; the
// picture and the mask are direct ByteBuffers the Kotlin side keeps for the
// life of the camera, so no call copies or allocates. UniFFI would copy each
// picture into a fresh buffer.
//
// Threading: nativeNew / nativeFree on a control thread; nativeRun on the
// inference worker; nativeOnFrame / nativeState on the capture thread
// (the C ABI keeps the model and the health watch behind separate locks).
// The Kotlin side guarantees a handle is never freed while another call
// holds it.

#include <jni.h>

#include <cstdint>

// The header cbindgen emits is plain C (no `extern "C"` guard of its own):
// included bare in C++ its functions are looked up under C++-mangled names
// the Rust archive does not define, and `System.loadLibrary` then fails on
// the device with "cannot locate symbol", taking SFrame down with it.
extern "C" {
#include "lakko_sframe_native.h"
}

namespace {

lakko_segmenter_t* AsSegmenter(jlong handle) {
    return reinterpret_cast<lakko_segmenter_t*>(static_cast<intptr_t>(handle));
}

uint64_t AsMillis(jlong now_ms) {
    return now_ms < 0 ? 0 : static_cast<uint64_t>(now_ms);
}

}  // namespace

extern "C" {

JNIEXPORT jlong JNICALL
Java_me_lakko_sframe_NativeSegmenter_nativeNew(
    JNIEnv* /*env*/,
    jobject /*thiz*/,
    jlong now_ms) {
    return static_cast<jlong>(
        reinterpret_cast<intptr_t>(lakko_segment_new(AsMillis(now_ms))));
}

JNIEXPORT void JNICALL
Java_me_lakko_sframe_NativeSegmenter_nativeFree(
    JNIEnv* /*env*/,
    jobject /*thiz*/,
    jlong handle) {
    lakko_segment_free(AsSegmenter(handle));
}

// Returns a lakko_segment_state_t. A null handle, a buffer that is not
// direct, or an unknown layout answers FAILED without touching anything:
// fail closed, like a wrong-size buffer inside the C ABI.
JNIEXPORT jint JNICALL
Java_me_lakko_sframe_NativeSegmenter_nativeRun(
    JNIEnv* env,
    jobject /*thiz*/,
    jlong handle,
    jobject pixels,
    jint pixels_len,
    jint layout,
    jlong now_ms,
    jobject mask) {
    if (handle == 0 || pixels == nullptr || mask == nullptr || pixels_len < 0) {
        return LAKKO_SEGMENT_STATE_T_FAILED;
    }
    if (layout != LAKKO_SEGMENT_LAYOUT_T_RGBA &&
        layout != LAKKO_SEGMENT_LAYOUT_T_BGRA &&
        layout != LAKKO_SEGMENT_LAYOUT_T_RGB) {
        return LAKKO_SEGMENT_STATE_T_FAILED;
    }
    void* in = env->GetDirectBufferAddress(pixels);
    const jlong in_cap = env->GetDirectBufferCapacity(pixels);
    void* out = env->GetDirectBufferAddress(mask);
    const jlong out_cap = env->GetDirectBufferCapacity(mask);
    if (in == nullptr || out == nullptr || in_cap < pixels_len || out_cap < 0) {
        return LAKKO_SEGMENT_STATE_T_FAILED;
    }
    return lakko_segment_run(
        AsSegmenter(handle),
        static_cast<const uint8_t*>(in),
        static_cast<uintptr_t>(pixels_len),
        static_cast<lakko_segment_layout_t>(layout),
        AsMillis(now_ms),
        static_cast<uint8_t*>(out),
        static_cast<uintptr_t>(out_cap));
}

JNIEXPORT jint JNICALL
Java_me_lakko_sframe_NativeSegmenter_nativeOnFrame(
    JNIEnv* /*env*/,
    jobject /*thiz*/,
    jlong handle,
    jlong now_ms) {
    return lakko_segment_on_frame(AsSegmenter(handle), AsMillis(now_ms));
}

JNIEXPORT jint JNICALL
Java_me_lakko_sframe_NativeSegmenter_nativeRestart(
    JNIEnv* /*env*/,
    jobject /*thiz*/,
    jlong handle,
    jlong now_ms) {
    return lakko_segment_restart(AsSegmenter(handle), AsMillis(now_ms));
}

JNIEXPORT jint JNICALL
Java_me_lakko_sframe_NativeSegmenter_nativeState(
    JNIEnv* /*env*/,
    jobject /*thiz*/,
    jlong handle) {
    return lakko_segment_state(AsSegmenter(handle));
}

// Returns a lakko_frame_action_t. A state outside the enum is judged as
// FAILED, so an unexpected value never sends the raw camera.
JNIEXPORT jint JNICALL
Java_me_lakko_sframe_NativeSegmenter_nativeFrameAction(
    JNIEnv* /*env*/,
    jobject /*thiz*/,
    jboolean selected,
    jint state) {
    lakko_segment_state_t s = LAKKO_SEGMENT_STATE_T_FAILED;
    if (state == LAKKO_SEGMENT_STATE_T_STARTING) {
        s = LAKKO_SEGMENT_STATE_T_STARTING;
    } else if (state == LAKKO_SEGMENT_STATE_T_WORKING) {
        s = LAKKO_SEGMENT_STATE_T_WORKING;
    }
    return lakko_segment_frame_action(selected == JNI_TRUE, s);
}

JNIEXPORT jint JNICALL
Java_me_lakko_sframe_NativeSegmenter_nativeInputWidth(
    JNIEnv* /*env*/,
    jobject /*thiz*/) {
    return static_cast<jint>(LAKKO_SEGMENT_INPUT_WIDTH);
}

JNIEXPORT jint JNICALL
Java_me_lakko_sframe_NativeSegmenter_nativeInputHeight(
    JNIEnv* /*env*/,
    jobject /*thiz*/) {
    return static_cast<jint>(LAKKO_SEGMENT_INPUT_HEIGHT);
}

}  // extern "C"
