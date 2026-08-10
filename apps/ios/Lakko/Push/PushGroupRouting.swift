// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import Security

/// ADR-0042 orchestration — mint / receive / derive / subscribe / notify
/// the opaque per-group push routing id (`push_gid`), closing the
/// invariant-#5 push social-graph leak.
///
/// Mirror of `apps/web/src/push/group-routing.ts`.
///
/// ## The load-bearing rule: OWNER mints, everyone else RECEIVES
///
/// `push_routing_secret` is 32 RANDOM bytes, so two members minting
/// independently would diverge and break routing (their `push_gid`s would
/// differ and the notify JOIN would never match). Therefore exactly ONE
/// source mints:
///
///   * Only the ADR-0022 group **OWNER** mints, once, and persists it
///     (``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``).
///     It then broadcasts the secret to the group over an E2EE MLS
///     application message (the DS sees ciphertext — invariant #2).
///   * Everyone else ONLY receives + persists the secret from that
///     envelope
///     (``receiveSecret(accountIdB64:groupIdHex:secret:group:senderLeaf:)``)
///     — they never mint.
///
/// The receive side enforces the same rule rather than trusting it: a
/// *changed* secret is accepted only when the MLS-authenticated sender leaf
/// resolves to a signature key the ADR-0022 role table authorises
/// (`lakko-security` MEDIUM-1). Otherwise any single member could
/// unilaterally rotate the group's routing id and strand an orphan push-gw
/// subscription per churn.
///
/// The authorised set is **owner + admins, with the owner taking
/// priority** — expressed as *who may overwrite what*, so no authority
/// level is ever persisted:
///
///   * the OWNER may always set the secret (rotate it, whatever is
///     stored and in whatever arrival order);
///   * an ADMIN may only **seed a hole** — set the FIRST-ever secret this
///     device holds — never overwrite an existing one;
///   * a plain member may only re-send the byte-identical value.
///
/// Admitting admins as seeders is what keeps delivery working: ADR-0022
/// lets owner OR admin add a member, and `redistribute` fires after any
/// add-member commit, so a member added by a non-owner admin learns the
/// secret from that admin's re-broadcast (the owner only broadcasts at
/// mint time). Denying admins the *overwrite* is what keeps the rotation
/// power owner-only.
/// See ``classifyIncomingSecret(incoming:stored:senderKey:authority:)``.
///
/// ## Never subscribe under the leaky `group_id`
///
/// The whole point of the ADR is that push-gw + APNs/FCM must not see the
/// DS journal key. So a member that does NOT yet hold the secret
/// **defers** its push subscription (transient no-push until the secret
/// arrives) — it never falls back to subscribing under `group_id`
/// (``subscribeUnderPushGidBestEffort(accountIdB64:groupIdHex:)`` is a no-op
/// when no secret is held). The publisher always holds the secret (it is a
/// member), so notify always has a `push_gid`.
///
/// ## Concurrency
///
/// Provisioning can be triggered from two places in the same session — the
/// legacy-sweep on bootstrap and the chat view's onAppear. A per-group
/// in-flight lock (plus the synchronous mint→persist, which flips
/// ``PushRoutingStore/hasSecret(accountIdB64:groupIdHex:)`` true before the
/// first `await`) coalesces them so the random secret is minted exactly
/// once (a second concurrent mint would overwrite + double-broadcast a
/// DIFFERENT secret).
///
/// `@MainActor` so the owner-role check + the E2EE `broadcast` (which runs
/// `Group.createApplicationMessage` in ``ChatRepository``) stay on the same
/// actor that owns the MLS `Group` handle.
@MainActor
public final class PushGroupRouting {

    /// `push_routing_secret` length (bytes) — mirrors the Rust
    /// `PUSH_ROUTING_SECRET_LEN`.
    public static let secretLen: Int = 32

    /// Total distribution-envelope length (bytes): the 29-byte domain
    /// separator + 1 version byte + the 32-byte secret. Used only as a
    /// cheap receive-side pre-filter before the authoritative FFI decode.
    /// Mirrors `lakko_contacts::push_routing` (29 + 1 + 32).
    public static let envelopeLen: Int = 29 + 1 + secretLen

    /// First byte of the envelope domain separator (`'l'` of
    /// `"lakko/push-routing-secret\0"`). The cheap pre-filter; the FFI
    /// `decodePushRoutingSecret` is the authoritative fail-closed check.
    private static let envelopeFirstByte: UInt8 = 0x6c

    private let store: PushRoutingStore

    /// Subscribe seam — `(push_gid) -> ()`. Defaults to the real push-gw
    /// call (device pseudonym from the Keychain + ``PushClient``);
    /// injectable so tests can spy on the target without touching the
    /// network (mirror of the web `PushClient` `vi.mock`).
    private let subscribeUnderPushGid: (Data) async throws -> Void

    /// Per-group in-flight provisioning keys, so a concurrent legacy-sweep
    /// + onAppear for the same group mint the random secret exactly once.
    private var inflight: Set<String> = []

    /// `nonisolated` so the initializer is callable as a default argument
    /// from a nonisolated context (``ChatRepository/init``); it only assigns
    /// stored properties.
    public nonisolated init(
        store: PushRoutingStore = PushRoutingStore(),
        subscribeUnderPushGid: @escaping (Data) async throws -> Void = { pushGid in
            let devicePseudonym = try DevicePushId.getOrCreate()
            _ = try await PushClient().subscribeGroup(
                devicePseudonym: devicePseudonym,
                groupPseudonym: pushGid
            )
        }
    ) {
        self.store = store
        self.subscribeUnderPushGid = subscribeUnderPushGid
    }

    // MARK: - Primitives (mirror of group-routing.ts helpers)

    /// Mint a fresh `push_routing_secret` (32 CSPRNG bytes). OWNER ONLY —
    /// everyone else receives it over the distribution envelope.
    public nonisolated static func mintSecret() -> Data? {
        var bytes = Data(count: secretLen)
        let status = bytes.withUnsafeMutableBytes { ptr -> Int32 in
            guard let base = ptr.baseAddress else { return errSecAllocate }
            return SecRandomCopyBytes(kSecRandomDefault, secretLen, base)
        }
        return status == errSecSuccess ? bytes : nil
    }

    /// Derive the stable opaque `push_gid` (32 bytes) from a secret via the
    /// audited FFI keyed-BLAKE3 derivation. Deterministic: every member
    /// that holds the same secret derives the same `push_gid`.
    public nonisolated static func derivePushGid(secret: Data) -> Data? {
        try? LakkoCore.derivePushGid(secret: secret)
    }

    /// Compose the E2EE distribution-envelope bytes for a secret (FFI).
    public nonisolated static func composeSecretEnvelope(secret: Data) throws -> Data {
        try composePushRoutingSecret(secret: secret)
    }

    /// Attempt to decode an MLS application-message plaintext as a
    /// push-routing-secret distribution envelope. Returns the 32-byte
    /// secret, or `nil` when the bytes are not this envelope (fail-closed
    /// — the authoritative check is the FFI codec's domain + version +
    /// exact-length gate). A cheap length/first-byte pre-filter avoids an
    /// FFI call on every chat message.
    public nonisolated static func tryDecodeSecretEnvelope(_ bytes: Data) -> Data? {
        guard bytes.count == envelopeLen, bytes.first == envelopeFirstByte else {
            return nil
        }
        // Not a valid push-routing envelope (domain / version / length
        // mismatch) → `nil` so the caller runs the normal chat parse.
        return try? decodePushRoutingSecret(plaintext: bytes)
    }

    // MARK: - Persist + resolve

    /// Persist a group's secret (device-key-encrypted) AND its
    /// `push_gid → (account, group_id)` reverse-index entry. Idempotent.
    /// Returns the derived `push_gid` bytes, or `nil` when the derivation
    /// fails (should not happen for a 32-byte secret).
    @discardableResult
    public func persistSecret(
        accountIdB64: String,
        groupIdHex: String,
        secret: Data
    ) -> Data? {
        guard let pushGid = Self.derivePushGid(secret: secret) else { return nil }
        store.putRouting(
            accountIdB64: accountIdB64,
            groupIdHex: groupIdHex,
            secret: secret,
            pushGidHex: Self.hex(pushGid)
        )
        return pushGid
    }

    /// Look up the `push_gid` bytes for a group, or `nil` when this device
    /// does not (yet) hold the secret. Used by the notify path (publisher)
    /// and the subscribe path.
    public func pushGid(accountIdB64: String, groupIdHex: String) -> Data? {
        guard let secret = store.getSecret(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex) else {
            return nil
        }
        return Self.derivePushGid(secret: secret)
    }

    /// Reverse-map a `push_gid` (lowercase hex) to `(account, group_id)`.
    /// Exposed for the Sprint-4 NSE decrypt reverse-map (Sprint 3 only
    /// POPULATES this via ``persistSecret``).
    public func lookupRoute(pushGidHex: String)
        -> (accountIdB64: String, groupIdHex: String)? {
        store.lookupRoute(pushGidHex: pushGidHex)
    }

    // MARK: - Subscribe / receive / redistribute

    /// Subscribe this device to push wakes for a group UNDER ITS
    /// `push_gid`. A member that does not hold the secret **defers** — it
    /// never subscribes under the leaky `group_id`. Best-effort: failures
    /// are logged and swallowed (push is a UX nicety; foreground polling
    /// still works).
    public func subscribeUnderPushGidBestEffort(
        accountIdB64: String,
        groupIdHex: String
    ) async {
        guard let pushGid = pushGid(accountIdB64: accountIdB64, groupIdHex: groupIdHex) else {
            // No secret held yet → DEFER. Never subscribe under group_id.
            return
        }
        do {
            try await subscribeUnderPushGid(pushGid)
        } catch {
            NSLog("push_gid subscribe failed: \(error)")
        }
    }

    /// RECEIVE side — a member got the distribution envelope: run the
    /// authority gate, then persist the secret (idempotent) + subscribe
    /// under the derived `push_gid`. Best-effort — never throws into the
    /// poll loop.
    ///
    /// **The authority gate (`lakko-security` MEDIUM-1).** The send side
    /// only ever lets the ADR-0022 OWNER mint
    /// (``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``),
    /// but without a matching receive-side check ANY member could broadcast
    /// a fresh 32-byte envelope and unilaterally rotate the whole group's
    /// routing id — every member would overwrite their secret and
    /// re-subscribe, and because there is no per-group unsubscribe each
    /// churn strands an orphan push-gw subscription that keeps receiving
    /// wakes. So a *changed* secret is accepted only from the owner, and a
    /// FIRST-ever secret additionally from an admin (which is how a member
    /// added by a non-owner admin ever learns it); see
    /// ``classifyIncomingSecret(incoming:stored:senderKey:authority:)`` for
    /// the full matrix.
    ///
    /// - Parameters:
    ///   - group: the live MLS `Group` — the ONLY authority for both the
    ///     role table (owner + admins) and the sender leaf → signature-key
    ///     mapping. Never trust a value carried in the envelope.
    ///   - senderLeaf: the MLS leaf index that authored the application
    ///     message (surfaced by `ProcessedMessage`, the same
    ///     MLS-authenticated attribution the ADR-0023 author-only gate
    ///     uses). `nil` ⇒ unattributable ⇒ only a byte-identical re-send
    ///     is accepted.
    public func receiveSecret(
        accountIdB64: String,
        groupIdHex: String,
        secret: Data,
        group: Group,
        senderLeaf: UInt32?
    ) async {
        let stored = store.getSecret(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        // `getSecret` returns nil for THREE different reasons: nothing
        // stored, no device wrap key, or the AEAD open failed. Only the
        // first is a provable hole. `hasSecret` is the key-free presence
        // check that separates them, so an admin cannot exploit a transient
        // Keychain / decrypt failure to overwrite a secret we really hold.
        let storedUnreadable = stored == nil
            && store.hasSecret(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let resolved = Self.resolveSenderAuthority(group: group, senderLeaf: senderLeaf)
        let verdict = Self.classifyIncomingSecret(
            incoming: secret,
            stored: stored,
            storedUnreadable: storedUnreadable,
            senderKey: resolved.senderKey,
            authority: resolved.authority
        )
        guard verdict == .accept else {
            // Low-cardinality diagnostics only — never the secret or any
            // signature key. `held` distinguishes the two refusal shapes:
            // an overwrite attempt (held: true) vs a seed from an
            // unauthorised sender (held: false).
            NSLog(
                "push-routing secret REFUSED: sender is not authorised to set it "
                    + "(leaf \(senderLeaf.map(String.init) ?? "none"), "
                    + "authority \(resolved.authority.diagnosticTag), "
                    + "held \(stored != nil), unreadable \(storedUnreadable))"
            )
            return
        }
        guard persistSecret(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, secret: secret) != nil else {
            return
        }
        await subscribeUnderPushGidBestEffort(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    // MARK: - Receive-side authority gate (lakko-security MEDIUM-1)

    /// Verdict for an inbound push-routing-secret distribution envelope.
    public enum SecretVerdict: Equatable {
        /// Persist the secret + (re-)subscribe under the derived `push_gid`.
        case accept
        /// Drop it: no persist, no subscribe, no state change at all.
        case reject
    }

    /// What the live MLS group says about who is allowed to set the
    /// group's `push_routing_secret`.
    ///
    /// The three states are deliberately kept distinct: a group with NO
    /// role table is not the same as a group whose role table could not be
    /// read (the first is a legacy / open group, the second is a failure
    /// that must fail closed).
    public enum SecretAuthority: Equatable {
        /// The group carries an ADR-0022 role table: the owner's 32-byte
        /// Ed25519 signature public key plus the admin set (each also a
        /// raw 32-byte signature key, as `RoleTable.admins` reports them).
        /// Owner outranks admin outranks member — see
        /// ``classifyIncomingSecret(incoming:stored:senderKey:authority:)``.
        case roleTable(owner: Data, admins: [Data])
        /// The group carries NO role table (a legacy / open group). There
        /// is no owner to gate on, so the pre-gate behaviour is preserved
        /// (accept) rather than bricking push for such groups — matching
        /// the ADR-0042 open-question decision to leave un-owned legacy
        /// groups alone until a natural re-creation.
        case noRoleTable
        /// The role table or the member roster could not be read (the FFI
        /// threw). Fail-closed: a *changed* secret is refused.
        case unreadable

        /// Low-cardinality tag for the refusal log — never the key bytes.
        var diagnosticTag: String {
            switch self {
            case .roleTable: return "role-table"
            case .noRoleTable: return "no-role-table"
            case .unreadable: return "unreadable"
            }
        }
    }

    /// The pure, side-effect-free receive-side authorization decision.
    ///
    /// **Owner outranks admin outranks member**, expressed purely as *who
    /// may overwrite what* — so no authority level is ever persisted
    /// alongside the secret, and the outcome does not depend on the order
    /// the envelopes arrive in (an owner broadcast always wins, whether it
    /// lands before or after an admin's).
    ///
    /// | stored vs incoming | authority + sender | verdict |
    /// |---|---|---|
    /// | identical | any sender, incl. a plain member | **accept** (idempotent re-broadcast) |
    /// | changed / first | `.roleTable`, sender == owner | **accept** (the owner may always rotate) |
    /// | **first, PROVABLY absent** | `.roleTable`, sender ∈ admins | **accept** (an admin may SEED a hole) |
    /// | **changed**, or absence unprovable | `.roleTable`, sender ∈ admins | **reject** (an admin may not OVERWRITE) |
    /// | changed / first | `.roleTable`, plain member (or no sender) | **reject** |
    /// | changed / first | `.noRoleTable` | **accept** (legacy / open group) |
    /// | changed / first | `.unreadable` | **reject** (fail-closed) |
    ///
    /// The admin-seeds row is what keeps delivery working: ADR-0022 lets
    /// owner OR admin add a member, and
    /// ``redistribute(accountIdB64:groupIdHex:broadcast:)`` fires after any
    /// add-member commit, so a member added by a non-owner admin holds no
    /// secret and would otherwise never get push routing (the owner only
    /// broadcasts at mint time). Withholding the OVERWRITE from admins is
    /// what keeps the group-wide rotation power owner-only — an admin can
    /// fill a hole but can never strand the group on a rotated
    /// `push_gid`.
    ///
    /// The byte-identical escape hatch is what keeps the owner's
    /// post-add-member `redistribute` idempotent even when it is relayed by
    /// a path whose sender attribution does not resolve — re-persisting the
    /// value already held changes nothing, so it can never be an attack.
    /// - Parameters:
    ///   - stored: the secret this device already holds, `nil` when it
    ///     holds none **or** could not read it.
    ///   - storedUnreadable: set when the local secret could NOT be read
    ///     (no device wrap key, or the AEAD open failed). `stored == nil`
    ///     then means "unknown", NOT "provably absent" — so the admin
    ///     hole-fill branch must refuse, else a transient Keychain / decrypt
    ///     failure would let an admin overwrite a secret this device really
    ///     holds. The owner branch is unaffected (an owner write is the
    ///     repair path for exactly that state).
    public static func classifyIncomingSecret(
        incoming: Data,
        stored: Data?,
        storedUnreadable: Bool = false,
        senderKey: Data?,
        authority: SecretAuthority
    ) -> SecretVerdict {
        // Byte-identical to what we already hold → a no-op re-persist.
        // Safe from anyone (a plain member included), and required for
        // `redistribute` idempotence.
        if let stored, stored == incoming { return .accept }

        switch authority {
        case .noRoleTable:
            // No ADR-0022 role table ⇒ nobody to check against. Preserve
            // the pre-gate behaviour for legacy / open groups.
            return .accept
        case .unreadable:
            // Could not read the roles / roster ⇒ cannot authorise a
            // CHANGE. Fail-closed (invariant #4).
            return .reject
        case .roleTable(let ownerKey, let admins):
            // An unattributable sender (no leaf, or a leaf that is not in
            // the current roster) can never authorise a set. An EMPTY key
            // is not a valid Ed25519 signature key — refuse it outright so
            // a degenerate role-table entry can never be matched by an
            // equally degenerate resolved sender.
            guard let senderKey, !senderKey.isEmpty else { return .reject }

            // OWNER — the priority. May always set the secret, whatever is
            // stored and in whatever order the envelopes arrive.
            if senderKey == ownerKey { return .accept }

            // ADMIN — may SEED a hole (nothing stored yet) so a member the
            // admin added gets routing, but may never OVERWRITE a secret
            // this device already holds. The hole must be PROVABLE: an
            // unreadable store is treated as possibly-occupied
            // (fail-closed, invariant #4).
            if stored == nil, !storedUnreadable, admins.contains(senderKey) {
                return .accept
            }

            // A plain member, or an admin attempting a rotation.
            return .reject
        }
    }

    /// Read the ADR-0022 role table (owner + admins) + resolve `senderLeaf`
    /// to its signature public key off the live MLS `Group`.
    ///
    /// The leaf → key mapping comes from `Group.members()` (each
    /// `GroupMember` carries `leafIndex` + `signaturePublicKey`), and the
    /// authorised keys from `Group.groupRoles()` (`RoleTable.owner` +
    /// `RoleTable.admins`) — both read from the group's own authenticated
    /// state, never from the envelope. Any FFI throw folds into
    /// ``SecretAuthority/unreadable`` so the classifier fails closed. A
    /// `senderLeaf` that is not in the current roster resolves to `nil`
    /// (⇒ rejected on a change), not to `.unreadable`.
    public static func resolveSenderAuthority(
        group: Group,
        senderLeaf: UInt32?
    ) -> (authority: SecretAuthority, senderKey: Data?) {
        let authority: SecretAuthority
        do {
            if let table = try group.groupRoles() {
                authority = .roleTable(owner: table.owner, admins: table.admins)
            } else {
                authority = .noRoleTable
            }
        } catch {
            return (.unreadable, nil)
        }

        guard let senderLeaf else { return (authority, nil) }
        do {
            let key = try group.members()
                .first { $0.leafIndex == senderLeaf }?
                .signaturePublicKey
            return (authority, key)
        } catch {
            return (.unreadable, nil)
        }
    }

    /// Re-broadcast the CURRENT secret to a group (used right after an
    /// add-member commit so the new member learns it on their next poll).
    /// No-op when this device does not hold the secret. Idempotent for
    /// existing members (a re-received secret is the same value → an
    /// idempotent re-persist). Best-effort.
    public func redistribute(
        accountIdB64: String,
        groupIdHex: String,
        broadcast: (Data) async throws -> Void
    ) async {
        guard let secret = store.getSecret(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex) else {
            return
        }
        do {
            try await broadcast(try Self.composeSecretEnvelope(secret: secret))
        } catch {
            NSLog("push-routing secret redistribute failed: \(error)")
        }
    }

    // MARK: - Owner-mint funnel

    /// Ensure a group's push routing is provisioned for this device.
    ///
    ///   * If the local client is the OWNER (ADR-0022) and no secret is
    ///     held yet: mint + persist + broadcast the distribution envelope
    ///     (via `broadcast`, which posts an E2EE MLS application message to
    ///     the group journal).
    ///   * Then subscribe under `push_gid` if the secret is held (owner
    ///     just minted it; a member may already hold one from a prior
    ///     receive), else defer.
    ///
    /// This single funnel serves BOTH the chat-open path (owner opens the
    /// freshly founded group) AND the legacy-sweep (owner opens /
    /// rehydrates an un-provisioned group). Non-owners never mint here —
    /// they only subscribe if they already hold a secret; otherwise they
    /// wait for the receive path.
    ///
    /// A per-group in-flight guard coalesces a concurrent legacy-sweep +
    /// onAppear. The mint → persist is synchronous (no `await` before
    /// ``PushRoutingStore/hasSecret(accountIdB64:groupIdHex:)`` flips true),
    /// so even absent the guard a second concurrent call would never
    /// re-mint; the guard additionally suppresses a redundant re-broadcast.
    ///
    /// - Parameter broadcast: posts the distribution-envelope bytes to the
    ///   group journal as an E2EE MLS application message
    ///   (persist-before-publish). Only invoked on the owner-mint branch.
    public func ensureProvisioned(
        accountIdB64: String,
        groupIdHex: String,
        group: Group,
        identity: Identity,
        broadcast: (Data) async throws -> Void
    ) async {
        let lockKey = accountIdB64 + "\u{0}" + groupIdHex
        if inflight.contains(lockKey) { return }
        inflight.insert(lockKey)
        defer { inflight.remove(lockKey) }

        // ADR-0022 role: `.owner`. `nil` = open / legacy group with no role
        // table — nobody mints (avoids divergence), matching the ADR-0042
        // open-question decision to keep un-owned legacy groups on deferred
        // push until a natural re-creation.
        let isOwner = (try? group.ownRole(identity: identity)) == .owner

        if isOwner, !store.hasSecret(accountIdB64: accountIdB64, groupIdHex: groupIdHex) {
            guard let secret = Self.mintSecret() else { return }
            // Mint → persist synchronously (flips `hasSecret` true) before
            // any await, then distribute to founding members over the E2EE
            // journal. Best-effort beyond the persist — a broadcast failure
            // retries on the next launch / add-member; the local secret +
            // reverse index are already durable.
            if persistSecret(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, secret: secret) != nil {
                do {
                    try await broadcast(try Self.composeSecretEnvelope(secret: secret))
                } catch {
                    NSLog("push-routing secret broadcast failed: \(error)")
                }
            }
        }

        // Subscribe (or defer) under push_gid. Owner just minted →
        // subscribes; a member with no secret → deferred (never under
        // group_id).
        await subscribeUnderPushGidBestEffort(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    // MARK: - Store passthrough

    /// Wipe every push-routing secret + reverse-index entry for the
    /// account. Called from ``ChatRepository/forgetDevice()``.
    public func clear(accountIdB64: String) {
        store.clear(accountIdB64: accountIdB64)
    }

    // MARK: - Helpers

    /// Lowercase-hex a byte buffer (the push-gw pseudonym wire form).
    public nonisolated static func hex(_ bytes: Data) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
    }
}
