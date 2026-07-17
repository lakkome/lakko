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
//   3. Own the per-call keying state and expose a mid-call `Rekey`
//      (G-18, ADR-0029) so an MLS epoch change during a live call
//      rotates the SFrame key with an overlap window.
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
// ## Mid-call rekey (G-18, ADR-0029)
//
// When the group's MLS epoch advances during a live call, the
// per-call seed changes. The platform shim re-derives it and calls
// `Rekey` on the encryptor + decryptor for that call:
//
//   - Encryptor: seals only under the CURRENT epoch, so a rekey is a
//     straight swap of the seal key — already-sealed frames are
//     already bytes in libwebrtc's pipeline and need no overlap. No
//     ring is retained on the send side.
//   - Decryptor: holds a `lakko_sframe_call_key_ring_t` (current +
//     previous key). A rekey rotates the ring, retaining the previous
//     key for the overlap window so in-flight frames sealed under the
//     old epoch still open; `Prune` drops it once the window elapses.
//
// ## Threading
//
// libwebrtc calls Encrypt() on the codec thread (one per outgoing
// track; serialised per encryptor) and Decrypt() on the network /
// decoder thread (one per incoming track; serialised per decryptor).
// `Rekey` / `Prune` are called from the signaling thread, i.e.
// CONCURRENTLY with Encrypt / Decrypt:
//
//   - The encryptor guards `participant_key_` with `seal_mutex_`
//     (Encrypt reads it while Rekey swaps it).
//   - The decryptor needs no C++ mutex — the Rust `CallKeyRing` is
//     `Mutex`-guarded internally, so open / rekey / prune all
//     serialise on the Rust side.

#ifndef LAKKO_SFRAME_BRIDGE_H_
#define LAKKO_SFRAME_BRIDGE_H_

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <mutex>

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
  //
  // `media_type` is the local send track's stream kind
  // (`LAKKO_SFRAME_MEDIA_TYPE_AUDIO` / `_VIDEO`, ADR-0034). A
  // participant's audio and video keys derive on distinct spaces, so
  // an encryptor is bound to exactly one media type for its lifetime;
  // Create refuses (nullptr) an unknown value. The platform shim
  // passes the track kind it is attaching to.
  static LakkoFrameEncryptor* Create(const uint8_t* seed,
                                     std::size_t seed_len,
                                     uint32_t local_leaf,
                                     uint8_t media_type);

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

  // Mid-call rekey (G-18, ADR-0029): swap the seal key to the new MLS
  // epoch's `seed`, keeping the SAME local leaf. Returns false on a bad
  // seed length or HKDF failure, in which case the current key is
  // retained (the caller keeps sealing under it — never plaintext).
  // Thread-safe against a concurrent Encrypt.
  bool Rekey(const uint8_t* seed, std::size_t seed_len);

  // RefCountInterface
  void AddRef() const override;
  rtc::RefCountReleaseStatus Release() const override;

 private:
  LakkoFrameEncryptor(lakko_sframe_participant_key_t* participant_key,
                      uint32_t local_leaf,
                      uint8_t media_type);

  // Owned. The seal key for the CURRENT epoch; `seal_framed` re-emits
  // the local leaf into the SFrame header (KID) internally (ADR-0013).
  // Guarded by `seal_mutex_` because Encrypt (codec thread) reads it
  // while Rekey (signaling thread) swaps it.
  lakko_sframe_participant_key_t* participant_key_;
  // The local leaf index, kept so Rekey can re-derive the seal key for
  // the same leaf under the new epoch.
  uint32_t local_leaf_;
  // The send track's stream kind (ADR-0034), kept so Rekey re-derives
  // the seal key on the SAME media-type space under the new epoch.
  uint8_t media_type_;
  mutable std::mutex seal_mutex_;

  mutable std::atomic<int> refcount_{0};
};

// ---------------------------------------------------------------
// Decryptor
// ---------------------------------------------------------------

class LakkoFrameDecryptor : public webrtc::FrameDecryptorInterface {
 public:
  // Construct from the same 32-byte per-call seed the encryptor
  // uses on every other participant. Returns nullptr on bad seed
  // length. The decryptor opens each frame through the call-key
  // ring, which derives the per-sender key internally and, during a
  // mid-call rekey overlap window, tries the previous epoch's key on
  // an AEAD failure (ADR-0029).
  //
  // `media_type` is the remote receive track's stream kind
  // (`LAKKO_SFRAME_MEDIA_TYPE_AUDIO` / `_VIDEO`, ADR-0034). A receiver
  // is bound to exactly one media type, so every open on this
  // decryptor derives the per-sender key on that one space; the
  // encryptor on the far side must have sealed on the same media type
  // or the frame will not open (fail-closed).
  static LakkoFrameDecryptor* Create(const uint8_t* seed,
                                     std::size_t seed_len,
                                     uint8_t media_type);

  ~LakkoFrameDecryptor() override;

  // FrameDecryptorInterface
  Result Decrypt(cricket::MediaType media_type,
                 const std::vector<uint32_t>& csrcs,
                 rtc::ArrayView<const uint8_t> additional_data,
                 rtc::ArrayView<const uint8_t> encrypted_frame,
                 rtc::ArrayView<uint8_t> frame) override;

  std::size_t GetMaxPlaintextByteSize(cricket::MediaType media_type,
                                      std::size_t encrypted_frame_size) override;

  // Mid-call rekey (G-18, ADR-0029): rotate the ring to the new MLS
  // epoch's `seed`, retaining the previous key until `now_ms +
  // clamp(overlap_ms)` so in-flight frames sealed under the old epoch
  // still open. Returns false on a bad seed length (the ring keeps its
  // current + previous keys). Thread-safe (the Rust ring is
  // `Mutex`-guarded internally).
  bool Rekey(const uint8_t* seed, std::size_t seed_len, uint64_t now_ms,
             uint64_t overlap_ms);

  // Drop the retained previous key once it has expired at `now_ms`
  // (closes the overlap window; seed-boundary forward secrecy).
  void Prune(uint64_t now_ms);

  // RefCountInterface
  void AddRef() const override;
  rtc::RefCountReleaseStatus Release() const override;

 private:
  LakkoFrameDecryptor(lakko_sframe_call_key_ring_t* ring, uint8_t media_type);

  // Owned. The overlap keyer (current + previous call key); ADR-0029.
  // Its internal Rust `Mutex` makes open / rekey / prune thread-safe.
  lakko_sframe_call_key_ring_t* ring_;
  // The receive track's stream kind (ADR-0034); every open derives the
  // per-sender key on this one space.
  uint8_t media_type_;

  mutable std::atomic<int> refcount_{0};
};

}  // namespace sframe
}  // namespace lakko

#endif  // LAKKO_SFRAME_BRIDGE_H_
