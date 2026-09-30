// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// Discover (`docs/ux/feed.md` section 6, ADR-0059): the Discover chip of the
/// Feed. Mirror of web `discover/Discover.tsx` `DiscoverView`: a search field
/// with the line that says searches are sent, the pick-topics card on first
/// open (and from "Edit topics"), the Discover settings menu with "Mix in
/// random topics", and the ranked channel cards.
struct DiscoverView: View {
    @ObservedObject var model: DiscoverModel

    @State private var query = ""
    @State private var editing = false

    private var showIntro: Bool {
        model.loaded && (editing || (model.prefs.topics.isEmpty && !model.prefs.introDismissed))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            searchField
            Text(model.unavailable ? DiscoverCopy.unavailable : DiscoverCopy.searchHelper)
                .font(.caption)
                .foregroundStyle(Color.lakkoTextMuted)
            if let notice = model.notice {
                Text(notice)
                    .font(.subheadline)
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(RoundedRectangle(cornerRadius: LakkoRadius.card).fill(Color.lakkoSurfaceSunken))
                    .accessibilityAddTraits(.updatesFrequently)
            }
            if model.search == .idle {
                suggestions
            } else {
                searchResults
            }
        }
        .task {
            model.load()
            await model.refresh()
        }
    }

    private var searchField: some View {
        HStack(spacing: 8) {
            TextField("Search channels", text: $query)
                .textFieldStyle(.roundedBorder)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .submitLabel(.search)
                .disabled(model.unavailable)
                .onSubmit { Task { await model.runSearch(query) } }
                .onChange(of: query) { _, value in
                    if value.count > 64 { query = String(value.prefix(64)) }
                    if value.isEmpty { model.clearSearch() }
                }
                .accessibilityLabel("Search channels")
            Button("Search") { Task { await model.runSearch(query) } }
                .disabled(model.unavailable || model.search == .busy)
        }
    }

    @ViewBuilder
    private var searchResults: some View {
        switch model.search {
        case .idle:
            EmptyView()
        case .busy:
            Text("Searching...")
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
        case .error(let message):
            Text(message)
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
        case .done(_, let cards):
            if cards.isEmpty {
                emptyLine(DiscoverCopy.noResults)
            } else {
                ForEach(cards) { DiscoverCardView(card: $0, model: model) }
            }
        }
    }

    @ViewBuilder
    private var suggestions: some View {
        if showIntro {
            PickTopicsCard(model: model, first: !editing) { editing = false }
        } else if model.loaded {
            settingsMenu
        }
        if model.refreshing && model.cards.isEmpty {
            ForEach(0..<2, id: \.self) { _ in
                RoundedRectangle(cornerRadius: LakkoRadius.card)
                    .fill(Color.lakkoSurfaceSunken)
                    .frame(height: 120)
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Loading")
        } else if model.cards.isEmpty {
            if !showIntro {
                emptyLine(model.unavailable ? DiscoverCopy.unavailable : DiscoverCopy.nothingYet)
            }
        } else {
            ForEach(model.cards) { DiscoverCardView(card: $0, model: model) }
        }
    }

    private var settingsMenu: some View {
        DisclosureGroup("Discover settings") {
            VStack(alignment: .leading, spacing: 8) {
                Button("Edit topics") { editing = true }
                Toggle(DiscoverCopy.mixLabel, isOn: Binding(
                    get: { model.prefs.mixRandom },
                    set: { model.setMixRandom($0) }))
                Text(DiscoverCopy.mixOffLine)
                    .font(.caption)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            .padding(.top, 8)
        }
        .font(.subheadline)
    }

    private func emptyLine(_ text: String) -> some View {
        Text(text)
            .multilineTextAlignment(.center)
            .foregroundStyle(Color.lakkoTextMuted)
            .frame(maxWidth: .infinity)
            .padding(.top, 32)
    }
}

/// One suggestion card. Also drawn where a saved feed mixes suggestions in.
struct DiscoverCardView: View {
    let card: DirectoryCard
    @ObservedObject var model: DiscoverModel

    @State private var busy = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                Text(String(card.name.trimmingCharacters(in: .whitespaces).prefix(1)).uppercased())
                    .font(.headline)
                    .frame(width: 40, height: 40)
                    .background(Circle().fill(Color.lakkoSurfaceSunken))
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(card.name)
                        .font(.subheadline.weight(.semibold))
                        .lineLimit(1)
                    // The handle is the owner's (or a member's) claim until the
                    // lookup before subscribing checks it: plain text, never a
                    // link (ADR-0059 section 3).
                    Text(verbatim: "@\(card.handle)")
                        .font(.caption)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .lineLimit(1)
                }
            }
            if let description = card.description {
                if !description.isEmpty {
                    Text(description)
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoText)
                        .lineLimit(2)
                }
            } else {
                Button("Show description") { Task { await model.openCard(card) } }
                    .font(.subheadline)
            }
            facts
            Text(DiscoverCards.reasonLine(card, topicName: model.topicName))
                .font(.caption)
                .foregroundStyle(Color.lakkoAccent)
            HStack(spacing: 12) {
                Button {
                    busy = true
                    Task {
                        await model.subscribe(card)
                        busy = false
                    }
                } label: {
                    Text("Subscribe")
                }
                .buttonStyle(.borderedProminent)
                .tint(Color.lakkoAccent)
                .disabled(busy)
                Button("Not interested") { model.notInterested(card.channelIdHex) }
                if let topic = card.topic {
                    Button("Hide this topic") { model.hideTopic(topic) }
                }
            }
            .font(.subheadline)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: LakkoRadius.card).fill(Color.lakkoSurface))
        .overlay(RoundedRectangle(cornerRadius: LakkoRadius.card).strokeBorder(Color.lakkoBorder))
        .contextMenu {
            Button("Not interested") { model.notInterested(card.channelIdHex) }
            if let topic = card.topic {
                Button("Hide this topic") { model.hideTopic(topic) }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Suggested channel \(card.name)")
    }

    private var facts: some View {
        HStack(spacing: 8) {
            if let topic = card.topic {
                Text(model.topicName(topic))
            }
            if let language = card.language, !language.isEmpty {
                Text(DiscoverLanguages.label(language))
            }
            if let band = card.band {
                Text("\(band) subscribers")
            }
        }
        .font(.caption)
        .foregroundStyle(Color.lakkoTextMuted)
    }
}

/// The pick-topics card (first open, and "Edit topics").
private struct PickTopicsCard: View {
    @ObservedObject var model: DiscoverModel
    let first: Bool
    let onDone: () -> Void

    @State private var chosen: Set<Int> = []
    @State private var language = ""
    @State private var seeded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(DiscoverCopy.pickTitle).font(.headline)
            Text(DiscoverCopy.pickBody)
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
                .fixedSize(horizontal: false, vertical: true)
            FlowChips(items: model.topics.map { (Int($0.id), $0.name) }, chosen: $chosen)
            Picker("Language", selection: $language) {
                ForEach(DiscoverLanguages.options(language), id: \.self) { code in
                    Text(DiscoverLanguages.label(code)).tag(code)
                }
            }
            HStack {
                Spacer()
                Button(first ? "Not now" : "Cancel") {
                    if first { model.dismissIntro() }
                    onDone()
                }
                Button("Save") {
                    let picks = chosen.sorted()
                    let lang = language
                    onDone()
                    Task { await model.savePicks(picks, language: lang) }
                }
                .buttonStyle(.borderedProminent)
                .tint(Color.lakkoAccent)
            }
        }
        .padding(12)
        .background(RoundedRectangle(cornerRadius: LakkoRadius.card).fill(Color.lakkoSurface))
        .overlay(RoundedRectangle(cornerRadius: LakkoRadius.card).strokeBorder(Color.lakkoBorder))
        .onAppear {
            guard !seeded else { return }
            seeded = true
            chosen = Set(model.prefs.topics)
            language = model.language
        }
        .accessibilityLabel("Pick topics")
    }
}

/// Topic chips that wrap onto several lines.
private struct FlowChips: View {
    let items: [(Int, String)]
    @Binding var chosen: Set<Int>

    var body: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 110), spacing: 8)], alignment: .leading, spacing: 8) {
            ForEach(items, id: \.0) { item in
                let selected = chosen.contains(item.0)
                Button {
                    if selected { chosen.remove(item.0) } else { chosen.insert(item.0) }
                } label: {
                    Text(item.1)
                        .font(.subheadline.weight(selected ? .semibold : .regular))
                        .lineLimit(1)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 6)
                        .background(Capsule().fill(selected ? Color.lakkoAccent : Color.lakkoSurfaceSunken))
                        .foregroundStyle(selected ? Color.lakkoAccentForeground : Color.lakkoText)
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(selected ? [.isSelected] : [])
            }
        }
    }
}
