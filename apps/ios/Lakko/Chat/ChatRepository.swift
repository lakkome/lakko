// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// In-memory + on-disk state holder for the user's MLS chats.
///
/// Mirror of:
///
///   * `apps/web/src/chat/ChatContext.tsx` (1.17f4a)
///   * `apps/android/.../chat/ChatRepository.kt` (1.15f4a)
///
/// **Owns the canonical ``Identity``** for the current session —
/// built once via ``bootstrap(secrets:)`` from the OPAQUE export
/// key, kept alive until ``clear()`` / ``forgetDevice()``. Every
/// chat operation threads this same handle through ``ChatClient``
/// so MLS state mutations accumulate on a single OpenMLS provider.
/// Throwaway `Identity.fromSeed` per op (the simpler shape that
/// ``ContactsRepository`` uses for its stateless sign / decrypt
/// surface) would lose every ratchet advance.
///
/// **Holds a per-group `Group` handle map** keyed by hex group id.
/// The Group instances carry MLS ratchet state; dropping them
/// mid-session would force a re-bootstrap from the snapshot. The
/// map itself is `@MainActor`-isolated, so no two callers ever
/// mutate it at literally the same instant. That is NOT the same as
/// serialization: `@MainActor` isolation is released at every
/// `await`, so two `async` callers interleave freely at their
/// suspension points. The four group-membership create-commit paths,
/// and the APPLY half of the poll (G-38), therefore run inside an
/// explicit per-group serializing chain — see
/// ``withCommitLock(groupIdHex:_:)``.
///
/// **Snapshot persistence** (1.14f5) runs after every
/// state-mutating call via ``IdentitySnapshotStore``. Failures
/// are logged-and-swallowed — the in-memory state is still
/// correct; the next mutation will retry the persist naturally.
///
/// ## Lifecycle
///
///   * ``bootstrap(secrets:)`` is called by
///     ``AuthRepository/signup(username:password:)`` /
///     `login` / `recover` once the session's ``IdentitySecrets``
///     are known. It builds the canonical Identity from the export
///     key and restores the on-disk snapshot if one exists.
///     Idempotent for the same account id; replaces the Identity
///     if called for a different account.
///   * ``clear()`` is called by ``AuthRepository/logout()`` —
///     drops the in-memory state but keeps the on-disk snapshot
///     envelope intact, so the next login on this device picks up
///     where we left off.
///   * ``forgetDevice()`` is called by
///     ``AuthRepository/forgetDevice()`` — additionally wipes the
///     on-disk snapshot envelope.
@MainActor
public final class ChatRepository: ObservableObject {

    // MARK: - Published state

    @Published public private(set) var state: ChatRepositoryState = ChatRepositoryState()

    // MARK: - Wiring

    private let chatClient: ChatClient
    private let dsClient: DsClient
    private let discovery: DiscoveryClient
    private let snapshotStore: IdentitySnapshotStore
    private let knownGroupsStore: KnownGroupsStore
    /// Phase 2a3f1 — persistent plaintext-message log.
    /// Survives app launches; mutated on every append /
    /// poll / send.
    private let messageLogStore: MessageLogStore
    /// ADR-0040 (G-35) — durable pending-commit outbox. The four
    /// group-membership commit paths write a `PendingCommitEntry`
    /// (commit + Welcome / rendezvous bytes) here BEFORE they
    /// publish, and the first `pollGroup` of a session sweeps it to
    /// re-publish any commit that crashed mid-window. See
    /// ``persistCommitEntryThenSnapshotOrThrow(_:)`` +
    /// ``sweepPendingCommits(record:)``.
    private let commitOutbox: CommitOutboxStore

    /// ADR-0042 — the opaque per-group push routing orchestrator. Owns the
    /// mint (owner-only) / receive / derive / subscribe-under-`push_gid`
    /// logic + the device-key-encrypted secret store + the App Group
    /// plaintext `push_gid → (account, group_id)` reverse index.
    private let pushRouting: PushGroupRouting

    /// The canonical ``Identity`` for the current session. `nil`
    /// until ``bootstrap(secrets:)`` populates it.
    private var canonicalIdentity: Identity?

    /// Account id (base64) of the session that bootstrapped this
    /// repository.
    private var currentAccountIdB64: String?

    /// Map of hex group id → live ``Group`` handle + per-thread
    /// timeline / cursor. Read + mutated only on `@MainActor`, and every
    /// UniFFI `Group` call is synchronous, so an individual access is never
    /// torn. Membership commits additionally serialize on
    /// ``withCommitLock(groupIdHex:_:)`` because their hazard spans an
    /// `await`, which `@MainActor` does not cover.
    ///
    /// A record is REPLACED (not mutated) by
    /// ``resetGroupToDurableSnapshot(groupIdHex:)``, so any code that waits
    /// on an `await` must re-read this map afterwards rather than hold the
    /// `GroupRecord` it captured before the wait.
    private var groups: [String: GroupRecord] = [:]

    /// ADR-0040 (G-35) — per-group chain tail: the **serializing** lock the
    /// four group-membership create-commit paths run inside, joined by the
    /// poll's apply half in G-38. Swift counterpart of the Android
    /// `ChatRepository` per-group mutex — the same lock its `runSplitPoll`
    /// takes — keyed per group so two different conversations never block each
    /// other.
    ///
    /// The value is the tail `Task` the next caller chains behind, type-erased
    /// to `Void` so one queue can hold sections returning different things.
    /// Same construction as ``PushGroupRouting`` `routingChain` (ADR-0044
    /// C-62), for the same reason: a read-decide-write that spans an `await`
    /// needs a lock, not actor isolation.
    ///
    /// **Serializing, not coalescing.** A successor WAITS and then runs in
    /// full against freshly-read state; it is never folded into the section
    /// already running. Folding was ADR-0043 finding F1 on the push path, and
    /// here it would be worse than useless — two membership operations answer
    /// two DIFFERENT intents (add Alice, remove Bob), so a fold would silently
    /// drop one.
    private var commitChain: [String: Task<Void, Never>] = [:]

    /// ADR-0040 (G-35) — hex group ids already swept this session, so the
    /// pending-commit outbox sweep runs at most once per group per session (on
    /// the first ``pollGroup(groupIdHex:waitSecs:)`` for that group). Reset by
    /// ``wipeInMemoryState()`` on a logout / account switch.
    private var sweptGroupsThisSession: Set<String> = []

    public init(
        chatClient: ChatClient = ChatClient(),
        dsClient: DsClient = DsClient(),
        discovery: DiscoveryClient = DiscoveryClient(),
        snapshotStore: IdentitySnapshotStore = IdentitySnapshotStore(),
        knownGroupsStore: KnownGroupsStore = KnownGroupsStore(),
        messageLogStore: MessageLogStore = MessageLogStore(),
        commitOutbox: CommitOutboxStore = CommitOutboxStore(),
        pushRouting: PushGroupRouting = PushGroupRouting()
    ) {
        self.chatClient = chatClient
        self.dsClient = dsClient
        self.discovery = discovery
        self.snapshotStore = snapshotStore
        self.knownGroupsStore = knownGroupsStore
        self.messageLogStore = messageLogStore
        self.commitOutbox = commitOutbox
        self.pushRouting = pushRouting
        // SECURITY (nonce-reuse-on-crash) — wire the fail-closed MLS
        // ratchet persister into the ChatClient so EVERY application send
        // (chat / edit / delete / reaction / typing / read-receipt /
        // call-signaling — they all ratchet via `createApplicationMessage`)
        // durably persists the post-advance (N+1) ratchet BEFORE the DS
        // publish, and aborts the send if it cannot. See
        // ``persistSnapshotDurableOrThrow()``.
        self.chatClient.ratchetPersister = { [weak self] in
            guard let self else {
                throw ChatError.argument(
                    "ChatRepository deallocated — send aborted before publish"
                )
            }
            try await self.persistSnapshotDurableOrThrow()
        }
    }

    // MARK: - Lifecycle

    /// Build the canonical Identity for the session from the
    /// OPAQUE export key + restore any on-disk MLS snapshot.
    ///
    /// Idempotent for the same account id (early-returns).
    /// Switching accounts (different `secrets.accountId`) wipes the
    /// in-memory state first.
    ///
    /// Failures from snapshot restore are logged-and-swallowed:
    /// a corrupted / stale envelope shouldn't block the user from
    /// continuing — they just start with an empty conversation
    /// list on this device, and peers re-bootstrap via fresh
    /// KeyPackages.
    public func bootstrap(secrets: IdentitySecrets) {
        let newAccountIdB64 = base64Encode(secrets.accountId)
        if currentAccountIdB64 == newAccountIdB64 { return }
        if currentAccountIdB64 != nil {
            wipeInMemoryState()
        }
        let identity: Identity
        do {
            // ADR-0017: the canonical AccountIdentity is seeded from the
            // phrase-derived seed, NOT the OPAQUE export key — so a
            // routine recover / password change preserves the identity.
            identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
        } catch {
            // Identity construction is deterministic + can't fail
            // unless the seed bytes are malformed (UniFFI
            // throws on wrong length). Surface as a no-op: the
            // state stays empty + the next login retries.
            return
        }

        let restored: Bool
        do {
            if let blob = snapshotStore.getSnapshot(accountIdB64: newAccountIdB64) {
                try identity.restoreStorage(snapshot: blob)
                restored = true
            } else {
                restored = false
            }
        } catch {
            restored = false
        }

        canonicalIdentity = identity
        currentAccountIdB64 = newAccountIdB64

        // Eagerly rebuild `Group` handles for every previously-
        // known group id (1.14f5). `restoreStorage` laid down
        // OpenMLS provider storage; without this loop the in-
        // memory map would still be empty + peers' commits to
        // those groups would miss `pollGroup`'s lookup until the
        // user re-opened each thread.
        if restored {
            for groupIdHex in knownGroupsStore.load(accountIdB64: newAccountIdB64) {
                guard let groupId = Self.hexToBytes(groupIdHex) else { continue }
                do {
                    guard let group = try loadGroup(
                        identity: identity,
                        groupId: groupId
                    ) else {
                        continue
                    }
                    groups[groupIdHex] = GroupRecord(group: group, groupId: groupId)
                    // ADR-0042 legacy-sweep — provision opaque push routing
                    // for every restored group. Owner + no secret yet → mint
                    // + persist + distribute the envelope + subscribe; a
                    // member holding a secret → subscribe under `push_gid`;
                    // anyone else → defer (no push under `group_id`).
                    // Idempotent + race-safe (shared in-flight guard with the
                    // chat view's onAppear). Best-effort.
                    ensurePushRouting(groupIdHex: groupIdHex)
                } catch {
                    // best-effort
                    continue
                }
            }
        }

        // Phase 2a3f1 — restore the plaintext message log on
        // top of the rehydrated Group handles. The two stores
        // are independent (MLS state ⊥ message timeline), so a
        // corrupt / missing log doesn't block the rest of the
        // bootstrap.
        if let log = messageLogStore.getLog(accountIdB64: newAccountIdB64) {
            for (groupIdHex, persisted) in log.groups {
                guard let record = groups[groupIdHex] else { continue }
                let messages = persisted.messages.compactMap { $0.toChatMessage() }
                record.seed(messages: messages, cursor: persisted.cursor)
            }
        }

        state = ChatRepositoryState(
            threads: buildThreadList(),
            bootstrapped: true
        )
    }

    /// Clear the in-memory state for the current session. Keeps
    /// the on-disk MLS snapshot intact — ``forgetDevice()`` is
    /// the full-wipe path.
    public func clear() {
        wipeInMemoryState()
        currentAccountIdB64 = nil
        canonicalIdentity = nil
        state = ChatRepositoryState()
    }

    /// Full wipe: clear in-memory state + delete the on-disk
    /// snapshot envelope.
    public func forgetDevice() {
        let accountIdB64 = currentAccountIdB64
        clear()
        if let accountIdB64 = accountIdB64 {
            snapshotStore.deleteSnapshot(accountIdB64: accountIdB64)
            knownGroupsStore.clear(accountIdB64: accountIdB64)
            // ADR-0040 (G-35) — drop any pending-commit outbox entries for the
            // account alongside the snapshot they were paired with, so a wiped
            // device leaves no stale commit to be swept later.
            commitOutbox.deleteOutboxForAccount(accountIdB64: accountIdB64)
            // ADR-0042 — wipe the push-routing secrets + the `push_gid`
            // reverse index so a forgotten device leaves no routing residue.
            pushRouting.clear(accountIdB64: accountIdB64)
            // Phase 2a3f1 — drop the plaintext message log too.
            // The Keychain wrap key is shared across accounts
            // (per-installation, not per-account) so we leave
            // it intact; only the per-account envelope file
            // gets removed.
            messageLogStore.deleteLog(accountIdB64: accountIdB64)
        }
    }

    private func wipeInMemoryState() {
        groups.removeAll()
        canonicalIdentity = nil
        // ADR-0040 (G-35) — a new session must re-sweep the outbox for every
        // group on its first poll.
        sweptGroupsThisSession.removeAll()
        // `commitChain` is deliberately NOT cleared. Dropping a tail does not
        // stop the section behind it — it only makes the next caller start
        // BESIDE that section, which is the race the chain exists to close.
        // Queued sections re-read `canonicalIdentity` / `groups` (now empty)
        // and fail their own guards, and the tail-guarded release in
        // ``withCommitLock(groupIdHex:_:)`` drains the map on its own.
    }

    // MARK: - Group operations

    /// Start a 1×1 MLS group with `recipientAccountId`.
    ///
    /// Steps:
    ///   1. Fetch the recipient's KeyPackage from the discovery
    ///      directory.
    ///   2. `Group.create(identity:)` — fresh group with us as
    ///      the founding member.
    ///   3. `group.addMemberFromWire(identity:keyPackage:)` —
    ///      produces a `(commit, welcome)` pair. Per MLS the
    ///      commit would be broadcast, but our group is
    ///      one-of-one before this call, so we drop it.
    ///   4. Wrap the welcome with `.welcome` kind and post it to
    ///      the recipient's stranger inbox via
    ///      ``DsClient/publishMailbox(pseudonym:payload:)`` keyed
    ///      by `IdentityPseudonym.fromIdentity(pubkey:)`.
    ///   5. Persist the MLS snapshot.
    ///
    /// Returns the new group's hex id so the caller can hand it
    /// straight to the Chat UI route.
    public func startGroupWith(
        recipientAccountId: Data,
        recipientSignaturePubkey: Data
    ) async throws -> String {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument(
                "ChatRepository not bootstrapped — call bootstrap() first"
            )
        }
        guard let keyPackageBytes = try await discovery.fetchKeyPackage(
            accountId: recipientAccountId
        ) else {
            throw ChatError.argument(
                "recipient has no KeyPackage uploaded yet"
            )
        }

        let group: Group
        let addResult: AddMember
        do {
            group = try Group.create(identity: identity)
            addResult = try group.addMemberFromWire(
                identity: identity,
                keyPackage: keyPackageBytes
            )
        } catch {
            throw ChatError.wrapped(
                "addMemberFromWire failed",
                underlying: error
            )
        }

        let welcomeEnvelope = wrapEnvelope(kind: .welcome, body: addResult.welcome)
        let recipientPseudonym: Data
        do {
            recipientPseudonym = try IdentityPseudonym
                .fromIdentity(pubkey: recipientSignaturePubkey)
                .asBytes()
        } catch {
            throw ChatError.wrapped(
                "IdentityPseudonym derivation failed",
                underlying: error
            )
        }

        do {
            _ = try await dsClient.publishMailbox(
                pseudonym: recipientPseudonym,
                payload: welcomeEnvelope
            )
        } catch {
            throw ChatError.wrapped(
                "DS publishMailbox (Welcome) failed",
                underlying: error
            )
        }

        let groupId = try group.groupId()
        let groupIdHex = DsClient.pseudonymHex(groupId)
        groups[groupIdHex] = GroupRecord(group: group, groupId: groupId)
        persistKnownGroupBestEffort(groupIdHex: groupIdHex)
        persistSnapshotBestEffort()
        state = state.with(threads: buildThreadList())
        // ADR-0042 — subscribe under `push_gid` (defers until this device
        // holds the group's `push_routing_secret`; never subscribes under
        // the DS `group_id`). The owner mints the secret on chat-open
        // (`ensurePushRouting`); until then this is a no-op deferral.
        subscribeGroupUnderPushGid(groupIdHex: groupIdHex)
        return groupIdHex
    }

    /// Join an MLS group from a `Welcome` blob.
    ///
    /// Returns the joined group's hex id.
    public func joinGroupFromWelcome(welcomeBytes: Data) async throws -> String {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument(
                "ChatRepository not bootstrapped — call bootstrap() first"
            )
        }
        let group: Group
        do {
            group = try Group.joinFromWelcome(
                welcome: welcomeBytes,
                identity: identity
            )
        } catch {
            throw ChatError.wrapped(
                "joinFromWelcome failed",
                underlying: error
            )
        }
        let groupId = try group.groupId()
        let groupIdHex = DsClient.pseudonymHex(groupId)
        groups[groupIdHex] = GroupRecord(group: group, groupId: groupId)
        persistKnownGroupBestEffort(groupIdHex: groupIdHex)
        persistSnapshotBestEffort()
        state = state.with(threads: buildThreadList())
        // ADR-0042 — subscribe under `push_gid` (defers until the joiner
        // receives the group's `push_routing_secret` over the distribution
        // envelope on its next poll; never subscribes under `group_id`).
        subscribeGroupUnderPushGid(groupIdHex: groupIdHex)
        return groupIdHex
    }

    /// ADR-0028 (G-3) — joiner step 1: request to join a group from a `/g/`
    /// link capability (the wire bytes the slug base64url-decodes to). Decodes
    /// + verifies the capability fail-closed (before any post), reads the
    /// group's current epoch from the journal head's public MLS framing
    /// (`epochOfMessage`, no group secret), crafts an external-join proposal
    /// (ADR-0027) for that epoch, and posts it to the group journal. Returns the
    /// group id + the joiner's signature key — the caller drives
    /// `pollForJoinWelcome` until an admin approves and the Welcome arrives.
    public func requestToJoinFromLink(
        capabilityWire: Data,
        nowMs: UInt64,
        blobId: Data? = nil
    ) async throws -> (groupId: Data, joinerSignatureKey: Data) {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        let cap: GroupInviteCapability
        do {
            cap = try GroupInviteCapability.decodeAndVerify(wire: capabilityWire)
        } catch {
            throw ChatError.wrapped("group invite link decode/verify failed", underlying: error)
        }
        guard cap.isValidNow(nowMs: nowMs) else {
            throw ChatError.argument("group invite link has expired")
        }
        // ADR-0028 OQ1 — when the link was a server-backed pointer, register one
        // consume against the ledger's soft `max_uses` ceiling before posting
        // the request (anti-abuse defence-in-depth; the admin approval gate
        // remains the real boundary). A repeat from this joiner is idempotent.
        // Inline links (no `blobId`) skip this — there is no ledger row.
        if let blobId,
           let accountIdB64 = currentAccountIdB64,
           let joinerAccountId = base64Decode(accountIdB64) {
            let outcome: ConsumeInviteResult
            do {
                outcome = try await discovery.consumeGroupInvite(
                    blobId: blobId,
                    capabilityBytes: capabilityWire,
                    joinerAccountId: joinerAccountId
                )
            } catch {
                throw ChatError.wrapped("group invite consume failed", underlying: error)
            }
            switch outcome {
            case .notFound:
                throw ChatError.argument("group invite not found or expired")
            case .exhausted:
                throw ChatError.argument("group invite has reached its use limit")
            case .consumed, .alreadyConsumed:
                break
            }
        }
        let groupId = cap.groupId()
        // Read the current epoch from the journal head (public framing; a
        // non-member can do this). Empty / unreachable journal ⇒ epoch 0.
        var epoch: UInt64 = 0
        if let raw = try? await dsClient.fetchGroup(groupId: groupId, after: 0, waitSecs: 0) {
            for m in raw.messages {
                if let e = epochOfMessage(bytes: m.payload) { epoch = e }
            }
        }
        let proposal: Data
        do {
            proposal = try identity.createExternalJoinProposal(groupId: groupId, epoch: epoch)
        } catch {
            throw ChatError.wrapped("createExternalJoinProposal failed", underlying: error)
        }
        do {
            _ = try await dsClient.publishGroup(groupId: groupId, payload: proposal)
        } catch {
            throw ChatError.wrapped(
                "DS publishGroup (external-join proposal) failed", underlying: error)
        }
        // ADR-0028 OQ2 — post a join-attribution envelope so the approving admin
        // can see which member's link this request arrived on. Best-effort +
        // advisory: a failure must not block the join (the admin approval gate is
        // the security boundary).
        if let attribution = try? composeGroupJoinAttribution(
            joinerSigningSeed: identity.ed25519Seed(),
            capabilityBytes: capabilityWire
        ) {
            _ = try? await dsClient.publishGroup(groupId: groupId, payload: attribution)
        }
        return (groupId, identity.signaturePublicKey())
    }

    /// ADR-0028 (G-3) — joiner step 2: poll the group journal for the
    /// rendezvous Welcome an admin posted after approving our request, and join
    /// from it. Run on a poll loop after `requestToJoinFromLink`. Returns the
    /// joined group's hex id on the first rendezvous addressed to our signature
    /// key, or `nil` if no Welcome has arrived yet (keep polling).
    public func pollForJoinWelcome(
        groupId: Data,
        joinerSignatureKey: Data
    ) async throws -> String? {
        let raw: FetchResult
        do {
            raw = try await dsClient.fetchGroup(groupId: groupId, after: 0, waitSecs: 0)
        } catch {
            throw ChatError.wrapped("DS fetchGroup (join poll) failed", underlying: error)
        }
        for m in raw.messages {
            guard let rendezvous = try? parseWelcomeRendezvous(bytes: m.payload) else { continue }
            if rendezvous.joinerSignatureKey == joinerSignatureKey {
                return try await joinGroupFromWelcome(welcomeBytes: rendezvous.welcome)
            }
        }
        return nil
    }

    /// Compose `(plaintext, attachments)` into the canonical
    /// 1.19a JSON envelope, encrypt for the current epoch of
    /// `groupIdHex`, and post the wire bytes to the DS. Appends
    /// the message to the in-memory timeline + persists the
    /// snapshot.
    ///
    /// Phase 1.19c3: empty `attachments` ⇒ text-only envelope
    /// (binary-identical to a `composeEnvelope(text: …,
    /// attachments: [])` call, which the 1.19a parser also
    /// accepts as a v=1 message). Empty `plaintext` is valid
    /// when at least one attachment is present (attachment-only
    /// message). Empty-and-empty is rejected at the call site
    /// upstream — the view model and screen layer enforce that.
    /// G3-1-call-key — derive the per-call SFrame seed + the
    /// local leaf index for a call inside `groupIdHex`.
    ///
    /// The seed is the MLS exporter output
    /// `export_secret(label = "lakko/call/v1", context = call_id,
    /// length = 32)` per audit-pack invariant C-1 — a
    /// deterministic function of the group's current epoch
    /// secret, so both the offerer and the answerer (same group,
    /// same `callId`, same epoch) derive **identical** seed
    /// bytes; the per-sender SFrame key then forks on each
    /// member's leaf index. Returns `nil` (call must refuse per
    /// security invariant #4 — never fall back to non-E2EE) when
    /// the repository is not bootstrapped, the group handle is
    /// missing, or the exporter call fails (e.g. epoch skew).
    ///
    /// The derivation lives here, in the layer that owns the
    /// MLS `Group` + `Identity`, rather than in the view model.
    public func deriveCallKey(
        groupIdHex: String,
        callId: Data
    ) -> (seed: Data, localLeaf: UInt32)? {
        guard let identity = canonicalIdentity else {
            NSLog("deriveCallKey: ChatRepository not bootstrapped")
            return nil
        }
        guard let record = groups[groupIdHex] else {
            NSLog("deriveCallKey: no group handle for %@", groupIdHex)
            return nil
        }
        do {
            let seed = try record.group.exportSecret(
                identity: identity,
                label: "lakko/call/v1",
                context: callId,
                length: 32
            )
            let leaf = try record.group.ownLeafIndex()
            return (seed, leaf)
        } catch {
            NSLog("deriveCallKey: exporter failed: %@", String(describing: error))
            return nil
        }
    }

    /// Number of members in the live MLS group for `groupIdHex`, or
    /// `nil` if there is no handle. Used by the call surface to route a
    /// >2-member chat through the SFU group-call path and a 2-member
    /// chat through the 1×1 P2P path.
    public func memberCount(groupIdHex: String) -> Int? {
        guard let record = groups[groupIdHex] else { return nil }
        return (try? record.group.memberCount()).map { Int($0) }
    }

    /// Current MLS epoch of the live group for `groupIdHex`, or `nil` when there
    /// is no handle. Exposed so the ADR-0040 (G-35) F1 reset can be asserted —
    /// after a commit persist-abort the in-memory group must read back at the
    /// pre-commit epoch N (not the eager-merged N+1).
    public func epoch(groupIdHex: String) -> UInt64? {
        guard let record = groups[groupIdHex] else { return nil }
        return try? record.group.epoch()
    }

    /// Roster of the live MLS group for `groupIdHex` — `(leafIndex,
    /// signaturePublicKey)` per member at the current epoch — or `nil` when
    /// there is no handle. The group-chat UI matches each `signaturePublicKey`
    /// against the accepted-contacts list to render a name, else a fingerprint.
    public func members(groupIdHex: String) -> [GroupMember]? {
        guard let record = groups[groupIdHex] else { return nil }
        return try? record.group.members()
    }

    /// Our own leaf index in `groupIdHex`, or `nil` when there is no handle.
    /// The roster UI uses it to mark the local member "(you)" and to refuse
    /// removing ourselves.
    public func ownLeafIndex(groupIdHex: String) -> UInt32? {
        guard let record = groups[groupIdHex] else { return nil }
        return try? record.group.ownLeafIndex()
    }

    /// Our own ADR-0022 role in `groupIdHex` (`.owner` / `.admin` / `.member`),
    /// or `nil` for an open / legacy group with no role table (or no handle).
    /// The members UI gates the add / remove affordances on this.
    public func ownRole(groupIdHex: String) -> Role? {
        guard let identity = canonicalIdentity else { return nil }
        guard let record = groups[groupIdHex] else { return nil }
        return try? record.group.ownRole(identity: identity)
    }

    /// The group's ADR-0022 role table (`owner` + `admins` signature keys), or
    /// `nil` for an open / legacy group. The members UI uses it to label the
    /// per-member promote / demote toggle.
    public func groupRoles(groupIdHex: String) -> RoleTable? {
        guard let record = groups[groupIdHex] else { return nil }
        return try? record.group.groupRoles()
    }

    /// Promote / demote a member as admin (owner only — enforced on the
    /// receiving side). Reads the role table, adds or removes the member's
    /// signature key from the admin set, and publishes the `setGroupRoles`
    /// commit (own-marked for echo suppression). Returns the commit's DS id.
    public func setMemberAdmin(
        groupIdHex: String,
        memberSignaturePubkey: Data,
        makeAdmin: Bool
    ) async throws -> Int64 {
        // ADR-0040 (G-35) — serialize the whole eager-merge-through-publish
        // region against every other membership commit on this group.
        try await withCommitLock(groupIdHex: groupIdHex) { [self] in
            try await setMemberAdminLocked(
                groupIdHex: groupIdHex,
                memberSignaturePubkey: memberSignaturePubkey,
                makeAdmin: makeAdmin
            )
        }
    }

    /// The ``setMemberAdmin(groupIdHex:memberSignaturePubkey:makeAdmin:)``
    /// critical section. Runs only under ``withCommitLock(groupIdHex:_:)`` and
    /// re-reads `canonicalIdentity` + `groups` itself — a predecessor may have
    /// replaced or evicted the record while this section was queued.
    private func setMemberAdminLocked(
        groupIdHex: String,
        memberSignaturePubkey: Data,
        makeAdmin: Bool
    ) async throws -> Int64 {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        guard let table = try? record.group.groupRoles() else {
            throw ChatError.argument("group has no role table")
        }
        var admins = table.admins.filter { $0 != memberSignaturePubkey }
        if makeAdmin {
            admins.append(memberSignaturePubkey)
        }
        let commitBytes: Data
        do {
            commitBytes = try record.group.setGroupRoles(
                identity: identity,
                owner: table.owner,
                admins: admins
            )
        } catch {
            throw ChatError.wrapped("setGroupRoles failed", underlying: error)
        }
        // ADR-0040 (G-35) F1 — the core `setGroupRoles` above eager-merged epoch
        // N -> N+1. Persist the outbox entry (commit only; a role change has no
        // Welcome) + the N+1 snapshot BEFORE the publish, FAIL-CLOSED. A throw
        // ABORTS the op (no publish) — the group is never advanced on the wire
        // without a durable, re-publishable record of this commit. There is NO
        // pre-persist artifact compute here (a role change has no Welcome), so the
        // reset-guarded `do` below trivially covers every post-merge statement —
        // the same F1 invariant the add / approve paths enforce by widening the
        // guard over their second-artifact compute.
        let entryId = newOutboxEntryId()
        do {
            try persistCommitEntryThenSnapshotOrThrow(
                NewCommitEntry(
                    id: entryId,
                    groupId: base64Encode(record.groupId),
                    targetEpoch: String(try record.group.epoch()),
                    commitB64: base64Encode(commitBytes)
                )
            )
        } catch {
            // ADR-0040 (G-35) F1 — the FAIL-CLOSED persist aborted AFTER the core
            // `setGroupRoles` eager-merged epoch N -> N+1 in memory. Roll the
            // in-memory group back to the durable snapshot (still N) so this abort
            // is "as if the op never happened" in memory too, then re-throw so the
            // UI sees the failure. Without the reset a second same-session commit
            // would build on the un-published N+1 and strand the whole group. See
            // ``resetGroupToDurableSnapshot(groupIdHex:)``.
            try resetGroupToDurableSnapshot(groupIdHex: groupIdHex)
            throw error
        }
        let commitPublish: PublishResult
        do {
            commitPublish = try await dsClient.publishGroup(
                groupId: record.groupId,
                payload: commitBytes
            )
        } catch {
            throw ChatError.wrapped("DS publishGroup (role change) failed", underlying: error)
        }
        chatClient.markOwnPublishedCommit(messageId: commitPublish.id)
        ackCommitBestEffort(entryId: entryId, commitMessageId: commitPublish.id)
        return commitPublish.id
    }

    /// Add an already-resolved recipient to an EXISTING group (mirror of
    /// ``startGroupWith`` but for a live group). The caller resolves +
    /// G-4-verifies `@username -> (accountId, signaturePubkey)` first (the
    /// pin-store verify lives in `ContactsClient`).
    ///
    /// Unlike the 1×1 create, the resulting commit MUST be published to the
    /// group journal so existing members advance their epoch, and marked as
    /// own so the poll loop suppresses the echo — re-applying our own commit
    /// would fork the MLS epoch. Returns the commit's DS message id.
    public func addMemberToGroup(
        groupIdHex: String,
        recipientAccountId: Data,
        recipientSignaturePubkey: Data
    ) async throws -> Int64 {
        // Fast-fail pre-checks, kept OUTSIDE the lock so an obviously-invalid
        // call reports the same error it always did without queueing. They are
        // re-run inside the locked section against fresh state — these two are
        // an early exit, not the authority.
        guard canonicalIdentity != nil else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard groups[groupIdHex] != nil else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        // The KeyPackage fetch touches NO MLS state, so it is deliberately
        // taken before the lock: it is a third-party network round-trip, and
        // holding a group's commit chain across it would stall unrelated
        // membership work on that group for its whole duration.
        guard let keyPackageBytes = try await discovery.fetchKeyPackage(
            accountId: recipientAccountId
        ) else {
            throw ChatError.argument("recipient has no KeyPackage uploaded yet")
        }

        // ADR-0040 (G-35) — serialize the whole eager-merge-through-publish
        // region against every other membership commit on this group.
        return try await withCommitLock(groupIdHex: groupIdHex) { [self] in
            try await addMemberToGroupLocked(
                groupIdHex: groupIdHex,
                recipientSignaturePubkey: recipientSignaturePubkey,
                keyPackageBytes: keyPackageBytes
            )
        }
    }

    /// The ``addMemberToGroup(groupIdHex:recipientAccountId:recipientSignaturePubkey:)``
    /// critical section. Runs only under ``withCommitLock(groupIdHex:_:)`` and
    /// re-reads `canonicalIdentity` + `groups` itself — a predecessor may have
    /// replaced or evicted the record while this section was queued.
    private func addMemberToGroupLocked(
        groupIdHex: String,
        recipientSignaturePubkey: Data,
        keyPackageBytes: Data
    ) async throws -> Int64 {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }

        let addResult: AddMember
        do {
            addResult = try record.group.addMemberFromWire(
                identity: identity,
                keyPackage: keyPackageBytes
            )
        } catch {
            throw ChatError.wrapped("addMemberFromWire failed", underlying: error)
        }

        // ADR-0040 (G-35) F1 — the core `addMemberFromWire` above eager-merged
        // epoch N -> N+1. EVERYTHING from here through the FAIL-CLOSED persist runs
        // INSIDE the reset-guarded `do` below — INCLUDING the second-artifact
        // compute — because ANY throw after the eager merge and before publish must
        // roll the in-memory group back to the durable N. A Welcome-envelope /
        // pseudonym compute that threw OUTSIDE this guard would leave the group at
        // the un-published N+1, so a later same-session commit builds on N+1 and
        // publishes commit(N+1 -> N+2), which peers still at N drop as wrong-epoch,
        // stranding the WHOLE group (the "device-ahead" hazard). `entryId` is
        // minted before the guard (non-throwing) so the post-publish ack can
        // reference it; the Welcome + pseudonym are deferred lets assigned inside
        // the guard and consumed after it at publish (the reset-`catch` always
        // throws, so reaching the publish implies both were assigned).
        let entryId = newOutboxEntryId()
        let welcomeEnvelope: Data
        let recipientPseudonym: Data
        do {
            // Compute the second artifact (the Welcome envelope) + its mailbox
            // destination so the outbox entry holds the EXACT bytes the restart
            // sweep will re-publish (no drift). The Welcome ships to the new
            // member's stranger inbox, addressed by their pubkey pseudonym.
            welcomeEnvelope = wrapEnvelope(kind: .welcome, body: addResult.welcome)
            do {
                recipientPseudonym = try IdentityPseudonym
                    .fromIdentity(pubkey: recipientSignaturePubkey)
                    .asBytes()
            } catch {
                throw ChatError.wrapped("IdentityPseudonym derivation failed", underlying: error)
            }
            // Persist the outbox entry (commit + Welcome, BOTH artifacts) + the N+1
            // snapshot BEFORE any publish, FAIL-CLOSED. A throw ABORTS the op (no
            // publish); the sweep re-publishes both artifacts on restart so a crash
            // between the two publishes never strands the new member without a
            // Welcome.
            try persistCommitEntryThenSnapshotOrThrow(
                NewCommitEntry(
                    id: entryId,
                    groupId: base64Encode(record.groupId),
                    targetEpoch: String(try record.group.epoch()),
                    commitB64: base64Encode(addResult.commit),
                    secondArtifactB64: base64Encode(welcomeEnvelope),
                    secondArtifactKind: .welcome,
                    secondArtifactMailboxB64: base64Encode(recipientPseudonym)
                )
            )
        } catch {
            // ADR-0040 (G-35) F1 — the artifact compute OR the FAIL-CLOSED persist
            // aborted AFTER the core `addMemberFromWire` eager-merged epoch
            // N -> N+1 in memory. Roll the in-memory group back to the durable
            // snapshot (still N) so this abort is "as if the op never happened" in
            // memory too, then re-throw so the UI sees the failure. Without the
            // reset a second same-session commit would build on the un-published
            // N+1 and strand the whole group. See
            // ``resetGroupToDurableSnapshot(groupIdHex:)``.
            try resetGroupToDurableSnapshot(groupIdHex: groupIdHex)
            throw error
        }

        // Publish artifact 1 — the commit to the group journal so existing
        // members advance their epoch.
        let commitPublish: PublishResult
        do {
            commitPublish = try await dsClient.publishGroup(
                groupId: record.groupId,
                payload: addResult.commit
            )
        } catch {
            throw ChatError.wrapped("DS publishGroup (add-member commit) failed", underlying: error)
        }
        chatClient.markOwnPublishedCommit(messageId: commitPublish.id)

        // Publish artifact 2 — the Welcome to the new member's mailbox (the SAME
        // bytes stored in the outbox entry).
        let welcomePublish: PublishResult
        do {
            welcomePublish = try await dsClient.publishMailbox(
                pseudonym: recipientPseudonym,
                payload: welcomeEnvelope
            )
        } catch {
            throw ChatError.wrapped("DS publishMailbox (Welcome) failed", underlying: error)
        }

        ackCommitBestEffort(
            entryId: entryId,
            commitMessageId: commitPublish.id,
            secondMessageId: welcomePublish.id
        )
        state = state.with(threads: buildThreadList())
        // ADR-0042 — re-broadcast the CURRENT push-routing secret so the
        // newly-added member learns it on their next poll (no-op if this
        // device holds no secret; idempotent for existing members). Runs
        // AFTER the add commit so the new member is already in the group.
        // Fire-and-forget; failures retry on the next add / launch sweep.
        redistributePushRoutingSecret(groupIdHex: groupIdHex)
        return commitPublish.id
    }

    /// Remove a member from an EXISTING group by leaf index. Publishes the
    /// removal commit to the group journal (own-marked for echo suppression).
    /// Returns the commit's DS message id.
    public func removeMemberFromGroup(
        groupIdHex: String,
        leafIndex: UInt32
    ) async throws -> Int64 {
        // Fast-fail pre-checks; the locked section re-runs them against fresh
        // state.
        guard canonicalIdentity != nil else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard groups[groupIdHex] != nil else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        // ADR-0043 C-61 / ADR-0044 — the Remove commit is published FIRST and
        // the rotation is not STARTED until it has landed. The ordering is
        // pinned behind this one seam rather than left as two adjacent
        // statements, because it now underwrites TWO guarantees: forward
        // secrecy (the rotation envelope is sealed at the post-removal epoch,
        // unreadable to the evicted leaf) AND recency (the new secret is
        // STAMPED at that epoch, so it strictly out-ranks the secret the
        // evicted member still holds instead of merely tying with it).
        return try await PushGroupRouting.publishRemoveThenRotate(
            // ADR-0040 (G-35) — ONLY the commit half takes the commit chain.
            // The rotation below publishes an application message, not a
            // commit, and already runs under the ADR-0044 routing chain; keeping
            // it outside means the two chains are never nested, so they cannot
            // deadlock against one another. C-61 ordering is unaffected — it is
            // enforced by `publishRemoveThenRotate` sequencing the two halves,
            // and the commit half still completes in full before the rotation
            // is started.
            publishRemove: {
                try await self.withCommitLock(groupIdHex: groupIdHex) {
                    try await self.publishRemoveCommit(
                        groupIdHex: groupIdHex,
                        leafIndex: leafIndex
                    )
                }
            },
            // Resolved INSIDE the closure, so this device's ADR-0022 role is
            // read after the removal landed — the same moment the previous
            // fire-and-forget call site read it.
            rotate: { [weak self] in
                guard let self,
                      let body = self.rotationAfterRemoval(groupIdHex: groupIdHex)
                else { return .skipNoSecret }
                return await body()
            }
        )
    }

    /// Apply + persist + publish a Remove commit. The `publishRemove` half of
    /// C-61: everything that must have COMPLETED before a rotation may start.
    /// Returns the commit's DS message id.
    ///
    /// Runs only under ``withCommitLock(groupIdHex:_:)`` and re-reads
    /// `canonicalIdentity` + `groups` itself — a predecessor may have replaced
    /// or evicted the record while this section was queued, so neither may be
    /// captured by the caller before the wait.
    private func publishRemoveCommit(
        groupIdHex: String,
        leafIndex: UInt32
    ) async throws -> Int64 {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        let commitBytes: Data
        do {
            commitBytes = try record.group.removeMember(
                identity: identity,
                leafIndex: leafIndex
            )
        } catch {
            throw ChatError.wrapped("removeMember failed", underlying: error)
        }
        // ADR-0040 (G-35) F1 — the core `removeMember` above eager-merged epoch
        // N -> N+1. Persist the outbox entry (commit only; a remove has no
        // Welcome) + the N+1 snapshot BEFORE the publish, FAIL-CLOSED. A throw
        // ABORTS the op (no publish). There is NO pre-persist artifact compute here
        // (a remove has no Welcome), so the reset-guarded `do` below trivially
        // covers every post-merge statement — the same F1 invariant the add /
        // approve paths enforce by widening the guard over their second-artifact
        // compute.
        let entryId = newOutboxEntryId()
        do {
            try persistCommitEntryThenSnapshotOrThrow(
                NewCommitEntry(
                    id: entryId,
                    groupId: base64Encode(record.groupId),
                    targetEpoch: String(try record.group.epoch()),
                    commitB64: base64Encode(commitBytes)
                )
            )
        } catch {
            // ADR-0040 (G-35) F1 — the FAIL-CLOSED persist aborted AFTER the core
            // `removeMember` eager-merged epoch N -> N+1 in memory. Roll the
            // in-memory group back to the durable snapshot (still N) so this abort
            // is "as if the op never happened" in memory too, then re-throw so the
            // UI sees the failure. Without the reset a second same-session commit
            // would build on the un-published N+1 and strand the whole group. See
            // ``resetGroupToDurableSnapshot(groupIdHex:)``.
            try resetGroupToDurableSnapshot(groupIdHex: groupIdHex)
            throw error
        }
        let commitPublish: PublishResult
        do {
            commitPublish = try await dsClient.publishGroup(
                groupId: record.groupId,
                payload: commitBytes
            )
        } catch {
            throw ChatError.wrapped("DS publishGroup (remove-member commit) failed", underlying: error)
        }
        chatClient.markOwnPublishedCommit(messageId: commitPublish.id)
        ackCommitBestEffort(entryId: entryId, commitMessageId: commitPublish.id)
        state = state.with(threads: buildThreadList())
        return commitPublish.id
    }

    /// ADR-0027 (G-3) — the joiner signature keys of the external-join
    /// requests this group has retained pending an owner/admin decision. Each
    /// entry is the `proposalRef` handle `approveExternalJoin` /
    /// `rejectExternalJoin` take. Empty when there are none (or no live Group
    /// handle for the id).
    public func pendingExternalJoins(groupIdHex: String) -> [Data] {
        guard let record = groups[groupIdHex] else { return [] }
        return (try? record.group.pendingExternalJoins()) ?? []
    }

    /// ADR-0028 OQ2 (G-3) — read the join-attribution envelopes from this group's
    /// journal so the admin UI can show which member's `/g/` link each pending
    /// request arrived on. For every valid attribution targeting this group
    /// (the embedded capability re-verified via `parseGroupJoinAttribution`),
    /// returns the joiner signature key, the issuer fingerprint, and whether the
    /// issuer is in the current roster. Advisory — the role-gated approval gate
    /// is the boundary, so failures degrade to an empty list.
    public func fetchJoinAttributions(
        groupIdHex: String
    ) async -> [(joinerSignatureKey: Data, issuerFingerprint: String, issuerInRoster: Bool)] {
        guard let record = groups[groupIdHex] else { return [] }
        guard let groupId = try? record.group.groupId() else { return [] }
        let rosterKeys = Set((try? record.group.members())?.map { $0.signaturePublicKey } ?? [])
        guard let raw = try? await dsClient.fetchGroup(groupId: groupId, after: 0, waitSecs: 0)
        else { return [] }
        var out: [(Data, String, Bool)] = []
        for m in raw.messages {
            guard let attr = try? parseGroupJoinAttribution(bytes: m.payload) else { continue }
            if attr.groupId != groupId { continue }
            out.append(
                (attr.joinerSignatureKey, attr.issuerFingerprint,
                 rosterKeys.contains(attr.issuerSignaturePubkey)))
        }
        return out
    }

    /// ADR-0027 (G-3) — approve a pending external-join request (owner/admin
    /// only; the core role-gates it, fail-closed). Drives the MLS approve
    /// (reuses the audited add path → Commit + Welcome), broadcasts the commit
    /// to the group journal (own-marked for echo suppression), then wraps the
    /// Welcome in a `WelcomeRendezvous` keyed on the joiner signature key and
    /// posts it to the SAME journal — the not-yet-member joiner polls the
    /// journal, matches the envelope to its own key, and opens the inner
    /// Welcome. Existing members skip the rendezvous entry (it is not an MLS
    /// message, so their receive loop drops it). Returns the commit's DS id.
    public func approveExternalJoin(
        groupIdHex: String,
        proposalRef: Data
    ) async throws -> Int64 {
        // ADR-0040 (G-35) — serialize the whole eager-merge-through-publish
        // region against every other membership commit on this group.
        try await withCommitLock(groupIdHex: groupIdHex) { [self] in
            try await approveExternalJoinLocked(
                groupIdHex: groupIdHex,
                proposalRef: proposalRef
            )
        }
    }

    /// The ``approveExternalJoin(groupIdHex:proposalRef:)`` critical section.
    /// Runs only under ``withCommitLock(groupIdHex:_:)`` and re-reads
    /// `canonicalIdentity` + `groups` itself — a predecessor may have replaced
    /// or evicted the record while this section was queued.
    private func approveExternalJoinLocked(
        groupIdHex: String,
        proposalRef: Data
    ) async throws -> Int64 {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        let addResult: AddMember
        do {
            addResult = try record.group.approveExternalJoin(
                identity: identity,
                proposalRef: proposalRef
            )
        } catch {
            throw ChatError.wrapped("approveExternalJoin failed", underlying: error)
        }

        // ADR-0040 (G-35) F1 — the core `approveExternalJoin` above (which reuses
        // the audited add path) eager-merged epoch N -> N+1. EVERYTHING from here
        // through the FAIL-CLOSED persist runs INSIDE the reset-guarded `do` below
        // — INCLUDING the rendezvous compute — because ANY throw after the eager
        // merge and before publish must roll the in-memory group back to the
        // durable N. A `wrapWelcomeRendezvous` that threw OUTSIDE this guard would
        // leave the group at the un-published N+1, so a later same-session commit
        // builds on N+1 and strands the whole group (the "device-ahead" hazard).
        // `entryId` is minted before the guard (non-throwing) so the post-publish
        // ack can reference it; the rendezvous is a deferred let assigned inside
        // the guard and consumed after it at publish (the reset-`catch` always
        // throws, so reaching the publish implies it was assigned).
        let entryId = newOutboxEntryId()
        let rendezvous: Data
        do {
            // Compute the second artifact (the rendezvous) so the outbox entry can
            // hold BOTH artifacts. The rendezvous rides the same group journal as
            // the commit.
            do {
                rendezvous = try wrapWelcomeRendezvous(
                    joinerSignatureKey: proposalRef,
                    welcome: addResult.welcome
                )
            } catch {
                throw ChatError.wrapped("wrapWelcomeRendezvous failed", underlying: error)
            }
            // Persist the outbox entry (commit + rendezvous, BOTH artifacts) + the
            // N+1 snapshot BEFORE any publish, FAIL-CLOSED. A throw ABORTS the op
            // (no publish); the sweep re-publishes both so a crash between the two
            // publishes never strands the joiner without a Welcome.
            try persistCommitEntryThenSnapshotOrThrow(
                NewCommitEntry(
                    id: entryId,
                    groupId: base64Encode(record.groupId),
                    targetEpoch: String(try record.group.epoch()),
                    commitB64: base64Encode(addResult.commit),
                    secondArtifactB64: base64Encode(rendezvous),
                    secondArtifactKind: .rendezvous
                )
            )
        } catch {
            // ADR-0040 (G-35) F1 — the rendezvous compute OR the FAIL-CLOSED
            // persist aborted AFTER the core `approveExternalJoin` (which reuses the
            // audited add path) eager-merged epoch N -> N+1 in memory. Roll the
            // in-memory group back to the durable snapshot (still N) so this abort
            // is "as if the op never happened" in memory too, then re-throw so the
            // UI sees the failure. Without the reset a second same-session commit
            // would build on the un-published N+1 and strand the whole group. See
            // ``resetGroupToDurableSnapshot(groupIdHex:)``.
            try resetGroupToDurableSnapshot(groupIdHex: groupIdHex)
            throw error
        }

        // Publish artifact 1 — the commit so every existing member admits the
        // joiner.
        let commitPublish: PublishResult
        do {
            commitPublish = try await dsClient.publishGroup(
                groupId: record.groupId,
                payload: addResult.commit
            )
        } catch {
            throw ChatError.wrapped("DS publishGroup (approve commit) failed", underlying: error)
        }
        chatClient.markOwnPublishedCommit(messageId: commitPublish.id)

        // Publish artifact 2 — the wrapped Welcome rendezvous to the same
        // journal (the SAME bytes stored in the outbox entry).
        let rendezvousPublish: PublishResult
        do {
            rendezvousPublish = try await dsClient.publishGroup(
                groupId: record.groupId,
                payload: rendezvous
            )
        } catch {
            throw ChatError.wrapped("DS publishGroup (Welcome rendezvous) failed", underlying: error)
        }

        ackCommitBestEffort(
            entryId: entryId,
            commitMessageId: commitPublish.id,
            secondMessageId: rendezvousPublish.id
        )
        state = state.with(threads: buildThreadList())
        return commitPublish.id
    }

    /// ADR-0027 (G-3) — reject a pending external-join request (owner/admin
    /// only). Local-only: drops the retained proposal so it leaves the pending
    /// list. No commit, no journal post — the joiner never receives a Welcome.
    public func rejectExternalJoin(
        groupIdHex: String,
        proposalRef: Data
    ) throws {
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        do {
            try record.group.rejectExternalJoin(proposalRef: proposalRef)
        } catch {
            throw ChatError.wrapped("rejectExternalJoin failed", underlying: error)
        }
        persistSnapshotBestEffort()
    }

    /// ADR-0028 (G-3) — mint a group invite-link (owner/admin side). Signs a
    /// `GroupInviteCapability` over this group's raw MLS `group_id` with the
    /// local identity's Ed25519 key and returns the `https://lakko.me/g/<slug>`
    /// link the owner shares out-of-band. A non-member opens it, reads the
    /// current epoch from the journal, and posts an external-join proposal an
    /// admin approves (ADR-0027). The link conveys only the `group_id` — it
    /// authorizes nobody; the admin gate is the security boundary. `0` = never
    /// expires.
    public func createGroupInviteLink(groupIdHex: String) async throws -> String {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        let wire: Data
        do {
            wire = try composeGroupInviteCapability(
                issuerSigningSeed: identity.ed25519Seed(),
                groupId: record.group.groupId(),
                expiryMs: 0
            )
        } catch {
            throw ChatError.wrapped("composeGroupInviteCapability failed", underlying: error)
        }
        // Production default (ADR-0028 OQ1): upload to the discovery ledger —
        // the server-side anti-abuse surface (per-joiner consume dedup + soft
        // max_uses) — and share the short pointer URL. On an upload failure (a
        // censored / offline network) fall back to the inline form so the link
        // still works without the round-trip. `max_uses` is a soft throttle,
        // not an access control: the admin approval gate stays the boundary.
        do {
            let blobId = try await discovery.uploadGroupInvite(
                capabilityBytes: wire, maxUses: UInt32.max)
            return try GroupInviteUrl.composePointerGroupInviteUrl(blobId: blobId)
        } catch {
            return (try? GroupInviteUrl.composeInlineGroupInviteUrl(capabilityBytes: wire))
                ?? ("https://lakko.me/g/" + base64UrlEncodeNoPad(wire))
        }
    }

    /// ADR-0028 revoke — the issuer revokes a previously-created group invite
    /// link. Only the server-backed pointer form (a 32-byte `blob_id`) is
    /// revocable; an inline link has no ledger row. Returns `true` when a ledger
    /// row was revoked, `false` for an inline / malformed link (nothing to do).
    public func revokeGroupInviteLink(link: String) async throws -> Bool {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard let range = link.range(of: "/g/", options: .backwards) else { return false }
        let slug = String(link[range.upperBound...])
        let parsed: GroupInviteUrl.ParsedGroupInvite
        do {
            parsed = try GroupInviteUrl.parseGroupInviteSlug(slug)
        } catch {
            return false
        }
        switch parsed {
        case .inline:
            return false
        case .pointer(let blobId):
            do {
                _ = try await discovery.revokeGroupInvite(
                    blobId: blobId, signingSeed: identity.ed25519Seed())
            } catch {
                throw ChatError.wrapped("group invite revoke failed", underlying: error)
            }
            return true
        }
    }

    /// ADR-0028 (G-3) — resolve a `/g/<slug>` slug to the capability wire bytes,
    /// fetching from the discovery ledger when the slug is a 32-byte
    /// content-addressed pointer (the production form) or passing the bytes
    /// straight through when it is an inline capability (the censored-region
    /// fallback). Returns the `blobId` (non-nil only for the pointer form) so
    /// the caller can drive the consume gate before requesting to join.
    public func resolveGroupInviteSlug(
        slug: String
    ) async throws -> (capabilityWire: Data, blobId: Data?) {
        let parsed: GroupInviteUrl.ParsedGroupInvite
        do {
            parsed = try GroupInviteUrl.parseGroupInviteSlug(slug)
        } catch {
            throw ChatError.wrapped("group invite link is malformed", underlying: error)
        }
        switch parsed {
        case .inline(let capabilityBytes):
            return (capabilityBytes, nil)
        case .pointer(let blobId):
            let fetched: Data?
            do {
                fetched = try await discovery.fetchGroupInvite(blobId: blobId)
            } catch {
                throw ChatError.wrapped("group invite fetch failed", underlying: error)
            }
            guard let capabilityWire = fetched else {
                throw ChatError.argument("group invite not found or expired")
            }
            return (capabilityWire, blobId)
        }
    }

    public func sendMessage(
        groupIdHex: String,
        plaintext: String,
        attachments: [AttachmentRef] = []
    ) async throws {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        // 1.19c3: build the JSON envelope before encrypting.
        // `composeEnvelopeBytes` routes through the UniFFI
        // `composeEnvelope` free function — wrong-length byte
        // fields are rejected there (defence-in-depth: the
        // UI already validates the AttachmentRefs it builds via
        // `AttachmentBridge.encryptAndUpload`).
        let envelopeBytes: Data
        do {
            envelopeBytes = try AttachmentBridge.composeEnvelopeBytes(
                text: plaintext,
                attachments: attachments
            )
        } catch {
            throw ChatError.wrapped(
                "envelope compose failed",
                underlying: error
            )
        }
        let result = try await chatClient.sendMessage(
            identity: identity,
            group: record.group,
            plaintext: envelopeBytes
        )
        // ADR-0023 — stamp our own leaf as the author so a
        // later edit / delete of this message passes the
        // author-only gate on every peer (and locally).
        let ownLeaf = try? record.group.ownLeafIndex()
        record.appendOutgoing(
            text: plaintext,
            attachments: attachments,
            messageId: result.messageId,
            createdAt: result.createdAt,
            authorLeaf: ownLeaf
        )
        // The MLS ratchet snapshot was already durably persisted
        // (fail-closed) inside `chatClient.sendMessage` BEFORE the publish,
        // and neither the publish nor `appendOutgoing` advances MLS state,
        // so re-persisting the snapshot here is unnecessary. Only the
        // plaintext timeline changed (the just-appended row), so save that
        // best-effort — its loss cannot cause nonce reuse.
        persistMessageLogBestEffort()
        state = state.with(threads: buildThreadList())
        // 1.20e — fan a wake out to every device subscribed to
        // this group. Fire-and-forget — the message is already
        // on the DS and recipients pick it up on next poll
        // even if push-gw is down. Off the user-visible send
        // critical path so the PoW solve doesn't double send
        // latency.
        Task { @MainActor [weak self, groupIdHex, messageId = result.messageId] in
            await self?.notifyPushGroupBestEffort(
                groupIdHex: groupIdHex,
                messageId: messageId
            )
        }
    }

    /// ADR-0023 — edit our own message `target` to `newText`.
    /// Composes the edit op envelope, publishes it via the
    /// existing application send path, and applies the new text
    /// locally (the sender never receives its own app message
    /// back). The author-only gate is enforced on every peer's
    /// receive side; locally we only edit our own outgoing rows.
    public func editMessage(
        groupIdHex: String,
        target: Int64,
        newText: String
    ) async throws {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        let envelopeBytes: Data
        do {
            envelopeBytes = try AttachmentBridge.composeEditBytes(
                target: target,
                newText: newText
            )
        } catch {
            throw ChatError.wrapped("edit envelope compose failed", underlying: error)
        }
        _ = try await chatClient.sendMessage(
            identity: identity,
            group: record.group,
            plaintext: envelopeBytes
        )
        record.applyLocalEdit(target: target, newText: newText)
        persistSnapshotBestEffort()
        state = state.with(threads: buildThreadList())
    }

    /// ADR-0023 — delete (tombstone) our own message `target`.
    /// Composes the delete op envelope, publishes it, and marks
    /// the message deleted locally.
    public func deleteMessage(
        groupIdHex: String,
        target: Int64
    ) async throws {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        let envelopeBytes: Data
        do {
            envelopeBytes = try AttachmentBridge.composeDeleteBytes(target: target)
        } catch {
            throw ChatError.wrapped("delete envelope compose failed", underlying: error)
        }
        _ = try await chatClient.sendMessage(
            identity: identity,
            group: record.group,
            plaintext: envelopeBytes
        )
        record.applyLocalDelete(target: target)
        persistSnapshotBestEffort()
        state = state.with(threads: buildThreadList())
    }

    /// ADR-0023 — react to message `target` with `emoji`.
    /// Composes the reaction op envelope, publishes it, and
    /// toggles our own `(ownLeaf, emoji)` pair locally. Any
    /// member may react (no author gate); re-reacting with the
    /// same emoji removes it.
    public func reactToMessage(
        groupIdHex: String,
        target: Int64,
        emoji: String
    ) async throws {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        let envelopeBytes: Data
        do {
            envelopeBytes = try AttachmentBridge.composeReactionBytes(
                target: target,
                emoji: emoji
            )
        } catch {
            throw ChatError.wrapped("reaction envelope compose failed", underlying: error)
        }
        _ = try await chatClient.sendMessage(
            identity: identity,
            group: record.group,
            plaintext: envelopeBytes
        )
        if let ownLeaf = try? record.group.ownLeafIndex() {
            record.applyLocalReaction(target: target, emoji: emoji, ownLeaf: ownLeaf)
        }
        persistSnapshotBestEffort()
        state = state.with(threads: buildThreadList())
    }

    /// Phase 3a9 — publish raw plaintext bytes as an MLS
    /// application message. Same MLS-wrap + DS-publish path
    /// as ``sendMessage`` but skips the chat-envelope
    /// composer and the timeline append — call signaling
    /// rides this channel out-of-band.
    public func publishApplicationMessage(
        groupIdHex: String,
        plaintextBytes: Data
    ) async throws {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        _ = try await chatClient.sendMessage(
            identity: identity,
            group: record.group,
            plaintext: plaintextBytes
        )
        persistSnapshotBestEffort()
    }

    /// ADR-0024 — publish an ephemeral typing signal for `groupIdHex`.
    /// Composes the typing op envelope (`{ typing: <bool> }`) and ships
    /// it through the existing application send path. Renders no bubble
    /// + no timeline append; the receivers fold it into ephemeral
    /// per-leaf typing UI state. The view model owns the throttle (one
    /// `true` per ~4 s) + the single `false` on blur / send — this
    /// method is the raw publish seam.
    public func sendTyping(
        groupIdHex: String,
        isTyping: Bool
    ) async throws {
        let envelopeBytes: Data
        do {
            envelopeBytes = try composeTypingEnvelope(isTyping: isTyping)
        } catch {
            throw ChatError.wrapped("typing envelope compose failed", underlying: error)
        }
        try await publishApplicationMessage(
            groupIdHex: groupIdHex,
            plaintextBytes: envelopeBytes
        )
    }

    /// ADR-0024 — publish a read receipt for `groupIdHex`: the highest
    /// messageId the local member has read in this group. Monotone +
    /// throttled at the view-model layer (only when the watermark
    /// advances past our own last-published value). Renders no bubble;
    /// receivers advance a monotone per-leaf watermark and render the
    /// author's own messages as "Read" once every other member's
    /// watermark covers them.
    public func sendReadReceipt(
        groupIdHex: String,
        upTo: Int64
    ) async throws {
        let envelopeBytes: Data
        do {
            envelopeBytes = try composeReadReceiptEnvelope(upTo: upTo)
        } catch {
            throw ChatError.wrapped("read-receipt envelope compose failed", underlying: error)
        }
        try await publishApplicationMessage(
            groupIdHex: groupIdHex,
            plaintextBytes: envelopeBytes
        )
    }

    /// ADR-0042 — push-gw wake fan-out routed under the opaque `push_gid`,
    /// NOT the DS `group_id`. When this device holds no secret for the
    /// group yet, the wake is SKIPPED rather than leaking the `group_id` to
    /// push-gw (invariant #5) — recipients still receive the message on
    /// their next foreground poll. A publisher is a member, so it normally
    /// holds the secret. Best-effort, off the critical path.
    private func notifyPushGroupBestEffort(
        groupIdHex: String,
        messageId: Int64
    ) async {
        guard let accountIdB64 = currentAccountIdB64,
              let pushGid = pushRouting.pushGid(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex) else {
            // No secret held yet → do NOT wake under the DS key.
            return
        }
        do {
            _ = try await PushClient().notifyGroup(
                groupPseudonym: pushGid,
                envelopePointer: messageId
            )
        } catch {
            NSLog("push notify-group failed: \(error)")
        }
    }

    // MARK: - ADR-0042 opaque push routing

    /// ADR-0042 — ensure this group's opaque push routing is provisioned
    /// for this device. Owner + no secret yet → mint + persist + broadcast
    /// the distribution envelope (E2EE journal) + subscribe under
    /// `push_gid`; a member holding a secret → subscribe under `push_gid`;
    /// anyone else → defer (no push under `group_id`). Idempotent +
    /// race-safe (a shared per-group in-flight guard in ``PushGroupRouting``
    /// coalesces the chat view's onAppear with the legacy-sweep).
    /// Fire-and-forget — never blocks the chat.
    public func ensurePushRouting(groupIdHex: String) {
        guard currentAccountIdB64 != nil, groups[groupIdHex] != nil else { return }
        Task { @MainActor [weak self] in
            await self?.performEnsurePushRouting(groupIdHex: groupIdHex)
        }
    }

    /// @MainActor body of ``ensurePushRouting(groupIdHex:)`` — reads the
    /// live `Group` handle + canonical identity on the actor and hands them
    /// to the ``PushGroupRouting`` funnel (same actor, so the non-`Sendable`
    /// `Group` never crosses an actor boundary).
    private func performEnsurePushRouting(groupIdHex: String) async {
        guard let identity = canonicalIdentity,
              let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex] else { return }
        await pushRouting.ensureProvisioned(
            accountIdB64: accountIdB64,
            groupIdHex: groupIdHex,
            group: record.group,
            identity: identity,
            // A vanished `self` must THROW, not optional-chain into a silent
            // success: ADR-0042 marks the secret distributed only when this
            // closure returns without throwing, and a no-op "success" would
            // durably record an envelope that never reached the journal.
            broadcast: { [weak self] bytes in
                guard let self else {
                    throw ChatError.argument("ChatRepository deallocated mid-broadcast")
                }
                try await self.broadcastPushRoutingControl(groupIdHex: groupIdHex, bytes: bytes)
            }
        )
    }

    /// ADR-0042 — subscribe under `push_gid` (defers when no secret is
    /// held; never subscribes under the DS `group_id`). Fire-and-forget.
    public func subscribeGroupUnderPushGid(groupIdHex: String) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        Task { @MainActor [weak self] in
            await self?.pushRouting.subscribeUnderPushGidBestEffort(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        }
    }

    /// ADR-0042 — a push-routing-secret distribution envelope arrived on
    /// the group journal: run the owner gate, then (on accept) persist the
    /// secret (idempotent) + subscribe under the derived `push_gid`.
    /// Fire-and-forget (best-effort).
    ///
    /// `senderLeaf` is the MLS-authenticated author of the envelope. It is
    /// resolved against the LIVE `Group` (roster + ADR-0022 role table) in
    /// ``performReceivePushRoutingSecret(groupIdHex:secret:senderLeaf:)`` —
    /// a *changed* secret from a non-owner is refused (`lakko-security`
    /// MEDIUM-1).
    public func receivePushRoutingSecret(
        groupIdHex: String,
        secret: Data,
        mintEpoch: UInt64,
        senderLeaf: UInt32?
    ) {
        guard currentAccountIdB64 != nil, groups[groupIdHex] != nil else { return }
        Task { @MainActor [weak self] in
            await self?.performReceivePushRoutingSecret(
                groupIdHex: groupIdHex, secret: secret, mintEpoch: mintEpoch,
                senderLeaf: senderLeaf)
        }
    }

    /// @MainActor body of
    /// ``receivePushRoutingSecret(groupIdHex:secret:senderLeaf:)`` — reads
    /// the live `Group` handle on the actor and hands it to the gate (same
    /// actor, so the non-`Sendable` `Group` never crosses an actor
    /// boundary). The group is the ONLY authority for the owner key + the
    /// leaf → signature-key mapping.
    private func performReceivePushRoutingSecret(
        groupIdHex: String,
        secret: Data,
        mintEpoch: UInt64,
        senderLeaf: UInt32?
    ) async {
        guard let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex] else { return }
        await pushRouting.receiveSecret(
            accountIdB64: accountIdB64,
            groupIdHex: groupIdHex,
            secret: secret,
            mintEpoch: mintEpoch,
            group: record.group,
            senderLeaf: senderLeaf
        )
    }

    /// ADR-0042 — re-broadcast the current secret to a group (after an
    /// add-member commit). No-op when this device holds no secret.
    /// Fire-and-forget.
    private func redistributePushRoutingSecret(groupIdHex: String) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        Task { @MainActor [weak self] in
            guard let self else { return }
            await self.pushRouting.redistribute(
                accountIdB64: accountIdB64,
                groupIdHex: groupIdHex,
                // Throws rather than optional-chaining into a silent success —
                // see the note in `performEnsurePushRouting`.
                broadcast: { [weak self] bytes in
                    guard let self else {
                        throw ChatError.argument("ChatRepository deallocated mid-broadcast")
                    }
                    try await self.broadcastPushRoutingControl(
                        groupIdHex: groupIdHex, bytes: bytes)
                }
            )
        }
    }

    /// ADR-0043 — this device just published a Remove commit, so the evicted
    /// member still holds the group's `push_routing_secret` and push-gw is
    /// still waking it. Re-key so nobody ever notifies the id it can derive
    /// again, and retire this device's own subscription under the old id.
    ///
    /// The DIRECT entry point (the removing device knows immediately — no
    /// membership diff needed). The wire-observed twin, for a removal
    /// committed by SOMEONE ELSE, is
    /// ``rotatePushRoutingSecretIfMembershipShrank(groupIdHex:previousMemberKeys:)``
    /// on the poll path. Fire-and-forget.
    /// ADR-0044 — the rotation body, as a closure
    /// ``PushGroupRouting/publishRemoveThenRotate(publishRemove:rotate:onRotationStarted:)``
    /// starts only AFTER the Remove commit has landed (C-61). The `mintEpoch`
    /// provider reads the LIVE group's epoch at mint time, so the stamp is the
    /// post-removal one and the rotation strictly out-ranks the secret the
    /// evicted member still holds.
    private func rotatePushRoutingSecretBody(
        accountIdB64: String,
        groupIdHex: String,
        role: Role?
    ) -> @MainActor () async -> PushGroupRouting.PushRotationOutcome {
        { [weak self] in
            guard let self else { return .skipNoSecret }
            return await self.pushRouting.rotateSecret(
                accountIdB64: accountIdB64,
                groupIdHex: groupIdHex,
                ownRole: role,
                // ADR-0044 — read LAZILY, at mint time (inside the serializing
                // rotation chain), never captured up front: a rotation queued
                // behind a predecessor must stamp the epoch it actually mints
                // at. An unreadable epoch fails closed to `skipUnknownEpoch`.
                mintEpoch: { [weak self] in
                    guard let group = self?.groups[groupIdHex]?.group else { return nil }
                    return PushGroupRouting.readGroupEpoch(group)
                },
                // Throws rather than optional-chaining into a silent success —
                // see the note in `performEnsurePushRouting`.
                broadcast: { [weak self] bytes in
                    guard let self else {
                        throw ChatError.argument("ChatRepository deallocated mid-broadcast")
                    }
                    try await self.broadcastPushRoutingControl(
                        groupIdHex: groupIdHex, bytes: bytes)
                }
            )
        }
    }

    /// The rotation half of C-61, handed to
    /// ``PushGroupRouting/publishRemoveThenRotate(publishRemove:rotate:onRotationStarted:)``.
    /// `nil` when there is no session, which skips the rotation entirely.
    private func rotationAfterRemoval(
        groupIdHex: String
    ) -> (@MainActor () async -> PushGroupRouting.PushRotationOutcome)? {
        guard let accountIdB64 = currentAccountIdB64 else { return nil }
        // The role is read here, on the removing device, at removal time.
        let role = ownRole(groupIdHex: groupIdHex)
        return rotatePushRoutingSecretBody(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, role: role)
    }

    /// ADR-0043 — someone ELSE's Remove commit just landed on the journal.
    /// A Commit surfaces from the poll loop with no membership delta
    /// attached, so the removal is INFERRED by diffing the member signature
    /// keys captured BEFORE the poll batch against the post-poll roster.
    ///
    /// Keys, not counts and not leaf indices: OpenMLS reuses a vacated leaf
    /// for the next Add, so a remove+add in one batch leaves both the count
    /// and the index set unchanged while a member really was evicted.
    ///
    /// This device's OWN removals never come through here (they were applied
    /// locally before this poll, so the baseline is already post-removal and
    /// shows no shrink); those rotate directly via
    /// ``rotatePushRoutingSecretAfterRemoval(groupIdHex:)``. Owner-or-admin
    /// only, no-op on growth. Fire-and-forget: a rotation failure must never
    /// break the poll loop.
    private func rotatePushRoutingSecretIfMembershipShrank(
        groupIdHex: String,
        previousMemberKeys: [String]?
    ) {
        guard let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex] else { return }
        let role = ownRole(groupIdHex: groupIdHex)
        let current = PushGroupRouting.memberSignatureKeys(group: record.group)
        Task { @MainActor [weak self] in
            guard let self else { return }
            await self.pushRouting.rotateIfMembershipShrank(
                accountIdB64: accountIdB64,
                groupIdHex: groupIdHex,
                ownRole: role,
                previousMemberKeys: previousMemberKeys,
                currentMemberKeys: current,
                // ADR-0044 — the same lazy post-commit read as the direct
                // path. The poll already applied the peer's Remove commit
                // locally before this runs, so the live epoch is the
                // post-removal one.
                mintEpoch: { [weak self] in
                    guard let group = self?.groups[groupIdHex]?.group else { return nil }
                    return PushGroupRouting.readGroupEpoch(group)
                },
                broadcast: { [weak self] bytes in
                    guard let self else {
                        throw ChatError.argument("ChatRepository deallocated mid-broadcast")
                    }
                    try await self.broadcastPushRoutingControl(
                        groupIdHex: groupIdHex, bytes: bytes)
                }
            )
        }
    }

    /// ADR-0042 — post a control envelope (a push-routing-secret
    /// distribution envelope) to the group journal as an E2EE MLS
    /// application message via the fail-closed persist-before-publish path.
    private func broadcastPushRoutingControl(groupIdHex: String, bytes: Data) async throws {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        _ = try await chatClient.broadcastControl(
            identity: identity,
            group: record.group,
            controlBytes: bytes
        )
    }

    /// Pull new messages from one group's journal + apply them to
    /// the in-memory timeline.
    ///
    /// ## G-38 — split poll: fetch outside the lock, apply inside it
    ///
    /// The poll APPLIES incoming commits (`processIncomingVerifyingRoles`
    /// mutates the shared OpenMLS provider through the same `Group` handle a
    /// membership commit is mid-operation on), so it is not a read-only
    /// observer and must not run beside a local commit. `@MainActor` isolation
    /// does not provide that on its own: a commit holds its create-through-
    /// publish region across `await dsClient.publishGroup`, and isolation is
    /// RELEASED at that suspension — a fully synchronous apply then lands
    /// squarely inside the window where the group has eager-merged N -> N+1 but
    /// the commit is not yet on the wire.
    ///
    /// So the apply takes the SAME per-group chain the four commit paths take
    /// (``withCommitLock(groupIdHex:_:)``), and only the apply:
    ///
    ///   1. Sweep the outbox (once per group per session) — OUTSIDE the lock,
    ///      unchanged, and deliberately so: it publishes commits itself, and the
    ///      chain is not reentrant.
    ///   2. Read the cursor and FETCH with it — OUTSIDE the lock. `waitSecs` is
    ///      a deliberate 30 s long poll on the live chat screen; taking the
    ///      lock across it would stall every send and membership commit on this
    ///      group behind an idle connection, which is G-37's own defect at a
    ///      per-group blast radius.
    ///   3. Take the lock, then RE-READ the record and the cursor. A predecessor
    ///      may have replaced the record
    ///      (``resetGroupToDurableSnapshot(groupIdHex:)``) or advanced the
    ///      cursor while this poll was parked on the network. A moved cursor
    ///      means the batch in hand starts at a row already consumed, so it is
    ///      DROPPED without applying and without touching the cursor — applying
    ///      it would re-feed consumed rows to OpenMLS and let the stale batch's
    ///      end cursor wind the record's cursor BACKWARD.
    ///   4. Apply + advance the cursor, atomically, in one non-suspending
    ///      region.
    ///
    /// ## No in-flight guard, unlike Android
    ///
    /// Android's `runSplitPoll` additionally claims a per-group in-flight flag
    /// and returns `SKIPPED_IN_FLIGHT` to the loser. iOS has never had one and
    /// deliberately still does not: the two poll sources here are a 30 s long
    /// poll (`ChatViewModel.pollOnceLongPoll`) and the user's manual refresh
    /// (`onRefreshClicked`), so a guard would make pressing Refresh a silent
    /// no-op for up to 30 s. It is only an optimisation — it saves a redundant
    /// fetch, it does not provide correctness. Correctness comes from the
    /// cursor-moved guard in ``applyFetchedBatch(groupIdHex:after:raw:)``: two
    /// overlapping polls both fetch, the first to reach the lock applies, and
    /// the other finds the cursor moved and drops its batch whole.
    ///
    /// ## Honest scope
    ///
    /// This closes the LOCAL interleave — this device's own poll against this
    /// device's own commit. A poll applying a REMOTE commit while a local one is
    /// in flight to the DS is a genuine MLS epoch fork that no client-side lock
    /// can address (it needs a DS-side epoch gate or proposal-based commits);
    /// that is G-38 residual 1 and is deliberately untouched here.
    public func pollGroup(
        groupIdHex: String,
        waitSecs: Int = 0
    ) async throws {
        guard canonicalIdentity != nil else { return }
        guard let record = groups[groupIdHex] else { return }
        // ADR-0040 (G-35) — sweep the durable pending-commit outbox ONCE per
        // group per session, BEFORE the first poll. Re-publishing a crashed
        // mid-window commit re-arms the own-echo filter for the re-published
        // commit's messageId, so the apply below drops it instead of feeding it
        // to `processIncoming` (which would raise `CannotDecryptOwnMessage`).
        // The guard-insert is synchronous + on the main actor, so the sweep runs
        // at most once even under overlapping poll tasks; an interleaving poll's
        // own-commit re-fetch is still a benign drop-and-continue in
        // `ChatClient.applyFetchedMessages`.
        //
        // G-38 — deliberately OUTSIDE the commit lock. The sweep publishes
        // commits of its own, and ``withCommitLock(groupIdHex:_:)`` is not
        // reentrant, so running it inside would deadlock against the apply's
        // own acquisition below.
        if !sweptGroupsThisSession.contains(groupIdHex) {
            sweptGroupsThisSession.insert(groupIdHex)
            await sweepPendingCommits(record: record)
        }
        // G-38 step 2 — the NETWORK half, outside the lock. Both values are read
        // here and re-validated under the lock; `groupId` is the record's cached
        // copy, so the fetch reads no `Group` handle at all.
        let after = record.cursor
        let groupId = record.groupId
        let raw = try await chatClient.fetchGroupJournal(
            groupId: groupId,
            after: after,
            waitSecs: waitSecs
        )
        // G-38 steps 3+4 — the MLS half, inside the per-group commit chain.
        try await withCommitLock(groupIdHex: groupIdHex) { [self] in
            applyFetchedBatch(groupIdHex: groupIdHex, after: after, raw: raw)
        }
    }

    /// G-38 — the locked half of ``pollGroup(groupIdHex:waitSecs:)``.
    ///
    /// Runs ONLY under ``withCommitLock(groupIdHex:_:)``, and is
    /// **non-suspending by construction**: every call it makes is synchronous
    /// (`ChatClient.applyFetchedMessages` is deliberately not `async`, and both
    /// push-routing follow-ups only SPAWN a detached `Task` and return). That
    /// makes the whole batch — the membership baseline, the MLS apply, the
    /// cursor advance and the snapshot — one atomic `@MainActor` region, so no
    /// other commit can observe it half-applied and no network wait can be
    /// reintroduced into it without a compile error.
    ///
    /// Re-reads `canonicalIdentity` and `groups` rather than trusting anything
    /// captured before the wait — a predecessor's persist-abort calls
    /// ``resetGroupToDurableSnapshot(groupIdHex:)``, which REPLACES the
    /// `GroupRecord` and can evict it entirely.
    private func applyFetchedBatch(
        groupIdHex: String,
        after: Int64,
        raw: FetchResult
    ) {
        guard let identity = canonicalIdentity else { return }
        guard let record = groups[groupIdHex] else { return }
        // G-38 — the cursor-moved guard. Another poll of this group applied a
        // batch while this one was parked on the network, so the rows in hand
        // start behind the live cursor. Drop them WITHOUT applying and WITHOUT
        // touching the cursor; the next poll re-fetches from where the record
        // actually is. Applying anyway would re-feed consumed rows to OpenMLS
        // and, worse, wind the cursor BACKWARD to this stale batch's end.
        guard record.cursor == after else { return }
        // ADR-0043 — baseline for the membership diff, captured BEFORE the
        // poll applies any commit. A Commit carries no membership delta out
        // of the poll loop, so a removal committed by SOMEONE ELSE is
        // inferred from this snapshot against the post-poll one. This
        // device's OWN removals never show up here (they were applied
        // locally before this poll, so the baseline is already
        // post-removal); those rotate directly in `removeMemberFromGroup`.
        //
        // G-38 — still captured INSIDE the lock, immediately before the apply.
        // Hoisting it out to where the fetch now runs would race the shrink diff
        // against any commit landing in the fetch window.
        let membersBeforePoll = PushGroupRouting.memberSignatureKeys(group: record.group)
        let result = chatClient.applyFetchedMessages(
            identity: identity,
            group: record.group,
            raw: raw,
            after: after
        )
        record.applyPollResult(
            result,
            callSignalingSink: { [weak self] payload in
                self?.callSignalingSink?(groupIdHex, payload)
            },
            chatStatusSink: { [weak self] status in
                self?.chatStatusSink?(groupIdHex, status)
            },
            pushRoutingSecretSink: { [weak self] secret, mintEpoch, senderLeaf in
                // ADR-0042 — owner-gate, then persist the secret + subscribe
                // under `push_gid`. ADR-0044 — plus the recency order.
                // Fire-and-forget (it only spawns a `Task`), so the routing
                // chain it eventually takes is never awaited while the commit
                // chain is held — the two locks are never nested.
                self?.receivePushRoutingSecret(
                    groupIdHex: groupIdHex, secret: secret, mintEpoch: mintEpoch,
                    senderLeaf: senderLeaf)
            }
        )
        if !result.events.isEmpty {
            persistSnapshotBestEffort()
        }
        state = state.with(threads: buildThreadList())
        // ADR-0043 — did someone else's Remove commit just land? Then the
        // evicted member still holds this group's push_routing_secret and
        // push-gw is still waking it. Driven off an ACTUAL before/after
        // member-set comparison, never off a "a poll happened" tick.
        // Fire-and-forget, same non-nesting argument as the sink above.
        rotatePushRoutingSecretIfMembershipShrank(
            groupIdHex: groupIdHex, previousMemberKeys: membersBeforePoll)
    }

    /// Phase 3a9 — closure the UI attaches to dispatch
    /// inbound call-signaling bytes into its
    /// ``CallSession`` state machine. `nil` when no call
    /// surface is attached; the per-record poll just drops
    /// the payload.
    public var callSignalingSink: ((String, Data) -> Void)?

    /// ADR-0024 — closure the chat view model attaches to receive
    /// inbound typing / read-receipt status events for a group (by
    /// hex id). These render no bubble + never touch the timeline;
    /// the view model folds them into ephemeral typing state +
    /// a monotone per-leaf read watermark. `nil` when no chat
    /// surface is attached; the per-record poll just drops them.
    public var chatStatusSink: ((String, ChatStatusEvent) -> Void)?

    /// Pull new messages for every active group. Per-group
    /// failures are swallowed so one bad apple doesn't block the
    /// others.
    public func pollAllGroups(waitSecs: Int = 0) async {
        let groupIds = Array(groups.keys)
        for groupIdHex in groupIds {
            do {
                try await pollGroup(groupIdHex: groupIdHex, waitSecs: waitSecs)
            } catch {
                // best-effort
                continue
            }
        }
    }

    // MARK: - Internals

    private func persistKnownGroupBestEffort(groupIdHex: String) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    /// Durably persist the canonical Identity's MLS provider snapshot and
    /// RE-THROW on any failure — the fail-closed counterpart of
    /// ``persistSnapshotBestEffort()``.
    ///
    /// Wired into ``ChatClient/ratchetPersister`` and invoked BEFORE the
    /// DS publish on every application send. A swallowed persist failure
    /// here would re-open the AEAD nonce-reuse-on-crash window:
    /// `createApplicationMessage` has already advanced the sender ratchet
    /// to generation N+1, and the OpenMLS provider is in-memory, so that
    /// N+1 state lives ONLY in this snapshot. If it does not durably land,
    /// the gen-N ciphertext must NOT be published — the caller aborts, and
    /// after a reload the ratchet is safe to re-consume generation N (only
    /// one gen-N ciphertext can ever exist on the wire).
    ///
    /// The plaintext message-log is NOT persisted here: its absence cannot
    /// cause nonce reuse (it is not MLS state), and it is written
    /// best-effort AFTER the send completes so it captures the just-sent
    /// row. When there is no canonical identity / account in scope (tests,
    /// pre-bootstrap) there is nothing durable to persist and no
    /// reload-from-snapshot path that could re-consume a generation, so
    /// this resolves as a no-op (mirror of the web no-op-when-`onPersist`-
    /// unset posture).
    private func persistSnapshotDurableOrThrow() throws {
        guard let identity = canonicalIdentity,
              let accountIdB64 = currentAccountIdB64 else { return }
        let blob = try identity.snapshotStorage()
        try snapshotStore.putSnapshotDurable(
            accountIdB64: accountIdB64,
            blob: blob
        )
    }

    // MARK: - ADR-0040 (G-35) per-group commit serialization

    /// Run `body` as the sole in-flight MLS-mutating section for one group.
    ///
    /// Five sections take this chain: the four group-membership create-commit
    /// paths, and — since G-38 — the APPLY half of
    /// ``pollGroup(groupIdHex:waitSecs:)`` (``applyFetchedBatch(groupIdHex:after:raw:)``).
    /// The poll belongs here because it is not a read-only observer: it feeds
    /// incoming commits to `processIncomingVerifyingRoles`, which mutates the
    /// shared OpenMLS provider through the very `Group` handle an in-flight
    /// commit is mid-operation on. Only the poll's apply is inside; its DS fetch
    /// stays outside (see ``pollGroup(groupIdHex:waitSecs:)``).
    ///
    /// ## Why actor isolation is not enough
    ///
    /// Every create-commit entry point in `lakko-mls` (`setGroupRoles` /
    /// `addMemberFromWire` / `removeMember` / `approveExternalJoin`)
    /// EAGER-MERGES epoch N -> N+1 into the shared in-memory OpenMLS provider
    /// *inside* the create call, before the commit has been published. The
    /// region from that merge to the completed publish is therefore a critical
    /// section: a second commit started inside it reads the group at N+1 and
    /// publishes commit(N+1 -> N+2) alongside a still-in-flight
    /// commit(N -> N+1). The DS assigns journal order by arrival, not by which
    /// call started first, so the two can land inverted — peers at N drop the
    /// N+1 commit as wrong-epoch, apply the N one, and settle at N+1 while this
    /// device sits at N+2. That is precisely the "device-ahead" outcome
    /// ADR-0040 exists to prevent, and the pending-commit outbox does NOT
    /// recover it: both publishes succeeded, so both entries are acked and the
    /// restart sweep has nothing to re-publish.
    ///
    /// `ChatRepository` is `@MainActor`, which makes each individual statement
    /// atomic but releases isolation at every `await` — and the region above
    /// contains one (`dsClient.publishGroup`). Actor isolation prevents data
    /// races; it does not prevent reentrancy. This chain does.
    ///
    /// ## Contract for `body`
    ///
    /// `body` runs AFTER an unbounded wait, so it must RE-READ
    /// ``canonicalIdentity`` and ``groups`` rather than close over values
    /// captured before the wait: a predecessor that aborted its persist calls
    /// ``resetGroupToDurableSnapshot(groupIdHex:)``, which REPLACES the
    /// `GroupRecord` (and can evict it entirely). Every `…Locked` method below
    /// re-derives both from `self` for exactly this reason.
    ///
    /// ## Deadlock audit
    ///
    /// The chain is not reentrant, so `body` must not call another
    /// commit-locked entry point. It does not: the locked sections reach only
    /// `Group` (synchronous UniFFI), ``persistCommitEntryThenSnapshotOrThrow(_:)``,
    /// ``resetGroupToDurableSnapshot(groupIdHex:)``, ``ackCommitBestEffort(entryId:commitMessageId:secondMessageId:)``,
    /// `DsClient.publishGroup` / `publishMailbox`, and
    /// `ChatClient.markOwnPublishedCommit` — none of which call back in.
    /// ``redistributePushRoutingSecret(groupIdHex:)`` is reached from the add
    /// path but only SPAWNS a detached `Task` and returns, so the routing lock
    /// it eventually takes is never awaited while this one is held. The
    /// ADR-0043 rotation half of ``removeMemberFromGroup(groupIdHex:leafIndex:)``
    /// is deliberately left OUTSIDE this lock (it publishes an application
    /// message, not a commit), so the two chains are never nested at all.
    ///
    /// G-38 re-audit, for the poll section: ``applyFetchedBatch(groupIdHex:after:raw:)``
    /// is NON-SUSPENDING, so it cannot await anything at all, let alone this
    /// lock. Its two follow-ups that do eventually take the ADR-0044 routing
    /// chain — ``receivePushRoutingSecret(groupIdHex:secret:mintEpoch:senderLeaf:)``
    /// and ``rotatePushRoutingSecretIfMembershipShrank(groupIdHex:previousMemberKeys:)``
    /// — only SPAWN a `Task` and return, so neither is awaited under this lock.
    /// The two entry points on the poll path that DO publish are both outside
    /// it: the DS fetch (by design, so a 30 s long poll blocks nothing) and
    /// ``sweepPendingCommits(record:)``, which re-publishes commits and would
    /// deadlock against the apply's acquisition if it were moved inside.
    ///
    /// ## Why the publish is not bounded by a timeout
    ///
    /// The ADR-0044 review required bounding network I/O taken under the
    /// push-routing lock, because there the work queued behind a wedged row is
    /// a rotation that must proceed. Here the inverse holds: the work queued
    /// behind is another COMMIT, and releasing the lock while a publish's fate
    /// is unknown re-opens the exact device-ahead hazard the lock closes. The
    /// publish is bounded anyway by `URLSession`'s own request timeout, which
    /// surfaces as a thrown publish failure with the outbox entry left un-acked
    /// — the state the restart sweep is built to recover.
    private func withCommitLock<T>(
        groupIdHex: String,
        _ body: @escaping @MainActor () async throws -> T
    ) async throws -> T {
        // Read the tail and replace it with NO `await` in between: `@MainActor`
        // cannot interleave another caller across a straight-line region, so two
        // callers can never observe the SAME predecessor and run beside each
        // other.
        let predecessor = commitChain[groupIdHex]
        let run = Task { @MainActor () async throws -> T in
            // Wait for everything already queued for this group. A predecessor
            // that threw must not block its successor, hence the discarded
            // result on a `Never`-failing erased tail.
            _ = await predecessor?.value
            return try await body()
        }
        // The erased tail completes exactly when `run` does, so a successor
        // chaining on it observes this section's writes.
        let tail = Task { @MainActor in _ = try? await run.value }
        commitChain[groupIdHex] = tail
        // Tail-guarded release — a successor that queued behind this section is
        // the tail now, and dropping it would let the caller after it start
        // beside it. Runs on both the success and the throw path.
        defer {
            if commitChain[groupIdHex] == tail {
                commitChain.removeValue(forKey: groupIdHex)
            }
        }
        return try await run.value
    }

    // MARK: - ADR-0040 (G-35) durable pending-commit outbox

    /// Persist the pending-commit outbox entry AND the N+1 MLS snapshot durably,
    /// FAIL-CLOSED, BEFORE any publish — the load-bearing step of every
    /// group-membership commit path.
    ///
    /// iOS has NO cross-store atomic transaction (the outbox is a per-entry file,
    /// the snapshot a separate AES-GCM file), so this uses the ADR-0040-sanctioned
    /// **entry-first, then snapshot** ordering. Both writes are fail-closed
    /// (throw): if EITHER throws, the caller ABORTS before publishing, so the
    /// group is never advanced on the wire without a durable, re-publishable
    /// record of the commit.
    ///
    /// The one torn middle state — outbox entry landed, snapshot still at N —
    /// provably means the commit was NEVER published (publish is strictly after
    /// BOTH writes), and ``sweepPendingCommits(record:)`` DISCARDS such an orphan
    /// entry (its `targetEpoch` was not reached by the durably-restored group).
    /// The forbidden state "snapshot at N+1 with no outbox entry" is impossible:
    /// the entry is written strictly first.
    ///
    /// Unlike the send-path ``persistSnapshotDurableOrThrow()`` (a no-op when
    /// there is no identity / account — that path has no reload that could
    /// re-consume a generation), this THROWS when the identity / device key is
    /// unavailable: a commit we cannot durably record must NOT be published.
    private func persistCommitEntryThenSnapshotOrThrow(_ entry: NewCommitEntry) throws {
        guard let identity = canonicalIdentity,
              let accountIdB64 = currentAccountIdB64 else {
            throw ChatError.argument(
                "ChatRepository not bootstrapped — commit aborted before publish"
            )
        }
        // 1) ENTRY FIRST (fail-closed). Holds the commit + Welcome / rendezvous
        //    bytes so a crash before / during publish can re-publish them.
        try commitOutbox.putNewDurable(accountIdB64: accountIdB64, entry: entry)
        // 2) SNAPSHOT SECOND (fail-closed). Advances the durable MLS state to
        //    N+1. A throw here leaves the torn entry-then-snapshot state the
        //    sweep discards (the commit is never published because we abort).
        let blob = try identity.snapshotStorage()
        try snapshotStore.putSnapshotDurable(accountIdB64: accountIdB64, blob: blob)
    }

    /// ADR-0040 (G-35) F1 — reset the in-memory MLS group for `groupIdHex` back
    /// to the last DURABLE snapshot (epoch N), dropping the un-published eager
    /// merge a commit path performed just before its FAIL-CLOSED persist aborted.
    ///
    /// Every group-membership create-commit (`setGroupRoles` / `addMemberFromWire`
    /// / `removeMember` / `approveExternalJoin`) EAGER-MERGES epoch N -> N+1 into
    /// the shared in-memory OpenMLS provider BEFORE
    /// ``persistCommitEntryThenSnapshotOrThrow(_:)`` durably records it. When that
    /// persist THROWS, the op correctly aborts before any publish — but the
    /// in-memory `Group` is left at N+1. A SECOND commit in the SAME session would
    /// then build on the un-published N+1 and publish commit(N+1 -> N+2); peers
    /// still at N drop it as wrong-epoch, stranding the WHOLE group (the
    /// "device-ahead" outcome ADR-0040 flags as worse than the single-device
    /// brick). This makes the abort "as if the op never happened" IN MEMORY too.
    ///
    /// Mechanism: because the durable persist FAILED, the on-disk snapshot is
    /// still at N — ``IdentitySnapshotStore/putSnapshotDurable(accountIdB64:blob:)``
    /// writes `.atomic`, so a failure leaves the prior N-envelope intact (and the
    /// entry-first ordering means a snapshot write is only reached after the entry
    /// landed; an entry-write failure never touches the snapshot at all). We
    /// restore that snapshot into the canonical identity's provider —
    /// `Identity.restoreStorage` REPLACES the whole provider map, discarding the
    /// eager-merged N+1 — and REBUILD the `Group` handle via `loadGroup`. The
    /// UniFFI `Group` holds an in-memory `MlsGroup` copy loaded once, so the stale
    /// N+1 handle must be DISCARDED + reloaded, not mutated in place. This mirrors
    /// exactly what ``bootstrap(secrets:)`` does on a cold start. The plaintext
    /// timeline (messages + cursor) is preserved — a membership commit appends no
    /// timeline row, and the log is independent of MLS state.
    ///
    /// Scope note: `restoreStorage` rolls the ENTIRE provider back to the durable
    /// snapshot (the only rollback primitive available without a `lakko-mls`
    /// change), but only the affected group's handle is rebuilt. In the normal
    /// case every other group already equals the durable snapshot (sends persist
    /// fail-closed; poll advances persist best-effort), so their handles stay
    /// consistent; the sole divergence is a group whose earlier best-effort persist
    /// silently failed — a pre-existing risk this reset neither creates nor
    /// worsens, and which self-heals on its next poll.
    ///
    /// FAIL-SAFE: if the durable snapshot is missing / unrestorable / unreloadable
    /// (should not happen for an existing, rehydrated group), the poisoned N+1
    /// handle is EVICTED from the map and a hard error is thrown — a later commit
    /// then fails "no live Group handle" rather than silently building on the
    /// un-published N+1.
    private func resetGroupToDurableSnapshot(groupIdHex: String) throws {
        guard let identity = canonicalIdentity,
              let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex] else {
            throw ChatError.argument(
                "cannot reset \(groupIdHex) to durable snapshot — repository not bootstrapped"
            )
        }
        guard let blob = snapshotStore.getSnapshot(accountIdB64: accountIdB64) else {
            // No durable snapshot for an existing group is a hard, unexpected
            // state. Evict the poisoned N+1 handle so a later commit fails "no
            // handle" rather than building on the un-published epoch.
            groups.removeValue(forKey: groupIdHex)
            state = state.with(threads: buildThreadList())
            throw ChatError.argument(
                "no durable MLS snapshot to reset \(groupIdHex) after a commit persist-abort"
            )
        }
        do {
            // Roll the shared provider back to N (REPLACES the eager-merged N+1).
            try identity.restoreStorage(snapshot: blob)
            // Rebuild the handle from the rolled-back provider — the old handle
            // still holds the stale N+1 in-memory `MlsGroup` copy.
            guard let reloaded = try loadGroup(
                identity: identity,
                groupId: record.groupId
            ) else {
                throw ChatError.argument("loadGroup returned nil for an existing group")
            }
            let fresh = GroupRecord(group: reloaded, groupId: record.groupId)
            // Preserve the plaintext timeline (MLS state is independent of the
            // message log; a membership commit appends no row).
            fresh.seed(messages: record.messages, cursor: record.cursor)
            groups[groupIdHex] = fresh
            state = state.with(threads: buildThreadList())
        } catch {
            // The restore / reload itself failed — do NOT leave the poisoned N+1
            // handle in the map. Evict it + surface a hard error.
            groups.removeValue(forKey: groupIdHex)
            state = state.with(threads: buildThreadList())
            throw ChatError.wrapped(
                "failed to reset \(groupIdHex) to durable snapshot after a commit persist-abort",
                underlying: error
            )
        }
    }

    /// Mark an outbox entry acked after BOTH artifacts have published.
    /// Best-effort: a failure leaves the entry un-acked, so the next restart
    /// sweep re-publishes it idempotently (peers drop the wrong-epoch duplicate;
    /// the committer's echo filter is re-armed).
    private func ackCommitBestEffort(
        entryId: String,
        commitMessageId: Int64?,
        secondMessageId: Int64? = nil
    ) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        commitOutbox.markAckedBestEffort(
            accountIdB64: accountIdB64,
            id: entryId,
            commitMessageId: commitMessageId,
            secondMessageId: secondMessageId
        )
    }

    /// Restart / group-open sweep (ADR-0040, G-35). Runs ONCE per group per
    /// session (from ``pollGroup(groupIdHex:waitSecs:)``, before the first poll).
    ///
    /// The pure ``planSweep(currentEpoch:entries:)`` classifies each un-acked
    /// entry against the group's durably-restored `Group.epoch()`
    /// (torn-write reconciliation):
    ///
    ///   - `currentEpoch >= targetEpoch` -> the commit WAS applied durably ->
    ///     re-publish idempotently + re-arm the own-echo filter, then ack.
    ///   - `currentEpoch <  targetEpoch` -> a torn entry-then-snapshot write
    ///     whose snapshot was lost -> the commit was NEVER published -> DISCARD
    ///     the orphan entry, stay at N (clean).
    private func sweepPendingCommits(record: GroupRecord) async {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        let groupIdB64 = base64Encode(record.groupId)
        let entries = commitOutbox.loadUnacked(
            accountIdB64: accountIdB64,
            groupIdB64: groupIdB64
        )
        guard !entries.isEmpty else { return }
        // The durably-restored epoch. A failure to read it is treated as 0
        // (fail-safe -> nothing is <= 0 unless targetEpoch is 0 too, so a
        // genuine pending entry is discarded rather than blindly re-published
        // against an unknown-epoch group).
        let currentEpoch = (try? record.group.epoch()) ?? 0
        for action in Self.planSweep(currentEpoch: currentEpoch, entries: entries) {
            switch action {
            case .discardTornWrite(let entry):
                // The snapshot never reached targetEpoch -> the commit was never
                // published -> drop the orphan, the device stays at N.
                commitOutbox.deleteEntryBestEffort(
                    accountIdB64: accountIdB64,
                    id: entry.id
                )
            case .republish(let entry):
                await republishPendingCommit(
                    record: record,
                    accountIdB64: accountIdB64,
                    entry: entry
                )
            }
        }
    }

    /// Pure sweep planner (ADR-0040 torn-write reconciliation) — see
    /// ``sweepPendingCommits(record:)``. Split out from the executing sweep so
    /// the load-bearing decision is directly unit-testable.
    ///
    /// An unparseable `targetEpoch` is treated as unreachable (`UInt64.max`) ->
    /// discard — a malformed entry can never be blindly re-published.
    ///
    /// `nonisolated` — a pure function over its arguments with no actor state, so
    /// callers (and unit tests) invoke it from any context.
    nonisolated static func planSweep(
        currentEpoch: UInt64,
        entries: [PendingCommitEntry]
    ) -> [CommitSweepAction] {
        entries.map { entry in
            let target = UInt64(entry.targetEpoch) ?? UInt64.max
            return currentEpoch >= target
                ? .republish(entry)
                : .discardTornWrite(entry)
        }
    }

    /// Re-publish one un-acked entry idempotently + re-arm the echo filter, then
    /// ack. A re-publish failure (offline / DS down) leaves the entry un-acked
    /// for the next sweep — safe. A corrupt (undecodable) commit is dropped.
    private func republishPendingCommit(
        record: GroupRecord,
        accountIdB64: String,
        entry: PendingCommitEntry
    ) async {
        guard let commitBytes = base64Decode(entry.commitB64) else {
            // Cannot re-publish garbage — drop the corrupt entry.
            commitOutbox.deleteEntryBestEffort(accountIdB64: accountIdB64, id: entry.id)
            return
        }
        do {
            let commitPub = try await dsClient.publishGroup(
                groupId: record.groupId,
                payload: commitBytes
            )
            // Echo re-arm: the device is durably at N+1, so feeding its own
            // re-fetched commit into `processIncoming` would raise
            // `CannotDecryptOwnMessage`. Suppress it on the very next poll.
            chatClient.markOwnPublishedCommit(messageId: commitPub.id)

            var secondId: Int64?
            if let secondB64 = entry.secondArtifactB64,
               let secondBytes = base64Decode(secondB64) {
                if entry.secondArtifactKind == .welcome,
                   let mailboxB64 = entry.secondArtifactMailboxB64,
                   let pseudonym = base64Decode(mailboxB64) {
                    // Welcome -> the new member's mailbox (NOT this device's
                    // group journal), so no echo re-arm — the committer never
                    // re-fetches it.
                    secondId = try await dsClient.publishMailbox(
                        pseudonym: pseudonym,
                        payload: secondBytes
                    ).id
                } else {
                    // Rendezvous -> the group journal. Existing members / the
                    // committer drop it (not an MLS message) in their receive
                    // loop's catch.
                    secondId = try await dsClient.publishGroup(
                        groupId: record.groupId,
                        payload: secondBytes
                    ).id
                }
            }
            ackCommitBestEffort(
                entryId: entry.id,
                commitMessageId: commitPub.id,
                secondMessageId: secondId
            )
        } catch {
            // Offline / DS down -> leave un-acked; the next sweep retries. Safe.
            NSLog("commit-outbox re-publish failed, will retry: \(entry.id) \(error)")
        }
    }

    private func persistSnapshotBestEffort() {
        guard let identity = canonicalIdentity,
              let accountIdB64 = currentAccountIdB64 else { return }
        do {
            let blob = try identity.snapshotStorage()
            snapshotStore.putSnapshot(
                accountIdB64: accountIdB64,
                blob: blob
            )
        } catch {
            // best-effort
        }
        // Phase 2a3f1 — co-located with the MLS snapshot
        // persist so every mutation that ratchets the group
        // also writes the resulting plaintext-message log.
        // Keeps the on-disk pair in sync without dispersing
        // call sites.
        persistMessageLogBestEffort()
    }

    /// Capture the current `(cursor, messages)` map across
    /// every known group and write it to the encrypted message
    /// log (Phase 2a3f1). Best-effort — Keychain / FS failures
    /// log + swallow.
    private func persistMessageLogBestEffort() {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        var slots: [String: PersistedGroup] = [:]
        for (hex, record) in groups {
            slots[hex] = PersistedGroup(
                cursor: record.cursor,
                messages: record.messages.map { PersistedMessage(from: $0) }
            )
        }
        let payload = MessageLogPayload(groups: slots)
        messageLogStore.putLog(accountIdB64: accountIdB64, payload: payload)
    }

    private func buildThreadList() -> [ChatThread] {
        groups.values.map { $0.toThread() }
            .sorted { $0.groupIdHex < $1.groupIdHex }
    }

    /// Decode a lowercase hex string into bytes. Returns `nil` on
    /// any malformed input (odd length, non-hex chars).
    private static func hexToBytes(_ hex: String) -> Data? {
        guard hex.count % 2 == 0 else { return nil }
        var out = Data()
        out.reserveCapacity(hex.count / 2)
        var i = hex.startIndex
        while i < hex.endIndex {
            let next = hex.index(i, offsetBy: 2)
            let byteStr = hex[i..<next]
            guard let byte = UInt8(byteStr, radix: 16) else {
                return nil
            }
            out.append(byte)
            i = next
        }
        return out
    }
}

// MARK: - State shape

/// Snapshot of every ``ChatRepository`` field a UI cares about.
public struct ChatRepositoryState: Equatable {
    public let threads: [ChatThread]
    public let bootstrapped: Bool

    public init(threads: [ChatThread] = [], bootstrapped: Bool = false) {
        self.threads = threads
        self.bootstrapped = bootstrapped
    }

    public func with(
        threads: [ChatThread]? = nil,
        bootstrapped: Bool? = nil
    ) -> ChatRepositoryState {
        ChatRepositoryState(
            threads: threads ?? self.threads,
            bootstrapped: bootstrapped ?? self.bootstrapped
        )
    }
}

/// One conversation thread — keyed by hex group id.
public struct ChatThread: Equatable {
    /// Hex-encoded MLS group id; used as the DS journal key and as
    /// the route argument for the 1×1 ChatScreen.
    public let groupIdHex: String
    public let messages: [ChatMessage]
    /// Cursor for the next ``ChatClient/pollMessages(identity:group:after:waitSecs:)``
    /// call.
    public let cursor: Int64

    public init(groupIdHex: String, messages: [ChatMessage], cursor: Int64) {
        self.groupIdHex = groupIdHex
        self.messages = messages
        self.cursor = cursor
    }
}

/// One message in a ``ChatThread``.
///
/// Phase 1.19c3: now carries an optional `attachments` list
/// alongside `text`. Outgoing messages populated locally on
/// ``ChatRepository/sendMessage(groupIdHex:plaintext:attachments:)``;
/// incoming messages populated from the parsed envelope on
/// ``ChatClient/pollMessages(identity:group:after:waitSecs:)``.
/// Empty list = pre-1.19-style text-only message; identical
/// rendering to before.
public struct ChatMessage: Equatable, Identifiable {
    public let messageId: Int64
    public let direction: Direction
    public internal(set) var text: String
    public let attachments: [AttachmentRef]
    public let createdAt: String
    /// ADR-0023 — the original author's MLS leaf index. Set on
    /// arrival from the Application frame's sender leaf; for our
    /// own outgoing messages it is `ownLeafIndex()`. `nil` for a
    /// frame whose sender leaf was unavailable (a system row, an
    /// external sender) — such a message can never be a valid
    /// edit / delete target (the author-only gate fails closed).
    public let authorLeaf: UInt32?
    /// ADR-0023 — `true` after an author-gated edit op folded its
    /// new text into this message. The UI renders a subtle
    /// "(edited)" marker.
    public internal(set) var edited: Bool
    /// ADR-0023 — `true` after an author-gated delete op
    /// tombstoned this message. The UI renders a muted "message
    /// deleted" placeholder in place of the body.
    public internal(set) var deleted: Bool
    /// ADR-0023 — reactions folded onto this message, one entry
    /// per `(reactorLeaf, emoji)`. Re-receiving the same pair
    /// toggles the entry off. The UI aggregates by emoji into
    /// chips with counts.
    public internal(set) var reactions: [Reaction]

    public init(
        messageId: Int64,
        direction: Direction,
        text: String,
        attachments: [AttachmentRef] = [],
        createdAt: String,
        authorLeaf: UInt32? = nil,
        edited: Bool = false,
        deleted: Bool = false,
        reactions: [Reaction] = []
    ) {
        self.messageId = messageId
        self.direction = direction
        self.text = text
        self.attachments = attachments
        self.createdAt = createdAt
        self.authorLeaf = authorLeaf
        self.edited = edited
        self.deleted = deleted
        self.reactions = reactions
    }

    /// `Identifiable` conformance for SwiftUI `ForEach`. The
    /// messageId is server-assigned + monotonic per journal, so
    /// it doubles as the stable identity.
    public var id: Int64 { messageId }

    /// ADR-0023 — aggregate the reaction list into `(emoji,
    /// count, reactedByOwnLeaf)` rows for the chip UI. A nil
    /// `ownLeaf` (no handle) renders the chips without the
    /// "reacted by you" highlight.
    public func reactionSummary(ownLeaf: UInt32?) -> [ReactionTally] {
        var order: [String] = []
        var counts: [String: Int] = [:]
        var mine: Set<String> = []
        for r in reactions {
            if counts[r.emoji] == nil { order.append(r.emoji) }
            counts[r.emoji, default: 0] += 1
            if let own = ownLeaf, r.reactorLeaf == own { mine.insert(r.emoji) }
        }
        return order.map {
            ReactionTally(
                emoji: $0,
                count: counts[$0] ?? 0,
                reactedByMe: mine.contains($0)
            )
        }
    }
}

/// ADR-0023 — a single reaction: who (MLS leaf) reacted with
/// which emoji. Attribution is per-epoch + ephemeral (the
/// open-question on leaf renumbering is accepted for v1).
public struct Reaction: Equatable {
    public let reactorLeaf: UInt32
    public let emoji: String

    public init(reactorLeaf: UInt32, emoji: String) {
        self.reactorLeaf = reactorLeaf
        self.emoji = emoji
    }
}

/// ADR-0023 — aggregated reaction row for the chip UI.
public struct ReactionTally: Equatable, Identifiable {
    public let emoji: String
    public let count: Int
    /// `true` when the local member contributed this emoji —
    /// tapping the chip toggles it back off.
    public let reactedByMe: Bool

    public var id: String { emoji }

    public init(emoji: String, count: Int, reactedByMe: Bool) {
        self.emoji = emoji
        self.count = count
        self.reactedByMe = reactedByMe
    }
}

public enum Direction: Equatable {
    case outgoing
    case incoming
    case system
}

// MARK: - Internal per-group record

/// Mutable per-group state — separate from the immutable
/// ``ChatThread`` snapshot the StateFlow surfaces.
///
/// Class (reference type) rather than struct because the inner
/// `Group` UniFFI handle is a class-bound reference + we want
/// the mutation paths to update the same record without copy-
/// on-write.
private final class GroupRecord {
    let group: Group
    let groupId: Data
    private(set) var messages: [ChatMessage] = []
    private(set) var cursor: Int64 = 0

    init(group: Group, groupId: Data) {
        self.group = group
        self.groupId = groupId
    }

    /// Phase 2a3f1 — seed the in-memory timeline + cursor from
    /// a persistent snapshot. Called from `ChatRepository.bootstrap`
    /// after the live `Group` handle has been rehydrated; the
    /// MLS state + the message log are stored separately on
    /// disk and the order they get reattached doesn't matter.
    /// Idempotent on the message ids — duplicate seeds replace
    /// rather than append.
    func seed(messages: [ChatMessage], cursor: Int64) {
        self.messages = messages
        self.cursor = cursor
    }

    func appendOutgoing(
        text: String,
        attachments: [AttachmentRef] = [],
        messageId: Int64,
        createdAt: String,
        authorLeaf: UInt32?
    ) {
        messages.append(
            ChatMessage(
                messageId: messageId,
                direction: .outgoing,
                text: text,
                attachments: attachments,
                createdAt: createdAt,
                authorLeaf: authorLeaf
            )
        )
    }

    /// ADR-0023 — apply an edit op locally to our own outgoing
    /// message (the sender never receives its own app message
    /// back, so the send path mutates the timeline directly).
    func applyLocalEdit(target: Int64, newText: String) {
        guard let idx = messages.firstIndex(where: { $0.messageId == target }) else { return }
        guard !messages[idx].deleted else { return }
        messages[idx].text = newText
        messages[idx].edited = true
    }

    /// ADR-0023 — apply a delete op locally to our own outgoing
    /// message.
    func applyLocalDelete(target: Int64) {
        guard let idx = messages.firstIndex(where: { $0.messageId == target }) else { return }
        messages[idx].deleted = true
    }

    /// ADR-0023 — toggle our own reaction locally on `target`.
    func applyLocalReaction(target: Int64, emoji: String, ownLeaf: UInt32) {
        toggleReaction(target: target, reactorLeaf: ownLeaf, emoji: emoji)
    }

    func applyPollResult(
        _ result: PollMessagesResult,
        callSignalingSink: ((Data) -> Void)? = nil,
        chatStatusSink: ((ChatStatusEvent) -> Void)? = nil,
        pushRoutingSecretSink: ((Data, UInt64, UInt32?) -> Void)? = nil
    ) {
        for event in result.events {
            switch event {
            case .application(let messageId, let text, let attachments, let senderLeaf, let createdAt):
                messages.append(
                    ChatMessage(
                        messageId: messageId,
                        direction: .incoming,
                        text: text,
                        attachments: attachments,
                        createdAt: createdAt,
                        authorLeaf: senderLeaf
                    )
                )
            case .op(_, let op, let target, let text, let emoji, let senderLeaf, let createdAt):
                // ADR-0024 — typing / read-receipt ops render NO bubble
                // and never touch the timeline. They surface to the view
                // model via `chatStatusSink` (ephemeral typing state +
                // monotone read watermark). The ADR-0023 mutation ops
                // (edit / delete / reaction) still fold into the target.
                switch op {
                case .typing(let isTyping):
                    if let senderLeaf = senderLeaf {
                        chatStatusSink?(
                            .typing(
                                senderLeaf: senderLeaf,
                                isTyping: isTyping,
                                createdAt: createdAt
                            )
                        )
                    }
                    continue
                case .readReceipt:
                    if let senderLeaf = senderLeaf {
                        chatStatusSink?(
                            .readReceipt(senderLeaf: senderLeaf, upTo: target)
                        )
                    }
                    continue
                case .edit, .delete, .reaction:
                    applyIncomingOp(
                        op: op,
                        target: target,
                        text: text,
                        emoji: emoji,
                        senderLeaf: senderLeaf
                    )
                }
            case .system(let messageId, let systemKind, let createdAt):
                messages.append(
                    ChatMessage(
                        messageId: messageId,
                        direction: .system,
                        text: Self.systemRowText(systemKind),
                        createdAt: createdAt
                    )
                )
            case .callSignaling(_, let payloadBytes, _):
                // Phase 3a9 — route into the CallSession
                // state machine. Out-of-band — never
                // touches the chat timeline.
                callSignalingSink?(payloadBytes)
            case .externalJoinRequest:
                // ADR-0027 (G-3) — a non-member's external Add proposal,
                // retained pending an owner/admin approval. Not a timeline
                // row; the members sheet reads the retained set via
                // `pendingExternalJoins(groupIdHex:)`. The `state` republish
                // below nudges the view model to re-query when its sheet is
                // open.
                break
            case .pushRoutingSecret(_, let secret, let mintEpoch, let senderLeaf, _):
                // ADR-0042 — a `push_routing_secret` distribution envelope.
                // Not a timeline row: routed out to the repository, which owns
                // the push-routing store AND the live `Group` handle the owner
                // gate needs. The MLS-authenticated `senderLeaf` rides along so
                // only the ADR-0022 owner can CHANGE the secret
                // (`lakko-security` MEDIUM-1); on accept the repository persists
                // it (idempotent) + subscribes under the derived opaque
                // `push_gid` (invariant #5). ADR-0044 — the `mintEpoch` rides
                // along so the gate can also ask whether the envelope is NEWER
                // than what is held, which is what refuses a stale replay.
                pushRoutingSecretSink?(secret, mintEpoch, senderLeaf)
            }
        }
        cursor = result.cursor
    }

    /// ADR-0023 — fold an incoming edit / delete / reaction op
    /// into its target message. The poll is incremental so each
    /// op is applied exactly once.
    ///
    /// **Author-only gate** (the security core): an edit or
    /// delete is applied ONLY when the op's sender leaf equals
    /// the target message's stored author leaf; otherwise it is
    /// dropped (fail-closed — invariant #4). An unknown target or
    /// an op with no sender leaf is likewise dropped. A reaction
    /// is accepted from any member (reacting to others' messages
    /// is the point) but attributed to its sender leaf for dedup
    /// + per-user toggle.
    private func applyIncomingOp(
        op: OpKind,
        target: Int64,
        text: String,
        emoji: String?,
        senderLeaf: UInt32?
    ) {
        guard let senderLeaf = senderLeaf else { return }
        guard let idx = messages.firstIndex(where: { $0.messageId == target }) else { return }
        switch op {
        case .edit:
            guard messages[idx].authorLeaf == senderLeaf else { return }
            guard !messages[idx].deleted else { return }
            messages[idx].text = text
            messages[idx].edited = true
        case .delete:
            guard messages[idx].authorLeaf == senderLeaf else { return }
            messages[idx].deleted = true
        case .reaction:
            guard let emoji = emoji, !emoji.isEmpty else { return }
            toggleReaction(target: target, reactorLeaf: senderLeaf, emoji: emoji)
        case .typing, .readReceipt:
            // ADR-0024 — typing / read-receipt ops are routed out of
            // the timeline fold in `applyPollResult` (they render no
            // bubble), so they never reach this path. Handled here only
            // to keep the switch exhaustive.
            return
        }
    }

    /// Toggle a `(reactorLeaf, emoji)` pair on `target`: present
    /// → remove, else add. Shared by the incoming-op fold + the
    /// local own-reaction path.
    private func toggleReaction(target: Int64, reactorLeaf: UInt32, emoji: String) {
        guard let idx = messages.firstIndex(where: { $0.messageId == target }) else { return }
        if let existing = messages[idx].reactions.firstIndex(
            where: { $0.reactorLeaf == reactorLeaf && $0.emoji == emoji }
        ) {
            messages[idx].reactions.remove(at: existing)
        } else {
            messages[idx].reactions.append(
                Reaction(reactorLeaf: reactorLeaf, emoji: emoji)
            )
        }
    }

    func toThread() -> ChatThread {
        ChatThread(
            groupIdHex: DsClient.pseudonymHex(groupId),
            messages: messages,
            cursor: cursor
        )
    }

    private static func systemRowText(_ kind: SystemKind) -> String {
        switch kind {
        case .commit: return "[mls commit]"
        case .proposal: return "[mls proposal]"
        case .externalJoinProposal: return "[mls external join]"
        }
    }
}
