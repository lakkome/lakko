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

void test_aad_binding() {
  std::printf("test_aad_binding ... ");

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
  const std::vector<uint8_t> aad_correct = {0x11, 0x22};
  const std::vector<uint8_t> aad_tampered = {0x11, 0x23};

  std::vector<uint8_t> ciphertext(enc->GetMaxCiphertextByteSize(
      cricket::MEDIA_TYPE_VIDEO, plaintext.size()));
  std::size_t ct_written = 0;
  int rc = enc->Encrypt(cricket::MEDIA_TYPE_VIDEO,
                        /*ssrc=*/0x1234,
                        rtc::MakeArrayView(aad_correct.data(), aad_correct.size()),
                        rtc::MakeArrayView(plaintext.data(), plaintext.size()),
                        rtc::MakeArrayView(ciphertext.data(), ciphertext.size()),
                        &ct_written);
  assert(rc == 0);

  // Decrypt with the tampered AAD — must fail.
  std::vector<uint8_t> recovered(dec->GetMaxPlaintextByteSize(
      cricket::MEDIA_TYPE_VIDEO, ct_written));
  auto result = dec->Decrypt(
      cricket::MEDIA_TYPE_VIDEO,
      std::vector<uint32_t>{},
      rtc::MakeArrayView(aad_tampered.data(), aad_tampered.size()),
      rtc::MakeArrayView(ciphertext.data(), ct_written),
      rtc::MakeArrayView(recovered.data(), recovered.size()));
  assert(result.status ==
             webrtc::FrameDecryptorInterface::Status::kFailedToDecrypt &&
         "AAD-tampered Decrypt unexpectedly returned kOk");

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

}  // namespace

int main() {
  test_round_trip_single_sender();
  test_aad_binding();
  test_multi_sender_into_one_receiver();
  test_bad_seed_length();
  std::printf("all sframe-bridge tests passed\n");
  return 0;
}
