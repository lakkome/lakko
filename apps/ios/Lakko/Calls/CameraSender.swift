// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import WebRTC

/// One of this device's camera senders in an SFU call (ADR-0070 steps 1 and
/// 5); the twin of `CameraSenderKind` in
/// `apps/web/src/calls/sender-activity.ts`.
enum CameraSenderKind: String, CaseIterable, Equatable {
    /// The camera as it is captured.
    case main
    /// The camera at half size, sent only from three people in the call on.
    case middle
    /// The camera at a quarter size, always sent.
    case light
}

/// A copy of the camera sent beside the main sender, each an ordinary sender
/// on a transceiver of its own; the twin of `CameraCopyKind` in
/// `apps/web/src/calls/camera-sender.ts`.
enum CameraCopyKind: String, Equatable {
    case light
    case middle

    /// Every copy, in the order they are added: the light one first, so it
    /// keeps the media line it had before there was a middle one.
    static let addOrder: [CameraCopyKind] = [.light, .middle]

    var senderKind: CameraSenderKind {
        switch self {
        case .light: return .light
        case .middle: return .middle
        }
    }
}

/// One send encoding of a camera sender, as plain values. Kept apart from
/// `RTCRtpEncodingParameters` so what is asked of the binding can be read and
/// tested without a peer connection.
struct CameraEncodingSpec: Equatable {
    /// Each side of the picture divided by this; `nil` sends it whole.
    let scaleResolutionDownBy: Double?
    /// The bitrate ceiling, bits a second; `nil` for none.
    let maxBitrateBps: Int?
    /// Whether the encoding is sent when the sender is created.
    let active: Bool
    /// How many temporal layers the encoding is asked to carry; `nil` asks
    /// for nothing and leaves the stack's default.
    var numTemporalLayers: Int? = nil
}

/// One sender of the plan ``CameraSender/plan(middleActive:)`` gives.
struct CameraSenderSpec: Equatable {
    let kind: CameraSenderKind
    /// The SFrame stream index the sender is sealed under (ADR-0035).
    let streamIndex: UInt8
    /// The id of the local track the sender carries.
    let trackId: String
    let encoding: CameraEncodingSpec
}

/// How an SFU call's camera is sent: as three ordinary senders (ADR-0070
/// steps 1 and 5), the twin of `apps/web/src/calls/camera-sender.ts`.
///
/// The main one carries the camera as captured. The light one carries the
/// same camera at a quarter of the size and at most 150 kbit/s, on a
/// transceiver of its own; every viewer receives it and its tile shows it
/// while the picture it watches stands still. The middle one carries it at
/// half the size and at most 500 kbit/s and sends only from three people in
/// the call on. No simulcast: which of them is sent is this client's and the
/// SFU's decision, never a layer allocator's.
///
/// All three are fed by ONE capturer: each copy is an `RTCVideoTrack` on an
/// `RTCVideoSource` of its own that the capturer feeds the same frames
/// (``CapturerFan``), and its sender scales the picture down with
/// `scaleResolutionDownBy` on its one encoding. That is the binding's form of
/// what the web client does with a cloned track; a further track on the
/// camera's own source is not, since that source serves every track the
/// smallest picture any of their encoders asks for.
///
/// Used by the SFU session only: the one-to-one call sends one sender.
///
/// The main sender is asked for three temporal layers, as the web client's
/// `L1T3` (ADR-0067 step 2): two frames in three are referenced by no later
/// base-layer frame, so one lost in a burst costs that frame rather than the
/// picture until a keyframe. This binding has no `scalabilityMode`, only
/// `numTemporalLayers`, so that is what is set; the copies are left at one
/// layer, as on web. What the stack keeps is read back and logged
/// (``temporalLayersLine(asked:kept:)``).
enum CameraSender {

    /// The SFrame stream index of the microphone.
    static let streamIndexAudio: UInt8 = 0
    /// The SFrame stream index of the main camera sender.
    static let streamIndexCamera: UInt8 = 1
    /// Reserved for a shared screen; no sender uses it yet.
    static let streamIndexScreenShare: UInt8 = 2
    /// The SFrame stream index of the light camera sender.
    static let streamIndexCameraLight: UInt8 = 3
    /// The SFrame stream index of the middle camera sender.
    static let streamIndexCameraMiddle: UInt8 = 4

    /// The light sender's picture is the camera's scaled down by this, each
    /// side.
    static let lightScaleDown: Double = 4
    /// The light sender's bitrate ceiling, bits a second.
    static let lightMaxBitrateBps = 150_000
    /// The middle sender's picture is the camera's scaled down by this, each
    /// side.
    static let middleScaleDown: Double = 2
    /// The middle sender's bitrate ceiling, bits a second.
    static let middleMaxBitrateBps = 500_000
    /// The temporal layers the main sender is asked for: the `T3` of the web
    /// client's `L1T3`.
    static let mainTemporalLayers = 3

    /// The SFrame stream index each sender is sealed under. Stated, never
    /// derived from the track kind: each is a further video sender of the
    /// participant and must not share another's key.
    static func streamIndex(of kind: CameraSenderKind) -> UInt8 {
        switch kind {
        case .main: return streamIndexCamera
        case .light: return streamIndexCameraLight
        case .middle: return streamIndexCameraMiddle
        }
    }

    /// The id of the local track each sender carries. Distinct, so the media
    /// log can tell the senders apart by their media source.
    static func trackId(of kind: CameraSenderKind) -> String {
        switch kind {
        case .main: return "lakko-video0"
        case .light: return "lakko-video-light0"
        case .middle: return "lakko-video-middle0"
        }
    }

    /// The sender a local track id names; `nil` for any other track.
    static func senderKind(ofTrackId trackId: String) -> CameraSenderKind? {
        CameraSenderKind.allCases.first { self.trackId(of: $0) == trackId }
    }

    /// The one encoding of a copy's sender. No rid: an ordinary sender, not a
    /// simulcast layer.
    static func copyEncoding(_ kind: CameraCopyKind, active: Bool = true) -> CameraEncodingSpec {
        switch kind {
        case .light:
            return CameraEncodingSpec(
                scaleResolutionDownBy: lightScaleDown, maxBitrateBps: lightMaxBitrateBps,
                active: active)
        case .middle:
            return CameraEncodingSpec(
                scaleResolutionDownBy: middleScaleDown, maxBitrateBps: middleMaxBitrateBps,
                active: active)
        }
    }

    /// The one encoding of the main sender: the picture whole, no ceiling,
    /// three temporal layers.
    static let mainEncoding = CameraEncodingSpec(
        scaleResolutionDownBy: nil, maxBitrateBps: nil, active: true,
        numTemporalLayers: mainTemporalLayers)

    /// The media log's line for what the stack kept of the temporal layers
    /// asked of the main sender. `kept` is the value read back from the
    /// sender after it was set; `nil` when the sender reports none.
    static func temporalLayersLine(asked: Int, kept: Int?) -> String {
        guard let kept else {
            return "camera sender: asked for \(asked) temporal layers, the stack reports none"
        }
        if kept == asked {
            return "camera sender: asked for \(asked) temporal layers, the stack kept \(kept)"
        }
        return "camera sender: asked for \(asked) temporal layers, the stack kept \(kept) instead"
    }

    /// What to do with a copy of the camera after the binding was asked for
    /// its transceiver (the twin of `addCopySender` in
    /// `apps/web/src/calls/camera-sender.ts`).
    enum CopyAddStep: Equatable {
        /// The transceiver was taken with its encoding: nothing more to do.
        case keep
        /// The transceiver was taken only without an encoding: the size, the
        /// ceiling and the activity must be set through the sender's
        /// parameters, and the copy removed if that does not hold.
        case configureWithParameters
        /// No transceiver at all: the call goes on without this copy.
        case absent
    }

    /// The step after the two attempts to add a copy's transceiver.
    ///
    /// - Parameters:
    ///   - withEncodingTaken: the binding took the transceiver with the
    ///     copy's encoding in its init.
    ///   - plainTaken: the binding took it without any encoding; asked only
    ///     when the first attempt was refused.
    static func copyAddStep(withEncodingTaken: Bool, plainTaken: @autoclosure () -> Bool)
        -> CopyAddStep
    {
        if withEncodingTaken { return .keep }
        return plainTaken() ? .configureWithParameters : .absent
    }

    /// Whether a copy configured through its sender's parameters may stay.
    /// `readBack` is the sender's first encoding as read after the set,
    /// `nil` when it has none. It stays only when the stack holds exactly the
    /// size and the ceiling asked for: anything else would be the camera
    /// sent another time at full size, or without its ceiling.
    static func copyIsSized(wanted: CameraEncodingSpec, readBack: CameraEncodingSpec?) -> Bool {
        guard let readBack else { return false }
        return readBack.scaleResolutionDownBy == wanted.scaleResolutionDownBy
            && readBack.maxBitrateBps == wanted.maxBitrateBps
            && readBack.active == wanted.active
    }

    /// The camera's senders in the order they are added: main, light, middle.
    ///
    /// The main and the light sender are created sending. The middle one is
    /// always created, so its media line exists from the first offer, and is
    /// created sending or not as `middleActive` says (the people-count rule
    /// and the SFU's last word, ``SenderActivityRule``).
    static func plan(middleActive: Bool) -> [CameraSenderSpec] {
        [
            CameraSenderSpec(
                kind: .main, streamIndex: streamIndex(of: .main), trackId: trackId(of: .main),
                encoding: mainEncoding),
            CameraSenderSpec(
                kind: .light, streamIndex: streamIndex(of: .light), trackId: trackId(of: .light),
                encoding: copyEncoding(.light)),
            CameraSenderSpec(
                kind: .middle, streamIndex: streamIndex(of: .middle),
                trackId: trackId(of: .middle),
                encoding: copyEncoding(.middle, active: middleActive)),
        ]
    }

    /// The `mid`s an offer names for one copy of the camera: the copy's
    /// media line when it has one, none otherwise. A transceiver has a `mid`
    /// once a local description that includes it is set; before that, and
    /// for a copy this device does not send, the list is empty.
    static func mids(ofCopyMid mid: String?) -> [String] {
        guard let mid, !mid.isEmpty else { return [] }
        return [mid]
    }

    /// The binding's own parameters for `spec`.
    static func rtcEncoding(_ spec: CameraEncodingSpec) -> RTCRtpEncodingParameters {
        let encoding = RTCRtpEncodingParameters()
        encoding.isActive = spec.active
        if let scale = spec.scaleResolutionDownBy {
            encoding.scaleResolutionDownBy = NSNumber(value: scale)
        }
        if let bitrate = spec.maxBitrateBps {
            encoding.maxBitrateBps = NSNumber(value: bitrate)
        }
        if let layers = spec.numTemporalLayers {
            encoding.numTemporalLayers = NSNumber(value: layers)
        }
        return encoding
    }

    /// Write `spec` onto an encoding the binding already holds, leaving the
    /// rest of it (its ssrc, its rid) as it is.
    static func apply(_ spec: CameraEncodingSpec, to encoding: RTCRtpEncodingParameters) {
        encoding.isActive = spec.active
        encoding.scaleResolutionDownBy = spec.scaleResolutionDownBy.map { NSNumber(value: $0) }
        encoding.maxBitrateBps = spec.maxBitrateBps.map { NSNumber(value: $0) }
        if let layers = spec.numTemporalLayers {
            encoding.numTemporalLayers = NSNumber(value: layers)
        }
    }

    /// An encoding of the binding as plain values.
    static func spec(of encoding: RTCRtpEncodingParameters) -> CameraEncodingSpec {
        CameraEncodingSpec(
            scaleResolutionDownBy: encoding.scaleResolutionDownBy?.doubleValue,
            maxBitrateBps: encoding.maxBitrateBps?.intValue,
            active: encoding.isActive,
            numTemporalLayers: encoding.numTemporalLayers?.intValue)
    }
}
