// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The SFU's last `video-needed` frame: which of this device's camera senders
/// somebody is being sent (ADR-0070, "senders nobody watches"). The whole
/// state each time; the last frame wins. The light sender is always needed
/// and is not named.
struct VideoNeeded: Equatable {
    let main: Bool
    let middle: Bool
}

/// Which of this device's camera senders encode and send now; the twin of
/// `apps/web/src/calls/sender-activity.ts`.
///
/// A sender's encoding is active exactly when the camera is on, the sender
/// exists, the people-count rule allows it (the middle sender only), and the
/// SFU's last word for it is not `false`. No word yet means needed. The light
/// sender ignores the SFU's word.
///
/// Why the encoding's `isActive` flag and neither of the two other ways to
/// silence a sender: a track with `isEnabled = false` is still encoded and
/// sent (black frames), so nothing is saved; and taking the track off the
/// sender changes what the media line negotiates at the next offer. The flag
/// stops the encoder and the packets and leaves the transceiver, the m-line,
/// the SSRC and the SFrame encryptor, key and stream index as they are: no
/// renegotiation.
///
/// Pure.
enum SenderActivityRule {

    /// What the decision is made from.
    struct Input: Equatable {
        /// This device has a camera track (see ``SfuCallSession``: not the
        /// track's `isEnabled`).
        var cameraOn: Bool
        /// The people-count rule (`middle_sender_wanted`) says the middle
        /// sender is sent. Read for the middle sender only.
        var middleWanted: Bool
        /// The SFU's last `video-needed` frame; `nil` before the first.
        var needed: VideoNeeded?
    }

    /// The SFU's last word for one sender: `true` or `false` for the main
    /// and the middle sender once a frame arrived, `nil` before any frame
    /// and always for the light sender, which the frame does not name.
    static func neededWord(_ kind: CameraSenderKind, _ needed: VideoNeeded?) -> Bool? {
        guard let needed else { return nil }
        switch kind {
        case .main: return needed.main
        case .middle: return needed.middle
        case .light: return nil
        }
    }

    /// Whether `kind`'s encoding is active: every condition has to hold.
    static func active(_ kind: CameraSenderKind, exists: Bool, _ input: Input) -> Bool {
        if !input.cameraOn || !exists { return false }
        if kind == .middle, !input.middleWanted { return false }
        return neededWord(kind, input.needed) != false
    }

    /// ``active(_:exists:_:)`` for every sender.
    static func allActive(
        _ input: Input, exists: (CameraSenderKind) -> Bool
    ) -> [CameraSenderKind: Bool] {
        var out: [CameraSenderKind: Bool] = [:]
        for kind in CameraSenderKind.allCases {
            out[kind] = active(kind, exists: exists(kind), input)
        }
        return out
    }

    /// A sender's `needed=` field in the call log: `yes`, `no`, or `-` when
    /// the SFU has said nothing about it (the light sender always).
    static func neededField(_ word: Bool?) -> String {
        guard let word else { return "-" }
        return word ? "yes" : "no"
    }

    /// ` needed=yes|no|- active=yes|no`, the tail a `send video` line gains.
    static func describe(needed: Bool?, active: Bool) -> String {
        " needed=\(neededField(needed)) active=\(active ? "yes" : "no")"
    }

    /// The one line for a `video-needed` frame that changed which sender is
    /// active: `call video needed main=.. middle=..` with what is active now.
    /// `nil` when the frame changed nothing that is active.
    static func describeChange(
        _ needed: VideoNeeded,
        before: [CameraSenderKind: Bool],
        after: [CameraSenderKind: Bool]
    ) -> String? {
        if CameraSenderKind.allCases.allSatisfy({ (before[$0] ?? false) == (after[$0] ?? false) }) {
            return nil
        }
        let yn: (Bool) -> String = { $0 ? "yes" : "no" }
        return "call video needed main=\(yn(needed.main)) middle=\(yn(needed.middle))"
            + " active main=\(yn(after[.main] ?? false)) middle=\(yn(after[.middle] ?? false))"
            + " light=\(yn(after[.light] ?? false))"
    }
}

/// What one application of the flag did.
enum SenderActivityOutcome: Equatable {
    /// The encoding says what was wanted (already, or after the set).
    case applied
    /// The sender has no encoding to set the flag on.
    case noEncoding
    /// The binding did not take it.
    case refused
}

/// The part of a sender ``SenderActivity`` uses: one read-modify-set of its
/// first encoding's `isActive`. An `RTCRtpSender` in production
/// (``RtcActivitySender``); a plain object in a test.
protocol ActivitySender: AnyObject {
    func applyEncodingActive(_ active: Bool) async -> SenderActivityOutcome
}

/// Keeps one sender's first encoding active or not, as last asked; the twin
/// of the web `SenderActivity`.
///
///   * **Serialised.** Each step reads, modifies and sets in one go, and the
///     next step starts only when it has settled.
///   * **The last word wins.** A step applies what is wanted when it runs, so
///     `true, false, true` asked faster than the binding answers ends at
///     `true`, never out of order.
///   * **Re-applied.** Every step reads the encoding as the binding holds it
///     now, so asking again after a renegotiation or a new sender
///     (``bind(_:)``) puts back a flag that was reset.
///   * **When in doubt, active.** A refused switch-off leaves the sender
///     sending and is not asked again until something else is wanted. A
///     refused switch-on is asked again at every later ``want(_:)``, because
///     a sender somebody watches must not stay off. Either is said once per
///     sender.
@MainActor
final class SenderActivity {

    private let name: String
    private let warn: (String) -> Void
    private var sender: ActivitySender?
    private var wanted: Bool?
    private var scheduled = false
    private var chain: Task<Void, Never>?
    /// A switch-off the binding refused on this sender: not asked again
    /// while switch-off stays what is wanted.
    private var offRefused = false
    private var warned = false

    /// - Parameter name: names the sender in the one warning: `main`,
    ///   `middle` or `light`.
    init(name: String, warn: @escaping (String) -> Void = { NSLog("%@", $0) }) {
        self.name = name
        self.warn = warn
    }

    /// The sender this keeps. A different one (first publish, mid-call
    /// camera-on) starts from nothing: no refusal is remembered and its
    /// warning may be said. `nil`: there is none.
    func bind(_ next: ActivitySender?) {
        if sender === next { return }
        sender = next
        offRefused = false
        warned = false
    }

    /// Whether ``bind(_:)`` was given a sender.
    var bound: Bool { sender != nil }

    /// What was last asked; `nil` before anything was.
    var lastWanted: Bool? { wanted }

    /// Ask for the encoding to be active or not. Safe to call at any time
    /// and as often as wanted.
    func want(_ active: Bool) {
        if wanted != active { offRefused = false }
        wanted = active
        guard sender != nil, !scheduled else { return }
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
        offRefused = false
        warned = false
    }

    private func step() async {
        scheduled = false
        guard let sender, let active = wanted else { return }
        if !active, offRefused { return }
        let outcome = await sender.applyEncodingActive(active)
        // The sender was replaced while the binding answered: the new one is
        // asked by the caller's next `want`.
        guard self.sender === sender, outcome == .refused else { return }
        if !active { offRefused = true }
        if warned { return }
        warned = true
        warn(
            "camera sender activity: the \(name) camera sender could not be "
                + (active ? "started, asking again" : "stopped, it goes on sending"))
    }
}
