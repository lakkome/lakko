// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Speech
import Translation
import XCTest
@testable import Lakko

/// ADR-0082 section 2 — translation and transcription never run anywhere but
/// on this device.
///
/// Three properties, each the test of one way the guarantee could break:
///
///  1. Every speech request the app builds carries
///     `requiresOnDeviceRecognition = true`, and a recognizer that cannot
///     recognize on device gets no request at all (the language is then
///     unavailable, never "try the network").
///  2. No other source builds a speech request or a translation session, so
///     (1) covers every one. The scan reads the app's sources from the
///     checkout the test was built from, like `NoSystemShareSheetTests`.
///  3. Nothing in the translation code makes a network request.
final class MessageTranslationFailClosedTests: XCTestCase {

    func testEverySpeechRequestIsBuiltOnDeviceOrNotAtAll() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("unused.m4a")
        let locales = Array(SFSpeechRecognizer.supportedLocales())
        var checked = 0
        for locale in locales {
            guard let recognizer = SFSpeechRecognizer(locale: locale) else { continue }
            let request = SpeechRequestFactory.onDeviceRequest(url: url, recognizer: recognizer)
            if recognizer.supportsOnDeviceRecognition {
                let built = try XCTUnwrap(request, "an on-device recognizer gets a request")
                XCTAssertTrue(built.requiresOnDeviceRecognition, "\(locale.identifier) must stay on device")
            } else {
                XCTAssertNil(request, "a recognizer without on-device support gets no request")
            }
            checked += 1
        }
        XCTAssertGreaterThan(checked, 0, "the simulator lists speech locales")
    }

    @available(iOS 18.0, *)
    func testTranslationSessionsUseTheTraditionalOnDeviceModel() throws {
        let configuration = TranslationSessionFactory.configuration(source: "es", target: "en")
        XCTAssertEqual(configuration.source?.minimalIdentifier, "es")
        XCTAssertEqual(configuration.target?.minimalIdentifier, "en")
        if #available(iOS 26.4, *) {
            XCTAssertEqual(configuration.preferredStrategy, .lowLatency)
        }
    }

    func testOnlyTheFactoriesBuildSpeechRequestsAndTranslationSessions() throws {
        let files = try swiftFiles()
        var hits: [String] = []
        for file in files {
            let code = try stripped(file)
            let name = file.lastPathComponent
            for word in ["SFSpeechURLRecognitionRequest(", "SFSpeechAudioBufferRecognitionRequest("]
            where code.contains(word) && name != "MessageTranslationEngines.swift" {
                hits.append("\(name): \(word)")
            }
            for word in ["TranslationSession(", ".translationTask(", "recognitionTask("]
            where code.contains(word) && name != "MessageTranslationEngines.swift" {
                hits.append("\(name): \(word)")
            }
        }
        XCTAssertEqual(hits, [], "speech requests and translation sessions are built in one file")

        let engines = try XCTUnwrap(files.first { $0.lastPathComponent == "MessageTranslationEngines.swift" })
        let code = try stripped(engines)
        // The URL request is created in exactly one place, the factory.
        XCTAssertEqual(code.components(separatedBy: "SFSpeechURLRecognitionRequest(").count - 1, 1)
        XCTAssertTrue(code.contains("requiresOnDeviceRecognition = true"))
    }

    func testTheTranslationCodeMakesNoNetworkRequest() throws {
        let files = try swiftFiles().filter { $0.deletingLastPathComponent().lastPathComponent == "Translation" }
        XCTAssertGreaterThanOrEqual(files.count, 5, "the scan must see the translation sources")
        var hits: [String] = []
        for file in files {
            let code = try stripped(file)
            for word in ["URLSession", "URLRequest", "dataTask(", "NWConnection", "AttachmentBridge.upload"]
            where code.contains(word) {
                hits.append("\(file.lastPathComponent): \(word)")
            }
        }
        XCTAssertEqual(hits, [])
    }

    // MARK: - Sources

    private func swiftFiles() throws -> [URL] {
        let root = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("Lakko", isDirectory: true)
        guard FileManager.default.isReadableFile(atPath: root.path) else {
            throw XCTSkip("the app's sources are not readable from this test run")
        }
        let walker = FileManager.default.enumerator(at: root, includingPropertiesForKeys: nil)
        return (walker?.compactMap { $0 as? URL } ?? []).filter { $0.pathExtension == "swift" }
    }

    /// The source with comments and string literals removed.
    private func stripped(_ file: URL) throws -> String {
        var out = try String(contentsOf: file, encoding: .utf8)
        for pattern in ["/\\*[\\s\\S]*?\\*/", "//[^\\n]*", "\"(?:\\\\.|[^\"\\\\\\n])*\""] {
            out = out.replacingOccurrences(of: pattern, with: " ", options: .regularExpression)
        }
        return out
    }
}
