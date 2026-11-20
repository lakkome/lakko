// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors
//
// Bridging header — Obj-C / Obj-C++ classes that Swift code in
// this target needs to import. xcodegen wires this file in via
// `SWIFT_OBJC_BRIDGING_HEADER` (see apps/ios/project.yml under
// the Lakko target's `settings.base`).
//
// Single entry today: the LakkoSFrameAttach shim
// (G3-sframe-native N-4). Pure Swift targets bridge nothing;
// adding more entries here is fine when the next Obj-C++ shim
// arrives.

#ifndef LAKKO_BRIDGING_HEADER_H_
#define LAKKO_BRIDGING_HEADER_H_

#import "Calls/SFrameAttach.h"
#import "Calls/CaptureNoiseProcessor.h"

// ADR-0089 — the camera background's per-frame C ABI (`lakko_segment_*`)
// in the same `liblakko_sframe_native.a` the SFrame shim links. Plain C.
// By relative path, not HEADER_SEARCH_PATHS: the test target re-reads this
// header through `@testable import Lakko` without the app's search paths.
#import "../../../target/lakko-include/lakko_sframe_native.h"

#endif  // LAKKO_BRIDGING_HEADER_H_
