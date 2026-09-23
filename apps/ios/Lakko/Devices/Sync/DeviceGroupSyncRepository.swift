// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// Cross-device sync runtime on iOS (Phase 2a3e2b).
///
/// Mirror of `apps/web/src/devices/sync/DeviceGroupSyncContext.tsx`
/// translated into a `@MainActor` `ObservableObject`. Owns the
/// per-session DeviceGroup handle + the four reactive state
/// maps (drafts / flags / readCursors / order) that the UI
/// observes via `@Published`, plus a background poll loop that
/// drains the DeviceGroup DS journal and a set of typed send
/// paths for the outbound side.
///
/// Lifecycle:
///
///   * [`bootstrap`] (idempotent for the same `accountId`) —
///     loads the DeviceGroup handle from the existing snapshot
///     or bootstraps a founder leaf, kicks off the poll loop.
///   * [`clear`] — cancels the poll loop, drops the handle,
///     clears every reactive map. Called from
///     ``AuthRepository/logout`` /
///     ``AuthRepository/forgetDevice``.
///
/// The poll loop runs in a single `Task` cancelled on `clear`;
/// the four send paths await on the main actor.
@MainActor
public final class DeviceGroupSyncRepository: ObservableObject {

    // MARK: - Public reactive state

    /// `true` once the per-session DeviceGroup handle is loaded
    /// and the poll loop has started.
    @Published public private(set) var ready: Bool = false

    /// Per-group draft (composer text) snapshot. Key is the
    /// `groupIdHex(groupId)` value the receiver-side helper
    /// produces.
    @Published public private(set) var drafts: [String: DraftState] = [:] {
        didSet { scheduleSyncStatePersist() }
    }

    /// Per-group UI flags snapshot (pin / mute / archive /
    /// unread-override).
    @Published public private(set) var flags: [String: FlagsState] = [:] {
        didSet {
            scheduleSyncStatePersist()
            mirrorMutedConversations()
        }
    }

    /// Per-group read cursor snapshot.
    @Published public private(set) var readCursors: [String: ReadCursorState] =
        [:] {
        didSet { scheduleSyncStatePersist() }
    }

    /// Whole-account explicit conversation order. `nil` when no
    /// `Order` payload has arrived yet.
    @Published public private(set) var order: OrderState? {
        didSet { scheduleSyncStatePersist() }
    }

    // MARK: - Internals

    private var identity: Identity?
    private var group: DeviceGroup?
    private var pollTask: Task<Void, Never>?
    /// The pending debounced snapshot write, cancelled by the next change.
    private var persistTask: Task<Void, Never>?
    private var activeAccountIdB64: String?
    /// Where the four maps live between launches. Its own folder under
    /// Application Support, sealed with the same Keychain wrap key the
    /// DeviceGroup snapshot uses - isolation there comes from the path.
    private let syncStateStore = DeviceGroupSnapshotStore(
        directoryName: "lakko-device-sync-state")

    public init() {}

    // MARK: - Lifecycle

    /// Bootstrap the cross-device sync runtime for the freshly-
    /// authenticated session. Idempotent for the same account.
    /// Called from `AppRoot` whenever `identitySecrets`
    /// transitions to non-nil.
    public func bootstrap(secrets: IdentitySecrets) {
        let accountIdB64 = base64Encode(secrets.accountId)
        if activeAccountIdB64 == accountIdB64 { return }
        if activeAccountIdB64 != nil {
            cancelPollAndDropHandles()
        }
        activeAccountIdB64 = accountIdB64
        let seed: Data
        do {
            seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(
                accountIdB64: accountIdB64
            )
        } catch {
            NSLog("DeviceGroupSync bootstrap: seed failure %@", error.localizedDescription)
            activeAccountIdB64 = nil
            return
        }
        let ensured: DeviceGroupLifecycle.Ensured
        do {
            ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
                accountIdBytes: secrets.accountId,
                accountIdB64: accountIdB64,
                deviceIdentitySeed: seed
            )
        } catch {
            NSLog("DeviceGroupSync bootstrap: ensureDeviceGroup %@", error.localizedDescription)
            activeAccountIdB64 = nil
            return
        }
        identity = ensured.identity
        group = ensured.group

        // Seed the four maps from this device's own disk BEFORE the poll
        // starts. They used to begin empty on every launch, and the journal
        // cannot refill them: the poll runs from cursor 0, but the MLS
        // snapshot was restored above, so every row it re-fetches is one this
        // state has already consumed. The snapshot is a head start, not an
        // authority - the poll folds newer rows on top through the same HLC
        // comparison, so a sibling device's later write still wins.
        if let blob = syncStateStore.getSnapshot(accountIdB64: accountIdB64),
           let restored = DeviceGroupSyncStateCodec.decode(blob) {
            drafts = restored.drafts
            flags = restored.flags
            readCursors = restored.readCursors
            order = restored.order
        }
        ready = true

        let groupId: Data
        do {
            groupId = try ensured.group.groupId()
        } catch {
            NSLog("DeviceGroupSync bootstrap: groupId %@", error.localizedDescription)
            return
        }
        pollTask = Task { [weak self] in
            await self?.runPollLoop(
                accountIdB64: accountIdB64,
                groupId: groupId
            )
        }
    }

    /// Tear down the runtime — cancel poll loop, drop FFI
    /// handles, clear every reactive map.
    public func clear() {
        cancelPollAndDropHandles()
        // Forget the snapshot too. Logout and forget-this-device both come
        // through here, and a draft left behind for the next account to sign
        // in on this device is the one piece of this state that reads as a
        // message.
        if let closing = activeAccountIdB64 {
            syncStateStore.deleteSnapshot(accountIdB64: closing)
        }
        activeAccountIdB64 = nil
        ready = false
        drafts = [:]
        flags = [:]
        readCursors = [:]
        order = nil
    }

    /// Publish the muted set into the App Group container
    /// (``ConversationMuteMirror``), so the three call paths that cannot reach
    /// this repository still know not to ring.
    ///
    /// UNLIKE the snapshot write below, this is NOT debounced. The debounce
    /// buys an AES seal and a file replace; a `UserDefaults` write costs
    /// neither, and half a second is long enough for the call a person just
    /// muted to ring anyway. Whole-set rather than per-row, because an unmute
    /// on a sibling device has to REMOVE a row.
    private func mirrorMutedConversations() {
        ConversationMuteMirror.replaceAll(
            mutedGroupIdsHex: Set(flags.filter { $0.value.muted }.keys))
    }

    /// Write the four maps to disk, shortly.
    ///
    /// Debounced because a draft moves on a keystroke and each write is an
    /// AES seal plus a file replace. Restoring the maps at bootstrap trips
    /// this too, writing back what was just read; harmless, and cheaper than
    /// a flag that has to be cleared correctly on every path out of
    /// ``bootstrap(secrets:)``.
    private func scheduleSyncStatePersist() {
        persistTask?.cancel()
        persistTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 500_000_000)
            guard !Task.isCancelled else { return }
            self?.persistSyncState()
        }
    }

    /// Write the four maps to disk.
    ///
    /// Called after anything that moves them - a local set or a sibling's row
    /// folded by the poll. Best-effort: the in-memory state stays good and
    /// the next change rewrites the envelope.
    private func persistSyncState() {
        guard let accountIdB64 = activeAccountIdB64 else { return }
        let snapshot = DeviceGroupSyncStateCodec.Snapshot(
            drafts: drafts, flags: flags, readCursors: readCursors, order: order)
        guard let blob = DeviceGroupSyncStateCodec.encode(snapshot) else { return }
        syncStateStore.putSnapshot(accountIdB64: accountIdB64, blob: blob)
    }

    private func cancelPollAndDropHandles() {
        pollTask?.cancel()
        pollTask = nil
        persistTask?.cancel()
        persistTask = nil
        identity = nil
        group = nil
    }

    // MARK: - Lookup helpers

    public func draft(for groupId: Data) -> DraftState? {
        drafts[groupIdHex(groupId)]
    }

    public func flags(for groupId: Data) -> FlagsState? {
        flags[groupIdHex(groupId)]
    }

    public func readCursor(for groupId: Data) -> ReadCursorState? {
        readCursors[groupIdHex(groupId)]
    }

    // MARK: - Outbound send paths

    /// Compose + publish a `Draft` payload. Empty `text` clears
    /// the receivers' draft for the same group.
    public func setDraft(
        groupId: Data,
        text: String,
        replyToId: Data = Data()
    ) async throws {
        let hlc = Hlc.mint()
        let body = DeviceGroupBody.draft(
            groupId: groupId,
            text: text,
            replyToId: replyToId
        )
        try await sendBody(body, hlc: hlc)
        // Optimistic local update — same HLC the wire echo
        // carries, so LWW dedups.
        let key = groupIdHex(groupId)
        drafts[key] = DraftState(
            text: text,
            replyToId: replyToId,
            updatedAtHlc: hlc
        )
    }

    /// Compose + publish a `Flags` payload. Whole-snapshot
    /// last-write-wins.
    public func setFlags(
        groupId: Data,
        pinned: Bool,
        muted: Bool,
        archived: Bool,
        unreadOverride: Bool
    ) async throws {
        let hlc = Hlc.mint()
        let body = DeviceGroupBody.flags(
            groupId: groupId,
            pinned: pinned,
            muted: muted,
            archived: archived,
            unreadOverride: unreadOverride
        )
        try await sendBody(body, hlc: hlc)
        let key = groupIdHex(groupId)
        flags[key] = FlagsState(
            pinned: pinned,
            muted: muted,
            archived: archived,
            unreadOverride: unreadOverride,
            updatedAtHlc: hlc
        )
    }

    /// Compose + publish a `ReadCursor` payload.
    public func setReadCursor(
        groupId: Data,
        upToMessageId: Data,
        readAtMs: UInt64
    ) async throws {
        let hlc = Hlc.mint()
        let body = DeviceGroupBody.readCursor(
            groupId: groupId,
            upToMessageId: upToMessageId,
            readAtMs: readAtMs
        )
        try await sendBody(body, hlc: hlc)
        let key = groupIdHex(groupId)
        readCursors[key] = ReadCursorState(
            upToMessageId: upToMessageId,
            readAtMs: readAtMs,
            updatedAtHlc: hlc
        )
    }

    /// Compose + publish an `Order` payload.
    public func setOrder(groupIds: [Data]) async throws {
        let hlc = Hlc.mint()
        let body = DeviceGroupBody.order(groupIds: groupIds)
        try await sendBody(body, hlc: hlc)
        order = OrderState(groupIds: groupIds, updatedAtHlc: hlc)
    }

    private func sendBody(_ body: DeviceGroupBody, hlc: UInt64) async throws {
        guard let group = group, let identity = identity,
              let accountIdB64 = activeAccountIdB64 else {
            throw NSError(
                domain: "DeviceGroupSync",
                code: 1,
                userInfo: [NSLocalizedDescriptionKey: "Sync not ready"]
            )
        }
        let wireBytes = try group.createDevicegroupMessage(
            identity: identity,
            body: body,
            hlc: hlc
        )
        let groupId = try group.groupId()
        let ds = DsClient()
        // G-13 (ADR-0015): wrap in the journal envelope (application messages
        // carry no attestation).
        let envelope = DeviceGroupJournalEnvelope.encode(
            message: wireBytes,
            attestation: nil
        )
        _ = try await ds.publishGroup(groupId: groupId, payload: envelope)
        DeviceGroupLifecycle.persistSnapshot(
            identity: identity,
            accountIdB64: accountIdB64
        )
    }

    // MARK: - Inbound poll loop

    private func runPollLoop(accountIdB64: String, groupId: Data) async {
        let ds = DsClient()
        var cursor: Int64 = 0
        while !Task.isCancelled {
            let result: FetchResult
            do {
                result = try await ds.fetchGroup(
                    groupId: groupId,
                    after: cursor,
                    limit: nil,
                    waitSecs: 25
                )
            } catch DsClientError.http(let status, _, _) where status == 404 {
                continue
            } catch let err as DsClientError {
                if case .network = err {
                    try? await Task.sleep(nanoseconds: 2_000_000_000)
                    continue
                }
                NSLog("DeviceGroupSync poll fetch failed %@", String(describing: err))
                try? await Task.sleep(nanoseconds: 5_000_000_000)
                continue
            } catch {
                NSLog("DeviceGroupSync poll fetch unexpected %@", error.localizedDescription)
                try? await Task.sleep(nanoseconds: 5_000_000_000)
                continue
            }

            // G-13 (ADR-0015): the 32-byte verifying key peers feed to
            // processIncomingVerifyingRemovals. A zero key is a fail-closed
            // fallback when the device-attest endpoint is unreachable — it
            // parses but verifies nothing, so a removal the policy requires to
            // be attested is refused rather than applied unverified; while the
            // policy is off, entries apply normally regardless.
            var attestVk: Data
            do {
                attestVk = try await DeviceAttestKey.fetch()
            } catch {
                NSLog("DeviceGroupSync device-attest key fetch failed: %@", error.localizedDescription)
                attestVk = Data(count: 32)
            }

            var mutated = false
            for message in result.messages {
                cursor = message.id
                guard let id = identity, let g = group else { return }
                // Unwrap the journal envelope (MLS message + any attestation).
                let entry: (message: Data, attestation: Data?)
                do {
                    entry = try DeviceGroupJournalEnvelope.decode(message.payload)
                } catch {
                    NSLog("DeviceGroupSync envelope decode failed: %@", error.localizedDescription)
                    continue
                }
                let attestations: [Data] = entry.attestation.map { [$0] } ?? []
                let processed: ProcessedMessage
                do {
                    processed = try g.processIncomingVerifyingRemovals(
                        identity: id,
                        messageBytes: entry.message,
                        attestations: attestations,
                        attestVerifyingKey: attestVk,
                        nowUnixSecs: Int64(Date().timeIntervalSince1970)
                    )
                } catch {
                    NSLog("DeviceGroupSync processIncomingVerifyingRemovals failed: %@", error.localizedDescription)
                    continue
                }
                mutated = true
                switch processed {
                case .application(let data, _, _):
                    // ADR-0023 added the sender leaf to the
                    // Application case; the DeviceGroup sync path
                    // doesn't authorise on it (removals are gated
                    // upstream by the attestation verify), so it's
                    // ignored here.
                    do {
                        let parsed = try parseDevicegroupPayload(plaintext: data)
                        applyParsed(parsed)
                    } catch {
                        NSLog("DeviceGroupSync parseDevicegroupPayload failed: %@", error.localizedDescription)
                    }
                case .commit, .proposal, .externalJoinProposal:
                    // No application-state mutation.
                    break
                }
            }

            if mutated {
                if let id = identity {
                    DeviceGroupLifecycle.persistSnapshot(
                        identity: id,
                        accountIdB64: accountIdB64
                    )
                }
            }
        }
    }

    private func applyParsed(_ parsed: ParsedDeviceGroupPayload) {
        let hlc = parsed.hlc
        switch parsed.body {
        case .readCursor(let groupId, let upToMessageId, let readAtMs):
            let key = groupIdHex(groupId)
            if shouldApply(candidateHlc: hlc, existingHlc: readCursors[key]?.updatedAtHlc) {
                readCursors[key] = ReadCursorState(
                    upToMessageId: upToMessageId,
                    readAtMs: readAtMs,
                    updatedAtHlc: hlc
                )
            }
        case .draft(let groupId, let text, let replyToId):
            let key = groupIdHex(groupId)
            if shouldApply(candidateHlc: hlc, existingHlc: drafts[key]?.updatedAtHlc) {
                drafts[key] = DraftState(
                    text: text,
                    replyToId: replyToId,
                    updatedAtHlc: hlc
                )
            }
        case .flags(let groupId, let pinned, let muted, let archived, let unreadOverride):
            let key = groupIdHex(groupId)
            if shouldApply(candidateHlc: hlc, existingHlc: flags[key]?.updatedAtHlc) {
                flags[key] = FlagsState(
                    pinned: pinned,
                    muted: muted,
                    archived: archived,
                    unreadOverride: unreadOverride,
                    updatedAtHlc: hlc
                )
            }
        case .order(let groupIds):
            if shouldApply(candidateHlc: hlc, existingHlc: order?.updatedAtHlc) {
                order = OrderState(groupIds: groupIds, updatedAtHlc: hlc)
            }
        }
    }
}
