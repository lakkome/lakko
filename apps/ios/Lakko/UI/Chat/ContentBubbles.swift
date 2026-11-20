// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import LakkoCore
import SwiftUI

// ADR-0092 / `docs/ux/attachments.md` — the bubbles of the new kinds: poll
// (section 7.2), checklist (8.2), location (9.3), contact card (10.2),
// article (11.2) and audio (section 6). Each folds through the core and
// draws; the actions go back through ``ChatViewModel``.

/// What a content bubble needs from the conversation.
struct ContentBubbleContext {
    let groupIdHex: String
    let ownAccountKey: Data?
    let nameOf: (Data) -> String?
    let isOwnerOrAdmin: (Data) -> Bool
    let vote: (Int64, [UInt8]) -> Void
    let tick: (Int64, ChecklistItemRef, Bool) -> Void
    let addItem: (Int64, String) -> Void
    let openArticle: (ChatMessage) -> Void
    let openResults: (ChatMessage) -> Void
    let contactCardState: (SharedContactCard) -> ContactCardButton
    let contactCardAction: (SharedContactCard, ChatMessage) -> Void
    /// A channel poll: its view comes from the channel model, not the fold.
    var pollOverride: ((ChatMessage) -> PollView?)? = nil
    var pollSendState: BallotSendState? = nil
    var pollRetry: (() -> Void)? = nil
}

/// The one button (or line) a contact card shows (UX section 10.2).
enum ContactCardButton: Equatable {
    case openChat
    case thisIsYou
    case add
    case working
    case notice(String)
}

/// The bubble of a content message.
struct ContentBubble: View {
    let message: ChatMessage
    let context: ContentBubbleContext
    @ObservedObject private var choices = PollChoiceCenter.shared
    @ObservedObject private var ballots = BallotQueue.shared
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if let stored = message.content, let parsed = ContentModel.parsed(stored), let content = parsed.content {
            switch content {
            case .poll:
                if let override = context.pollOverride {
                    if let view = override(message) {
                        PollCard(
                            view: view, sendState: context.pollSendState, enabled: message.messageId > 0,
                            onVote: { context.vote(message.messageId, $0) },
                            onRetry: { context.pollRetry?() }, onResults: {})
                    }
                } else if let view = PollView.make(
                    message: message, ownAccountKey: context.ownAccountKey,
                    registerChoice: choices.choice(groupIdHex: context.groupIdHex, pollMessageId: message.messageId),
                    nowMs: ChatRepository.nowMs()) {
                    PollCard(
                        view: view,
                        sendState: ballots.state(groupIdHex: context.groupIdHex, pollMessageId: message.messageId),
                        enabled: message.messageId > 0,
                        onVote: { context.vote(message.messageId, $0) },
                        onRetry: { BallotQueue.shared.retry(groupIdHex: context.groupIdHex, pollMessageId: message.messageId) },
                        onResults: { context.openResults(message) })
                }
            case .checklist:
                if let view = ChecklistView.make(message: message) {
                    ChecklistCard(
                        view: view, ownKey: context.ownAccountKey, nameOf: context.nameOf,
                        enabled: message.messageId > 0,
                        onTick: { context.tick(message.messageId, $0, $1) },
                        onAdd: { context.addItem(message.messageId, $0) })
                }
            case .location(let location):
                LocationCard(location: location)
            case .contactCard(let card):
                ContactCardView(
                    name: card.name, username: card.username,
                    action: (context.contactCardState(card), { context.contactCardAction(card, message) }))
            case .article(let article):
                ArticleCard(article: article, attachments: message.attachments) { context.openArticle(message) }
            }
        }
    }
}

// MARK: - Poll card

struct PollCard: View {
    let view: PollView
    let sendState: BallotSendState?
    let enabled: Bool
    let onVote: ([UInt8]) -> Void
    let onRetry: () -> Void
    let onResults: () -> Void

    @ObservedObject private var l10n = Localization.shared
    @State private var ticked: Set<UInt8> = []

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(view.poll.body.question)
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(Color.lakkoText)
            Text(kindLine).font(.footnote).foregroundStyle(Color.lakkoTextMuted)
            if view.unverifiable {
                Text(l10n.t("poll.unverifiable")).font(.footnote).foregroundStyle(Color.lakkoWarning)
            }
            ForEach(Array(view.poll.body.options.enumerated()), id: \.offset) { index, option in
                optionRow(index: index, text: option)
            }
            if !view.showsResults, view.poll.body.multiple, !ticked.isEmpty, canVote {
                Button(l10n.t("poll.vote")) { onVote(ticked.sorted()) }
                    .buttonStyle(.borderedProminent)
                    .tint(Color.lakkoAccent)
            }
            if view.showsResults, let quiz = view.quiz, !quiz.explanation.isEmpty {
                Text(quiz.explanation)
                    .font(.footnote)
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Color.lakkoAccentSoft)
                    .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control))
            }
            footer
        }
        .padding(12)
        .frame(maxWidth: 360, alignment: .leading)
    }

    private var canVote: Bool { enabled && !view.closed && !view.unverifiable && sendState != .sending }

    private var kindLine: String {
        var key: String
        switch (view.anonymous, view.quiz != nil) {
        case (true, true): key = "poll.kind.anonymous_quiz"
        case (true, false): key = "poll.kind.anonymous"
        case (false, true): key = "poll.kind.quiz"
        case (false, false): key = "poll.kind.public"
        }
        var line = l10n.t(key)
        if view.poll.body.multiple { line += " · " + l10n.t("poll.kind.multiple") }
        return line
    }

    @ViewBuilder
    private func optionRow(index: Int, text: String) -> some View {
        let i = UInt8(index)
        let mine = view.ownChoices?.contains(i) ?? false
        if view.showsResults {
            let percent = view.percent(index)
            let correct = view.quiz.map { Int($0.correct) == index } ?? false
            let wrongMine = view.quiz != nil && mine && !correct
            ZStack(alignment: .leading) {
                GeometryReader { geo in
                    RoundedRectangle(cornerRadius: 6)
                        .fill(correct ? Color.lakkoSuccess.opacity(0.25)
                              : wrongMine ? Color.lakkoDanger.opacity(0.25) : Color.lakkoAccentSoft)
                        .frame(width: geo.size.width * CGFloat(percent) / 100)
                }
                HStack {
                    if mine { LkIconView(.checkmark, size: 14, relativeTo: .footnote) }
                    Text(text)
                    Spacer()
                    Text("\(percent)%").font(.footnote.monospacedDigit())
                }
                .padding(.horizontal, 8)
            }
            .frame(minHeight: 36)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(l10n.t(
                "poll.option_result", texts: ["option": text, "percent": "\(percent)"],
                counts: ["count": UInt64(view.counts.indices.contains(index) ? view.counts[index] : 0)])
                + (mine ? ", " + l10n.t("poll.your_choice") : ""))
        } else {
            Button {
                guard canVote else { return }
                if view.poll.body.multiple {
                    if ticked.contains(i) { ticked.remove(i) } else { ticked.insert(i) }
                } else {
                    onVote([i])
                }
            } label: {
                HStack(spacing: 10) {
                    if sendState == .sending, mine {
                        ProgressView().controlSize(.small)
                    } else {
                        LkIconView(view.poll.body.multiple
                                   ? (ticked.contains(i) ? .checkmarkCircleFill : .circle)
                                   : .circle, size: 20)
                            .foregroundStyle(Color.lakkoAccent)
                    }
                    Text(text).foregroundStyle(Color.lakkoText)
                    Spacer()
                }
                .frame(minHeight: 36)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(!canVote)
        }
    }

    @ViewBuilder
    private var footer: some View {
        HStack(spacing: 6) {
            if sendState == .sending {
                Text(l10n.t("poll.sending_vote"))
            } else if sendState == .failed {
                Text(l10n.t("poll.vote_failed")).foregroundStyle(Color.lakkoDanger)
                Button(l10n.t("poll.try_again"), action: onRetry)
            } else {
                Text(l10n.t("poll.votes", counts: ["n": UInt64(view.voters)]))
                if view.closed {
                    Text("·")
                    Text(l10n.t("poll.closed"))
                } else if let closeAt = view.poll.body.closeAtMs {
                    Text("·")
                    Text(l10n.t("poll.closes_in", texts: ["time": PollTime.remaining(untilMs: closeAt)]))
                }
            }
        }
        .font(.footnote)
        .foregroundStyle(Color.lakkoTextMuted)
        if !view.anonymous, view.ownChoices != nil, !view.choicesByVoter.isEmpty {
            Button(l10n.t("poll.view_results"), action: onResults).font(.footnote)
        }
    }
}

enum PollTime {
    /// `{time}` of `Closes in {time}`, platform-formatted.
    static func remaining(untilMs: Int64) -> String {
        let seconds = max(0, Double(untilMs - ChatRepository.nowMs()) / 1000)
        let f = DateComponentsFormatter()
        f.unitsStyle = .full
        f.maximumUnitCount = 1
        f.allowedUnits = [.day, .hour, .minute]
        return f.string(from: max(60, seconds)) ?? ""
    }
}

/// UX section 7.4 — the results of a non-anonymous poll: per option, the
/// people who chose it.
struct PollResultsScreen: View {
    let view: PollView
    let nameOf: (Data) -> String
    let onClose: () -> Void
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        NavigationStack {
            List {
                ForEach(Array(view.poll.body.options.enumerated()), id: \.offset) { index, option in
                    Section {
                        ForEach(voters(for: index), id: \.self) { voter in
                            Text(nameOf(voter))
                        }
                    } header: {
                        HStack {
                            Text(option)
                            Spacer()
                            Text("\(view.counts.indices.contains(index) ? view.counts[index] : 0)")
                        }
                    }
                }
            }
            .navigationTitle(l10n.t("poll.results.title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button(l10n.t("common.done"), action: onClose) }
            }
        }
    }

    private func voters(for index: Int) -> [Data] {
        view.choicesByVoter.filter { $0.choices.contains(UInt8(index)) }.map(\.voter)
    }
}

// MARK: - Checklist card

struct ChecklistCard: View {
    let view: ChecklistView
    let ownKey: Data?
    let nameOf: (Data) -> String?
    let enabled: Bool
    let onTick: (ChecklistItemRef, Bool) -> Void
    let onAdd: (String) -> Void

    @ObservedObject private var l10n = Localization.shared
    @State private var adding = false
    @State private var newItem = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(view.state.title).font(.system(size: 15, weight: .semibold))
            Text(l10n.t("checklist.done_of", texts: [
                "done": "\(view.doneCount)", "total": "\(view.state.items.count)"]))
                .font(.footnote)
                .foregroundStyle(Color.lakkoTextMuted)
            ForEach(Array(view.state.items.enumerated()), id: \.offset) { _, item in
                itemRow(item)
            }
            if enabled, view.mayAdd(ownKey) {
                if adding {
                    TextField(l10n.t("checklist.add_item"), text: $newItem)
                        .onSubmit {
                            let text = ContentText.trimmed(newItem)
                            if !text.isEmpty { onAdd(ContentText.clamp(text, bytes: contentLimits().maxChecklistItemBytes)) }
                            newItem = ""
                            adding = false
                        }
                } else {
                    Button(l10n.t("checklist.add_item")) { adding = true }.font(.footnote)
                }
            }
            if view.closed {
                Text(l10n.t("poll.closed")).font(.footnote).foregroundStyle(Color.lakkoTextMuted)
            }
        }
        .padding(12)
        .frame(maxWidth: 360, alignment: .leading)
    }

    private func itemRow(_ item: ChecklistStateItem) -> some View {
        let done = item.doneBy != nil
        let doneByName = item.doneBy.flatMap { by -> String? in
            // In a one-to-one only the other person is named; the card does
            // not name this device's own account.
            by == ownKey ? nil : nameOf(by).map { $0.components(separatedBy: " ").first ?? $0 }
        }
        return Button {
            guard enabled, view.mayTick(ownKey) else { return }
            onTick(item.item, !done)
        } label: {
            HStack(spacing: 10) {
                LkIconView(done ? .checkmarkCircleFill : .circle, size: 20)
                    .foregroundStyle(Color.lakkoAccent)
                Text(item.text).foregroundStyle(done ? Color.lakkoTextMuted : Color.lakkoText)
                Spacer()
                if let doneByName {
                    Text(doneByName).font(.caption).foregroundStyle(Color.lakkoTextMuted)
                }
            }
            .frame(minHeight: 32)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(done
            ? l10n.t("checklist.item_done", texts: ["text": item.text, "name": doneByName ?? ""])
            : l10n.t("checklist.item_not_done", texts: ["text": item.text]))
    }
}

// MARK: - Location card

struct LocationCard: View {
    let location: SharedLocation
    @ObservedObject private var l10n = Localization.shared
    @Environment(\.openURL) private var openURL

    var body: some View {
        let coords = LocationText.coordinates(latE7: location.latE7, lonE7: location.lonE7)
        let distance = LocationText.distance(meters: location.accuracyM)
        Button {
            if let url = LocationText.appleMapsURL(latE7: location.latE7, lonE7: location.lonE7, name: location.name) {
                openURL(url)
            }
        } label: {
            HStack(alignment: .top, spacing: 12) {
                LkIconView(.location, size: 20)
                    .foregroundStyle(Color.lakkoAccent)
                    .frame(width: 40, height: 40)
                    .background(Circle().fill(Color.lakkoAccentSoft))
                VStack(alignment: .leading, spacing: 2) {
                    if let name = location.name { Text(name).foregroundStyle(Color.lakkoText) }
                    Text(coords).font(.footnote).foregroundStyle(Color.lakkoTextMuted)
                    Text(l10n.t("location.accuracy", texts: ["distance": distance]))
                        .font(.caption)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
            }
            .padding(12)
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel((location.name.map { $0 + ", " } ?? "")
            + l10n.t("location.a11y", texts: ["coordinates": coords, "distance": distance]))
    }
}

// MARK: - Contact card

struct ContactCardView: View {
    let name: String
    let username: String
    let action: (ContactCardButton, () -> Void)?
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 12) {
                Text(avatarInitials(name: name))
                    .font(.headline)
                    .foregroundStyle(Color.lakkoAccentForeground)
                    .frame(width: 44, height: 44)
                    .background(Circle().fill(Color.lakkoAccent))
                VStack(alignment: .leading, spacing: 2) {
                    Text(name).foregroundStyle(Color.lakkoText)
                    Text("@\(username)").font(.footnote).foregroundStyle(Color.lakkoTextMuted)
                }
            }
            if let (state, perform) = action {
                switch state {
                case .openChat:
                    Button(l10n.t("common.open_chat"), action: perform).font(.footnote.weight(.semibold))
                case .add:
                    Button(l10n.t("contact.add"), action: perform).font(.footnote.weight(.semibold))
                case .thisIsYou:
                    Text(l10n.t("contact.this_is_you")).font(.footnote).foregroundStyle(Color.lakkoTextMuted)
                case .working:
                    ProgressView().controlSize(.small)
                case .notice(let text):
                    Text(text).font(.footnote).foregroundStyle(Color.lakkoTextMuted)
                }
            }
        }
        .padding(12)
    }
}

// MARK: - Article card

struct ArticleCard: View {
    let article: Article
    let attachments: [AttachmentRef]
    let onOpen: () -> Void
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        Button(action: onOpen) {
            VStack(alignment: .leading, spacing: 8) {
                if let banner = firstImage {
                    ArticleImage(ref: banner).frame(maxHeight: 180).clipped()
                }
                Text(article.title)
                    .font(.system(size: 17, weight: .semibold))
                    .lineLimit(3)
                    .foregroundStyle(Color.lakkoText)
                let excerpt = articleExcerpt(article: article)
                if !excerpt.isEmpty {
                    Text(excerpt).font(.footnote).lineLimit(4).foregroundStyle(Color.lakkoTextMuted)
                }
                Text(l10n.t("article.read")).font(.footnote.weight(.semibold)).foregroundStyle(Color.lakkoAccent)
            }
            .padding(12)
            .frame(maxWidth: 360, alignment: .leading)
        }
        .buttonStyle(.plain)
    }

    private var firstImage: AttachmentRef? {
        for block in article.blocks {
            if case .image(let index, _) = block, Int(index) < attachments.count { return attachments[Int(index)] }
        }
        return nil
    }
}

/// An article image: downloaded and decrypted on first show.
struct ArticleImage: View {
    let ref: AttachmentRef
    @State private var image: UIImage?

    var body: some View {
        Group {
            if let image {
                Image(uiImage: image).resizable().scaledToFit()
            } else {
                Rectangle().fill(Color.lakkoSurfaceSunken).frame(height: 120)
            }
        }
        .task(id: ref.blobId) {
            if let bytes = try? await AttachmentBridge.downloadAndDecrypt(ref: ref) {
                image = UIImage(data: bytes)
            }
        }
    }
}

// MARK: - Audio

/// UX section 6 — one audio file plays at a time.
@MainActor
final class AudioFilePlayer: NSObject, ObservableObject, AVAudioPlayerDelegate {
    static let shared = AudioFilePlayer()
    @Published private(set) var playingKey: Data?
    @Published private(set) var progress: Double = 0
    @Published private(set) var failedKey: Data?
    private var player: AVAudioPlayer?
    private var ticker: Timer?

    func toggle(ref: AttachmentRef) {
        if playingKey == ref.blobId {
            player?.pause()
            playingKey = nil
            return
        }
        player?.stop()
        Task { @MainActor in
            guard let bytes = try? await AttachmentBridge.downloadAndDecrypt(ref: ref),
                  let p = try? AVAudioPlayer(data: bytes) else {
                failedKey = ref.blobId
                return
            }
            try? AVAudioSession.sharedInstance().setCategory(.playback)
            p.delegate = self
            p.play()
            player = p
            playingKey = ref.blobId
            failedKey = nil
            ticker?.invalidate()
            ticker = Timer.scheduledTimer(withTimeInterval: 0.25, repeats: true) { [weak self] _ in
                Task { @MainActor in
                    guard let self, let p = self.player, p.duration > 0 else { return }
                    self.progress = p.currentTime / p.duration
                }
            }
        }
    }

    func seek(to fraction: Double) {
        guard let p = player else { return }
        p.currentTime = p.duration * fraction
    }

    nonisolated func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
        Task { @MainActor in
            self.playingKey = nil
            self.progress = 0
            self.ticker?.invalidate()
        }
    }
}

struct AudioBubble: View {
    let ref: AttachmentRef
    @ObservedObject private var player = AudioFilePlayer.shared
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        let playing = player.playingKey == ref.blobId
        let title = ref.title ?? ref.name ?? l10n.t("attach.menu.audio")
        HStack(spacing: 12) {
            Button { player.toggle(ref: ref) } label: {
                LkIconView(playing ? .pauseFill : .playFill, size: 20)
                    .foregroundStyle(Color.lakkoAccentForeground)
                    .frame(width: 40, height: 40)
                    .background(RoundedRectangle(cornerRadius: LakkoRadius.control).fill(Color.lakkoAccent))
            }
            .buttonStyle(.plain)
            .accessibilityLabel(l10n.t(playing ? "audio.pause" : "audio.play", texts: ["title": title]))
            VStack(alignment: .leading, spacing: 2) {
                Text(title).lineLimit(1).foregroundStyle(Color.lakkoText)
                if let artist = ref.artist { Text(artist).font(.footnote).lineLimit(1).foregroundStyle(Color.lakkoTextMuted) }
                if player.failedKey == ref.blobId {
                    Text(l10n.t("audio.unplayable")).font(.caption).foregroundStyle(Color.lakkoDanger)
                } else {
                    ProgressView(value: playing ? player.progress : 0)
                        .tint(Color.lakkoAccent)
                    if let ms = ref.durationMs {
                        Text(Duration.milliseconds(Int64(ms)).formatted(.time(pattern: .minuteSecond)))
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                }
            }
            if let thumb = ref.thumb {
                ThumbTile(thumb: thumb)
            }
        }
        .padding(12)
        .frame(maxWidth: 360)
    }
}

/// The cover art tile of an audio bubble (44 points, rounded).
struct ThumbTile: View {
    let thumb: ThumbRef
    @State private var image: UIImage?

    var body: some View {
        Group {
            if let image { Image(uiImage: image).resizable().scaledToFill() } else { Color.lakkoSurfaceSunken }
        }
        .frame(width: 44, height: 44)
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .task(id: thumb.blobId) {
            let ref = AttachmentRef(
                blobId: thumb.blobId, key: thumb.key, nonce: thumb.nonce, mime: thumb.mime, size: thumb.size,
                name: nil, kind: nil, durationMs: nil, waveform: nil, width: nil, height: nil, thumb: nil,
                title: nil, artist: nil)
            if let bytes = try? await AttachmentBridge.downloadAndDecrypt(ref: ref) { image = UIImage(data: bytes) }
        }
    }
}
