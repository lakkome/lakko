// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0082 on iOS: the local store (section 5) and the center's states (UX
/// section 3), driven with fake engines. The real engines are Apple's and are
/// not exercised here (`MessageTranslationFailClosedTests` pins how they are
/// called).
@MainActor
final class MessageTranslationTests: XCTestCase {

    private let account = "dGVzdC1hY2NvdW50LWlkLTEyMw=="
    private let conversation = "abcdef0123456789"
    private var directory: URL!

    override func setUp() async throws {
        AccountStoreGate.resetForTest()
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("translation-tests-\(UUID().uuidString)", isDirectory: true)
    }

    override func tearDown() async throws {
        try? FileManager.default.removeItem(at: directory)
        AccountStoreGate.resetForTest()
    }

    private let spanish = "Hola, ¿cómo estás? Mañana vamos a la playa con mis amigos y después comemos juntos."

    private func message(_ id: Int64, _ text: String, outgoing: Bool = false) -> ChatMessage {
        ChatMessage(messageId: id, direction: outgoing ? .outgoing : .incoming, text: text, createdAt: "")
    }

    private func makeCenter(
        translation: FakeTranslationEngine? = nil,
        transcription: FakeTranscriptionEngine? = nil,
        antiCensorship: Bool = false,
        expensive: Bool = false
    ) -> (MessageTranslationCenter, MessageTranslationStore) {
        let store = MessageTranslationStore(directory: directory)
        let defaults = UserDefaults(suiteName: "translation-tests-\(UUID().uuidString)")!
        let settings = MessageTranslationSettings(defaults: defaults)
        settings.target = "en"
        let center = MessageTranslationCenter(
            translationEngine: translation ?? FakeTranslationEngine(),
            transcriptionEngine: transcription ?? FakeTranscriptionEngine(), settings: settings,
            store: store, accountProvider: { [account] in account },
            antiCensorshipOn: { antiCensorship }, expensivePath: { expensive })
        return (center, store)
    }

    private func eventually(_ condition: () -> Bool, file: StaticString = #filePath, line: UInt = #line) async {
        for _ in 0..<200 {
            if condition() { return }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTFail("condition not met", file: file, line: line)
    }

    // MARK: - Store

    func testStoreSealsResultsAndKeepsTheTextOutOfTheFile() throws {
        let store = MessageTranslationStore(directory: directory)
        store.putResult(
            accountIdB64: account, conversation: conversation, messageId: 7, kind: .translation,
            result: .init(text: "secret words", language: "en", source: "es", opId: 1, complete: true))
        let raw = try Data(contentsOf: try XCTUnwrap(store.fileURL(account)))
        XCTAssertNil(raw.range(of: Data("secret words".utf8)), "the file holds ciphertext only")
        let values = try store.fileURL(account)!.resourceValues(forKeys: [.isExcludedFromBackupKey])
        XCTAssertEqual(values.isExcludedFromBackup, true)

        let reopened = MessageTranslationStore(directory: directory)
        XCTAssertEqual(reopened.result(accountIdB64: account, conversation: conversation, messageId: 7,
                                       kind: .translation)?.text, "secret words")
    }

    func testStoreDropsAMessageDeletedForEveryoneAndCountsWhatDeleteAllRemoves() {
        let store = MessageTranslationStore(directory: directory)
        for id: Int64 in 1...3 {
            store.putResult(
                accountIdB64: account, conversation: conversation, messageId: id, kind: .translation,
                result: .init(text: "t\(id)", language: "en", source: "es", opId: 1, complete: true))
        }
        store.removeMessage(accountIdB64: account, conversation: conversation, messageId: 2)
        XCTAssertNil(store.result(accountIdB64: account, conversation: conversation, messageId: 2, kind: .translation))
        XCTAssertEqual(store.count(accountIdB64: account), 2)
        XCTAssertEqual(store.deleteAllResults(accountIdB64: account), 2)
        XCTAssertEqual(store.count(accountIdB64: account), 0)
    }

    func testASealedAccountTakesNoWriteAndTheWipeRemovesTheFile() throws {
        let store = MessageTranslationStore(directory: directory)
        store.putResult(
            accountIdB64: account, conversation: conversation, messageId: 1, kind: .transcript,
            result: .init(text: "x", language: "en", source: nil, opId: 1, complete: true))
        AccountStoreGate.seal(account)
        store.putResult(
            accountIdB64: account, conversation: conversation, messageId: 2, kind: .transcript,
            result: .init(text: "y", language: "en", source: nil, opId: 1, complete: true))
        XCTAssertNil(store.result(accountIdB64: account, conversation: conversation, messageId: 2, kind: .transcript))
        try store.clear(accountIdB64: account)
        XCTAssertFalse(FileManager.default.fileExists(atPath: store.fileURL(account)!.path))
    }

    func testAnEditBackToTheSameWordsMakesAStoredTranslationStale() async {
        let engine = FakeTranslationEngine()
        let (center, store) = makeCenter(translation: engine)
        let original = message(10, spanish)
        center.translate(conversation: conversation, message: original)
        await eventually { engine.translateCalls == 1 && store.result(
            accountIdB64: account, conversation: conversation, messageId: 10, kind: .translation) != nil }
        XCTAssertEqual(store.result(accountIdB64: account, conversation: conversation, messageId: 10,
                                    kind: .translation)?.opId, 0, "never edited = op id 0")

        // The author edited it and then edited it back: the same words, a
        // newer op. The stored result is stale and is made again.
        center.showOriginal(conversation: conversation, message: original)
        let editedBack = ChatMessage(
            messageId: 10, direction: .incoming, text: spanish, createdAt: "", edited: true, lastOpId: 42)
        center.translate(conversation: conversation, message: editedBack)
        await eventually { engine.translateCalls == 2 }
        await eventually { store.result(accountIdB64: account, conversation: conversation, messageId: 10,
                                        kind: .translation)?.opId == 42 }
    }

    func testTheLastOpIdSurvivesPersistAndReload() throws {
        var edited = ChatMessage(messageId: 5, direction: .incoming, text: "hola", createdAt: "t")
        edited.noteOp(40)
        edited.noteOp(12)  // an older op re-fetched never lowers it
        XCTAssertEqual(edited.lastOpId, 40)
        let data = try JSONEncoder().encode(PersistedMessage(from: edited))
        let reloaded = try XCTUnwrap(try JSONDecoder().decode(PersistedMessage.self, from: data).toChatMessage())
        XCTAssertEqual(reloaded.lastOpId, 40)

        let never = ChatMessage(messageId: 6, direction: .incoming, text: "hola", createdAt: "t")
        let neverData = try JSONEncoder().encode(PersistedMessage(from: never))
        XCTAssertNil(try JSONDecoder().decode(PersistedMessage.self, from: neverData).toChatMessage()?.lastOpId)
        XCTAssertEqual(MessageTranslationCenter.opId(never), 0)
    }

    // MARK: - The menu row and the bubble

    func testAnIncomingMessageInAnotherLanguageOffersTranslateAndOwnDoesNot() async {
        let (center, _) = makeCenter()
        let incoming = message(10, spanish)
        center.observe(conversation: conversation, messages: [incoming])
        _ = center.menuState(conversation: conversation, message: incoming)
        await eventually { center.menuState(conversation: conversation, message: incoming) == .translate }
        XCTAssertEqual(center.menuState(conversation: conversation, message: message(11, spanish, outgoing: true)),
                       .unavailable)
    }

    func testTranslateShowsTheTranslationAndCopyCopiesIt() async {
        let engine = FakeTranslationEngine()
        let (center, store) = makeCenter(translation: engine)
        let incoming = message(10, spanish)
        center.translate(conversation: conversation, message: incoming)
        await eventually {
            if case .translated = center.phase(conversation: conversation, messageId: 10) { return true }
            return false
        }
        XCTAssertEqual(center.copyText(conversation: conversation, message: incoming), "EN:" + spanish)
        XCTAssertEqual(center.menuState(conversation: conversation, message: incoming), .showOriginal)
        XCTAssertNotNil(store.result(accountIdB64: account, conversation: conversation, messageId: 10, kind: .translation))

        center.showOriginal(conversation: conversation, message: incoming)
        XCTAssertEqual(center.phase(conversation: conversation, messageId: 10), .original(source: "es"))
        XCTAssertEqual(center.copyText(conversation: conversation, message: incoming), spanish)

        // A stored result is reused: no second engine call.
        center.translate(conversation: conversation, message: incoming)
        XCTAssertEqual(engine.translateCalls, 1)
    }

    func testAPackIsAskedForAndNeverDownloadedWhileAntiCensorshipIsOn() async {
        let engine = FakeTranslationEngine(state: .needsPack)
        let (center, _) = makeCenter(translation: engine, antiCensorship: true)
        center.translate(conversation: conversation, message: message(10, spanish))
        await eventually { center.phase(conversation: conversation, messageId: 10) == .blocked }
        XCTAssertNil(center.packRequest)
        XCTAssertEqual(engine.prepareCalls, 0)
    }

    func testAPackOnMobileDataWaitsForWifi() async {
        let engine = FakeTranslationEngine(state: .needsPack)
        let (center, _) = makeCenter(translation: engine, expensive: true)
        center.translate(conversation: conversation, message: message(10, spanish))
        await eventually { center.packRequest != nil }
        XCTAssertEqual(center.packRequest?.onMobileData, true)
        center.packRequest?.download()
        XCTAssertEqual(center.phase(conversation: conversation, messageId: 10), .waitingForWifi("es"))
        XCTAssertEqual(engine.prepareCalls, 0)
    }

    func testTheBarNamesTheConversationsLanguageAndDismissingKeepsItAway() async {
        let (center, _) = makeCenter()
        let messages = (1...8).map { message(Int64($0), spanish) }
        center.observe(conversation: conversation, messages: messages)
        await eventually { center.bars[conversation] == "es" }
        center.dismissBar(conversation: conversation)
        center.observe(conversation: conversation, messages: messages)
        try? await Task.sleep(nanoseconds: 100_000_000)
        XCTAssertNil(center.bars[conversation])
    }

    func testDeleteForEveryoneForgetsTheMessage() async {
        let (center, store) = makeCenter()
        let incoming = message(10, spanish)
        center.translate(conversation: conversation, message: incoming)
        await eventually { center.phase(conversation: conversation, messageId: 10) != .translating
            && center.phase(conversation: conversation, messageId: 10) != nil }
        store.removeMessage(accountIdB64: account, conversation: conversation, messageId: 10)
        center.forgetMessage(conversation: conversation, messageId: 10)
        XCTAssertNil(center.phase(conversation: conversation, messageId: 10))
        XCTAssertNil(store.result(accountIdB64: account, conversation: conversation, messageId: 10, kind: .translation))
    }

    func testNoTranscriptButtonWhenTheDeviceCannotTranscribe() async {
        let (center, _) = makeCenter(transcription: FakeTranscriptionEngine(state: .unavailable))
        _ = center.transcriptButtonOffered(conversation: conversation)
        try? await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertFalse(center.transcriptButtonOffered(conversation: conversation))

        let (ready, _) = makeCenter(transcription: FakeTranscriptionEngine(state: .ready))
        _ = ready.transcriptButtonOffered(conversation: conversation)
        await eventually { ready.transcriptButtonOffered(conversation: conversation) }
    }
}

// MARK: - Fakes

@MainActor
final class FakeTranslationEngine: MessageTranslationEngine {
    var state: TranslationEngineState
    var translateCalls = 0
    var prepareCalls = 0

    init(state: TranslationEngineState = .ready) { self.state = state }

    var isSupported: Bool { true }
    func state(source: String, target: String) async -> TranslationEngineState {
        sameLanguage(a: source, b: target) ? .unavailable : state
    }
    func sourceLanguages() async -> [String] { ["es", "de", "en"] }
    func installedLanguages(target: String) async -> [String] { state == .ready ? ["es"] : [] }
    func translate(_ text: String, source: String, target: String) async throws -> String {
        translateCalls += 1
        return "EN:" + text
    }
    func preparePack(source: String, target: String) async throws {
        prepareCalls += 1
        state = .ready
    }
}

@MainActor
final class FakeTranscriptionEngine: MessageTranscriptionEngine {
    let state: TranslationEngineState
    init(state: TranslationEngineState = .ready) { self.state = state }

    var isSupported: Bool { true }
    func state(language: String) async -> TranslationEngineState { state }
    func languages() async -> [String] { ["en", "es"] }
    func installedLanguages() async -> [String] { [] }
    func downloadPack(language: String, progress: @escaping @MainActor (Double) -> Void) async throws {}
    func transcribe(audio: URL, language: String, partial: @escaping @MainActor (String) -> Void) async throws -> String {
        "hello"
    }
}
