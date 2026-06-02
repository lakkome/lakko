// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors
//
// LakkoFrameEncryptor / LakkoFrameDecryptor — concrete C++
// subclasses of webrtc::FrameEncryptorInterface and
// webrtc::FrameDecryptorInterface (G3-sframe-native N-3).
//
// These are the only types in the SFrame native shim that the
// platform-specific glue (N-4 iOS Obj-C++, N-5 Android JNI) ever
// pokes at. Both implementations trampoline through the flat C
// ABI exposed by `lakko-sframe-native` (N-1) — every AEAD seal,
// every AEAD open, every SFrame wire-header pack/unpack happens
// on the Rust side. The C++ layer does three jobs and no more:
//
//   1. Subclass the libwebrtc interfaces with the exact vtable
//      shape branch-heads/6422 (M125) expects, so the platform
//      WebRTC binary's `RTCRtpSender::SetFrameEncryptor` / `Rtp
//      Receiver::SetFrameDecryptor` plumbing accepts our pointer.
//   2. Bridge libwebrtc's `rtc::ArrayView<const uint8_t>` /
//      `rtc::ArrayView<uint8_t>` to the `(const uint8_t*, size_t)`
//      shape the C ABI expects.
//   3. Maintain a tiny per-sender_leaf ParticipantKey cache on the
//      decryptor side (libwebrtc may call Decrypt from multiple
//      threads; std::mutex guards the map).
//
// All Rust-side errors collapse to libwebrtc's documented failure
// shapes:
//
//   - Encrypt() — non-zero return == failure. We pick `-1` for
//     "the encryptor itself errored" and let libwebrtc drop the
//     frame; SFrame contributors note this is the standard behaviour
//     of `RtpSenderEgress` upstream.
//   - Decrypt() — `Result(Status::kFailedToDecrypt, 0)`. Note we
//     never return `kRecoverable`; once an SFrame frame fails to
//     open, there is no recovery path. `kFailedToDecrypt` is
//     handled by libwebrtc the same way as Encrypt() returning
//     non-zero: the frame is dropped.
//
// ## Lifecycle
//
// `Create` returns a raw heap-allocated pointer with refcount=0.
// The caller (the platform shim) wraps it in `rtc::scoped_refptr`,
// which calls `AddRef()` and bumps refcount to 1. When the
// scoped_refptr goes out of scope, `Release()` drops to 0 and
// `delete this` runs in `Release()` per the canonical libwebrtc
// pattern (`rtc::FinalRefCountedObject<T>` does the same — we
// inline the machinery to avoid pulling in one more header).
//
// ## Threading
//
// libwebrtc calls Encrypt() on the codec thread (one per outgoing
// track; serialised per encryptor). Decrypt() is called on the
// network or decoder thread (one per incoming track; serialised
// per decryptor). The encryptor's seal path is therefore single-
// threaded against any one LakkoFrameEncryptor; we do NOT need a
// mutex around `participant_key_`. The decryptor's per-sender
// ParticipantKey map IS shared across multiple sender_leaf values
// arriving on the same incoming track (e.g. when an SFU forwards
// frames from N senders into one receiver); `lookup_or_derive` is
// guarded by `cache_mutex_`.

#ifndef LAKKO_SFRAME_BRIDGE_H_
#define LAKKO_SFRAME_BRIDGE_H_

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <mutex>
#include <unordered_map>

#include "api/crypto/frame_decryptor_interface.h"
#include "api/crypto/frame_encryptor_interface.h"

extern "C" {
#include "lakko_sframe_native.h"
}

namespace lakko {
namespace sframe {

// ---------------------------------------------------------------
// Encryptor
// ---------------------------------------------------------------

class LakkoFrameEncryptor : public webrtc::FrameEncryptorInterface {
 public:
  // Construct from a 32-byte per-call seed
  // (`Group::export_secret("lakko/call/v1", call_id, 32)`) plus
  // the local participant's leaf index. Returns nullptr on bad
  // seed length or HKDF failure. The caller takes ownership;
  // first AddRef bumps the refcount from 0 to 1.
  //
  // `seed_len` must be exactly 32; the lakko-sframe-native crate
  // refuses any other length with `BAD_SEED_LENGTH`. We surface
  // that as nullptr because libwebrtc has no error channel on
  // construction; an N-4 / N-5 boot probe that gets nullptr here
  // must refuse to install the encryptor (security invariant #4:
  // no fallback to non-E2EE).
  static LakkoFrameEncryptor* Create(const uint8_t* seed,
                                     std::size_t seed_len,
                                     uint32_t local_leaf);

  ~LakkoFrameEncryptor() override;

  // FrameEncryptorInterface
  int Encrypt(cricket::MediaType media_type,
              uint32_t ssrc,
              rtc::ArrayView<const uint8_t> additional_data,
              rtc::ArrayView<const uint8_t> frame,
              rtc::ArrayView<uint8_t> encrypted_frame,
              std::size_t* bytes_written) override;

  std::size_t GetMaxCiphertextByteSize(cricket::MediaType media_type,
                                       std::size_t frame_size) override;

  // RefCountInterface
  void AddRef() const override;
  rtc::RefCountReleaseStatus Release() const override;

 private:
  LakkoFrameEncryptor(lakko_sframe_call_key_t* call_key,
                      lakko_sframe_participant_key_t* participant_key);

  // Owned. Freed in the destructor via the matching `_free` symbol.
  // The local leaf index is not stored separately — it is baked into
  // `participant_key_` at derivation, and `seal_framed` re-emits it
  // into the SFrame header (KID) internally (ADR-0013).
  lakko_sframe_call_key_t* call_key_;
  lakko_sframe_participant_key_t* participant_key_;

  mutable std::atomic<int> refcount_{0};
};

// ---------------------------------------------------------------
// Decryptor
// ---------------------------------------------------------------

class LakkoFrameDecryptor : public webrtc::FrameDecryptorInterface {
 public:
  // Construct from the same 32-byte per-call seed the encryptor
  // uses on every other participant. Returns nullptr on bad seed
  // length. The decryptor lazily derives per-sender ParticipantKey
  // entries as frames arrive — the first frame from each new
  // `sender_leaf` triggers one HKDF derivation; subsequent frames
  // from the same sender hit the cache.
  static LakkoFrameDecryptor* Create(const uint8_t* seed,
                                     std::size_t seed_len);

  ~LakkoFrameDecryptor() override;

  // FrameDecryptorInterface
  Result Decrypt(cricket::MediaType media_type,
                 const std::vector<uint32_t>& csrcs,
                 rtc::ArrayView<const uint8_t> additional_data,
                 rtc::ArrayView<const uint8_t> encrypted_frame,
                 rtc::ArrayView<uint8_t> frame) override;

  std::size_t GetMaxPlaintextByteSize(cricket::MediaType media_type,
                                      std::size_t encrypted_frame_size) override;

  // RefCountInterface
  void AddRef() const override;
  rtc::RefCountReleaseStatus Release() const override;

 private:
  explicit LakkoFrameDecryptor(lakko_sframe_call_key_t* call_key);

  // Returns a non-owning pointer to the cached ParticipantKey for
  // `sender_leaf`, deriving it if absent. Returns nullptr only on
  // HKDF failure (practically unreachable; the underlying lakko-
  // sframe-native call only fails on null call_key). Thread-safe.
  lakko_sframe_participant_key_t* lookup_or_derive(uint32_t sender_leaf);

  // Owned. Freed in the destructor.
  lakko_sframe_call_key_t* call_key_;

  // Owned. Map values are freed in the destructor (one
  // `lakko_sframe_participant_key_free` per entry).
  mutable std::mutex cache_mutex_;
  std::unordered_map<uint32_t, lakko_sframe_participant_key_t*>
      participant_keys_;

  mutable std::atomic<int> refcount_{0};
};

}  // namespace sframe
}  // namespace lakko

#endif  // LAKKO_SFRAME_BRIDGE_H_
