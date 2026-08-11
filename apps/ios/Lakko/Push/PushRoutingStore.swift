// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation

/// ADR-0042 — persistence for the opaque per-group push routing id.
///
/// Mirror of `apps/web/src/push/group-routing-store.ts`.
///
/// ## What this store holds
///
/// Two things, in two locations chosen by their sensitivity:
///
///   1. **secrets** — the per-`(account, group)` `push_routing_secret`
///      (32 bytes of E2EE key material minted once by the group OWNER,
///      ADR-0042), together with a durable **`distributed`** flag saying
///      whether that secret's distribution envelope was ever confirmed on
///      the wire. It derives the opaque `push_gid`, so it is sensitive
///      and stored **device-key-encrypted at rest** — AES-256-GCM under a
///      Keychain-resident wrap key (``KeychainWrapKey``), the same
///      posture as the MLS snapshot (``IdentitySnapshotStore``) /
///      message log. The encrypted blob rides in `UserDefaults.standard`
///      (app-private): it is **foreground-only** — the NSE never needs
///      the raw secret, so it does not move into the App Group.
///
/// ## Why the `distributed` flag rides INSIDE the sealed payload
///
/// The flag is not itself secret — it could have been a plaintext
/// sibling key. It lives inside the AES-GCM payload anyway because that
/// makes secret + flag **one value written by one `UserDefaults.set`**,
/// so they can never desync. A separate key would be two writes, and a
/// crash between them leaves a stored secret whose flag row is missing —
/// indistinguishable from a legacy (pre-flag) row, which is read as
/// *already distributed*. That is exactly the permanently-undelivered
/// state the flag exists to prevent, so the atomic single-write shape is
/// the load-bearing property; the one wasted byte of ciphertext is the
/// whole cost.
///
/// ## Record layout (inside the sealed plaintext)
///
///   * **legacy** — exactly ``secretLen`` bytes, a bare secret written
///     before this flag existed. Read as `distributed == true`: those
///     groups are provisioned and working today, and re-broadcasting for
///     every one of them on upgrade would be a pointless journal write.
///   * **current** — `flag(1) ‖ secret(32)`, flag `0x00` pending /
///     `0x01` distributed.
///
/// Any other length or an unknown flag byte is unreadable → the record
/// reads as `nil` while ``hasSecret(accountIdB64:groupIdHex:)`` stays
/// true, which is the existing "stored but unreadable" state the receive
/// gate already fails closed on (invariant #4).
///
///   2. **routes** — the **`push_gid → (account, group_id)` reverse
///      index**, keyed by the lowercase-hex `push_gid`. This is the map
///      the ADR-0041 silent-push decrypt path reverse-resolves in
///      Sprint 4 (`resolveAccount(pushGidHex)` → the real `group_id` for
///      the DS fetch). It is stored **plaintext** in the App Group shared
///      `UserDefaults` suite (``SharedStorage/sharedDefaults()``): `push_gid`
///      is a one-way BLAKE3 derivation (reveals nothing) and `group_id`
///      is a public DS routing address already shared with the server —
///      exactly the rationale that keeps ``KnownGroupsStore`` unencrypted
///      in the shared suite. Plaintext is REQUIRED so the read-only NSE
///      can read it without the device key.
///
/// ## Invariants
///
///   * #2 / #5 — the reverse index maps an OPAQUE `push_gid` to a PUBLIC
///     `group_id`; no plaintext PII, no key material. The raw secret (the
///     only sensitive value) is device-key-encrypted. Nothing here ever
///     leaves the device.
///   * #7 — the secret is wrapped under the hardware-backed device wrap
///     key (`kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`).
public final class PushRoutingStore {

    /// A decrypted push-routing row: the group's `push_routing_secret` plus
    /// whether its distribution envelope was ever confirmed on the wire.
    ///
    /// `distributed == false` means the secret is durably minted locally but
    /// the group has **not** been told — every other member is still
    /// deferring its push subscription, so the owner must re-broadcast. The
    /// envelope itself is never stored: it is recomputable from the secret
    /// at any time (`composePushRoutingSecret`), so only this one bit needs
    /// to survive a relaunch.
    public struct PushRoutingRecord: Equatable {
        /// The 32-byte `push_routing_secret`.
        public let secret: Data
        /// Whether the distribution broadcast for ``secret`` succeeded.
        public let distributed: Bool

        public init(secret: Data, distributed: Bool) {
            self.secret = secret
            self.distributed = distributed
        }
    }

    /// `push_routing_secret` length (bytes) — mirrors the Rust
    /// `PUSH_ROUTING_SECRET_LEN`. Defined here (rather than reused from
    /// ``PushGroupRouting``) to keep this store FFI-clean: it is compiled
    /// into the NSE target, which does not link `LakkoCore`.
    private static let secretLen: Int = 32

    /// Record flag byte — the distribution broadcast has not been confirmed.
    private static let flagPending: UInt8 = 0x00
    /// Record flag byte — the distribution broadcast succeeded.
    private static let flagDistributed: UInt8 = 0x01

    /// Keychain service tag for the AES wrap key that seals the secret at
    /// rest. Distinct from the snapshot / message-log wrap keys — one wrap
    /// key per app installation for push-routing secrets.
    private static let wrapKeyService: String = "me.lakko.push-routing-wrap.v1"
    private static let wrapKeyAccount: String = "wrap-key"

    /// Per-`(account, group)` key prefix for the device-key-encrypted
    /// secret blob in `UserDefaults.standard`.
    private static let secretKeyPrefix: String = "me.lakko.push-routing-secret.v1."

    /// Per-`push_gid` key prefix for the plaintext reverse-index row in
    /// the App Group shared suite (NSE-readable in Sprint 4).
    private static let routeKeyPrefix: String = "me.lakko.push-route.v1."

    /// App-private location for the encrypted secret (foreground-only).
    private let secretDefaults: UserDefaults
    /// App Group shared suite for the plaintext reverse index (NSE-readable).
    /// `nil` when the App Group is not provisioned → routes are written to
    /// `secretDefaults` as a fail-open fallback (same posture as
    /// ``KnownGroupsStore``); the NSE cannot read them then, but the
    /// foreground app still routes correctly.
    private let routeDefaults: UserDefaults

    public init(
        secretDefaults: UserDefaults = .standard,
        routeDefaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) {
        self.secretDefaults = secretDefaults
        self.routeDefaults = routeDefaults ?? secretDefaults
    }

    /// Backward-compatible single-store convenience — routes both the
    /// secret and the route location to one `UserDefaults` instance. Used
    /// by tests that inject a transient suite for isolation.
    public convenience init(defaults: UserDefaults) {
        self.init(secretDefaults: defaults, routeDefaults: defaults)
    }

    // MARK: - Public API

    /// Persist a group's `push_routing_secret` + its `distributed` flag
    /// (device-key-encrypted, one atomic write) AND its
    /// `push_gid → (account, group_id)` reverse-index entry.
    /// Idempotent: re-persisting the same `(account, group)` / secret
    /// overwrites both rows in place (a no-op in effect).
    ///
    /// - Parameter distributed: whether the distribution envelope for this
    ///   secret is already known to have reached the group journal. The
    ///   owner-mint path passes `false` (nothing is on the wire yet); the
    ///   receive path passes `true` (the envelope demonstrably travelled —
    ///   that is how it got here). Deliberately has **no default**: guessing
    ///   wrong in either direction is a delivery bug, so every call site
    ///   states it.
    public func putRouting(
        accountIdB64: String,
        groupIdHex: String,
        secret: Data,
        pushGidHex: String,
        distributed: Bool
    ) {
        guard let combined = seal(
            Self.encodeRecord(secret: secret, distributed: distributed)) else { return }
        secretDefaults.set(combined, forKey: Self.secretKey(accountIdB64, groupIdHex))
        // The plaintext reverse-index row: [account, group] keyed by push_gid.
        routeDefaults.set(
            [accountIdB64, groupIdHex],
            forKey: Self.routeKey(pushGidHex)
        )
    }

    /// Read + decrypt a group's push-routing row (secret + `distributed`
    /// flag), or `nil` when this device holds none for the
    /// `(account, group)` pair (a member that has not yet received the
    /// distribution envelope). Also `nil` on an AES-GCM auth failure
    /// (wrap-key rotated / tampered ciphertext) or an uninterpretable
    /// record — the caller treats those as "no readable secret" and
    /// combines with ``hasSecret(accountIdB64:groupIdHex:)`` to tell a
    /// provable absence from an unreadable row.
    public func getRecord(accountIdB64: String, groupIdHex: String) -> PushRoutingRecord? {
        guard let combined = secretDefaults.data(
            forKey: Self.secretKey(accountIdB64, groupIdHex)) else {
            return nil
        }
        guard let keyBytes = KeychainWrapKey.read(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount) else {
            return nil
        }
        let wrapKey = SymmetricKey(data: keyBytes)
        guard let sealed = try? AES.GCM.SealedBox(combined: combined),
              let plaintext = try? AES.GCM.open(sealed, using: wrapKey) else {
            return nil
        }
        return Self.decodeRecord(plaintext)
    }

    /// Read + decrypt just a group's `push_routing_secret`. Convenience
    /// over ``getRecord(accountIdB64:groupIdHex:)`` for the callers that do
    /// not care whether the distribution broadcast landed (derive /
    /// subscribe / notify / the receive gate's "what do we already hold").
    public func getSecret(accountIdB64: String, groupIdHex: String) -> Data? {
        getRecord(accountIdB64: accountIdB64, groupIdHex: groupIdHex)?.secret
    }

    /// Mark a group's stored secret as distributed, after its distribution
    /// envelope was confirmed on the wire. Re-seals the row in place; the
    /// reverse-index row is untouched (the `push_gid` did not change).
    ///
    /// - Parameter matching: the secret that was actually broadcast. The
    ///   flag is flipped ONLY when the stored secret is byte-identical, so
    ///   the flag can never end up describing a different secret than the
    ///   one that travelled (e.g. an owner rotation that interleaved with
    ///   an in-flight broadcast).
    /// - Returns: whether the stored row is now marked distributed.
    @discardableResult
    public func markDistributed(
        accountIdB64: String,
        groupIdHex: String,
        matching secret: Data
    ) -> Bool {
        setDistributed(
            true, accountIdB64: accountIdB64, groupIdHex: groupIdHex, matching: secret)
    }

    /// Mark a group's stored secret as **pending** distribution, after a
    /// distribution broadcast for it FAILED. Re-seals the row in place (the
    /// same single atomic write as ``markDistributed(accountIdB64:groupIdHex:matching:)``);
    /// the reverse-index row is untouched (the `push_gid` did not change).
    ///
    /// This is what makes a failed **redistribute** recoverable. The
    /// `distributed` flag is per-GROUP, not per-member, so a redistribute
    /// (fired after an add-member commit, for the benefit of the ONE new
    /// member) that fails on a row the mint broadcast already settled would
    /// otherwise leave the flag `true` — no sweep would ever re-send, and
    /// that new member would defer push forever. Flipping the row back to
    /// pending puts it in **this device's**
    /// ``PushGroupRouting/ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``
    /// re-broadcast branch, which retries on every launch / chat-open until
    /// it lands. That branch is open to any secret HOLDER rather than the
    /// owner alone, because ADR-0022 lets an ADMIN add a member: the admin
    /// runs the failing redistribute, so the pending row is the ADMIN's while
    /// the owner's stays settled, and an owner-only retry would never fire.
    /// The retry is byte-identical, so the ADR-0042 receive gate
    /// reads it as an idempotent no-op re-persist for every member that
    /// already holds it, and as the seed the new member needs.
    ///
    /// - Parameter matching: the secret whose broadcast failed. The flag is
    ///   flipped ONLY when the stored secret is byte-identical, so a failed
    ///   OLD-secret broadcast can never un-settle a NEWLY rotated secret
    ///   that did travel.
    /// - Returns: whether the stored row is now marked pending. `false`
    ///   (a no-op) when no row is stored, the stored secret differs, or the
    ///   re-seal failed.
    @discardableResult
    public func markPending(
        accountIdB64: String,
        groupIdHex: String,
        matching secret: Data
    ) -> Bool {
        setDistributed(
            false, accountIdB64: accountIdB64, groupIdHex: groupIdHex, matching: secret)
    }

    /// Shared body of ``markDistributed(accountIdB64:groupIdHex:matching:)``
    /// and ``markPending(accountIdB64:groupIdHex:matching:)``: re-seal the row
    /// with a new flag value in ONE `UserDefaults.set`, so secret + flag can
    /// never desync (the load-bearing single-write property documented on the
    /// type). A no-op when the row is absent or holds a different secret.
    private func setDistributed(
        _ distributed: Bool,
        accountIdB64: String,
        groupIdHex: String,
        matching secret: Data
    ) -> Bool {
        guard let record = getRecord(accountIdB64: accountIdB64, groupIdHex: groupIdHex),
              record.secret == secret else {
            return false
        }
        // Already in the requested state → skip the pointless re-encrypt + write.
        if record.distributed == distributed { return true }
        guard let combined = seal(
            Self.encodeRecord(secret: record.secret, distributed: distributed)) else {
            return false
        }
        secretDefaults.set(combined, forKey: Self.secretKey(accountIdB64, groupIdHex))
        return true
    }

    /// Cheap existence check — does this device already hold a
    /// `push_routing_secret` for the `(account, group)` pair? No device
    /// key needed (only checks the key's presence), so it is the guard the
    /// owner uses to avoid re-minting a secret it already provisioned.
    public func hasSecret(accountIdB64: String, groupIdHex: String) -> Bool {
        secretDefaults.object(forKey: Self.secretKey(accountIdB64, groupIdHex)) != nil
    }

    /// Reverse-map a `push_gid` (lowercase hex) to its owning
    /// `(account, group_id)`, or `nil` when unknown. This is the read the
    /// ADR-0041 silent-push decrypt path performs in Sprint 4 to turn an
    /// opaque wake `g` back into the real DS `group_id`. Reads the App
    /// Group shared suite first, then the app-private fallback.
    public func lookupRoute(pushGidHex: String)
        -> (accountIdB64: String, groupIdHex: String)? {
        let key = Self.routeKey(pushGidHex)
        for defaults in orderedRouteDefaults() {
            guard let pair = defaults.stringArray(forKey: key),
                  pair.count == 2 else { continue }
            return (accountIdB64: pair[0], groupIdHex: pair[1])
        }
        return nil
    }

    /// Wipe every push-routing secret + reverse-index entry for
    /// `accountIdB64`. Called from ``ChatRepository/forgetDevice()`` so a
    /// device wipe leaves no push-routing residue (mirror of the web
    /// `clearAllGroupRouting`). Enumerates the secret + route keyspaces
    /// and removes only the rows belonging to the account (routes are
    /// keyed by `push_gid`, so the owning account is read from each row).
    public func clear(accountIdB64: String) {
        // secrets — keyed `<prefix><account> <group>`, so a prefix match
        // is exact for this account.
        let secretAccountPrefix = Self.secretKeyPrefix + accountIdB64 + " "
        for (key, _) in secretDefaults.dictionaryRepresentation()
        where key.hasPrefix(secretAccountPrefix) {
            secretDefaults.removeObject(forKey: key)
        }
        // routes — keyed by `push_gid`, so match on the stored account.
        for defaults in orderedRouteDefaults() {
            for (key, _) in defaults.dictionaryRepresentation()
            where key.hasPrefix(Self.routeKeyPrefix) {
                guard let pair = defaults.stringArray(forKey: key),
                      pair.first == accountIdB64 else { continue }
                defaults.removeObject(forKey: key)
            }
        }
    }

    // MARK: - Test seam

    /// Seal + store a RAW record plaintext, bypassing ``encodeRecord``.
    ///
    /// Test-only (`internal`, reachable via `@testable import`), never
    /// called from production code. It exists so a test can reproduce the
    /// LEGACY pre-flag on-disk shape (a bare secret with no flag byte) and
    /// an unreadable row without duplicating the private key shape + wrap-key
    /// service tag, which would silently rot if either changed.
    func putRawRecordForTesting(
        accountIdB64: String,
        groupIdHex: String,
        plaintext: Data
    ) {
        guard let combined = seal(plaintext) else { return }
        secretDefaults.set(combined, forKey: Self.secretKey(accountIdB64, groupIdHex))
    }

    // MARK: - Record codec + sealing

    /// Seal a record plaintext under the Keychain-resident device wrap key.
    /// `combined` lays out `nonce || ciphertext || tag` per RFC 5116 §3.1 —
    /// the same on-disk envelope shape ``IdentitySnapshotStore`` produces.
    /// `nil` on a seal failure, which leaves the caller's rows unwritten
    /// (a missing secret reads as "not provisioned" and defers).
    private func seal(_ plaintext: Data) -> Data? {
        let wrapKey = SymmetricKey(data: KeychainWrapKey.loadOrCreate(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount))
        return try? AES.GCM.seal(plaintext, using: wrapKey).combined
    }

    /// `flag(1) ‖ secret(32)`.
    private static func encodeRecord(secret: Data, distributed: Bool) -> Data {
        var out = Data([distributed ? flagDistributed : flagPending])
        out.append(secret)
        return out
    }

    /// Parse a sealed record plaintext. A bare ``secretLen``-byte payload is
    /// a LEGACY row written before the flag existed: it is read as
    /// **distributed**, so an upgrade does not re-broadcast for every group
    /// that already works. Anything else unrecognised → `nil` (unreadable,
    /// fail-closed) rather than a guessed flag.
    private static func decodeRecord(_ plaintext: Data) -> PushRoutingRecord? {
        if plaintext.count == secretLen {
            return PushRoutingRecord(secret: plaintext, distributed: true)
        }
        guard plaintext.count == secretLen + 1, let flag = plaintext.first else { return nil }
        let secret = Data(plaintext.dropFirst())
        switch flag {
        case flagPending: return PushRoutingRecord(secret: secret, distributed: false)
        case flagDistributed: return PushRoutingRecord(secret: secret, distributed: true)
        default: return nil
        }
    }

    // MARK: - Key shapes

    /// Composite secret key. `accountIdB64` (base64) never contains a
    /// space, so the space separator to `groupIdHex` (hex) is unambiguous
    /// (same separator shape as the web `secretRowKey`).
    private static func secretKey(_ accountIdB64: String, _ groupIdHex: String) -> String {
        secretKeyPrefix + accountIdB64 + " " + groupIdHex
    }

    private static func routeKey(_ pushGidHex: String) -> String {
        routeKeyPrefix + pushGidHex
    }

    /// Ordered route read/clear locations: the shared suite first, then
    /// the app-private fallback — de-duplicated when they are the same
    /// instance (the unprovisioned / test convenience case).
    private func orderedRouteDefaults() -> [UserDefaults] {
        routeDefaults === secretDefaults ? [routeDefaults] : [routeDefaults, secretDefaults]
    }
}
