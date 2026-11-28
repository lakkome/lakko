// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// "Get back in": the list of ways back into an account, in two groups, each
/// row pushing its form on the anonymous stack. Pushed from the log-in
/// screen ("Forgot password?"); the system back returns there.
///
///   - "Brings back your chats": another device (first), a passkey, the
///     12-word recovery phrase.
///   - "Logs you in on this device": three words from the phrase (only on a
///     device that holds the phrase and the keys of its cached account), an
///     authenticator code, an email code (a mailed code gives a new password
///     and never brings keys back).
///
/// Every method's logic is ``RecoverViewModel``; the screen only presents it.
public struct RecoverScreen: View {

    @ObservedObject public var viewModel: RecoverViewModel
    public var onNavigateToLogin: () -> Void
    public var onNavigateToSignup: () -> Void

    public init(
        viewModel: RecoverViewModel,
        onNavigateToLogin: @escaping () -> Void,
        onNavigateToSignup: @escaping () -> Void
    ) {
        self.viewModel = viewModel
        self.onNavigateToLogin = onNavigateToLogin
        self.onNavigateToSignup = onNavigateToSignup
    }

    /// `nil` shows the list; a method pushes that method's form.
    @State private var chosen: RecoverViewModel.Mode?
    @ObservedObject private var l10n = Localization.shared

    public var body: some View {
        AuthScreen {
            AuthTitle(
                title: l10n.t("recovery.title"),
                subtitle: l10n.t("recovery.lede"))

            group(titleKey: "recovery.group.keys", methods: viewModel.keysMethods)
            group(titleKey: "recovery.group.signin", methods: viewModel.signInMethods)

            AuthHint(text: l10n.t(viewModel.accessOnlyNoteKey))

            AuthLinkRow(
                message: "recovery.remembered",
                title: l10n.t("auth.login.submit"),
                action: onNavigateToLogin)
            AuthLinkRow(
                message: "auth.login.no_account",
                title: l10n.t("auth.signup.submit"),
                action: onNavigateToSignup)
        }
        .navigationTitle("")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { viewModel.refreshMethods() }
        .navigationDestination(item: $chosen) { mode in
            AuthScreen {
                AuthTitle(title: l10n.t(mode.titleKey), subtitle: l10n.t(mode.formLeadKey))
                RecoverForm(
                    viewModel: viewModel,
                    cachedUsername: viewModel.cachedUsername,
                    onBackToList: {
                        viewModel.leaveForm()
                        chosen = nil
                    }
                )
            }
            .navigationTitle("")
            .navigationBarTitleDisplayMode(.inline)
        }
    }

    /// One labelled run of rows.
    @ViewBuilder
    private func group(titleKey: String?, methods: [RecoverViewModel.Mode]) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            if let titleKey {
                Text(l10n.t(titleKey))
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Color.lakkoTextMuted)
                    .accessibilityAddTraits(.isHeader)
            }
            RecoveryMethodList(
                rows: methods.map {
                    RecoveryMethodRow(
                        id: $0.rowId, icon: $0.icon,
                        title: l10n.t($0.titleKey), detail: l10n.t($0.detailKey))
                },
                disabled: viewModel.busy
            ) { id in
                guard let mode = methods.first(where: { $0.rowId == id }) else { return }
                viewModel.mode = mode
                chosen = mode
            }
        }
    }
}

/// One row of a method list: an icon, a title and a second line.
struct RecoveryMethodRow: Identifiable, Equatable {
    let id: String
    let icon: LkIcon
    let title: String
    let detail: String
}

/// A card of method rows with chevrons, shared by the recovery list and the
/// restore prompt so the two screens name and draw the routes the same way.
struct RecoveryMethodList: View {
    let rows: [RecoveryMethodRow]
    var disabled: Bool = false
    let onChoose: (String) -> Void

    var body: some View {
        VStack(spacing: 0) {
            ForEach(rows) { row in
                Button {
                    onChoose(row.id)
                } label: {
                    HStack(spacing: 12) {
                        LkIconView(row.icon, size: 20)
                            .frame(width: 28)
                            .foregroundStyle(Color.lakkoAccent)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(row.title)
                                .font(.body.weight(.medium))
                                .foregroundStyle(Color.lakkoText)
                            Text(row.detail)
                                .font(.footnote)
                                .foregroundStyle(Color.lakkoTextMuted)
                                .multilineTextAlignment(.leading)
                        }
                        Spacer()
                        LkIconView(.chevronForward, size: 14, relativeTo: .footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    .padding(.vertical, 12)
                    .padding(.horizontal, 16)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(disabled)
                if row.id != rows.last?.id {
                    Divider().padding(.leading, 56)
                }
            }
        }
        .background(Color.lakkoSurface)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
        .overlay(
            RoundedRectangle(cornerRadius: LakkoRadius.card)
                .stroke(Color.lakkoBorder, lineWidth: 1)
        )
    }
}

extension RecoverViewModel.Mode {
    /// A stable id for the row.
    var rowId: String {
        switch self {
        case .email: return "email"
        case .anotherDevice: return "device"
        case .passkey: return "passkey"
        case .phrase: return "phrase"
        case .words: return "words"
        case .totp: return "totp"
        }
    }

    /// The catalog key of the row title (ADR-0061).
    var titleKey: String {
        switch self {
        case .email: return "recovery.email.row.title"
        case .anotherDevice: return "recovery.method.device.title"
        case .passkey: return "recovery.method.passkey.title"
        case .phrase: return "recovery.method.phrase.title"
        case .words: return "recovery.method.words.title"
        case .totp: return "recovery.method.totp.title"
        }
    }

    /// The catalog key of the row's second line.
    var detailKey: String {
        switch self {
        case .email: return "recovery.email.row.body"
        case .anotherDevice: return "recovery.method.device.sub"
        case .passkey: return "recovery.method.passkey.sub"
        case .phrase: return "recovery.method.phrase.sub"
        case .words: return "recovery.method.words.sub"
        case .totp: return "recovery.method.totp.sub"
        }
    }

    var icon: LkIcon {
        switch self {
        case .email: return .envelope
        case .anotherDevice: return .smartphoneWaves
        case .passkey: return .passkey
        case .phrase: return .key
        case .words: return .words
        case .totp: return .lockRotation
        }
    }

    /// The catalog key of the line under the form title.
    var formLeadKey: String {
        switch self {
        case .email: return "recovery.email.start.body"
        case .anotherDevice: return "recovery.device.intro_qr"
        case .passkey: return "recovery.passkey.intro"
        case .phrase: return "recovery.phrase.intro"
        case .words: return "recovery.method.words.sub"
        case .totp: return "recovery.totp.intro"
        }
    }
}

// MARK: - Form

private struct RecoverForm: View {
    @ObservedObject var viewModel: RecoverViewModel
    /// `nil` on a fresh device: the form then asks for the username so the
    /// account can be found.
    let cachedUsername: String?
    let onBackToList: () -> Void

    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if viewModel.methodUnavailable {
            MethodUnavailable(onBack: onBackToList)
        } else {
            form
        }
    }

    @ViewBuilder
    private var form: some View {
        // The phrase and the authenticator name the account by username; a
        // passkey needs none, the three words belong to the cached account,
        // and the another-device form asks in its own section.
        if viewModel.mode == .phrase || viewModel.mode == .totp {
            if let cachedUsername {
                cachedAccountLine("@\(cachedUsername)")
            } else {
                AuthTextField(
                    label: l10n.t("auth.username.label"),
                    text: $viewModel.username,
                    contentType: .username,
                    disabled: viewModel.busy
                )
                UsernameRuleHint(input: viewModel.username)
            }
        }

        switch viewModel.mode {
        case .email:
            EmailRecoverSection(model: viewModel.email, onBackToList: onBackToList)
        case .anotherDevice:
            anotherDeviceSection
        case .passkey:
            passkeySection
        case .words:
            if viewModel.wordsRetired {
                // The route was suspended on this device: one sentence
                // and the way back to a list without this row.
                if let error = viewModel.error {
                    AuthHint(text: error, isError: true)
                }
                AuthSecondaryButton(title: l10n.t("recovery.methods.back_to_list"), action: onBackToList)
            } else {
                WordsRecoverSection(viewModel: viewModel)
            }
        case .totp:
            totpSection
        case .phrase:
            PhraseEntryGrid(
                phrase: $viewModel.phrase,
                disabled: viewModel.busy || viewModel.phraseRecall != nil || viewModel.phraseRecallBusy)

            AuthSecureField(
                label: l10n.t("auth.password.new.label"),
                text: $viewModel.password,
                contentType: .newPassword,
                disabled: viewModel.busy
            )
            AuthHint(text: l10n.t(
                "auth.password.hint", counts: ["count": UInt64(RecoverViewModel.minPasswordLength)]))

            AuthSecureField(
                label: l10n.t("auth.password.new_confirm.label"),
                text: $viewModel.confirm,
                contentType: .newPassword,
                disabled: viewModel.busy,
                submitLabel: .go,
                onSubmit: viewModel.onSubmit
            )

            if let error = viewModel.error {
                AuthHint(text: error, isError: true)
            }

            if let offer = viewModel.phraseRecall {
                // UX 19.7 — the words belong to the account as it was before
                // its keys were changed: the question, on the same form. The
                // new password above is the one the account gets.
                Text(l10n.t(
                    RecallCopy.phraseChanged,
                    texts: ["date": RecallCopy.date(ms: offer.window.changedAtMs, locale: l10n.locale)]))
                    .font(.headline)
                    .foregroundStyle(Color.lakkoText)
                VStack(alignment: .leading, spacing: 8) {
                    AuthHint(text: l10n.t(RecallCopy.effectKeys))
                    AuthHint(text: l10n.t(RecallCopy.effectFactors))
                    AuthHint(text: l10n.t(RecallCopy.effectMessages))
                }
                AuthSecondaryButton(
                    title: l10n.t(AccountNoticeCopy.wasMeKey),
                    disabled: viewModel.phraseRecallBusy,
                    action: viewModel.phraseRecallWasMe)
                AuthPrimaryButton(
                    title: l10n.t(RecallCopy.phraseTakeBack),
                    busy: viewModel.phraseRecallBusy,
                    busyTitle: l10n.t(RecallCopy.working),
                    action: viewModel.phraseRecallTakeBack
                )
            } else {
                AuthPrimaryButton(
                    title: l10n.t("recovery.submit"),
                    busy: viewModel.busy,
                    busyTitle: l10n.t("common.checking"),
                    action: viewModel.onSubmit
                )
            }
        }
    }

    private func cachedAccountLine(_ name: String) -> some View {
        HStack(spacing: 8) {
            LkIconView(.personSquare, size: 20)
                .foregroundStyle(Color.lakkoTextMuted)
            Text(l10n.t("recovery.cached_account", texts: ["username": name]))
                .font(.body.weight(.medium))
                .foregroundStyle(Color.lakkoText)
        }
    }

    /// One button: the system sheet lists the passkeys saved for Lakko.
    @ViewBuilder
    private var passkeySection: some View {
        if let error = viewModel.error {
            AuthHint(text: error, isError: true)
        }
        AuthPrimaryButton(
            title: l10n.t("recovery.passkey.submit"),
            busy: viewModel.busy,
            busyTitle: l10n.t("recovery.passkey.waiting"),
            action: viewModel.submitPasskey
        )
    }

    /// A six-digit authenticator code signs the person in; a device without
    /// the keys then shows the restore prompt.
    @ViewBuilder
    private var totpSection: some View {
        AuthTextField(
            label: l10n.t("recovery.totp.code_label"),
            placeholder: "123456",
            text: $viewModel.totpCode,
            keyboard: .numberPad,
            contentType: .oneTimeCode,
            disabled: viewModel.busy,
            submitLabel: .go,
            onSubmit: viewModel.submitTotp
        )
        if let error = viewModel.error {
            AuthHint(text: error, isError: true)
        }
        AuthPrimaryButton(
            title: l10n.t("common.continue"),
            busy: viewModel.busy,
            busyTitle: l10n.t("common.checking"),
            action: viewModel.submitTotp
        )
    }

    /// Restore through an already linked device. The pairing code names the
    /// account by the login handle of the typed username (ADR-0056, no
    /// lookup), then the link runs with the recovery payload.
    @ViewBuilder
    private var anotherDeviceSection: some View {
        if let target = viewModel.deviceRestoreTarget {
            HStack(spacing: 8) {
                LkIconView(.personSquare, size: 20)
                    .foregroundStyle(Color.lakkoTextMuted)
                Text("@\(target.username)")
                    .font(.body.weight(.medium))
                    .foregroundStyle(Color.lakkoText)
            }
            RestoreViaDevicePanel(
                target: .byName(loginHandle: target.loginHandle),
                onRecovered: { outcome in
                    try await viewModel.completeFromDeviceRestore(outcome: outcome)
                }
            )
        } else {
            AuthTextField(
                label: l10n.t("auth.username.label"),
                text: $viewModel.username,
                contentType: .username,
                disabled: viewModel.busy,
                submitLabel: .go,
                onSubmit: viewModel.prepareDeviceRestore
            )
            UsernameRuleHint(input: viewModel.username)
            if let error = viewModel.error {
                AuthHint(text: error, isError: true)
            }
            AuthPrimaryButton(
                title: l10n.t("common.continue"),
                busy: viewModel.busy,
                busyTitle: l10n.t("common.continue"),
                action: viewModel.prepareDeviceRestore
            )
        }
    }
}

// MARK: - Three words

/// "Three words from your phrase": the form. No username field (the account
/// is the one cached on this device); three inputs labelled with their word
/// numbers in the order drawn. The check is local; a miss draws new
/// positions and counts against the budget shown under the fields.
private struct WordsRecoverSection: View {
    @ObservedObject var viewModel: RecoverViewModel
    @ObservedObject private var pause: RetryCountdown
    @ObservedObject private var serverWait: RetryCountdown
    @ObservedObject private var l10n = Localization.shared
    @FocusState private var focused: Int?

    init(viewModel: RecoverViewModel) {
        self.viewModel = viewModel
        self.pause = viewModel.wordsPause
        self.serverWait = viewModel.serverWait
    }

    var body: some View {
        if let account = viewModel.threeWordsAccount {
            HStack(spacing: 8) {
                LkIconView(.personSquare, size: 20)
                    .foregroundStyle(Color.lakkoTextMuted)
                Text(l10n.t("recovery.cached_account", texts: ["username": "@\(account.username)"]))
                    .font(.body.weight(.medium))
                    .foregroundStyle(Color.lakkoText)
            }
        }

        if pause.waiting {
            WaitNotice(countdown: pause, messageKey: "recovery.words.paused")
        } else {
            Text(l10n.t(
                "recovery.words.prompt",
                texts: ["positions": l10n.positions(viewModel.wordsPositions)]))
                .font(.body)
                .foregroundStyle(Color.lakkoText)

            ForEach(Array(viewModel.wordsPositions.enumerated()), id: \.offset) { index, number in
                VStack(alignment: .leading, spacing: 6) {
                    AuthFieldLabel(l10n.t("recovery_phrase.word_label", texts: ["position": String(number)]))
                    TextField("", text: entry(index))
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled(true)
                        .keyboardType(.asciiCapable)
                        .submitLabel(index == viewModel.wordsPositions.count - 1 ? .go : .next)
                        .onSubmit {
                            if index + 1 < viewModel.wordsPositions.count {
                                focused = index + 1
                            } else {
                                focused = nil
                                viewModel.submitWords()
                            }
                        }
                        .focused($focused, equals: index)
                        .disabled(viewModel.busy)
                        .authFieldChrome(focused: focused == index)
                        .accessibilityLabel(
                            l10n.t("recovery_phrase.word_label", texts: ["position": String(number)]))
                }
            }

            AuthHint(text: l10n.t("recovery.words.signin_only"))

            if let error = viewModel.error {
                AuthHint(text: error, isError: true)
            }
            if let left = viewModel.wordsTriesLeft {
                AuthHint(text: l10n.t("recovery.words.tries_left", counts: ["count": UInt64(left)]))
            }
            WaitNotice(countdown: serverWait)

            AuthPrimaryButton(
                title: l10n.t("recovery.words.submit"),
                busy: viewModel.busy,
                busyTitle: l10n.t("common.checking"),
                disabled: !viewModel.wordsComplete || serverWait.waiting,
                action: viewModel.submitWords
            )
        }
    }

    private func entry(_ index: Int) -> Binding<String> {
        Binding(
            get: { index < viewModel.wordsEntries.count ? viewModel.wordsEntries[index] : "" },
            set: { newValue in
                guard index < viewModel.wordsEntries.count else { return }
                viewModel.wordsEntries[index] = newValue.lowercased()
            }
        )
    }
}

// MARK: - Email code

/// "Email code" (UX 19.3): username and address, the code, the
/// authenticator code when the account has one, then a new password. Step 2
/// is drawn the same whatever was typed; nothing before the new password
/// tells which kind of account this is.
struct EmailRecoverSection: View {
    @ObservedObject var model: EmailRecoverModel
    let onBackToList: () -> Void
    @ObservedObject private var resend: RetryCountdown
    @ObservedObject private var wait: RetryCountdown
    @ObservedObject private var l10n = Localization.shared

    init(model: EmailRecoverModel, onBackToList: @escaping () -> Void) {
        self.model = model
        self.onBackToList = onBackToList
        self.resend = model.resend
        self.wait = model.wait
    }

    var body: some View {
        switch model.state.step {
        case .start: startStep
        case .code: codeStep
        case .totp: totpStep
        case .newPassword: passwordStep
        case .restoring: progress("common.saving")
        }
    }

    @ViewBuilder private var startStep: some View {
        // The username is pre-filled; the address never is.
        AuthTextField(
            label: l10n.t("auth.username.label"),
            text: $model.username,
            contentType: .username,
            disabled: model.busy
        )
        if let usernameError = model.usernameError {
            AuthHint(text: usernameError.text, isError: true)
        } else {
            UsernameRuleHint(input: model.username)
        }
        AuthTextField(
            label: l10n.t("recovery_email.field"),
            placeholder: "name@example.com",
            text: $model.address,
            keyboard: .emailAddress,
            contentType: .emailAddress,
            disabled: model.busy,
            submitLabel: .go,
            onSubmit: start
        )
        if !model.address.isEmpty, !model.addressValid {
            AuthHint(text: l10n.t("recovery_email.invalid"), isError: true)
        }
        line
        AuthPrimaryButton(
            title: l10n.t("recovery_email.send"),
            busy: model.busy,
            busyTitle: l10n.t("common.sending"),
            disabled: !model.canStart,
            action: start
        )
    }

    @ViewBuilder private var codeStep: some View {
        Text(l10n.t("recovery.email.check.title"))
            .font(.headline)
            .foregroundStyle(Color.lakkoText)
            .accessibilityAddTraits(.isHeader)
        AuthHint(text: l10n.t("recovery.email.check.body"))
        AuthTextField(
            label: l10n.t("recovery_email.code.title"),
            placeholder: "123456",
            text: $model.code,
            keyboard: .numberPad,
            contentType: .oneTimeCode,
            disabled: model.busy,
            submitLabel: .go,
            onSubmit: verify
        )
        line
        AuthPrimaryButton(
            title: l10n.t("common.continue"),
            busy: model.busy,
            busyTitle: l10n.t("common.checking"),
            disabled: EmailEnrol.normalizedCode(model.code) == nil,
            action: verify
        )
        ResendRow(countdown: resend, disabled: model.busy || wait.waiting, action: start)
        AuthLinkRow(title: l10n.t("common.back"), disabled: model.busy) { model.backToStart() }
    }

    @ViewBuilder private var totpStep: some View {
        Text(l10n.t("recovery.email.totp.title"))
            .font(.headline)
            .foregroundStyle(Color.lakkoText)
            .accessibilityAddTraits(.isHeader)
        AuthHint(text: l10n.t("recovery.email.totp.body"))
        AuthTextField(
            label: l10n.t("recovery.totp.code_label"),
            placeholder: "123456",
            text: $model.totpCode,
            keyboard: .numberPad,
            contentType: .oneTimeCode,
            disabled: model.busy,
            submitLabel: .go,
            onSubmit: submitTotp
        )
        line
        AuthPrimaryButton(
            title: l10n.t("common.continue"),
            busy: model.busy,
            busyTitle: l10n.t("common.checking"),
            disabled: EmailEnrol.normalizedCode(model.totpCode) == nil,
            action: submitTotp
        )
        // There is no way past this step without the authenticator.
        AuthLinkRow(title: l10n.t("recovery.email.totp.lost"), disabled: model.busy, action: onBackToList)
    }

    @ViewBuilder private var passwordStep: some View {
        Text(l10n.t("recovery.new_password.title"))
            .font(.headline)
            .foregroundStyle(Color.lakkoText)
            .accessibilityAddTraits(.isHeader)
        AuthSecureField(
            label: l10n.t("auth.password.new.label"),
            text: $model.password,
            contentType: .newPassword,
            disabled: model.busy
        )
        AuthHint(text: l10n.t(
            "auth.password.hint", counts: ["count": UInt64(NewPasswordRule.minLength)]))
        AuthSecureField(
            label: l10n.t("auth.password.new_confirm.label"),
            text: $model.confirmPassword,
            contentType: .newPassword,
            disabled: model.busy,
            submitLabel: .go,
            onSubmit: save
        )
        if let passwordError = model.passwordError {
            AuthHint(text: passwordError.text, isError: true)
        }
        line
        AuthPrimaryButton(
            title: l10n.t("recovery.email.save"),
            busy: model.busy,
            busyTitle: l10n.t("common.saving"),
            disabled: wait.waiting,
            action: save
        )
    }

    private func progress(_ key: String) -> some View {
        HStack(spacing: 10) {
            ProgressView().controlSize(.small)
            Text(l10n.t(key))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
        }
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder private var line: some View {
        if let line = model.state.line {
            if let key = line.key {
                AuthHint(text: l10n.t(key), isError: true)
            } else {
                WaitNotice(countdown: wait)
            }
        }
    }

    private func start() { Task { await model.start() } }
    private func verify() { Task { await model.verify() } }
    private func submitTotp() { Task { await model.submitTotp() } }
    private func save() { Task { await model.save() } }
}
