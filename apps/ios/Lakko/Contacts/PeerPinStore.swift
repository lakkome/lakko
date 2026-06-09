// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// G-4 (ADR-0016): persistent per-account account-pubkey PIN store.
///
/// Mirror of `apps/web/src/contacts/local-store.ts` (schema v2 pin
/// map). The Android twin (`apps/android/.../contacts/PeerPinStore.kt`)
/// lands in the same sprint wave.
///
/// ## What this is for
///
/// A peer's discovery identity bundle carries the peer's
/// AccountIdentity Ed25519 `signature_pubkey`; that key **rotates**
/// on recover-from-words (ADR-0007). ADR-0016 correctness rule #3
/// requires the pin be keyed on the stable **account_id**, not on
/// the (mutable) pubkey — keying on the pubkey would fork the
/// contact silently when the key changes, defeating the whole
/// supersession-proof bridge.
///
/// Each pin records:
///
///   * `pinnedSignaturePubkeyB64` — the currently-trusted
///     AccountIdentity pubkey for this account (mutable; advanced
///     only via a verified supersession proof on the fetch path —
///     see `ContactsClient.verifyBundleAccountKey`).
///   * `rotationMarker` — a **monotonic** marker (ADR-0016 rule #4):
///     the `expires_at` unix-seconds of the last-accepted proof,
///     default `0` for a plain TOFU-pin. A re-pin that would move the
///     marker backwards is refused (`SetPinResult.rejectedBackward`),
///     blunting a within-window rollback to an older successor.
///
/// ## Threat model / persistence choice
///
/// The pin bytes are **NOT secret** — a public key is public by
/// definition. The *trust decision* they encode is security-critical,
/// but the bytes themselves carry no secret. So, like the web side
/// (`localStorage`, not the encrypted IDB store), we persist in
/// `UserDefaults` rather than the Keychain. (Contrast
/// ``ExportKeyStore``, which stores the 32-byte OPAQUE export-key
/// *seed* and therefore lives in the Keychain.) `UserDefaults`
/// survives app relaunch and process termination, which is all
/// rule #3/#4 require.
///
/// One JSON blob under a single versioned key holds the whole
/// `account_id (base64) → pin` map, matching the web's one-row
/// schema. A junk / malformed entry is dropped on read rather than
/// mis-trusted.
public final class PeerPinStore {

    /// Versioned storage key — a future schema change can land
    /// alongside v1 without collision.
    private static let storageKey = "me.lakko.contacts.pins.v1"

    /// The backing store. Injectable so tests can use an isolated
    /// `UserDefaults(suiteName:)` instead of `.standard`.
    private let defaults: UserDefaults

    /// One pinned AccountIdentity pubkey for a peer, keyed (in the
    /// map) on the peer's stable `account_id` (base64). The value,
    /// not the key, holds the mutable pubkey (ADR-0016 rule #3).
    public struct AccountPubkeyPin: Codable, Equatable {
        /// The currently-trusted AccountIdentity Ed25519 pubkey for
        /// this account, base64-encoded. Advanced only via a
        /// verified supersession proof on the fetch path.
        public let pinnedSignaturePubkeyB64: String
        /// Monotonic rotation marker (ADR-0016 rule #4): the
        /// `expires_at` unix-seconds of the last-accepted proof, or
        /// `0` for a plain TOFU-pin. A re-pin that would lower this
        /// is refused.
        public let rotationMarker: Int64

        public init(pinnedSignaturePubkeyB64: String, rotationMarker: Int64) {
            self.pinnedSignaturePubkeyB64 = pinnedSignaturePubkeyB64
            self.rotationMarker = rotationMarker
        }
    }

    /// Outcome of a ``setPin(accountIdB64:pinnedSignaturePubkeyB64:rotationMarker:)``
    /// call. `.set` covers both a first-time TOFU-pin and a
    /// monotonic forward re-pin; `.rejectedBackward` signals the
    /// marker would have moved backwards (ADR-0016 rule #4) so
    /// nothing was written.
    public enum SetPinResult: Equatable {
        case set
        case rejectedBackward
    }

    /// - Parameter defaults: the backing `UserDefaults`. Defaults to
    ///   `.standard`; tests inject an isolated suite.
    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    // MARK: - Read / write the whole map

    /// Load the `account_id (base64) → pin` map, dropping any
    /// structurally-malformed entry rather than mis-trusting it.
    private func loadMap() -> [String: AccountPubkeyPin] {
        guard let raw = defaults.data(forKey: Self.storageKey) else {
            return [:]
        }
        // A decode failure (corrupt blob / future schema we can't
        // read) drops the whole map — fail-closed: better to TOFU
        // a fresh observation than to mis-trust garbage. The next
        // write rebuilds the blob cleanly.
        return (try? JSONDecoder().decode(
            [String: AccountPubkeyPin].self, from: raw
        )) ?? [:]
    }

    private func saveMap(_ map: [String: AccountPubkeyPin]) {
        guard let encoded = try? JSONEncoder().encode(map) else {
            // Encoding a `[String: AccountPubkeyPin]` cannot fail in
            // practice (only String keys + Codable values); if it
            // somehow does, leaving the prior blob in place is the
            // safe outcome — we never write garbage.
            NSLog("PeerPinStore: pin-map encode failed; leaving prior state")
            return
        }
        defaults.set(encoded, forKey: Self.storageKey)
    }

    // MARK: - Public API (mirror of the web local-store pin surface)

    /// Read the pinned AccountIdentity pubkey for a peer, keyed on
    /// the peer's stable `account_id` (base64, standard alphabet).
    ///
    /// Returns `nil` when no pin exists yet — the caller treats that
    /// as the TOFU (trust-on-first-use) case and pins the
    /// freshly-observed key. A present pin is what the caller
    /// compares the fetched `signature_pubkey` against; a mismatch
    /// demands a verified supersession proof (ADR-0016 rule #2,
    /// fail-closed).
    public func readPin(accountIdB64: String) -> AccountPubkeyPin? {
        loadMap()[accountIdB64]
    }

    /// Set or update the account-pubkey pin for `accountIdB64`,
    /// enforcing the monotonic-marker invariant (ADR-0016 rule #4).
    ///
    /// Semantics (identical to the web `setPin`):
    ///   * No existing pin → store `{ pubkey, marker }` (TOFU or first
    ///     verified pin). Returns `.set`.
    ///   * Existing pin and `marker >= existing.rotationMarker` →
    ///     overwrite the pubkey + advance the marker. Returns `.set`.
    ///     (A `==` marker permits re-pinning the *same* successor — an
    ///     idempotent re-observation — without forking.)
    ///   * Existing pin and `marker < existing.rotationMarker` →
    ///     **refuse**, leave the pin untouched, return
    ///     `.rejectedBackward`. This blunts a within-window rollback
    ///     to an older successor.
    ///
    /// The pubkey is intentionally **mutable** for a given
    /// `account_id`: a changed key is a *detected mutation*, not a new
    /// contact (ADR-0016 rule #3). The trust gate that authorises the
    /// mutation lives on the fetch path, not here.
    @discardableResult
    public func setPin(
        accountIdB64: String,
        pinnedSignaturePubkeyB64: String,
        rotationMarker: Int64
    ) -> SetPinResult {
        var map = loadMap()
        if let existing = map[accountIdB64],
           rotationMarker < existing.rotationMarker {
            return .rejectedBackward
        }
        map[accountIdB64] = AccountPubkeyPin(
            pinnedSignaturePubkeyB64: pinnedSignaturePubkeyB64,
            rotationMarker: rotationMarker
        )
        saveMap(map)
        return .set
    }

    /// Remove the pin for `accountIdB64`. No-op when absent. Exposed
    /// for the "forget contact" / re-verify-from-scratch flows.
    public func removePin(accountIdB64: String) {
        var map = loadMap()
        guard map[accountIdB64] != nil else { return }
        map.removeValue(forKey: accountIdB64)
        saveMap(map)
    }

    /// Wipe the entire pin map. Called from a device-wipe flow
    /// (mirror of the web `clearAll`'s pin half). No-op when absent.
    public func clearAll() {
        defaults.removeObject(forKey: Self.storageKey)
    }
}
