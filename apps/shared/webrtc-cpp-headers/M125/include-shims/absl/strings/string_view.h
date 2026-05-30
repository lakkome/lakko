// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors
//
// Lakko-local shim for `absl/strings/string_view.h`.
//
// libwebrtc M125's `rtc_base/checks.h` uses `absl::string_view`
// as a function parameter type on `WriteFatalLog` + several
// `MakeVal` overloads. Like the type-traits shim
// (see ../meta/type_traits.h), `absl::string_view` is a
// pre-C++17 backport of `std::string_view`; on C++17+ the
// two are API-compatible (read-only views over a contiguous
// byte range, same member function names + semantics).
//
// We alias `absl::string_view` to `std::string_view` so the
// `checks.h` declarations parse against a fully-defined
// type. The platform shims (N-4 iOS / N-5 Android) never
// actually call `WriteFatalLog` — they go through the
// Lakko-local C ABI in `lakko-sframe-native` — so the
// runtime symbol that libwebrtc's binary uses is
// irrelevant to our code path.
//
// ## ABI safety
//
// `absl::string_view` and `std::string_view` have identical
// memory layout (a pair `{const char*, size_t}`) and
// identical member-function ABIs across the major
// implementations (libstdc++, libc++, MSVC STL). The N-3
// subclass never passes a `string_view` across the libwebrtc
// boundary — only `ArrayView<const uint8_t>` + scalars +
// `cricket::MediaType` — so an ABI mismatch on `string_view`
// would not affect our code paths even if one existed.

#ifndef LAKKO_ABSL_STRINGS_STRING_VIEW_H_
#define LAKKO_ABSL_STRINGS_STRING_VIEW_H_

#include <string_view>

namespace absl {

using string_view = std::string_view;

}  // namespace absl

#endif  // LAKKO_ABSL_STRINGS_STRING_VIEW_H_
