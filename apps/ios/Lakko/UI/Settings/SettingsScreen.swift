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

    /// `docs/ux/text-size.md` — the chat text size, per device.
    @AppStorage(ChatTextSize.storageKey)
    private var chatTextSizeRaw = ChatTextSize.defaultStep.rawValue
    @State private var choosingTextSize = false

    @State private var confirmingLogout = false
    /// Reactions-and-emoji spec section 6 — the quick-reaction picker.
    @State private var choosingQuickReaction = false
    @ObservedObject private var quickReaction = QuickReactionSetting.shared
    /// Section 12 — the Reactions notification switch, mirrored for the view.
    @State private var reactionNotifications = ReactionNotificationPreference.isOn
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
                        LkIconView(.chevronForward, size: 14, relativeTo: .footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    .padding(.vertical, 4)
                }
                .buttonStyle(.plain)
            }

            Section(l10n.t("settings.section.account")) {
                row(l10n.t("settings.section.profile"), icon: .personSquare) { onOpen(.profile) }
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
                row(l10n.t("who_can_find_me.title"), icon: .person2Gear) {
                    onOpen(.whoCanFindMe)
                }
                // ADR-0080 — per device, on by default.
                Toggle(isOn: Binding(
                    get: { deviceCards.showCity },
                    set: { deviceCards.setShowCity($0) }
                )) {
                    Label(l10n.t("settings.privacy.show_city"), icon: .location)
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
                    row(l10n.t("settings.password.title"), icon: .lock) { onOpen(.password) }
                }
                row(l10n.t("settings.passkeys.title"), icon: .passkey) { onOpen(.passkeys) }
                // UX 17.5 — carries the exclamation-mark badge while a device
                // added to the account is pending here.
                DevicesSettingsRow { onOpen(.devices) }
                row(l10n.t("settings.recovery.title"), icon: .lifepreserver) { onOpen(.recoveryFactors) }
                // ADR-0059 section 6 — every group the "Share my channels"
                // switch is on in (docs/ux/feed.md 6.3).
                row(l10n.t("discover.shared.title"), icon: .radiowaves) {
                    onOpen(.channelsIShare)
                }
            }

            // Reactions-and-emoji spec section 12 — one switch, on by
            // default, per device (the notification extension reads it too).
            Section {
                Toggle(isOn: Binding(
                    get: { reactionNotifications },
                    set: { reactionNotifications = $0; ReactionNotificationPreference.isOn = $0 }
                )) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(l10n.t("settings.notifications.reactions"))
                        Text(l10n.t("settings.notifications.reactions_sub"))
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                }
                .tint(Color.lakkoAccent)
            } header: {
                Text(l10n.t("settings.section.notifications"))
            }

            // `docs/ux/chat-folders.md` section 4 — between Notifications
            // (not a section on this client) and Appearance.
            Section {
                row(l10n.t("settings.section.folders"), icon: .folder) { onOpen(.chatFolders(reorder: false)) }
            }

            Section {
                LakkoPicker(
                    l10n.t("settings.appearance.theme"), icon: .appearance,
                    selection: appearance,
                    options: AppearanceSetting.allCases.map {
                        LakkoPickerOption(value: $0, label: l10n.t($0.labelKey))
                    })
                // `docs/ux/text-size.md` — below Theme, the current step as
                // trailing text; opening it shows the steps and a preview.
                Button { choosingTextSize = true } label: {
                    HStack {
                        Label(l10n.t("settings.text_size.title"), icon: .textSize)
                            .foregroundStyle(Color.lakkoText)
                        Spacer()
                        Text(l10n.t(ChatTextSize.from(stored: chatTextSizeRaw).labelKey))
                            .foregroundStyle(Color.lakkoTextMuted)
                        LkIconView(.chevronForward, size: 14, relativeTo: .footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("settings-text-size")
                // Section 6 (D4) — the quick reaction: the current emoji,
                // tapping opens the picker; a pick saves and closes.
                Button { choosingQuickReaction = true } label: {
                    HStack {
                        Label(l10n.t("settings.appearance.quick_reaction"), icon: .thumbsup)
                            .foregroundStyle(Color.lakkoText)
                        Spacer()
                        Text(quickReaction.current).font(.title3)
                        LkIconView(.chevronForward, size: 14, relativeTo: .footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                }
                .buttonStyle(.plain)
            } header: {
                Text(l10n.t("settings.section.appearance"))
            } footer: {
                Text(l10n.t("settings.appearance.quick_reaction_sub"))
            }
            .sheet(isPresented: $choosingTextSize) {
                NavigationStack {
                    TextSizeScreen(raw: $chatTextSizeRaw)
                        .navigationTitle(l10n.t("settings.text_size.title"))
                        .navigationBarTitleDisplayMode(.inline)
                        .toolbar {
                            ToolbarItem(placement: .topBarTrailing) {
                                Button(l10n.t("common.done")) { choosingTextSize = false }
                            }
                        }
                }
                .presentationDetents([.medium, .large])
            }
            .sheet(isPresented: $choosingQuickReaction) {
                NavigationStack {
                    EmojiTab(onPick: { emoji in
                        quickReaction.choose(emoji)
                        choosingQuickReaction = false
                    })
                    .navigationTitle(l10n.t("settings.appearance.quick_reaction"))
                    .navigationBarTitleDisplayMode(.inline)
                    .toolbar {
                        ToolbarItem(placement: .topBarTrailing) {
                            Button(l10n.t("common.done")) { choosingQuickReaction = false }
                        }
                    }
                }
                .presentationDetents([.medium, .large])
            }

            // UX spec `docs/ux/localization.md` section 1: a top-level row
            // after Appearance, trailing text = the current language in its
            // own name.
            Section {
                Button { onOpen(.language) } label: {
                    HStack {
                        Label(l10n.t("settings.language.title"), icon: .globe)
                            .foregroundStyle(Color.lakkoText)
                        Spacer()
                        Text(l10n.ownName(of: l10n.language))
                            .foregroundStyle(Color.lakkoTextMuted)
                        LkIconView(.chevronForward, size: 14, relativeTo: .footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                }
                .buttonStyle(.plain)
            }

            Section {
                Toggle(isOn: $autoplayVideoNotes) {
                    Label(l10n.t("settings.media.autoplay_video_notes.title"), icon: .playCircle)
                }
            } header: {
                Text(l10n.t("settings.section.media"))
            } footer: {
                Text(l10n.t("settings.media.autoplay_video_notes.sub"))
            }

            Section(l10n.t("settings.section.network")) {
                row(l10n.t("settings.anti_censorship.title"), icon: .networkShield) {
                    onOpen(.antiCensorship)
                }
            }

            Section(l10n.t("settings.section.about")) {
                HStack {
                    Label(l10n.t("settings.about.version"), icon: .infoCircle)
                        .foregroundStyle(Color.lakkoText)
                    Spacer()
                    Text(versionLabel)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
                HStack {
                    Label(l10n.t("settings.about.licence"), icon: .docText)
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
                    Label(l10n.t("settings.account.log_out"), icon: .logout)
                }
                .disabled(accountDeletion.isBusy)
                Button(role: .destructive) {
                    confirmingForget = true
                } label: {
                    Label(l10n.t("settings.account.forget_device.label"), icon: .trash)
                }
                .disabled(accountDeletion.isBusy)
                Button(role: .destructive) {
                    confirmingDelete = true
                } label: {
                    HStack {
                        Label(l10n.t("settings.account.delete.confirm"), icon: .personCircleXmark)
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
        // Settings opened: the phrase-saved flag and the protection rows
        // follow the server. On the list, not on a row of it: a row with no
        // content still takes a row's height.
        .task { await accountProtection?.refresh() }
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

    private func row(_ title: String, icon: LkIcon, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack {
                Label(title, icon: icon)
                    .foregroundStyle(Color.lakkoText)
                Spacer()
                LkIconView(.chevronForward, size: 14, relativeTo: .footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
        }
        .buttonStyle(.plain)
    }
}

/// The `Devices` row of Settings (UX 17.5): the same row as its neighbours,
/// with the exclamation-mark badge while a device added to the account is
/// pending here. Its own view observes the warning, so Settings is not
/// re-evaluated by it.
private struct DevicesSettingsRow: View {
    let action: () -> Void
    @ObservedObject private var notice = NewDeviceNoticeCenter.shared
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        let attention = notice.badgeVisible
        Button(action: action) {
            HStack {
                Label(l10n.t("settings.devices.title"), icon: .devices)
                    .foregroundStyle(Color.lakkoText)
                Spacer()
                if attention { NewDeviceAttentionBadge() }
                LkIconView(.chevronForward, size: 14, relativeTo: .footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(
            attention ? l10n.t("settings.devices.attention_a11y") : l10n.t("settings.devices.title"))
    }
}

/// `docs/ux/text-size.md` — the three steps as one choice with a check mark,
/// under a preview drawn at the selected step: an incoming bubble with a
/// reaction chip under it and an outgoing bubble. A choice applies at once.
struct TextSizeScreen: View {
    @Binding var raw: String
    @ObservedObject private var l10n = Localization.shared

    private var selected: ChatTextSize { ChatTextSize.from(stored: raw) }

    var body: some View {
        List {
            Section {
                TextSizePreview()
                    .environment(\.chatTextScale, selected.factor)
                    .listRowInsets(EdgeInsets(top: 12, leading: 12, bottom: 12, trailing: 12))
                    .accessibilityElement(children: .combine)
            }
            Section {
                ForEach(ChatTextSize.allCases) { step in
                    Button {
                        raw = step.rawValue
                    } label: {
                        HStack {
                            Text(l10n.t(step.labelKey)).foregroundStyle(Color.lakkoText)
                            Spacer()
                            if step == selected {
                                LkIconView(.checkmark, size: 20)
                                    .foregroundStyle(Color.lakkoAccent)
                            }
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(step == selected ? .isSelected : [])
                    .accessibilityIdentifier("text-size-\(step.rawValue)")
                }
            }
        }
    }
}

/// The two bubbles of the preview, drawn with the chat's own fonts and chip.
private struct TextSizePreview: View {
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        VStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 4) {
                bubble(l10n.t("settings.text_size.preview_incoming"), outgoing: false)
                ReactionChipLabel(emoji: "👍", content: .count(1.formatted(.number.locale(l10n.locale))))
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            bubble(l10n.t("settings.text_size.preview_outgoing"), outgoing: true)
                .frame(maxWidth: .infinity, alignment: .trailing)
        }
    }

    private func bubble(_ text: String, outgoing: Bool) -> some View {
        Text(text)
            .chatFont(.body)
            .padding(.vertical, 8)
            .padding(.horizontal, 12)
            .background(outgoing ? Color.lakkoAccent : Color.lakkoSurfaceSunken)
            .foregroundStyle(outgoing ? Color.lakkoAccentForeground : Color.lakkoText)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.bubble, style: .continuous))
    }
}
