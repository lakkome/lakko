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
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    private var showIntro: Bool {
        model.loaded && (editing || (model.prefs.topics.isEmpty && !model.prefs.introDismissed))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            searchField
            if model.unavailable {
                Text(DiscoverCopy.unavailable)
                    .font(.caption)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
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
            TextField(l10n.t("discover.search.placeholder"), text: $query)
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
                .accessibilityLabel(l10n.t("discover.search.placeholder"))
            Button(l10n.t("discover.search.action")) { Task { await model.runSearch(query) } }
                .disabled(model.unavailable || model.search == .busy)
        }
    }

    @ViewBuilder
    private var searchResults: some View {
        switch model.search {
        case .idle:
            EmptyView()
        case .busy:
            Text(l10n.t("discover.search.searching"))
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
            .accessibilityLabel(l10n.t("common.loading"))
        } else if model.cards.isEmpty {
            if !showIntro {
                emptyLine(model.unavailable ? DiscoverCopy.unavailable : DiscoverCopy.nothingYet)
            }
        } else {
            ForEach(model.cards) { DiscoverCardView(card: $0, model: model) }
        }
    }

    private var settingsMenu: some View {
        DisclosureGroup(l10n.t("discover.settings")) {
            VStack(alignment: .leading, spacing: 8) {
                Button(l10n.t("discover.edit_topics")) { editing = true }
                Toggle(DiscoverCopy.mixLabel, isOn: Binding(
                    get: { model.prefs.mixRandom },
                    set: { model.setMixRandom($0) }))
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
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared
    /// ADR-0099 / UX 2.1 — Report from the card's menu, its listing included.
    @State private var reportRequest: ReportRequest?

    private var cardReport: ReportRequest? {
        guard let id = ChannelsClient.hexToData(card.channelIdHex), id.count == 16 else { return nil }
        return ReportRequest(
            target: .channel(channelId: id), targetName: card.name, conversationKind: .discover,
            fixedEvidence: card.listingWire.map { [FfiEvidenceItem.listing(wire: $0)] } ?? [])
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                Text(avatarInitials(name: card.name))
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
                Button(l10n.t("discover.card.show_description")) { Task { await model.openCard(card) } }
                    .font(.subheadline)
            }
            facts
            // A channel this device already holds says so and only opens
            // (UX spec section 6.2): no Subscribe, no Not interested.
            let relation = model.relation(card.channelIdHex)
            Text(DiscoverCards.relationLine(relation)
                ?? DiscoverCards.reasonLine(card, topicName: model.topicName))
                .font(.caption)
                .foregroundStyle(Color.lakkoAccent)
            if relation != .none {
                Button {
                    model.onOpenChannel?(card.channelIdHex)
                } label: {
                    Text(l10n.t("common.open"))
                }
                .buttonStyle(.borderedProminent)
                .tint(Color.lakkoAccent)
                .font(.subheadline)
            } else {
                HStack(spacing: 12) {
                    Button {
                        busy = true
                        Task {
                            await model.subscribe(card)
                            busy = false
                        }
                    } label: {
                        Text(l10n.t("channels.subscribe.action"))
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Color.lakkoAccent)
                    .disabled(busy)
                    Button(l10n.t("discover.card.not_interested")) { model.notInterested(card.channelIdHex) }
                    if let topic = card.topic {
                        Button(l10n.t("discover.card.hide_topic")) { model.hideTopic(topic) }
                    }
                }
                .font(.subheadline)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: LakkoRadius.card).fill(Color.lakkoSurface))
        .overlay(RoundedRectangle(cornerRadius: LakkoRadius.card).strokeBorder(Color.lakkoBorder))
        // Lakko's own long-press menu, never the system context menu.
        .lakkoLongPressMenu {
            var items: [LakkoMenuItem] = []
            if model.relation(card.channelIdHex) == .none {
                items.append(.action(l10n.t("discover.card.not_interested")) { model.notInterested(card.channelIdHex) })
                if let topic = card.topic {
                    items.append(.action(l10n.t("discover.card.hide_topic")) { model.hideTopic(topic) })
                }
            }
            // ADR-0099 / UX 2.1 — never on a channel this account owns.
            if model.relation(card.channelIdHex) != .own, let report = cardReport {
                items.append(.action(l10n.t("report.action"), icon: .flag, destructive: true) {
                    reportRequest = report
                })
            }
            return items
        }
        .reportSheet($reportRequest)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(l10n.t("discover.card.a11y", texts: ["name": card.name]))
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
                Text(l10n.t("discover.card.subscribers", texts: ["band": band]))
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
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(DiscoverCopy.pickTitle).font(.headline)
            Text(DiscoverCopy.pickBody)
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
                .fixedSize(horizontal: false, vertical: true)
            FlowChips(items: model.topics.map { (Int($0.id), DiscoverTopics.name(Int($0.id))) }, chosen: $chosen)
            LakkoPicker(
                l10n.t("discover.language"), selection: $language,
                options: DiscoverLanguages.options(language).map {
                    LakkoPickerOption(value: $0, label: DiscoverLanguages.label($0))
                })
            HStack {
                Spacer()
                Button(l10n.t(first ? "common.not_now" : "common.cancel")) {
                    if first { model.dismissIntro() }
                    onDone()
                }
                Button(l10n.t("common.save")) {
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
        .accessibilityLabel(l10n.t("discover.pick.a11y"))
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
