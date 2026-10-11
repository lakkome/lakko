// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors
//
// Standalone test for the N-3 C++ subclass. Pure C++17 — no gtest, no
// libwebrtc runtime — to keep the build chain short: cargo build the
// staticlib, c++ compile + link, run.
//
// What it proves, all in the ADR-0071 frame layout
// (`clear_prefix || ciphertext_and_tag || sframe_header || trailer`):
//   1. LakkoFrameEncryptor::Encrypt produces a sealed frame that fits the
//      reported max size (frame + 36), and LakkoFrameDecryptor::Decrypt
//      recovers the original frame, never longer than its input.
//   2. One decryptor serves N senders (the SFU forwarding shape), keyed by
//      the KID each frame carries.
//   3. The host `additional_data` is not authenticated; every byte of the
//      sealed frame is (clear prefix, ciphertext, tag, header, trailer).
//   4. The codec decides the clear prefix: a VP8 key frame keeps 10 bytes,
//      any other VP8 frame 3, audio / OTHER / an unknown value none.
//   5. The canonical ADR-0071 golden frame opens; the header-first layout
//      of ADR-0013 is refused.
//   6. `SetCodec` may race `Encrypt`; a mid-call rekey keeps its overlap.
//   7. Refcount machinery — AddRef + Release reach 0 and free.

#include "sframe-bridge.h"

#include <array>
#include <atomic>
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <thread>
#include <utility>
#include <vector>

namespace {

constexpr std::size_t kSeedLen = 32;

// Decode a lowercase-hex string into bytes (golden-vector helper).
std::vector<uint8_t> hex_decode(const char* hex) {
  auto nibble = [](char c) -> int {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return 0;
  };
  std::vector<uint8_t> out;
  for (std::size_t i = 0; hex[i] != '\0' && hex[i + 1] != '\0'; i += 2) {
    out.push_back(static_cast<uint8_t>((nibble(hex[i]) << 4) | nibble(hex[i + 1])));
  }
  return out;
}

// Mock seed — in production this comes from
// `Group::export_secret("lakko/call/v1", call_id, 32)`.
constexpr std::array<uint8_t, kSeedLen> kTestSeed = {
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
    0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
    0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18,
    0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
};

using DecryptStatus = webrtc::FrameDecryptorInterface::Status;

constexpr std::size_t kTagBytes = 16;
constexpr std::size_t kTrailerBytes = 3;

// Seal `frame` through the encryptor exactly as libwebrtc would: size the
// output with GetMaxCiphertextByteSize, call Encrypt, cut to what was
// written.
std::vector<uint8_t> seal_frame(lakko::sframe::LakkoFrameEncryptor* enc,
                                cricket::MediaType media_type,
                                const std::vector<uint8_t>& frame) {
  const std::size_t max = enc->GetMaxCiphertextByteSize(media_type, frame.size());
  assert(max == frame.size() + 36 &&
         "a sealed frame is allowed tag 16 + header 17 + trailer 3");
  std::vector<uint8_t> wire(max);
  std::size_t written = 0;
  int rc = enc->Encrypt(media_type, /*ssrc=*/0,
                        rtc::ArrayView<const uint8_t>(),
                        rtc::MakeArrayView(frame.data(), frame.size()),
                        rtc::MakeArrayView(wire.data(), wire.size()), &written);
  assert(rc == 0 && "Encrypt failed");
  assert(written <= wire.size());
  wire.resize(written);
  return wire;
}

// Open `wire` through the decryptor exactly as libwebrtc would. On failure
// the returned vector is empty.
std::pair<DecryptStatus, std::vector<uint8_t>> open_frame(
    lakko::sframe::LakkoFrameDecryptor* dec,
    cricket::MediaType media_type,
    const std::vector<uint8_t>& wire) {
  const std::size_t max = dec->GetMaxPlaintextByteSize(media_type, wire.size());
  assert(max <= wire.size() && "the opened frame never exceeds the sealed one");
  std::vector<uint8_t> out(max);
  auto result = dec->Decrypt(media_type, std::vector<uint32_t>{},
                             rtc::ArrayView<const uint8_t>(),
                             rtc::MakeArrayView(wire.data(), wire.size()),
                             rtc::MakeArrayView(out.data(), out.size()));
  if (result.status != DecryptStatus::kOk) {
    assert(result.bytes_written == 0 &&
           "a frame that does not open hands no bytes upward");
    return {result.status, {}};
  }
  assert(result.bytes_written <= wire.size());
  out.resize(result.bytes_written);
  return {result.status, out};
}

// The three trailer fields of a sealed frame (ADR-0071).
struct Trailer {
  std::size_t clear_len;
  std::size_t header_len;
  uint8_t version;
};

Trailer trailer_of(const std::vector<uint8_t>& wire) {
  assert(wire.size() >= kTrailerBytes + 1 + kTagBytes);
  return Trailer{wire[wire.size() - 3], wire[wire.size() - 2],
                 wire[wire.size() - 1]};
}

// Read the KID and the counter out of a sealed frame's SFrame header, which
// sits just before the trailer. The header decoder of the C ABI takes a
// header-first byte string, so it is handed the tail of the frame.
void header_of(const std::vector<uint8_t>& wire, uint32_t* kid,
               uint64_t* counter) {
  const Trailer t = trailer_of(wire);
  assert(t.version == 0x01);
  assert(t.header_len >= 1 && t.header_len <= 17);
  assert(t.clear_len + kTagBytes + t.header_len + kTrailerBytes <= wire.size());
  const uint8_t* header =
      wire.data() + wire.size() - kTrailerBytes - t.header_len;
  uint8_t* rest = nullptr;
  std::size_t rest_len = 0;
  lakko_sframe_status_t parse = lakko_sframe_unwrap_frame(
      header, t.header_len + kTrailerBytes, kid, counter, &rest, &rest_len);
  assert(parse == LAKKO_SFRAME_STATUS_T_OK &&
         "the sealed frame's header did not parse");
  assert(rest_len == kTrailerBytes &&
         "the header is exactly as long as the trailer says");
  lakko_sframe_buffer_free(rest, rest_len);
}

// A VP8 key frame: frame tag with the key-frame bit clear, the start code
// 9d 01 2a, 640x480, then a body. The same bytes the ADR-0071 golden seals.
std::vector<uint8_t> vp8_key_frame() {
  std::vector<uint8_t> frame = {0x10, 0x02, 0x00, 0x9d, 0x01,
                                0x2a, 0x80, 0x02, 0xe0, 0x01};
  const char* body = "lakko media frame golden v1";
  frame.insert(frame.end(), body, body + std::strlen(body));
  return frame;
}

// A VP8 frame that is not a key frame: the low bit of the first byte is set.
std::vector<uint8_t> vp8_delta_frame() {
  std::vector<uint8_t> frame = {0x11, 0x02, 0x00};
  const char* body = "a delta frame body, longer than a tag";
  frame.insert(frame.end(), body, body + std::strlen(body));
  return frame;
}

void test_round_trip_single_sender() {
  std::printf("test_round_trip_single_sender ... ");

  constexpr uint32_t kAliceLeaf = 7;
  auto* enc =
      lakko::sframe::LakkoFrameEncryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size(),
                                                 kAliceLeaf, /*stream_index=*/0);
  assert(enc != nullptr && "Encryptor::Create returned nullptr");
  enc->AddRef();  // refcount: 0 -> 1

  auto* dec =
      lakko::sframe::LakkoFrameDecryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size());
  assert(dec != nullptr && "Decryptor::Create returned nullptr");
  dec->AddRef();

  const std::vector<uint8_t> plaintext = {
      'h', 'e', 'l', 'l', 'o', ' ', 'l', 'a', 'k', 'k', 'o',
  };
  const std::vector<uint8_t> aad = {0xAA, 0xBB, 0xCC, 0xDD};

  std::vector<uint8_t> ciphertext(enc->GetMaxCiphertextByteSize(
      cricket::MEDIA_TYPE_AUDIO, plaintext.size()));
  std::size_t ct_written = 0;
  int rc = enc->Encrypt(cricket::MEDIA_TYPE_AUDIO,
                        /*ssrc=*/0xDEADBEEF,
                        rtc::MakeArrayView(aad.data(), aad.size()),
                        rtc::MakeArrayView(plaintext.data(), plaintext.size()),
                        rtc::MakeArrayView(ciphertext.data(), ciphertext.size()),
                        &ct_written);
  assert(rc == 0 && "Encrypt failed");
  assert(ct_written > 0);
  assert(ct_written <= ciphertext.size());
  assert(ciphertext.size() == plaintext.size() + 36 &&
         "GetMaxCiphertextByteSize allows tag 16 + header 17 + trailer 3");
  // Audio keeps nothing clear: the frame is ciphertext+tag, header, trailer.
  assert(ciphertext[ct_written - 1] == 0x01 && "trailer version");
  assert(ciphertext[ct_written - 3] == 0 && "audio keeps no clear prefix");
  assert(ct_written ==
         plaintext.size() + 16 + ciphertext[ct_written - 2] + 3);

  std::vector<uint8_t> recovered(dec->GetMaxPlaintextByteSize(
      cricket::MEDIA_TYPE_AUDIO, ct_written));
  auto result = dec->Decrypt(
      cricket::MEDIA_TYPE_AUDIO,
      /*csrcs=*/std::vector<uint32_t>{},
      rtc::MakeArrayView(aad.data(), aad.size()),
      rtc::MakeArrayView(ciphertext.data(), ct_written),
      rtc::MakeArrayView(recovered.data(), recovered.size()));
  assert(result.status == webrtc::FrameDecryptorInterface::Status::kOk &&
         "Decrypt did not return kOk");
  assert(result.bytes_written == plaintext.size());
  assert(std::memcmp(recovered.data(), plaintext.data(), plaintext.size()) == 0 &&
         "Recovered plaintext does not match");

  // Release: refcount 1 -> 0, should `delete this`.
  assert(enc->Release() ==
         rtc::RefCountReleaseStatus::kDroppedLastRef);
  assert(dec->Release() ==
         rtc::RefCountReleaseStatus::kDroppedLastRef);
  std::printf("ok\n");
}

// The host RTP `additional_data` is NOT part of the AEAD additional data
// (ADR-0013, kept by ADR-0071); the frame's own SFrame header is. This test
// proves both halves:
//   (a) host `additional_data` is IGNORED — encrypt and decrypt with
//       deliberately mismatched additional_data still round-trips. This is
//       the property that lets a web RTCRtpScriptTransform frame decrypt
//       verbatim on iOS/Android (and vice versa): the host RTP metadata is
//       not reconstructible across platforms, so it cannot be part of the
//       authenticated set.
//   (b) the SFrame header IS bound — flipping a header byte makes the open
//       fail (kFailedToDecrypt).
void test_additional_data_ignored_header_is_aad() {
  std::printf("test_additional_data_ignored_header_is_aad ... ");

  constexpr uint32_t kAliceLeaf = 7;
  auto* enc =
      lakko::sframe::LakkoFrameEncryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size(),
                                                 kAliceLeaf, /*stream_index=*/0);
  auto* dec =
      lakko::sframe::LakkoFrameDecryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size());
  enc->AddRef();
  dec->AddRef();

  const std::vector<uint8_t> plaintext = {1, 2, 3, 4, 5};
  const std::vector<uint8_t> aad_enc = {0x11, 0x22};
  const std::vector<uint8_t> aad_dec_mismatch = {0x99, 0x88, 0x77};

  std::vector<uint8_t> ciphertext(enc->GetMaxCiphertextByteSize(
      cricket::MEDIA_TYPE_VIDEO, plaintext.size()));
  std::size_t ct_written = 0;
  int rc = enc->Encrypt(cricket::MEDIA_TYPE_VIDEO,
                        /*ssrc=*/0x1234,
                        rtc::MakeArrayView(aad_enc.data(), aad_enc.size()),
                        rtc::MakeArrayView(plaintext.data(), plaintext.size()),
                        rtc::MakeArrayView(ciphertext.data(), ciphertext.size()),
                        &ct_written);
  assert(rc == 0);

  // (a) Decrypt with a DIFFERENT host additional_data — must SUCCEED,
  //     because host additional_data is not part of the AAD.
  std::vector<uint8_t> recovered(dec->GetMaxPlaintextByteSize(
      cricket::MEDIA_TYPE_VIDEO, ct_written));
  auto result = dec->Decrypt(
      cricket::MEDIA_TYPE_VIDEO,
      std::vector<uint32_t>{},
      rtc::MakeArrayView(aad_dec_mismatch.data(), aad_dec_mismatch.size()),
      rtc::MakeArrayView(ciphertext.data(), ct_written),
      rtc::MakeArrayView(recovered.data(), recovered.size()));
  assert(result.status == webrtc::FrameDecryptorInterface::Status::kOk &&
         "mismatched host additional_data must NOT fail decryption "
         "(ADR-0013: header-as-AAD, not host AAD)");
  assert(result.bytes_written == plaintext.size());
  assert(std::memcmp(recovered.data(), plaintext.data(),
                     plaintext.size()) == 0);

  // (b) Flip the first byte of the SFrame header (its config octet —
  //     part of the bound additional data) and decrypt — must fail.
  //     Either the header decode rejects it or the AEAD tag check does;
  //     both surface as kFailedToDecrypt. The header sits just before
  //     the 3-byte trailer, which states its length.
  std::vector<uint8_t> tampered_wire(ciphertext.begin(),
                                     ciphertext.begin() +
                                         static_cast<std::ptrdiff_t>(ct_written));
  const std::size_t header_len = tampered_wire[tampered_wire.size() - 2];
  tampered_wire[tampered_wire.size() - 3 - header_len] ^= 0x01;
  std::vector<uint8_t> recovered2(dec->GetMaxPlaintextByteSize(
      cricket::MEDIA_TYPE_VIDEO, tampered_wire.size()));
  auto result2 = dec->Decrypt(
      cricket::MEDIA_TYPE_VIDEO,
      std::vector<uint32_t>{},
      rtc::MakeArrayView(aad_enc.data(), aad_enc.size()),
      rtc::MakeArrayView(tampered_wire.data(), tampered_wire.size()),
      rtc::MakeArrayView(recovered2.data(), recovered2.size()));
  assert(result2.status ==
             webrtc::FrameDecryptorInterface::Status::kFailedToDecrypt &&
         "header-tampered Decrypt unexpectedly returned kOk "
         "(the SFrame header IS in the bound additional data)");

  enc->Release();
  dec->Release();
  std::printf("ok\n");
}

void test_multi_sender_into_one_receiver() {
  std::printf("test_multi_sender_into_one_receiver ... ");

  constexpr uint32_t kAliceLeaf = 1;
  constexpr uint32_t kBobLeaf = 2;
  constexpr uint32_t kCarolLeaf = 3;

  auto* enc_alice =
      lakko::sframe::LakkoFrameEncryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size(),
                                                 kAliceLeaf, /*stream_index=*/0);
  auto* enc_bob =
      lakko::sframe::LakkoFrameEncryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size(),
                                                 kBobLeaf, /*stream_index=*/0);
  auto* enc_carol =
      lakko::sframe::LakkoFrameEncryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size(),
                                                 kCarolLeaf, /*stream_index=*/0);

  // One decryptor receives forwarded frames from all three.
  auto* dec =
      lakko::sframe::LakkoFrameDecryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size());

  enc_alice->AddRef();
  enc_bob->AddRef();
  enc_carol->AddRef();
  dec->AddRef();

  struct Sender {
    lakko::sframe::LakkoFrameEncryptor* enc;
    std::vector<uint8_t> plaintext;
  };
  std::vector<Sender> senders = {
      {enc_alice, {'A', 'L', 'I', 'C', 'E'}},
      {enc_bob, {'B', 'O', 'B'}},
      {enc_carol, {'C', 'A', 'R', 'O', 'L', '!'}},
  };

  const std::vector<uint8_t> aad = {0xFF};

  for (auto& s : senders) {
    std::vector<uint8_t> ct(s.enc->GetMaxCiphertextByteSize(
        cricket::MEDIA_TYPE_AUDIO, s.plaintext.size()));
    std::size_t ct_written = 0;
    int rc = s.enc->Encrypt(
        cricket::MEDIA_TYPE_AUDIO, /*ssrc=*/0,
        rtc::MakeArrayView(aad.data(), aad.size()),
        rtc::MakeArrayView(s.plaintext.data(), s.plaintext.size()),
        rtc::MakeArrayView(ct.data(), ct.size()),
        &ct_written);
    assert(rc == 0);

    std::vector<uint8_t> recovered(dec->GetMaxPlaintextByteSize(
        cricket::MEDIA_TYPE_AUDIO, ct_written));
    auto result = dec->Decrypt(
        cricket::MEDIA_TYPE_AUDIO, std::vector<uint32_t>{},
        rtc::MakeArrayView(aad.data(), aad.size()),
        rtc::MakeArrayView(ct.data(), ct_written),
        rtc::MakeArrayView(recovered.data(), recovered.size()));
    assert(result.status == webrtc::FrameDecryptorInterface::Status::kOk);
    assert(result.bytes_written == s.plaintext.size());
    assert(std::memcmp(recovered.data(), s.plaintext.data(), s.plaintext.size()) == 0);
  }

  // The decryptor cache should hold three entries now. We don't
  // have a public accessor; the test relies on coverage that
  // all three round-trips succeeded — which is only possible
  // if `lookup_or_derive` correctly derived one ParticipantKey
  // per distinct sender_leaf observed.

  enc_alice->Release();
  enc_bob->Release();
  enc_carol->Release();
  dec->Release();
  std::printf("ok\n");
}

void test_bad_seed_length() {
  std::printf("test_bad_seed_length ... ");

  // Below CallKey::CALL_KEY_LEN.
  const std::array<uint8_t, 8> short_seed{};
  auto* enc =
      lakko::sframe::LakkoFrameEncryptor::Create(short_seed.data(),
                                                 short_seed.size(),
                                                 0, /*stream_index=*/0);
  assert(enc == nullptr && "Encryptor::Create should reject short seed");

  auto* dec =
      lakko::sframe::LakkoFrameDecryptor::Create(short_seed.data(),
                                                 short_seed.size());
  assert(dec == nullptr && "Decryptor::Create should reject short seed");

  std::printf("ok\n");
}

// Tampering inside the ciphertext body — flip a single bit in the
// middle of the ciphertext. AEAD tag verification must catch it.
void test_ciphertext_tamper() {
  std::printf("test_ciphertext_tamper ... ");

  constexpr uint32_t kAliceLeaf = 11;
  auto* enc =
      lakko::sframe::LakkoFrameEncryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size(),
                                                 kAliceLeaf, /*stream_index=*/0);
  auto* dec =
      lakko::sframe::LakkoFrameDecryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size());
  enc->AddRef();
  dec->AddRef();

  // Plaintext deliberately > 16B so the body slot has bytes that
  // are not the AEAD tag (tag is the last 16B). We flip a byte
  // somewhere in the middle to exercise the body authentication
  // path rather than only the tag.
  const std::vector<uint8_t> plaintext = {
      'l', 'a', 'k', 'k', 'o', '-', 's', 'f', 'r', 'a', 'm', 'e',
      '-', 'b', 'r', 'i', 'd', 'g', 'e', '-', 't', 'a', 'm', 'p',
      'e', 'r', '-', 't', 'e', 's', 't', '!',
  };
  const std::vector<uint8_t> aad = {0x55, 0xAA};

  std::vector<uint8_t> ciphertext(enc->GetMaxCiphertextByteSize(
      cricket::MEDIA_TYPE_AUDIO, plaintext.size()));
  std::size_t ct_written = 0;
  int rc = enc->Encrypt(cricket::MEDIA_TYPE_AUDIO,
                        /*ssrc=*/0xCAFE,
                        rtc::MakeArrayView(aad.data(), aad.size()),
                        rtc::MakeArrayView(plaintext.data(), plaintext.size()),
                        rtc::MakeArrayView(ciphertext.data(), ciphertext.size()),
                        &ct_written);
  assert(rc == 0);
  // AEAD tag is the trailing 16 bytes (ChaCha20-Poly1305 / AES-GCM).
  constexpr std::size_t kAeadTagBytes = 16;
  assert(ct_written > kAeadTagBytes && "ct must be larger than AEAD tag");

  // Audio keeps no clear prefix, so the sealed frame starts with the
  // ciphertext of the 32 plaintext bytes; the tag, the header and the
  // trailer follow. The middle of the plaintext's span is a body byte.
  const std::size_t mid = plaintext.size() / 2;
  ciphertext[mid] ^= 0x40;  // single-bit flip is enough for AEAD

  std::vector<uint8_t> recovered(dec->GetMaxPlaintextByteSize(
      cricket::MEDIA_TYPE_AUDIO, ct_written));
  auto result = dec->Decrypt(
      cricket::MEDIA_TYPE_AUDIO,
      std::vector<uint32_t>{},
      rtc::MakeArrayView(aad.data(), aad.size()),
      rtc::MakeArrayView(ciphertext.data(), ct_written),
      rtc::MakeArrayView(recovered.data(), recovered.size()));
  assert(result.status ==
             webrtc::FrameDecryptorInterface::Status::kFailedToDecrypt &&
         "Ciphertext-body tamper unexpectedly returned kOk");

  enc->Release();
  dec->Release();
  std::printf("ok\n");
}

// Encrypt N frames with the same encryptor — verify every one
// encrypts successfully + every one decrypts on the receiver side
// regardless of arrival order. SFrame counters live in the wire
// header; the decryptor parses each frame's own counter and does
// NOT require monotonic-order arrival across the wire.
void test_counter_monotonicity_and_out_of_order() {
  std::printf("test_counter_monotonicity_and_out_of_order ... ");

  constexpr uint32_t kAliceLeaf = 21;
  constexpr std::size_t kFrameCount = 1000;

  auto* enc =
      lakko::sframe::LakkoFrameEncryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size(),
                                                 kAliceLeaf, /*stream_index=*/0);
  auto* dec =
      lakko::sframe::LakkoFrameDecryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size());
  enc->AddRef();
  dec->AddRef();

  // Each frame's plaintext encodes its own sequence number so we
  // can verify out-of-order delivery still recovers the correct
  // plaintext.
  struct WireFrame {
    std::vector<uint8_t> ct;
    std::size_t expected_plaintext_seq;
  };
  std::vector<WireFrame> wire;
  wire.reserve(kFrameCount);

  const std::vector<uint8_t> aad = {0x01, 0x02, 0x03, 0x04};

  for (std::size_t i = 0; i < kFrameCount; ++i) {
    // 8-byte plaintext = big-endian i. Independent of any wire
    // counter the encryptor adds.
    std::array<uint8_t, 8> pt{};
    for (int b = 0; b < 8; ++b) {
      pt[7 - b] = static_cast<uint8_t>((i >> (b * 8)) & 0xFFu);
    }

    std::vector<uint8_t> ct(enc->GetMaxCiphertextByteSize(
        cricket::MEDIA_TYPE_VIDEO, pt.size()));
    std::size_t ct_written = 0;
    int rc = enc->Encrypt(cricket::MEDIA_TYPE_VIDEO, /*ssrc=*/0,
                          rtc::MakeArrayView(aad.data(), aad.size()),
                          rtc::MakeArrayView(pt.data(), pt.size()),
                          rtc::MakeArrayView(ct.data(), ct.size()),
                          &ct_written);
    assert(rc == 0 && "Encrypt failed mid-stream");
    ct.resize(ct_written);
    wire.push_back(WireFrame{std::move(ct), i});
  }

  // Fixed-seed deterministic shuffle (Math.random is forbidden in
  // CC scripts and we want reproducible CI). Pairwise rotate
  // mod-prime walk — covers every index uniformly within one pass.
  for (std::size_t i = 0; i < wire.size(); ++i) {
    const std::size_t j = (i * 7919u + 31u) % wire.size();
    std::swap(wire[i], wire[j]);
  }

  // Decrypt in shuffled order.
  for (const auto& w : wire) {
    std::vector<uint8_t> recovered(dec->GetMaxPlaintextByteSize(
        cricket::MEDIA_TYPE_VIDEO, w.ct.size()));
    auto result = dec->Decrypt(
        cricket::MEDIA_TYPE_VIDEO, std::vector<uint32_t>{},
        rtc::MakeArrayView(aad.data(), aad.size()),
        rtc::MakeArrayView(w.ct.data(), w.ct.size()),
        rtc::MakeArrayView(recovered.data(), recovered.size()));
    assert(result.status == webrtc::FrameDecryptorInterface::Status::kOk &&
           "Decrypt failed for an out-of-order frame");
    assert(result.bytes_written == 8);

    std::size_t recovered_seq = 0;
    for (int b = 0; b < 8; ++b) {
      recovered_seq = (recovered_seq << 8) | recovered[b];
    }
    assert(recovered_seq == w.expected_plaintext_seq &&
           "Out-of-order decrypt returned wrong plaintext");
  }

  enc->Release();
  dec->Release();
  std::printf("ok\n");
}

// Two distinct CallKey instances seeded with different bytes —
// frames encrypted under seed_A must not decrypt under seed_B.
// Mirrors the per-call key isolation invariant exporter labels
// guarantee at the MLS layer.
void test_per_call_key_isolation() {
  std::printf("test_per_call_key_isolation ... ");

  constexpr uint32_t kAliceLeaf = 31;

  std::array<uint8_t, kSeedLen> seed_b = kTestSeed;
  seed_b[0] ^= 0xFFu;  // diverge in the first byte → distinct CallKey

  auto* enc_a =
      lakko::sframe::LakkoFrameEncryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size(),
                                                 kAliceLeaf, /*stream_index=*/0);
  auto* dec_b =
      lakko::sframe::LakkoFrameDecryptor::Create(seed_b.data(),
                                                 seed_b.size());
  enc_a->AddRef();
  dec_b->AddRef();

  const std::vector<uint8_t> plaintext = {'i', 's', 'o', 'l', 'a', 't', 'e'};
  const std::vector<uint8_t> aad = {0x99};

  std::vector<uint8_t> ciphertext(enc_a->GetMaxCiphertextByteSize(
      cricket::MEDIA_TYPE_AUDIO, plaintext.size()));
  std::size_t ct_written = 0;
  int rc = enc_a->Encrypt(cricket::MEDIA_TYPE_AUDIO, /*ssrc=*/0,
                          rtc::MakeArrayView(aad.data(), aad.size()),
                          rtc::MakeArrayView(plaintext.data(), plaintext.size()),
                          rtc::MakeArrayView(ciphertext.data(), ciphertext.size()),
                          &ct_written);
  assert(rc == 0);

  std::vector<uint8_t> recovered(dec_b->GetMaxPlaintextByteSize(
      cricket::MEDIA_TYPE_AUDIO, ct_written));
  auto result = dec_b->Decrypt(
      cricket::MEDIA_TYPE_AUDIO,
      std::vector<uint32_t>{},
      rtc::MakeArrayView(aad.data(), aad.size()),
      rtc::MakeArrayView(ciphertext.data(), ct_written),
      rtc::MakeArrayView(recovered.data(), recovered.size()));
  assert(result.status ==
             webrtc::FrameDecryptorInterface::Status::kFailedToDecrypt &&
         "Decryptor with a different per-call seed unexpectedly returned kOk");

  enc_a->Release();
  dec_b->Release();
  std::printf("ok\n");
}

// 1 byte and 64 KiB payloads round-trip — covers both extremes of
// realistic media frames (audio Opus packets are ~80B; an
// intra-coded 1080p H.264 keyframe is in the ~40 KiB range). Empty
// payloads (0 bytes) are NOT a libwebrtc shape — RTP frames are
// always at least 1 byte — so we don't bother with that edge.
void test_payload_size_extremes() {
  std::printf("test_payload_size_extremes ... ");

  constexpr uint32_t kAliceLeaf = 41;
  auto* enc =
      lakko::sframe::LakkoFrameEncryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size(),
                                                 kAliceLeaf, /*stream_index=*/0);
  auto* dec =
      lakko::sframe::LakkoFrameDecryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size());
  enc->AddRef();
  dec->AddRef();

  const std::vector<uint8_t> aad = {0x77, 0x77, 0x77};

  // Sizes tested: 1 byte (smallest realistic frame), 80 (Opus
  // packet), 1500 (Ethernet MTU equivalent), 65536 (one 64 KiB
  // video keyframe slice).
  const std::vector<std::size_t> sizes = {1, 80, 1500, 65536};

  for (std::size_t sz : sizes) {
    std::vector<uint8_t> pt(sz);
    for (std::size_t i = 0; i < sz; ++i) {
      // Deterministic but non-trivial pattern so any byte-level
      // corruption is visible if the round-trip ever silently
      // truncates.
      pt[i] = static_cast<uint8_t>(((i * 131u) ^ 0xA5u) & 0xFFu);
    }

    std::vector<uint8_t> ct(enc->GetMaxCiphertextByteSize(
        cricket::MEDIA_TYPE_VIDEO, pt.size()));
    std::size_t ct_written = 0;
    int rc = enc->Encrypt(cricket::MEDIA_TYPE_VIDEO, /*ssrc=*/0,
                          rtc::MakeArrayView(aad.data(), aad.size()),
                          rtc::MakeArrayView(pt.data(), pt.size()),
                          rtc::MakeArrayView(ct.data(), ct.size()),
                          &ct_written);
    assert(rc == 0 && "Encrypt failed for a payload-extremes size");

    std::vector<uint8_t> recovered(dec->GetMaxPlaintextByteSize(
        cricket::MEDIA_TYPE_VIDEO, ct_written));
    auto result = dec->Decrypt(
        cricket::MEDIA_TYPE_VIDEO, std::vector<uint32_t>{},
        rtc::MakeArrayView(aad.data(), aad.size()),
        rtc::MakeArrayView(ct.data(), ct_written),
        rtc::MakeArrayView(recovered.data(), recovered.size()));
    assert(result.status == webrtc::FrameDecryptorInterface::Status::kOk &&
           "Decrypt failed for a payload-extremes size");
    assert(result.bytes_written == sz);
    assert(std::memcmp(recovered.data(), pt.data(), sz) == 0 &&
           "Round-tripped plaintext differs at a payload-extremes size");
  }

  enc->Release();
  dec->Release();
  std::printf("ok\n");
}

// Cross-stack golden vector for the ADR-0071 layout. The canonical frame
// (`seed = 0x42 x 32`, leaf 3, stream 1, counter 1, a VP8 key frame whose
// body is "lakko media frame golden v1", 59 bytes sealed) is pinned
// byte-for-byte in the Rust core (`lakko_calls::media_frame`), the wasm
// surface, the web suite and the `lakko-sframe-native` C ABI test. This
// asserts the C++ bridge — the `LakkoFrameEncryptor` / `LakkoFrameDecryptor`
// the iOS Obj-C++ shim and the Android JNI module wrap before handing bytes
// to libwebrtc — speaks the SAME wire:
//   (1) the decryptor OPENS the golden frame to the golden VP8 key frame;
//   (2) the encryptor, told the codec is VP8, seals the same input into a
//       frame of the same shape (10 clear bytes, the composite KID, a
//       64-bit counter) and the decryptor opens it.
// Since ADR-0069 an encryptor starts at a random counter below 2^62 and
// nothing on the C ABI sets a counter, so the encryptor cannot reproduce the
// golden bytes; its seal is proved by opening what it sealed.
void test_media_frame_golden_vector() {
  std::printf("test_media_frame_golden_vector ... ");

  std::array<uint8_t, kSeedLen> golden_seed{};
  golden_seed.fill(0x42);
  constexpr uint32_t kGoldenLeaf = 3;
  constexpr uint8_t kGoldenStream = 1;  // the camera
  const std::vector<uint8_t> golden_frame = vp8_key_frame();
  const std::vector<uint8_t> golden_wire = hex_decode(
      "1002009d012a8002e0012dc56b47f2bfd3ebf051d8893d6d2b2b355f29f3980b60"
      "e19c2555c9ad3a6ca9e95f08b228768c0f59a6509103010a0301");
  assert(golden_wire.size() == 59);
  assert(golden_frame.size() == 37);

  // (1) The decryptor opens the canonical golden frame — a frame any other
  // stack seals opens on the native media path. A non-empty host
  // additional_data is passed on purpose: it is not authenticated.
  auto* dec = lakko::sframe::LakkoFrameDecryptor::Create(
      golden_seed.data(), golden_seed.size(), "video");
  assert(dec != nullptr && "golden Decryptor::Create returned nullptr");
  dec->AddRef();

  const std::vector<uint8_t> host_aad = {0xDE, 0xAD, 0xBE, 0xEF};
  std::vector<uint8_t> recovered(dec->GetMaxPlaintextByteSize(
      cricket::MEDIA_TYPE_VIDEO, golden_wire.size()));
  auto result = dec->Decrypt(
      cricket::MEDIA_TYPE_VIDEO, std::vector<uint32_t>{},
      rtc::MakeArrayView(host_aad.data(), host_aad.size()),
      rtc::MakeArrayView(golden_wire.data(), golden_wire.size()),
      rtc::MakeArrayView(recovered.data(), recovered.size()));
  assert(result.status == DecryptStatus::kOk &&
         "the C++ bridge Decrypt of the golden frame did not return kOk");
  assert(result.bytes_written == golden_frame.size());
  assert(std::memcmp(recovered.data(), golden_frame.data(),
                     golden_frame.size()) == 0 &&
         "the C++ bridge Decrypt of the golden frame returned the wrong bytes");

  // The golden's own shape, read the way a receiver reads it.
  const Trailer golden_trailer = trailer_of(golden_wire);
  assert(golden_trailer.clear_len == 10);
  assert(golden_trailer.header_len == 3);
  assert(golden_trailer.version == 0x01);
  uint32_t golden_kid = 0;
  uint64_t golden_counter = 0;
  header_of(golden_wire, &golden_kid, &golden_counter);
  assert(golden_kid == 0x0301u && golden_counter == 1);

  // (2) The encryptor seals two frames of the same input.
  auto* enc = lakko::sframe::LakkoFrameEncryptor::Create(
      golden_seed.data(), golden_seed.size(), kGoldenLeaf, kGoldenStream);
  assert(enc != nullptr && "golden Encryptor::Create returned nullptr");
  enc->AddRef();
  enc->SetCodec(LAKKO_SFRAME_MEDIA_CODEC_VP8);

  uint64_t counters[2] = {0, 0};
  for (int i = 0; i < 2; ++i) {
    const std::vector<uint8_t> wire =
        seal_frame(enc, cricket::MEDIA_TYPE_VIDEO, golden_frame);

    // Same clear prefix as the golden, byte for byte.
    const Trailer t = trailer_of(wire);
    assert(t.clear_len == 10 && t.version == 0x01);
    assert(std::memcmp(wire.data(), golden_wire.data(), 10) == 0 &&
           "the first 10 bytes of a VP8 key frame stay as the encoder made them");
    assert(wire.size() ==
           golden_frame.size() + kTagBytes + t.header_len + kTrailerBytes);

    // The whole KID (leaf 3 << 8 | stream 1) and the 64-bit counter.
    uint32_t kid = 0;
    uint64_t counter = 0;
    header_of(wire, &kid, &counter);
    assert(kid == 0x0301u && "the header KID must be (leaf 3 << 8) | stream 1");
    // The start is uniform in [0, 2^62) and the first frame carries
    // start + 1, so the counter lies in [1, 2^62].
    assert(counter >= 1 && counter <= (uint64_t{1} << 62) &&
           "the counter must be the 64-bit one from a random 62-bit start");
    counters[i] = counter;

    auto own = open_frame(dec, cricket::MEDIA_TYPE_VIDEO, wire);
    assert(own.first == DecryptStatus::kOk &&
           "the decryptor did not open the frame the encryptor sealed");
    assert(own.second == golden_frame);
  }
  assert(counters[1] == counters[0] + 1 &&
         "consecutive frames carry consecutive counters");

  enc->Release();
  dec->Release();
  std::printf("ok\n");
}

// ADR-0071: the codec the shim names decides how much of each frame stays in
// the clear, and nothing else about the frame.
void test_codec_decides_the_clear_prefix() {
  std::printf("test_codec_decides_the_clear_prefix ... ");

  constexpr uint32_t kLeaf = 9;
  auto* enc = lakko::sframe::LakkoFrameEncryptor::Create(
      kTestSeed.data(), kTestSeed.size(), kLeaf, /*stream_index=*/1);
  auto* dec = lakko::sframe::LakkoFrameDecryptor::Create(kTestSeed.data(),
                                                         kTestSeed.size());
  assert(enc != nullptr && dec != nullptr);
  enc->AddRef();
  dec->AddRef();

  const std::vector<uint8_t> key = vp8_key_frame();
  const std::vector<uint8_t> delta = vp8_delta_frame();

  // Seal `frame`, check the clear prefix is `expect` bytes of the frame
  // itself, and that the frame opens back whole.
  auto check = [&](cricket::MediaType media, const std::vector<uint8_t>& frame,
                   std::size_t expect) {
    const std::vector<uint8_t> wire = seal_frame(enc, media, frame);
    const Trailer t = trailer_of(wire);
    assert(t.version == 0x01);
    assert(t.clear_len == expect && "wrong clear prefix length");
    assert(wire.size() == frame.size() + kTagBytes + t.header_len + kTrailerBytes);
    assert(std::memcmp(wire.data(), frame.data(), expect) == 0 &&
           "the clear prefix is the frame's own leading bytes");
    auto opened = open_frame(dec, media, wire);
    assert(opened.first == DecryptStatus::kOk);
    assert(opened.second == frame);
  };

  // Before the shim has named a codec a video encryptor seals as OTHER.
  assert(enc->Codec() == LAKKO_SFRAME_MEDIA_CODEC_OTHER);
  check(cricket::MEDIA_TYPE_VIDEO, key, 0);
  check(cricket::MEDIA_TYPE_VIDEO, delta, 0);

  // VP8: a key frame keeps its first 10 bytes, any other frame its first 3.
  enc->SetCodec(LAKKO_SFRAME_MEDIA_CODEC_VP8);
  assert(enc->Codec() == LAKKO_SFRAME_MEDIA_CODEC_VP8);
  check(cricket::MEDIA_TYPE_VIDEO, key, 10);
  check(cricket::MEDIA_TYPE_VIDEO, delta, 3);
  // A frame shorter than its prefix is left clear whole.
  check(cricket::MEDIA_TYPE_VIDEO, std::vector<uint8_t>{0x11, 0x02}, 2);
  check(cricket::MEDIA_TYPE_VIDEO, std::vector<uint8_t>{0x10, 0x02, 0x00, 0x9d}, 4);

  // A frame libwebrtc hands over as audio keeps nothing clear even on an
  // encryptor told VP8: the media type wins.
  check(cricket::MEDIA_TYPE_AUDIO, key, 0);

  // Opus, OTHER and a value the C ABI does not know keep nothing clear.
  enc->SetCodec(LAKKO_SFRAME_MEDIA_CODEC_OPUS);
  check(cricket::MEDIA_TYPE_VIDEO, key, 0);
  enc->SetCodec(LAKKO_SFRAME_MEDIA_CODEC_OTHER);
  check(cricket::MEDIA_TYPE_VIDEO, key, 0);
  enc->SetCodec(200);
  assert(enc->Codec() == 200);
  check(cricket::MEDIA_TYPE_VIDEO, key, 0);
  check(cricket::MEDIA_TYPE_VIDEO, delta, 0);

  // The name mapping the platform shims use.
  using Enc = lakko::sframe::LakkoFrameEncryptor;
  assert(Enc::CodecFromName("VP8") == LAKKO_SFRAME_MEDIA_CODEC_VP8);
  assert(Enc::CodecFromName("vp8") == LAKKO_SFRAME_MEDIA_CODEC_VP8);
  assert(Enc::CodecFromName("video/VP8") == LAKKO_SFRAME_MEDIA_CODEC_VP8);
  assert(Enc::CodecFromName("opus") == LAKKO_SFRAME_MEDIA_CODEC_OPUS);
  assert(Enc::CodecFromName("audio/opus") == LAKKO_SFRAME_MEDIA_CODEC_OPUS);
  assert(Enc::CodecFromName("H264") == LAKKO_SFRAME_MEDIA_CODEC_OTHER);
  assert(Enc::CodecFromName("VP9") == LAKKO_SFRAME_MEDIA_CODEC_OTHER);
  assert(Enc::CodecFromName("VP80") == LAKKO_SFRAME_MEDIA_CODEC_OTHER);
  assert(Enc::CodecFromName("red") == LAKKO_SFRAME_MEDIA_CODEC_OTHER);
  assert(Enc::CodecFromName("rtx") == LAKKO_SFRAME_MEDIA_CODEC_OTHER);
  assert(Enc::CodecFromName("") == LAKKO_SFRAME_MEDIA_CODEC_OTHER);
  assert(Enc::CodecFromName(nullptr) == LAKKO_SFRAME_MEDIA_CODEC_OTHER);

  enc->Release();
  dec->Release();
  std::printf("ok\n");
}

// Every byte of a sealed frame is authenticated: the clear prefix, the
// ciphertext, the tag, the SFrame header and the trailer. Flipping any one
// byte of a sealed VP8 key frame and of a sealed VP8 delta frame must fail,
// and so must cutting the frame short or adding to it.
void test_every_byte_is_authenticated() {
  std::printf("test_every_byte_is_authenticated ... ");

  constexpr uint32_t kLeaf = 12;
  auto* enc = lakko::sframe::LakkoFrameEncryptor::Create(
      kTestSeed.data(), kTestSeed.size(), kLeaf, /*stream_index=*/1);
  auto* dec = lakko::sframe::LakkoFrameDecryptor::Create(kTestSeed.data(),
                                                         kTestSeed.size());
  assert(enc != nullptr && dec != nullptr);
  enc->AddRef();
  dec->AddRef();
  enc->SetCodec(LAKKO_SFRAME_MEDIA_CODEC_VP8);

  const std::vector<std::vector<uint8_t>> frames = {vp8_key_frame(),
                                                    vp8_delta_frame()};
  for (const auto& frame : frames) {
    const std::vector<uint8_t> wire =
        seal_frame(enc, cricket::MEDIA_TYPE_VIDEO, frame);
    const Trailer t = trailer_of(wire);
    assert(t.clear_len == ((frame[0] & 0x01) == 0 ? 10u : 3u));

    // Untouched, it opens.
    auto intact = open_frame(dec, cricket::MEDIA_TYPE_VIDEO, wire);
    assert(intact.first == DecryptStatus::kOk && intact.second == frame);

    // Flip each byte in turn, in two different bit positions.
    for (std::size_t i = 0; i < wire.size(); ++i) {
      for (uint8_t mask : {uint8_t{0x01}, uint8_t{0x80}}) {
        std::vector<uint8_t> tampered = wire;
        tampered[i] ^= mask;
        auto opened = open_frame(dec, cricket::MEDIA_TYPE_VIDEO, tampered);
        if (opened.first == DecryptStatus::kOk) {
          std::printf("byte %zu of %zu (mask 0x%02x) opened after a flip\n", i,
                      wire.size(), mask);
        }
        assert(opened.first == DecryptStatus::kFailedToDecrypt &&
               "a sealed frame opened with one byte flipped");
      }
    }

    // Cut short by one byte from either end, and extended by one byte.
    std::vector<uint8_t> no_last(wire.begin(), wire.end() - 1);
    assert(open_frame(dec, cricket::MEDIA_TYPE_VIDEO, no_last).first ==
           DecryptStatus::kFailedToDecrypt);
    std::vector<uint8_t> no_first(wire.begin() + 1, wire.end());
    assert(open_frame(dec, cricket::MEDIA_TYPE_VIDEO, no_first).first ==
           DecryptStatus::kFailedToDecrypt);
    std::vector<uint8_t> longer = wire;
    longer.insert(longer.begin(), 0x00);
    assert(open_frame(dec, cricket::MEDIA_TYPE_VIDEO, longer).first ==
           DecryptStatus::kFailedToDecrypt);
  }

  enc->Release();
  dec->Release();
  std::printf("ok\n");
}

// The header-first layout of ADR-0013 (`sframe_header || ciphertext`) is not
// accepted on the media path any more: neither its pinned golden vector nor
// a frame sealed in it just now under the right key opens. (The C ABI still
// exports the header-first seal; the bridge does not call it.)
void test_header_first_layout_is_refused() {
  std::printf("test_header_first_layout_is_refused ... ");

  std::array<uint8_t, kSeedLen> golden_seed{};
  golden_seed.fill(0x42);
  auto* dec = lakko::sframe::LakkoFrameDecryptor::Create(golden_seed.data(),
                                                         golden_seed.size());
  assert(dec != nullptr);
  dec->AddRef();

  // The ADR-0013 golden: seed 0x42 x 32, leaf 3, stream 0, counter 1.
  const std::vector<uint8_t> old_golden = hex_decode(
      "910300fe2c2af65799594a4c693ff4c9e0d49b0565756ddd7299b0945fc8adb1"
      "c9c12fced57c6937f372e6b94bdae40a");
  assert(open_frame(dec, cricket::MEDIA_TYPE_AUDIO, old_golden).first ==
         DecryptStatus::kFailedToDecrypt &&
         "the header-first golden vector opened on the media path");

  // A header-first frame sealed now, under the key the decryptor would
  // derive, for several body lengths (so that whatever the last three bytes
  // of the ciphertext happen to be, none reads as a frame of this layout).
  lakko_sframe_call_key_t* call_key = nullptr;
  lakko_sframe_status_t status = lakko_sframe_call_key_new(
      golden_seed.data(), golden_seed.size(), &call_key);
  assert(status == LAKKO_SFRAME_STATUS_T_OK);
  lakko_sframe_participant_key_t* pk = nullptr;
  status = lakko_sframe_participant_key_new(call_key, 3, 1, &pk);
  assert(status == LAKKO_SFRAME_STATUS_T_OK);
  for (std::size_t body = 1; body <= 64; ++body) {
    const std::vector<uint8_t> plaintext(body, 0x5A);
    uint8_t* wire_ptr = nullptr;
    std::size_t wire_len = 0;
    status = lakko_sframe_seal_framed(pk, plaintext.data(), plaintext.size(),
                                      &wire_ptr, &wire_len);
    assert(status == LAKKO_SFRAME_STATUS_T_OK);
    const std::vector<uint8_t> wire(wire_ptr, wire_ptr + wire_len);
    lakko_sframe_buffer_free(wire_ptr, wire_len);
    assert(open_frame(dec, cricket::MEDIA_TYPE_VIDEO, wire).first ==
           DecryptStatus::kFailedToDecrypt &&
           "a header-first frame opened on the media path");
  }
  lakko_sframe_participant_key_free(pk);
  lakko_sframe_call_key_free(call_key);

  dec->Release();
  std::printf("ok\n");
}

// `SetCodec` is called from the signaling thread while the codec thread is
// inside `Encrypt`. A smoke test of that race: one thread seals a stream of
// VP8 key frames while another keeps changing the codec. Every frame must
// come out sealed under one codec value or another (10 clear bytes or none)
// and open back to the input; the counters must stay consecutive.
void test_set_codec_races_encrypt() {
  std::printf("test_set_codec_races_encrypt ... ");

  constexpr uint32_t kLeaf = 17;
  constexpr std::size_t kFrames = 20000;
  auto* enc = lakko::sframe::LakkoFrameEncryptor::Create(
      kTestSeed.data(), kTestSeed.size(), kLeaf, /*stream_index=*/1);
  auto* dec = lakko::sframe::LakkoFrameDecryptor::Create(kTestSeed.data(),
                                                         kTestSeed.size());
  assert(enc != nullptr && dec != nullptr);
  enc->AddRef();
  dec->AddRef();

  const std::vector<uint8_t> frame = vp8_key_frame();
  std::vector<std::vector<uint8_t>> sealed;
  sealed.reserve(kFrames);
  std::atomic<bool> done{false};

  std::thread setter([&] {
    const uint8_t values[] = {LAKKO_SFRAME_MEDIA_CODEC_VP8,
                              LAKKO_SFRAME_MEDIA_CODEC_OTHER,
                              LAKKO_SFRAME_MEDIA_CODEC_OPUS, 200};
    std::size_t i = 0;
    while (!done.load(std::memory_order_acquire)) {
      enc->SetCodec(values[i++ % 4]);
    }
  });
  std::thread sealer([&] {
    for (std::size_t i = 0; i < kFrames; ++i) {
      sealed.push_back(seal_frame(enc, cricket::MEDIA_TYPE_VIDEO, frame));
    }
    done.store(true, std::memory_order_release);
  });
  sealer.join();
  setter.join();

  assert(sealed.size() == kFrames);
  std::size_t with_prefix = 0;
  std::size_t without_prefix = 0;
  uint64_t previous_counter = 0;
  for (std::size_t i = 0; i < sealed.size(); ++i) {
    const Trailer t = trailer_of(sealed[i]);
    assert((t.clear_len == 10 || t.clear_len == 0) &&
           "a frame was sealed under no codec value that was ever set");
    if (t.clear_len == 10) {
      ++with_prefix;
    } else {
      ++without_prefix;
    }
    uint32_t kid = 0;
    uint64_t counter = 0;
    header_of(sealed[i], &kid, &counter);
    assert(kid == ((kLeaf << 8) | 1u));
    assert((i == 0 || counter == previous_counter + 1) &&
           "the counter skipped or repeated under the race");
    previous_counter = counter;
    auto opened = open_frame(dec, cricket::MEDIA_TYPE_VIDEO, sealed[i]);
    assert(opened.first == DecryptStatus::kOk && opened.second == frame);
  }
  std::printf("(%zu with a clear prefix, %zu without) ", with_prefix,
              without_prefix);

  enc->Release();
  dec->Release();
  std::printf("ok\n");
}

// G-18 / ADR-0029: a mid-call rekey rotates the encryptor's seal key + the
// decryptor's ring. A frame sealed under the OLD epoch still opens through the
// decryptor's overlap window; a fresh frame opens under the NEW epoch; and
// pruning the expired window closes the fall-back (seed-boundary forward
// secrecy, C-9 / C-10).
void test_mid_call_rekey_overlap() {
  std::printf("test_mid_call_rekey_overlap ... ");

  constexpr uint32_t kLeaf = 5;
  std::array<uint8_t, kSeedLen> seed_b = kTestSeed;
  seed_b[0] ^= 0xFF;  // a distinct epoch-B seed

  auto* enc = lakko::sframe::LakkoFrameEncryptor::Create(
      kTestSeed.data(), kTestSeed.size(), kLeaf, /*stream_index=*/0);
  assert(enc != nullptr);
  enc->AddRef();
  auto* dec = lakko::sframe::LakkoFrameDecryptor::Create(kTestSeed.data(),
                                                         kTestSeed.size());
  assert(dec != nullptr);
  dec->AddRef();

  const std::vector<uint8_t> pt_old = {'o', 'l', 'd'};
  const std::vector<uint8_t> pt_new = {'n', 'e', 'w'};
  const std::vector<uint8_t> aad = {0x01, 0x02};

  auto seal = [&](const std::vector<uint8_t>& pt, std::size_t* out_len) {
    std::vector<uint8_t> wire(
        enc->GetMaxCiphertextByteSize(cricket::MEDIA_TYPE_AUDIO, pt.size()));
    int rc = enc->Encrypt(cricket::MEDIA_TYPE_AUDIO, 0,
                          rtc::MakeArrayView(aad.data(), aad.size()),
                          rtc::MakeArrayView(pt.data(), pt.size()),
                          rtc::MakeArrayView(wire.data(), wire.size()), out_len);
    assert(rc == 0 && "seal failed");
    return wire;
  };
  auto open = [&](const std::vector<uint8_t>& wire, std::size_t wire_len) {
    std::vector<uint8_t> rec(
        dec->GetMaxPlaintextByteSize(cricket::MEDIA_TYPE_AUDIO, wire_len));
    auto r = dec->Decrypt(cricket::MEDIA_TYPE_AUDIO, std::vector<uint32_t>{},
                          rtc::MakeArrayView(aad.data(), aad.size()),
                          rtc::MakeArrayView(wire.data(), wire_len),
                          rtc::MakeArrayView(rec.data(), rec.size()));
    rec.resize(r.status == webrtc::FrameDecryptorInterface::Status::kOk
                   ? r.bytes_written
                   : 0);
    return std::make_pair(r.status, rec);
  };

  // Seal an epoch-A frame, still in flight when the rotation lands.
  std::size_t old_len = 0;
  std::vector<uint8_t> old_wire = seal(pt_old, &old_len);

  // Rekey both sides to epoch B, 1 s overlap window at t=1000 (expires 2000).
  assert(enc->Rekey(seed_b.data(), seed_b.size()) && "encryptor rekey failed");
  assert(dec->Rekey(seed_b.data(), seed_b.size(), 1000, 1000) &&
         "decryptor rekey failed");

  // A fresh epoch-B frame.
  std::size_t new_len = 0;
  std::vector<uint8_t> new_wire = seal(pt_new, &new_len);

  // The old frame still opens via the decryptor's overlap window...
  auto old_opened = open(old_wire, old_len);
  assert(old_opened.first == webrtc::FrameDecryptorInterface::Status::kOk &&
         "old-epoch frame did not open through the overlap window");
  assert(old_opened.second == pt_old);

  // ...and the new frame opens under the current epoch.
  auto new_opened = open(new_wire, new_len);
  assert(new_opened.first == webrtc::FrameDecryptorInterface::Status::kOk &&
         "new-epoch frame did not open under the current key");
  assert(new_opened.second == pt_new);

  // The codec set before a rekey survives it: a VP8 key frame sealed under
  // the new epoch still keeps its 10 clear bytes and opens.
  enc->SetCodec(LAKKO_SFRAME_MEDIA_CODEC_VP8);
  const bool rekeyed_again = enc->Rekey(seed_b.data(), seed_b.size());
  assert(rekeyed_again);
  assert(enc->Codec() == LAKKO_SFRAME_MEDIA_CODEC_VP8);
  {
    const std::vector<uint8_t> key = vp8_key_frame();
    const std::vector<uint8_t> video_wire =
        seal_frame(enc, cricket::MEDIA_TYPE_VIDEO, key);
    assert(trailer_of(video_wire).clear_len == 10);
    auto video_opened = open_frame(dec, cricket::MEDIA_TYPE_VIDEO, video_wire);
    assert(video_opened.first == DecryptStatus::kOk &&
           video_opened.second == key);
  }

  // Prune past the window → the old frame no longer opens.
  dec->Prune(2000);
  auto old_after_prune = open(old_wire, old_len);
  assert(old_after_prune.first ==
             webrtc::FrameDecryptorInterface::Status::kFailedToDecrypt &&
         "old-epoch frame opened after the overlap window was pruned");
  // The new frame still opens under the current key.
  assert(open(new_wire, new_len).first ==
         webrtc::FrameDecryptorInterface::Status::kOk);

  enc->Release();
  dec->Release();
  std::printf("ok\n");
}

}  // namespace

// An empty frame is nothing to decrypt, not a failed decryption.
//
// Audio silence transmission sends payload-free frames by design, so a healthy
// call delivers them routinely. Running one through the opener yields
// WIRE_PARSE — correctly, there is no header — and reporting that upward as
// kFailedToDecrypt tells libwebrtc a decryption failed when none was attempted,
// which counts against the stream and, on a live call, sent an investigation
// after a keying bug that did not exist.
void test_empty_frame_is_not_a_decrypt_failure() {
  std::printf("test_empty_frame_is_not_a_decrypt_failure ... ");

  auto* dec = lakko::sframe::LakkoFrameDecryptor::Create(kTestSeed.data(),
                                                         kTestSeed.size(),
                                                         "audio");
  assert(dec != nullptr);
  dec->AddRef();

  // Zero-length in, zero-length out, and an OK status.
  std::vector<uint8_t> out(16, 0xAA);
  auto result = dec->Decrypt(cricket::MEDIA_TYPE_AUDIO, {}, {},
                             rtc::ArrayView<const uint8_t>(),
                             rtc::ArrayView<uint8_t>(out.data(), out.size()));
  assert(result.status == webrtc::FrameDecryptorInterface::Status::kOk &&
         "an empty frame must not read as a failed decryption");
  assert(result.bytes_written == 0 && "an empty frame yields no plaintext");

  // And a genuinely bad frame still fails, so the short-circuit has not made
  // the decryptor permissive about frames that DO carry bytes.
  const std::array<uint8_t, 4> garbage{0x01, 0x02, 0x03, 0x04};
  auto bad = dec->Decrypt(cricket::MEDIA_TYPE_AUDIO, {}, {},
                          rtc::ArrayView<const uint8_t>(garbage.data(),
                                                        garbage.size()),
                          rtc::ArrayView<uint8_t>(out.data(), out.size()));
  assert(bad.status != webrtc::FrameDecryptorInterface::Status::kOk &&
         "a non-empty unopenable frame must still fail");

  dec->Release();
  std::printf("ok\n");
}

int main() {
  test_round_trip_single_sender();
  test_additional_data_ignored_header_is_aad();
  test_multi_sender_into_one_receiver();
  test_bad_seed_length();
  test_ciphertext_tamper();
  test_counter_monotonicity_and_out_of_order();
  test_per_call_key_isolation();
  test_payload_size_extremes();
  test_media_frame_golden_vector();
  test_codec_decides_the_clear_prefix();
  test_every_byte_is_authenticated();
  test_header_first_layout_is_refused();
  test_set_codec_races_encrypt();
  test_mid_call_rekey_overlap();
  test_empty_frame_is_not_a_decrypt_failure();
  std::printf("all sframe-bridge tests passed\n");
  return 0;
}
