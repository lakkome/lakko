// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The light companion and the middle track of a remote camera (the twin of
/// `apps/web/src/calls/light-stream.ts`).
///
/// A publisher sends its camera as up to three ordinary senders (ADR-0070
/// steps 1 and 5), and the SFU gives each subscriber up to three video
/// tracks: the **main** one, whose MSID stream id is the source peer id (as
/// for every remote stream); a **light companion**, whose stream id is the
/// source peer id followed by ``lightSuffix``; and, for a publisher that has
/// a middle sender, a **middle** track under the peer id followed by
/// ``middleSuffix``, whose packets the SFU forwards only while this device
/// asks for that person's middle picture (``VideoWants``).
///
/// The companion is a second picture of a person already in the call, and
/// the middle track a third. Neither is ever a participant of its own: no
/// tile, no camera flag. The tile of its peer decodes the companion beside
/// the picture it watches and shows it while that one stands still
/// (``RemoteLayerController``); the middle track is one of the two pictures a
/// tile can watch.
enum LightStream {

    /// What the SFU appends to a peer id to name that peer's light companion.
    static let lightSuffix = "~light"
    /// What the SFU appends to a peer id to name that peer's middle track.
    static let middleSuffix = "~mid"

    /// Which of a peer's streams a remote stream id names.
    enum Role: String, Equatable {
        case main
        case middle
        case light
    }

    /// A remote MSID stream id, read.
    struct RemoteStreamId: Equatable {
        /// The source peer.
        let peerId: String
        /// The peer's main stream (audio and the camera as it is sent), its
        /// middle track, or its light companion.
        let role: Role
    }

    private static let suffixes: [(String, Role)] = [
        (lightSuffix, .light),
        (middleSuffix, .middle),
    ]

    /// Split a remote stream id into the peer it belongs to and which of that
    /// peer's streams it is. One suffix is read, at the end. A bare suffix
    /// (no peer id before it) names no peer and is read as an ordinary stream
    /// id.
    static func split(_ streamId: String) -> RemoteStreamId {
        for (suffix, role) in suffixes
        where streamId.count > suffix.count && streamId.hasSuffix(suffix) {
            return RemoteStreamId(peerId: String(streamId.dropLast(suffix.count)), role: role)
        }
        return RemoteStreamId(peerId: streamId, role: .main)
    }

    /// The light stream id of a peer (the inverse of ``split(_:)``).
    static func lightStreamId(of peerId: String) -> String {
        peerId + lightSuffix
    }

    /// The middle stream id of a peer (the inverse of ``split(_:)``).
    static func middleStreamId(of peerId: String) -> String {
        peerId + middleSuffix
    }
}

/// Where a remote video track of an SFU call goes.
enum RemoteVideoRoute: Equatable {
    /// A person's main picture, under the stream id the SFU gave it (`nil`
    /// when the binding reported none).
    case main(streamId: String?)
    /// The middle track of `peerId`.
    case middle(peerId: String)
    /// The light companion of `peerId`.
    case light(peerId: String)

    /// The route of a track that arrived under `streamId`.
    static func of(streamId: String?) -> RemoteVideoRoute {
        guard let streamId else { return .main(streamId: nil) }
        let id = LightStream.split(streamId)
        switch id.role {
        case .main: return .main(streamId: streamId)
        case .middle: return .middle(peerId: id.peerId)
        case .light: return .light(peerId: id.peerId)
        }
    }
}
