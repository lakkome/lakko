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
    /// ADR-0061 — the interface language; every catalog string here follows it.
    @ObservedObject private var l10n = Localization.shared
    /// ADR-0080 — the "Show my city to my other devices" switch.
    @ObservedObject private var deviceCards = DeviceCardCenter.shared
    /// The existing delete-account flow (auth `DELETE /account`, the passkey
    /// action ceremony on a 403, the discovery purge, then the local wipe)
    /// lives on ``PasskeysViewModel``; the danger zone drives that same
    /// object rather than a second implementation.
    @StateObject private var accountDeletion: PasskeysViewModel

    /// ADR-0075 — what protects the account: the status line, the banner
    /// and the two rows of the Account section.
    private let accountProtection: AccountProtectionCenter?

    init(
        account: SessionAccount,
        authRepository: AuthRepository,
        accountProtection: AccountProtectionCenter? = nil,
        onOpen: @escaping (TabRoute) -> Void,
        onLogout: @escaping () -> Void,
        onForgetDevice: @escaping () -> Void,
        onAccountDeleted: @escaping (String) -> Void
    ) {
        self.account = account
        self.accountProtection = accountProtection
        self.onOpen = onOpen
        self.onLogout = onLogout
        self.onForgetDevice = onForgetDevice
        self._accountDeletion = StateObject(
            wrappedValue: PasskeysViewModel(
                authRepository: authRepository, wipeAfterDeletion: onAccountDeleted))
    }
    /// ADR-0047 / UX spec section 3 — "Autoplay video notes", default OFF:
    /// autoplay downloads media, so it is the user's choice, per device.
    @AppStorage(MediaPrefs.autoplayKey) private var autoplayVideoNotes = false
    @AppStorage(AppearanceSetting.storageKey)
    private var appearanceRaw = AppearanceSetting.system.rawValue

    @State private var confirmingLogout = false
    @State private var confirmingForget = false
    @State private var confirmingDelete = false

    /// "1.2 (34)" from the bundle; the catalog's "unknown" when the bundle
    /// carries none.
    /// The signing date of the active translation update, in the
    /// interface locale.
    static func translationsDate(_ millis: Int64, locale: Locale) -> String {
        Date(timeIntervalSince1970: TimeInterval(millis) / 1000)
            .formatted(.dateTime.year().month(.abbreviated).day().locale(locale))
    }

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
        case (true, true): return l10n.t("common.unknown")
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
        return l10n.t("settings.me.guest")
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
                            size: 56,
                            photo: .own
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
                        Image(systemName: "chevron.forward")
                            .font(.footnote.weight(.semibold))
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    .padding(.vertical, 4)
                }
                .buttonStyle(.plain)
            }

            Section(l10n.t("settings.section.account")) {
                row(l10n.t("settings.section.profile"), systemImage: "person.crop.circle") { onOpen(.profile) }
                // UX 19.1 / 19.2 / 19.5 — its own view observes the centre,
                // so this screen is not re-evaluated by it.
                if let accountProtection {
                    AccountProtectionSection(
                        center: accountProtection,
                        hasUsername: !account.username.isEmpty,
                        onOpen: onOpen)
                }
            }

            // `docs/ux/who-can-find-me.md` section 2 — Privacy, after Profile.
            Section {
                row(l10n.t("who_can_find_me.title"), systemImage: "person.2.badge.gearshape") {
                    onOpen(.whoCanFindMe)
                }
                // ADR-0080 — per device, on by default.
                Toggle(isOn: Binding(
                    get: { deviceCards.showCity },
                    set: { deviceCards.setShowCity($0) }
                )) {
                    Label(l10n.t("settings.privacy.show_city"), systemImage: "mappin.and.ellipse")
                        .foregroundStyle(Color.lakkoText)
                }
            } header: {
                Text(l10n.t("settings.section.privacy"))
            } footer: {
                Text(l10n.t("settings.privacy.show_city.sub"))
            }

            Section(l10n.t("settings.section.security")) {
                // Choosing a new password needs the login handle of a
                // username, so a guest account has no row.
                if !account.username.isEmpty {
                    row(l10n.t("settings.password.title"), systemImage: "lock") { onOpen(.password) }
                }
                row(l10n.t("settings.passkeys.title"), systemImage: "person.badge.key") { onOpen(.passkeys) }
                row(l10n.t("settings.devices.title"), systemImage: "laptopcomputer.and.iphone") {
                    onOpen(.devices)
                }
                row(l10n.t("settings.recovery.title"), systemImage: "lifepreserver") { onOpen(.recoveryFactors) }
                // ADR-0059 section 6 — every group the "Share my channels"
                // switch is on in (docs/ux/feed.md 6.3).
                row(l10n.t("discover.shared.title"), systemImage: "dot.radiowaves.left.and.right") {
                    onOpen(.channelsIShare)
                }
            }

            // `docs/ux/chat-folders.md` section 4 — between Notifications
            // (not a section on this client) and Appearance.
            Section {
                row(l10n.t("settings.section.folders"), systemImage: "folder") { onOpen(.chatFolders(reorder: false)) }
            }

            Section(l10n.t("settings.section.appearance")) {
                LakkoPicker(
                    l10n.t("settings.appearance.theme"), systemImage: "circle.lefthalf.filled",
                    selection: appearance,
                    options: AppearanceSetting.allCases.map {
                        LakkoPickerOption(value: $0, label: l10n.t($0.labelKey))
                    })
            }

            // UX spec `docs/ux/localization.md` section 1: a top-level row
            // after Appearance, trailing text = the current language in its
            // own name.
            Section {
                Button { onOpen(.language) } label: {
                    HStack {
                        Label(l10n.t("settings.language.title"), systemImage: "globe")
                            .foregroundStyle(Color.lakkoText)
                        Spacer()
                        Text(l10n.ownName(of: l10n.language))
                            .foregroundStyle(Color.lakkoTextMuted)
                        Image(systemName: "chevron.forward")
                            .font(.footnote.weight(.semibold))
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                }
                .buttonStyle(.plain)
            }

            Section {
                Toggle(isOn: $autoplayVideoNotes) {
                    Label(l10n.t("settings.media.autoplay_video_notes.title"), systemImage: "play.circle")
                }
            } header: {
                Text(l10n.t("settings.section.media"))
            } footer: {
                Text(l10n.t("settings.media.autoplay_video_notes.sub"))
            }

            Section(l10n.t("settings.section.network")) {
                row(l10n.t("settings.anti_censorship.title"), systemImage: "network.badge.shield.half.filled") {
                    onOpen(.antiCensorship)
                }
            }

            Section(l10n.t("settings.section.about")) {
                HStack {
                    Label(l10n.t("settings.about.version"), systemImage: "info.circle")
                        .foregroundStyle(Color.lakkoText)
                    Spacer()
                    Text(versionLabel)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
                HStack {
                    Label(l10n.t("settings.about.licence"), systemImage: "doc.text")
                        .foregroundStyle(Color.lakkoText)
                    Spacer()
                    Text("AGPL-3.0-or-later")
                        .foregroundStyle(Color.lakkoTextMuted)
                }
                // ADR-0080 — the city database's attribution (CC BY 4.0).
                Text(l10n.t("settings.about.city_data"))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
                // ADR-0061 section 5 — only while a signed translation
                // update is active (UX spec section 7).
                if let signedAt = l10n.translationsSignedAtMillis {
                    Text(l10n.t(
                        "settings.about.translations_updated",
                        texts: ["date": Self.translationsDate(signedAt, locale: l10n.locale)]
                    ))
                    .foregroundStyle(Color.lakkoTextMuted)
                }
            }

            Section {
                Button(role: .destructive) {
                    confirmingLogout = true
                } label: {
                    Label(l10n.t("settings.account.log_out"), systemImage: "rectangle.portrait.and.arrow.right")
                }
                .disabled(accountDeletion.isBusy)
                Button(role: .destructive) {
                    confirmingForget = true
                } label: {
                    Label(l10n.t("settings.account.forget_device.label"), systemImage: "trash")
                }
                .disabled(accountDeletion.isBusy)
                Button(role: .destructive) {
                    confirmingDelete = true
                } label: {
                    HStack {
                        Label(l10n.t("settings.account.delete.confirm"), systemImage: "person.crop.circle.badge.xmark")
                        if accountDeletion.isBusy {
                            Spacer()
                            ProgressView().controlSize(.small)
                        }
                    }
                }
                .disabled(accountDeletion.isBusy)
            } header: {
                Text(l10n.t("settings.section.danger_zone"))
            } footer: {
                Text(l10n.t("settings.account.danger_zone.footer"))
            }
        }
        .listStyle(.insetGrouped)
        // A UIKit-backed List keeps the mirroring it was built with when the
        // environment's layout direction flips under it (its glyphs come out
        // mirrored), so the list is rebuilt when the direction changes.
        .id(l10n.layoutDirection)
        .navigationTitle(l10n.t("nav.settings"))
        .navigationBarTitleDisplayMode(.large)
        .lakkoDialog(
            isPresented: $confirmingLogout,
            title: l10n.t("settings.account.log_out.title"),
            message: l10n.t("settings.account.log_out.body")
        ) {
            [.danger(l10n.t("settings.account.log_out"), perform: onLogout), .cancel()]
        }
        .lakkoDialog(
            isPresented: $confirmingForget,
            title: l10n.t("settings.account.forget_device.title"),
            message: l10n.t("settings.account.forget_device.body_device")
        ) {
            [.danger(l10n.t("settings.account.forget_device.confirm"), perform: onForgetDevice), .cancel()]
        }
        // UX 17.3 — the app's own dialog: it carries the proof block.
        .sheet(isPresented: $confirmingDelete) {
            DeleteAccountDialog(viewModel: accountDeletion, onClose: { confirmingDelete = false })
                .presentationDetents([.medium, .large])
        }
    }

    private func row(_ title: String, systemImage: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack {
                Label(title, systemImage: systemImage)
                    .foregroundStyle(Color.lakkoText)
                Spacer()
                Image(systemName: "chevron.forward")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Color.lakkoTextMuted)
            }
        }
        .buttonStyle(.plain)
    }
}
