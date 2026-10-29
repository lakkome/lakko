// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The six-digit entry of "Confirm on another device" on the NEW device
/// (`docs/ux/identity-recovery.md` section 4). The field is disabled until the
/// other device answered; Continue is enabled at six digits.
struct DeviceLinkCodeEntry: View {
    @ObservedObject var link: DeviceLinkJoin
    @Binding var typedCode: String
    let onContinue: () -> Void
    /// Post a new request (one try per request).
    let onStartAgain: () -> Void
    @ObservedObject private var l10n = Localization.shared

    /// Digits only, at most six (spaces and dashes of a paste dropped).
    static func digits(_ input: String) -> String {
        String(input.filter(\.isNumber).prefix(6))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("__ __ __", text: $typedCode)
                .keyboardType(.numberPad)
                .textContentType(.oneTimeCode)
                .font(.title2.monospacedDigit())
                .textFieldStyle(.roundedBorder)
                .accessibilityLabel(l10n.t("device_link.code.field_a11y"))
                .disabled(link.phase != .enterCode)
                .onChange(of: typedCode) { _, value in
                    let clean = Self.digits(value)
                    if clean != value { typedCode = clean }
                }
            switch link.phase {
            case .idle, .waiting:
                Text(DeviceLinkJoinCopy.waiting)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            case .enterCode:
                Text(l10n.t("device_link.code.enter_six_digits"))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                Button(l10n.t("common.continue"), action: onContinue)
                    .buttonStyle(.borderedProminent)
                    .disabled(Self.digits(typedCode).count != 6)
            case .checking:
                ProgressView(l10n.t("common.checking"))
            case .confirmed:
                EmptyView()
            case .wrongCode:
                hint(DeviceLinkJoinCopy.wrongCode)
            case .declined:
                hint(DeviceLinkJoinCopy.declined)
            case .expired:
                hint(DeviceLinkJoinCopy.expired)
            case .failed(let message):
                hint(message.text)
            }
        }
    }

    private func hint(_ text: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(text, systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(.red)
                .font(.footnote)
            Button(l10n.t("device_link.code.start_again"), action: onStartAgain)
        }
    }
}

/// The OLD device's "New sign-in request" modal. Polls while it is up so the
/// new device's payload is acted on as soon as it arrives.
struct DeviceLinkRequestModal: View {
    @ObservedObject var flow: DeviceLinkResponderFlow
    /// UX 17.4 — the proof block of step 1 (absent while a proof is live).
    @ObservedObject var proof: FreshAuthProofModel
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        ScrollView {
        VStack(spacing: 16) {
            Text(DeviceLinkModalCopy.title)
                .font(.title3.weight(.semibold))
            switch flow.modal {
            case .confirm:
                // Second security pass, finding 6 — ask before answering.
                Text(DeviceLinkModalCopy.askBody)
                    .multilineTextAlignment(.center)
                // UX 17.4 — one decision on one screen: the block sits under
                // the question and above the buttons. Nothing is sent for
                // the request until the person said yes AND proved it.
                FreshAuthBlock(model: proof, onSubmit: { Task { await flow.yesItsMe() } })
                Button {
                    Task { await flow.yesItsMe() }
                } label: {
                    Text(proof.layout.working ? l10n.t("common.checking") : DeviceLinkModalCopy.yes)
                }
                .buttonStyle(.borderedProminent)
                .disabled(!proof.layout.ready)
                // Never needs a proof; enabled in every state.
                Button(DeviceLinkModalCopy.notMe, role: .destructive) {
                    Task { await flow.notMe() }
                }
            case .proofNeeded:
                // The proof ran out between the steps: the block again, the
                // request kept.
                FreshAuthBlock(model: proof, onSubmit: { Task { await flow.continueAfterProof() } })
                Button {
                    Task { await flow.continueAfterProof() }
                } label: {
                    Text(l10n.t(proof.layout.working ? "common.checking" : "common.continue"))
                }
                .buttonStyle(.borderedProminent)
                .disabled(!proof.layout.ready)
                Button(DeviceLinkModalCopy.notMe, role: .destructive) {
                    Task { await flow.notMe() }
                }
            case .expired:
                info(DeviceLinkModalCopy.expired)
            case .code(let code):
                Text(DeviceLinkModalCopy.body)
                    .multilineTextAlignment(.center)
                Text(code)
                    .font(.system(size: 40, weight: .semibold, design: .monospaced))
                    .accessibilityLabel(DeviceLinkModalCopy.codeA11y)
                    .accessibilityValue(code.replacingOccurrences(of: " ", with: ""))
                Text(DeviceLinkModalCopy.warning)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                Button(DeviceLinkModalCopy.notMe, role: .destructive) {
                    Task { await flow.notMe() }
                }
            case .adding:
                ProgressView(DeviceLinkModalCopy.adding)
            case .keysNotSent:
                info(DeviceLinkModalCopy.keysNotSent)
            case .wrongCode:
                info(DeviceLinkModalCopy.wrongCode)
            case .declined:
                info(DeviceLinkModalCopy.declined)
            case .handledElsewhere:
                info(DeviceLinkModalCopy.handledElsewhere)
            case .tooMany:
                info(DeviceLinkAnswerLimit.copy)
            case .failed:
                info(DeviceLinkModalCopy.failed)
            case nil:
                EmptyView()
            }
        }
        .padding(24)
        }
        .onAppear { proof.refresh() }
        .task(id: flow.modal) {
            // While the code is up, look for the new device's payload; while
            // asking, notice a request that went away.
            switch flow.modal {
            case .code, .confirm, .proofNeeded: break
            default: return
            }
            await proof.opened()
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                // A step change restarts this task and cancels the old one
                // mid-sleep; a cancelled check would cancel its own request.
                if Task.isCancelled { break }
                await flow.check()
            }
        }
        .interactiveDismissDisabled({
            if case .code = flow.modal { return true }
            if case .adding = flow.modal { return true }
            return false
        }())
    }

    private func info(_ text: String) -> some View {
        VStack(spacing: 12) {
            Text(text).multilineTextAlignment(.center)
            Button(l10n.t("common.ok")) { flow.dismiss() }
                .buttonStyle(.borderedProminent)
        }
    }
}
