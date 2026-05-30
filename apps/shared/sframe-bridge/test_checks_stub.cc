// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors
//
// Stub implementation of `rtc::webrtc_checks_impl::FatalLog`
// so the N-3 standalone round-trip test can link without a
// libwebrtc runtime.
//
// Why this stub exists:
//
// libwebrtc's `rtc::ArrayView<T>` constructors call `RTC_CHECK`
// to assert bounds invariants; `RTC_CHECK` expands to calls into
// `rtc::webrtc_checks_impl::FatalLog`, which is implemented in
// libwebrtc's `rtc_base/checks.cc`. On iOS / Android the platform
// libwebrtc binary supplies that symbol; for the standalone
// round-trip test under apps/shared/sframe-bridge/build.sh there
// IS no libwebrtc binary in the link line.
//
// We supply a stub that:
//   - prints the fatal-log arguments to stderr (so a real bug
//     surfaces during testing rather than getting silently
//     swallowed)
//   - calls `std::abort()` to match the `RTC_NORETURN` contract
//
// This file is compiled ONLY into the standalone test binary
// (build.sh links it in). It is NOT shipped into the iOS / Android
// platform builds — those builds get FatalLog from the real
// libwebrtc binary the WebRTC.framework / libwebrtc.so ships.
//
// ## Security posture
//
// The encryptor / decryptor production code does NOT depend on
// FatalLog behaviour for correctness — every error in the AEAD
// path collapses to an `int rc != 0` (encrypt) or
// `Status::kFailedToDecrypt` (decrypt). FatalLog only fires if
// `rtc::MakeArrayView` is called with a malformed
// `(ptr, size)` pair, which never happens through normal
// invocation. So the stub's behaviour (abort vs. log-and-recover)
// has no observable effect on the security-relevant test cases.

#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <string_view>

#include "rtc_base/checks.h"

namespace rtc {
namespace webrtc_checks_impl {

void FatalLog(const char* file,
              int line,
              const char* message,
              const CheckArgType* fmt,
              ...) {
  std::fprintf(stderr, "[test stub] FatalLog: %s:%d: %s\n",
               file, line, message ? message : "(null)");
  (void)fmt;
  std::abort();
}

void FatalLog(const char* file, int line) {
  std::fprintf(stderr, "[test stub] FatalLog: %s:%d\n", file, line);
  std::abort();
}

void WriteFatalLog(const char* file, int line, absl::string_view output) {
  std::fprintf(stderr, "[test stub] WriteFatalLog: %s:%d: %.*s\n",
               file, line, static_cast<int>(output.size()), output.data());
  std::abort();
}

void WriteFatalLog(absl::string_view output) {
  std::fprintf(stderr, "[test stub] WriteFatalLog: %.*s\n",
               static_cast<int>(output.size()), output.data());
  std::abort();
}

}  // namespace webrtc_checks_impl
}  // namespace rtc
