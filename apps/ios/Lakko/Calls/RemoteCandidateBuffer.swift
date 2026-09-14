// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// One remote ICE candidate as it arrived on the signaling wire.
struct RemoteIceCandidate: Equatable {
    let sdpMid: String
    let sdpMLineIndex: Int32
    let candidate: String
}

/// Holds remote ICE candidates that arrive before the 1x1 peer connection can
/// take them, and releases them in arrival order once it can. The Swift twin of
/// Android's `RemoteCandidateBuffer`.
///
/// ## The defect it closes
///
/// The caller trickles its candidates right behind its offer. On the answering
/// side the offer only rings: the peer connection is built when the user
/// accepts, so every candidate that arrived while ringing found no connection
/// and was dropped. On both sides a candidate can also land between the SDP
/// arriving and `setRemoteDescription` completing, and WebRTC refuses a
/// candidate while there is no remote description. Losing the candidates that
/// carry the only reachable path (a relay candidate, say) leaves ICE with
/// nothing to connect over.
///
/// ## Contract
///
/// * Until `markRemoteDescriptionApplied()`, `offer(_:)` keeps the candidate and
///   returns `true`; the caller must not apply it.
/// * `markRemoteDescriptionApplied()` returns everything kept, oldest first,
///   exactly once; from then on `offer(_:)` returns `false` and the caller
///   applies the candidate directly.
/// * `reset()` forgets everything and buffers again (call teardown).
/// * At most `capacity` candidates are kept; later ones are dropped, so a peer
///   cannot grow this without bound while a call rings.
///
/// Not thread-safe on its own: `CallSession` is `@MainActor` and is the only
/// owner.
struct RemoteCandidateBuffer {
    static let defaultCapacity = 64

    private let capacity: Int
    private var pending: [RemoteIceCandidate] = []
    private var remoteDescriptionApplied = false
    private(set) var dropped = 0

    init(capacity: Int = RemoteCandidateBuffer.defaultCapacity) {
        self.capacity = capacity
    }

    /// Keep `candidate` if the remote description is not applied yet.
    /// - Returns: `true` when the buffer took (or, over capacity, discarded)
    ///   the candidate; `false` when the caller should apply it now.
    mutating func offer(_ candidate: RemoteIceCandidate) -> Bool {
        if remoteDescriptionApplied { return false }
        if pending.count < capacity {
            pending.append(candidate)
        } else {
            dropped += 1
        }
        return true
    }

    /// Record that the remote description is set; returns the kept
    /// candidates, oldest first.
    mutating func markRemoteDescriptionApplied() -> [RemoteIceCandidate] {
        if remoteDescriptionApplied { return [] }
        remoteDescriptionApplied = true
        let drained = pending
        pending.removeAll()
        return drained
    }

    /// Forget every kept candidate and buffer again.
    mutating func reset() {
        pending.removeAll()
        remoteDescriptionApplied = false
        dropped = 0
    }
}
