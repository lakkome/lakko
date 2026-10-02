// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LocalAuthentication
import SwiftUI

/// The device-authentication gate in front of "Show recovery phrase": Face
/// ID, Touch ID or the device passcode (`deviceOwnerAuthentication`). A
/// seam so the screen's rule can be tested without a system prompt.
struct DeviceOwnerGate {

    enum Outcome: Equatable {
        /// The device confirmed its owner.
        case confirmed
        /// The person dismissed the prompt: nothing to say.
        case cancelled
        /// The device has no passcode, so it cannot confirm anyone.
        case unavailable
        /// The check failed.
        case failed
    }

    var authenticate: (_ reason: String) async -> Outcome

    /// The system prompt.
    static let live = DeviceOwnerGate { reason in
        let context = LAContext()
        var policyError: NSError?
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &policyError) else {
            return .unavailable
        }
        do {
            return try await context.evaluatePolicy(
                .deviceOwnerAuthentication, localizedReason: reason) ? .confirmed : .failed
        } catch let error as LAError {
            switch error.code {
            case .userCancel, .appCancel, .systemCancel: return .cancelled
            case .passcodeNotSet: return .unavailable
            default: return .failed
            }
        } catch {
            return .failed
        }
    }
}

/// Settings > Security > Recovery: what brings the account back, as it
/// stands on this device. Mirror of `apps/web/src/settings/RecoveryPage.tsx`.
///
///   - "Brings back your chats on a new device": the recovery phrase (shown
///     only after the device confirmed its owner), passkeys (how many, how
///     many restore keys), other devices.
///   - "Signs you in when you forget your password": the three-word check
///     (informational: there is nothing to set up) and the authenticator
///     app.
struct RecoverySettingsScreen: View {
    @ObservedObject var authRepository: AuthRepository
    /// Push a route on the Settings stack.
    var onOpen: (TabRoute) -> Void = { _ in }
    var gate: DeviceOwnerGate = .live

    /// The phrase while it is on screen; `nil` when hidden.
    @State private var shownPhrase: String?
    @State private var phraseError: UiText?
    @State private var checking = false
    /// `(passkeys, passkeys that restore keys)`; `nil` until loaded.
    @State private var passkeyCounts: (total: Int, restoring: Int)?
    @Environment(\.scenePhase) private var scenePhase
    @ObservedObject private var l10n = Localization.shared

    private var account: SessionAccount? {
        guard case .session(let account) = authRepository.state else { return nil }
        return account
    }

    private var phraseStoredHere: Bool {
        guard let account else { return false }
        return DevicePhrase.isStored(accountIdB64: account.accountIdB64)
    }

    /// Whether the three-word route works on this device for this account:
    /// the device holds the phrase, and the account has a username. The keys
    /// are here (this is a session).
    private var threeWordsHere: Bool {
        ThreeWords.offers(
            cachedUsername: account?.username,
            phraseOnDevice: phraseStoredHere,
            keysOnDevice: true
        )
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                card(titleKey: "settings.recovery.group.keys") {
                    phraseRow
                    Divider()
                    passkeysRow
                    Divider()
                    devicesRow
                }
                card(titleKey: "settings.recovery.group.signin") {
                    wordsRow
                }
                if let session = DeviceSession.current(authRepository) {
                    card(titleKey: nil) {
                        VStack(alignment: .leading, spacing: 12) {
                            Label(l10n.t("settings.recovery.totp.title"), systemImage: "lock.rotation")
                                .font(.headline)
                            TotpEnrollView(sessionToken: session.sessionToken)
                        }
                    }
                } else {
                    Text(l10n.t("settings.recovery.sign_in_first"))
                        .foregroundStyle(Color.lakkoDanger)
                }
            }
            .padding(16)
        }
        .background(Color.lakkoBackground.ignoresSafeArea())
        .navigationTitle(l10n.t("settings.recovery.title"))
        .navigationBarTitleDisplayMode(.inline)
        .task { await loadPasskeyCounts() }
        // The words leave the screen when the app does.
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { shownPhrase = nil }
        }
        .onDisappear { shownPhrase = nil }
    }

    // MARK: - Rows

    @ViewBuilder
    private var phraseRow: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(l10n.t("recovery.method.phrase.title"), systemImage: "key")
                .font(.headline)
            Text(l10n.t("settings.recovery.phrase.sub"))
                .font(.footnote)
                .foregroundStyle(Color.lakkoTextMuted)
            HStack(spacing: 8) {
                Image(systemName: phraseStoredHere ? "checkmark.circle.fill" : "exclamationmark.circle")
                    .foregroundStyle(phraseStoredHere ? Color.lakkoSuccess : Color.lakkoWarning)
                Text(l10n.t(phraseStoredHere
                     ? "settings.recovery.phrase.saved_here"
                     : "settings.recovery.phrase.not_saved_here"))
                    .font(.subheadline)
            }
            if let shownPhrase {
                AuthHint(text: l10n.t("settings.recovery.phrase.confirm_show"))
                PhraseGrid(words: PhraseWords.words(of: shownPhrase))
                AuthHint(text: l10n.t("recovery_phrase.save.anyone_with_words"))
                AuthSecondaryButton(title: l10n.t("settings.recovery.phrase.hide")) {
                    self.shownPhrase = nil
                }
            } else if phraseStoredHere {
                if let phraseError {
                    AuthHint(text: phraseError.text, isError: true)
                }
                AuthSecondaryButton(
                    title: l10n.t(checking ? "common.checking" : "settings.recovery.phrase.show"),
                    systemImage: "eye",
                    disabled: checking,
                    action: showPhrase
                )
            }
        }
    }

    private var passkeysRow: some View {
        Button { onOpen(.passkeys) } label: {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 6) {
                    Label(l10n.t("settings.passkeys.title"), systemImage: "person.badge.key")
                        .font(.headline)
                        .foregroundStyle(Color.lakkoText)
                    Text(l10n.t("settings.recovery.passkeys.sub"))
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .multilineTextAlignment(.leading)
                    if let passkeyCounts {
                        Text(passkeyStatus(passkeyCounts))
                            .font(.subheadline)
                            .foregroundStyle(Color.lakkoText)
                    }
                }
                Spacer()
                Image(systemName: "chevron.forward")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private func passkeyStatus(_ counts: (total: Int, restoring: Int)) -> String {
        guard counts.total > 0 else { return l10n.t("settings.recovery.passkeys.none") }
        return l10n.t("settings.recovery.passkeys.count", counts: ["count": UInt64(counts.total)])
            + " "
            + l10n.t("settings.recovery.passkeys.restoring", counts: ["count": UInt64(counts.restoring)])
    }

    private var devicesRow: some View {
        Button { onOpen(.devices) } label: {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 6) {
                    Label(l10n.t("settings.recovery.devices.title"), systemImage: "laptopcomputer.and.iphone")
                        .font(.headline)
                        .foregroundStyle(Color.lakkoText)
                    Text(l10n.t("settings.security.devices.sub"))
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .multilineTextAlignment(.leading)
                }
                Spacer()
                Image(systemName: "chevron.forward")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    /// Informational: the three-word check needs no setup and has no switch.
    private var wordsRow: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(l10n.t("recovery.method.words.title"), systemImage: "text.word.spacing")
                .font(.headline)
            Text(l10n.t(threeWordsHere
                 ? "settings.recovery.words.on_this_device"
                 : "settings.recovery.words.not_on_this_device"))
                .font(.footnote)
                .foregroundStyle(Color.lakkoTextMuted)
        }
    }

    // MARK: - Card

    @ViewBuilder
    private func card<Content: View>(
        titleKey: String?,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            if let titleKey {
                Text(l10n.t(titleKey))
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Color.lakkoTextMuted)
                    .accessibilityAddTraits(.isHeader)
            }
            VStack(alignment: .leading, spacing: 14) {
                content()
            }
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.lakkoSurface)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
            .overlay(
                RoundedRectangle(cornerRadius: LakkoRadius.card)
                    .stroke(Color.lakkoBorder, lineWidth: 1)
            )
        }
    }

    // MARK: - Actions

    /// "Show recovery phrase": the device confirms its owner first (Face
    /// ID, Touch ID or the passcode). The phrase is read from the device,
    /// never from a server.
    private func showPhrase() {
        guard !checking else { return }
        checking = true
        phraseError = nil
        Task {
            let outcome = await gate.authenticate(l10n.t("settings.recovery.phrase.device_auth_reason"))
            checking = false
            switch outcome {
            case .confirmed:
                if let phrase = authRepository.devicePhrase() {
                    shownPhrase = phrase
                } else {
                    phraseError = .key("settings.recovery.phrase.not_saved_here")
                }
            case .cancelled:
                break
            case .unavailable:
                phraseError = .key("settings.recovery.phrase.device_auth_unavailable")
            case .failed:
                phraseError = .key("settings.recovery.phrase.device_auth_failed")
            }
        }
    }

    private func loadPasskeyCounts() async {
        guard let token = authRepository.identitySecrets?.sessionToken else { return }
        let client = WebauthnClient()
        client.setSessionToken(token)
        guard let credentials = try? await client.listCredentials().credentials else { return }
        let withKeys = (try? await AuthClient().passkeyVaultList(sessionToken: token)) ?? []
        let ids = Set(credentials.map(\.credential_id_b64))
        passkeyCounts = (credentials.count, withKeys.filter { ids.contains($0) }.count)
    }
}
