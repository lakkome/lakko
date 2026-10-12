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
/// One JSON blob per signed-in ACCOUNT holds that account's whole
/// `account_id (base64) → pin` map (`me.lakko.contacts.pins.v2.<owner>`).
/// A junk / malformed entry is dropped on read rather than mis-trusted.
///
/// ## Per account (2026-09-30)
///
/// The map used to live under one device-wide key, so a second account
/// signed in on the same phone inherited the first account's trust
/// decisions (and, through them, the fact of who it talked to). The pins
/// belong to the contact list they sit beside (``AcceptedContactsStore``),
/// so they are keyed the same way: by the account that holds them. A store
/// built without an owner acts on the account ``AcceptedContactsStore``
/// has bound, resolved at each call; with none bound, reads are empty and
/// writes are dropped — never filed under a shared key.
public final class PeerPinStore {

    /// Versioned storage-key prefix; the owning account id follows.
    private static let prefix = "me.lakko.contacts.pins.v2."
    /// The device-wide key this store used before it was per account. Never
    /// read; removed by ``clear(ownerAccountIdB64:defaults:)``.
    static let legacyGlobalKey = "me.lakko.contacts.pins.v1"

    /// The backing store. Injectable so tests can use an isolated
    /// `UserDefaults(suiteName:)` instead of `.standard`.
    private let defaults: UserDefaults
    /// The account whose pins this store holds; `nil` = the bound account,
    /// read at each call.
    private let ownerAccountIdB64: String?

    /// The key of the owning account's map, `nil` when no account is known.
    private var storageKey: String? {
        (ownerAccountIdB64 ?? AcceptedContactsStore.boundAccountIdB64).map { Self.prefix + $0 }
    }

    /// One pinned AccountIdentity pubkey for a peer, keyed (in the
    /// map) on the peer's stable `account_id` (base64). The value,
    /// not the key, holds the mutable pubkey (ADR-0016 rule #3).
    public struct AccountPubkeyPin: Codable, Equatable {
        /// The currently-trusted AccountIdentity Ed25519 pubkey for
        /// this account, base64-encoded. Advanced only via a
        /// verified supersession proof on the fetch path.
        public var pinnedSignaturePubkeyB64: String
        /// Monotonic rotation marker (ADR-0016 rule #4): the
        /// `expires_at` unix-seconds of the last-accepted proof, or
        /// `0` for a plain TOFU-pin. A re-pin that would lower this
        /// is refused.
        public var rotationMarker: Int64
        /// KT-log S3d: the last transparency-log inclusion verdict
        /// observed for this account on the identity-fetch path (the
        /// raw-value string of a ``LogVerifyStatus``). The log check is
        /// the equivocation-detection layer ON TOP of the
        /// supersession/pin gate (ADR-0016); this records the latest
        /// best-effort outcome so the UI can surface it. `nil` on a pin
        /// written before S3d (or by a path that did not run the
        /// check). Mirror of the web pin-record `logStatus`.
        public var logStatus: String?
        /// KT-log S4: the `is_reset` marker the transparency log's
        /// `/latest` entry carried for the CURRENTLY-PINNED key, when a
        /// latest entry was read (`confirmed` / `mismatch`). `true`
        /// means the log records an owner-initiated identity RESET
        /// (ADR-0017 lost-everything fresh-start); `false` means a
        /// non-reset entry; `nil` when no latest entry was read (or the
        /// pin predates S4). Stored next to ``logStatus`` so the UI can
        /// surface the reset-vs-compromise distinction. Mirror of the
        /// web pin-record `isReset`.
        public var isReset: Bool?
        /// G-30 #2 (ADR-0019): the transparency-log VERSION at which the
        /// currently-pinned key was chain-confirmed (the `/latest` entry's
        /// per-account-key `version`). Written ONLY on a `confirmed`
        /// rotation-chain verdict
        /// (``TransparencyVerify/verifyRotationChain(accountId:servedPubkey:ctx:)``).
        /// The next rotation's chain proof must reference a
        /// STRICTLY-NEWER logged version, so persisting it gives the chain
        /// verifier its downgrade-protection floor. `nil` on a pin that
        /// has never been chain-confirmed (a TOFU pin, or one written
        /// before G-30 #2 — synthesized `Codable` decodes an absent key
        /// to `nil`, so old blobs migrate transparently). Mirror of the
        /// web pin-record `pinnedLogVersion`.
        public var pinnedLogVersion: UInt64?

        public init(
            pinnedSignaturePubkeyB64: String,
            rotationMarker: Int64,
            logStatus: String? = nil,
            isReset: Bool? = nil,
            pinnedLogVersion: UInt64? = nil,
            acceptedKeyGeneration: UInt64? = nil,
            previousKeysB64: [String]? = nil,
            supersededKeysB64: [String]? = nil,
            resetSignedAtMs: Int64? = nil
        ) {
            self.pinnedSignaturePubkeyB64 = pinnedSignaturePubkeyB64
            self.rotationMarker = rotationMarker
            self.logStatus = logStatus
            self.isReset = isReset
            self.pinnedLogVersion = pinnedLogVersion
            self.acceptedKeyGeneration = acceptedKeyGeneration
            self.previousKeysB64 = previousKeysB64
            self.supersededKeysB64 = supersededKeysB64
            self.resetSignedAtMs = resetSignedAtMs
        }

        /// ADR-0065 R.2 rule 2 - the key generation this device last
        /// ACCEPTED for the account: the bundle's generation at the first
        /// pin, then the generation of each accepted reset. A reset is
        /// accepted only strictly above it, so an old attestation cannot roll
        /// a contact back. `nil` for a pin written before this was recorded
        /// (its baseline is taken at the next unchanged fetch).
        public var acceptedKeyGeneration: UInt64?
        /// R.2 "the re-pin" - every key this account held before a reset
        /// this device accepted, oldest first. Display aliases for stored
        /// history only.
        public var previousKeysB64: [String]?
        /// The keys an ACCEPTED RESET retired (never a proven predecessor,
        /// which is the same person's key): a row or a claim from one of
        /// these is dropped, and no successor is ever built with one.
        public var supersededKeysB64: [String]?
        /// The `signed_at` of the latest accepted reset, UNIX milliseconds:
        /// what "sent after the reset" is measured against (R.7 case 1).
        public var resetSignedAtMs: Int64?
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

    /// - Parameters:
    ///   - defaults: the backing `UserDefaults`. Defaults to `.standard`;
    ///     tests inject an isolated suite.
    ///   - ownerAccountIdB64: the signed-in account whose pins these are.
    ///     `nil` (the default) follows the account ``AcceptedContactsStore``
    ///     has bound.
    public init(defaults: UserDefaults = .standard, ownerAccountIdB64: String? = nil) {
        self.defaults = defaults
        self.ownerAccountIdB64 = ownerAccountIdB64
    }

    /// Forget-device: drop one account's pin map (and the old device-wide
    /// map, which nothing reads any more). Other accounts are untouched.
    public static func clear(ownerAccountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: prefix + ownerAccountIdB64)
        defaults.removeObject(forKey: legacyGlobalKey)
    }

    // MARK: - Read / write the whole map

    /// Load the `account_id (base64) → pin` map, dropping any
    /// structurally-malformed entry rather than mis-trusting it.
    private func loadMap() -> [String: AccountPubkeyPin] {
        guard let storageKey, let raw = defaults.data(forKey: storageKey) else {
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
        guard let storageKey else {
            // No signed-in account: a pin with no owner would be a shared
            // trust decision, which is what this store exists not to keep.
            NSLog("PeerPinStore: no account bound; pin not written")
            return
        }
        guard let encoded = try? JSONEncoder().encode(map) else {
            // Encoding a `[String: AccountPubkeyPin]` cannot fail in
            // practice (only String keys + Codable values); if it
            // somehow does, leaving the prior blob in place is the
            // safe outcome — we never write garbage.
            NSLog("PeerPinStore: pin-map encode failed; leaving prior state")
            return
        }
        defaults.set(encoded, forKey: storageKey)
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
        rotationMarker: Int64,
        keyGeneration: UInt64? = nil
    ) -> SetPinResult {
        var map = loadMap()
        let existing = map[accountIdB64]
        if let existing = existing,
           rotationMarker < existing.rotationMarker {
            return .rejectedBackward
        }
        // Preserve any recorded `logStatus` (KT-log S3d) + `isReset`
        // (KT-log S4) + `pinnedLogVersion` (G-30 #2) across a re-pin —
        // they are orthogonal, separately-written fields, and a key
        // rotation shouldn't silently drop the last log verdict / reset
        // marker / downgrade floor. The fetch path re-records all of them
        // right after a re-pin (the chain-confirmed version only on a
        // `confirmed` rotation), so a stale carried value is replaced
        // immediately (mirror of the web `setPin`).
        map[accountIdB64] = AccountPubkeyPin(
            pinnedSignaturePubkeyB64: pinnedSignaturePubkeyB64,
            rotationMarker: rotationMarker,
            logStatus: existing?.logStatus,
            isReset: existing?.isReset,
            pinnedLogVersion: existing?.pinnedLogVersion,
            // ADR-0065 R.2 - the accepted-generation baseline: the caller's
            // (a first pin records the bundle's generation) or the one held.
            acceptedKeyGeneration: keyGeneration ?? existing?.acceptedKeyGeneration,
            previousKeysB64: existing?.previousKeysB64,
            supersededKeysB64: existing?.supersededKeysB64,
            resetSignedAtMs: existing?.resetSignedAtMs
        )
        saveMap(map)
        return .set
    }

    /// Record the latest transparency-log inclusion verdict (KT-log
    /// S3d) for an account, keyed on the same stable `account_id` as the
    /// pin. Mirror of the web `recordLogStatus`.
    ///
    /// Writes onto the existing pin record so the verdict travels with
    /// the pinned key. **No-op when no pin exists** — the log check is
    /// always run right after the supersession gate (which TOFU-pins on
    /// first sight), so by the time a verdict is recorded a pin is
    /// present; if for any reason it is not, we do not fabricate a pin
    /// entry with no pubkey. Returns whether a write happened (a present
    /// pin).
    @discardableResult
    public func recordLogStatus(accountIdB64: String, logStatus: String) -> Bool {
        var map = loadMap()
        guard let existing = map[accountIdB64] else { return false }
        if existing.logStatus == logStatus { return true } // idempotent
        var updated = existing
        updated.logStatus = logStatus
        map[accountIdB64] = updated
        saveMap(map)
        return true
    }

    /// Read the recorded log-verify status for an account, or `nil` when
    /// no pin / no status. Exposed for the UI signal. Mirror of the web
    /// `readLogStatus`.
    public func readLogStatus(accountIdB64: String) -> String? {
        loadMap()[accountIdB64]?.logStatus
    }

    /// Record the transparency log's `is_reset` marker (KT-log S4) for
    /// an account, keyed on the same stable `account_id` as the pin.
    ///
    /// Mirrors ``recordLogStatus(accountIdB64:logStatus:)``: writes onto
    /// the existing pin record so the marker travels with the pinned
    /// key, and is a **no-op when no pin exists** (the log check runs
    /// right after the supersession gate, which TOFU-pins on first
    /// sight, so a pin is present by the time a marker is recorded). A
    /// `nil` argument (no latest entry was read — `absent` /
    /// `unreachable` / `off` / `rollback`) CLEARS any stale marker so
    /// the UI never surfaces a reset claim that the current log state no
    /// longer supports. Returns whether a write happened (a present
    /// pin). Mirror of the web `recordLogReset`.
    @discardableResult
    public func recordLogReset(accountIdB64: String, isReset: Bool?) -> Bool {
        var map = loadMap()
        guard let existing = map[accountIdB64] else { return false }
        if existing.isReset == isReset { return true } // idempotent (incl. nil==nil)
        var updated = existing
        updated.isReset = isReset
        map[accountIdB64] = updated
        saveMap(map)
        return true
    }

    /// Read the recorded `is_reset` marker (KT-log S4) for an account,
    /// or `nil` when no pin / no recorded marker. Exposed for the UI.
    /// Mirror of the web `readLogReset`.
    public func readLogReset(accountIdB64: String) -> Bool? {
        loadMap()[accountIdB64]?.isReset
    }

    /// Record the chain-confirmed transparency-log VERSION (G-30 #2,
    /// ADR-0019) for an account, keyed on the same stable `account_id`
    /// as the pin.
    ///
    /// Mirrors ``recordLogReset(accountIdB64:isReset:)``: writes onto the
    /// existing pin record so the version travels with the pinned key,
    /// and is a **no-op when no pin exists** (the supersession gate
    /// TOFU-pins on first sight, so a pin is present by the time a chain
    /// verdict is recorded). A `nil` argument CLEARS any stored version.
    /// The caller persists it ONLY on a `confirmed` rotation-chain
    /// verdict — a transient `unreachable` / `mismatch` / `absent`
    /// outcome must NOT wipe a previously-confirmed version, so the
    /// caller does not pass `nil` on those (see
    /// ``ContactsClient/checkTransparencyLog(accountId:accountIdB64:servedPubkey:oldPin:supersessionProofB64:)``).
    /// Idempotent on an unchanged value (no write). Returns whether a
    /// write happened (a present pin). Mirror of the web
    /// `recordLogVersion`.
    @discardableResult
    public func recordLogVersion(accountIdB64: String, version: UInt64?) -> Bool {
        var map = loadMap()
        guard let existing = map[accountIdB64] else { return false }
        if existing.pinnedLogVersion == version { return true } // idempotent (incl. nil==nil)
        var updated = existing
        updated.pinnedLogVersion = version
        map[accountIdB64] = updated
        saveMap(map)
        return true
    }

    /// Read the recorded chain-confirmed log version (G-30 #2) for an
    /// account, or `nil` when no pin / no recorded version. The next
    /// rotation's chain proof is checked against this as the downgrade
    /// floor (default `0` when absent). Mirror of the web
    /// `readLogVersion`.
    public func readLogVersion(accountIdB64: String) -> UInt64? {
        loadMap()[accountIdB64]?.pinnedLogVersion
    }

    /// Remove the pin for `accountIdB64`. No-op when absent. Exposed
    /// for the "forget contact" / re-verify-from-scratch flows.
    // MARK: - ADR-0065 R.2 - an accepted reset

    /// One accepted reset, as ``acceptReset(_:retiredPredecessorsB64:)`` reads it.
    public struct AcceptedReset: Equatable {
        public let accountIdB64: String
        /// The key this device held for the account (the pin, or a verified
        /// binding's key for a member it never pinned).
        public let heldKeyB64: String
        public let newKeyB64: String
        public let keyGeneration: UInt64
        public let signedAtMs: Int64
        public let nowMs: Int64

        public init(
            accountIdB64: String, heldKeyB64: String, newKeyB64: String,
            keyGeneration: UInt64, signedAtMs: Int64, nowMs: Int64
        ) {
            self.accountIdB64 = accountIdB64
            self.heldKeyB64 = heldKeyB64
            self.newKeyB64 = newKeyB64
            self.keyGeneration = keyGeneration
            self.signedAtMs = signedAtMs
            self.nowMs = nowMs
        }
    }

    public enum AcceptResetResult: Equatable {
        /// The re-pin was written.
        case accepted
        /// The pin already names the new key at this generation or above.
        case already
        /// The generation is not above the accepted one: nothing written.
        case stale
        /// No signed-in account to write under.
        case noAccount
    }

    /// The re-pin of an accepted reset (R.2 "The re-pin"): `pin = new key`,
    /// the marker never backwards, the accepted generation, and the held key
    /// (with every proven predecessor of it, which a reset supersedes too)
    /// appended to the previous and superseded keys. The log verdict and the
    /// chain floor described the old key and are cleared. Mirror of the web
    /// `acceptReset`, minus the contact row, which ``ContactsRepository``
    /// re-keys on the accepted-reset event.
    @discardableResult
    public func acceptReset(_ reset: AcceptedReset, retiredPredecessorsB64: [String] = []) -> AcceptResetResult {
        guard storageKey != nil else { return .noAccount }
        var map = loadMap()
        let existing = map[reset.accountIdB64]
        let acceptedGeneration = existing?.acceptedKeyGeneration ?? 0
        if let existing, existing.pinnedSignaturePubkeyB64 == reset.newKeyB64 {
            if acceptedGeneration >= reset.keyGeneration { return .already }
        } else if reset.keyGeneration <= acceptedGeneration {
            return .stale
        }
        var retired: [String] = []
        func retire(_ key: String) {
            if key != reset.newKeyB64, !retired.contains(key) { retired.append(key) }
        }
        retiredPredecessorsB64.forEach(retire)
        if let existing { retire(existing.pinnedSignaturePubkeyB64) }
        retire(reset.heldKeyB64)
        var previous = existing?.previousKeysB64 ?? []
        var superseded = existing?.supersededKeysB64 ?? []
        for key in retired {
            if !previous.contains(key) { previous.append(key) }
            if !superseded.contains(key) { superseded.append(key) }
        }
        map[reset.accountIdB64] = AccountPubkeyPin(
            pinnedSignaturePubkeyB64: reset.newKeyB64,
            rotationMarker: resetRepinMarker(heldMarker: existing?.rotationMarker ?? 0, nowMs: reset.nowMs),
            logStatus: nil,
            isReset: nil,
            pinnedLogVersion: nil,
            acceptedKeyGeneration: reset.keyGeneration,
            previousKeysB64: previous,
            supersededKeysB64: superseded,
            resetSignedAtMs: reset.signedAtMs)
        saveMap(map)
        return .accepted
    }

    /// Every pin of the signed-in account, by account id.
    public func allPins() -> [String: AccountPubkeyPin] {
        loadMap()
    }

    /// The generation last accepted for the account; 0 when none.
    public func acceptedGeneration(accountIdB64: String) -> UInt64 {
        loadMap()[accountIdB64]?.acceptedKeyGeneration ?? 0
    }

    /// `superseded key -> account id` over every pin: the keys accepted
    /// resets retired. Mirror of the web `loadSupersededKeys`.
    public func loadSupersededKeys() -> [String: String] {
        var out: [String: String] = [:]
        for (accountIdB64, pin) in loadMap() {
            for key in pin.supersededKeysB64 ?? [] { out[key] = accountIdB64 }
        }
        return out
    }

    public func removePin(accountIdB64: String) {
        var map = loadMap()
        guard map[accountIdB64] != nil else { return }
        map.removeValue(forKey: accountIdB64)
        saveMap(map)
    }

    /// Wipe the owning account's whole pin map (mirror of the web
    /// `clearAll`'s pin half). No-op when absent or when no account is known.
    public func clearAll() {
        guard let storageKey else { return }
        defaults.removeObject(forKey: storageKey)
    }
}
