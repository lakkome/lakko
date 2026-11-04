// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI
import UIKit

// `docs/ux/translation-and-transcription.md` — the controls: the text of a
// bubble with its quiet line (3.2, 3.3), the conversation bar (3.6), the
// switch of 3.7, the pack dialog (3.5), the picker (3.4), the transcript
// button and block (4), and Settings (6). What each shows is decided in
// ``MessageTranslationCenter``; these draw it.

// MARK: - The text of a bubble

/// The message text, or its translation, and the quiet line under it.
struct TranslatableMessageText: View {
    let message: ChatMessage
    let outgoing: Bool
    let linkColor: Color
    /// Called with a Lakko link of this deployment (`docs/ux/message-links.md`).
    let onOwnLink: (URL) -> Void
    @ObservedObject private var center = MessageTranslationCenter.shared
    @ObservedObject private var l10n = Localization.shared
    @Environment(\.translationConversation) private var conversation

    var body: some View {
        let phase = conversation.flatMap { center.phase(conversation: $0, messageId: message.messageId) }
        VStack(alignment: .leading, spacing: 3) {
            if case .translated(let text, _) = phase {
                MessageText(text: text, linkColor: linkColor, onOwnLink: onOwnLink).font(.body)
            } else {
                MessageText(text: message.text, linkColor: linkColor, onOwnLink: onOwnLink).font(.body)
            }
            if let phase, let conversation {
                TranslationQuietLine(phase: phase, outgoing: outgoing, actions: .init(
                    showOriginal: { center.showOriginal(conversation: conversation, message: message) },
                    translate: { center.translate(conversation: conversation, message: message) },
                    retry: { center.retry(conversation: conversation, message: message) },
                    notLanguage: { center.openPicker(conversation: conversation, message: message) },
                    cancel: { center.cancelDownload(key: MessageTranslationCenter.key(conversation, message.messageId)) }
                ))
            }
        }
    }
}

/// The one quiet line in the bubble's secondary text colour (UX 3.2).
struct TranslationQuietLine: View {
    struct Actions {
        var showOriginal: () -> Void = {}
        var translate: () -> Void = {}
        var retry: () -> Void = {}
        var notLanguage: () -> Void = {}
        var cancel: () -> Void = {}
    }

    let phase: MessageTranslationCenter.Phase
    let outgoing: Bool
    var prefix: String? = nil
    let actions: Actions
    @ObservedObject private var l10n = Localization.shared

    private var tint: Color { outgoing ? Color.lakkoAccentForeground.opacity(0.8) : Color.lakkoTextMuted }
    private var linkTint: Color { outgoing ? Color.lakkoAccentForeground : Color.lakkoAccent }

    var body: some View {
        HStack(spacing: 4) {
            if let prefix {
                Text(prefix)
                dot
            }
            switch phase {
            case .translating:
                Text(l10n.t("translate.line.translating"))
            case .translated(_, let source):
                let name = LanguageNames.inInterface(source)
                Text(l10n.t("translate.line.translated", texts: ["language": name]))
                dot
                link(l10n.t("translate.line.show_original"), actions.showOriginal)
                dot
                link(l10n.t("translate.line.not_language", texts: ["language": name]), actions.notLanguage)
            case .original(let source):
                Text(LanguageNames.inInterface(source))
                dot
                link(l10n.t("translate.line.translate"), actions.translate)
            case .failed:
                Text(l10n.t("translate.line.failed"))
                dot
                link(l10n.t("translate.line.try_again"), actions.retry)
            case .sameLanguage(let target):
                Text(l10n.t("translate.line.same_language", texts: ["language": LanguageNames.inInterface(target)]))
            case .unsupported(let source):
                Text(l10n.t("translate.unsupported_source", texts: ["language": LanguageNames.inInterface(source)]))
            case .downloading(let language, let percent):
                if let percent {
                    Text(l10n.t(
                        "translate.pack.downloading",
                        texts: ["language": LanguageNames.inInterface(language), "percent": String(percent)]))
                } else {
                    Text(l10n.t("translate.line.translating"))
                }
                dot
                link(l10n.t("translate.pack.cancel"), actions.cancel)
            case .waitingForWifi(let language):
                Text(l10n.t("translate.pack.waiting_wifi", texts: ["language": LanguageNames.inInterface(language)]))
                dot
                link(l10n.t("translate.pack.cancel"), actions.cancel)
            case .blocked:
                Text(l10n.t("translate.pack.blocked_anti_censorship"))
            }
        }
        .font(.caption)
        .foregroundStyle(tint)
        .fixedSize(horizontal: false, vertical: true)
    }

    private var dot: some View { Text("·").accessibilityHidden(true) }

    private func link(_ title: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title).foregroundStyle(linkTint)
        }
        .buttonStyle(.plain)
    }
}

// MARK: - The conversation bar (UX 3.6)

struct TranslationBar: View {
    let conversation: String
    let messages: [ChatMessage]
    @ObservedObject private var center = MessageTranslationCenter.shared
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if let language = center.bars[conversation] {
            HStack(spacing: 10) {
                Image(systemName: "character.bubble")
                    .foregroundStyle(Color.lakkoAccent)
                    .accessibilityHidden(true)
                Text(l10n.t("translate.bar.body", texts: ["language": LanguageNames.inInterface(language)]))
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoText)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Button(l10n.t("translate.bar.translate")) {
                    center.acceptBar(conversation: conversation, messages: messages)
                }
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(Color.lakkoAccent)
                Button(l10n.t("translate.bar.not_now")) { center.dismissBar(conversation: conversation) }
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
                Button { center.dismissBar(conversation: conversation) } label: {
                    Image(systemName: "xmark").font(.caption.weight(.semibold))
                }
                .foregroundStyle(Color.lakkoTextMuted)
                .accessibilityLabel(l10n.t("translate.bar.close"))
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .background(Color.lakkoSurface)
            .overlay(alignment: .bottom) {
                Rectangle().fill(Color.lakkoBorder).frame(height: 0.5)
            }
        }
    }
}

// MARK: - The switch of UX 3.7

/// `Translate automatically`, for a conversation's info screen.
struct AutoTranslateToggle: View {
    let conversation: String
    @ObservedObject private var center = MessageTranslationCenter.shared
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if center.settings.showTranslate && center.translationSupported {
            Toggle(isOn: Binding(
                get: { center.autoTranslate(conversation: conversation) },
                set: { center.setAutoTranslate(conversation: conversation, on: $0) }
            )) {
                Label(l10n.t("translate.auto"), systemImage: "character.bubble")
                    .foregroundStyle(Color.lakkoText)
            }
            .tint(Color.lakkoAccent)
        }
    }
}

// MARK: - Overlays of a conversation screen

/// The pack dialog, the picker and the translation session host, attached
/// once to a conversation screen.
struct TranslationOverlays: ViewModifier {
    let conversation: String
    let messages: [ChatMessage]
    @ObservedObject private var center = MessageTranslationCenter.shared
    @ObservedObject private var l10n = Localization.shared

    func body(content: Content) -> some View {
        content
            .environment(\.translationConversation, conversation)
            .modifier(TranslationTaskHost(center: center))
            .onAppear { center.observe(conversation: conversation, messages: messages) }
            .onChange(of: messages) { _, updated in
                center.observe(conversation: conversation, messages: updated)
            }
            // UX 3.5 — the pack dialog, Lakko's own (never a system alert).
            .lakkoDialog(item: Binding(
                get: { center.packRequest },
                set: { center.packRequest = $0 }
            )) { request in
                PackDialog.content(request)
            }
            // UX 3.4 — the picker: Lakko's own action sheet of rows.
            .lakkoActionSheet(
                isPresented: Binding(
                    get: { center.pickerRequest != nil },
                    set: { if !$0 { center.pickerRequest = nil } }),
                title: l10n.t("translate.picker.title")
            ) {
                guard let request = center.pickerRequest else { return [] }
                return TranslationLanguageChoices.rows(request.languages, checked: nil) { tag in
                    request.choose(tag)
                }
            }
    }
}

/// UX 3.5 — what the pack dialog says. The privacy sentence and who the pack
/// comes from are in its body; the size is left out because the iOS engines
/// do not report one (the spec: left out rather than guessed). `Not now` is
/// the cancel action, so tapping the dim means `Not now` too.
enum PackDialog {
    static func content(_ request: MessageTranslationCenter.PackRequest) -> LakkoDialogContent {
        let name = LanguageNames.inInterface(request.language)
        let title: String
        let body: String
        switch request.kind {
        case .translation:
            title = Localization.text("translate.pack.title.translation", texts: ["language": name])
            body = Localization.text("translate.pack.body.translation", texts: ["language": name])
        case .transcription:
            title = Localization.text("translate.pack.title.transcription", texts: ["language": name])
            body = Localization.text("translate.pack.body.transcription", texts: ["language": name])
        }
        let download = request.onMobileData
            ? Localization.text("translate.pack.download_wifi")
            : Localization.text("translate.pack.download")
        return LakkoDialogContent(
            title: title,
            message: body + "\n\n" + Localization.text("translate.pack.source.ios"),
            actions: [
                .cancel(Localization.text("translate.pack.not_now"), perform: request.notNow),
                .primary(download, perform: request.download),
            ])
    }
}

/// UX 3.4 — the languages this device can do, as rows of Lakko's own action
/// sheet: own names (``LanguageNames/own(_:)``), sorted by the name in the
/// interface language.
enum TranslationLanguageChoices {
    @MainActor
    static func rows(
        _ languages: [String], checked: String?, choose: @escaping (String) -> Void
    ) -> [LakkoMenuItem] {
        let locale = Localization.currentLocale
        let sorted = languages.sorted {
            LanguageNames.inInterface($0).compare(LanguageNames.inInterface($1), locale: locale) == .orderedAscending
        }
        return sorted.map { tag in
            LakkoMenuItem.choice(
                LanguageNames.own(tag),
                checked: checked.map { sameLanguage(a: $0, b: tag) } ?? false
            ) { choose(tag) }
        }
    }
}

// MARK: - Transcripts (UX section 4)

/// What a voice or video bubble needs to offer a transcript: the message it
/// belongs to. `nil` for a take still being sent.
struct TranscriptTarget: Equatable {
    let messageId: Int64
    /// The message's last op id (0 = never edited): a transcript made before
    /// an edit of the message is stale.
    var lastOpId: Int64 = 0
}

/// The `Aa` button. Its space is part of the row in every state
/// (media-messages.md section 2): when the button is not offered the space
/// stays reserved and empty.
struct TranscriptButton: View {
    /// Shorter than the caption line it sits on, so adding it changes the
    /// row's height in no state (`MediaBubbleLayoutTests` measures the line).
    /// The tap target is widened by `contentShape` padding, not by the frame.
    static let size = CGSize(width: 24, height: 12)

    let target: TranscriptTarget?
    let ref: AttachmentRef
    let mediaKey: String
    let tint: Color
    @ObservedObject private var center = MessageTranslationCenter.shared
    @ObservedObject private var l10n = Localization.shared
    @EnvironmentObject private var media: ChatMediaController
    @Environment(\.translationConversation) private var conversation
    @Environment(\.messageMenuPresenter) private var menu
    @State private var pulse = false

    var body: some View {
        ZStack {
            if let target, let conversation, center.transcriptButtonOffered(conversation: conversation) {
                button(conversation: conversation, target: target)
            } else {
                Color.clear.accessibilityHidden(true)
            }
        }
        .frame(width: Self.size.width, height: Self.size.height)
    }

    @ViewBuilder
    private func button(conversation: String, target: TranscriptTarget) -> some View {
        let phase = center.transcriptPhase(conversation: conversation, messageId: target.messageId)
        let open = center.transcriptIsOpen(conversation: conversation, messageId: target.messageId)
        let unavailable = media.failedKeys.contains(mediaKey)
        let selected: Bool = {
            guard open else { return false }
            switch phase {
            case .transcript, .noSpeech: return true
            default: return false
            }
        }()
        Button {
            guard menu?.isOpen != true else { return }
            center.toggleTranscript(
                conversation: conversation, messageId: target.messageId, lastOpId: target.lastOpId,
                ref: ref, media: media)
        } label: {
            ZStack {
                RoundedRectangle(cornerRadius: 4)
                    .fill(selected ? tint.opacity(0.25) : Color.clear)
                Text("Aa")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(tint)
                    .opacity(isTranscribing(phase) && pulse ? 0.35 : 1)
                if phase == .downloadingAudio {
                    ProgressView().scaleEffect(0.45).tint(tint)
                }
            }
            .frame(width: Self.size.width, height: Self.size.height)
            .overlay(RoundedRectangle(cornerRadius: 4).stroke(tint.opacity(0.6), lineWidth: 0.75))
            .contentShape(Rectangle().inset(by: -10))
        }
        .buttonStyle(.plain)
        .disabled(unavailable)
        .opacity(unavailable ? 0.4 : 1)
        .accessibilityLabel(open ? l10n.t("transcript.button.hide") : l10n.t("transcript.button.show"))
        .accessibilityAddTraits(selected ? .isSelected : [])
        .onChange(of: isTranscribing(phase)) { _, transcribing in
            if transcribing {
                withAnimation(.easeInOut(duration: 0.7).repeatForever(autoreverses: true)) { pulse = true }
            } else {
                pulse = false
            }
        }
    }

    private func isTranscribing(_ phase: MessageTranslationCenter.TranscriptPhase?) -> Bool {
        if case .transcribing = phase { return true }
        return false
    }
}

/// The transcript under the row, inside the same bubble (UX 4.2, 4.3).
struct TranscriptBlock: View {
    let target: TranscriptTarget?
    let ref: AttachmentRef
    let outgoing: Bool
    let width: CGFloat
    @ObservedObject private var center = MessageTranslationCenter.shared
    @ObservedObject private var l10n = Localization.shared
    @EnvironmentObject private var media: ChatMediaController
    @Environment(\.translationConversation) private var conversation
    @State private var expanded = false

    private var tint: Color { outgoing ? Color.lakkoAccentForeground.opacity(0.8) : Color.lakkoTextMuted }
    private var linkTint: Color { outgoing ? Color.lakkoAccentForeground : Color.lakkoAccent }

    var body: some View {
        if let target, let conversation, center.transcriptIsOpen(conversation: conversation, messageId: target.messageId),
           let phase = center.transcriptPhase(conversation: conversation, messageId: target.messageId) {
            content(conversation: conversation, target: target, phase: phase)
                .frame(width: width, alignment: .leading)
        }
    }

    @ViewBuilder
    private func content(conversation: String, target: TranscriptTarget, phase: MessageTranslationCenter.TranscriptPhase) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            switch phase {
            case .downloadingAudio:
                EmptyView()
            case .transcribing(let partial):
                if !partial.isEmpty { Text(partial).font(.body) }
                line { Text(l10n.t("transcript.transcribing")) }
            case .transcript(let text, let language):
                transcript(conversation: conversation, target: target, text: text, language: language)
            case .noSpeech:
                line { Text(l10n.t("transcript.no_speech")) }
            case .failed:
                line {
                    Text(l10n.t("transcript.failed"))
                    Text("·").accessibilityHidden(true)
                    link(l10n.t("translate.line.try_again")) {
                        center.retryTranscript(
                            conversation: conversation, messageId: target.messageId, lastOpId: target.lastOpId,
                            ref: ref, media: media)
                    }
                }
            case .permissionDenied:
                // The same way the recorder sends a person to Settings when
                // the microphone is not allowed (`MediaDeniedCard`).
                Text(l10n.t("transcript.permission_denied"))
                    .font(.caption)
                    .foregroundStyle(tint)
                    .fixedSize(horizontal: false, vertical: true)
                link(l10n.t("chat.record.open_settings")) {
                    if let url = URL(string: UIApplication.openSettingsURLString) {
                        UIApplication.shared.open(url)
                    }
                }
                .font(.caption)
            case .downloadingPack(let language, let percent):
                line {
                    Text(l10n.t("translate.pack.downloading", texts: [
                        "language": LanguageNames.inInterface(language), "percent": String(percent ?? 0),
                    ]))
                    Text("·").accessibilityHidden(true)
                    link(l10n.t("translate.pack.cancel")) {
                        center.cancelDownload(key: MessageTranslationCenter.key(conversation, target.messageId, transcript: true))
                    }
                }
            case .waitingForWifi(let language):
                line {
                    Text(l10n.t("translate.pack.waiting_wifi", texts: ["language": LanguageNames.inInterface(language)]))
                    Text("·").accessibilityHidden(true)
                    link(l10n.t("translate.pack.cancel")) {
                        center.cancelDownload(key: MessageTranslationCenter.key(conversation, target.messageId, transcript: true))
                    }
                }
            case .blocked:
                line { Text(l10n.t("translate.pack.blocked_anti_censorship")) }
            case .unsupported(let language):
                line { Text(l10n.t("transcript.unavailable_source", texts: ["language": LanguageNames.inInterface(language)])) }
            }
        }
    }

    @ViewBuilder
    private func transcript(conversation: String, target: TranscriptTarget, text: String, language: String) -> some View {
        let translation = center.phase(conversation: conversation, messageId: target.messageId, transcript: true)
        let shown: String = {
            if case .translated(let translated, _) = translation { return translated }
            return text
        }()
        Text(shown)
            .font(.body)
            .lineLimit(expanded ? nil : 6)
        if !expanded, shown.count > 280 {
            link(l10n.t("transcript.show_more")) { expanded = true }
                .font(.caption)
        }
        if let translation {
            TranslationQuietLine(
                phase: translation, outgoing: outgoing,
                prefix: l10n.t("transcript.line"),
                actions: .init(
                    showOriginal: { center.showOriginal(conversation: conversation, message: stub(target)) },
                    translate: { center.translate(conversation: conversation, message: stub(target)) },
                    retry: { center.retry(conversation: conversation, message: stub(target)) },
                    notLanguage: { center.openPicker(conversation: conversation, message: stub(target)) },
                    cancel: {
                        center.cancelDownload(key: MessageTranslationCenter.key(conversation, target.messageId, transcript: true))
                    }))
        } else {
            line {
                Text(l10n.t("transcript.line"))
                Text("·").accessibilityHidden(true)
                link(l10n.t("transcript.hide")) {
                    center.toggleTranscript(
                conversation: conversation, messageId: target.messageId, lastOpId: target.lastOpId,
                ref: ref, media: media)
                }
                Text("·").accessibilityHidden(true)
                link(l10n.t("translate.line.not_language", texts: ["language": LanguageNames.inInterface(language)])) {
                    center.openTranscriptPicker(
                        conversation: conversation, messageId: target.messageId, lastOpId: target.lastOpId,
                        ref: ref, media: media)
                }
            }
        }
    }

    /// A transcript's translation is keyed on the message id alone; the
    /// center reads the transcript, not the message text.
    private func stub(_ target: TranscriptTarget) -> ChatMessage {
        ChatMessage(
            messageId: target.messageId, direction: outgoing ? .outgoing : .incoming, text: "", createdAt: "",
            lastOpId: target.lastOpId == 0 ? nil : target.lastOpId)
    }

    private func line<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        HStack(spacing: 4) { content() }
            .font(.caption)
            .foregroundStyle(tint)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func link(_ title: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) { Text(title).foregroundStyle(linkTint) }
            .buttonStyle(.plain)
    }
}
