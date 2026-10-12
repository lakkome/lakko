// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import Security

/// Forget device and Delete account: remove every piece of ONE account's
/// local data from this device (maintainer rule, 2026-09-30).
///
/// This is the only list of the account's persistent stores. Both flows reach
/// it through ``AppServices/wipeAccountLocalData(accountIdB64:)``; nothing
/// else keeps a private, partial copy of it (Delete account used to call the
/// auth half alone and left every chat store behind). Log out does not come
/// here: it keeps the account's data for its next sign-in.
///
/// What goes:
///   * every store keyed by the account (UserDefaults standard and App Group
///     suite, Keychain items, files in Application Support and the App Group
///     container), whether or not the runtime that owns it is bound;
///   * every device-wide key that holds this account's data: the retired v1
///     rows the per-account stores replaced, the unclaimed-founding marks of
///     the account's own journals, the selected folder tab, the add-device
///     answer log, the old device-wide recent emoji and skin tones,
///     decrypted media in the temporary directory and the account export
///     file.
///
/// What stays: other accounts' data, and genuine device settings (theme,
/// media preferences, the anti-censorship switch, update-check
/// bookkeeping, the KT-log root floor, the per-installation wrap keys and the
/// push device pseudonym, which the push unsubscribe handles).
///
/// Order: the push binding and the delivered notifications first, then the
/// rest. Best effort per store: one failing store never stops the others, and
/// each failure is logged in plain ASCII. At the end the stores are walked
/// (``AccountDataInventory``) and anything still naming the account is logged.
@MainActor
enum AccountLocalDataWipe {

    /// What a wipe could not do. Empty on a clean wipe.
    struct Report: Equatable {
        /// Steps that threw.
        var failedSteps: [String] = []
        /// Items still naming the account after every step ran.
        var leftovers: [AccountDataInventory.Item] = []
    }

    /// Device-wide `UserDefaults` key of the chat list's selected folder tab
    /// (`ChatListScreen`): a folder id of the signed-in account.
    static let selectedFolderTabKey = "lakko.chatFolders.selectedTab"

    /// Prefix of the per-`(account, group)` admin knock cursor
    /// (`ChatRepository.pollKnocks`); the account id follows.
    static let knockCursorPrefix = "me.lakko.chat.knock-cursor.v1."

    /// The retired v1 push reverse-index rows: keyed by `push_gid` alone, the
    /// value `[accountIdB64, groupIdHex]`. Never read since v2.
    static let legacyPushRoutePrefix = "me.lakko.push-route.v1."

    /// Name of the account export file ``PasskeysViewModel/exportData()``
    /// writes to the temporary directory.
    static let accountExportFileName = "lakko-account-export.json"

    /// Temporary directory of decrypted attachments handed to a share sheet.
    static let sharedAttachmentsDirectoryName = "shared-attachments"

    /// What a wipe may leave in place.
    enum Kept: Hashable {
        /// ADR-0066 section 1.1 — the STAGED wipe a device runs when the
        /// account's keys were reset elsewhere: everything goes except the
        /// recall record (``RecallRecord``), which is what lets the person
        /// take the account back from this device. Every other caller wipes
        /// it with the rest.
        case recallRecord
    }

    /// Remove every local store of `accountIdB64`.
    ///
    /// - Parameters:
    ///   - keeping: empty for the full wipe (Forget device, Delete account,
    ///     a removed device, the end of a recall); `[.recallRecord]` for the
    ///     staged wipe. The staged wipe is the full list minus that one step.
    ///   - removeDeliveredNotifications: injectable for tests.
    @discardableResult
    static func wipe(
        accountIdB64: String,
        keeping: Set<Kept> = [],
        removeDeliveredNotifications: () -> Void = AuthRepository.removeAllDeliveredNotifications
    ) -> Report {
        var report = Report()
        func step(_ name: String, _ body: () throws -> Void) {
            do {
                try body()
            } catch {
                report.failedSteps.append(name)
                NSLog("[account-wipe] ERR step %@ failed: %@", name, String(describing: error))
            }
        }
        let accountId = base64Decode(accountIdB64)
        let standard = UserDefaults.standard
        let shared = SharedStorage.sharedDefaults()

        // 1. Push first: no background wake is handled for this account from
        //    here on, and nothing it received stays on the lock screen.
        step("push-binding") {
            let binding = PushAccountBindingStore.live
            if binding.current()?.accountIdB64 == accountIdB64 { binding.clear() }
        }
        step("delivered-notifications") { removeDeliveredNotifications() }
        step("push-routing") { PushRoutingStore().clear(accountIdB64: accountIdB64) }
        step("push-routing-legacy-v1") {
            for defaults in [standard, shared].compactMap({ $0 }) {
                for (key, value) in defaults.dictionaryRepresentation()
                where key.hasPrefix(legacyPushRoutePrefix) {
                    if (value as? [String])?.first == accountIdB64 {
                        defaults.removeObject(forKey: key)
                    }
                }
            }
        }

        // 2. The session record and the account's keys.
        step("session-record") {
            let sessions = SessionStore()
            if sessions.load()?.accountIdB64 == accountIdB64 { sessions.clear() }
        }
        step("session-token") { SessionTokenStore.deleteSessionToken(accountIdB64: accountIdB64) }
        step("device-key") { DeviceKeyStore.deleteDeviceKey(accountIdB64: accountIdB64) }
        step("recovery-phrase") { try PhraseStore().deletePhrase(accountIdB64: accountIdB64) }
        step("three-words-budget") { ThreeWordsBudgetStore().clearAll(accountIdB64: accountIdB64) }
        step("recovery-markers") { RecoveryMarkers().clear(accountIdB64: accountIdB64) }
        // ADR-0067 — the protection flags, the notice cursor and the "I did
        // this" markers.
        step("account-protection") { AccountProtectionStore().clear(accountIdB64: accountIdB64) }
        // ADR-0066 — the "account key recorded at auth" mark and the owed
        // device removals belong to the account's data on this device.
        step("identity-key-record") { IdentityKeyRecordMarks().clear(accountIdB64: accountIdB64) }
        step("device-removal-owed") { DeviceRemovalStore().clear(accountIdB64: accountIdB64) }
        // ADR-0066 part 3 — the owed key rotation, the pending rotation
        // envelopes of a kept device, the account's own predecessor seed and
        // the "keys were changed" line.
        step("key-rotation-owed") { KeyRotationStore().clear(accountIdB64: accountIdB64) }
        step("key-rotation-pending") { KeychainKeyRotationPendingStore().clear(accountIdB64: accountIdB64) }
        step("own-predecessor-seed") { KeychainOwnPredecessorStore().clear(accountIdB64: accountIdB64) }
        step("keys-rotated-notice") { KeysRotatedNoticeStore().clear(accountIdB64: accountIdB64) }
        step("profile-resend-owed") { ProfileResendOwed.Store().clear(accountIdB64: accountIdB64) }
        step("predecessor-inbox-cursor") {
            PredecessorMailbox.CursorStore().clear(accountIdB64: accountIdB64)
            PredecessorMailbox.AdoptionStore().clear(accountIdB64: accountIdB64)
        }
        // ADR-0066 section 1.1 — the recall record: the one step the staged
        // wipe skips.
        if !keeping.contains(.recallRecord) {
            step(recallRecordStepName) {
                KeychainRecallRecordStore().clear(accountIdB64: accountIdB64)
            }
        }
        step("backup-floor") { BackupFloorStore.deleteFloor(accountIdB64: accountIdB64) }
        step("auto-backup") { AutoBackupStore.clear(accountIdB64: accountIdB64) }
        step("export-key") { ExportKeyStore.deleteExportKey(accountIdB64: accountIdB64) }
        step("account-identity-seed") {
            AccountIdentitySeedStore.deleteAccountIdentitySeed(accountIdB64: accountIdB64)
        }
        step("device-identity-seed") {
            DeviceIdentityStore.deleteDeviceIdentitySeed(accountIdB64: accountIdB64)
        }
        step("invite-link-secrets") {
            InviteLinkSecretStore.shared.clear(accountIdB64: accountIdB64)
        }
        step("anon-tokens") {
            // This account's stored tokens; any other account's stay. The
            // pool stops writing for this account too, so the token the
            // session's push unregister spends cannot put the rows back.
            AnonTokenPool.shared.forget(accountKey: accountIdB64)
        }
        step("reach") { ReachCenter.shared.forget(accountIdB64: accountIdB64) }

        // 3. The DeviceGroup and the cross-device synced state. The founding
        //    mark of the account's DeviceGroup journal goes too; the id is
        //    derived from the account id.
        step("device-group-snapshot") {
            DeviceGroupSnapshotStore().deleteSnapshot(accountIdB64: accountIdB64)
        }
        step("synced-state") { SyncedStateStore.delete(accountIdB64: accountIdB64) }
        step("synced-state-pending") {
            PendingSyncTracker(accountIdB64: accountIdB64).clear()
        }
        step("device-group-founding-mark") {
            guard let accountId else { return }
            let groupId = try deviceGroupIdFor(accountId: accountId)
            JournalFoundingMarks.shared.clear(journalIdHex: DsClient.pseudonymHex(groupId))
        }

        // 4. Chats.
        step("mls-snapshot") { IdentitySnapshotStore().deleteSnapshot(accountIdB64: accountIdB64) }
        step("snapshot-leaf-key") { SnapshotLeafKeyStore().clear(accountIdB64: accountIdB64) }
        step("known-groups") { KnownGroupsStore().clear(accountIdB64: accountIdB64) }
        step("group-names") { GroupNameStore().clear(accountIdB64: accountIdB64) }
        step("shared-pins") { SharedPinStore().clear(accountIdB64: accountIdB64) }
        step("personal-pins") { PersonalPinStore().clear(accountIdB64: accountIdB64) }
        step("read-watermarks") { ReadWatermarkStore().clear(accountIdB64: accountIdB64) }
        step("profile-requests") { ProfileRequestStore().clear(accountIdB64: accountIdB64) }
        step("group-calls") { GroupCallStore().clear(accountIdB64: accountIdB64) }
        step("device-readmission") {
            DeviceReadmissionMemory.clearForAccount(accountIdB64: accountIdB64)
        }
        step("commit-outbox") {
            CommitOutboxStore().deleteOutboxForAccount(accountIdB64: accountIdB64)
        }
        step("registrar-seeds") { RegistrarSeedStore().clear(accountIdB64: accountIdB64) }
        step("committer-beacons") {
            BeaconObservationStore().clearForAccount(accountIdB64: accountIdB64)
        }
        step("pending-requests") {
            PendingRequestStore().clearForAccount(accountIdB64: accountIdB64)
        }
        step("unapplied-requests") {
            UnappliedRequestStore().clearForAccount(accountIdB64: accountIdB64)
        }
        step("last-observed-owner") {
            LastObservedOwnerStore().clearForAccount(accountIdB64: accountIdB64)
        }
        step("leave-requests") { LeaveRequestStore().clearForAccount(accountIdB64: accountIdB64) }
        step("knock-cursors") {
            removeKeys(in: standard, withPrefix: knockCursorPrefix + accountIdB64 + ".")
        }
        step("message-log") { MessageLogStore().deleteLog(accountIdB64: accountIdB64) }

        // 5. Contacts and profiles.
        step("verified-bindings") { VerifiedBindingStore.clear(ownerAccountIdB64: accountIdB64) }
        step("reset-continuity") { Continuity.clearMarker(accountIdB64: accountIdB64) }
        // ADR-0065 R.5-R.7 — the successor-claim state, the continuity
        // bookkeeping, the held messages and the unsent-lineage marker.
        step("lineage-mirror") { ConversationLineageMirror.clear(accountIdB64: accountIdB64) }
        step("continuity-mirror") { ContinuityMirror.clear(accountIdB64: accountIdB64) }
        step("group-closed") { GroupClosedStore.clear(ownerAccountIdB64: accountIdB64) }
        step("continuity-store") { ContinuityStore.clear(ownerAccountIdB64: accountIdB64) }
        step("held-sends") { HeldSendQueue.clearAccount(ownerAccountId: accountIdB64) }
        step("lineage-unsent") {
            standard.removeObject(
                forKey: DeviceGroupSyncRepository.lineageUnsentKey(accountIdB64: accountIdB64))
        }
        step("profiles") { ProfileDirectory.shared.forget(accountIdB64: accountIdB64) }
        step("contacts-inbox") { ContactsInboxStore().clear(accountIdB64: accountIdB64) }
        // ADR-0065 R.3 — the requests kept waiting for an identity verdict.
        step("contacts-inbox-waiting") { KnownRequestWaitingStore().clear(accountIdB64: accountIdB64) }
        // ADR-0066 section 3.5a rule 5 — the unanswered outgoing requests
        // (recipients and the intro text), a Keychain item.
        step("contacts-sent-requests") { KeychainSentRequestsStore().clear(accountIdB64: accountIdB64) }
        step("accepted-contacts") { AcceptedContactsStore.clear(accountIdB64: accountIdB64) }
        step("peer-pins") { PeerPinStore.clear(ownerAccountIdB64: accountIdB64) }
        step("proven-predecessors") { ProvenPredecessorStore.clear(ownerAccountIdB64: accountIdB64) }
        step("pairing-known-groups") { StateBundle.clearKnownGroupIds(accountIdB64: accountIdB64) }

        // 6. Channels. The founding marks of the account's own channels are
        //    read off its channel list, so they go before the list does.
        step("channel-founding-marks") {
            let channelIds = Set(
                (ChannelLogStore().get(accountIdB64: accountIdB64)?.channels ?? [])
                    .map { $0.id.lowercased() })
            guard !channelIds.isEmpty else { return }
            let marks = JournalFoundingMarks.shared
            for journalIdHex in marks.all() {
                guard let groupId = CoPublisher.hexToData(journalIdHex),
                      let channelId = channelIdFromPublisherGroupId(groupId: groupId),
                      channelIds.contains(ChannelsComposer.bytesToHex(channelId).lowercased())
                else { continue }
                marks.clear(journalIdHex: journalIdHex)
            }
        }
        step("channels") { ChannelsUiState.forget(accountIdB64: accountIdB64) }
        step("co-published-journals") {
            CoPublishedJournals.shared.clear(accountIdB64: accountIdB64)
        }
        // ADR-0063 — the decrypted channel posts the feed reads.
        step("channel-post-log") { ChannelPostLogStore().delete(accountIdB64: accountIdB64) }
        // ADR-0059 — G-43 publisher roles learned this session (in memory,
        // keyed per (account, channel) owner key; cleared whole).
        step("publisher-roles") { ChannelPublisherRoleCache.shared.clearAll() }
        // ADR-0059 — Discover: picked topics, decoys, cached buckets,
        // not-interested channels, the share switches and the lists members
        // shared.
        step("discover") { DiscoverStore().delete(accountIdB64: accountIdB64) }

        // 6b. The feed (ADR-0063): saved feeds, and `docs/ux/feed.md`
        //     section 5 hidden conversations, last looks and channel read
        //     markers.
        step("feeds") { FeedStore().delete(accountIdB64: accountIdB64) }
        step("feed-reading") { FeedReadingStore().delete(accountIdB64: accountIdB64) }
        // The Feed tab's selection (it can name one of this account's saved
        // feeds) and the device banner: device-wide keys, as on Android.
        step("feed-prefs") {
            standard.removeObject(forKey: FeedViewModel.selectionKey)
            standard.removeObject(forKey: FeedViewModel.bannerKey)
        }

        // 7. Calls and media.
        step("call-history") { CallHistoryStore.shared.forget(accountIdB64: accountIdB64) }
        step("call-peer-names") { CallPeerNameMirror.clear(accountIdB64: accountIdB64) }
        step("conversation-mutes") { ConversationMuteMirror.clear(accountIdB64: accountIdB64) }
        step("media-played") { MediaPrefs().clearPlayed(accountIdB64: accountIdB64) }
        step("emoji-recents-and-tones") { EmojiIndex.clear(accountIdB64: accountIdB64, standard) }

        // 8. Device-wide keys and files holding this account's data.
        step("selected-folder-tab") { standard.removeObject(forKey: selectedFolderTabKey) }
        step("device-link-answers") {
            standard.removeObject(forKey: DeviceLinkAnswerLimit.defaultKey)
        }
        step("new-device-notice") {
            NewDeviceNoticeStore(defaults: standard).clear(accountIdB64: accountIdB64)
        }
        step("decrypted-media") {
            MediaSessionCache.shared.purge()
            MediaTempFiles.sweepStale()
            try removeIfPresent(
                FileManager.default.temporaryDirectory
                    .appendingPathComponent(sharedAttachmentsDirectoryName, isDirectory: true))
        }
        step("account-export-file") {
            try removeIfPresent(
                FileManager.default.temporaryDirectory
                    .appendingPathComponent(accountExportFileName))
        }

        // 9. Whatever still names the account is a store this list missed.
        report.leftovers = AccountDataInventory.remaining(accountIdB64: accountIdB64)
            .filter { !(keeping.contains(.recallRecord) && AccountDataInventory.isRecallRecord($0)) }
        if !report.leftovers.isEmpty {
            let byPlace = Dictionary(grouping: report.leftovers, by: { $0.place })
            for (place, items) in byPlace.sorted(by: { $0.key.rawValue < $1.key.rawValue }) {
                NSLog("[account-wipe] WARN %d item(s) still name the account in %@",
                      items.count, place.rawValue)
            }
        }
        NSLog("[account-wipe] done: %d step(s) failed, %d leftover item(s)",
              report.failedSteps.count, report.leftovers.count)
        return report
    }

    /// The name of the one step ``Kept/recallRecord`` skips.
    static let recallRecordStepName = "recall-record"

    private static func removeKeys(in defaults: UserDefaults, withPrefix prefix: String) {
        for key in defaults.dictionaryRepresentation().keys where key.hasPrefix(prefix) {
            defaults.removeObject(forKey: key)
        }
    }

    private static func removeIfPresent(_ url: URL) throws {
        guard FileManager.default.fileExists(atPath: url.path) else { return }
        try FileManager.default.removeItem(at: url)
    }
}

/// Walk the stores an account's data can live in and list every item that
/// names the account: a `UserDefaults` key or value (standard and App Group
/// suite), a Keychain item's service or account attribute, a file or folder
/// name under Application Support or the App Group container.
///
/// "Names" means contains one of the account id's encodings: base64 with and
/// without padding, the filesystem-safe form (`/` as `_`), base64url, and hex.
/// Encrypted contents are not inspected, and stores that key by something
/// derived from the account (a DeviceGroup journal id) are out of its reach;
/// ``AccountLocalDataWipe`` handles those by name.
enum AccountDataInventory {

    enum Place: String, CaseIterable {
        case standardDefaults = "standard-defaults"
        case sharedDefaults = "shared-defaults"
        case keychain
        case applicationSupport = "application-support"
        case appGroupContainer = "app-group-container"
    }

    struct Item: Hashable, CustomStringConvertible {
        let place: Place
        let name: String
        var description: String { "\(place.rawValue): \(name)" }
    }

    /// Whether `item` is the recall record's Keychain item
    /// (``KeychainRecallRecordStore``): the one item the staged wipe leaves.
    static func isRecallRecord(_ item: Item) -> Bool {
        item.place == .keychain && item.name.hasPrefix(KeychainRecallRecordStore.serviceTag + " | ")
    }

    /// The encodings of `accountIdB64` a store may use.
    static func needles(accountIdB64: String) -> [String] {
        let unpadded = accountIdB64.replacingOccurrences(of: "=", with: "")
        var out: Set<String> = [
            accountIdB64,
            unpadded,
            unpadded.replacingOccurrences(of: "/", with: "_"),
            unpadded.replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_"),
        ]
        if let bytes = base64Decode(accountIdB64) {
            out.insert(bytes.map { String(format: "%02x", $0) }.joined())
        }
        return out.filter { $0.count >= 8 }.sorted()
    }

    /// Every item that still names `accountIdB64`.
    static func remaining(accountIdB64: String) -> [Item] {
        let needles = needles(accountIdB64: accountIdB64)
        guard !needles.isEmpty else { return [] }
        func names(_ text: String) -> Bool { needles.contains { text.contains($0) } }

        var out: [Item] = []
        out += defaultsItems(UserDefaults.standard, place: .standardDefaults, names: names)
        if let shared = SharedStorage.sharedDefaults() {
            out += defaultsItems(shared, place: .sharedDefaults, names: names)
        }
        out += keychainItems(names: names)
        if let appSupport = try? FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask,
            appropriateFor: nil, create: false) {
            out += fileItems(under: appSupport, place: .applicationSupport, names: names)
        }
        if let container = SharedStorage.containerURL() {
            out += fileItems(under: container, place: .appGroupContainer, names: names)
        }
        return out
    }

    private static func defaultsItems(
        _ defaults: UserDefaults, place: Place, names: (String) -> Bool
    ) -> [Item] {
        defaults.dictionaryRepresentation().compactMap { key, value in
            names(key) || valueNames(value, names: names) ? Item(place: place, name: key) : nil
        }
    }

    private static func valueNames(_ value: Any, names: (String) -> Bool) -> Bool {
        switch value {
        case let text as String:
            return names(text)
        case let data as Data:
            return String(data: data, encoding: .utf8).map(names) ?? false
        case let array as [Any]:
            return array.contains { valueNames($0, names: names) }
        case let dict as [String: Any]:
            return dict.contains { names($0.key) || valueNames($0.value, names: names) }
        default:
            return false
        }
    }

    private static func keychainItems(names: (String) -> Bool) -> [Item] {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecReturnAttributes: true,
            kSecMatchLimit: kSecMatchLimitAll,
        ]
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let rows = result as? [[String: Any]] else { return [] }
        return rows.compactMap { row in
            let service = row[kSecAttrService as String] as? String ?? ""
            let account = row[kSecAttrAccount as String] as? String ?? ""
            let group = row[kSecAttrAccessGroup as String] as? String ?? ""
            guard names(service) || names(account) else { return nil }
            return Item(place: .keychain, name: "\(service) | \(account) | \(group)")
        }
    }

    private static func fileItems(under root: URL, place: Place, names: (String) -> Bool) -> [Item] {
        guard let walker = FileManager.default.enumerator(
            at: root, includingPropertiesForKeys: nil, options: []) else { return [] }
        var out: [Item] = []
        let rootPath = root.standardizedFileURL.path
        for case let url as URL in walker {
            let path = url.standardizedFileURL.path
            let relative = path.hasPrefix(rootPath) ? String(path.dropFirst(rootPath.count)) : path
            // The UserDefaults backing plists are walked through the API above.
            if relative.hasPrefix("/Library/Preferences") { continue }
            if names(url.lastPathComponent) { out.append(Item(place: place, name: relative)) }
        }
        return out
    }
}
