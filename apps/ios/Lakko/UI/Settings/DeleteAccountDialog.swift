// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

// UX `docs/ux/identity-recovery.md` section 17.3 — "Delete your account?",
// drawn by the app (never a system confirmation dialog: it carries the proof
// block, and no system dialog gains a password field for this).

/// What the delete dialog shows and enables, pure.
enum DeleteAccountDialogGate {

    enum Form: Equatable {
        /// The account has a passkey: no field, the passkey notice. The one
        /// gesture bound to this deletion is the proof, whatever was proven
        /// before; no passkey sign-in runs first.
        case passkey
        /// No passkey, a proof is live: the body and the buttons.
        case plain
        /// No passkey, no proof: the proof block, no passkey button.
        case block
        /// No passkey, `I do not know my password`: the account cannot be
        /// deleted; `I know my password` goes back.
        case cannot
    }

    static func form(hasPasskey: Bool, proofLive: Bool, choseNoPassword: Bool) -> Form {
        if hasPasskey { return .passkey }
        if proofLive { return .plain }
        return choseNoPassword ? .cannot : .block
    }

    /// Whether the danger button is drawn at all.
    static func showsDelete(form: Form) -> Bool { form != .cannot }

    /// The danger button's label: its working word while the deletion (or
    /// the password check in front of it) runs.
    static func deleteLabelKey(busy: Bool) -> String {
        busy ? "settings.account.delete.working" : "settings.account.delete.confirm"
    }

    /// Whether `Delete account` is enabled.
    static func canDelete(form: Form, blockReady: Bool, busy: Bool) -> Bool {
        guard !busy else { return false }
        switch form {
        case .passkey, .plain: return true
        case .block: return blockReady
        case .cannot: return false
        }
    }
}

/// The app's own delete-account dialog, presented as a sheet.
struct DeleteAccountDialog: View {
    @ObservedObject var viewModel: PasskeysViewModel
    let onClose: () -> Void

    @StateObject private var proof: FreshAuthProofModel
    @State private var choseNoPassword = false
    @ObservedObject private var l10n = Localization.shared

    init(viewModel: PasskeysViewModel, onClose: @escaping () -> Void) {
        self.viewModel = viewModel
        self.onClose = onClose
        self._proof = StateObject(wrappedValue: viewModel.makeProof(.deleteAccount))
    }

    private var form: DeleteAccountDialogGate.Form {
        DeleteAccountDialogGate.form(
            hasPasskey: proof.hasUsablePasskey,
            proofLive: proof.proofLive,
            choseNoPassword: choseNoPassword)
    }

    private var busy: Bool { viewModel.isBusy || proof.layout.working }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Text(l10n.t("settings.account.delete.title"))
                    .font(.title3.weight(.semibold))
                    .foregroundStyle(Color.lakkoText)
                    .accessibilityAddTraits(.isHeader)
                Text(l10n.t("settings.account.delete.body"))
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)

                switch form {
                case .passkey:
                    AuthHint(text: l10n.t("proof.passkey.notice"))
                case .plain:
                    EmptyView()
                case .block:
                    FreshAuthBlock(model: proof, onSubmit: delete)
                    link("proof.no_password_link") { choseNoPassword = true }
                case .cannot:
                    Text(l10n.t("proof.delete.cannot"))
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoText)
                    link("proof.know_password_link") { choseNoPassword = false }
                }

                if case .error(let text) = viewModel.message {
                    AuthHint(text: text.text, isError: true)
                }

                if DeleteAccountDialogGate.showsDelete(form: form) {
                    let enabled = DeleteAccountDialogGate.canDelete(
                        form: form, blockReady: proof.layout.ready, busy: busy)
                    Button(action: delete) {
                        HStack(spacing: 8) {
                            if busy { ProgressView().controlSize(.small) }
                            Text(l10n.t(DeleteAccountDialogGate.deleteLabelKey(busy: busy)))
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
                    .disabled(!enabled)
                    .opacity(enabled ? 1 : 0.5)
                }
                AuthSecondaryButton(title: l10n.t("common.cancel"), disabled: busy, action: onClose)
            }
            .padding(24)
        }
        .background(Color.lakkoBackground)
        .environment(\.layoutDirection, l10n.layoutDirection)
        .interactiveDismissDisabled(busy)
        .task { await proof.opened() }
        .onDisappear {
            // The typed password is not kept after the dialog closes.
            proof.closed()
            viewModel.clearMessage()
        }
    }

    private func delete() {
        guard DeleteAccountDialogGate.canDelete(form: form, blockReady: proof.layout.ready, busy: busy) else {
            return
        }
        Task {
            // On success the device is wiped and the whole signed-in app goes
            // away with this dialog.
            await viewModel.deleteAccount(proof: proof, hasPasskey: form == .passkey)
        }
    }

    private func link(_ key: String, action: @escaping () -> Void) -> some View {
        Button(l10n.t(key), action: action)
            .buttonStyle(.plain)
            .font(.footnote.weight(.medium))
            .foregroundStyle(Color.lakkoAccent)
            .frame(minHeight: 44, alignment: .leading)
            .disabled(busy)
    }
}
