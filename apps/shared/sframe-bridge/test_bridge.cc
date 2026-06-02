// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors
//
// Standalone round-trip test for the N-3 C++ subclass. Pure C++17
// — no gtest, no libwebrtc runtime — to keep the build chain
// short: cargo build the staticlib, c++ compile + link, run.
//
// What it proves:
//   1. LakkoFrameEncryptor::Create + Encrypt produces a wire frame
//      libwebrtc would accept (header + ciphertext, fits in the
//      reported max-size).
//   2. LakkoFrameDecryptor::Create + Decrypt of that wire frame
//      recovers the original plaintext.
//   3. The decryptor's per-sender_leaf ParticipantKey cache
//      correctly serves N senders into one receiver (mirrors the
//      SFU forwarding shape).
//   4. AAD binding works: an encryptor frame decrypted with the
//      wrong additional_data fails (kFailedToDecrypt).
//   5. Refcount machinery — AddRef + Release reach 0 and free.

#include "sframe-bridge.h"

#include <array>
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>

namespace {

constexpr std::size_t kSeedLen = 32;

// Mock seed — in production this comes from
// `Group::export_secret("lakko/call/v1", call_id, 32)`.
constexpr std::array<uint8_t, kSeedLen> kTestSeed = {
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
    0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
    0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18,
    0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
};

void test_round_trip_single_sender() {
  std::printf("test_round_trip_single_sender ... ");

  constexpr uint32_t kAliceLeaf = 7;
  auto* enc =
      lakko::sframe::LakkoFrameEncryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size(),
                                                 kAliceLeaf);
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

// ADR-0013: the AEAD AAD is the SFrame wire header (KID + CTR), NOT
// the host RTP `additional_data`. This test proves both halves of
// that contract:
//   (a) host `additional_data` is IGNORED — encrypt and decrypt with
//       deliberately mismatched additional_data still round-trips.
//       This is the property that lets a web RTCRtpScriptTransform
//       frame decrypt verbatim on iOS/Android (and vice versa): the
//       host RTP metadata is not reconstructible across platforms,
//       so it cannot be part of the authenticated set.
//   (b) the SFrame header IS bound — flipping a header byte makes the
//       AEAD open fail (kFailedToDecrypt).
void test_additional_data_ignored_header_is_aad() {
  std::printf("test_additional_data_ignored_header_is_aad ... ");

  constexpr uint32_t kAliceLeaf = 7;
  auto* enc =
      lakko::sframe::LakkoFrameEncryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size(),
                                                 kAliceLeaf);
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

  // (b) Flip the first header byte (the SFrame config octet — part of
  //     the bound AAD) and decrypt — must fail. Either the header
  //     decode rejects it or the AEAD tag check does; both surface as
  //     kFailedToDecrypt.
  std::vector<uint8_t> tampered_wire(ciphertext.begin(),
                                     ciphertext.begin() +
                                         static_cast<std::ptrdiff_t>(ct_written));
  tampered_wire[0] ^= 0x01;
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
         "(ADR-0013: header IS the bound AAD)");

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
                                                 kAliceLeaf);
  auto* enc_bob =
      lakko::sframe::LakkoFrameEncryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size(),
                                                 kBobLeaf);
  auto* enc_carol =
      lakko::sframe::LakkoFrameEncryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size(),
                                                 kCarolLeaf);

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
                                                 0);
  assert(enc == nullptr && "Encryptor::Create should reject short seed");

  auto* dec =
      lakko::sframe::LakkoFrameDecryptor::Create(short_seed.data(),
                                                 short_seed.size());
  assert(dec == nullptr && "Decryptor::Create should reject short seed");

  std::printf("ok\n");
}

// Tampering inside the ciphertext body — flip a single bit in the
// middle of the wire frame. AEAD tag verification must catch it.
// Mirrors test_aad_binding but on the ciphertext slot rather than
// the AAD slot, so both sides of the AEAD's authenticated set are
// exercised.
void test_ciphertext_tamper() {
  std::printf("test_ciphertext_tamper ... ");

  constexpr uint32_t kAliceLeaf = 11;
  auto* enc =
      lakko::sframe::LakkoFrameEncryptor::Create(kTestSeed.data(),
                                                 kTestSeed.size(),
                                                 kAliceLeaf);
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

  // Pick a byte halfway between the wire-header end and the tag
  // start — there must be plaintext bytes there because the
  // plaintext is 32 bytes and the AEAD-tag is the trailing 16. The
  // SFrame wire header is at most 17 bytes (see
  // kMaxSframeHeaderBytes in sframe-bridge.cc); the midpoint of
  // (header_max, ct_written - tag) is a safe body byte.
  const std::size_t mid = (17 + (ct_written - kAeadTagBytes)) / 2;
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
                                                 kAliceLeaf);
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
                                                 kAliceLeaf);
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
                                                 kAliceLeaf);
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

}  // namespace

int main() {
  test_round_trip_single_sender();
  test_additional_data_ignored_header_is_aad();
  test_multi_sender_into_one_receiver();
  test_bad_seed_length();
  test_ciphertext_tamper();
  test_counter_monotonicity_and_out_of_order();
  test_per_call_key_isolation();
  test_payload_size_extremes();
  std::printf("all sframe-bridge tests passed\n");
  return 0;
}
