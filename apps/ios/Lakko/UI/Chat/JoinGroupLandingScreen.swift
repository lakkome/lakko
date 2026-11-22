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
/// own mailbox, the session inbox poll joins it, and once the group appears
/// this screen moves straight to its chat. The link conveys the
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
/// one the link was made for.
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
/// So the wait copy states the truth about the wait and offers nothing to
/// press for a day: a device asks once per group per day
/// (`RequestsSentStore.askAgainAfterMs`, `docs/ux/group-invite-page.md`
/// "After `Request to join`"). After a day the page says "No answer yet." and
/// offers `Ask again`, the same request the Requests sent list's `Ask again`
/// sends, which mints a fresh KeyPackage and restarts the day. It never
/// claims that is why. Opened again for a group this device already asked to
/// join, the page goes straight to the same state.
@MainActor
public final class JoinGroupLandingViewModel: ObservableObject {
    /// Local FSM. `loading` (resolve + verify the capability) → `ready` (the
    /// card, waiting for `Request to join`) → `requesting` → `waiting` (poll
    /// loop; `noAnswer` once the request is a day old) → `joined` (the view
    /// opens the chat); `invalid` for a link that does not verify or has
    /// expired, `damaged` for a link whose fragment is present but does not
    /// open the preview, `failed` for everything else.
    public enum State: Equatable {
        case loading
        case ready
        case requesting
        /// Sent under a day ago: the waiting line, nothing to press.
        case waiting
        /// Sent a day ago or more: "No answer yet." and `Ask again`.
        case noAnswer
        case joined(groupIdHex: String)
        case invalid
        /// The fragment is not a link secret, or the preview does not open
        /// under it. Fail-closed: no preview-less fallback.
        case damaged
        /// The ledger could not be asked for the link (the network, a server
        /// error): the link may be fine, the page offers it again.
        case openFailed
        /// ADR-0053 — one of the shared failure states; the raw error is
        /// logged, never shown.
        case failed(JoinRequestFailure)
    }

    @Published public private(set) var state: State = .loading

    /// The opened preview, `nil` without one (spec section 3). Set once the
    /// capability verifies and never cleared while the card is up, so it stays
    /// on the card through the wait.
    @Published public private(set) var preview: GroupInviteFacts?
    /// ADR-0087 section 7.2 — the link is a channel subscriber invitation
    /// (preview kind 3): the page is a channel's link page.
    @Published public private(set) var channel: ChannelLinkFacts?
    /// ADR-0087 section 15.2 — the link invites to publish in a channel
    /// (preview kind 2): the page says "{name} invites you to be a publisher
    /// of {channel}." with `Accept`, which runs the join in place, as the
    /// card in a chat does. Never the group page (seen live 2026-11-12:
    /// "Icons QA invites you to a group, 0 members").
    @Published private(set) var publisher: CoPublisherCardFacts?

    private let slug: String
    private let linkKeyFragment: String?
    private let repository: ChatRepository
    private let intake: InviteIntake
    private var didRun = false
    private var pollTask: Task<Void, Never>?
    /// The opened (plaintext) capability bytes + the ledger pointer they came from, kept
    /// from `run()` so `Request to join` and `Ask again` post against the
    /// bytes the person was shown, not a second fetch.
    private var resolved: (capabilityWire: Data, blobId: Data)?
    /// In flight, so `Ask again` cannot be double-tapped into two journal
    /// rows. Separate from `state`, which STAYS `.noAnswer` while an
    /// `Ask again` is sent: going back through `.requesting` would hide the
    /// card's line and read as a restart of something that never stopped.
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

    /// Resolve the slug (the sealed blob is fetched from the ledger; an inline
    /// slug, the removed form, is invalid), open it under the fragment's link secret
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
        let located: (sealed: Data, blobId: Data)
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
            // A link that is not there was answered above; this is a failure
            // to ask the ledger, which says nothing about the link.
            let failure = JoinRequestFailure.classify(error)
            state = failure == .generic ? .openFailed : .failed(failure)
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
        case .channel(let facts, let channel):
            preview = facts
            self.channel = channel
        case .publisher(let facts, let invite):
            preview = facts
            // The link `Accept` runs: the same form the card in a chat
            // carries, so the card and the page show one join's progress.
            guard let base = try? GroupInviteUrl.composePointerGroupInviteUrl(
                    blobId: located.blobId, origin: ApiEndpoints.webOrigin),
                  let link = URL(string: InviteLinkSecret.link(base: base, secret: secret)) else {
                state = .damaged
                return
            }
            publisher = CoPublisherCardFacts(
                channelIdHex: invite.channelIdHex, channelName: invite.channelName,
                inviterName: invite.inviterName, link: link, avatar: invite.avatar)
        case .damaged:
            state = .damaged
            return
        }
        // The PLAINTEXT capability: what the consume's joiner tag and the join
        // attribution are computed over.
        resolved = (cap.wire(), located.blobId)
        // A channel's link page subscribes, a publisher invitation accepts;
        // neither has a join request.
        guard channel == nil, publisher == nil else {
            state = .ready
            return
        }
        // Opened again for a group this device already asked to join: the
        // same state as right after the request, never a second
        // `Request to join` inside the day.
        let groupId = cap.groupId()
        let sentAtMs = AcceptedContactsStore.boundAccountIdB64.flatMap {
            RequestsSentStore.entry(accountIdB64: $0, groupIdHex: AttachmentBridge.hex(groupId))?.sentAtMs
        }
        let groupIdHex = DsClient.pseudonymHex(groupId)
        state = Self.stateOnOpen(
            groupIdHex: groupIdHex, requestSentAtMs: sentAtMs,
            groupHeld: repository.hasGroup(groupIdHex: groupIdHex),
            nowMs: Int64(Date().timeIntervalSince1970 * 1000))
        if state == .waiting || state == .noAnswer {
            startPolling(groupId: groupId)
        }
    }

    /// The page's state once the link opened, pure: `ready` when this device
    /// never asked to join; the joined group when it asked and the group is
    /// here; otherwise the wait, by the request's age.
    nonisolated static func stateOnOpen(
        groupIdHex: String, requestSentAtMs: Int64?, groupHeld: Bool, nowMs: Int64
    ) -> State {
        guard let requestSentAtMs else { return .ready }
        if groupHeld { return .joined(groupIdHex: groupIdHex) }
        return waitState(sentAtMs: requestSentAtMs, nowMs: nowMs)
    }

    /// Under a day: the waiting line with nothing to press; a day or more:
    /// "No answer yet." and `Ask again`. The rule of the Requests sent list.
    nonisolated static func waitState(sentAtMs: Int64, nowMs: Int64) -> State {
        RequestsSentStore.mayAskAgain(sentAtMs: sentAtMs, nowMs: nowMs) ? .noAnswer : .waiting
    }

    /// The chat the page hands over to: set once the join landed.
    nonisolated static func chatToOpen(_ state: State) -> String? {
        if case .joined(let groupIdHex) = state { return groupIdHex }
        return nil
    }

    /// What a publisher invitation link (preview kind 2) says.
    struct PublisherLinkFacts: Equatable {
        let channelIdHex: String
        let channelName: String
        let inviterName: String?
        let avatar: ProfileAvatar?
    }

    /// What the link's secret does to the card.
    enum PreviewOutcome: Equatable {
        /// The capability carries no preview: the card without one.
        case none
        /// The preview opened under the link secret.
        case opened(GroupInviteFacts)
        /// A channel subscriber invitation (ADR-0087 section 7, kind 3).
        case channel(GroupInviteFacts, ChannelLinkFacts)
        /// A publisher invitation (ADR-0087 section 15.2, kind 2): a channel
        /// id and no subscriber keys.
        case publisher(GroupInviteFacts, PublisherLinkFacts)
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
        if let subscriber = opened.subscriber, let channelId = opened.channelId, channelId.count == 16 {
            return .channel(
                GroupInviteFacts(
                    groupName: opened.groupName, inviterName: opened.inviterName,
                    memberCount: opened.memberCount),
                ChannelLinkFacts(
                    channelIdHex: AttachmentBridge.hex(channelId), name: opened.groupName ?? "",
                    description: subscriber.description, access: subscriber.access,
                    avatar: ProfileAvatar(opened.avatar)))
        }
        if let channelId = opened.channelId, channelId.count == 16 {
            return .publisher(
                GroupInviteFacts(
                    groupName: opened.groupName, inviterName: opened.inviterName,
                    memberCount: opened.memberCount),
                PublisherLinkFacts(
                    channelIdHex: AttachmentBridge.hex(channelId), channelName: opened.groupName ?? "",
                    inviterName: opened.inviterName, avatar: ProfileAvatar(opened.avatar)))
        }
        return .opened(GroupInviteFacts(
            groupName: opened.groupName,
            inviterName: opened.inviterName,
            memberCount: opened.memberCount))
    }

    /// ADR-0045 S7 residual 1 — send the request. Reached from the card's
    /// `Request to join` and, a day later, from `Ask again`, which is the
    /// joiner's only repair: a fresh request mints a fresh KeyPackage and
    /// restarts the day. Deliberately user-driven and never automatic — each
    /// press is a journal row held to the DS's 30-day TTL, and a timer would
    /// spin against a group that is merely quiet.
    public func request() async {
        // Inside the day there is nothing to press; refuse a stray call too.
        guard !requesting, state != .waiting, let resolved else { return }
        requesting = true
        defer { requesting = false }
        if state == .ready { state = .requesting }
        // A re-request replaces the poll rather than adding a second one.
        pollTask?.cancel()
        let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
        do {
            // Request to join — consuming the ledger row (ADR-0028 OQ1).
            let result = try await repository.requestToJoinFromLink(
                capabilityWire: resolved.capabilityWire,
                nowMs: nowMs,
                blobId: resolved.blobId
            )
            // Section 6.2 — the request shows under `Requests sent` until the
            // group appears.
            if let accountIdB64 = AcceptedContactsStore.boundAccountIdB64 {
                RequestsSentStore.record(
                    groupId: result.groupId, name: preview?.groupName,
                    capabilityWire: resolved.capabilityWire, blobId: resolved.blobId,
                    accountIdB64: accountIdB64, nowMs: Int64(nowMs))
            }
            state = Self.waitState(sentAtMs: Int64(nowMs), nowMs: Int64(nowMs))
            startPolling(groupId: result.groupId)
        } catch {
            NSLog("group join: the request failed: %@", String(describing: error))
            state = .failed(JoinRequestFailure.classify(error))
        }
    }

    /// "Try again" on the generic failure: re-run whatever failed — the link
    /// resolve when it never resolved, the request otherwise.
    public func retry() async {
        if state == .openFailed {
            state = .loading
            await load()
            return
        }
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
    /// ADR-0087 section 7.2 — `Subscribe` on a channel's link page.
    private let onSubscribeChannel: (ChannelLinkFacts) -> Void
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    public init(
        viewModel: JoinGroupLandingViewModel,
        onDone: @escaping () -> Void,
        onOpenChat: @escaping (String) -> Void,
        onSubscribeChannel: @escaping (ChannelLinkFacts) -> Void = { _ in }
    ) {
        self._viewModel = StateObject(wrappedValue: viewModel)
        self.onDone = onDone
        self.onOpenChat = onOpenChat
        self.onSubscribeChannel = onSubscribeChannel
    }

    public var body: some View {
        ScrollView {
            card
                .padding(16)
        }
        .background(Color.lakkoBackground.ignoresSafeArea())
        // A channel's link page is titled with the channel (as on web), never
        // "Join a group".
        .navigationTitle(viewModel.channel.map { $0.name.isEmpty ? l10n.t("channels.meta.title") : $0.name }
            ?? (viewModel.publisher != nil ? l10n.t("channels.join.title") : l10n.t("group.join.title")))
        .task { await viewModel.run() }
        // The join landed: straight to the group's chat, no extra step.
        .onChange(of: viewModel.state) { _, state in
            guard let groupIdHex = JoinGroupLandingViewModel.chatToOpen(state) else { return }
            viewModel.dismiss()
            onOpenChat(groupIdHex)
        }
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
            case .openFailed:
                Text(inviteLinkOpenFailedMessage)
                    .foregroundStyle(Color.lakkoText)
                    .fixedSize(horizontal: false, vertical: true)
                Button(joinTryAgain) {
                    Task { await viewModel.retry() }
                }
                .buttonStyle(.borderedProminent)
                .tint(.lakkoAccent)
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
            case .ready where viewModel.publisher != nil:
                if let publisher = viewModel.publisher {
                    publisherBlock(publisher)
                }
            case .ready where viewModel.channel != nil:
                if let channel = viewModel.channel {
                    channelBlock(channel)
                    Button(l10n.t("channels.subscribe.action")) {
                        viewModel.dismiss()
                        onSubscribeChannel(channel)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(.lakkoAccent)
                }
            case .ready:
                headlineBlock
                Button(l10n.t("group.join.request")) {
                    Task { await viewModel.request() }
                }
                .buttonStyle(.borderedProminent)
                .tint(.lakkoAccent)
                .disabled(viewModel.requesting)
            case .requesting:
                headlineBlock
                ProgressView(l10n.t("group.join.sending"))
                    .tint(.lakkoAccent)
            case .waiting:
                // Inside the day: the one line, nothing to press.
                headlineBlock
                Text(joinWaitPrimary)
                    .foregroundStyle(Color.lakkoText)
                    .fixedSize(horizontal: false, vertical: true)
            case .noAnswer:
                headlineBlock
                Text(l10n.t("requests_sent.no_answer"))
                    .foregroundStyle(Color.lakkoText)
                    .fixedSize(horizontal: false, vertical: true)
                Button(l10n.t("requests_sent.ask_again")) {
                    Task { await viewModel.request() }
                }
                .buttonStyle(.bordered)
                .tint(.lakkoAccent)
                .disabled(viewModel.requesting)
            case .joined:
                // Shown only while the chat opens (the `onChange` below).
                headlineBlock
                Text(l10n.t("group.join.succeeded_opening"))
                    .foregroundStyle(Color.lakkoText)
                    .fixedSize(horizontal: false, vertical: true)
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

    /// Section 7.2 — "Anna invites you to / Berlin news / Private channel".
    private func channelBlock(_ channel: ChannelLinkFacts) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            InitialsAvatar(
                name: channel.name, identity: channel.channelIdHex, size: 64,
                placeholderIcon: .megaphoneFill, photo: channel.avatar.map(AvatarPhoto.avatar),
                playsVideo: true)
            Text(viewModel.headline.lead)
                .font(.title3)
                .foregroundStyle(Color.lakkoText)
            Text(channel.name)
                .font(.title2).bold()
                .foregroundStyle(Color.lakkoText)
            Text(l10n.t(channel.isPublic ? "channels.header.public" : "channels.header.private"))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
            if !channel.description.isEmpty {
                Text(channel.description)
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoText)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .accessibilityElement(children: .combine)
    }

    /// Section 15.2 — "Icons QA invites you to be a publisher of Walk 22
    /// channel." and the card's own control: `Accept` runs the join here,
    /// then the waiting line; `Open` once this device publishes.
    private func publisherBlock(_ facts: CoPublisherCardFacts) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            InitialsAvatar(
                name: facts.displayName, identity: facts.channelIdHex, size: 64,
                placeholderIcon: .megaphoneFill, photo: facts.avatar.map(AvatarPhoto.avatar),
                playsVideo: true)
            Text(facts.inviteLine)
                .font(.title3)
                .foregroundStyle(Color.lakkoText)
                .fixedSize(horizontal: false, vertical: true)
            CoPublisherInviteAction(facts: facts)
        }
    }
}

/// ADR-0087 section 7 — what a channel subscriber invitation link carries.
public struct ChannelLinkFacts: Equatable {
    public let channelIdHex: String
    public let name: String
    public let description: String
    public let access: FfiChannelAccess
    public let avatar: ProfileAvatar?

    public var isPublic: Bool {
        if case .public = access { return true }
        return false
    }
}
