// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Runs the automatic backup: watches for changes worth keeping, and flushes
/// them on a slow tick.
///
/// Mirror of the web `backup/auto-backup-driver.ts`, down to where the
/// membership check lives. It is asked HERE, at the tick, rather than hooked
/// onto the snapshot write: that path runs only after an MLS mutation, so a
/// device that had already joined everything and merely opened the app never
/// marked itself dirty - precisely the person whose backup is most stale.
/// Asking at the tick also makes the first run after install a change, because
/// nothing on this device can vouch for what the server already holds.
///
/// The address-book half needs no watcher: the contact list is read at upload
/// time, and a change to it is caught by the same tick through
/// ``AutoBackupStore/markDirty(accountIdB64:nowMs:defaults:)`` from the accept
/// path.
@MainActor
public final class AutoBackupDriver {

    /// Everything a run needs from the session, read at each tick because a
    /// session token is rebound and a snapshot moves.
    public struct Inputs {
        public let accountId: @MainActor () -> Data?
        public let accountIdB64: @MainActor () -> String?
        public let sessionToken: @MainActor () -> String?
        /// The canonical AccountIdentity seed (`Identity.fromSeed`).
        public let accountIdentitySeed: @MainActor () -> Data?
        /// The groups this device knows about, or `nil` before the chat layer
        /// has loaded them - which is not the same as "no groups" and must not
        /// be read as a departure from every one of them.
        public let knownGroupIds: @MainActor () -> [String]?
        /// The whole MLS storage snapshot, or `nil` when no identity is loaded.
        public let snapshot: @MainActor () -> Data?
        /// ADR-0058 — the synced state's bytes, or `nil` when none is held.
        public let syncedState: @MainActor () -> Data?

        public init(
            accountId: @escaping @MainActor () -> Data?,
            accountIdB64: @escaping @MainActor () -> String?,
            sessionToken: @escaping @MainActor () -> String?,
            accountIdentitySeed: @escaping @MainActor () -> Data?,
            knownGroupIds: @escaping @MainActor () -> [String]?,
            snapshot: @escaping @MainActor () -> Data?,
            syncedState: @escaping @MainActor () -> Data? = { nil }
        ) {
            self.accountId = accountId
            self.accountIdB64 = accountIdB64
            self.sessionToken = sessionToken
            self.accountIdentitySeed = accountIdentitySeed
            self.knownGroupIds = knownGroupIds
            self.snapshot = snapshot
            self.syncedState = syncedState
        }
    }

    public enum Outcome: String, Equatable {
        case clean
        case throttled
        case notReady
        case uploaded
        case failed
    }

    private let inputs: Inputs
    private var loop: Task<Void, Never>?

    public init(inputs: Inputs) {
        self.inputs = inputs
    }

    /// Start ticking. Idempotent - a second call is a no-op, so a view that
    /// re-appears does not stack loops.
    public func start() {
        guard loop == nil else { return }
        loop = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                _ = await self?.runOnce()
                try? await Task.sleep(nanoseconds: AutoBackup.tickSeconds * 1_000_000_000)
            }
        }
    }

    public func stop() {
        loop?.cancel()
        loop = nil
    }

    /// One pass. Exposed so a test can drive it without a clock.
    @discardableResult
    public func runOnce(nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)) async -> Outcome {
        guard let accountIdB64 = inputs.accountIdB64() else { return .notReady }

        if let groups = inputs.knownGroupIds(),
           AutoBackupStore.knownGroupsChanged(accountIdB64: accountIdB64, groupIds: groups) {
            AutoBackupStore.markDirty(accountIdB64: accountIdB64, nowMs: nowMs)
        }

        let state = AutoBackupStore.load(accountIdB64: accountIdB64)
        switch AutoBackup.verdict(state, nowMs: nowMs) {
        case .clean: return .clean
        case .throttled: return .throttled
        case .due: break
        }

        guard let accountId = inputs.accountId(),
              let token = inputs.sessionToken(), !token.isEmpty,
              let seed = inputs.accountIdentitySeed(),
              let snapshot = inputs.snapshot()
        else {
            // Leaves the dirty flag alone, so the backup goes out at the next
            // tick once the session is whole; clearing it here would silently
            // drop the change that made it dirty.
            return .notReady
        }

        let identity: Identity
        do {
            identity = try Identity.fromSeed(seed: seed)
        } catch {
            NSLog("auto backup: identity derive failed: \(error)")
            return .notReady
        }

        // Read at upload time rather than watched: the list is small, the tick
        // is slow, and a snapshot taken now is by definition the one being
        // uploaded.
        let contacts = AcceptedContactsStore.load(accountIdB64: accountIdB64).map { c in
            BackupContactRecord(
                ed25519Pubkey: c.request.fromPubkey,
                x25519Pubkey: Data(count: 32),
                displayName: c.displayName ?? "",
                accountId: c.request.fromAccountId
            )
        }

        let client = BackupClient()
        client.setSessionToken(token)
        let result = await client.uploadFromCache(
            accountId: accountId,
            accountIdB64: accountIdB64,
            ed25519Seed: identity.ed25519Seed(),
            contacts: contacts,
            wholeStorageSnapshot: snapshot,
            // The index the snapshot cannot supply. Read from the same
            // accessor the membership check above uses, so an upload always
            // carries the set it was marked dirty for.
            knownGroupIdsHex: inputs.knownGroupIds() ?? [],
            syncedState: inputs.syncedState()
        )
        guard case .uploaded = result else {
            // `cacheMiss` is the ordinary state for a device that joined by
            // pairing rather than by signup or recover: it holds the account
            // but never saw the phrase, so it cannot seal an envelope. Not an
            // error, and not worth a line on every tick.
            if case .cacheMiss = result {
                return .failed
            }
            NSLog("auto backup: upload returned \(result)")
            return .failed
        }
        AutoBackupStore.save(
            accountIdB64: accountIdB64,
            state: AutoBackup.markedUploaded(state, nowMs: nowMs)
        )
        return .uploaded
    }
}
