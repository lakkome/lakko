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
//   4. Hold the one fact the frame layout needs that libwebrtc's
//      encryptor interface does not carry: which codec a video sender
//      negotiated (`SetCodec`, ADR-0071).
//
// ## Frame layout (ADR-0071)
//
// Every frame is sealed as
//
//   clear_prefix || ciphertext_and_tag || sframe_header || trailer
//
// where `clear_prefix` is the codec's own frame header (10 bytes of a
// VP8 key frame, 3 of any other VP8 frame, nothing for Opus or for a
// codec without a rule), so that a receiver with no generic frame
// descriptor still finds key frames. Every byte that is not ciphertext
// is authenticated. The header-first layout of ADR-0013 is neither
// produced nor accepted here: a frame in it fails to open.
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
//   - The encryptor's codec is a `std::atomic<uint8_t>`: `SetCodec`
//     stores it, Encrypt loads it once per frame. A frame in flight
//     during a `SetCodec` is sealed under one value or the other, and
//     either opens: the receiver reads the split from the frame's own
//     trailer, never from a codec.
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
  // `stream_index` is the local send track's per-sender stream ordinal
  // (0 = audio, 1 = first video, 2 = second video / screen-share, …;
  // ADR-0035). The seal KID is composed `(local_leaf << 8) |
  // stream_index` and folded whole into the key derivation, so every
  // one of a participant's concurrent streams keys into a disjoint
  // `(key, nonce_salt)` space. An encryptor is bound to exactly one
  // stream for its lifetime; the platform shim passes the ordinal of
  // the track it is attaching to. Every `uint8_t` is a valid stream
  // index, so there is no fail-closed rejection on this argument.
  static LakkoFrameEncryptor* Create(const uint8_t* seed,
                                     std::size_t seed_len,
                                     uint32_t local_leaf,
                                     uint8_t stream_index);

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

  // Tell the encryptor which codec its sender negotiated (ADR-0071), as
  // one of the `LAKKO_SFRAME_MEDIA_CODEC_*` values of the C ABI. It
  // decides how many leading bytes of each frame stay in the clear. A
  // value the C ABI does not know is stored and treated as OTHER.
  //
  // libwebrtc's `Encrypt` names a media type and no codec, so the
  // platform shim calls this once the sender's parameters name one (the
  // first codec after negotiation) and again when a renegotiation
  // changes it. Until then the encryptor seals as OTHER: nothing clear,
  // the frame still opens, and a receiver with no frame descriptor
  // cannot find its key frames.
  //
  // A frame libwebrtc hands over as audio is always sealed with nothing
  // clear, whatever was set here.
  //
  // Callable from any thread, concurrently with Encrypt.
  void SetCodec(uint8_t codec);

  // The value last given to `SetCodec` (OTHER before the first call).
  uint8_t Codec() const;

  // Map a negotiated codec name to a `LAKKO_SFRAME_MEDIA_CODEC_*` value.
  // Accepts the bare name libwebrtc's sender parameters carry ("VP8",
  // "opus") and the MIME form ("video/VP8", "audio/opus"), without
  // regard to case. Anything else, null and the RED / RTX / ULPFEC
  // wrappers included, is OTHER.
  static uint8_t CodecFromName(const char* name);

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
                      uint8_t stream_index);

  // Owned. The seal key for the CURRENT epoch; `seal_media_frame` emits
  // the composite KID (`(local_leaf << 8) | stream_index`) into the
  // SFrame header internally (ADR-0035 / ADR-0071). Guarded by
  // `seal_mutex_` because Encrypt (codec thread) reads it while Rekey
  // (signaling thread) swaps it.
  lakko_sframe_participant_key_t* participant_key_;
  // The local leaf index, kept so Rekey can re-derive the seal key for
  // the same leaf under the new epoch.
  uint32_t local_leaf_;
  // The send track's per-sender stream ordinal (ADR-0035), kept so
  // Rekey re-derives the seal key on the SAME stream under the new
  // epoch.
  uint8_t stream_index_;
  mutable std::mutex seal_mutex_;

  // The sender's negotiated codec (ADR-0071); OTHER until `SetCodec`.
  std::atomic<uint8_t> codec_{LAKKO_SFRAME_MEDIA_CODEC_OTHER};

  mutable std::atomic<int> refcount_{0};
};

// ---------------------------------------------------------------
// Decryptor
// ---------------------------------------------------------------

// A host-supplied sink for the bridge's rare diagnostic lines.
//
// The bridge is shared by iOS and Android and no libwebrtc logging header is
// vendored here, so it cannot pick a platform logger of its own. The host
// installs one (`NSLog` on iOS, `__android_log_print` on Android) and the
// bridge stays platform-neutral. `nullptr` — the default — discards.
//
// `line` is plain ASCII, owned by the bridge, and valid only for the duration
// of the call. The sink may run on a libwebrtc worker thread.
using LogSink = void (*)(const char* line);

// Install (or, with nullptr, remove) the diagnostic sink. Safe to call at any
// time; the last writer wins.
void SetLogSink(LogSink sink);

class LakkoFrameDecryptor : public webrtc::FrameDecryptorInterface {
 public:
  // Construct from the same 32-byte per-call seed the encryptor
  // uses on every other participant. Returns nullptr on bad seed
  // length. The decryptor opens each frame through the call-key
  // ring, which reads the composite KID from the frame's own SFrame
  // header (ADR-0035; the header sits before the 3-byte trailer at the
  // end of the frame, ADR-0071) — the single per-stream discriminator that
  // survives the SFU — derives that sender+stream's key internally,
  // and, during a mid-call rekey overlap window, tries the previous
  // epoch's key on an AEAD failure (ADR-0029).
  //
  // The decryptor takes NO stream argument: a single decryptor bound
  // to one remote receive track opens every frame on it by reading the
  // KID per frame, so audio and either video stream are each keyed
  // correctly with no local media-type or MID signal.
  //
  // `label` names which receive track this decryptor serves ("audio",
  // "video"), for the diagnostic lines only. A live call left it ambiguous
  // which of two decryptors a failure came from — the line was timestamped
  // twelve seconds before the video decryptor even existed, so it had to be the
  // audio one, which nobody could tell from the line itself. Defaulted so the
  // other platform's call sites compile unchanged.
  static LakkoFrameDecryptor* Create(const uint8_t* seed,
                                     std::size_t seed_len,
                                     const char* label = nullptr);

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
  LakkoFrameDecryptor(lakko_sframe_call_key_ring_t* ring, const char* label);

  // Emit one line the first time ANY frame reaches this decryptor.
  //
  // This is the line that separates "the decryptor is rejecting frames" from
  // "the decryptor is never called". A live call showed 108 packets received
  // and 27 frames dropped with no failure line from the video decryptor at all,
  // which is only explicable if `Decrypt` was never reached for video — and no
  // amount of failure logging can say that, because the absence of a failure
  // and the absence of a call look identical.
  void ReportFirstCall(std::size_t frame_size);

  // Emit one line naming why the FIRST frame failed to open: the status, the
  // KID read off the frame's own header (split into leaf and stream), and the
  // counter. Subsequent failures are silent.
  //
  // Takes a raw `(pointer, size)` rather than an `rtc::ArrayView` on purpose.
  // Constructing an ArrayView in OUR code instantiates its checked constructor,
  // which references `rtc::webrtc_checks_impl::FatalLog` — a symbol that lives
  // inside the precompiled WebRTC framework and is not linked into this bridge,
  // so the app fails at link. Receiving an ArrayView that libwebrtc built (as
  // `Decrypt` does) is fine; building one is not.
  void ReportFirstFailure(lakko_sframe_status_t status,
                          const uint8_t* frame_data, std::size_t frame_size);

  // Owned. The overlap keyer (current + previous call key); ADR-0029.
  // Its internal Rust `Mutex` makes open / rekey / prune thread-safe.
  // The per-frame open reads the composite KID from the frame's header
  // (ADR-0035), so the decryptor holds no per-stream state.
  lakko_sframe_call_key_ring_t* ring_;

  // Which track this decryptor serves, for the diagnostic lines. A bounded
  // copy: the caller's string is not owned and the decryptor outlives the
  // attach call.
  char label_[16];

  // At most three diagnostic lines per decryptor, never per frame. A decryptor
  // that cannot open the first frame cannot open the thousandth either, and a
  // per-frame log on the network thread at 30 fps is its own outage.
  //
  // A zero-length frame never reaches the failure path at all: `Decrypt`
  // returns it as "nothing to decrypt" before the opener runs, because an audio
  // DTX gap is empty by design and is not an error.
  std::atomic<bool> reported_first_call_{false};
  std::atomic<bool> reported_failure_{false};

  mutable std::atomic<int> refcount_{0};
};

}  // namespace sframe
}  // namespace lakko

#endif  // LAKKO_SFRAME_BRIDGE_H_
