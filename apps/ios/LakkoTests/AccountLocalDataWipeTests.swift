// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
import Security
@testable import Lakko

/// Forget device and Delete account leave nothing of the account behind
/// (maintainer rule, 2026-09-30), tested by inventory rather than by listing.
///
/// Every account-scoped store is seeded for two accounts through the real
/// stores (real `UserDefaults.standard` and App Group suite, real Keychain,
/// real files), ``AccountLocalDataWipe`` runs for one, and then the stores are
/// WALKED: every `UserDefaults` key and value, every Keychain generic
/// password, every file name under Application Support and the App Group
/// container. Nothing may still name the wiped account; the other account's
/// items must be exactly what they were.
///
/// The walk finds anything keyed or valued by the account id. Three things it
/// cannot see by name are asserted directly: the unclaimed-founding marks
/// (keyed by journal id, derived from the account), and the device-wide
/// folder tab, add-device answer log, old recent emoji and tones and
/// temporary decrypted media.
@MainActor
final class AccountLocalDataWipeTests: XCTestCase {

    /// Bytes chosen so the base64 form carries both `/` and `+`, which the
    /// file stores rewrite (`/` -> `_`).
    private let accountIdA = Data([
        0xFF, 0xFF, 0xFF, 0xFB, 0xEF, 0xBE, 0x5A, 0x11,
        0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0xA1,
    ])
    private let accountIdB = Data([
        0x0B, 0x1B, 0x2B, 0x3B, 0x4B, 0x5B, 0x6B, 0x7B,
        0x8B, 0x9B, 0xAB, 0xBB, 0xCB, 0xDB, 0xEB, 0xB2,
    ])
    /// The account of the unknown-store test alone: its planted stores name
    /// no account any other test inventories, so a removal the App Group
    /// preferences daemon has not applied yet cannot reach another test's
    /// walk (the full suite saw `lakko.test-unknown-store.v1` survive into
    /// the next test's inventory of account A).
    private let accountIdC = Data([
        0x0C, 0x1C, 0x2C, 0x3C, 0x4C, 0x5C, 0x6C, 0x7C,
        0x8C, 0x9C, 0xAC, 0xBC, 0xCC, 0xDC, 0xEC, 0xC3,
    ])
    private var a: String { base64Encode(accountIdA) }
    private var b: String { base64Encode(accountIdB) }
    private var c: String { base64Encode(accountIdC) }
    private static let unknownSharedKey = "lakko.test-unknown-store.v1"

    private let channelA = Data(repeating: 0xCA, count: 16)
    private let channelB = Data(repeating: 0xCB, count: 16)

    // Device-wide values this test touches, restored afterwards.
    private var savedSession: PersistedSession?
    private var savedBinding: PushAccountBinding?
    private var savedFolderTab: Any?
    private var savedAnswers: Any?
    private var savedMarks: [String] = []

    override func setUp() async throws {
        try await super.setUp()
        savedSession = SessionStore().load()
        savedBinding = PushAccountBindingStore.live.current()
        savedFolderTab = UserDefaults.standard.object(forKey: AccountLocalDataWipe.selectedFolderTabKey)
        savedAnswers = UserDefaults.standard.object(forKey: DeviceLinkAnswerLimit.defaultKey)
        savedMarks = JournalFoundingMarks.shared.all()
        // A planted unknown store left by an interrupted run.
        SharedStorage.sharedDefaults()?.removeObject(forKey: Self.unknownSharedKey)
        wipeQuietly(a)
        wipeQuietly(b)
        // The clean-up above is a wipe like any other and seals both
        // accounts. Each test then seeds them as a signed-in account's own
        // stores, which is the account landing again.
        AccountStoreGate.open(a)
        AccountStoreGate.open(b)
    }

    override func tearDown() async throws {
        wipeQuietly(a)
        wipeQuietly(b)
        if let savedSession { SessionStore().save(savedSession) }
        if let savedBinding {
            PushAccountBindingStore.live.bind(accountIdB64: savedBinding.accountIdB64)
        }
        UserDefaults.standard.set(savedFolderTab, forKey: AccountLocalDataWipe.selectedFolderTabKey)
        UserDefaults.standard.set(savedAnswers, forKey: DeviceLinkAnswerLimit.defaultKey)
        for mark in savedMarks { JournalFoundingMarks.shared.mark(journalIdHex: mark) }
        try await super.tearDown()
    }

    // MARK: - The inventory

    func testWipeRemovesEveryItemNamingTheAccountAndKeepsTheOther() throws {
        try seed(accountId: accountIdA, channelId: channelA)
        try seed(accountId: accountIdB, channelId: channelB)
        // The signed-in account's device-wide rows.
        SessionStore().save(PersistedSession(accountIdB64: a, username: "alice"))
        PushAccountBindingStore.live.bind(accountIdB64: a)

        let beforeA = AccountDataInventory.remaining(accountIdB64: a)
        let beforeB = AccountDataInventory.remaining(accountIdB64: b)
        let perPlace = Dictionary(grouping: beforeA, by: { $0.place }).mapValues(\.count)
        NSLog("[account-wipe-test] seeded for the wiped account: total %d, %@",
              beforeA.count,
              perPlace.map { "\($0.key.rawValue)=\($0.value)" }.sorted().joined(separator: " "))
        // The walk reaches every kind of store the seed wrote to.
        XCTAssertGreaterThanOrEqual(perPlace[.standardDefaults] ?? 0, 20)
        XCTAssertGreaterThanOrEqual(perPlace[.keychain] ?? 0, 10)
        if SharedStorage.sharedDefaults() != nil {
            XCTAssertGreaterThan(perPlace[.sharedDefaults] ?? 0, 3)
        }
        if SharedStorage.containerURL() != nil {
            XCTAssertGreaterThan(perPlace[.appGroupContainer] ?? 0, 2)
        }
        XCTAssertGreaterThan(perPlace[.applicationSupport] ?? 0, 2)

        var removals = 0
        let report = AccountLocalDataWipe.wipe(accountIdB64: a, removeDeliveredNotifications: { removals += 1 })

        XCTAssertEqual(report.failedSteps, [])
        XCTAssertEqual(report.leftovers, [], "every store naming the account is wiped")
        XCTAssertEqual(AccountDataInventory.remaining(accountIdB64: a), [])
        XCTAssertEqual(removals, 1)
        XCTAssertNil(SessionStore().load(), "the session record named the wiped account")
        XCTAssertNil(PushAccountBindingStore.live.current(), "the binding named the wiped account")
        XCTAssertFalse(X25519BindingConfirmedMarks().isConfirmed(accountIdB64: a), "the binding mark is wiped")
        XCTAssertTrue(X25519BindingConfirmedMarks().isConfirmed(accountIdB64: b))

        // The other account's data is exactly what it was.
        XCTAssertEqual(Set(AccountDataInventory.remaining(accountIdB64: b)), Set(beforeB))
        XCTAssertFalse(beforeB.isEmpty)
        NSLog("[account-wipe-test] other account kept: %d item(s)", beforeB.count)
    }

    /// ADR-0074 section 1.1 — the STAGED wipe is the full list minus ONE
    /// item: after it, the only thing on the device that still names the
    /// account is the recall record, and the full wipe then removes that too.
    func testTheStagedWipeDeletesEverythingExceptTheRecallRecord() throws {
        try seed(accountId: accountIdA, channelId: channelA)
        try seed(accountId: accountIdB, channelId: channelB)
        SessionStore().save(PersistedSession(accountIdB64: a, username: "alice"))
        let beforeA = AccountDataInventory.remaining(accountIdB64: a)
        let beforeB = AccountDataInventory.remaining(accountIdB64: b)
        let recallItems = beforeA.filter(AccountDataInventory.isRecallRecord)
        XCTAssertEqual(recallItems.count, 1, "the seed planted one recall record for the account")

        let staged = AccountLocalDataWipe.wipe(
            accountIdB64: a, keeping: [.recallRecord], removeDeliveredNotifications: {})

        XCTAssertEqual(staged.failedSteps, [])
        XCTAssertEqual(staged.leftovers, [], "the kept record is not a leftover of the staged wipe")
        XCTAssertEqual(
            AccountDataInventory.remaining(accountIdB64: a), recallItems,
            "the full wipe's list minus the record: nothing else of the account is left")
        // The other account is untouched, its own recall record included.
        XCTAssertEqual(Set(AccountDataInventory.remaining(accountIdB64: b)), Set(beforeB))
        KeychainRecallRecordStore().clear(accountIdB64: b)
        // What it keeps is exactly the record, readable, with its four fields.
        let kept = KeychainRecallRecordStore().load()
        XCTAssertEqual(kept?.accountIdB64, a)
        XCTAssertEqual(kept?.oldSeed, Data(repeating: 9, count: RecallRecord.seedLength))
        XCTAssertEqual(kept?.username, "alice")
        // The account identity seed, the phrase, the device key and the
        // session are gone.
        XCTAssertNil(try AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: a))
        XCTAssertNil(try DeviceKeyStore.getDeviceKey(accountIdB64: a))
        XCTAssertNil(SessionStore().load())

        // The end of the recall (Yes, closed, expired): the full wipe.
        let full = AccountLocalDataWipe.wipe(accountIdB64: a, removeDeliveredNotifications: {})
        XCTAssertEqual(full.failedSteps, [])
        XCTAssertEqual(AccountDataInventory.remaining(accountIdB64: a), [])
    }

    /// The recall record is stored in the app's own Keychain group, never in
    /// the shared group the notification extension can read.
    func testTheRecallRecordIsNotInTheSharedKeychainGroup() throws {
        try KeychainRecallRecordStore().put(RecallRecord(
            accountIdB64: a, oldSeed: Data(repeating: 9, count: RecallRecord.seedLength),
            username: "alice", learnedAtMs: 1))
        defer { KeychainRecallRecordStore().clear(accountIdB64: a) }
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: KeychainRecallRecordStore.serviceTag,
            kSecAttrAccount: a,
            kSecReturnAttributes: true,
            kSecMatchLimit: kSecMatchLimitAll,
        ]
        var result: CFTypeRef?
        XCTAssertEqual(SecItemCopyMatching(query as CFDictionary, &result), errSecSuccess)
        let rows = try XCTUnwrap(result as? [[String: Any]])
        XCTAssertEqual(rows.count, 1)
        let group = rows[0][kSecAttrAccessGroup as String] as? String ?? ""
        XCTAssertFalse(group.isEmpty)
        XCTAssertFalse(
            group.hasSuffix(SharedStorage.keychainAccessGroup),
            "the record must not be readable by the notification extension")
        XCTAssertEqual(
            rows[0][kSecAttrAccessible as String] as? String,
            kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String,
            "this device only, no iCloud Keychain")
    }

    /// The walk is what makes the list checkable: a store the wipe does not
    /// know about is reported, in every place it can be, rather than passing
    /// silently.
    func testAStoreTheWipeDoesNotKnowIsReportedAsLeftover() throws {
        let unknownKey = "lakko.test-unknown-store.v1.\(c)"
        UserDefaults.standard.set(Data([1]), forKey: unknownKey)
        SharedStorage.sharedDefaults()?.set(["x": c], forKey: Self.unknownSharedKey)
        defer {
            UserDefaults.standard.removeObject(forKey: unknownKey)
            SharedStorage.sharedDefaults()?.removeObject(forKey: Self.unknownSharedKey)
            SharedStorage.sharedDefaults()?.synchronize()
        }
        let appSupport = try FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
        let unknownFile = appSupport.appendingPathComponent(
            "lakko-test-unknown/\(c.replacingOccurrences(of: "/", with: "_")).bin")
        try FileManager.default.createDirectory(
            at: unknownFile.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data([1]).write(to: unknownFile)
        defer { try? FileManager.default.removeItem(at: unknownFile.deletingLastPathComponent()) }

        let report = AccountLocalDataWipe.wipe(accountIdB64: c, removeDeliveredNotifications: {})

        let places = Set(report.leftovers.map(\.place))
        XCTAssertTrue(report.leftovers.contains(AccountDataInventory.Item(place: .standardDefaults, name: unknownKey)))
        XCTAssertTrue(places.contains(.applicationSupport))
        if SharedStorage.sharedDefaults() != nil {
            XCTAssertTrue(places.contains(.sharedDefaults), "a value naming the account is found too")
        }
    }

    // MARK: - What the walk cannot see by name

    func testFoundingMarksOfTheAccountsOwnJournalsGoAndOthersStay() throws {
        try seed(accountId: accountIdA, channelId: channelA)
        try seed(accountId: accountIdB, channelId: channelB)
        // ADR-0078 section 6 — both ids are derived from the lineage seed:
        // the oldest own predecessor when one is held, as `seed` stores one.
        let ownerA = try Identity.fromSeed(seed: predecessorSeed(accountIdA))
        let ownerB = try Identity.fromSeed(seed: predecessorSeed(accountIdB))
        let deviceGroupA = DsClient.pseudonymHex(deviceGroupIdFor(accountIdentity: ownerA))
        let deviceGroupB = DsClient.pseudonymHex(deviceGroupIdFor(accountIdentity: ownerB))
        let channelGroupA = DsClient.pseudonymHex(try publisherGroupIdFor(ownerIdentity: ownerA, channelId: channelA))
        let channelGroupB = DsClient.pseudonymHex(try publisherGroupIdFor(ownerIdentity: ownerB, channelId: channelB))
        for mark in [deviceGroupA, deviceGroupB, channelGroupA, channelGroupB] {
            JournalFoundingMarks.shared.mark(journalIdHex: mark)
        }

        AccountLocalDataWipe.wipe(accountIdB64: a, removeDeliveredNotifications: {})

        let marks = JournalFoundingMarks.shared
        XCTAssertFalse(marks.isMarked(journalIdHex: deviceGroupA))
        XCTAssertFalse(marks.isMarked(journalIdHex: channelGroupA))
        XCTAssertTrue(marks.isMarked(journalIdHex: deviceGroupB))
        XCTAssertTrue(marks.isMarked(journalIdHex: channelGroupB))
    }

    func testDeviceWideKeysAndDecryptedMediaHoldingTheAccountsDataGo() throws {
        let defaults = UserDefaults.standard
        defaults.set("00ff", forKey: AccountLocalDataWipe.selectedFolderTabKey)
        DeviceLinkAnswerLimit().record()
        // The recent emoji and tones from before they were per account.
        defaults.set(["\u{1F600}"], forKey: EmojiIndex.recentKey)
        defaults.set(["\u{1F44B}": "\u{1F44B}\u{1F3FF}"], forKey: EmojiIndex.tonesKey)
        let fm = FileManager.default
        let chatMedia = MediaTempFiles.defaultParent.appendingPathComponent("wipe-test/blob/voice.m4a")
        let shared = fm.temporaryDirectory
            .appendingPathComponent(AccountLocalDataWipe.sharedAttachmentsDirectoryName)
            .appendingPathComponent("wipe-test/photo.jpg")
        let export = fm.temporaryDirectory.appendingPathComponent(AccountLocalDataWipe.accountExportFileName)
        for url in [chatMedia, shared, export] {
            try fm.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try Data([1, 2, 3]).write(to: url)
        }

        let report = AccountLocalDataWipe.wipe(accountIdB64: a, removeDeliveredNotifications: {})

        XCTAssertEqual(report.failedSteps, [])
        XCTAssertNil(defaults.object(forKey: AccountLocalDataWipe.selectedFolderTabKey))
        XCTAssertNil(defaults.object(forKey: DeviceLinkAnswerLimit.defaultKey))
        XCTAssertTrue(DeviceLinkAnswerLimit().canAnswer())
        XCTAssertNil(defaults.object(forKey: EmojiIndex.recentKey))
        XCTAssertNil(defaults.object(forKey: EmojiIndex.tonesKey))
        XCTAssertFalse(fm.fileExists(atPath: chatMedia.path))
        XCTAssertFalse(fm.fileExists(atPath: shared.path))
        XCTAssertFalse(fm.fileExists(atPath: export.path))
    }

    func testDeviceSettingsStay() {
        let defaults = UserDefaults.standard
        let keys = [
            AppearanceSetting.storageKey, MediaPrefs.autoplayKey, MediaPrefs.recordModeKey,
        ]
        let saved = keys.map { defaults.object(forKey: $0) }
        defer { for (key, value) in zip(keys, saved) { defaults.set(value, forKey: key) } }
        defaults.set("dark", forKey: AppearanceSetting.storageKey)
        defaults.set(true, forKey: MediaPrefs.autoplayKey)
        defaults.set("videoNote", forKey: MediaPrefs.recordModeKey)

        AccountLocalDataWipe.wipe(accountIdB64: a, removeDeliveredNotifications: {})

        XCTAssertEqual(defaults.string(forKey: AppearanceSetting.storageKey), "dark")
        XCTAssertTrue(defaults.bool(forKey: MediaPrefs.autoplayKey))
        XCTAssertEqual(defaults.string(forKey: MediaPrefs.recordModeKey), "videoNote")
    }

    // MARK: - Seeding

    /// Write one account's rows into every account-scoped store, through each
    /// store's own API where it has a simple one.
    private func seed(accountId: Data, channelId: Data) throws {
        let x = base64Encode(accountId)
        let group = "aa" + DsClient.pseudonymHex(accountId)
        let standard = UserDefaults.standard
        let channelHex = ChannelsComposer.bytesToHex(channelId)

        // Keychain.
        try SessionTokenStore.putSessionToken(
            accountIdB64: x, token: PersistedSessionToken(sessionToken: "tok", sessionExpiresAtMs: 1))
        try DeviceKeyStore.putDeviceKey(accountIdB64: x, rootKey: Data(repeating: 1, count: deviceRootKeyLength))
        try PhraseStore().putPhrase(accountIdB64: x, phraseBytes: Data("abandon ability".utf8))
        // A guest account's phrase row, under its own key (`GuestAccount.swift`).
        try PhraseStore().putPhrase(
            accountIdB64: guestPhraseKey(accountIdB64: x), phraseBytes: Data("abandon ability".utf8))
        try BackupFloorStore.putFloor(
            accountIdB64: x, floor: BackupFloor(version: 1, envelopeHash: Data(repeating: 2, count: 32)))
        try ExportKeyStore.putExportKey(
            accountIdB64: x, exportKey: Data(repeating: 3, count: ExportKeyStore.exportKeyLength))
        try AccountIdentitySeedStore.putAccountIdentitySeed(
            accountIdB64: x, seed: accountSeed(accountId))
        // ADR-0074 section 1.1 — the recall record of a reset elsewhere.
        try KeychainRecallRecordStore().put(RecallRecord(
            accountIdB64: x, oldSeed: Data(repeating: 9, count: RecallRecord.seedLength),
            username: "alice", learnedAtMs: 1))
        try DeviceIdentityStore.putDeviceIdentitySeed(
            accountIdB64: x, seed: Data(repeating: 5, count: DeviceIdentityStore.seedLength))
        try DeviceIdentityStore.putDeviceBinding(accountIdB64: x, binding: Data([6, 6]))
        InviteLinkSecretStore.shared.put(accountIdB64: x, slug: "slug", secret: Data(repeating: 7, count: 12))
        PasskeyNoPrfStore().insert("cred", accountIdB64: x)
        KeychainAnonTokenStore().save(
            accountKey: x, context: .pushRegister,
            tokens: [AnonToken(context: AnonTokenContext.pushRegister.rawValue, epoch: 1, headerValue: "h")])
        KeychainReachPersistence().save(ReachRecord(), accountIdB64: x)
        // ADR-0074 part 3 — the account's own predecessor seed (key material
        // a pairing state bundle may have brought) with its adoption time,
        // and the unanswered outgoing contact requests with their intro.
        try KeychainOwnPredecessorStore().add(predecessorSeed(accountId), accountIdB64: x)
        PredecessorMailbox.AdoptionStore().recordIfAbsent(accountIdB64: x, keyB64: "old-key", nowMs: 1)
        try KeychainSentRequestsStore().save(
            [SentContactRequest(
                recipientUsername: "bob", recipientAccountIdB64: "peer", messageId: 1,
                createdAt: "2026-10-11T00:00:00Z", intro: "hello")],
            accountIdB64: x)
        ChannelPublisherKeyStore.save(Data([1]), accountIdB64: x)
        try ChannelPublishCounterStore.save(Data([1]), accountIdB64: x)
        ChannelSubscriberKeyStore.save(Data([1]), accountIdB64: x)

        // Files.
        IdentitySnapshotStore().putSnapshot(accountIdB64: x, blob: Data([1, 2]))
        MessageLogStore().putLog(accountIdB64: x, payload: MessageLogPayload(groups: [:]))
        ChannelLogStore().put(
            accountIdB64: x,
            payload: ChannelLogPayload(
                channels: [PersistedChannel(
                    id: channelHex, localName: "c", localDescription: "", addedAtMs: 1, handle: "h")],
                subscribedSeeds: []))
        DeviceGroupSnapshotStore().putSnapshot(accountIdB64: x, blob: Data([1, 2]))
        // ADR-0063 feed + ADR-0059 Discover (this branch's stores).
        ChannelPostLogStore().put(accountIdB64: x, payload: ChannelPostLogPayload(channels: [:]))
        // `docs/ux/channel-notifications.md` section 5 — the per-channel
        // "last notified post" cursors.
        ChannelNotifiedStore().save(accountIdB64: x, channelIdHex: channelHex, cursor: 7, keep: [channelHex])
        try FeedStore().save(accountIdB64: x, state: FeedState())
        try FeedReadingStore().save(accountIdB64: x, reading: FeedReading.decode(Data()))
        try DiscoverStore().save(accountIdB64: x, kind: .prefs, plaintext: Data([1]))
        try DiscoverStore().save(accountIdB64: x, kind: .shared, plaintext: Data([1]))
        SyncedStateStore.save(SyncedState(), accountIdB64: x)
        try CommitOutboxStore().putNewDurable(
            accountIdB64: x,
            entry: NewCommitEntry(id: "c1", groupId: group, targetEpoch: "1", commitB64: "AA=="))
        let calls = SealedCallHistoryBacking()
        calls.save(accountIdB64: x, data: Data("[]".utf8))
        calls.save(accountIdB64: CallHistoryStore.backfillMarkerKey(x), data: Data([1]))

        // UserDefaults, through the stores.
        // ADR-0073: the owed-publish record of a key reset, with its
        // attestation, and the phrase-owed marker.
        RecoveryMarkers().markPublishOwed(
            KeyResetPublishRecord(
                attestation: Data(repeating: 0xA7, count: 145), keyGeneration: 2, signedAtMs: 1,
                newSignaturePubkey: Data(repeating: 0x0A, count: 32)),
            accountIdB64: x)
        RecoveryMarkers().markPhraseOwed(accountIdB64: x)
        X25519BindingConfirmedMarks().markConfirmed(accountIdB64: x)
        SnapshotLeafKeyStore().save(accountIdB64: x, leafKeyB64: "leaf")
        KnownGroupsStore().add(accountIdB64: x, groupIdHex: group)
        GroupNameStore().record(
            accountIdB64: x, groupIdHex: group, value: StoredGroupName(name: "n", updatedAtMs: 1))
        SharedPinStore().record(
            accountIdB64: x, groupIdHex: group, messageId: 1, value: StoredSharedPin(pinned: true, updatedAtMs: 1))
        PersonalPinStore().set(accountIdB64: x, groupIdHex: group, messageId: 1, pinnedAtMs: 1)
        ProfileRequestStore().record(accountIdB64: x, groupIdHex: group, atMs: 1)
        DeviceReadmissionMemory.recordRemoved(
            accountIdB64: x, groupIdHex: group, before: ["l1"], after: [])
        PushRoutingStore().putRouting(
            accountIdB64: x, groupIdHex: group, secret: Data(repeating: 8, count: 32),
            pushGidHex: String(repeating: "ab", count: 32), distributed: true, mintEpoch: 1)
        RegistrarSeedStore().put(
            accountIdB64: x, groupIdHex: group, seed: Data(repeating: 9, count: 32), rank: .member)
        BeaconObservationStore().record(accountIdB64: x, groupIdB64: "g", senderKeyB64: "s", atMs: 1)
        PendingRequestStore().record(accountIdB64: x, groupIdB64: "g", key: "k", atMs: 1, own: true)
        LastObservedOwnerStore().remember(accountIdB64: x, groupIdB64: "g", ownerKeyB64: "o")
        LeaveRequestStore().remember(accountIdB64: x, groupIdB64: "g", atMs: 1)
        try ProfileDirectory.shared.setOwnDisplayName("Name", accountIdB64: x)
        ContactsInboxStore().save(PersistedContactsInbox.empty, accountIdB64: x)
        StateBundle.saveKnownGroupIds(["abc"], accountIdB64: x)
        CoPublishedJournals(defaults: standard, accountIdB64: { x }).mark(journalIdHex: "ab", accountIdB64: x)
        // ADR-0090 — an owed own-account hand-over and a queued device request.
        ChannelDeviceAdmissionStore(accountIdB64: x).markHandoverOwed(channelHex)
        ChannelDeviceAdmissionStore(accountIdB64: x).enqueue(
            [(plaintext: Data([1]), sender: Data(repeating: 2, count: 32))], channelId: channelHex)
        MediaPrefs(defaults: standard, accountIdB64: { x }).markPlayed("played", accountIdB64: x)
        EmojiIndex.saveRecent(["\u{1F600}"], standard, accountIdB64: x)
        EmojiIndex.saveTonePrefs(["\u{1F44B}": "\u{1F44B}\u{1F3FF}"], standard, accountIdB64: x)
        ChannelRotationStateStore.save(
            accountIdB64: x, channelId: channelHex, state: channelRotationStateAfterRotation(nowMs: 1))
        PendingSyncTracker(accountIdB64: x).markFailed()
        AutoBackupStore.save(accountIdB64: x, state: .clean)
        RestoredKnownGroupsStore.add(
            accountIdB64: x, groupIdsHex: [String(repeating: "ab", count: 32)])
        BackupRestoreRetryStore.mark(accountIdB64: x)
        RestoredDeviceStore.noteRestored(accountIdB64: x, knownConversations: 1)
        RestoredRejoinAnswerStore.record(.answered, accountIdB64: x)
        // The guest marker.
        GuestAccountMarker().mark(accountIdB64: x, nowMs: 1_760_000_000_000)
        GuestAccountMarker().noteUsernameClaimed(accountIdB64: x, username: "mira")

        // UserDefaults, raw: stores whose writers take a live model (a group
        // call record, a member identity, a contact), under the keys the
        // stores themselves read.
        standard.set(Data("{}".utf8), forKey: "group_calls.\(x)")
        standard.set([group: ["s": NSNumber(value: 1)]], forKey: "read_watermarks.\(x)")
        standard.set(["k": ["v": false]], forKey: "lakko.unapplied-requests.v1.\(x).g")
        standard.set(Data("[]".utf8), forKey: "lakko.contacts.accepted.v2.\(x)")
        standard.set(Data("{}".utf8), forKey: "me.lakko.contacts.pins.v2.\(x)")
        standard.set(Data("{}".utf8), forKey: "lakko.profiles.received.v1.\(x)")
        standard.set(3, forKey: "\(AccountLocalDataWipe.knockCursorPrefix)\(x).\(group)")

        // App Group suite.
        CallPeerNameMirror.mirror(accountIdB64: x, groupIdHex: group, name: "Peer")
        ConversationMuteMirror.replaceAll(accountIdB64: x, mutedGroupIdsHex: [group])
        SharedStorage.sharedDefaults()?.set(
            [x, group], forKey: AccountLocalDataWipe.legacyPushRoutePrefix + "gid-" + group)
    }

    /// A distinct account seed per account, so the seed-derived ids of two
    /// accounts differ.
    /// The account's own predecessor seed, distinct per account so two
    /// seeded accounts never share a lineage.
    private func predecessorSeed(_ accountId: Data) -> Data {
        Data(accountSeed(accountId).map { $0 ^ 0x0C })
    }

    private func accountSeed(_ accountId: Data) -> Data {
        let first = accountId.first ?? 0
        return Data((0..<AccountIdentitySeedStore.seedLength).map {
            UInt8(truncatingIfNeeded: Int(first) &+ $0)
        })
    }

    private func wipeQuietly(_ accountIdB64: String) {
        AccountLocalDataWipe.wipe(accountIdB64: accountIdB64, removeDeliveredNotifications: {})
    }
}
