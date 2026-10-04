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

/// Which of a remote camera's two tracks a video receiver carries: the main
/// one or the light companion (``LightStream``). `unknown` when the report
/// does not say which track the receiver belongs to.
enum ReceiverLayer: String, Equatable {
    case main
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

/// One layer of this device's own camera sender.
struct OutboundVideoLayer: Equatable {
    /// The layer's RTP stream id (`q`, `f`); `nil` when the camera is sent as
    /// one encoding.
    var rid: String?
    var scalabilityMode: String?
    var qualityLimitationReason: String?
    var frameWidth: Int64?
    var frameHeight: Int64?
    var framesPerSecond: Double?
}

/// One statistics reading, reduced to the video this client logs.
struct CallMediaReading: Equatable {
    /// Remote video streams, ordered by statistics id so a stream keeps its
    /// number from one interval to the next.
    var inbound: [(id: String, counters: InboundVideoCounters)] = []
    /// Every layer the camera sender reports, ordered by rid.
    var outbound: [OutboundVideoLayer] = []

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

    /// Reduce a report to a reading. `lightTrackIds` holds the ids of the
    /// remote tracks that are light companions; a video receiver whose
    /// `trackIdentifier` is among them is `light`, any other is `main`.
    static func read(
        _ entries: [CallMediaStatEntry], lightTrackIds: Set<String>
    ) -> CallMediaReading {
        var reading = CallMediaReading()
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
                    layer = lightTrackIds.contains(trackId) ? .light : .main
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
                reading.outbound.append(OutboundVideoLayer(
                    rid: string("rid"),
                    scalabilityMode: string("scalabilityMode"),
                    qualityLimitationReason: string("qualityLimitationReason"),
                    frameWidth: (entry.values["frameWidth"] as? NSNumber)?.int64Value,
                    frameHeight: (entry.values["frameHeight"] as? NSNumber)?.int64Value,
                    framesPerSecond: (entry.values["framesPerSecond"] as? NSNumber)?.doubleValue))
            default:
                continue
            }
        }
        reading.inbound.sort { $0.id < $1.id }
        reading.outbound.sort { ($0.rid ?? "") < ($1.rid ?? "") }
        return reading
    }

    /// One line per remote video stream: how it changed since `before` (from
    /// zero when `before` is `nil`, when the stream is new, or when its
    /// counters went backwards). Each line ends with `layer=`: `main`,
    /// `light`, or `-` when unknown.
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

    /// The camera sender, one line for each layer it sends. A simulcast
    /// layer's line ends with its `rid`; a single encoding's line has none.
    static func describeOutbound(_ label: String, _ layers: [OutboundVideoLayer]) -> [String] {
        layers.map { layer in
            let size: String
            if let w = layer.frameWidth, let h = layer.frameHeight {
                size = "\(w)x\(h)"
            } else {
                size = "-"
            }
            let fps = layer.framesPerSecond.map { String(format: "%.0f", $0) } ?? "-"
            return "\(label) send video mode=\(layer.scalabilityMode ?? "-") size=\(size)"
                + " fps=\(fps) limited_by=\(layer.qualityLimitationReason ?? "-")"
                + (layer.rid.map { " rid=\($0)" } ?? "")
        }
    }

    /// What one remote peer's tile showed, in one line: how often it switched
    /// to the light track, how long the light track was the shown one, and
    /// the longest and the total displayed stall. Whole milliseconds.
    /// `peerId` is the SFU's per-call peer id, the one the stream ids carry.
    static func describeShown(
        _ label: String, peerId: String, _ shown: LayerDisplayCounters
    ) -> String {
        "\(label) shown peer=\(peerId) light_switches=\(shown.lightSwitches)"
            + " light_ms=\(shown.lightMs) stall_max_ms=\(shown.stallMaxMs)"
            + " stall_total_ms=\(shown.stallTotalMs)"
    }

    /// ``describeShown(_:peerId:_:)`` for every peer, in the order given.
    static func describeShownPeers(
        _ label: String, _ byPeer: [(peerId: String, counters: LayerDisplayCounters)]
    ) -> [String] {
        byPeer.map { describeShown(label, peerId: $0.peerId, $0.counters) }
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
