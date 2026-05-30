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
                                                 uint32_t local_leaf) {
  lakko_sframe_call_key_t* call_key = nullptr;
  lakko_sframe_status_t status =
      lakko_sframe_call_key_new(seed, seed_len, &call_key);
  if (status != LAKKO_SFRAME_STATUS_T_OK || call_key == nullptr) {
    return nullptr;
  }

  lakko_sframe_participant_key_t* participant_key = nullptr;
  status = lakko_sframe_participant_key_new(call_key, local_leaf,
                                            &participant_key);
  if (status != LAKKO_SFRAME_STATUS_T_OK || participant_key == nullptr) {
    lakko_sframe_call_key_free(call_key);
    return nullptr;
  }

  return new LakkoFrameEncryptor(call_key, participant_key, local_leaf);
}

LakkoFrameEncryptor::LakkoFrameEncryptor(
    lakko_sframe_call_key_t* call_key,
    lakko_sframe_participant_key_t* participant_key,
    uint32_t local_leaf)
    : call_key_(call_key),
      participant_key_(participant_key),
      local_leaf_(local_leaf) {}

LakkoFrameEncryptor::~LakkoFrameEncryptor() {
  // Free in reverse order of construction. The free routines
  // tolerate null pointers (per the C ABI contract documented in
  // lakko_sframe_native.h) but we paired construction + ownership
  // so these are always live here.
  lakko_sframe_participant_key_free(participant_key_);
  lakko_sframe_call_key_free(call_key_);
}

int LakkoFrameEncryptor::Encrypt(cricket::MediaType /*media_type*/,
                                 uint32_t /*ssrc*/,
                                 rtc::ArrayView<const uint8_t> additional_data,
                                 rtc::ArrayView<const uint8_t> frame,
                                 rtc::ArrayView<uint8_t> encrypted_frame,
                                 std::size_t* bytes_written) {
  if (bytes_written == nullptr) {
    return -1;
  }
  *bytes_written = 0;

  // 1. Seal the plaintext frame under the next per-participant
  //    counter, with the RTP additional_data bound into the AAD.
  uint32_t counter = 0;
  uint8_t* ct_ptr = nullptr;
  std::size_t ct_len = 0;
  lakko_sframe_status_t seal_status = lakko_sframe_seal(
      participant_key_,
      additional_data.data(), additional_data.size(),
      frame.data(), frame.size(),
      &counter,
      &ct_ptr, &ct_len);
  if (seal_status != LAKKO_SFRAME_STATUS_T_OK) {
    return -1;
  }

  // 2. Pack the SFrame wire header (kid=local_leaf, ctr=counter)
  //    followed by the ciphertext. Two allocs per frame is
  //    suboptimal; an inlined header+ct emit is a follow-up.
  uint8_t* wire_ptr = nullptr;
  std::size_t wire_len = 0;
  lakko_sframe_status_t wrap_status = lakko_sframe_wrap_frame(
      local_leaf_, counter, ct_ptr, ct_len, &wire_ptr, &wire_len);
  // We're done with the bare ciphertext regardless of whether
  // wrapping succeeded.
  lakko_sframe_buffer_free(ct_ptr, ct_len);
  if (wrap_status != LAKKO_SFRAME_STATUS_T_OK) {
    return -1;
  }

  // 3. Copy into libwebrtc's output buffer.
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
                                                 std::size_t seed_len) {
  lakko_sframe_call_key_t* call_key = nullptr;
  lakko_sframe_status_t status =
      lakko_sframe_call_key_new(seed, seed_len, &call_key);
  if (status != LAKKO_SFRAME_STATUS_T_OK || call_key == nullptr) {
    return nullptr;
  }
  return new LakkoFrameDecryptor(call_key);
}

LakkoFrameDecryptor::LakkoFrameDecryptor(lakko_sframe_call_key_t* call_key)
    : call_key_(call_key) {}

LakkoFrameDecryptor::~LakkoFrameDecryptor() {
  // Drain the per-sender cache before freeing the parent call key.
  // No need to hold the mutex — we're in the destructor, so no
  // other thread can be observing this object (callers must have
  // released their refcount before letting us destruct).
  for (auto& entry : participant_keys_) {
    lakko_sframe_participant_key_free(entry.second);
  }
  participant_keys_.clear();
  lakko_sframe_call_key_free(call_key_);
}

lakko_sframe_participant_key_t* LakkoFrameDecryptor::lookup_or_derive(
    uint32_t sender_leaf) {
  // Fast path: try the cache under the lock.
  {
    std::lock_guard<std::mutex> guard(cache_mutex_);
    auto it = participant_keys_.find(sender_leaf);
    if (it != participant_keys_.end()) {
      return it->second;
    }
  }

  // Slow path: derive. Done outside the lock because HKDF is
  // pure-CPU + a contended mutex would serialise N parallel
  // first-frame arrivals on the codec thread for no reason. The
  // cost is that two threads racing on a fresh sender_leaf may
  // both derive; the loser frees its own copy below.
  lakko_sframe_participant_key_t* derived = nullptr;
  lakko_sframe_status_t status =
      lakko_sframe_participant_key_new(call_key_, sender_leaf, &derived);
  if (status != LAKKO_SFRAME_STATUS_T_OK || derived == nullptr) {
    return nullptr;
  }

  // Insert-or-find under the lock to settle the race.
  {
    std::lock_guard<std::mutex> guard(cache_mutex_);
    auto [it, inserted] = participant_keys_.emplace(sender_leaf, derived);
    if (!inserted) {
      // Another thread won — free our copy, return theirs.
      lakko_sframe_participant_key_free(derived);
      return it->second;
    }
    return derived;
  }
}

webrtc::FrameDecryptorInterface::Result LakkoFrameDecryptor::Decrypt(
    cricket::MediaType /*media_type*/,
    const std::vector<uint32_t>& /*csrcs*/,
    rtc::ArrayView<const uint8_t> additional_data,
    rtc::ArrayView<const uint8_t> encrypted_frame,
    rtc::ArrayView<uint8_t> frame) {
  // 1. Unwrap the SFrame wire header to extract sender_leaf +
  //    counter + bare ciphertext.
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

  // 2. Resolve the per-sender ParticipantKey (derive lazily).
  lakko_sframe_participant_key_t* participant_key =
      lookup_or_derive(sender_leaf);
  if (participant_key == nullptr) {
    lakko_sframe_buffer_free(ct_ptr, ct_len);
    return Result(Status::kFailedToDecrypt, 0);
  }

  // 3. Open the AEAD payload.
  uint8_t* pt_ptr = nullptr;
  std::size_t pt_len = 0;
  lakko_sframe_status_t open_status = lakko_sframe_open(
      participant_key,
      additional_data.data(), additional_data.size(),
      ct_ptr, ct_len, counter,
      &pt_ptr, &pt_len);
  lakko_sframe_buffer_free(ct_ptr, ct_len);
  if (open_status != LAKKO_SFRAME_STATUS_T_OK) {
    return Result(Status::kFailedToDecrypt, 0);
  }

  // 4. Copy into libwebrtc's output buffer.
  if (pt_len > frame.size()) {
    // GetMaxPlaintextByteSize undercounted — bug in this file.
    lakko_sframe_buffer_free(pt_ptr, pt_len);
    return Result(Status::kFailedToDecrypt, 0);
  }
  std::memcpy(frame.data(), pt_ptr, pt_len);
  lakko_sframe_buffer_free(pt_ptr, pt_len);

  return Result(Status::kOk, pt_len);
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
