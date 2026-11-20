// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import LakkoCore
import SwiftUI
import UIKit

/// A channel's info, opened by tapping its name (`docs/ux/conversation-info.md`
/// sections 1 and 2.3; `docs/ux/group-and-channel-privacy.md` section 12.1): a
/// screen pushed over the channel, the same order as a group's — header,
/// actions, detail, exit block.
///
/// Which blocks show is `channelInfoSections` in `crates/lakko-ui-rules`,
/// shared with web and Android, over one capability record; this layout
/// decides how a block looks, never whether it is there.
struct ChannelInfoSheet: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data
    var foundHandle: String = ""
    /// Section 14.1 — Invite people needs the contacts and the chats.
    let contacts: ContactsViewModel?
    let chat: ChatRepository?
    /// The channel has left this device's list and the screen behind this
    /// one has nothing left to show.
    let onUnsubscribed: () -> Void

    @ObservedObject private var l10n = Localization.shared
    @ObservedObject private var reach = ReachCenter.shared
    /// ADR-0099 step 5 — a channel moderators removed.
    @ObservedObject private var removals = ChannelRemovals.shared
    /// ADR-0099 step 3 — a suspended account invites nobody.
    @Environment(\.accountSuspended) private var accountSuspended
    @State private var confirmingUnsubscribe = false
    @State private var typeOpen = false
    @State private var inviteFailedNames: [String] = []
    @State private var copiedHandle = false
    /// Bumped when ``CoPublisherKeyBook/publishersChanged`` fires.
    @State private var publishersRevision = 0
    @State private var confirmingDelete = false
    @State private var deleting = false
    @State private var deleteFailed = false
    /// ADR-0099 / UX 2.1 — the report sheet for the channel.
    @State private var reportRequest: ReportRequest?
    /// UX 4.5.2 — the header photo (the owner changes it).
    @StateObject private var photo: ChannelPhotoModel
    @StateObject private var reactions: ChannelReactionsSetting

    init(
        channelId: String, state: ChannelsUiState, client: ChannelsClient, accountIdentitySeed: Data,
        foundHandle: String = "", contacts: ContactsViewModel? = nil, chat: ChatRepository? = nil,
        onUnsubscribed: @escaping () -> Void
    ) {
        self.channelId = channelId
        self.state = state
        self.client = client
        self.accountIdentitySeed = accountIdentitySeed
        self.foundHandle = foundHandle
        self.contacts = contacts
        self.chat = chat
        self.onUnsubscribed = onUnsubscribed
        _photo = StateObject(wrappedValue: ChannelPhotoModel(
            channelId: channelId, state: state, client: client, accountIdentitySeed: accountIdentitySeed))
        _reactions = StateObject(wrappedValue: ChannelReactionsSetting(
            channelId: channelId, state: state, client: client, accountIdentitySeed: accountIdentitySeed))
    }

    private var channel: ChannelHandle? { state.channels.first { $0.id == channelId } }
    private var isPublisher: Bool { state.keysByChannel[channelId] != nil }
    private var isOwner: Bool { ChannelOwnership.isOwner(channelId: channelId, state: state) }
    private var subscriberCount: Int? { state.subscriberCountByChannel[channelId] }

    private var role: ChannelRole {
        if isOwner { return .owner }
        return isPublisher ? .publisher : .subscriber
    }

    /// The `@name` this device knows: the record's, else the one it holds or
    /// was found by.
    private var handle: String {
        let fromRecord = state.extras(channelId).handle?.handle ?? ""
        let held = ChannelSettings.heldHandle(channelId: channelId)?.handle ?? ""
        let local = ChannelsFormatting.bareHandle(channel?.handle ?? foundHandle)
        return [fromRecord, held, local].first { !$0.isEmpty } ?? ""
    }

    private var removal: ChannelRemovals.Status? { removals.status(channelId) }

    private var sections: [InfoSection] {
        // Read so the screen redraws when the publisher record changes.
        _ = publishersRevision
        return ChannelRemovalRules.infoSections(baseSections, removed: removal != nil)
    }

    private var baseSections: [InfoSection] {
        SuspendedInfo.withoutSending(ChannelReaderDeviceInfo.sections(
            readerDevice: state.readerDeviceRole(channelId) != nil,
            channelInfoSections(caps: ChannelInfoCapabilities(
            role: role,
            hasHandle: !handle.isEmpty,
            // The co-publishers this account approved; a co-publisher always
            // has the owner. Delete channel itself waits for the core route.
            hasOtherPublishers: CoPublisherKeyBook.hasOtherPublishers(
                channelIdHex: channelId, isOwner: isOwner),
            membersOfGroupsAndChannelsOn: reach.setting.mode == .some && reach.setting.groups != nil))),
            suspended: accountSuspended)
    }

    private var title: String {
        ChannelsFormatting.title(
            sharedName: channel?.sharedName ?? "", localName: channel?.localName ?? "", handle: handle)
    }

    private var description: String {
        (channel?.displayDescription ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// UX 4.4 — a channel whose `@name` lapsed (or never landed).
    private var nameLapsed: Bool {
        guard isOwner else { return false }
        guard let held = ChannelSettings.heldHandle(channelId: channelId) else { return true }
        return held.leaseUntilMs < UInt64(Date().timeIntervalSince1970 * 1000)
    }

    var body: some View {
        List {
            header
            if nameLapsed, removal == nil {
                Section {
                    HStack {
                        Text(l10n.t("channels.info.handle_lapsed"))
                            .font(.subheadline)
                            .foregroundStyle(Color.lakkoText)
                        Spacer()
                        Button(l10n.t("group_info.handle.choose")) { typeOpen = true }
                            .buttonStyle(.borderless)
                    }
                }
            }
            if sections.contains(.renameGroup) {
                ChannelRenameSection(
                    channelId: channelId, state: state, client: client, accountIdentitySeed: accountIdentitySeed)
            } else if sections.contains(.channelDescription), !description.isEmpty {
                Section {
                    Text(description).font(.subheadline).foregroundStyle(Color.lakkoText)
                }
            }
            actions
            detail
            if sections.contains(.membersCanFindMe) {
                MembersCanFindMeSection(groupIdHex: channelId, kind: .channel)
            }
            // ADR-0099 / UX 2.1 — a subscriber reports the channel; its posts
            // may be picked.
            if sections.contains(.report), let id = ChannelsClient.hexToData(channelId), id.count == 16 {
                Section {
                    Button(role: .destructive) {
                        // The posts this device holds for the channel: its
                        // local post log (the one the Feed reads), with the
                        // wires of posts loaded this session as a fallback.
                        reportRequest = ChannelReportSource.request(
                            channelId: id, title: title,
                            logged: AcceptedContactsStore.boundAccountIdB64
                                .flatMap { ChannelPostLogStore().get(accountIdB64: $0)?.channels[channelId] }?
                                .posts ?? [],
                            loaded: state.postsByChannel[channelId] ?? [])
                    } label: {
                        Label(l10n.t("report.action"), icon: .flag)
                            .foregroundStyle(Color.lakkoDanger)
                    }
                }
            }
            if sections.contains(.unsubscribe) {
                Section {
                    Button(role: .destructive) { confirmingUnsubscribe = true } label: {
                        Text(l10n.t("channels.unsubscribe.action"))
                    }
                } footer: {
                    Text(l10n.t("channels.info.unsubscribe_sub"))
                }
            }
            // ADR-0087 phase B — the owner, with no publisher left.
            if sections.contains(.deleteGroup) {
                Section {
                    Button(role: .destructive) { confirmingDelete = true } label: {
                        Text(l10n.t("channels.delete.control"))
                    }
                    .disabled(deleting)
                } footer: {
                    if deleteFailed {
                        Text(l10n.t("channels.delete.failed")).foregroundStyle(Color.lakkoDanger)
                    }
                }
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: CoPublisherKeyBook.publishersChanged)
            .receive(on: DispatchQueue.main)) { _ in
            publishersRevision += 1
        }
        .listStyle(.insetGrouped)
        .navigationTitle(l10n.t("chat.info.channel"))
        .navigationBarTitleDisplayMode(.inline)
        .navigationDestination(isPresented: $typeOpen) {
            ChannelTypeScreen(
                channelId: channelId, state: state, client: client,
                accountIdentitySeed: accountIdentitySeed, onClose: { typeOpen = false })
        }
        .lakkoDialog(
            isPresented: $confirmingDelete,
            title: l10n.t("channels.delete.title"),
            message: handle.isEmpty
                ? l10n.t("channels.delete.body_no_handle")
                : l10n.t("channels.delete.body", texts: ["name": handle])
        ) {
            [.danger(l10n.t("channels.delete.control")) { Task { await deleteChannel() } }, .cancel()]
        }
        .reportSheet($reportRequest) { follow in
            if follow == .unsubscribe { unsubscribe() }
        }
        .lakkoDialog(
            isPresented: $confirmingUnsubscribe,
            title: l10n.t("channels.unsubscribe.title_named", texts: ["name": title]),
            message: l10n.t("channels.unsubscribe.body")
        ) {
            [.danger(l10n.t("channels.unsubscribe.action")) { unsubscribe() }, .cancel()]
        }
        .task {
            // Section 15.1 — the exact count, for the owner and publishers.
            guard isPublisher, removal == nil, let identity = try? Identity.fromSeed(seed: accountIdentitySeed),
                  let exact = try? await client.subscriberCount(channelIdHex: channelId, accountIdentity: identity)
            else { return }
            state.setSubscriberCount(channelId: channelId, count: Int(clamping: exact))
        }
    }

    // MARK: - Header

    private var header: some View {
        Section {
            VStack(spacing: 8) {
                GroupPhotoHeader(viewModel: photo)
                Text(title)
                    .font(.title2.weight(.semibold))
                    .foregroundStyle(Color.lakkoText)
                    .multilineTextAlignment(.center)
                if let line = ChannelHeaderLine.text(
                    isPublic: state.extras(channelId).isPublic, subscriberCount: subscriberCount) {
                    Text(line)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
                // ADR-0099 step 5 / UX 3 — said only once the log proved it.
                if let removal {
                    Text(l10n.t(removal.lineKey))
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .multilineTextAlignment(.center)
                }
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 8)
            .listRowBackground(Color.clear)
            // Section 4 — the `@name` row with Copy, no share sheet.
            if sections.contains(.handle) {
                HStack {
                    Text("@" + handle)
                        .foregroundStyle(Color.lakkoText)
                        .environment(\.layoutDirection, .leftToRight)
                    Spacer()
                    Button {
                        UIPasteboard.general.string = "@" + handle
                        copiedHandle = true
                    } label: {
                        Text(l10n.t(copiedHandle ? "common.copied" : "common.copy"))
                    }
                    .buttonStyle(.borderless)
                    .accessibilityLabel(l10n.t("group_info.handle.copy_a11y"))
                }
            }
        }
    }

    // MARK: - Actions

    @ViewBuilder
    private var actions: some View {
        if sections.contains(.invitePeople) || sections.contains(.inviteLink) {
            Section {
                if sections.contains(.invitePeople), let contacts, let chat {
                    NavigationLink {
                        ChannelInvitePeopleScreen(
                            channelId: channelId, state: state, client: client,
                            accountIdentitySeed: accountIdentitySeed, contacts: contacts, chat: chat,
                            onDone: { failed in inviteFailedNames = failed.filter { !$0.isEmpty } })
                    } label: {
                        Text(l10n.t("channels.info.invite_people")).foregroundStyle(Color.lakkoText)
                    }
                }
                if sections.contains(.inviteLink) {
                    NavigationLink {
                        ChannelInviteLinkScreen(
                            channelId: channelId, state: state, client: client,
                            accountIdentitySeed: accountIdentitySeed)
                    } label: {
                        Text(l10n.t("chat.invite_link.label")).foregroundStyle(Color.lakkoText)
                    }
                }
            } footer: {
                if !inviteFailedNames.isEmpty {
                    Text(l10n.t("channels.invite.failed", texts: [
                        "names": inviteFailedNames.joined(separator: ", ")]))
                        .foregroundStyle(Color.lakkoDanger)
                }
            }
        }
    }

    // MARK: - Detail

    @ViewBuilder
    private var detail: some View {
        Section {
            if sections.contains(.subscribers) {
                NavigationLink {
                    ChannelSubscribersScreen(
                        channelId: channelId, state: state, client: client, accountIdentitySeed: accountIdentitySeed)
                } label: {
                    valueRow(
                        l10n.t("channels.subscribers.title"),
                        value: subscriberCount.map {
                            // Section 15.1 — exact to the owner and publishers,
                            // the band to everyone else.
                            isPublisher ? $0.formatted(.number.locale(l10n.locale)) : ChannelHeaderLine.band($0)
                        })
                }
            }
            if sections.contains(.admins) {
                NavigationLink {
                    ChannelPublishersScreen(
                        channelId: channelId, state: state, client: client,
                        accountIdentitySeed: accountIdentitySeed, isOwner: isOwner,
                        contacts: contacts, chat: chat)
                } label: {
                    valueRow(l10n.t("channels.info.publishers"), value: nil)
                }
            }
            if sections.contains(.reactions) {
                NavigationLink {
                    ChannelReactionsScreen(
                        channelId: channelId, state: state, client: client, accountIdentitySeed: accountIdentitySeed)
                } label: {
                    valueRow(l10n.t("group_reactions.title"), value: reactions.summary)
                }
            }
            if sections.contains(.groupType) {
                NavigationLink {
                    ChannelTypeScreen(
                        channelId: channelId, state: state, client: client,
                        accountIdentitySeed: accountIdentitySeed, onClose: {})
                } label: {
                    valueRow(
                        l10n.t("channel_type.title"),
                        value: state.extras(channelId).isPublic.map {
                            l10n.t($0 ? "group_type.public" : "group_type.private")
                        })
                }
            }
        }
    }

    private func valueRow(_ title: String, value: String?) -> some View {
        HStack {
            Text(title).foregroundStyle(Color.lakkoText)
            Spacer()
            if let value { Text(value).foregroundStyle(Color.lakkoTextMuted) }
        }
    }

    /// Section 9.5 — the row and its posts go, here and on the account's
    /// other devices; the server forgets the subscriber.
    /// The service deletes the channel and every subscriber's device ends it
    /// on its next read (`410 channel-deleted`); this device ends it now.
    private func deleteChannel() async {
        deleting = true
        deleteFailed = false
        defer { deleting = false }
        do {
            let identity = try Identity.fromSeed(seed: accountIdentitySeed)
            try await client.deleteChannel(channelIdHex: channelId, accountIdentity: identity)
            state.endDeleted(channelId)
            onUnsubscribed()
            // ADR-0064 section 1 — the channel's vault goes with it, best
            // effort: a failure does not undo the delete.
            let id = channelId
            let seed = accountIdentitySeed
            Task { @MainActor in await ChannelVaultHead.deleteVault(channelIdHex: id, currentSeed: seed) }
        } catch {
            deleteFailed = true
        }
    }

    private func unsubscribe() {
        if removal != nil, isPublisher {
            // ADR-0099 step 5 — the owner or a publisher takes a removed
            // channel out of the list, on every device of the account (the
            // conversation-end record); the service is not asked (it answers
            // 410 for the channel anyway).
            ChannelUnsubscribe.leaveRemoved(state: state, channelId: channelId)
            state.onUnsubscribedHere?(channelId)
            onUnsubscribed()
            return
        }
        ChannelUnsubscribe.stopFollowing(state: state, channelId: channelId)
        state.onUnsubscribedHere?(channelId)
        onUnsubscribed()
    }
}

/// Section 12.1 — the owner renames the channel and edits its description in
/// place: a new metadata record that every subscriber reads.
private struct ChannelRenameSection: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data

    @ObservedObject private var l10n = Localization.shared
    @State private var name = ""
    @State private var description = ""
    @State private var seeded = false
    @State private var busy = false
    @State private var error: UiText?

    private var held: SharedChannelMeta? { state.sharedMeta(channelId: channelId) }
    private var channel: ChannelHandle? { state.channels.first { $0.id == channelId } }

    private var dirty: Bool {
        name.trimmingCharacters(in: .whitespacesAndNewlines) != (held?.name ?? channel?.localName ?? "")
            || description.trimmingCharacters(in: .whitespacesAndNewlines)
                != (held?.description ?? channel?.localDescription ?? "")
    }

    var body: some View {
        Section {
            TextField(l10n.t("channels.field.name"), text: $name)
                .onChange(of: name) { _, value in
                    if value.count > ChannelMetaRecord.nameMaxChars { name = String(value.prefix(ChannelMetaRecord.nameMaxChars)) }
                    if value.contains(where: \.isNewline) { name = value.filter { !$0.isNewline } }
                }
            TextField(l10n.t("channels.field.description_optional"), text: $description, axis: .vertical)
                .lineLimit(1...5)
                .onChange(of: description) { _, value in
                    if value.count > ChannelMetaRecord.descriptionMaxChars {
                        description = String(value.prefix(ChannelMetaRecord.descriptionMaxChars))
                    }
                }
            if dirty {
                Button(l10n.t(busy ? "common.saving" : "common.save")) { Task { await save() } }
                    .disabled(busy)
            }
        } footer: {
            if let error { Text(error.text).foregroundStyle(Color.lakkoDanger) }
        }
        .disabled(busy)
        .onAppear {
            guard !seeded else { return }
            seeded = true
            name = held?.name ?? channel?.localName ?? ""
            description = held?.description ?? channel?.localDescription ?? ""
        }
    }

    private func save() async {
        busy = true
        error = nil
        defer { busy = false }
        let now = max(UInt64(Date().timeIntervalSince1970 * 1000), (held?.updatedAtMs ?? 0) &+ 1)
        // The record keeps everything else it carries: the flag and the
        // `@name`, the photo, the reactions, the Discover fields.
        guard let meta = ChannelMetaRecord.fresh(name: name, description: description, nowMs: now, keeping: held) else {
            error = .key("channels.meta.error.name_empty")
            return
        }
        do {
            let identity = try Identity.fromSeed(seed: accountIdentitySeed)
            try await ChannelMetaRecord.post(
                state: state, client: client, channelId: channelId, meta: meta, accountIdentity: identity)
            state.updateLocalMeta(channelId: channelId, localName: meta.name, localDescription: meta.description)
            if meta.listed {
                try? await ChannelListing.refreshAfterEdit(
                    state: state, client: client, channelIdHex: channelId, identity: identity)
            }
        } catch {
            self.error = ChannelsErrorCopy.text(
                for: error, prefixKey: "channels.meta.error.save_prefix", fallbackKey: "channels.meta.error.save")
        }
    }
}

/// Section 15.2 — `Publishers`. The list of publishers and `Remove
/// publisher` wait for the PublisherGroup member list (a core item); until
/// then the owner answers co-publisher requests here.
struct ChannelPublishersScreen: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data
    let isOwner: Bool
    /// UX 15.2 — `Add publisher` picks a contact and sends the card in the
    /// one-to-one.
    let contacts: ContactsViewModel?
    let chat: ChatRepository?
    @ObservedObject private var l10n = Localization.shared
    @State private var addFailedNames: [String] = []

    /// UX 15.2 — one row per publisher: the owner first, then each
    /// publisher, from the PublisherGroup's members.
    private struct Row: Identifiable, Equatable {
        let accountKey: Data
        let isOwner: Bool
        let isSelf: Bool
        var id: Data { accountKey }
    }

    @State private var rows: [Row] = []
    @State private var removing: Row?
    @State private var confirmingStepDown = false
    @State private var busy = false
    @State private var errorKey: String?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                VStack(spacing: 0) {
                    ForEach(rows) { row in
                        HStack {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(name(row)).foregroundStyle(Color.lakkoText)
                                Text(l10n.t(row.isOwner ? "channels.publishers.role_owner" : "channels.publishers.role_publisher"))
                                    .font(.footnote).foregroundStyle(Color.lakkoTextMuted)
                            }
                            Spacer()
                            if isOwner, !row.isOwner {
                                Button(l10n.t("channels.publishers.remove")) { removing = row }
                                    .foregroundStyle(Color.lakkoDanger)
                                    .disabled(busy)
                            } else if !isOwner, row.isSelf {
                                Button(l10n.t("channels.publishers.step_down")) { confirmingStepDown = true }
                                    .foregroundStyle(Color.lakkoDanger)
                                    .disabled(busy)
                            }
                        }
                        .padding(14)
                        if row != rows.last { Divider().padding(.leading, 14) }
                    }
                }
                .background(Color.lakkoSurface)
                .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous))
                if let errorKey {
                    Text(l10n.t(errorKey)).font(.footnote).foregroundStyle(Color.lakkoDanger)
                }
                if isOwner, let contacts, let chat {
                    NavigationLink {
                        ChannelInvitePeopleScreen(
                            channelId: channelId, state: state, client: client,
                            accountIdentitySeed: accountIdentitySeed, contacts: contacts, chat: chat,
                            onDone: { failed in addFailedNames = failed.filter { !$0.isEmpty } },
                            publisher: true)
                    } label: {
                        HStack {
                            Label(l10n.t("channels.publishers.add"), icon: .personPlus)
                                .foregroundStyle(Color.lakkoText)
                            Spacer()
                            LkIconView(.chevronForward, size: 14, relativeTo: .footnote)
                                .foregroundStyle(Color.lakkoTextMuted)
                        }
                        .padding(14)
                        .background(Color.lakkoSurface)
                        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous))
                    }
                    .buttonStyle(.plain)
                }
                if isOwner, !addFailedNames.isEmpty {
                    Text(l10n.t("channels.invite.failed", texts: ["names": addFailedNames.joined(separator: ", ")]))
                        .font(.footnote).foregroundStyle(Color.lakkoDanger)
                }
                if isOwner {
                    CoPublishersPanel(
                        channelId: channelId, state: state, client: client, accountIdentitySeed: accountIdentitySeed)
                }
            }
            .padding(16)
        }
        .background(Color.lakkoBackground)
        .navigationTitle(l10n.t("channels.info.publishers"))
        .navigationBarTitleDisplayMode(.inline)
        .task { load() }
        .onReceive(NotificationCenter.default.publisher(for: CoPublisherKeyBook.publishersChanged)
            .receive(on: DispatchQueue.main)) { _ in load() }
        .lakkoDialog(item: $removing) { row in
            LakkoDialogContent(
                title: l10n.t("channels.publishers.remove"),
                message: l10n.t("channels.publishers.remove_body", texts: ["name": name(row)]),
                field: nil,
                actions: [
                    .danger(l10n.t("channels.publishers.remove")) { Task { await remove(row) } },
                    .cancel(),
                ])
        }
        .lakkoDialog(
            isPresented: $confirmingStepDown, title: l10n.t("channels.publishers.step_down"),
            message: l10n.t("channels.publishers.step_down_body")
        ) {
            [
                .danger(l10n.t("channels.publishers.step_down")) { Task { await stepDown() } },
                .cancel(),
            ]
        }
    }

    private func name(_ row: Row) -> String {
        if row.isSelf { return l10n.t("channels.members.you") }
        return ProfileDirectory.shared.resolve(signatureKey: row.accountKey).name
    }

    private func open() -> (identity: Identity, group: PublisherGroup)? {
        guard let identity = try? ChannelPublisherIdentity.shared.identity(currentSeed: accountIdentitySeed),
              let group = (try? CoPublisher.loadPublisherGroup(identity: identity, channelIdHex: channelId)) ?? nil
        else { return nil }
        return (identity, group)
    }

    private func load() {
        guard let (identity, group) = open(), let members = try? group.members() else { return }
        let owner = CoPublisher.ownerAccountKey(group)
        // ADR-0090 — leaves are devices; "you" is this ACCOUNT.
        let own = identity.accountPublicKey() ?? identity.signaturePublicKey()
        var seen: Set<Data> = []
        var list: [Row] = []
        for member in members.sorted(by: { $0.leafIndex < $1.leafIndex }) {
            let key = member.accountKey.isEmpty ? member.signaturePublicKey : member.accountKey
            guard seen.insert(key).inserted else { continue }
            list.append(Row(accountKey: key, isOwner: key == owner, isSelf: key == own))
        }
        rows = list
        CoPublisher.refreshPublishers(identity: identity, group: group, channelIdHex: channelId)
    }

    private func remove(_ row: Row) async {
        guard let account = AcceptedContactsStore.boundAccountIdB64 else { return }
        busy = true
        errorKey = nil
        defer { busy = false }
        do {
            try await CoPublisher.removePublisher(
                context: ChannelPublisherCommits.Context(
                    channelIdHex: channelId, currentSeed: accountIdentitySeed, accountIdB64: account,
                    channelsClient: client),
                state: state, accountKey: row.accountKey)
            load()
        } catch {
            errorKey = "channels.publishers.remove_failed"
            load()
        }
    }

    private func stepDown() async {
        guard let (identity, group) = open() else { return }
        busy = true
        errorKey = nil
        defer { busy = false }
        do {
            try await CoPublisher.stepDown(ds: DsClient(), identity: identity, publisherGroup: group)
            errorKey = "channels.publishers.step_down_sent"
        } catch {
            errorKey = "channels.publishers.step_down_failed"
        }
    }
}
