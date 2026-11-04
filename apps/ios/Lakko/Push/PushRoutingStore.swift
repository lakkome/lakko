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
/// ## The `mintEpoch` field — ADR-0044, durable state, NOT a wire field
///
/// The v2 distribution envelope carries an 8-byte `mint_epoch` the minter
/// stamps from `Group.epoch()` at MINT time, and the receive gate orders
/// envelopes by the pair `(mintEpoch, secretBytes)`. That order is only
/// sound if the stamp **travels with the secret for that secret's whole
/// life**: every re-broadcast recomposes the envelope from the stored
/// secret, so it must replay the epoch the secret was MINTED under, never
/// the current one. A re-stamp at send time would make a stale secret
/// out-rank the live one and reproduce the very F-A replay the order
/// exists to close — with the ordering gate fully in place and working as
/// specified.
///
/// So the epoch is durable per-`(account, group)` state and lives here,
/// inside the same sealed single-write record as the secret + flag. It is
/// a monotone counter, not key material (invariant #2 — nothing here is
/// secret except the AEAD-wrapped secret itself), so sealing it is a
/// consequence of the single-write property, not a confidentiality need.
///
/// ## Record layout (inside the sealed plaintext)
///
///   * **legacy, pre-flag** — exactly ``secretLen`` bytes, a bare secret
///     written before the `distributed` flag existed. Read as
///     `distributed == true` (those groups are provisioned and working
///     today, and re-broadcasting for every one of them on upgrade would
///     be a pointless journal write) and `mintEpoch == 0`.
///   * **legacy, pre-epoch (ADR-0042 / ADR-0043)** — `flag(1) ‖
///     secret(32)`. Read with its recorded flag and `mintEpoch == 0`.
///   * **current (ADR-0044)** — `flag(1) ‖ mintEpoch(8, BE) ‖ secret(32)`,
///     flag `0x00` pending / `0x01` distributed.
///
/// A legacy row reads as epoch **0**, the LOWEST position in the canonical
/// order, so any authorised stamped mint supersedes a legacy secret —
/// the direction that converges. The inverse default (a high epoch) would
/// let an unstamped legacy secret out-rank every future rotation.
///
/// Any other length or an unknown flag byte is unreadable → the record
/// reads as `nil` while ``hasSecret(accountIdB64:groupIdHex:)`` stays
/// true, which is the existing "stored but unreadable" state the receive
/// gate already fails closed on (invariant #4).
///
///   2. **routes** — the **`(account, push_gid) → group_id` reverse
///      index**. This is the map the ADR-0041 silent-push decrypt path
///      reads to turn a wake's opaque `g` back into the real `group_id` for
///      the DS fetch. It is stored **plaintext** in the App Group shared
///      `UserDefaults` suite (``SharedStorage/sharedDefaults()``): `push_gid`
///      is a one-way BLAKE3 derivation (reveals nothing) and `group_id`
///      is a public DS routing address already shared with the server —
///      exactly the rationale that keeps ``KnownGroupsStore`` unencrypted
///      in the shared suite. Plaintext is REQUIRED so the read-only NSE
///      can read it without the device key.
///
/// ## Why a route is keyed by account as well (amended 2026-09-30)
///
/// Two accounts on one device that are members of one group derive the SAME
/// `push_gid`, so a row keyed by `push_gid` alone was simply the last writer's,
/// and a background path reading it decrypted as whichever account wrote last.
/// ADR-0042's amendment makes the background paths handle a wake only for the
/// account the device pseudonym is bound to (``PushAccountBinding``), so the
/// index is looked up for `(bound account, push_gid)` and a row belonging to
/// another account is a miss. Deletes are scoped the same way. The previous
/// `me.lakko.push-route.v1.` rows are no longer read.
///
/// The two age differently, which ADR-0043 makes load-bearing: a rotation
/// OVERWRITES the secret row in place (same `(account, group)` key) but only
/// ADDS to the routes (keyed by the freshly derived `pushGidHex`). The
/// superseded route entry is therefore retired explicitly, via
/// ``removeRoute(accountIdB64:pushGidHex:)``.
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
        /// ADR-0044 — the MLS group epoch ``secret`` was MINTED under,
        /// replayed verbatim by every re-broadcast and compared by the
        /// receive gate's canonical order. `0` for a legacy row written
        /// before this field existed (the lowest position in the order).
        ///
        /// Read as part of the record, never separately: the secret and its
        /// stamp are ONE value. A torn read could pair a superseded secret
        /// with a live epoch, which is precisely the F-A shape the order
        /// exists to refuse.
        public let mintEpoch: UInt64

        public init(secret: Data, distributed: Bool, mintEpoch: UInt64) {
            self.secret = secret
            self.distributed = distributed
            self.mintEpoch = mintEpoch
        }
    }

    /// `push_routing_secret` length (bytes) — mirrors the Rust
    /// `PUSH_ROUTING_SECRET_LEN`. Defined here (rather than reused from
    /// ``PushGroupRouting``) to keep this store FFI-clean: it is compiled
    /// into the NSE target, which does not link `LakkoCore`.
    private static let secretLen: Int = 32

    /// ADR-0044 — byte length of the big-endian `mintEpoch` field, mirroring
    /// the Rust `PUSH_ROUTING_SECRET_MINT_EPOCH_LEN`.
    private static let mintEpochLen: Int = 8

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

    /// Per-`(account, push_gid)` key prefix for the plaintext reverse-index
    /// row in the App Group shared suite (NSE-readable). v2: the v1 rows were
    /// keyed by `push_gid` alone and are no longer read.
    private static let routeKeyPrefix: String = "me.lakko.push-route.v2."

    /// ADR-0078 — per-`(account, push_gid)` key prefix for this device's
    /// push-gw subscription id (32 lowercase hex characters), in the same
    /// NSE-readable suite as the route rows. A wake names the id as `s`, and
    /// a background path handles it only when the bound account's row for the
    /// wake's `push_gid` holds that id; the device removes its own push-gw row
    /// by it. No device pseudonym exists any more.
    private static let subKeyPrefix: String = "me.lakko.push-sub.v1."

    /// Length of a subscription id in bytes.
    public static let subscriptionIdLength: Int = 16
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
    /// `(account, push_gid) → group_id` reverse-index entry.
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
    /// - Parameter mintEpoch: ADR-0044 — the epoch this secret was MINTED
    ///   under. On a mint / rotation that is `Group.epoch()` read at mint
    ///   time; on a RECEIVE it is the epoch carried by the accepted
    ///   envelope, NOT the receiver's current epoch — the stamp identifies
    ///   the secret, not the moment it was applied. Also has no default,
    ///   for the same reason: a guessed stamp silently reorders the group.
    public func putRouting(
        accountIdB64: String,
        groupIdHex: String,
        secret: Data,
        pushGidHex: String,
        distributed: Bool,
        mintEpoch: UInt64
    ) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        guard let combined = seal(Self.encodeRecord(
            secret: secret, distributed: distributed, mintEpoch: mintEpoch)) else { return }
        secretDefaults.set(combined, forKey: Self.secretKey(accountIdB64, groupIdHex))
        // The plaintext reverse-index row: group_id keyed by (account, push_gid).
        routeDefaults.set(
            groupIdHex,
            forKey: Self.routeKey(accountIdB64, pushGidHex)
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
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return false }
        defer { gate.leave() }
        guard let record = getRecord(accountIdB64: accountIdB64, groupIdHex: groupIdHex),
              record.secret == secret else {
            return false
        }
        // Already in the requested state → skip the pointless re-encrypt + write.
        if record.distributed == distributed { return true }
        // ADR-0044 — re-seal with the STORED `mintEpoch`, never a fresh one.
        // This path only records whether the envelope reached the wire; the
        // secret's identity, stamp included, is untouched. Re-stamping here
        // would let a settle/un-settle bookkeeping write silently promote a
        // secret's position in the total order.
        guard let combined = seal(Self.encodeRecord(
            secret: record.secret,
            distributed: distributed,
            mintEpoch: record.mintEpoch)) else {
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

    /// Reverse-map a `push_gid` (lowercase hex) to the real `group_id` it
    /// routes for `accountIdB64`, or `nil` when this account holds no row for
    /// it. A row another account wrote for the same `push_gid` is a miss.
    /// This is the read the ADR-0041 silent-push paths perform, always for
    /// the account the wake is bound to (``PushAccountBinding``). Reads the
    /// App Group shared suite first, then the app-private fallback.
    public func lookupRoute(accountIdB64: String, pushGidHex: String) -> String? {
        let key = Self.routeKey(accountIdB64, pushGidHex.lowercased())
        for defaults in orderedRouteDefaults() {
            guard let groupIdHex = defaults.string(forKey: key), !groupIdHex.isEmpty else {
                continue
            }
            return groupIdHex
        }
        return nil
    }

    /// ADR-0078 — the reverse map for a WAKE: the `group_id` the bound
    /// account's row for `push_gid` routes, but only when this device's
    /// subscription id for that row equals the wake's `s`. A wake for a
    /// subscription this account does not hold (another account that used
    /// the device, a stale row) is a miss.
    public func lookupRoute(
        accountIdB64: String,
        pushGidHex: String,
        subscriptionIdHex: String?
    ) -> String? {
        guard let presented = subscriptionIdHex.flatMap(PushAccountBinding.normalizedSubscriptionIdHex),
              let held = subscriptionId(accountIdB64: accountIdB64, pushGidHex: pushGidHex),
              PushAccountBinding.hex(held) == presented else {
            return nil
        }
        return lookupRoute(accountIdB64: accountIdB64, pushGidHex: pushGidHex)
    }

    /// ADR-0078 — this device's subscription id for `(account, push_gid)`, or
    /// `nil` when it never subscribed under it.
    public func subscriptionId(accountIdB64: String, pushGidHex: String) -> Data? {
        let key = Self.subKey(accountIdB64, pushGidHex.lowercased())
        for defaults in orderedRouteDefaults() {
            guard let hex = defaults.string(forKey: key),
                  let normal = PushAccountBinding.normalizedSubscriptionIdHex(hex) else { continue }
            return Self.bytes(fromHex: normal)
        }
        return nil
    }

    /// ADR-0078 — this device's subscription id for `(account, push_gid)`,
    /// minting and recording 16 random bytes when none is held.
    public func ensureSubscriptionId(accountIdB64: String, pushGidHex: String) -> Data {
        if let held = subscriptionId(accountIdB64: accountIdB64, pushGidHex: pushGidHex) {
            return held
        }
        var rng = SystemRandomNumberGenerator()
        let fresh = Data((0..<Self.subscriptionIdLength).map { _ in UInt8.random(in: 0...255, using: &rng) })
        if let gate = AccountStoreGate.enter(accountIdB64) {
            routeDefaults.set(
                PushAccountBinding.hex(fresh),
                forKey: Self.subKey(accountIdB64, pushGidHex.lowercased()))
            gate.leave()
        }
        return fresh
    }

    /// Every `(push_gid, group_id)` route row of `accountIdB64` (re-sending
    /// every subscription after a token change).
    public func routes(accountIdB64: String) -> [(pushGidHex: String, groupIdHex: String)] {
        let prefix = Self.routeKeyPrefix + accountIdB64 + " "
        var seen: Set<String> = []
        var out: [(pushGidHex: String, groupIdHex: String)] = []
        for defaults in orderedRouteDefaults() {
            for (key, value) in defaults.dictionaryRepresentation() where key.hasPrefix(prefix) {
                let gid = String(key.dropFirst(prefix.count))
                guard let groupIdHex = value as? String, !groupIdHex.isEmpty,
                      seen.insert(gid).inserted else { continue }
                out.append((pushGidHex: gid, groupIdHex: groupIdHex))
            }
        }
        return out
    }

    /// Every subscription id `accountIdB64` holds on this device (removing
    /// the account's push-gw rows at sign-out). Must be read BEFORE
    /// ``clear(accountIdB64:)``.
    public func subscriptionIds(accountIdB64: String) -> [Data] {
        let prefix = Self.subKeyPrefix + accountIdB64 + " "
        var seen: Set<String> = []
        var out: [Data] = []
        for defaults in orderedRouteDefaults() {
            for (key, value) in defaults.dictionaryRepresentation() where key.hasPrefix(prefix) {
                guard let hex = (value as? String).flatMap(PushAccountBinding.normalizedSubscriptionIdHex),
                      seen.insert(hex).inserted else { continue }
                out.append(Self.bytes(fromHex: hex))
            }
        }
        return out
    }

    /// Delete ONE reverse-index entry: `accountIdB64`'s row for `push_gid`
    /// (lowercase hex). Another account's row for the same `push_gid` stays.
    ///
    /// ADR-0043. The asymmetry this closes: the secret row is keyed
    /// `(account, group)` and is **overwritten in place** by a rotation, but
    /// a route row is keyed by `push_gid`, so a rotation only ever **adds**
    /// a row — the superseded entry survives forever. Left behind, each
    /// rotation grows the reverse map by one dead entry whose `push_gid`
    /// nothing will ever notify again.
    ///
    /// Removes from BOTH ordered route locations (the App Group shared suite
    /// AND the app-private fallback), because
    /// ``lookupRoute(accountIdB64:pushGidHex:)`` reads both — deleting from
    /// only the shared suite would leave a fallback row that still resolves.
    ///
    /// A no-op when the row is absent (`removeObject` on an unknown key
    /// succeeds), so the caller never has to check first — a rotation on a
    /// device that had not yet indexed the old gid is simply a no-op.
    ///
    /// Scoped to the ONE key: never touches the secret store, so it can never
    /// strand a group by deleting the material a live `push_gid` is derived
    /// from.
    public func removeRoute(accountIdB64: String, pushGidHex: String) {
        let key = Self.routeKey(accountIdB64, pushGidHex.lowercased())
        let sub = Self.subKey(accountIdB64, pushGidHex.lowercased())
        for defaults in orderedRouteDefaults() {
            defaults.removeObject(forKey: key)
            defaults.removeObject(forKey: sub)
        }
    }

    /// Drop one group's secret row (ADR-0087 section 9): the group is gone
    /// from this device, and a re-add receives a secret of its own.
    public func removeSecret(accountIdB64: String, groupIdHex: String) {
        secretDefaults.removeObject(forKey: Self.secretKey(accountIdB64, groupIdHex))
    }

    /// Wipe every push-routing secret + reverse-index entry for
    /// `accountIdB64`. Called from ``AccountLocalDataWipe`` so a
    /// device wipe leaves no push-routing residue (mirror of the web
    /// `clearAllGroupRouting`). Both keyspaces are keyed by account first,
    /// so a prefix match removes exactly this account's rows.
    public func clear(accountIdB64: String) {
        // secrets — keyed `<prefix><account> <group>`, so a prefix match
        // is exact for this account.
        let secretAccountPrefix = Self.secretKeyPrefix + accountIdB64 + " "
        for (key, _) in secretDefaults.dictionaryRepresentation()
        where key.hasPrefix(secretAccountPrefix) {
            secretDefaults.removeObject(forKey: key)
        }
        // routes and subscription ids — keyed `<prefix><account> <push_gid>`,
        // same prefix match.
        let routeAccountPrefix = Self.routeKeyPrefix + accountIdB64 + " "
        let subAccountPrefix = Self.subKeyPrefix + accountIdB64 + " "
        for defaults in orderedRouteDefaults() {
            for (key, _) in defaults.dictionaryRepresentation()
            where key.hasPrefix(routeAccountPrefix) || key.hasPrefix(subAccountPrefix) {
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

    /// `flag(1) ‖ mintEpoch(8, BE) ‖ secret(32)` — ADR-0044.
    ///
    /// Big-endian, matching the wire encoding the Rust core pins, so the
    /// stored stamp and the transmitted one are the same number written the
    /// same way and a store round-trip can never reorder a group.
    private static func encodeRecord(
        secret: Data,
        distributed: Bool,
        mintEpoch: UInt64
    ) -> Data {
        var out = Data([distributed ? flagDistributed : flagPending])
        out.append(contentsOf: withUnsafeBytes(of: mintEpoch.bigEndian) { Array($0) })
        out.append(secret)
        return out
    }

    /// Parse a sealed record plaintext, newest layout first.
    ///
    ///   * `flag(1) ‖ mintEpoch(8, BE) ‖ secret(32)` — the ADR-0044 record.
    ///   * `flag(1) ‖ secret(32)` — a LEGACY row from ADR-0042 / ADR-0043,
    ///     read with its recorded flag and `mintEpoch == 0`.
    ///   * a bare ``secretLen``-byte payload — a LEGACY row from before the
    ///     flag existed, read as **distributed** (so an upgrade does not
    ///     re-broadcast for every group that already works) and
    ///     `mintEpoch == 0`.
    ///
    /// Epoch `0` is the LOWEST position in the canonical order, so a legacy
    /// secret is superseded by the first authorised stamped mint rather than
    /// out-ranking every future rotation.
    ///
    /// Anything else unrecognised → `nil` (unreadable, fail-closed) rather
    /// than a guessed flag or a guessed stamp.
    private static func decodeRecord(_ plaintext: Data) -> PushRoutingRecord? {
        // Legacy, pre-flag: a bare secret.
        if plaintext.count == secretLen {
            return PushRoutingRecord(secret: plaintext, distributed: true, mintEpoch: 0)
        }
        guard let flag = plaintext.first, let distributed = flagValue(flag) else { return nil }
        let body = plaintext.dropFirst()
        // Legacy, pre-epoch: flag + secret, no stamp.
        if body.count == secretLen {
            return PushRoutingRecord(
                secret: Data(body), distributed: distributed, mintEpoch: 0)
        }
        // Current: flag + big-endian epoch + secret.
        guard body.count == mintEpochLen + secretLen else { return nil }
        let epochBytes = body.prefix(mintEpochLen)
        let epoch = epochBytes.reduce(UInt64(0)) { ($0 << 8) | UInt64($1) }
        return PushRoutingRecord(
            secret: Data(body.dropFirst(mintEpochLen)),
            distributed: distributed,
            mintEpoch: epoch
        )
    }

    /// Interpret a record flag byte; `nil` for an unknown value (fail-closed
    /// — the row reads as unreadable rather than defaulting a flag).
    private static func flagValue(_ flag: UInt8) -> Bool? {
        switch flag {
        case flagPending: return false
        case flagDistributed: return true
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

    /// Composite route key, the same space-separated shape as
    /// ``secretKey(_:_:)``: a base64 account id never contains a space.
    private static func routeKey(_ accountIdB64: String, _ pushGidHex: String) -> String {
        routeKeyPrefix + accountIdB64 + " " + pushGidHex
    }

    /// Composite subscription-id key, the same shape as ``routeKey(_:_:)``.
    private static func subKey(_ accountIdB64: String, _ pushGidHex: String) -> String {
        subKeyPrefix + accountIdB64 + " " + pushGidHex
    }

    /// Bytes of an even-length lowercase hex string (already validated).
    private static func bytes(fromHex hex: String) -> Data {
        var out = Data(capacity: hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            out.append(UInt8(hex[index..<next], radix: 16) ?? 0)
            index = next
        }
        return out
    }

    /// Ordered route read/clear locations: the shared suite first, then
    /// the app-private fallback — de-duplicated when they are the same
    /// instance (the unprovisioned / test convenience case).
    private func orderedRouteDefaults() -> [UserDefaults] {
        routeDefaults === secretDefaults ? [routeDefaults] : [routeDefaults, secretDefaults]
    }
}
