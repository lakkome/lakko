// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// Groups > New group (docs/ux/new-group.md): a name field above the
/// accepted contacts, one checkbox per contact, and Create.
///
/// A name with nobody picked founds a group with nobody else in it; people
/// are added from its info as in any group. With people picked the name is
/// optional (ADR-0051 §3: sent into the group right after the
/// creating commit, so the first Welcome recipients get it with their first
/// poll; empty means the title is built from the members). The group is
/// created with the first picked contact through the same
/// `startGroupWith` path a contact accept uses (one Welcome, this device the
/// owner per ADR-0022), then the remaining picks are added one after another
/// through the chat's normal add path with all of its obligations (ADR-0040
/// durable commit record, ADR-0045 proposal + owner sweep, echo suppression,
/// push-routing redistribution). A later add that fails does not undo the
/// group: the chat opens with the members that did land and says who is
/// missing.
struct NewGroupScreen: View {
    @ObservedObject var contacts: ContactsViewModel
    let chatRepository: ChatRepository
    /// Called with the new group's hex id and, when some adds failed, the
    /// notice the chat should show (rendered in the language of the moment
    /// it is on screen, ADR-0061).
    let onCreated: (_ groupIdHex: String, _ notice: UiText?) -> Void

    @State private var name: String = ""
    /// ADR-0087 section 2.1 — the proposed `@name`, following the name field
    /// until the person edits it.
    @State private var handle: String = ""
    @State private var handleEdited = false
    @State private var isPublic = false
    /// UX 4.5.3 — the photo chosen on the form, uploaded at Create.
    @State private var photo: PendingAvatarMedia?
    /// Under the photo: why Create stopped (upload failed / limit).
    @State private var photoError: String?
    /// UX 3.3 — "That @name is taken." from the live lookup (public only).
    @State private var handleTaken = false
    @State private var takenCheck: Task<Void, Never>?
    /// One random per form, so the suffix does not change on every keystroke.
    @State private var handleRandom: UInt64 = UInt64.random(in: 0...UInt64.max)
    @State private var selected: Set<Data> = []
    @State private var busy = false
    /// The catalog key of the founding failure, rendered when shown.
    @State private var errorMessage: String?
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    /// UX spec §2 — one line, at most 64 characters.
    private static let maxNameLength = 64

    private var accepted: [AcceptedContact] { contacts.repositoryState.accepted }

    /// The picks in list order — the first one founds the group.
    private var picks: [AcceptedContact] {
        accepted.filter { selected.contains($0.request.fromPubkey) }
    }

    private var trimmedName: String {
        name.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// What Create needs (UX 3.1): a well-formed `@name` and, when public,
    /// a name. Nobody has to be picked (a group of one, section 10).
    static func canCreate(name: String, handle: String, isPublic: Bool) -> Bool {
        ConversationHandles.wellFormed(handle)
            && (!isPublic || !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
    }

    /// UX 3.3 — public only: 500 ms after typing stops, from 3 characters,
    /// a lookup that answers means "taken". A private name is checked only at
    /// Create (a lookup sees public names only).
    private func scheduleTakenCheck() {
        takenCheck?.cancel()
        handleTaken = false
        guard isPublic, handle.count >= 3, ConversationHandles.wellFormed(handle) else { return }
        let typed = handle
        takenCheck = Task { @MainActor in
            try? await Task.sleep(nanoseconds: 500_000_000)
            guard !Task.isCancelled, typed == handle else { return }
            let discovery = DiscoveryClient()
            guard let pair = try? await discovery.oprfName(typed) else { return }
            let answered = (try? await discovery.lookupRaw(
                LookupRequest(oprfHash: base64Encode(pair.lookupKey)))) ?? nil
            guard !Task.isCancelled, typed == handle else { return }
            handleTaken = answered != nil
        }
    }

    /// Nothing was created: the uploaded photo is never referenced.
    private func deleteUploaded(_ avatar: ProfileAvatar?) {
        guard let avatar else { return }
        BlobLifecycle.shared.delete([avatar.still.blobId] + (avatar.video.map { [$0.blobId] } ?? []))
    }

    private func proposedHandle() -> String {
        suggestHandle(name: trimmedName, kind: .group, private: !isPublic, random: handleRandom)
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
                TextField(l10n.t("chats.new_group.name_label"), text: $name)
                    .disabled(busy)
                    .onChange(of: name) { _, value in
                        if value.count > Self.maxNameLength {
                            name = String(value.prefix(Self.maxNameLength))
                        }
                        if value.contains(where: \.isNewline) {
                            name = value.filter { !$0.isNewline }
                        }
                    }
            } footer: {
                if isPublic && trimmedName.isEmpty {
                    Text(l10n.t("group_type.needs_name")).foregroundStyle(Color.lakkoDanger)
                } else {
                    Text(l10n.t("new_group.name_hint"))
                }
            }
            // ADR-0087 / UX 3.1 — the @name and who can join.
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
                .disabled(busy)
                LakkoPicker(
                    l10n.t("group_type.who_can_join"), systemImage: "person.2",
                    selection: $isPublic,
                    options: [
                        LakkoPickerOption(value: false, label: l10n.t("group_type.private")),
                        LakkoPickerOption(value: true, label: l10n.t("group_type.public")),
                    ])
            } footer: {
                VStack(alignment: .leading, spacing: 4) {
                    if !handle.isEmpty && !ConversationHandles.wellFormed(handle) {
                        Text(l10n.t("group_handle.error.bad_form")).foregroundStyle(Color.lakkoDanger)
                    } else if isPublic && handleTaken {
                        Text(l10n.t("group_handle.error.taken")).foregroundStyle(Color.lakkoDanger)
                    } else {
                        Text(l10n.t(isPublic ? "group_type.handle_public" : "group_type.handle_private"))
                    }
                    Text(l10n.t(isPublic ? "group_type.public_desc" : "group_type.private_desc"))
                }
            }
            Section(l10n.t("chat.members.title")) {
                if accepted.isEmpty {
                    // Nobody to pick: a name alone founds the group.
                    Text(l10n.t("new_group.no_contacts"))
                        .foregroundStyle(Color.lakkoTextMuted)
                } else {
                    ForEach(accepted, id: \.request.fromPubkey) { entry in
                        contactRow(entry)
                    }
                }
            }
            if let errorMessage {
                Section {
                    Text(l10n.t(errorMessage))
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoDanger)
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(l10n.t("chats.new_group.title"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button(l10n.t(busy ? "chats.new_group.creating" : "chats.create.group")) {
                    Task { await create() }
                }
                .disabled(busy || !Self.canCreate(name: name, handle: handle, isPublic: isPublic))
            }
        }
        .disabled(busy)
        .onAppear { if handle.isEmpty { handle = proposedHandle() } }
        .onChange(of: name) { _, _ in if !handleEdited { handle = proposedHandle() } }
        .onChange(of: isPublic) { _, _ in
            if !handleEdited { handle = proposedHandle() }
            scheduleTakenCheck()
        }
        .onChange(of: handle) { _, _ in scheduleTakenCheck() }
        .onChange(of: photo) { _, _ in photoError = nil }
    }

    @ViewBuilder
    private func contactRow(_ entry: AcceptedContact) -> some View {
        // A contact whose request carried no account id (a pre-ADR-0021
        // record) cannot be added: `startGroupWith` / `addMemberToGroup` need
        // it to fetch the KeyPackages. Listed, dimmed, unselectable.
        let addable = entry.request.fromAccountId != nil
        let isOn = selected.contains(entry.request.fromPubkey) && addable
        ResolvedName(signatureKey: entry.request.fromPubkey) { resolved in
            Button {
                guard addable else { return }
                if isOn {
                    selected.remove(entry.request.fromPubkey)
                } else {
                    selected.insert(entry.request.fromPubkey)
                }
            } label: {
                HStack(spacing: 12) {
                    InitialsAvatar(
                        name: resolved.name,
                        identity: entry.request.fromPubkey.base64EncodedString(),
                        size: 40,
                        photo: .person(entry.request.fromPubkey)
                    )
                    VStack(alignment: .leading, spacing: 2) {
                        Text(resolved.listLabel)
                            .foregroundStyle(addable ? Color.lakkoText : Color.lakkoTextMuted)
                        if !addable {
                            Text(l10n.t("chats.new_group.accept_again"))
                                .font(.footnote)
                                .foregroundStyle(Color.lakkoTextMuted)
                        }
                    }
                    Spacer()
                    Image(systemName: isOn ? "checkmark.circle.fill" : "circle")
                        .foregroundStyle(isOn ? Color.lakkoAccent : Color.lakkoTextMuted)
                        .accessibilityHidden(true)
                }
            }
            .disabled(!addable)
            .accessibilityAddTraits(isOn ? .isSelected : [])
        }
    }

    private func create() async {
        if busy { return }
        let chosen = picks
        guard Self.canCreate(name: name, handle: handle, isPublic: isPublic) else { return }
        busy = true
        defer { busy = false }
        errorMessage = nil

        // 0. UX 4.5.3 — the photo is uploaded before anything is created, so
        //    a failed upload creates nothing and the form stays filled.
        var uploadedPhoto: ProfileAvatar?
        if let photo {
            do {
                uploadedPhoto = try await ProfileAvatarUpload.upload(photo)
            } catch AttachmentBridgeError.uploadLimited {
                photoError = "group_photo.error.limit"
                return
            } catch {
                photoError = "group_photo.error.upload"
                return
            }
        }

        // 1. Found the group with the first pick (one Welcome, we are the
        //    owner), or with nobody else when none was picked. A failure here
        //    created nothing: the form stays filled.
        // 1. Found the group with nobody else (this account's other devices
        //    join in the founding commit), claiming the @name BEFORE anything
        //    about it leaves this device (UX 3.1 / 3.3): a held name creates
        //    nothing and the form stays filled. The picks are added after,
        //    through the normal add path.
        let groupIdHex: String
        var claimed: (value: FfiConversationHandle, leaseUntilMs: UInt64)?
        do {
            let wanted = handle
            let publicGroup = isPublic
            let typedName = trimmedName
            let photoForRecord = uploadedPhoto
            groupIdHex = try await chatRepository.foundGroupWithNobodyElse(beforePublish: { groupId in
                claimed = try await chatRepository.claimHandleBeforeFounding(
                    groupId: groupId, handle: wanted, isPublic: publicGroup, name: typedName,
                    avatar: photoForRecord)
            })
        } catch let refusal as DiscoveryClient.HandleWriteError {
            errorMessage = refusal.code == "name-taken"
                ? "group_handle.error.held"
                : (refusal.status == 429 ? "group_handle.error.too_many" : "new_group.error.secure_setup")
            deleteUploaded(uploadedPhoto)
            return
        } catch let token as AnonTokenUnavailable {
            errorMessage = AnonTokenUnavailable.isQuotaExhausted(token)
                ? "group_handle.error.too_many" : "group_handle.error.offline"
            deleteUploaded(uploadedPhoto)
            return
        } catch let DiscoveryClientError.network(_, underlying) {
            NSLog("New group: offline: \(underlying)")
            errorMessage = "group_handle.error.offline"
            deleteUploaded(uploadedPhoto)
            return
        } catch {
            NSLog("New group: founding failed: \(error)")
            errorMessage = "new_group.error.secure_setup"
            deleteUploaded(uploadedPhoto)
            return
        }
        if let accountIdB64 = AcceptedContactsStore.boundAccountIdB64 {
            chatRepository.groupSettingsStore.markCreatedHere(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        }
        /// Catalog keys and their arguments, rendered when shown.
        var notices: [(key: String, texts: [String: String])] = []

        // 2. The name, right after the creating commit and BEFORE the
        //    follow-up adds, so the first recipients get it with their first
        //    poll and every later add's growth re-send has a stored name to
        //    carry. A failed send leaves the group nameless; the members
        //    panel offers the rename.
        if !trimmedName.isEmpty {
            do {
                try await chatRepository.setGroupName(groupIdHex: groupIdHex, name: trimmedName)
            } catch {
                NSLog("New group: name send failed: \(error)")
                notices.append((key: "new_group.name_not_sent", texts: [:]))
            }
        }

        // 2a. UX 4.5.3 — the photo, sent after the creating commit like the
        //     name. A failed send stays owed and goes when the device is back.
        if let uploadedPhoto {
            try? await chatRepository.setGroupPhoto(groupIdHex: groupIdHex, avatar: uploadedPhoto)
        }

        // 2b. ADR-0087 section 3 — the @name claimed above, stored and sent
        //     into the group now that it exists.
        if let claimed {
            await chatRepository.adoptOwnHandle(
                groupIdHex: groupIdHex, value: claimed.value, leaseUntilMs: claimed.leaseUntilMs,
                name: trimmedName)
        }

        // 3. The remaining picks, one after another through the normal add
        //    path. Each failure is collected, never fatal.
        var failed: [String] = []
        for entry in chosen {
            guard let accountId = entry.request.fromAccountId else { continue }
            do {
                _ = try await chatRepository.addMemberToGroup(
                    groupIdHex: groupIdHex,
                    recipientAccountId: accountId,
                    recipientSignaturePubkey: entry.request.fromPubkey
                )
            } catch {
                NSLog("New group: add failed: \(error)")
                failed.append(ProfileDirectory.shared.resolve(signatureKey: entry.request.fromPubkey).name)
            }
        }
        if !failed.isEmpty {
            notices.append((
                key: "chat.members.add.error.some_failed",
                texts: ["names": Localization.joinList(failed)]))
        }
        let setupNotice: UiText? = notices.isEmpty ? nil : UiText {
            notices
                .map { Localization.text($0.key, texts: $0.texts) }
                .reduce("") { acc, next in
                    acc.isEmpty
                        ? next
                        : Localization.text("common.sentence_pair", texts: ["first": acc, "second": next])
                }
        }
        onCreated(groupIdHex, setupNotice)
    }
}
