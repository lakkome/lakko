// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// Groups > New group (docs/ux/new-group.md): a name field above the
/// accepted contacts, one checkbox per contact, and Create.
///
/// The name is optional (ADR-0051 §3: sent into the group right after the
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

    var body: some View {
        List {
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
                Text(l10n.t("new_group.name_hint"))
            }
            Section(l10n.t("chat.members.title")) {
                if accepted.isEmpty {
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
                .disabled(busy || picks.isEmpty)
            }
        }
        .disabled(busy)
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
                        size: 40
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
        guard let first = chosen.first, let firstAccountId = first.request.fromAccountId else { return }
        busy = true
        defer { busy = false }
        errorMessage = nil

        // 1. Found the group with the first pick (one Welcome, we are the
        //    owner). A failure here created nothing: the form stays filled.
        let groupIdHex: String
        do {
            groupIdHex = try await chatRepository.startGroupWith(
                recipientAccountId: firstAccountId,
                recipientSignaturePubkey: first.request.fromPubkey,
                // ADR-0062 — a group, fixed in the GroupContext, even while
                // it still has two people.
                kind: .group
            )
        } catch {
            NSLog("New group: founding failed: \(error)")
            errorMessage = "chats.new_group.error"
            return
        }
        /// Catalog keys and their arguments, rendered when shown.
        var notices: [(key: String, texts: [String: String])] = []

        // 2. The name, right after the creating commit and BEFORE the
        //    follow-up adds, so the first recipients get it with their first
        //    poll and every later add's growth re-send has a stored name to
        //    carry. A failed send leaves the group nameless; the members
        //    panel offers the rename.
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmedName.isEmpty {
            do {
                try await chatRepository.setGroupName(groupIdHex: groupIdHex, name: trimmedName)
            } catch {
                NSLog("New group: name send failed: \(error)")
                notices.append((key: "new_group.name_not_sent", texts: [:]))
            }
        }

        // 3. The remaining picks, one after another through the normal add
        //    path. Each failure is collected, never fatal.
        var failed: [String] = []
        for entry in chosen.dropFirst() {
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
