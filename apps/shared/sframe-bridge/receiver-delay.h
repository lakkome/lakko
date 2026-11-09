// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// Setting a remote receiver's minimum playout delay through libwebrtc's C++
// object, which neither public binding wraps (ADR-0070, note of 2026-10-12).
//
// `webrtc::RtpReceiverInterface::SetJitterBufferMinimumDelay(
// absl::optional<double> delay_seconds)` is a virtual of the engine object
// behind `RTCRtpReceiver` (iOS) and `org.webrtc.RtpReceiver` (Android). This
// header reaches it by vtable slot, not through a class declaration: the
// engine's own header pulls in a closure of several dozen headers Lakko does
// not vendor, and, decisively, the binaries do not share one vtable encoding
// (see "What was read from the binaries"). Everything this file assumes is
// written down as a constant with the evidence beside it.
//
// Header-only on purpose: the iOS target and the Android JNI library each
// compile it into the one translation unit that uses it, so no build recipe
// has to learn a new source file.
//
// ## What was read from the binaries
//
// Source of the class: `api/rtp_receiver_interface.h` at
// `branch-heads/6422` commit 8505a9838ea91c66c96c173d30cd66f9dbcc7548
// (sha256 c39e06bf6ef9220fa82d90bfb168248ec042e6b61309994d8fc8f18133a4fb58).
// stasel/WebRTC 125.0.0 is built from exactly that commit, unmodified. The
// webrtc-sdk fork (`m125_release`, the source of
// io.github.webrtc-sdk:android:125.6422.07) has never touched that file nor
// `api/ref_count.h`: both are byte-identical to upstream.
//
// Virtuals in declaration order (Itanium ABI: one slot each, two for the
// virtual destructor):
//
//    0 AddRef                  1 Release                 2,3 destructor
//    4 track                   5 dtls_transport          6 stream_ids
//    7 streams                 8 media_type              9 id
//   10 GetParameters          11 SetParameters          12 SetObserver
//   13 SetJitterBufferMinimumDelay                      14 GetSources
//   15 SetFrameDecryptor      16 GetFrameDecryptor
//   17 SetDepacketizerToDecoderFrameTransformer
//
// Read from the machine code of every slice Lakko ships (not reasoned):
//
//   * iOS device arm64, simulator arm64, simulator x86_64
//     (WebRTC.framework of stasel/WebRTC 125.0.0): 8-byte absolute slots.
//     The framework's own Objective-C methods call `id` at +0x48,
//     `GetParameters` at +0x50, `SetObserver` at +0x60 and
//     `SetFrameDecryptor` at +0x78; the `VideoRtpReceiver` vtable's slot 13
//     (+0x68) is a function that stores its argument into the cached delay
//     and forwards `GetMs()` to the media channel, and slot 14 takes a
//     struct-return pointer (`GetSources`).
//   * Android x86_64 (libjingle_peerconnection_so.so of 125.6422.07): the
//     same 8-byte absolute slots (`nativeGetId` +0x48, `nativeGetParameters`
//     +0x50, `nativeSetObserver` +0x60, `nativeSetFrameDecryptor` +0x78;
//     slot 13 the same function shape).
//   * Android arm64-v8a: RELATIVE vtables. A slot is a signed 32-bit offset
//     from the vtable's address point (`ldrsw x9, [x8, #off]; add x8, x8,
//     x9; blr x8`); `nativeGetId` reads +0x24, `nativeGetParameters` +0x28,
//     `nativeSetObserver` +0x30, `nativeSetFrameDecryptor` +0x3c, so slot 13
//     is at +0x34. Chromium's build sets this for Android arm64 only
//     (`use_relative_vtables_abi` in build/config/compiler/compiler.gni).
//
// Since 2026-11-09 iOS links webrtc-sdk 125.6422.07 (ADR-0088's capture
// hook) instead of stasel/WebRTC 125.0.0. Its three iOS slices have NOT been
// read for this setter, so the iOS shim does not pin them and refuses the
// setter there (observe-only). What was compared on them, for the frame
// encryptor / decryptor attach only: the six `setFrameEncryptor:` /
// `setFrameDecryptor:` methods are instruction-identical to stasel's (the
// framework calls AddRef / Release of Lakko's object through absolute 8-byte
// slots), and a scan of the device arm64 slice finds an `ldrsw` within three
// instructions before a `blr` exactly twice, as in stasel's: neither binary
// is built with relative vtables.
//
// The argument: `absl::optional<double>` is `std::optional<double>` in these
// builds (`ABSL_OPTION_USE_STD_OPTIONAL 2`, C++17 and later). Read from the
// callee (`JitterBufferDelay::Set`) in every slice: the value arrives as two
// INTEGER register words, the double's bits in the first and the engaged
// flag in the low byte of the second (arm64 `x1`/`w2`, x86_64 `rsi`/`dl`),
// and is stored as 8 bytes plus one byte at +8. On x86_64 that is not where
// a plain `{double, bool}` would go (it would travel in `xmm0`): the union
// with a `char` inside `std::optional` makes the first word INTEGER class.
// `EngineOptionalDouble` below reproduces that shape exactly.
//
// The inline namespace of the two C++ libraries differs (the engine links
// its own libc++ statically); a virtual call goes by slot and by calling
// convention, neither of which involves a mangled name.
//
// ## What a caller must have established first
//
// This file cannot know which binary is loaded. The platform shim compares
// the identity the linker stamped into the loaded WebRTC image (the Mach-O
// `LC_UUID` on iOS, the ELF GNU build id on Android) with the identities of
// the slices the evidence above was read from, and does not call in here for
// any other image. That identity pin is the ONE memory-safety gate: every
// call this file makes, the probe's own two included, goes through a slot
// and a calling convention that are only known to be right for those images.
//
// The bracket probe below is not a second, independent gate. What it shows,
// on an image that already passed the pin: that the vtable encoding
// (absolute or relative slots) is the one this build assumes, and that slot
// 8 and slot 16 are `media_type()` and `GetFrameDecryptor()`. It does not
// show that slot 13 is the setter, nor that the setter takes its argument
// the way `EngineOptionalDouble` passes it; those two rest on the reading of
// the pinned binaries alone. It is a consistency check that catches a wrong
// encoding or a shifted table early, and it is itself only safe to run
// behind the pin.

#ifndef LAKKO_SFRAME_BRIDGE_RECEIVER_DELAY_H_
#define LAKKO_SFRAME_BRIDGE_RECEIVER_DELAY_H_

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <type_traits>

#include "api/crypto/frame_decryptor_interface.h"

namespace lakko {
namespace receiver_delay {

// What a call into this file says. The numeric values cross the Obj-C and
// JNI boundaries verbatim.
enum class Status : int {
  // The delay was handed to the engine.
  kOk = 0,
  // Nothing was set on this receiver: unsupported platform, a null
  // receiver, a receiver with no Lakko decryptor to run the probe against
  // yet, or one whose decryptor is not the one the caller holds for it (the
  // caller's record of this receiver is stale). Says nothing about the
  // engine: the probe stays open and another receiver may still pass it.
  kNotAvailable = 1,
  // The engine answered a probe question with something that is not
  // Lakko's: the layout cannot be trusted. Nothing is set for the rest of
  // the process.
  kProbeFailed = 2,
};

// How the loaded engine encodes a vtable slot.
enum class VtableAbi {
  // An 8-byte function address per slot.
  kAbsolute,
  // A signed 32-bit offset from the vtable's address point per slot.
  kRelative,
};

// The encoding of the engine this translation unit will run against.
constexpr VtableAbi PlatformVtableAbi() {
#if defined(__ANDROID__) && defined(__aarch64__)
  return VtableAbi::kRelative;
#else
  return VtableAbi::kAbsolute;
#endif
}

// The calling convention and the slot sizes above were read for 64-bit arm
// and x86 only. Anything else is refused at run time.
constexpr bool PlatformSupported() {
#if (defined(__aarch64__) || defined(__x86_64__)) && !defined(_WIN32)
  return sizeof(void*) == 8;
#else
  return false;
#endif
}

// Slots of `webrtc::RtpReceiverInterface` (see the table above).
constexpr int kSlotRelease = 1;
constexpr int kSlotMediaType = 8;
constexpr int kSlotSetJitterBufferMinimumDelay = 13;
constexpr int kSlotGetFrameDecryptor = 16;

// `cricket::MediaType` as the engine returns it from `media_type()`.
constexpr int kMediaTypeAudio = 0;
constexpr int kMediaTypeVideo = 1;
static_assert(kMediaTypeAudio == static_cast<int>(cricket::MEDIA_TYPE_AUDIO),
              "must be cricket::MEDIA_TYPE_AUDIO of the vendored header");
static_assert(kMediaTypeVideo == static_cast<int>(cricket::MEDIA_TYPE_VIDEO),
              "must be cricket::MEDIA_TYPE_VIDEO of the vendored header");

// The engine clamps the delay to ten seconds itself; nothing larger is sent.
constexpr uint32_t kMaxDelayMs = 10000;

// `std::optional<double>` as the engine's callee reads it: a union of one
// char and the double, then the engaged flag. Trivially copyable, 16 bytes,
// so it travels in two INTEGER register words on arm64 and on x86_64.
struct EngineOptionalDouble {
  union {
    char none;
    double value;
  };
  bool engaged;
};
static_assert(std::is_trivially_copyable<EngineOptionalDouble>::value,
              "must be passed in registers like std::optional<double>");
static_assert(sizeof(EngineOptionalDouble) == 16,
              "std::optional<double> is 16 bytes in the engine");
static_assert(offsetof(EngineOptionalDouble, engaged) == 8,
              "the engine reads the engaged flag at +8");

// What `GetFrameDecryptor()` returns, as far as the calling convention is
// concerned: a class holding one pointer that is NOT trivial for the purpose
// of calls (the engine's `rtc::scoped_refptr` has a destructor), so it comes
// back through a caller-provided slot. Unlike `scoped_refptr` it releases
// nothing by itself: the pointer is compared first, and only a pointer that
// is Lakko's own decryptor is ever released.
struct ReturnedRef {
  void* ptr;
  ReturnedRef() : ptr(nullptr) {}
  ReturnedRef(const ReturnedRef& other) : ptr(other.ptr) {}
  ~ReturnedRef() {}
};
static_assert(!std::is_trivially_copyable<ReturnedRef>::value,
              "must be returned in memory like rtc::scoped_refptr");
static_assert(sizeof(ReturnedRef) == sizeof(void*),
              "rtc::scoped_refptr is one pointer");

// One diagnostic line, plain ASCII, no trailing newline. May be null.
using LogFn = void (*)(const char* line);

// Whether `candidate` is the address of a frame decryptor Lakko created and
// that has not been destroyed (`lakko::sframe::LakkoFrameDecryptor::IsLive`
// in the shims). Must not dereference `candidate`. May be null, which reads
// as "no pointer is known to be Lakko's".
using IsLiveOwnDecryptorFn = bool (*)(const void* candidate);

// The function a vtable slot of `object` names under `abi`.
inline const void* SlotTarget(const void* object, int slot, VtableAbi abi) {
  const unsigned char* vptr = nullptr;
  std::memcpy(&vptr, object, sizeof(vptr));
  if (abi == VtableAbi::kRelative) {
    int32_t offset = 0;
    std::memcpy(&offset, vptr + 4 * static_cast<std::ptrdiff_t>(slot),
                sizeof(offset));
    return vptr + offset;
  }
  const void* target = nullptr;
  std::memcpy(&target, vptr + 8 * static_cast<std::ptrdiff_t>(slot),
              sizeof(target));
  return target;
}

// The Itanium C++ ABI passes `this` as the first ordinary argument, and a
// class returned in memory through the same hidden slot a free function
// uses, so each virtual is callable as a free function of these types.
using MediaTypeFn = int (*)(const void* self);
using ReleaseFn = int (*)(const void* self);
using SetDelayFn = void (*)(void* self, EngineOptionalDouble delay);
using GetDecryptorFn = ReturnedRef (*)(const void* self);

// 0: the probe has not run. 1: it passed. 2: it failed.
inline std::atomic<int>& ProbeState() {
  static std::atomic<int> state{0};
  return state;
}

// The probe failed in this process (nothing will be set).
inline bool ProbeHasFailed() {
  return ProbeState().load(std::memory_order_acquire) == 2;
}

// The probe passed in this process.
inline bool ProbeHasPassed() {
  return ProbeState().load(std::memory_order_acquire) == 1;
}

// Whether the "this receiver's decryptor is not the one the caller holds"
// line has been written in this process.
inline std::atomic<bool>& StaleDecryptorLogged() {
  static std::atomic<bool> logged{false};
  return logged;
}

// Host tests only: forget the probe's answer.
inline void ResetProbeForTesting() {
  ProbeState().store(0, std::memory_order_release);
  StaleDecryptorLogged().store(false, std::memory_order_release);
}

inline void Emit(LogFn log, const char* line) {
  if (log != nullptr) {
    log(line);
  }
}

// Drop one reference on an engine object (`RefCountInterface::Release`).
// For a reference the caller owns, such as the one the iOS `nativeRtpReceiver`
// accessor hands out.
inline void ReleaseEngineRef(const void* object,
                             VtableAbi abi = PlatformVtableAbi()) {
  if (object == nullptr || !PlatformSupported()) {
    return;
  }
  auto release = reinterpret_cast<ReleaseFn>(
      const_cast<void*>(SlotTarget(object, kSlotRelease, abi)));
  (void)release(object);
}

// Set the minimum playout delay of the engine receiver at `receiver` to
// `delay_ms` milliseconds; 0 gives the delay back to the engine (the
// engine's `nullopt`).
//
// `expected_media_type` is what the receiver's track says it is
// (`kMediaTypeAudio` / `kMediaTypeVideo`); `attached_decryptor` is the
// decryptor Lakko attached to this very receiver, or null when there is
// none.
//
// Before the first set of the process the receiver is asked two questions
// whose answers are known, one through a slot declared before the setter
// and one through a slot declared after it: `media_type()` must say
// `expected_media_type`, and `GetFrameDecryptor()` must return
// `attached_decryptor`. Only if both hold is the setter's slot called, then
// and from then on. `media_type()` is asked again on every call (it does not
// leave the calling thread).
//
// A wrong answer is one of two different things, and they are kept apart:
//
//   * The engine answered something that is not Lakko's: `media_type()`
//     disagrees with the track, or `GetFrameDecryptor()` returned a non-null
//     pointer that `is_live_own_decryptor` does not know. The layout cannot
//     be trusted. Every further set in the process stops (`kProbeFailed`),
//     logged once. The pointer that came back is never touched, a reference
//     on it included, so that one reference is deliberately leaked.
//   * `GetFrameDecryptor()` returned nothing, or a live decryptor of Lakko's
//     that is not `attached_decryptor`. The slot did what its name says; it
//     is the CALLER's record of this receiver that is stale (a second
//     binding-level wrapper of the same engine receiver was given a newer
//     decryptor, or the decryptor was detached through another wrapper).
//     This receiver is skipped (`kNotAvailable`), the probe stays open, and
//     the reference the engine added on Lakko's own object is given back.
//     Only a pointer `is_live_own_decryptor` vouches for is ever released.
//
// With a null `is_live_own_decryptor` every non-null mismatch is the first
// kind.
//
// The engine's object is a proxy: the setter and `GetFrameDecryptor` block
// the calling thread until the engine's worker thread has run them. Call
// from a thread that may block and that is not one of the engine's own.
// The caller keeps `receiver` and `attached_decryptor` alive for the call.
inline Status SetMinimumPlayoutDelay(
    void* receiver,
    uint32_t delay_ms,
    int expected_media_type,
    webrtc::FrameDecryptorInterface* attached_decryptor,
    IsLiveOwnDecryptorFn is_live_own_decryptor,
    LogFn log,
    VtableAbi abi = PlatformVtableAbi()) {
  if (!PlatformSupported() || receiver == nullptr) {
    return Status::kNotAvailable;
  }
  std::atomic<int>& state = ProbeState();
  if (state.load(std::memory_order_acquire) == 2) {
    return Status::kProbeFailed;
  }

  auto media_type = reinterpret_cast<MediaTypeFn>(
      const_cast<void*>(SlotTarget(receiver, kSlotMediaType, abi)));
  if (media_type(receiver) != expected_media_type) {
    if (state.exchange(2, std::memory_order_acq_rel) != 2) {
      Emit(log,
           "[lakko.receiver-delay] probe FAILED: media_type() did not match "
           "the track kind; playout delay stays observe-only");
    }
    return Status::kProbeFailed;
  }

  if (state.load(std::memory_order_acquire) == 0) {
    if (attached_decryptor == nullptr) {
      // Nothing to compare the second answer against: not a failure, and
      // the probe stays open for a receiver that has a decryptor.
      return Status::kNotAvailable;
    }
    auto get_decryptor = reinterpret_cast<GetDecryptorFn>(
        const_cast<void*>(SlotTarget(receiver, kSlotGetFrameDecryptor, abi)));
    const ReturnedRef returned = get_decryptor(receiver);
    if (returned.ptr != static_cast<void*>(attached_decryptor)) {
      const bool own = returned.ptr != nullptr &&
                       is_live_own_decryptor != nullptr &&
                       is_live_own_decryptor(returned.ptr);
      if (returned.ptr == nullptr || own) {
        // The caller's record of this receiver is stale; the engine is not
        // in question. A live decryptor of Lakko's came back with the
        // reference the engine added for the value it returned.
        if (own) {
          static_cast<webrtc::FrameDecryptorInterface*>(returned.ptr)
              ->Release();
        }
        if (!StaleDecryptorLogged().exchange(true,
                                             std::memory_order_acq_rel)) {
          Emit(log,
               "[lakko.receiver-delay] a receiver's decryptor is not the "
               "one held for it (none, or a newer one of Lakko's); that "
               "receiver is skipped, the probe stays open");
        }
        return Status::kNotAvailable;
      }
      // Whatever came back is not touched again, a reference on it
      // included: it is not known to be what the slot's name says.
      if (state.exchange(2, std::memory_order_acq_rel) != 2) {
        Emit(log,
             "[lakko.receiver-delay] probe FAILED: GetFrameDecryptor() "
             "returned something that is not a decryptor of Lakko's; "
             "playout delay stays observe-only");
      }
      return Status::kProbeFailed;
    }
    // The engine added a reference for the value it returned.
    attached_decryptor->Release();
    int unknown = 0;
    if (state.compare_exchange_strong(unknown, 1,
                                      std::memory_order_acq_rel)) {
      Emit(log,
           "[lakko.receiver-delay] probe passed: media_type() and "
           "GetFrameDecryptor() answered as expected");
    } else if (unknown == 2) {
      return Status::kProbeFailed;
    }
  }

  EngineOptionalDouble delay;
  std::memset(&delay, 0, sizeof(delay));
  if (delay_ms != 0) {
    const uint32_t bounded = delay_ms > kMaxDelayMs ? kMaxDelayMs : delay_ms;
    delay.value = static_cast<double>(bounded) / 1000.0;
    delay.engaged = true;
  }
  auto set_delay = reinterpret_cast<SetDelayFn>(const_cast<void*>(
      SlotTarget(receiver, kSlotSetJitterBufferMinimumDelay, abi)));
  set_delay(receiver, delay);
  return Status::kOk;
}

}  // namespace receiver_delay
}  // namespace lakko

#endif  // LAKKO_SFRAME_BRIDGE_RECEIVER_DELAY_H_
