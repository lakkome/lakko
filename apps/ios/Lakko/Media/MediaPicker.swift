// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// ADR-0047 §3-§4 / UX spec §4-§5 — the one picker with two tabs, Emoji and
/// GIF, presented as a bottom sheet.
///
///   * **Compose** mode (the composer's smiley button): Emoji inserts at the
///     caret and keeps the sheet open; GIF sends immediately.
///   * **Reaction** mode (the reaction bar's `+`): Emoji only, a tap reacts and
///     closes. Only a single emoji grapheme can be produced.
struct MediaPickerSheet: View {

    enum Mode: Equatable {
        case compose
        case reaction
    }

    enum Tab: Hashable {
        case emoji
        case gif
    }

    let mode: Mode
    @ObservedObject var media: ChatMediaController
    let onEmoji: (String) -> Void
    let onClose: () -> Void

    @ObservedObject private var gifAvailability = GifAvailability.shared
    @State private var tab: Tab = .emoji

    private var showGifTab: Bool { mode == .compose && !gifAvailability.providerMissing }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                if showGifTab {
                    Picker(Localization.text("chat.composer.emoji_gifs"), selection: $tab) {
                        Text(Localization.text("chat.emoji.tab")).tag(Tab.emoji)
                        Text(Localization.text("chat.media.gif")).tag(Tab.gif)
                    }
                    .pickerStyle(.segmented)
                    .padding(.horizontal, 16)
                    .padding(.bottom, 8)
                }
                if tab == .gif, showGifTab {
                    GifTab(media: media, onSent: onClose)
                } else {
                    EmojiTab(onPick: onEmoji)
                }
            }
            .navigationTitle(mode == .reaction ? Localization.text("chat.emoji.choose_reaction") : "")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button(Localization.text("common.done"), action: onClose)
                }
            }
        }
        .onChange(of: gifAvailability.providerMissing) { _, missing in
            if missing { tab = .emoji }
        }
    }
}

/// Whether this server's GIF proxy answered "not configured" — session only,
/// so a server that gains a key shows the tab again after a relaunch.
@MainActor
final class GifAvailability: ObservableObject {
    static let shared = GifAvailability()
    @Published var providerMissing = false
}

// MARK: - Emoji

struct EmojiTab: View {
    let onPick: (String) -> Void

    private let dataset = EmojiDataset.bundled
    @State private var query = ""
    @State private var recent: [String] = EmojiIndex.loadRecent()
    @State private var tones: [String: String] = EmojiIndex.loadTonePrefs()

    private static let groupIcons = [
        "face.smiling", "hand.raised", "leaf", "fork.knife", "car",
        "soccerball", "lightbulb", "number", "flag",
    ]

    private let columns = [GridItem(.adaptive(minimum: 42), spacing: 4)]

    var body: some View {
        VStack(spacing: 8) {
            TextField(Localization.text("chat.emoji.search"), text: $query)
                .textFieldStyle(.roundedBorder)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .padding(.horizontal, 16)
            ScrollViewReader { proxy in
                if query.trimmingCharacters(in: .whitespaces).isEmpty {
                    categoryStrip(proxy)
                }
                ScrollView {
                    if !query.trimmingCharacters(in: .whitespaces).isEmpty {
                        let results = EmojiIndex.search(dataset, query: query)
                        if results.isEmpty {
                            Text(Localization.text("chat.emoji.no_results"))
                                .foregroundStyle(.secondary)
                                .padding(.top, 24)
                        } else {
                            LazyVGrid(columns: columns, spacing: 4) {
                                ForEach(results, id: \.emoji) { entry in cell(entry) }
                            }
                            .padding(.horizontal, 12)
                        }
                    } else {
                        LazyVGrid(columns: columns, spacing: 4, pinnedViews: []) {
                            if !recent.isEmpty {
                                Section(header: header(Localization.text("chat.emoji.recent")).id("recent")) {
                                    ForEach(recent, id: \.self) { emoji in
                                        recentCell(emoji)
                                    }
                                }
                            }
                            ForEach(Array(dataset.groups.enumerated()), id: \.offset) { index, name in
                                Section(header: header(Self.groupTitle(name)).id("group-\(index)")) {
                                    ForEach(dataset.entries.filter { $0.group == index }, id: \.emoji) { entry in
                                        cell(entry)
                                    }
                                }
                            }
                        }
                        .padding(.horizontal, 12)
                    }
                }
            }
        }
    }

    /// The bundled Unicode data names its groups in English; each known group
    /// has a catalog message (ADR-0061, batch 3), an unknown one shows as-is.
    static func groupTitle(_ name: String) -> String {
        let keys = [
            "Smileys & Emotion": "chat.emoji.group.smileys",
            "People & Body": "chat.emoji.group.people",
            "Animals & Nature": "chat.emoji.group.animals",
            "Food & Drink": "chat.emoji.group.food",
            "Travel & Places": "chat.emoji.group.travel",
            "Activities": "chat.emoji.group.activities",
            "Objects": "chat.emoji.group.objects",
            "Symbols": "chat.emoji.group.symbols",
            "Flags": "chat.emoji.group.flags",
        ]
        return keys[name].map { Localization.text($0) } ?? name
    }

    private func header(_ title: String) -> some View {
        Text(title)
            .font(.caption.bold())
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.top, 8)
    }

    private func categoryStrip(_ proxy: ScrollViewProxy) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 14) {
                ForEach(Array(dataset.groups.enumerated()), id: \.offset) { index, name in
                    Button {
                        withAnimation { proxy.scrollTo("group-\(index)", anchor: .top) }
                    } label: {
                        Image(systemName: index < Self.groupIcons.count ? Self.groupIcons[index] : "circle")
                    }
                    .accessibilityLabel(Self.groupTitle(name))
                }
            }
            .padding(.horizontal, 16)
        }
        .foregroundStyle(.secondary)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Localization.text("chat.emoji.categories"))
    }

    private func pick(_ emoji: String) {
        guard EmojiIndex.isSingleEmojiGrapheme(emoji) else { return }
        recent = EmojiIndex.pushRecent(recent, emoji)
        EmojiIndex.saveRecent(recent)
        onPick(emoji)
    }

    private func cell(_ entry: EmojiEntry) -> some View {
        let shown = EmojiIndex.displayEmoji(entry, prefs: tones)
        return Button { pick(shown) } label: {
            Text(shown).font(.system(size: 30))
                .frame(width: 42, height: 42)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(entry.name)
        .contextMenu {
            // UX spec §4 — long-press shows the tone variants; the chosen one
            // becomes this emoji's default on this device.
            if let tones = entry.tones {
                ForEach([entry.emoji] + tones, id: \.self) { variant in
                    Button(variant) {
                        self.tones = EmojiIndex.withTonePref(self.tones, entry: entry, variant: variant)
                        EmojiIndex.saveTonePrefs(self.tones)
                        pick(variant)
                    }
                }
            }
        }
    }

    private func recentCell(_ emoji: String) -> some View {
        Button { pick(emoji) } label: {
            Text(emoji).font(.system(size: 30))
                .frame(width: 42, height: 42)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(dataset.byEmoji[emoji]?.name ?? emoji)
    }
}

// MARK: - GIF

@MainActor
final class GifSearchModel: ObservableObject {
    enum Status: Equatable {
        case idle
        case loadingFirst
        case loaded
        case noResults
        case unavailable
        case rateLimited
    }

    @Published var query = "" {
        didSet {
            guard query != oldValue else { return }
            debouncer.schedule { [weak self] in self?.reload() }
        }
    }
    @Published private(set) var items: [GifItem] = []
    @Published private(set) var status: Status = .idle
    @Published private(set) var loadingMore = false

    private let client: GifClient
    private let debouncer = Debouncer(delayMs: GifClient.searchDebounceMs)
    private var nextOffset: Int?
    private var generation = 0

    init(client: GifClient) {
        self.client = client
    }

    func start() {
        if status == .idle { reload() }
    }

    func submit() { debouncer.flush() }

    func cancel() { debouncer.cancel() }

    func reload() {
        generation += 1
        let mine = generation
        let q = query
        items = []
        nextOffset = nil
        status = .loadingFirst
        Task {
            let result = await client.fetchPage(query: q, offset: 0)
            guard mine == generation else { return }
            apply(result, append: false)
        }
    }

    func loadMoreIfNeeded(current item: GifItem) {
        guard !loadingMore, status == .loaded, let offset = nextOffset, item.id == items.last?.id else { return }
        loadingMore = true
        let mine = generation
        let q = query
        Task {
            let result = await client.fetchPage(query: q, offset: offset)
            loadingMore = false
            guard mine == generation else { return }
            apply(result, append: true)
        }
    }

    private func apply(_ result: GifResult<GifPage>, append: Bool) {
        switch result {
        case .ok(let page):
            let known = Set(items.map(\.id))
            items += page.items.filter { !known.contains($0.id) }
            nextOffset = page.nextOffset
            status = items.isEmpty ? .noResults : .loaded
        case .failure(.notConfigured):
            GifAvailability.shared.providerMissing = true
            status = .unavailable
        case .failure(.rateLimited):
            if !append { status = .rateLimited }
        case .failure:
            if !append { status = .unavailable }
        }
    }
}

struct GifTab: View {
    @ObservedObject var media: ChatMediaController
    let onSent: () -> Void

    @StateObject private var model: GifSearchModel
    @AppStorage(MediaPrefs.gifNoteKey) private var noteDismissed = false

    init(media: ChatMediaController, onSent: @escaping () -> Void) {
        self.media = media
        self.onSent = onSent
        _model = StateObject(wrappedValue: GifSearchModel(client: media.gifClient))
    }

    private let columns = [GridItem(.flexible(), spacing: 6), GridItem(.flexible(), spacing: 6)]

    var body: some View {
        VStack(spacing: 8) {
            if !noteDismissed {
                // UX spec §5 — what the server learns, stated before searching.
                HStack(alignment: .top, spacing: 8) {
                    Text(GifCopy.privacyNote).font(.footnote)
                    Spacer(minLength: 0)
                    Button(Localization.text("chat.gif.note_dismiss")) { noteDismissed = true }
                        .font(.footnote.bold())
                }
                .padding(10)
                .background(Color(.secondarySystemBackground))
                .clipShape(RoundedRectangle(cornerRadius: 10))
                .padding(.horizontal, 16)
            }
            TextField(Localization.text("chat.gif.search"), text: $model.query)
                .textFieldStyle(.roundedBorder)
                .submitLabel(.search)
                .onSubmit { model.submit() }
                .autocorrectionDisabled()
                .padding(.horizontal, 16)
            ScrollView {
                content
                    .padding(.horizontal, 12)
            }
            Text(GifCopy.attribution)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .padding(.bottom, 6)
        }
        .onAppear { model.start() }
        .onDisappear { model.cancel() }
    }

    @ViewBuilder
    private var content: some View {
        switch model.status {
        case .idle, .loadingFirst:
            LazyVGrid(columns: columns, spacing: 6) {
                ForEach(0..<8, id: \.self) { _ in
                    RoundedRectangle(cornerRadius: 8)
                        .fill(Color(.tertiarySystemFill))
                        .frame(height: 120)
                }
            }
            .accessibilityLabel(Localization.text("chat.gif.loading"))
        case .noResults:
            Text(GifCopy.noResults).foregroundStyle(.secondary).padding(.top, 24)
        case .unavailable:
            VStack(spacing: 8) {
                Text(GifCopy.unavailable).foregroundStyle(.secondary)
                Button(Localization.text("common.retry")) { model.reload() }
            }
            .padding(.top, 24)
        case .rateLimited:
            VStack(spacing: 8) {
                Text(GifCopy.rateLimited).foregroundStyle(.secondary)
                Button(Localization.text("common.retry")) { model.reload() }
            }
            .padding(.top, 24)
        case .loaded:
            LazyVGrid(columns: columns, spacing: 6) {
                ForEach(model.items) { item in
                    GifPreviewTile(item: item, client: media.gifClient)
                        .onTapGesture {
                            media.sendGif(item)
                            onSent()
                        }
                        .onAppear { model.loadMoreIfNeeded(current: item) }
                        .accessibilityAddTraits(.isButton)
                        .accessibilityLabel(Localization.text("chat.gif.send"))
                }
            }
            if model.loadingMore { ProgressView().padding() }
        }
    }
}

/// One looping, muted preview, fetched with the session token through the
/// proxy (the proxy route needs `Authorization`, which an image loader cannot
/// send) and decoded with ImageIO so it animates.
struct GifPreviewTile: View {
    let item: GifItem
    let client: GifClient

    @State private var media: GifMedia?
    @State private var failed = false

    private var height: CGFloat {
        let aspect = Double(item.height) / Double(max(1, item.width))
        return CGFloat(min(220, max(80, 170 * aspect)))
    }

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 8).fill(Color(.tertiarySystemFill))
            if let media, media.mime != "video/mp4" {
                AnimatedImageView(source: .data(media.bytes))
                    .clipShape(RoundedRectangle(cornerRadius: 8))
            } else if failed {
                Image(systemName: "photo").foregroundStyle(.secondary)
            }
        }
        .frame(height: height)
        .contentShape(Rectangle())
        .task(id: item.id) {
            switch await client.fetchMedia(item.previewURL, maxBytes: GifClient.maxPreviewBytes) {
            case .ok(let fetched): media = fetched
            case .failure(.notConfigured):
                GifAvailability.shared.providerMissing = true
                failed = true
            case .failure: failed = true
            }
        }
    }
}
