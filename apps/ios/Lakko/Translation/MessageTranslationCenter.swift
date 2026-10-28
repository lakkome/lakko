// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore
import os
import SwiftUI

/// ADR-0082 / `docs/ux/translation-and-transcription.md` — what the
/// conversation screens show about translating and transcribing, and the
/// work behind it.
///
/// Every decision the three clients must make alike is the shared rule's
/// (`translationOffered`, `conversationBarLanguage`, `transcriptOffered`,
/// `storedResultIsCurrent`, the language comparisons), called through
/// UniFFI; this type gathers the inputs (a detection, an engine answer, the
/// settings) and keeps the state each control draws. The engines are
/// ``MessageTranslationEngine`` and ``MessageTranscriptionEngine``.
///
/// Foreground only: nothing here runs from the notification extension or a
/// background wake (ADR-0082 section 7).
@MainActor
final class MessageTranslationCenter: ObservableObject {

    static let shared = MessageTranslationCenter()

    // MARK: - What a control draws

    /// The quiet line under a message's (or a transcript's) text.
    enum Phase: Equatable {
        case translating
        case translated(text: String, source: String)
        /// `Show original` was chosen: the line reads `Spanish · Translate`.
        case original(source: String)
        case failed
        /// Shown for a few seconds, then cleared.
        case sameLanguage(String)
        /// The picker led to a language this device cannot translate from.
        case unsupported(String)
        case downloading(language: String, percent: Int?)
        case waitingForWifi(String)
        /// Anti-censorship is on: no pack is downloaded (UX 3.5).
        case blocked
    }

    enum TranscriptPhase: Equatable {
        case downloadingAudio
        case transcribing(partial: String)
        case transcript(text: String, language: String)
        case noSpeech
        case failed
        /// Speech recognition is not allowed for Lakko in the system settings.
        case permissionDenied
        case downloadingPack(language: String, percent: Int?)
        case waitingForWifi(String)
        case blocked
        case unsupported(String)
    }

    /// UX 3.5 — the pack dialog.
    struct PackRequest: Identifiable {
        enum Kind { case translation, transcription }
        let id = UUID()
        let kind: Kind
        let language: String
        let onMobileData: Bool
        let download: () -> Void
        let notNow: () -> Void
    }

    /// UX 3.4 — the language picker.
    struct PickerRequest: Identifiable {
        enum Kind { case translation, transcription }
        let id = UUID()
        let kind: Kind
        let languages: [String]
        let choose: (String) -> Void
    }

    typealias MenuState = MessageTranslateMenuState

    @Published private(set) var phases: [String: Phase] = [:]
    @Published private(set) var transcripts: [String: TranscriptPhase] = [:]
    @Published private(set) var openTranscripts: Set<String> = []
    @Published private(set) var bars: [String: String] = [:]
    @Published var packRequest: PackRequest?
    @Published var pickerRequest: PickerRequest?
    /// Bumped when an engine answer arrives, so menus and buttons re-read.
    @Published private(set) var engineGeneration = 0

    let translationEngine: MessageTranslationEngine
    let transcriptionEngine: MessageTranscriptionEngine
    let settings: MessageTranslationSettings
    private let store: MessageTranslationStore
    private let accountProvider: @MainActor () -> String?
    private let antiCensorshipOn: @MainActor () -> Bool
    private let expensivePath: @MainActor () -> Bool

    private var translationStates: [String: TranslationEngineState] = [:]
    private var transcriptionStates: [String: TranslationEngineState] = [:]
    private var anyTranslation: Bool?
    private var detections: [String: (text: String, detected: DetectedLanguage?)] = [:]
    private var tasks: [String: Task<Void, Never>] = [:]
    private var waitingForWifi: [String: () -> Void] = [:]
    /// Messages automatic translation already tried, so a timeline refresh
    /// does not start it again after a failure or a same-language answer.
    private var autoAttempted: Set<String> = []
    private var cancellables: Set<AnyCancellable> = []
    private static let log = Logger(subsystem: "me.lakko", category: "translation")

    init(
        translationEngine: MessageTranslationEngine? = nil,
        transcriptionEngine: MessageTranscriptionEngine? = nil,
        settings: MessageTranslationSettings? = nil,
        store: MessageTranslationStore = .shared,
        accountProvider: @escaping @MainActor () -> String? = { MessageTranslationPurge.boundAccountIdB64 },
        antiCensorshipOn: (@MainActor () -> Bool)? = nil,
        expensivePath: (@MainActor () -> Bool)? = nil
    ) {
        if let translationEngine {
            self.translationEngine = translationEngine
        } else if #available(iOS 18.0, *) {
            self.translationEngine = AppleTranslationEngine()
        } else {
            self.translationEngine = NoTranslationEngine()
        }
        self.transcriptionEngine = transcriptionEngine ?? AppleTranscriptionEngine()
        self.settings = settings ?? .shared
        self.store = store
        self.accountProvider = accountProvider
        self.antiCensorshipOn = antiCensorshipOn ?? {
            AntiCensorshipConfig.current.armed || AntiCensorshipManager.shared.isEnabled
        }
        self.expensivePath = expensivePath ?? { NetworkCostWatcher.shared.isExpensive }
        if expensivePath == nil {
            NetworkCostWatcher.shared.$isExpensive
                .removeDuplicates()
                .sink { [weak self] expensive in
                    if !expensive { self?.resumeWaitingForWifi() }
                }
                .store(in: &cancellables)
        }
        self.settings.objectWillChange
            .sink { [weak self] _ in
                Task { @MainActor in self?.settingsChanged() }
            }
            .store(in: &cancellables)
    }

    /// The op id a stored result is compared on (ADR-0082 section 5): the
    /// message's last edit or delete op, 0 when it was never edited.
    static func opId(_ message: ChatMessage) -> Int64 { message.lastOpId ?? 0 }

    static func key(_ conversation: String, _ messageId: Int64, transcript: Bool = false) -> String {
        "\(conversation.lowercased())|\(messageId)|\(transcript ? "t" : "m")"
    }

    private var account: String? { accountProvider() }
    private var target: String { settings.effectiveTarget }

    // MARK: - Engine answers

    /// Whether this device can translate at all (Settings, UX section 6).
    var translationSupported: Bool { translationEngine.isSupported }
    var transcriptionSupported: Bool { transcriptionEngine.isSupported }

    func translationState(source: String) -> TranslationEngineState? {
        translationStates["\(source)>\(target)"]
    }

    private func loadTranslationState(source: String) async -> TranslationEngineState {
        let key = "\(source)>\(target)"
        if let cached = translationStates[key] { return cached }
        let state = await translationEngine.state(source: source, target: target)
        translationStates[key] = state
        engineGeneration &+= 1
        return state
    }

    private func loadAnyTranslation() async -> Bool {
        if let anyTranslation { return anyTranslation }
        let sources = await translationEngine.sourceLanguages()
        let any = sources.contains { !sameLanguage(a: $0, b: target) }
        anyTranslation = any
        engineGeneration &+= 1
        return any
    }

    private func loadTranscriptionState(language: String) async -> TranslationEngineState {
        if let cached = transcriptionStates[language] { return cached }
        let state = await transcriptionEngine.state(language: language)
        transcriptionStates[language] = state
        engineGeneration &+= 1
        return state
    }

    /// Engine answers may change: a pack arrived, the target moved.
    func invalidateEngineAnswers() {
        translationStates.removeAll()
        transcriptionStates.removeAll()
        anyTranslation = nil
        engineGeneration &+= 1
    }

    private func settingsChanged() {
        invalidateEngineAnswers()
        for conversation in Set(bars.keys) { bars[conversation] = nil }
    }

    // MARK: - Detection

    private func detection(key: String, text: String) -> DetectedLanguage? {
        if let cached = detections[key], cached.text == text { return cached.detected }
        let detected = MessageLanguageDetector.detect(text)
        detections[key] = (text, detected)
        return detected
    }

    // MARK: - The conversation

    /// The timeline changed: detect, decide the bar, translate automatically
    /// where the switch is on, and drop results of messages deleted for
    /// everyone. Called by the conversation screen.
    func observe(conversation: String, messages: [ChatMessage]) {
        guard let account else { return }
        let prefs = store.prefs(accountIdB64: account, conversation: conversation)
        var recent: [DetectedLanguage] = []
        let window = Int(translationBarWindow())
        for message in messages.reversed() {
            if message.deleted {
                if store.result(accountIdB64: account, conversation: conversation,
                                messageId: message.messageId, kind: .translation) != nil {
                    store.removeMessage(accountIdB64: account, conversation: conversation, messageId: message.messageId)
                }
                continue
            }
            guard message.direction == .incoming, !message.text.isEmpty, message.messageId > 0 else { continue }
            let key = Self.key(conversation, message.messageId)
            let detected = detection(key: key, text: message.text)
            if recent.count < window, let detected { recent.append(detected) }
            if prefs.autoTranslate, phases[key] == nil, !autoAttempted.contains(key),
               wantsAutoTranslation(detected) {
                autoAttempted.insert(key)
                translate(conversation: conversation, message: message)
            }
        }
        updateBar(conversation: conversation, recent: recent, prefs: prefs)
        Task { _ = await loadAnyTranslation() }
    }

    private func wantsAutoTranslation(_ detected: DetectedLanguage?) -> Bool {
        guard let detected,
              detected.confidencePermille >= translationDetectionConfidentPermille()
        else { return false }
        return !readsLanguage(readingLanguages: settings.readingLanguages, tag: detected.tag)
    }

    private func updateBar(conversation: String, recent: [DetectedLanguage], prefs: MessageTranslationStore.ConversationPrefs) {
        let input = ConversationBarInput(
            enabled: settings.showTranslate,
            autoTranslateOn: prefs.autoTranslate,
            recentIncoming: recent,
            dismissed: prefs.dismissed)
        guard let language = conversationBarLanguage(input: input, readingLanguages: settings.readingLanguages) else {
            if bars[conversation] != nil { bars[conversation] = nil }
            return
        }
        Task {
            let state = await loadTranslationState(source: language)
            // The rule names a language; the engine decides whether the bar
            // may appear (shown only when it is not unavailable).
            let show = state != .unavailable
            if show, bars[conversation] != language { bars[conversation] = language }
            if !show, bars[conversation] != nil { bars[conversation] = nil }
        }
    }

    /// The bar's `Translate`: automatic translation on (UX 3.6, 3.7).
    func acceptBar(conversation: String, messages: [ChatMessage]) {
        setAutoTranslate(conversation: conversation, on: true, messages: messages)
        bars[conversation] = nil
    }

    /// `Not now` and close: this language never brings the bar back here.
    func dismissBar(conversation: String) {
        guard let account, let language = bars[conversation] else { return }
        var prefs = store.prefs(accountIdB64: account, conversation: conversation)
        if !prefs.dismissed.contains(language) { prefs.dismissed.append(language) }
        store.setPrefs(accountIdB64: account, conversation: conversation, prefs: prefs)
        bars[conversation] = nil
    }

    func autoTranslate(conversation: String) -> Bool {
        guard let account else { return false }
        return store.prefs(accountIdB64: account, conversation: conversation).autoTranslate
    }

    /// UX 3.7 — the per-conversation switch. Off shows every message in its
    /// original again.
    func setAutoTranslate(conversation: String, on: Bool, messages: [ChatMessage]? = nil) {
        guard let account else { return }
        var prefs = store.prefs(accountIdB64: account, conversation: conversation)
        prefs.autoTranslate = on
        store.setPrefs(accountIdB64: account, conversation: conversation, prefs: prefs)
        objectWillChange.send()
        if on {
            if let messages { observe(conversation: conversation, messages: messages) }
        } else {
            let prefix = conversation.lowercased() + "|"
            for key in phases.keys where key.hasPrefix(prefix) && key.hasSuffix("|m") {
                cancel(key)
                phases[key] = nil
            }
            autoAttempted = autoAttempted.filter { !$0.hasPrefix(prefix) }
        }
    }

    // MARK: - The menu row

    func menuState(conversation: String, message: ChatMessage) -> MenuState {
        let (key, text, _) = source(conversation: conversation, message: message)
        guard let text else { return .unavailable }
        if case .translated = phases[key] { return .showOriginal }
        let detected = detection(key: key, text: text)
        let engine: TranslationEngineState = detected.flatMap { translationState(source: $0.tag) } ?? .unavailable
        if let detected, translationState(source: detected.tag) == nil {
            Task { _ = await loadTranslationState(source: detected.tag) }
        }
        let input = TranslateOfferInput(
            enabled: settings.showTranslate,
            hasText: true,
            isOwn: message.direction == .outgoing,
            detected: detected,
            engineForDetected: engine,
            anyEngine: anyTranslation ?? false)
        switch translationOffered(input: input, readingLanguages: settings.readingLanguages) {
        case .translate: return .translate
        case .translateAskLanguage: return .translateAskLanguage
        case TranslateOffer.none: return .unavailable
        }
    }

    /// Which text the menu acts on: the message's own, else its transcript
    /// (UX 4.2, "a transcript is text like any other for the menu").
    private func source(conversation: String, message: ChatMessage) -> (key: String, text: String?, transcript: Bool) {
        if !message.text.isEmpty {
            return (Self.key(conversation, message.messageId), message.text, false)
        }
        let tKey = Self.key(conversation, message.messageId, transcript: true)
        if case .transcript(let text, _) = transcripts[tKey], !text.isEmpty {
            return (tKey, text, true)
        }
        return (tKey, nil, true)
    }

    /// What Copy puts on the clipboard: the text shown (UX 3.2).
    func copyText(conversation: String, message: ChatMessage) -> String? {
        let (key, text, _) = source(conversation: conversation, message: message)
        if case .translated(let translated, _) = phases[key] { return translated }
        return text
    }

    /// The menu's `Translate`.
    func translate(conversation: String, message: ChatMessage, askLanguage: Bool = false) {
        let (key, text, transcript) = source(conversation: conversation, message: message)
        guard let text, let account else { return }
        let prefs = store.prefs(accountIdB64: account, conversation: conversation)
        let chosen = prefs.messageSources[String(message.messageId)] ?? prefs.translationSource
        let detected = detection(key: key, text: text)
        let confident = detected.map { $0.confidencePermille >= translationDetectionConfidentPermille() } ?? false
        let source: String? = prefs.messageSources[String(message.messageId)]
            ?? (confident ? detected?.tag : nil)
            ?? chosen
        guard !askLanguage, let source else {
            openPicker(conversation: conversation, message: message)
            return
        }
        run(key: key, conversation: conversation, messageId: message.messageId, opId: Self.opId(message),
            text: text, source: source, transcript: transcript)
    }

    /// `Show original`.
    func showOriginal(conversation: String, message: ChatMessage) {
        let (key, _, _) = source(conversation: conversation, message: message)
        cancel(key)
        if case .translated(_, let source) = phases[key] {
            phases[key] = .original(source: source)
        } else {
            phases[key] = nil
        }
    }

    /// `Not Spanish?` — the picker of the languages this device translates from.
    func openPicker(conversation: String, message: ChatMessage) {
        Task {
            let languages = await translationEngine.sourceLanguages()
                .filter { !sameLanguage(a: $0, b: target) }
            pickerRequest = PickerRequest(kind: .translation, languages: languages) { [weak self] tag in
                self?.chooseSource(conversation: conversation, message: message, tag: tag)
            }
        }
    }

    /// The picker's choice applies to this message and is remembered for the
    /// conversation (UX 3.4).
    func chooseSource(conversation: String, message: ChatMessage, tag: String) {
        guard let account, let canonical = canonicalLanguageTag(tag: tag) else { return }
        var prefs = store.prefs(accountIdB64: account, conversation: conversation)
        prefs.messageSources[String(message.messageId)] = canonical
        prefs.translationSource = canonical
        store.setPrefs(accountIdB64: account, conversation: conversation, prefs: prefs)
        let (key, text, transcript) = source(conversation: conversation, message: message)
        guard let text else { return }
        run(key: key, conversation: conversation, messageId: message.messageId, opId: Self.opId(message),
            text: text, source: canonical, transcript: transcript)
    }

    private func run(
        key: String, conversation: String, messageId: Int64, opId: Int64,
        text: String, source: String, transcript: Bool
    ) {
        guard let account else { return }
        if sameLanguage(a: source, b: target) {
            showSameLanguage(key: key)
            return
        }
        let kind: MessageTranslationStore.Kind = transcript ? .transcriptTranslation : .translation
        if let stored = store.result(accountIdB64: account, conversation: conversation, messageId: messageId, kind: kind),
           storedResultIsCurrent(storedLastOpId: stored.opId, currentLastOpId: opId,
                                 storedTarget: stored.language, currentTarget: target) {
            phases[key] = .translated(text: stored.text, source: stored.source ?? source)
            return
        }
        cancel(key)
        phases[key] = .translating
        let target = self.target
        tasks[key] = Task { [weak self] in
            guard let self else { return }
            let state = await self.loadTranslationState(source: source)
            switch state {
            case .unavailable:
                self.phases[key] = .unsupported(source)
            case .needsPack:
                self.phases[key] = nil
                self.askForPack(kind: .translation, language: source, key: key) { [weak self] in
                    self?.downloadTranslationPack(
                        key: key, source: source, then: {
                            self?.run(key: key, conversation: conversation, messageId: messageId, opId: opId,
                                      text: text, source: source, transcript: transcript)
                        })
                }
            case .ready:
                do {
                    let translated = try await self.translationEngine.translate(text, source: source, target: target)
                    guard !Task.isCancelled else { return }
                    if translated.trimmingCharacters(in: .whitespacesAndNewlines)
                        == text.trimmingCharacters(in: .whitespacesAndNewlines) {
                        self.showSameLanguage(key: key)
                        return
                    }
                    self.store.putResult(
                        accountIdB64: account, conversation: conversation, messageId: messageId, kind: kind,
                        result: .init(text: translated, language: target, source: source, opId: opId, complete: true))
                    self.phases[key] = .translated(text: translated, source: source)
                } catch {
                    guard !Task.isCancelled else { return }
                    Self.log.warning("translation failed")
                    self.phases[key] = .failed
                }
            }
        }
    }

    private func showSameLanguage(key: String) {
        phases[key] = .sameLanguage(target)
        Task { [weak self] in
            try? await Task.sleep(nanoseconds: 3_000_000_000)
            if case .sameLanguage = self?.phases[key] { self?.phases[key] = nil }
        }
    }

    /// `Try again` on a failed translation.
    func retry(conversation: String, message: ChatMessage) {
        let (key, _, _) = source(conversation: conversation, message: message)
        phases[key] = nil
        translate(conversation: conversation, message: message)
    }

    func cancelDownload(key: String) {
        cancel(key)
        waitingForWifi[key] = nil
        phases[key] = nil
        if transcripts[key] != nil { transcripts[key] = nil }
    }

    private func cancel(_ key: String) {
        tasks[key]?.cancel()
        tasks[key] = nil
    }

    func phase(conversation: String, messageId: Int64, transcript: Bool = false) -> Phase? {
        phases[Self.key(conversation, messageId, transcript: transcript)]
    }

    // MARK: - Packs (UX 3.5)

    private func askForPack(kind: PackRequest.Kind, language: String, key: String, download: @escaping () -> Void) {
        if antiCensorshipOn() {
            switch kind {
            case .translation: phases[key] = .blocked
            case .transcription: transcripts[key] = .blocked
            }
            return
        }
        let onMobile = expensivePath() && !settings.mobileData
        packRequest = PackRequest(kind: kind, language: language, onMobileData: onMobile, download: { [weak self] in
            self?.packRequest = nil
            guard let self else { return }
            if self.expensivePath() && !self.settings.mobileData {
                switch kind {
                case .translation: self.phases[key] = .waitingForWifi(language)
                case .transcription: self.transcripts[key] = .waitingForWifi(language)
                }
                self.waitingForWifi[key] = download
            } else {
                download()
            }
        }, notNow: { [weak self] in
            self?.packRequest = nil
            if kind == .transcription { self?.transcripts[key] = nil }
        })
    }

    private func resumeWaitingForWifi() {
        let waiting = waitingForWifi
        waitingForWifi.removeAll()
        for (_, start) in waiting { start() }
    }

    private func downloadTranslationPack(key: String, source: String, then: @escaping () -> Void) {
        guard !antiCensorshipOn() else {
            phases[key] = .blocked
            return
        }
        // Apple draws the approval and the progress of its own packs; no
        // percentage reaches the app.
        phases[key] = .downloading(language: source, percent: nil)
        let target = self.target
        tasks[key] = Task { [weak self] in
            guard let self else { return }
            do {
                try await self.translationEngine.preparePack(source: source, target: target)
                self.invalidateEngineAnswers()
                self.phases[key] = nil
                then()
            } catch {
                guard !Task.isCancelled else { return }
                self.phases[key] = .failed
            }
        }
    }

    // MARK: - Transcripts (UX section 4)

    /// The language a voice message is transcribed in (UX 4.4): the one last
    /// chosen for the conversation, else the interface language. Detecting
    /// the spoken language before transcribing is not something the iOS
    /// engines do.
    func transcriptLanguage(conversation: String) -> String {
        guard let account else { return settings.interfaceLanguage }
        return store.prefs(accountIdB64: account, conversation: conversation).transcriptSource
            ?? settings.interfaceLanguage
    }

    /// Whether the `Aa` button is drawn for a voice or video message.
    func transcriptButtonOffered(conversation: String) -> Bool {
        let language = transcriptLanguage(conversation: conversation)
        guard let state = transcriptionStates[language] else {
            Task { _ = await loadTranscriptionState(language: language) }
            return false
        }
        return transcriptOffered(enabled: settings.showTranscript, hasAudio: true, engine: state)
    }

    func transcriptPhase(conversation: String, messageId: Int64) -> TranscriptPhase? {
        transcripts[Self.key(conversation, messageId, transcript: true)]
    }

    func transcriptIsOpen(conversation: String, messageId: Int64) -> Bool {
        openTranscripts.contains(Self.key(conversation, messageId, transcript: true))
    }

    /// The `Aa` button, and `Hide`.
    func toggleTranscript(
        conversation: String, messageId: Int64, lastOpId: Int64 = 0, ref: AttachmentRef, media: ChatMediaController
    ) {
        let key = Self.key(conversation, messageId, transcript: true)
        if openTranscripts.contains(key) {
            openTranscripts.remove(key)
            return
        }
        openTranscripts.insert(key)
        if case .transcript = transcripts[key] { return }
        if case .noSpeech = transcripts[key] { return }
        transcribe(conversation: conversation, messageId: messageId, opId: lastOpId, ref: ref, media: media, language: nil)
    }

    /// `Not Spanish?` under a transcript.
    func openTranscriptPicker(
        conversation: String, messageId: Int64, lastOpId: Int64 = 0, ref: AttachmentRef, media: ChatMediaController
    ) {
        Task {
            let languages = await transcriptionEngine.languages()
            pickerRequest = PickerRequest(kind: .transcription, languages: languages) { [weak self] tag in
                guard let self, let account = self.account, let canonical = canonicalLanguageTag(tag: tag) else { return }
                var prefs = self.store.prefs(accountIdB64: account, conversation: conversation)
                prefs.transcriptSource = canonical
                self.store.setPrefs(accountIdB64: account, conversation: conversation, prefs: prefs)
                self.transcribe(
                    conversation: conversation, messageId: messageId, opId: lastOpId, ref: ref, media: media,
                    language: canonical)
            }
        }
    }

    func retryTranscript(
        conversation: String, messageId: Int64, lastOpId: Int64 = 0, ref: AttachmentRef, media: ChatMediaController
    ) {
        transcribe(conversation: conversation, messageId: messageId, opId: lastOpId, ref: ref, media: media, language: nil)
    }

    private func transcribe(
        conversation: String, messageId: Int64, opId: Int64, ref: AttachmentRef, media: ChatMediaController,
        language: String?
    ) {
        guard let account else { return }
        let key = Self.key(conversation, messageId, transcript: true)
        let language = language ?? transcriptLanguage(conversation: conversation)
        if let stored = store.result(accountIdB64: account, conversation: conversation, messageId: messageId, kind: .transcript),
           stored.complete,
           storedResultIsCurrent(storedLastOpId: stored.opId, currentLastOpId: opId,
                                 storedTarget: stored.language, currentTarget: language) {
            transcripts[key] = stored.text.isEmpty ? .noSpeech : .transcript(text: stored.text, language: stored.language)
            return
        }
        cancel(key)
        tasks[key] = Task { [weak self] in
            guard let self else { return }
            let state = await self.loadTranscriptionState(language: language)
            switch state {
            case .unavailable:
                self.transcripts[key] = .unsupported(language)
                return
            case .needsPack:
                self.transcripts[key] = nil
                self.askForPack(kind: .transcription, language: language, key: key) { [weak self] in
                    self?.downloadTranscriptionPack(key: key, language: language) {
                        self?.transcribe(conversation: conversation, messageId: messageId, opId: opId, ref: ref,
                                         media: media, language: language)
                    }
                }
                return
            case .ready:
                break
            }
            // The audio is fetched and authenticated the way playing does it
            // (ChatMediaController's AEAD open fails before a byte is written),
            // so partial or unauthenticated bytes are never transcribed.
            self.transcripts[key] = .downloadingAudio
            let file: URL
            do {
                file = try await media.mediaFile(for: ref)
            } catch {
                self.transcripts[key] = .failed
                return
            }
            guard !Task.isCancelled else { return }
            self.transcripts[key] = .transcribing(partial: "")
            do {
                let text = try await self.transcriptionEngine.transcribe(audio: file, language: language) { [weak self] partial in
                    if case .transcribing = self?.transcripts[key] { self?.transcripts[key] = .transcribing(partial: partial) }
                }
                guard !Task.isCancelled else { return }
                self.store.putResult(
                    accountIdB64: account, conversation: conversation, messageId: messageId, kind: .transcript,
                    result: .init(text: text, language: language, source: nil, opId: opId, complete: true))
                self.transcripts[key] = text.isEmpty ? .noSpeech : .transcript(text: text, language: language)
            } catch MessageTranslationError.permissionDenied {
                Self.log.info("transcription refused: speech recognition not allowed")
                self.transcripts[key] = .permissionDenied
            } catch {
                guard !Task.isCancelled else { return }
                Self.log.warning("transcription failed")
                self.transcripts[key] = .failed
            }
        }
    }

    private func downloadTranscriptionPack(key: String, language: String, then: @escaping () -> Void) {
        guard !antiCensorshipOn() else {
            transcripts[key] = .blocked
            return
        }
        transcripts[key] = .downloadingPack(language: language, percent: 0)
        tasks[key] = Task { [weak self] in
            guard let self else { return }
            do {
                try await self.transcriptionEngine.downloadPack(language: language) { [weak self] fraction in
                    self?.transcripts[key] = .downloadingPack(
                        language: language, percent: Int((fraction * 100).rounded()))
                }
                self.invalidateEngineAnswers()
                self.transcripts[key] = nil
                then()
            } catch {
                guard !Task.isCancelled else { return }
                self.transcripts[key] = .failed
            }
        }
    }

    // MARK: - Lifecycle

    /// Delete for everyone removed the message.
    func forgetMessage(conversation: String, messageId: Int64) {
        for transcript in [false, true] {
            let key = Self.key(conversation, messageId, transcript: transcript)
            cancel(key)
            phases[key] = nil
            transcripts[key] = nil
            openTranscripts.remove(key)
            detections[key] = nil
        }
    }

    /// Settings > Delete all translations and transcripts.
    @discardableResult
    func deleteAll() -> Int {
        for task in tasks.values { task.cancel() }
        tasks.removeAll()
        phases.removeAll()
        transcripts.removeAll()
        openTranscripts.removeAll()
        guard let account else { return 0 }
        return store.deleteAllResults(accountIdB64: account)
    }

    func storedCount() -> Int {
        guard let account else { return 0 }
        return store.count(accountIdB64: account)
    }

    /// Forget device: the in-memory state of the account goes with its file.
    func forgetAccount() {
        for task in tasks.values { task.cancel() }
        tasks.removeAll()
        phases.removeAll()
        transcripts.removeAll()
        openTranscripts.removeAll()
        bars.removeAll()
        detections.removeAll()
        autoAttempted.removeAll()
        waitingForWifi.removeAll()
        packRequest = nil
        pickerRequest = nil
    }
}

/// What the message menu offers for one message (UX 3.1).
public enum MessageTranslateMenuState: Equatable, Sendable {
    case unavailable
    case translate
    case translateAskLanguage
    case showOriginal
}

// MARK: - Environment

private struct TranslationConversationKey: EnvironmentKey {
    static let defaultValue: String? = nil
}

extension EnvironmentValues {
    /// The conversation (group id, hex) whose rows are drawn below, or `nil`
    /// where no conversation screen is above (a preview, a fixture): the rows
    /// then offer no translation and no transcript.
    var translationConversation: String? {
        get { self[TranslationConversationKey.self] }
        set { self[TranslationConversationKey.self] = newValue }
    }
}
