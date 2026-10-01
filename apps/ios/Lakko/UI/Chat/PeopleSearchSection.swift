// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// What "Search people" does, supplied by the app shell.
public struct PeopleSearchActions {
    /// Local then network search for the typed text.
    public var search: @MainActor (String) async -> PeopleSearchOutcome
    /// Send a contact request to a result, with its introduction.
    public var sendRequest: @MainActor (FoundPerson, String) async throws -> Void

    public init(
        search: @escaping @MainActor (String) async -> PeopleSearchOutcome,
        sendRequest: @escaping @MainActor (FoundPerson, String) async throws -> Void
    ) {
        self.search = search
        self.sendRequest = sendRequest
    }
}

/// `docs/ux/who-can-find-me.md` section 6.1 — the "Search people" row and its
/// results: avatar, name, `@username` when verified, and Add contact. A row
/// never says why the person was found.
struct PeopleSearchSection: View {
    let query: String
    let actions: PeopleSearchActions

    @State private var busy = false
    @State private var outcome: PeopleSearchOutcome?
    @State private var searchedFor = ""
    @State private var requesting: FoundPerson?
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if PeopleSearch.offersNetworkSearch(query) {
                Button { Task { await run() } } label: {
                    HStack(spacing: 12) {
                        Image(systemName: "person.2")
                            .foregroundStyle(Color.lakkoAccent)
                        Text(l10n.t("search.people.search"))
                            .foregroundStyle(Color.lakkoText)
                        Spacer()
                        if busy {
                            ProgressView().controlSize(.small)
                                .accessibilityLabel(l10n.t("search.people.searching_a11y"))
                        } else {
                            Image(systemName: "chevron.right")
                                .font(.footnote.weight(.semibold))
                                .foregroundStyle(Color.lakkoTextMuted)
                        }
                    }
                }
                .buttonStyle(.plain)
                .disabled(busy)
            }
            if searchedFor == query, let outcome {
                if case .found(let people) = outcome {
                    ForEach(people) { person in
                        HStack(spacing: 12) {
                            InitialsAvatar(
                                name: person.displayName ?? person.username ?? "?",
                                identity: person.accountKey.base64EncodedString(),
                                size: 40)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(person.displayName ?? l10n.t("contacts.request.someone"))
                                    .foregroundStyle(Color.lakkoText)
                                if let username = person.username {
                                    Text("@\(username)")
                                        .font(.subheadline)
                                        .foregroundStyle(Color.lakkoTextMuted)
                                }
                            }
                            Spacer()
                            // A contact has nothing to add; someone who does
                            // not let this account find them is shown by
                            // name only (UX section 6.1, web `canAddContact`).
                            if person.canAddContact {
                                Button(l10n.t("search.people.add_contact")) { requesting = person }
                                    .buttonStyle(.borderless)
                            }
                        }
                    }
                } else if let message = PeopleSearch.message(for: outcome) {
                    Text(message)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .accessibilityAddTraits(.updatesFrequently)
                }
            }
        }
        .sheet(item: $requesting) { person in
            ContactRequestSheet(person: person, send: actions.sendRequest)
        }
    }

    private func run() async {
        busy = true
        defer { busy = false }
        let typed = query
        let result = await actions.search(typed)
        searchedFor = typed
        outcome = result
    }
}

/// The contact request to a search result, with its introduction.
private struct ContactRequestSheet: View {
    let person: FoundPerson
    let send: @MainActor (FoundPerson, String) async throws -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var intro = ""
    @State private var sending = false
    @State private var error: UiText?
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField(l10n.t("contacts.add.intro_label"), text: $intro, axis: .vertical)
                        .lineLimit(3...6)
                } header: {
                    Text(person.displayName ?? person.username.map { "@\($0)" } ?? l10n.t("contacts.request.someone"))
                        .textCase(nil)
                }
                if let error {
                    Text(error.text).foregroundStyle(Color.lakkoDanger)
                }
            }
            .navigationTitle(l10n.t("search.people.add_contact"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(l10n.t("common.cancel")) { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if sending {
                        ProgressView()
                    } else {
                        Button(l10n.t("search.people.send_request")) { Task { await submit() } }
                    }
                }
            }
        }
    }

    private func submit() async {
        sending = true
        defer { sending = false }
        do {
            try await send(person, intro)
            dismiss()
        } catch {
            // Rendered from the error's kind, never its English description:
            // a refused request reads `search.people.request_refused`, a
            // network failure the offline line, anything else the generic
            // send failure (web `contactsErrorText(err, "contacts.error.send")`).
            self.error = ContactsViewModel.contactsErrorText(for: error, fallbackKey: "contacts.error.send")
        }
    }
}
