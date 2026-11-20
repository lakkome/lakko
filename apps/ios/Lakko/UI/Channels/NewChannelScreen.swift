// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI

/// `docs/ux/group-and-channel-privacy.md` section 3.2 — New channel: the
/// photo, `Channel name` (required), `Description`, the `@name` (proposed from
/// the name until edited), `Who can subscribe`, and while public `Topic` and
/// `Language`. The states are section 3.3's.
struct NewChannelScreen: View {
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data
    var sessionTokenProvider: () -> String? = { nil }
    /// The channel exists: its conversation opens (section 3.3, "Created").
    var onCreated: (String) -> Void = { _ in }
    @Environment(\.dismiss) private var dismiss

    @State private var name: String = ""
    @State private var description: String = ""
    @State private var handle: String = ""
    @State private var handleEdited = false
    @State private var handleRandom = UInt64.random(in: 0...UInt64.max)
    @State private var isPublic = false
    @State private var topic: UInt8 = 0
    @State private var language: String = DiscoverLanguages.appLanguage()
    @State private var photo: PendingAvatarMedia?
    @State private var photoError: String?
    @State private var busy = false
    @State private var errorKey: String?
    @State private var handleTaken = false
    @State private var handleRateLimited = false
    @State private var takenCheck: Task<Void, Never>?
    @ObservedObject private var l10n = Localization.shared

    private let topics = channelTopics()

    private var trimmedName: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }

    private var canCreate: Bool {
        !busy && !trimmedName.isEmpty && ConversationHandles.wellFormed(handle) && !(isPublic && handleTaken)
    }

    private func proposedHandle() -> String {
        suggestHandle(name: trimmedName, kind: .channel, private: !isPublic, random: handleRandom)
    }

    var body: some View {
        List {
            Section {
                NewGroupPhotoSlot(name: trimmedName, media: $photo, disabled: busy)
                    .listRowBackground(Color.clear)
                if let photoError {
                    Text(l10n.t(photoError))
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoDanger)
                        .frame(maxWidth: .infinity)
                        .listRowBackground(Color.clear)
                }
            }
            Section {
                TextField(l10n.t("channels.new.name_label"), text: $name)
                TextField(l10n.t("channels.new.description_label"), text: $description, axis: .vertical)
                    .lineLimit(2...4)
            } footer: {
                Text(l10n.t("channels.new.subtitle"))
            }
            Section {
                HStack(spacing: 2) {
                    Text("@").foregroundStyle(Color.lakkoTextMuted)
                    TextField(l10n.t("group_type.handle"), text: Binding(
                        get: { handle },
                        set: { handle = ConversationHandles.canonical($0); handleEdited = true }))
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .environment(\.layoutDirection, .leftToRight)
                }
                LakkoPicker(
                    l10n.t("channel_type.who_can_subscribe"), icon: .person2,
                    selection: $isPublic,
                    options: [
                        LakkoPickerOption(value: false, label: l10n.t("group_type.private")),
                        LakkoPickerOption(value: true, label: l10n.t("group_type.public")),
                    ])
                if isPublic {
                    LakkoPicker(
                        l10n.t("discover.listing.topic"), selection: $topic,
                        options: topics.map { LakkoPickerOption(value: $0.id, label: DiscoverTopics.name(Int($0.id))) })
                    LakkoPicker(
                        l10n.t("discover.language"), selection: $language,
                        options: DiscoverLanguages.options(language).map {
                            LakkoPickerOption(value: $0, label: DiscoverLanguages.label($0))
                        })
                }
            } footer: {
                VStack(alignment: .leading, spacing: 4) {
                    if !handle.isEmpty && !ConversationHandles.wellFormed(handle) {
                        Text(l10n.t("group_handle.error.bad_form")).foregroundStyle(Color.lakkoDanger)
                    } else if isPublic && handleTaken {
                        Text(l10n.t("group_handle.error.taken")).foregroundStyle(Color.lakkoDanger)
                    } else if isPublic && handleRateLimited {
                        Text(l10n.t("common.error.too_many_attempts")).foregroundStyle(Color.lakkoTextMuted)
                    } else {
                        Text(l10n.t(isPublic ? "channel_type.handle_public" : "channel_type.handle_private"))
                    }
                    Text(l10n.t(isPublic ? "channel_type.public_desc" : "channel_type.private_desc"))
                }
            }
            if let errorKey {
                Section {
                    Text(l10n.t(errorKey)).font(.footnote).foregroundStyle(Color.lakkoDanger)
                }
            }
        }
        .listStyle(.insetGrouped)
        .disabled(busy)
        .navigationTitle(l10n.t("channels.new.title"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button(l10n.t(busy ? "chats.new_group.creating" : "channels.new.create")) {
                    Task { await create() }
                }
                .disabled(!canCreate)
            }
        }
        .onAppear { if handle.isEmpty { handle = proposedHandle() } }
        .onChange(of: name) { _, _ in if !handleEdited { handle = proposedHandle() } }
        .onChange(of: isPublic) { _, _ in
            if !handleEdited { handle = proposedHandle() }
            scheduleTakenCheck()
        }
        .onChange(of: handle) { _, _ in scheduleTakenCheck() }
        .onChange(of: photo) { _, _ in photoError = nil }
    }

    /// Section 3.3 — a public name is looked up 500 ms after typing stops;
    /// a private one is never (whether it is free is known only at Create).
    private func scheduleTakenCheck() {
        takenCheck?.cancel()
        handleTaken = false
        handleRateLimited = false
        let typed = ConversationHandles.canonical(handle)
        guard isPublic, typed.count >= 3, ConversationHandles.wellFormed(typed) else { return }
        // A name a failed create of this account claimed is this account's
        // (a retry takes it back): the lookup answering it is not "taken".
        if claimedHere(typed) { return }
        if let held = HandleTakenCheck.held(typed) {
            handleTaken = held == .taken
            return
        }
        takenCheck = Task { @MainActor in
            try? await Task.sleep(nanoseconds: HandleTakenCheck.debounceNanoseconds)
            guard !Task.isCancelled, typed == ConversationHandles.canonical(handle) else { return }
            let answer = await HandleTakenCheck.check(typed)
            guard !Task.isCancelled, typed == ConversationHandles.canonical(handle) else { return }
            handleTaken = answer == .taken
            handleRateLimited = answer == .rateLimited
        }
    }

    /// Whether a failed create on this account claimed `typed` (its channel
    /// id is kept for the retry).
    private func claimedHere(_ typed: String) -> Bool {
        guard let account = AcceptedContactsStore.boundAccountIdB64 else { return false }
        return PendingChannelHandleStore.channelId(forHandle: typed, accountIdB64: account) != nil
    }

    private func deleteUploaded(_ avatar: ProfileAvatar?) {
        guard let avatar else { return }
        BlobLifecycle.shared.delete([avatar.still.blobId] + (avatar.video.map { [$0.blobId] } ?? []))
    }

    private func create() async {
        guard canCreate else { return }
        let wanted = ConversationHandles.canonical(handle)
        if isPublic, HandleTakenCheck.held(wanted) == .taken, !claimedHere(wanted) {
            handleTaken = true
            return
        }
        guard sessionTokenProvider() != nil, let accountIdB64 = AcceptedContactsStore.boundAccountIdB64 else {
            errorKey = "channels.new.error.sign_in"
            return
        }
        busy = true
        defer { busy = false }
        errorKey = nil
        // Section 3.3 / 4.5.3 — the photo goes up before anything is created.
        var avatar: ProfileAvatar?
        if let photo {
            do {
                avatar = try await ProfileAvatarUpload.upload(photo)
            } catch AttachmentBridgeError.uploadLimited {
                photoError = "group_photo.error.limit"
                return
            } catch AttachmentBridgeError.storageFull {
                photoError = "group_photo.error.full"
                return
            } catch {
                photoError = "group_photo.error.upload"
                return
            }
        }
        // A create that failed after claiming this name: the same channel id,
        // hence the same write key, which the claim answers as the first time.
        var pendingId = PendingChannelHandleStore.channelId(forHandle: wanted, accountIdB64: accountIdB64)
        // Security pass 2026-11-15 (F5) — a kept id whose create finished is
        // never founded over (another owner device may hold its group): it is
        // forgotten and this create takes a fresh id.
        if let kept = pendingId, !PendingChannelHandleStore.keptIdUsable(
            listedHere: state.channels.contains { $0.id.lowercased() == kept.lowercased() },
            inRegister: ChannelSubscriptionSync.shared.registeredKeys(channelId: kept) != nil) {
            PendingChannelHandleStore.forget(handle: wanted, accountIdB64: accountIdB64)
            pendingId = nil
        }
        let id = pendingId ?? ChannelsComposer.freshChannelIdHex()
        let channelName = trimmedName
        let channelDescription = description.trimmingCharacters(in: .whitespacesAndNewlines)
        // 1. The `@name`, before the channel exists: a held name aborts with
        //    nothing created (section 3.3, "The name is held").
        let written: ConversationHandles.Written
        let journal: Data
        let facts: ConversationHandles.PublicFacts
        do {
            let owner = try Identity.fromSeed(seed: accountIdentitySeed)
            let lineage = ChannelLineage.identity(accountIdB64: accountIdB64, currentSeed: accountIdentitySeed) ?? owner
            (journal, facts) = try ConversationHandles.channelFacts(
                lineageIdentity: lineage, channelId: try ChannelsComposer.channelIdFromHex(id),
                name: channelName, description: channelDescription, avatar: avatar?.ffi)
            PendingChannelHandleStore.keep(handle: wanted, channelIdHex: id, accountIdB64: accountIdB64)
            // A retry under the kept id is the same key: the claim answers it
            // as the first one did.
            written = try await ConversationHandles.claim(
                discovery: DiscoveryClient(), accountSeed: owner.ed25519Seed(), journalId: journal,
                handle: wanted, isPublic: isPublic, facts: isPublic ? facts : nil)
        } catch let refusal as DiscoveryClient.HandleWriteError where refusal.code == "name-taken" {
            HandleTakenCheck.note(wanted, .taken)
            PendingChannelHandleStore.forget(handle: wanted, accountIdB64: accountIdB64)
            errorKey = "group_handle.error.held"
            deleteUploaded(avatar)
            return
        } catch let token as AnonTokenUnavailable {
            errorKey = AnonTokenUnavailable.isQuotaExhausted(token) ? "channels.new.too_many" : "group_handle.error.offline"
            deleteUploaded(avatar)
            return
        } catch {
            errorKey = ChannelSettings.errorKey(error)
            deleteUploaded(avatar)
            return
        }
        // 2. The channel, its PublisherGroup and journal.
        do {
            let owner = try Identity.fromSeed(seed: accountIdentitySeed)
            _ = try await client.createChannel(channelIdHex: id, accountIdentity: owner, isPrivate: !isPublic)
            // Security pass 2026-11-15 (F1) — a private channel gets its first
            // admission seed now and marks no row: no genuine subscriber
            // exists yet, so any row the service lists was planted there.
            if !isPublic { ChannelInvites.mintAtCreation(channelIdHex: id) }
            let identity = try ChannelPublisherIdentity.shared.identity(currentSeed: accountIdentitySeed)
            // New channel is the one place a PublisherGroup is founded.
            let keys = try ChannelsComposer.freshPublisherKeys(
                identity: identity, accountIdB64: accountIdB64, channelIdHex: id,
                existingSigningSeed: state.persistedPublisherSigningSeed(channelId: id),
                mayFound: true)
            let publisherGroup = try CoPublisher.foundPublisherGroup(
                identity: identity, accountIdB64: accountIdB64, channelIdHex: id)
            try ChannelPublisherIdentity.shared.persistOrThrow()
            let signer = try publisherGroup.dsAccessSigner(identity: identity)
            let groupId = try publisherGroup.groupId()
            try await JournalOwnership.claimOrReset(
                root: try CoPublisher.rootSigner(identity: identity, publisherGroup: publisherGroup),
                journalId: groupId, epoch: signer.epoch(), epochKey: signer.verifyingKey(), ds: DsClient())
            JournalFoundingMarks.shared.clear(journalIdHex: DsClient.pseudonymHex(groupId))
            // ADR-0064 section 7 (2026-11-14) — a new channel gets its vault
            // head now, so an owner device added later can enter it alone.
            await ChannelVaultHead.writeCurrent(channelIdHex: id, group: publisherGroup, identity: identity)
            state.addChannel(.init(
                id: id, localName: channelName, localDescription: channelDescription,
                addedAtMs: UInt64(Date().timeIntervalSince1970 * 1000), handle: written.handle))
            state.setKeys(channelId: id, keys: keys)
            // ADR-0050 section 2.9 (amended 2026-11-13) — the account's other
            // devices list the channel as its owner's (readers there).
            state.onRelationshipStarted?(id)
            PendingChannelHandleStore.forget(handle: wanted, accountIdB64: accountIdB64)
            if let generation = state.currentKeyGeneration {
                state.recordOwnerKeyGeneration(channelId: id, generation: generation)
            }
            HeldHandleStore.put(HeldHandleStore.Entry(
                kind: "channel", handle: written.handle, isPublic: isPublic,
                journalIdHex: AttachmentBridge.hex(journal), channelIdHex: id,
                name: channelName, description: channelDescription,
                leaseUntilMs: written.leaseUntilMs), accountIdB64)
            HandleTakenCheck.note(written.handle, .taken)
            state.updateExtras(id) {
                $0.avatar = avatar
                if isPublic { $0.readKey = facts.key }
            }
            // 3. The metadata record with the signed name, the flag, the
            //    photo, so a subscriber's first read titles the channel.
            var handleSeed = try ConversationHandles.seed(accountSeed: owner.ed25519Seed(), journalId: journal)
            defer { RootHandover.wipe(&handleSeed) }
            let now = UInt64(Date().timeIntervalSince1970 * 1000)
            let value = try signConversationHandle(
                handleSeed: handleSeed, groupId: journal, updatedAtMs: now, public: isPublic,
                handle: written.handle, certificate: written.certificate)
            let meta = SharedChannelMeta(
                name: channelName, description: channelDescription, updatedAtMs: now,
                topic: isPublic ? topic : 0, language: isPublic ? language : "", listed: isPublic,
                handle: value, avatar: avatar?.ffi, reactions: nil)
            // Kept as this device's record and owed BEFORE it is sent: a
            // failed post leaves the owner's device holding the record it
            // meant to send, and every publisher pass re-issues THAT record
            // (``ChannelPublisherLoops/reissueOwedRecord``), flag and signed
            // name included, until it lands.
            ChannelMetaRecord.keepFirst(state: state, channelId: id, meta: meta)
            do {
                try await ChannelMetaRecord.post(
                    state: state, client: client, channelId: id, meta: meta, accountIdentity: owner)
                state.updateExtras(id) { $0.recordOwed = nil }
            } catch {
                NSLog("[channels] the first metadata record did not post; the publisher pass re-issues it")
            }
            if isPublic {
                try? await ChannelListing.list(
                    state: state, client: client, channelIdHex: id, identity: owner,
                    topic: topic, language: language)
            }
            ChannelPublisherLoops.shared.markOpened(id)
            onCreated(id)
        } catch let err as LegacyChannelError {
            NSLog("[channels] create refused: %@", err.message)
            errorKey = "channels.new.failed"
        } catch {
            NSLog("[channels] create failed: %@", String(describing: error))
            errorKey = "channels.new.failed"
        }
    }
}
