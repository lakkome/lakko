// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// What a group call's video is doing, read from the peer connection's own
/// counters and from the tiles (ADR-0067 steps 1 and 5); the twin of
/// `apps/web/src/calls/media-counters.ts` for the lines this client logs.
///
/// Diagnostics only: counts and durations, nothing about who is in the call
/// beyond the SFU's per-call peer id, which the stream ids already carry.
///
/// Pure: plain statistics entries in, plain lines out. The session owns the
/// statistics read and the timer.

/// One entry of a statistics report with its id, which stays the same for a
/// stream across readings.
struct CallMediaStatEntry {
    let id: String
    let type: String
    let values: [String: NSObject]
}

/// Which of a remote camera's tracks a video receiver carries: the main one,
/// the middle one or the light companion (``LightStream``). `unknown` when
/// the report does not say which track the receiver belongs to.
enum ReceiverLayer: String, Equatable {
    case main
    case middle
    case light
    case unknown = "-"
}

/// One remote video stream's cumulative receiver counters.
struct InboundVideoCounters: Equatable {
    var layer: ReceiverLayer
    var packetsReceived: Int64 = 0
    var packetsLost: Int64 = 0
    var nackCount: Int64 = 0
    var pliCount: Int64 = 0
    var freezeCount: Int64 = 0
    var totalFreezesDuration: Double = 0
    var framesDecoded: Int64 = 0
    var framesDropped: Int64 = 0
    var jitterBufferDelay: Double = 0
    var jitterBufferEmittedCount: Int64 = 0
}

/// One of this device's own camera senders, as the statistics report it.
struct OutboundVideoSender: Equatable {
    /// Which sender it is; `nil` when the report does not tie the stream to
    /// one of the three.
    var sender: CameraSenderKind?
    var scalabilityMode: String?
    var qualityLimitationReason: String?
    var frameWidth: Int64?
    var frameHeight: Int64?
    var framesPerSecond: Double?
    /// Cumulative bytes sent on the stream.
    var bytesSent: Int64 = 0
}

/// What the call log says of one sender's activity (``SenderActivityRule``):
/// the SFU's last word for it (`nil`: none) and whether its encoding is asked
/// to be active.
struct OutboundActivity: Equatable {
    let needed: Bool?
    let active: Bool
}

/// One statistics reading, reduced to the video this client logs.
struct CallMediaReading: Equatable {
    /// Remote video streams, ordered by statistics id so a stream keeps its
    /// number from one interval to the next.
    var inbound: [(id: String, counters: InboundVideoCounters)] = []
    /// Every camera sender the report carries, in the order main, middle,
    /// light, then any the report does not name.
    var outbound: [OutboundVideoSender] = []

    static func == (a: CallMediaReading, b: CallMediaReading) -> Bool {
        a.outbound == b.outbound
            && a.inbound.count == b.inbound.count
            && zip(a.inbound, b.inbound).allSatisfy { $0.id == $1.id && $0.counters == $1.counters }
    }
}

enum CallMediaCounters {

    /// The label of a per-interval line.
    static let intervalLabel = "call media interval"
    /// The label of an end-of-call line.
    static let totalLabel = "call media total"
    /// How often the session reads, milliseconds (the web client's interval).
    static let intervalMs: UInt64 = 2_000

    /// Reduce a report to a reading.
    ///
    /// `lightTrackIds` and `middleTrackIds` hold the ids of the remote tracks
    /// that are light companions and middle tracks; a video receiver whose
    /// `trackIdentifier` is among them is `light` or `middle`, any other is
    /// `main`.
    ///
    /// An `outbound-rtp` video stream is one of this device's camera
    /// senders. Which one is read from the stream's `mid` (`senderOfMid`,
    /// the media lines the session knows) and, where the report carries no
    /// `mid` or the session does not know it, from the track id of the
    /// stream's `media-source` (``CameraSender/senderKind(ofTrackId:)``).
    static func read(
        _ entries: [CallMediaStatEntry],
        lightTrackIds: Set<String>,
        middleTrackIds: Set<String> = [],
        senderOfMid: [String: CameraSenderKind] = [:]
    ) -> CallMediaReading {
        var reading = CallMediaReading()
        // The local track behind each media source, by the source's id.
        var sourceTrackIds: [String: String] = [:]
        for entry in entries where entry.type == "media-source" {
            if let trackId = entry.values["trackIdentifier"] as? String {
                sourceTrackIds[entry.id] = trackId
            }
        }
        for entry in entries {
            // `kind` is the current spelling; `mediaType` the older one.
            let kind = (entry.values["kind"] as? String) ?? (entry.values["mediaType"] as? String)
            guard kind == "video" else { continue }
            let int: (String) -> Int64 = { (entry.values[$0] as? NSNumber)?.int64Value ?? 0 }
            let double: (String) -> Double = {
                let v = (entry.values[$0] as? NSNumber)?.doubleValue ?? 0
                return v.isFinite ? v : 0
            }
            let string: (String) -> String? = {
                guard let s = entry.values[$0] as? String, !s.isEmpty else { return nil }
                return s
            }
            switch entry.type {
            case "inbound-rtp":
                let layer: ReceiverLayer
                if let trackId = string("trackIdentifier") {
                    if lightTrackIds.contains(trackId) {
                        layer = .light
                    } else if middleTrackIds.contains(trackId) {
                        layer = .middle
                    } else {
                        layer = .main
                    }
                } else {
                    layer = .unknown
                }
                reading.inbound.append((
                    id: entry.id,
                    counters: InboundVideoCounters(
                        layer: layer,
                        packetsReceived: int("packetsReceived"),
                        packetsLost: int("packetsLost"),
                        nackCount: int("nackCount"),
                        pliCount: int("pliCount"),
                        freezeCount: int("freezeCount"),
                        totalFreezesDuration: double("totalFreezesDuration"),
                        framesDecoded: int("framesDecoded"),
                        framesDropped: int("framesDropped"),
                        jitterBufferDelay: double("jitterBufferDelay"),
                        jitterBufferEmittedCount: int("jitterBufferEmittedCount"))
                ))
            case "outbound-rtp":
                var sender = string("mid").flatMap { senderOfMid[$0] }
                if sender == nil, let sourceId = string("mediaSourceId"),
                    let trackId = sourceTrackIds[sourceId] {
                    sender = CameraSender.senderKind(ofTrackId: trackId)
                }
                reading.outbound.append(OutboundVideoSender(
                    sender: sender,
                    scalabilityMode: string("scalabilityMode"),
                    qualityLimitationReason: string("qualityLimitationReason"),
                    frameWidth: (entry.values["frameWidth"] as? NSNumber)?.int64Value,
                    frameHeight: (entry.values["frameHeight"] as? NSNumber)?.int64Value,
                    framesPerSecond: (entry.values["framesPerSecond"] as? NSNumber)?.doubleValue,
                    bytesSent: int("bytesSent")))
            default:
                continue
            }
        }
        reading.inbound.sort { $0.id < $1.id }
        reading.outbound.sort { senderOrder($0.sender) < senderOrder($1.sender) }
        return reading
    }

    /// The order senders are named in: main, middle, light, then unnamed.
    private static func senderOrder(_ sender: CameraSenderKind?) -> Int {
        sender.flatMap { CameraSenderKind.allCases.firstIndex(of: $0) }
            ?? CameraSenderKind.allCases.count
    }

    /// One line per remote video stream: how it changed since `before` (from
    /// zero when `before` is `nil`, when the stream is new, or when its
    /// counters went backwards). Each line ends with `layer=`: `main`,
    /// `middle`, `light`, or `-` when unknown.
    static func describeInbound(
        _ label: String, before: CallMediaReading?, now: CallMediaReading
    ) -> [String] {
        var lines: [String] = []
        for (index, stream) in now.inbound.enumerated() {
            let cur = stream.counters
            var base = InboundVideoCounters(layer: cur.layer)
            if let prev = before?.inbound.first(where: { $0.id == stream.id })?.counters,
                prev.packetsReceived <= cur.packetsReceived {
                base = prev
            }
            let emitted = max(0, cur.jitterBufferEmittedCount - base.jitterBufferEmittedCount)
            let jitterMs: String = emitted > 0
                ? String(
                    format: "%.0f",
                    max(0, cur.jitterBufferDelay - base.jitterBufferDelay) / Double(emitted) * 1000)
                : "-"
            let freezeSeconds = String(
                format: "%.2f", max(0, cur.totalFreezesDuration - base.totalFreezesDuration))
            lines.append(
                "\(label) video#\(index + 1)"
                    + " pkts=\(max(0, cur.packetsReceived - base.packetsReceived))"
                    + " lost=\(max(0, cur.packetsLost - base.packetsLost))"
                    + " nack=\(max(0, cur.nackCount - base.nackCount))"
                    + " jb_ms=\(jitterMs)"
                    + " pli=\(max(0, cur.pliCount - base.pliCount))"
                    + " freezes=\(max(0, cur.freezeCount - base.freezeCount))"
                    + " freeze_s=\(freezeSeconds)"
                    + " decoded=\(max(0, cur.framesDecoded - base.framesDecoded))"
                    + " dropped=\(max(0, cur.framesDropped - base.framesDropped))"
                    + " layer=\(cur.layer.rawValue)")
        }
        return lines
    }

    /// The camera, one line for each of its senders (main, middle, light):
    /// the size and frame rate it sends, the bitrate over the interval since
    /// `before` (`-` without an earlier reading or an elapsed time), and,
    /// where `activityOf` answers, the SFU's last word for it and whether its
    /// encoding is asked to be active: `needed=yes|no|- active=yes|no`. When
    /// `ceilingOf` is given (``PublishBudgetRule``: the bitrate ceiling the
    /// sender's encoding holds, bits a second, `nil` for none) every line
    /// ends with `ceiling=N` in kbit/s, or `ceiling=-`.
    ///
    /// A sender named in `existing` that the report carries no stream for
    /// (an inactive encoding may report none) still has its line, all
    /// dashes, so a sender that was switched off does not vanish from the
    /// log.
    static func describeOutbound(
        _ label: String,
        before: CallMediaReading? = nil,
        now: CallMediaReading,
        elapsedMs: UInt64 = 0,
        activityOf: (CameraSenderKind) -> OutboundActivity? = { _ in nil },
        existing: [CameraSenderKind] = [],
        ceilingOf: ((CameraSenderKind) -> Int?)? = nil
    ) -> [String] {
        var senders = now.outbound
        for kind in CameraSenderKind.allCases
        where existing.contains(kind) && !senders.contains(where: { $0.sender == kind }) {
            senders.append(OutboundVideoSender(sender: kind))
        }
        return senders.map { out in
            let size: String
            if let w = out.frameWidth, let h = out.frameHeight {
                size = "\(w)x\(h)"
            } else {
                size = "-"
            }
            let fps = out.framesPerSecond.map { String(format: "%.0f", $0) } ?? "-"
            var kbps = "-"
            if elapsedMs > 0, let sender = out.sender,
                let prev = before?.outbound.first(where: { $0.sender == sender }),
                prev.bytesSent <= out.bytesSent {
                let bits = Double(out.bytesSent - prev.bytesSent) * 8
                kbps = String(format: "%.0f", bits / Double(elapsedMs))
            }
            let activity = out.sender.flatMap(activityOf)
            return "\(label) send video mode=\(out.scalabilityMode ?? "-") size=\(size)"
                + " fps=\(fps) limited_by=\(out.qualityLimitationReason ?? "-")"
                + " kbps=\(kbps) sender=\(out.sender?.rawValue ?? "-")"
                + (activity.map {
                    SenderActivityRule.describe(needed: $0.needed, active: $0.active)
                } ?? "")
                + (ceilingOf.map { ceilingOf in
                    PublishBudgetRule.describeSenderCeiling(out.sender.flatMap(ceilingOf))
                } ?? "")
        }
    }

    /// What one remote peer's tile showed, in one line: which picture this
    /// device asks for of them and which one the SFU gives (`-` when it has
    /// not said), how often the tile switched to the light track, how long
    /// the light track was the shown one, and the longest and the total
    /// displayed stall. Whole milliseconds. `peerId` is the SFU's per-call
    /// peer id, the one the stream ids carry.
    static func describeShown(
        _ label: String, peerId: String, _ shown: LayerDisplayCounters,
        want: TileVideoQuality, given: TileVideoQuality? = nil
    ) -> String {
        "\(label) shown peer=\(peerId) want=\(want.wireName) given=\(given?.wireName ?? "-")"
            + " light_switches=\(shown.lightSwitches)"
            + " light_ms=\(shown.lightMs) stall_max_ms=\(shown.stallMaxMs)"
            + " stall_total_ms=\(shown.stallTotalMs)"
    }

    /// ``describeShown(_:peerId:_:want:given:)`` for every peer, in the order
    /// given.
    static func describeShownPeers(
        _ label: String, _ byPeer: [(peerId: String, counters: LayerDisplayCounters)],
        wantOf: (String) -> TileVideoQuality,
        givenOf: (String) -> TileVideoQuality? = { _ in nil }
    ) -> [String] {
        byPeer.map {
            describeShown(
                label, peerId: $0.peerId, $0.counters, want: wantOf($0.peerId),
                given: givenOf($0.peerId))
        }
    }
}

extension LayerDisplayCounters {
    /// Counters with nothing counted.
    static let zero = LayerDisplayCounters(
        lightSwitches: 0, lightMs: 0, stallMaxMs: 0, stallTotalMs: 0)

    /// Two sets of counters as one: sums, and the longer of the longest
    /// stalls.
    func adding(_ other: LayerDisplayCounters) -> LayerDisplayCounters {
        LayerDisplayCounters(
            lightSwitches: lightSwitches &+ other.lightSwitches,
            lightMs: lightMs &+ other.lightMs,
            stallMaxMs: max(stallMaxMs, other.stallMaxMs),
            stallTotalMs: stallTotalMs &+ other.stallTotalMs)
    }
}

/// What a tile hands the ledger: the two reads of the shared rule
/// (`LayerDisplay` of `lakko-ui-rules`). A protocol so the ledger's own
/// arithmetic can be tested with a plain value.
protocol LayerCounterSource: AnyObject {
    /// Everything counted since the last drain, and start a new interval.
    func drain() -> LayerDisplayCounters
    /// Everything counted since the tile started.
    func counters() -> LayerDisplayCounters
}

extension LayerDisplay: LayerCounterSource {}

/// A tile's counters that can also say which picture the tile shows now.
protocol LayerShownSource: AnyObject {
    /// What the tile shows.
    func shown() -> ShownLayer
}

extension LayerDisplay: LayerShownSource {}

/// What every remote tile of a call counted, by peer.
///
/// A peer can be drawn by several tiles over a call (the grid, the stage, a
/// tile rebuilt when the layout changes). Each tile registers its counters
/// while it is on screen. An interval is the tiles' `drain()`; the total is
/// their `counters()`. A tile that goes away leaves both behind.
@MainActor
final class LayerDisplayLedger {

    /// The ledger of the call this device is in: the tiles write to it, the
    /// group call's media log reads it.
    static let shared = LayerDisplayLedger()

    private var live: [Int: (peerId: String, source: LayerCounterSource)] = [:]
    private var nextToken = 0
    /// Drained from tiles that have since gone, not yet reported.
    private var pendingInterval: [String: LayerDisplayCounters] = [:]
    /// The whole count of tiles that have gone.
    private var finishedTotal: [String: LayerDisplayCounters] = [:]

    /// A tile of `peerId` is on screen. Returns what ``unregister(_:)`` takes.
    func register(peerId: String, source: LayerCounterSource) -> Int {
        nextToken += 1
        live[nextToken] = (peerId, source)
        return nextToken
    }

    /// The tile is gone: keep what it counted.
    func unregister(_ token: Int) {
        guard let entry = live.removeValue(forKey: token) else { return }
        pendingInterval[entry.peerId] =
            (pendingInterval[entry.peerId] ?? .zero).adding(entry.source.drain())
        finishedTotal[entry.peerId] =
            (finishedTotal[entry.peerId] ?? .zero).adding(entry.source.counters())
    }

    /// Every peer a tile showed since the last call, ordered by peer id, and
    /// start a new interval. A peer no tile showed is absent.
    func takeInterval() -> [(peerId: String, counters: LayerDisplayCounters)] {
        var byPeer = pendingInterval
        pendingInterval.removeAll()
        for entry in live.values {
            byPeer[entry.peerId] = (byPeer[entry.peerId] ?? .zero).adding(entry.source.drain())
        }
        return Self.ordered(byPeer)
    }

    /// Every peer a tile ever showed, counted from the start.
    func totals() -> [(peerId: String, counters: LayerDisplayCounters)] {
        var byPeer = finishedTotal
        for entry in live.values {
            byPeer[entry.peerId] = (byPeer[entry.peerId] ?? .zero).adding(entry.source.counters())
        }
        return Self.ordered(byPeer)
    }

    /// Whether a tile of `peerId` on screen shows its light picture now
    /// (``RemoteLayerController``). `false` with no tile of theirs, or with
    /// one that cannot say.
    func lightShown(peerId: String) -> Bool {
        live.values.contains {
            $0.peerId == peerId && ($0.source as? LayerShownSource)?.shown() == .light
        }
    }

    /// Forget what gone tiles counted: a new call starts. Tiles still on
    /// screen stay registered.
    func reset() {
        pendingInterval.removeAll()
        finishedTotal.removeAll()
    }

    private static func ordered(
        _ byPeer: [String: LayerDisplayCounters]
    ) -> [(peerId: String, counters: LayerDisplayCounters)] {
        byPeer.keys.sorted().map { (peerId: $0, counters: byPeer[$0] ?? .zero) }
    }
}
