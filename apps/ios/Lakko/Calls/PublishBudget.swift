// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The SFU's last `publish-budget` frame, kilobits a second: how much this
/// device's main and middle camera sender may send (ADR-0068, amendment of
/// 2026-10-11). The whole state each time; the last frame wins.
struct PublishBudget: Equatable {
    let mainKbps: UInt32
    let middleKbps: UInt32
}

/// How much this device's camera senders may send, as the SFU says; the twin
/// of `apps/web/src/calls/publish-budget.ts`.
///
/// The SFU estimates this device's uplink and tells it, over Lakko's own
/// signaling, the ceiling for its main and its middle camera sender. The same
/// estimate goes out as REMB, but a sender's own bitrate controller decides
/// what it makes of a REMB; a ceiling on the sender's encoding is kept in
/// every engine. So the ceiling is what is applied: `maxBitrateBps` of the
/// sender's one encoding, and nothing else.
///
/// The server decides, the client executes. There is no allocation rule here:
/// how the estimate is shared between the senders, the floors and the maxima
/// are the SFU's (`services/sfu/src/publish_budget.rs`), so the three clients
/// cannot drift. This holds
///
///   * the reading of the frame's two figures (``read(mainKbps:middleKbps:)``);
///   * the one mapping (``senderCeiling(_:_:)``): the main sender's ceiling
///     is `main_kbps`, the middle sender's is `middle_kbps`, in bits a
///     second; the light sender is never touched (it keeps the ceiling it was
///     created with); and before the first frame nothing is written, so each
///     sender keeps what it was created with (the main one no ceiling, the
///     middle one ``CameraSender/middleMaxBitrateBps``);
///   * the two diagnostics lines.
///
/// A ceiling is applied to a sender whether or not it is switched on
/// (``SenderActivityRule``): the SFU gives a figure for a sender nobody
/// watches too, and it is what that sender starts under.
///
/// Audio is never limited here. Pure.
enum PublishBudgetRule {

    /// The highest ceiling the frame may carry, kbit/s
    /// (`lakko_sfu_proto::MAX_PUBLISH_BUDGET_KBPS`). Checked by the shared
    /// parser; repeated here only to refuse a frame that says nonsense.
    static let maxKbps: UInt32 = 20_000

    /// The one warning a malformed `publish-budget` frame gets.
    static let malformedWarning = "sfu signaling: ignored a malformed publish-budget frame"

    /// The senders the frame names, in the order the diagnostics name them.
    static let budgetedKinds: [CameraSenderKind] = [.main, .middle]

    /// The frame's two ceilings as a budget, or `nil` when either is zero or
    /// above the protocol's bound.
    static func read(mainKbps: UInt32, middleKbps: UInt32) -> PublishBudget? {
        let valid: (UInt32) -> Bool = { $0 >= 1 && $0 <= maxKbps }
        guard valid(mainKbps), valid(middleKbps) else { return nil }
        return PublishBudget(mainKbps: mainKbps, middleKbps: middleKbps)
    }

    /// Whether a text frame the shared parser refused says it is a
    /// `publish-budget` one. Only decides whether the refusal gets the
    /// frame's own warning; nothing is read from such a frame.
    static func claimsPublishBudget(_ raw: String) -> Bool {
        guard let data = raw.data(using: .utf8),
              let parsed = try? JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed]),
              let object = parsed as? [String: Any]
        else { return false }
        return (object["kind"] as? String) == "publish-budget"
    }

    /// The ceiling to set on `kind`'s encoding, bits a second, or `nil` when
    /// nothing is to be written: the light sender always, and every sender
    /// before the first frame (it keeps what it was created with).
    static func senderCeiling(_ kind: CameraSenderKind, _ budget: PublishBudget?) -> Int? {
        guard let budget else { return nil }
        switch kind {
        case .main: return Int(budget.mainKbps) * 1000
        case .middle: return Int(budget.middleKbps) * 1000
        case .light: return nil
        }
    }

    /// The one line for a `publish-budget` frame that changed the budget:
    /// `call publish budget main=.. middle=..`, kbit/s. `nil` when the frame
    /// repeats the last one.
    static func describeChange(before: PublishBudget?, _ budget: PublishBudget) -> String? {
        if before == budget { return nil }
        return "call publish budget main=\(budget.mainKbps) middle=\(budget.middleKbps)"
    }

    /// ` ceiling=N`, the tail a `send video` line gains: the bitrate ceiling
    /// the sender's encoding holds now, in kbit/s (rounded down), or `-` when
    /// it holds none or the sender could not be read.
    static func describeSenderCeiling(_ maxBitrateBps: Int?) -> String {
        " ceiling=\(maxBitrateBps.map { String($0 / 1000) } ?? "-")"
    }
}

/// What one application of a ceiling did.
enum SenderCeilingOutcome: Equatable {
    /// The encoding holds the ceiling (already, or after the set).
    case applied
    /// The sender has no encoding to set the ceiling on.
    case noEncoding
    /// The binding did not take it: read back, the encoding holds something
    /// else.
    case refused
}

/// One read-modify-set of an encoding's bitrate ceiling, over plain values so
/// it runs in a test. `current` is the first encoding's `maxBitrateBps` as
/// the sender holds it (`nil`: none); `hasEncoding` says whether there is a
/// first encoding at all.
enum SenderCeilingWrite {

    /// What to do before any set.
    enum Step: Equatable {
        /// Nothing to write: answer with this outcome.
        case done(SenderCeilingOutcome)
        /// Set the first encoding's ceiling to this many bits a second and
        /// leave the rest of the encoding as it is.
        case set(Int)
    }

    static func step(hasEncoding: Bool, current: Int?, wanted: Int) -> Step {
        guard hasEncoding else { return .done(.noEncoding) }
        if current == wanted { return .done(.applied) }
        return .set(wanted)
    }

    /// The outcome of a set, from what the encoding holds when read back.
    /// The binding's setter reports nothing, so this is the only answer.
    static func outcome(readBack: Int?, wanted: Int) -> SenderCeilingOutcome {
        readBack == wanted ? .applied : .refused
    }
}

/// The part of a sender ``SenderCeiling`` uses: one read-modify-set of its
/// first encoding's `maxBitrateBps`. An `RTCRtpSender` in production
/// (``RtcCeilingSender``); a plain object in a test.
protocol CeilingSender: AnyObject {
    func applyEncodingCeiling(_ maxBitrateBps: Int) async -> SenderCeilingOutcome
}

/// Keeps one sender's first encoding under the ceiling last asked; the twin
/// of the web `SenderCeiling`.
///
///   * **Serialised.** Each step is one read-modify-set, and the next step
///     starts only when it has settled. In production the read-modify-set of
///     every writer of a sender's parameters is one block on one serial queue
///     (``SenderParameterWrites``), so none overlaps another and none puts
///     back what another just changed.
///   * **The last word wins.** A step applies what is wanted when it starts.
///   * **Re-applied.** Every step reads the encoding as the binding holds it
///     now, so asking again (every media-log tick does) puts back a ceiling a
///     renegotiation or a new sender (``bind(_:)``) lost.
///   * **Refused.** A write that did not take is asked again at every later
///     ``want(_:)``, and said once per sender.
///   * **Before the first word** (`want(nil)`), nothing is written.
@MainActor
final class SenderCeiling {

    private let name: String
    private let warn: (String) -> Void
    private var sender: CeilingSender?
    private var wanted: Int?
    private var scheduled = false
    private var chain: Task<Void, Never>?
    private var warned = false

    /// - Parameter name: names the sender in the one warning: `main` or
    ///   `middle`.
    init(name: String, warn: @escaping (String) -> Void = { NSLog("%@", $0) }) {
        self.name = name
        self.warn = warn
    }

    /// The sender this keeps. A different one (first publish, mid-call
    /// camera-on, a copy added again) may say its warning. `nil`: there is
    /// none.
    func bind(_ next: CeilingSender?) {
        if sender === next { return }
        sender = next
        warned = false
    }

    /// Whether ``bind(_:)`` was given a sender.
    var bound: Bool { sender != nil }

    /// What was last asked, bits a second; `nil` before anything was.
    var lastWanted: Int? { wanted }

    /// Ask for the encoding's ceiling, bits a second; `nil`: nothing is asked
    /// (the sender keeps what it holds). Safe to call at any time and as
    /// often as wanted.
    func want(_ maxBitrateBps: Int?) {
        wanted = maxBitrateBps
        guard sender != nil, maxBitrateBps != nil, !scheduled else { return }
        scheduled = true
        let previous = chain
        chain = Task { @MainActor [weak self] in
            await previous?.value
            await self?.step()
        }
    }

    /// Returns when every step asked for so far has settled.
    func settled() async {
        await chain?.value
    }

    /// Forget the sender and what was asked: the call is over.
    func reset() {
        sender = nil
        wanted = nil
        warned = false
    }

    private func step() async {
        scheduled = false
        guard let sender, let maxBitrateBps = wanted else { return }
        let outcome = await sender.applyEncodingCeiling(maxBitrateBps)
        // The sender was replaced while the binding answered: the new one is
        // asked by the caller's next `want`.
        guard self.sender === sender, outcome == .refused else { return }
        if warned { return }
        warned = true
        warn(
            "camera sender ceiling: the \(name) camera sender's bitrate ceiling "
                + "could not be set, asking again")
    }
}
