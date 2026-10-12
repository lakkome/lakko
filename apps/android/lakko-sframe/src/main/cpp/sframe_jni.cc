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

#include <android/log.h>
#include <link.h>

#include <atomic>
#include <cstdint>
#include <cstring>

#include "receiver-delay.h"
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

// Negotiated codec (ADR-0071). Tell the encryptor which codec its sender
// negotiated, as one of the `LAKKO_SFRAME_MEDIA_CODEC_*` values (0 other,
// 1 VP8, 2 Opus; any other value is stored and sealed as "other"). It
// decides how many leading bytes of each frame stay in the clear. Only
// the low 8 bits of `codec` are used; the Kotlin facade passes 0..=2. A
// null pointer is a no-op. An atomic store on the bridge side, so it is
// safe against a concurrent Encrypt on the codec thread.
JNIEXPORT void JNICALL
Java_me_lakko_sframe_LakkoFrameEncryptor_nativeSetCodec(
    JNIEnv* /*env*/,
    jobject /*thiz*/,
    jlong native_ptr,
    jint codec) {
    if (native_ptr == 0) {
        return;
    }
    auto* enc = reinterpret_cast<lakko::sframe::LakkoFrameEncryptor*>(native_ptr);
    enc->SetCodec(static_cast<uint8_t>(codec));
}

// Map a negotiated codec name ("VP8", "opus", "video/VP8", "audio/opus",
// any case) to its `LAKKO_SFRAME_MEDIA_CODEC_*` value; anything else and
// null map to 0 (other). The one mapping iOS uses too
// (`LakkoFrameEncryptor::CodecFromName`).
JNIEXPORT jint JNICALL
Java_me_lakko_sframe_LakkoFrameEncryptor_nativeCodecFromName(
    JNIEnv* env,
    jobject /*thiz*/,
    jstring name) {
    if (name == nullptr) {
        return static_cast<jint>(LAKKO_SFRAME_MEDIA_CODEC_OTHER);
    }
    const char* chars = env->GetStringUTFChars(name, /*isCopy=*/nullptr);
    if (chars == nullptr) {
        return static_cast<jint>(LAKKO_SFRAME_MEDIA_CODEC_OTHER);
    }
    const uint8_t codec = lakko::sframe::LakkoFrameEncryptor::CodecFromName(chars);
    env->ReleaseStringUTFChars(name, chars);
    return static_cast<jint>(codec);
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

// ---------------------------------------------------------------
// ReceiverPlayoutDelay (ADR-0070, note of 2026-10-12)
// ---------------------------------------------------------------
//
// A remote receiver's minimum playout delay, set through the engine's
// `webrtc::RtpReceiverInterface::SetJitterBufferMinimumDelay`, which
// `org.webrtc.RtpReceiver` does not bind. The call goes by vtable slot
// (`apps/shared/sframe-bridge/receiver-delay.h`, where the evidence for
// the slot, the slot encoding of each ABI and the calling convention is
// written down). That evidence was read from the machine code of
// `libjingle_peerconnection_so.so` of io.github.webrtc-sdk:android
// 125.6422.07, so nothing is called unless the library loaded in this
// process IS that binary, by its ELF build id.

namespace {

const char kDelayLogTag[] = "lakko.receiver-delay";
const char kEngineLibrarySuffix[] = "libjingle_peerconnection_so.so";

// The GNU build id of the slice of 125.6422.07 this library is built for.
// The two slices do not share a vtable encoding (arm64-v8a is built with
// relative vtables, x86_64 with absolute ones), so each is pinned by itself.
#if defined(__aarch64__)
const uint8_t kVerifiedBuildId[] = {0x7b, 0x8b, 0x82, 0x9c,
                                    0xed, 0x80, 0x02, 0x1d};
#elif defined(__x86_64__)
const uint8_t kVerifiedBuildId[] = {0x30, 0x68, 0x54, 0xeb,
                                    0xdf, 0x81, 0x49, 0x05};
#else
const uint8_t kVerifiedBuildId[] = {0};
#endif

constexpr std::size_t kMaxBuildIdLen = 64;

struct EngineBuildId {
    bool found = false;
    std::size_t len = 0;
    uint8_t bytes[kMaxBuildIdLen] = {};
};

bool EndsWith(const char* text, const char* suffix) {
    const std::size_t text_len = std::strlen(text);
    const std::size_t suffix_len = std::strlen(suffix);
    return text_len >= suffix_len &&
           std::strcmp(text + (text_len - suffix_len), suffix) == 0;
}

// Read the NT_GNU_BUILD_ID note of the loaded engine library out of its
// mapped PT_NOTE segments. Every length is checked against the segment.
int FindEngineBuildId(struct dl_phdr_info* info, std::size_t /*size*/,
                      void* data) {
    if (info->dlpi_name == nullptr ||
        !EndsWith(info->dlpi_name, kEngineLibrarySuffix)) {
        return 0;
    }
    auto* out = static_cast<EngineBuildId*>(data);
    for (int i = 0; i < info->dlpi_phnum; ++i) {
        const ElfW(Phdr)& segment = info->dlpi_phdr[i];
        if (segment.p_type != PT_NOTE) {
            continue;
        }
        const auto* cursor =
            reinterpret_cast<const uint8_t*>(info->dlpi_addr + segment.p_vaddr);
        std::size_t left = static_cast<std::size_t>(segment.p_memsz);
        while (left >= sizeof(ElfW(Nhdr))) {
            ElfW(Nhdr) note;
            std::memcpy(&note, cursor, sizeof(note));
            const std::size_t name_size =
                (static_cast<std::size_t>(note.n_namesz) + 3u) & ~std::size_t{3};
            const std::size_t desc_size =
                (static_cast<std::size_t>(note.n_descsz) + 3u) & ~std::size_t{3};
            if (name_size > left - sizeof(note) ||
                desc_size > left - sizeof(note) - name_size) {
                break;
            }
            const uint8_t* name = cursor + sizeof(note);
            const uint8_t* desc = name + name_size;
            if (note.n_type == NT_GNU_BUILD_ID && note.n_namesz == 4 &&
                std::memcmp(name, "GNU", 4) == 0 && note.n_descsz > 0 &&
                note.n_descsz <= kMaxBuildIdLen) {
                out->found = true;
                out->len = note.n_descsz;
                std::memcpy(out->bytes, desc, note.n_descsz);
                return 1;
            }
            cursor = desc + desc_size;
            left -= sizeof(note) + name_size + desc_size;
        }
    }
    // The engine library, with no build id: stop looking.
    return 1;
}

EngineBuildId LoadedEngineBuildId() {
    EngineBuildId id;
    dl_iterate_phdr(&FindEngineBuildId, &id);
    return id;
}

// 0: not decided. 1: the loaded engine is the verified binary. 2: it is not.
std::atomic<int> g_engine_verified{0};

bool EngineIsVerified() {
    int decided = g_engine_verified.load(std::memory_order_acquire);
    if (decided == 0) {
        const EngineBuildId id = LoadedEngineBuildId();
        const bool ok = lakko::receiver_delay::PlatformSupported() &&
                        id.found && id.len == sizeof(kVerifiedBuildId) &&
                        std::memcmp(id.bytes, kVerifiedBuildId, id.len) == 0;
        decided = ok ? 1 : 2;
        g_engine_verified.store(decided, std::memory_order_release);
        if (ok) {
            __android_log_print(
                ANDROID_LOG_INFO, kDelayLogTag,
                "setter available: the loaded libjingle_peerconnection_so.so "
                "is the verified binary of 125.6422.07");
        } else {
            __android_log_print(
                ANDROID_LOG_WARN, kDelayLogTag,
                "setter NOT available: the loaded "
                "libjingle_peerconnection_so.so is not the verified binary "
                "of 125.6422.07 (build id %s); the playout delay stays "
                "observe-only until the evidence is redone for this binary",
                id.found ? "differs" : "unreadable");
        }
    }
    return decided == 1;
}

void DelayLogSink(const char* line) {
    if (line != nullptr) {
        __android_log_print(ANDROID_LOG_INFO, kDelayLogTag, "%s", line);
    }
}

}  // namespace

extern "C" {

// The build id of the loaded engine library as lowercase hex, or null when
// the library is not loaded or carries none. For the log and the tests.
JNIEXPORT jstring JNICALL
Java_me_lakko_sframe_ReceiverPlayoutDelay_nativeEngineBuildId(
    JNIEnv* env,
    jobject /*thiz*/) {
    const EngineBuildId id = LoadedEngineBuildId();
    if (!id.found) {
        return nullptr;
    }
    static const char kHex[] = "0123456789abcdef";
    char text[2 * kMaxBuildIdLen + 1];
    for (std::size_t i = 0; i < id.len; ++i) {
        text[2 * i] = kHex[id.bytes[i] >> 4];
        text[2 * i + 1] = kHex[id.bytes[i] & 0x0f];
    }
    text[2 * id.len] = '\0';
    return env->NewStringUTF(text);
}

// Whether a set can be attempted in this process: the loaded engine is the
// verified binary and the probe has not failed.
JNIEXPORT jboolean JNICALL
Java_me_lakko_sframe_ReceiverPlayoutDelay_nativeIsAvailable(
    JNIEnv* /*env*/,
    jobject /*thiz*/) {
    return (EngineIsVerified() && !lakko::receiver_delay::ProbeHasFailed())
               ? JNI_TRUE
               : JNI_FALSE;
}

// Set the minimum playout delay of the engine receiver at
// `receiver_ptr` (what `org.webrtc.RtpReceiver.getNativeRtpReceiver()`
// returns: a raw `webrtc::RtpReceiverInterface*` the Java object holds a
// reference on) to `delay_ms`; 0 gives it back to the engine.
// `media_type` is 0 for an audio receiver and 1 for a video one;
// `decryptor_ptr` is the native pointer of the `LakkoFrameDecryptor`
// attached to this receiver, or 0. Returns
// `lakko::receiver_delay::Status` as an int.
//
// Blocks until the engine's worker thread has run the call. The caller
// keeps the Java `RtpReceiver` (and the decryptor) undisposed for the call.
JNIEXPORT jint JNICALL
Java_me_lakko_sframe_ReceiverPlayoutDelay_nativeSet(
    JNIEnv* /*env*/,
    jobject /*thiz*/,
    jlong receiver_ptr,
    jint delay_ms,
    jint media_type,
    jlong decryptor_ptr) {
    using lakko::receiver_delay::Status;
    if (lakko::receiver_delay::ProbeHasFailed()) {
        return static_cast<jint>(Status::kProbeFailed);
    }
    if (receiver_ptr == 0 || delay_ms < 0 || !EngineIsVerified() ||
        (media_type != lakko::receiver_delay::kMediaTypeAudio &&
         media_type != lakko::receiver_delay::kMediaTypeVideo)) {
        return static_cast<jint>(Status::kNotAvailable);
    }
    webrtc::FrameDecryptorInterface* decryptor =
        decryptor_ptr == 0
            ? nullptr
            : reinterpret_cast<lakko::sframe::LakkoFrameDecryptor*>(
                  decryptor_ptr);
    const Status status = lakko::receiver_delay::SetMinimumPlayoutDelay(
        reinterpret_cast<void*>(receiver_ptr),
        static_cast<uint32_t>(delay_ms), static_cast<int>(media_type),
        decryptor, &DelayLogSink);
    return static_cast<jint>(status);
}

}  // extern "C"
