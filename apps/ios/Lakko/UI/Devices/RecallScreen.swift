// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// "Was it you?" (docs/ux/identity-recovery.md section 18.4, ADR-0074
/// section 1): what an old device shows after the account's keys were reset
/// on another device. It replaces the whole app, at launch too; the chats and
/// contacts are already deleted when it appears.
///
/// Three steps on ONE screen, never a system dialog:
///
///   * the question, with **Yes, it was me** and **No, take the account
///     back** (the primary; enabled only once the server confirmed it is
///     possible);
///   * the **Yes** confirmation (`Delete and sign in` / `Go back`);
///   * the take-back form: the password from before the reset, a new
///     password twice, the three effect lines.
///
/// What it shows is ``RecallScreenState``; what a tap does is
/// ``AuthRepository``'s. The view keeps only the typed passwords, which are
/// dropped when the form is left.
struct RecallScreen: View {
    @ObservedObject var authRepository: AuthRepository
    let state: RecallScreenState

    @State private var oldPassword = ""
    @State private var newPassword = ""
    @State private var repeated = ""
    /// UX 19.7 — `Use my recovery phrase instead`: the 12 words stand where
    /// the old password does.
    @State private var usePhrase = false
    @State private var phrase = ""
    @StateObject private var wait = RetryCountdown()
    @ObservedObject private var l10n = Localization.shared
    @Environment(\.scenePhase) private var scenePhase
    @AccessibilityFocusState private var focusOnNo: Bool

    private var displayedAccount: String {
        state.username.isEmpty ? l10n.t("devices.signed_out.this_account") : "@\(state.username)"
    }

    private func date(_ ms: Int64) -> String {
        RecallCopy.date(ms: ms, locale: l10n.locale)
    }

    var body: some View {
        AuthScreen {
            switch state.step {
            case .question: question
            case .confirmYes: confirmYes
            case .form: form
            }
        }
        // The first check, and the re-check while the server could not be
        // asked: every few seconds, and whenever the app comes forward.
        .task(id: state.check == .offline) {
            if state.check == .checking { await authRepository.refreshRecallStatus() }
            while state.check == .offline, !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 5_000_000_000)
                guard !Task.isCancelled else { return }
                await authRepository.refreshRecallStatus()
            }
        }
        .onChange(of: scenePhase) { _, phase in
            guard phase == .active else { return }
            Task { await authRepository.refreshRecallStatus() }
        }
        .onChange(of: state.form) { _, status in
            if case .rateLimited(let secs) = status, let secs, secs > 0 {
                wait.start(forSecs: secs)
            }
        }
        .onChange(of: state.step) { _, step in
            // The typed passwords do not outlive the form.
            if step != .form {
                oldPassword = ""
                newPassword = ""
                repeated = ""
                phrase = ""
                usePhrase = false
            }
        }
    }

    // MARK: - The question

    @ViewBuilder private var question: some View {
        VStack(alignment: .leading, spacing: 12) {
            AuthTitle(
                title: l10n.t(RecallCopy.title),
                subtitle: l10n.t(
                    RecallCopy.body,
                    texts: ["name": displayedAccount, "date": date(state.resetAtMs)])
            )
            Text(l10n.t(RecallCopy.question))
                .font(.headline)
                .foregroundStyle(Color.lakkoText)
        }
        // Title, body and question are one announcement.
        .accessibilityElement(children: .combine)

        // Stacked, full width, neither styled as destructive; **No** is the
        // primary.
        AuthSecondaryButton(title: l10n.t(RecallCopy.yes)) { authRepository.recallTapYes() }
        AuthPrimaryButton(title: l10n.t(RecallCopy.no), disabled: !state.canTakeBack) {
            authRepository.recallTapNo()
        }
        .accessibilityFocused($focusOnNo)
        .onAppear { focusOnNo = true }

        switch state.check {
        case .checking:
            HStack(spacing: 10) {
                ProgressView().controlSize(.small)
                Text(l10n.t(RecallCopy.checking))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            .accessibilityElement(children: .combine)
        case .offline:
            AuthHint(text: l10n.t(RecallCopy.offline), isError: true)
        case .open(let window):
            AuthHint(text: l10n.t(RecallCopy.until, texts: ["date": date(window.expiresAtMs)]))
        }
    }

    // MARK: - Yes, it was me

    @ViewBuilder private var confirmYes: some View {
        AuthTitle(
            title: l10n.t(RecallCopy.title),
            subtitle: l10n.t(RecallCopy.yesConfirm)
        )
        .accessibilityElement(children: .combine)
        AuthPrimaryButton(title: l10n.t(RecallCopy.yesDelete)) { authRepository.recallConfirmYes() }
        AuthSecondaryButton(title: l10n.t(RecallCopy.yesBack)) { authRepository.recallBack() }
    }

    // MARK: - No, take the account back

    private var locked: Bool { RecallForm.locked(state.form) }

    /// Whether the server said this anchor accepts the phrase as the proof.
    private var phraseArm: Bool {
        if case .open(let window) = state.check { return window.phraseArm }
        return false
    }

    private var phraseProof: Bool { usePhrase && phraseArm }

    private var canSubmit: Bool {
        if phraseProof {
            return RecallForm.canSubmit(
                phrase: phrase, newPassword: newPassword, repeated: repeated,
                status: state.form, waiting: wait.waiting)
        }
        return RecallForm.canSubmit(
            oldPassword: oldPassword, newPassword: newPassword, repeated: repeated,
            status: state.form, waiting: wait.waiting)
    }

    @ViewBuilder private var form: some View {
        AuthTitle(title: l10n.t(RecallCopy.formTitle), subtitle: l10n.t(RecallCopy.formLede))

        // None is pre-filled. The first offers the saved password, the other
        // two a new one.
        if phraseProof {
            PhraseEntryGrid(phrase: $phrase, disabled: locked)
        } else {
            AuthSecureField(
                label: l10n.t(RecallCopy.oldPassword),
                text: $oldPassword,
                contentType: .password,
                disabled: locked
            )
        }
        if let key = RecallCopy.formErrorKey(state.form, phraseProof: phraseProof) {
            AuthHint(text: l10n.t(key), isError: true)
        }
        if phraseArm, !phraseProof {
            // Only for an account that had a saved phrase before the reset.
            AuthLinkRow(title: l10n.t(RecallCopy.usePhrase), disabled: locked) { usePhrase = true }
        }
        if case .wrongPassword(let triesLeft) = state.form, let triesLeft {
            AuthHint(text: l10n.t(RecallCopy.triesLeft, counts: ["count": UInt64(max(0, triesLeft))]))
        }
        if case .rateLimited(let secs) = state.form {
            if wait.waiting {
                WaitNotice(countdown: wait, messageKey: RecallCopy.rateLimited)
            } else if secs == nil {
                // The server named no wait: the plain line, no countdown.
                AuthHint(text: l10n.t("common.error.too_many_attempts"), isError: true)
            }
        }

        AuthSecureField(
            label: l10n.t(RecallCopy.newPassword),
            text: $newPassword,
            contentType: .newPassword,
            disabled: locked
        )
        AuthHint(text: l10n.t(
            "auth.password.hint", counts: ["count": UInt64(NewPasswordRule.minLength)]))
        AuthSecureField(
            label: l10n.t(RecallCopy.repeatPassword),
            text: $repeated,
            contentType: .newPassword,
            disabled: locked,
            submitLabel: .go,
            onSubmit: submit
        )
        if !repeated.isEmpty, RecallForm.problem(
            oldPassword: oldPassword, newPassword: newPassword, repeated: repeated) == .mismatch {
            AuthHint(text: l10n.t("auth.password.error.mismatch"), isError: true)
        }

        VStack(alignment: .leading, spacing: 8) {
            effect(RecallCopy.effectKeys)
            effect(RecallCopy.effectFactors)
            effect(RecallCopy.effectMessages)
        }

        AuthPrimaryButton(
            title: l10n.t(RecallCopy.submit),
            busy: state.form == .working,
            busyTitle: l10n.t(RecallCopy.working),
            disabled: !canSubmit,
            action: submit
        )
        // Back stays usable while rate limited; not while working.
        AuthSecondaryButton(title: l10n.t(RecallCopy.back), disabled: locked) {
            authRepository.recallBack()
        }
    }

    private func effect(_ key: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(verbatim: "-")
                .foregroundStyle(Color.lakkoTextMuted)
                .accessibilityHidden(true)
            Text(l10n.t(key))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoText)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func submit() {
        guard canSubmit else { return }
        let proof: RecallProof = phraseProof ? .phrase(phrase) : .password(Data(oldPassword.utf8))
        let new = Data(newPassword.utf8)
        Task { await authRepository.recallTakeBack(proof: proof, newPassword: new) }
    }
}

/// UX 18.4, "Done": the one line at the top of the chat list for the session
/// the account was taken back in. Dismissible; gone with the session.
struct RecalledNoticeBanner: View {
    @ObservedObject var authRepository: AuthRepository
    /// UX 18.3 — `Show new phrase` on the "keys were changed" line.
    var onShowNewPhrase: () -> Void = {}
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        // UX 18.3 — a KEPT device after "remove and change keys": the app as
        // it was, with every chat, and this one dismissible line. Kept across
        // launches until dismissed.
        if authRepository.keysRotatedNotice {
            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .top, spacing: 12) {
                    LkIconView(.key, size: 20, relativeTo: .subheadline)
                        .foregroundStyle(Color.lakkoAccent)
                        .accessibilityHidden(true)
                    Text(l10n.t(KeysRotatedCopy.notice))
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoText)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    Button(l10n.t("common.dismiss")) { authRepository.dismissKeysRotatedNotice() }
                        .font(.footnote)
                        .frame(minWidth: 44, minHeight: 44)
                        .contentShape(Rectangle())
                }
                // ADR-0075 — only a phrase-protected account has words to show.
                if authRepository.sessionPhraseProtected {
                    Button(l10n.t(KeysRotatedCopy.showPhrase)) { onShowNewPhrase() }
                        .font(.footnote.weight(.medium))
                        .frame(minHeight: 44)
                        .contentShape(Rectangle())
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            .background(Color.lakkoSurface)
        }
        if authRepository.accountRecalledThisSession {
            HStack(alignment: .top, spacing: 12) {
                LkIconView(.shieldCheck, size: 20, relativeTo: .subheadline)
                    .foregroundStyle(Color.lakkoAccent)
                    .accessibilityHidden(true)
                Text(l10n.t(RecallCopy.done))
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoText)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Button(l10n.t("common.dismiss")) { authRepository.dismissRecalledNotice() }
                    .font(.footnote)
                    .frame(minWidth: 44, minHeight: 44)
                    .contentShape(Rectangle())
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            .background(Color.lakkoSurface)
        }
    }
}

/// UX 18.3 — while a kept device is fetching the account's new keys: the chat
/// list stays, and the header shows `Updating keys...` where
/// "Synchronizing..." would be. Never a full-screen state. A view of its own,
/// so the chat list is not re-evaluated by the auth repository.
struct KeysUpdatingLabel: View {
    @ObservedObject var authRepository: AuthRepository
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if authRepository.keysUpdating {
            Text(l10n.t(KeysRotatedCopy.updating))
                .font(.footnote)
                .foregroundStyle(.secondary)
                .accessibilityAddTraits(.updatesFrequently)
        }
    }
}
