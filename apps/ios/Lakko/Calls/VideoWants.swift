// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import Foundation
import LakkoCore

/// Which picture of a person a tile shows, and what this device asks the SFU
/// for (`docs/ux/group-call-grid.md` section 7.6, ADR-0070 step 5); the twin
/// of `apps/web/src/calls/video-wants.ts`.
///
/// A person whose camera is on sends up to three copies of it: the main one,
/// a middle one (half the size) and the light one (a quarter). The light one
/// always arrives. Of the two heavy ones the SFU forwards the one this device
/// says it shows, per person, in a `video-wants` frame; a person this device
/// shows no heavy picture of costs it the light stream alone.
///
/// Which one a tile shows is the shared rule (`tile_video_quality` of
/// `lakko-ui-rules`, over UniFFI here): the people in the call, the tile's
/// width in physical pixels, and whether it is the pinned person or the
/// stage. A quality is the binding's own `TileVideoQuality`.
///
/// Pure: no SwiftUI, no peer connection; timers and the clock are handed in.
enum VideoWants {

    /// The most entries a `video-wants` frame carries
    /// (`lakko_sfu_proto::MAX_VIDEO_WANTS`).
    static let maxEntries = 64
    /// The fewest milliseconds between two `video-wants` frames. Chosen.
    static let minIntervalMs: Int64 = 200
    /// How long a tile's width must stand still before it is used,
    /// milliseconds. Chosen.
    static let tileWidthSettleMs: Int64 = 300

    /// A width in points as physical pixels: what a picture is actually
    /// stretched over. A scale the screen does not give (or gives as
    /// nonsense) is read as 1.
    static func physicalTileWidth(points: CGFloat, scale: CGFloat) -> UInt32 {
        let ratio = scale.isFinite && scale > 0 ? scale : 1
        guard points.isFinite, points > 0 else { return 0 }
        return UInt32(clamping: Int((points * ratio).rounded()))
    }

    /// Which picture a tile shows: the shared rule's answer. `people` counts
    /// everyone in the call, this device's own person included;
    /// `tileWidthPx` is in physical pixels.
    static func tileQuality(
        _ rules: TileQualityRules = SharedTileQualityRules(),
        people: Int, tileWidthPx: UInt32, pinnedOrStage: Bool
    ) -> TileVideoQuality {
        rules.tileVideoQuality(
            people: UInt32(clamping: max(0, people)), tileWidthPx: tileWidthPx,
            pinnedOrStage: pinnedOrStage)
    }

    /// What a tile asks the SFU for, given the rule's answer. The rule's
    /// `light` means the light stream alone, which is nothing at all for a
    /// person who sends no light copy (a client from before ADR-0070): such a
    /// tile asks for `middle`, which the SFU serves as the main track when
    /// there is no middle one either.
    static func askedQuality(_ quality: TileVideoQuality, hasLight: Bool) -> TileVideoQuality {
        quality == .light && !hasLight ? .middle : quality
    }

    /// The track a tile's heavy renderer plays for a quality: the main track
    /// for `main`; the middle track for `middle`, or the main one while the
    /// person has no middle track (the SFU forwards the main track then);
    /// none for `light`, where the light track alone is shown.
    static func heavyTrack<T>(for quality: TileVideoQuality, main: T?, middle: T?) -> T? {
        switch quality {
        case .light: return nil
        case .middle: return middle ?? main
        case .main: return main
        }
    }

    /// `wants` as a frame within the protocol's bound. A frame replaces the
    /// whole set and a person it leaves out is at `main`, so past
    /// ``maxEntries`` people the `main` entries are the ones left out; if the
    /// rest still does not fit, the last to join are left out (and are sent
    /// in full).
    static func frame(
        _ wants: [(peer: String, quality: TileVideoQuality)], max: Int = maxEntries
    ) -> [SfuVideoWant] {
        var entries = wants
        if entries.count > max { entries = entries.filter { $0.quality != .main } }
        if entries.count > max { entries = Array(entries.prefix(max)) }
        return entries.map { SfuVideoWant(peer: $0.peer, quality: $0.quality) }
    }

    /// The one line a sent `video-wants` frame gets in the call log.
    static func describe(_ frame: [SfuVideoWant]) -> String {
        "call video wants "
            + frame.map { "\($0.peer)=\($0.quality.wireName)" }.joined(separator: " ")
    }
}

/// The part of the shared rules a tile asks. A protocol so a test can hand in
/// the real binding or a recorder alike.
protocol TileQualityRules {
    func tileVideoQuality(
        people: UInt32, tileWidthPx: UInt32, pinnedOrStage: Bool
    ) -> TileVideoQuality
    func middleSenderWanted(people: UInt32) -> Bool
}

/// The rules as `lakko-ui-rules` answers them.
struct SharedTileQualityRules: TileQualityRules {
    func tileVideoQuality(
        people: UInt32, tileWidthPx: UInt32, pinnedOrStage: Bool
    ) -> TileVideoQuality {
        LakkoCore.tileVideoQuality(
            people: people, tileWidthPx: tileWidthPx, pinnedOrStage: pinnedOrStage)
    }

    func middleSenderWanted(people: UInt32) -> Bool {
        LakkoCore.middleSenderWanted(people: people)
    }
}

extension TileVideoQuality {
    /// main over middle over light.
    var rank: Int {
        switch self {
        case .light: return 0
        case .middle: return 1
        case .main: return 2
        }
    }

    /// The name the quality has in the `video-wants` frame and the call log.
    var wireName: String {
        switch self {
        case .main: return "main"
        case .middle: return "middle"
        case .light: return "light"
        }
    }

    /// The higher of two qualities.
    static func higher(_ a: TileVideoQuality, _ b: TileVideoQuality) -> TileVideoQuality {
        a.rank >= b.rank ? a : b
    }

    /// The lower of two qualities.
    static func lower(_ a: TileVideoQuality, _ b: TileVideoQuality) -> TileVideoQuality {
        a.rank <= b.rank ? a : b
    }
}

/// What every mounted tile wants, by person.
///
/// A person can be drawn by several tiles at once (the stage and the strip
/// while the layout changes), so each tile reports under its own key and the
/// person's want is the highest of them. A person of the call no tile reports
/// for (another page of the grid, a camera that is off, a collapsed call)
/// wants `light`: the SFU then forwards neither heavy track of theirs.
struct VideoWantsBook {

    /// The people of the call other than this device, in the order they
    /// joined.
    private var peers: [String] = []
    private var tiles: [ObjectIdentifier: (peerId: String, quality: TileVideoQuality)] = [:]

    init() {}

    /// The people of the call, other than this device: replaces the set.
    mutating func setPeers(_ peerIds: [String]) {
        peers = []
        for id in peerIds where !peers.contains(id) { peers.append(id) }
    }

    /// A person joined.
    mutating func addPeer(_ peerId: String) {
        if !peers.contains(peerId) { peers.append(peerId) }
    }

    /// A person left: they are in no later set.
    mutating func removePeer(_ peerId: String) {
        peers.removeAll { $0 == peerId }
    }

    /// How many people are in the call, this device's own person included.
    var people: Int { peers.count + 1 }

    /// The tile `tile` (the identity of any object that lives as long as the
    /// tile) shows `peerId` at `quality`; `nil` when it stops showing a
    /// picture of them. Returns whether anything changed.
    @discardableResult
    mutating func setTile(
        _ tile: ObjectIdentifier, peerId: String, quality: TileVideoQuality?
    ) -> Bool {
        guard let quality else { return tiles.removeValue(forKey: tile) != nil }
        if let before = tiles[tile], before.peerId == peerId, before.quality == quality {
            return false
        }
        tiles[tile] = (peerId, quality)
        return true
    }

    /// What this device shows of `peerId`: the highest of their mounted
    /// tiles, `light` with none.
    func quality(of peerId: String) -> TileVideoQuality {
        tiles.values.reduce(TileVideoQuality.light) { best, tile in
            tile.peerId == peerId ? TileVideoQuality.higher(best, tile.quality) : best
        }
    }

    /// The whole set: every person of the call, in the order they joined.
    func wants() -> [(peer: String, quality: TileVideoQuality)] {
        peers.map { (peer: $0, quality: quality(of: $0)) }
    }

    /// Forget everything: the call is over.
    mutating func clear() {
        peers = []
        tiles = [:]
    }
}

/// The timer and the clock ``VideoWantsSender``, ``SettledWidth`` and
/// ``TileControlsReveal`` use; replaced in tests.
struct CallTimerDeps {
    /// Milliseconds on any clock.
    var now: () -> Int64
    /// Run `work` after `ms`; returns what cancels it.
    var schedule: (_ ms: Int64, _ work: @escaping () -> Void) -> () -> Void

    /// The main queue and the wall clock.
    static let main = CallTimerDeps(
        now: { Int64(Date().timeIntervalSince1970 * 1000) },
        schedule: { ms, work in
            let item = DispatchWorkItem(block: work)
            DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(Int(max(0, ms))), execute: item)
            return { item.cancel() }
        })
}

/// Sends the book's whole set when it changed, coalesced; the twin of the web
/// `VideoWantsSender`.
///
/// Every send goes through a timer, so the reports of one turn (every tile of
/// a grid measuring itself in the same pass) become one frame, and two frames
/// are never closer than `minIntervalMs`. ``changed()`` sends only a set that
/// differs from the last one sent; ``resend()`` sends the set whatever it is
/// (after the publish offer went out, and after a person joins). `send`
/// returns whether the frame went out; one that did not (no connection yet)
/// is tried again at the next call. An empty set is never sent: with nobody
/// else in the call there is nothing to ask for.
final class VideoWantsSender {

    private let wants: () -> [(peer: String, quality: TileVideoQuality)]
    private let send: ([SfuVideoWant]) -> Bool
    private let deps: CallTimerDeps
    private let minIntervalMs: Int64
    private var lastKey: String?
    private var lastSentAt: Int64?
    private var cancelTimer: (() -> Void)?
    private var force = false

    init(
        wants: @escaping () -> [(peer: String, quality: TileVideoQuality)],
        send: @escaping ([SfuVideoWant]) -> Bool,
        deps: CallTimerDeps = .main,
        minIntervalMs: Int64 = VideoWants.minIntervalMs
    ) {
        self.wants = wants
        self.send = send
        self.deps = deps
        self.minIntervalMs = minIntervalMs
    }

    /// The set may have changed.
    func changed() {
        schedule()
    }

    /// Send the set at the next opportunity even if it is the one last sent.
    func resend() {
        force = true
        schedule()
    }

    /// Stop: nothing is sent after this, and a later use starts afresh.
    func reset() {
        cancelTimer?()
        cancelTimer = nil
        lastKey = nil
        lastSentAt = nil
        force = false
    }

    private func schedule() {
        guard cancelTimer == nil else { return }
        let wait = lastSentAt.map { max(0, $0 + minIntervalMs - deps.now()) } ?? 0
        cancelTimer = deps.schedule(wait) { [weak self] in
            guard let self else { return }
            self.cancelTimer = nil
            self.flush()
        }
    }

    private func flush() {
        let frame = VideoWants.frame(wants())
        let key = frame.map { "\($0.peer)=\($0.quality.wireName)" }.joined(separator: ",")
        if frame.isEmpty {
            lastKey = key
            force = false
            return
        }
        if !force, key == lastKey { return }
        guard send(frame) else { return }
        lastKey = key
        force = false
        lastSentAt = deps.now()
    }
}

/// A tile's width, settled. The first width a tile measures is used at once
/// (a new tile has nothing to keep showing); every later one only after it
/// has stood for ``VideoWants/tileWidthSettleMs``, so a rotation or a layout
/// that changes in steps does not ask the sender for a key frame on every
/// step.
final class SettledWidth {

    private let apply: (UInt32) -> Void
    private let deps: CallTimerDeps
    private let settleMs: Int64
    private var measured = false
    private var applied: UInt32 = 0
    private var cancelTimer: (() -> Void)?

    init(
        deps: CallTimerDeps = .main,
        settleMs: Int64 = VideoWants.tileWidthSettleMs,
        apply: @escaping (UInt32) -> Void
    ) {
        self.apply = apply
        self.deps = deps
        self.settleMs = settleMs
    }

    /// The tile was measured at `width`.
    func observe(_ width: UInt32) {
        cancelTimer?()
        cancelTimer = nil
        if !measured {
            measured = true
            applied = width
            apply(width)
            return
        }
        if width == applied { return }
        cancelTimer = deps.schedule(settleMs) { [weak self] in
            guard let self else { return }
            self.cancelTimer = nil
            self.applied = width
            self.apply(width)
        }
    }

    /// The tile is gone: nothing is applied after this.
    func dispose() {
        cancelTimer?()
        cancelTimer = nil
    }
}
