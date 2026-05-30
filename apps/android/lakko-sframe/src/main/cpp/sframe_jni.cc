// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors
//
// JNI bridge between the Kotlin LakkoFrameEncryptor /
// LakkoFrameDecryptor classes and the C++ N-3 bridge layer
// (apps/shared/sframe-bridge/sframe-bridge.{h,cc}).
// G3-sframe-native N-5.
//
// What this file does:
//
//   1. Take the (seed, leaf) tuple from Kotlin via JNI, call
//      `lakko::sframe::LakkoFrameEncryptor::Create(...)` which
//      builds the C++ subclass of `webrtc::FrameEncryptor
//      Interface` and trampolines through the
//      lakko-sframe-native (N-1) C ABI for every AEAD seal +
//      RFC 9605 wire-header pack.
//   2. AddRef() before returning the pointer to Kotlin — the
//      Kotlin `LakkoFrameEncryptor` instance now holds one
//      refcount. libwebrtc's Java glue will internally
//      AddRef() again when the sender attaches; that's still
//      balanced by libwebrtc's own Release on detach.
//   3. Release() on the Kotlin side's `nativeRelease(ptr)` so
//      our refcount drops to 0 and `delete this` runs.
//
// Threading: libwebrtc calls
// FrameEncryptorInterface::Encrypt on the codec thread, and
// the per-encryptor Encrypt path is serialised on that
// thread. nativeCreate / nativeRelease run on the JVM thread
// the caller invokes them from. The two threads never touch
// the same `enc` pointer concurrently — Create returns before
// libwebrtc grabs the pointer, and Release happens after
// libwebrtc has already detached.

#include <jni.h>

#include <cstdint>

#include "sframe-bridge.h"

// SPDX: every JNI entry point below has the canonical
// `Java_<package>_<class>_<method>` symbol. The Kotlin side
// declares them via `external fun nativeCreate(...)` inside
// a companion object — Kotlin lowers companion-object
// `@JvmStatic external fun foo(): Long` to the same JNI
// symbol as a plain Java `static native long foo()`.

extern "C" {

// ---------------------------------------------------------------
// LakkoFrameEncryptor
// ---------------------------------------------------------------

JNIEXPORT jlong JNICALL
Java_me_lakko_sframe_LakkoFrameEncryptor_00024Companion_nativeCreate(
    JNIEnv* env,
    jobject /*thiz*/,
    jbyteArray seed,
    jint local_leaf) {
    if (seed == nullptr) {
        return 0;
    }
    const jsize seed_len = env->GetArrayLength(seed);
    if (seed_len <= 0) {
        return 0;
    }

    // GetByteArrayElements returns a pointer the JVM may have
    // pinned in place or copied. Either way we only hold it for
    // the duration of Create — N-3's `Create` copies the seed
    // bytes into the per-call state, so no reference outlives
    // this stack frame.
    jbyte* seed_bytes = env->GetByteArrayElements(seed, /*isCopy=*/nullptr);
    if (seed_bytes == nullptr) {
        return 0;
    }

    lakko::sframe::LakkoFrameEncryptor* enc =
        lakko::sframe::LakkoFrameEncryptor::Create(
            reinterpret_cast<const uint8_t*>(seed_bytes),
            static_cast<std::size_t>(seed_len),
            static_cast<uint32_t>(local_leaf));

    // JNI_ABORT skips copying back; we never modified the
    // bytes so the abort path is the right choice.
    env->ReleaseByteArrayElements(seed, seed_bytes, JNI_ABORT);

    if (enc == nullptr) {
        return 0;
    }
    // Refcount: 0 -> 1 (Kotlin side owns this retain).
    enc->AddRef();
    return reinterpret_cast<jlong>(enc);
}

JNIEXPORT void JNICALL
Java_me_lakko_sframe_LakkoFrameEncryptor_00024Companion_nativeRelease(
    JNIEnv* /*env*/,
    jobject /*thiz*/,
    jlong native_ptr) {
    if (native_ptr == 0) {
        return;
    }
    auto* enc = reinterpret_cast<lakko::sframe::LakkoFrameEncryptor*>(native_ptr);
    enc->Release();
}

// ---------------------------------------------------------------
// LakkoFrameDecryptor
// ---------------------------------------------------------------

JNIEXPORT jlong JNICALL
Java_me_lakko_sframe_LakkoFrameDecryptor_00024Companion_nativeCreate(
    JNIEnv* env,
    jobject /*thiz*/,
    jbyteArray seed) {
    if (seed == nullptr) {
        return 0;
    }
    const jsize seed_len = env->GetArrayLength(seed);
    if (seed_len <= 0) {
        return 0;
    }
    jbyte* seed_bytes = env->GetByteArrayElements(seed, /*isCopy=*/nullptr);
    if (seed_bytes == nullptr) {
        return 0;
    }

    lakko::sframe::LakkoFrameDecryptor* dec =
        lakko::sframe::LakkoFrameDecryptor::Create(
            reinterpret_cast<const uint8_t*>(seed_bytes),
            static_cast<std::size_t>(seed_len));

    env->ReleaseByteArrayElements(seed, seed_bytes, JNI_ABORT);

    if (dec == nullptr) {
        return 0;
    }
    dec->AddRef();
    return reinterpret_cast<jlong>(dec);
}

JNIEXPORT void JNICALL
Java_me_lakko_sframe_LakkoFrameDecryptor_00024Companion_nativeRelease(
    JNIEnv* /*env*/,
    jobject /*thiz*/,
    jlong native_ptr) {
    if (native_ptr == 0) {
        return;
    }
    auto* dec = reinterpret_cast<lakko::sframe::LakkoFrameDecryptor*>(native_ptr);
    dec->Release();
}

}  // extern "C"
