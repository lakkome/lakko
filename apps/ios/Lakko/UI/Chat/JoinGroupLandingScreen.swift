// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// ADR-0028 (G-3) — `/g/<slug>` group join-ticket landing.
///
/// A non-member opens an owner's `https://lakko.me/g/<slug>` link; this screen
/// decodes + verifies the `GroupInviteCapability`, posts an external-join
/// proposal (ADR-0027) to the group journal, and polls until the rendezvous
/// Welcome lands, then surfaces "Joined". The link conveys only the `group_id`;
/// the admin approval gate is the security boundary.
///
/// **The wait is TWO-stage since ADR-0045 S7, and the copy says so.** An
/// admin's approve no longer admits anyone by itself: it publishes an `Add`
/// proposal, and the rendezvous appears only once a committer sweeps that
/// proposal into a commit — a step ADR-0045 decision 2 gives no deadline. So
/// this poll spans both stages, and promising immediacy here would be the
/// optimistic-update lie the UX spec forbids.
@MainActor
public final class JoinGroupLandingViewModel: ObservableObject {
    /// Local FSM. `requesting` → `waiting` (poll loop) → `joined` / `failed`.
    public enum State: Equatable {
        case requesting
        case waiting
        case joined(groupIdHex: String)
        case failed(String)
    }

    @Published public private(set) var state: State = .requesting

    private let slug: String
    private let repository: ChatRepository
    private let intake: InviteIntake
    private var didRun = false
    private var pollTask: Task<Void, Never>?

    public init(slug: String, repository: ChatRepository, intake: InviteIntake) {
        self.slug = slug
        self.repository = repository
        self.intake = intake
    }

    /// Drive the request exactly once (the `.task` modifier may re-fire across
    /// body rebuilds; the `didRun` guard keeps it to one request).
    public func run() async {
        guard !didRun else { return }
        didRun = true
        let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
        do {
            // Resolve the slug (a pointer fetches the capability from the
            // ledger; an inline link passes the bytes through), then request to
            // join — consuming the ledger row when the link was a pointer
            // (ADR-0028 OQ1).
            let resolved = try await repository.resolveGroupInviteSlug(slug: slug)
            let result = try await repository.requestToJoinFromLink(
                capabilityWire: resolved.capabilityWire,
                nowMs: nowMs,
                blobId: resolved.blobId
            )
            state = .waiting
            startPolling(groupId: result.groupId, joinerKey: result.joinerSignatureKey)
        } catch {
            state = .failed(error.localizedDescription)
        }
    }

    private func startPolling(groupId: Data, joinerKey: Data) {
        let repo = repository
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                do {
                    if let hex = try await repo.pollForJoinWelcome(
                        groupId: groupId,
                        joinerSignatureKey: joinerKey
                    ) {
                        self?.state = .joined(groupIdHex: hex)
                        return
                    }
                } catch {
                    self?.state = .failed(error.localizedDescription)
                    return
                }
                try? await Task.sleep(nanoseconds: 3_000_000_000)
            }
        }
    }

    /// Cancel the poll loop + drop the parked slug so a later `/g/` tap goes
    /// through cleanly.
    public func dismiss() {
        pollTask?.cancel()
        intake.clearGroupJoin()
    }
}

/// The landing view for a tapped `/g/<slug>` group join-ticket link.
public struct JoinGroupLandingScreen: View {
    @StateObject private var viewModel: JoinGroupLandingViewModel
    private let onDone: () -> Void

    public init(viewModel: JoinGroupLandingViewModel, onDone: @escaping () -> Void) {
        self._viewModel = StateObject(wrappedValue: viewModel)
        self.onDone = onDone
    }

    public var body: some View {
        VStack(spacing: 16) {
            Text("Join group")
                .font(.title2)
                .bold()
            switch viewModel.state {
            case .requesting:
                ProgressView("Sending request…")
            case .waiting:
                ProgressView(
                    "Waiting for an admin to approve, then for the change to be applied…")
            case .joined:
                Text("Joined! You can find the group in your chats.")
                    .multilineTextAlignment(.center)
                Button("Done") {
                    viewModel.dismiss()
                    onDone()
                }
            case .failed(let message):
                Text(message)
                    .foregroundStyle(.red)
                    .multilineTextAlignment(.center)
                Button("Close") {
                    viewModel.dismiss()
                    onDone()
                }
            }
        }
        .padding()
        .task { await viewModel.run() }
    }
}
