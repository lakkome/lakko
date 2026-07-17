// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

#include "sframe-bridge.h"

#include <cstring>

namespace lakko {
namespace sframe {

namespace {

// SFrame wire header for our `kid` + `ctr` range is at most 17
// bytes (1 config + up to 8 kid + up to 8 ctr); the AEAD tag is
// fixed 16 bytes for ChaCha20-Poly1305 (and 16 for AES-GCM-128).
// We use this conservative upper bound to size encrypted_frame
// buffers libwebrtc allocates via GetMaxCiphertextByteSize.
//
// Why a conservative constant rather than a precise calculation:
// libwebrtc calls GetMaxCiphertextByteSize once per stream
// configuration, well before frames arrive, so over-allocating
// by a handful of bytes per frame is irrelevant. Under-
// allocating would cause Encrypt() to silently truncate and
// drop frames at decode time — a far worse outcome.
constexpr std::size_t kMaxSframeHeaderBytes = 17;
constexpr std::size_t kAeadTagBytes = 16;

}  // namespace

// ---------------------------------------------------------------
// LakkoFrameEncryptor
// ---------------------------------------------------------------

// static
LakkoFrameEncryptor* LakkoFrameEncryptor::Create(const uint8_t* seed,
                                                 std::size_t seed_len,
                                                 uint32_t local_leaf,
                                                 uint8_t media_type) {
  lakko_sframe_call_key_t* call_key = nullptr;
  lakko_sframe_status_t status =
      lakko_sframe_call_key_new(seed, seed_len, &call_key);
  if (status != LAKKO_SFRAME_STATUS_T_OK || call_key == nullptr) {
    return nullptr;
  }

  // Derive the seal key on this encryptor's fixed media-type space
  // (ADR-0034). An unknown media_type is refused (BAD_MEDIA_TYPE) →
  // nullptr, which the boot probe treats like any other Create failure
  // (invariant #4: no non-E2EE fallback).
  lakko_sframe_participant_key_t* participant_key = nullptr;
  status = lakko_sframe_participant_key_new(call_key, local_leaf, media_type,
                                            &participant_key);
  // The call key is only needed to derive the seal key; drop it either way.
  lakko_sframe_call_key_free(call_key);
  if (status != LAKKO_SFRAME_STATUS_T_OK || participant_key == nullptr) {
    return nullptr;
  }

  return new LakkoFrameEncryptor(participant_key, local_leaf, media_type);
}

LakkoFrameEncryptor::LakkoFrameEncryptor(
    lakko_sframe_participant_key_t* participant_key,
    uint32_t local_leaf,
    uint8_t media_type)
    : participant_key_(participant_key),
      local_leaf_(local_leaf),
      media_type_(media_type) {}

LakkoFrameEncryptor::~LakkoFrameEncryptor() {
  // The free routine tolerates null pointers (per the C ABI contract
  // documented in lakko_sframe_native.h) but we paired construction +
  // ownership so this is always live here.
  lakko_sframe_participant_key_free(participant_key_);
}

int LakkoFrameEncryptor::Encrypt(cricket::MediaType /*media_type*/,
                                 uint32_t /*ssrc*/,
                                 rtc::ArrayView<const uint8_t> /*additional_data*/,
                                 rtc::ArrayView<const uint8_t> frame,
                                 rtc::ArrayView<uint8_t> encrypted_frame,
                                 std::size_t* bytes_written) {
  if (bytes_written == nullptr) {
    return -1;
  }
  *bytes_written = 0;

  // Seal the plaintext into a complete SFrame wire frame
  // (header || ciphertext) in one call, with the SFrame header
  // (KID = local_leaf, CTR) bound as the AEAD AAD — per ADR-0013.
  //
  // The host RTP `additional_data` is intentionally NOT bound: that
  // byte string is not reconstructible on the web
  // `RTCRtpScriptTransform` side, so binding it would break
  // web <-> native interop. The header authenticates the frame's
  // routing (KID + CTR); media confidentiality + integrity come
  // from the AEAD itself.
  //
  // `participant_key_` is read under `seal_mutex_` because a mid-call
  // Rekey (signaling thread) may swap it concurrently (ADR-0029).
  uint8_t* wire_ptr = nullptr;
  std::size_t wire_len = 0;
  lakko_sframe_status_t status;
  {
    std::lock_guard<std::mutex> guard(seal_mutex_);
    status = lakko_sframe_seal_framed(
        participant_key_, frame.data(), frame.size(), &wire_ptr, &wire_len);
  }
  if (status != LAKKO_SFRAME_STATUS_T_OK) {
    return -1;
  }

  // Copy into libwebrtc's output buffer.
  if (wire_len > encrypted_frame.size()) {
    // GetMaxCiphertextByteSize undercounted — bug in this file.
    lakko_sframe_buffer_free(wire_ptr, wire_len);
    return -1;
  }
  std::memcpy(encrypted_frame.data(), wire_ptr, wire_len);
  *bytes_written = wire_len;

  lakko_sframe_buffer_free(wire_ptr, wire_len);
  return 0;
}

bool LakkoFrameEncryptor::Rekey(const uint8_t* seed, std::size_t seed_len) {
  // Derive the new epoch's seal key BEFORE taking the lock, so the codec
  // thread is never blocked on HKDF; only the pointer swap is under the lock.
  lakko_sframe_call_key_t* call_key = nullptr;
  if (lakko_sframe_call_key_new(seed, seed_len, &call_key) !=
          LAKKO_SFRAME_STATUS_T_OK ||
      call_key == nullptr) {
    return false;
  }
  // Re-derive on the SAME media-type space this encryptor was created
  // with (ADR-0034) — a rekey changes only the epoch, never the stream.
  lakko_sframe_participant_key_t* fresh = nullptr;
  lakko_sframe_status_t status = lakko_sframe_participant_key_new(
      call_key, local_leaf_, media_type_, &fresh);
  lakko_sframe_call_key_free(call_key);
  if (status != LAKKO_SFRAME_STATUS_T_OK || fresh == nullptr) {
    // Keep sealing under the current key — never emit plaintext (invariant #4).
    return false;
  }

  lakko_sframe_participant_key_t* stale = nullptr;
  {
    std::lock_guard<std::mutex> guard(seal_mutex_);
    stale = participant_key_;
    participant_key_ = fresh;
  }
  lakko_sframe_participant_key_free(stale);
  return true;
}

std::size_t LakkoFrameEncryptor::GetMaxCiphertextByteSize(
    cricket::MediaType /*media_type*/,
    std::size_t frame_size) {
  return frame_size + kMaxSframeHeaderBytes + kAeadTagBytes;
}

void LakkoFrameEncryptor::AddRef() const {
  refcount_.fetch_add(1, std::memory_order_relaxed);
}

rtc::RefCountReleaseStatus LakkoFrameEncryptor::Release() const {
  // `acq_rel` on the decrement is the canonical refcount pattern
  // — happens-before edges from any earlier `AddRef` need to
  // synchronise with the final delete.
  const int previous = refcount_.fetch_sub(1, std::memory_order_acq_rel);
  if (previous == 1) {
    delete this;
    return rtc::RefCountReleaseStatus::kDroppedLastRef;
  }
  return rtc::RefCountReleaseStatus::kOtherRefsRemained;
}

// ---------------------------------------------------------------
// LakkoFrameDecryptor
// ---------------------------------------------------------------

// static
LakkoFrameDecryptor* LakkoFrameDecryptor::Create(const uint8_t* seed,
                                                 std::size_t seed_len,
                                                 uint8_t media_type) {
  // Refuse an unknown media_type up front (invariant #4) so the boot
  // probe never installs a decryptor that would fail every open. The
  // ring itself carries no media type — it is supplied per open.
  if (media_type != LAKKO_SFRAME_MEDIA_TYPE_AUDIO &&
      media_type != LAKKO_SFRAME_MEDIA_TYPE_VIDEO) {
    return nullptr;
  }
  lakko_sframe_call_key_ring_t* ring = nullptr;
  lakko_sframe_status_t status =
      lakko_sframe_call_key_ring_new(seed, seed_len, &ring);
  if (status != LAKKO_SFRAME_STATUS_T_OK || ring == nullptr) {
    return nullptr;
  }
  return new LakkoFrameDecryptor(ring, media_type);
}

LakkoFrameDecryptor::LakkoFrameDecryptor(lakko_sframe_call_key_ring_t* ring,
                                         uint8_t media_type)
    : ring_(ring), media_type_(media_type) {}

LakkoFrameDecryptor::~LakkoFrameDecryptor() {
  // Callers must have released their refcount before letting us
  // destruct, so no Decrypt / Rekey is in flight against `ring_`.
  lakko_sframe_call_key_ring_free(ring_);
}

webrtc::FrameDecryptorInterface::Result LakkoFrameDecryptor::Decrypt(
    cricket::MediaType /*media_type*/,
    const std::vector<uint32_t>& /*csrcs*/,
    rtc::ArrayView<const uint8_t> /*additional_data*/,
    rtc::ArrayView<const uint8_t> encrypted_frame,
    rtc::ArrayView<uint8_t> frame) {
  // 1. Peek the SFrame header to resolve sender_leaf (KID) for key
  //    selection. The bare ciphertext `unwrap_frame` also returns is
  //    unused here — the ring re-parses the full wire and binds the
  //    on-wire header as the AEAD AAD (ADR-0013) — so we free it.
  uint32_t sender_leaf = 0;
  uint32_t counter = 0;
  uint8_t* ct_ptr = nullptr;
  std::size_t ct_len = 0;
  lakko_sframe_status_t unwrap_status = lakko_sframe_unwrap_frame(
      encrypted_frame.data(), encrypted_frame.size(),
      &sender_leaf, &counter, &ct_ptr, &ct_len);
  if (unwrap_status != LAKKO_SFRAME_STATUS_T_OK) {
    return Result(Status::kFailedToDecrypt, 0);
  }
  lakko_sframe_buffer_free(ct_ptr, ct_len);
  (void)counter;  // CTR is read again inside the ring open from the wire.

  // 2. Open through the ring's overlap fall-back: the current epoch's
  //    per-sender key, and — during a mid-call rekey window — the
  //    previous epoch's key on an AEAD failure (ADR-0029). The on-wire
  //    SFrame header is the AEAD AAD; the host `additional_data` is
  //    intentionally NOT used (mirrors Encrypt; web <-> native interop).
  uint8_t* pt_ptr = nullptr;
  std::size_t pt_len = 0;
  lakko_sframe_status_t open_status =
      lakko_sframe_call_key_ring_open_framed_with_overlap(
          ring_, sender_leaf, media_type_, encrypted_frame.data(),
          encrypted_frame.size(), &pt_ptr, &pt_len);
  if (open_status != LAKKO_SFRAME_STATUS_T_OK) {
    return Result(Status::kFailedToDecrypt, 0);
  }

  // 3. Copy into libwebrtc's output buffer.
  if (pt_len > frame.size()) {
    // GetMaxPlaintextByteSize undercounted — bug in this file.
    lakko_sframe_buffer_free(pt_ptr, pt_len);
    return Result(Status::kFailedToDecrypt, 0);
  }
  std::memcpy(frame.data(), pt_ptr, pt_len);
  lakko_sframe_buffer_free(pt_ptr, pt_len);

  return Result(Status::kOk, pt_len);
}

bool LakkoFrameDecryptor::Rekey(const uint8_t* seed, std::size_t seed_len,
                                uint64_t now_ms, uint64_t overlap_ms) {
  // The ring is Mutex-guarded internally, so this is safe against a
  // concurrent Decrypt on the network thread.
  return lakko_sframe_call_key_ring_rekey(ring_, seed, seed_len, now_ms,
                                          overlap_ms) ==
         LAKKO_SFRAME_STATUS_T_OK;
}

void LakkoFrameDecryptor::Prune(uint64_t now_ms) {
  lakko_sframe_call_key_ring_prune(ring_, now_ms);
}

std::size_t LakkoFrameDecryptor::GetMaxPlaintextByteSize(
    cricket::MediaType /*media_type*/,
    std::size_t encrypted_frame_size) {
  // Plaintext is strictly smaller than ciphertext (no header, no
  // AEAD tag). Conservatively return the input size — over-
  // allocation is harmless; under-allocation truncates frames.
  return encrypted_frame_size;
}

void LakkoFrameDecryptor::AddRef() const {
  refcount_.fetch_add(1, std::memory_order_relaxed);
}

rtc::RefCountReleaseStatus LakkoFrameDecryptor::Release() const {
  const int previous = refcount_.fetch_sub(1, std::memory_order_acq_rel);
  if (previous == 1) {
    delete this;
    return rtc::RefCountReleaseStatus::kDroppedLastRef;
  }
  return rtc::RefCountReleaseStatus::kOtherRefsRemained;
}

}  // namespace sframe
}  // namespace lakko
