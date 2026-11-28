// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import Security
import UserNotifications

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
/// device's push tokens, which the push unsubscribe forgets).
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
        /// ADR-0074 section 1.1 — the STAGED wipe a device runs when the
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
        // The account is sealed for the whole wipe (``AccountStoreGate``): a
        // store write already in flight finishes first and is then removed
        // with the rest; one that starts later is refused. Without it such a
        // write could land after its store's step and leave a forgotten
        // account's data on the device.
        // The staged wipe keeps the recall record, whose own later writes are
        // refused only after a full wipe.
        AccountStoreGate.seal(accountIdB64, wiping: true, staged: keeping.contains(.recallRecord)) {
            wipeSealed(
                accountIdB64: accountIdB64,
                keeping: keeping,
                removeDeliveredNotifications: removeDeliveredNotifications)
        }
    }

    private static func wipeSealed(
        accountIdB64: String,
        keeping: Set<Kept>,
        removeDeliveredNotifications: () -> Void
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
        let standard = UserDefaults.standard
        let shared = SharedStorage.sharedDefaults()
        // ADR-0078 section 6 — the DeviceGroup and PublisherGroup ids are
        // derived from the account seed (the conversation seed), not from any
        // id, so they are read here, before the steps below delete the seeds,
        // for the founding marks of steps 3 and 6.
        let lineageAccount = DeviceGroupLifecycle.lineageSeed(accountIdB64: accountIdB64, conversationKeys: nil)
            .flatMap { try? Identity.fromSeed(seed: $0) }
        let deviceGroupJournalHex = lineageAccount.map {
            DsClient.pseudonymHex(deviceGroupIdFor(accountIdentity: $0))
        }

        // 1. Push first: no background wake is handled for this account from
        //    here on, and nothing it received stays on the lock screen.
        step("push-binding") {
            let binding = PushAccountBindingStore.live
            if binding.current()?.accountIdB64 == accountIdB64 { binding.clear() }
        }
        step("delivered-notifications") { removeDeliveredNotifications() }
        step("push-routing") {
            // ADR-0078 — the subscription ids name this account's push-gw
            // rows; hand them to the unsubscribe that runs after the wipe
            // before the route rows (where they live) go.
            let store = PushRoutingStore()
            PendingPushUnsubscribes.stash(store.subscriptionIds(accountIdB64: accountIdB64))
            store.clear(accountIdB64: accountIdB64)
        }
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
        step("passkeys-no-prf") { PasskeyNoPrfStore().clear(accountIdB64: accountIdB64) }
        step("recovery-markers") { RecoveryMarkers().clear(accountIdB64: accountIdB64) }
        // ADR-0075 — the protection flags, the notice cursor and the "I did
        // this" markers.
        step("account-protection") { AccountProtectionStore().clear(accountIdB64: accountIdB64) }
        // ADR-0074 — the "account key recorded at auth" mark and the owed
        // device removals belong to the account's data on this device.
        step("identity-key-record") { IdentityKeyRecordMarks().clear(accountIdB64: accountIdB64) }
        // Security review 2026-11-21 — "the directory row carries the X25519
        // binding of these keys".
        step("x25519-binding-confirmed") { X25519BindingConfirmedMarks().clear(accountIdB64: accountIdB64) }
        step("device-removal-owed") { DeviceRemovalStore().clear(accountIdB64: accountIdB64) }
        // ADR-0074 part 3 — the owed key rotation, the pending rotation
        // envelopes of a kept device, the account's own predecessor seed and
        // the "keys were changed" line.
        step("key-rotation-owed") { KeyRotationStore().clear(accountIdB64: accountIdB64) }
        step("key-rotation-pending") { KeychainKeyRotationPendingStore().clear(accountIdB64: accountIdB64) }
        step("own-predecessor-seed") { KeychainOwnPredecessorStore().clear(accountIdB64: accountIdB64) }
        // G-56 — the recorded own conversation key goes with the seeds.
        step("own-conversation-key") { KeychainOwnConversationKeyStore().clear(accountIdB64: accountIdB64) }
        step("joined-device-group-id") { JoinedDeviceGroupIdStore().clear(accountIdB64: accountIdB64) }
        step("keys-rotated-notice") { KeysRotatedNoticeStore().clear(accountIdB64: accountIdB64) }
        step("profile-resend-owed") { ProfileResendOwed.Store().clear(accountIdB64: accountIdB64) }
        // ADR-0101 — reaching an account after a key reset: the reset's own
        // record, the kept continuity targets and mailbox cursor, the channel
        // returns (held, sent, shown), the recovery-key registrations, the
        // recovered channels, the history requests and the carried profiles.
        step("reset-continuity") {
            ResetContinuityStore(defaults: standard).clear(accountIdB64: accountIdB64)
            ContinuityTargetStore.clear(accountIdB64: accountIdB64, defaults: standard)
            ContinuityMailboxWindow.CursorStore(defaults: standard).clear(accountIdB64: accountIdB64)
            HeldChannelReturnStore(defaults: standard).clear(accountIdB64: accountIdB64)
            SentChannelReturnStore(defaults: standard).clear(accountIdB64: accountIdB64)
            ChannelRecoveryRegisteredStore(defaults: standard).clear(accountIdB64: accountIdB64)
            ReturnedInvitationStore.clearStored(accountIdB64: accountIdB64, defaults: standard)
            RecoveredChannelStore(defaults: standard).clearAll(accountIdB64: accountIdB64)
            HistoryReturnStore(defaults: standard).clear(accountIdB64: accountIdB64)
            ResendOriginLog(defaults: standard).clear(accountIdB64: accountIdB64)
            HistoryServedStore(defaults: standard).clear(accountIdB64: accountIdB64)
            ProfileCarriedStore(defaults: standard).clear(accountIdB64: accountIdB64)
        }
        step("predecessor-inbox-cursor") {
            PredecessorMailbox.CursorStore().clear(accountIdB64: accountIdB64)
            PredecessorMailbox.AdoptionStore().clear(accountIdB64: accountIdB64)
        }
        // ADR-0074 section 1.1 — the recall record: the one step the staged
        // wipe skips.
        if !keeping.contains(.recallRecord) {
            step(recallRecordStepName) {
                KeychainRecallRecordStore().clear(accountIdB64: accountIdB64)
            }
        }
        // A guest account's phrase is kept under a key of its own
        // (``guestPhraseKey(accountIdB64:)``), so it has a step of its own;
        // then the guest marker.
        step("guest-recovery-phrase") {
            try PhraseStore().deletePhrase(accountIdB64: guestPhraseKey(accountIdB64: accountIdB64))
        }
        step("guest-account") { GuestAccountMarker().clear(accountIdB64: accountIdB64) }
        step("backup-floor") { BackupFloorStore.deleteFloor(accountIdB64: accountIdB64) }
        step("auto-backup") { AutoBackupStore.clear(accountIdB64: accountIdB64) }
        // The conversations a restored backup named, and the mark of a
        // backup restore that is still owed.
        step("backup-restored-groups") {
            RestoredKnownGroupsStore.clear(accountIdB64: accountIdB64)
        }
        step("backup-restore-retry") {
            BackupRestoreRetryStore.clear(accountIdB64: accountIdB64)
        }
        step("backup-restored-device") {
            RestoredDeviceStore.clear(accountIdB64: accountIdB64)
        }
        step("devicegroup-rejoin-answer") {
            RestoredRejoinAnswerStore.clear(accountIdB64: accountIdB64)
        }
        step("channel-pending-handles") {
            PendingChannelHandleStore.clear(accountIdB64: accountIdB64)
        }
        step("group-pending-handles") {
            PendingGroupHandleStore.clear(accountIdB64: accountIdB64)
        }
        step("export-key") {
            ExportKeyStore.deleteExportKey(accountIdB64: accountIdB64)
            Task { @MainActor in RunExportKey.clear(accountIdB64: accountIdB64) }
        }
        // ADR-0101 section 7 — the server half of the owned-channels password
        // lock, when the locks were never made on this device.
        step("owned-channels-secret") {
            OwnedChannelsPendingSecret.delete(accountIdB64: accountIdB64)
            OwnedChannelsResetCarryStore.delete(accountIdB64: accountIdB64)
            OwnedChannelsOwed.deleteAll(accountIdB64: accountIdB64)
        }
        step("account-identity-seed") {
            AccountIdentitySeedStore.deleteAccountIdentitySeed(accountIdB64: accountIdB64)
        }
        step("device-identity-seed") {
            DeviceIdentityStore.deleteDeviceIdentitySeed(accountIdB64: accountIdB64)
        }
        step("invite-link-secrets") {
            InviteLinkSecretStore.shared.clear(accountIdB64: accountIdB64)
        }
        step("own-links") {
            // ADR-0078 section 10: this device's records of the links it
            // minted, with their revocation seeds.
            OwnLinkStore.shared.clear(accountIdB64: accountIdB64)
        }
        step("owed-link-row-deletions") {
            // ADR-0079 section 4: the ledger rows this device revoked and
            // still owes a deletion of.
            OwedLinkRowDeletionStore.clear(accountIdB64: accountIdB64)
        }
        step("anon-tokens") {
            // This account's stored tokens; any other account's stay. The
            // pool stops writing for this account too, so the token the
            // session's push unregister spends cannot put the rows back.
            AnonTokenPool.shared.forget(accountKey: accountIdB64)
        }
        step("reach") { ReachCenter.shared.forget(accountIdB64: accountIdB64) }
        step("copublisher-keys") { CoPublisherKeyBook.clear(accountIdB64: accountIdB64) }
        // ADR-0090 — owed own-account hand-overs and queued device requests.
        step("channel-device-admission") { ChannelDeviceAdmissionStore.clear(accountIdB64: accountIdB64) }
        step("publisher-links") { PublisherLinkApproval.clear(accountIdB64: accountIdB64) }

        // 3. The DeviceGroup and the cross-device synced state. The founding
        //    mark of the account's DeviceGroup journal goes too; the id is
        //    derived from the account seed (read at the start).
        step("device-group-snapshot") {
            DeviceGroupSnapshotStore().deleteSnapshot(accountIdB64: accountIdB64)
        }
        step("synced-state") { SyncedStateStore.delete(accountIdB64: accountIdB64) }
        step("synced-state-pending") {
            PendingSyncTracker(accountIdB64: accountIdB64).clear()
        }
        step("device-group-founding-mark") {
            guard let deviceGroupJournalHex else { return }
            JournalFoundingMarks.shared.clear(journalIdHex: deviceGroupJournalHex)
        }

        // 4. Chats.
        step("mls-snapshot") { IdentitySnapshotStore().deleteSnapshot(accountIdB64: accountIdB64) }
        step("snapshot-leaf-key") { SnapshotLeafKeyStore().clear(accountIdB64: accountIdB64) }
        step("known-groups") { KnownGroupsStore().clear(accountIdB64: accountIdB64) }
        step("group-names") { GroupNameStore().clear(accountIdB64: accountIdB64) }
        // ADR-0087 — group photos, allowed reactions, @names, knock seeds,
        // the names this account holds, unseen reactions and recent ones.
        step("group-settings") { GroupSettingsStore().clear(accountIdB64: accountIdB64) }
        step("held-handles") { standard.removeObject(forKey: HeldHandleStore.key(accountIdB64)) }
        step("unseen-reactions") { UnseenReactionsStore().clear(accountIdB64: accountIdB64) }
        step("recent-reactions") { standard.removeObject(forKey: RecentReactions.key(accountIdB64)) }
        step("knock-versions") { standard.removeObject(forKey: KnockVersionStore.key(accountIdB64)) }
        // ADR-0091 phase 2 — the behind records, the re-admitter's memory,
        // the members' knock keys and the owed gap histories.
        step("conversation-reentry") {
            standard.removeObject(forKey: ConversationReentryStore.key(accountIdB64))
            standard.removeObject(forKey: ReentryAdmissionMemory.key(accountIdB64))
            standard.removeObject(forKey: ReentryKnockKeyStore.key(accountIdB64))
            standard.removeObject(forKey: ReentryHistoryOwed.key(accountIdB64))
            // When this device last knew each conversation, and the knock
            // keys it delivered (walk-2 rules R1 and R4).
            standard.removeObject(forKey: ReentryRowClockStore.key(accountIdB64))
            standard.removeObject(forKey: ReentryKnockKeySentStore.key(accountIdB64))
            // When this device first saw each leaf (the gap history's floor).
            standard.removeObject(forKey: ReentryLeafFirstSeenStore.key(accountIdB64))
            // Base scenario 9 — when this device first held each conversation.
            ConversationHeldSince.clear(accountIdB64: accountIdB64)
            // The gap histories still looked for, and their wrap key.
            ReentryGapLookStore.wipe(accountIdB64: accountIdB64, defaults: standard)
        }
        step("requests-sent") { standard.removeObject(forKey: RequestsSentStore.key(accountIdB64)) }
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
        step("membership-ends") { MembershipEndStore().clear(accountIdB64: accountIdB64) }
        step("group-arrivals") { GroupArrivalStore().clearForAccount(accountIdB64: accountIdB64) }
        step("knock-cursors") {
            removeKeys(in: standard, withPrefix: knockCursorPrefix + accountIdB64 + ".")
        }
        // `docs/ux/group-and-channel-privacy.md` section 8.1 — the announced
        // and unshown request-to-join sets.
        step("join-request-signals") {
            JoinRequestSignalStore(defaults: standard).clear(accountIdB64: accountIdB64)
        }
        step("message-log") { MessageLogStore().deleteLog(accountIdB64: accountIdB64) }
        // ADR-0099 — the evidence kept for reports, and the reports this
        // device sent (device-only, never synced, never in the backup).
        step("message-evidence") { MessageEvidenceStore.shared.deleteAll(accountIdB64: accountIdB64) }
        step("sent-reports") { ReportHistoryStore.shared.deleteAll(accountIdB64: accountIdB64) }
        step("blocked-sender-mirror") { BlockedSenderMirror.clear(accountIdB64: accountIdB64) }
        // ADR-0099 step 4 — the in-memory suspension marks (nothing of them
        // is on disk).
        step("suspension-marks") { SuspensionMarks.shared.reset() }
        step("channel-removals") { ChannelRemovals.shared.reset() }
        // ADR-0092 — the channel-poll credentials, the article drafts, the
        // queued ballots and who admitted this device to each group.
        step("content-kinds") {
            removeKeys(in: standard, withPrefix: "lakko.anon-credential.v1." + accountIdB64 + ".")
            removeKeys(in: standard, withPrefix: ArticleDraftStore.prefix + accountIdB64 + ".")
            standard.removeObject(forKey: BallotQueue.storeKey(accountIdB64))
            standard.removeObject(forKey: ContentAdmitters.key(accountIdB64))
        }
        // ADR-0082 section 5 — the translations, transcripts and
        // per-conversation translation choices, and the audio extracted for a
        // transcription still under way.
        step("message-translations") {
            try MessageTranslationStore.shared.clear(accountIdB64: accountIdB64)
            if MessageTranslationPurge.boundAccountIdB64 == accountIdB64 {
                MessageTranslationCenter.shared.forgetAccount()
            }
            try removeIfPresent(AudioExtract.directory)
        }

        // 5. Contacts and profiles.
        step("verified-bindings") { VerifiedBindingStore.clear(ownerAccountIdB64: accountIdB64) }
        // ADR-0073 R.5-R.7 — the successor-claim state, the continuity
        // bookkeeping, the held messages and the unsent-lineage marker.
        step("lineage-mirror") { ConversationLineageMirror.clear(accountIdB64: accountIdB64) }
        step("continuity-mirror") { ContinuityMirror.clear(accountIdB64: accountIdB64) }
        step("group-closed") { GroupClosedStore.clear(ownerAccountIdB64: accountIdB64) }
        step("continuity-store") { ContinuityStore.clear(ownerAccountIdB64: accountIdB64) }
        step("held-sends") { HeldSendQueue.clearAccount(ownerAccountId: accountIdB64) }
        // `docs/ux/connection-state.md` section 7.3 — the outbox and its
        // prepared files.
        step("send-outbox") { SendOutbox.clearAccount(ownerAccountId: accountIdB64) }
        step("pending-display-copies") { PendingDisplayCopies.clearAccount(ownerAccountIdB64: accountIdB64) }
        step("lineage-unsent") {
            standard.removeObject(
                forKey: DeviceGroupSyncRepository.lineageUnsentKey(accountIdB64: accountIdB64))
        }
        step("own-username-unsent") {
            standard.removeObject(
                forKey: DeviceGroupSyncRepository.ownUsernameUnsentKey(accountIdB64: accountIdB64))
        }
        step("recovery-email-unsent") {
            standard.removeObject(
                forKey: DeviceGroupSyncRepository.recoveryEmailUnsentKey(accountIdB64: accountIdB64))
        }
        // ADR-0079 — the unsent link-record marker and the people added
        // through their invite link (their queued messages are in the
        // held-send queue, cleared above).
        step("link-records-unsent") {
            standard.removeObject(
                forKey: DeviceGroupSyncRepository.linkRecordsUnsentKey(accountIdB64: accountIdB64))
        }
        // ADR-0050 section 2.9 (amended 2026-11-13) — the unsent channel
        // subscription marker and the channel intents not folded yet.
        step("channel-subscriptions-unsent") {
            standard.removeObject(
                forKey: DeviceGroupSyncRepository.channelSubscriptionsUnsentKey(accountIdB64: accountIdB64))
            ChannelSubscriptionSync.OwedStore.clear(accountIdB64: accountIdB64, defaults: standard)
        }
        step("pending-contacts") { PendingContactStore.clear(accountIdB64: accountIdB64) }
        // ADR-0079 (reworked 2026-10-17) — the link requests not decided yet
        // (sealed, they carry link secrets) and the claims of link requests.
        step("link-requests") { LinkRequestInbox.clear(accountIdB64: accountIdB64) }
        step("link-claims") { LinkClaims.Record.clear(accountIdB64: accountIdB64) }
        step("profiles") { ProfileDirectory.shared.forget(accountIdB64: accountIdB64) }
        // ADR-0098 — who this account learned was deleted.
        step("deleted-accounts") { DeletedAccountStore.clear(accountIdB64: accountIdB64) }
        // ADR-0084 — the decrypted avatars this device cached (and the
        // prepared ones waiting for Save).
        // ADR-0065 — the owed blob deletes and the keep schedule; the passes.
        step("blob-lifecycle") {
            BlobLifecycleStore().clear(accountIdB64: accountIdB64)
            Task { await BlobAccess.shared.clear() }
        }
        step("avatars") {
            AvatarImages.shared.purge()
            try? FileManager.default.removeItem(at: AvatarMediaFiles.directory)
        }
        step("contacts-inbox") { ContactsInboxStore().clear(accountIdB64: accountIdB64) }
        // ADR-0073 R.3 — the requests kept waiting for an identity verdict.
        step("contacts-inbox-waiting") { KnownRequestWaitingStore().clear(accountIdB64: accountIdB64) }
        // ADR-0074 section 3.5a rule 5 — the unanswered outgoing requests
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
            guard !channelIds.isEmpty, let lineageAccount else { return }
            // ADR-0078 section 6: an owned channel's PublisherGroup id is the
            // owner's derivation over the channel id; nothing inverts it.
            let owned = Set(channelIds.compactMap { hex -> String? in
                guard let channelId = CoPublisher.hexToData(hex),
                      let groupId = try? publisherGroupIdFor(
                        ownerIdentity: lineageAccount, channelId: channelId)
                else { return nil }
                return DsClient.pseudonymHex(groupId)
            })
            let marks = JournalFoundingMarks.shared
            for journalIdHex in marks.all() where owned.contains(journalIdHex) {
                marks.clear(journalIdHex: journalIdHex)
            }
        }
        step("channels") { ChannelsUiState.forget(accountIdB64: accountIdB64) }
        step("co-published-journals") {
            CoPublishedJournals.shared.clear(accountIdB64: accountIdB64)
        }
        // G-50 — the channel provider (the PublisherGroups, the keys of an
        // unanswered co-publisher request) and the recorded PublisherGroup
        // commits.
        step("channel-provider") {
            if ChannelPublisherIdentity.shared.boundAccountId == accountIdB64 {
                ChannelPublisherIdentity.shared.unbind()
            }
            ChannelPublisherIdentity.deleteOnDeviceSnapshot(accountIdB64: accountIdB64)
        }
        step("publisher-commits") { PublisherCommitStore.deleteAll(accountIdB64: accountIdB64) }
        // ADR-0063 — the decrypted channel posts the feed reads.
        step("channel-post-log") { ChannelPostLogStore().delete(accountIdB64: accountIdB64) }
        // `docs/ux/channel-notifications.md` section 5 — the per-channel
        // "last notified post" cursors (the delivered notifications go with
        // the `delivered-notifications` step).
        step("channel-notified") {
            ChannelNotifiedStore(defaults: standard).clear(accountIdB64: accountIdB64)
        }
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
        // feeds): a device-wide key, as on Android.
        step("feed-prefs") {
            standard.removeObject(forKey: FeedViewModel.selectionKey)
        }

        // 7. Calls and media.
        step("call-history") { CallHistoryStore.shared.forget(accountIdB64: accountIdB64) }
        step("call-peer-names") { CallPeerNameMirror.clear(accountIdB64: accountIdB64) }
        step("person-names") { PersonNameMirror.clear(accountIdB64: accountIdB64) }
        step("conversation-mutes") { ConversationMuteMirror.clear(accountIdB64: accountIdB64) }
        step("media-played") { MediaPrefs().clearPlayed(accountIdB64: accountIdB64) }
        // ADR-0089 — the camera background chosen on this device and the own
        // photos behind it.
        step("call-backgrounds") { try CameraBackground.delete(accountIdB64: accountIdB64) }
        step("emoji-recents-and-tones") { EmojiIndex.clear(accountIdB64: accountIdB64, standard) }

        // 8. Device-wide keys and files holding this account's data.
        step("selected-folder-tab") { standard.removeObject(forKey: selectedFolderTabKey) }
        step("device-link-answers") {
            standard.removeObject(forKey: DeviceLinkAnswerLimit.defaultKey)
        }
        step("new-device-notice") {
            NewDeviceNoticeStore(defaults: standard).clear(accountIdB64: accountIdB64)
            standard.removeObject(
                forKey: DeviceGroupSyncRepository.deviceAcksUnsentKey(accountIdB64: accountIdB64))
            let ids = [NewDeviceNoticeCenter.notificationIdentifier(accountIdB64: accountIdB64)]
            UNUserNotificationCenter.current().removePendingNotificationRequests(withIdentifiers: ids)
            UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: ids)
        }
        // ADR-0080 — the cards of the account's devices, this device's own
        // card, its city answer and its "Show my city" switch.
        step("device-cards") {
            standard.removeObject(forKey: DeviceCards.cardsKey(accountIdB64: accountIdB64))
            standard.removeObject(forKey: DeviceCards.cityKey(accountIdB64: accountIdB64))
            standard.removeObject(forKey: DeviceCards.showCityKey(accountIdB64: accountIdB64))
            standard.removeObject(forKey: DeviceCards.resendOwedKey(accountIdB64: accountIdB64))
        }
        step("decrypted-media") {
            MediaSessionCache.shared.purge()
            // `docs/ux/media-messages.md` section 11 — the composer trays.
            ComposerTrays.shared.removeAll()
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
