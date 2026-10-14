// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// ADR-0028 (G-3) — `/g/<slug>` group join-ticket landing.
///
/// A non-member opens an owner's `https://lakko.me/g/<slug>#<key>` link; this
/// screen decodes + verifies the `GroupInviteCapability`, opens its sealed
/// preview with the key from the URL fragment (ADR-0028 v2), shows the card of
/// `docs/ux/group-invite-page.md` section 3, and on `Request to join` KNOCKS
/// (ADR-0053): a probe learns the group's epoch and a second knock carries the
/// external-join proposal (ADR-0027). The Welcome then reaches this device's
/// own mailbox, the session inbox poll joins it, and this screen surfaces
/// "You joined the group." with "Open chat" once the group appears. The link conveys the
/// `group_id` and a claim about who is inviting to what; the admin approval
/// gate is the security boundary.
///
/// **The preview is shown, absent, or the link is refused - never guessed.**
/// No fragment renders the card without a preview, and the page does not say
/// why. A fragment that is not a 16-character link secret, or a secret the
/// preview does not open under, refuses the link as damaged (ADR-0028
/// amendment of 2026-09-27): the ledger id is no longer a hash of the
/// capability, and the preview key - bound to the capability's group id and
/// issuer - is the one check that the capability the server returned is the
/// one the link was made for. The issuer fingerprint stays at the foot of
/// every card as the one thing the inviter cannot fake.
///
/// **The wait is TWO-stage since ADR-0045 S7, and the copy says so.** An
/// admin's approve no longer admits anyone by itself: it publishes an `Add`
/// proposal, and the Welcome is sent only once a committer sweeps that
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
/// rather than an omission: nothing addressed to a non-member says "not
/// carried", and the joiner's mailbox is open for writes by anyone, so such a
/// notice would be a forgeable denial primitive. Inference does not work
/// either: the joiner cannot read the claimed journal (ADR-0053), and no
/// Welcome arriving is byte-for-byte what *no admin has approved yet* looks
/// like, which is the ordinary state.
///
/// So the wait copy states the truth about the wait, and the screen offers the
/// one repair this side has: `Request again`, user-driven, which mints a fresh
/// KeyPackage. It never claims that is why.
@MainActor
public final class JoinGroupLandingViewModel: ObservableObject {
    /// Local FSM. `loading` (resolve + verify the capability) → `ready` (the
    /// card, waiting for `Request to join`) → `requesting` → `waiting` (poll
    /// loop) → `joined`; `invalid` for a link that does not verify or has
    /// expired, `damaged` for a link whose fragment is present but does not
    /// open the preview, `failed` for everything else.
    public enum State: Equatable {
        case loading
        case ready
        case requesting
        case waiting
        case joined(groupIdHex: String)
        case invalid
        /// The fragment is not a link secret, or the preview does not open
        /// under it. Fail-closed: no preview-less fallback.
        case damaged
        /// ADR-0053 — one of the shared failure states; the raw error is
        /// logged, never shown.
        case failed(JoinRequestFailure)
    }

    @Published public private(set) var state: State = .loading

    /// The opened preview, `nil` without one (spec section 3). Set once the
    /// capability verifies and never cleared while the card is up, so it stays
    /// on the card through the wait.
    @Published public private(set) var preview: GroupInviteFacts?

    /// The issuer's key fingerprint - the foot of every card.
    @Published public private(set) var fingerprint: String?

    private let slug: String
    private let linkKeyFragment: String?
    private let repository: ChatRepository
    private let intake: InviteIntake
    private var didRun = false
    private var pollTask: Task<Void, Never>?
    /// The opened (plaintext) capability bytes + the ledger pointer they came from, kept
    /// from `run()` so `Request to join` and `Request again` post against the
    /// bytes the person was shown, not a second fetch.
    private var resolved: (capabilityWire: Data, blobId: Data?)?
    /// In flight, so `Request again` cannot be double-tapped into two journal
    /// rows. Separate from `state`, which STAYS `.waiting` across a re-request:
    /// going back through `.requesting` would hide the wait copy and read as a
    /// restart of something that never stopped.
    @Published public private(set) var requesting = false

    public init(
        slug: String,
        linkKeyFragment: String?,
        repository: ChatRepository,
        intake: InviteIntake
    ) {
        self.slug = slug
        self.linkKeyFragment = linkKeyFragment
        self.repository = repository
        self.intake = intake
    }

    /// The card's headline for the current preview (spec section 3).
    public var headline: GroupInviteHeadline { GroupInviteHeadline.derive(from: preview) }

    /// Resolve + verify the link exactly once (the `.task` modifier may re-fire
    /// across body rebuilds; the `didRun` guard keeps it to one pass).
    public func run() async {
        guard !didRun else { return }
        didRun = true
        await load()
    }

    /// Resolve the slug (a pointer fetches the sealed blob from the ledger; an
    /// inline link carries it), open it under the fragment's link secret
    /// (ADR-0028 amendment 2026-09-28 — this also verifies the signature),
    /// check the expiry and open the preview under the same secret. Nothing is
    /// posted here.
    private func load() async {
        // A missing or malformed fragment is a damaged link on every kind,
        // refused before anything is fetched for it.
        guard let secret = InviteLinkSecret.read(linkKeyFragment) else {
            state = .damaged
            return
        }
        let located: (sealed: Data, blobId: Data?)
        do {
            located = try await repository.resolveGroupInviteSlug(slug: slug)
        } catch let error as GroupInviteLinkError {
            NSLog("group join: the invite link resolved to nothing: %@", String(describing: error))
            state = .invalid
            return
        } catch {
            // The screen was left while the link was being looked up. Nothing
            // was posted and nothing failed: stay on the loading card and let
            // the next appearance look it up again.
            if Cancellation.endedByCancellation(error) {
                didRun = false
                return
            }
            NSLog("group join: resolving the invite link failed: %@", String(describing: error))
            state = .failed(JoinRequestFailure.classify(error))
            return
        }
        let cap: GroupInviteCapability
        do {
            cap = try GroupInviteCapability.openLink(sealed: located.sealed, linkSecret: secret)
        } catch {
            state = .damaged
            return
        }
        let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
        guard cap.isValidNow(nowMs: nowMs) else {
            state = .invalid
            return
        }
        switch Self.previewOutcome(of: cap, secret: secret) {
        case .none:
            preview = nil
        case .opened(let facts):
            preview = facts
        case .damaged:
            state = .damaged
            return
        }
        // The PLAINTEXT capability: what the consume's joiner tag and the join
        // attribution are computed over.
        resolved = (cap.wire(), located.blobId)
        fingerprint = cap.issuerFingerprint()
        state = .ready
    }

    /// What the link's secret does to the card.
    enum PreviewOutcome: Equatable {
        /// The capability carries no preview (a co-publisher link): the card
        /// without one.
        case none
        /// The preview opened under the link secret.
        case opened(GroupInviteFacts)
        /// The capability carries a preview that does not open under the
        /// secret the blob opened with.
        case damaged
    }

    /// Decide the preview for a capability the link secret already opened.
    static func previewOutcome(
        of cap: GroupInviteCapability, secret: Data
    ) -> PreviewOutcome {
        guard cap.hasPreview() else { return .none }
        guard let opened = try? cap.openPreview(linkKey: secret) else { return .damaged }
        return .opened(GroupInviteFacts(
            groupName: opened.groupName,
            inviterName: opened.inviterName,
            memberCount: opened.memberCount))
    }

    /// ADR-0045 S7 residual 1 — send the request. Reached from the card's
    /// `Request to join` and again from the `Request again` control, which is
    /// the joiner's only repair: a fresh request mints a fresh KeyPackage.
    /// Deliberately user-driven and never automatic — each press is a journal
    /// row held to the DS's 30-day TTL, and a timer would spin against a group
    /// that is merely quiet.
    public func request() async {
        guard !requesting, let resolved else { return }
        requesting = true
        defer { requesting = false }
        if state == .ready { state = .requesting }
        // A re-request replaces the poll rather than adding a second one.
        pollTask?.cancel()
        let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
        do {
            // Request to join — consuming the ledger row when the link was a
            // pointer (ADR-0028 OQ1).
            let result = try await repository.requestToJoinFromLink(
                capabilityWire: resolved.capabilityWire,
                nowMs: nowMs,
                blobId: resolved.blobId
            )
            state = .waiting
            startPolling(groupId: result.groupId)
        } catch {
            NSLog("group join: the request failed: %@", String(describing: error))
            state = .failed(JoinRequestFailure.classify(error))
        }
    }

    /// "Try again" on the generic failure: re-run whatever failed — the link
    /// resolve when it never resolved, the request otherwise.
    public func retry() async {
        guard case .failed = state else { return }
        if resolved == nil {
            state = .loading
            await load()
        } else {
            state = .ready
            await request()
        }
    }

    private func startPolling(groupId: Data) {
        let repo = repository
        let groupIdHex = DsClient.pseudonymHex(groupId)
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                // ADR-0053 point 6 — the Welcome reaches this device's own
                // mailbox (the sweep addresses every add there, a link joiner
                // included), and the session-wide inbox poll joins it. So the
                // wait is on the group appearing, never on the group journal,
                // which a non-member cannot read.
                if repo.hasGroup(groupIdHex: groupIdHex) {
                    self?.state = .joined(groupIdHex: groupIdHex)
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

/// The landing view for a tapped `/g/<slug>` group join-ticket link: one card
/// whose headline changes with the state while the controls do not (spec
/// section 3).
public struct JoinGroupLandingScreen: View {
    @StateObject private var viewModel: JoinGroupLandingViewModel
    private let onDone: () -> Void
    /// Open the joined group (replaces this screen with the chat).
    private let onOpenChat: (String) -> Void
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    public init(
        viewModel: JoinGroupLandingViewModel,
        onDone: @escaping () -> Void,
        onOpenChat: @escaping (String) -> Void
    ) {
        self._viewModel = StateObject(wrappedValue: viewModel)
        self.onDone = onDone
        self.onOpenChat = onOpenChat
    }

    public var body: some View {
        ScrollView {
            card
                .padding(16)
        }
        .background(Color.lakkoBackground.ignoresSafeArea())
        .navigationTitle(l10n.t("group.join.title"))
        .task { await viewModel.run() }
    }

    @ViewBuilder
    private var card: some View {
        VStack(alignment: .leading, spacing: 16) {
            switch viewModel.state {
            case .loading:
                ProgressView()
                    .frame(maxWidth: .infinity)
            case .damaged:
                Text(groupInviteDamagedMessage)
                    .foregroundStyle(Color.lakkoText)
                    .fixedSize(horizontal: false, vertical: true)
                Button(l10n.t("common.close")) {
                    viewModel.dismiss()
                    onDone()
                }
                .tint(.lakkoAccent)
            case .invalid:
                Text(groupInviteInvalidMessage)
                    .foregroundStyle(Color.lakkoText)
                    .fixedSize(horizontal: false, vertical: true)
                Button(l10n.t("common.close")) {
                    viewModel.dismiss()
                    onDone()
                }
                .tint(.lakkoAccent)
            case .failed(let failure):
                headlineBlock
                Text(failure.message)
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoDanger)
                    .fixedSize(horizontal: false, vertical: true)
                if failure.offersRetry {
                    Button(joinTryAgain) {
                        Task { await viewModel.retry() }
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(.lakkoAccent)
                    .disabled(viewModel.requesting)
                }
                Button(l10n.t("common.close")) {
                    viewModel.dismiss()
                    onDone()
                }
                .tint(.lakkoAccent)
                footer
            case .ready:
                headlineBlock
                Button(l10n.t("group.join.request")) {
                    Task { await viewModel.request() }
                }
                .buttonStyle(.borderedProminent)
                .tint(.lakkoAccent)
                .disabled(viewModel.requesting)
                footer
            case .requesting:
                headlineBlock
                ProgressView(l10n.t("group.join.sending"))
                    .tint(.lakkoAccent)
                footer
            case .waiting:
                headlineBlock
                Text(joinWaitPrimary)
                    .foregroundStyle(Color.lakkoText)
                    .fixedSize(horizontal: false, vertical: true)
                Text(joinWaitSecondary)
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .fixedSize(horizontal: false, vertical: true)
                Button(joinRequestAgain) {
                    Task { await viewModel.request() }
                }
                .tint(.lakkoAccent)
                .disabled(viewModel.requesting)
                footer
            case .joined(let groupIdHex):
                headlineBlock
                Text(joinSucceededMessage)
                    .foregroundStyle(Color.lakkoText)
                    .fixedSize(horizontal: false, vertical: true)
                Button(joinOpenChat) {
                    viewModel.dismiss()
                    onOpenChat(groupIdHex)
                }
                .buttonStyle(.borderedProminent)
                .tint(.lakkoAccent)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(16)
        .background(Color.lakkoSurface)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous)
                .stroke(Color.lakkoBorder, lineWidth: 1)
        )
    }

    /// `<Inviter> invites you to` / `You are invited to`, the group name (or
    /// ` a group` folded into the first line), `1 member` / `N members`.
    private var headlineBlock: some View {
        let headline = viewModel.headline
        return VStack(alignment: .leading, spacing: 4) {
            Text(headline.lead)
                .font(.title3)
                .foregroundStyle(Color.lakkoText)
                .fixedSize(horizontal: false, vertical: true)
            if let group = headline.groupName {
                Text(group)
                    .font(.title2)
                    .bold()
                    .foregroundStyle(Color.lakkoText)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let members = headline.members {
                Text(members)
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
        }
        .accessibilityElement(children: .combine)
    }

    /// `Invited by <fingerprint>` - small, muted, one line, so a person who was
    /// told the digits out of band can still compare them.
    @ViewBuilder
    private var footer: some View {
        if let fingerprint = viewModel.fingerprint {
            Text(GroupInviteHeadline.footer(fingerprint: fingerprint))
                .font(.caption)
                .foregroundStyle(Color.lakkoTextMuted)
                .lineLimit(1)
                .minimumScaleFactor(0.6)
                .textSelection(.enabled)
        }
    }
}
