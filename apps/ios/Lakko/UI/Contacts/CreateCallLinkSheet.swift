// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// The choices the "Create call link" form offers, and its one validation
/// rule — pulled out of the view so the offered set is pinned by a test rather
/// than by whatever the picker happens to list.
///
/// A `/c/` call link is independent of any chat (ADR-0032): the owner founds a
/// fresh call group when they start it, so the form lives on the Contacts
/// screen beside the contact invite link, not in a chat header.
enum CallLinkForm {

    /// 1 hour / 24 hours / 7 days. The composer also knows 30 days and
    /// never-expires; neither is offered, because a call link that outlives
    /// the call is a standing capability to ask into a room.
    static let expiryChoices: [CallInviteExpiryPreset] = [.oneHour, .oneDay, .sevenDays]

    /// 1 / 10 / unlimited uses.
    static let maxUsesChoices: [CallInviteMaxUsesPreset] = [.single, .ten, .unlimited]

    static let mediaChoices: [CallMediaPreset] = [.audio, .audioVideo]

    static func title(_ expiry: CallInviteExpiryPreset) -> String {
        switch expiry {
        case .oneHour: return "1 hour"
        case .oneDay: return "24 hours"
        case .sevenDays: return "7 days"
        case .thirtyDays: return "30 days"
        case .never: return "Never"
        }
    }

    static func title(_ maxUses: CallInviteMaxUsesPreset) -> String {
        switch maxUses {
        case .single: return "1"
        case .ten: return "10"
        case .unlimited: return "Unlimited"
        }
    }

    static func title(_ media: CallMediaPreset) -> String {
        switch media {
        case .audio: return "Audio"
        case .audioVideo: return "Video"
        }
    }

    /// `nil` when the label fits the capability's UTF-8 byte budget
    /// (``CallInviteUrl/labelMaxLen``), else the message the form shows. The
    /// budget is BYTES, not characters, so a label of emoji or Cyrillic runs
    /// out well before 64 visible characters.
    static func labelError(_ label: String) -> String? {
        let bytes = label.utf8.count
        guard bytes > CallInviteUrl.labelMaxLen else { return nil }
        return "Label is too long (\(bytes) of \(CallInviteUrl.labelMaxLen) bytes)."
    }
}

/// Drives one "Create call link" attempt: the form state, the busy state while
/// the upload solves its proof-of-work, and the resulting link.
@MainActor
final class CreateCallLinkViewModel: ObservableObject {

    enum Phase: Equatable {
        case editing
        case creating
        case created(url: String)
        case failed(String)
    }

    @Published var media: CallMediaPreset = .audio
    @Published var expiry: CallInviteExpiryPreset = .oneDay
    @Published var maxUses: CallInviteMaxUsesPreset = .ten
    @Published var label: String = ""
    @Published private(set) var phase: Phase = .editing

    private let authRepository: AuthRepository

    init(authRepository: AuthRepository) {
        self.authRepository = authRepository
    }

    var labelError: String? { CallLinkForm.labelError(label) }

    var canCreate: Bool {
        labelError == nil && phase != .creating
    }

    /// Compose the owner-signed capability, upload it (PoW-gated) and build the
    /// `/c/<slug>` link.
    func create() async {
        guard canCreate else { return }
        guard let secrets = authRepository.identitySecrets else {
            phase = .failed("Sign in again to create a call link.")
            return
        }
        phase = .creating
        do {
            let identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
            let composed = try await CallInviteComposer.composeAndUpload(
                ownerSigningSeed: identity.ed25519Seed(),
                ownerAccountId: secrets.accountId,
                identity: identity,
                discoveryClient: DiscoveryClient(sessionToken: secrets.sessionToken),
                media: media,
                expiryPreset: expiry,
                maxUsesPreset: maxUses,
                label: label.trimmingCharacters(in: .whitespacesAndNewlines)
            )
            phase = .created(url: composed.url)
        } catch {
            NSLog("create call link failed: %@", String(describing: error))
            phase = .failed(
                (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
            )
        }
    }

    /// Back to the form after a failure, keeping what was entered.
    func editAgain() {
        phase = .editing
    }
}

/// The "Create call link" sheet: Audio/Video, expiry, max uses and an optional
/// label; then the link with Copy / Share and "Start call", which opens the
/// owner host flow of ``JoinCallLandingScreen`` for this link.
struct CreateCallLinkSheet: View {
    @StateObject private var viewModel: CreateCallLinkViewModel
    let onDismiss: () -> Void
    let onStartCall: (String) -> Void

    init(
        authRepository: AuthRepository,
        onDismiss: @escaping () -> Void,
        onStartCall: @escaping (String) -> Void
    ) {
        _viewModel = StateObject(wrappedValue: CreateCallLinkViewModel(authRepository: authRepository))
        self.onDismiss = onDismiss
        self.onStartCall = onStartCall
    }

    var body: some View {
        NavigationStack {
            Group {
                switch viewModel.phase {
                case .created(let url):
                    created(url: url)
                default:
                    form
                }
            }
            .navigationTitle("Call link")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button(viewModelIsCreated ? "Done" : "Cancel", action: onDismiss)
                        .disabled(viewModel.phase == .creating)
                }
            }
        }
        .interactiveDismissDisabled(viewModel.phase == .creating)
        .presentationDetents([.large])
    }

    private var viewModelIsCreated: Bool {
        if case .created = viewModel.phase { return true }
        return false
    }

    private var form: some View {
        Form {
            Section {
                Picker("Call type", selection: $viewModel.media) {
                    ForEach(CallLinkForm.mediaChoices, id: \.self) { choice in
                        Text(CallLinkForm.title(choice)).tag(choice)
                    }
                }
                .pickerStyle(.segmented)
            } header: {
                Text("Call type")
            }

            Section {
                Picker("Expires after", selection: $viewModel.expiry) {
                    ForEach(CallLinkForm.expiryChoices, id: \.self) { choice in
                        Text(CallLinkForm.title(choice)).tag(choice)
                    }
                }
                Picker("Max uses", selection: $viewModel.maxUses) {
                    ForEach(CallLinkForm.maxUsesChoices, id: \.self) { choice in
                        Text(CallLinkForm.title(choice)).tag(choice)
                    }
                }
            } header: {
                Text("Link")
            } footer: {
                Text(
                    "Anyone with the link can ask to join. You admit them when " +
                    "the call is running; the link stops working when it expires " +
                    "or runs out of uses."
                )
            }

            Section {
                TextField("Label (optional)", text: $viewModel.label)
                    .textInputAutocapitalization(.sentences)
                if let err = viewModel.labelError {
                    Text(err)
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
            } footer: {
                Text("The label is shown to people who open the link.")
            }

            if case .failed(let message) = viewModel.phase {
                Section {
                    Text("Could not create the link: \(message)")
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
            }

            Section {
                Button {
                    Task { await viewModel.create() }
                } label: {
                    HStack {
                        if viewModel.phase == .creating {
                            ProgressView()
                            Text("Creating link…")
                        } else {
                            Image(systemName: "link.badge.plus")
                            Text("Create call link")
                        }
                    }
                    .frame(maxWidth: .infinity)
                }
                .disabled(!viewModel.canCreate)
            } footer: {
                if viewModel.phase == .creating {
                    Text("This can take a few seconds while your device solves an anti-spam check.")
                }
            }
        }
        .disabled(viewModel.phase == .creating)
    }

    private func created(url: String) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Share this link with the people you want in the call. Start the call when you are ready — people who open the link wait until you admit them.")
                .font(.body)
                .foregroundStyle(.secondary)

            // The same link row the invite-links sheet uses (UX spec section
            // 5.3): a title, what the link allows, Copy and Share. Never the
            // raw URL in a text field.
            LinkActionsRow(
                title: viewModel.label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    ? "Call link" : viewModel.label.trimmingCharacters(in: .whitespacesAndNewlines),
                details: [
                    "\(CallLinkForm.title(viewModel.media)) call",
                    "Expires after \(CallLinkForm.title(viewModel.expiry).lowercased()) · "
                        + "\(CallLinkForm.title(viewModel.maxUses).lowercased()) use"
                        + (viewModel.maxUses == .single ? "" : "s"),
                ],
                url: url
            )
            .padding(12)
            .background(Color.lakkoSurfaceSunken)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))

            Button {
                onStartCall(url)
            } label: {
                Label("Start call", systemImage: "phone.fill")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)

            Spacer()
        }
        .padding(24)
    }
}
