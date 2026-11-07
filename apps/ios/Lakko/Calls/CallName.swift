// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The name a participant gives themself for one call
/// (`docs/ux/calls.md` section 4.1, 2026-11-07; `lakko_calls::call_name`).
/// iOS mirror of the web `calls/call-name.ts` and the Android `CallName.kt`.
///
/// Any participant of a group or link call may change the name the others see
/// on their tile, for this call only. The profile name never changes. The name
/// rides the sealed SFU relay, signed with the sender's MLS leaf key, and a
/// receiver takes it only from the peer whose verified announcement named that
/// leaf key. A one-to-one call has no relay and carries no call name.
///
/// ``CallNameChannel`` is the per-call state: this device's own name and its
/// sequence number (re-sent after every announcement, so a late joiner learns
/// it), and the newest name received per peer. Pure: the signing, the
/// verifying and the sending come in through ``CallNameChannel/Deps``.
@MainActor
final class CallNameChannel {

    /// A verified call name: its sequence number and the name (`nil` = the
    /// peer went back to its default naming).
    struct Verified: Equatable {
        let seq: UInt64
        let name: String?
    }

    /// What the channel is plugged into.
    struct Deps {
        /// This device's signed call name as `selfPeerId`, numbered `seq`, or
        /// `nil` when it cannot make one yet (no group).
        var compose: @MainActor (_ selfPeerId: String, _ seq: UInt64, _ name: String) -> Data?
        /// A call name from `from`, verified against the leaf key its
        /// announcement verified with, or `nil` on any refusal.
        var verify: @MainActor (_ from: String, _ signerKey: Data, _ bytes: Data) -> Verified?
        /// Seal and send a relay plaintext to the call; `false` when it could
        /// not (no relay key, no socket).
        var sendSealed: @MainActor (_ plaintext: Data) -> Bool
        /// This device's SFU peer id, once the SFU's Welcome named it.
        var selfPeerId: @MainActor () -> String?
        /// A peer's call name changed: the name, or `nil` when it went back
        /// to its default naming.
        var onName: @MainActor (_ peerId: String, _ name: String?) -> Void
    }

    private let deps: Deps
    private var ownName = ""
    private var ownSeq: UInt64 = 0
    /// SFU peer id -> the leaf signature key its verified announcement named.
    private var peerKeys: [String: Data] = [:]
    /// SFU peer id -> its latest call name received before its announcement
    /// verified (at most one per peer).
    private var pending: [String: Data] = [:]
    /// SFU peer id -> the newest accepted sequence number.
    private var seqs: [String: UInt64] = [:]

    init(deps: Deps) {
        self.deps = deps
    }

    /// The name this device set for the call (`""` = none).
    var ownCallName: String { ownName }

    /// Set this device's name for the call. `name` is already normalised (the
    /// display-name rule, trimmed); `""` goes back to the default. Returns
    /// whether it changed. Sent at once when possible, and again after every
    /// announcement.
    @discardableResult
    func setOwnName(_ name: String) -> Bool {
        // The same name again, or "" before any name was set: nothing to say.
        if name == ownName { return false }
        ownName = name
        ownSeq += 1
        resend()
        return true
    }

    /// Send the current own name again (after the Welcome, a rekey, a join,
    /// the periodic announcement). Nothing before a name was ever set, and
    /// nothing before the SFU named this device.
    func resend() {
        guard ownSeq > 0, let selfPeerId = deps.selfPeerId(),
              let wire = deps.compose(selfPeerId, ownSeq, ownName)
        else { return }
        _ = deps.sendSealed(wire)
    }

    /// `peerId`'s announcement verified with `signerKey`: a name it sent
    /// before is read now.
    func peerIdentified(_ peerId: String, signerKey: Data) {
        guard !signerKey.isEmpty else { return }
        peerKeys[peerId] = signerKey
        guard let held = pending.removeValue(forKey: peerId) else { return }
        received(from: peerId, bytes: held)
    }

    /// A sealed call name from `peerId` opened.
    func received(from peerId: String, bytes: Data) {
        guard let key = peerKeys[peerId] else {
            // Before its announcement verified: keep the latest one only.
            pending[peerId] = bytes
            return
        }
        guard let verified = deps.verify(peerId, key, bytes) else { return }
        // `call_name_is_newer`: strictly greater; a re-send is not an update.
        if let stored = seqs[peerId], verified.seq <= stored { return }
        seqs[peerId] = verified.seq
        deps.onName(peerId, verified.name)
    }

    /// `peerId` left: what it said goes with it, so a rejoin under a new id
    /// starts fresh.
    func peerLeft(_ peerId: String) {
        peerKeys.removeValue(forKey: peerId)
        pending.removeValue(forKey: peerId)
        seqs.removeValue(forKey: peerId)
    }
}

/// Names this device set, per call id (lower-case hex), for a rejoin of the
/// same call within this app run (`docs/ux/calls.md` section 4.1). In memory
/// only; never stored.
@MainActor
enum CallNameMemory {
    private static var names: [String: String] = [:]

    /// The call name this device set for `callIdHex` earlier in this app run,
    /// `""` when none.
    static func remembered(callIdHex: String) -> String {
        names[callIdHex] ?? ""
    }

    /// Remember (or forget, for `""`) this device's name for `callIdHex`.
    static func remember(callIdHex: String, name: String) {
        if name.isEmpty {
            names.removeValue(forKey: callIdHex)
        } else {
            names[callIdHex] = name
        }
    }

    /// Forget everything (tests).
    static func clearAll() {
        names = [:]
    }
}

/// The rule of the call-name field in the call's Settings sheet: what Save
/// would store, or why it cannot.
enum CallNameField {
    /// What the field's text means.
    enum Verdict: Equatable {
        /// Save stores `name` (`""` = back to the default).
        case save(String)
        /// The text is the name already set: Save stays disabled.
        case unchanged
        /// The text fails the display-name rule; `reason` is the copy.
        case invalid(reason: String)
    }

    /// The verdict for `text` when the own call name is `current`.
    /// `check` is the display-name rule (``checkDisplayName(_:normalize:)``):
    /// trimmed, blank -> `nil`, or a throw with the reason.
    static func verdict(
        text: String,
        current: String,
        check: (String) throws -> String? = { try checkDisplayName($0) }
    ) -> Verdict {
        let normalised: String
        do {
            normalised = try check(text) ?? ""
        } catch {
            return .invalid(
                reason: (error as? LocalizedError)?.errorDescription
                    ?? Localization.text("display_name.error.unknown"))
        }
        return normalised == current ? .unchanged : .save(normalised)
    }
}
