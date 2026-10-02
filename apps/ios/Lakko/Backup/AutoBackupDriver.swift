// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Runs the automatic backup: watches for changes worth keeping, and flushes
/// them on a slow tick.
///
/// Mirror of the web `backup/auto-backup-driver.ts`, down to where the
/// membership check lives. It is asked HERE, at the tick, rather than hooked
/// onto the write of the device's local MLS snapshot: that path runs only
/// after an MLS mutation, so a device that had already joined everything and
/// merely opened the app never marked itself dirty - precisely the person
/// whose backup is most stale.
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
    /// session token is rebound and the group set moves.
    public struct Inputs {
        public let accountId: @MainActor () -> Data?
        public let accountIdB64: @MainActor () -> String?
        public let sessionToken: @MainActor () -> String?
        /// The canonical AccountIdentity seed (`Identity.fromSeed`).
        public let accountIdentitySeed: @MainActor () -> Data?
        /// The groups this device holds, or `nil` before the chat layer has
        /// loaded them - which is not the same as "no groups" and must not be
        /// read as a departure from every one of them. No upload goes out
        /// while it is `nil`.
        public let knownGroupIds: @MainActor () -> [String]?
        /// ADR-0058 — the synced state's bytes, or `nil` when none is held.
        public let syncedState: @MainActor () -> Data?

        public init(
            accountId: @escaping @MainActor () -> Data?,
            accountIdB64: @escaping @MainActor () -> String?,
            sessionToken: @escaping @MainActor () -> String?,
            accountIdentitySeed: @escaping @MainActor () -> Data?,
            knownGroupIds: @escaping @MainActor () -> [String]?,
            syncedState: @escaping @MainActor () -> Data? = { nil }
        ) {
            self.accountId = accountId
            self.accountIdB64 = accountIdB64
            self.sessionToken = sessionToken
            self.accountIdentitySeed = accountIdentitySeed
            self.knownGroupIds = knownGroupIds
            self.syncedState = syncedState
        }
    }

    /// One backup the driver sends. There is no MLS snapshot in it: the
    /// account backup carries none (see ``BackupClient``).
    public struct Upload {
        public let accountId: Data
        public let accountIdB64: String
        public let sessionToken: String
        public let ed25519Seed: Data
        public let contacts: [BackupContactRecord]
        /// The groups this device holds plus the ones a restored backup named
        /// that it has not joined (``RestoredKnownGroupsStore``).
        public let knownGroupIdsHex: [String]
        public let syncedState: Data?
    }

    /// Sends one ``Upload``. Injectable so a test can read what would go out.
    public typealias Uploader = @MainActor (Upload) async -> UploadResult

    public enum Outcome: String, Equatable {
        case clean
        case throttled
        case notReady
        case uploaded
        case failed
    }

    private let inputs: Inputs
    private let uploader: Uploader
    private let defaults: UserDefaults
    private var loop: Task<Void, Never>?

    /// - Parameter uploader: `nil` is ``liveUpload``.
    public init(
        inputs: Inputs,
        uploader: Uploader? = nil,
        defaults: UserDefaults = .standard
    ) {
        self.inputs = inputs
        self.uploader = uploader ?? Self.liveUpload
        self.defaults = defaults
    }

    /// The production upload: seal under the cached recovery phrase and PUT.
    public static let liveUpload: Uploader = { upload in
        let client = BackupClient()
        client.setSessionToken(upload.sessionToken)
        return await client.uploadFromCache(
            accountId: upload.accountId,
            accountIdB64: upload.accountIdB64,
            ed25519Seed: upload.ed25519Seed,
            contacts: upload.contacts,
            knownGroupIdsHex: upload.knownGroupIdsHex,
            syncedState: upload.syncedState
        )
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
           AutoBackupStore.knownGroupsChanged(
               accountIdB64: accountIdB64, groupIds: groups, defaults: defaults) {
            AutoBackupStore.markDirty(accountIdB64: accountIdB64, nowMs: nowMs, defaults: defaults)
        }

        let state = AutoBackupStore.load(accountIdB64: accountIdB64, defaults: defaults)
        switch AutoBackup.verdict(state, nowMs: nowMs) {
        case .clean: return .clean
        case .throttled: return .throttled
        case .due: break
        }

        // A phrase restore's backup step is still owed
        // (``BackupRestoreRetryStore``). Uploading now would replace the blob
        // the next attempt is going to fetch. The dirty flag stays, so the
        // backup goes out once the restore has reached a definitive answer.
        if BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64, defaults: defaults) {
            return .notReady
        }

        guard let accountId = inputs.accountId(),
              let token = inputs.sessionToken(), !token.isEmpty,
              let seed = inputs.accountIdentitySeed(),
              let ownGroups = inputs.knownGroupIds()
        else {
            // Leaves the dirty flag alone, so the backup goes out at the next
            // tick once the session is whole and the chat layer has loaded;
            // clearing it here would silently drop the change that made it
            // dirty.
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
        // is slow, and what is read now is by definition what is uploaded.
        let contacts = AcceptedContactsStore.load(accountIdB64: accountIdB64).map { c in
            BackupContactRecord(
                ed25519Pubkey: c.request.fromPubkey,
                x25519Pubkey: Data(count: 32),
                displayName: c.displayName ?? "",
                accountId: c.request.fromAccountId
            )
        }

        let result = await uploader(Upload(
            accountId: accountId,
            accountIdB64: accountIdB64,
            sessionToken: token,
            ed25519Seed: identity.ed25519Seed(),
            contacts: contacts,
            // The conversations of the account: the groups this device holds
            // (the set the membership check above was run on) and the ones a
            // restored backup named that it has not joined yet, so an upload
            // from a freshly restored device does not erase the list.
            knownGroupIdsHex: RestoredKnownGroupsStore.uploadGroupIds(
                accountIdB64: accountIdB64, own: ownGroups, defaults: defaults),
            syncedState: inputs.syncedState()
        ))
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
            state: AutoBackup.markedUploaded(state, nowMs: nowMs),
            defaults: defaults
        )
        return .uploaded
    }
}
