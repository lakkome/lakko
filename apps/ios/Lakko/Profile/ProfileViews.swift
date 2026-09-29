// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

// MARK: - Username rule hint

/// The inline reason under a username field (UX spec §4): shown while the
/// user types, only for a name that fails the rule, never for an empty field.
struct UsernameRuleHint: View {
    let input: String

    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if let message = checkUsername(input).inlineMessage {
            Text(message)
                .font(.footnote)
                .foregroundStyle(Color.lakkoDanger)
                .accessibilityLabel(l10n.t("username.a11y_hint", texts: ["message": message]))
        }
    }
}

// MARK: - Own profile preview

/// How others see this account, for the live preview (UX spec §4): `Alice
/// Petrova · @alice`, just the name for a guest, just `@alice` without a name.
///
/// `nil` when there is neither — others then see the key fingerprint, which
/// the settings screen says in words rather than showing an unreadable key.
func ownProfilePreview(displayName: String?, username: String?) -> String? {
    let name = displayName?.trimmingCharacters(in: .whitespacesAndNewlines)
    let hasName = !(name ?? "").isEmpty
    let hasUsername = !(username ?? "").isEmpty
    switch (hasName, hasUsername) {
    case (true, true): return "\(name ?? "") · @\(username ?? "")"
    case (true, false): return name
    case (false, true): return "@\(username ?? "")"
    case (false, false): return nil
    }
}

// MARK: - Settings -> Profile

/// State for ``ProfileSettingsScreen``.
@MainActor
final class ProfileSettingsViewModel: ObservableObject {
    @Published var draftName: String
    @Published private(set) var saving = false
    /// Kept as ``UiText`` so a notice on screen follows a live language
    /// switch (ADR-0058).
    @Published private(set) var savedNotice: UiText?
    @Published private(set) var saveError: UiText?

    let username: String?
    private let directory: ProfileDirectory
    private let publishToAllGroups: () async -> Void

    init(
        directory: ProfileDirectory? = nil,
        publishToAllGroups: @escaping () async -> Void
    ) {
        let directory = directory ?? .shared
        self.directory = directory
        self.username = directory.ownUsername
        self.draftName = directory.ownProfile?.displayName ?? ""
        self.publishToAllGroups = publishToAllGroups
    }

    /// The live validation message for the draft, or `nil` when it is fine.
    var draftError: String? {
        do {
            _ = try checkDisplayName(draftName)
            return nil
        } catch {
            return (error as? LocalizedError)?.errorDescription
                ?? Localization.text("display_name.error.unknown")
        }
    }

    /// The draft as it would be saved, `nil` for blank or refused.
    private var normalizedDraft: String? {
        do { return try checkDisplayName(draftName) } catch { return nil }
    }

    /// The preview line of how others will see this account.
    var preview: String? {
        return ownProfilePreview(displayName: normalizedDraft, username: username)
    }

    /// `true` when Save would change something.
    var canSave: Bool {
        guard !saving, draftError == nil else { return false }
        return normalizedDraft != directory.ownProfile?.displayName
    }

    func save() {
        guard canSave else { return }
        saving = true
        saveError = nil
        savedNotice = nil
        do {
            try directory.setOwnDisplayName(draftName)
        } catch {
            saving = false
            NSLog("[profile] WARN name not saved: \(error)")
            saveError = UiText {
                AuthErrorCopy.rule(error) ?? Localization.text("settings.profile.error.generic")
            }
            return
        }
        draftName = directory.ownProfile?.displayName ?? ""
        savedNotice = .key("settings.profile.saved")
        Task {
            await publishToAllGroups()
            saving = false
        }
    }
}

/// Settings -> Profile (UX spec §4): the name field (one line, at most 64
/// characters), the live preview of how others see this account, Save, and the
/// username read-only.
struct ProfileSettingsScreen: View {
    /// Owned here so a parent re-render never discards a draft being typed.
    @StateObject private var viewModel: ProfileSettingsViewModel
    @ObservedObject private var l10n = Localization.shared

    init(viewModel: @autoclosure @escaping () -> ProfileSettingsViewModel) {
        self._viewModel = StateObject(wrappedValue: viewModel())
    }

    var body: some View {
            Form {
                Section {
                    TextField(l10n.t("settings.profile.name.label"), text: $viewModel.draftName)
                        .textInputAutocapitalization(.words)
                        .submitLabel(.done)
                        .onSubmit { viewModel.save() }
                        .disabled(viewModel.saving)
                    if let error = viewModel.draftError {
                        Text(error)
                            .font(.footnote)
                            .foregroundStyle(.red)
                    }
                } header: {
                    Text(l10n.t("settings.profile.name.label"))
                } footer: {
                    Text(l10n.t("settings.profile.name.hint", counts: ["count": 64]))
                }

                Section(l10n.t("settings.profile.preview.label")) {
                    if let preview = viewModel.preview {
                        Text(preview)
                    } else {
                        Text(l10n.t("settings.profile.preview.no_name"))
                            .foregroundStyle(.secondary)
                    }
                }

                Section(l10n.t("auth.username.label")) {
                    if let username = viewModel.username {
                        Text("@\(username)")
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    } else {
                        Text(l10n.t("settings.profile.username.none"))
                            .foregroundStyle(.secondary)
                    }
                }

                Section {
                    Button {
                        viewModel.save()
                    } label: {
                        HStack {
                            if viewModel.saving { ProgressView().controlSize(.small) }
                            Text(l10n.t("common.save"))
                        }
                    }
                    .disabled(!viewModel.canSave)
                    if let notice = viewModel.savedNotice {
                        Text(notice.text)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                    if let error = viewModel.saveError {
                        Text(error.text)
                            .font(.footnote)
                            .foregroundStyle(.red)
                    }
                }
            }
            .navigationTitle(l10n.t("settings.section.profile"))
            .navigationBarTitleDisplayMode(.inline)
    }
}

// MARK: - A resolved name, observed

/// Re-renders `content` with the resolved name whenever a profile or a
/// username verdict changes, so every surface updates in place (UX spec §6).
struct ResolvedName<Content: View>: View {
    @ObservedObject private var directory: ProfileDirectory
    let signatureKey: Data
    let content: (ResolvedDisplayName) -> Content

    @MainActor
    init(
        signatureKey: Data,
        directory: ProfileDirectory? = nil,
        @ViewBuilder content: @escaping (ResolvedDisplayName) -> Content
    ) {
        self._directory = ObservedObject(wrappedValue: directory ?? .shared)
        self.signatureKey = signatureKey
        self.content = content
    }

    var body: some View {
        // Reading `revision` subscribes this view to every change.
        let _ = directory.revision
        content(directory.resolve(signatureKey: signatureKey))
    }
}
