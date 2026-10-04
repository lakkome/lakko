// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The light companion of a remote camera (the twin of
/// `apps/web/src/calls/light-stream.ts`).
///
/// For a publisher that sends two simulcast layers the SFU gives each
/// subscriber two video tracks: the main one, whose MSID stream id is the
/// source peer id (as for every remote stream), and a light companion that
/// carries only the light layer, whose stream id is the source peer id
/// followed by ``suffix``. A publisher without simulcast has no companion.
///
/// The companion is a second picture of a person already in the call. It is
/// never a participant of its own: no tile, no camera flag. The tile of its
/// peer decodes it beside the main track and shows it while the main one
/// stands still (``RemoteLayerController``).
enum LightStream {

    /// What the SFU appends to a peer id to name that peer's light companion.
    static let suffix = "~light"

    /// A remote MSID stream id, read.
    struct RemoteStreamId: Equatable {
        /// The source peer.
        let peerId: String
        /// Whether the stream is that peer's light companion.
        let light: Bool
    }

    /// Split a remote stream id into the peer it belongs to and whether it is
    /// the light companion. The bare suffix (no peer id before it) names no
    /// peer and is read as an ordinary stream id.
    static func split(_ streamId: String) -> RemoteStreamId {
        if streamId.count > suffix.count, streamId.hasSuffix(suffix) {
            return RemoteStreamId(peerId: String(streamId.dropLast(suffix.count)), light: true)
        }
        return RemoteStreamId(peerId: streamId, light: false)
    }

    /// The light stream id of a peer (the inverse of ``split(_:)``).
    static func lightStreamId(of peerId: String) -> String {
        peerId + suffix
    }
}

/// Where a remote video track of an SFU call goes.
enum RemoteVideoRoute: Equatable {
    /// A person's main picture, under the stream id the SFU gave it (`nil`
    /// when the binding reported none).
    case main(streamId: String?)
    /// The light companion of `peerId`.
    case light(peerId: String)

    /// The route of a track that arrived under `streamId`.
    static func of(streamId: String?) -> RemoteVideoRoute {
        guard let streamId else { return .main(streamId: nil) }
        let id = LightStream.split(streamId)
        return id.light ? .light(peerId: id.peerId) : .main(streamId: streamId)
    }
}
