// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0065 R.2 - the VERIFIED BINDING of an account this device never
// pinned, and the one rule that decides what a group member's roster key is
// worth against the directory.
//
// iOS mirror of `apps/web/src/contacts/verified-binding.ts`.
//
// The acceptance rule (`ResetVerdict.swift`) judges a change from "the key
// this device HOLDS for the account". For a contact that is the pin. For a
// group member this device never pinned, the account id comes from the
// member's OWN profile envelope, and a member can write any account id
// there. If the roster key were taken as the held key on that claim alone, a
// member could name ANOTHER account's id; once that account ever reset, an
// admin's client would read "member key -> that account's key" as a reset,
// propose the member's removal and add the other account's devices in its
// place.
//
// So a profile's claimed id establishes nothing. What does is a binding this
// device saw for itself: `(account id -> account key)` is recorded ONLY when
// this device itself fetched that account id's bundle through the fail-closed
// gate and the fetched key EQUALLED the roster key. The reset path for a
// non-pinned member runs only from such a binding, and only when it names
// exactly the member's roster key. The first observation is never a reset.
//
// A pinned account never reaches this file: the pin is the verified binding.
// An accepted reset and a proven rotation both write a pin, after which the
// binding is dropped.

/// One verified binding.
public struct VerifiedBinding: Codable, Equatable {
    /// The account key the directory served for the account id, standard
    /// base64, equal to the roster key at the moment it was recorded.
    public let keyB64: String
    /// The bundle's `key_generation` at that moment: the accepted-generation
    /// baseline a later reset must be strictly above (R.2 rule 2).
    public let keyGeneration: UInt64
    /// When this device recorded it, UNIX milliseconds.
    public let verifiedAtMs: Int64

    public init(keyB64: String, keyGeneration: UInt64, verifiedAtMs: Int64) {
        self.keyB64 = keyB64
        self.keyGeneration = keyGeneration
        self.verifiedAtMs = verifiedAtMs
    }
}

/// What a group member's roster key is worth for one account id.
public enum MemberHeldVerdict: Equatable {
    /// The account is pinned: the pin decides and the roster key is ignored.
    case pin
    /// No binding, and the directory serves exactly the roster key: record
    /// the binding now. A first observation, never a reset.
    case record
    /// The binding names the roster key and the directory still serves it.
    case confirmed
    /// The binding names the roster key and the directory serves ANOTHER
    /// key: a change from a key this device verified. It is judged from the
    /// binding exactly as from a pin.
    case held(heldKeyB64: String, acceptedGeneration: UInt64)
    /// Nothing is established for this roster key under this account id: no
    /// verdict, nothing done, nothing shown.
    case none
}

/// | pinned | binding | binding names the roster key | directory serves the roster key | verdict     |
/// |--------|---------|------------------------------|---------------------------------|-------------|
/// | yes    | -       | -                            | -                               | `pin`       |
/// | no     | none    | -                            | yes                             | `record`    |
/// | no     | none    | -                            | no                              | `none`      |
/// | no     | held    | yes                          | yes                             | `confirmed` |
/// | no     | held    | yes                          | no                              | `held`      |
/// | no     | held    | no                           | any                             | `none`      |
///
/// Row 3 is the attack this file exists for (a member claiming another
/// account's id, that account having reset at any time) and also the honest
/// first sight of a member whose account already moved on: neither is told
/// apart here, and neither is acted on. Row 6 is the same claim made after
/// this device already verified the account under its real key.
public func memberHeldKey(
    pinned: Bool,
    binding: VerifiedBinding?,
    rosterKeyB64: String,
    fetchedKeyB64: String
) -> MemberHeldVerdict {
    if pinned { return .pin }
    guard let binding else {
        return fetchedKeyB64 == rosterKeyB64 ? .record : MemberHeldVerdict.none
    }
    if binding.keyB64 != rosterKeyB64 { return MemberHeldVerdict.none }
    if fetchedKeyB64 == rosterKeyB64 { return .confirmed }
    return .held(heldKeyB64: binding.keyB64, acceptedGeneration: binding.keyGeneration)
}

/// The bindings of one signed-in account, in `UserDefaults` like the pin
/// store (public keys and account ids; nothing secret). Never carried to
/// another device: a binding is what THIS device fetched. Removed with the
/// account's data.
public final class VerifiedBindingStore {

    static let prefix = "me.lakko.contacts.verified-bindings.v1."

    private let defaults: UserDefaults
    private let ownerAccountIdB64: String?

    public init(defaults: UserDefaults = .standard, ownerAccountIdB64: String? = nil) {
        self.defaults = defaults
        self.ownerAccountIdB64 = ownerAccountIdB64
    }

    private var storageKey: String? {
        (ownerAccountIdB64 ?? AcceptedContactsStore.boundAccountIdB64).map { Self.prefix + $0 }
    }

    private func load() -> [String: VerifiedBinding] {
        guard let storageKey, let raw = defaults.data(forKey: storageKey) else { return [:] }
        // An unreadable blob reads as "no binding": the fail-closed direction
        // (nothing is judged from it).
        return (try? JSONDecoder().decode([String: VerifiedBinding].self, from: raw)) ?? [:]
    }

    private func save(_ all: [String: VerifiedBinding]) {
        guard let storageKey, let raw = try? JSONEncoder().encode(all) else { return }
        defaults.set(raw, forKey: storageKey)
    }

    /// The verified binding of `accountIdB64`, or `nil`.
    public func read(accountIdB64: String) -> VerifiedBinding? {
        load()[accountIdB64]
    }

    /// Record a binding. WRITE-ONCE: a held binding is never replaced here (a
    /// change from it is a rotation or a reset, which ends in a pin, never in
    /// a second binding). Returns whether it was written.
    @discardableResult
    public func record(accountIdB64: String, binding: VerifiedBinding) -> Bool {
        guard storageKey != nil, !binding.keyB64.isEmpty else { return false }
        var all = load()
        if all[accountIdB64] != nil { return false }
        all[accountIdB64] = binding
        save(all)
        return true
    }

    /// Drop a binding (the account is pinned now: the pin is the binding).
    public func drop(accountIdB64: String) {
        var all = load()
        guard all[accountIdB64] != nil else { return }
        all.removeValue(forKey: accountIdB64)
        save(all)
    }

    /// Forget every binding of the account (its data is wiped).
    public static func clear(ownerAccountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: prefix + ownerAccountIdB64)
    }
}
