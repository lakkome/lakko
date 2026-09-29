// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The Settings tab (UX spec section 5.5): a grouped list with the account
/// header on top, then Account, Security, Appearance, Media, Network, About
/// and the danger zone. Every row pushes an existing screen onto the tab's
/// stack; nothing here is a primary destination of its own.
struct SettingsScreen: View {
    let account: SessionAccount
    /// Push a route on the Settings stack.
    let onOpen: (TabRoute) -> Void
    let onLogout: () -> Void
    let onForgetDevice: () -> Void

    /// ADR-0048 — the own name in the header updates when saved.
    @ObservedObject private var profiles = ProfileDirectory.shared
    /// The existing delete-account flow (auth `DELETE /account`, the passkey
    /// action ceremony on a 403, the discovery purge, then the local wipe)
    /// lives on ``PasskeysViewModel``; the danger zone drives that same
    /// object rather than a second implementation.
    @StateObject private var accountDeletion: PasskeysViewModel

    init(
        account: SessionAccount,
        authRepository: AuthRepository,
        onOpen: @escaping (TabRoute) -> Void,
        onLogout: @escaping () -> Void,
        onForgetDevice: @escaping () -> Void
    ) {
        self.account = account
        self.onOpen = onOpen
        self.onLogout = onLogout
        self.onForgetDevice = onForgetDevice
        self._accountDeletion = StateObject(
            wrappedValue: PasskeysViewModel(authRepository: authRepository))
    }
    /// ADR-0047 / UX spec section 3 — "Autoplay video notes", default OFF:
    /// autoplay downloads media, so it is the user's choice, per device.
    @AppStorage(MediaPrefs.autoplayKey) private var autoplayVideoNotes = false
    @AppStorage(AppearanceSetting.storageKey)
    private var appearanceRaw = AppearanceSetting.system.rawValue

    @State private var confirmingLogout = false
    @State private var confirmingForget = false
    @State private var confirmingDelete = false

    /// "1.2 (34)" from the bundle; "Unknown" when the bundle carries none.
    private var versionLabel: String {
        let info = Bundle.main.infoDictionary ?? [:]
        let short = (info["CFBundleShortVersionString"] as? String)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let build = (info["CFBundleVersion"] as? String)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        switch (short.isEmpty, build.isEmpty) {
        case (false, false): return short == build ? short : "\(short) (\(build))"
        case (false, true): return short
        case (true, false): return build
        case (true, true): return "Unknown"
        }
    }

    private var appearance: Binding<AppearanceSetting> {
        Binding(
            get: { AppearanceSetting(rawValue: appearanceRaw) ?? .system },
            set: { appearanceRaw = $0.rawValue }
        )
    }

    private var displayName: String {
        let name = profiles.ownProfile?.displayName?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !name.isEmpty { return name }
        if !account.username.isEmpty { return account.username }
        return "Guest"
    }

    var body: some View {
        List {
            Section {
                Button { onOpen(.profile) } label: {
                    HStack(spacing: 12) {
                        InitialsAvatar(
                            name: displayName,
                            // Hashed for the tint only; the id itself is never shown.
                            identity: account.accountIdB64,
                            size: 56
                        )
                        VStack(alignment: .leading, spacing: 2) {
                            Text(displayName)
                                .font(.headline)
                                .foregroundStyle(Color.lakkoText)
                            if !account.username.isEmpty {
                                Text("@\(account.username)")
                                    .font(.subheadline)
                                    .foregroundStyle(Color.lakkoTextMuted)
                            }
                        }
                        Spacer()
                        Image(systemName: "chevron.right")
                            .font(.footnote.weight(.semibold))
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    .padding(.vertical, 4)
                }
                .buttonStyle(.plain)
            }

            Section("Account") {
                row("Profile", systemImage: "person.crop.circle") { onOpen(.profile) }
            }

            Section("Security") {
                row("Passkeys", systemImage: "person.badge.key") { onOpen(.passkeys) }
                row("Devices", systemImage: "laptopcomputer.and.iphone") { onOpen(.devices) }
                row("Recovery", systemImage: "lifepreserver") { onOpen(.recoveryFactors) }
            }

            // `docs/ux/chat-folders.md` section 4 — between Notifications
            // (not a section on this client) and Appearance.
            Section {
                row("Chat folders", systemImage: "folder") { onOpen(.chatFolders(reorder: false)) }
            }

            Section("Appearance") {
                Picker(selection: appearance) {
                    ForEach(AppearanceSetting.allCases) { setting in
                        Text(setting.label).tag(setting)
                    }
                } label: {
                    Label("Theme", systemImage: "circle.lefthalf.filled")
                }
            }

            Section("Media") {
                Toggle(isOn: $autoplayVideoNotes) {
                    Label("Autoplay video notes", systemImage: "play.circle")
                }
            }

            Section("Network") {
                row("Anti-censorship", systemImage: "network.badge.shield.half.filled") {
                    onOpen(.antiCensorship)
                }
            }

            Section("About") {
                HStack {
                    Label("Version", systemImage: "info.circle")
                        .foregroundStyle(Color.lakkoText)
                    Spacer()
                    Text(versionLabel)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
                HStack {
                    Label("Licence", systemImage: "doc.text")
                        .foregroundStyle(Color.lakkoText)
                    Spacer()
                    Text("AGPL-3.0-or-later")
                        .foregroundStyle(Color.lakkoTextMuted)
                }
            }

            Section {
                Button(role: .destructive) {
                    confirmingLogout = true
                } label: {
                    Label("Log out", systemImage: "rectangle.portrait.and.arrow.right")
                }
                .disabled(accountDeletion.isBusy)
                Button(role: .destructive) {
                    confirmingForget = true
                } label: {
                    Label("Forget this device", systemImage: "trash")
                }
                .disabled(accountDeletion.isBusy)
                Button(role: .destructive) {
                    confirmingDelete = true
                } label: {
                    HStack {
                        Label("Delete account", systemImage: "person.crop.circle.badge.xmark")
                        if accountDeletion.isBusy {
                            Spacer()
                            ProgressView().controlSize(.small)
                        }
                    }
                }
                .disabled(accountDeletion.isBusy)
                if case .error(let text) = accountDeletion.message {
                    Text(text)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoDanger)
                }
            } header: {
                Text("Danger zone")
            } footer: {
                Text(
                    "Log out keeps your data on this device. Forget this device "
                        + "removes it; you can only come back with your recovery phrase "
                        + "or another signed-in device. Delete account erases the account "
                        + "on the server and wipes this device; this cannot be undone."
                )
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("Settings")
        .navigationBarTitleDisplayMode(.large)
        .confirmationDialog(
            "Log out of Lakko?",
            isPresented: $confirmingLogout,
            titleVisibility: .visible
        ) {
            Button("Log out", role: .destructive, action: onLogout)
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Your messages stay on this device. Log back in with your password or passkey.")
        }
        .confirmationDialog(
            "Forget this device?",
            isPresented: $confirmingForget,
            titleVisibility: .visible
        ) {
            Button("Forget this device", role: .destructive, action: onForgetDevice)
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(
                "Everything on this device is removed and the device leaves your "
                    + "account. You will need your recovery phrase or another "
                    + "signed-in device to use this account here again."
            )
        }
        .confirmationDialog(
            "Delete your account permanently?",
            isPresented: $confirmingDelete,
            titleVisibility: .visible
        ) {
            Button("Delete my account", role: .destructive) {
                Task { await accountDeletion.deleteAccount() }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(
                "Your account is erased from the server (username, keys, recovery "
                    + "factors, backup) and this device is wiped. This cannot be undone."
            )
        }
    }

    private func row(_ title: String, systemImage: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack {
                Label(title, systemImage: systemImage)
                    .foregroundStyle(Color.lakkoText)
                Spacer()
                Image(systemName: "chevron.right")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Color.lakkoTextMuted)
            }
        }
        .buttonStyle(.plain)
    }
}
