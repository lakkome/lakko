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

/// The pure half of the screen: what its rows say. Host-tested.
enum WhoCanFindMeCopy {
    static let header =
        "Whoever can find you by username or name can ask to be your contact. "
        + "People with your invite link or QR code can always reach you."
    static let nobodyHelper = "Only people with your invite link or QR code can reach you."
    static let contactsOfContactsHelper = "People your contacts have added."
    static let contactsOfContactsSpreading =
        "Contacts of your contacts can find you once your contacts next open Lakko."
    static let allGroupsHelper = "Everyone in the groups and channels you're in."
    static let everyoneHelper = "Anyone can find you by your username or name."
    static let offline = "Will apply when you're back online."
    static let failed = "Couldn't update who can find you. Try again."
    static let emptyList = "You're not in any groups or channels yet."

    /// "Only the ones you choose (N of M)", N counting only scopes that still
    /// exist on this device.
    static func chosenHelper(setting: FfiVisibilitySetting, all: [FfiScopeId]) -> String {
        let chosen = Set(setting.chosen.map { key($0) })
        let n = all.filter { chosen.contains(key($0)) }.count
        return "Only the ones you choose (\(n) of \(all.count))."
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
                Picker("Who can find me", selection: mode) {
                    Text("Nobody").tag(FfiVisibilityMode.nobody)
                    Text("Some people").tag(FfiVisibilityMode.some)
                    Text("Everyone").tag(FfiVisibilityMode.everyone)
                }
                .pickerStyle(.inline)
                .labelsHidden()
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
                    Toggle("Contacts of my contacts", isOn: contactsOfContacts)
                } footer: {
                    Text(fofSpreading && setting.contactsOfContacts
                        ? WhoCanFindMeCopy.contactsOfContactsSpreading
                        : WhoCanFindMeCopy.contactsOfContactsHelper)
                }
                Section {
                    Toggle("Members of my groups and channels", isOn: groupsVisible)
                    if setting.groups != nil {
                        Picker("Groups and channels", selection: groupsForm) {
                            Text("All groups and channels").tag(FfiGroupsForm.all)
                            Text("Only the ones I choose").tag(FfiGroupsForm.chosen)
                        }
                        .pickerStyle(.inline)
                        .labelsHidden()
                        if setting.groups == .chosen {
                            Button {
                                showingList = true
                            } label: {
                                HStack {
                                    Text("Choose groups and channels")
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
        .navigationTitle("Who can find me")
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
                }
            }
            switch reach.status {
            case .pendingOffline:
                Text(WhoCanFindMeCopy.offline).textCase(nil)
            case .failed:
                HStack {
                    Text(WhoCanFindMeCopy.failed).textCase(nil)
                    Button("Retry") { reach.sync() }
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
            if !filtered(groups).isEmpty {
                Section("Groups") {
                    ForEach(filtered(groups)) { row in
                        Toggle(isOn: binding(row)) { scopeLabel(row) }
                    }
                }
            }
            if !filtered(channels).isEmpty {
                Section("Channels") {
                    ForEach(filtered(channels)) { row in
                        Toggle(isOn: binding(row)) { scopeLabel(row) }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("Groups and channels")
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
            content.searchable(text: $query)
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

    private var scope: FfiScopeId? {
        CoPublisher.hexToData(groupIdHex).map { FfiScopeId(kind: .group, id: $0) }
    }

    var body: some View {
        if reach.setting.mode == .some, reach.setting.groups != nil, let scope {
            Section {
                Toggle("Members can find me", isOn: Binding(
                    get: {
                        reach.setting.groups == .all
                            || WhoCanFindMeCopy.isChosen(scope, in: reach.setting)
                    },
                    set: { on in
                        if reach.setMembersCanFindMe(scope, on: on) { switchedToChosen = true }
                    }))
            } footer: {
                if switchedToChosen {
                    Text("Now only the groups and channels you choose. Change in Privacy.")
                }
            }
        }
    }
}
