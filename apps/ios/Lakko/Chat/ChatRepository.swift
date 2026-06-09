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
/// map is mutated under an actor-style serial DispatchQueue so a
/// `sendMessage` and a parallel `pollAllGroups` don't race against
/// one another's `ChatClient` calls (the underlying OpenMLS storage
/// isn't internally synchronised for concurrent writes against the
/// same group).
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

    /// The canonical ``Identity`` for the current session. `nil`
    /// until ``bootstrap(secrets:)`` populates it.
    private var canonicalIdentity: Identity?

    /// Account id (base64) of the session that bootstrapped this
    /// repository.
    private var currentAccountIdB64: String?

    /// Map of hex group id → live ``Group`` handle + per-thread
    /// timeline / cursor. Mutated only on `@MainActor`; reads from
    /// the main actor are lock-free, MLS-mutation calls hop into
    /// `Task.detached` for the off-actor work.
    private var groups: [String: GroupRecord] = [:]

    public init(
        chatClient: ChatClient = ChatClient(),
        dsClient: DsClient = DsClient(),
        discovery: DiscoveryClient = DiscoveryClient(),
        snapshotStore: IdentitySnapshotStore = IdentitySnapshotStore(),
        knownGroupsStore: KnownGroupsStore = KnownGroupsStore(),
        messageLogStore: MessageLogStore = MessageLogStore()
    ) {
        self.chatClient = chatClient
        self.dsClient = dsClient
        self.discovery = discovery
        self.snapshotStore = snapshotStore
        self.knownGroupsStore = knownGroupsStore
        self.messageLogStore = messageLogStore
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
                    // 1.20e — subscribe to push wakes for each
                    // rehydrated group. Idempotent server-side
                    // so re-subscribing on every cold start is
                    // free.
                    Task { [groupId] in
                        await Self.subscribeGroupPushBestEffort(groupId: groupId)
                    }
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
        // 1.20e — subscribe to push wakes for this new group.
        Task { [groupId] in
            await Self.subscribeGroupPushBestEffort(groupId: groupId)
        }
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
        // 1.20e — subscribe to push wakes for the just-joined
        // group. Same fire-and-forget pattern as the create
        // path above; idempotent server-side.
        Task { [groupId] in
            await Self.subscribeGroupPushBestEffort(groupId: groupId)
        }
        return groupIdHex
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
        record.appendOutgoing(
            text: plaintext,
            attachments: attachments,
            messageId: result.messageId,
            createdAt: result.createdAt
        )
        persistSnapshotBestEffort()
        state = state.with(threads: buildThreadList())
        // 1.20e — fan a wake out to every device subscribed to
        // this group. Fire-and-forget — the message is already
        // on the DS and recipients pick it up on next poll
        // even if push-gw is down. Off the user-visible send
        // critical path so the PoW solve doesn't double send
        // latency.
        Task { [groupId = record.groupId, messageId = result.messageId] in
            await Self.notifyPushGroupBestEffort(
                groupId: groupId,
                messageId: messageId
            )
        }
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

    /// 1.20e — push-gw wake fan-out. Best-effort, off the
    /// critical path. Errors logged via NSLog, never re-thrown.
    private static func notifyPushGroupBestEffort(
        groupId: Data,
        messageId: Int64
    ) async {
        do {
            _ = try await PushClient().notifyGroup(
                groupPseudonym: groupId,
                envelopePointer: messageId
            )
        } catch {
            NSLog("push notify-group failed: \(error)")
        }
    }

    /// 1.20e — subscribe this device to push wakes for the
    /// given group. Fired after every successful group join /
    /// create / rehydrate; server-side idempotent so re-runs
    /// are cheap no-ops.
    private static func subscribeGroupPushBestEffort(groupId: Data) async {
        do {
            let pseudonym = try DevicePushId.getOrCreate()
            _ = try await PushClient().subscribeGroup(
                devicePseudonym: pseudonym,
                groupPseudonym: groupId
            )
        } catch {
            NSLog("push group subscribe failed: \(error)")
        }
    }

    /// Pull new messages from one group's journal + apply them to
    /// the in-memory timeline.
    public func pollGroup(
        groupIdHex: String,
        waitSecs: Int = 0
    ) async throws {
        guard let identity = canonicalIdentity else { return }
        guard let record = groups[groupIdHex] else { return }
        let result = try await chatClient.pollMessages(
            identity: identity,
            group: record.group,
            after: record.cursor,
            waitSecs: waitSecs
        )
        record.applyPollResult(result) { [weak self] payload in
            self?.callSignalingSink?(groupIdHex, payload)
        }
        if !result.events.isEmpty {
            persistSnapshotBestEffort()
        }
        state = state.with(threads: buildThreadList())
    }

    /// Phase 3a9 — closure the UI attaches to dispatch
    /// inbound call-signaling bytes into its
    /// ``CallSession`` state machine. `nil` when no call
    /// surface is attached; the per-record poll just drops
    /// the payload.
    public var callSignalingSink: ((String, Data) -> Void)?

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
    public let text: String
    public let attachments: [AttachmentRef]
    public let createdAt: String

    public init(
        messageId: Int64,
        direction: Direction,
        text: String,
        attachments: [AttachmentRef] = [],
        createdAt: String
    ) {
        self.messageId = messageId
        self.direction = direction
        self.text = text
        self.attachments = attachments
        self.createdAt = createdAt
    }

    /// `Identifiable` conformance for SwiftUI `ForEach`. The
    /// messageId is server-assigned + monotonic per journal, so
    /// it doubles as the stable identity.
    public var id: Int64 { messageId }
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
        createdAt: String
    ) {
        messages.append(
            ChatMessage(
                messageId: messageId,
                direction: .outgoing,
                text: text,
                attachments: attachments,
                createdAt: createdAt
            )
        )
    }

    func applyPollResult(
        _ result: PollMessagesResult,
        callSignalingSink: ((Data) -> Void)? = nil
    ) {
        for event in result.events {
            switch event {
            case .application(let messageId, let text, let attachments, let createdAt):
                messages.append(
                    ChatMessage(
                        messageId: messageId,
                        direction: .incoming,
                        text: text,
                        attachments: attachments,
                        createdAt: createdAt
                    )
                )
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
            }
        }
        cursor = result.cursor
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
