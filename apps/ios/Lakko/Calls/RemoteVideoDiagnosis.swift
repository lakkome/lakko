// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// What the inbound video statistics say about a remote picture that has not
/// appeared.
///
/// ## Why this exists
///
/// A live Android-to-iOS call reached a state the UI alone cannot explain:
/// Android reported `framesEncoded=28, bytesSent=51084, currentDirection=SEND_ONLY,
/// sframeArmed=true` four seconds after its camera went on, and iOS reported the
/// video track attaching and then nothing — no frame, for a minute. Every layer
/// on the iOS side looks correct when read: the offer routes to the
/// renegotiation path, the answer is built, and the decryptor is armed on the
/// new receiver with the same per-call seed and no locally assumed stream index.
///
/// Reading a picture's absence off the screen cannot separate the three very
/// different reasons for it, and they have opposite fixes:
///
///   * nothing arrives at all — a transport or m-line problem, on the wire;
///   * packets arrive and nothing decodes — on this stack that is overwhelmingly
///     the frame decryptor rejecting every frame, since an SFrame open failure
///     discards the frame before the decoder ever sees it;
///   * frames decode and nothing is drawn — the renderer was never bound, which
///     is a view-layer bug and nothing to do with media.
///
/// So the counters are read directly and folded into one verdict, which is
/// logged as a single line. The next live call reads the answer off the console
/// instead of needing a debugger on two devices at once.
///
/// This type is deliberately pure and knows nothing about WebRTC: it takes four
/// numbers and one bool, which is what makes it testable.
public enum RemoteVideoDiagnosis: String, Equatable, Sendable {
    /// Not one packet has arrived. The picture's absence is upstream of
    /// anything this client does with frames — ICE, the transport, or an m-line
    /// that is not actually receiving.
    case noPacketsArriving = "NO_PACKETS"
    /// Packets arrive but nothing has decoded. On a stack where every frame is
    /// SFrame-sealed this is the decryptor's shape: an open failure drops the
    /// frame before the decoder is reached, so the packet counter climbs and the
    /// decoded counter does not.
    case packetsNotDecoding = "PACKETS_NOT_DECODING"
    /// Frames are decoding but nothing has been drawn. The media is fine and the
    /// renderer is not bound to the track.
    case decodedButNotRendered = "DECODED_NOT_RENDERED"
    /// A picture is on the screen.
    case flowing = "FLOWING"

    /// One line in the style the Android client already logs, so the two sides
    /// of a call read the same way in a transcript. Plain text, counters and a
    /// codec name only — nothing here identifies a person or carries payload.
    ///
    /// `pliCount` and `keyFramesDecoded` narrow ``packetsNotDecoding`` further.
    /// A receiver that joins mid-stream has no keyframe to start from, so every
    /// arriving frame references one it does not have and is dropped before
    /// assembly — the shape seen on this call. `pliCount > 0` says this side
    /// asked the sender for one, which moves the question to whether the sender
    /// honoured it; `pliCount == 0` says the asking is what is broken.
    ///
    /// The codec is in the line because it is the one thing that makes
    /// ``packetsNotDecoding`` ambiguous: a frame the decryptor rejects and a
    /// frame in a format this device cannot decode both count packets and decode
    /// nothing. Naming the negotiated codec lets a reader tell them apart
    /// without a second call.
    public func logLine(
        _ stats: InboundVideoStats, rendered: Bool, reading: RemoteVideoReading
    ) -> String {
        "CallSession: remote video \(reading.label): \(rawValue) "
            + "(packetsReceived=\(stats.packetsReceived), "
            + "framesReceived=\(stats.framesReceived), "
            + "framesDecoded=\(stats.framesDecoded), "
            + "framesDropped=\(stats.framesDropped), "
            + "bytesReceived=\(stats.bytesReceived), "
            + "pliCount=\(stats.pliCount), "
            + "keyFramesDecoded=\(stats.keyFramesDecoded), "
            + "codec=\(stats.codec ?? "unknown"), "
            + "decoder=\(stats.decoder ?? "none"), "
            + "rendered=\(rendered))"
    }

    /// Fold the counters into a verdict.
    ///
    /// The order of the tests is the order of the pipeline, so the verdict names
    /// the FIRST stage that failed rather than the last symptom — which is the
    /// stage worth looking at.
    ///
    /// - Parameter rendered: whether a frame has actually reached a renderer
    ///   (`RemoteVideoFrameProbe`). Decoded-but-not-rendered is a real and
    ///   separate failure, so the picture's own signal is an input here, never
    ///   inferred from the counters.
    public static func diagnose(_ stats: InboundVideoStats, rendered: Bool) -> RemoteVideoDiagnosis {
        if rendered { return .flowing }
        if stats.packetsReceived <= 0 { return .noPacketsArriving }
        if stats.framesDecoded <= 0 { return .packetsNotDecoding }
        return .decodedButNotRendered
    }
}

/// WHEN a set of counters was read, which is as load-bearing as the counters.
///
/// ## Why one reading was worse than useless
///
/// The first version of this probe fired once, four seconds after the remote
/// video TRACK attached — and a track attaches when the SDP is applied, which on
/// a live Android-to-iOS call was **nine seconds before the sender sent its
/// first packet**. So it faithfully reported `NO_PACKETS` about a moment when no
/// packet could possibly have existed, and never looked again. The reading was
/// true and said nothing, which is worse than silence because it invites a
/// conclusion.
///
/// The fix is to anchor the second reading to an event on the WIRE rather than
/// to a local clock: several seconds after the first packet actually arrives,
/// when the sender is known to be flowing. Both readings are kept and labelled,
/// because "nothing yet at four seconds, still nothing once packets were
/// flowing" and "nothing yet at four seconds, decoding by the time packets
/// arrived" are different findings and the early line alone cannot tell them
/// apart.
public enum RemoteVideoReading: Equatable, Sendable {
    /// A fixed delay after the track attached. Says whether anything arrived
    /// immediately; says nothing at all about a sender that starts later.
    case early(afterSeconds: Int)
    /// A settling delay after the FIRST packet was observed — the reading that
    /// describes a moment when the sender is known to be sending.
    case afterFirstPacket(settleSeconds: Int)
    /// Frames are decoding (or a picture is on screen), so the question is
    /// answered and the probe stops early.
    case decoding
    /// The budget ran out. Reported rather than dropped: a probe that goes
    /// quiet is indistinguishable from one that never ran.
    case budgetExpired(afterSeconds: Int)

    /// The phrase that distinguishes the readings in a transcript.
    public var label: String {
        switch self {
        case .early(let seconds):
            return "early (\(seconds)s after track attach)"
        case .afterFirstPacket(let seconds):
            return "late (\(seconds)s after first packet)"
        case .decoding:
            return "late (decoding)"
        case .budgetExpired(let seconds):
            return "late (budget expired \(seconds)s after track attach)"
        }
    }
}

/// What the probe's ticker should do with one sample.
public enum RemoteVideoProbeStep: Equatable, Sendable {
    case keepWaiting
    case report(RemoteVideoReading)
}

extension RemoteVideoDiagnosis {

    /// The ticker's whole rule, as a pure function of one sample.
    ///
    /// Ordered best-news-first: decoding ends the question outright, so it is
    /// asked before either deadline. The settle deadline is next because it is
    /// the reading actually wanted — taken while the sender is known to be
    /// flowing — and the budget is the fallback that guarantees a line is
    /// emitted even on a call where no packet ever arrives.
    ///
    /// - Parameters:
    ///   - secondsSinceFirstPacket: `nil` until a sample shows a packet.
    ///   - secondsRemaining: of the overall budget, measured from track attach.
    public static func step(
        stats: InboundVideoStats,
        rendered: Bool,
        secondsSinceFirstPacket: Int?,
        secondsRemaining: Int,
        settleSeconds: Int
    ) -> RemoteVideoProbeStep {
        if rendered || stats.framesDecoded > 0 {
            return .report(.decoding)
        }
        if let since = secondsSinceFirstPacket, since >= settleSeconds {
            return .report(.afterFirstPacket(settleSeconds: settleSeconds))
        }
        if secondsRemaining <= 0 {
            return .report(.budgetExpired(afterSeconds: 0))
        }
        return .keepWaiting
    }
}

/// The `inbound-rtp` counters for the remote video stream.
public struct InboundVideoStats: Equatable, Sendable {
    public let packetsReceived: Int64
    public let framesReceived: Int64
    public let framesDecoded: Int64
    public let framesDropped: Int64
    public let bytesReceived: Int64
    /// Picture-loss indications this receiver has SENT. A receiver with no
    /// decodable frame should be asking the sender for a keyframe; if this
    /// stays at zero while frames are dropped, the asking is what is broken,
    /// and the sender will never know to send an IDR.
    public let pliCount: Int64
    /// Keyframes actually decoded. Zero alongside arriving packets is the
    /// signature of a receiver that joined mid-stream and never got an IDR to
    /// start from — every later frame references one it does not have.
    public let keyFramesDecoded: Int64
    /// The negotiated payload type, e.g. `video/H264`. `nil` when the report
    /// carries no codec for this stream.
    public let codec: String?
    /// The decoder libwebrtc actually instantiated, when it says.
    public let decoder: String?

    public init(
        packetsReceived: Int64,
        framesReceived: Int64,
        framesDecoded: Int64,
        framesDropped: Int64,
        bytesReceived: Int64,
        pliCount: Int64 = 0,
        keyFramesDecoded: Int64 = 0,
        codec: String? = nil,
        decoder: String? = nil
    ) {
        self.packetsReceived = packetsReceived
        self.framesReceived = framesReceived
        self.framesDecoded = framesDecoded
        self.framesDropped = framesDropped
        self.bytesReceived = bytesReceived
        self.pliCount = pliCount
        self.keyFramesDecoded = keyFramesDecoded
        self.codec = codec
        self.decoder = decoder
    }

    /// No inbound video stream in the report at all — distinct from a stream
    /// whose counters are all zero, though both diagnose as `noPacketsArriving`.
    public static let none = InboundVideoStats(
        packetsReceived: 0, framesReceived: 0, framesDecoded: 0,
        framesDropped: 0, bytesReceived: 0)
}
