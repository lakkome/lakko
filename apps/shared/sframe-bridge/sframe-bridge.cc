// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

#include "sframe-bridge.h"

#include <cstdio>
#include <cstring>
#include <mutex>
#include <vector>

namespace lakko {
namespace sframe {

namespace {

// What a sealed frame (ADR-0071) adds to the encoded frame: the AEAD
// tag (16 bytes, ChaCha20-Poly1305), the SFrame header (at most 17
// bytes: 1 config + up to 8 kid + up to 8 ctr) and the trailer (3
// bytes). The clear prefix is part of the frame itself and adds
// nothing. So a sealed frame is at most 36 bytes longer than its
// input; GetMaxCiphertextByteSize reports exactly that bound.
//
// Why a conservative constant rather than a precise calculation:
// libwebrtc calls GetMaxCiphertextByteSize once per stream
// configuration, well before frames arrive, so over-allocating
// by a handful of bytes per frame is irrelevant. Under-
// allocating would cause Encrypt() to silently truncate and
// drop frames at decode time — a far worse outcome.
constexpr std::size_t kMaxSframeHeaderBytes = 17;
constexpr std::size_t kAeadTagBytes = 16;
constexpr std::size_t kTrailerBytes = 3;

// ASCII case-insensitive equality on C strings. Codec names are ASCII;
// no locale is consulted.
bool EqualsIgnoreAsciiCase(const char* a, const char* b) {
  for (;; ++a, ++b) {
    unsigned char ca = static_cast<unsigned char>(*a);
    unsigned char cb = static_cast<unsigned char>(*b);
    if (ca >= 'A' && ca <= 'Z') ca = static_cast<unsigned char>(ca + 32);
    if (cb >= 'A' && cb <= 'Z') cb = static_cast<unsigned char>(cb + 32);
    if (ca != cb) return false;
    if (ca == '\0') return true;
  }
}

}  // namespace

// ---------------------------------------------------------------
// LakkoFrameEncryptor
// ---------------------------------------------------------------

// static
LakkoFrameEncryptor* LakkoFrameEncryptor::Create(const uint8_t* seed,
                                                 std::size_t seed_len,
                                                 uint32_t local_leaf,
                                                 uint8_t stream_index) {
  lakko_sframe_call_key_t* call_key = nullptr;
  lakko_sframe_status_t status =
      lakko_sframe_call_key_new(seed, seed_len, &call_key);
  if (status != LAKKO_SFRAME_STATUS_T_OK || call_key == nullptr) {
    return nullptr;
  }

  // Derive the seal key on this encryptor's fixed stream (ADR-0035):
  // KID = (local_leaf << 8) | stream_index. Every uint8_t stream_index
  // is valid, so this only fails on an (unreachable) HKDF error →
  // nullptr, which the boot probe treats like any other Create failure
  // (invariant #4: no non-E2EE fallback).
  lakko_sframe_participant_key_t* participant_key = nullptr;
  status = lakko_sframe_participant_key_new(call_key, local_leaf, stream_index,
                                            &participant_key);
  // The call key is only needed to derive the seal key; drop it either way.
  lakko_sframe_call_key_free(call_key);
  if (status != LAKKO_SFRAME_STATUS_T_OK || participant_key == nullptr) {
    return nullptr;
  }

  return new LakkoFrameEncryptor(participant_key, local_leaf, stream_index);
}

LakkoFrameEncryptor::LakkoFrameEncryptor(
    lakko_sframe_participant_key_t* participant_key,
    uint32_t local_leaf,
    uint8_t stream_index)
    : participant_key_(participant_key),
      local_leaf_(local_leaf),
      stream_index_(stream_index) {}

LakkoFrameEncryptor::~LakkoFrameEncryptor() {
  // The free routine tolerates null pointers (per the C ABI contract
  // documented in lakko_sframe_native.h) but we paired construction +
  // ownership so this is always live here.
  lakko_sframe_participant_key_free(participant_key_);
}

int LakkoFrameEncryptor::Encrypt(cricket::MediaType media_type,
                                 uint32_t /*ssrc*/,
                                 rtc::ArrayView<const uint8_t> /*additional_data*/,
                                 rtc::ArrayView<const uint8_t> frame,
                                 rtc::ArrayView<uint8_t> encrypted_frame,
                                 std::size_t* bytes_written) {
  if (bytes_written == nullptr) {
    return -1;
  }
  *bytes_written = 0;

  // How many leading bytes of this frame stay in the clear (ADR-0071).
  // Audio never keeps any, whatever codec was set: the media type is the
  // one thing libwebrtc does say here. For video the codec comes from
  // `SetCodec`; until the shim has called it the value is OTHER and
  // nothing is clear. For VP8 the Rust side reads the frame's own tag to
  // tell a key frame (10 bytes) from any other frame (3 bytes).
  const uint8_t codec = media_type == cricket::MEDIA_TYPE_AUDIO
                            ? static_cast<uint8_t>(LAKKO_SFRAME_MEDIA_CODEC_OPUS)
                            : codec_.load(std::memory_order_acquire);
  const std::size_t clear_prefix_len =
      lakko_sframe_media_clear_prefix_len(codec, frame.data(), frame.size());

  // Seal into `clear_prefix || ciphertext_and_tag || sframe_header ||
  // trailer` in one call. The header carries KID = (local_leaf << 8) |
  // stream_index and the counter (ADR-0035, ADR-0069); the AEAD
  // additional data is `sframe_header || clear_prefix || trailer`, so
  // every byte that is not ciphertext is authenticated.
  //
  // The host RTP `additional_data` is intentionally NOT bound: that
  // byte string is not reconstructible on the web
  // `RTCRtpScriptTransform` side, so binding it would break
  // web <-> native interop.
  //
  // `participant_key_` is read under `seal_mutex_` because a mid-call
  // Rekey (signaling thread) may swap it concurrently (ADR-0029).
  uint8_t* wire_ptr = nullptr;
  std::size_t wire_len = 0;
  lakko_sframe_status_t status;
  {
    std::lock_guard<std::mutex> guard(seal_mutex_);
    status = lakko_sframe_seal_media_frame(participant_key_, frame.data(),
                                           frame.size(), clear_prefix_len,
                                           &wire_ptr, &wire_len);
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

void LakkoFrameEncryptor::SetCodec(uint8_t codec) {
  codec_.store(codec, std::memory_order_release);
}

uint8_t LakkoFrameEncryptor::Codec() const {
  return codec_.load(std::memory_order_acquire);
}

// static
uint8_t LakkoFrameEncryptor::CodecFromName(const char* name) {
  if (name == nullptr) {
    return LAKKO_SFRAME_MEDIA_CODEC_OTHER;
  }
  if (EqualsIgnoreAsciiCase(name, "VP8") ||
      EqualsIgnoreAsciiCase(name, "video/VP8")) {
    return LAKKO_SFRAME_MEDIA_CODEC_VP8;
  }
  if (EqualsIgnoreAsciiCase(name, "opus") ||
      EqualsIgnoreAsciiCase(name, "audio/opus")) {
    return LAKKO_SFRAME_MEDIA_CODEC_OPUS;
  }
  return LAKKO_SFRAME_MEDIA_CODEC_OTHER;
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
  // Re-derive on the SAME stream this encryptor was created with
  // (ADR-0035) — a rekey changes only the epoch, never the stream.
  lakko_sframe_participant_key_t* fresh = nullptr;
  lakko_sframe_status_t status = lakko_sframe_participant_key_new(
      call_key, local_leaf_, stream_index_, &fresh);
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
  return frame_size + kAeadTagBytes + kMaxSframeHeaderBytes + kTrailerBytes;
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
// Diagnostic sink
// ----------------------------------------------------------------------------

namespace {

std::atomic<LogSink> g_log_sink{nullptr};

// Emit one diagnostic line, if a host installed a sink. Never throws, never
// allocates, and is a no-op when nothing is listening.
void EmitLog(const char* line) {
  LogSink sink = g_log_sink.load(std::memory_order_acquire);
  if (sink != nullptr) {
    sink(line);
  }
}

// The status enum spelled out, so a diagnostic line can be read without the
// header to hand.
const char* StatusName(lakko_sframe_status_t status) {
  switch (status) {
    case LAKKO_SFRAME_STATUS_T_OK: return "OK";
    case LAKKO_SFRAME_STATUS_T_NULL_POINTER: return "NULL_POINTER";
    case LAKKO_SFRAME_STATUS_T_BAD_SEED_LENGTH: return "BAD_SEED_LENGTH";
    case LAKKO_SFRAME_STATUS_T_KEY_DERIVATION: return "KEY_DERIVATION";
    case LAKKO_SFRAME_STATUS_T_COUNTER_EXHAUSTED: return "COUNTER_EXHAUSTED";
    case LAKKO_SFRAME_STATUS_T_AEAD_FAILED: return "AEAD_FAILED";
    case LAKKO_SFRAME_STATUS_T_WIRE_PARSE: return "WIRE_PARSE";
    case LAKKO_SFRAME_STATUS_T_KID_OVERFLOW: return "KID_OVERFLOW";
    case LAKKO_SFRAME_STATUS_T_CTR_OVERFLOW: return "CTR_OVERFLOW";
    case LAKKO_SFRAME_STATUS_T_BAD_CLEAR_PREFIX: return "BAD_CLEAR_PREFIX";
    default: return "UNKNOWN";
  }
}

}  // namespace

void SetLogSink(LogSink sink) {
  g_log_sink.store(sink, std::memory_order_release);
}

// ----------------------------------------------------------------------------
// LakkoFrameDecryptor
// ---------------------------------------------------------------

// static
namespace {

// The decryptors alive in this process, as `FrameDecryptorInterface`
// addresses, for `LakkoFrameDecryptor::IsLive`. A handful per call, so a
// vector under a mutex. Both are leaked on purpose: a decryptor the engine
// still holds may be destroyed after this file's statics would have been.
std::mutex& LiveDecryptorsMutex() {
  static std::mutex* mutex = new std::mutex();
  return *mutex;
}

std::vector<const void*>& LiveDecryptors() {
  static std::vector<const void*>* live = new std::vector<const void*>();
  return *live;
}

}  // namespace

bool LakkoFrameDecryptor::IsLive(const void* candidate) {
  if (candidate == nullptr) {
    return false;
  }
  std::lock_guard<std::mutex> lock(LiveDecryptorsMutex());
  for (const void* live : LiveDecryptors()) {
    if (live == candidate) {
      return true;
    }
  }
  return false;
}

LakkoFrameDecryptor* LakkoFrameDecryptor::Create(const uint8_t* seed,
                                                 std::size_t seed_len,
                                                 const char* label) {
  // The decryptor reads the composite KID from each frame's header
  // (ADR-0035), so it carries no per-stream state — a bad seed length
  // is the only Create-time failure, surfaced as nullptr (invariant
  // #4: the boot probe refuses to install a decryptor that cannot key).
  lakko_sframe_call_key_ring_t* ring = nullptr;
  lakko_sframe_status_t status =
      lakko_sframe_call_key_ring_new(seed, seed_len, &ring);
  if (status != LAKKO_SFRAME_STATUS_T_OK || ring == nullptr) {
    return nullptr;
  }
  return new LakkoFrameDecryptor(ring, label);
}

LakkoFrameDecryptor::LakkoFrameDecryptor(lakko_sframe_call_key_ring_t* ring,
                                         const char* label)
    : ring_(ring) {
  // Bounded copy: the caller's string is not owned and this object outlives
  // the attach call that supplied it.
  const char* source = (label != nullptr && label[0] != '\0') ? label : "unknown";
  std::snprintf(label_, sizeof(label_), "%s", source);
  const void* self = static_cast<const webrtc::FrameDecryptorInterface*>(this);
  std::lock_guard<std::mutex> lock(LiveDecryptorsMutex());
  LiveDecryptors().push_back(self);
}

LakkoFrameDecryptor::~LakkoFrameDecryptor() {
  {
    const void* self =
        static_cast<const webrtc::FrameDecryptorInterface*>(this);
    std::lock_guard<std::mutex> lock(LiveDecryptorsMutex());
    std::vector<const void*>& live = LiveDecryptors();
    for (std::size_t i = 0; i < live.size(); ++i) {
      if (live[i] == self) {
        live[i] = live.back();
        live.pop_back();
        break;
      }
    }
  }
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
  // Open through the ring's overlap fall-back. The ring reads the
  // 3-byte trailer at the end of the frame, splits it into
  // `clear_prefix || ciphertext_and_tag || sframe_header || trailer`
  // (ADR-0071), reads the composite KID — `(sender_leaf << 8) |
  // stream_index` — from the SFrame header (ADR-0035), derives that
  // sender+stream's key, and — during a mid-call rekey window — tries
  // the previous epoch's key on an AEAD failure (ADR-0029). So the
  // decryptor needs no leaf, stream or codec argument. The additional
  // data is `sframe_header || clear_prefix || trailer` off the wire;
  // the host `additional_data` is intentionally NOT used (mirrors
  // Encrypt; web <-> native interop). A frame that does not split
  // (which includes every frame in the header-first layout of
  // ADR-0013) or does not authenticate under either key surfaces as a
  // non-OK status → the frame is dropped (kFailedToDecrypt), never
  // passed through (invariant #4). The output is `clear_prefix ||
  // plaintext`, always shorter than the input.
  ReportFirstCall(encrypted_frame.size());

  // An empty frame is NOTHING TO DECRYPT, not a failed decryption.
  //
  // Audio silence transmission (DTX) sends frames with no payload by design, so
  // an empty frame is an ordinary thing to receive on a healthy call. Running
  // it through the opener produced a WIRE_PARSE failure — correctly, there is
  // no header to parse — and returning `kFailedToDecrypt` then told libwebrtc a
  // decryption had failed, which is false and counts against the stream. It
  // also spent this decryptor's one diagnostic report on the least informative
  // frame of the call, which is how a live investigation lost a day.
  //
  // Returning zero plaintext bytes is safe: no byte of the input is handed
  // upward, so nothing unauthenticated passes and invariant #4 is untouched —
  // there is no content here to recover or forge.
  if (encrypted_frame.empty()) {
    return Result(Status::kOk, 0);
  }

  uint8_t* pt_ptr = nullptr;
  std::size_t pt_len = 0;
  lakko_sframe_status_t open_status =
      lakko_sframe_call_key_ring_open_media_frame_with_overlap(
          ring_, encrypted_frame.data(), encrypted_frame.size(), &pt_ptr,
          &pt_len);
  if (open_status != LAKKO_SFRAME_STATUS_T_OK) {
    ReportFirstFailure(open_status, encrypted_frame.data(), encrypted_frame.size());
    return Result(Status::kFailedToDecrypt, 0);
  }

  // Copy into libwebrtc's output buffer.
  if (pt_len > frame.size()) {
    // GetMaxPlaintextByteSize undercounted — bug in this file.
    lakko_sframe_buffer_free(pt_ptr, pt_len);
    return Result(Status::kFailedToDecrypt, 0);
  }
  std::memcpy(frame.data(), pt_ptr, pt_len);
  lakko_sframe_buffer_free(pt_ptr, pt_len);

  return Result(Status::kOk, pt_len);
}

void LakkoFrameDecryptor::ReportFirstCall(std::size_t frame_size) {
  if (reported_first_call_.exchange(true, std::memory_order_acq_rel)) {
    return;
  }
  char line[160];
  std::snprintf(
      line, sizeof(line),
      "LakkoFrameDecryptor: first frame reached decrypt: label=%s bytes=%zu",
      label_, frame_size);
  EmitLog(line);
}

void LakkoFrameDecryptor::ReportFirstFailure(
    lakko_sframe_status_t status, const uint8_t* frame_data,
    std::size_t frame_size) {
  // An empty frame never reaches here — `Decrypt` returns it as "nothing to
  // decrypt" before the opener runs — so every report below describes a frame
  // that genuinely carried bytes and genuinely failed to open.
  //
  // Once per decryptor. `exchange` is the whole guard: a racing second frame on
  // another thread sees `true` and returns.
  if (reported_failure_.exchange(true, std::memory_order_acq_rel)) {
    return;
  }
  // Re-read the frame for the values that name WHICH failure this is: the
  // lengths its trailer claims, and — when those are consistent — the KID the
  // SFrame header carries (`(sender_leaf << 8) | stream_index`, ADR-0035) and
  // the frame counter. A wrong key, a KID the receiver cannot derive for, and
  // a frame that is not in this layout at all are three different bugs with
  // three different fixes, and the status code alone does not separate them.
  // The read is independent of the open that just failed, so it still answers
  // when the open did not.
  //
  // Layout (ADR-0071): `clear_prefix || ciphertext_and_tag || sframe_header ||
  // trailer`, trailer = `clear_prefix_len || sframe_header_len || version`.
  // No key material and no frame bytes are logged: a KID is a routing
  // identifier and a counter is a sequence number, and neither is secret.
  char line[256];
  bool reported = false;
  if (frame_size >= kTrailerBytes) {
    const std::size_t clear_len = frame_data[frame_size - 3];
    const std::size_t header_len = frame_data[frame_size - 2];
    const unsigned version = frame_data[frame_size - 1];
    const bool lengths_fit =
        header_len >= 1 && header_len <= kMaxSframeHeaderBytes &&
        clear_len + kAeadTagBytes + header_len + kTrailerBytes <= frame_size;
    if (lengths_fit) {
      // The header decoder takes a header-first byte string, so hand it the
      // tail of the frame starting at the header; what follows the header
      // there is the trailer, which it returns as a 3-byte body to free. The
      // counter is 64-bit (ADR-0069).
      const uint8_t* header = frame_data + frame_size - kTrailerBytes - header_len;
      uint32_t kid = 0;
      uint64_t counter = 0;
      uint8_t* rest = nullptr;
      std::size_t rest_len = 0;
      lakko_sframe_status_t parse_status = lakko_sframe_unwrap_frame(
          header, header_len + kTrailerBytes, &kid, &counter, &rest, &rest_len);
      if (parse_status == LAKKO_SFRAME_STATUS_T_OK) {
        lakko_sframe_buffer_free(rest, rest_len);
        std::snprintf(
            line, sizeof(line),
            "LakkoFrameDecryptor: first decrypt failure: label=%s status=%s(%d) "
            "kid=%u (leaf=%u stream=%u) counter=%llu clearBytes=%zu "
            "frameBytes=%zu",
            label_, StatusName(status), static_cast<int>(status), kid, kid >> 8,
            kid & 0xFFu, static_cast<unsigned long long>(counter), clear_len,
            frame_size);
        reported = true;
      }
    }
    if (!reported) {
      // The trailer does not describe this frame, or the header it points at
      // does not parse — which is itself the answer: what arrived is not a
      // frame in this layout.
      std::snprintf(
          line, sizeof(line),
          "LakkoFrameDecryptor: first decrypt failure: label=%s status=%s(%d) "
          "layoutUnreadable trailer=(clear=%zu header=%zu version=%u) "
          "frameBytes=%zu",
          label_, StatusName(status), static_cast<int>(status), clear_len,
          header_len, version, frame_size);
      reported = true;
    }
  }
  if (!reported) {
    std::snprintf(
        line, sizeof(line),
        "LakkoFrameDecryptor: first decrypt failure: label=%s status=%s(%d) "
        "shorterThanTrailer frameBytes=%zu",
        label_, StatusName(status), static_cast<int>(status), frame_size);
  }
  EmitLog(line);
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
  // The opened frame (`clear_prefix || plaintext`) is strictly
  // shorter than the sealed one (no tag, no header, no trailer).
  // Conservatively return the input size — over-allocation is
  // harmless; under-allocation truncates frames.
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
