// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI
import UIKit

/// The Feed destination (`docs/ux/feed.md`, ADR-0058). Mirror of web
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
/// Not yet here (as on web): the "New" divider, unpinning a preset, feed
/// icons, and the item menu.
struct FeedScreen: View {
    @ObservedObject var model: FeedViewModel
    /// Open a group at a message.
    let onOpenGroup: (_ groupIdHex: String, _ messageId: Int64) -> Void
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

    var body: some View {
        VStack(spacing: 0) {
            switcher
            if let saved = model.selectedSaved {
                savedActions(saved)
            }
            list
        }
        .background(Color.lakkoBackground)
        .navigationTitle("Feed")
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button("New feed") { beginNew() }
                    Button("Add a shared feed") {
                        model.importing = FeedImportRequest(bytes: nil, refused: false)
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .accessibilityLabel("Feed menu")
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
        .onAppear {
            model.start()
            model.discover.onOpen = { card in onOpenFoundChannel(card.channelIdHex, card.handle) }
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
                    chip(key.label, selected: model.selection == .preset(key)) {
                        model.choose(.preset(key))
                    }
                }
                chip("Discover", selected: model.selection == .discover) {
                    model.choose(.discover)
                }
                ForEach(model.saved) { saved in
                    chip(saved.rule.name, selected: model.selection == .saved(saved.id)) {
                        model.choose(.saved(saved.id))
                    }
                }
                Button {
                    beginNew()
                } label: {
                    Label("New feed", systemImage: "plus")
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
        .accessibilityLabel("Feeds")
    }

    private func chip(_ label: String, selected: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(label)
                .font(.subheadline.weight(selected ? .semibold : .regular))
                .lineLimit(1)
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
            Button("Edit") {
                model.editError = nil
                model.editing = .edit(saved.id)
            }
            Button("Share") { model.sharing = saved }
            Spacer()
        }
        .font(.subheadline)
        .padding(.horizontal, 16)
        .padding(.bottom, 6)
    }

    // MARK: - List

    private var list: some View {
        GeometryReader { outer in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    topSentinel
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
            .refreshable { await model.refresh() }
            .overlay(alignment: .top) {
                if model.selection != .discover && model.newCount > 0 {
                    Button {
                        model.showPending()
                    } label: {
                        Text("Show \(model.newCount) new")
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
            Text("Your feed shows messages this device has received.")
                .font(.subheadline)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button("Got it") { model.dismissBanner() }
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
                ForEach(Array(items.enumerated()), id: \.element.key) { index, item in
                    itemView(item, viewportHeight: viewportHeight)
                    // A saved feed that asks for suggestions shows a channel
                    // card after some items (ADR-0059, UX spec 4.1 section 4).
                    if model.suggestionRate != .never {
                        MixedSuggestion(
                            discover: model.discover, itemIndex: index, itemCount: items.count,
                            rate: model.suggestionRate)
                    }
                }
                Text("You're up to date.")
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
            .accessibilityLabel("Loading")
        }
    }

    @ViewBuilder
    private var emptyState: some View {
        VStack(spacing: 12) {
            if model.sourceCount == 0 {
                Text("Your feed fills up from your groups and channels.")
                    .multilineTextAlignment(.center)
                Button("Discover channels") { model.choose(.discover) }
                    .buttonStyle(.borderedProminent)
            } else if model.candidateCount == 0 {
                Text("Nothing here yet. New messages from your groups and channels will show up here.")
                    .multilineTextAlignment(.center)
            } else {
                Text("No messages match this feed.")
                if let saved = model.selectedSaved {
                    Button("Edit feed") { model.editing = .edit(saved.id) }
                        .buttonStyle(.borderedProminent)
                }
            }
        }
        .foregroundStyle(Color.lakkoTextMuted)
        .frame(maxWidth: .infinity)
        .padding(.top, 48)
    }

    // MARK: - Item

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
                        Text("Forwarded from \(forwarded)")
                            .font(.caption)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    (Text(FeedCopy.body(ref.message))
                        + Text(ref.message.edited ? " (edited)" : "")
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
                .accessibilityLabel("Reactions")
            }
            if !item.replies.isEmpty {
                replies(item, viewportHeight: viewportHeight)
            }
        }
        .padding(12)
        .background(RoundedRectangle(cornerRadius: LakkoRadius.card).fill(Color.lakkoSurface))
        .overlay(RoundedRectangle(cornerRadius: LakkoRadius.card).strokeBorder(Color.lakkoBorder))
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
                    Text("\(model.author(last)): \(FeedCopy.body(last.message))")
                        .font(.caption)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .lineLimit(1)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .buttonStyle(.plain)
        .accessibilityValue(isOpen ? "Expanded" : "Collapsed")
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
