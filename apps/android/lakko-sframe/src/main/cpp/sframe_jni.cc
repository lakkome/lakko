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
Java_me_lakko_sframe_LakkoFrameEncryptor_nativeCreate(
    JNIEnv* env,
    jobject /*thiz*/,
    jbyteArray seed,
    jint local_leaf,
    jint stream_index) {
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

    // `stream_index` (ADR-0035) is the local send track's per-sender
    // stream ordinal (0 = audio, 1 = first video, 2 = screen-share …).
    // The seal KID is composed `(local_leaf << 8) | stream_index`, so
    // every one of a participant's concurrent streams keys into a
    // disjoint `(key, nonce_salt)` space. Every `uint8_t` is a valid
    // stream index — the C++ Create never rejects on this argument.
    lakko::sframe::LakkoFrameEncryptor* enc =
        lakko::sframe::LakkoFrameEncryptor::Create(
            reinterpret_cast<const uint8_t*>(seed_bytes),
            static_cast<std::size_t>(seed_len),
            static_cast<uint32_t>(local_leaf),
            static_cast<uint8_t>(stream_index));

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
Java_me_lakko_sframe_LakkoFrameEncryptor_nativeRelease(
    JNIEnv* /*env*/,
    jobject /*thiz*/,
    jlong native_ptr) {
    if (native_ptr == 0) {
        return;
    }
    auto* enc = reinterpret_cast<lakko::sframe::LakkoFrameEncryptor*>(native_ptr);
    enc->Release();
}

// Mid-call rekey (G-18, ADR-0029). Swap the encryptor's seal key to the
// new MLS epoch's `seed`, keeping the same local leaf. Returns JNI_FALSE
// on a bad seed length / HKDF failure or a null pointer — the C++ side
// then retains the current key (the caller keeps sealing under it, never
// plaintext, invariant #4). Thread-safe against a concurrent Encrypt on
// the codec thread (the bridge guards the seal key with a mutex).
JNIEXPORT jboolean JNICALL
Java_me_lakko_sframe_LakkoFrameEncryptor_nativeRekey(
    JNIEnv* env,
    jobject /*thiz*/,
    jlong native_ptr,
    jbyteArray seed) {
    if (native_ptr == 0 || seed == nullptr) {
        return JNI_FALSE;
    }
    const jsize seed_len = env->GetArrayLength(seed);
    if (seed_len <= 0) {
        return JNI_FALSE;
    }
    jbyte* seed_bytes = env->GetByteArrayElements(seed, /*isCopy=*/nullptr);
    if (seed_bytes == nullptr) {
        return JNI_FALSE;
    }
    auto* enc = reinterpret_cast<lakko::sframe::LakkoFrameEncryptor*>(native_ptr);
    const bool ok = enc->Rekey(
        reinterpret_cast<const uint8_t*>(seed_bytes),
        static_cast<std::size_t>(seed_len));
    env->ReleaseByteArrayElements(seed, seed_bytes, JNI_ABORT);
    return ok ? JNI_TRUE : JNI_FALSE;
}

// ---------------------------------------------------------------
// LakkoFrameDecryptor
// ---------------------------------------------------------------

JNIEXPORT jlong JNICALL
Java_me_lakko_sframe_LakkoFrameDecryptor_nativeCreate(
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

    // The decryptor takes NO stream argument (ADR-0035): it opens each
    // frame through the call-key ring, which reads the composite KID
    // `(sender_leaf << 8) | stream_index` from the frame's own SFrame
    // header and derives that sender+stream's key internally. One
    // decryptor bound to a remote receive track opens audio and every
    // video stream on it with no local media-type or MID signal.
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
Java_me_lakko_sframe_LakkoFrameDecryptor_nativeRelease(
    JNIEnv* /*env*/,
    jobject /*thiz*/,
    jlong native_ptr) {
    if (native_ptr == 0) {
        return;
    }
    auto* dec = reinterpret_cast<lakko::sframe::LakkoFrameDecryptor*>(native_ptr);
    dec->Release();
}

// Mid-call rekey (G-18, ADR-0029). Rotate the decryptor's call-key ring
// to the new MLS epoch's `seed`, retaining the previous epoch's key until
// `now_ms + clamp(overlap_ms)` so in-flight frames sealed under the old
// epoch still open. Returns JNI_FALSE on a bad seed length / null pointer
// (the ring keeps its current + previous keys). Thread-safe (the Rust
// ring is Mutex-guarded internally).
JNIEXPORT jboolean JNICALL
Java_me_lakko_sframe_LakkoFrameDecryptor_nativeRekey(
    JNIEnv* env,
    jobject /*thiz*/,
    jlong native_ptr,
    jbyteArray seed,
    jlong now_ms,
    jlong overlap_ms) {
    if (native_ptr == 0 || seed == nullptr) {
        return JNI_FALSE;
    }
    const jsize seed_len = env->GetArrayLength(seed);
    if (seed_len <= 0) {
        return JNI_FALSE;
    }
    jbyte* seed_bytes = env->GetByteArrayElements(seed, /*isCopy=*/nullptr);
    if (seed_bytes == nullptr) {
        return JNI_FALSE;
    }
    auto* dec = reinterpret_cast<lakko::sframe::LakkoFrameDecryptor*>(native_ptr);
    const bool ok = dec->Rekey(
        reinterpret_cast<const uint8_t*>(seed_bytes),
        static_cast<std::size_t>(seed_len),
        static_cast<uint64_t>(now_ms),
        static_cast<uint64_t>(overlap_ms));
    env->ReleaseByteArrayElements(seed, seed_bytes, JNI_ABORT);
    return ok ? JNI_TRUE : JNI_FALSE;
}

// Close the overlap window (G-18, ADR-0029): drop the retained previous
// key once it has expired at `now_ms`, restoring seed-boundary forward
// secrecy. A null pointer is a no-op.
JNIEXPORT void JNICALL
Java_me_lakko_sframe_LakkoFrameDecryptor_nativePrune(
    JNIEnv* /*env*/,
    jobject /*thiz*/,
    jlong native_ptr,
    jlong now_ms) {
    if (native_ptr == 0) {
        return;
    }
    auto* dec = reinterpret_cast<lakko::sframe::LakkoFrameDecryptor*>(native_ptr);
    dec->Prune(static_cast<uint64_t>(now_ms));
}

}  // extern "C"
