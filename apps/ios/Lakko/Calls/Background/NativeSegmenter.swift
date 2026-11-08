// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The Rust frame gate (`lakko_segment_frame_action`, ADR-0089 section 3):
/// the one rule for what may leave the device. Written once in
/// `lakko-segment`; this only translates the C enums.
enum BackgroundGate {
    static func action(
        selected: Bool, state: BackgroundSegmentState
    ) -> BackgroundFrameAction {
        let answer = lakko_segment_frame_action(selected, NativeSegmenter.cState(state))
        switch answer {
        case LAKKO_FRAME_ACTION_T_SEND_CAMERA: return .sendCamera
        case LAKKO_FRAME_ACTION_T_SEND_PROCESSED: return .sendProcessed
        case LAKKO_FRAME_ACTION_T_DROP: return .drop
        case LAKKO_FRAME_ACTION_T_DROP_AND_STOP_CAMERA: return .dropAndStopCamera
        default:
            // A value this build does not know: nothing is sent.
            return .drop
        }
    }
}

/// One camera's segmenter over the C ABI `lakko_segment_*` in
/// `liblakko_sframe_native.a`. Owns the pointer for its life.
///
/// `onFrame` and `restart` take only the watch's lock on the Rust side and
/// may run while `run` is segmenting on another thread (the C ABI's contract).
final class NativeSegmenter: BackgroundSegmenting {
    private let pointer: OpaquePointer?

    /// Loads the model (tens of milliseconds): call it off the capture
    /// thread. A model that cannot load leaves the watch `Failed`.
    init(nowMs: UInt64) {
        pointer = lakko_segment_new(nowMs)
    }

    deinit {
        lakko_segment_free(pointer)
    }

    func onFrame(nowMs: UInt64) -> BackgroundSegmentState {
        // A null pointer is answered `Failed` by the C ABI.
        Self.state(lakko_segment_on_frame(pointer, nowMs))
    }

    func run(bgra: [UInt8], nowMs: UInt64, mask: inout [UInt8]) -> BackgroundSegmentState {
        guard let pointer else { return .failed }
        let maskCount = mask.count
        let answer = bgra.withUnsafeBufferPointer { input in
            mask.withUnsafeMutableBufferPointer { output in
                lakko_segment_run(
                    pointer,
                    input.baseAddress,
                    UInt(input.count),
                    LAKKO_SEGMENT_LAYOUT_T_BGRA,
                    nowMs,
                    output.baseAddress,
                    UInt(maskCount))
            }
        }
        return Self.state(answer)
    }

    func restart(nowMs: UInt64) {
        _ = lakko_segment_restart(pointer, nowMs)
    }

    static func state(_ raw: lakko_segment_state_t) -> BackgroundSegmentState {
        switch raw {
        case LAKKO_SEGMENT_STATE_T_STARTING: return .starting
        case LAKKO_SEGMENT_STATE_T_WORKING: return .working
        default: return .failed
        }
    }

    static func cState(_ state: BackgroundSegmentState) -> lakko_segment_state_t {
        switch state {
        case .starting: return LAKKO_SEGMENT_STATE_T_STARTING
        case .working: return LAKKO_SEGMENT_STATE_T_WORKING
        case .failed: return LAKKO_SEGMENT_STATE_T_FAILED
        }
    }
}
