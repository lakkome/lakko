// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// lakko-engines: Apple's on-device translation, transcription and language
// detection for the macOS desktop app (ADR-0082 note of 2026-11-18,
// ADR-0085). The Tauri host starts it on demand and talks to it over its
// standard input and output, one JSON object per line; the host's own crate
// forbids `unsafe`, so Apple's Swift frameworks are reached from here rather
// than through FFI.
//
// | | Engine | Local guarantee |
// |---|---|---|
// | Detection | `NLLanguageRecognizer` | runs in process; the framework has no network API |
// | Translation | `TranslationSession(installedSource:target:preferredStrategy: .lowLatency)` | Apple documents on-device processing; `lowLatency` is the traditional model, never Apple Intelligence; an `installedSource` session only uses packs already on the Mac and never starts a download |
// | Transcription | `SpeechAnalyzer` + `SpeechTranscriber` | Apple documents the transcriber as on-device |
//
// The same choices as the iOS client (`apps/ios/Lakko/Translation/
// MessageTranslationEngines.swift`). macOS 26.4 or later; on anything older
// the binary does not load and the host reports both engines absent.
//
// Nothing is written to disk here: text arrives in the request and audio as
// 16 kHz mono 16-bit PCM in memory, decoded by the page. Nothing is logged:
// this process writes only answers to standard output, and an answer that
// fails names an error code, never a text, a transcript or a language.

import AVFoundation
import Foundation
import NaturalLanguage
import Speech
import Translation

// MARK: - Wire

/// Writes one answer per line; concurrent requests answer in any order.
final class Output: @unchecked Sendable {
    private let lock = NSLock()
    func send(_ object: [String: Any]) {
        guard var data = try? JSONSerialization.data(withJSONObject: object, options: []) else { return }
        data.append(0x0A)
        lock.lock()
        FileHandle.standardOutput.write(data)
        lock.unlock()
    }
}

enum EngineError: Error {
    case badRequest
    case unavailable
    case notInstalled
    case failed

    var code: String {
        switch self {
        case .badRequest: return "bad-request"
        case .unavailable: return "unavailable"
        case .notInstalled: return "not-installed"
        case .failed: return "failed"
        }
    }
}

/// The most PCM samples one transcription takes: 16 minutes at 16 kHz, a
/// little over the 15-minute voice-message bound (ADR-0047).
let maxSamples = 16 * 60 * 16_000

// MARK: - Detection

func detect(_ text: String) -> [[String: Any]] {
    let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !trimmed.isEmpty else { return [] }
    let recognizer = NLLanguageRecognizer()
    recognizer.processString(trimmed)
    return recognizer.languageHypotheses(withMaximum: 6)
        .filter { $0.key != .undetermined }
        .sorted { $0.value > $1.value }
        .map { ["tag": $0.key.rawValue, "permille": Int(max(0, min(1000, ($0.value * 1000).rounded())))] }
}

// MARK: - Translation

func language(_ tag: String) -> Locale.Language { Locale.Language(identifier: tag) }

func sameLanguage(_ a: String, _ b: String) -> Bool {
    let la = language(a), lb = language(b)
    return la.languageCode == lb.languageCode && la.script == lb.script
}

func availability() -> LanguageAvailability {
    // Asked with the strategy the sessions use: a plain availability answered
    // `installed` for pairs whose `lowLatency` model was missing (seen on iOS).
    LanguageAvailability(preferredStrategy: .lowLatency)
}

func translationState(source: String, target: String) async -> String {
    if sameLanguage(source, target) { return "unavailable" }
    switch await availability().status(from: language(source), to: language(target)) {
    case .installed: return "ready"
    case .supported: return "needs-pack"
    case .unsupported: return "unavailable"
    @unknown default: return "unavailable"
    }
}

func translate(_ text: String, source: String, target: String) async throws -> String {
    guard await translationState(source: source, target: target) == "ready" else {
        throw EngineError.notInstalled
    }
    let session = TranslationSession(
        installedSource: language(source), target: language(target), preferredStrategy: .lowLatency)
    do {
        return try await session.translate(text).targetText
    } catch {
        throw EngineError.failed
    }
}

/// The languages that translate into `target` with a pack already on this
/// Mac (UX section 6, the list `Managed by your system`), as the iOS client
/// lists them: every supported source whose pair answers installed.
func installedTranslationSources(target: String) async -> [String] {
    var out: [String] = []
    for lang in await availability().supportedLanguages {
        let tag = lang.minimalIdentifier
        if sameLanguage(tag, target) || out.contains(tag) { continue }
        if await translationState(source: tag, target: target) == "ready" { out.append(tag) }
    }
    return out
}

// MARK: - Transcription

/// `en` alone is read with this Mac's region, so the same language picks the
/// same model every time (`supportedLocale(equivalentTo:)` otherwise varied
/// between `en_US` and `en_CA` across runs, measured 2026-11-18).
func speechLocale(_ tag: String) async -> Locale? {
    var wanted = Locale(identifier: tag)
    if wanted.region == nil, let region = Locale.current.region, let code = wanted.language.languageCode {
        wanted = Locale(languageCode: code, script: wanted.language.script, languageRegion: region)
    }
    if let exact = await SpeechTranscriber.supportedLocale(equivalentTo: wanted) { return exact }
    return await SpeechTranscriber.supportedLocale(equivalentTo: Locale(identifier: tag))
}

func transcriber(_ locale: Locale) -> SpeechTranscriber {
    SpeechTranscriber(locale: locale, transcriptionOptions: [], reportingOptions: [], attributeOptions: [])
}

/// Installed is read from `installedLocales`: `AssetInventory.status` answered
/// `supported` for a model that was on this Mac and transcribed (macOS 27.2,
/// measured 2026-11-18).
func isInstalled(_ locale: Locale) async -> Bool {
    let id = locale.identifier(.bcp47)
    return await SpeechTranscriber.installedLocales.contains { $0.identifier(.bcp47) == id }
}

func speechState(_ tag: String) async -> String {
    guard SpeechTranscriber.isAvailable, let locale = await speechLocale(tag) else { return "unavailable" }
    if await isInstalled(locale) { return "ready" }
    switch await AssetInventory.status(forModules: [transcriber(locale)]) {
    case .installed: return "ready"
    case .supported, .downloading: return "needs-pack"
    case .unsupported: return "unavailable"
    @unknown default: return "unavailable"
    }
}

/// The speech models on this Mac (UX section 6), by locale identifier.
func installedSpeechLanguages() async -> [String] {
    guard SpeechTranscriber.isAvailable else { return [] }
    var out: [String] = []
    for locale in await SpeechTranscriber.installedLocales {
        let id = locale.identifier(.bcp47)
        if !out.contains(id) { out.append(id) }
    }
    return out
}

func installSpeech(_ tag: String) async throws {
    guard SpeechTranscriber.isAvailable, let locale = await speechLocale(tag) else { throw EngineError.unavailable }
    do {
        if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber(locale)]) {
            try await request.downloadAndInstall()
        }
    } catch {
        throw EngineError.failed
    }
}

func transcribe(pcm: Data, tag: String) async throws -> String {
    guard SpeechTranscriber.isAvailable, let locale = await speechLocale(tag) else { throw EngineError.unavailable }
    guard await isInstalled(locale) else { throw EngineError.notInstalled }
    let samples = pcm.count / 2
    guard samples > 0, samples <= maxSamples, pcm.count % 2 == 0,
          let inFormat = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 16_000, channels: 1, interleaved: true),
          let input = AVAudioPCMBuffer(pcmFormat: inFormat, frameCapacity: AVAudioFrameCount(samples)),
          let channel = input.int16ChannelData
    else { throw EngineError.badRequest }
    input.frameLength = AVAudioFrameCount(samples)
    pcm.withUnsafeBytes { raw in
        let source = raw.bindMemory(to: Int16.self)
        for i in 0..<samples { channel[0][i] = Int16(littleEndian: source[i]) }
    }

    let module = transcriber(locale)
    guard let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [module]) else {
        throw EngineError.unavailable
    }
    let buffer: AVAudioPCMBuffer
    if format == inFormat {
        buffer = input
    } else {
        guard let converter = AVAudioConverter(from: inFormat, to: format) else { throw EngineError.failed }
        let capacity = AVAudioFrameCount(Double(samples) * format.sampleRate / inFormat.sampleRate) + 1024
        guard let out = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: capacity) else { throw EngineError.failed }
        var fed = false
        var error: NSError?
        converter.convert(to: out, error: &error) { _, status in
            if fed {
                status.pointee = .endOfStream
                return nil
            }
            fed = true
            status.pointee = .haveData
            return input
        }
        if error != nil { throw EngineError.failed }
        buffer = out
    }

    let analyzer = SpeechAnalyzer(modules: [module])
    let collector = Task { () throws -> String in
        var text = ""
        for try await result in module.results where result.isFinal {
            text += String(result.text.characters)
        }
        return text
    }
    let (stream, continuation) = AsyncStream<AnalyzerInput>.makeStream()
    continuation.yield(AnalyzerInput(buffer: buffer))
    continuation.finish()
    do {
        try await analyzer.start(inputSequence: stream)
        try await analyzer.finalizeAndFinishThroughEndOfInput()
        return try await collector.value.trimmingCharacters(in: .whitespacesAndNewlines)
    } catch {
        collector.cancel()
        throw EngineError.failed
    }
}

// MARK: - Dispatch

func string(_ request: [String: Any], _ key: String, max: Int) throws -> String {
    guard let v = request[key] as? String, !v.isEmpty, v.utf8.count <= max else { throw EngineError.badRequest }
    return v
}

/// A language tag as the page sends one: at most 35 bytes of letters, digits
/// and hyphens (BCP 47's longest practical form).
func tag(_ request: [String: Any], _ key: String) throws -> String {
    let v = try string(request, key, max: 35)
    guard v.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_") }) else {
        throw EngineError.badRequest
    }
    return v
}

func answer(_ request: [String: Any]) async throws -> [String: Any] {
    switch request["op"] as? String {
    case "probe":
        return ["translation": true, "transcription": SpeechTranscriber.isAvailable]
    case "detect":
        return ["hypotheses": detect(try string(request, "text", max: 1 << 20))]
    case "translation-state":
        return ["state": await translationState(source: try tag(request, "source"), target: try tag(request, "target"))]
    case "translate":
        let text = try string(request, "text", max: 1 << 20)
        return ["text": try await translate(text, source: try tag(request, "source"), target: try tag(request, "target"))]
    case "speech-state":
        return ["state": await speechState(try tag(request, "language"))]
    case "installed":
        return [
            "translation": await installedTranslationSources(target: try tag(request, "target")),
            "transcription": await installedSpeechLanguages(),
        ]
    case "speech-install":
        try await installSpeech(try tag(request, "language"))
        return [:]
    case "transcribe":
        guard let b64 = request["pcm"] as? String, let pcm = Data(base64Encoded: b64) else {
            throw EngineError.badRequest
        }
        return ["text": try await transcribe(pcm: pcm, tag: try tag(request, "language"))]
    default:
        throw EngineError.badRequest
    }
}

let output = Output()

if CommandLine.arguments.dropFirst().first == "--probe" {
    output.send(["translation": true, "transcription": SpeechTranscriber.isAvailable])
    exit(0)
}

// Standard input is read on a thread of its own: the main thread must stay
// free, since the Translation framework's availability answer never arrived
// while the main thread was blocked reading (measured 2026-11-18).
let reader = Thread {
    while let line = readLine(strippingNewline: true) {
        guard let data = line.data(using: .utf8),
              let request = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let id = request["id"] as? Int
        else { continue }
        let box = UncheckedBox(request)
        Task.detached {
            do {
                var reply = try await answer(box.value)
                reply["id"] = id
                reply["ok"] = true
                output.send(reply)
            } catch let e as EngineError {
                output.send(["id": id, "ok": false, "error": e.code])
            } catch {
                output.send(["id": id, "ok": false, "error": EngineError.failed.code])
            }
        }
    }
    // Standard input closed: the host is gone.
    exit(0)
}
reader.start()
dispatchMain()

/// A JSON object crossing into a task; it is read there and nowhere else.
struct UncheckedBox: @unchecked Sendable {
    let value: [String: Any]
    init(_ value: [String: Any]) { self.value = value }
}
