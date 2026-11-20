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
            expensivePath: { expensive })
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

    /// ADR-0082 amendment of 2026-11-19: a pack is offered and downloaded
    /// with anti-censorship on, as with it off.
    func testAPackIsOfferedAndDownloadedWithAntiCensorshipOn() async {
        let previous = AntiCensorshipConfig.current
        AntiCensorshipConfig.current = AntiCensorshipConfig(
            armed: true, bootstrapConfigJson: Data(), trustAnchor: Data())
        defer { AntiCensorshipConfig.current = previous }
        let engine = FakeTranslationEngine(state: .needsPack)
        let (center, _) = makeCenter(translation: engine)
        center.translate(conversation: conversation, message: message(10, spanish))
        await eventually { center.packRequest != nil }
        XCTAssertEqual(center.packRequest?.kind, .translation)
        center.packRequest?.download()
        await eventually { engine.prepareCalls == 1 }
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

// MARK: - An unsure guess that cannot end on a correctable line

extension MessageTranslationTests {

    private func guess(_ tag: String, _ permille: UInt16, _ alternatives: [String] = []) -> (String) -> LanguageCandidates? {
        { _ in LanguageCandidates(detected: DetectedLanguage(tag: tag, confidencePermille: permille), alternatives: alternatives) }
    }

    func testAnUnsureGuessOfTheTargetLanguageOpensThePickerWithItMarked() async {
        let engine = FakeTranslationEngine()
        let (center, _) = makeCenter(translation: engine)
        center.detect = guess("en", 100)
        center.translate(conversation: conversation, message: message(10, "ok"), askLanguage: true)
        await eventually { center.pickerRequest != nil }
        XCTAssertEqual(center.pickerRequest?.marked, "en")
        XCTAssertTrue(center.pickerRequest?.languages.contains("en") ?? false, "the guess is listed to be seen")
        XCTAssertNil(center.phase(conversation: conversation, messageId: 10), "no 'already in' line")
        XCTAssertEqual(engine.translateCalls, 0)
    }

    func testAnUnsureGuessTheEngineCannotTranslateFromOpensThePickerWithItMarked() async {
        let engine = FakeTranslationEngine(state: .unavailable)
        let (center, _) = makeCenter(translation: engine)
        center.detect = guess("es", 100)
        center.translate(conversation: conversation, message: message(10, "hola"))
        await eventually { center.pickerRequest != nil }
        XCTAssertEqual(center.pickerRequest?.marked, "es")
        XCTAssertNil(center.phase(conversation: conversation, messageId: 10), "no 'Can't translate from' line")
    }

    func testAnUnsureGuessTheEngineCanDoTranslatesAtOnce() async {
        let engine = FakeTranslationEngine()
        let (center, _) = makeCenter(translation: engine)
        center.detect = guess("es", 100)
        center.translate(conversation: conversation, message: message(10, "hola"), askLanguage: true)
        await eventually {
            if case .translated(_, let source) = center.phase(conversation: conversation, messageId: 10) {
                return source == "es"
            }
            return false
        }
        XCTAssertNil(center.pickerRequest)
    }

    func testAConfidentSourceKeepsTheExistingLines() async {
        let sameEngine = FakeTranslationEngine()
        let (same, _) = makeCenter(translation: sameEngine)
        same.detect = guess("en", 990)
        same.translate(conversation: conversation, message: message(10, "hello there"))
        await eventually { same.phase(conversation: conversation, messageId: 10) == .sameLanguage("en") }
        XCTAssertNil(same.pickerRequest)

        let (cannot, _) = makeCenter(translation: FakeTranslationEngine(state: .unavailable))
        cannot.detect = guess("es", 990)
        cannot.translate(conversation: conversation, message: message(11, "hola"))
        await eventually { cannot.phase(conversation: conversation, messageId: 11) == .unsupported("es") }
        XCTAssertNil(cannot.pickerRequest)
    }
}

// MARK: - The source order and the language line up front (UX 3.1 / 3.2)

extension MessageTranslationTests {

    private func detecting(_ table: [String: DetectedLanguage]) -> (String) -> LanguageCandidates? {
        { text in table[text].map { LanguageCandidates(detected: $0, alternatives: []) } }
    }

    /// Found live 2026-11-15: a remembered Dutch pick read a Ukrainian
    /// message (uk at 670, under the confident line) from Dutch.
    func testARememberedPickDoesNotTranslateAnUnsureGuessFromItself() async {
        let (center, _) = makeCenter()
        let ukrainian = "Я завтра піду на ринок по овочі."
        center.detect = detecting([
            "goedemorgen": DetectedLanguage(tag: "nl", confidencePermille: 400),
            ukrainian: DetectedLanguage(tag: "uk", confidencePermille: 670),
        ])
        center.chooseSource(conversation: conversation, message: message(10, "goedemorgen"), tag: "nl")
        center.translate(conversation: conversation, message: message(11, ukrainian))
        await eventually {
            if case .translated(_, let source) = center.phase(conversation: conversation, messageId: 11) {
                return source == "uk"
            }
            return false
        }
        // With no guess at all, the remembered pick is used.
        center.translate(conversation: conversation, message: message(12, "???"))
        await eventually {
            if case .translated(_, let source) = center.phase(conversation: conversation, messageId: 12) {
                return source == "nl"
            }
            return false
        }
        // An unusable guess (the target language) yields to the remembered pick, not the picker.
        center.detect = detecting(["ok sure": DetectedLanguage(tag: "en", confidencePermille: 300)])
        center.translate(conversation: conversation, message: message(13, "ok sure"))
        await eventually {
            if case .translated(_, let source) = center.phase(conversation: conversation, messageId: 13) {
                return source == "nl"
            }
            return false
        }
        XCTAssertNil(center.pickerRequest)
    }

    private func settledLine(_ center: MessageTranslationCenter, _ message: ChatMessage) async -> MessageTranslationCenter.Phase? {
        _ = center.quietLine(conversation: conversation, message: message)
        for _ in 0..<20 {
            if center.quietLine(conversation: conversation, message: message) != nil { break }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        return center.quietLine(conversation: conversation, message: message)
    }

    func testTheLanguageLineNamesAnotherLanguageWithTranslate() async {
        let (center, _) = makeCenter()
        let line = await settledLine(center, message(10, spanish))
        XCTAssertEqual(line, .detected(source: "es", canTranslate: true))
    }

    /// Walk 7 (2026-11-20): English test messages read "Polish · Translate".
    /// An unsure guess is never named up front.
    func testTheLanguageLineDoesNotNameAnUnsureGuess() async {
        let (center, _) = makeCenter()
        center.detect = guess("es", 300)
        let line = await settledLine(center, message(10, "hola"))
        XCTAssertNil(line)
    }

    func testNoLanguageLineOnOwnMessagesOrWithoutText() async {
        let (center, _) = makeCenter()
        let own = await settledLine(center, message(10, spanish, outgoing: true))
        XCTAssertNil(own)
        let empty = await settledLine(center, message(11, ""))
        XCTAssertNil(empty)
    }

    func testNoLanguageLineForAReadingLanguageConfidentOrUnsure() async {
        let (center, _) = makeCenter()
        center.detect = guess("en", 990)
        let confident = await settledLine(center, message(10, "hello there"))
        XCTAssertNil(confident)
        let (unsure, _) = makeCenter()
        unsure.detect = guess("en", 300)
        let unsureLine = await settledLine(unsure, message(11, "ok"))
        XCTAssertNil(unsureLine)
    }

    private func line(_ tag: String?, _ permille: UInt16, canTranslate: Bool) -> TranslationSourceResolution {
        TranslationSourceResolution(
            lineTag: tag, lineConfidencePermille: permille, lineCanTranslate: canTranslate,
            translateSource: tag, guess: nil)
    }

    func testNoLanguageLineWithTheTranslateSettingOff() {
        let spanish = line("es", 990, canTranslate: true)
        XCTAssertNil(MessageTranslationCenter.languageLine(
            resolution: spanish, isOwn: false, enabled: false, readingLanguages: ["en"]))
        XCTAssertEqual(MessageTranslationCenter.languageLine(
            resolution: spanish, isOwn: false, enabled: true, readingLanguages: ["en"]),
            .detected(source: "es", canTranslate: true))
        // The rule named no language: no line.
        XCTAssertNil(MessageTranslationCenter.languageLine(
            resolution: line(nil, 200, canTranslate: false), isOwn: false, enabled: true, readingLanguages: ["en"]))
    }

    func testTheLanguageLineHasNoTranslateWhenTheDeviceCannotTranslateFromIt() async {
        let (center, _) = makeCenter(translation: FakeTranslationEngine(state: .unavailable))
        let line = await settledLine(center, message(10, spanish))
        XCTAssertEqual(line, .detected(source: "es", canTranslate: false))
    }

    func testNoLanguageLineForAnUnsureGuessTheDeviceCannotTranslateFrom() async {
        // Seen on a Pixel: "goedendag" named Turkmen, an unsure guess, with no
        // close candidate the device can translate from.
        let engine = FakeTranslationEngine()
        engine.unavailable = ["tk", "da"]
        let (center, _) = makeCenter(translation: engine)
        center.detect = guess("tk", 200, ["da"])
        let line = await settledLine(center, message(10, "goedendag"))
        XCTAssertNil(line)
    }

    func testTheDetectorCarriesTheCloseCandidatesOfANearTie() {
        // The recognizer's hypotheses through the shared rule; a confident
        // sentence has none.
        let de = MessageLanguageDetector.candidates(
            "Ich gehe morgen auf den Markt, um Gemüse zu kaufen. Willst du mitkommen?")
        XCTAssertEqual(de?.detected.tag, "de")
        XCTAssertEqual(de?.alternatives, [])
        XCTAssertNil(MessageLanguageDetector.candidates("   "))
        // Hypotheses as the recognizer hands them: the rule, not this client.
        let set = languageCandidatesFromHypotheses(text: "hola, qué tal?", hypotheses: [
            LanguageHypothesis(tag: "ca", probabilityPermille: 400),
            LanguageHypothesis(tag: "es", probabilityPermille: 350),
        ])
        XCTAssertEqual(set?.alternatives, ["es"])
    }

    func testAConfidentLanguageOutsideTheEngineKeepsItsNameAlone() async {
        // A real Turkmen message: named, not mislabelled as a candidate.
        let engine = FakeTranslationEngine()
        engine.unavailable = ["tk"]
        let (center, _) = makeCenter(translation: engine)
        center.detect = guess("tk", 950)
        let line = await settledLine(center, message(10, "Men ertir bazara gitjek."))
        XCTAssertEqual(line, .detected(source: "tk", canTranslate: false))
    }

    func testAnUnsureGuessItCannotTranslateFromIsTranslatedByItsFirstUsableCandidate() async {
        // Seen on a Pixel: "hola, qué tal?" read as unsure Catalan. Translate
        // uses Spanish; an unsure guess names nothing up front (walk 7).
        let engine = FakeTranslationEngine()
        engine.unavailable = ["ca"]
        let (center, _) = makeCenter(translation: engine)
        center.detect = guess("ca", 180, ["es", "fr", "it"])
        let incoming = message(10, "hola, qué tal?")
        let line = await settledLine(center, incoming)
        XCTAssertNil(line)
        center.translate(conversation: conversation, message: incoming)
        await eventually { engine.translateCalls == 1 }
        XCTAssertEqual(engine.lastSource, "es")
    }

    func testWithoutAUsableCandidateTheGuessOpensThePickerWithTheCandidatesFirst() async {
        let engine = FakeTranslationEngine()
        engine.unavailable = ["tk", "da"]
        let (center, _) = makeCenter(translation: engine)
        center.detect = guess("tk", 200, ["da"])
        center.translate(conversation: conversation, message: message(10, "goedendag"))
        await eventually { center.pickerRequest != nil }
        XCTAssertEqual(center.pickerRequest?.marked, "tk")
        XCTAssertEqual(center.pickerRequest?.candidates, ["tk", "da"])
        XCTAssertEqual(engine.translateCalls, 0)
        // `Not X?` lists the close candidates first.
        XCTAssertEqual(
            TranslationLanguageChoices.ordered(["de", "en", "es", "it"], candidates: ["ca", "es", "it"]),
            ["es", "it", "de", "en"])
    }

    func testAPickDecidesANearTieButNeverAGuessItIsNotACandidateOf() async {
        let engine = FakeTranslationEngine()
        let (center, _) = makeCenter(translation: engine)
        // The pick makes Italian the conversation's language.
        center.detect = guess("es", 110, ["it", "la"])
        center.chooseSource(conversation: conversation, message: message(10, "Ciao a tutti"), tag: "it")
        let tie = message(11, "Ciao, come stai?")
        // Unsure: no line up front (walk 7), Translate starts from Italian.
        let line = await settledLine(center, tie)
        XCTAssertNil(line)
        let before = engine.translateCalls
        center.translate(conversation: conversation, message: tie)
        await eventually { engine.translateCalls == before + 1 }
        XCTAssertEqual(engine.lastSource, "it")
        // A guess Italian is not a candidate of keeps its own language.
        center.detect = guess("uk", 400, ["ru"])
        center.translate(conversation: conversation, message: message(12, "Можешь мне позже перезвонить?"))
        await eventually { engine.translateCalls == before + 2 }
        XCTAssertEqual(engine.lastSource, "uk")
    }

    func testAConfidentDetectionBecomesTheConversationsLanguage() async {
        let (center, store) = makeCenter()
        center.detect = { text in
            text == "ciao a tutti, come va oggi?"
                ? LanguageCandidates(detected: DetectedLanguage(tag: "it", confidencePermille: 950), alternatives: [])
                : nil
        }
        center.observe(conversation: conversation, messages: [message(10, "ciao a tutti, come va oggi?")])
        XCTAssertEqual(
            store.prefs(accountIdB64: account, conversation: conversation).languages.detections.map(\.tag), ["it"])
    }

    func testG55TwoNamesHolaSpanishWhateverOrderTheChatIsObservedIn() async {
        // Seen on a Pixel: the remembered language was whichever confident
        // detection ran last. One confident message each in es, it, uk, de,
        // ru: Spanish and Italian tie, and the detector lists Spanish first.
        let seen: [(Int64, String, String)] = [
            (1, "Mañana voy al mercado.", "es"), (2, "Ci vediamo domani.", "it"),
            (3, "Завтра я піду на ринок.", "uk"), (4, "Morgen gehe ich.", "de"),
            (5, "Завтра я пойду.", "ru"),
        ]
        // The last order ends on Italian: the order that named it Italian before.
        let orders = [seen, Array(seen.reversed()), [seen[1], seen[4], seen[0], seen[3], seen[2]],
                      [seen[0], seen[2], seen[3], seen[4], seen[1]]]
        for (index, order) in orders.enumerated() {
            let engine = FakeTranslationEngine()
            let (center, _) = makeCenter(translation: engine)
            center.detect = { text in
                if text == "hola, qué tal?" {
                    return LanguageCandidates(
                        detected: DetectedLanguage(tag: "ca", confidencePermille: 180), alternatives: ["es", "fr", "it"])
                }
                return seen.first { $0.1 == text }.map {
                    LanguageCandidates(detected: DetectedLanguage(tag: $0.2, confidencePermille: 950), alternatives: [])
                }
            }
            // Observed one message at a time, in this order.
            for entry in order {
                center.observe(conversation: conversation, messages: [message(entry.0, entry.1)])
            }
            // A fresh message id per order: the result store is shared.
            let hola = message(Int64(90 + index), "hola, qué tal?")
            // Unsure: no line up front (walk 7); Translate starts from Spanish.
            let line = await settledLine(center, hola)
            XCTAssertNil(line)
            center.translate(conversation: conversation, message: hola)
            await eventually { engine.translateCalls == 1 }
            XCTAssertEqual(engine.lastSource, "es")
        }
    }

    func testAShownTranslationReplacesTheLanguageLine() async {
        let (center, _) = makeCenter()
        let incoming = message(10, spanish)
        center.translate(conversation: conversation, message: incoming)
        await eventually {
            if case .translated = center.quietLine(conversation: conversation, message: incoming) { return true }
            return false
        }
        // An automatic translation too.
        let (auto, _) = makeCenter()
        let other = message(11, spanish)
        auto.setAutoTranslate(conversation: conversation, on: true, messages: [other])
        await eventually {
            if case .translated = auto.quietLine(conversation: conversation, message: other) { return true }
            return false
        }
    }

    func testTheLanguageLineReadsTheMenuRowsDetection() async {
        let (center, _) = makeCenter()
        var calls = 0
        center.detect = { _ in
            calls += 1
            return LanguageCandidates(detected: DetectedLanguage(tag: "es", confidencePermille: 990), alternatives: [])
        }
        let incoming = message(10, spanish)
        _ = center.menuState(conversation: conversation, message: incoming)
        _ = await settledLine(center, incoming)
        _ = center.menuState(conversation: conversation, message: incoming)
        XCTAssertEqual(calls, 1)
    }
}

// MARK: - Fakes

@MainActor
final class FakeTranslationEngine: MessageTranslationEngine {
    var state: TranslationEngineState
    var translateCalls = 0
    var prepareCalls = 0
    var lastSource: String?
    /// Sources this engine cannot translate from, whatever `state` says.
    var unavailable: Set<String> = []

    init(state: TranslationEngineState = .ready) { self.state = state }

    var isSupported: Bool { true }
    func state(source: String, target: String) async -> TranslationEngineState {
        sameLanguage(a: source, b: target) || unavailable.contains(source) ? .unavailable : state
    }
    func sourceLanguages() async -> [String] { ["es", "de", "en"] }
    func installedLanguages(target: String) async -> [String] { state == .ready ? ["es"] : [] }
    func translate(_ text: String, source: String, target: String) async throws -> String {
        translateCalls += 1
        lastSource = source
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
