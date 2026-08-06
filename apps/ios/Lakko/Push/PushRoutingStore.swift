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
///      ADR-0042). It derives the opaque `push_gid`, so it is sensitive
///      and stored **device-key-encrypted at rest** — AES-256-GCM under a
///      Keychain-resident wrap key (``KeychainWrapKey``), the same
///      posture as the MLS snapshot (``IdentitySnapshotStore``) /
///      message log. The encrypted blob rides in `UserDefaults.standard`
///      (app-private): it is **foreground-only** — the NSE never needs
///      the raw secret, so it does not move into the App Group.
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

    /// Persist a group's `push_routing_secret` (device-key-encrypted) AND
    /// its `push_gid → (account, group_id)` reverse-index entry.
    /// Idempotent: re-persisting the same `(account, group)` / secret
    /// overwrites both rows in place (a no-op in effect).
    public func putRouting(
        accountIdB64: String,
        groupIdHex: String,
        secret: Data,
        pushGidHex: String
    ) {
        let wrapKey = SymmetricKey(data: KeychainWrapKey.loadOrCreate(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount))
        // `combined` lays out `nonce || ciphertext || tag` per RFC 5116
        // §3.1 — same on-disk envelope shape ``IdentitySnapshotStore``
        // produces. A seal failure leaves both rows unwritten (the caller
        // treats a missing secret as "not provisioned" and defers).
        guard let combined = try? AES.GCM.seal(secret, using: wrapKey).combined else { return }
        secretDefaults.set(combined, forKey: Self.secretKey(accountIdB64, groupIdHex))
        // The plaintext reverse-index row: [account, group] keyed by push_gid.
        routeDefaults.set(
            [accountIdB64, groupIdHex],
            forKey: Self.routeKey(pushGidHex)
        )
    }

    /// Read + decrypt a group's `push_routing_secret`, or `nil` when this
    /// device holds none for the `(account, group)` pair (a member that
    /// has not yet received the distribution envelope). Returns `nil` on
    /// an AES-GCM auth failure (wrap-key rotated / tampered ciphertext) —
    /// the caller treats that as "no secret held" and defers.
    public func getSecret(accountIdB64: String, groupIdHex: String) -> Data? {
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
        return plaintext
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
