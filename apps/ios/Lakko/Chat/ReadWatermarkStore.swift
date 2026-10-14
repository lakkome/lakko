// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0024 — per-account map `groupIdHex -> { senderSignatureKeyB64 -> upTo }`
/// in `UserDefaults`, the ``GroupNameStore`` shape.
///
/// Why durable: a read receipt is a journal row, and the DS poll resumes past
/// rows already applied. A watermark held only by the open chat screen was
/// therefore lost for good whenever the receipt arrived while another chat was
/// open, or before a relaunch — and the author's message stayed at "Sent"
/// although every other member had read it.
///
/// Keyed on the sender's signature key, never a leaf index (C-68): the map is
/// monotone and never reset on a membership change, so a leaf key would hand a
/// departed member's read history to whoever takes the vacated slot.
///
/// Monotone: ``merge(accountIdB64:groupIdHex:sender:upTo:)`` never lowers a
/// stored value (``PresenceOps/foldReadWatermark(_:sender:upTo:)``), so a
/// re-sync replay of an old receipt is idempotent. Bounded naturally — one
/// entry per sender per group — so there is no pruning.
public final class ReadWatermarkStore {

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// The stored watermarks for one group (empty when none).
    public func load(accountIdB64: String, groupIdHex: String) -> PresenceOps.ReadWatermarkMap {
        guard let raw = map(accountIdB64: accountIdB64)[groupIdHex] else { return [:] }
        var out: PresenceOps.ReadWatermarkMap = [:]
        for (sender, value) in raw {
            if let number = value as? NSNumber {
                out[sender] = number.int64Value
            }
        }
        return out
    }

    /// Fold one receipt into the group's stored map, advance-only. Returns the
    /// new map when the stored value advanced, `nil` when it did not (a replay
    /// or a lower value), so the caller publishes only real changes.
    @discardableResult
    public func merge(
        accountIdB64: String,
        groupIdHex: String,
        sender: MemberIdentity,
        upTo: Int64
    ) -> PresenceOps.ReadWatermarkMap? {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return nil }
        defer { gate.leave() }
        let before = load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let after = PresenceOps.foldReadWatermark(before, sender: sender, upTo: upTo)
        guard after != before else { return nil }
        var current = map(accountIdB64: accountIdB64)
        current[groupIdHex] = after.mapValues { NSNumber(value: $0) }
        defaults.set(current, forKey: Self.key(accountIdB64))
        return after
    }

    /// Forget one group's watermarks (the group was deleted locally).
    public func remove(accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var current = map(accountIdB64: accountIdB64)
        current[groupIdHex] = nil
        defaults.set(current, forKey: Self.key(accountIdB64))
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64))
    }

    private func map(accountIdB64: String) -> [String: [String: Any]] {
        defaults.dictionary(forKey: Self.key(accountIdB64)) as? [String: [String: Any]] ?? [:]
    }

    static func key(_ accountIdB64: String) -> String {
        "read_watermarks.\(accountIdB64)"
    }
}
