// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

// UX `reporting-and-moderation.md` section 4 — what a suspended person sees:
// the one screen that says why, the appeal sheet, and the read-only app's
// banner and composer line.

/// Whether the signed-in account is suspended (ADR-0098 step 3). Read by
/// every surface that sends: composers, call buttons, add-contact and invite
/// entry points. Set at the app root from ``SuspensionCenter``.
private struct AccountSuspendedKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    /// `true` while the account is in suspended mode: nothing that sends is
    /// offered.
    var accountSuspended: Bool {
        get { self[AccountSuspendedKey.self] }
        set { self[AccountSuspendedKey.self] = newValue }
    }
}

/// The lines of the screen, from the loaded details (pure, so the tests
/// can read them).
@MainActor
enum SuspendedScreenCopy {
    /// `Reason: {reason}.`
    static func reasonLine(_ details: SuspensionDetails, l10n: Localization) -> String {
        l10n.t("suspended.self.reason", texts: [
            "reason": l10n.t(SuspensionReasonText.catalogKey(reasonName: details.reasonName)),
        ])
    }

    /// `Until {date}.` or `This suspension has no end date.`
    static func endLine(_ details: SuspensionDetails, l10n: Localization) -> String {
        guard let untilMs = details.untilMs else { return l10n.t("suspended.self.no_end") }
        let date = Date(timeIntervalSince1970: TimeInterval(untilMs) / 1000)
            .formatted(.dateTime.year().month(.abbreviated).day().hour().minute().locale(l10n.locale))
        return l10n.t("suspended.self.until", texts: ["date": date])
    }
}

/// The button between Appeal and Sign out (UX section 4, amended
/// 2026-11-20): `Read my chats` where the account's keys are on this device,
/// `Delete account` where they are not (Settings cannot be reached there).
enum SuspendedScreenAction: Equatable {
    case readChats
    case deleteAccount

    static func middle(keysHere: Bool) -> SuspendedScreenAction {
        keysHere ? .readChats : .deleteAccount
    }
}

/// "Your account is suspended" — in place of the app while suspended mode
/// is on and the person has not chosen to read their chats. A device that
/// holds no keys for the account shows it alone, without "Read my chats".
struct SuspendedScreen: View {
    @ObservedObject var center: SuspensionCenter
    /// The account's keys are on this device, so its chats can be read.
    let keysHere: Bool
    let onSignOut: () -> Void
    /// Delete account, for a device without the account's keys.
    var authRepository: AuthRepository? = nil
    var onAccountDeleted: (String) -> Void = { _ in }

    @ObservedObject private var l10n = Localization.shared
    @State private var appealOpen = false
    @State private var signingOut = false

    var body: some View {
        AuthScreen {
            AuthTitle(title: l10n.t("suspended.self.title"))
            detailsBlock
            Text(l10n.t("suspended.self.body"))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
                .frame(maxWidth: .infinity, alignment: .leading)
            appealBlock
            switch SuspendedScreenAction.middle(keysHere: keysHere) {
            case .readChats:
                AuthSecondaryButton(title: l10n.t("suspended.self.read")) { center.readChats() }
            case .deleteAccount:
                if let authRepository {
                    SuspendedDeleteAccountButton(authRepository: authRepository, onDeleted: onAccountDeleted)
                }
            }
            AuthSecondaryButton(title: l10n.t("suspended.self.sign_out"), disabled: signingOut) {
                guard !signingOut else { return }
                signingOut = true
                onSignOut()
            }
        }
        .sheet(isPresented: $appealOpen) {
            AppealSheet(center: center, onClose: { appealOpen = false })
        }
    }

    @ViewBuilder private var detailsBlock: some View {
        switch center.details {
        case .loading:
            HStack(spacing: 10) {
                ProgressView()
                Text(l10n.t("suspended.self.loading"))
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            .accessibilityElement(children: .combine)
        case .failed:
            AuthHint(text: l10n.t("suspended.self.load_failed"), isError: true)
            AuthSecondaryButton(title: l10n.t("common.retry")) { center.retryDetails() }
        case .loaded(let details):
            VStack(alignment: .leading, spacing: 6) {
                Text(SuspendedScreenCopy.reasonLine(details, l10n: l10n))
                Text(SuspendedScreenCopy.endLine(details, l10n: l10n))
            }
            .font(.body)
            .foregroundStyle(Color.lakkoText)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .combine)
        }
    }

    @ViewBuilder private var appealBlock: some View {
        if case .loaded(let details) = center.details {
            switch details.appeal {
            case .none:
                AuthPrimaryButton(title: l10n.t("suspended.appeal")) {
                    center.beginAppeal()
                    appealOpen = true
                }
            case .sent:
                appealLine(l10n.t("suspended.appeal.sent"))
            case .declined:
                appealLine(l10n.t("suspended.appeal.declined"))
            }
        }
    }

    private func appealLine(_ text: String) -> some View {
        Text(text)
            .font(.subheadline)
            .foregroundStyle(Color.lakkoText)
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: LakkoRadius.control).fill(Color.lakkoSurfaceSunken))
    }
}

/// The appeal sheet: one field (1,000 characters), sent once.
struct AppealSheet: View {
    @ObservedObject var center: SuspensionCenter
    let onClose: () -> Void

    @ObservedObject private var l10n = Localization.shared
    @State private var text = ""
    @FocusState private var focused: Bool

    private var sending: Bool { center.appealPhase == .sending }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    TextField(l10n.t("suspended.appeal.field"), text: $text, axis: .vertical)
                        .lineLimit(5...12)
                        .focused($focused)
                        .authFieldChrome(focused: focused)
                        .disabled(sending)
                        .onChange(of: text) { _, value in
                            let clamped = SuspensionAppeal.clamp(value)
                            if clamped != value { text = clamped }
                        }
                    HStack {
                        AuthHint(text: l10n.t("suspended.appeal.once"))
                        Text("\(SuspensionAppeal.length(text))/\(SuspensionAppeal.maxCharacters)")
                            .font(.footnote.monospacedDigit())
                            .foregroundStyle(Color.lakkoTextMuted)
                            .environment(\.layoutDirection, .leftToRight)
                            .accessibilityHidden(true)
                    }
                    switch center.appealPhase {
                    case .failed:
                        AuthHint(text: l10n.t("suspended.appeal.failed"), isError: true)
                    case .unavailable:
                        AuthHint(text: l10n.t("suspended.appeal.unavailable"), isError: true)
                    case .editing, .sending:
                        EmptyView()
                    }
                    AuthPrimaryButton(
                        title: l10n.t("suspended.appeal.send"),
                        busy: sending,
                        disabled: !SuspensionAppeal.sendable(text)
                    ) {
                        Task {
                            if await center.sendAppeal(text) { onClose() }
                        }
                    }
                }
                .padding(16)
            }
            .background(Color.lakkoBackground)
            .navigationTitle(l10n.t("suspended.appeal.title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(l10n.t("common.cancel")) { onClose() }
                        .disabled(sending)
                }
            }
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
        .interactiveDismissDisabled(sending)
        .onAppear { focused = true }
    }
}

/// The row at the top of the chat list while reading a suspended account's
/// chats: tapping it shows the suspended screen again.
struct SuspendedBanner: View {
    let onOpen: () -> Void
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        Button(action: onOpen) {
            HStack(spacing: 10) {
                LkIconView(.shieldExclamation, size: 20)
                    .foregroundStyle(Color.lakkoDanger)
                    .accessibilityHidden(true)
                Text(l10n.t("suspended.self.title"))
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Color.lakkoText)
                    .frame(maxWidth: .infinity, alignment: .leading)
                LkIconView(.chevronForward, size: 14, relativeTo: .footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .accessibilityHidden(true)
            }
            .padding(12)
            .background(RoundedRectangle(cornerRadius: LakkoRadius.control).fill(Color.lakkoSurfaceSunken))
            .padding(.horizontal, 16)
            .padding(.vertical, 6)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(.isButton)
    }
}

/// The line that takes a composer's slot while suspended: no field, no
/// send control.
struct SuspendedComposerLine: View {
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        Text(l10n.t("suspended.self.composer"))
            .font(.footnote)
            .foregroundStyle(Color.lakkoTextMuted)
            .frame(maxWidth: .infinity, alignment: .center)
            .padding(12)
            .background(Color.lakkoBackground)
    }
}

/// `Delete account` on the suspended screen of a device without the
/// account's keys: the app's usual confirmation (``DeleteAccountDialog``),
/// deleting with the `s1` and the grant it holds, then the usual local
/// cleanup.
struct SuspendedDeleteAccountButton: View {
    @StateObject private var deletion: PasskeysViewModel
    @ObservedObject private var l10n = Localization.shared
    @State private var confirming = false

    init(authRepository: AuthRepository, onDeleted: @escaping (String) -> Void) {
        _deletion = StateObject(wrappedValue: PasskeysViewModel(
            authRepository: authRepository, wipeAfterDeletion: onDeleted))
    }

    var body: some View {
        Button { confirming = true } label: {
            HStack(spacing: 8) {
                if deletion.isBusy { ProgressView().controlSize(.small) }
                Text(l10n.t("suspended.self.delete"))
                    .font(.body.weight(.medium))
            }
            .foregroundStyle(Color.lakkoDanger)
            .frame(maxWidth: .infinity, minHeight: 44)
            .overlay(
                RoundedRectangle(cornerRadius: LakkoRadius.control)
                    .stroke(Color.lakkoDanger, lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
        .disabled(deletion.isBusy)
        .sheet(isPresented: $confirming) {
            DeleteAccountDialog(viewModel: deletion, onClose: { confirming = false })
                .presentationDetents([.medium, .large])
        }
    }
}
