// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// State + dispatcher for the per-thread ``ChatScreen``.
///
/// Mirror of:
///
///   * `apps/web/src/chat/ChatRoute.tsx` (1.17f4b)
///   * `apps/android/.../ui/chat/ChatViewModel.kt` (1.15f4b)
///
/// Combines the ``ChatRepository``-side thread (looked up by
/// `groupIdHex`) with VM-local composer state (the current draft
/// input + the send-busy / send-error flags) into a single set of
/// `@Published` slots SwiftUI observes.
///
/// Continuous foreground polling lives on the screen via
/// `.task { while !Task.isCancelled { … } }`, same pattern as
/// 1.14e5's contacts inbox loop.
@MainActor
public final class ChatViewModel: ObservableObject {

    // MARK: - Constructor inputs

    public let groupIdHex: String

    // MARK: - Form fields

    @Published public var draft: String = ""

    /// Phase 1.19c3 — attachments staged in the composer. The
    /// user picks photo / file; each call to
    /// ``onAttachmentPicked(bytes:mime:name:)`` encrypts +
    /// uploads to storage immediately so the ref is ready by
    /// the time Send is tapped. Cleared on successful Send, on
    /// per-chip remove, and on screen dismount.
    @Published public private(set) var pendingAttachments: [AttachmentRef] = []

    // MARK: - Ephemeral status

    @Published public private(set) var sending: Bool = false
    @Published public private(set) var sendError: String?
    @Published public private(set) var refreshing: Bool = false
    /// `true` while one or more picked items are being
    /// encrypted + uploaded. The composer disables Send and the
    /// picker buttons until it flips back.
    @Published public private(set) var uploading: Bool = false

    /// Polish 1.x — per-pick upload-progress detail. `nil` when
    /// no upload is in flight; otherwise carries the file name
    /// + which stage of the encrypt / upload pipeline the
    /// in-flight pick is at, so the composer tray can render a
    /// specific "Uploading hello.png…" caption instead of a
    /// generic "Uploading…". The user picks one file at a time
    /// (Apple's photo picker UX is single-image-per-pick by
    /// default in the ChatScreen), so no file-index ornament —
    /// the index is always 1.
    @Published public private(set) var uploadStage: UploadStage?

    /// Stage of a single pick's encrypt+upload pipeline. Mirror
    /// of the web client's `uploadProgress.stage` discriminator.
    public enum UploadStage: Equatable {
        case encrypting(fileName: String)
        case uploading(fileName: String)
    }

    // MARK: - Mirror of the repository's per-thread state

    /// The messages list for this group's thread. Reactive — every
    /// repository `state` update lands here via Combine
    /// subscription.
    @Published public private(set) var messages: [ChatMessage] = []
    /// `false` when the repository has no thread for this groupIdHex
    /// — e.g. immediately after process restart before snapshot
    /// rehydration has rebuilt the per-group handles. The UI still
    /// renders the composer (sendMessage surfaces a clear error if
    /// the handle is missing).
    @Published public private(set) var threadKnown: Bool = false

    // MARK: - Group membership (A4)

    /// Live MLS roster for this group at the current epoch — refreshed by
    /// ``refreshMembers()`` and after every add / remove.
    @Published public private(set) var members: [GroupMember] = []
    /// `true` while the members sheet is presented.
    @Published public var membersPanelOpen: Bool = false
    /// `@username` typed into the add-member field.
    @Published public var addMemberInput: String = ""
    /// `true` while an add or remove commit is in flight (disables the form).
    @Published public private(set) var membershipBusy: Bool = false
    /// Last add/remove error surfaced to the sheet, else `nil`.
    @Published public private(set) var membershipError: String?
    /// ADR-0022 — `true` when the local member may add / remove (owner, admin,
    /// or an open/legacy group with no role table); `false` for a plain member.
    /// The sheet hides the add field + per-member Remove when this is `false`.
    @Published public private(set) var canManageMembers: Bool = true
    /// ADR-0022 — `true` when the local member is the owner (only the owner sees
    /// the per-member promote / demote toggle).
    @Published public private(set) var isOwner: Bool = false
    /// The group's current role table (owner + admins), or `nil` for an open
    /// group — drives the per-member admin labelling.
    @Published public private(set) var roleTable: RoleTable?

    // MARK: - Wiring

    private let repository: ChatRepository
    /// Supplies the current auth session token for minting the SFU
    /// call-admission JWT. Defaults to `{ nil }` (group calls then
    /// refuse, fail-closed); the composition root passes
    /// `{ authRepository.identitySecrets?.sessionToken }`.
    private let sessionTokenProvider: () -> String?
    /// Resolves `@username -> (accountId, signaturePubkey)` through the
    /// G-4-verified contacts path for the add-member flow. Defaults to a
    /// fail-closed stub (add-member then refuses); the composition root
    /// passes `{ try await contactsClient.resolveVerifiedRecipient(...) }`.
    private let resolveRecipient: (String) async throws -> (accountId: Data, signaturePubkey: Data)
    private var cancellables: Set<AnyCancellable> = []
    /// Phase 2a3e2b-iOS — cross-device sync. Attached via
    /// [`attachSync`] from the screen so the existing init
    /// signature (1.14f2) stays binary-compatible. `nil` until
    /// the screen lifecycle wires it in.
    private weak var deviceGroupSync: DeviceGroupSyncRepository?
    /// Raw group_id bytes for the active thread — cached so
    /// the broadcast / seed paths don't re-fetch on every
    /// keystroke.
    private var groupIdBytes: Data?
    /// Last text the broadcast path published. Used to dedup
    /// echoes that loop back through the poll loop.
    private var lastBroadcastDraft: String?
    /// Debounce timer for outgoing Draft payloads.
    private var draftBroadcastWork: DispatchWorkItem?
    private var didSeedDraft: Bool = false

    public init(
        repository: ChatRepository,
        groupIdHex: String,
        sessionTokenProvider: @escaping () -> String? = { nil },
        resolveRecipient: @escaping (String) async throws
            -> (accountId: Data, signaturePubkey: Data) = { _ in
                throw ChatError.argument(
                    "no @username resolver wired — add-member unavailable"
                )
            }
    ) {
        self.repository = repository
        self.groupIdHex = groupIdHex
        self.sessionTokenProvider = sessionTokenProvider
        self.resolveRecipient = resolveRecipient
        sync(state: repository.state)
        repository.$state
            .receive(on: RunLoop.main)
            .sink { [weak self] newState in
                self?.sync(state: newState)
            }
            .store(in: &cancellables)
        // Phase 3a9 — attach a call-signaling sink to the
        // repository. The closure runs on the main actor
        // because the repository's poll loop already
        // dispatches results on @MainActor.
        repository.callSignalingSink = { [weak self] hex, payloadBytes in
            guard hex == groupIdHex else { return }
            Task { @MainActor in
                self?.handleIncomingCallSignaling(payloadBytes: payloadBytes)
            }
        }
    }

    // MARK: - Phase 3a9 — call surface helpers

    /// Tap-to-call from the chat header. Mints a fresh
    /// `call_id` and builds the session (deriving + arming the
    /// SFrame key) but does NOT dial yet — the session sits in
    /// `.idle`, which `driveFrom` maps to the prejoin phase.
    /// The user picks mic / camera on the prejoin screen and the
    /// Join button calls `CallSession.dial` (G3-1-prejoin-join).
    public func startCall(audio: Bool = true, video: Bool = false) {
        guard activeCall == nil else { return }
        _ = (audio, video)
        let callId = CallSession.mintCallId()
        let session = makeSession(callId: callId)
        activeCall = session
    }

    /// Accept the in-flight inbound call.
    public func acceptIncomingCall() {
        activeCall?.acceptCall()
    }

    /// Decline the in-flight inbound call.
    public func declineIncomingCall() {
        activeCall?.declineCall()
    }

    /// Hang up the active call.
    public func hangUpCall() {
        activeCall?.hangUp(reason: .hungUp)
    }

    /// Clear the overlay after a `done` transition. Called
    /// from the SwiftUI overlay's dismiss action.
    public func clearFinishedCall() {
        if activeCall?.state == .done {
            activeCall = nil
        }
    }

    private func makeSession(callId: Data) -> CallSession {
        // Capture the repository + groupIdHex for the
        // outbound publish callback. We can't depend on
        // ContextsRepository here (the chat client is
        // owned by `ChatRepository`); the publish goes
        // straight through the repository's `publishGroup`
        // path via a thin helper.
        let session = CallSession(callId: callId) { [weak self] bytes in
            guard let self = self else { return }
            Task { @MainActor in
                await self.publishCallSignaling(bytes)
            }
        }
        // G3-1-call-key — derive the per-call SFrame seed from
        // the chat group's MLS exporter and arm the session
        // BEFORE it dials / accepts. Both directions go through
        // `makeSession`, so this single call covers the outgoing
        // (`startCall`) and the incoming (`handleIncomingCallSignaling`)
        // paths. Without the seed, `attachSFrame` refuses the
        // call per security invariant #4 (no non-E2EE fallback);
        // a failed derivation leaves the seed unset, which is the
        // correct fail-closed outcome (the call simply refuses).
        if let key = repository.deriveCallKey(groupIdHex: groupIdHex, callId: callId) {
            session.setCallKey(seed: key.seed, localLeaf: key.localLeaf)
        } else {
            NSLog(
                "makeSession: call-key derivation failed — " +
                "call will refuse per security invariant #4"
            )
        }
        return session
    }

    @MainActor
    private func publishCallSignaling(_ bytes: Data) async {
        do {
            try await repository.publishApplicationMessage(
                groupIdHex: groupIdHex,
                plaintextBytes: bytes
            )
        } catch {
            NSLog("publishCallSignaling failed: %@", error.localizedDescription)
        }
    }

    private func handleIncomingCallSignaling(payloadBytes: Data) {
        do {
            let parsed = try parseCallSignaling(plaintext: payloadBytes)
            let bodyCallId = CallSession.callId(of: parsed)
            // Spin up an answerer session for a first-seen
            // inbound offer; otherwise route into the active
            // session if call_ids match.
            if activeCall == nil {
                if case .offer = parsed {
                    let session = makeSession(callId: bodyCallId)
                    activeCall = session
                    session.applyIncomingSignaling(parsed)
                }
                return
            }
            guard let session = activeCall, session.callId == bodyCallId else {
                // Stale signaling for a different call_id —
                // drop silently.
                return
            }
            session.applyIncomingSignaling(parsed)
        } catch {
            NSLog("parseCallSignaling failed: %@", error.localizedDescription)
        }
    }

    // MARK: - Phase 3a9 — call surface

    /// Active 1×1 call for this thread. `nil` when no call.
    /// Published so the SwiftUI overlay re-renders on every
    /// state transition.
    @Published public private(set) var activeCall: CallSession?

    /// Active multi-party SFU group call for this thread (`nil` when
    /// none). A group chat (>2 members) routes through this; a 1×1 chat
    /// stays on `activeCall`. Published so the UI presents the call
    /// overlay driven by `sfuCallUiState`.
    @Published private(set) var activeSfuCall: SfuCallUi?

    /// The `CallUiState` the active SFU call drives (roster / phase /
    /// E2EE). The SwiftUI overlay observes this while `activeSfuCall`
    /// is non-nil. Reused across calls. `internal` — `SfuCallUi` /
    /// `CallUiState` are app-module types consumed by `ChatScreen`.
    let sfuCallUiState = CallUiState()

    /// `true` when this chat has >2 MLS members — the call button then
    /// routes through the SFU group-call path; a 2-member chat stays on
    /// the 1×1 P2P `CallSession`. `nil` member count (no handle) reads
    /// as a 1×1.
    var isGroupChat: Bool {
        (repository.memberCount(groupIdHex: groupIdHex) ?? 0) > 2
    }

    /// Start a multi-party SFU group call for this (group) chat. Mints
    /// the call-admission JWT, derives the per-call SFrame seed + leaf
    /// from the MLS exporter, and connects through `SfuCallUi`. Fails
    /// closed (NSLog + no call) on any missing prerequisite — SFU URL,
    /// session token, or call-key derivation.
    func startGroupCall(video: Bool = false) {
        guard activeSfuCall == nil, activeCall == nil else { return }
        guard let base = SfuConfig.baseUrl else {
            NSLog("startGroupCall: SFU URL not configured (LakkoSfuUrl) — refusing")
            return
        }
        guard let token = sessionTokenProvider() else {
            NSLog("startGroupCall: no session token — re-authenticate to start a group call")
            return
        }
        let callIdBytes = CallSession.mintCallId()
        let callIdHex = callIdBytes.map { String(format: "%02x", $0) }.joined()
        guard let key = repository.deriveCallKey(groupIdHex: groupIdHex, callId: callIdBytes) else {
            NSLog("startGroupCall: call-key derivation failed — refusing per invariant #4")
            return
        }
        let jwt = CallJwtClient()
        jwt.setSessionToken(token)
        Task { @MainActor in
            do {
                let minted = try await jwt.mint(callId: callIdHex, role: .participant)
                let config = SfuCallSession.Config(
                    sfuBaseUrl: base,
                    jwt: minted.jwt,
                    seed: key.seed,
                    localLeaf: key.localLeaf,
                    audio: true,
                    video: video
                )
                // ADR-0021 — precompute the per-call peer_tag -> contact
                // label map once, so the SFU roster resolves to the
                // identifier the rest of the app shows for that contact.
                // The tag is re-derived locally (the SFU never sends a
                // name); a tag collision falls back to the pseudonym.
                var tagToLabel: [String: String] = [:]
                var collidedTags = Set<String>()
                for c in AcceptedContactsStore.load() {
                    guard let accountId = c.request.fromAccountId else { continue }
                    let accountIdHex = accountId
                        .map { String(format: "%02x", $0) }
                        .joined()
                    let tag = sfuPeerTag(callId: callIdHex, accountId: accountIdHex)
                    if tagToLabel[tag] != nil {
                        collidedTags.insert(tag)
                    } else {
                        // Prefer the user-chosen display name; fall back to
                        // the short pubkey fingerprint (ADR-0021 follow-up).
                        tagToLabel[tag] = c.displayLabel
                    }
                }
                let resolveDisplayName: (String, String, String) -> String = {
                    _, pseudonym, peerTag in
                    if !peerTag.isEmpty, !collidedTags.contains(peerTag),
                        let label = tagToLabel[peerTag] {
                        return label
                    }
                    return pseudonym.isEmpty ? "Peer" : String(pseudonym.prefix(8))
                }
                let handle = SfuCallUi(
                    config: config,
                    ui: sfuCallUiState,
                    resolveDisplayName: resolveDisplayName
                )
                activeSfuCall = handle
                try await handle.connect()
            } catch {
                NSLog("startGroupCall failed: %@", error.localizedDescription)
                activeSfuCall?.hangUp()
                activeSfuCall = nil
            }
        }
    }

    /// Hang up the active SFU group call + clear it.
    func hangUpGroupCall() {
        activeSfuCall?.hangUp()
        activeSfuCall = nil
    }

    // MARK: - Group membership actions (A4)

    /// Open / close the members sheet. Refreshes the roster on open.
    func toggleMembersPanel() {
        membersPanelOpen.toggle()
        if membersPanelOpen {
            membershipError = nil
            refreshMembers()
        }
    }

    /// Re-read the live MLS roster + the local member's role from the repository.
    func refreshMembers() {
        members = repository.members(groupIdHex: groupIdHex) ?? []
        let role = repository.ownRole(groupIdHex: groupIdHex)
        canManageMembers = role != .member
        isOwner = role == .owner
        roleTable = repository.groupRoles(groupIdHex: groupIdHex)
    }

    /// `true` when `member` is an admin in the current role table.
    func memberIsAdmin(_ member: GroupMember) -> Bool {
        roleTable?.admins.contains(member.signaturePublicKey) ?? false
    }

    /// `true` when `member` is the group owner (the owner row never shows a
    /// promote / demote toggle).
    func memberIsOwner(_ member: GroupMember) -> Bool {
        roleTable?.owner == member.signaturePublicKey
    }

    /// Our own leaf index — the roster marks it "(you)" and blocks self-removal.
    var ownLeafIndex: UInt32? {
        repository.ownLeafIndex(groupIdHex: groupIdHex)
    }

    /// Human label for a roster member: a matched contact's chosen name (or its
    /// fingerprint), else a short fingerprint of the signature key. "(you)" is
    /// appended for the local leaf. Mirrors the ADR-0021 resolution the call
    /// roster uses, keyed on the member's signature public key.
    func memberLabel(_ member: GroupMember) -> String {
        let isYou = ownLeafIndex == member.leafIndex
        let suffix = isYou ? " (you)" : ""
        for c in AcceptedContactsStore.load()
        where c.request.fromPubkey == member.signaturePublicKey {
            return c.displayLabel + suffix
        }
        let fp = member.signaturePublicKey.prefix(4)
            .map { String(format: "%02x", $0) }
            .joined()
        return "Member " + fp + suffix
    }

    /// Resolve the typed `@username` (G-4-verified) and add it to this group:
    /// publish the add commit to the journal (own-marked) + ferry the Welcome.
    /// Fail-closed — a resolve / verify / publish failure surfaces an error and
    /// leaves the group untouched.
    func onAddMember() {
        let raw = addMemberInput.trimmingCharacters(in: .whitespacesAndNewlines)
        let username = raw.hasPrefix("@") ? String(raw.dropFirst()) : raw
        guard !username.isEmpty else { return }
        guard !membershipBusy else { return }
        membershipBusy = true
        membershipError = nil
        Task { @MainActor in
            defer { membershipBusy = false }
            do {
                let resolved = try await resolveRecipient(username)
                _ = try await repository.addMemberToGroup(
                    groupIdHex: groupIdHex,
                    recipientAccountId: resolved.accountId,
                    recipientSignaturePubkey: resolved.signaturePubkey
                )
                addMemberInput = ""
                refreshMembers()
            } catch {
                membershipError = error.localizedDescription
                NSLog("onAddMember failed: %@", error.localizedDescription)
            }
        }
    }

    /// Remove a member by leaf index: publish the removal commit to the journal
    /// (own-marked). Refuses to remove the local leaf. Fail-closed on error.
    func onRemoveMember(leafIndex: UInt32) {
        guard ownLeafIndex != leafIndex else {
            membershipError = "You cannot remove yourself from the group."
            return
        }
        guard !membershipBusy else { return }
        membershipBusy = true
        membershipError = nil
        Task { @MainActor in
            defer { membershipBusy = false }
            do {
                _ = try await repository.removeMemberFromGroup(
                    groupIdHex: groupIdHex,
                    leafIndex: leafIndex
                )
                refreshMembers()
            } catch {
                membershipError = error.localizedDescription
                NSLog("onRemoveMember failed: %@", error.localizedDescription)
            }
        }
    }

    /// Promote / demote a member as admin (owner only — the toggle is shown
    /// only when `isOwner`). Publishes the `setGroupRoles` commit. Fail-closed.
    func onToggleAdmin(member: GroupMember, makeAdmin: Bool) {
        guard !membershipBusy else { return }
        membershipBusy = true
        membershipError = nil
        Task { @MainActor in
            defer { membershipBusy = false }
            do {
                _ = try await repository.setMemberAdmin(
                    groupIdHex: groupIdHex,
                    memberSignaturePubkey: member.signaturePublicKey,
                    makeAdmin: makeAdmin
                )
                refreshMembers()
            } catch {
                membershipError = error.localizedDescription
                NSLog("onToggleAdmin failed: %@", error.localizedDescription)
            }
        }
    }

    /// Phase 2a3e2c-iOS — read-cursor broadcast tracker.
    /// Highest broadcast messageId; the per-`messages` effect
    /// schedules a fresh `ReadCursor` payload only when the
    /// newest visible row's id exceeds this value.
    private var lastBroadcastReadId: Int64 = -1
    private var readCursorWork: DispatchWorkItem?

    /// Bind to the process-scoped
    /// ``DeviceGroupSyncRepository`` (Phase 2a3e2b). Called
    /// from `ChatScreen.onAppear`. Subscribes to:
    ///
    ///   * incoming `Draft` payloads keyed on this thread —
    ///     seed `draft` once on first mount when local is
    ///     empty (avoids stomping mid-typing on this device).
    ///   * local `draft` changes — debounce 500 ms then
    ///     broadcast.
    ///   * local `messages` mutations — debounce 1.5 s then
    ///     broadcast a `ReadCursor` payload pointing at the
    ///     highest message-id (Phase 2a3e2c).
    public func attachSync(_ sync: DeviceGroupSyncRepository) {
        deviceGroupSync = sync
        // Resolve groupIdBytes lazily — the ChatRepository
        // exposes the per-thread record's groupId; we hex-
        // decode the `groupIdHex` we already carry.
        groupIdBytes = Self.hexToBytes(groupIdHex)
        guard let groupIdBytes = groupIdBytes else { return }

        // Observe the sync repository's drafts map. On every
        // change, seed locally IF the local composer is
        // empty + we haven't seeded yet.
        let key = groupIdHex
        sync.$drafts
            .receive(on: RunLoop.main)
            .sink { [weak self] drafts in
                guard let self = self else { return }
                guard !self.didSeedDraft else { return }
                guard let incoming = drafts[key] else { return }
                if self.draft.isEmpty {
                    self.draft = incoming.text
                }
                self.didSeedDraft = true
            }
            .store(in: &cancellables)

        // Observe own `draft` changes + broadcast debounced.
        $draft
            .receive(on: RunLoop.main)
            .sink { [weak self] text in
                self?.scheduleDraftBroadcast(text: text, groupIdBytes: groupIdBytes)
            }
            .store(in: &cancellables)

        // Phase 2a3e2c-iOS — read-cursor broadcast. Every time
        // the timeline grows past the previously-broadcast
        // value, mint a fresh `ReadCursor` payload pointing at
        // the newest visible message.
        $messages
            .receive(on: RunLoop.main)
            .sink { [weak self] msgs in
                self?.scheduleReadCursorBroadcast(
                    messages: msgs,
                    groupIdBytes: groupIdBytes
                )
            }
            .store(in: &cancellables)
    }

    private func scheduleReadCursorBroadcast(
        messages: [ChatMessage],
        groupIdBytes: Data
    ) {
        guard !messages.isEmpty else { return }
        // Find the highest server-assigned messageId. Outgoing
        // rows with messageId == 0 (pre-ack) get skipped — the
        // ack path will trigger a fresh effect run.
        var highest: Int64 = -1
        for m in messages where m.messageId > 0 {
            if m.messageId > highest { highest = m.messageId }
        }
        if highest <= lastBroadcastReadId { return }
        let target = highest
        readCursorWork?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self = self,
                  let sync = self.deviceGroupSync else { return }
            self.lastBroadcastReadId = target
            // 8 big-endian bytes — wire shape stable across
            // platforms.
            var idBytes = Data(count: 8)
            var v = UInt64(bitPattern: target)
            for i in (0..<8).reversed() {
                idBytes[i] = UInt8(v & 0xff)
                v >>= 8
            }
            let now = UInt64(Date().timeIntervalSince1970 * 1000)
            Task { @MainActor in
                do {
                    try await sync.setReadCursor(
                        groupId: groupIdBytes,
                        upToMessageId: idBytes,
                        readAtMs: now
                    )
                } catch {
                    NSLog("setReadCursor failed: %@", error.localizedDescription)
                }
            }
        }
        readCursorWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5, execute: work)
    }

    private func scheduleDraftBroadcast(text: String, groupIdBytes: Data) {
        if text == lastBroadcastDraft { return }
        draftBroadcastWork?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self = self,
                  let sync = self.deviceGroupSync else { return }
            self.lastBroadcastDraft = text
            Task { @MainActor in
                do {
                    try await sync.setDraft(
                        groupId: groupIdBytes,
                        text: text
                    )
                } catch {
                    NSLog("setDraft failed: %@", error.localizedDescription)
                }
            }
        }
        draftBroadcastWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5, execute: work)
    }

    private static func hexToBytes(_ hex: String) -> Data? {
        guard hex.count % 2 == 0 else { return nil }
        var bytes = Data(capacity: hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let b = UInt8(hex[index..<next], radix: 16) else { return nil }
            bytes.append(b)
            index = next
        }
        return bytes
    }

    private func sync(state: ChatRepositoryState) {
        let thread = state.threads.first { $0.groupIdHex == groupIdHex }
        self.messages = thread?.messages ?? []
        self.threadKnown = thread != nil
    }

    // MARK: - Actions

    public func onSendClicked() {
        guard !sending else { return }
        let trimmed = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        let stagedAttachments = pendingAttachments
        // 1.19c3: attachment-only messages are valid (empty
        // text + ≥1 attachment); pre-1.19 text-only messages
        // still valid (≥1 char text + zero attachments). The
        // only invalid input is the all-empty composer.
        guard !trimmed.isEmpty || !stagedAttachments.isEmpty else { return }

        sending = true
        sendError = nil
        Task {
            do {
                try await repository.sendMessage(
                    groupIdHex: groupIdHex,
                    plaintext: trimmed,
                    attachments: stagedAttachments
                )
                sending = false
                sendError = nil
                draft = ""
                pendingAttachments = []
            } catch {
                sending = false
                // 1.19c3: leave `pendingAttachments` populated
                // on send failure so the user can retry without
                // re-picking the same files.
                sendError = (error as? LocalizedError)?.errorDescription
                    ?? "Send failed."
            }
        }
    }

    /// Drive the encrypt + upload flow for one picked item.
    /// Called by the screen layer after a photo / document
    /// picker resolves to raw bytes + MIME + filename.
    /// Sequential — one picked item at a time, called from the
    /// screen's `Task { … }` per pick — keeps the upload-state
    /// flag honest.
    public func onAttachmentPicked(
        bytes: Data,
        mime: String?,
        name: String?
    ) async {
        uploading = true
        sendError = nil
        // Drive `uploadStage` immediately so the composer tray
        // doesn't show the generic spinner for the brief encrypt
        // window. `AttachmentBridge.encryptAndUpload` flips the
        // stage to `.uploading` once it reaches the PUT path.
        let displayName = (name?.isEmpty ?? true) ? "attachment" : name!
        uploadStage = .encrypting(fileName: displayName)
        defer {
            uploading = false
            uploadStage = nil
        }
        do {
            let ref = try await AttachmentBridge.encryptAndUpload(
                bytes: bytes,
                mime: mime,
                name: name,
                onStage: { [weak self] stage in
                    // Hop back to MainActor for the published
                    // mutation — `onStage` may run on whatever
                    // thread the URLSession completion lands on.
                    Task { @MainActor in
                        switch stage {
                        case .encrypting:
                            self?.uploadStage = .encrypting(fileName: displayName)
                        case .uploading:
                            self?.uploadStage = .uploading(fileName: displayName)
                        }
                    }
                }
            )
            pendingAttachments.append(ref)
        } catch let err as AttachmentBridgeError {
            sendError = err.errorDescription
        } catch {
            sendError = "Attachment failed: \(error)"
        }
    }

    /// Drop a staged attachment from the pending tray. Index
    /// matches the order in ``pendingAttachments``.
    public func onRemovePendingAttachment(at index: Int) {
        guard pendingAttachments.indices.contains(index) else { return }
        pendingAttachments.remove(at: index)
    }

    public func onRefreshClicked() {
        refreshing = true
        Task {
            defer { refreshing = false }
            do {
                try await repository.pollGroup(groupIdHex: groupIdHex)
            } catch {
                // best-effort
            }
        }
    }

    /// One long-poll tick — used by ``ChatScreen``'s
    /// `.task { … }` modifier in a `while !Task.isCancelled` loop.
    /// Silent (does NOT flip `refreshing`) so the spinner doesn't
    /// stay on for the full 30s long-poll window.
    public func pollOnceLongPoll(waitSecs: Int = ChatViewModel.defaultLongPollSecs) async {
        do {
            try await repository.pollGroup(
                groupIdHex: groupIdHex,
                waitSecs: waitSecs
            )
        } catch {
            // best-effort
        }
    }

    /// DS long-poll budget — same value as
    /// ``ContactsViewModel/defaultLongPollSecs``.
    public static let defaultLongPollSecs: Int = 30
}
