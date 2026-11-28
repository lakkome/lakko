// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0101 sections 1 to 5 — the RESET side of reaching an account after its
// key reset: what the resetting device keeps so its conversations, groups,
// channels, profile and history come back by themselves.
//
// After the attested publish (``KeyResetPublishRecord``) the reset owes one
// more step, the announcement, kept in its own record (``ResetContinuity``,
// `announced`) so that a failure here never withholds the new words the way
// an owed publish does; it is retried at every session landing:
//
//   1. claim the CONTINUITY mailbox (`MailboxKey.continuityFromAccountIdentity`,
//      claimed with `dsContinuityMailboxSigner`), never the real one;
//   2. register on it the one reach key a peer that accepted the reset can
//      derive, `continuityReachSeed(account id, key generation)`, expiring
//      with the window;
//   3. sign the binding and upload it to discovery beside the bundle.
//
// The window is counted from the attestation's `signed_at`, the instant every
// peer counts the continuity key's usability from (``ContinuityTargets``).
// On iOS the publish record learns `signed_at` at its verify step, so the
// record starts with `expiresAtMs == 0` (unknown) and is completed there.
//
// iOS mirror of `apps/web/src/auth/reset-continuity.ts`. Public values only
// (a key, an expiry, a generation); no seed is stored here.

/// The record one account keeps on the resetting device.
public struct ResetContinuity: Codable, Equatable, Sendable {
    /// Unix ms: the continuity mailbox is read strictly before it. `0` while
    /// the attestation's `signed_at` is not known yet.
    public var expiresAtMs: Int64
    /// Standard base64 of the 32-byte account key of before the reset, or
    /// `nil` when it could not be established.
    public var previousKeyB64: String?
    /// The generation the reset moved the account to: the continuity reach
    /// key is derived from it.
    public var keyGeneration: UInt64
    /// The announcement (claim, reach key, binding) landed.
    public var announced: Bool
    /// ADR-0101 section 11 (2026-11-28) — whether the reset carried a seal:
    /// `false` when it was made with none, or its seal was refused and it
    /// was published without one. Peers move nothing to an unsealed reset,
    /// so this device acts on nothing that arrives for it as if they had
    /// (``ResetContinuityRules/ownResetUnsealed(_:)``). `nil` in a record
    /// written before this was kept.
    public var sealed: Bool?

    public init(
        expiresAtMs: Int64, previousKeyB64: String?, keyGeneration: UInt64, announced: Bool = false,
        sealed: Bool? = nil
    ) {
        self.expiresAtMs = expiresAtMs
        self.previousKeyB64 = previousKeyB64
        self.keyGeneration = keyGeneration
        self.announced = announced
        self.sealed = sealed
    }
}

/// The pure rules of the reset side.
public enum ResetContinuityRules {
    /// The window (ADR-0101 section 2, the DS journal retention; chosen, not
    /// measured). Equal to the core's `continuityWindowMs()`.
    public static let windowMs: Int64 = 30 * 24 * 60 * 60 * 1000

    /// The binding's expiry for a reset signed at `signedAtMs`.
    public static func expiryMs(signedAtMs: Int64) -> Int64 { signedAtMs + windowMs }

    /// Whether the announcement is still owed at `nowMs` (never after the
    /// window: a binding for a past window is refused by everyone; never
    /// before the expiry is known).
    public static func announcementOwed(_ record: ResetContinuity?, nowMs: Int64) -> Bool {
        guard let record, !record.announced, record.expiresAtMs > 0 else { return false }
        return nowMs < record.expiresAtMs
    }

    /// ADR-0101 section 11 — this device's own reset was made without a seal
    /// (or published without one). Nothing a peer sends for it is acted on:
    /// its continuity mailbox is not read, a held channel return is dropped,
    /// a conversation joined is not a returned one. Only an explicit `false`
    /// counts; a record that does not say is treated as before.
    public static func ownResetUnsealed(_ record: ResetContinuity?) -> Bool {
        record?.sealed == false
    }

    /// Whether the continuity mailbox is read at `nowMs`.
    public static func mailboxReadable(expiresAtMs: Int64?, nowMs: Int64) -> Bool {
        guard let expiresAtMs, expiresAtMs > 0 else { return false }
        return nowMs < expiresAtMs
    }

    /// The verdict of the KT log on the served key, as the previous-key rule
    /// reads it.
    public enum LogStanding: Equatable, Sendable {
        /// The log's latest entry names the served key.
        case confirmed
        /// No log operator key is pinned in this build: the client's posture
        /// everywhere is then the directory.
        case off
        /// Any other verdict (mismatch, not found, a refusal).
        case other
    }

    /// The previous own key a device may rely on, from what it read before
    /// the reset was sent:
    ///
    /// | directory served | log verdict | previous key |
    /// |------------------|-------------|--------------|
    /// | none             | -           | none         |
    /// | key K            | confirmed   | K            |
    /// | key K            | off         | K            |
    /// | key K            | other / unasked | none     |
    /// | key K == new key | -           | none         |
    public static func previousKeyStanding(
        servedKeyB64: String?, logStatus: LogStanding?, newKeyB64: String
    ) -> String? {
        guard let served = servedKeyB64, served != newKeyB64 else { return nil }
        switch logStatus {
        case .confirmed?, .off?: return served
        default: return nil
        }
    }
}

/// Per-account store of ``ResetContinuity`` (UserDefaults, public values).
public struct ResetContinuityStore {
    /// Not `lakko.reset-continuity.v1.`: that prefix is ``ContinuityStore``'s.
    static let prefix = "lakko.reset-announcement.v1."

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func read(accountIdB64: String) -> ResetContinuity? {
        guard let data = defaults.data(forKey: Self.prefix + accountIdB64),
              let record = try? JSONDecoder().decode(ResetContinuity.self, from: data),
              record.expiresAtMs >= 0 else { return nil }
        return record
    }

    /// Write (replace) the record. Refused once the account's stores were
    /// sealed (``AccountStoreGate``).
    public func write(_ record: ResetContinuity, accountIdB64: String) {
        guard let data = try? JSONEncoder().encode(record) else { return }
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(data, forKey: Self.prefix + accountIdB64)
    }

    /// Complete the expiry once the attestation's `signed_at` is known.
    public func setSignedAt(_ signedAtMs: Int64, accountIdB64: String) {
        guard var record = read(accountIdB64: accountIdB64), signedAtMs > 0 else { return }
        let expiry = ResetContinuityRules.expiryMs(signedAtMs: signedAtMs)
        guard record.expiresAtMs != expiry else { return }
        record.expiresAtMs = expiry
        write(record, accountIdB64: accountIdB64)
    }

    /// The reset was published without its seal after all (the directory
    /// refused the seal): the record says unsealed.
    public func markUnsealed(accountIdB64: String) {
        guard var record = read(accountIdB64: accountIdB64), record.sealed != false else { return }
        record.sealed = false
        write(record, accountIdB64: accountIdB64)
    }

    public func markAnnounced(accountIdB64: String) {
        guard var record = read(accountIdB64: accountIdB64), !record.announced else { return }
        record.announced = true
        write(record, accountIdB64: accountIdB64)
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.prefix + accountIdB64)
    }
}
