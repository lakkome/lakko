// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors
//
// Compile-only probe — proves the vendored M125 header subset
// is sufficient to declare a concrete subclass of
// webrtc::FrameEncryptorInterface + webrtc::FrameDecryptorInterface.
// If this file compiles, the N-3 C++ subclass under
// apps/shared/sframe-bridge/ has every header it needs.
//
// Build manually (the real CMake wiring lands in N-3):
//
//   c++ -std=c++17 -fsyntax-only \
//       -I apps/shared/webrtc-cpp-headers/M125/include \
//       apps/shared/webrtc-cpp-headers/M125/probe.cc
//
// No `int main()` — `-fsyntax-only` checks the header layer
// without requiring a libwebrtc runtime to link against. The
// runtime link happens on the iOS / Android platform side,
// where the WebRTC.framework / libwebrtc.so provides the
// `webrtc::FrameEncryptorInterface` vtable.

#include "api/crypto/frame_decryptor_interface.h"
#include "api/crypto/frame_encryptor_interface.h"

namespace lakko_sframe {

// Probe — concrete subclass of FrameEncryptorInterface. We
// don't ship this; the real subclass is in
// apps/shared/sframe-bridge/sframe-bridge.cc (N-3). This is
// only here to prove the headers compile.
class ProbeEncryptor : public webrtc::FrameEncryptorInterface {
 public:
  int Encrypt(cricket::MediaType /*media_type*/,
              uint32_t /*ssrc*/,
              rtc::ArrayView<const uint8_t> /*additional_data*/,
              rtc::ArrayView<const uint8_t> /*frame*/,
              rtc::ArrayView<uint8_t> /*encrypted_frame*/,
              size_t* bytes_written) override {
    *bytes_written = 0;
    return 0;
  }

  size_t GetMaxCiphertextByteSize(cricket::MediaType /*media_type*/,
                                  size_t frame_size) override {
    return frame_size + 33;  // SFrame header (17) + AEAD tag (16)
  }
};

class ProbeDecryptor : public webrtc::FrameDecryptorInterface {
 public:
  Result Decrypt(cricket::MediaType /*media_type*/,
                 const std::vector<uint32_t>& /*csrcs*/,
                 rtc::ArrayView<const uint8_t> /*additional_data*/,
                 rtc::ArrayView<const uint8_t> /*encrypted_frame*/,
                 rtc::ArrayView<uint8_t> /*frame*/) override {
    return Result(Status::kOk, 0);
  }

  size_t GetMaxPlaintextByteSize(cricket::MediaType /*media_type*/,
                                 size_t encrypted_frame_size) override {
    return encrypted_frame_size;
  }
};

}  // namespace lakko_sframe
