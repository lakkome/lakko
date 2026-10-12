// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0065 "Phase 2 revised", R.4 - the CONTINUITY reach key.
//
// iOS mirror of `apps/web/src/reach/continuity.ts`.
//
// A reset changes every reach key and the mailbox pseudonym (they derive
// from the account seed), and the new identity knows none of its former
// contacts, so nobody holds a grant for its new mailbox. The continuity key
// is what lets a peer that ACCEPTED the reset deliver a Welcome there:
//
//     continuity seed = continuityReachSeed(account id, key generation)
//
// Both sides derive it from public values the peer verified (the account id
// and the attested generation):
//
//   * the RESETTING device registers the key's public half on its new
//     account mailbox in its reset order, with an expiry (this file's
//     marker; `ReachCenter` keeps it registered until then);
//   * a PEER signs its Welcome with `DsProofSigner.forReach(seed)`
//     (`ReachCenter.writeAuthCandidates`).
//
// It is a write permission, not a secret that protects content: anyone who
// knows the account id and the generation can derive it (R.4 "What it
// admits"). Once the new identity is in a conversation its profile hands out
// the ordinary grants and the peer does not use this key again.

public enum Continuity {

    /// How long the continuity key stays registered, from the reset's
    /// `signed_at`: the DS journal retention (decision D2). Chosen for parity
    /// with how long an offline peer's traffic survives anyway, not measured.
    public static let keyTtlMs: Int64 = 30 * 24 * 60 * 60 * 1000

    /// The expiry the key is registered with.
    public static func expiryMs(signedAtMs: Int64) -> Int64 {
        signedAtMs + keyTtlMs
    }

    /// What the resetting device remembers about its own last reset.
    public struct Marker: Codable, Equatable {
        /// The generation the reset moved the account to.
        public let keyGeneration: UInt64
        /// The attestation's `signed_at`, UNIX milliseconds.
        public let signedAtMs: Int64

        public init(keyGeneration: UInt64, signedAtMs: Int64) {
            self.keyGeneration = keyGeneration
            self.signedAtMs = signedAtMs
        }
    }

    /// The key the resetting device wants registered now.
    public struct Wanted: Equatable {
        public let keyGeneration: UInt64
        public let expiresAtMs: Int64
    }

    /// Whether the continuity key is still to be kept registered:
    ///
    /// | marker                         | wanted                         |
    /// |--------------------------------|--------------------------------|
    /// | none                           | no                             |
    /// | present, before the expiry     | yes, with that expiry          |
    /// | present, at or past the expiry | no (the reach sync removes it) |
    public static func keyWanted(marker: Marker?, nowMs: Int64) -> Wanted? {
        guard let marker, marker.keyGeneration > 0 else { return nil }
        let expiresAtMs = expiryMs(signedAtMs: marker.signedAtMs)
        if nowMs >= expiresAtMs { return nil }
        return Wanted(keyGeneration: marker.keyGeneration, expiresAtMs: expiresAtMs)
    }

    /// Whether a PEER still signs a Welcome with the continuity key: inside
    /// the same window, counted from the attestation's `signed_at` it
    /// verified. Past it the key is no longer registered, and the write would
    /// be refused (a peer that returns later cannot re-establish silently,
    /// R.4 "Expiry").
    public static func keyUsable(signedAtMs: Int64, nowMs: Int64) -> Bool {
        nowMs < expiryMs(signedAtMs: signedAtMs)
    }

    /// The `signed_at` of a 177-byte key-reset attestation, read from its
    /// fixed layout (`version(1) ‖ account_ref(32) ‖ journal_id(32) ‖
    /// key_generation(8) ‖ new_pubkey(32) ‖ signed_at(8 BE) ‖ sig(64)`), or
    /// `nil` for any other length. The resetting device reads its OWN
    /// attestation here, which auth minted for it inside the reset; a peer
    /// never uses this (it takes `signed_at` from the verifier).
    public static func attestationSignedAtMs(_ attestation: Data) -> Int64? {
        guard attestation.count == 177 else { return nil }
        let bytes = [UInt8](attestation)
        var value: UInt64 = 0
        for byte in bytes[105..<113] { value = (value << 8) | UInt64(byte) }
        return Int64(bitPattern: value)
    }

    // MARK: - The marker

    static let markerPrefix = "me.lakko.reset-continuity."

    /// Record this device's own reset (the reset's client order).
    public static func writeMarker(_ marker: Marker, accountIdB64: String, defaults: UserDefaults = .standard) {
        guard let raw = try? JSONEncoder().encode(marker) else { return }
        defaults.set(raw, forKey: markerPrefix + accountIdB64)
    }

    /// The marker, or `nil` (none, or an unreadable row).
    public static func readMarker(accountIdB64: String, defaults: UserDefaults = .standard) -> Marker? {
        guard let raw = defaults.data(forKey: markerPrefix + accountIdB64) else { return nil }
        return try? JSONDecoder().decode(Marker.self, from: raw)
    }

    /// Forget the marker (the account's data is wiped).
    public static func clearMarker(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: markerPrefix + accountIdB64)
    }

    // MARK: - The peer's side

    /// The continuity key a Welcome may be signed with.
    public struct Auth: Equatable {
        public let accountId: Data
        public let keyGeneration: UInt64
    }

    /// The continuity key a Welcome to `targetAccountKeyB64` may be signed
    /// with (R.4), or `nil`: only when that key is the one an accepted reset
    /// re-pinned this device to, and only inside the key's window, counted
    /// from the attestation's `signed_at` this device verified.
    public static func auth(
        targetAccountKeyB64: String,
        pins: [String: PeerPinStore.AccountPubkeyPin],
        nowMs: Int64
    ) -> Auth? {
        for (accountIdB64, pin) in pins where pin.pinnedSignaturePubkeyB64 == targetAccountKeyB64 {
            guard let signedAtMs = pin.resetSignedAtMs,
                  let keyGeneration = pin.acceptedKeyGeneration, keyGeneration > 0,
                  keyUsable(signedAtMs: signedAtMs, nowMs: nowMs),
                  let accountId = Data(base64Encoded: accountIdB64), accountId.count == 16
            else { continue }
            return Auth(accountId: accountId, keyGeneration: keyGeneration)
        }
        return nil
    }
}
