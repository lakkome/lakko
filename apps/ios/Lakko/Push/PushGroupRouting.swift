// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import Security

/// ADR-0042 orchestration — mint / receive / derive / subscribe / notify
/// the opaque per-group push routing id (`push_gid`), closing the
/// invariant-#5 push social-graph leak — plus the ADR-0043 rotation of that
/// id when a member is REMOVED (see the rotation section at the bottom of
/// this file for why removal is the only event that rotates).
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
///   * an ADMIN may **seed a hole** — set the FIRST-ever secret this
///     device holds — AND (ADR-0043, ratified option (b)) **overwrite** an
///     established one;
///   * a plain member may only re-send the byte-identical value.
///
/// Admitting admins as seeders is what keeps delivery working: ADR-0022
/// lets owner OR admin add a member, and `redistribute` fires after any
/// add-member commit, so a member added by a non-owner admin learns the
/// secret from that admin's re-broadcast (the owner only broadcasts at
/// mint time). Admitting admins as **rotators** (ADR-0043) is what makes
/// rotation-on-removal reachable at all, since ADR-0022 also lets an admin
/// commit the Remove: refusing the overwrite left an admin able to evict a
/// member but unable to retire that member's routing id, so the eviction
/// completed with the ex-member's wake stream intact.
///
/// The cost of the admin tier is recorded honestly in ADR-0043 § fork (b):
/// it widens G-36 residual 3 from "a malicious admin can seed a secret to a
/// member who holds none" to "a malicious admin can churn the group's
/// routing id at will". The capability gain is small (an admin can already
/// evict members outright) and the churn is a wake outage, never a
/// downgrade — a member without the current secret still DEFERS, it never
/// falls back to the leaky `group_id`.
///
/// **Rotation never MINTS.** Option (b) widened OVERWRITE authority, never
/// MINT authority: ``rotateSecret(accountIdB64:groupIdHex:ownRole:broadcast:)``
/// is a no-op on a device that holds no secret, so an admin can never become
/// a second minter and diverge the group's `push_gid`.
/// See ``classifyIncomingSecret(incoming:stored:senderKey:authority:)``.
///
/// ## A mint is not done until the group has been told
///
/// The owner persists before it broadcasts (minting twice would diverge the
/// group's routing id — far worse than sending one envelope twice), so a
/// broadcast that fails leaves a durably stored secret nobody else knows.
/// The store therefore carries a durable **`distributed`** flag beside each
/// secret, and the retry branch of
/// ``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``
/// re-broadcasts whenever it is still `false`. Unlike the ADR-0040
/// commit-outbox this needs no stored bytes: the distribution envelope is
/// recomputable from the secret at any time
/// (``composeSecretEnvelope(secret:)``), so recording *whether* it landed is
/// the entire durable state. Re-broadcasting is idempotent for every member
/// because the receive gate accepts a byte-identical secret from anyone.
///
/// The same one bit covers the *post-add-member*
/// ``redistribute(accountIdB64:groupIdHex:broadcast:)``, which is where the
/// per-GROUP flag would otherwise fall short of a per-MEMBER guarantee: that
/// broadcast runs against a row the mint already settled, for the benefit of
/// the ONE member just added, so a failure must un-settle the row
/// (``PushRoutingStore/markPending(accountIdB64:groupIdHex:matching:)``) —
/// else the flag stays `true`, no sweep re-sends, and that member defers push
/// forever.
///
/// **The retry is NOT owner-gated, and must not be.** Only the MINT is:
/// the secret is random, so two independent mints diverge the group's
/// `push_gid`. Re-sending bytes the device already holds picks nothing —
/// the receive gate accepts a byte-identical secret from ANY sender before
/// authority is even consulted — so any holder may retry. That matters
/// because ADR-0022 lets an ADMIN add a member: the admin's `redistribute`
/// runs, and if it fails only the ADMIN's row is left pending. An owner-only
/// retry branch would make that flag inert and the newly-added member would
/// never be sent the secret by anyone.
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

    /// Unsubscribe seam — `(push_gid) -> ()`. ADR-0043: the counterpart the
    /// rotation needs to retire a SUPERSEDED id. Defaults to the real
    /// `POST /push/group/unsubscribe` call, which had **zero call sites**
    /// before this ADR (the wrapper existed on all three clients and was
    /// never invoked); injectable so tests can spy on the target.
    private let unsubscribeUnderPushGid: (Data) async throws -> Void

    /// Per-group in-flight provisioning keys, so a concurrent legacy-sweep
    /// + onAppear for the same group mint the random secret exactly once.
    private var inflight: Set<String> = []

    /// Per-group in-flight ROTATION tasks (ADR-0043).
    ///
    /// Two triggers can observe the same removal (this device's own
    /// `removeMemberFromGroup` and a concurrent poll-loop diff). Interleaving
    /// them is the damaging case: both would read the SAME old secret, both
    /// mint, the second persist wins — leaving the first new `push_gid`
    /// subscribed and indexed but never notified again, i.e. exactly the
    /// orphan this ADR exists to prevent. Coalescing rules that out.
    ///
    /// Unlike the provisioning guard (a skip-if-in-flight `Set`) this holds
    /// the `Task` so a second caller **awaits the same rotation** and observes
    /// its outcome, rather than silently returning early with a verdict that
    /// does not describe what happened. Mirrors the web `rotating`
    /// `Map<string, Promise>`.
    ///
    /// The cost is that a SECOND removal observed while a rotation is in
    /// flight can be folded into it, and that rotation's envelope may have
    /// been sealed at an epoch where the second evictee was still a member.
    /// That is tolerable because the poll-path diff is a redundant safety net,
    /// not the primary path: ADR-0022 requires the Remove committer to be
    /// owner-or-admin, and that device runs its own rotation strictly AFTER
    /// its own commit — so every removal already has one correctly-sequenced
    /// rotation from the device that made it.
    private var rotating: [String: Task<PushRotationOutcome, Never>] = [:]

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
        },
        unsubscribeUnderPushGid: @escaping (Data) async throws -> Void = { pushGid in
            let devicePseudonym = try DevicePushId.getOrCreate()
            _ = try await PushClient().unsubscribeGroup(
                devicePseudonym: devicePseudonym,
                groupPseudonym: pushGid
            )
        }
    ) {
        self.store = store
        self.subscribeUnderPushGid = subscribeUnderPushGid
        self.unsubscribeUnderPushGid = unsubscribeUnderPushGid
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

    /// Persist a group's secret + its `distributed` flag
    /// (device-key-encrypted) AND its `push_gid → (account, group_id)`
    /// reverse-index entry. Idempotent. Returns the derived `push_gid`
    /// bytes, or `nil` when the derivation fails (should not happen for a
    /// 32-byte secret).
    ///
    /// - Parameter distributed: whether the group has already been told this
    ///   secret. The owner-mint path persists `false` FIRST and flips it
    ///   only once the broadcast lands; the receive path persists `true`
    ///   (the envelope's arrival IS the proof it travelled).
    @discardableResult
    public func persistSecret(
        accountIdB64: String,
        groupIdHex: String,
        secret: Data,
        distributed: Bool
    ) -> Data? {
        guard let pushGid = Self.derivePushGid(secret: secret) else { return nil }
        store.putRouting(
            accountIdB64: accountIdB64,
            groupIdHex: groupIdHex,
            secret: secret,
            pushGidHex: Self.hex(pushGid),
            distributed: distributed
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

    /// ADR-0043 — retire a SUPERSEDED `push_gid`: drop this device's push-gw
    /// subscription under it, then delete its reverse-index row.
    ///
    /// Called on BOTH sides of a rotation with the OLD secret — by the
    /// rotator after it re-arms under the new id, and by every remaining
    /// member when the gated receive path applies a CHANGED secret. Both
    /// must, and for the same reason: `POST /push/group/unsubscribe` deletes
    /// exactly the `(device, group)` row named in the request, and a client
    /// knows only its OWN device pseudonym, so nobody can tear down anyone
    /// else's row. If only the rotator unsubscribed, every OTHER member would
    /// leave a live row under the old id — one dead subscription per member
    /// per rotation, which is the "versioned cluster archive" ADR-0043
    /// § Context calls strictly worse than not rotating at all.
    ///
    /// The **subscription** teardown is what carries the privacy value; the
    /// route row is local bookkeeping (a reverse-map miss already degrades to
    /// the generic "New message" banner under the ADR-0041 fail-closed
    /// posture), so it is deleted immediately — the simpler invariant —
    /// rather than retained for an overlap window.
    ///
    /// Best-effort and never throws: the caller has ALREADY switched to the
    /// new id, so a failure here must not unwind the rotation. The cost of a
    /// failed unsubscribe is a stale row that nothing notifies again.
    public func retireOldPushGid(oldSecret: Data) async {
        guard let oldGid = Self.derivePushGid(secret: oldSecret) else {
            NSLog("push-routing old push_gid derivation failed")
            return
        }
        do {
            try await unsubscribeUnderPushGid(oldGid)
        } catch {
            // The new id is already armed; a failed unsubscribe leaves a row
            // that nothing notifies again, not a broken group.
            NSLog("push_gid unsubscribe failed: \(error)")
        }
        store.removeRoute(pushGidHex: Self.hex(oldGid))
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
    /// re-subscribe, stranding an orphan push-gw subscription per churn. So
    /// a *changed* secret is accepted only from the owner or an ADR-0022
    /// admin (ADR-0043 option (b)), never a plain member; see
    /// ``classifyIncomingSecret(incoming:stored:senderKey:authority:)`` for
    /// the full matrix.
    ///
    /// **ADR-0043 — the receiving half of a rotation.** When the accepted
    /// secret CHANGES the one this device held, the id it was subscribed
    /// under is now dead, so the old `push_gid` is retired here too
    /// (``retireOldPushGid(oldSecret:)``). Every member tears down its OWN
    /// push-gw row because no client can delete anyone else's; skip this and
    /// each rotation leaves one dead row PER MEMBER. The old secret is read
    /// from the `stored` snapshot taken BEFORE the persist below — the secret
    /// row is overwritten in place, so after the write the old bytes are
    /// unrecoverable and the retiring id could never be named.
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
        // ADR-0043 — did this accepted secret SUPERSEDE one we held? Captured
        // from the pre-persist snapshot, because the row is clobbered in place.
        let superseded: Data? = (stored != nil && stored != secret) ? stored : nil
        // `distributed: true` — this secret ARRIVED over the group journal,
        // so its distribution envelope is provably on the wire. Since the
        // retry branch of `ensureProvisioned` is open to ANY holder, this flag
        // is what keeps a receiver from re-broadcasting an envelope it just
        // watched travel: a settled row is never retried by anyone.
        guard persistSecret(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, secret: secret,
            distributed: true) != nil else {
            return
        }
        // Arm the NEW id first, so a failure between the two never leaves this
        // device subscribed to nothing.
        await subscribeUnderPushGidBestEffort(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        // ADR-0043 — then retire the superseded one. This device is the only
        // party that can drop its own push-gw row.
        if let superseded {
            NSLog(
                "push-routing secret ROTATED by a peer (retiring the old push_gid; "
                    + "authority \(resolved.authority.diagnosticTag))")
            await retireOldPushGid(oldSecret: superseded)
        }
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
    /// | **first, PROVABLY absent** | `.roleTable`, sender ∈ admins | **accept** (an admin SEEDS a hole) |
    /// | **changed**, or absence unprovable | `.roleTable`, sender ∈ admins | **accept** (ADR-0043 — an admin may ROTATE) |
    /// | changed / first | `.roleTable`, plain member (or no sender) | **reject** |
    /// | changed / first | `.noRoleTable` | **accept** (legacy / open group) |
    /// | changed / first | `.unreadable` | **reject** (fail-closed) |
    ///
    /// The admin-seeds row is what keeps delivery working: ADR-0022 lets
    /// owner OR admin add a member, and
    /// ``redistribute(accountIdB64:groupIdHex:broadcast:)`` fires after any
    /// add-member commit, so a member added by a non-owner admin holds no
    /// secret and would otherwise never get push routing (the owner only
    /// broadcasts at mint time).
    ///
    /// **ADR-0043 (ratified option (b)) widened the admin row from a refusal
    /// to an acceptance.** ADR-0022 lets an admin commit the Remove that
    /// makes a rotation necessary, so refusing the overwrite left an admin
    /// able to evict a member but unable to retire that member's routing id.
    /// The `stored` / `storedUnreadable` pair no longer gates an admin's
    /// *authority* — it only distinguishes a provable seed from a rotate for
    /// the diagnostic log. Owner is still evaluated FIRST, so the
    /// arrival-order-independent "owner takes priority" property is
    /// unchanged, and the widening stops at the admin tier: a plain member is
    /// still refused whether or not a secret is held.
    ///
    /// This widens G-36 residual 3 to "a malicious admin can churn the
    /// group's routing id at will" — a wake outage, never a downgrade (a
    /// member without the current secret DEFERS, invariant #4).
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
    ///     then means "unknown", NOT "provably absent" — so an admin write is
    ///     a ROTATE rather than a seed. Since ADR-0043 option (b) authorises
    ///     an admin either way this no longer changes the accept/reject
    ///     outcome; it keeps the diagnostic honest about what was observed.
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

            // ADMIN — may SEED a hole (so a member the admin added gets
            // routing) AND, under ADR-0043 option (b), ROTATE an established
            // secret (so an admin-committed Remove can retire the evicted
            // member's routing id). `stored` / `storedUnreadable` no longer
            // gate the authority; both cases accept.
            if admins.contains(senderKey) { return .accept }

            // A plain member — the tier boundary the widening did NOT cross.
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
    ///
    /// Unlike the mint, this runs against a row the mint already settled, so
    /// BOTH outcomes have to be recorded: a success settles the flag exactly
    /// like the mint-time broadcast (so an owner whose original broadcast
    /// failed stops re-broadcasting once ANY of them lands), and a FAILURE
    /// un-settles it (so the newly-added member this redistribute was for is
    /// not stranded on a permanently `true` flag that no sweep would ever
    /// re-send). See ``broadcastAndSettle(secret:accountIdB64:groupIdHex:what:broadcast:)``.
    ///
    /// **This device does not have to be the owner** — ADR-0022 lets an admin
    /// add a member too, so an admin reaches this path, and the pending flag
    /// it leaves behind is what makes ITS OWN retry fire: the re-broadcast
    /// branch of
    /// ``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``
    /// is open to any secret HOLDER, not just the owner. It has to be — the
    /// owner's row is still settled and the owner never learns this broadcast
    /// failed, so an owner-only retry branch would strand the newly-added
    /// member permanently. Re-sending an identical secret needs no authority
    /// (the receive gate accepts it from anyone); only the mint does.
    public func redistribute(
        accountIdB64: String,
        groupIdHex: String,
        broadcast: (Data) async throws -> Void
    ) async {
        guard let secret = store.getSecret(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex) else {
            return
        }
        await broadcastAndSettle(
            secret: secret, accountIdB64: accountIdB64, groupIdHex: groupIdHex,
            what: "redistribute", broadcast: broadcast)
    }

    /// Compose + broadcast a secret's distribution envelope, then durably
    /// record the outcome: on success the group has been told
    /// (``PushRoutingStore/markDistributed(accountIdB64:groupIdHex:matching:)``),
    /// on failure it has NOT
    /// (``PushRoutingStore/markPending(accountIdB64:groupIdHex:matching:)``).
    ///
    /// This is the whole recovery mechanism: the envelope is never stored
    /// because it is recomputable from the secret, so a failed broadcast
    /// needs to leave behind exactly one bit — "not yet on the wire" — and
    /// ``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``
    /// picks it up on the next launch / chat-open.
    ///
    /// **Why the failure path WRITES rather than leaving the flag alone.**
    /// The flag is per-GROUP, not per-member. On the mint / re-broadcast
    /// paths the row is already pending, so the write is a no-op — but a
    /// ``redistribute(accountIdB64:groupIdHex:broadcast:)`` runs against a row
    /// the mint already settled, for the benefit of the ONE member that was
    /// just added. If that broadcast fails and the flag stays `true`, no
    /// sweep ever re-sends and the new member defers push forever: the same
    /// permanent-no-push bug the flag exists to prevent, scoped to one member
    /// instead of the whole group. Un-settling the row is what puts it back in
    /// **this device's own** retry branch — which is why that branch is open to
    /// any holder and not just the owner: an admin may have been the one to add
    /// the member, and the owner's row would still read settled. `matching:`
    /// keeps a failed OLD-secret broadcast from un-settling a newly rotated
    /// secret that did travel.
    private func broadcastAndSettle(
        secret: Data,
        accountIdB64: String,
        groupIdHex: String,
        what: String,
        broadcast: (Data) async throws -> Void
    ) async {
        do {
            try await broadcast(try Self.composeSecretEnvelope(secret: secret))
            store.markDistributed(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, matching: secret)
        } catch {
            store.markPending(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, matching: secret)
            NSLog(
                "push-routing secret \(what) failed (secret marked "
                    + "undistributed; this device re-broadcasts on the next "
                    + "provision): \(error)")
        }
    }

    // MARK: - Owner-mint funnel

    /// Ensure a group's push routing is provisioned for this device.
    ///
    ///   * OWNER (ADR-0022), no secret held → **mint**: persist marked
    ///     undistributed, broadcast the distribution envelope (via
    ///     `broadcast`, which posts an E2EE MLS application message to the
    ///     group journal), and mark distributed once it lands. A non-owner
    ///     holding nothing does neither — it waits for the receive path.
    ///   * ANY HOLDER (owner, admin or plain member), secret held but **not**
    ///     distributed → **re-broadcast**: recompose the envelope from the
    ///     stored secret and try again, marking distributed on success.
    ///   * Then subscribe under `push_gid` if the secret is held (owner
    ///     just minted it; a member may already hold one from a prior
    ///     receive), else defer.
    ///
    /// ## Why the re-broadcast branch exists
    ///
    /// A mint whose broadcast fails (offline, DS error) leaves the secret
    /// durably stored, so a guard shaped `isOwner && !hasSecret` would
    /// short-circuit on every later launch and never retry — the group's
    /// other members never learn the secret and defer push **forever**
    /// (only an unrelated add-member's
    /// ``redistribute(accountIdB64:groupIdHex:broadcast:)`` would
    /// accidentally repair it). The durable `distributed` flag is what makes
    /// that failure recoverable: the envelope itself is never stored because
    /// it is recomputable from the secret, so one bit is the entire state
    /// needed. A re-broadcast is safe by construction — the ADR-0042 receive
    /// gate accepts a byte-identical secret from any sender, so it is an
    /// idempotent no-op re-persist for every member that already has it.
    ///
    /// ## Why only the MINT is owner-gated
    ///
    /// The two branches need different authority, so the guard is split
    /// rather than wrapping both in `isOwner`:
    ///
    ///   * the **mint** CHOOSES a value — 32 random bytes — so two
    ///     independent minters would diverge the group's `push_gid` and break
    ///     routing. Exactly one source may create it: the ADR-0022 owner.
    ///   * the **re-broadcast** re-sends bytes the device already holds. The
    ///     receive gate accepts a byte-identical secret from ANY sender, and
    ///     checks that *before* consulting authority, so this needs none —
    ///     and grants none: a member could always have replayed an identical
    ///     envelope to the same effect.
    ///
    /// Gating the retry on `isOwner` too is what left ADR-0022's admin path
    /// broken: an ADMIN may add a member, so an admin runs
    /// ``redistribute(accountIdB64:groupIdHex:broadcast:)``; when that fails,
    /// the ADMIN's row goes pending while the owner's row stays settled and
    /// the owner never learns anything went wrong. With an owner-only retry
    /// nobody re-sends and the newly-added member gets no push, permanently.
    ///
    /// This single funnel serves BOTH the chat-open path (any member opens a
    /// group) AND the legacy-sweep (rehydrating an un-provisioned group).
    /// A non-owner never MINTS here; it re-broadcasts only a secret it
    /// already holds, and otherwise just subscribes (or defers, when it holds
    /// none, and waits for the receive path).
    ///
    /// A per-group in-flight guard coalesces a concurrent legacy-sweep +
    /// onAppear. The mint → persist is synchronous (no `await` before
    /// ``PushRoutingStore/hasSecret(accountIdB64:groupIdHex:)`` flips true),
    /// so even absent the guard a second concurrent call would never
    /// re-mint; the guard additionally suppresses a redundant re-broadcast.
    ///
    /// - Parameter broadcast: posts the distribution-envelope bytes to the
    ///   group journal as an E2EE MLS application message
    ///   (persist-before-publish). Only invoked on the two write branches
    ///   (the owner-only mint + the any-holder re-broadcast); a device holding
    ///   no secret never reaches it. It MUST throw when the bytes did not reach
    ///   the journal — a silent no-op return would durably mark an
    ///   undelivered secret as distributed.
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

        if !store.hasSecret(accountIdB64: accountIdB64, groupIdHex: groupIdHex) {
            // MINT — OWNER ONLY. The secret is 32 RANDOM bytes, so a second
            // independent mint would diverge the group's `push_gid`; exactly
            // one source may create it. A non-owner holding nothing simply
            // waits for the distribution envelope (and defers push meanwhile).
            if isOwner {
                // Persist synchronously (flips `hasSecret` true) before any
                // await, marked NOT distributed, then tell the group over the
                // E2EE journal. Persist-before-broadcast stays: minting twice
                // would diverge the group's routing id, which is far worse
                // than re-broadcasting one envelope.
                guard let secret = Self.mintSecret() else { return }
                if persistSecret(
                    accountIdB64: accountIdB64, groupIdHex: groupIdHex, secret: secret,
                    distributed: false) != nil {
                    await broadcastAndSettle(
                        secret: secret, accountIdB64: accountIdB64, groupIdHex: groupIdHex,
                        what: "broadcast", broadcast: broadcast)
                }
            }
        } else if let pending = store.getRecord(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex), !pending.distributed {
            // RE-BROADCAST — the repair branch, open to ANY HOLDER (owner,
            // admin or plain member). A broadcast for this secret failed
            // (offline / DS error), so somebody who needs it never learned it:
            // after a failed MINT that is the whole group, after a failed
            // `redistribute(accountIdB64:groupIdHex:broadcast:)` it is the one
            // member that was just added. Either way they defer push forever
            // unless this row is retried.
            //
            // **Why this branch needs no authority.** It re-sends the exact
            // bytes already stored, and the ADR-0042 receive gate accepts a
            // BYTE-IDENTICAL secret from any sender *before* any authority is
            // consulted (see
            // ``classifyIncomingSecret(incoming:stored:senderKey:authority:)``),
            // so every member that already holds it performs a no-op
            // re-persist and the member that does not gets the seed. Owner
            // authority is required for the MINT — where the value is chosen —
            // never for re-sending a value the group already agreed on, so
            // this grants a member no capability it did not already have (it
            // could always replay an identical envelope and be accepted).
            //
            // Gating it on `isOwner` is what left the hole this closes: ADR-0022
            // lets an ADMIN add a member, so an admin runs `redistribute`, and
            // its failure marked the ADMIN's row pending while the owner's row
            // stayed settled — nobody would ever retry.
            await broadcastAndSettle(
                secret: pending.secret, accountIdB64: accountIdB64, groupIdHex: groupIdHex,
                what: "re-broadcast", broadcast: broadcast)
        }
        // A device holding a secret it cannot READ (`getRecord` nil while
        // `hasSecret` is true — wrap key rotated / corrupt row) falls through
        // deliberately: it must not re-mint (that would diverge the group's
        // routing id) and cannot recompose the envelope from a secret it
        // cannot read.

        // Subscribe (or defer) under push_gid. Owner just minted →
        // subscribes; a member with no secret → deferred (never under
        // group_id).
        await subscribeUnderPushGidBestEffort(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    // MARK: - ADR-0043: rotate the routing secret when a member is REMOVED
    //
    // ## What rotation buys, stated precisely
    //
    // A removed member keeps the group's `push_routing_secret` on disk, so it
    // can still derive the current `push_gid` — and since nothing tears its
    // subscription down, push-gw keeps waking that device for a group it was
    // evicted from. The leak is not storage hygiene: an ex-member learns
    // "this group was active at time T" on every send, indefinitely.
    //
    // It cannot be closed by unsubscribing, because
    // `POST /push/group/unsubscribe` deletes exactly the `(device, group)`
    // row named in the request and a client knows only its OWN device
    // pseudonym — no remaining member can reach the ex-member's row, and the
    // ex-member's client has no "you were removed" handling to do it itself.
    // Re-keying is the only mechanism the GROUP has: after a rotation nobody
    // ever notifies the old id again, so that row goes **inert without the
    // ex-member's cooperation**.
    //
    // What it does NOT buy: unlinkability of the CLUSTER. The provider sees
    // `{A,B,C,D}` under the old id and `{A,B,C}` under the new one and
    // re-links by token-set intersection — the set difference even names the
    // evicted device. That is a deliberate, recorded trade (ADR-0043 § crux),
    // not an oversight: closing it needs per-device-blinded routing ids
    // (G-36 residual 1).
    //
    // ## Rotate on REMOVAL only, never on add
    //
    // An added member is *supposed* to route —
    // ``redistribute(accountIdB64:groupIdHex:broadcast:)`` already hands it
    // the current secret. Rotating on add would also turn that redistribute
    // into a CHANGED secret against every existing member's stored one on
    // every add, for zero privacy gain.

    /// Verdict of the pure ``membershipRotationDecision(previousMemberKeys:currentMemberKeys:ownRole:)``
    /// core.
    public enum PushRotationDecision: Equatable {
        /// Membership SHRANK and this device may rotate.
        case rotate
        /// Membership grew or is unchanged — an add / role change / ordinary
        /// commit. The routing id stays stable, which is the whole point of
        /// rotating only at removals.
        case skipNoShrink
        /// No usable before/after snapshot (first observation of the group,
        /// or a `members()` read that threw). Never rotate on an unknown
        /// delta — a missing snapshot would otherwise read as "everyone
        /// departed".
        case skipUnknownMembership
        /// A shrink, but this device is a plain member (or the group has no
        /// role table). Rotating is owner-or-admin only.
        case skipNotAuthorized
    }

    /// Outcome of the rotation orchestration — the decision values plus what
    /// the IO half resolved to.
    public enum PushRotationOutcome: Equatable {
        /// Minted, persisted, broadcast, re-subscribed, old id retired.
        case rotated
        /// This device holds no READABLE secret, so there is nothing to
        /// ROTATE. Minting is owner-only and lives in
        /// ``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``;
        /// rotation deliberately never mints a first secret, so an admin can
        /// never become a second minter and diverge the group's `push_gid`.
        ///
        /// Covers the unreadable row too (no device wrap key / AEAD open
        /// failed): the OLD id could never be named from bytes we cannot
        /// read, so rotating would strand it subscribed forever.
        case skipNoSecret
        /// See ``PushRotationDecision/skipNoShrink``.
        case skipNoShrink
        /// See ``PushRotationDecision/skipUnknownMembership``.
        case skipUnknownMembership
        /// See ``PushRotationDecision/skipNotAuthorized``.
        case skipNotAuthorized
    }

    /// Stable per-member identities for the membership diff: the
    /// lowercase-hex Ed25519 signature public keys from `Group.members()`.
    ///
    /// Keys, NOT leaf indices: OpenMLS reuses a vacated leaf for the next
    /// Add, so a remove-then-add pair can leave the index set identical while
    /// the membership genuinely changed. The signature key is stable for a
    /// member's lifetime in the group.
    ///
    /// Returns `nil` when the snapshot cannot be read (a torn group), which
    /// the decision core treats as "unknown", never as "everybody left".
    public nonisolated static func memberSignatureKeys(group: Group) -> [String]? {
        guard let roster = try? group.members() else { return nil }
        return roster.map { hex($0.signaturePublicKey) }
    }

    /// Pure decision core — did the membership SHRINK, and may this device
    /// rotate? No IO, so the whole matrix is directly testable (and is the
    /// verbatim port of the web `membershipRotationDecision`, like the
    /// ADR-0037 `restoreVerdict` gate).
    ///
    /// "Shrank" = at least one member present in the previous snapshot is
    /// absent from the current one. Deliberately not a count comparison: a
    /// poll batch can carry a Remove AND an Add, which leaves the count equal
    /// while a member was evicted — and that member must still lose its
    /// routing id.
    ///
    /// The authority check is owner-OR-admin (ADR-0043 ratified option (b)),
    /// matching who ADR-0022 lets commit the Remove in the first place. A
    /// `nil` role (no role table) does NOT rotate: such a group has no owner,
    /// so nobody ever minted a secret for it and there is nothing to rotate —
    /// and letting every member of an un-owned group rotate would have them
    /// each mint divergent secrets.
    ///
    /// - Parameters:
    ///   - previousMemberKeys: member keys observed BEFORE the commits
    ///     landed, or `nil` if unknown.
    ///   - currentMemberKeys: member keys observed AFTER, or `nil` if unknown.
    ///   - ownRole: this device's ADR-0022 role; `nil` = no role table.
    public nonisolated static func membershipRotationDecision(
        previousMemberKeys: [String]?,
        currentMemberKeys: [String]?,
        ownRole: Role?
    ) -> PushRotationDecision {
        guard let before = previousMemberKeys, let after = currentMemberKeys else {
            return .skipUnknownMembership
        }
        // A first observation has no baseline to diff against.
        if before.isEmpty { return .skipUnknownMembership }
        let present = Set(after)
        guard before.contains(where: { !present.contains($0) }) else { return .skipNoShrink }
        guard ownRole == .owner || ownRole == .admin else { return .skipNotAuthorized }
        return .rotate
    }

    /// ADR-0043 — re-mint the group's `push_routing_secret`, redistribute it,
    /// and retire the superseded `push_gid`.
    ///
    /// **The caller MUST have already published the Remove commit.** The
    /// envelope is an MLS application message: sealed at epoch N
    /// (pre-removal) the evicted member is still a member of that epoch and
    /// can OPEN it, learning the new secret and defeating the whole change.
    /// Sealed at N+1 it is unreadable to the removed leaf — RFC 9420 forward
    /// secrecy is exactly the property doing the work here. This is a hard
    /// sequencing requirement, not an optimisation.
    ///
    /// The steps, in the one order that works:
    ///
    ///   1. read the OLD secret and 2. derive the OLD `push_gid` — **before**
    ///      the store write, because the secret row is keyed
    ///      `(account, group)` and is clobbered in place. Persist first and
    ///      the id being retired can no longer be named: an undeletable
    ///      orphan that keeps receiving wakes, i.e. the exact defect being
    ///      fixed;
    ///   3. mint the NEW secret and 4. persist it `distributed: false`;
    ///   5. broadcast, marking distributed only once the send lands. That
    ///      flag is the whole durability mechanism, reused verbatim from the
    ///      mint path: a failed rotation broadcast leaves the row PENDING,
    ///      and the re-broadcast branch of
    ///      ``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``
    ///      — open to ANY holder — re-sends the stored (i.e. the new) secret
    ///      on the next chat-open / launch. Without it, a failed rotation
    ///      broadcast strands the whole group: the rotator alone on the new
    ///      id, everyone else on the old one, nobody notifying either;
    ///   6. subscribe under the NEW id, then 7./8. retire the OLD one
    ///      (``retireOldPushGid(oldSecret:)``) — in that order, so the device
    ///      is never unsubscribed from both at once.
    ///
    /// Rotation NEVER mints a first secret: with no secret held this is a
    /// no-op (``PushRotationOutcome/skipNoSecret``). Minting stays owner-only
    /// in ``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``,
    /// so widening rotation to admins does not create a second minter.
    ///
    /// Never throws — push is best-effort and a rotation failure must not
    /// unwind the membership change that triggered it.
    @discardableResult
    public func rotateSecret(
        accountIdB64: String,
        groupIdHex: String,
        ownRole: Role?,
        broadcast: @escaping (Data) async throws -> Void
    ) async -> PushRotationOutcome {
        guard ownRole == .owner || ownRole == .admin else { return .skipNotAuthorized }
        let lockKey = accountIdB64 + "\u{0}" + groupIdHex
        // Coalesce: a second trigger for the same group AWAITS the in-flight
        // rotation instead of starting a second one (two mints would strand
        // the first new push_gid subscribed-but-never-notified).
        if let inflightTask = rotating[lockKey] { return await inflightTask.value }

        let task = Task { @MainActor [weak self] () -> PushRotationOutcome in
            guard let self else { return .skipNoSecret }
            return await self.performRotation(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, broadcast: broadcast)
        }
        rotating[lockKey] = task
        let outcome = await task.value
        rotating.removeValue(forKey: lockKey)
        return outcome
    }

    /// Body of ``rotateSecret(accountIdB64:groupIdHex:ownRole:broadcast:)``,
    /// past the authority gate + the in-flight lock. Steps 1-8 of the ADR.
    private func performRotation(
        accountIdB64: String,
        groupIdHex: String,
        broadcast: (Data) async throws -> Void
    ) async -> PushRotationOutcome {
        // (1)+(2) — the OLD secret, read BEFORE anything overwrites it. The
        // gid is derived from these bytes at teardown time, in
        // `retireOldPushGid`. An absent OR unreadable row cannot be rotated:
        // the old id could never be named, so a rotation would strand it
        // subscribed forever.
        guard let oldSecret = store.getSecret(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex) else {
            return .skipNoSecret
        }

        // (3)+(4) — mint + persist PENDING. Persist before broadcasting for
        // the same reason the mint path does: two divergent random secrets
        // are worse than two identical envelopes.
        guard let next = Self.mintSecret() else { return .skipNoSecret }
        guard persistSecret(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, secret: next,
            distributed: false) != nil else {
            return .skipNoSecret
        }

        // (5) — redistribute at the POST-removal epoch. Failure leaves the
        // row pending for the existing any-holder retry branch; it must NOT
        // abort the local switch, which is already durable.
        await broadcastAndSettle(
            secret: next, accountIdB64: accountIdB64, groupIdHex: groupIdHex,
            what: "rotation", broadcast: broadcast)

        // (6) — arm the NEW id first, so a failure between the two never
        // leaves this device subscribed to nothing.
        await subscribeUnderPushGidBestEffort(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex)

        // (7)+(8) — retire the OLD id: unsubscribe this device's row, drop
        // the stale reverse-index entry.
        await retireOldPushGid(oldSecret: oldSecret)
        return .rotated
    }

    /// ADR-0043 — the WIRE-OBSERVED trigger: rotate when this device notices
    /// that the membership shrank, i.e. when SOMEONE ELSE committed the
    /// Remove.
    ///
    /// A Commit surfaces from the poll loop with no membership delta attached
    /// (surfacing removed leaves from `lakko-mls` would be exact, but it is
    /// an invariant-#3 change to the MLS state machine and needs its own
    /// ADR), so the removal is INFERRED from a `members()` snapshot taken
    /// either side of the poll batch. The snapshot is cheap to read, so the
    /// diff is effectively free.
    ///
    /// The device that performed the removal itself does NOT come through
    /// here: it applied its own commit locally before the poll, so the next
    /// poll's baseline is already post-removal and shows no shrink. It
    /// rotates directly instead, via
    /// ``rotateSecret(accountIdB64:groupIdHex:ownRole:broadcast:)``, right
    /// after its Remove commit is published.
    ///
    /// Never throws.
    @discardableResult
    public func rotateIfMembershipShrank(
        accountIdB64: String,
        groupIdHex: String,
        ownRole: Role?,
        previousMemberKeys: [String]?,
        currentMemberKeys: [String]?,
        broadcast: @escaping (Data) async throws -> Void
    ) async -> PushRotationOutcome {
        switch Self.membershipRotationDecision(
            previousMemberKeys: previousMemberKeys,
            currentMemberKeys: currentMemberKeys,
            ownRole: ownRole
        ) {
        case .skipNoShrink: return .skipNoShrink
        case .skipUnknownMembership: return .skipUnknownMembership
        case .skipNotAuthorized: return .skipNotAuthorized
        case .rotate:
            return await rotateSecret(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, ownRole: ownRole,
                broadcast: broadcast)
        }
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
