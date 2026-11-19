// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import Foundation
import LakkoCore
import NaturalLanguage
import Network
import os
import Speech
import SwiftUI
import Translation

/// ADR-0082 section 1 — the iOS engines, each of which runs on this device.
///
/// | | Engine | Local guarantee |
/// |---|---|---|
/// | Detection | `NLLanguageRecognizer` | NaturalLanguage runs in process; it has no network API |
/// | Translation | `TranslationSession` (iOS 18+) | Apple documents on-device processing. On iOS 26.4+ the session is built with the `lowLatency` strategy, which Apple documents as "traditional models", never the Apple Intelligence path |
/// | Transcription, iOS 26+ | `SpeechAnalyzer` + `SpeechTranscriber` | Apple documents the transcriber as on-device; it does not support a locale `SFSpeechRecognizer` only supports via the network |
/// | Transcription, below 26 | `SFSpeechURLRecognitionRequest` | only when `supportsOnDeviceRecognition` is true, always with `requiresOnDeviceRecognition = true` (``SpeechRequestFactory``) |
///
/// When a guarantee cannot be confirmed the language is `unavailable` and the
/// control is absent (section 2). No file here creates a network request:
/// `MessageTranslationFailClosedTests` scans this directory for `URLSession`.
///
/// Logs carry engine states and failure kinds only, never a language, a text
/// or a transcript (section 8).

private let engineLog = Logger(subsystem: "me.lakko", category: "translation-engine")

// MARK: - Detection

enum MessageLanguageDetector {

    /// The most likely language of `text` and how sure the recognizer is, in
    /// thousandths. `nil` when it names no language.
    static func detect(_ text: String) -> DetectedLanguage? {
        candidates(text)?.detected
    }

    /// The detection with its close candidates (ADR-0082 note of 2026-11-15):
    /// the recognizer's hypotheses, as probabilities, through the shared rule
    /// `languageCandidatesFromHypotheses`, the rule the core detector and the
    /// browser's detector use too.
    static func candidates(_ text: String) -> LanguageCandidates? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        let recognizer = NLLanguageRecognizer()
        recognizer.processString(trimmed)
        let hypotheses = recognizer.languageHypotheses(withMaximum: 6)
            .filter { $0.key != .undetermined }
            .map { LanguageHypothesis(
                tag: $0.key.rawValue,
                probabilityPermille: UInt16(max(0, min(1000, ($0.value * 1000).rounded())))) }
        return languageCandidatesFromHypotheses(text: trimmed, hypotheses: hypotheses)
    }
}

// MARK: - Translation

/// What translation can do on this device.
@MainActor
protocol MessageTranslationEngine: AnyObject {
    /// Whether this OS has a translation engine at all.
    var isSupported: Bool { get }
    func state(source: String, target: String) async -> TranslationEngineState
    /// The languages it can translate from, canonical.
    func sourceLanguages() async -> [String]
    /// Languages whose pack is on the device (into `target`).
    func installedLanguages(target: String) async -> [String]
    func translate(_ text: String, source: String, target: String) async throws -> String
    /// Download the pack for the pair. On iOS the system draws its own
    /// approval and progress; there is no other way to obtain an Apple pack.
    func preparePack(source: String, target: String) async throws
}

/// The answer for a device without the Translation framework (iOS 17).
@MainActor
final class NoTranslationEngine: MessageTranslationEngine {
    var isSupported: Bool { false }
    func state(source: String, target: String) async -> TranslationEngineState { .unavailable }
    func sourceLanguages() async -> [String] { [] }
    func installedLanguages(target: String) async -> [String] { [] }
    func translate(_ text: String, source: String, target: String) async throws -> String {
        throw MessageTranslationError.unavailable
    }
    func preparePack(source: String, target: String) async throws { throw MessageTranslationError.unavailable }
}

enum MessageTranslationError: Error, Equatable {
    case unavailable
    case notInstalled
    case failed
    case permissionDenied
}

/// How every Apple translation session is configured. One place, so the
/// fail-closed test has one thing to read.
@available(iOS 18.0, *)
enum TranslationSessionFactory {

    static func configuration(source: String, target: String) -> TranslationSession.Configuration {
        var configuration = TranslationSession.Configuration(
            source: Locale.Language(identifier: source),
            target: Locale.Language(identifier: target))
        if #available(iOS 26.4, *) {
            // `highFidelity` may use Apple Intelligence models; `lowLatency`
            // is the traditional on-device model (Apple's documentation).
            configuration.preferredStrategy = .lowLatency
        }
        return configuration
    }

    /// A session over installed packs only: it throws `notInstalled` rather
    /// than prompting a download, so a translation can never start one.
    @available(iOS 26.0, *)
    static func installedSession(source: String, target: String) -> TranslationSession {
        let s = Locale.Language(identifier: source)
        let t = Locale.Language(identifier: target)
        if #available(iOS 26.4, *) {
            return TranslationSession(installedSource: s, target: t, preferredStrategy: .lowLatency)
        }
        return TranslationSession(installedSource: s, target: t)
    }

    static func state(_ status: LanguageAvailability.Status) -> TranslationEngineState {
        switch status {
        case .installed: return .ready
        case .supported: return .needsPack
        case .unsupported: return .unavailable
        @unknown default: return .unavailable
        }
    }
}

/// Apple's Translation framework. Below iOS 26 a session exists only inside
/// a SwiftUI `.translationTask`, so the work runs through
/// ``TranslationTaskRunner``, which a view hosts (``TranslationTaskHost``).
@available(iOS 18.0, *)
@MainActor
final class AppleTranslationEngine: MessageTranslationEngine {

    let runner = TranslationTaskRunner()
    /// Asked with the strategy the sessions use (`lowLatency` on iOS 26.4+).
    /// A plain `LanguageAvailability()` answered `installed` for pairs whose
    /// `lowLatency` model was not on the device, so the session then threw
    /// `notInstalled` and every such message ended in "Couldn't translate"
    /// instead of offering the pack (seen on an iPhone, iOS 27.2, 2026-11-15).
    private let availability: LanguageAvailability = {
        if #available(iOS 26.4, *) {
            return LanguageAvailability(preferredStrategy: .lowLatency)
        }
        return LanguageAvailability()
    }()

    var isSupported: Bool { true }

    func state(source: String, target: String) async -> TranslationEngineState {
        if sameLanguage(a: source, b: target) { return .unavailable }
        let status = await availability.status(
            from: Locale.Language(identifier: source), to: Locale.Language(identifier: target))
        return TranslationSessionFactory.state(status)
    }

    func sourceLanguages() async -> [String] {
        var out: [String] = []
        for language in await availability.supportedLanguages {
            guard let tag = canonicalLanguageTag(tag: language.minimalIdentifier),
                  !out.contains(tag) else { continue }
            out.append(tag)
        }
        return out
    }

    func installedLanguages(target: String) async -> [String] {
        var out: [String] = []
        for tag in await sourceLanguages() where !sameLanguage(a: tag, b: target) {
            if await state(source: tag, target: target) == .ready { out.append(tag) }
        }
        return out
    }

    func translate(_ text: String, source: String, target: String) async throws -> String {
        // Checked right before: a session that can request downloads would
        // otherwise prompt for a pack the person never agreed to (UX 3.5).
        guard await state(source: source, target: target) == .ready else {
            throw MessageTranslationError.notInstalled
        }
        if #available(iOS 26.0, *) {
            let session = TranslationSessionFactory.installedSession(source: source, target: target)
            return try await session.translate(text).targetText
        }
        return try await runner.translate(text, source: source, target: target)
    }

    func preparePack(source: String, target: String) async throws {
        try await runner.prepare(source: source, target: target)
    }
}

/// Runs translation work inside the session a `.translationTask` hands out.
///
/// Jobs run in order; a job for another language pair moves the
/// configuration, which makes SwiftUI call the task again with a new session.
@available(iOS 18.0, *)
@MainActor
final class TranslationTaskRunner: ObservableObject {

    @Published private(set) var configuration: TranslationSession.Configuration?

    private enum Work { case translate(String), prepare }

    private struct Job {
        let source: String
        let target: String
        let work: Work
        let continuation: CheckedContinuation<String, Error>
    }

    private var jobs: [Job] = []
    private var running = false

    func translate(_ text: String, source: String, target: String) async throws -> String {
        try await withCheckedThrowingContinuation { continuation in
            jobs.append(Job(source: source, target: target, work: .translate(text), continuation: continuation))
            scheduleIfIdle()
        }
    }

    func prepare(source: String, target: String) async throws {
        _ = try await withCheckedThrowingContinuation { continuation in
            jobs.append(Job(source: source, target: target, work: .prepare, continuation: continuation))
            scheduleIfIdle()
        }
    }

    /// The `.translationTask` action.
    func run(_ session: TranslationSession) async {
        running = true
        while let job = jobs.first, matches(job) {
            jobs.removeFirst()
            do {
                switch job.work {
                case .translate(let text):
                    job.continuation.resume(returning: try await session.translate(text).targetText)
                case .prepare:
                    try await session.prepareTranslation()
                    job.continuation.resume(returning: "")
                }
            } catch {
                job.continuation.resume(throwing: error)
            }
        }
        running = false
        scheduleIfIdle()
    }

    private func matches(_ job: Job) -> Bool {
        guard let configuration else { return false }
        return configuration == TranslationSessionFactory.configuration(source: job.source, target: job.target)
            || (configuration.source?.minimalIdentifier == Locale.Language(identifier: job.source).minimalIdentifier
                && configuration.target?.minimalIdentifier == Locale.Language(identifier: job.target).minimalIdentifier)
    }

    private func scheduleIfIdle() {
        guard !running, let next = jobs.first else { return }
        if configuration != nil, matches(next) {
            configuration?.invalidate()
        } else {
            configuration = TranslationSessionFactory.configuration(source: next.source, target: next.target)
        }
    }
}

/// Hosts the session of ``TranslationTaskRunner`` on a view that stays on
/// screen while a conversation is open. A no-op below iOS 18.
struct TranslationTaskHost: ViewModifier {
    @ObservedObject var center: MessageTranslationCenter

    func body(content: Content) -> some View {
        if #available(iOS 18.0, *), let engine = center.translationEngine as? AppleTranslationEngine {
            content.modifier(RunnerHost(runner: engine.runner))
        } else {
            content
        }
    }

    @available(iOS 18.0, *)
    private struct RunnerHost: ViewModifier {
        @ObservedObject var runner: TranslationTaskRunner
        func body(content: Content) -> some View {
            content.translationTask(runner.configuration) { session in
                await runner.run(session)
            }
        }
    }
}

// MARK: - Transcription

@MainActor
protocol MessageTranscriptionEngine: AnyObject {
    var isSupported: Bool { get }
    func state(language: String) async -> TranslationEngineState
    func languages() async -> [String]
    func installedLanguages() async -> [String]
    func downloadPack(language: String, progress: @escaping @MainActor (Double) -> Void) async throws
    /// The transcript of the audio in `audio` (an authenticated, decrypted
    /// file); `partial` sees the text so far. An empty result is "no speech".
    func transcribe(
        audio: URL, language: String, partial: @escaping @MainActor (String) -> Void
    ) async throws -> String
}

/// How every legacy speech request is built: always on device.
///
/// ADR-0082 section 2 — a request without `requiresOnDeviceRecognition` may
/// be sent to Apple's servers. Every `SFSpeechURLRecognitionRequest` in the
/// app is made here, and `MessageTranslationFailClosedTests` checks both that
/// the flag is set and that no other file builds one.
enum SpeechRequestFactory {

    /// `nil` when the recognizer cannot recognize on device: the caller then
    /// treats the language as unavailable, never as "try the network".
    static func onDeviceRequest(url: URL, recognizer: SFSpeechRecognizer) -> SFSpeechURLRecognitionRequest? {
        guard recognizer.supportsOnDeviceRecognition else { return nil }
        let request = SFSpeechURLRecognitionRequest(url: url)
        request.requiresOnDeviceRecognition = true
        request.shouldReportPartialResults = true
        request.addsPunctuation = true
        guard request.requiresOnDeviceRecognition else { return nil }
        return request
    }
}

@MainActor
final class AppleTranscriptionEngine: MessageTranscriptionEngine {

    var isSupported: Bool { true }

    /// Speech recognition needs the person's permission on every path.
    static func authorize() async -> Bool {
        switch SFSpeechRecognizer.authorizationStatus() {
        case .authorized: return true
        case .denied, .restricted: return false
        case .notDetermined:
            return await withCheckedContinuation { continuation in
                SFSpeechRecognizer.requestAuthorization { status in
                    continuation.resume(returning: status == .authorized)
                }
            }
        @unknown default: return false
        }
    }

    private var usesAnalyzer: Bool {
        if #available(iOS 26.0, *) { return SpeechTranscriber.isAvailable }
        return false
    }

    func state(language: String) async -> TranslationEngineState {
        if #available(iOS 26.0, *), SpeechTranscriber.isAvailable {
            guard let locale = await SpeechTranscriber.supportedLocale(equivalentTo: Locale(identifier: language))
            else { return .unavailable }
            let module = SpeechTranscriber(locale: locale, preset: .transcription)
            switch await AssetInventory.status(forModules: [module]) {
            case .installed: return .ready
            case .supported, .downloading: return .needsPack
            case .unsupported: return .unavailable
            @unknown default: return .unavailable
            }
        }
        guard let recognizer = SFSpeechRecognizer(locale: Locale(identifier: language)),
              recognizer.supportsOnDeviceRecognition
        else { return .unavailable }
        return .ready
    }

    func languages() async -> [String] {
        var locales: [Locale] = []
        if #available(iOS 26.0, *), SpeechTranscriber.isAvailable {
            locales = await SpeechTranscriber.supportedLocales
        } else {
            locales = SFSpeechRecognizer.supportedLocales().filter {
                SFSpeechRecognizer(locale: $0)?.supportsOnDeviceRecognition == true
            }
        }
        var out: [String] = []
        for locale in locales {
            guard let tag = canonicalLanguageTag(tag: locale.identifier), !out.contains(tag) else { continue }
            out.append(tag)
        }
        return out
    }

    func installedLanguages() async -> [String] {
        if #available(iOS 26.0, *), SpeechTranscriber.isAvailable {
            var out: [String] = []
            for locale in await SpeechTranscriber.installedLocales {
                if let tag = canonicalLanguageTag(tag: locale.identifier), !out.contains(tag) { out.append(tag) }
            }
            return out
        }
        // Below iOS 26 the on-device models are the dictation languages the
        // system keeps; they are not packs Lakko asked for.
        return await languages()
    }

    func downloadPack(language: String, progress: @escaping @MainActor (Double) -> Void) async throws {
        guard #available(iOS 26.0, *), SpeechTranscriber.isAvailable,
              let locale = await SpeechTranscriber.supportedLocale(equivalentTo: Locale(identifier: language))
        else { throw MessageTranslationError.unavailable }
        let module = SpeechTranscriber(locale: locale, preset: .transcription)
        guard let request = try await AssetInventory.assetInstallationRequest(supporting: [module]) else {
            return  // already installed
        }
        let observation = request.progress.observe(\.fractionCompleted, options: [.new]) { p, _ in
            let fraction = p.fractionCompleted
            Task { @MainActor in progress(fraction) }
        }
        defer { observation.invalidate() }
        try await request.downloadAndInstall()
    }

    func transcribe(
        audio: URL, language: String, partial: @escaping @MainActor (String) -> Void
    ) async throws -> String {
        guard await Self.authorize() else { throw MessageTranslationError.permissionDenied }
        let file = try await AudioExtract.m4a(from: audio)
        defer { try? FileManager.default.removeItem(at: file) }
        if #available(iOS 26.0, *), SpeechTranscriber.isAvailable {
            return try await analyze(file: file, language: language, partial: partial)
        }
        return try await recognize(file: file, language: language, partial: partial)
    }

    @available(iOS 26.0, *)
    private func analyze(
        file: URL, language: String, partial: @escaping @MainActor (String) -> Void
    ) async throws -> String {
        guard let locale = await SpeechTranscriber.supportedLocale(equivalentTo: Locale(identifier: language))
        else { throw MessageTranslationError.unavailable }
        let transcriber = SpeechTranscriber(
            locale: locale, transcriptionOptions: [], reportingOptions: [.volatileResults], attributeOptions: [])
        guard await AssetInventory.status(forModules: [transcriber]) == .installed else {
            throw MessageTranslationError.notInstalled
        }
        let analyzer = SpeechAnalyzer(modules: [transcriber])
        let collector = Task { @MainActor () throws -> String in
            var final = ""
            for try await result in transcriber.results {
                let piece = String(result.text.characters)
                if result.isFinal {
                    final += piece
                    partial(final)
                } else {
                    partial(final + piece)
                }
            }
            return final
        }
        let audioFile = try AVAudioFile(forReading: file)
        if let end = try await analyzer.analyzeSequence(from: audioFile) {
            try await analyzer.finalizeAndFinish(through: end)
        } else {
            await analyzer.cancelAndFinishNow()
        }
        return try await collector.value.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func recognize(
        file: URL, language: String, partial: @escaping @MainActor (String) -> Void
    ) async throws -> String {
        guard let recognizer = SFSpeechRecognizer(locale: Locale(identifier: language)),
              let request = SpeechRequestFactory.onDeviceRequest(url: file, recognizer: recognizer)
        else { throw MessageTranslationError.unavailable }
        return try await withCheckedThrowingContinuation { continuation in
            var finished = false
            _ = recognizer.recognitionTask(with: request) { result, error in
                if finished { return }
                if let result {
                    let text = result.bestTranscription.formattedString
                    if result.isFinal {
                        finished = true
                        continuation.resume(returning: text)
                    } else {
                        Task { @MainActor in partial(text) }
                    }
                } else if let error {
                    finished = true
                    // "No speech detected" arrives as an error; it is a
                    // result, not a failure.
                    let code = (error as NSError).code
                    if code == 1110 {
                        continuation.resume(returning: "")
                    } else {
                        engineLog.warning("on-device recognition failed")
                        continuation.resume(throwing: MessageTranslationError.failed)
                    }
                }
            }
        }
    }
}

/// The audio track of a decrypted voice or video message as an M4A file, so
/// both engines read one format. Written to a temporary directory of its own
/// and removed by the caller; the wipe removes the directory too.
enum AudioExtract {

    static let directoryName = "transcribe-audio"

    static var directory: URL {
        FileManager.default.temporaryDirectory.appendingPathComponent(directoryName, isDirectory: true)
    }

    static func m4a(from source: URL) async throws -> URL {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let out = directory.appendingPathComponent(UUID().uuidString + ".m4a")
        let asset = AVURLAsset(url: source)
        guard let export = AVAssetExportSession(asset: asset, presetName: AVAssetExportPresetAppleM4A) else {
            throw MessageTranslationError.failed
        }
        if #available(iOS 18.0, *) {
            try await export.export(to: out, as: .m4a)
        } else {
            export.outputURL = out
            export.outputFileType = .m4a
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                export.exportAsynchronously { continuation.resume() }
            }
            guard export.status == .completed else { throw MessageTranslationError.failed }
        }
        return out
    }
}

@MainActor
final class NoTranscriptionEngine: MessageTranscriptionEngine {
    var isSupported: Bool { false }
    func state(language: String) async -> TranslationEngineState { .unavailable }
    func languages() async -> [String] { [] }
    func installedLanguages() async -> [String] { [] }
    func downloadPack(language: String, progress: @escaping @MainActor (Double) -> Void) async throws {
        throw MessageTranslationError.unavailable
    }
    func transcribe(
        audio: URL, language: String, partial: @escaping @MainActor (String) -> Void
    ) async throws -> String {
        throw MessageTranslationError.unavailable
    }
}

// MARK: - Network

/// Whether the device is on an expensive path (mobile data, a hotspot): a
/// pack is downloaded only off one unless the person allows mobile data
/// (UX 3.5). Reads the path; sends nothing.
@MainActor
final class NetworkCostWatcher: ObservableObject {
    static let shared = NetworkCostWatcher()

    @Published private(set) var isExpensive = false
    private let monitor = NWPathMonitor()

    private init() {
        monitor.pathUpdateHandler = { [weak self] path in
            let expensive = path.isExpensive
            Task { @MainActor in self?.isExpensive = expensive }
        }
        monitor.start(queue: DispatchQueue(label: "me.lakko.translation.path"))
    }
}
