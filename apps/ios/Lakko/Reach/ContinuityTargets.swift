// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0073 R.4 and ADR-0101 sections 1 to 6 — reaching an account after its
// key reset, the PEER side.
//
// A reset changes every reach key and the mailbox (they derive from the
// account seed), and the new identity knows none of its former contacts, so
// nobody holds a grant for its new mailbox. ADR-0101: the resetting device
// announces a second mailbox, the CONTINUITY mailbox, in a 105-byte binding
// signed by its new identity key, served beside its identity bundle for a
// window; it registers on that mailbox only the continuity reach key
//
//     continuity seed = continuityReachSeed(account id, key generation)
//
// which a peer that ACCEPTED the reset (ADR-0073 R.2) derives too. So the
// peer verifies the binding under the attested new key it pinned, keeps the
// verified mailbox key with its expiry, writes there (signed with the
// continuity reach key) after every ordinary grant it holds, and stops at
// expiry.
//
// iOS mirror of `apps/web/src/reach/continuity.ts`. The rules are pure; the
// store is `UserDefaults`, per signed-in account, PUBLIC values only.

/// A verified continuity mailbox.
public struct ContinuityTarget: Codable, Equatable, Sendable {
    /// Standard base64 of the 32-byte continuity mailbox public key.
    public let mailboxPkB64: String
    /// Unix ms; the target is not used at or after it.
    public let expiresAtMs: Int64

    public init(mailboxPkB64: String, expiresAtMs: Int64) {
        self.mailboxPkB64 = mailboxPkB64
        self.expiresAtMs = expiresAtMs
    }
}

public enum ContinuityTargets {
    /// How long the continuity key stays usable, from the reset's
    /// `signed_at` (decision D2). Equal to the core's `continuityWindowMs()`.
    public static let keyTtlMs: Int64 = 30 * 24 * 60 * 60 * 1000

    /// Whether a PEER would still sign a write with the continuity key.
    public static func continuityKeyUsable(signedAtMs: Int64, nowMs: Int64) -> Bool {
        nowMs < signedAtMs + keyTtlMs
    }

    /// `version(1) ‖ continuity_mailbox_pk(32) ‖ expires_at_ms(8 BE) ‖ sig(64)`.
    public static let bindingLen = 105

    /// The binding's `expires_at_ms`, read off the layout (NOT a
    /// verification), or `nil` for a wrong length.
    public static func bindingExpiryMs(_ binding: Data) -> Int64? {
        guard binding.count == bindingLen else { return nil }
        let bytes = [UInt8](binding)
        var value: UInt64 = 0
        for i in 33..<41 { value = (value << 8) | UInt64(bytes[i]) }
        guard value <= UInt64(Int64.max) else { return nil }
        return Int64(value)
    }

    /// The continuity mailbox a served binding names, verified under the
    /// attested new key at `nowMs`, or `nil` (absent, malformed, signed by
    /// another key, expired, expiry too far). `verify` is the core's
    /// `verifyContinuityBinding`. Never throws.
    public static func peerTarget(
        bindingB64: String?, attestedKey: Data, nowMs: Int64,
        verify: (_ binding: Data, _ identityPk: Data, _ nowMs: UInt64) throws -> Data
    ) -> ContinuityTarget? {
        guard let bindingB64, !bindingB64.isEmpty, let binding = base64Decode(bindingB64),
              let expiry = bindingExpiryMs(binding), expiry > nowMs, nowMs >= 0 else { return nil }
        guard let mailboxPk = try? verify(binding, attestedKey, UInt64(nowMs)), mailboxPk.count == 32 else {
            return nil
        }
        return ContinuityTarget(mailboxPkB64: base64Encode(mailboxPk), expiresAtMs: expiry)
    }

    /// Whether a held target may still be written to at `nowMs`.
    public static func targetLive(_ target: ContinuityTarget?, nowMs: Int64) -> Bool {
        guard let target else { return false }
        return nowMs < target.expiresAtMs
    }

    /// ADR-0101 section 4 — `newIdentityReachable`: true when this device
    /// holds an ordinary way in (a grant or a delegation the write plan
    /// offers), or a verified, unexpired continuity binding AND the accepted
    /// reset's continuity key is still usable (two windows, two clocks).
    public static func newIdentityReachable(
        ordinaryCandidates: Int, continuity: ContinuityTarget?, resetSignedAtMs: Int64?, nowMs: Int64
    ) -> Bool {
        if ordinaryCandidates > 0 { return true }
        guard let resetSignedAtMs else { return false }
        return targetLive(continuity, nowMs: nowMs)
            && continuityKeyUsable(signedAtMs: resetSignedAtMs, nowMs: nowMs)
    }
}

/// The verified continuity mailboxes kept per signed-in account, keyed on
/// the person's NEW account key (standard base64).
public struct ContinuityTargetStore {
    static let prefix = "lakko.continuity-targets.v1."

    private let defaults: UserDefaults
    private let accountIdB64: String

    public init(accountIdB64: String, defaults: UserDefaults = .standard) {
        self.accountIdB64 = accountIdB64
        self.defaults = defaults
    }

    private var key: String { Self.prefix + accountIdB64 }

    private func all() -> [String: ContinuityTarget] {
        guard let data = defaults.data(forKey: key) else { return [:] }
        return (try? JSONDecoder().decode([String: ContinuityTarget].self, from: data)) ?? [:]
    }

    /// Keep (or, with `nil`, forget) the target of `newKeyB64`. Expired
    /// entries are dropped on every write.
    public func remember(newKeyB64: String, target: ContinuityTarget?, nowMs: Int64) {
        var entries = all().filter { $0.value.expiresAtMs > nowMs }
        entries[newKeyB64] = target
        guard let data = try? JSONEncoder().encode(entries),
              let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(data, forKey: key)
    }

    /// The live target held for `newKeyB64`, or `nil`.
    public func target(of newKeyB64: String, nowMs: Int64) -> ContinuityTarget? {
        let held = all()[newKeyB64]
        return ContinuityTargets.targetLive(held, nowMs: nowMs) ? held : nil
    }

    public static func clear(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: prefix + accountIdB64)
    }
}
