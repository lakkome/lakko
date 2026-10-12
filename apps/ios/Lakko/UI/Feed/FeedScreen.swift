// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI
import UIKit

/// The Feed destination (`docs/ux/feed.md`, ADR-0063). Mirror of web
/// `routes/Feed.tsx`.
///
/// What is here: the feed switcher (presets, saved feeds, New feed), Edit and
/// Share on a saved feed, the one-time device banner, the items with their
/// reactions and folded replies, opening an item in its chat or channel, the
/// "Show N new" pill and pull to refresh, and the empty, loading and caught-up
/// states. An item fully on screen for two seconds counts as seen.
///
/// Discover (ADR-0059, UX spec section 6) is the fourth chip: channel
/// suggestions instead of items (``DiscoverView``). A saved feed whose rule
/// asks for suggestions ("rarely" / "sometimes") mixes the same cards in
/// between its items (``DiscoverCards/mix(itemCount:cardCount:rate:)``).
///
/// Also here (UX spec sections 4.1 and 5): each chip's icon, the item menu
/// (a context menu: Open, Reply, Copy, Forward, Hide from feeds — a long press
/// on a reply inside an expanded item opens the parent item's menu), the
/// hidden list in the feed menu with the "Hidden from feeds." toast and its
/// Undo, the "New" divider per feed, and the refresh failure banner.
///
/// Not yet here (as on web): unpinning a preset.
struct FeedScreen: View {
    @ObservedObject var model: FeedViewModel
    /// Open a group at a message.
    let onOpenGroup: (_ groupIdHex: String, _ messageId: Int64) -> Void
    /// Open a group at a message with the reply strip set to it.
    let onReplyGroup: (_ groupIdHex: String, _ messageId: Int64) -> Void
    /// Open the conversation a forward was just sent to.
    let onOpenConversation: (_ groupIdHex: String) -> Void
    /// Open a channel.
    let onOpenChannel: (_ channelId: String) -> Void
    /// Go to the conversation list.
    let onGoToChats: () -> Void
    /// ADR-0059 — open the subscribe screen for a channel a Discover card's
    /// handle was verified to name.
    let onOpenFoundChannel: (_ channelId: String, _ handle: String) -> Void

    static let scrollSpace = "feed-scroll"
    /// Within this many points of the top, new items show at once.
    private static let topSlack: CGFloat = 40

    @State private var expanded: Set<String> = []
    @State private var showingHidden = false
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared
    private static let topId = "feed-top"

    var body: some View {
        VStack(spacing: 0) {
            switcher
            if let saved = model.selectedSaved {
                savedActions(saved)
            }
            list
        }
        .background(Color.lakkoBackground)
        .navigationTitle(l10n.t("feed.title"))
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button(l10n.t("feed.new")) { beginNew() }
                    Button(l10n.t("feed.hidden.title")) { showingHidden = true }
                    Button(l10n.t("feed.import.title")) {
                        model.importing = FeedImportRequest(bytes: nil, refused: false)
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .accessibilityLabel(l10n.t("feed.menu.a11y"))
            }
        }
        .navigationDestination(item: $model.editing) { editing in
            if let initial = model.editorInitial(editing) {
                FeedEditorScreen(
                    model: model,
                    editing: editing,
                    initial: initial)
            }
        }
        .sheet(item: $model.sharing) { saved in
            FeedShareSheet(saved: saved)
        }
        .sheet(item: $model.importing) { request in
            FeedImportScreen(model: model, request: request)
        }
        .sheet(isPresented: $showingHidden) {
            HiddenFromFeedsSheet(model: model, reading: model.reading)
        }
        .sheet(
            isPresented: Binding(
                get: { model.forwarding != nil },
                set: { open in if !open { model.forwarding = nil } })
        ) {
            if let ref = model.forwarding {
                ForwardPickerScreen(
                    rows: model.forwardTargets(),
                    onPick: { row in
                        Task { @MainActor in
                            if await model.forward(ref, to: row.id) { onOpenConversation(row.id) }
                        }
                    },
                    onCancel: { model.forwarding = nil })
            }
        }
        .overlay(alignment: .bottom) { toast }
        .onAppear {
            model.start()
            model.discover.onOpen = { card in onOpenFoundChannel(card.channelIdHex, card.handle) }
            model.discover.onOpenChannel = { onOpenChannel($0) }
        }
        .onDisappear { model.stop() }
    }

    private func beginNew() {
        model.editError = nil
        model.editing = .new
    }

    // MARK: - Switcher

    private var switcher: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(FeedPresetKey.allCases, id: \.self) { key in
                    chip(key.label, symbol: FeedIcons.presetSymbol(key), selected: model.selection == .preset(key)) {
                        model.choose(.preset(key))
                    }
                }
                chip(l10n.t("feed.preset.discover"), symbol: FeedIcons.discoverSymbol, selected: model.selection == .discover) {
                    model.choose(.discover)
                }
                ForEach(model.saved) { saved in
                    chip(
                        saved.rule.name, symbol: FeedIcons.symbol(saved.rule.icon),
                        selected: model.selection == .saved(saved.id)
                    ) {
                        model.choose(.saved(saved.id))
                    }
                }
                Button {
                    beginNew()
                } label: {
                    Label(l10n.t("feed.new"), systemImage: "plus")
                        .font(.subheadline)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 6)
                        .overlay(
                            Capsule().strokeBorder(Color.lakkoBorder, style: StrokeStyle(lineWidth: 1, dash: [4])))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Color.lakkoTextMuted)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(l10n.t("feed.switcher.a11y"))
    }

    private func chip(
        _ label: String, symbol: String, selected: Bool, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 5) {
                Image(systemName: symbol)
                    .font(.caption)
                    .accessibilityHidden(true)
                Text(label)
                    .lineLimit(1)
            }
            .font(.subheadline.weight(selected ? .semibold : .regular))
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(Capsule().fill(selected ? Color.lakkoAccent : Color.lakkoSurfaceSunken))
            .foregroundStyle(selected ? Color.lakkoAccentForeground : Color.lakkoText)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? [.isSelected] : [])
    }

    private func savedActions(_ saved: SavedFeedView) -> some View {
        HStack(spacing: 16) {
            Button(l10n.t("feed.edit")) {
                model.editError = nil
                model.editing = .edit(saved.id)
            }
            Button(l10n.t("feed.share")) { model.sharing = saved }
            Spacer()
        }
        .font(.subheadline)
        .padding(.horizontal, 16)
        .padding(.bottom, 6)
    }

    // MARK: - List

    private var list: some View {
        GeometryReader { outer in
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        topSentinel
                            .id(Self.topId)
                        if model.refreshFailed && model.selection != .discover {
                            refreshFailedBanner
                        }
                        if model.selection == .discover {
                            DiscoverView(model: model.discover)
                        } else {
                            if !model.bannerDismissed && model.candidateCount > 0 {
                                banner
                            }
                            content(viewportHeight: outer.size.height)
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.bottom, 24)
                }
                .coordinateSpace(name: Self.scrollSpace)
                .refreshable {
                    await model.refresh()
                    proxy.scrollTo(Self.topId, anchor: .top)
                }
                .overlay(alignment: .top) {
                    if model.selection != .discover && model.newCount > 0 {
                        Button {
                            model.showPending()
                            withAnimation { proxy.scrollTo(Self.topId, anchor: .top) }
                        } label: {
                            Text(l10n.t("feed.show_new", counts: ["count": UInt64(max(0, model.newCount))]))
                                .font(.subheadline.weight(.semibold))
                                .padding(.horizontal, 14)
                                .padding(.vertical, 8)
                                .background(Capsule().fill(Color.lakkoAccent))
                                .foregroundStyle(Color.lakkoAccentForeground)
                        }
                        .buttonStyle(.plain)
                        .padding(.top, 8)
                    }
                }
            }
        }
    }

    private var refreshFailedBanner: some View {
        HStack(spacing: 10) {
            Image(systemName: "exclamationmark.triangle")
                .foregroundStyle(Color.lakkoDanger)
                .accessibilityHidden(true)
            Text(l10n.t("feed.refresh_failed"))
                .font(.subheadline)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button(l10n.t("common.retry")) { Task { await model.refresh() } }
                .font(.subheadline)
        }
        .padding(12)
        .background(RoundedRectangle(cornerRadius: LakkoRadius.card).fill(Color.lakkoSurfaceSunken))
    }

    /// The "Hidden from feeds." toast with Undo, or a failed forward's notice.
    @ViewBuilder
    private var toast: some View {
        if model.hiddenToast != nil {
            HStack(spacing: 12) {
                Text(l10n.t("feed.hidden.toast"))
                    .font(.subheadline)
                Button(l10n.t("common.undo")) { model.undoHide() }
                    .font(.subheadline.weight(.semibold))
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
            .background(Capsule().fill(Color.lakkoSurfaceSunken))
            .overlay(Capsule().strokeBorder(Color.lakkoBorder))
            .padding(.bottom, 12)
            .accessibilityElement(children: .contain)
        } else if let notice = model.notice {
            Text(notice)
                .font(.subheadline)
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
                .background(Capsule().fill(Color.lakkoSurfaceSunken))
                .overlay(Capsule().strokeBorder(Color.lakkoBorder))
                .padding(.bottom, 12)
                .accessibilityAddTraits(.isStaticText)
        }
    }

    /// Reports whether the reader is at the top, where new items show at once.
    private var topSentinel: some View {
        GeometryReader { geo in
            let y = geo.frame(in: .named(Self.scrollSpace)).minY
            Color.clear
                .onChange(of: y >= -Self.topSlack) { _, atTop in model.nearTop = atTop }
        }
        .frame(height: 0)
    }

    private var banner: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "info.circle")
                .foregroundStyle(Color.lakkoTextMuted)
            Text(l10n.t("feed.banner.local"))
                .font(.subheadline)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button(l10n.t("feed.banner.dismiss")) { model.dismissBanner() }
                .font(.subheadline)
        }
        .padding(12)
        .background(RoundedRectangle(cornerRadius: LakkoRadius.card).fill(Color.lakkoSurfaceSunken))
    }

    @ViewBuilder
    private func content(viewportHeight: CGFloat) -> some View {
        if let items = model.shown {
            if items.isEmpty {
                emptyState
            } else {
                let divider = model.dividerIndex
                ForEach(Array(items.enumerated()), id: \.element.key) { index, item in
                    if divider == index {
                        newDivider
                    }
                    itemView(item, viewportHeight: viewportHeight)
                    // A saved feed that asks for suggestions shows a channel
                    // card after some items (ADR-0059, UX spec 4.1 section 4).
                    if model.suggestionRate != .never {
                        MixedSuggestion(
                            discover: model.discover, itemIndex: index, itemCount: items.count,
                            rate: model.suggestionRate)
                    }
                }
                Text(l10n.t("feed.end"))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 12)
            }
        } else {
            // Skeleton items, never a spinner over an empty screen.
            ForEach(0..<3, id: \.self) { _ in
                RoundedRectangle(cornerRadius: LakkoRadius.card)
                    .fill(Color.lakkoSurfaceSunken)
                    .frame(height: 88)
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(l10n.t("common.loading"))
        }
    }

    @ViewBuilder
    private var emptyState: some View {
        VStack(spacing: 12) {
            if model.sourceCount == 0 {
                Text(l10n.t("feed.empty.no_sources"))
                    .multilineTextAlignment(.center)
                Button(l10n.t("feed.empty.discover")) { model.choose(.discover) }
                    .buttonStyle(.borderedProminent)
            } else if model.candidateCount == 0 {
                Text(l10n.t("feed.empty.nothing_yet"))
                    .multilineTextAlignment(.center)
            } else {
                Text(l10n.t("feed.empty.no_match"))
                if let saved = model.selectedSaved {
                    Button(l10n.t("feed.edit_feed")) { model.editing = .edit(saved.id) }
                        .buttonStyle(.borderedProminent)
                }
            }
        }
        .foregroundStyle(Color.lakkoTextMuted)
        .frame(maxWidth: .infinity)
        .padding(.top, 48)
    }

    /// "New since you last looked" (UX spec section 5).
    private var newDivider: some View {
        HStack(spacing: 8) {
            Rectangle().fill(Color.lakkoAccent).frame(height: 1)
            Text(l10n.t("feed.divider.new"))
                .font(.caption.weight(.semibold))
                .foregroundStyle(Color.lakkoAccent)
            Rectangle().fill(Color.lakkoAccent).frame(height: 1)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(l10n.t("feed.divider.new"))
        .accessibilityAddTraits(.isHeader)
    }

    // MARK: - Item

    /// The item menu (UX spec section 5). A reply row inside an expanded item
    /// shows its parent's menu.
    @ViewBuilder
    private func menu(_ ref: FeedCandidateRef) -> some View {
        Button {
            open(ref)
        } label: {
            Label(l10n.t("common.open"), systemImage: "arrow.up.right")
        }
        if FeedForward.canReply(ref) {
            Button {
                onReplyGroup(ref.source.idHex, ref.message.messageId)
            } label: {
                Label(l10n.t("chat.message.reply"), systemImage: "arrowshape.turn.up.left")
            }
        }
        Button {
            model.copyText(ref)
        } label: {
            Label(l10n.t("common.copy"), systemImage: "doc.on.doc")
        }
        .disabled(!FeedForward.canCopy(ref.message))
        Button {
            model.forwarding = ref
        } label: {
            Label(l10n.t("chat.message.forward"), systemImage: "arrowshape.turn.up.right")
        }
        Divider()
        Button {
            model.hide(ref.source.key)
        } label: {
            Label(l10n.t("feed.action.hide"), systemImage: "eye.slash")
        }
        .accessibilityHint(l10n.t("feed.action.hide_hint"))
    }

    private func open(_ ref: FeedCandidateRef) {
        switch ref.source.kind {
        case .channel: onOpenChannel(ref.source.idHex)
        case .group: onOpenGroup(ref.source.idHex, ref.message.messageId)
        }
    }

    private func timeText(_ message: FeedMessage) -> String {
        guard let ms = message.timeMs else { return "" }
        return ChatListFormatting.lastMessageTime(
            ms: ms, nowMs: model.nowMs, calendar: .current, locale: .current)
    }

    private func itemView(_ item: RankedFeedItem, viewportHeight: CGFloat) -> some View {
        let ref = item.ref
        return VStack(alignment: .leading, spacing: 8) {
            Button {
                open(ref)
            } label: {
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Text(ref.source.title)
                            .font(.subheadline.weight(.semibold))
                            .lineLimit(1)
                        Spacer()
                        Text(timeText(ref.message))
                            .font(.caption)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    // A channel speaks as itself: its name is already the source.
                    if ref.source.kind == .group {
                        Text(model.author(ref))
                            .font(.caption.weight(.medium))
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    if let forwarded = ref.message.forwardAuthor {
                        Text(l10n.t("chat.forward.from", texts: ["name": forwarded]))
                            .font(.caption)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    (Text(FeedCopy.body(ref.message))
                        + Text(ref.message.edited ? " " + l10n.t("chat.message.edited") : "")
                        .foregroundColor(Color.lakkoTextMuted))
                        .font(.body)
                        .foregroundStyle(Color.lakkoText)
                        .multilineTextAlignment(.leading)
                        .lineLimit(8)
                    if let why = FeedCopy.whyLine(ref, preset: model.whyPreset) {
                        Text(why)
                            .font(.caption)
                            .foregroundStyle(Color.lakkoAccent)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)

            let reactions = FeedModel.reactionSummary(ref.message)
            if !reactions.isEmpty {
                HStack(spacing: 6) {
                    ForEach(reactions, id: \.emoji) { r in
                        Text("\(r.emoji) \(r.count)")
                            .font(.caption)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 3)
                            .background(Capsule().fill(Color.lakkoSurfaceSunken))
                    }
                }
                .accessibilityElement(children: .combine)
                .accessibilityLabel(l10n.t("feed.item.reactions_a11y"))
            }
            if !item.replies.isEmpty {
                replies(item, viewportHeight: viewportHeight)
            }
        }
        .padding(12)
        .background(RoundedRectangle(cornerRadius: LakkoRadius.card).fill(Color.lakkoSurface))
        .overlay(RoundedRectangle(cornerRadius: LakkoRadius.card).strokeBorder(Color.lakkoBorder))
        .contentShape(.contextMenuPreview, RoundedRectangle(cornerRadius: LakkoRadius.card))
        // One menu for the whole card, the expanded replies included: a long
        // press on a reply opens its parent's menu.
        .contextMenu { menu(ref) }
        .modifier(SeenWatcher(
            enabled: model.wantsSeen(ref),
            viewportHeight: viewportHeight,
            onSeen: { model.markSeen(ref) }))
    }

    @ViewBuilder
    private func replies(_ item: RankedFeedItem, viewportHeight: CGFloat) -> some View {
        let isOpen = expanded.contains(item.key)
        Button {
            if isOpen { expanded.remove(item.key) } else { expanded.insert(item.key) }
        } label: {
            VStack(alignment: .leading, spacing: 2) {
                Text(FeedCopy.repliesLabel(item.replies.count))
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Color.lakkoAccent)
                if !isOpen, let last = item.replies.last {
                    Text(l10n.t("chats.preview.sender", texts: ["name": model.author(last), "text": FeedCopy.body(last.message)]))
                        .font(.caption)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .lineLimit(1)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .buttonStyle(.plain)
        .accessibilityValue(l10n.t(isOpen ? "common.expanded" : "common.collapsed"))
        if isOpen {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(item.replies, id: \.key) { reply in
                    Button {
                        open(reply)
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(model.author(reply))
                                .font(.caption.weight(.medium))
                                .foregroundStyle(Color.lakkoTextMuted)
                            Text(FeedCopy.body(reply.message))
                                .font(.subheadline)
                                .foregroundStyle(Color.lakkoText)
                                .multilineTextAlignment(.leading)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .buttonStyle(.plain)
                    .modifier(SeenWatcher(
                        enabled: model.wantsSeen(reply),
                        viewportHeight: viewportHeight,
                        onSeen: { model.markSeen(reply) }))
                }
            }
            .padding(.leading, 12)
        }
    }
}

/// Starts the two-second dwell when the view is fully on screen (or fills
/// it), cancels it when the view leaves, and reports the dwell only while
/// the app is active.
private struct SeenWatcher: ViewModifier {
    let enabled: Bool
    let viewportHeight: CGFloat
    let onSeen: () -> Void

    @State private var dwell: Task<Void, Never>?

    func body(content: Content) -> some View {
        if enabled {
            content
                .background(
                    GeometryReader { geo in
                        let frame = geo.frame(in: .named(FeedScreen.scrollSpace))
                        Color.clear
                            .onAppear { update(frame) }
                            .onChange(of: frame) { _, next in update(next) }
                    })
                .onDisappear {
                    dwell?.cancel()
                    dwell = nil
                }
        } else {
            content
        }
    }

    /// Fully on screen, or filling most of the screen when taller than it.
    private func fullyShown(_ frame: CGRect) -> Bool {
        let visibleTop = max(frame.minY, 0)
        let visibleBottom = min(frame.maxY, viewportHeight)
        let visible = visibleBottom - visibleTop
        if frame.minY >= 0 && frame.maxY <= viewportHeight { return true }
        return viewportHeight > 0 && visible >= viewportHeight * 0.9
    }

    private func update(_ frame: CGRect) {
        if fullyShown(frame) {
            guard dwell == nil else { return }
            dwell = Task { @MainActor in
                try? await Task.sleep(nanoseconds: UInt64(FeedSeen.dwellSeconds * 1_000_000_000))
                guard !Task.isCancelled else { return }
                if UIApplication.shared.applicationState == .active { onSeen() }
            }
        } else {
            dwell?.cancel()
            dwell = nil
        }
    }
}

/// The suggestion card a mixing feed shows after one item, when there is one.
private struct MixedSuggestion: View {
    @ObservedObject var discover: DiscoverModel
    let itemIndex: Int
    let itemCount: Int
    let rate: SuggestionRate

    var body: some View {
        let after = DiscoverCards.cardAfter(itemCount: itemCount, cardCount: discover.cards.count, rate: rate)
        if let card = after[itemIndex], discover.cards.indices.contains(card) {
            DiscoverCardView(card: discover.cards[card], model: discover)
        }
    }
}


/// The feed menu's "Hidden from feeds" list (UX spec section 5): the
/// conversations this device still holds that are hidden, each with Show
/// again.
private struct HiddenFromFeedsSheet: View {
    @ObservedObject var model: FeedViewModel
    /// Observed so Show again updates the list at once.
    @ObservedObject var reading: FeedReadingModel
    @Environment(\.dismiss) private var dismiss
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        NavigationStack {
            Group {
                let hidden = FeedReading.hiddenSources(model.allSources, hidden: reading.reading.hidden)
                if hidden.isEmpty {
                    Text(l10n.t("feed.hidden.empty"))
                        .foregroundStyle(Color.lakkoTextMuted)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    List(hidden, id: \.idHex) { source in
                        HStack {
                            Text(source.title)
                                .lineLimit(1)
                            Spacer()
                            Button(l10n.t("feed.hidden.show_again")) { model.showAgain(source.key) }
                                .buttonStyle(.borderless)
                        }
                    }
                    .listStyle(.plain)
                }
            }
            .navigationTitle(l10n.t("feed.hidden.title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(l10n.t("common.close")) { dismiss() }
                }
            }
        }
        .presentationDetents([.medium, .large])
    }
}
