// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import WebRTC

/// One send encoding of the camera, as plain values (ADR-0067 step 5).
///
/// The twin of an `RTCRtpEncodingParameters` this file fills in, kept apart
/// from it so what is asked of the binding can be read and tested without a
/// peer connection.
struct CameraEncodingSpec: Equatable {
    /// The layer's RTP stream id.
    let rid: String
    /// Each side of the picture divided by this; `nil` sends it whole.
    let scaleResolutionDownBy: Double?
    /// The layer's bitrate ceiling, bits a second; `nil` for none.
    let maxBitrateBps: Int?
}

/// How the group call's camera is sent: as two simulcast layers, the full
/// picture and a light one (the twin of `apps/web/src/calls/camera-sender.ts`).
///
/// The SFU forwards both to every viewer as two tracks, and a viewer's tile
/// shows the light one while the full one stands still
/// (`services/sfu/README.md`, the `~light` contract). Used by the SFU session
/// only: the one-to-one call sends one encoding, as before.
///
/// Temporal layers are not asked for. This binding has no `scalabilityMode`;
/// it has `numTemporalLayers`, which this client has never set, and whether
/// the encoder honours it under simulcast is not something that can be read
/// off a build.
enum CameraSender {

    /// The light layer's RTP stream id. The SFU matches it by this exact name.
    static let ridLow = "q"
    /// The full layer's RTP stream id.
    static let ridFull = "f"
    /// The light layer is the camera picture scaled down by this, each side.
    static let lowScaleDown: Double = 4
    /// The light layer's bitrate ceiling, bits a second.
    static let lowMaxBitrateBps = 150_000
    /// Whether the camera is sent as two layers at all.
    static let simulcastEnabled = true

    /// The two encodings, light layer first (the order the SFU and the web
    /// client use).
    static func simulcastEncodings() -> [CameraEncodingSpec] {
        [
            CameraEncodingSpec(
                rid: ridLow, scaleResolutionDownBy: lowScaleDown,
                maxBitrateBps: lowMaxBitrateBps),
            CameraEncodingSpec(rid: ridFull, scaleResolutionDownBy: nil, maxBitrateBps: nil),
        ]
    }

    /// What the camera ended up being sent as.
    enum Outcome: Equatable {
        /// Two simulcast layers.
        case simulcast
        /// One encoding, as before simulcast.
        case single
        /// The binding gave no sender at all.
        case none
    }

    /// Add the camera, trying two layers first and falling back to the one
    /// encoding this client has always sent. Both the first publish and the
    /// mid-call camera-on come through here, because encodings can only be
    /// given when the sender is created.
    ///
    /// - Parameters:
    ///   - simulcast: `false` skips straight to the single encoding.
    ///   - addWithEncodings: create the sender with these encodings, or `nil`
    ///     when the binding refuses them.
    ///   - addPlain: create the sender the way it was created before
    ///     simulcast.
    ///   - log: one line for each refusal.
    static func add<Sender>(
        simulcast: Bool = simulcastEnabled,
        addWithEncodings: ([CameraEncodingSpec]) -> Sender?,
        addPlain: () -> Sender?,
        log: (String) -> Void = { NSLog("%@", $0) }
    ) -> (sender: Sender?, outcome: Outcome) {
        if simulcast {
            if let sender = addWithEncodings(simulcastEncodings()) {
                return (sender, .simulcast)
            }
            log("camera sender: simulcast refused, sending one encoding")
        }
        if let sender = addPlain() {
            return (sender, .single)
        }
        log("camera sender: no sender was created")
        return (nil, .none)
    }

    /// The binding's own parameters for `specs`.
    static func rtcEncodings(_ specs: [CameraEncodingSpec]) -> [RTCRtpEncodingParameters] {
        specs.map { spec in
            let encoding = RTCRtpEncodingParameters()
            encoding.rid = spec.rid
            encoding.isActive = true
            if let scale = spec.scaleResolutionDownBy {
                encoding.scaleResolutionDownBy = NSNumber(value: scale)
            }
            if let bitrate = spec.maxBitrateBps {
                encoding.maxBitrateBps = NSNumber(value: bitrate)
            }
            return encoding
        }
    }
}
