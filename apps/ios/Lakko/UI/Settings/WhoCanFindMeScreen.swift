// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// One group or channel in the "Only the ones I choose" list
/// (`docs/ux/who-can-find-me.md` section 4).
struct VisibilityScopeRow: Identifiable, Equatable {
    let id: String
    let scope: FfiScopeId
    let title: String
}

/// The pure half of the screen: what its rows say, rendered in the interface
/// language on every read (the web `COPY`). Host-tested.
enum WhoCanFindMeCopy {
    private static func t(_ key: String) -> String { Localization.text(key) }

    static var header: String {
        Localization.text(
            "common.sentence_pair",
            texts: ["first": t("who_can_find_me.lede"), "second": t("who_can_find_me.lede_link")])
    }
    static var nobodyHelper: String { t("who_can_find_me.mode.nobody_helper") }
    static var contactsOfContactsHelper: String { t("who_can_find_me.contacts_of_contacts_helper") }
    static var contactsOfContactsSpreading: String { t("who_can_find_me.contacts_of_contacts_spreading") }
    static var allGroupsHelper: String { t("who_can_find_me.groups_all_helper") }
    static var everyoneHelper: String { t("who_can_find_me.mode.everyone_helper") }
    static var offline: String { t("who_can_find_me.status.offline") }
    static var failed: String { t("who_can_find_me.status.failed") }
    static var emptyList: String { t("who_can_find_me.list.empty") }
    /// The notice after a group's switch moved the setting to "Only the ones
    /// I choose"; the Privacy section is named, not linked, on iOS.
    static var excludedNotice: String {
        Localization.text(
            "who_can_find_me.members_excluded_notice",
            texts: ["privacy": t("settings.section.privacy")])
    }

    /// "Only the ones you choose (N of M)", N counting only scopes that still
    /// exist on this device.
    static func chosenHelper(setting: FfiVisibilitySetting, all: [FfiScopeId]) -> String {
        let chosen = Set(setting.chosen.map { key($0) })
        let n = all.filter { chosen.contains(key($0)) }.count
        return Localization.text(
            "who_can_find_me.groups_chosen_helper",
            texts: ["chosen": String(n), "total": String(all.count)])
    }

    static func key(_ scope: FfiScopeId) -> String {
        (scope.kind == .group ? "g:" : "c:") + scope.id.base64EncodedString()
    }

    /// Whether `scope` is switched on in the chosen list.
    static func isChosen(_ scope: FfiScopeId, in setting: FfiVisibilitySetting) -> Bool {
        setting.chosen.contains { key($0) == key(scope) }
    }
}

/// Settings > Privacy > Who can find me (UX spec section 3). Every tap goes
/// through the one copy of the transition rules in `lakko_ui_rules`
/// (`chooseVisibilityMode`, `setContactsOfContacts`, `setGroupsVisible`,
/// `setGroupsForm`, `setScopeChosen`), then ``ReachCenter/updateSetting(_:)``
/// stamps and applies it. There is no Save button.
struct WhoCanFindMeScreen: View {
    @ObservedObject var reach: ReachCenter
    let groups: [VisibilityScopeRow]
    let channels: [VisibilityScopeRow]

    @State private var showingList = false
    /// Set when 2.1 is turned on, until this device knows it spread.
    @State private var fofSpreading = false
    @ObservedObject private var l10n = Localization.shared

    private var setting: FfiVisibilitySetting { reach.setting }
    private var allScopes: [FfiScopeId] { (groups + channels).map(\.scope) }

    private var mode: Binding<FfiVisibilityMode> {
        Binding(
            get: { setting.mode },
            set: { next in
                guard next != setting.mode else { return }
                reach.updateSetting(chooseVisibilityMode(setting: setting, mode: next))
            })
    }

    private var contactsOfContacts: Binding<Bool> {
        Binding(
            get: { setting.contactsOfContacts },
            set: { on in
                if on { fofSpreading = true }
                reach.updateSetting(setContactsOfContacts(setting: setting, on: on))
            })
    }

    private var groupsVisible: Binding<Bool> {
        Binding(
            get: { setting.groups != nil },
            set: { on in reach.updateSetting(setGroupsVisible(setting: setting, on: on)) })
    }

    private var groupsForm: Binding<FfiGroupsForm> {
        Binding(
            get: { setting.groups ?? .all },
            set: { form in
                let wasChosen = setting.groups == .chosen
                reach.updateSetting(setGroupsForm(setting: setting, form: form))
                // Section 3: selecting "Only the ones I choose" for the first
                // time opens the list straight away.
                if form == .chosen, !wasChosen { showingList = true }
            })
    }

    var body: some View {
        List {
            Section {
                LakkoInlinePicker(selection: mode, options: [
                    LakkoPickerOption(value: FfiVisibilityMode.nobody, label: l10n.t("who_can_find_me.mode.nobody")),
                    LakkoPickerOption(value: FfiVisibilityMode.some, label: l10n.t("who_can_find_me.mode.some")),
                    LakkoPickerOption(value: FfiVisibilityMode.everyone, label: l10n.t("who_can_find_me.mode.everyone")),
                ])
            } header: {
                statusHeader
            } footer: {
                switch setting.mode {
                case .nobody: Text(WhoCanFindMeCopy.nobodyHelper)
                case .everyone: Text(WhoCanFindMeCopy.everyoneHelper)
                case .some: EmptyView()
                }
            }

            if setting.mode == .some {
                Section {
                    Toggle(l10n.t("who_can_find_me.contacts_of_contacts"), isOn: contactsOfContacts)
                } footer: {
                    Text(fofSpreading && setting.contactsOfContacts
                        ? WhoCanFindMeCopy.contactsOfContactsSpreading
                        : WhoCanFindMeCopy.contactsOfContactsHelper)
                }
                Section {
                    Toggle(l10n.t("who_can_find_me.groups"), isOn: groupsVisible)
                    if setting.groups != nil {
                        LakkoInlinePicker(selection: groupsForm, options: [
                            LakkoPickerOption(value: FfiGroupsForm.all, label: l10n.t("who_can_find_me.groups.all")),
                            LakkoPickerOption(value: FfiGroupsForm.chosen, label: l10n.t("who_can_find_me.groups.chosen")),
                        ])
                        if setting.groups == .chosen {
                            Button {
                                showingList = true
                            } label: {
                                HStack {
                                    Text(l10n.t("who_can_find_me.list.choose"))
                                    Spacer()
                                    Image(systemName: "chevron.right")
                                        .font(.footnote.weight(.semibold))
                                        .foregroundStyle(Color.lakkoTextMuted)
                                }
                            }
                        }
                    }
                } footer: {
                    if setting.groups == .chosen {
                        Text(WhoCanFindMeCopy.chosenHelper(setting: setting, all: allScopes))
                    } else {
                        Text(WhoCanFindMeCopy.allGroupsHelper)
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(l10n.t("who_can_find_me.title"))
        .navigationBarTitleDisplayMode(.inline)
        .navigationDestination(isPresented: $showingList) {
            ChosenScopesScreen(reach: reach, groups: groups, channels: channels)
        }
    }

    @ViewBuilder
    private var statusHeader: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Text(WhoCanFindMeCopy.header)
                    .textCase(nil)
                if reach.status == .applying {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel(l10n.t("who_can_find_me.applying_a11y"))
                }
            }
            switch reach.status {
            case .pendingOffline:
                Text(WhoCanFindMeCopy.offline).textCase(nil)
            case .failed:
                HStack {
                    Text(WhoCanFindMeCopy.failed).textCase(nil)
                    Button(l10n.t("common.retry")) { reach.sync() }
                        .textCase(nil)
                }
                .foregroundStyle(Color.lakkoDanger)
            case .applied, .applying:
                EmptyView()
            }
        }
    }
}

/// The "Only the ones I choose" list (UX spec section 4): Groups, then
/// Channels, a switch each.
struct ChosenScopesScreen: View {
    @ObservedObject var reach: ReachCenter
    let groups: [VisibilityScopeRow]
    let channels: [VisibilityScopeRow]
    @State private var query = ""
    @ObservedObject private var l10n = Localization.shared

    private func filtered(_ rows: [VisibilityScopeRow]) -> [VisibilityScopeRow] {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !q.isEmpty else { return rows }
        return rows.filter { $0.title.lowercased().contains(q) }
    }

    private func binding(_ row: VisibilityScopeRow) -> Binding<Bool> {
        Binding(
            get: { WhoCanFindMeCopy.isChosen(row.scope, in: reach.setting) },
            set: { on in
                guard let next = try? setScopeChosen(
                    setting: reach.setting, scope: row.scope, on: on) else { return }
                reach.updateSetting(next)
            })
    }

    var body: some View {
        List {
            if groups.isEmpty && channels.isEmpty {
                Text(WhoCanFindMeCopy.emptyList)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            if !(groups.isEmpty && channels.isEmpty),
               filtered(groups).isEmpty, filtered(channels).isEmpty {
                Text(l10n.t("who_can_find_me.list.no_match"))
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            if !filtered(groups).isEmpty {
                Section(l10n.t("who_can_find_me.list.groups")) {
                    ForEach(filtered(groups)) { row in
                        Toggle(isOn: binding(row)) { scopeLabel(row) }
                    }
                }
            }
            if !filtered(channels).isEmpty {
                Section(l10n.t("who_can_find_me.list.channels")) {
                    ForEach(filtered(channels)) { row in
                        Toggle(isOn: binding(row)) { scopeLabel(row) }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(l10n.t("who_can_find_me.groups.chosen"))
        .navigationBarTitleDisplayMode(.inline)
        .modifier(SearchWhenLong(count: groups.count + channels.count, query: $query))
    }

    private func scopeLabel(_ row: VisibilityScopeRow) -> some View {
        HStack(spacing: 12) {
            InitialsAvatar(name: row.title, identity: row.id, size: 32)
            Text(row.title)
        }
    }
}

/// A search field only when there is more than a screen of rows (section 4).
private struct SearchWhenLong: ViewModifier {
    let count: Int
    @Binding var query: String

    func body(content: Content) -> some View {
        if count > 10 {
            content.searchable(text: $query, prompt: Localization.shared.t("who_can_find_me.list.search"))
        } else {
            content
        }
    }
}

/// `Members can find me` in a group's info (UX spec section 5): absent when
/// the setting does not include groups at all; under "All" it shows on and
/// turning it off switches the setting to "Only the ones I choose" with every
/// other group and channel on, said once in place.
struct MembersCanFindMeSection: View {
    let groupIdHex: String
    @ObservedObject private var reach = ReachCenter.shared
    @State private var switchedToChosen = false
    @ObservedObject private var l10n = Localization.shared

    private var scope: FfiScopeId? {
        CoPublisher.hexToData(groupIdHex).map { FfiScopeId(kind: .group, id: $0) }
    }

    var body: some View {
        if reach.setting.mode == .some, reach.setting.groups != nil, let scope {
            Section {
                Toggle(l10n.t("who_can_find_me.members_can_find_me"), isOn: Binding(
                    get: {
                        reach.setting.groups == .all
                            || WhoCanFindMeCopy.isChosen(scope, in: reach.setting)
                    },
                    set: { on in
                        if reach.setMembersCanFindMe(scope, on: on) { switchedToChosen = true }
                    }))
            } footer: {
                if switchedToChosen {
                    Text(WhoCanFindMeCopy.excludedNotice)
                }
            }
        }
    }
}
