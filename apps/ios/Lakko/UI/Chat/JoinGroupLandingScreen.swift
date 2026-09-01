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
///
/// ## What this side is NOT told (ADR-0045 S7 residual 1)
///
/// A sweep can exclude the approval-derived `Add` — `narrow_sweep_by_exclusion`
/// drops whatever refuses to build, and the commonest cause needs no adversary
/// at all (an expired KeyPackage, reachable because a request has no deadline).
/// This screen is never told when that happens, and the reason is structural
/// rather than an omission: there is no artifact addressed to a non-member, and
/// the journal is unauthenticated — the DS pseudonym IS the `group_id` and there
/// is no auth on it — so any "your request was not carried" envelope would be a
/// forgeable denial primitive. Inference does not work either: an epoch
/// advancing with no rendezvous for us is byte-for-byte what *no admin has
/// approved yet* looks like, which is the ordinary state.
///
/// So the wait copy states the truth about the wait, and the screen offers the
/// one repair this side has: `Request again`, user-driven, which mints a fresh
/// KeyPackage. It never claims that is why.
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
    /// In flight, so `Request again` cannot be double-tapped into two journal
    /// rows. Separate from `state`, which STAYS `.waiting` across a re-request:
    /// going back through `.requesting` would hide the wait copy and read as a
    /// restart of something that never stopped.
    @Published public private(set) var requesting = false

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
        await request()
    }

    /// ADR-0045 S7 residual 1 — send the request. Reached once from ``run()``
    /// and again from the `Request again` control, which is the joiner's only
    /// repair: a fresh request mints a fresh KeyPackage. Deliberately
    /// user-driven and never automatic — each press is a journal row held to
    /// the DS's 30-day TTL, and a timer would spin against a group that is
    /// merely quiet.
    public func request() async {
        guard !requesting else { return }
        requesting = true
        defer { requesting = false }
        // A re-request replaces the poll rather than adding a second one.
        pollTask?.cancel()
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
                ProgressView(joinWaitPrimary)
                Text(joinWaitSecondary)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                Button(joinRequestAgain) {
                    Task { await viewModel.request() }
                }
                .disabled(viewModel.requesting)
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
