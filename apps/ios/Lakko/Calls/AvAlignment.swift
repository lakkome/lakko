// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Keeping a remote person's sound and picture together in a group call: the
/// iOS side of the shared rule (ADR-0070, note of 2026-10-12); the twin of
/// `apps/web/src/calls/av-align.ts`.
///
/// A subscriber gets, per remote person, one audio track and up to three
/// video tracks: main (stream id `<peer>`), middle (`<peer>~mid`) and light
/// (`<peer>~light`). libwebrtc's own lip sync covers the main track only (it
/// shares the audio's stream id). The rule that ties the others to the sound
/// is written once, in Rust (`crates/lakko-ui-rules`, `av_align`), and
/// reached here over UniFFI as `AvAlign`. Nothing of the rule is restated in
/// this file.
///
/// What is here is what the rule cannot know:
///
///   * which receiver is which (person, audio | main | middle | light);
///   * the receivers' statistics as the rule's inputs
///     (``AvAlignStats/trackInput(prev:cur:)``);
///   * which track a tile draws (``AvAlignStats/shownSlot(_:)``);
///   * what this libwebrtc binding lets Lakko set (``AvReceiverDelay``);
///   * the log lines.
///
/// ``CallAvAlign`` holds one rule per remote person for one call. It has no
/// peer connection, no timer and no statistics read of its own: the session
/// reads the report, owns the clock and says what is shown.

// MARK: - What this binding can set

/// What the libwebrtc binding of this app (stasel/WebRTC 125.0.0) lets Lakko
/// set on a receiver's playout delay.
///
/// Nothing. `RTCRtpReceiver.h` of that framework declares `receiverId`,
/// `parameters`, `track` and `delegate` and no delay of any kind; the binary
/// carries no Objective-C selector for one either (the engine's C++
/// `RtpReceiverInterface::SetJitterBufferMinimumDelay` is not wrapped). So
/// the rule runs here in its observe-only capability: it is fed, the skew is
/// measured and logged, and nothing is written. A picture on the middle or
/// the light track can therefore run ahead of the sound on iOS exactly as it
/// did before; this file does not change that, it measures it.
///
/// Decided here, in Lakko's code, and never assumed from the engine: when a
/// binding that has a setter is adopted, these two are what changes, and
/// ``CallAvAlign`` already carries the answers out as ``CallAvAlign/Write``.
enum AvReceiverDelay {
    /// An audio receiver's playout delay can be set.
    static let audioSettable = false
    /// A video receiver's playout delay can be set.
    static let videoSettable = false
}

// MARK: - Receivers

/// One of a remote person's receivers.
enum AvSlot: String, Equatable, Hashable, CaseIterable {
    case audio
    case main
    case middle
    case light

    /// The video slots in the order the log line names them.
    static let video: [AvSlot] = [.main, .middle, .light]

    var isAudio: Bool { self == .audio }

    /// The slot of a receiver of `kind` (`audio` / `video`) that arrived
    /// under a stream of `role`. A person's audio travels under their main
    /// stream only; audio under a companion's stream id names no slot.
    static func of(kind: String, role: LightStream.Role) -> AvSlot? {
        switch kind {
        case "audio":
            return role == .main ? .audio : nil
        case "video":
            switch role {
            case .main: return .main
            case .middle: return .middle
            case .light: return .light
            }
        default:
            return nil
        }
    }
}

// MARK: - Statistics as the rule's inputs

/// The cumulative counters of one `inbound-rtp` entry the rule's inputs are
/// made of.
struct AvInboundSample: Equatable {
    var packetsReceived: Int64 = 0
    /// Seconds, summed over everything that left the jitter buffer.
    var jitterBufferDelay: Double = 0
    var jitterBufferEmittedCount: Int64 = 0
    /// Seconds, the same sum for the delay the network alone would need;
    /// zero where the engine does not report it.
    var jitterBufferMinimumDelay: Double = 0
}

/// What one receiver reports for one interval, as the rule takes it.
struct AvTrackInput: Equatable {
    /// Packets arrived in the interval.
    var flowing: Bool
    /// Average time in the jitter buffer over the interval, whole
    /// milliseconds; `nil` when nothing left the buffer.
    var delayMs: UInt32?
    /// The delay the network alone would need, whole milliseconds; `nil`
    /// when not measured.
    var networkMs: UInt32?

    /// A receiver with no reading: not flowing, nothing measured.
    static let none = AvTrackInput(flowing: false, delayMs: nil, networkMs: nil)
}

/// What is known of how a person is drawn now.
struct AvShownFacts: Equatable {
    /// A tile draws a picture of the person at all.
    var hasTile: Bool
    /// The picture this device asks the SFU for of them (``VideoWants``).
    var wanted: TileVideoQuality
    /// What the SFU says it gives (``VideoGiven``); `nil` when it has not
    /// said.
    var given: TileVideoQuality?
    /// The person has a middle track / a light companion.
    var hasMiddle: Bool
    var hasLight: Bool
    /// A tile of theirs shows its light picture now (``RemoteLayerController``).
    var lightOnTop: Bool
}

enum AvAlignStats {

    /// Reduce a connection's statistics report to the samples the rule
    /// needs, by the id of the track each `inbound-rtp` entry belongs to.
    /// An entry that does not say which track it belongs to is left out.
    /// Nothing else of the report is read.
    static func inboundSamples(_ entries: [CallStatEntry]) -> [String: AvInboundSample] {
        var byTrack: [String: AvInboundSample] = [:]
        for entry in entries where entry.type == "inbound-rtp" {
            guard let trackId = entry.values["trackIdentifier"] as? String, !trackId.isEmpty
            else { continue }
            let int: (String) -> Int64 = {
                max(0, (entry.values[$0] as? NSNumber)?.int64Value ?? 0)
            }
            let seconds: (String) -> Double = {
                let v = (entry.values[$0] as? NSNumber)?.doubleValue ?? 0
                return v.isFinite && v > 0 ? v : 0
            }
            byTrack[trackId] = AvInboundSample(
                packetsReceived: int("packetsReceived"),
                jitterBufferDelay: seconds("jitterBufferDelay"),
                jitterBufferEmittedCount: int("jitterBufferEmittedCount"),
                jitterBufferMinimumDelay: seconds("jitterBufferMinimumDelay"))
        }
        return byTrack
    }

    /// One interval of one receiver: `cur` against `prev`. With no `prev`,
    /// or with counters that went backwards (a stream replaced under the
    /// same track), the interval is measured from zero. With no `cur` the
    /// receiver reported nothing.
    static func trackInput(prev: AvInboundSample?, cur: AvInboundSample?) -> AvTrackInput {
        guard let cur else { return .none }
        var base = AvInboundSample()
        if let prev, prev.packetsReceived <= cur.packetsReceived,
            prev.jitterBufferEmittedCount <= cur.jitterBufferEmittedCount {
            base = prev
        }
        let emitted = max(0, cur.jitterBufferEmittedCount - base.jitterBufferEmittedCount)
        let perEmittedMs: (Double) -> UInt32 = { seconds in
            let ms = (max(0, seconds) / Double(emitted) * 1000).rounded()
            // Far above anything a receiver reports; keeps the conversion
            // total whatever the report holds.
            return ms.isFinite ? UInt32(min(ms, 3_600_000)) : 0
        }
        let network = max(0, cur.jitterBufferMinimumDelay - base.jitterBufferMinimumDelay)
        return AvTrackInput(
            flowing: cur.packetsReceived - base.packetsReceived > 0,
            delayMs: emitted > 0
                ? perEmittedMs(cur.jitterBufferDelay - base.jitterBufferDelay) : nil,
            networkMs: emitted > 0 && network > 0 ? perEmittedMs(network) : nil)
    }

    /// The one track a tile draws of a person now, or `nil` when none does.
    /// The tile's own arithmetic, asked the way the tile asks it: the heavy
    /// picture is the lower of what is wanted and what is given
    /// (``VideoGiven/shownQuality(wanted:given:hasLight:)``), the middle
    /// picture is the main track while the person has no middle one, and the
    /// light copy is the one drawn while the tile shows it.
    static func shownSlot(_ facts: AvShownFacts) -> AvSlot? {
        guard facts.hasTile else { return nil }
        let quality = VideoGiven.shownQuality(
            wanted: facts.wanted, given: facts.given, hasLight: facts.hasLight)
        if quality == .light { return facts.hasLight ? .light : nil }
        if facts.lightOnTop && facts.hasLight { return .light }
        return quality == .middle && facts.hasMiddle ? .middle : .main
    }

    /// The rule's reading interval as a timer period, milliseconds: what the
    /// rule says, or one second when it says something a timer cannot run at
    /// (outside a quarter of a second to two seconds, the longest interval
    /// the rule counts in full).
    static func readingIntervalMs(_ ruleMs: UInt64 = avAlignReadingMs()) -> UInt64 {
        (250...2000).contains(ruleMs) ? ruleMs : 1000
    }
}

// MARK: - The log lines

/// What the last reading said of one of a person's video tracks.
struct AvVideoSync: Equatable {
    let slot: AvSlot
    let delayMs: UInt32?
    let askedMs: UInt32
    /// The track's delay minus the audio's; `nil` when either was not
    /// measured.
    let skewMs: Int32?
    let reason: String
}

/// What the last reading said of one person.
struct AvPeerSync: Equatable {
    let audioDelayMs: UInt32?
    let audioAskedMs: UInt32
    let audioReason: String
    /// The video tracks the person has, main first.
    let video: [AvVideoSync]
}

enum AvAlignLog {

    private static func ms<T: BinaryInteger>(_ value: T?) -> String {
        value.map { String($0) } ?? "-"
    }

    /// One person in one line (plain text, no names; `peerId` is the SFU's
    /// per-call peer id). The audio as `delay/ask=asked/reason`, each video
    /// track the person has as `delay/ask=asked/skew=skew/reason`; a figure
    /// that was not measured is `-`, `ask=0` is nothing asked. The reasons
    /// are the shared rule's own words. The same line the web client writes:
    ///
    /// `call media interval sync peer=p7 audio=420/ask=0/no-setter main=431/ask=0/skew=11/engine-synced light=290/ask=0/skew=-130/no-setter`
    static func describeSync(_ label: String, peerId: String, _ state: AvPeerSync) -> String {
        var parts = ["\(label) sync peer=\(peerId)"]
        parts.append(
            "audio=\(ms(state.audioDelayMs))/ask=\(state.audioAskedMs)/\(state.audioReason)")
        for v in state.video {
            parts.append(
                "\(v.slot.rawValue)=\(ms(v.delayMs))/ask=\(v.askedMs)"
                    + "/skew=\(ms(v.skewMs))/\(v.reason)")
        }
        return parts.joined(separator: " ")
    }

    /// What the rule can achieve on this device, in one line. `capability`
    /// is the shared rule's word for the two setters.
    ///
    /// `call media sync capability=observe-only audio_setter=no video_setter=no interval_ms=1000`
    static func describeCapability(
        _ label: String, audioSettable: Bool, videoSettable: Bool, intervalMs: UInt64
    ) -> String {
        let capability = avCapabilityName(
            capability: avAlignCapability(
                audioSettable: audioSettable, videoSettable: videoSettable))
        return "\(label) sync capability=\(capability)"
            + " audio_setter=\(audioSettable ? "yes" : "no")"
            + " video_setter=\(videoSettable ? "yes" : "no")"
            + " interval_ms=\(intervalMs)"
    }

    /// One line for a write that did not take on a person's receiver, with
    /// the capability their rule goes on in.
    static func describeWriteFailed(
        _ label: String, peerId: String, slot: AvSlot, capability: String
    ) -> String {
        "\(label) sync peer=\(peerId) \(slot.rawValue) target not taken;"
            + " continuing capability=\(capability)"
    }
}

// MARK: - One call

/// The alignment of every remote person of one call.
///
/// The session registers each receiver as its track arrives
/// (``setReceiver(peerId:slot:trackId:)``) and takes a person away when they
/// leave; once per reading interval it hands over the receivers' samples and
/// says what is shown (``read(nowMs:samples:shownOf:)``) and gets back what
/// to write to which receiver. A person's rule is built with their first
/// receiver and dropped when they leave or the call ends (``clear()``).
///
/// A write that does not take is told back (``writeFailed(peerId:slot:)``):
/// that person's rule is built again as one that cannot set that kind, so it
/// goes on measuring and stops asking.
///
/// Not thread-safe: the session uses it on the main actor.
final class CallAvAlign {

    /// One answer of the rule to carry out: set the playout delay of the
    /// receiver of `trackId` to `setMs` (`0` gives the target back).
    struct Write: Equatable {
        let peerId: String
        let slot: AvSlot
        let trackId: String
        let setMs: UInt32
    }

    private struct Receiver {
        var trackId: String
        var prev: AvInboundSample?
    }

    private final class Person {
        var rule: AvAlignProtocol
        var audioFailed = false
        var videoFailed = false
        var receivers: [AvSlot: Receiver] = [:]
        var last: AvPeerSync?
        init(rule: AvAlignProtocol) { self.rule = rule }
    }

    private let audioSettable: Bool
    private let videoSettable: Bool
    private let makeRule: (_ audioSettable: Bool, _ videoSettable: Bool) -> AvAlignProtocol
    private var people: [String: Person] = [:]
    /// The people in the order their first receiver arrived.
    private var order: [String] = []

    init(
        audioSettable: Bool = AvReceiverDelay.audioSettable,
        videoSettable: Bool = AvReceiverDelay.videoSettable,
        makeRule: @escaping (Bool, Bool) -> AvAlignProtocol = {
            AvAlign(audioSettable: $0, videoSettable: $1)
        }
    ) {
        self.audioSettable = audioSettable
        self.videoSettable = videoSettable
        self.makeRule = makeRule
    }

    /// The people who have a rule now, in the order they got it.
    var peerIds: [String] { order }

    /// `peerId` has a receiver for `slot`.
    func has(_ peerId: String, _ slot: AvSlot) -> Bool {
        people[peerId]?.receivers[slot] != nil
    }

    /// `peerId`'s `slot` track arrived as `trackId`. A different track under
    /// a slot the person already had means that receiver was replaced: the
    /// rule is told, and tells every receiver its target again.
    func setReceiver(peerId: String, slot: AvSlot, trackId: String) {
        guard !peerId.isEmpty, !trackId.isEmpty else { return }
        let person = self.person(peerId)
        if let before = person.receivers[slot] {
            guard before.trackId != trackId else { return }
            person.rule.receiversReplaced()
        }
        person.receivers[slot] = Receiver(trackId: trackId, prev: nil)
    }

    /// `peerId` left the call: their rule is dropped.
    func removePeer(_ peerId: String) {
        people.removeValue(forKey: peerId)
        order.removeAll { $0 == peerId }
    }

    /// Every receiver of the call was recreated: nothing has been told to
    /// them, and their counters start again.
    func receiversReplaced() {
        for person in people.values {
            person.rule.receiversReplaced()
            for slot in Array(person.receivers.keys) { person.receivers[slot]?.prev = nil }
        }
    }

    /// One reading interval, taken at `nowMs` (one clock, whole
    /// milliseconds). `samples` holds the sample of every receiver that
    /// reported, by track id; `shownOf` says which one track a tile draws of
    /// a person now. Returns the rule's answers that are to be written.
    func read(
        nowMs: UInt64, samples: [String: AvInboundSample], shownOf: (String) -> AvSlot?
    ) -> [Write] {
        var writes: [Write] = []
        for peerId in order {
            guard let person = people[peerId] else { continue }
            if person.receivers.isEmpty {
                person.last = nil
                continue
            }
            var inputs: [AvSlot: AvTrackInput] = [:]
            for (slot, receiver) in person.receivers {
                let cur = samples[receiver.trackId]
                inputs[slot] = AvAlignStats.trackInput(prev: receiver.prev, cur: cur)
                if let cur { person.receivers[slot]?.prev = cur }
            }
            let shown = shownOf(peerId)
            let video: (AvSlot) -> AvVideoReading = { slot in
                let input = inputs[slot] ?? .none
                return AvVideoReading(
                    flowing: input.flowing,
                    shown: inputs[slot] != nil && shown == slot,
                    // libwebrtc synchronises the main track itself: it
                    // shares the audio's stream id.
                    engineSynced: slot == .main,
                    delayMs: input.delayMs, networkMs: input.networkMs)
            }
            let audio = inputs[.audio] ?? .none
            let plan = person.rule.read(
                nowMs: nowMs,
                readings: AvReadings(
                    audio: AvAudioReading(
                        flowing: audio.flowing, delayMs: audio.delayMs,
                        networkMs: audio.networkMs),
                    main: video(.main), middle: video(.middle), light: video(.light)))
            let planOf: (AvSlot) -> AvVideoPlan = { slot in
                switch slot {
                case .middle: return plan.middle
                case .light: return plan.light
                default: return plan.main
                }
            }
            person.last = AvPeerSync(
                audioDelayMs: audio.delayMs,
                audioAskedMs: plan.audio.askedMs,
                audioReason: avAudioReasonName(reason: plan.audio.reason),
                video: AvSlot.video.filter { person.receivers[$0] != nil }.map { slot in
                    let p = planOf(slot)
                    return AvVideoSync(
                        slot: slot, delayMs: inputs[slot]?.delayMs, askedMs: p.askedMs,
                        skewMs: p.skewMs, reason: avVideoReasonName(reason: p.reason))
                })
            if let set = plan.audio.setMs, let receiver = person.receivers[.audio] {
                writes.append(
                    Write(peerId: peerId, slot: .audio, trackId: receiver.trackId, setMs: set))
            }
            for slot in AvSlot.video {
                if let set = planOf(slot).setMs, let receiver = person.receivers[slot] {
                    writes.append(
                        Write(peerId: peerId, slot: slot, trackId: receiver.trackId, setMs: set))
                }
            }
        }
        return writes
    }

    /// A write to `peerId`'s `slot` receiver failed or did not read back.
    /// Their rule is built again as one that cannot set that kind and starts
    /// as for a person who has just joined. Returns the capability it goes
    /// on in, the rule's own word, the first time this happens for that kind
    /// of that person; `nil` when it was already known (nothing to report
    /// twice) or the person is gone.
    func writeFailed(peerId: String, slot: AvSlot) -> String? {
        guard let person = people[peerId] else { return nil }
        if slot.isAudio {
            guard !person.audioFailed else { return nil }
            person.audioFailed = true
        } else {
            guard !person.videoFailed else { return nil }
            person.videoFailed = true
        }
        let flags = self.flags(person)
        person.rule = makeRule(flags.audio, flags.video)
        person.last = nil
        for key in Array(person.receivers.keys) { person.receivers[key]?.prev = nil }
        return avCapabilityName(
            capability: avAlignCapability(
                audioSettable: flags.audio, videoSettable: flags.video))
    }

    /// One line per person the last reading covered, in the order they
    /// joined.
    func lines(_ label: String) -> [String] {
        order.compactMap { peerId in
            people[peerId]?.last.map { AvAlignLog.describeSync(label, peerId: peerId, $0) }
        }
    }

    /// The call is over: every rule is dropped.
    func clear() {
        people.removeAll()
        order.removeAll()
    }

    private func flags(_ person: Person) -> (audio: Bool, video: Bool) {
        (audioSettable && !person.audioFailed, videoSettable && !person.videoFailed)
    }

    private func person(_ peerId: String) -> Person {
        if let person = people[peerId] { return person }
        let person = Person(rule: makeRule(audioSettable, videoSettable))
        people[peerId] = person
        order.append(peerId)
        return person
    }
}
