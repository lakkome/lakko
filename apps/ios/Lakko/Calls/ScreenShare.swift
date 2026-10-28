// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import Foundation
import LakkoCore
import WebRTC

/// Receiving a shared screen (ADR-0083, `docs/ux/screen-share.md` section 5).
///
/// This client never captures or sends a screen. It accepts signed share
/// states, folds them through the shared `CallScreenShareState` (C-87, written
/// once in `lakko-calls` so the three clients agree on the current share), and
/// shows the current share as a tile of its own.
///
/// Everything here is plain and testable without WebRTC: the fold wrapper, the
/// share as the screen shows it, the layout the share switches to and back,
/// what is announced, and the zoom arithmetic.

// MARK: - The fold

/// One call's share fold. A thin wrapper over the core's `CallScreenShareState`
/// that remembers the last `current()` it read, so a caller learns whether the
/// share on screen changed.
@MainActor
final class ScreenShareFold {
    private let state: CallScreenShareState?
    /// The current share, as of the last change.
    private(set) var current: CallCurrentScreenShare?

    /// `ownKey` is this device's own leaf signature key. This client never
    /// shares, so it decides only `isOwn`; a key the core refuses leaves the
    /// fold inert (no share is ever shown), which fails toward showing nothing.
    init(ownKey: Data) {
        state = try? CallScreenShareState(ownKey: ownKey)
    }

    /// Fold one verified state. Returns whether the current share changed.
    @discardableResult
    func observe(_ verified: VerifiedCallScreenShare) -> Bool {
        guard let state else { return false }
        _ = try? state.observe(shareState: verified)
        return refresh()
    }

    /// The participant whose leaf key is `signerKey` left: their share ends.
    /// Returns whether the current share changed.
    @discardableResult
    func participantLeft(signerKey: Data) -> Bool {
        guard let state else { return false }
        _ = state.participantLeft(signerKey: signerKey)
        return refresh()
    }

    private func refresh() -> Bool {
        let next = state?.current()
        guard next != current else { return false }
        current = next
        return true
    }
}

// MARK: - The share on screen

/// The share the screen shows: whose it is, and which share (a new share by
/// the same person is a different share).
struct ScreenShareShown: Equatable {
    /// The roster id of the sharer's camera tile (`"peer-<id>"` in a group
    /// call, `"remote"` in a one-to-one).
    let sharerParticipantId: String
    let seq: UInt64
    let signerKey: Data

    /// The share tile's own id, beside the sharer's camera tile.
    var tileId: String { ScreenShareShown.tileId(forSharer: sharerParticipantId) }

    static let tileIdPrefix = "screen:"

    static func tileId(forSharer participantId: String) -> String {
        tileIdPrefix + participantId
    }

    /// The current share of a fold as this screen shows it: the sharer's
    /// roster id from `sharerParticipantId(peerId)`. `nil` for no share, and
    /// for a share of this device's own (it never makes one).
    static func of(
        _ current: CallCurrentScreenShare?,
        sharerParticipantId: (String) -> String
    ) -> ScreenShareShown? {
        guard let current, !current.isOwn else { return nil }
        return ScreenShareShown(
            sharerParticipantId: sharerParticipantId(current.peerId),
            seq: current.seq,
            signerKey: current.signerKey)
    }
}

// MARK: - The layout

/// What a share does to the group layout (`docs/ux/screen-share.md` 5.2):
/// the first share switches to Focus, a replacing share changes nothing, and
/// when the last one stops the layout returns to what it was before the first
/// began.
struct ScreenShareLayoutMemory: Equatable {
    /// A share is live, and ``before`` holds the choice to return to.
    private(set) var active = false
    private(set) var before: GroupLayoutKind?

    /// The layout choice after the share went from live or not to `hasShare`.
    mutating func shareChanged(hasShare: Bool, choice: GroupLayoutKind?) -> GroupLayoutKind? {
        switch (active, hasShare) {
        case (false, true):
            active = true
            before = choice
            return .focus
        case (true, false):
            active = false
            let restored = before
            before = nil
            return restored
        default:
            return choice
        }
    }
}

// MARK: - What is announced

/// The two announcements of `docs/ux/screen-share.md` section 5.3, polite and
/// once each.
enum ScreenShareAnnouncement: Equatable {
    case started(sharerParticipantId: String)
    case stopped(sharerParticipantId: String)

    /// The catalog key of the line.
    var key: String {
        switch self {
        case .started: return "call.screen_share.started"
        case .stopped: return "call.screen_share.stopped"
        }
    }

    /// What to say when the share on screen goes from `old` to `new`: a new
    /// share is announced as started (a replacing one too: its sharer started
    /// sharing), a share that ends with none after it as stopped, and the
    /// same share again nothing.
    static func of(old: ScreenShareShown?, new: ScreenShareShown?) -> ScreenShareAnnouncement? {
        switch (old, new) {
        case (nil, nil):
            return nil
        case (let old?, nil):
            return .stopped(sharerParticipantId: old.sharerParticipantId)
        case (let old, let new?):
            if old == new { return nil }
            return .started(sharerParticipantId: new.sharerParticipantId)
        }
    }
}

// MARK: - Zoom

/// The zoom of a share on the large place: pinch, pan, and a double tap back
/// to whole (`docs/ux/screen-share.md` section 5.3). Pure arithmetic.
enum ScreenShareZoom {
    /// Whole: the share fits the place.
    static let minScale: CGFloat = 1
    /// Chosen, not measured: enough to read small text of a large screen.
    static let maxScale: CGFloat = 5
    /// One step of the accessibility zoom actions. Chosen.
    static let step: CGFloat = 1.5

    static func clampScale(_ scale: CGFloat) -> CGFloat {
        guard scale.isFinite else { return minScale }
        return min(maxScale, max(minScale, scale))
    }

    /// An offset kept inside what the zoomed picture can move: at whole there
    /// is nowhere to pan, and at scale `s` the picture can move half of the
    /// part of it that lies outside the place, either way.
    static func clampOffset(_ offset: CGSize, scale: CGFloat, size: CGSize) -> CGSize {
        let s = clampScale(scale)
        let maxX = max(0, (s - 1) * size.width / 2)
        let maxY = max(0, (s - 1) * size.height / 2)
        func clamp(_ v: CGFloat, _ m: CGFloat) -> CGFloat {
            guard v.isFinite else { return 0 }
            return min(m, max(-m, v))
        }
        return CGSize(width: clamp(offset.width, maxX), height: clamp(offset.height, maxY))
    }
}

// MARK: - The share's picture

/// The sharers' screen tracks and their first-frame probes, keyed on the
/// sharer's roster id. A share tile says it is waiting for the picture until
/// its track renders a frame (`docs/ux/screen-share.md` section 5.3); a track
/// handle arrives with the SDP and proves nothing.
@MainActor
final class ScreenShareTracks {
    private var probes: [String: RemoteVideoFrameProbe] = [:]
    private var probed: [String: RTCVideoTrack] = [:]

    /// Keep `track` (or, `nil`, drop it) as `id`'s screen and probe it.
    func attach(_ track: RTCVideoTrack?, sharer id: String, ui: CallUiState) {
        detach(id)
        ui.setScreenShareTrack(track, forSharer: id)
        guard let track else { return }
        let probe = RemoteVideoFrameProbe { [weak self, weak ui] in
            guard let self, self.probed[id] === track else { return }
            NSLog("ScreenShareTracks: screen share first frame rendered peer=%@", id)
            ui?.setScreenShareFramed(true, forSharer: id)
        }
        probes[id] = probe
        probed[id] = track
        track.add(probe)
    }

    /// Detach every probe: the call ended.
    func detachAll(ui: CallUiState) {
        for id in Array(probed.keys) { attach(nil, sharer: id, ui: ui) }
    }

    private func detach(_ id: String) {
        if let probe = probes[id], let track = probed[id] { track.remove(probe) }
        probes.removeValue(forKey: id)
        probed.removeValue(forKey: id)
    }
}
