// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Application-message orchestrator on top of MLS groups.
///
/// Mirror of:
///
///   * `apps/web/src/chat/client.ts` (1.17f2)
///   * `apps/android/.../chat/ChatClient.kt` (1.15f2)
///
/// Two operations:
///
///   * ``sendMessage(identity:group:plaintext:)`` —
///     `Group.createApplicationMessage(identity:plaintext:)` →
///     POST to `/ds/group/{group_id_hex}/messages` via
///     ``DsClient/publishGroup(groupId:payload:)``. Returns the DS
///     message id (also the cursor value).
///
///   * ``pollMessages(identity:group:after:waitSecs:)`` —
///     ``DsClient/fetchGroup(groupId:after:limit:waitSecs:)`` for
///     the group's journal → for each row,
///     `group.processIncoming(identity:bytes:)` → dispatch to
///     ``IncomingChatEvent``. Application messages carry plaintext
///     + an `.application(_:)` discriminator; commits / proposals
///     advance MLS state but surface as `.system(...)` rows the UI
///     can render as a thin separator (or skip).
///
/// ## Self-message echo
///
/// `Group.createApplicationMessage` produces wire bytes that look
/// like a normal MLS application message — the DS does not
/// deduplicate, and a local poll WILL pick our own messages back
/// up. OpenMLS can't decrypt the sender's own application messages
/// at the same epoch, so it would surface as a `processIncoming`
/// failure: we'd see a "can't decrypt own message" exception,
/// swallow it, and the UI would never see the row.
///
/// Instead we **track the rows of our own publishes** during the
/// session and drop matches on receive. The registry lives on the
/// ``ChatClient`` instance — swapping clients (e.g. logout) loses
/// the list; pending echoes after a swap surface as the
/// processIncoming-failed path above, which is harmless but
/// leaves a single noisy log entry per orphaned echo.
public final class ChatClient {

    private let ds: DsClient

    /// Rows this device published itself, so the next poll drops the echo.
    ///
    /// Keyed on `(journal, id)` — see ``OwnEchoRegistry``. It used to be a bare
    /// `Set<Int64>` of ids, and because a DS id is assigned per journal, an id
    /// published in one chat swallowed the next row numbered the same in a
    /// DIFFERENT chat — cursor already past it, so the row was gone for good.
    /// That dropped ordinary messages, reactions, read receipts, membership
    /// commits and incoming call offers alike; it showed up as an incoming call
    /// that rang only while its own chat was the visible screen.
    private let ownPublished = OwnEchoRegistry()

    /// Fail-closed MLS-ratchet persister wired by ``ChatRepository``.
    ///
    /// ``sendMessage(identity:group:plaintext:)`` invokes it AFTER
    /// `createApplicationMessage` (which advances the sender ratchet to
    /// generation N+1) and BEFORE the DS publish. If it THROWS,
    /// `sendMessage` throws and never publishes — the gen-N ciphertext
    /// never leaves the device, so the ratchet is safe to re-consume
    /// generation N after a reload (nonce-reuse-on-crash defence, mirror
    /// of the web `ChatClient.onPersist` / `persistDurableOrThrow`).
    ///
    /// `nil` for tests / standalone callers with no reload-from-snapshot
    /// path — such callers persist nothing, so a reload restores no group
    /// and cannot re-consume a generation; they are outside this bug's
    /// blast radius, and the invocation is skipped. Production
    /// (``ChatRepository``) always wires it, so the gate is live there.
    public var ratchetPersister: (() async throws -> Void)?

    /// ADR-0053 — how a publish to a group journal is signed, wired by
    /// ``ChatRepository`` so the request carries the previous-epoch signer and
    /// the claim-on-open retry the repository holds. Keyed by the journal's
    /// hex id. `nil` (tests, standalone callers) or a `nil` answer falls back
    /// to signing with the group's own current-epoch signer.
    public var dsAuthProvider: (@MainActor (_ groupIdHex: String) -> DsAuth?)?

    /// The auth for a publish to `group`'s journal: the repository's when it
    /// has one, else the group's current-epoch signer, else unsigned (an
    /// unclaimed journal still accepts that).
    func dsAuth(identity: Identity, group: Group, groupId: Data) async -> DsAuth? {
        if let provider = dsAuthProvider,
           let provided = await provider(DsClient.pseudonymHex(groupId)) {
            return provided
        }
        return (try? group.dsAccessSigner(identity: identity)).map { DsAuth(signer: $0) }
    }

    public init(ds: DsClient = DsClient()) {
        self.ds = ds
    }

    /// Encrypt `plaintext` for the current group epoch and post
    /// the wire bytes to the DS group journal.
    ///
    /// Caller keeps `group` alive — the `Group` handle carries the
    /// MLS ratchet state; dropping it would lose every subsequent
    /// message. 1.14f5 wires snapshot persistence so a restart can
    /// rehydrate the state from disk.
    ///
    /// With ``ratchetPersister`` wired (production, via ``ChatRepository``),
    /// the post-advance (generation N+1) ratchet snapshot is durably
    /// persisted **before** the DS publish, and a failed persist ABORTS
    /// the send (fail-closed) so the gen-N ciphertext never reaches the DS
    /// without its N+1 ratchet state on disk — see the persist-before-
    /// publish comment below for the nonce-reuse-on-crash rationale.
    ///
    /// ADR-0054 decision 1 — `wake`, when present, rides the publish as the
    /// `x-lakko-wake` header and the DS wakes the group's subscribed devices
    /// after appending the row. There is no separate push-gw request.
    public func sendMessage(
        identity: Identity,
        group: Group,
        plaintext: Data,
        wake: DsWake? = nil
    ) async throws -> SendMessageResult {
        guard !plaintext.isEmpty else {
            throw ChatError.argument("plaintext must be non-empty")
        }

        // `createApplicationMessage` ADVANCES the MLS sender ratchet: it
        // consumes the current generation N (deriving the AEAD key+nonce
        // from N) and leaves the in-memory OpenMLS provider at generation
        // N+1. Because the provider is in-memory, that N+1 state lives
        // ONLY in the snapshot until it is persisted.
        let wireBytes: Data
        do {
            wireBytes = try group.createApplicationMessage(
                identity: identity,
                plaintext: plaintext
            )
        } catch {
            throw ChatError.wrapped(
                "createApplicationMessage failed",
                underlying: error
            )
        }

        let groupId: Data
        do {
            groupId = try group.groupId()
        } catch {
            throw ChatError.wrapped(
                "group.groupId unavailable",
                underlying: error
            )
        }

        // SECURITY (nonce-reuse-on-crash) — PERSIST BEFORE PUBLISH,
        // FAIL-CLOSED. The N+1 ratchet state produced just above MUST be
        // durably on disk before the gen-N ciphertext reaches the DS.
        // Otherwise a crash (or a silently-failed persist) between the
        // publish and the persist would reload the ratchet at N, and the
        // next send would re-consume generation N — an identical
        // (key, nonce) on a different plaintext (C1 XOR C2 = P1 XOR P2
        // plaintext leak to the zero-trust server + Poly1305 tag forgery).
        // So AWAIT the durable persist; if it does NOT land, ABORT — do
        // NOT publish — so the gen-N ciphertext never leaves this device
        // (only one gen-N ciphertext can ever exist on the wire, so gen N
        // stays safe to re-consume after a reload). A crash in the NEW
        // window (after this persist, before the publish) merely SKIPS
        // generation N; RFC 9420 receivers tolerate sender-ratchet gaps,
        // so there is no reuse either way. Mirror of the web
        // `persistDurableOrThrow()` call in `apps/web/src/chat/client.ts`.
        if let ratchetPersister {
            do {
                try await ratchetPersister()
            } catch {
                throw ChatError.wrapped(
                    "MLS snapshot persist failed — send aborted to prevent nonce reuse",
                    underlying: error
                )
            }
        }

        let publish: PublishResult
        do {
            publish = try await ds.publishGroup(
                groupId: groupId,
                payload: wireBytes,
                auth: await dsAuth(identity: identity, group: group, groupId: groupId),
                wake: wake
            )
        } catch {
            throw ChatError.wrapped("DS publish failed", underlying: error)
        }
        ownPublished.remember(
            groupIdHex: DsClient.pseudonymHex(groupId),
            messageId: publish.id
        )

        return SendMessageResult(
            messageId: publish.id,
            createdAt: publish.createdAt
        )
    }

    /// Register a DS message id as one we published — typically a
    /// commit returned by `ContactsClient.addMemberToGroup`'s
    /// broadcast step. The next
    /// ``pollMessages(identity:group:after:waitSecs:)`` will see the row and
    /// drop it via the ``OwnEchoRegistry``, same path
    /// ``sendMessage(identity:group:plaintext:)`` uses for application
    /// messages.
    ///
    /// `groupIdHex` is REQUIRED, not convenience: a DS id is assigned per
    /// journal, so an id alone names a row in every group at once and would
    /// silence an unrelated chat's next row of that number.
    public func markOwnPublishedCommit(groupIdHex: String, messageId: Int64) {
        ownPublished.remember(groupIdHex: groupIdHex, messageId: messageId)
    }

    /// Drop every outstanding registration for one journal.
    ///
    /// Called when this device leaves a group or its membership ends: that
    /// journal is never polled again (``ChatRepository/pollGroup(groupIdHex:waitSecs:)``
    /// gates on it), so nothing can ever consume those entries and they would
    /// sit in the registry for the life of the process.
    public func forgetOwnPublished(groupIdHex: String) {
        ownPublished.forget(groupIdHex: groupIdHex)
    }

    /// How many registrations are outstanding. Diagnostics and tests only.
    var ownPublishedCount: Int { ownPublished.count }

    /// ADR-0042 — broadcast a CONTROL application message to the group
    /// journal (currently: the push-routing-secret distribution envelope).
    ///
    /// Mirrors ``sendMessage(identity:group:plaintext:)``'s
    /// persist-before-publish fail-closed discipline —
    /// `createApplicationMessage` advances the sender ratchet, so the
    /// post-advance state MUST be durably persisted (via the wired
    /// ``ratchetPersister``) before the ciphertext reaches the DS (the AEAD
    /// nonce-reuse-on-crash class). Unlike ``sendMessage`` it fires NO
    /// user-facing push wake: a routing-secret handshake should not wake
    /// devices (a member without the secret cannot be woken under
    /// `push_gid` yet anyway — they catch up on the next foreground poll).
    ///
    /// Records the DS id in the own-published registry so the broadcaster's
    /// own next poll drops the echo (the owner already holds the secret it
    /// minted). Returns the DS message id.
    ///
    /// Never carries an ADR-0054 `x-lakko-wake` header: a control envelope
    /// is silent.
    @discardableResult
    public func broadcastControl(
        identity: Identity,
        group: Group,
        controlBytes: Data
    ) async throws -> Int64 {
        guard !controlBytes.isEmpty else {
            throw ChatError.argument("controlBytes must be non-empty")
        }

        let wireBytes: Data
        do {
            wireBytes = try group.createApplicationMessage(
                identity: identity,
                plaintext: controlBytes
            )
        } catch {
            throw ChatError.wrapped("createApplicationMessage failed", underlying: error)
        }

        let groupId: Data
        do {
            groupId = try group.groupId()
        } catch {
            throw ChatError.wrapped("group.groupId unavailable", underlying: error)
        }

        // PERSIST BEFORE PUBLISH, FAIL-CLOSED — same rationale as
        // `sendMessage`. A failed persist ABORTS the broadcast so the
        // gen-N ciphertext never reaches the DS without its N+1 ratchet
        // state durably on disk.
        if let ratchetPersister {
            do {
                try await ratchetPersister()
            } catch {
                throw ChatError.wrapped(
                    "MLS snapshot persist failed — control broadcast aborted to prevent nonce reuse",
                    underlying: error
                )
            }
        }

        let publish: PublishResult
        do {
            publish = try await ds.publishGroup(
                groupId: groupId,
                payload: wireBytes,
                auth: await dsAuth(identity: identity, group: group, groupId: groupId)
            )
        } catch {
            throw ChatError.wrapped("DS publishGroup (control) failed", underlying: error)
        }
        ownPublished.remember(
            groupIdHex: DsClient.pseudonymHex(groupId),
            messageId: publish.id
        )
        return publish.id
    }

    /// Pull all rows newer than `after` for the group's journal,
    /// decrypt application messages, return a list of discriminated
    /// ``IncomingChatEvent``s + a fresh cursor.
    ///
    /// Each `group.processIncoming(identity:bytes:)` mutates MLS
    /// provider storage (application messages drain keys, commits
    /// replace state, proposals queue). 1.14f5 wires a persist
    /// hook here so the snapshot stays in sync.
    ///
    /// Bad frames / wrong epochs / malformed wire are silently
    /// dropped — the cursor still advances so we don't re-process.
    /// Same posture as the contacts inbox poll.
    ///
    /// G-38 — the composition of the two halves below. The production caller
    /// (``ChatRepository/pollGroup(groupIdHex:waitSecs:)``) drives them
    /// SEPARATELY so the network half runs outside the per-group commit lock
    /// and only the MLS half runs inside it; this convenience form is what a
    /// caller with no lock to respect uses.
    public func pollMessages(
        identity: Identity,
        group: Group,
        after: Int64 = 0,
        waitSecs: Int = 0
    ) async throws -> PollMessagesResult {
        let groupId: Data
        do {
            groupId = try group.groupId()
        } catch {
            throw ChatError.wrapped(
                "group.groupId unavailable",
                underlying: error
            )
        }
        let raw = try await fetchGroupJournal(
            groupId: groupId,
            after: after,
            waitSecs: waitSecs,
            auth: await dsAuth(identity: identity, group: group, groupId: groupId)
        )
        return applyFetchedMessages(
            identity: identity,
            group: group,
            groupIdHex: DsClient.pseudonymHex(groupId),
            raw: raw,
            after: after
        )
    }

    /// G-38 — the NETWORK half of
    /// ``pollMessages(identity:group:after:waitSecs:)``, split out so the caller
    /// can run it OUTSIDE the per-group commit lock.
    ///
    /// Touches no MLS state whatsoever: a plain `GET` against the DS journal
    /// keyed by a `groupId` the caller already holds (the `GroupRecord` caches
    /// it), so not even the `Group` handle is read here.
    ///
    /// That separation is the whole point. `waitSecs` is a DELIBERATE long poll
    /// — 30 s on the live chat screen — and serializing it behind a membership
    /// commit's publish would recreate G-37 at a per-group blast radius: every
    /// send and commit on the group would queue behind an idle connection.
    ///
    /// The MLS half is
    /// ``applyFetchedMessages(identity:group:groupIdHex:raw:after:)``,
    /// which the caller runs under the lock. See
    /// ``ChatRepository/pollGroup(groupIdHex:waitSecs:)`` for the composition
    /// and the guards that keep the two halves consistent.
    public func fetchGroupJournal(
        groupId: Data,
        after: Int64 = 0,
        waitSecs: Int = 0,
        auth: DsAuth? = nil
    ) async throws -> FetchResult {
        do {
            return try await ds.fetchGroup(
                groupId: groupId,
                after: after,
                waitSecs: waitSecs,
                auth: auth
            )
        } catch {
            throw ChatError.wrapped("DS fetch failed", underlying: error)
        }
    }

    /// G-38 — the MLS half of
    /// ``pollMessages(identity:group:after:waitSecs:)``: apply an
    /// already-fetched batch.
    ///
    /// **DELIBERATELY NOT `async`.** Every statement below mutates OpenMLS
    /// provider state through the shared `Group` handle and must run inside the
    /// caller's per-group commit lock; being non-`async` is a STRUCTURAL
    /// guarantee that no network wait can ever be reintroduced into this
    /// critical section — a suspending call would simply not compile here. It
    /// also means the whole apply is one straight-line `@MainActor` region on
    /// the repository side, so it can never interleave with another
    /// synchronous region.
    ///
    /// `after` is the cursor the batch was fetched with, and seeds the returned
    /// cursor so an empty batch does not rewind it.
    ///
    /// `groupIdHex` names the journal the batch came off. It is REQUIRED
    /// because the own-echo filter below is per journal: a DS id identifies a
    /// row only together with the group whose journal assigned it.
    public func applyFetchedMessages(
        identity: Identity,
        group: Group,
        groupIdHex: String,
        raw: FetchResult,
        after: Int64 = 0
    ) -> PollMessagesResult {
        var events: [IncomingChatEvent] = []
        var cursor = after
        var touchedMlsState = false
        for message in raw.messages {
            cursor = message.id

            // Drop our own echoes — the DS doesn't dedupe and
            // OpenMLS can't decrypt them on the sender side at
            // the same epoch.
            //
            // Matched on THIS journal's id, never on the id alone. Ids are per
            // journal and every chat starts at 1, so an id-only match dropped
            // another chat's row of the same number — a fetched row, silently
            // discarded with the cursor advanced past it, which is what kept
            // incoming calls from ringing outside the open chat.
            if ownPublished.consume(groupIdHex: groupIdHex, messageId: message.id) {
                continue
            }

            // Marked BEFORE the call, not after — see
            // ``PollMessagesResult/touchedMlsState``.
            touchedMlsState = true
            let processed: ProcessedMessage
            do {
                // ADR-0022 — the CHAT receive loop enforces the group-admin
                // role policy: an add / remove / role-change commit whose
                // committer's role does not authorise it is REFUSED (throws
                // here) and skipped, never merged (fail-closed). A group with
                // no role table (legacy) behaves like the open
                // `processIncoming`. This is the chat group, not the SFU call
                // group (the call path reuses the chat exporter, not a separate
                // Group), so the call-path caveat does not apply here.
                processed = try group.processIncomingVerifyingRoles(
                    identity: identity,
                    bytes: message.payload
                )
            } catch {
                // Bad frame, wrong epoch, malformed wire, OR a role-unauthorized
                // commit refused by the gate. Drop + advance the cursor —
                // deliberately: a row that can never apply must not be able to
                // wedge the poll, and only a PERSIST failure holds the cursor.
                // The two are distinguishable because they are different
                // operations at different points in the batch.
                continue
            }
            events.append(
                Self.toEvent(
                    messageId: message.id,
                    createdAt: message.createdAt,
                    processed: processed,
                    // ADR-0023 — the roster is read HERE, one line after the
                    // apply, because that is the epoch this row decrypted at.
                    // A leaf resolved any later can name whoever was added into
                    // a vacated slot since.
                    group: group
                )
            )
        }

        return PollMessagesResult(
            events: events,
            cursor: cursor,
            hasMore: raw.hasMore,
            touchedMlsState: touchedMlsState
        )
    }

    // MARK: - ProcessedMessage → IncomingChatEvent

    /// ADR-0023 — **the production entry point**, and the reason it exists
    /// rather than every caller passing its own resolver.
    ///
    /// There are exactly TWO receive paths — this client's poll, and the
    /// ADR-0045 committer sweep's verdict apply in `ChatRepository`, which
    /// consumes journal rows the poll will never fetch again — and they must
    /// resolve authorship identically, or which path happened to reach a row
    /// decides who its author is. Handing each a closure left that agreement to
    /// two call sites keeping the same rule; taking the `Group` instead leaves
    /// them nothing to disagree about, so the poll's tests cover the sweep's
    /// resolution too. The closure form below stays for the classification
    /// tests, which drive the arms without a live group.
    ///
    /// Callers must pass the SAME `Group` handle they just applied the row
    /// through, on the line after applying it: that is the one moment the
    /// roster is the epoch's roster.
    static func toEvent(
        messageId: Int64,
        createdAt: String,
        processed: ProcessedMessage,
        group: Group
    ) -> IncomingChatEvent {
        toEvent(
            messageId: messageId,
            createdAt: createdAt,
            processed: processed,
            resolveSender: { AuthorIdentity.resolve(group: group, leafIndex: $0) },
            resolveDevice: { AuthorIdentity.resolveDevice(group: group, leafIndex: $0) }
        )
    }

    /// Internal rather than private so `LakkoTests` can drive the
    /// classification chain against a real ``ProcessedMessage`` and real
    /// FFI-composed envelopes. Reimplementing the chain in a test double is
    /// the failure mode this avoids: a double whose parse THROWS on binary
    /// input where the real ``parseBodyEnvelope(bytes:)`` DEGRADES would make
    /// the service-envelope tests pass with the filter arms deleted.
    ///
    /// `resolveSender` maps an MLS leaf index to the member's signature key
    /// against the group's roster AS IT STANDS NOW. Both callers invoke this
    /// immediately after `processIncomingVerifyingRoles` returned, which is the
    /// one moment "now" is the epoch the message was sealed at — an Application
    /// frame only decrypts at the current epoch, and a Commit that renumbers
    /// leaves is a separate journal row applied before or after, never during.
    /// Resolving later, from a stored leaf, is what lets a member added into a
    /// departed member's vacated leaf inherit that member's authorship. See
    /// ``AuthorIdentity``.
    static func toEvent(
        messageId: Int64,
        createdAt: String,
        processed: ProcessedMessage,
        resolveSender: (UInt32) -> MemberIdentity?,
        resolveDevice: (UInt32) -> MemberIdentity? = { _ in nil }
    ) -> IncomingChatEvent {
        switch processed {
        case .application(let data, let senderLeaf, _):
            // Phase 3a8/9 — sniff for call-signaling proto
            // bytes before running the chat-envelope parser.
            // Cheap unambiguous discriminator: the proto's
            // first wire tag is one of the CallSignalingPayload
            // body oneof tags (fields 10..16, see
            // `looksLikeCallSignaling`); the chat envelope
            // starts with `{` (0x7B). No overlap.
            if Self.looksLikeCallSignaling(data) {
                return .callSignaling(
                    messageId: messageId,
                    payloadBytes: data,
                    createdAt: createdAt,
                    senderLeaf: senderLeaf,
                    senderDeviceKey: senderLeaf.flatMap(resolveDevice),
                    senderKey: senderLeaf.flatMap(resolveSender)
                )
            }
            // ADR-0042 — sniff for a push-routing-secret distribution
            // envelope before the chat parser. The FFI codec is fail-closed
            // (domain + version + exact-length gate), so a non-envelope
            // plaintext returns `nil` and falls through to the normal chat
            // parse. Never a timeline bubble — the receive loop persists the
            // secret + subscribes under the derived opaque `push_gid`.
            // The MLS-authenticated `senderLeaf` rides along so the receive
            // side can enforce the owner gate (`lakko-security` MEDIUM-1) —
            // the same attribution the ADR-0023 author-only gate uses.
            if let decoded = PushGroupRouting.tryDecodeSecretEnvelope(data) {
                return .pushRoutingSecret(
                    messageId: messageId,
                    secret: decoded.secret,
                    mintEpoch: decoded.mintEpoch,
                    senderLeaf: senderLeaf,
                    createdAt: createdAt
                )
            }
            // ADR-0045 — sniff for a committer-liveness beacon before the
            // chat parser. Without this arm the beacon reaches
            // ``parseBodyEnvelope(bytes:)``, whose lossy-UTF-8 fallback
            // degrades it (correctly, per its own contract — legacy plaintext
            // must still render) into a visible bubble reading
            // `lakko/committer-beacon/v1` plus two control bytes. Never a
            // timeline row: it is an observation about who can sweep, and the
            // two things the field-less envelope deliberately omits — WHO and
            // HOW OLD — ride along from the MLS-authenticated `senderLeaf`
            // and the DS row's `createdAt`.
            if Self.looksLikeCommitterBeacon(data) {
                return .committerBeacon(
                    messageId: messageId,
                    senderLeaf: senderLeaf,
                    createdAt: createdAt
                )
            }
            // ADR-0051 — sniff for a group-name envelope before the chat
            // parser, for the same reason as the arms above: the lossy-UTF-8
            // fallback would otherwise render it as a bubble reading
            // `lakko/group-name/v1` plus binary. It carries the MLS-authenticated
            // `senderLeaf` onward (not a resolved key) because the receive side
            // resolves it against the role table exactly as the push-routing
            // secret does — owner / admin may set the name, a member may not.
            // A malformed envelope (a bound violation) is refused by the codec
            // as a whole and falls through to the chat parser like any other
            // unrecognised plaintext — the same stance the NotificationService
            // filter takes, so the timeline and the notification agree.
            if let groupName = Self.decodeGroupNameIfEnvelope(data) {
                return .groupName(
                    messageId: messageId,
                    name: groupName.name,
                    updatedAtMs: groupName.updatedAtMs,
                    senderLeaf: senderLeaf,
                    createdAt: createdAt
                )
            }
            // ADR-0052 decision 3 — sniff for a shared-pin envelope before the
            // chat parser, for the same reason as the group-name arm above:
            // the lossy-UTF-8 fallback would otherwise render it as a bubble
            // reading `lakko/pin/v1` plus binary. Never a timeline row. It
            // carries the MLS-authenticated `senderLeaf` onward (not a
            // resolved key) because the receive side resolves it against the
            // role table and the people count exactly as the group name does
            // (owner / admin, any member of a table-less group, either
            // participant of a one-to-one).
            if let pin = Self.decodePinIfEnvelope(data) {
                return .pin(
                    messageId: messageId,
                    op: pin,
                    senderLeaf: senderLeaf,
                    createdAt: createdAt
                )
            }
            // ADR-0023 — the identity that leaf names, resolved HERE because
            // here is where the roster is the one the message was sealed
            // under. Every downstream attribution reads this, never the leaf.
            //
            // Below the three service-envelope sniffs on purpose: those carry
            // the leaf onward and resolve it themselves against the role table
            // they each need, so resolving for them would be a wasted roster
            // read. It is still the same synchronous region, hence still the
            // same epoch.
            let senderKey = senderLeaf.flatMap(resolveSender)
            // ADR-0048 — sniff for a profile envelope before the chat parser,
            // for the same reason as the beacon arm above: the lossy-UTF-8
            // fallback would otherwise render it as a bubble reading
            // `lakko/profile/v1` plus binary. Placed AFTER the identity
            // resolution because a profile is bound to exactly that identity —
            // the signature key the sender leaf names in the epoch the message
            // was sealed at (C-68), never to anything the payload says.
            if let profile = Self.decodeProfileIfEnvelope(data) {
                return .profile(
                    messageId: messageId,
                    profile: profile,
                    senderKey: senderKey,
                    createdAt: createdAt
                )
            }
            // ADR-0058 — the five "who can find me" service envelopes
            // (introduction offer, introductions bundle, visibility setting,
            // reach state, committer delegation). Never a timeline row, and
            // after the identity resolution: each is bound to exactly the
            // sender the leaf names in this epoch (C-68).
            if let scoped = ScopedDiscoveryEnvelope.decode(data) {
                return .scopedDiscovery(
                    messageId: messageId,
                    envelope: scoped,
                    senderKey: senderKey,
                    createdAt: createdAt
                )
            }
            // ADR-0048 §2 trigger 4 — sniff for a profile REQUEST ("introduce
            // yourselves") before the chat parser, for the same reason as the
            // beacon arm: the lossy-UTF-8 fallback would otherwise render it as
            // a bubble reading `lakko/profile-request/v1` plus a control byte.
            // Field-less like the beacon, so the exact-length peek is a
            // complete test and the FFI decode behind it is the version gate.
            // Placed after the identity resolution so the receive side can
            // tell another person's request from this account's own echo
            // (another of our devices asking) without a second roster read.
            if Self.looksLikeProfileRequest(data) {
                return .profileRequest(
                    messageId: messageId,
                    senderKey: senderKey,
                    createdAt: createdAt
                )
            }
            // ADR-0023: feed the application bytes through the
            // structured ``parseBodyEnvelope(bytes:)`` surface,
            // which branches a plain chat message from an
            // edit / delete / reaction op. The plaintext was
            // already authenticated by the MLS Application
            // message's MAC, so the worst case is a malformed
            // envelope from a buggy peer — never a tampered
            // payload. Legacy pre-1.19 plain-UTF-8 bytes and
            // unknown / ambiguous op envelopes degrade to a
            // `.chat` body via the parser's bounded fail-closed
            // fallback. The `senderLeaf` (the original author's
            // MLS leaf index) rides every event so the receive
            // loop can enforce the author-only edit/delete gate
            // + attribute reactions.
            let parsed = AttachmentBridge.parseBodyEnvelopeBytes(data)
            switch parsed.kind {
            case .chat:
                // ADR-0052 — a reply is a chat body that also names a target;
                // the parser sets `replyTo` only on `.chat`, and the snapshot
                // fields ride beside it as the replier wrote them.
                return .application(
                    messageId: messageId,
                    text: parsed.text,
                    attachments: parsed.attachments,
                    senderKey: senderKey,
                    createdAt: createdAt,
                    reply: parsed.replyTo.map {
                        ReplyRef(
                            target: $0,
                            author: parsed.replyAuthor,
                            excerpt: parsed.replyExcerpt)
                    },
                    // ADR-0052 decision 2 — a forward is a chat body that also
                    // names its original author and, when the forwarder held
                    // it, the original message. Carried as data, never
                    // resolved here; the bubble draws the line from it.
                    forward: ForwardPayload.received(
                        author: parsed.forwardedFrom,
                        originGroupIdB64: parsed.forwardOriginGroupB64,
                        originMessageId: parsed.forwardOriginMessageId)
                )
            case .edit:
                return .op(
                    messageId: messageId,
                    op: .edit,
                    target: parsed.target ?? 0,
                    text: parsed.text,
                    emoji: nil,
                    senderKey: senderKey,
                    createdAt: createdAt
                )
            case .delete:
                return .op(
                    messageId: messageId,
                    op: .delete,
                    target: parsed.target ?? 0,
                    text: "",
                    emoji: nil,
                    senderKey: senderKey,
                    createdAt: createdAt
                )
            case .reaction:
                return .op(
                    messageId: messageId,
                    op: .reaction,
                    target: parsed.target ?? 0,
                    text: "",
                    emoji: parsed.emoji,
                    senderKey: senderKey,
                    createdAt: createdAt
                )
            case .typing:
                // ADR-0024 — an ephemeral typing signal. `target` is
                // unused; the bool rides `isTyping`. The receive loop
                // folds it into per-leaf ephemeral UI state (no bubble).
                return .op(
                    messageId: messageId,
                    op: .typing(isTyping: parsed.isTyping ?? false),
                    target: 0,
                    text: "",
                    emoji: nil,
                    senderKey: senderKey,
                    createdAt: createdAt
                )
            case .readReceipt:
                // ADR-0024 — a read receipt. `target` is the read-up-to
                // watermark (the highest messageId the sender has read).
                // The receive loop advances a monotone per-leaf watermark.
                return .op(
                    messageId: messageId,
                    op: .readReceipt,
                    target: parsed.target ?? 0,
                    text: "",
                    emoji: nil,
                    senderKey: senderKey,
                    createdAt: createdAt
                )
            }
        case .commit:
            return .system(
                messageId: messageId,
                systemKind: .commit,
                createdAt: createdAt
            )
        case .proposal(let record):
            // ADR-0045 — a membership proposal is a PANEL signal, never a
            // timeline row (UX §10). It is surfaced with its `ProposalRecord`
            // intact rather than flattened to a system bubble, because the
            // members sheet needs what was proposed AND whether the receipt gate
            // retained it, and the DS row's `createdAt` is what the quiet window
            // is measured against — a local clock would make that window mean
            // something different on every device.
            return .membershipProposal(
                messageId: messageId,
                record: record,
                createdAt: createdAt
            )
        case .externalJoinProposal(let joinerSignatureKey, let retained):
            // ADR-0027 (G-3) — surface the joiner's signature key the members
            // sheet needs to approve / reject the request, rather than a
            // content-less system row.
            //
            // `retained` is carried rather than dropped, and the distinction is
            // not cosmetic: the core fires this event even when the quarantine
            // (`MAX_PENDING_EXTERNAL_JOINS`) was full and the request was NOT
            // kept. A request that was not retained cannot be approved by
            // anyone — `approveExternalJoin` has nothing to look up and refuses
            // with `NoSuchJoinRequest` — so offering Approve for it is an
            // affordance that cannot work. The UI must render such a request as
            // informational at most.
            return .externalJoinRequest(
                messageId: messageId,
                joinerSignatureKey: joinerSignatureKey,
                retained: retained,
                createdAt: createdAt
            )
        }
    }

    /// Phase 3a8/9 — cheap byte-sniff for the
    /// `CallSignalingPayload` proto. Returns `true` when
    /// the payload opens with one of the oneof body tags:
    ///
    ///   * 0x52 / 0x5A / 0x62 / 0x6A — the 1x1 bodies, fields 10..13
    ///     (offer / answer / candidate / end), matched on the first byte alone
    ///     as they always have been;
    ///   * 0x72 / 0x7A — `GroupCallStart` (14) / `GroupCallJoin` (15);
    ///   * 0x82 0x01 — `GroupCallLeave` (16), whose tag
    ///     `(16 << 3) | 2 = 130` no longer fits one varint byte.
    ///
    /// The group tags were missing, so a group-call body fell through to
    /// `parseBodyEnvelope`'s lossy-UTF-8 fallback and rendered as a garbage
    /// chat bubble. They are matched on their FRAMING, not the first byte:
    /// `0x72` / `0x7A` are ASCII `r` / `z`, so a first-byte rule would swallow
    /// every legacy plain-text body that starts with one. See
    /// ``isFramedGroupCallBody(_:tagLength:)``.
    ///
    /// Chat envelopes start with `{` (0x7B). A false positive downstream
    /// surfaces as a proto-decode error and the caller swallows. Byte-for-byte
    /// the same rule as the NSE's `nseLooksLikeCallSignaling`.
    static func looksLikeCallSignaling(_ bytes: Data) -> Bool {
        guard let first = bytes.first else { return false }
        switch first {
        case 0x52, 0x5A, 0x62, 0x6A:
            return true
        case 0x72, 0x7A:
            return isFramedGroupCallBody(bytes, tagLength: 1)
        case 0x82:
            return bytes.count >= 2
                && bytes[bytes.startIndex + 1] == 0x01
                && isFramedGroupCallBody(bytes, tagLength: 2)
        default:
            return false
        }
    }

    /// `tag || varint(len) || body`, where the body is EXACTLY the rest of the
    /// payload and opens with the 16-byte `call_id` field every group-call
    /// message carries first (`0x0A 0x10`). That is the whole of a prost
    /// `CallSignalingPayload` holding one group body, and no plain text has
    /// that shape by accident.
    static func isFramedGroupCallBody(_ bytes: Data, tagLength: Int) -> Bool {
        var index = bytes.startIndex + tagLength
        var length = 0
        var shift = 0
        while true {
            // At most three varint bytes — far beyond any group body's size.
            guard index < bytes.endIndex, shift <= 14 else { return false }
            let byte = bytes[index]
            index += 1
            length |= Int(byte & 0x7F) << shift
            if byte & 0x80 == 0 { break }
            shift += 7
        }
        // 18 = the call_id field alone (tag + length + 16 bytes).
        guard length >= 18, bytes.endIndex - index == length else { return false }
        return bytes[index] == 0x0A && bytes[index + 1] == 0x10
    }

    /// First byte of the ADR-0045 committer-beacon domain separator (`'l'` of
    /// `"lakko/committer-beacon/v1"` + a trailing NUL). The cheap half of the
    /// pre-filter; the FFI `decodeCommitterBeacon` behind it is the
    /// authoritative fail-closed check. The same `'l'` every `lakko/…` domain
    /// starts with, so on its own it decides nothing — it only skips an FFI
    /// call for a chat envelope (which starts `{`).
    private static let committerBeaconFirstByte: UInt8 = 0x6C

    /// The beacon's exact wire length, read from the FFI rather than written
    /// as a local `27`. The constant exists in
    /// `crates/lakko-contacts/src/committer_beacon.rs` and is exported
    /// precisely so the hosts do not each carry a copy of a number nothing
    /// would pin; the foreground app links the full `LakkoCore`, so there is
    /// no reason to compromise here the way the slim NSE surface once had to.
    private static let committerBeaconLen = Int(committerBeaconEnvelopeLen())

    /// ADR-0045 — is this application plaintext a committer-liveness beacon?
    ///
    /// Unlike the other envelope filters in this file, the length-plus-prefix
    /// peek is a COMPLETE test rather than merely a fast path: the beacon
    /// carries no fields at all, so every beacon on the wire is byte-identical
    /// and the exact length plus the domain prefix already pin all but the
    /// version byte. The FFI decode behind it is confirmation, and it is what
    /// makes the arm fail-closed on an unknown schema version — a future v2
    /// beacon this build does not understand is NOT silently swallowed here;
    /// it falls through to the chat parser like any other unrecognised
    /// plaintext.
    /// ADR-0048 — the profile carried by this application plaintext, or `nil`
    /// when it is not a well-formed profile envelope.
    ///
    /// The first-byte peek only keeps an FFI call off the path for a chat
    /// envelope (which opens with `{`); `decodeProfileEnvelope` is the whole
    /// verdict — domain, version, and every field bound. An envelope that
    /// fails a bound is refused as a whole and falls through to the chat parser
    /// like any other unrecognised plaintext, the same stance the beacon arm
    /// takes for an unknown version and the rule the NotificationService filter
    /// applies, so the timeline and the notification agree.
    static func decodeProfileIfEnvelope(_ bytes: Data) -> ProfileClaim? {
        guard bytes.first == profileEnvelopeFirstByte,
              let decoded = try? decodeProfileEnvelope(plaintext: bytes)
        else { return nil }
        return ProfileClaim(
            displayName: decoded.displayName,
            username: decoded.username,
            updatedAtMs: decoded.updatedAtMs,
            // Envelope v2 — the sender's account id, or `nil` when it sent
            // none; what a group-call tile is matched on (ADR-0021).
            accountId: decoded.accountId,
            // Envelope v3 (ADR-0058) — the username certificate and the reach
            // grants of this conversation.
            usernameCertificate: decoded.usernameCertificate,
            grants: decoded.grants
        )
    }

    /// First byte of the ADR-0048 profile domain separator (`'l'` of
    /// `"lakko/profile/v1"`). Decides nothing on its own.
    private static let profileEnvelopeFirstByte: UInt8 = 0x6C

    /// ADR-0051 — the group name carried by this application plaintext, or
    /// `nil` when it is not a well-formed group-name envelope.
    ///
    /// The first-byte peek only keeps an FFI call off the path for a chat
    /// envelope (which opens with `{`); `decodeGroupNameEnvelope` is the whole
    /// verdict — domain, version, and every field bound. An empty decoded name
    /// is a valid envelope meaning "clear the name" and is returned as such.
    static func decodeGroupNameIfEnvelope(_ bytes: Data) -> FfiGroupName? {
        guard bytes.first == groupNameEnvelopeFirstByte,
              let decoded = try? decodeGroupNameEnvelope(plaintext: bytes)
        else { return nil }
        return decoded
    }

    /// First byte of the ADR-0051 group-name domain separator (`'l'` of
    /// `"lakko/group-name/v1"`). Decides nothing on its own.
    private static let groupNameEnvelopeFirstByte: UInt8 = 0x6C

    /// ADR-0052 decision 3 — the shared-pin operation carried by this
    /// application plaintext, or `nil` when it is not a well-formed pin
    /// envelope.
    ///
    /// The first-byte peek only keeps an FFI call off the path for a chat
    /// envelope (which opens with `{`); `decodePinEnvelope` is the whole
    /// verdict — domain, exact length, version and action. An unpin is a
    /// valid envelope (`pinned == false`) and is returned as such.
    static func decodePinIfEnvelope(_ bytes: Data) -> FfiPinOp? {
        guard bytes.first == pinEnvelopeFirstByte,
              let decoded = try? decodePinEnvelope(plaintext: bytes)
        else { return nil }
        return decoded
    }

    /// First byte of the ADR-0052 pin domain separator (`'l'` of
    /// `"lakko/pin/v1"`). Decides nothing on its own.
    private static let pinEnvelopeFirstByte: UInt8 = 0x6C

    static func looksLikeCommitterBeacon(_ bytes: Data) -> Bool {
        guard bytes.count == committerBeaconLen,
              bytes.first == committerBeaconFirstByte
        else { return false }
        // Not a beacon (wrong domain / unknown version) → `false` so the
        // caller runs the normal chat parse.
        guard (try? decodeCommitterBeacon(plaintext: bytes)) != nil else { return false }
        return true
    }

    /// First byte of the ADR-0048 profile-request domain separator (`'l'` of
    /// `"lakko/profile-request/v1"` + a trailing NUL). Decides nothing on its
    /// own — the same `'l'` every `lakko/…` domain starts with.
    private static let profileRequestFirstByte: UInt8 = 0x6C

    /// The request's exact wire length, read from the FFI
    /// (`profileRequestEnvelopeLen()`) rather than written as a local `26`,
    /// for the reason the beacon length is: a stale literal fails in the
    /// dangerous direction, silently letting the request through to the chat
    /// parser as a bubble.
    private static let profileRequestLen = Int(profileRequestEnvelopeLen())

    /// ADR-0048 §2 trigger 4 — is this application plaintext a profile request?
    ///
    /// The same shape as ``looksLikeCommitterBeacon(_:)``: the request carries
    /// no fields, so the exact length plus the domain prefix pin all but the
    /// version byte, and the FFI decode behind the peek is what keeps the arm
    /// fail-closed on an unknown schema version — a v2 request this build
    /// cannot read falls through to the chat parser like any other
    /// unrecognised plaintext rather than being swallowed here.
    static func looksLikeProfileRequest(_ bytes: Data) -> Bool {
        guard bytes.count == profileRequestLen,
              bytes.first == profileRequestFirstByte
        else { return false }
        guard (try? decodeProfileRequest(plaintext: bytes)) != nil else { return false }
        return true
    }
}

// MARK: - Result types

/// Result of ``ChatClient/sendMessage(identity:group:plaintext:)``.
public struct SendMessageResult: Equatable {
    /// DS-assigned monotonic id (also the cursor value for the
    /// next ``ChatClient/pollMessages(identity:group:after:waitSecs:)``
    /// call).
    public let messageId: Int64
    /// ISO-8601 timestamp from the DS.
    public let createdAt: String

    public init(messageId: Int64, createdAt: String) {
        self.messageId = messageId
        self.createdAt = createdAt
    }
}

/// Discriminated chat event surfaced by
/// ``ChatClient/pollMessages(identity:group:after:waitSecs:)``.
///
/// Phase 1.19c3: `.application` now carries
/// `(text, attachments)` from the 1.19a JSON envelope (parsed
/// via the UniFFI ``parseEnvelope(bytes:)`` surface from 1.19b).
/// Legacy pre-1.19 plain-UTF-8 bytes collapse cleanly to
/// `(text: <utf-8 lossy>, attachments: [])` via the parser's
/// documented fallback paths — existing bubbles render
/// unchanged.
public enum IncomingChatEvent: Equatable {
    /// Application-data message (user-visible chat row).
    ///
    /// ADR-0023: carries the original author's IDENTITY — their signature
    /// public key, base64, resolved from the MLS sender leaf against the roster
    /// of the epoch this message decrypted at. This is what the author-only
    /// edit / delete gate compares, and what the timeline persists.
    ///
    /// It is deliberately NOT the leaf. A leaf index is a position in the
    /// ratchet tree, not a person: OpenMLS hands a vacated leaf to the next
    /// member added, so a leaf recorded now can name somebody else after the
    /// next commit. Resolving at receipt — the one moment the roster is
    /// authoritative for the epoch the frame was sealed at — is what makes the
    /// value mean anything later. See ``AuthorIdentity``.
    ///
    /// `nil` for a frame whose sender the client could not resolve (a control
    /// message, an external sender, a torn roster read) — such a message can
    /// never be a valid edit / delete target.
    ///
    /// ADR-0052 — `reply` is set when the envelope carried a `reply_to`: the
    /// target id plus whatever snapshot of the quoted author and excerpt the
    /// replier sent. Carried as data, never resolved against the timeline
    /// here; the row draws the quote from it.
    ///
    /// ADR-0052 decision 2 — `forward` is set when the envelope carried a
    /// `forwarded_from` or an origin reference: the original author's name
    /// as the forwarder wrote it, and the original message's location when
    /// the forwarder sent one. Data, not a resolved reference.
    case application(
        messageId: Int64,
        text: String,
        attachments: [AttachmentRef],
        senderKey: MemberIdentity?,
        createdAt: String,
        reply: ReplyRef?,
        forward: ForwardRef?
    )
    /// ADR-0023 — an edit / delete / reaction op carried in the
    /// same JSON envelope shape as a chat message. The
    /// repository folds it into the *target* message (by
    /// `target` == the target's DS `messageId`) rather than
    /// rendering it as a standalone bubble. The op itself still
    /// consumes a DS `messageId` and surfaces here so the
    /// receive loop applies it exactly once (the poll is
    /// incremental).
    case op(
        messageId: Int64,
        op: OpKind,
        target: Int64,
        text: String,
        emoji: String?,
        /// The op sender's IDENTITY: their signature public key,
        /// base64, resolved from the MLS sender leaf against the
        /// roster of the epoch this op decrypted at. The
        /// author-only gate (edit / delete) checks it against the
        /// target's stored author identity, reactions attribute
        /// the toggle to it, and typing / read receipts are keyed
        /// on it — none of which may key on a leaf, which the next
        /// member added inherits. `nil` ⇒ the op is dropped
        /// (fail-closed).
        senderKey: MemberIdentity?,
        createdAt: String
    )
    /// Commit / proposal / external-join-proposal frame. MLS
    /// state advanced, no user-visible bytes.
    case system(messageId: Int64, systemKind: SystemKind, createdAt: String)
    /// Phase 3a8/9 — call-signaling event lifted out of an
    /// MLS application message. The chat repository
    /// dispatches it into the ``CallSession`` state machine
    /// instead of the chat timeline.
    ///
    /// `senderLeaf` is the MLS-authenticated sender of the row: a group-call
    /// body naming any other leaf is dropped by the record's router.
    /// `senderDeviceKey` (the leaf's own signature key) and `senderKey` (its
    /// ADR-0050 person key) are that leaf resolved AT RECEIPT (C-68) — the only
    /// identities the group-call record keys a participant on, so a member
    /// added into a vacated leaf never inherits a departed member's place in
    /// the call.
    case callSignaling(
        messageId: Int64, payloadBytes: Data, createdAt: String, senderLeaf: UInt32?,
        senderDeviceKey: MemberIdentity?, senderKey: MemberIdentity?)
    /// ADR-0027 (G-3) — an external-join *request*: a non-member posted an
    /// external Add proposal to this group's journal and the MLS core retained
    /// it pending an owner / admin approval. Carries the joiner's leaf
    /// signature key (the `proposalRef` handle the approve / reject repository
    /// methods take). Not a timeline bubble — the view model folds it into a
    /// pending list rendered in the members sheet (the retained set,
    /// `group.pendingExternalJoins()`, is the source of truth; this event is
    /// the nudge that a new request landed).
    case externalJoinRequest(messageId: Int64, joinerSignatureKey: Data, retained: Bool, createdAt: String)
    /// ADR-0058 — one of the five "who can find me" service envelopes. Never
    /// a timeline bubble. `senderKey` is the MLS sender resolved at receipt
    /// (C-68); a `nil` sender makes the receive side drop it.
    case scopedDiscovery(
        messageId: Int64,
        envelope: ScopedDiscoveryEnvelope,
        senderKey: MemberIdentity?,
        createdAt: String
    )
    /// ADR-0048 — a member's profile (display name + claimed username).
    /// Never a timeline bubble. `senderKey` is the MLS sender leaf resolved to
    /// a signature key at receipt (C-68); a `nil` sender means the profile has
    /// no authenticated identity to bind to and is dropped.
    case profile(
        messageId: Int64,
        profile: ProfileClaim,
        senderKey: MemberIdentity?,
        createdAt: String
    )
    /// ADR-0051 — the group's shared name (an empty `name` clears it). Never
    /// a timeline bubble: the receive loop runs the role gate against the LIVE
    /// group (owner / admin, or any member when there is no role table), then
    /// the strictly-newer order, and re-titles the chat on accept.
    ///
    /// `senderLeaf` is the MLS-authenticated author, resolved against the
    /// roster + the ADR-0022 role table at receipt; `nil` means unattributable,
    /// so only a byte-identical re-send is accepted.
    case groupName(
        messageId: Int64,
        name: String,
        updatedAtMs: UInt64,
        senderLeaf: UInt32?,
        createdAt: String
    )
    /// ADR-0052 decision 3 — a shared pin or unpin of ONE message in this
    /// conversation. Never a timeline bubble: the receive loop runs the
    /// standing check against the LIVE group (owner / admin, any member when
    /// there is no role table, either participant of a one-to-one), then the
    /// strictly-newer order FOR THAT MESSAGE, and stores the action on accept
    /// (an unpin is stored, never deleted).
    ///
    /// `senderLeaf` is the MLS-authenticated author, resolved against the
    /// roster + the ADR-0022 role table at receipt; `nil` means unattributable,
    /// so only a byte-identical re-send is accepted.
    case pin(
        messageId: Int64,
        op: FfiPinOp,
        senderLeaf: UInt32?,
        createdAt: String
    )
    /// ADR-0048 §2 trigger 4 — a member asked the group to introduce itself
    /// (a newcomer's pull for the names it could not read from earlier
    /// epochs). Never a timeline bubble: the receive loop answers by
    /// republishing this device's own profile through the throttled, jittered
    /// growth path, or stays silent when it holds no profile.
    ///
    /// The envelope carries NO fields (26 bytes, identical for every sender),
    /// for the reasons the beacon gives. `senderKey` is the asker's account
    /// key resolved from the MLS-authenticated leaf at receipt (C-68): the
    /// receive side uses it only to tell another person's request from this
    /// account's own echo, and answers no request it cannot attribute.
    case profileRequest(
        messageId: Int64,
        senderKey: MemberIdentity?,
        createdAt: String
    )
    /// ADR-0042 — an in-group push-routing-secret distribution envelope
    /// (the group OWNER broadcast the group's `push_routing_secret`). Never
    /// a timeline bubble: the receive loop persists the secret + subscribes
    /// under the derived opaque `push_gid` (invariant #5). Classified by
    /// its domain-separated envelope prefix via the FFI fail-closed codec.
    ///
    /// `senderLeaf` is the MLS leaf index that authored the envelope (the
    /// MLS-authenticated attribution from `ProcessedMessage`). The receive
    /// side resolves it against the live group roster + the ADR-0022 role
    /// table so only the OWNER can *change* the group's routing secret
    /// (`lakko-security` MEDIUM-1). `nil` ⇒ unattributable ⇒ only a
    /// byte-identical re-send is accepted.
    ///
    /// `mintEpoch` is the ADR-0044 recency stamp the envelope carried — the
    /// primary key of the canonical total order. It travels WITH the secret
    /// all the way to the gate: a stale re-broadcast carries a valid secret
    /// from an authorised sender and is refused only by that order.
    case pushRoutingSecret(
        messageId: Int64,
        secret: Data,
        mintEpoch: UInt64,
        senderLeaf: UInt32?,
        createdAt: String
    )
    /// ADR-0045 decision 2 — a committer-liveness beacon: an owner / admin
    /// announcing it is foregrounded and able to sweep the pending proposal
    /// set. Never a timeline bubble.
    ///
    /// The envelope carries NO fields at all (a fixed 27 bytes, identical on
    /// every call and for every sender), deliberately: naming the sender would
    /// add an unauthenticated identity claim next to the MLS-authenticated
    /// one, and a self-reported timestamp would let a candidate hold the
    /// committer duty by asserting its own freshness. So everything the
    /// takeover fold needs comes from the two associated values here — WHO,
    /// from the MLS-authenticated `senderLeaf`, and HOW OLD, from the DS row's
    /// `createdAt` — which is exactly why this is surfaced as an event rather
    /// than dropped inside the parser. The fold itself (`actingCommitter`) is
    /// not built yet; this case is the seam it will consume.
    ///
    /// `senderLeaf` is `nil` for a control frame / external sender, which the
    /// fold must treat as unusable (fail-closed — an unattributable beacon
    /// proves no candidate is live).
    case committerBeacon(
        messageId: Int64,
        senderLeaf: UInt32?,
        createdAt: String
    )

    /// ADR-0045 — a standalone membership proposal landed. **No state advance**
    /// — a proposal is not epoch-advancing, which is the whole point: two
    /// members proposing concurrently compose instead of forking the loser.
    ///
    /// Not a timeline row (UX §10): the members sheet reads the core's retained
    /// pending set for what is actually pending, and joins it to the DS row
    /// timestamp carried here so the quiet window means the same thing on every
    /// device. `record.stored == false` is the receipt gate's REFUSAL reported
    /// as data — no sweep will ever commit it, so it is not a pending request.
    case membershipProposal(
        messageId: Int64,
        record: ProposalRecord,
        createdAt: String
    )
}

/// ADR-0024 — an inbound ephemeral chat-status signal lifted out of
/// the poll fold. Typing + read-receipt ops render no bubble and never
/// touch the message timeline; they surface through the repository's
/// `chatStatusSink` so the view model can fold them into ephemeral
/// per-member typing state + a monotone per-member read watermark.
///
/// `sender` is a signature key on both cases, never a leaf: the watermark is
/// monotone and is never reset on a membership change, so a leaf key handed a
/// departed member's whole read history to whoever was added into their slot.
/// Typing is the same shape with a ~6 s lifetime and is keyed the same way so
/// there is one rule rather than two. See ``AuthorIdentity``.
public enum ChatStatusEvent: Equatable {
    /// `sender` is (or stopped) typing. `createdAt` is the DS row's ISO-8601
    /// timestamp — the view model drops a signal older than the expiry window
    /// (re-sync replay guard).
    case typing(sender: MemberIdentity, isTyping: Bool, createdAt: String)
    /// `sender` has read up to `upTo` (inclusive). The view model advances a
    /// monotone watermark (a lower value never overwrites a higher one —
    /// re-sync replay is idempotent).
    case readReceipt(sender: MemberIdentity, upTo: Int64)
}

/// ADR-0023 / ADR-0024 — which mutation or status signal an
/// ``IncomingChatEvent/op`` carries.
public enum OpKind: Equatable {
    case edit
    case delete
    case reaction
    /// ADR-0024 — an ephemeral typing signal. `true` ⇒ the sender is
    /// typing now; `false` ⇒ they stopped (blur / sent). Never renders
    /// a bubble — folds into per-leaf ephemeral UI state.
    case typing(isTyping: Bool)
    /// ADR-0024 — a read receipt. The op's `target` is the read-up-to
    /// watermark (the highest messageId the sender has read). Advances a
    /// monotone per-leaf watermark; never renders a bubble.
    case readReceipt
}

/// Kind discriminator for ``IncomingChatEvent/system(messageId:systemKind:createdAt:)``.
public enum SystemKind: Equatable {
    case commit
    case externalJoinProposal
}

/// Result of ``ChatClient/pollMessages(identity:group:after:waitSecs:)``.
public struct PollMessagesResult: Equatable {
    public let events: [IncomingChatEvent]
    /// Fresh cursor — pass back as
    /// ``ChatClient/pollMessages(identity:group:after:waitSecs:)``'s
    /// `after` argument.
    public let cursor: Int64
    /// `true` if the DS reported more rows past the last returned
    /// id; caller should poll again immediately with the new
    /// cursor.
    public let hasMore: Bool
    /// `true` when at least one row in this batch was HANDED to the MLS core.
    ///
    /// Deliberately NOT "at least one row applied successfully". A throw is no
    /// proof that nothing durable changed: `process_incoming_verifying_roles`
    /// runs `process_message` — which decrypts and ratchets — BEFORE the
    /// ADR-0022 role gate can refuse a commit or the ADR-0045 receipt gate can
    /// drop a proposal. Rows the echo filter dropped never reach the core, so
    /// they do not set it.
    ///
    /// The caller must not advance the journal cursor past such a batch until
    /// the provider snapshot has durably landed — see `Chat/DurableCursor.swift`.
    public let touchedMlsState: Bool

    public init(
        events: [IncomingChatEvent],
        cursor: Int64,
        hasMore: Bool,
        touchedMlsState: Bool = false
    ) {
        self.events = events
        self.cursor = cursor
        self.hasMore = hasMore
        self.touchedMlsState = touchedMlsState
    }
}

// MARK: - Errors

public enum ChatError: Error {
    /// Programmer error — surfaced before any network call.
    case argument(String)
    /// Wraps any underlying failure (DS, MLS process, FFI). The
    /// `underlying` carries the original error for diagnostics.
    case wrapped(String, underlying: Error)
    /// ADR-0045 — a direct membership commit was asked for while this group
    /// still holds pending proposals, so the core would refuse it.
    ///
    /// This is a USER-FACING state, not a programmer error: under ADR-0045 a
    /// non-empty pending set is the ordinary condition between any member's
    /// request and the next committer sweep. It has its own case rather than
    /// riding ``argument(_:)`` for two reasons — a test can assert on it by
    /// identity instead of by prose, and the core's own refusal string (which
    /// names `commit_pending_proposals` / `stage_pending_proposals`, two Rust
    /// entry points no user can call) can never reach the screen through it.
    ///
    /// The copy follows `docs/ux/proposal-based-membership.md` §5.1: it states
    /// inaction, names no person, and implies no deadline — ADR-0045 decision 2
    /// deliberately gives a pending request none.
    case membershipChangesPending
}

extension ChatError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .argument(let message):
            return message
        case .wrapped(let message, let underlying):
            // ADR-0053 — a refused proof with the device clock off says so in
            // words a person can act on, not as a transport dump.
            if let copy = DsClientError.clockSkewCopy(in: underlying) { return copy }
            return "\(message): \(underlying)"
        case .membershipChangesPending:
            return ChatError.membershipChangesPendingCopy
        }
    }

    /// The one string for ``ChatError/membershipChangesPending``, exposed so a
    /// test can pin it without re-typing it (a second copy is how the two
    /// platforms would drift apart).
    public static let membershipChangesPendingCopy =
        "Other membership changes are waiting to apply, so this request cannot be approved yet. "
        + "They are applied by the group owner or an admin, and this will work once they have."
}
