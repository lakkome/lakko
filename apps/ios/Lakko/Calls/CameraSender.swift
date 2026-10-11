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
/// All three are fed by ONE capturer: each copy is a further `RTCVideoTrack`
/// on the camera's `RTCVideoSource`, and its sender scales the picture down
/// with `scaleResolutionDownBy` on its one encoding. That is the binding's
/// form of what the web client does with a cloned track.
///
/// Used by the SFU session only: the one-to-one call sends one sender.
///
/// Temporal layers are not asked for. The web client asks for `L1T3` on the
/// main sender; this binding has no `scalabilityMode`, only
/// `numTemporalLayers`, which this client has never set.
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
                encoding: CameraEncodingSpec(
                    scaleResolutionDownBy: nil, maxBitrateBps: nil, active: true)),
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
        return encoding
    }
}
