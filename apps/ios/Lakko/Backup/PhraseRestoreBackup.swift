// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Where a restore gets the account's backup from: fetch, the ADR-0037
/// rollback gate, open, parse. One method so the routes that restore an
/// identity from the phrase can be driven by a fake in tests.
protocol BackupRestoreSource {
    func restore(
        sessionToken: String,
        phraseBytes: Data,
        gate: RestoreGateContext
    ) async -> RestoreOutcome
}

/// The production source: ``BackupClient/restore(phraseBytes:gate:)`` over a
/// short-lived client bound to the session being landed. The client comes
/// from the repository's one backup-client seam
/// (``AuthRepository/makeBackupClient``), which the restore, the first backup
/// and the re-seal after a rotation share.
struct LiveBackupRestoreSource: BackupRestoreSource {
    func restore(
        sessionToken: String,
        phraseBytes: Data,
        gate: RestoreGateContext
    ) async -> RestoreOutcome {
        let client = await MainActor.run { AuthRepository.makeBackupClient() }
        client.setSessionToken(sessionToken)
        return await client.restore(phraseBytes: phraseBytes, gate: gate)
    }
}

/// What a restored backup put on this device.
struct AppliedBackup: Equatable {
    /// Contacts that were not on the device before.
    var contactsAdded: Int = 0
    /// Whether the backup carried an ADR-0058 synced state this build reads,
    /// which was folded into the store.
    var syncedStateMerged: Bool = false
    /// How many conversations the backup named.
    var knownGroups: Int = 0
    /// How many own predecessor seeds this device holds after the backup, `0`
    /// when the backup carried none or the slot could not be written.
    var ownPredecessors: Int = 0
}

/// What of the person's own state a restored backup put on this device.
struct AppliedOwnState: Equatable {
    /// The name and photo went to the own profile store.
    var ownProfile = false
    /// The "who can find me" setting was taken.
    var visibility = false
}

/// How a phrase restore's backup step ended. Never an error to the caller:
/// no outcome fails the sign-in.
enum PhraseRestoreBackupOutcome: Equatable {
    /// The backup was opened and applied.
    case applied(AppliedBackup)
    /// The account has no backup (404). Not an error.
    case noBackup
    /// The served blob was refused (rolled back / tampered, a stale head the
    /// user declined, or it did not open under the phrase). Nothing applied.
    case refused
    /// The fetch failed. Nothing applied, and the restore stays owed
    /// (``BackupRestoreRetryStore``).
    case fetchFailed
}

/// Applying an account backup to this device.
///
/// A backup carries the account's contacts, the ADR-0058 synced state, the
/// list of its conversations and its own predecessor seeds (ADR-0074 section
/// 3.7). It carries NO MLS state, and this takes no
/// `Identity`: a restored device's MLS state is whatever it already held
/// locally for the account (possibly nothing), and it is admitted to its
/// chats by their other members (ADR-0050 section 2.5a).
enum PhraseRestoreBackup {

    /// Merge a parsed backup into the per-account stores of `accountIdB64`,
    /// named explicitly because the session may not be installed yet.
    ///
    ///   * Contacts: only the ones not already present (by signature key), so
    ///     a contact accepted after the backup is never dropped or replaced.
    ///   * Synced state: `merge_from` through ``SyncedStateStore``; every
    ///     register keeps the newer stamp.
    ///   * Conversations: recorded in ``RestoredKnownGroupsStore``. Never in
    ///     ``KnownGroupsStore``, which lists the groups this device holds.
    ///   * Own predecessors, FIRST: stored where a kept device holds them,
    ///     before anything reads the slot. A device restored from the phrase
    ///     holds the newest key only, and every conversation leaf of the
    ///     account is bound to the oldest (ADR-0074 section 3.7): without
    ///     them its binding names a key no conversation knows.
    ///
    /// A live repository bound to the account is told through the same two
    /// notifications the pairing bundle and the DeviceGroup restore use. A
    /// restore that runs before the chat layer bootstraps needs neither: the
    /// first binding is minted from the slot. One that runs after it (a
    /// retried restore) re-mints as the bundle path does
    /// (``ChatRepository/rebindAfterStateBundle(accountIdB64:)``).
    @discardableResult
    static func apply(
        _ parsed: ParsedBackupPayload,
        accountIdB64: String,
        context: String,
        defaults: UserDefaults = .standard,
        ownPredecessors: OwnPredecessorStoring = KeychainOwnPredecessorStore()
    ) -> AppliedBackup {
        var applied = AppliedBackup()

        if !parsed.ownPredecessors.isEmpty {
            do {
                applied.ownPredecessors = try StateBundle.storeOwnPredecessors(
                    parsed.ownPredecessors.map {
                        StateBundle.OwnPredecessor(
                            seedB64: $0.seed.base64EncodedString(),
                            adoptedAtMs: Int64(clamping: $0.adoptedAtMs))
                    },
                    accountIdB64: accountIdB64,
                    store: ownPredecessors,
                    adoptions: PredecessorMailbox.AdoptionStore(defaults: defaults))
            } catch {
                NSLog("backup restore (\(context)): the account's own predecessors were not stored")
            }
        }

        // The backup does not carry the inbox metadata, so the reconstructed
        // request holds sentinels (ADR-0021): a negative messageId, a zero
        // pseudonym, no intro.
        let existing = AcceptedContactsStore.load(accountIdB64: accountIdB64)
        let restored = reconstructRestoredContacts(
            existing: existing,
            restored: parsed.acceptedContacts,
            createdAtMs: parsed.createdAtMs
        )
        if !restored.isEmpty {
            AcceptedContactsStore.save(existing + restored, accountIdB64: accountIdB64)
            applied.contactsAdded = restored.count
        }
        if !restored.isEmpty || applied.ownPredecessors > 0 {
            // A repository already bound to this account merges the contacts
            // (its next save would otherwise overwrite the write above), and
            // the chat layer re-reads the predecessor slot.
            NotificationCenter.default.post(
                name: .lakkoStateBundleApplied, object: restored,
                userInfo: [StateBundle.appliedAccountKey: accountIdB64])
        }

        // ADR-0058 section 5.2 — folders, pins, mutes, archive, read cursors,
        // drafts, merged before the DeviceGroup poll seeds from the store. A
        // running sync runtime for the account folds it too.
        // A state this build refuses (the version 1 encoding) is a backup that
        // holds none: the restore carries on without it.
        if !parsed.syncedState.isEmpty,
           case .merged = SyncedStateStore.mergeIntoStored(
               bytes: parsed.syncedState, accountIdB64: accountIdB64) {
            NotificationCenter.default.post(
                name: .lakkoSyncedStateRestored, object: nil,
                userInfo: ["accountIdB64": accountIdB64, "bytes": parsed.syncedState])
            applied.syncedStateMerged = true
        }

        // The conversations the account has. This device holds none of them
        // on the strength of the backup; the list is kept so its own uploads
        // go on naming them until it is admitted.
        let groupIdsHex = parsed.knownGroupIds.map(hexString(fromBytes:))
        RestoredKnownGroupsStore.add(
            accountIdB64: accountIdB64, groupIdsHex: groupIdsHex, defaults: defaults)
        applied.knownGroups = groupIdsHex.count

        NSLog(
            "backup restore (\(context)): \(applied.contactsAdded) new contact(s) of "
                + "\(parsed.acceptedContacts.count), synced state "
                + "\(applied.syncedStateMerged ? "merged" : "absent"), "
                + "\(applied.knownGroups) conversation(s) named, "
                + "\(applied.ownPredecessors) own predecessor(s), backup of \(parsed.createdAtMs)")
        return applied
    }

    /// The person's own name, photo and "who can find me", from the backup.
    /// Apart from ``apply(_:accountIdB64:context:defaults:ownPredecessors:)``
    /// because their owners live on the main actor.
    ///
    /// Fail closed: an empty or undecodable field changes nothing. The name
    /// and photo go to the own profile store only if it holds nothing newer.
    /// The setting takes the path a setting from another device takes
    /// (``ReachCenter/adoptRestoredSetting(envelope:accountIdB64:)``). What
    /// this device noted about the Everyone publication is dropped either way:
    /// a restored device republishes.
    @MainActor
    @discardableResult
    static func applyOwnState(
        ownProfile: Data,
        visibility: Data,
        accountIdB64: String,
        context: String,
        profiles: ProfileDirectory = .shared,
        reach: ReachCenter = .shared
    ) -> AppliedOwnState {
        var applied = AppliedOwnState()
        if !ownProfile.isEmpty {
            applied.ownProfile = profiles.adoptRestoredOwnProfile(
                envelope: ownProfile, accountIdB64: accountIdB64) == .adopted
        }
        reach.forgetPublishedEveryone(accountIdB64: accountIdB64)
        if !visibility.isEmpty {
            applied.visibility = reach.adoptRestoredSetting(
                envelope: visibility, accountIdB64: accountIdB64)
        }
        // A setting that was not newer, or a name alone, still owes the
        // publication the restore dropped (the active account only: an
        // account not active yet syncs when its session starts).
        if !applied.visibility { reach.sync(forAccount: accountIdB64) }
        NSLog("backup restore (\(context)): own profile \(applied.ownProfile ? "restored" : "kept"), "
            + "who can find me \(applied.visibility ? "restored" : "kept")")
        return applied
    }

    /// Lowercase hex, the form ``KnownGroupsStore`` and
    /// ``RestoredKnownGroupsStore`` hold.
    static func hexString(fromBytes bytes: Data) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
    }
}

/// "A phrase restore's backup step is still owed."
///
/// A route that restores the identity from the phrase sets this BEFORE it
/// fetches anything, and so before the session lands. It is removed only by a
/// definitive answer: the backup was applied, there is none (404), the head
/// gate refused it, the person declined a stale one, the blob does not open
/// under the phrase or does not parse, or this device holds no cached phrase
/// to open it with. A transport failure is not an answer, and neither is the
/// app being killed mid-restore: the mark stays, and each session landing for
/// the account makes one more attempt.
///
/// While the mark is set this device uploads no backup of its own
/// (``AutoBackupDriver`` and the recover baseline both stand aside): an upload
/// would replace the very blob the next attempt is going to fetch.
///
/// Per account, in `UserDefaults`. Removed by ``AccountLocalDataWipe``; kept
/// on Log out.
public enum BackupRestoreRetryStore {

    static let keyPrefix = "lakko.backup.restore-retry.v1."

    static func key(accountIdB64: String) -> String {
        keyPrefix + accountIdB64
    }

    public static func isPending(
        accountIdB64: String, defaults: UserDefaults = .standard
    ) -> Bool {
        defaults.bool(forKey: key(accountIdB64: accountIdB64))
    }

    public static func mark(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.set(true, forKey: key(accountIdB64: accountIdB64))
    }

    public static func clear(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: key(accountIdB64: accountIdB64))
    }
}

/// Section 7.2 of `docs/ux/identity-recovery.md`: this device was restored from
/// the recovery phrase or a passkey, and no conversation has arrived on it
/// yet. Written when the restore finishes, read by the chat list, gone for
/// good once the first conversation arrives.
///
/// Per account, in `UserDefaults`. Removed by ``AccountLocalDataWipe``; kept
/// on Log out.
public enum RestoredDeviceStore {

    public struct Record: Codable, Equatable {
        /// How many conversations the backup named.
        public var knownConversations: Int
        /// The list has not seen a conversation since the restore.
        public var awaitingFirstConversation: Bool
        /// The own profile is to be republished once the chats are loaded.
        public var profileRepublishOwed: Bool

        public init(
            knownConversations: Int, awaitingFirstConversation: Bool = true,
            profileRepublishOwed: Bool = true
        ) {
            self.knownConversations = knownConversations
            self.awaitingFirstConversation = awaitingFirstConversation
            self.profileRepublishOwed = profileRepublishOwed
        }
    }

    static let keyPrefix = "lakko.backup.restored-device.v1."

    static func key(accountIdB64: String) -> String { keyPrefix + accountIdB64 }

    public static func load(accountIdB64: String, defaults: UserDefaults = .standard) -> Record? {
        defaults.data(forKey: key(accountIdB64: accountIdB64))
            .flatMap { try? JSONDecoder().decode(Record.self, from: $0) }
    }

    private static func save(_ record: Record, accountIdB64: String, defaults: UserDefaults) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        guard let data = try? JSONEncoder().encode(record) else { return }
        defaults.set(data, forKey: key(accountIdB64: accountIdB64))
    }

    /// A restore finished. A device that already saw a conversation since an
    /// earlier restore stays that way only if this one names none.
    public static func noteRestored(
        accountIdB64: String, knownConversations: Int, defaults: UserDefaults = .standard
    ) {
        save(
            Record(knownConversations: knownConversations),
            accountIdB64: accountIdB64, defaults: defaults)
    }

    /// The first conversation arrived: neither the card nor the banner shows
    /// again.
    public static func noteConversationArrived(accountIdB64: String, defaults: UserDefaults = .standard) {
        guard var record = load(accountIdB64: accountIdB64, defaults: defaults),
              record.awaitingFirstConversation else { return }
        record.awaitingFirstConversation = false
        save(record, accountIdB64: accountIdB64, defaults: defaults)
    }

    /// Whether the own profile is owed; asking settles it.
    public static func takeProfileRepublish(accountIdB64: String, defaults: UserDefaults = .standard) -> Bool {
        guard var record = load(accountIdB64: accountIdB64, defaults: defaults),
              record.profileRepublishOwed else { return false }
        record.profileRepublishOwed = false
        save(record, accountIdB64: accountIdB64, defaults: defaults)
        return true
    }

    public static func clear(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: key(accountIdB64: accountIdB64))
    }
}

/// What the chat list says about a restored device, pure (section 7.2).
public enum RestoredListNotice: Equatable {
    /// The ordinary list.
    case none
    /// `chats.restored.card` with the button to Contacts.
    case card
    /// `chats.restored.banner_other_devices`.
    case banner

    /// - Parameters:
    ///   - record: the marker, `nil` on a device that was not restored.
    ///   - listEmpty: no conversation is on the list.
    ///   - otherDeviceSignedIn: `nil` while that is not known yet.
    public static func decide(
        record: RestoredDeviceStore.Record?, listEmpty: Bool, otherDeviceSignedIn: Bool?
    ) -> RestoredListNotice {
        guard let record, record.awaitingFirstConversation, listEmpty,
              let otherDeviceSignedIn else { return .none }
        if otherDeviceSignedIn { return .banner }
        return record.knownConversations > 0 ? .card : .none
    }
}

public extension Notification.Name {
    /// A backup was applied on this device; `userInfo["accountIdB64"]` names
    /// the account.
    static let lakkoBackupRestored = Notification.Name("me.lakko.backup-restored")
}
