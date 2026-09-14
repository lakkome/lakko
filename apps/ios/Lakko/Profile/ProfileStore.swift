// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0048 — a person's profile as THEY state it: display name, the username
/// they claim, and the sender-clock time they set it.
///
/// Every field is a claim. The binding to a person is supplied by the
/// RECEIVER (the signature key the profile was authenticated under, C-68);
/// the username is shown only after ``UsernameVerifier`` resolves it to that
/// same key.
public struct ProfileClaim: Codable, Equatable, Hashable, Sendable {
    /// Display name, or `nil` for no name.
    public let displayName: String?
    /// Claimed username in canonical form (no `@`), or `nil`.
    public let username: String?
    /// Sender-clock UNIX milliseconds at which the profile was set.
    public let updatedAtMs: UInt64

    public init(displayName: String?, username: String?, updatedAtMs: UInt64) {
        self.displayName = displayName
        self.username = username
        self.updatedAtMs = updatedAtMs
    }
}

/// Where the received-profile map lives on disk. A seam so tests run without
/// `UserDefaults`.
protocol ProfilePersistence {
    func load(accountIdB64: String) -> Data?
    func save(_ data: Data, accountIdB64: String)
    func delete(accountIdB64: String)
}

/// Production persistence: a JSON blob per account in `UserDefaults`.
///
/// App-private, not the App Group suite: the only reader that could want it
/// is the NotificationServiceExtension, and that process cannot attribute a
/// row to a sender (the slim FFI exposes no roster), so it has nothing to look
/// up. Same data class as the accepted-contacts list — names of people the
/// user talks to — and like it never sent anywhere (invariants #1, #2).
struct UserDefaultsProfilePersistence: ProfilePersistence {
    let defaults: UserDefaults
    let keyPrefix: String

    init(defaults: UserDefaults = .standard, keyPrefix: String = "lakko.profiles.received.v1.") {
        self.defaults = defaults
        self.keyPrefix = keyPrefix
    }

    func load(accountIdB64: String) -> Data? {
        defaults.data(forKey: keyPrefix + accountIdB64)
    }

    func save(_ data: Data, accountIdB64: String) {
        defaults.set(data, forKey: keyPrefix + accountIdB64)
    }

    func delete(accountIdB64: String) {
        defaults.removeObject(forKey: keyPrefix + accountIdB64)
    }
}

/// Outcome of offering a received profile to the store.
enum ProfileAcceptance: Equatable {
    /// Stored: the first profile for this key, or strictly newer than the one
    /// held.
    case stored
    /// Not newer than the stored one (equal is not newer — a re-broadcast is
    /// not an update, and a backwards sender clock only delays its own update).
    case notNewer
    /// No authenticated 32-byte signature key to bind it to. Dropped.
    case noSender
    /// No account is active, so there is nowhere to keep it.
    case inactive
}

/// ADR-0048 §3 — received profiles, keyed by the sender's SIGNATURE KEY
/// resolved at receipt (C-68).
///
/// Never keyed on a leaf index (OpenMLS hands a vacated leaf to the next
/// member added) and never on anything the envelope itself names (the payload
/// carries no identity on purpose). The two sources are:
///
///   * an in-group profile — the MLS sender leaf resolved against the roster of
///     the epoch the message decrypted at (``ChatClient/toEvent``);
///   * a contact request — `ContactRequest.from_pubkey`, which the request
///     signature authenticates.
///
/// The order is injected (`profileIsNewer` in production) so no Swift copy of
/// the rule exists.
@MainActor
final class ReceivedProfileStore {
    private let persistence: ProfilePersistence
    private let isNewer: (UInt64, UInt64) -> Bool
    private var accountIdB64: String?
    private var profiles: [String: ProfileClaim] = [:]

    init(
        persistence: ProfilePersistence,
        isNewer: @escaping (_ incoming: UInt64, _ stored: UInt64) -> Bool
    ) {
        self.persistence = persistence
        self.isNewer = isNewer
    }

    /// Switch to `accountIdB64` (or to none) and load its map. A corrupt blob
    /// loads as empty — the next profile received re-populates it.
    func activate(accountIdB64: String?) {
        guard accountIdB64 != self.accountIdB64 else { return }
        self.accountIdB64 = accountIdB64
        profiles = [:]
        guard let accountIdB64, let data = persistence.load(accountIdB64: accountIdB64) else {
            return
        }
        profiles = (try? JSONDecoder().decode([String: ProfileClaim].self, from: data)) ?? [:]
    }

    /// Offer a received profile bound to `signatureKeyB64`. Keeps it only when
    /// it is the first for that key or strictly newer.
    func accept(_ claim: ProfileClaim, signatureKeyB64: String?) -> ProfileAcceptance {
        guard let accountIdB64 else { return .inactive }
        guard let key = signatureKeyB64, Self.isSignatureKey(key) else { return .noSender }
        if let stored = profiles[key], !isNewer(claim.updatedAtMs, stored.updatedAtMs) {
            return .notNewer
        }
        profiles[key] = claim
        if let data = try? JSONEncoder().encode(profiles) {
            persistence.save(data, accountIdB64: accountIdB64)
        }
        return .stored
    }

    /// The stored profile for `signatureKeyB64`, if any.
    func profile(signatureKeyB64: String) -> ProfileClaim? {
        profiles[signatureKeyB64]
    }

    /// Drop the account's map from disk (forget-device) and from memory when
    /// it is the active one.
    func clear(accountIdB64: String) {
        persistence.delete(accountIdB64: accountIdB64)
        if self.accountIdB64 == accountIdB64 { profiles = [:] }
    }

    /// A base64 string that decodes to exactly 32 bytes — an Ed25519 public
    /// key. Anything else (a sentinel, a torn roster read, a leaf rendered as
    /// text) is not an identity a profile may be bound to.
    static func isSignatureKey(_ b64: String) -> Bool {
        Data(base64Encoded: b64)?.count == 32
    }
}

// MARK: - Own profile

/// The profile this device publishes for its own account.
public struct OwnProfile: Codable, Equatable, Sendable {
    /// The normalised display name, or `nil` for no name.
    public let displayName: String?
    /// When it was last set, in UNIX milliseconds. `0` for a profile never set.
    public let updatedAtMs: UInt64

    public init(displayName: String?, updatedAtMs: UInt64) {
        self.displayName = displayName
        self.updatedAtMs = updatedAtMs
    }
}

/// Local persistence of the own profile, per account.
///
/// Local only: there is no per-account profile on any server (ADR-0048
/// alternatives), and a second device of the same account keeps its own.
struct OwnProfileStore {
    let defaults: UserDefaults
    let keyPrefix: String

    init(defaults: UserDefaults = .standard, keyPrefix: String = "lakko.profiles.own.v1.") {
        self.defaults = defaults
        self.keyPrefix = keyPrefix
    }

    func load(accountIdB64: String) -> OwnProfile? {
        guard let data = defaults.data(forKey: keyPrefix + accountIdB64) else { return nil }
        return try? JSONDecoder().decode(OwnProfile.self, from: data)
    }

    /// Save `displayName` (already normalised) and return what was stored.
    ///
    /// The stamp is `max(nowMs, previous + 1)`: receivers keep a profile only
    /// when it is STRICTLY newer, so two saves in the same millisecond — or a
    /// device clock that stepped backwards — must still produce an update
    /// every peer accepts.
    @discardableResult
    func save(displayName: String?, accountIdB64: String, nowMs: UInt64) -> OwnProfile {
        let previous = load(accountIdB64: accountIdB64)?.updatedAtMs ?? 0
        let stamp = max(nowMs, previous &+ 1)
        let profile = OwnProfile(displayName: displayName, updatedAtMs: stamp)
        if let data = try? JSONEncoder().encode(profile) {
            defaults.set(data, forKey: keyPrefix + accountIdB64)
        }
        return profile
    }

    func delete(accountIdB64: String) {
        defaults.removeObject(forKey: keyPrefix + accountIdB64)
    }
}
