// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors
//
// Lakko-local shim for `absl/meta/type_traits.h`.
//
// libwebrtc M125's `rtc_base/checks.h` transitively includes
// `absl/meta/type_traits.h` and uses two symbols from it:
//
//   - `absl::enable_if_t<...>`
//   - `absl::underlying_type_t<T>`
//
// Both are pre-C++17 backports of the same-named `std::`
// aliases (abseil keeps them for compatibility with older
// toolchains). On C++17+ — which is what the Lakko platform
// shims target — they are interchangeable with the `std::`
// equivalents at the API level.
//
// ## Why a shim instead of vendoring upstream abseil
//
// Upstream `absl/meta/type_traits.h` is ~700 LOC and pulls
// a long chain of internal abseil-cpp headers
// (`absl/base/...`, `absl/meta/...`). Vendoring the
// transitive closure under our `webrtc-cpp-headers/M125/`
// would multiply the bundle by ~30x and create a parallel
// maintenance surface that has nothing to do with the
// SFrame goal.
//
// ## ABI safety
//
// These are pure type-alias templates — they affect compile-
// time type resolution and produce zero runtime artefacts.
// The platform shims (N-4 iOS, N-5 Android) link against
// libwebrtc binaries that statically linked the upstream
// abseil; our shim only feeds the compiler enough information
// to type-check the `webrtc::FrameEncryptorInterface` /
// `FrameDecryptorInterface` declarations. No symbol from
// this header ever appears in the linked binary because
// `enable_if_t` / `underlying_type_t` are alias templates
// — they vanish during template instantiation.

#ifndef LAKKO_ABSL_META_TYPE_TRAITS_H_
#define LAKKO_ABSL_META_TYPE_TRAITS_H_

#include <type_traits>

namespace absl {

// C++14 alias. Identical to `std::enable_if_t` on C++14+.
template <bool B, typename T = void>
using enable_if_t = typename std::enable_if<B, T>::type;

// C++14 alias. Identical to `std::underlying_type_t` on C++14+.
template <typename T>
using underlying_type_t = typename std::underlying_type<T>::type;

// `void_t` — used inside the larger checks.h template
// machinery via SFINAE. Identical to `std::void_t` on C++17+.
template <typename...>
using void_t = void;

// `remove_const_t`, `remove_reference_t`, `decay_t` — also
// pulled in by checks.h's MakeVal overload set.
template <typename T>
using remove_const_t = typename std::remove_const<T>::type;
template <typename T>
using remove_reference_t = typename std::remove_reference<T>::type;
template <typename T>
using decay_t = typename std::decay<T>::type;

}  // namespace absl

#endif  // LAKKO_ABSL_META_TYPE_TRAITS_H_
